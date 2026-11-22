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
/// Which stubs have been reported, and the most recent calls (for crash reports).
static SEEN: Mutex<Vec<bool>> = Mutex::new(Vec::new());
static RECENT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static GAME: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// Called by stubs of functions Orbis does not implement yet.
extern "sysv64" fn unimplemented(index: u64) -> u64 {
    let name = MISSING.lock().unwrap().get(index as usize).cloned().unwrap_or_default();
    {
        let mut seen = SEEN.lock().unwrap();
        if seen.len() <= index as usize {
            seen.resize(index as usize + 1, false);
        }
        if !seen[index as usize] {
            seen[index as usize] = true;
            eprintln!("[orbis] unimplemented system function: {name}");
        }
    }
    let mut recent = RECENT.lock().unwrap();
    recent.push(name);
    if recent.len() > 8 {
        recent.remove(0);
    }
    0
}

/// Reports where the game stopped (address, offset in the game, instruction bytes, the last
/// system calls) and ends the process, instead of a silent crash.
#[cfg(windows)]
unsafe extern "system" fn on_exception(info: *mut windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    let rec = &*(*info).ExceptionRecord;
    let ctx = &*(*info).ContextRecord;
    let code = rec.ExceptionCode as u32;
    // Ignore debugger/C++ notifications; report real faults.
    if code < 0xC000_0000 {
        return 0; // EXCEPTION_CONTINUE_SEARCH
    }
    let rip = ctx.Rip as usize;
    let (base, size) = *GAME.lock().unwrap();
    let place = if rip >= base && rip < base + size { format!("game code at offset {:#x}", rip - base) } else { "outside the game image".to_string() };
    let what = match code {
        0xC000_0005 => {
            let kind = if rec.NumberParameters >= 2 { if rec.ExceptionInformation[0] == 0 { "read" } else { "write" } } else { "access" };
            format!("access violation ({kind} of address {:#x})", if rec.NumberParameters >= 2 { rec.ExceptionInformation[1] } else { 0 })
        }
        0xC000_001D => "illegal instruction".to_string(),
        0xC000_0094 => "integer division by zero".to_string(),
        0xC000_00FD => "stack overflow".to_string(),
        c => format!("exception {c:#x}"),
    };
    let bytes: Vec<String> = if rip >= base && rip + 16 <= base + size {
        (0..16).map(|i| format!("{:02x}", *(rip as *const u8).add(i))).collect()
    } else {
        vec![]
    };
    eprintln!("[orbis] the game stopped: {what}");
    eprintln!("[orbis]   at {rip:#x} ({place})");
    if !bytes.is_empty() {
        eprintln!("[orbis]   instruction bytes: {}", bytes.join(" "));
    }
    eprintln!("[orbis]   registers: rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x} rsi={:#x} rdi={:#x} rsp={:#x}", ctx.Rax, ctx.Rbx, ctx.Rcx, ctx.Rdx, ctx.Rsi, ctx.Rdi, ctx.Rsp);
    // Call chain: follow saved frame pointers (games are built with them).
    let mut frames = vec![];
    let mut rbp = ctx.Rbp as usize;
    for _ in 0..12 {
        if rbp < 0x10000 || rbp % 8 != 0 {
            break;
        }
        let ret = *((rbp + 8) as *const usize);
        if ret >= base && ret < base + size {
            frames.push(format!("{:#x}", ret - base));
        } else {
            frames.push(format!("{ret:#x}"));
        }
        let next = *(rbp as *const usize);
        if next <= rbp {
            break;
        }
        rbp = next;
    }
    if !frames.is_empty() {
        eprintln!("[orbis]   called from (game offsets): {}", frames.join(" <- "));
    }
    let recent = RECENT.lock().map(|r| r.clone()).unwrap_or_default();
    if !recent.is_empty() {
        eprintln!("[orbis]   last system calls: {}", recent.join(" → "));
    }
    windows_sys::Win32::System::Threading::ExitProcess(3);
}

#[cfg(windows)]
pub fn install_crash_report() {
    unsafe { windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler(1, Some(on_exception)) };
}

#[cfg(not(windows))]
pub fn install_crash_report() {}

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
    let mut by_symbol: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();

    for (offset, kind, sym, addend) in relocs {
        let target = unsafe { base.add(offset as usize) as *mut u64 };
        let value = match kind {
            R_X86_64_RELATIVE => base as u64 + addend as u64,
            R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                let name = exe.symbol_name(sym).unwrap_or_default();
                let nid = name.split('#').next().unwrap_or("").to_string();
                if let Some(&stub) = by_symbol.get(&sym) {
                    stub + if kind == R_X86_64_64 { addend as u64 } else { 0 }
                } else if let Some(&(_, addr)) = table.get(&nid) {
                    by_symbol.insert(sym, addr as u64);
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
                    by_symbol.insert(sym, stub as u64);
                    stub as u64
                }
            }
            _ => continue,
        };
        unsafe { target.write_unaligned(value) };
    }
    *GAME.lock().unwrap() = (base as usize, size);
    let tls_size = crate::tls::setup(exe, base)?;
    let fs_patched = crate::tls::patch_fs_accesses(exe, base);
    eprintln!("[orbis] thread-local storage: {tls_size} bytes at {:#x}, {fs_patched} fs: accesses rewritten", crate::tls::THREAD_POINTER);
    Ok(Loaded { base, entry: base as u64 + exe.entry, bound, stubbed: stub_count })
}

extern "sysv64" fn exit_handler() {}

/// Calls the entry point: entry(args, exit_handler), System V convention.
pub fn run(loaded: &Loaded) -> i32 {
    let mut args = [0u64; 32];
    let entry: extern "sysv64" fn(*mut u64, extern "sysv64" fn()) -> i32 = unsafe { std::mem::transmute(loaded.entry as usize) };
    entry(args.as_mut_ptr(), exit_handler)
}
