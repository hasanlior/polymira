//! PS5 AGC command-buffer builders and minimal synchronous PM4 command processor.
//!
//! Games build Direct Command Buffers (DCBs) using `libSceAgc` helpers and submit them via
//! `sceAgcDriverSubmitDcb`. Each builder allocates dwords from the guest's command-buffer
//! cursor, writes PM4 type-3 packets, and returns a pointer to the packet.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

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

pub extern "sysv64" fn create_interpolant_mapping(regs: *mut u32, gs: usize, ps: usize) -> i32 {
    if regs.is_null() {
        return INVALID_ARG;
    }
    unsafe {
        for i in 0..32u32 {
            *regs.add((i * 2) as usize) = 0x0191 + i; // SPI_PS_INPUT_CNTL_0 + i (CX-relative)
            *regs.add((i * 2 + 1) as usize) = i;
        }
        if is_committed(gs, 0x60) && is_committed(ps, 0x60) {
            let gs_tab = *((gs + 0x38) as *const usize);
            let gs_end = *((gs + 0x20) as *const usize);
            let ps_tab = *((ps + 0x30) as *const usize);
            let ps_end = *((ps + 0x20) as *const usize);
            let gs_n = gs_end.saturating_sub(gs_tab) / 4;
            let ps_n = ps_end.saturating_sub(ps_tab) / 4;
            if gs_n > 0 && gs_n <= 32 && ps_n > 0 && ps_n <= 32 && is_committed(gs_tab, gs_n * 4) && is_committed(ps_tab, ps_n * 4) {
                let ge = std::slice::from_raw_parts(gs_tab as *const u32, gs_n);
                let pe = std::slice::from_raw_parts(ps_tab as *const u32, ps_n);
                for (pi, &pw) in pe.iter().take_while(|&&w| w != 0).enumerate() {
                    let sem = pw & 0xFF;
                    if let Some(&gw) = ge.iter().take_while(|&&w| w != 0).find(|&&g| (g & 0xFF) == sem) {
                        let param = (gw >> 8) & 0x1F;
                        *regs.add(pi * 2 + 1) = param;
                    } else if (pw & 0xF00) == 0 {
                        // Default attribute vector (e.g. normal when omitted).
                        *regs.add(pi * 2 + 1) = 0x20;
                    }
                }
            }
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
    index_32bit: bool,
    cs_cache: BTreeMap<usize, (bool, bool, bool, bool)>,
    frame_verts: Vec<crate::vk::Vertex>,
    frame_idxs: Vec<u32>,
    frame_batches: Vec<crate::vk::DrawBatch>,
    frame_gverts: Vec<crate::vk::GVertex>,
    frame_gidxs: Vec<u32>,
    frame_vs_params: Vec<u32>,
    frame_gens: Vec<crate::vk::GenDraw>,
    pending_flip: Option<(i32, i32, i32, i64)>,
    // Reusable scratch buffers for try_generic_draw (eliminates ~200 MB/frame of heap alloc/zeroing).
    scratch_seen: Vec<(u32, u32)>,
    scratch_gen: u32,
    scratch_unique: Vec<u32>,
    scratch_remap: Vec<u32>,
    scratch_verts: Vec<crate::vk::GVertex>,
}

static GPU: Mutex<GpuState> = Mutex::new(GpuState {
    cx: BTreeMap::new(),
    sh: BTreeMap::new(),
    uc: BTreeMap::new(),
    clear_rgba: [0.0, 0.0, 0.0, 1.0],
    bound_tex: None,
    index_base: 0,
    index_count: 0,
    index_32bit: false,
    cs_cache: BTreeMap::new(),
    frame_verts: Vec::new(),
    frame_idxs: Vec::new(),
    frame_batches: Vec::new(),
    frame_gverts: Vec::new(),
    frame_gidxs: Vec::new(),
    frame_vs_params: Vec::new(),
    frame_gens: Vec::new(),
    pending_flip: None,
    scratch_seen: Vec::new(),
    scratch_gen: 0,
    scratch_unique: Vec::new(),
    scratch_remap: Vec::new(),
    scratch_verts: Vec::new(),
});

static READY_DRAWS: Mutex<(Vec<crate::vk::Vertex>, Vec<u32>, Vec<crate::vk::DrawBatch>)> =
    Mutex::new((Vec::new(), Vec::new(), Vec::new()));

pub fn take_frame_draws() -> (Vec<crate::vk::Vertex>, Vec<u32>, Vec<crate::vk::DrawBatch>) {
    let mut g = READY_DRAWS.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *g)
}

static READY_GEN: Mutex<(Vec<crate::vk::GVertex>, Vec<crate::vk::GenDraw>)> = Mutex::new((Vec::new(), Vec::new()));

/// Generic draws of the last finished frame (pairs with `take_frame_draws`).
pub fn take_frame_gen() -> (Vec<crate::vk::GVertex>, Vec<crate::vk::GenDraw>) {
    let mut g = READY_GEN.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *g)
}

#[cfg(windows)]
std::thread_local! {
    static COMMIT_CACHE: std::cell::RefCell<([(usize, usize); 8], usize)> =
        const { std::cell::RefCell::new(([(0, 0); 8], 0)) };
}

fn flush_committed_cache() {
    #[cfg(windows)]
    COMMIT_CACHE.with(|c| {
        let mut g = c.borrow_mut();
        g.0 = [(0, 0); 8];
        g.1 = 0;
    });
}

fn is_committed(addr: usize, len: usize) -> bool {
    if addr < 0x1_0000_0000 || addr > 0x7FFF_FFFF_FFFF || len == 0 {
        return false;
    }
    let end = addr.saturating_add(len);
    #[cfg(windows)]
    {
        let hit = COMMIT_CACHE.with(|c| {
            let g = c.borrow();
            g.0.iter().any(|&(b, e)| b != 0 && addr >= b && end <= e)
        });
        if hit {
            return true;
        }
        unsafe {
            let mut mbi: windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if windows_sys::Win32::System::Memory::VirtualQuery(
                addr as *const std::ffi::c_void,
                &mut mbi,
                std::mem::size_of::<windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION>(),
            ) != 0
                && mbi.State == windows_sys::Win32::System::Memory::MEM_COMMIT
            {
                let base = mbi.BaseAddress as usize;
                let reg_end = base.saturating_add(mbi.RegionSize);
                if reg_end >= end {
                    COMMIT_CACHE.with(|c| {
                        let mut g = c.borrow_mut();
                        let idx = g.1 & 7;
                        g.0[idx] = (base, reg_end);
                        g.1 = g.1.wrapping_add(1);
                    });
                    return true;
                }
            }
            false
        }
    }
    #[cfg(not(windows))]
    {
        let _ = end;
        false
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1F) as i32;
    let frac = (h & 0x3FF) as u32;
    if exp == 0 {
        if frac == 0 {
            f32::from_bits(sign)
        } else {
            let mut f = frac;
            let mut e = -14i32;
            while (f & 0x400) == 0 {
                f <<= 1;
                e -= 1;
            }
            f &= 0x3FF;
            f32::from_bits(sign | (((e + 127) as u32) << 23) | (f << 13))
        }
    } else if exp == 31 {
        f32::from_bits(sign | 0x7F80_0000 | (frac << 13))
    } else {
        f32::from_bits(sign | (((exp - 15 + 127) as u32) << 23) | (frac << 13))
    }
}

fn bound_shader_pair(gpu: &GpuState) -> (usize, usize) {
    let read_pair = |lo_reg: u32, hi_reg: u32| -> usize {
        let lo = *gpu.sh.get(&lo_reg).unwrap_or(&0) as u64;
        let hi = *gpu.sh.get(&hi_reg).unwrap_or(&0) as u64;
        ((lo | ((hi & 0xFF) << 32)) << 8) as usize
    };
    let vs = [read_pair(0x88, 0x89), read_pair(0xC8, 0xC9), read_pair(0x48, 0x49)]
        .into_iter()
        .find(|&a| a >= 0x1_0000_0000)
        .unwrap_or(0);
    let ps = read_pair(0x08, 0x09);
    (vs, ps)
}

