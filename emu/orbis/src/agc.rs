//! PS5 AGC command-buffer builders and minimal synchronous PM4 command processor.
//!
//! Games build Direct Command Buffers (DCBs) using `libSceAgc` helpers and submit them via
//! `sceAgcDriverSubmitDcb`. Each builder allocates dwords from the guest's command-buffer
//! cursor, writes PM4 type-3 packets, and returns a pointer to the packet.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const INVALID_ARG: i32 = 0x8000_0001u32 as i32;

// Standard AMD GFX10 PM4 type-3 opcodes.
const OP_NOP: u32 = 0x10;
const OP_SET_BASE: u32 = 0x11;
const OP_INDEX_BUFFER_SIZE: u32 = 0x13;
const OP_DISPATCH_DIRECT: u32 = 0x15;
const OP_DISPATCH_INDIRECT: u32 = 0x16;
const OP_SET_PREDICATION: u32 = 0x20;
const OP_INDEX_BASE: u32 = 0x26;
const OP_DRAW_INDIRECT_MULTI: u32 = 0x2C;
const OP_NUM_INSTANCES: u32 = 0x2F;
const OP_DRAW_INDEX_OFFSET_2: u32 = 0x35;
const OP_WRITE_DATA: u32 = 0x37;
const OP_DRAW_INDEX_INDIRECT_MULTI: u32 = 0x38;
const OP_WAIT_REG_MEM: u32 = 0x3C;
const OP_INDIRECT_BUFFER: u32 = 0x3F;
const OP_COPY_DATA: u32 = 0x40;
const OP_EVENT_WRITE: u32 = 0x46;
const OP_SET_SH_REG_INDIRECT: u32 = 0x63;
const OP_SET_UC_REG_INDIRECT: u32 = 0x64;
const OP_SET_CONTEXT_REG: u32 = 0x69;
const OP_SET_SH_REG: u32 = 0x76;
const OP_SET_UCONFIG_REG: u32 = 0x79;
const OP_SET_UC_REG_INDEX: u32 = 0x7A;
const OP_SET_CX_REG_INDIRECT: u32 = 0x9F;
const OP_DMA_DATA: u32 = 0x50;

const TYPE2_NOP: u32 = 0x8000_0000;
const MAX_NOP_DWORDS: u32 = 0x4001;

// Private AGC markers carried inside OP_NOP bits [7:2].
const R_ZERO: u32 = 0x00;
const R_DRAW_INDEX_AUTO: u32 = 0x04;
const R_WAIT_FLIP_DONE: u32 = 0x06;
const R_WAIT_MEM32: u32 = 0x0A;
const R_PUSH_MARKER: u32 = 0x0B;
const R_POP_MARKER: u32 = 0x0C;
const R_INDEX_COUNT: u32 = 0x0D;
const R_SH_REGS_INDIRECT: u32 = 0x11;
const R_CX_REGS_INDIRECT: u32 = 0x12;
const R_UC_REGS_INDIRECT: u32 = 0x13;
const R_ACQUIRE_MEM: u32 = 0x14;
const R_WRITE_DATA: u32 = 0x15;
const R_WAIT_MEM64: u32 = 0x16;
const R_FLIP: u32 = 0x17;
const R_RELEASE_MEM: u32 = 0x18;
const R_DMA_DATA: u32 = 0x19;
const R_CONTEXT_STATE: u32 = 0x1C;

#[inline]
const fn pm4(len_dwords: u32, op: u32, sub: u32) -> u32 {
    0xC000_0000 | (((len_dwords.wrapping_sub(2)) & 0x3FFF) << 16) | ((op & 0xFF) << 8) | ((sub & 0x3F) << 2)
}

#[inline]
const fn pm4_len(hdr: u32) -> u32 {
    ((hdr >> 16) & 0x3FFF) + 2
}

#[inline]
const fn pm4_op(hdr: u32) -> u32 {
    (hdr >> 8) & 0xFF
}

#[inline]
const fn pm4_sub(hdr: u32) -> u32 {
    (hdr >> 2) & 0x3F
}

// ── Command buffer cursor management ──────────────────────────────────
//
// Guest command-buffer layout:
//   +0x10: cursor_up (u64)   — next write pointer
//   +0x18: cursor_down (u64) — end / limit pointer
//   +0x20: callback (u64)    — guest buffer-full handler(cb, needed_dwords, userdata)
//   +0x28: userdata (u64)
//   +0x30: reserved (u32)    — dwords held back for trailer packets

static CB_LOCK: Mutex<()> = Mutex::new(());

unsafe fn cb_remaining(cb: usize) -> (u64, u64, u64, u64, u32, u32) {
    let up = *((cb + 0x10) as *const u64);
    let down = *((cb + 0x18) as *const u64);
    let func = *((cb + 0x20) as *const u64);
    let udata = *((cb + 0x28) as *const u64);
    let reserved = *((cb + 0x30) as *const u32);
    let avail = (down.wrapping_sub(up) as i64 / 4) as u32;
    let rem = avail.saturating_sub(reserved);
    (up, down, func, udata, reserved, rem)
}

/// Reserves `dwords` from a guest command buffer and advances its cursor.
fn alloc_dwords(cb: usize, dwords: u32) -> usize {
    if cb == 0 || dwords == 0 {
        return 0;
    }
    let mut guard = CB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let (mut up, _down, func, udata, reserved, rem) = cb_remaining(cb);
        if dwords > rem {
            if func == 0 {
                return 0;
            }
            // Drop lock while calling back into guest code.
            drop(guard);
            let f: extern "sysv64" fn(usize, u64, u64) -> u64 = std::mem::transmute(func as usize);
            let _ = f(cb, (dwords + reserved) as u64, udata);
            guard = CB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let (new_up, _, _, _, _, new_rem) = cb_remaining(cb);
            if dwords > new_rem {
                return 0;
            }
            up = new_up;
        }
        *((cb + 0x10) as *mut u64) = up + (dwords as u64) * 4;
        drop(guard);
        up as usize
    }
}

fn emit(cb: usize, words: &[u32]) -> usize {
    let dst = alloc_dwords(cb, words.len() as u32);
    if dst == 0 {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(words.as_ptr(), dst as *mut u32, words.len());
    }
    dst
}

// ── General command packets ───────────────────────────────────────────

pub extern "sysv64" fn cb_nop(cb: usize, dwords: u32) -> usize {
    if cb == 0 || dwords == 0 || dwords > MAX_NOP_DWORDS {
        return 0;
    }
    let dst = alloc_dwords(cb, dwords);
    if dst == 0 {
        return 0;
    }
    unsafe {
        let p = dst as *mut u32;
        *p = if dwords == 1 { TYPE2_NOP } else { pm4(dwords, OP_NOP, R_ZERO) };
        for i in 1..dwords as usize {
            *p.add(i) = 0;
        }
    }
    dst
}

