//! Loads a PS4 executable into memory and runs it.
//!
//! Steps: map the loadable segments, apply relocations, bind every imported function either
//! to a native implementation (hle.rs) or to a stub that reports the missing function, then
//! call the entry point. PS4 and PC share the x86-64 instruction set, so the game's own code
//! runs directly; only the system it talks to is emulated.

use std::sync::Mutex;

use crate::format::{Executable, PT_LOAD, PT_SCE_RELRO};
use crate::hle;

const R_X86_64_64: u32 = 1;
const R_X86_64_GLOB_DAT: u32 = 6;
const R_X86_64_JUMP_SLOT: u32 = 7;
const R_X86_64_RELATIVE: u32 = 8;

static MISSING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Called by stubs of functions Orbis does not implement yet.
extern "sysv64" fn unimplemented(index: u64) -> u64 {
    let name = MISSING.lock().unwrap().get(index as usize).cloned().unwrap_or_default();
    eprintln!("[orbis] unimplemented system function: {name}");
    0
}

#[cfg(windows)]
fn alloc_rwx(size: usize) -> *mut u8 {
    use windows_sys::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE};
    unsafe { VirtualAlloc(std::ptr::null(), size, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as *mut u8 }
}

#[cfg(not(windows))]
fn alloc_rwx(_size: usize) -> *mut u8 {
    std::ptr::null_mut()
}

pub struct Loaded {
    pub base: *mut u8,
    pub entry: u64,
    pub bound: usize,
    pub stubbed: usize,
}

pub fn load(exe: &Executable) -> Result<Loaded, String> {
    let segs: Vec<_> = exe.phdrs.iter().filter(|p| p.kind == PT_LOAD || p.kind == PT_SCE_RELRO).collect();
    if segs.is_empty() {
        return Err("no loadable segments".into());
    }
    let end = segs.iter().map(|p| p.vaddr + p.memsz).max().unwrap();
    let size = ((end + 0xFFFF) & !0xFFFF) as usize;
    let base = alloc_rwx(size);
    if base.is_null() {
        return Err("could not reserve memory for the executable".into());
    }
    for p in &segs {
        let src = exe.elf.get(p.offset as usize..(p.offset + p.filesz) as usize).ok_or("segment outside file")?;
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(p.vaddr as usize), src.len()) };
    }

    // Stubs: mov rdi, index ; mov rax, handler ; jmp rax  (22 bytes, padded to 32)
    let relocs = exe.relocations();
    let stubs = alloc_rwx(relocs.len().max(1) * 32);
    let mut stub_count = 0usize;
    let mut bound = 0usize;
    let table = hle::table();

    for (offset, kind, sym, addend) in relocs {
        let target = unsafe { base.add(offset as usize) as *mut u64 };
        let value = match kind {
            R_X86_64_RELATIVE => base as u64 + addend as u64,
            R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                let name = exe.symbol_name(sym).unwrap_or_default();
                let nid = name.split('#').next().unwrap_or("").to_string();
                if let Some(&(_, addr)) = table.get(&nid) {
                    bound += 1;
                    addr as u64 + if kind == R_X86_64_64 { addend as u64 } else { 0 }
                } else {
                    let label = crate::known::names().get(&nid).map(|n| format!("{n} ({nid})")).unwrap_or(nid);
                    let index = {
                        let mut m = MISSING.lock().unwrap();
                        m.push(label);
                        (m.len() - 1) as u64
                    };
                    let stub = unsafe { stubs.add(stub_count * 32) };
                    stub_count += 1;
                    let mut code = Vec::with_capacity(32);
                    code.extend_from_slice(&[0x48, 0xBF]);
                    code.extend_from_slice(&index.to_le_bytes());
                    code.extend_from_slice(&[0x48, 0xB8]);
                    code.extend_from_slice(&(unimplemented as usize as u64).to_le_bytes());
                    code.extend_from_slice(&[0xFF, 0xE0]);
                    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub, code.len()) };
                    stub as u64
                }
            }
            _ => continue,
        };
        unsafe { target.write_unaligned(value) };
    }
    Ok(Loaded { base, entry: base as u64 + exe.entry, bound, stubbed: stub_count })
}

extern "sysv64" fn exit_handler() {}

/// Calls the entry point: entry(args, exit_handler), System V convention.
pub fn run(loaded: &Loaded) -> i32 {
    let mut args = [0u64; 32];
    let entry: extern "sysv64" fn(*mut u64, extern "sysv64" fn()) -> i32 = unsafe { std::mem::transmute(loaded.entry as usize) };
    entry(args.as_mut_ptr(), exit_handler)
}