/// Committed-memory check for the shader interpreters: keeps an 8-entry MRU cache of
/// committed regions per thread so vertex shaders reading multiple buffers (CB + V# + VB)
/// never thrash VirtualQuery syscalls.
fn mem_ok(addr: usize, len: usize) -> bool {
    thread_local! {
        static REGIONS: std::cell::Cell<[(usize, usize); 8]> = const { std::cell::Cell::new([(0, 0); 8]) };
        static NEXT_SLOT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let end = addr.saturating_add(len);
    let hit = REGIONS.with(|r| {
        let arr = r.get();
        for &(lo, hi) in &arr {
            if addr >= lo && end <= hi {
                return true;
            }
        }
        false
    });
    if hit {
        return true;
    }
    if addr < 0x1_0000_0000 || addr > 0x7FFF_FFFF_FFFF || len == 0 {
        return false;
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT};
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        if VirtualQuery(addr as *const std::ffi::c_void, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 || mbi.State != MEM_COMMIT {
            return false;
        }
        let (b, e) = (mbi.BaseAddress as usize, mbi.BaseAddress as usize + mbi.RegionSize);
        REGIONS.with(|r| {
            let mut arr = r.get();
            let slot = NEXT_SLOT.with(|s| {
                let i = s.get();
                s.set((i + 1) & 7);
                i
            });
            arr[slot] = (b, e);
            r.set(arr);
        });
        return end <= e;
    }
    #[allow(unreachable_code)]
    is_committed(addr, len)
}

struct VsJobSpec {
    run: fn(usize, usize, usize),
    ctx: usize,
    total: usize,
    chunk: usize,
}

struct VsPoolState {
    lock: Mutex<(u64, Option<VsJobSpec>)>,
    cvar: std::sync::Condvar,
    next: std::sync::atomic::AtomicUsize,
    remaining: std::sync::atomic::AtomicUsize,
    n_workers: usize,
}

static VS_POOL: LazyLock<std::sync::Arc<VsPoolState>> = LazyLock::new(|| {
    let n_threads = std::thread::available_parallelism().map(|n| n.get().min(12)).unwrap_or(8);
    let n_workers = n_threads.saturating_sub(1);
    let state = std::sync::Arc::new(VsPoolState {
        lock: Mutex::new((0, None)),
        cvar: std::sync::Condvar::new(),
        next: std::sync::atomic::AtomicUsize::new(0),
        remaining: std::sync::atomic::AtomicUsize::new(0),
        n_workers,
    });
    for w in 0..n_workers {
        let st = state.clone();
        let _ = std::thread::Builder::new().name(format!("orbis-vs-{w}")).spawn(move || {
            let mut seen_gen = 0u64;
            loop {
                let spec = {
                    let mut g = st.lock.lock().unwrap_or_else(|e| e.into_inner());
                    while g.0 == seen_gen {
                        g = st.cvar.wait(g).unwrap_or_else(|e| e.into_inner());
                    }
                    seen_gen = g.0;
                    g.1.as_ref().map(|j| (j.run, j.ctx, j.total, j.chunk))
                };
                if let Some((run, ctx, total, chunk)) = spec {
                    loop {
                        let start = st.next.fetch_add(chunk, Ordering::Relaxed);
                        if start >= total {
                            break;
                        }
                        run(ctx, start, (start + chunk).min(total));
                    }
                    st.remaining.fetch_sub(1, Ordering::Release);
                }
            }
        });
    }
    state
});

fn vs_parallel_for<F: Fn(usize, usize) + Sync>(total: usize, f: F) {
    let pool = &*VS_POOL;
    if pool.n_workers == 0 || total < 256 {
        f(0, total);
        return;
    }
    fn trampoline<F: Fn(usize, usize) + Sync>(ctx: usize, start: usize, end: usize) {
        let f = unsafe { &*(ctx as *const F) };
        f(start, end);
    }
    let chunk = (total / ((pool.n_workers + 1) * 4)).clamp(64, 512);
    pool.next.store(0, Ordering::Relaxed);
    pool.remaining.store(pool.n_workers, Ordering::Release);
    {
        let mut g = pool.lock.lock().unwrap_or_else(|e| e.into_inner());
        g.0 = g.0.wrapping_add(1);
        g.1 = Some(VsJobSpec {
            run: trampoline::<F>,
            ctx: &f as *const F as usize,
            total,
            chunk,
        });
        pool.cvar.notify_all();
    }
    loop {
        let start = pool.next.fetch_add(chunk, Ordering::Relaxed);
        if start >= total {
            break;
        }
        f(start, (start + chunk).min(total));
    }
    let mut spins = 0u32;
    while pool.remaining.load(Ordering::Acquire) != 0 {
        spins += 1;
        if spins < 64 {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
}

/// Runs the bound vertex (NGG) shader on the CPU for the vertices of a draw and prints the
/// clip-space positions (ORBIS_VS_CPU=1). Checks the interpreter against known-good draws.
fn vs_cpu_probe(gpu: &GpuState, idx_off: usize, idx_cnt: usize) {
    static SEEN: std::sync::Mutex<Vec<(usize, u32)>> = std::sync::Mutex::new(Vec::new());
    let (vs_addr, _) = bound_shader_pair(gpu);
    {
        let mut seen = SEEN.lock().unwrap();
        let entry = seen.iter_mut().find(|(a, _)| *a == vs_addr);
        match entry {
            Some((_, n)) if *n >= 3 => return,
            Some((_, n)) => *n += 1,
            None => seen.push((vs_addr, 1)),
        }
    }
    if !is_committed(vs_addr, 256) {
        return;
    }
    let code = unsafe { std::slice::from_raw_parts(vs_addr as *const u32, 1 << 14) };
    let ib = (gpu.index_base as usize).saturating_add(idx_off * 2);
    if !is_committed(ib, idx_cnt.min(6) * 2) {
        return;
    }
    let idxs = unsafe { std::slice::from_raw_parts(ib as *const u16, idx_cnt.min(6)) };
    let mut line = String::new();
    for &ix in idxs.iter().take(4) {
        let mut lane = crate::rdna::interp::Lane::new();
        lane.s[3] = 0x101; // one vertex, one primitive in this "wave"
        for k in 0..24u32 {
            lane.s[8 + k as usize] = *gpu.sh.get(&(0x8C + k)).unwrap_or(&0);
        }
        lane.v[5] = ix as u32; // VertexID
        lane.v[8] = 0; // InstanceID
        match crate::rdna::interp::run(code, &mut lane, mem_ok) {
            Ok(()) => {
                let pos = lane.exports.iter().find(|e| e.target == 12).map(|e| e.value);
                let params = lane.exports.iter().filter(|e| e.target >= 32).count();
                match pos {
                    Some(p) => line += &format!(" v{ix}=({:.3},{:.3},{:.3},{:.3})+{params}p", p[0], p[1], p[2], p[3]),
                    None => line += &format!(" v{ix}=no-pos"),
                }
            }
            Err(e) => {
                line += &format!(" v{ix}: {e}");
                break;
            }
        }
    }
    let (_, ps_addr) = bound_shader_pair(gpu);
    let idx_of = |addr: usize| crate::gpu::SHADERS.lock().map(|l| l.iter().position(|(_, c, _)| *c == addr)).ok().flatten();
    eprintln!("[orbis] VS-CPU vs={vs_addr:#x} (s{:?}) ps={ps_addr:#x} (s{:?}) n={idx_cnt}:{line}", idx_of(vs_addr), idx_of(ps_addr));
}

/// Shader code at a guest address, as long a slice as is mapped (decoding stops at s_endpgm).
fn shader_words(addr: usize) -> Option<&'static [u32]> {
    for n in [1usize << 14, 1 << 12, 1 << 10, 1 << 8] {
        if is_committed(addr, n * 4) {
            return Some(unsafe { std::slice::from_raw_parts(addr as *const u32, n) });
        }
    }
    None
}

type PsCache = HashMap<(usize, u32, u64), Option<(std::sync::Arc<Vec<u32>>, bool)>>;
type DecodedCache = HashMap<(usize, u64), std::sync::Arc<crate::rdna::interp::CompiledShader>>;
/// Shaders with real branches: translated per execution path (key: shader, path hash).
static PATH_SHADERS: LazyLock<Mutex<std::collections::HashSet<usize>>> = LazyLock::new(|| Mutex::new(Default::default()));
static PATH_CACHE: LazyLock<Mutex<PsCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static PS_CACHE: LazyLock<Mutex<PsCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static DECODED_CACHE: LazyLock<Mutex<DecodedCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
type VsSpvCache = HashMap<(usize, u64, [u32; 8], [u16; 8]), Option<(std::sync::Arc<Vec<u32>>, u32)>>;
static VS_SPV_CACHE: LazyLock<Mutex<VsSpvCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static GEN_STATS: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static FRAME_STATS_ON: LazyLock<bool> = LazyLock::new(|| std::env::var_os("ORBIS_FRAME_STATS").is_some());
static PERF_VS_NS: AtomicU64 = AtomicU64::new(0);
static PERF_PREP_NS: AtomicU64 = AtomicU64::new(0);
static PERF_DEDUP_NS: AtomicU64 = AtomicU64::new(0);
static PERF_PS_NS: AtomicU64 = AtomicU64::new(0);
static PERF_GPU_VS_DRAWS: AtomicU64 = AtomicU64::new(0);
static PERF_GPU_VS_IDXS: AtomicU64 = AtomicU64::new(0);
static PERF_CS_NS: AtomicU64 = AtomicU64::new(0);
static PERF_CS_CNT: AtomicU64 = AtomicU64::new(0);
static PERF_CS_THR: AtomicU64 = AtomicU64::new(0);
// Per-frame VS profiler: vs_addr -> (draws, unique_verts, inst_count, total_ns)
static PERF_VS_MAP: LazyLock<Mutex<HashMap<usize, (u64, u64, usize, u64)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn shader_hash(code: &[u32]) -> u64 {
    code.iter().take(64).fold(0xcbf2_9ce4_8422_2325u64, |h, &w| (h ^ w as u64).wrapping_mul(0x100_0000_01b3))
}

fn compiled_shader(addr: usize, code: &[u32], hash: u64) -> std::sync::Arc<crate::rdna::interp::CompiledShader> {
    let mut cache = DECODED_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry((addr, hash))
        .or_insert_with(|| std::sync::Arc::new(crate::rdna::interp::CompiledShader::new(code)))
        .clone()
}

/// (generic draws, VS failures, PS untranslatable, skipped) since start.
pub fn gen_stats() -> [u64; 4] {
    std::array::from_fn(|k| GEN_STATS[k].load(Ordering::Relaxed))
}

/// Draws one indexed draw through the generic path: the bound NGG vertex shader runs on the
/// CPU per vertex, the pixel shader is translated to SPIR-V. Returns false to let the older
/// layout-guessing path try the draw.
fn try_generic_draw(gpu: &mut GpuState, idx_off: usize, idx_cnt: usize) -> bool {
    let t_prep = (*FRAME_STATS_ON).then(std::time::Instant::now);
    // CB_TARGET_MASK (MRT0 bits): nothing written → stencil/depth-only pass, skip before VS!
    let wmask = gpu.cx.get(&0x8E).copied().unwrap_or(0xF) & 0xF;
    if wmask == 0 {
        GEN_STATS[3].fetch_add(1, Ordering::Relaxed);
        return true;
    }
    let (vs_addr, ps_addr) = bound_shader_pair(gpu);
    let (Some(vs_code), Some(ps_code)) = (shader_words(vs_addr), shader_words(ps_addr)) else {
        return false;
    };
    let ib_bytes = if gpu.index_32bit { idx_cnt * 4 } else { idx_cnt * 2 };
    let ib = (gpu.index_base as usize).saturating_add(if gpu.index_32bit { idx_off * 4 } else { idx_off * 2 });
    if idx_cnt < 3 || !is_committed(ib, ib_bytes) || gpu.frame_gverts.len() >= 2_000_000 {
        return false;
    }
    // Pixel shader translation (cached per address, input layout and code hash).
    let input_addr = gpu.cx.get(&0x1B4).copied().filter(|&v| v != 0).or_else(|| gpu.cx.get(&0x1B3).copied()).unwrap_or(0x302);
    let hash = shader_hash(ps_code);
    let vs_hash = shader_hash(vs_code);
    let vs_sh = compiled_shader(vs_addr, vs_code, vs_hash);
    let ps_sh = compiled_shader(ps_addr, ps_code, hash);
    let tr = {
        let mut cache = PS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entry((ps_addr, input_addr, hash))
            .or_insert_with(|| match crate::rdna::ps::translate(ps_code, input_addr) {
                Ok(t) => Some((std::sync::Arc::new(t.spirv), t.cube)),
                Err(e) if e == "control flow" => {
                    PATH_SHADERS.lock().unwrap_or_else(|e| e.into_inner()).insert(ps_addr ^ hash as usize);
                    None
                }
                Err(e) => {
                    eprintln!("[orbis] PS {ps_addr:#x}: not translated ({e})");
                    if let Ok(dir) = std::env::var("ORBIS_GEN_DUMP") {
                        let n = crate::rdna::decode_shader(ps_code).last().map(|i| i.pc as usize / 4 + i.words.len()).unwrap_or(0);
                        let b: Vec<u8> = ps_code[..n].iter().flat_map(|w| w.to_le_bytes()).collect();
                        let _ = std::fs::write(format!("{dir}/bad_{ps_addr:x}.bin"), b);
                    }
                    None
                }
            })
            .clone()
    };
    let path_mode = tr.is_none() && PATH_SHADERS.lock().unwrap_or_else(|e| e.into_inner()).contains(&(ps_addr ^ hash as usize));
    if tr.is_none() && !path_mode {
        GEN_STATS[2].fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let mut ps_lane = crate::rdna::interp::Lane::new();
    for k in 0..32u32 {
        ps_lane.s[k as usize] = *gpu.sh.get(&(0x0C + k)).unwrap_or(&0);
    }
    // Branch-free shaders: scalar pass now. Shaders with branches: traced after the vertex
    // shader has run (the path depends on the interpolated attributes).
    let pre = if path_mode {
        None
    } else {
        match crate::rdna::interp::run_scalar_compiled(&ps_sh, &mut ps_lane, mem_ok) {
            Ok(info) => Some(info),
            Err(_) => {
                GEN_STATS[2].fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
    };
    let (xs, xo) = (f32::from_bits(*gpu.cx.get(&0x10F).unwrap_or(&0)), f32::from_bits(*gpu.cx.get(&0x110).unwrap_or(&0)));
    let (ys, yo) = (f32::from_bits(*gpu.cx.get(&0x111).unwrap_or(&0)), f32::from_bits(*gpu.cx.get(&0x112).unwrap_or(&0)));
    // Colour target 0: CB_COLOR0_BASE (+ _EXT) and its size from CB_COLOR0_ATTRIB2.
    let rt = {
        let base = *gpu.cx.get(&0x318).unwrap_or(&0) as u64 | ((*gpu.cx.get(&0x390).unwrap_or(&0) as u64 & 0xFF) << 32);
        let a2 = *gpu.cx.get(&0x3B0).unwrap_or(&0);
        ((base << 8) as usize, ((a2 >> 14) & 0x3FFF) + 1, (a2 & 0x3FFF) + 1)
    };
    let (tw, th) = (rt.1 as f32, rt.2 as f32);
    // Clip position → Vulkan clip space of the target: screen = ndc * scale + offset (pixels).
    let have_vp = xs != 0.0 && ys != 0.0 && xs.is_finite() && ys.is_finite() && rt.0 != 0;
    let to_target = |p: [f32; 4]| -> [f32; 4] {
        if have_vp {
            [(p[0] * xs + p[3] * xo) * 2.0 / tw - p[3], (p[1] * ys + p[3] * yo) * 2.0 / th - p[3], p[2], p[3]]
        } else {
            [p[0], -p[1], p[2], p[3]]
        }
    };
    // Deduplicate indices directly from guest IB into reusable scratch buffers (zero heap alloc).
    if let Some(t) = t_prep {
        PERF_PREP_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let t_dedup = (*FRAME_STATS_ON).then(std::time::Instant::now);
    let cntl: [u32; 8] = std::array::from_fn(|n| gpu.cx.get(&(0x191 + n as u32)).map(|v| v & 0x3F).unwrap_or(n as u32));
    let vs_ud: [u32; 24] = std::array::from_fn(|k| *gpu.sh.get(&(0x8C + k as u32)).unwrap_or(&0));
    let (min_ix, max_ix) = if gpu.index_32bit {
        unsafe { std::slice::from_raw_parts(ib as *const u32, idx_cnt) }
            .iter()
            .fold((u32::MAX, 0u32), |(lo, hi), &v| (lo.min(v), hi.max(v)))
    } else {
        unsafe { std::slice::from_raw_parts(ib as *const u16, idx_cnt) }
            .iter()
            .fold((u32::MAX, 0u32), |(lo, hi), &v| {
                let v = v as u32;
                (lo.min(v), hi.max(v))
            })
    };
    let prim = gpu.uc.get(&0x242).copied().map(|v| if v > 0xFF { v >> 8 } else { v }).unwrap_or(4);
    let indexed_draw = prim != 0x11;

    // Runtime Auto-GPU Vertex Shader Offload: run large 3D triangle-list draws (>= 36 indices)
    // natively on the GPU via translated SPIR-V VS (zero CPU per-vertex shading, zero GVBUF bloat!).
    if indexed_draw && idx_cnt >= 36 && tr.is_some() && pre.is_some() {
        let vp = [
            xs.to_bits(),
            xo.to_bits(),
            ys.to_bits(),
            yo.to_bits(),
            if tw > 0.0 { (2.0 / tw).to_bits() } else { 0 },
            if th > 0.0 { (2.0 / th).to_bits() } else { 0 },
            have_vp as u32,
            0,
        ];
        if let Some(gpu_data) = crate::rdna::vs::extract_gpu_vs_draw(&vs_sh, &vs_ud, max_ix, vp, mem_ok) {
            let vs_spv = {
                let mut cache = VS_SPV_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                cache
                    .entry((vs_addr, vs_hash, cntl, gpu_data.mubuf_key))
                    .or_insert_with(|| match crate::rdna::vs::translate(&vs_sh, &cntl, &gpu_data.mubuf_key) {
                        Ok((spv, slots)) => Some((std::sync::Arc::new(spv), slots)),
                        Err(e) => {
                            if *FRAME_STATS_ON {
                                eprintln!("[orbis] GPU-VS {vs_addr:#x}: fallback ({e})");
                            }
                            None
                        }
                    })
                    .clone()
            };
            if let Some((vs_spv, expected_slots)) = vs_spv {
                if gpu_data.params.len() != expected_slots as usize {
                    eprintln!("[orbis] GPU-VS SLOT MISMATCH vs={vs_addr:#x}: params={} expected={expected_slots}", gpu_data.params.len());
                } else {
                let (tr_spv, cube) = tr.unwrap();
                let info = pre.unwrap();
                let ps_hash = hash ^ (std::sync::Arc::as_ptr(&tr_spv) as u64);
                let decode_img = |(t, _): &([u32; 8], [u32; 4])| {
                    let addr = (((t[0] as u64) | (((t[1] & 0xFF) as u64) << 32)) << 8) as usize;
                    let w = ((t[1] >> 30) | ((t[2] & 0xFFF) << 2)) + 1;
                    let h = ((t[2] >> 14) & 0x3FFF) + 1;
                    let swiz = ((t[3] >> 20) & 0x1F) | (((t[1] >> 20) & 0x1FF) << 8) | ((t[3] & 0xFFF) << 20);
                    (addr, w, h, swiz)
                };
                let tex = info.images.first().map(decode_img);
                let extra_texs: [Option<(usize, u32, u32, u32)>; 3] = std::array::from_fn(|k| info.images.get(k + 1).map(decode_img));
                let extra_samps: [[u32; 4]; 3] = std::array::from_fn(|k| info.images.get(k + 1).map(|(_, s)| *s).unwrap_or([0; 4]));
                let mut slots = [0u32; 64];
                for (k, &v) in info.slots.iter().take(64).enumerate() {
                    slots[k] = v;
                }
                let scissor = {
                    let tl = *gpu.cx.get(&0x90).unwrap_or(&0);
                    let br = *gpu.cx.get(&0x91).unwrap_or(&0x4000_4000);
                    let (x0, y0) = ((tl & 0x7FFF) as i32, ((tl >> 16) & 0x7FFF) as i32);
                    let (x1, y1) = (((br & 0x7FFF) as i32).min(rt.1 as i32), (((br >> 16) & 0x7FFF) as i32).min(rt.2 as i32));
                    if x1 > x0 && y1 > y0 { [x0, y0, x1 - x0, y1 - y0] } else { [0, 0, rt.1 as i32, rt.2 as i32] }
                };
                // Align VS param offset to 256 bytes (64 u32s) for Vulkan minStorageBufferOffsetAlignment.
                let pad = (64 - (gpu.frame_vs_params.len() & 63)) & 63;
                if pad > 0 {
                    gpu.frame_vs_params.resize(gpu.frame_vs_params.len() + pad, 0);
                }
                let vs_param_off = (gpu.frame_vs_params.len() * 4) as u32;
                gpu.frame_vs_params.extend_from_slice(&gpu_data.params);
                // Copy raw guest indices straight into gibuf (zero CPU dedup / zero GVertex!).
                let n_out = idx_cnt / 3 * 3;
                let first_index = gpu.frame_gidxs.len() as u32;
                gpu.frame_gidxs.reserve(n_out);
                if gpu.index_32bit {
                    gpu.frame_gidxs.extend_from_slice(unsafe { std::slice::from_raw_parts(ib as *const u32, n_out) });
                } else {
                    for &ix16 in unsafe { std::slice::from_raw_parts(ib as *const u16, n_out) } {
                        gpu.frame_gidxs.push(ix16 as u32);
                    }
                }
                let blend = gpu.cx.get(&0x1E0).copied().unwrap_or(0);
                let dctl = gpu.cx.get(&0x200).copied().unwrap_or(0);
                let dkey = dctl & 0x77;
                let g = gpu.frame_gens.len() as u32;
                let vs_ptr_key = std::sync::Arc::as_ptr(&vs_spv) as usize;
                gpu.frame_gens.push(crate::vk::GenDraw {
                    key: (ps_addr ^ (ps_hash as usize) ^ vs_ptr_key.rotate_left(17), blend, wmask | (dkey << 8) | (1 << 24)),
                    wmask,
                    spirv: tr_spv,
                    blend,
                    first_vertex: 0,
                    vertex_count: 0,
                    first_index,
                    index_count: n_out as u32,
                    slots,
                    tex,
                    extra_texs,
                    layers: if cube { 6 } else { 1 },
                    rt,
                    scissor,
                    samp: info.images.first().map(|(_, s)| *s).unwrap_or([0; 4]),
                    extra_samps,
                    depth: dctl,
                    zclear: if matches!((dctl >> 4) & 7, 4 | 6) { 0.0 } else { 1.0 },
                    vs_spirv: Some(vs_spv),
                    vs_param_off,
                    vs_vbs: gpu_data.vbs,
                });
                gpu.frame_batches.push(crate::vk::DrawBatch {
                    first_index: 0,
                    index_count: 0,
                    tex_addr: 0,
                    tex_w: 0,
                    tex_h: 0,
                    tex_swiz: 0,
                    gen: Some(g),
                });
                if let Some(t) = t_dedup {
                    PERF_DEDUP_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                }
                if *FRAME_STATS_ON {
                    PERF_GPU_VS_DRAWS.fetch_add(1, Ordering::Relaxed);
                    PERF_GPU_VS_IDXS.fetch_add(n_out as u64, Ordering::Relaxed);
                    static GVS_DIAG: AtomicU64 = AtomicU64::new(0);
                    if GVS_DIAG.fetch_add(1, Ordering::Relaxed) < 3 {
                        eprintln!(
                            "[orbis] GPU-VS-DIAG vs={vs_addr:#x} idx={n_out} max_ix={max_ix} mk={:x?} vbs={:x?} vp={:?} mubuf_off_str={:?}",
                            &gpu_data.mubuf_key[..gpu_data.n_mubuf as usize],
                            &gpu_data.vbs[..gpu_data.n_vbs as usize],
                            (xs, xo, ys, yo, tw, th, have_vp),
                            &gpu_data.params[8..8 + (gpu_data.n_mubuf as usize) * 2]
                        );
                    }
                }
                GEN_STATS[0].fetch_add(1, Ordering::Relaxed);
                return true;
                }
            }
        }
    }
    let span = max_ix.saturating_sub(min_ix) as usize;
    gpu.scratch_unique.clear();
    gpu.scratch_remap.clear();
    gpu.scratch_remap.reserve(idx_cnt);
    if span < 131_072 {
        if gpu.scratch_seen.len() <= span {
            gpu.scratch_seen.resize(span + 1, (0, 0));
        }
        gpu.scratch_gen = gpu.scratch_gen.wrapping_add(1);
        if gpu.scratch_gen == 0 {
            gpu.scratch_seen.fill((0, 0));
            gpu.scratch_gen = 1;
        }
        let gen_tag = gpu.scratch_gen;
        if gpu.index_32bit {
            for &ix in unsafe { std::slice::from_raw_parts(ib as *const u32, idx_cnt) } {
                let entry = &mut gpu.scratch_seen[(ix - min_ix) as usize];
                let u = if entry.0 == gen_tag {
                    entry.1
                } else {
                    let id = gpu.scratch_unique.len() as u32;
                    *entry = (gen_tag, id);
                    gpu.scratch_unique.push(ix);
                    id
                };
                gpu.scratch_remap.push(u);
            }
        } else {
            for &ix16 in unsafe { std::slice::from_raw_parts(ib as *const u16, idx_cnt) } {
                let ix = ix16 as u32;
                let entry = &mut gpu.scratch_seen[(ix - min_ix) as usize];
                let u = if entry.0 == gen_tag {
                    entry.1
                } else {
                    let id = gpu.scratch_unique.len() as u32;
                    *entry = (gen_tag, id);
                    gpu.scratch_unique.push(ix);
                    id
                };
                gpu.scratch_remap.push(u);
            }
        }
    } else {
        let mut seen: HashMap<u32, u32> = HashMap::with_capacity(idx_cnt.min(4096));
        for k in 0..idx_cnt {
            let ix = if gpu.index_32bit {
                unsafe { *(ib as *const u32).add(k) }
            } else {
                unsafe { *(ib as *const u16).add(k) as u32 }
            };
            let u = *seen.entry(ix).or_insert_with(|| {
                let id = gpu.scratch_unique.len() as u32;
                gpu.scratch_unique.push(ix);
                id
            });
            gpu.scratch_remap.push(u);
        }
    }
    if let Some(t) = t_dedup {
        PERF_DEDUP_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let unique_len = gpu.scratch_unique.len();
    let first_vertex = gpu.frame_gverts.len() as u32;
    if indexed_draw {
        if gpu.frame_gverts.len() + unique_len > 2_000_000 {
            return false;
        }
        gpu.frame_gverts.resize(first_vertex as usize + unique_len, crate::vk::GVertex::default());
    } else {
        gpu.scratch_verts.resize(unique_len, crate::vk::GVertex::default());
    }
    let unique = gpu.scratch_unique.as_slice();
    let verts_ptr = if indexed_draw {
        unsafe { gpu.frame_gverts.as_mut_ptr().add(first_vertex as usize) as usize }
    } else {
        gpu.scratch_verts.as_mut_ptr() as usize
    };
    let failed = std::sync::atomic::AtomicBool::new(false);
    let t_vs = (*FRAME_STATS_ON).then(std::time::Instant::now);
    let hoisted = crate::rdna::interp::hoist_vs_draw(&vs_sh, &vs_ud, max_ix, mem_ok);
    if let Some(plan) = hoisted.as_ref() {
        vs_parallel_for(unique_len, |start, end| {
            if failed.load(Ordering::Relaxed) {
                return;
            }
            let mut vreg = [0u32; 256];
            let out_slice = unsafe { std::slice::from_raw_parts_mut((verts_ptr as *mut crate::vk::GVertex).add(start), end - start) };
            for (&ix, dst) in unique[start..end].iter().zip(out_slice.iter_mut()) {
                let Some((p, attr)) = crate::rdna::interp::eval_vs_hoisted(plan, &mut vreg, ix, &cntl) else {
                    failed.store(true, Ordering::Relaxed);
                    return;
                };
                *dst = crate::vk::GVertex { pos: to_target(p), attr };
            }
        });
    } else {
        // Fallback: scalar prefix + general interpreter.
        let mut prefix_lane = crate::rdna::interp::Lane::new();
        prefix_lane.s[3] = 0x101;
        prefix_lane.s[8..32].copy_from_slice(&vs_ud);
        if vs_sh.scalar_prefix_len > 0
            && crate::rdna::interp::run_compiled_prefix(&vs_sh, &mut prefix_lane, mem_ok).is_err()
        {
            GEN_STATS[1].fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let base_s = prefix_lane.s;
        let base_scc = prefix_lane.scc;
        let start_pc = vs_sh.scalar_prefix_len;
        vs_parallel_for(unique_len, |start, end| {
            if failed.load(Ordering::Relaxed) {
                return;
            }
            thread_local! {
                static TL_LANE: std::cell::RefCell<crate::rdna::interp::Lane> = std::cell::RefCell::new(crate::rdna::interp::Lane::new());
            }
            TL_LANE.with(|cell| {
                let mut lane = cell.borrow_mut();
                let out_slice = unsafe { std::slice::from_raw_parts_mut((verts_ptr as *mut crate::vk::GVertex).add(start), end - start) };
                for (&ix, dst) in unique[start..end].iter().zip(out_slice.iter_mut()) {
                    lane.s = base_s;
                    lane.scc = base_scc;
                    lane.v.fill(0);
                    lane.exports.clear();
                    lane.v[5] = ix;
                    lane.v[8] = 0;
                    if crate::rdna::interp::run_compiled_from(&vs_sh, start_pc, &mut lane, mem_ok).is_err() {
                        failed.store(true, Ordering::Relaxed);
                        return;
                    }
                    let mut pos = None;
                    let mut attr = [[0f32; 4]; 8];
                    for e in &lane.exports {
                        if e.target == 12 {
                            pos = Some(e.value);
                        } else if (32..96).contains(&e.target) {
                            let p = e.target - 32;
                            for n in 0..8 {
                                if cntl[n] == p {
                                    attr[n] = e.value;
                                }
                            }
                        }
                    }
                    let Some(p) = pos else {
                        failed.store(true, Ordering::Relaxed);
                        return;
                    };
                    *dst = crate::vk::GVertex { pos: to_target(p), attr };
                }
            });
        });
    }
    if let Some(t) = t_vs {
        let dt = t.elapsed().as_nanos() as u64;
        PERF_VS_NS.fetch_add(dt, Ordering::Relaxed);
        let mut m = PERF_VS_MAP.lock().unwrap_or_else(|e| e.into_inner());
        let e = m.entry(vs_addr).or_insert((0, 0, vs_sh.insts.len(), 0));
        e.0 += 1;
        e.1 += unique.len() as u64;
        e.3 += dt;
        if unique.len() >= 1000 && hoisted.is_none() {
            static MISS_VS: LazyLock<Mutex<std::collections::HashSet<usize>>> = LazyLock::new(|| Mutex::new(Default::default()));
            if MISS_VS.lock().unwrap_or_else(|e| e.into_inner()).insert(vs_addr) {
                eprintln!("[orbis] VS-HOIST-MISS {vs_addr:#x}: can_hoist={} insts={} unique={}", vs_sh.can_hoist_scalar, vs_sh.insts.len(), unique.len());
            }
        }
    }
    if failed.load(Ordering::Relaxed) {
        if indexed_draw {
            gpu.frame_gverts.truncate(first_vertex as usize);
        }
        GEN_STATS[1].fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let t_ps = (*FRAME_STATS_ON).then(std::time::Instant::now);
    let (tr, cube, info) = match (tr, pre) {
        (Some((tr, cube)), Some(info)) => (tr, cube, info),
        _ => {
            let attrs = if indexed_draw {
                gpu.frame_gverts[first_vertex as usize + gpu.scratch_remap[0] as usize].attr
            } else {
                gpu.scratch_verts[gpu.scratch_remap[0] as usize].attr
            };
            let Ok((pcs, info)) = crate::rdna::interp::trace_ps_compiled(&ps_sh, &mut ps_lane, &attrs, mem_ok) else {
                if indexed_draw {
                    gpu.frame_gverts.truncate(first_vertex as usize);
                }
                GEN_STATS[2].fetch_add(1, Ordering::Relaxed);
                return false;
            };
            let phash = pcs.iter().fold(hash, |h, &w| (h ^ w as u64).wrapping_mul(0x100_0000_01b3));
            let got = PATH_CACHE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry((ps_addr, input_addr, phash))
                .or_insert_with(|| match crate::rdna::ps::translate_path(ps_code, input_addr, &pcs) {
                    Ok(t) => Some((std::sync::Arc::new(t.spirv), t.cube)),
                    Err(e) => {
                        eprintln!("[orbis] PS {ps_addr:#x}: path not translated ({e})");
                        None
                    }
                })
                .clone();
            let Some((tr, cube)) = got else {
                if indexed_draw {
                    gpu.frame_gverts.truncate(first_vertex as usize);
                }
                GEN_STATS[2].fetch_add(1, Ordering::Relaxed);
                return false;
            };
            (tr, cube, info)
        }
    };
    if let Some(t) = t_ps {
        PERF_PS_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    let hash = hash ^ (std::sync::Arc::as_ptr(&tr) as u64);
    let decode_img = |(t, _): &([u32; 8], [u32; 4])| {
        let addr = (((t[0] as u64) | (((t[1] & 0xFF) as u64) << 32)) << 8) as usize;
        let w = ((t[1] >> 30) | ((t[2] & 0xFFF) << 2)) + 1;
        let h = ((t[2] >> 14) & 0x3FFF) + 1;
        let swiz = ((t[3] >> 20) & 0x1F) | (((t[1] >> 20) & 0x1FF) << 8) | ((t[3] & 0xFFF) << 20);
        (addr, w, h, swiz)
    };
    let tex = info.images.first().map(decode_img);
    let extra_texs: [Option<(usize, u32, u32, u32)>; 3] = std::array::from_fn(|k| info.images.get(k + 1).map(decode_img));
    let extra_samps: [[u32; 4]; 3] = std::array::from_fn(|k| info.images.get(k + 1).map(|(_, s)| *s).unwrap_or([0; 4]));
    if cube && std::env::var_os("ORBIS_TEX_LOG").is_some() {
        if let Some((t, _)) = info.images.first() {
            eprintln!("[orbis] CUBE {tex:x?} type={:#x} last_level={} base_level={} depth={} T#={t:08x?}", t[3] >> 28, (t[3] >> 16) & 0xF, (t[3] >> 12) & 0xF, t[4] & 0x1FFF);
        }
    }
    if std::env::var_os("ORBIS_TEX_LOG").is_some() {
        static SEEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());
        if let Some((t, _)) = info.images.first() {
            let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            if !seen.contains(&(t[0] as usize)) {
                seen.push(t[0] as usize);
                eprintln!("[orbis] TEX ps={ps_addr:#x} fmt={} {tex:x?} T#={t:08x?}", (t[1] >> 20) & 0x1FF);
            }
        }
    }
    // The final copy of the offscreen render target to the display: we draw straight to the
    // swapchain, so its source texture is not in guest memory. Drop the draw.

    let mut slots = [0u32; 64];
    for (k, &v) in info.slots.iter().take(64).enumerate() {
        slots[k] = v;
    }
    // PA_SC_GENERIC_SCISSOR_TL / BR, clipped to the target.
    let scissor = {
        let tl = *gpu.cx.get(&0x90).unwrap_or(&0);
        let br = *gpu.cx.get(&0x91).unwrap_or(&0x4000_4000);
        let (x0, y0) = ((tl & 0x7FFF) as i32, ((tl >> 16) & 0x7FFF) as i32);
        let (x1, y1) = (((br & 0x7FFF) as i32).min(rt.1 as i32), (((br >> 16) & 0x7FFF) as i32).min(rt.2 as i32));
        if x1 > x0 && y1 > y0 { [x0, y0, x1 - x0, y1 - y0] } else { [0, 0, rt.1 as i32, rt.2 as i32] }
    };
    // Primitive type: triangle list (indexed directly in gibuf!) or rect list (3 corners → 2 triangles).
    let (vertex_count, first_index, index_count) = if indexed_draw {
        let n_out = gpu.scratch_remap.len() / 3 * 3;
        let fi = gpu.frame_gidxs.len() as u32;
        gpu.frame_gidxs.reserve(n_out);
        for &r in &gpu.scratch_remap[..n_out] {
            gpu.frame_gidxs.push(first_vertex + r);
        }
        (unique_len as u32, fi, n_out as u32)
    } else {
        for tri in gpu.scratch_remap.chunks_exact(3) {
            let (a, b, c) = (
                gpu.scratch_verts[tri[0] as usize],
                gpu.scratch_verts[tri[1] as usize],
                gpu.scratch_verts[tri[2] as usize],
            );
            let mut d = a;
            d.pos = std::array::from_fn(|k| b.pos[k] + c.pos[k] - a.pos[k]);
            for n in 0..8 {
                d.attr[n] = std::array::from_fn(|k| b.attr[n][k] + c.attr[n][k] - a.attr[n][k]);
            }
            gpu.frame_gverts.extend_from_slice(&[a, b, c, c, b, d]);
        }
        (gpu.frame_gverts.len() as u32 - first_vertex, 0, 0)
    };
        if let Ok(dir) = std::env::var("ORBIS_GEN_DUMP").map(|d| d).and_then(|d| if std::path::Path::new(&format!("{d}/ps_{ps_addr:x}.bin")).exists() { Err(std::env::VarError::NotPresent) } else { Ok(d) }) {
            let n = crate::rdna::decode_shader(ps_code).last().map(|i| i.pc as usize / 4 + i.words.len()).unwrap_or(0);
            let b: Vec<u8> = ps_code[..n].iter().flat_map(|w| w.to_le_bytes()).collect();
            let _ = std::fs::write(format!("{dir}/ps_{ps_addr:x}.bin"), b);
            let _ = std::fs::write(format!("{dir}/ps_{ps_addr:x}.spv"), tr.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>());
            eprintln!("[orbis] GEN slots {ps_addr:#x}: {:x?} blend={:#x} input={input_addr:#x}", info.slots, gpu.cx.get(&0x1E0).copied().unwrap_or(0));
        }
    if std::env::var_os("ORBIS_GEN_LOG").is_some() {
        static N: AtomicU64 = AtomicU64::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 60 {
            let v: Vec<String> = gpu.frame_gverts[first_vertex as usize..].iter().take(3).map(|g| format!("({:.2},{:.2},{:.2},{:.2}) a0=({:.2},{:.2},{:.2},{:.2})", g.pos[0], g.pos[1], g.pos[2], g.pos[3], g.attr[0][0], g.attr[0][1], g.attr[0][2], g.attr[0][3])).collect();
            eprintln!("[orbis] GEN ps={ps_addr:#x} prim={prim:#x} n={vertex_count} vp=({xs},{xo},{ys},{yo}) tex={tex:x?} slots={} cntl={cntl:?} {}", info.slots.len(), v.join(" "));
        }
    }
    if std::env::var_os("ORBIS_CX_LOG").is_some() {
        static N: AtomicU64 = AtomicU64::new(0);
        if N.fetch_add(1, Ordering::Relaxed) % 500 == 499 {
            let cx: Vec<String> = gpu.cx.iter().map(|(k, v)| format!("{k:x}={v:x}")).collect();
            let uc: Vec<String> = gpu.uc.iter().map(|(k, v)| format!("{k:x}={v:x}")).collect();
            eprintln!("[orbis] CX {}
[orbis] UC {}", cx.join(" "), uc.join(" "));
        }
    }
    let blend = gpu.cx.get(&0x1E0).copied().unwrap_or(0);
    let dctl = gpu.cx.get(&0x200).copied().unwrap_or(0);
    let dkey = dctl & 0x77;
    let g = gpu.frame_gens.len() as u32;
    gpu.frame_gens.push(crate::vk::GenDraw {
        key: (ps_addr ^ hash as usize, blend, wmask | (dkey << 8)),
        wmask,
        spirv: tr.clone(),
        blend,
        first_vertex,
        vertex_count,
        first_index,
        index_count,
        slots,
        tex,
        extra_texs,
        layers: if cube { 6 } else { 1 },
        rt,
        scissor,
        samp: info.images.first().map(|(_, s)| *s).unwrap_or([0; 4]),
        extra_samps,
        depth: dctl,
        // Depth is cleared through HTILE metadata we do not see: start from the far plane of
        // the compare direction (1.0 for LESS-style tests, 0.0 for GREATER-style ones).
        zclear: if matches!((dctl >> 4) & 7, 4 | 6) { 0.0 } else { 1.0 },
        vs_spirv: None,
        vs_param_off: 0,
        vs_vbs: [(0, 0); 4],
    });
    gpu.frame_batches.push(crate::vk::DrawBatch { first_index: 0, index_count: 0, tex_addr: 0, tex_w: 0, tex_h: 0, tex_swiz: 0, gen: Some(g) });
    if std::env::var_os("ORBIS_RT_LOG").is_some() {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        if (15000..15040).contains(&n) {
            eprintln!("[orbis] DRAW#{n} ps={ps_addr:#x} rt={:#x} {}x{} tex={tex:x?} cube={cube} verts={vertex_count} blend={blend:#x} mask={wmask:#x} sc={scissor:?} dctl={dctl:#x} zclear={:?} db={:x?}", rt.0, rt.1, rt.2, gpu.cx.get(&0x00B), (gpu.cx.get(&0x10), gpu.cx.get(&0x11), gpu.cx.get(&0x12), gpu.cx.get(&0x14)));
        }
    }
    GEN_STATS[0].fetch_add(1, Ordering::Relaxed);
    true
}

/// Compute shaders that can run one thread at a time (no LDS, barriers or cross-lane ops).
static CS_CACHE: LazyLock<Mutex<HashMap<usize, Option<std::sync::Arc<crate::rdna::interp::CompiledShader>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Runs a compute dispatch on the CPU, thread by thread (small dispatches only: glyph and
/// texture uploads, buffer clears and copies). Returns false to leave it to other handling.
fn cpu_dispatch(gpu: &GpuState, gx: u32, gy: u32, gz: u32) -> bool {
    let cs_addr = (((*gpu.sh.get(&0x20C).unwrap_or(&0) as u64) | ((*gpu.sh.get(&0x20D).unwrap_or(&0) as u64) << 32)) << 8) as usize;
    let tx = (*gpu.sh.get(&0x207).unwrap_or(&1) & 0xFFFF).max(1);
    let ty = (*gpu.sh.get(&0x208).unwrap_or(&1) & 0xFFFF).max(1);
    let tz = (*gpu.sh.get(&0x209).unwrap_or(&1) & 0xFFFF).max(1);
    let total = gx as u64 * gy as u64 * gz as u64 * (tx * ty * tz) as u64;
    if total == 0 || total > (1 << 17) {
        return false;
    }
    let sh = {
        let mut cache = CS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entry(cs_addr)
            .or_insert_with(|| {
                let code = shader_words(cs_addr)?;
                let c = crate::rdna::interp::CompiledShader::new(code);
                let simple = c.insts.iter().all(|i| {
                    i.fmt != crate::rdna::Fmt::Ds
                        && i.name != "s_barrier"
                        && !i.name.contains("readlane")
                        && !i.name.contains("atomic")
                        && i.ext != crate::rdna::Ext::Dpp16
                        && i.ext != crate::rdna::Ext::Dpp8
                });
                let supported_mem = c.insts.iter().all(|i| match i.fmt {
                    crate::rdna::Fmt::Flat | crate::rdna::Fmt::Global | crate::rdna::Fmt::Scratch => false,
                    _ => true,
                });
                (simple && supported_mem).then(|| std::sync::Arc::new(c))
            })
            .clone()
    };
    let Some(sh) = sh else { return false };
    if std::env::var_os("ORBIS_CS_LOG").is_some() {
        static SEEN: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());
        let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if !g.contains(&(cs_addr, total)) && g.len() < 200 {
            g.push((cs_addr, total));
            let ops: std::collections::BTreeSet<&str> = sh.insts.iter().filter(|i| matches!(i.fmt, crate::rdna::Fmt::Mimg | crate::rdna::Fmt::Mubuf | crate::rdna::Fmt::Mtbuf)).map(|i| i.name).collect();
            let ud: Vec<String> = (0..16u32).map(|k| format!("{:x}", gpu.sh.get(&(0x240 + k)).unwrap_or(&0))).collect();
            if let (Ok(dir), Some(code)) = (std::env::var("ORBIS_GEN_DUMP"), shader_words(cs_addr)) {
                let n = crate::rdna::decode_shader(code).last().map(|i| i.pc as usize / 4 + i.words.len()).unwrap_or(0);
                let b: Vec<u8> = code[..n].iter().flat_map(|w| w.to_le_bytes()).collect();
                let _ = std::fs::write(format!("{dir}/cs_{cs_addr:x}.bin"), b);
            }
            eprintln!("[orbis] CS {cs_addr:#x} groups={gx}x{gy}x{gz} threads={total} ops={ops:?} ud={}", ud.join(","));
        }
    }
    let t0 = std::time::Instant::now();
    struct Timer(std::time::Instant, u64, usize);
    impl Drop for Timer {
        fn drop(&mut self) {
            static NS: AtomicU64 = AtomicU64::new(0);
            static CNT: AtomicU64 = AtomicU64::new(0);
            let dt = self.0.elapsed().as_nanos() as u64;
            if *FRAME_STATS_ON {
                PERF_CS_NS.fetch_add(dt, Ordering::Relaxed);
                PERF_CS_CNT.fetch_add(1, Ordering::Relaxed);
                PERF_CS_THR.fetch_add(self.1, Ordering::Relaxed);
                if dt >= 20_000_000 {
                    eprintln!("[orbis] PERF slow CS {:#x}: {:.1}ms ({} threads)", self.2, dt as f64 / 1e6, self.1);
                }
            }
            let ns = NS.fetch_add(dt, Ordering::Relaxed);
            let c = CNT.fetch_add(1, Ordering::Relaxed);
            if std::env::var_os("ORBIS_CS_LOG").is_some() && c % 500 == 499 {
                eprintln!("[orbis] CPU compute: {} dispatches, {} ms total, last {} threads", c + 1, ns / 1_000_000, self.1);
            }
        }
    }
    let _timer = Timer(t0, total, cs_addr);
    let rsrc2 = *gpu.sh.get(&0x213).unwrap_or(&0);
    let user = ((rsrc2 >> 1) & 0x1F) as usize;
    let user_data: Vec<u32> = (0..user as u32).map(|k| *gpu.sh.get(&(0x240 + k)).unwrap_or(&0)).collect();
    let mut lane = crate::rdna::interp::Lane::new();
    for z in 0..gz {
        for y in 0..gy {
            for x in 0..gx {
                for k in 0..tz {
                    for j in 0..ty {
                        for i in 0..tx {
                            lane.reset();
                            lane.stores = true;
                            lane.s[..user].copy_from_slice(&user_data);
                            let mut n = user;
                            for (bit, id) in [(7, x), (8, y), (9, z)] {
                                if rsrc2 & (1 << bit) != 0 {
                                    lane.s[n] = id;
                                    n += 1;
                                }
                            }
                            if rsrc2 & (1 << 10) != 0 {
                                lane.s[n] = tx * ty * tz;
                            }
                            lane.v[0] = i;
                            lane.v[1] = j;
                            lane.v[2] = k;
                            if let Err(e) = crate::rdna::interp::run_compiled(&sh, &mut lane, mem_ok) {
                                static LOGGED: AtomicU64 = AtomicU64::new(0);
                                if LOGGED.fetch_add(1, Ordering::Relaxed) < 20 {
                                    eprintln!("[orbis] CPU compute {cs_addr:#x}: {e}");
                                }
                                CS_CACHE.lock().unwrap_or_else(|e| e.into_inner()).insert(cs_addr, None);
                                return false;
                            }
                        }
                    }
                }
            }
        }
    }
    true
}

fn compile_bound_graphics_pipeline(gpu: &GpuState) {
    let (vs_addr, ps_addr) = bound_shader_pair(gpu);
    if !is_committed(vs_addr, 64) || !is_committed(ps_addr, 64) {
        return;
    }
    let vs_words = unsafe { std::slice::from_raw_parts(vs_addr as *const u32, 64) };
    let ps_words = unsafe { std::slice::from_raw_parts(ps_addr as *const u32, 64) };
    let (_, vs_spv) = crate::shader::compile_rdna2_to_spirv(vs_words, crate::shader::ShaderStage::Vertex);
    let (ps_dec, ps_spv) = crate::shader::compile_rdna2_to_spirv(ps_words, crate::shader::ShaderStage::Fragment);
    let _ = crate::vk::with_renderer(|r| r.ensure_shader_pipeline(vs_addr, ps_addr, &vs_spv, &ps_spv, ps_dec.has_image_sample));
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
    if std::env::var_os("ORBIS_CX_LOG").is_some() {
        static SEEN: Mutex<Vec<(u64, u32)>> = Mutex::new(Vec::new());
        let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() < 40 && !g.contains(&(table_addr, count)) {
            g.push((table_addr, count));
            let raw: Vec<String> = pairs.iter().take(24).map(|[a, b]| format!("{a:x}:{b:x}")).collect();
            if pairs.first().map(|p| p[0]) == Some(0xFFFF_FFFF) {
                let before = unsafe { std::slice::from_raw_parts((table_addr as usize - 64) as *const u32, 16 + 32) };
                eprintln!("[orbis] RAW {name} @{table_addr:#x}-64: {:08x?}", before);
            }
            eprintln!("[orbis] TABLE {name} @{table_addr:#x} n={count}: {}", raw.join(" "));
        }
    }
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
                            let total = ((width as usize) * (height as usize)).min(1_000_000);
                            let sample = unsafe { std::slice::from_raw_parts(addr as *const u32, total) };
                            let nz = sample.iter().filter(|&&x| x != 0).count();
                            let first_nz = sample.iter().enumerate().find(|(_, &x)| x != 0).map(|(idx, &val)| (idx, val));
                            let swiz = (vals[3] >> 20) & 0x1F;
                            eprintln!("[orbis] GPU:   bind T# @{addr:#x} {width}x{height} fmt={:#x} swiz={swiz} nz={nz}/{total} first={first_nz:x?}", (vals[1] >> 20) & 0x3FF);
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
                            if f.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0) && f[3] > 0.0 && (f[0] < 0.99 || f[1] < 0.99 || f[2] < 0.99) {
                                gpu.clear_rgba = [f[0], f[1], f[2], f[3]];
                                crate::video::set_clear_color(gpu.clear_rgba);
                            }
                        }
                    }
                }
            }
            OP_SET_UC_REG_INDEX if len >= 3 && (w[1] >> 28) == 2 => {
                gpu.index_32bit = (w[2] & 3) == 1;
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
                compile_bound_graphics_pipeline(gpu);
                let idx_off = w[2] as usize;
                let gen_idx_cnt = (w[3] as usize).min(131_072);
                let idx_cnt = (w[3] as usize).min(2048);
                let vs_ud: [u32; 8] = std::array::from_fn(|k| *gpu.sh.get(&(0x8C + k as u32)).unwrap_or(&0));
                let ps_ud: [u32; 8] = std::array::from_fn(|k| *gpu.sh.get(&(0x0C + k as u32)).unwrap_or(&0));

                if std::env::var_os("ORBIS_VS_CPU").is_some() {
                    vs_cpu_probe(gpu, idx_off, idx_cnt);
                }
                // Decode real UI/quad geometry: MVP matrix at vs[0..2], vertex V# table at vs[4..6], u16 indices at index_base + idx_off*2
                let mvp_ptr = (vs_ud[0] as u64 | (((vs_ud[1] & 0xFFFF) as u64) << 32)) as usize;
                let vt_ptr = (vs_ud[4] as u64 | (((vs_ud[5] & 0xFFFF) as u64) << 32)) as usize;
                let ib_ptr = (gpu.index_base as usize).saturating_add(idx_off * 2);
                let generic_done = std::env::var_os("ORBIS_NO_GENERIC").is_none() && try_generic_draw(gpu, idx_off, gen_idx_cnt);
                // Legacy DrawBatch always renders onto the display RT; never let offscreen RT draws
                // (e.g. 2048x1024 animated terrain atlas updates) leak onto the main display.
                let rt_a2 = *gpu.cx.get(&0x3B0).unwrap_or(&0);
                let rt_w = ((rt_a2 >> 14) & 0x3FFF) + 1;
                let rt_h = (rt_a2 & 0x3FFF) + 1;
                let is_offscreen_atlas = *gpu.cx.get(&0x318).unwrap_or(&0) != 0 && rt_w != 3840 && rt_h != 2160 && rt_w != 1920 && rt_h != 1080;
                if !generic_done
                    && !is_offscreen_atlas
                    && idx_cnt >= 3
                    && gpu.frame_verts.len() < 12000
                    && is_committed(mvp_ptr, 64)
                    && is_committed(vt_ptr, 64)
                    && is_committed(ib_ptr, idx_cnt * 2)
                {
                    let m = unsafe { std::slice::from_raw_parts(mvp_ptr as *const f32, 16) };
                    let vt = unsafe { std::slice::from_raw_parts(vt_ptr as *const u32, 16) };
                    let vb_ptr = (vt[0] as u64 | (((vt[1] & 0xFFFF) as u64) << 32)) as usize;
                    let stride = ((vt[1] >> 16) & 0x3FFF) as usize;
                    let tint = [
                        f32::from_bits(vt[12]),
                        f32::from_bits(vt[13]),
                        f32::from_bits(vt[14]),
                        f32::from_bits(vt[15]),
                    ];
                    let has_tint = tint.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0) && tint[3] > 0.0;
                    // Check if PS binds a T# texture descriptor vs a small constant buffer
                    let ps_is_tex = (ps_ud[1] & 0x0380_0000) == 0x0380_0000;
                    let (tex_addr, tex_w, tex_h, tex_swiz) = if ps_is_tex {
                        let a = (((ps_ud[0] as u64) | (((ps_ud[1] & 0xFF) as u64) << 32)) << 8) as usize;
                        let tw = ((ps_ud[1] >> 30) | ((ps_ud[2] & 0xFFF) << 2)) + 1;
                        let th = ((ps_ud[2] >> 14) & 0x3FFF) + 1;
                        let sw = ((ps_ud[3] >> 20) & 0x1F) | (((ps_ud[1] >> 20) & 0x1FF) << 8) | ((ps_ud[3] & 0xFFF) << 20);
                        (a, tw, th, sw)
                    } else {
                        (0, 0, 0, 0)
                    };
                    // Skip final offscreen-render-target ScreenBlit (3840x2160 swiz>=24) since we draw directly to swapchain
                    let is_rt_blit = ps_is_tex && tex_w >= 1920 && tex_h >= 1080 && (tex_swiz & 0x1F) >= 24;
                    let ps_col = if !ps_is_tex && ps_ud[2] > 0 && ps_ud[2] <= 8 && (ps_ud[1] & 0xFFFF) != 0 {
                        let p = (ps_ud[0] as u64 | (((ps_ud[1] & 0xFFFF) as u64) << 32)) as usize;
                        if is_committed(p, 32) {
                            let c1 = unsafe { std::slice::from_raw_parts((p + 16) as *const f32, 4) };
                            let c0 = unsafe { std::slice::from_raw_parts(p as *const f32, 4) };
                            if c1.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0) && c1[3] > 0.0 {
                                Some([c1[0], c1[1], c1[2], c1[3]])
                            } else if c0.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0) && c0[3] > 0.0 {
                                Some([c0[0], c0[1], c0[2], c0[3]])
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if !is_rt_blit && stride >= 12 && stride <= 64 && is_committed(vb_ptr, stride * 16) {
                        let idxs = unsafe { std::slice::from_raw_parts(ib_ptr as *const u16, idx_cnt) };
                        let max_idx = idxs.iter().copied().max().unwrap_or(0) as usize;
                        if max_idx < 2048 && is_committed(vb_ptr, (max_idx + 1) * stride) {
                            // Detect packed f16x4 UI vertex (stride 12/16/20 with w at +6 == 0x3C00 == 1.0f16)
                            let is_f16 = matches!(stride, 12 | 16 | 20)
                                && unsafe { *((vb_ptr + 6) as *const u16) } == 0x3C00;
                            // Detect 16-byte font glyph quad: v[0] and v[1] share base UV at +8, local corner (0/1) in +4/+6
                            let is_font_glyph = is_f16
                                && stride == 16
                                && max_idx >= 1
                                && unsafe { *((vb_ptr + 8) as *const u32) == *((vb_ptr + 16 + 8) as *const u32) };
                            // Skip unpopulated 1024x1024 cubemap skybox cube so it doesn't cover 2D UI before cubemap support
                            let is_skybox_cube = !is_f16 && idx_cnt == 36 && tex_w == 1024 && tex_h == 1024;
                            if !is_skybox_cube {
                                let base_v = gpu.frame_verts.len() as u32;
                                let first_index = gpu.frame_idxs.len() as u32;
                                let mut any_nonzero = false;
                                for vi in 0..=max_idx {
                                    let vp = vb_ptr + vi * stride;
                                    let (x, y, z, u, v) = if is_font_glyph {
                                        unsafe {
                                            let corner = f16_to_f32(*((vp + 4) as *const u16)).round() as i32;
                                            let (du, dv) = match corner {
                                                0 => (0.0, 1.0), // bottom-left
                                                1 => (1.0, 1.0), // bottom-right
                                                2 => (1.0, 0.0), // top-right
                                                _ => (0.0, 0.0), // top-left (3)
                                            };
                                            let ub = *((vp + 8) as *const u16) as f32 / 65536.0;
                                            let vb_uv = *((vp + 10) as *const u16) as f32 / 65536.0;
                                            (
                                                f16_to_f32(*(vp as *const u16)),
                                                f16_to_f32(*((vp + 2) as *const u16)),
                                                0.0,
                                                ub + du * (1.0 / 16.0),
                                                vb_uv + dv * (1.0 / 16.0),
                                            )
                                        }
                                    } else if is_f16 {
                                        unsafe {
                                            (
                                                f16_to_f32(*(vp as *const u16)),
                                                f16_to_f32(*((vp + 2) as *const u16)),
                                                f16_to_f32(*((vp + 4) as *const u16)),
                                                *((vp + 8) as *const u16) as f32 / 65535.0,
                                                *((vp + 10) as *const u16) as f32 / 65535.0,
                                            )
                                        }
                                    } else {
                                        unsafe {
                                            (
                                                *(vp as *const f32),
                                                *((vp + 4) as *const f32),
                                                *((vp + 8) as *const f32),
                                                0.0,
                                                0.0,
                                            )
                                        }
                                    };
                                    if x != 0.0 || y != 0.0 {
                                        any_nonzero = true;
                                    }
                                    // Column-major mat4 * (x, y, z, 1)
                                    let cx = m[0] * x + m[4] * y + m[8] * z + m[12];
                                    let cy = -(m[1] * x + m[5] * y + m[9] * z + m[13]); // Vulkan Y-down
                                    let cz = (m[2] * x + m[6] * y + m[10] * z + m[14]).clamp(0.0, 1.0);
                                    let cw = m[3] * x + m[7] * y + m[11] * z + m[15];
                                    let cw = if cw.abs() < 1e-6 { 1.0 } else { cw };
                                    let col_off = if stride == 16 && is_f16 {
                                        Some(12)
                                    } else if stride >= 20 {
                                        Some(16)
                                    } else {
                                        None
                                    };
                                    let vcol = if let Some(off) = col_off {
                                        let rgba = unsafe { *((vp + off) as *const u32) };
                                        if rgba != 0 {
                                            [
                                                (rgba & 0xFF) as f32 / 255.0,
                                                ((rgba >> 8) & 0xFF) as f32 / 255.0,
                                                ((rgba >> 16) & 0xFF) as f32 / 255.0,
                                                ((rgba >> 24) & 0xFF) as f32 / 255.0,
                                            ]
                                        } else {
                                            [1.0, 1.0, 1.0, 1.0]
                                        }
                                    } else {
                                        [1.0, 1.0, 1.0, 1.0]
                                    };
                                    let col = if ps_is_tex {
                                        vcol
                                    } else if let Some(pc) = ps_col {
                                        [pc[0] * vcol[0], pc[1] * vcol[1], pc[2] * vcol[2], pc[3] * vcol[3]]
                                    } else if has_tint {
                                        [tint[0] * vcol[0], tint[1] * vcol[1], tint[2] * vcol[2], tint[3] * vcol[3]]
                                    } else {
                                        vcol
                                    };
                                    gpu.frame_verts.push(crate::vk::Vertex {
                                        pos: [cx / cw, cy / cw, cz / cw, 1.0],
                                        uv: [u, v],
                                        color: col,
                                    });
                                }
                                if any_nonzero {
                                    for &ix in idxs {
                                        gpu.frame_idxs.push(base_v + ix as u32);
                                    }
                                    gpu.frame_batches.push(crate::vk::DrawBatch {
                                        first_index,
                                        index_count: idxs.len() as u32,
                                        tex_addr,
                                        tex_w,
                                        tex_h,
                                        tex_swiz,
                                        gen: None,
                                    });
                                } else {
                                    gpu.frame_verts.truncate(base_v as usize);
                                }
                            }
                        }
                    }
                }

                if trace {
                    let ps_p = (ps_ud[0] as u64 | (((ps_ud[1] & 0xFFFF) as u64) << 32)) as usize;
                    let mut ps_f = Vec::new();
                    if (ps_ud[1] & 0x0380_0000) != 0x0380_0000 && ps_ud[2] <= 8 && is_committed(ps_p, 48) {
                        ps_f.extend_from_slice(unsafe { std::slice::from_raw_parts(ps_p as *const f32, 12) });
                    }
                    let vt = if is_committed(vt_ptr, 64) {
                        unsafe { std::slice::from_raw_parts(vt_ptr as *const u32, 4) }
                    } else {
                        &[0, 0, 0, 0]
                    };
                    let st = (vt[1] >> 16) & 0x3FFF;
                    let vb = (vt[0] as u64 | (((vt[1] & 0xFFFF) as u64) << 32)) as usize;
                    let mut vhex = Vec::new();
                    let nv = if st == 16 { 8 } else { 2 };
                    if st >= 8 && st <= 64 && is_committed(vb, (st * nv) as usize) {
                        let sl = unsafe { std::slice::from_raw_parts(vb as *const u32, ((st * nv) / 4) as usize) };
                        vhex.extend_from_slice(sl);
                    }
                    let mut ihex = Vec::new();
                    if is_committed(ib_ptr, 24) {
                        let sl = unsafe { std::slice::from_raw_parts(ib_ptr as *const u16, 12) };
                        ihex.extend_from_slice(sl);
                    }
                    let ps_tex = (ps_ud[1] & 0x0380_0000) == 0x0380_0000;
                    let tw = if ps_tex { ((ps_ud[1] >> 30) | ((ps_ud[2] & 0xFFF) << 2)) + 1 } else { 0 };
                    let th = if ps_tex { ((ps_ud[2] >> 14) & 0x3FFF) + 1 } else { 0 };
                    if st == 16 && tw == 128 && th == 128 && is_committed(vb, (w[3] as usize / 6) * 4 * 16) {
                        let ng = (w[3] as usize / 6).min(128);
                        let mut text = String::new();
                        for g in 0..ng {
                            let gp = vb + g * 4 * 16;
                            let u = unsafe { *((gp + 8) as *const u16) } >> 12;
                            let v = unsafe { *((gp + 10) as *const u16) } >> 12;
                            let ch = ((v << 4) | u) as u8;
                            text.push(if ch >= 32 && ch < 127 { ch as char } else { '?' });
                        }
                        eprintln!("[orbis] GPU:   TEXT({ng}): \"{text}\"");
                    }
                }
            }
            OP_DISPATCH_DIRECT if len >= 5 => {
                let cs_lo = *gpu.sh.get(&0x20C).unwrap_or(&0) as u64;
                let cs_hi = *gpu.sh.get(&0x20D).unwrap_or(&0) as u64;
                let cs_addr = ((cs_lo | (cs_hi << 32)) << 8) as usize;
                if cs_addr >= 0x1_0000_0000 && is_committed(cs_addr, 256) {
                    let (has_load, has_store, has_img, is_byte_copy) = *gpu.cs_cache.entry(cs_addr).or_insert_with(|| {
                        let nwords = if is_committed(cs_addr, 1024) { 256 } else { 64 };
                        let words = unsafe { std::slice::from_raw_parts(cs_addr as *const u32, nwords) };
                        let d = crate::shader::decode_rdna2(words);
                        (d.has_buffer_load, d.has_buffer_store, d.has_image_sample, d.ops.len() > 48)
                    });
                    if !has_img && has_store && !has_load {
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
                    } else if !has_img && has_load && has_store {
                        // Compute buffer copy:
                        //   s[0:3]=src V#, s[4:7]=dst V#, s[8:11]=const V#
                        //   - Dword-aligned copy (ops <= 48): { +0x00: src_off_dw, +0x10: count_dw, +0x20: dst_off_dw }
                        //   - Unaligned byte copy (ops > 48): { +0x00: count_bytes, +0x10: src_off_bytes, +0x20: dst_off_bytes }
                        if let (Some((src, sbytes)), Some((dst, dbytes))) =
                            (decode_vsharp(&gpu.sh, 0x240), decode_vsharp(&gpu.sh, 0x244))
                        {
                            let mut src_off = 0usize;
                            let mut dst_off = 0usize;
                            let mut copy_bytes = sbytes.min(dbytes);
                            if let Some((cb, cbytes)) = decode_vsharp(&gpu.sh, 0x248) {
                                if cbytes >= 36 && is_committed(cb, 36) {
                                    let f0 = f32::from_bits(unsafe { *(cb as *const u32) });
                                    let f4 = f32::from_bits(unsafe { *((cb + 0x10) as *const u32) });
                                    let f8 = f32::from_bits(unsafe { *((cb + 0x20) as *const u32) });
                                    let (so, cnt, doff) = if is_byte_copy {
                                        (f4.max(0.0) as usize, f0.max(0.0) as usize, f8.max(0.0) as usize)
                                    } else {
                                        (
                                            (f0.max(0.0) as usize).saturating_mul(4),
                                            (f4.max(0.0) as usize).saturating_mul(4),
                                            (f8.max(0.0) as usize).saturating_mul(4),
                                        )
                                    };
                                    if cnt > 0 && so + cnt <= sbytes && doff + cnt <= dbytes {
                                        src_off = so;
                                        dst_off = doff;
                                        copy_bytes = cnt;
                                    }
                                }
                            }
                            let src_at = src.saturating_add(src_off);
                            let dst_at = dst.saturating_add(dst_off);
                            if is_committed(src_at, copy_bytes) && is_committed(dst_at, copy_bytes) {
                                unsafe {
                                    std::ptr::copy(src_at as *const u8, dst_at as *mut u8, copy_bytes);
                                }
                            }
                        }
                    } else if has_img {
                        // Compute image-to-image slice copy (e.g. populating cubemap faces):
                        // s[0..7] = source T#, *(u64 at s[12..13]) = destination T#.
                        let src_t: [u32; 8] = std::array::from_fn(|k| *gpu.sh.get(&(0x240 + k as u32)).unwrap_or(&0));
                        let dst_tp = ((*gpu.sh.get(&0x24C).unwrap_or(&0) as u64)
                            | (((*gpu.sh.get(&0x24D).unwrap_or(&0) & 0xFFFF) as u64) << 32)) as usize;
                        let mut copied_fast = false;
                        if dst_tp >= 0x1_0000_0000 && is_committed(dst_tp, 32) {
                            let dst_t: [u32; 8] = unsafe { *(dst_tp as *const [u32; 8]) };
                            let decode_t = |t: &[u32; 8]| -> (usize, u32, u32, u32, u32, usize) {
                                let addr = (((t[0] as u64) | (((t[1] & 0xFF) as u64) << 32)) << 8) as usize;
                                let fmt = (t[1] >> 20) & 0x1FF;
                                let w = ((t[1] >> 30) | ((t[2] & 0xFFF) << 2)) + 1;
                                let h = ((t[2] >> 14) & 0x3FFF) + 1;
                                let swiz = (t[3] >> 20) & 0x1F;
                                let slice = ((t[4] >> 16) & 0x1FFF) as usize;
                                (addr, fmt, w, h, swiz, slice)
                            };
                            let (sa, sfmt, sw, sh, sswiz, sslice) = decode_t(&src_t);
                            let (da, dfmt, dw, dh, dswiz, dslice) = decode_t(&dst_t);
                            if sa >= 0x1_0000_0000
                                && da >= 0x1_0000_0000
                                && sfmt == dfmt
                                && sfmt >= 32
                                && sw == dw
                                && sh == dh
                                && sswiz == dswiz
                                && sw > 0
                                && sh > 0
                                && sw <= 4096
                                && sh <= 4096
                            {
                                let slice_bytes = if sswiz == 0 {
                                    (sw as usize) * (sh as usize) * 4
                                } else {
                                    (((sw as usize) + 127) / 128) * (((sh as usize) + 127) / 128) * 65536
                                };
                                let src_at = sa.saturating_add(sslice.saturating_mul(slice_bytes));
                                let dst_at = da.saturating_add(dslice.saturating_mul(slice_bytes));
                                if is_committed(src_at, slice_bytes) && is_committed(dst_at, slice_bytes) {
                                    unsafe {
                                        std::ptr::copy(src_at as *const u8, dst_at as *mut u8, slice_bytes);
                                    }
                                    copied_fast = true;
                                }
                            }
                        }
                        if !copied_fast {
                            let _ = cpu_dispatch(gpu, w[1], w[2], w[3]);
                        }
                        if std::env::var_os("ORBIS_DUMP_DRAW").is_some() {
                            static IMG_LOG: AtomicU64 = AtomicU64::new(0);
                            let il = IMG_LOG.fetch_add(1, Ordering::Relaxed);
                            if il < 8 {
                                let r240: Vec<(u32, u32)> = (0x240..0x254).filter_map(|r| gpu.sh.get(&r).map(|&v| (r, v))).collect();
                                let mut cb_vals = Vec::new();
                                if let Some((cb, cbytes)) = decode_vsharp(&gpu.sh, 0x248) {
                                    if is_committed(cb, cbytes.min(64)) {
                                        let sl = unsafe { std::slice::from_raw_parts(cb as *const u32, (cbytes / 4).min(16)) };
                                        cb_vals.extend_from_slice(sl);
                                    }
                                }
                                let mut dst_w = Vec::new();
                                if is_committed(dst_tp, 32) {
                                    let sl = unsafe { std::slice::from_raw_parts(dst_tp as *const u32, 8) };
                                    dst_w.extend_from_slice(sl);
                                }
                                eprintln!(
                                    "[orbis] GPU:   IMG_CS#{il} ({},{},{}) cs={cs_addr:#x} sh={r240:x?} cb={cb_vals:x?} dst({dst_tp:#x})={dst_w:x?}",
                                    w[1], w[2], w[3]
                                );
                            }
                        }
                    }
                }
                if trace {
                    let cs_regs: Vec<(u32, u32)> = gpu.sh.iter().filter(|(&k, _)| k >= 0x200).map(|(&k, &v)| (k, v)).collect();
                    eprintln!("[orbis] GPU:   DISPATCH ({}, {}, {}) cs={cs_addr:#x} sh200={cs_regs:x?}", w[1], w[2], w[3]);
                }
            }
            OP_NOP if std::env::var_os("ORBIS_CX_LOG").is_some() && {
                static SEEN: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
                let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
                if !g.iter().any(|&(s2, l2)| s2 == sub && l2 == len as u32) {
                    g.push((sub, len as u32));
                    let words: Vec<String> = w.iter().take(8).map(|x| format!("{x:08x}")).collect();
                    eprintln!("[orbis] NOP sub={sub:#x} len={len} {}", words.join(" "));
                }
                false
            } => {}
            OP_NOP => match sub {
                R_DRAW_INDEX_AUTO if len >= 2 => {
                    if trace {
                        eprintln!("[orbis] GPU:   DRAW_INDEX_AUTO count={}", w[1]);
                    }
                }
                R_SH_REGS_INDIRECT if len >= 4 => {
                    let tbl = ((w[2] & !3) as u64) | ((w[3] as u64) << 32);
                    apply_indirect_regs(&mut gpu.sh, tbl, w[1] & 0x3FFF, trace, "sh");
                }
                R_CX_REGS_INDIRECT if len >= 4 => {
                    let tbl = ((w[2] & !3) as u64) | ((w[3] as u64) << 32);
                    apply_indirect_regs(&mut gpu.cx, tbl, w[1] & 0x3FFF, trace, "cx");
                }
                R_UC_REGS_INDIRECT if len >= 4 => {
                    let tbl = ((w[2] & !3) as u64) | ((w[3] as u64) << 32);
                    apply_indirect_regs(&mut gpu.uc, tbl, w[1] & 0x3FFF, trace, "uc");
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
                        eprintln!("[orbis] GPU:   flip handle={handle} index={index} mode={mode} arg={arg:#x} verts={} idxs={}", gpu.frame_verts.len(), gpu.frame_idxs.len());
                    }
                    if (!gpu.frame_verts.is_empty() && !gpu.frame_idxs.is_empty()) || !gpu.frame_gens.is_empty() {
                        if let Ok(mut rd) = READY_DRAWS.lock() {
                            rd.0 = std::mem::take(&mut gpu.frame_verts);
                            rd.1 = std::mem::take(&mut gpu.frame_idxs);
                            rd.2 = std::mem::take(&mut gpu.frame_batches);
                            if let Ok(mut rg) = READY_GEN.lock() {
                                std::mem::swap(&mut rg.0, &mut gpu.frame_gverts);
                                gpu.frame_gverts.clear();
                                std::mem::swap(&mut rg.1, &mut gpu.frame_gens);
                                gpu.frame_gens.clear();
                                crate::vk::swap_gen_indices(&mut gpu.frame_gidxs);
                                crate::vk::swap_vs_params(&mut gpu.frame_vs_params);
                            }
                        }
                    } else {
                        gpu.frame_verts.clear();
                        gpu.frame_idxs.clear();
                        gpu.frame_batches.clear();
                        gpu.frame_gverts.clear();
                        gpu.frame_gidxs.clear();
                        gpu.frame_vs_params.clear();
                        gpu.frame_gens.clear();
                    }
                    gpu.pending_flip = Some((handle, index, mode, arg));
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
    let t_sub = (*FRAME_STATS_ON).then(std::time::Instant::now);
    if *FRAME_STATS_ON {
        PERF_VS_NS.store(0, Ordering::Relaxed);
        PERF_PREP_NS.store(0, Ordering::Relaxed);
        PERF_DEDUP_NS.store(0, Ordering::Relaxed);
        PERF_PS_NS.store(0, Ordering::Relaxed);
        PERF_GPU_VS_DRAWS.store(0, Ordering::Relaxed);
        PERF_GPU_VS_IDXS.store(0, Ordering::Relaxed);
        PERF_CS_NS.store(0, Ordering::Relaxed);
        PERF_CS_CNT.store(0, Ordering::Relaxed);
        PERF_CS_THR.store(0, Ordering::Relaxed);
        PERF_VS_MAP.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
    let (n, flip) = {
        flush_committed_cache();
        let mut gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
        let n = SUBMIT_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        let trace = (std::env::var_os("ORBIS_TRACE_GPU").is_some() && n <= 3)
            || (std::env::var_os("ORBIS_DUMP_DRAW").is_some() && (n == 60 || n == 600));
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
        (n, gpu.pending_flip.take())
    };
    let t_flip = (*FRAME_STATS_ON).then(std::time::Instant::now);
    if let Some((handle, index, mode, arg)) = flip {
        crate::video::submit_flip(handle, index, mode, arg);
    }
    if let Some(t0) = t_sub {
        let total_ms = t0.elapsed().as_nanos() as f64 / 1e6;
        if n % 60 == 0 || total_ms >= 50.0 {
            let vs_ms = PERF_VS_NS.load(Ordering::Relaxed) as f64 / 1e6;
            let prep_ms = PERF_PREP_NS.load(Ordering::Relaxed) as f64 / 1e6;
            let dedup_ms = PERF_DEDUP_NS.load(Ordering::Relaxed) as f64 / 1e6;
            let ps_ms = PERF_PS_NS.load(Ordering::Relaxed) as f64 / 1e6;
            let gvs_d = PERF_GPU_VS_DRAWS.load(Ordering::Relaxed);
            let gvs_i = PERF_GPU_VS_IDXS.load(Ordering::Relaxed);
            let cs_ms = PERF_CS_NS.load(Ordering::Relaxed) as f64 / 1e6;
            let cs_cnt = PERF_CS_CNT.load(Ordering::Relaxed);
            let cs_thr = PERF_CS_THR.load(Ordering::Relaxed);
            let flip_ms = t_flip.map(|tf| tf.elapsed().as_nanos() as f64 / 1e6).unwrap_or(0.0);
            eprintln!(
                "[orbis] PERF submit #{n}: total={total_ms:.1}ms vs={vs_ms:.1}ms prep={prep_ms:.1}ms dedup={dedup_ms:.1}ms ps={ps_ms:.1}ms gpu_vs={gvs_d}d/{gvs_i}i cs={cs_ms:.1}ms({cs_cnt}x/{cs_thr}thr) flip={flip_ms:.1}ms"
            );
            if total_ms >= 50.0 {
                let mut v: Vec<_> = PERF_VS_MAP.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(&a, &s)| (a, s)).collect();
                v.sort_by(|a, b| b.1 .3.cmp(&a.1 .3));
                let top: Vec<String> = v.iter().take(4).map(|(a, (d, uv, insts, ns))| {
                    format!("{a:#x}({d}d/{uv}v/{insts}i={:.1}ms)", *ns as f64 / 1e6)
                }).collect();
                if !top.is_empty() {
                    eprintln!("[orbis] PERF top VS #{n}: {}", top.join(" | "));
                }
            }
        }
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