pub extern "sysv64" fn cb_dispatch(cb: usize, gx: u32, gy: u32, gz: u32, modifier: u32) -> usize {
    emit(cb, &[pm4(5, OP_DISPATCH_DIRECT, 0), gx, gy, gz, (modifier & 0xA038) | 0x41])
}

pub extern "sysv64" fn dcb_dispatch_indirect(cb: usize, offset: u32, modifier: u32) -> usize {
    emit(cb, &[pm4(3, OP_DISPATCH_INDIRECT, 0), offset, (modifier & 0xA038) | 0x41])
}

pub extern "sysv64" fn dcb_set_base_indirect_args(cb: usize, base_idx: u32, addr: u64) -> usize {
    emit(cb, &[pm4(4, OP_SET_BASE, 0) | (base_idx << 1), 1, (addr as u32) & !7, (addr >> 32) as u32])
}

pub extern "sysv64" fn get_data_packet_payload_address(out: *mut u64, cmd: usize, kind: i32) -> i32 {
    if out.is_null() || cmd == 0 {
        return INVALID_ARG;
    }
    let payload = if kind == 0 {
        let hdr = unsafe { *(cmd as *const u32) };
        if (hdr & 0x3FFF_0000) == 0x3FFF_0000 { 0 } else { cmd as u64 + 4 }
    } else {
        cmd as u64 + 8
    };
    unsafe { *out = payload };
    0
}

pub extern "sysv64" fn dcb_push_marker(cb: usize, text: *const u8) -> usize {
    if cb == 0 || text.is_null() {
        return 0;
    }
    let bytes = unsafe {
        let mut n = 0usize;
        while n < 4095 && *text.add(n) != 0 {
            n += 1;
        }
        std::slice::from_raw_parts(text, n)
    };
    let payload_dw = ((bytes.len() + 4) / 4).max(1) as u32;
    let total_dw = payload_dw + 1;
    let dst = alloc_dwords(cb, total_dw);
    if dst == 0 {
        return 0;
    }
    unsafe {
        *(dst as *mut u32) = pm4(total_dw, OP_NOP, R_PUSH_MARKER);
        std::ptr::write_bytes((dst + 4) as *mut u8, 0, (payload_dw as usize) * 4);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (dst + 4) as *mut u8, bytes.len());
    }
    dst
}

pub extern "sysv64" fn dcb_pop_marker(cb: usize) -> usize {
    emit(cb, &[pm4(2, OP_NOP, R_POP_MARKER), 0])
}

pub extern "sysv64" fn dcb_acquire_mem(
    cb: usize, engine: u32, cb_db_op: u32, gcr: u32, base: u64, size: u64, poll: u32,
) -> usize {
    if cb == 0 || (engine & 0xFF) > 1 {
        return 0;
    }
    let base_units = (base >> 8) as u32;
    let size_units = if size == 0 { 0 } else { ((size.saturating_add(255)) >> 8).min(u32::MAX as u64) as u32 };
    emit(cb, &[
        pm4(8, OP_NOP, R_ACQUIRE_MEM),
        ((engine & 0xFF) << 31) | cb_db_op,
        size_units,
        0,
        base_units,
        0,
        poll / 40,
        gcr,
    ])
}

pub extern "sysv64" fn cb_release_mem(
    cb: usize,
    action: u32,
    gcr: u32,
    dest: u32,
    cache_policy: u32,
    dest_addr: u64,
    data_sel: u64,
    data: u64,
    gds_off: u64,
    gds_size: u64,
    interrupt: u64,
    int_ctx: u64,
) -> usize {
    let action = action & 0xFF;
    let gcr = gcr & 0xFFFF;
    let dest = dest & 0xFF;
    let cache_policy = cache_policy & 0xFF;
    let data_sel = (data_sel & 0xFF) as u32;
    let interrupt = (interrupt & 0xFF) as u32;
    let _ = (dest, gds_off, gds_size);
    if cb == 0 {
        return 0;
    }
    let addr = if interrupt == 4 { 0 } else { dest_addr };
    let val = if interrupt == 4 { 0 } else { data };
    emit(cb, &[
        pm4(8, OP_NOP, R_RELEASE_MEM),
        action | (cache_policy << 8),
        gcr | (data_sel << 16) | (interrupt << 24),
        addr as u32,
        (addr >> 32) as u32,
        val as u32,
        (val >> 32) as u32,
        (int_ctx as u32) & 0x07FF_FFFF,
    ])
}

pub extern "sysv64" fn dcb_write_data(
    cb: usize,
    dest: u32,
    cache_policy: u32,
    dest_addr: u64,
    data_ptr: *const u32,
    dword_count: u32,
    increment: u64,
    confirm: u64,
) -> usize {
    if cb == 0 || data_ptr.is_null() || dword_count > 0x3FFD {
        return 0;
    }
    let total = dword_count + 4;
    let dst = alloc_dwords(cb, total);
    if dst == 0 {
        return 0;
    }
    let ctrl = (dest & 0xFF)
        | ((cache_policy & 0xFF) << 8)
        | (((increment as u32) & 0xFF) << 16)
        | (((confirm as u32) & 0xFF) << 24);
    unsafe {
        let p = dst as *mut u32;
        *p = pm4(total, OP_NOP, R_WRITE_DATA);
        *p.add(1) = ctrl;
        *p.add(2) = dest_addr as u32;
        *p.add(3) = (dest_addr >> 32) as u32;
        std::ptr::copy_nonoverlapping(data_ptr, p.add(4), dword_count as usize);
    }
    dst
}

pub extern "sysv64" fn dcb_wait_reg_mem(
    cb: usize,
    size: u32,
    cmp_fn: u32,
    op: u32,
    cache_policy: u32,
    addr: u64,
    reference: u64,
    mask: u64,
    poll: u32,
) -> usize {
    let size = size & 0xFF;
    let cmp_fn = cmp_fn & 0xFF;
    let op = op & 0xFF;
    let cache_policy = cache_policy & 0xFF;
    if cb == 0 || size > 1 {
        return 0;
    }
    let pkt_dw = if size == 0 { 7u32 } else { 9u32 };
    let sub = if size == 0 { R_WAIT_MEM32 } else { R_WAIT_MEM64 };
    let poll_enc = (poll >> 4).min(0xFFFF);
    let dst = alloc_dwords(cb, pkt_dw);
    if dst == 0 {
        return 0;
    }
    unsafe {
        let p = dst as *mut u32;
        *p = pm4(pkt_dw, OP_NOP, sub);
        *p.add(1) = (addr as u32) & if size == 0 { !3 } else { !7 };
        *p.add(2) = ((addr >> 32) as u32) & 0x3_FFFF;
        *p.add(3) = mask as u32;
        if size == 0 {
            let ctrl = 0x10 | (cmp_fn & 7) | ((op & 3) << 8) | ((op & 0xC) << 4) | ((cache_policy & 3) << 25);
            *p.add(4) = reference as u32;
            *p.add(5) = ctrl;
            *p.add(6) = poll_enc;
        } else {
            let ctrl = 0x10 | (cmp_fn & 7) | ((op & 1) << 8) | ((op & 6) << 5) | ((cache_policy & 3) << 25);
            *p.add(4) = (mask >> 32) as u32;
            *p.add(5) = reference as u32;
            *p.add(6) = (reference >> 32) as u32;
            *p.add(7) = ctrl;
            *p.add(8) = poll_enc;
        }
    }
    dst
}

