//! Thread-local storage.
//!
//! PS4/PS5 code (FreeBSD x86-64 ABI) finds its thread data through the `fs` segment register:
//! `mov rax, fs:[0]` loads the thread pointer, and variables live just below it. Windows does
//! not let a program point `fs` at its own data, so Orbis
//!   1. places the thread control block at a fixed address below 2 GB (THREAD_POINTER),
//!   2. copies the game's TLS template (PT_TLS) right below it,
//!   3. rewrites every `fs:[disp32]` access in the game's code into a plain memory access to
//!      `[THREAD_POINTER + disp32]` (same length: the 0x64 prefix is dropped, a NOP pads).
//! Single-threaded for now: every thread shares this block.

use crate::format::{Executable, PT_LOAD, PT_TLS};

pub const THREAD_POINTER: usize = 0x1000_0000;

#[cfg(windows)]
fn alloc_at(addr: usize, size: usize) -> *mut u8 {
    use windows_sys::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};
    unsafe { VirtualAlloc(addr as *const _, size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) as *mut u8 }
}
#[cfg(not(windows))]
fn alloc_at(_addr: usize, _size: usize) -> *mut u8 {
    std::ptr::null_mut()
}

/// Sets up the TLS block from the template. Returns the size of the TLS area.
pub fn setup(exe: &Executable, base: *mut u8) -> Result<usize, String> {
    let (tls_size, image, align) = match exe.segment(PT_TLS) {
        Some(p) => (p.memsz as usize, unsafe { std::slice::from_raw_parts(base.add(p.vaddr as usize), p.filesz as usize) }.to_vec(), p.align.max(16) as usize),
        None => (0, vec![], 16),
    };
    let aligned = (tls_size + align - 1) / align * align;
    let below = ((aligned + 0xFFFF) / 0x10000) * 0x10000;
    let start = THREAD_POINTER - below.max(0x10000);
    let total = (THREAD_POINTER - start) + 0x10000;
    // Reserved once per process (the self-test loads two executables).
    static RESERVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let have = RESERVED.load(std::sync::atomic::Ordering::SeqCst);
    if have == 0 || have > start {
        let mem = alloc_at(start, total);
        if mem.is_null() || mem as usize != start {
            return Err(format!("could not reserve thread-local storage at {start:#x}"));
        }
        RESERVED.store(start, std::sync::atomic::Ordering::SeqCst);
    }
    unsafe {
        let data = (THREAD_POINTER - aligned) as *mut u8;
        std::ptr::copy_nonoverlapping(image.as_ptr(), data, image.len());
        // Thread control block: first field points to itself (what `fs:[0]` returns).
        *(THREAD_POINTER as *mut u64) = THREAD_POINTER as u64;
    }
    Ok(aligned)
}

/// Extra bytes after ModRM/SIB/disp32 for opcodes that carry an immediate.
fn immediate_len(op: u8) -> Option<usize> {
    match op {
        0x8B | 0x89 | 0x03 | 0x01 | 0x2B | 0x29 | 0x33 | 0x31 | 0x3B | 0x39 | 0x8A | 0x88 | 0x0B | 0x09 | 0x23 | 0x21 | 0x85 => Some(0),
        0x80 | 0x83 | 0xC6 => Some(1),
        0x81 | 0xC7 => Some(4),
        _ => None,
    }
}

/// Rewrites `fs:[disp32]` accesses in executable segments. Returns how many were patched.
pub fn patch_fs_accesses(exe: &Executable, base: *mut u8) -> usize {
    let mut patched = 0;
    for p in exe.phdrs.iter().filter(|p| p.kind == PT_LOAD && p.flags & 1 != 0) {
        let code = unsafe { std::slice::from_raw_parts_mut(base.add(p.vaddr as usize), p.filesz as usize) };
        let mut i = 0;
        while i + 10 < code.len() {
            if code[i] != 0x64 {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            if (0x40..=0x4F).contains(&code[j]) {
                j += 1;
            }
            let op = code[j];
            let (modrm, sib) = (code[j + 1], code[j + 2]);
            let Some(imm) = immediate_len(op) else {
                i += 1;
                continue;
            };
            // ModRM with mod=00, rm=100 (SIB follows) and SIB 0x25: absolute disp32, no base/index.
            if modrm & 0xC7 != 0x04 || sib != 0x25 {
                i += 1;
                continue;
            }
            let disp = i32::from_le_bytes(code[j + 3..j + 7].try_into().unwrap()) as i64;
            let new_disp = THREAD_POINTER as i64 + disp;
            if new_disp < 0 || new_disp > i32::MAX as i64 {
                i += 1;
                continue;
            }
            let end = j + 7 + imm; // one past the instruction
            // Drop the fs prefix: shift the rest left by one byte, fix the address, pad with NOP.
            code.copy_within(i + 1..end, i);
            let d = j - 1 + 3;
            code[d..d + 4].copy_from_slice(&(new_disp as i32).to_le_bytes());
            code[end - 1] = 0x90;
            patched += 1;
            i = end;
        }
    }
    patched
}