pub extern "sysv64" fn dcb_copy_data(
    cb: usize,
    dst_sel: u32,
    dst_pol: u32,
    dst_addr: u64,
    src_sel: u32,
    src_pol: u32,
    src_val: u64,
    elem_size: u64,
    confirm: u64,
) -> usize {
    if cb == 0 {
        return 0;
    }
    let ds = dst_sel & 0xFF;
    let ss = src_sel & 0xFF;
    let ctrl = ((ss >> 1) & 0xF)
        | (((ds >> 1) & 0xF) << 8)
        | ((ss & 1) << 30)
        | ((src_pol & 3) << 13)
        | (((elem_size as u32) & 1) << 16)
        | (((confirm as u32) & 1) << 20)
        | ((dst_pol & 3) << 25);
    emit(cb, &[
        pm4(6, OP_COPY_DATA, 0),
        ctrl,
        src_val as u32,
        (src_val >> 32) as u32,
        dst_addr as u32,
        (dst_addr >> 32) as u32,
    ])
}

pub extern "sysv64" fn dcb_set_predication(
    cb: usize, cond: u32, op: u32, wait_op: u32, addr: u64,
) -> usize {
    let flags = ((cond & 1) << 8) | ((wait_op & 1) << 12) | ((op & 7) << 16);
    emit(cb, &[pm4(4, OP_SET_PREDICATION, R_ZERO), flags, (addr as u32) & !0xF, (addr >> 32) as u32])
}

// ── Events & flip packets ─────────────────────────────────────────────

pub extern "sysv64" fn dcb_event_write(cb: usize, event_type: u32, addr: u64) -> usize {
    let et = event_type & 0xFF;
    if cb == 0 || et > 0x3F {
        return 0;
    }
    let has_addr = et == 0x38 || et == 0x39;
    let idx = if has_addr { 1 } else if matches!(et, 0x07 | 0x0F | 0x10) { 4 } else { 0 };
    if has_addr {
        emit(cb, &[pm4(4, OP_EVENT_WRITE, 0), et | (idx << 8), (addr as u32) & !7, (addr >> 32) as u32])
    } else {
        emit(cb, &[pm4(2, OP_EVENT_WRITE, 0), et | (idx << 8)])
    }
}

pub extern "sysv64" fn dcb_wait_until_safe_for_rendering(cb: usize, handle: u32, index: u32) -> usize {
    emit(cb, &[pm4(7, OP_NOP, R_WAIT_FLIP_DONE), handle, index, 0, 0, 0, 0])
}

pub extern "sysv64" fn dcb_set_flip(cb: usize, handle: u32, index: i32, mode: u32, arg: u64) -> usize {
    emit(cb, &[
        pm4(6, OP_NOP, R_FLIP),
        handle,
        index as u32,
        mode,
        arg as u32,
        (arg >> 32) as u32,
    ])
}

pub extern "sysv64" fn dcb_context_state_op(cb: usize, op: u32) -> usize {
    if cb == 0 || op > 3 {
        return 0;
    }
    emit(cb, &[pm4(5, OP_NOP, R_CONTEXT_STATE), op, 0, 0, 0])
}

// ── Register packets ──────────────────────────────────────────────────

pub extern "sysv64" fn cb_set_sh_reg_range_direct_size(count: u32) -> usize {
    if count == 0 || count > 0x3FFE {
        0
    } else {
        ((count + 2) as usize) * 4
    }
}

pub extern "sysv64" fn cb_set_sh_reg_range_direct(
    cb: usize, offset: u32, values: *const u32, count: u32,
) -> usize {
    if cb == 0 || offset == 0 || offset > 0x3FF || count == 0 || count > 0x3FFE {
        return 0;
    }
    let total = count + 2;
    let dst = alloc_dwords(cb, total);
    if dst == 0 {
        return 0;
    }
    unsafe {
        let p = dst as *mut u32;
        *p = pm4(total, OP_SET_SH_REG, 0);
        *p.add(1) = offset;
        if !values.is_null() {
            std::ptr::copy_nonoverlapping(values, p.add(2), count as usize);
        } else {
            std::ptr::write_bytes(p.add(2), 0, (count as usize) * 4);
        }
    }
    dst
}

fn emit_regs_indirect(cb: usize, op: u32, regs: u64, count: u32) -> usize {
    emit(cb, &[
        pm4(5, op, 0),
        (regs as u32) & !3,
        (regs >> 32) as u32,
        0x8000_0000,
        count & 0x3FFF,
    ])
}

pub extern "sysv64" fn dcb_set_cx_regs_indirect(cb: usize, regs: u64, count: u32) -> usize {
    emit_regs_indirect(cb, OP_SET_CX_REG_INDIRECT, regs, count)
}

pub extern "sysv64" fn dcb_set_sh_regs_indirect(cb: usize, regs: u64, count: u32) -> usize {
    emit_regs_indirect(cb, OP_SET_SH_REG_INDIRECT, regs, count)
}

pub extern "sysv64" fn dcb_set_uc_regs_indirect(cb: usize, regs: u64, count: u32) -> usize {
    emit_regs_indirect(cb, OP_SET_UC_REG_INDIRECT, regs, count)
}

// ── Packet patching ───────────────────────────────────────────────────

fn patch_indirect_addr(cmd: usize, addr: u64) -> i32 {
    if cmd == 0 {
        return INVALID_ARG;
    }
    unsafe {
        let hdr = *(cmd as *const u32);
        let op = pm4_op(hdr);
        if matches!(op, OP_SET_CX_REG_INDIRECT | OP_SET_SH_REG_INDIRECT | OP_SET_UC_REG_INDIRECT) && pm4_len(hdr) == 5 {
            *((cmd + 4) as *mut u32) = (addr as u32) & !3;
            *((cmd + 8) as *mut u32) = (addr >> 32) as u32;
            return 0;
        }
        if op == OP_NOP && matches!(pm4_sub(hdr), R_CX_REGS_INDIRECT | R_SH_REGS_INDIRECT | R_UC_REGS_INDIRECT) && pm4_len(hdr) == 4 {
            *((cmd + 8) as *mut u64) = addr;
            return 0;
        }
    }
    INVALID_ARG
}

fn patch_indirect_add_regs(cmd: usize, add: u32) -> i32 {
    if cmd == 0 {
        return INVALID_ARG;
    }
    unsafe {
        let hdr = *(cmd as *const u32);
        let op = pm4_op(hdr);
        if matches!(op, OP_SET_CX_REG_INDIRECT | OP_SET_SH_REG_INDIRECT | OP_SET_UC_REG_INDIRECT) && pm4_len(hdr) == 5 {
            let p = (cmd + 16) as *mut u32;
            let cur = *p;
            *p = (cur & !0x3FFF) | ((cur + add) & 0x3FFF);
            return 0;
        }
        if op == OP_NOP && matches!(pm4_sub(hdr), R_CX_REGS_INDIRECT | R_SH_REGS_INDIRECT | R_UC_REGS_INDIRECT) && pm4_len(hdr) == 4 {
            let p = (cmd + 4) as *mut u32;
            *p = (*p).wrapping_add(add);
            return 0;
        }
    }
    INVALID_ARG
}

pub extern "sysv64" fn patch_reg_indirect_addr(cmd: usize, addr: u64) -> i32 {
    patch_indirect_addr(cmd, addr)
}

pub extern "sysv64" fn patch_reg_indirect_add(cmd: usize, add: u32) -> i32 {
    patch_indirect_add_regs(cmd, add)
}

fn is_write_data_pkt(addr: usize) -> bool {
    if addr < 0x10000 {
        return false;
    }
    let hdr = unsafe { *(addr as *const u32) };
    (hdr >> 30) == 3 && (pm4_op(hdr) == OP_WRITE_DATA || (pm4_op(hdr) == OP_NOP && pm4_sub(hdr) == R_WRITE_DATA))
}

pub extern "sysv64" fn patch_write_data_addr(a: u64, b: u64) -> i32 {
    if is_write_data_pkt(a as usize) {
        unsafe { *((a as usize + 8) as *mut u64) = b };
        0
    } else if is_write_data_pkt(b as usize) {
        unsafe { *((b as usize + 8) as *mut u64) = a };
        0
    } else if b != 0 {
        unsafe { *(b as *mut u64) = a };
        0
    } else {
        INVALID_ARG
    }
}

pub extern "sysv64" fn patch_wait_reg_mem_addr(cmd: usize, addr: u64) -> i32 {
    if cmd == 0 {
        return INVALID_ARG;
    }
    unsafe {
        let hdr = *(cmd as *const u32);
        let op = pm4_op(hdr);
        let sub = pm4_sub(hdr);
        if op == OP_WAIT_REG_MEM {
            *((cmd + 8) as *mut u64) = addr;
            return 0;
        }
        if op == OP_NOP && (sub == R_WAIT_MEM32 || sub == R_WAIT_MEM64) {
            let mask = if sub == R_WAIT_MEM32 { !3u32 } else { !7u32 };
            *((cmd + 4) as *mut u32) = (addr as u32) & mask;
            *((cmd + 8) as *mut u32) = ((addr >> 32) as u32) & 0x3_FFFF;
            return 0;
        }
    }
    INVALID_ARG
}

pub extern "sysv64" fn patch_eop_addr(cmd: usize, addr: u64) -> i32 {
    if cmd == 0 {
        return INVALID_ARG;
    }
    unsafe {
        let hdr = *(cmd as *const u32);
        if pm4_op(hdr) == OP_NOP && pm4_sub(hdr) == R_RELEASE_MEM {
            *((cmd + 12) as *mut u64) = addr;
            return 0;
        }
    }
    INVALID_ARG
}

// ── Draw packets & shader state helpers ───────────────────────────────

pub extern "sysv64" fn dcb_set_index_count(cb: usize, count: u32) -> usize {
    emit(cb, &[pm4(2, OP_NOP, R_INDEX_COUNT), count])
}

pub extern "sysv64" fn dcb_set_num_instances(cb: usize, count: u32) -> usize {
    emit(cb, &[pm4(2, OP_NUM_INSTANCES, 0), count])
}

pub extern "sysv64" fn dcb_set_index_buffer(cb: usize, addr: u64, count: u32) -> usize {
    emit(cb, &[
        pm4(3, OP_INDEX_BASE, 0),
        addr as u32,
        (addr >> 32) as u32,
        pm4(2, OP_INDEX_BUFFER_SIZE, 0),
        count,
    ])
}

pub extern "sysv64" fn dcb_set_index_size_ex(cb: usize, size: u32, cache: u32, inst_id: u32) -> usize {
    const VGT_INDEX_TYPE: u32 = 0x0242;
    let val = 0x400 | (size & 3) | ((cache & 3) << 6) | ((inst_id & 1) << 14);
    emit(cb, &[pm4(3, OP_SET_UC_REG_INDEX, 0), 0x2000_0000 | VGT_INDEX_TYPE, val])
}

pub extern "sysv64" fn dcb_draw_index_auto(cb: usize, count: u32, _modifier: u64) -> usize {
    emit(cb, &[pm4(7, OP_NOP, R_DRAW_INDEX_AUTO), count, 0, 0, 0, 0, 0])
}

pub extern "sysv64" fn dcb_draw_index_offset(cb: usize, offset: u32, count: u32, modifier: u64) -> usize {
    let init = if (modifier & (1 << 32)) != 0 { 0 } else { ((modifier as u32) >> 3) & 0x20 };
    emit(cb, &[pm4(5, OP_DRAW_INDEX_OFFSET_2, 0), count.max(1), offset, count, init])
}

fn emit_draw_indirect_multi(
    cb: usize, op: u32, data_off: u32, count_from_mem: u32, draw_count: u32, count_addr: u64, stride: u32, modifier: u64,
) -> usize {
    if cb == 0 || count_from_mem > 1 {
        return 0;
    }
    let init = if (modifier & (1 << 32)) != 0 { 2 } else { (((modifier as u32) >> 3) & 0x20) | 2 };
    let ca = count_addr & !3;
    emit(cb, &[
        pm4(10, op, 0),
        data_off,
        0x280,
        0x280,
        count_from_mem << 30,
        draw_count,
        ca as u32,
        (ca >> 32) as u32,
        stride,
        init,
    ])
}

pub extern "sysv64" fn dcb_draw_indirect_multi(
    cb: usize, data_off: u32, cfm: u32, count: u32, count_addr: u64, stride: u32, modifier: u64,
) -> usize {
    emit_draw_indirect_multi(cb, OP_DRAW_INDIRECT_MULTI, data_off, cfm, count, count_addr, stride, modifier)
}

pub extern "sysv64" fn dcb_draw_index_indirect_multi(
    cb: usize, data_off: u32, cfm: u32, count: u32, count_addr: u64, stride: u32, modifier: u64,
) -> usize {
    emit_draw_indirect_multi(cb, OP_DRAW_INDEX_INDIRECT_MULTI, data_off, cfm, count, count_addr, stride, modifier)
}

pub extern "sysv64" fn create_prim_state(
    cx_regs: *mut u32, uc_regs: *mut u32, _hs: usize, gs: usize, prim_type: u32,
) -> i32 {
    if cx_regs.is_null() && uc_regs.is_null() {
        return 0;
    }
    if gs == 0 {
        return INVALID_ARG;
    }
    unsafe {
        if !cx_regs.is_null() {
            std::ptr::write_bytes(cx_regs, 0, 4);
        }
        if !uc_regs.is_null() {
            std::ptr::write_bytes(uc_regs, 0, 6);
            *uc_regs.add(4) = 0xC242; // VGT_PRIMITIVE_TYPE
            *uc_regs.add(5) = prim_type;
        }
    }
    0
}

pub extern "sysv64" fn create_interpolant_mapping(regs: *mut u32, _gs: usize, _ps: usize) -> i32 {
    if regs.is_null() {
        return INVALID_ARG;
    }
    unsafe {
        for i in 0..32u32 {
            *regs.add((i * 2) as usize) = 0x0191 + i; // SPI_PS_INPUT_CNTL_0 + i (CX-relative)
            *regs.add((i * 2 + 1) as usize) = i;
        }
    }
    0
}

// ── DCB submission & synchronous PM4 command processor ────────────────

#[repr(C)]
pub struct SubmitPacket {
    addr: u64,
    dwords: u32,
}

#[derive(Default)]
struct GpuState {
    cx: BTreeMap<u32, u32>,
    sh: BTreeMap<u32, u32>,
    uc: BTreeMap<u32, u32>,
    clear_rgba: [f32; 4],
    bound_tex: Option<(usize, u32, u32)>,
    index_base: u64,
    index_count: u32,
    cs_cache: BTreeMap<usize, (bool, bool, usize)>,
}

static GPU: Mutex<GpuState> = Mutex::new(GpuState {
    cx: BTreeMap::new(),
    sh: BTreeMap::new(),
    uc: BTreeMap::new(),
    clear_rgba: [0.0, 0.0, 0.0, 1.0],
    bound_tex: None,
    index_base: 0,
    index_count: 0,
    cs_cache: BTreeMap::new(),
});

fn is_committed(addr: usize, len: usize) -> bool {
    if addr < 0x1_0000_0000 || addr > 0x7FFF_FFFF_FFFF || len == 0 {
        return false;
    }
    #[cfg(windows)]
    unsafe {
        let mut mbi: windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        windows_sys::Win32::System::Memory::VirtualQuery(
            addr as *const std::ffi::c_void,
            &mut mbi,
            std::mem::size_of::<windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION>(),
        ) != 0
            && mbi.State == windows_sys::Win32::System::Memory::MEM_COMMIT
            && (mbi.BaseAddress as usize + mbi.RegionSize) >= addr.saturating_add(len)
    }
    #[cfg(not(windows))]
    {
        let _ = (addr, len);
        false
    }
}

fn decode_vsharp(sh: &BTreeMap<u32, u32>, base_reg: u32) -> Option<(usize, usize)> {
    let w0 = *sh.get(&base_reg)?;
    let w1 = *sh.get(&(base_reg + 1))?;
    let w2 = *sh.get(&(base_reg + 2))?;
    let addr = (w0 as u64) | (((w1 & 0xFFFF) as u64) << 32);
    let stride = ((w1 >> 16) & 0x3FFF) as usize;
    let num_records = w2 as usize;
    let bytes = if stride > 0 { stride.saturating_mul(num_records) } else { num_records };
    if addr >= 0x1_0000_0000 && bytes > 0 && bytes <= 64 * 1024 * 1024 {
        Some((addr as usize, bytes))
    } else {
        None
    }
}
static SUBMIT_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_DRAWS: AtomicU64 = AtomicU64::new(0);
static LAST_DISPATCHES: AtomicU64 = AtomicU64::new(0);
static GPU_TIMESTAMP: AtomicU64 = AtomicU64::new(1);

pub fn stats() -> (u64, u64, u64) {
    (
        SUBMIT_COUNT.load(Ordering::Relaxed),
        LAST_DRAWS.load(Ordering::Relaxed),
        LAST_DISPATCHES.load(Ordering::Relaxed),
    )
}

fn apply_indirect_regs(bank: &mut BTreeMap<u32, u32>, table_addr: u64, count: u32, trace: bool, name: &str) {
    if table_addr == 0 || count == 0 || count > 0x3FFF {
        return;
    }
    let pairs = unsafe { std::slice::from_raw_parts(table_addr as *const [u32; 2], count as usize) };
    for &[reg, val] in pairs {
        bank.insert(reg, val);
        if trace {
            eprintln!("[orbis] GPU:   {name}[{reg:#010x}] = {val:#010x}");
        }
    }
}

fn execute_dma_data(dst_sel: u32, src_sel: u32, dst: u64, src: u64, size: usize) {
    if dst == 0 || size == 0 || size > 128 * 1024 * 1024 {
        return;
    }
    // src_sel == 2 means constant 32-bit fill (clear); src_sel == 0/5 means memory copy.
    if matches!(dst_sel, 0 | 5) {
        if src_sel == 2 {
            let fill = src as u32;
            unsafe {
                let words = size / 4;
                let p = dst as *mut u32;
                for k in 0..words {
                    *p.add(k) = fill;
                }
                let rem = size % 4;
                if rem > 0 {
                    let b = fill.to_le_bytes();
                    std::ptr::copy_nonoverlapping(b.as_ptr(), (dst as *mut u8).add(words * 4), rem);
                }
            }
        } else if src != 0 && matches!(src_sel, 0 | 5) {
            unsafe {
                std::ptr::copy(src as *const u8, dst as *mut u8, size);
            }
        }
    }
}

fn execute_stream(
    gpu: &mut GpuState,
    addr: usize,
    dwords: usize,
    depth: u32,
    trace: bool,
    stats: &mut BTreeMap<u32, usize>,
) {
    if addr == 0 || dwords == 0 || depth > 8 {
        return;
    }
    let stream = unsafe { std::slice::from_raw_parts(addr as *const u32, dwords) };
    let mut i = 0usize;
    while i < stream.len() {
        let hdr = stream[i];
        let ptype = hdr >> 30;
        if ptype == 2 || hdr == 0 {
            i += 1;
            continue;
        }
        if ptype != 3 {
            i += 1;
            continue;
        }
        let len = pm4_len(hdr) as usize;
        if len < 2 || i + len > stream.len() {
            break;
        }
        let op = pm4_op(hdr);
        let sub = pm4_sub(hdr);
        let key = if op == OP_NOP { (op << 8) | sub } else { op << 8 };
        *stats.entry(key).or_insert(0) += 1;
        let w = &stream[i..i + len];

        match op {
            OP_INDIRECT_BUFFER if len >= 4 => {
                let ib_addr = ((w[1] & !3) as u64) | ((w[2] as u64) << 32);
                let ib_dw = (w[3] & 0x000F_FFFF) as usize;
                execute_stream(gpu, ib_addr as usize, ib_dw, depth + 1, trace, stats);
            }
            OP_SET_CX_REG_INDIRECT if len >= 5 => {
                let tbl = ((w[1] & !3) as u64) | ((w[2] as u64) << 32);
                apply_indirect_regs(&mut gpu.cx, tbl, w[4] & 0x3FFF, trace, "cx");
            }
            OP_SET_SH_REG_INDIRECT if len >= 5 => {
                let tbl = ((w[1] & !3) as u64) | ((w[2] as u64) << 32);
                apply_indirect_regs(&mut gpu.sh, tbl, w[4] & 0x3FFF, trace, "sh");
            }
            OP_SET_UC_REG_INDIRECT if len >= 5 => {
                let tbl = ((w[1] & !3) as u64) | ((w[2] as u64) << 32);
                apply_indirect_regs(&mut gpu.uc, tbl, w[4] & 0x3FFF, trace, "uc");
            }
            OP_SET_CONTEXT_REG if len >= 3 => {
                let base = w[1] & 0xFFFF;
                for (k, &v) in w[2..].iter().enumerate() {
                    gpu.cx.insert(base + k as u32, v);
                }
            }
            OP_INDEX_BASE if len >= 3 => {
                gpu.index_base = (w[1] as u64) | ((w[2] as u64) << 32);
            }
            OP_INDEX_BUFFER_SIZE if len >= 2 => {
                gpu.index_count = w[1];
            }
            OP_SET_SH_REG if len >= 3 => {
                let base = w[1] & 0xFFFF;
                let vals = &w[2..];
                for (k, &v) in vals.iter().enumerate() {
                    gpu.sh.insert(base + k as u32, v);
                }
                // Detect 8-dword T# image descriptor or 4-dword V# constant buffer bound at PS user-data 0x0C.
                if base == 0x0C && vals.len() >= 8 {
                    let addr = ((vals[0] as u64) | (((vals[1] & 0xFF) as u64) << 32)) << 8;
                    let width = ((vals[1] >> 30) | ((vals[2] & 0xFFF) << 2)) + 1;
                    let height = ((vals[2] >> 14) & 0x3FFF) + 1;
                    if addr >= 0x1_0000_0000 && width <= 8192 && height <= 8192 {
                        gpu.bound_tex = Some((addr as usize, width, height));
                        if trace {
                            let sample = unsafe { std::slice::from_raw_parts(addr as *const u32, 64) };
                            let nz = sample.iter().filter(|&&x| x != 0).count();
                            eprintln!("[orbis] GPU:   bind T# @{addr:#x} {width}x{height} fmt={:#x} nz64={nz}", (vals[1] >> 20) & 0x3FF);
                        }
                    }
                } else if base == 0x0C && vals.len() == 4 && vals[2] <= 64 && (vals[1] & 0xFFFF) != 0 {
                    let ptr = (vals[0] as u64) | (((vals[1] & 0xFFFF) as u64) << 32);
                    if ptr >= 0x1_0000_0000 && ptr < 0x7FFF_FFFF_FFFF {
                        #[cfg(windows)]
                        let readable = unsafe {
                            let mut mbi: windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION = std::mem::zeroed();
                            windows_sys::Win32::System::Memory::VirtualQuery(
                                ptr as *const std::ffi::c_void,
                                &mut mbi,
                                std::mem::size_of::<windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION>(),
                            ) != 0
                                && mbi.State == windows_sys::Win32::System::Memory::MEM_COMMIT
                        };
                        #[cfg(not(windows))]
                        let readable = false;
                        if readable {
                            let f = unsafe { std::slice::from_raw_parts(ptr as *const f32, 4) };
                            if f.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0) && f[3] > 0.0 {
                                gpu.clear_rgba = [f[0], f[1], f[2], f[3]];
                                crate::video::set_clear_color(gpu.clear_rgba);
                            }
                        }
                    }
                }
            }
            OP_SET_UCONFIG_REG | OP_SET_UC_REG_INDEX if len >= 3 => {
                let base = w[1] & 0xFFFF;
                for (k, &v) in w[2..].iter().enumerate() {
                    gpu.uc.insert(base + k as u32, v);
                }
            }
            OP_DMA_DATA if len >= 7 => {
                let header = w[1];
                let dst_sel = (header >> 20) & 0xF;
                let src_sel = (header >> 29) & 0x7;
                let src = (w[2] as u64) | ((w[3] as u64) << 32);
                let dst = (w[4] as u64) | ((w[5] as u64) << 32);
                let size = (w[6] & 0x03FF_FFFF) as usize;
                execute_dma_data(dst_sel, src_sel, dst, src, size);
            }
            OP_WRITE_DATA if len >= 4 => {
                let dst_sel = (w[1] >> 8) & 0xF;
                let no_inc = (w[1] >> 16) & 1 != 0;
                let dst = ((w[2] & !3) as u64) | ((w[3] as u64) << 32);
                if dst != 0 && matches!(dst_sel, 2 | 5) {
                    unsafe {
                        let p = dst as *mut u32;
                        for (k, &val) in w[4..].iter().enumerate() {
                            *p.add(if no_inc { 0 } else { k }) = val;
                        }
                    }
                }
            }
            OP_COPY_DATA if len >= 6 => {
                let ctrl = w[1];
                let src_sel = ctrl & 0xF;
                let dst_sel = (ctrl >> 8) & 0xF;
                let is_64 = (ctrl >> 16) & 1 != 0;
                let src = (w[2] as u64) | ((w[3] as u64) << 32);
                let dst = (w[4] as u64) | ((w[5] as u64) << 32);
                if dst != 0 && matches!(dst_sel, 2 | 5) {
                    let val: u64 = if src_sel == 5 {
                        src
                    } else if src != 0 && matches!(src_sel, 2 | 5) {
                        unsafe { if is_64 { *(src as *const u64) } else { *(src as *const u32) as u64 } }
                    } else {
                        0
                    };
                    unsafe {
                        if is_64 {
                            *(dst as *mut u64) = val;
                        } else {
                            *(dst as *mut u32) = val as u32;
                        }
                    }
                }
            }
            OP_EVENT_WRITE if len >= 2 => {
                if trace {
                    eprintln!("[orbis] GPU:   event_write type={:#x} len={len}", w[1] & 0x3F);
                }
            }
            OP_DRAW_INDEX_OFFSET_2 if len >= 5 => {
                if trace {
                    let vs_ud: Vec<u32> = (0x8C..0x94).map(|r| *gpu.sh.get(&r).unwrap_or(&0)).collect();
                    let ps_ud: Vec<u32> = (0x0C..0x14).map(|r| *gpu.sh.get(&r).unwrap_or(&0)).collect();
                    eprintln!(
                        "[orbis] GPU:   DRAW_INDEX_OFFSET_2 off={} count={} ib={:#x}({}) vs={vs_ud:x?} ps={ps_ud:x?}",
                        w[2], w[3], gpu.index_base, gpu.index_count
                    );
                }
            }
            OP_DISPATCH_DIRECT if len >= 5 => {
                let cs_lo = *gpu.sh.get(&0x20C).unwrap_or(&0) as u64;
                let cs_hi = *gpu.sh.get(&0x20D).unwrap_or(&0) as u64;
                let cs_addr = ((cs_lo | (cs_hi << 32)) << 8) as usize;
                if cs_addr >= 0x1_0000_0000 && is_committed(cs_addr, 256) {
                    let (has_load, has_store, _) = *gpu.cs_cache.entry(cs_addr).or_insert_with(|| {
                        let words = unsafe { std::slice::from_raw_parts(cs_addr as *const u32, 64) };
                        let d = crate::shader::decode_rdna2(words);
                        (d.has_buffer_load, d.has_buffer_store, d.ops.len())
                    });
                    if has_store && !has_load {
                        // Compute buffer clear (SHADER #1 pattern: s[0:3] = V#, s[4:7] = clear RGBA/u32x4)
                        if let Some((dst, bytes)) = decode_vsharp(&gpu.sh, 0x240) {
                            if is_committed(dst, bytes) {
                                let fill = [
                                    *gpu.sh.get(&0x244).unwrap_or(&0),
                                    *gpu.sh.get(&0x245).unwrap_or(&0),
                                    *gpu.sh.get(&0x246).unwrap_or(&0),
                                    *gpu.sh.get(&0x247).unwrap_or(&0),
                                ];
                                let quads = bytes / 16;
                                unsafe {
                                    let p = dst as *mut [u32; 4];
                                    for k in 0..quads {
                                        *p.add(k) = fill;
                                    }
                                }
                            }
                        }
                    } else if has_load && has_store {
                        // Compute buffer copy (SHADER #5 pattern: s[0:3] = src V#, s[4:7] = dst V#)
                        if let (Some((src, sbytes)), Some((dst, dbytes))) =
                            (decode_vsharp(&gpu.sh, 0x240), decode_vsharp(&gpu.sh, 0x244))
                        {
                            let n = sbytes.min(dbytes);
                            if is_committed(src, n) && is_committed(dst, n) {
                                unsafe {
                                    std::ptr::copy(src as *const u8, dst as *mut u8, n);
                                }
                            }
                        }
                    }
                }
                if trace {
                    eprintln!("[orbis] GPU:   DISPATCH ({}, {}, {}) cs={cs_addr:#x}", w[1], w[2], w[3]);
                }
            }
            OP_NOP => match sub {
                R_DRAW_INDEX_AUTO if len >= 2 => {
                    if trace {
                        eprintln!("[orbis] GPU:   DRAW_INDEX_AUTO count={}", w[1]);
                    }
                }
                R_DMA_DATA if len >= 7 => {
                    let dst = (w[1] as u64) | ((w[2] as u64) << 32);
                    let src = (w[3] as u64) | ((w[4] as u64) << 32);
                    let size = (w[5] & 0x03FF_FFFF) as usize;
                    let ctrl = w[6];
                    let src_sel = (ctrl >> 29) & 0x7;
                    let dst_sel = (ctrl >> 20) & 0xF;
                    execute_dma_data(dst_sel, src_sel, dst, src, size);
                }
                R_WRITE_DATA if len >= 4 => {
                    let ctrl = w[1];
                    let dest = ctrl & 0xFF;
                    let inc = (ctrl >> 16) & 0xFF;
                    let dst = (w[2] as u64) | ((w[3] as u64) << 32);
                    if trace {
                        eprintln!("[orbis] GPU:   write_data dest={dest} inc={inc} dst={dst:#x} words={:?}", &w[4..]);
                    }
                    if dst >= 0x10000 {
                        unsafe {
                            let p = dst as *mut u32;
                            for (k, &val) in w[4..].iter().enumerate() {
                                *p.add(if inc == 0 { k } else { 0 }) = val;
                            }
                        }
                    }
                }
                R_RELEASE_MEM if len >= 8 => {
                    let data_sel = (w[2] >> 16) & 0xFF;
                    let interrupt = (w[2] >> 24) & 0xFF;
                    let dst = (w[3] as u64) | ((w[4] as u64) << 32);
                    let data = (w[5] as u64) | ((w[6] as u64) << 32);
                    let int_ctx = w[7] as u64;
                    if trace {
                        eprintln!("[orbis] GPU:   release_mem sel={data_sel} int={interrupt} dst={dst:#x} data={data:#x} ctx={int_ctx:#x}");
                    }
                    if dst != 0 {
                        unsafe {
                            match data_sel {
                                1 => *(dst as *mut u32) = data as u32,
                                2 => *(dst as *mut u64) = data,
                                3 => *(dst as *mut u64) = GPU_TIMESTAMP.fetch_add(1000, Ordering::SeqCst),
                                _ => {
                                    if interrupt == 5 {
                                        *(dst as *mut u32) = data as u32;
                                    } else if interrupt == 6 {
                                        *(dst as *mut u64) = data;
                                    }
                                }
                            }
                        }
                    }
                    if interrupt != 0 {
                        crate::equeue::trigger_gpu(int_ctx);
                    }
                }
                R_FLIP if len >= 6 => {
                    let handle = w[1] as i32;
                    let index = w[2] as i32;
                    let mode = w[3] as i32;
                    let arg = ((w[4] as u64) | ((w[5] as u64) << 32)) as i64;
                    if trace {
                        eprintln!("[orbis] GPU:   flip handle={handle} index={index} mode={mode} arg={arg:#x}");
                    }
                    crate::video::submit_flip(handle, index, mode, arg);
                }
                _ => {}
            },
            _ => {}
        }
        i += len;
    }
}

pub extern "sysv64" fn submit_dcb(pkt: *const SubmitPacket) -> i32 {
    if pkt.is_null() {
        return INVALID_ARG;
    }
    let (addr, dwords) = unsafe { ((*pkt).addr as usize, (*pkt).dwords as usize) };
    if dwords > 0xF_FFFF {
        return INVALID_ARG;
    }
    let mut gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let n = SUBMIT_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    let trace = (std::env::var_os("ORBIS_TRACE_GPU").is_some() && n <= 3)
        || (std::env::var_os("ORBIS_DUMP_DRAW").is_some() && n == 180);
    let mut stats = BTreeMap::new();
    execute_stream(&mut gpu, addr, dwords, 0, trace, &mut stats);
    let draws = (*stats.get(&((OP_NOP << 8) | R_DRAW_INDEX_AUTO)).unwrap_or(&0)
        + *stats.get(&(OP_DRAW_INDEX_OFFSET_2 << 8)).unwrap_or(&0)
        + *stats.get(&(OP_DRAW_INDIRECT_MULTI << 8)).unwrap_or(&0)
        + *stats.get(&(OP_DRAW_INDEX_INDIRECT_MULTI << 8)).unwrap_or(&0)) as u64;
    let dispatches = (*stats.get(&(OP_DISPATCH_DIRECT << 8)).unwrap_or(&0)
        + *stats.get(&(OP_DISPATCH_INDIRECT << 8)).unwrap_or(&0)) as u64;
    LAST_DRAWS.store(draws, Ordering::Relaxed);
    LAST_DISPATCHES.store(dispatches, Ordering::Relaxed);
    crate::equeue::trigger_gpu(0);
    if n <= 5 || n % 60 == 0 || std::env::var_os("ORBIS_TRACE_GPU").is_some() {
        let summary: Vec<String> = stats.iter().map(|(k, v)| format!("{k:#06x}:{v}")).collect();
        let nsh = crate::gpu::SHADERS.lock().map(|s| s.len()).unwrap_or(0);
        eprintln!(
            "[orbis] GPU: submit #{n} ({dwords} dwords @ {addr:#x}, {nsh} shaders, clear={:.2?}, tex={:?}) [{}]",
            gpu.clear_rgba, gpu.bound_tex, summary.join(" ")
        );
    }
    0
}

pub extern "sysv64" fn suspend_point() -> i32 {
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("#LtTouSCZjHM", cb_nop as *const () as usize),                        // sceAgcCbNop
        ("#k3GhuSNmBLU", cb_dispatch as *const () as usize),                   // sceAgcCbDispatch
        ("#CtB+A9-VxO0", dcb_dispatch_indirect as *const () as usize),         // sceAgcDcbDispatchIndirect
        ("#RmaJwLtc8rY", dcb_set_base_indirect_args as *const () as usize),    // sceAgcDcbSetBaseIndirectArgs
        ("#V++UgBtQhn0", get_data_packet_payload_address as *const () as usize), // sceAgcGetDataPacketPayloadAddress
        ("#+kSrjIVxKFE", dcb_push_marker as *const () as usize),               // sceAgcDcbPushMarker
        ("#H7uZqCoNuWk", dcb_pop_marker as *const () as usize),                // sceAgcDcbPopMarker
        ("#57labkp+rSQ", dcb_acquire_mem as *const () as usize),               // sceAgcDcbAcquireMem
        ("#wr23dPKyWc0", cb_release_mem as *const () as usize),                // sceAgcCbReleaseMem
        ("#i1jyy49AjXU", dcb_write_data as *const () as usize),                // sceAgcDcbWriteData
        ("#VmW0Tdpy420", dcb_wait_reg_mem as *const () as usize),              // sceAgcDcbWaitRegMem
        ("#1rZSWUv1IRc", dcb_copy_data as *const () as usize),                 // sceAgcDcbCopyData
        ("#bbFueFP+J4k", dcb_set_predication as *const () as usize),           // sceAgcDcbSetPredication
        ("#aJf+j5yntiU", dcb_event_write as *const () as usize),               // sceAgcDcbEventWrite
        ("#MWiElSNE8j8", dcb_wait_until_safe_for_rendering as *const () as usize), // sceAgcDcbWaitUntilSafeForRendering
        ("#YUeqkyT7mEQ", dcb_set_flip as *const () as usize),                  // sceAgcDcbSetFlip
        ("#qj7QZpgr9Uw", dcb_context_state_op as *const () as usize),          // sceAgcDcbContextStateOp
        ("#n2fD4A+pb+g", cb_set_sh_reg_range_direct as *const () as usize),    // sceAgcCbSetShRegisterRangeDirect
        ("#bxGoVxpdSPQ", cb_set_sh_reg_range_direct_size as *const () as usize), // sceAgcCbSetShRegisterRangeDirectGetSize
        ("#ZvwO9euwYzc", dcb_set_cx_regs_indirect as *const () as usize),      // sceAgcDcbSetCxRegistersIndirect
        ("#-HOOCn0JY48", dcb_set_sh_regs_indirect as *const () as usize),      // sceAgcDcbSetShRegistersIndirect
        ("#hvUfkUIQcOE", dcb_set_uc_regs_indirect as *const () as usize),      // sceAgcDcbSetUcRegistersIndirect
        ("#vcmNN+AAXnY", patch_reg_indirect_addr as *const () as usize),       // sceAgcSetCxRegIndirectPatchSetAddress
        ("#Qrj4c+61z4A", patch_reg_indirect_addr as *const () as usize),       // sceAgcSetShRegIndirectPatchSetAddress
        ("#6lNcCp+fxi4", patch_reg_indirect_addr as *const () as usize),       // sceAgcSetUcRegIndirectPatchSetAddress
        ("#d-6uF9sZDIU", patch_reg_indirect_add as *const () as usize),        // sceAgcSetCxRegIndirectPatchAddRegisters
        ("#z2duB-hHQSM", patch_reg_indirect_add as *const () as usize),        // sceAgcSetShRegIndirectPatchAddRegisters
        ("#vRoArM9zaIk", patch_reg_indirect_add as *const () as usize),        // sceAgcSetUcRegIndirectPatchAddRegisters
        ("#fPSCdQxgpSw", patch_write_data_addr as *const () as usize),         // sceAgcWriteDataPatchSetAddressOrOffset
        ("#3KDcnM3lrcU", patch_wait_reg_mem_addr as *const () as usize),       // sceAgcWaitRegMemPatchAddress
        ("#0fWWK5uG9rQ", patch_eop_addr as *const () as usize),                // sceAgcQueueEndOfPipeActionPatchAddress
        ("#8N2tmT3jmC8", dcb_set_index_count as *const () as usize),           // sceAgcDcbSetIndexCount
        ("#tSBxhAPyytQ", dcb_set_num_instances as *const () as usize),         // sceAgcDcbSetNumInstances
        ("#l4fM9K-Lyks", dcb_set_index_buffer as *const () as usize),          // sceAgcDcbSetIndexBuffer
        ("#-KRzWekV120", dcb_set_index_size_ex as *const () as usize),         // sceAgcDcbSetIndexSizeEx
        ("#Yw0jKSqop+E", dcb_draw_index_auto as *const () as usize),           // sceAgcDcbDrawIndexAuto
        ("#B+aG9DUnTKA", dcb_draw_index_offset as *const () as usize),         // sceAgcDcbDrawIndexOffset
        ("#kUlvghKs-mA", dcb_draw_indirect_multi as *const () as usize),       // sceAgcDcbDrawIndirectMulti
        ("#ypVBz4uPKcQ", dcb_draw_index_indirect_multi as *const () as usize), // sceAgcDcbDrawIndexIndirectMulti
        ("#D9sr1xGUriE", create_prim_state as *const () as usize),             // sceAgcCreatePrimState
        ("#HV4j+E0MBHE", create_interpolant_mapping as *const () as usize),    // sceAgcCreateInterpolantMapping
        ("#UglJIZjGssM", submit_dcb as *const () as usize),                    // sceAgcDriverSubmitDcb
        ("#AhGvpITrf4M", submit_dcb as *const () as usize),                    // sceAgcDriverAgrSubmitDcb
        ("#h9z6+0hEydk", suspend_point as *const () as usize),                 // sceAgcSuspendPoint
    ]
}
