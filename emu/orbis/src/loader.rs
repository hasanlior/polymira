//! Loads a PS4 executable into memory and runs it.
//!
//! Steps: map the loadable segments, apply relocations, bind every imported function either
//! to a native implementation (hle.rs) or to a stub that reports the missing function, then
//! call the entry point. PS4 and PC share the x86-64 instruction set, so the game's own code
//! runs directly; only the system it talks to is emulated.

use std::sync::Mutex;

use crate::format::{Executable, PT_LOAD, PT_SCE_PROCPARAM, PT_SCE_RELRO, PT_TLS};
use crate::hle;

const R_X86_64_64: u32 = 1;
const R_X86_64_GLOB_DAT: u32 = 6;
const R_X86_64_JUMP_SLOT: u32 = 7;
const R_X86_64_RELATIVE: u32 = 8;

static MISSING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Which stubs have been reported, call counts, and the most recent calls (for crash reports).
static SEEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static RECENT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static GAME: Mutex<(usize, usize)> = Mutex::new((0, 0));
static TRACE_STUBS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Called by stubs of functions Orbis does not implement yet.
extern "sysv64" fn unimplemented(index: u64) -> u64 {
    let idx = index as usize;
    let count = {
        let mut seen = SEEN.lock().unwrap();
        if seen.len() <= idx {
            seen.resize(idx + 1, 0);
        }
        seen[idx] += 1;
        seen[idx]
    };
    let trace = *TRACE_STUBS.get_or_init(|| std::env::var_os("ORBIS_TRACE").is_some());
    if count == 1 || count == 50_000 || trace {
        let name = MISSING.lock().unwrap().get(idx).cloned().unwrap_or_default();
        if count == 1 {
            eprintln!("[orbis] unimplemented system function: {name}");
        } else if count == 50_000 {
            eprintln!("[orbis] HOT STUB (50000 calls): {name}");
        }
        if trace {
            eprintln!("[orbis] call {name}");
        }
        let mut recent = RECENT.lock().unwrap();
        recent.push(name);
        if recent.len() > 8 {
            recent.remove(0);
        }
    }
    0
}

/// Whether `addr..addr+8` can be read (the crash report must never fault itself).
#[cfg(windows)]
fn readable(addr: usize) -> bool {
    use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS};
    if addr < 0x10000 {
        return false;
    }
    let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(addr as *const _, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    n != 0 && mbi.State == MEM_COMMIT && mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0 && addr + 16 <= mbi.BaseAddress as usize + mbi.RegionSize
}

/// Emulates AMD SSE4a bit-field instructions (`INSERTQ` / `EXTRQ`) missing on Intel CPUs.
/// Returns `Some(instr_len)` if the bytes at `code` are a supported SSE4a instruction and
/// updates `xmm[0..16]` (as `(low_u64, high_u64)` pairs) in place.
fn emulate_sse4a(code: &[u8], xmm: &mut [(u64, u64); 16]) -> Option<usize> {
    if code.len() < 4 {
        return None;
    }
    let prefix = code[0];
    if prefix != 0xF2 && prefix != 0x66 {
        return None;
    }
    let mut i = 1usize;
    let mut rex = 0u8;
    if i < code.len() && (code[i] & 0xF0) == 0x40 {
        rex = code[i];
        i += 1;
    }
    if i + 3 > code.len() || code[i] != 0x0F {
        return None;
    }
    let op = code[i + 1];
    if op != 0x78 && op != 0x79 {
        return None;
    }
    let modrm = code[i + 2];
    if (modrm >> 6) != 3 {
        return None;
    }
    i += 3;

    let reg = ((((rex >> 2) & 1) << 3) | ((modrm >> 3) & 7)) as usize;
    let rm = (((rex & 1) << 3) | (modrm & 7)) as usize;

    let mask_for = |len_raw: u8| -> u64 {
        let n = (len_raw & 0x3F) as u32;
        if n == 0 || n >= 64 { u64::MAX } else { (1u64 << n) - 1 }
    };

    match (prefix, op) {
        // INSERTQ xmm1(reg), xmm2(rm), imm8(len), imm8(idx)
        (0xF2, 0x78) => {
            if i + 2 > code.len() {
                return None;
            }
            let len = code[i];
            let idx = (code[i + 1] & 0x3F) as u32;
            let mask = mask_for(len);
            let src_lo = xmm[rm].0;
            let dst_lo = xmm[reg].0;
            xmm[reg].0 = (dst_lo & !mask.wrapping_shl(idx)) | ((src_lo & mask).wrapping_shl(idx));
            Some(i + 2)
        }
        // INSERTQ xmm1(reg), xmm2(rm)  (len = xmm2[69:64], idx = xmm2[77:72])
        (0xF2, 0x79) => {
            let src_lo = xmm[rm].0;
            let src_hi = xmm[rm].1;
            let len = (src_hi & 0x3F) as u8;
            let idx = ((src_hi >> 8) & 0x3F) as u32;
            let mask = mask_for(len);
            let dst_lo = xmm[reg].0;
            xmm[reg].0 = (dst_lo & !mask.wrapping_shl(idx)) | ((src_lo & mask).wrapping_shl(idx));
            Some(i)
        }
        // EXTRQ xmm1(rm), imm8(len), imm8(idx)
        (0x66, 0x78) if ((modrm >> 3) & 7) == 0 => {
            if i + 2 > code.len() {
                return None;
            }
            let len = code[i];
            let idx = (code[i + 1] & 0x3F) as u32;
            let mask = mask_for(len);
            xmm[rm].0 = xmm[rm].0.wrapping_shr(idx) & mask;
            Some(i + 2)
        }
        // EXTRQ xmm1(reg), xmm2(rm)  (len = xmm2[5:0], idx = xmm2[13:8])
        (0x66, 0x79) => {
            let src_lo = xmm[rm].0;
            let len = (src_lo & 0x3F) as u8;
            let idx = ((src_lo >> 8) & 0x3F) as u32;
            let mask = mask_for(len);
            xmm[reg].0 = xmm[reg].0.wrapping_shr(idx) & mask;
            Some(i)
        }
        _ => None,
    }
}

/// Reports where the game stopped (address, offset in the game, instruction bytes, the last
/// system calls) and ends the process, instead of a silent crash.
#[cfg(windows)]
unsafe extern "system" fn on_exception(info: *mut windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    let rec = &*(*info).ExceptionRecord;
    let ctx = &mut *(*info).ContextRecord;
    let code = rec.ExceptionCode as u32;
    // Emulate AMD SSE4a (INSERTQ / EXTRQ) on Intel CPUs transparently.
    if code == 0xC000_001D {
        let rip = ctx.Rip as usize;
        if readable(rip) {
            let bytes = std::slice::from_raw_parts(rip as *const u8, 8);
            let mut xmm = [(0u64, 0u64); 16];
            for r in 0..16 {
                let m = ctx.Anonymous.FltSave.XmmRegisters[r];
                xmm[r] = (m.Low, m.High as u64);
            }
            if let Some(len) = emulate_sse4a(bytes, &mut xmm) {
                for r in 0..16 {
                    ctx.Anonymous.FltSave.XmmRegisters[r].Low = xmm[r].0;
                    ctx.Anonymous.FltSave.XmmRegisters[r].High = xmm[r].1 as i64;
                }
                ctx.Rip += len as u64;
                return -1; // EXCEPTION_CONTINUE_EXECUTION
            }
        }
    }
    // Ignore debugger/C++ exceptions (0xE06D7363 = MSVC C++, used by Vulkan/NVIDIA drivers); report real faults.
    if code < 0xC000_0000 || code == 0xE06D_7363 || code == 0xE043_4352 {
        return 0; // EXCEPTION_CONTINUE_SEARCH
    }
    let rip = ctx.Rip as usize;
    {
        // First, a minimal line that needs no locks or allocation.
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        let mut line = *b"[orbis] fault at 0x0000000000000000
";
        let hex = b"0123456789abcdef";
        for i in 0..16 {
            line[19 + i] = hex[(rip >> ((15 - i) * 4)) & 0xF];
        }
        let mut n = 0u32;
        WriteFile(GetStdHandle(STD_ERROR_HANDLE), line.as_ptr(), line.len() as u32, &mut n, std::ptr::null_mut());
    }
    // A fault inside this report ends the process quietly instead of recursing.
    static IN_REPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if IN_REPORT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        windows_sys::Win32::System::Threading::ExitProcess(3);
    }
    // Never use eprintln! here: the faulting thread may be inside a write to stderr already.
    use std::fmt::Write as _;
    let mut out = String::new();
    let (base, size) = GAME.try_lock().map(|g| *g).unwrap_or((0, 0));
    let place = describe(rip);
    let what = match code {
        0xC000_0005 => {
            let kind = if rec.NumberParameters >= 2 { if rec.ExceptionInformation[0] == 0 { "read" } else if rec.ExceptionInformation[0] == 8 { "execute" } else { "write" } } else { "access" };
            format!("access violation ({kind} of address {:#x})", if rec.NumberParameters >= 2 { rec.ExceptionInformation[1] } else { 0 })
        }
        0xC000_001D => "illegal instruction".to_string(),
        0xC000_0094 => "integer division by zero".to_string(),
        0xC000_00FD => "stack overflow".to_string(),
        c => format!("exception {c:#x}"),
    };
    let known = IMAGES.try_lock().map(|v| v.iter().any(|(_, b, s)| rip >= *b && rip + 16 <= b + s)).unwrap_or(false);
    let _ = writeln!(out, "[orbis] the game stopped: {what}");
    let _ = writeln!(out, "[orbis]   at {rip:#x} ({place})");
    if (known || (rip >= base && rip + 16 <= base + size)) && readable(rip) {
        let bytes: Vec<String> = (0..16).map(|i| format!("{:02x}", *(rip as *const u8).add(i))).collect();
        let _ = writeln!(out, "[orbis]   instruction bytes: {}", bytes.join(" "));
    }
    let _ = writeln!(out, "[orbis]   registers: rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x} rsi={:#x} rdi={:#x} rsp={:#x} rbp={:#x}", ctx.Rax, ctx.Rbx, ctx.Rcx, ctx.Rdx, ctx.Rsi, ctx.Rdi, ctx.Rsp, ctx.Rbp);
    // A jump to a bad address (null function pointer): the caller's return address is at [rsp].
    if !readable(rip) && readable(ctx.Rsp as usize) {
        let ret = *(ctx.Rsp as usize as *const usize);
        let _ = writeln!(out, "[orbis]   jumped here from the call before {}", describe(ret));
    }
    // Call chain: follow saved frame pointers (games are built with them).
    let mut frames = vec![];
    let mut rbp = ctx.Rbp as usize;
    for _ in 0..12 {
        if rbp % 8 != 0 || !readable(rbp) {
            break;
        }
        let ret = *((rbp + 8) as *const usize);
        frames.push(describe(ret));
        let next = *(rbp as *const usize);
        if next <= rbp {
            break;
        }
        rbp = next;
    }
    if !frames.is_empty() {
        let _ = writeln!(out, "[orbis]   called from: {}", frames.join(" <- "));
    }
    if let Ok(r) = RECENT.try_lock() {
        if !r.is_empty() {
            let _ = writeln!(out, "[orbis]   last system calls: {}", r.join(" → "));
        }
    }
    {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        let mut n = 0u32;
        WriteFile(GetStdHandle(STD_ERROR_HANDLE), out.as_ptr(), out.len() as u32, &mut n, std::ptr::null_mut());
    }
    windows_sys::Win32::System::Threading::ExitProcess(3);
}

#[cfg(windows)]
unsafe extern "system" fn on_unhandled(info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    on_exception(info as *mut _)
}

/// Reserves stack for the crash report on the current thread (stack overflows).
/// Also commits the whole stack up front: console code uses large stack frames without
/// probing each page, which would jump over Windows' guard page.
pub fn install_thread_guard() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, PAGE_READWRITE};
        let (mut low, mut high) = (0usize, 0usize);
        windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits(&mut low, &mut high);
        let keep = 0x10000; // leave the bottom as Windows' overflow guard
        if high > low + keep * 2 {
            VirtualAlloc((low + keep) as *const _, high - low - keep, MEM_COMMIT, PAGE_READWRITE);
        }
        let mut guarantee: u32 = 0x8000;
        windows_sys::Win32::System::Threading::SetThreadStackGuarantee(&mut guarantee);
    }
}

#[cfg(windows)]
pub fn install_crash_report() {
    unsafe {
        let mut guarantee: u32 = 0x10000;
        windows_sys::Win32::System::Threading::SetThreadStackGuarantee(&mut guarantee);
        windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler(1, Some(on_exception));
        windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(on_unhandled));
    }
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
    /// Modules the game brought with it (sce_module/*.prx, *.prx next to eboot.bin).
    pub modules: Vec<String>,
    /// (module name, init function) in the order they must run before the game's entry.
    inits: Vec<(String, u64)>,
}

/// Every mapped image: (name, base, size), for crash reports.
static IMAGES: Mutex<Vec<(String, usize, usize)>> = Mutex::new(Vec::new());
/// Thread-local blocks, indexed by TLS module id (dynamic TLS model, __tls_get_addr).
static TLS_BLOCKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

const R_X86_64_DTPMOD64: u32 = 16;
const R_X86_64_DTPOFF64: u32 = 17;
const DT_INIT: i64 = 0xC;

/// Module TLS templates (initial image, size), by TLS id; ids 0 and 1 are unused here.
static TLS_TEMPLATES: Mutex<Vec<(Vec<u8>, usize)>> = Mutex::new(Vec::new());

thread_local! {
    /// This thread's blocks for module TLS (the main thread uses TLS_BLOCKS).
    static MY_BLOCKS: std::cell::RefCell<std::collections::HashMap<u64, usize>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// `void* __tls_get_addr(tls_index*)`: { module id, offset }.
pub extern "sysv64" fn tls_get_addr(ti: *const u64) -> usize {
    let (m, off) = unsafe { (*ti, *ti.add(1)) };
    let tp = crate::tls::current();
    if m == 1 {
        return tp - crate::tls::static_size() + off as usize;
    }
    if tp == crate::tls::THREAD_POINTER {
        return TLS_BLOCKS.lock().unwrap().get(m as usize).copied().unwrap_or(0) + off as usize;
    }
    let base = MY_BLOCKS.with(|b| {
        *b.borrow_mut().entry(m).or_insert_with(|| {
            let t = TLS_TEMPLATES.lock().unwrap();
            let (image, size) = t.get(m as usize).cloned().unwrap_or_default();
            let block = Box::leak(vec![0u8; size + 0x40].into_boxed_slice()).as_mut_ptr();
            unsafe { std::ptr::copy_nonoverlapping(image.as_ptr(), block, image.len()) };
            block as usize
        })
    });
    base + off as usize
}

/// Where an address belongs: "libc.prx+0x1234", or the plain address.
pub fn describe(addr: usize) -> String {
    if let Ok(images) = IMAGES.try_lock() {
        for (name, base, size) in images.iter() {
            if addr >= *base && addr < base + size {
                return format!("{name}+{:#x}", addr - base);
            }
        }
    }
    format!("{addr:#x}")
}

/// Exports of the game's modules by NID (for Orbis functions that wrap them).
static EXPORTS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, u64>>> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

pub fn export_addr(name: &str) -> Option<usize> {
    EXPORTS.lock().ok()?.get(&crate::nid::nid(name)).map(|&a| a as usize)
}

/// Every mapped image (name, base, size), sorted by base.
pub fn images() -> Vec<(String, usize, usize)> {
    let mut v = IMAGES.lock().map(|v| v.clone()).unwrap_or_default();
    v.sort_by_key(|x| x.1);
    v
}

/// (base, size) of a mapped module, by file name.
pub fn image(name: &str) -> Option<(usize, usize)> {
    IMAGES.lock().ok()?.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name)).map(|(_, b, s)| (*b, *s))
}

struct Image {
    name: String,
    exe: Executable,
    base: *mut u8,
    size: usize,
}

fn map(name: &str, exe: Executable) -> Result<Image, String> {
    let segs: Vec<_> = exe.phdrs.iter().filter(|p| p.kind == PT_LOAD || p.kind == PT_SCE_RELRO).cloned().collect();
    if segs.is_empty() {
        return Err(format!("{name}: no loadable segments"));
    }
    let end = segs.iter().map(|p| p.vaddr + p.memsz).max().unwrap();
    let size = ((end + 0xFFFF) & !0xFFFF) as usize;
    let base = alloc_rwx(size);
    if base.is_null() {
        return Err(format!("{name}: could not reserve memory"));
    }
    for p in &segs {
        let src = exe.elf.get(p.offset as usize..(p.offset + p.filesz) as usize).ok_or("segment outside file")?;
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(p.vaddr as usize), src.len()) };
    }
    IMAGES.lock().unwrap().push((name.to_string(), base as usize, size));
    Ok(Image { name: name.to_string(), exe, base, size })
}

/// Modules shipped with the game: *.prx in sce_module/ and next to eboot.bin. libc first.
fn game_modules(main: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    let Some(dir) = main.parent() else { return out };
    for d in [dir.join("sce_module"), dir.to_path_buf()] {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut v: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("prx")).unwrap_or(false)).collect();
        v.sort();
        out.extend(v);
    }
    out.sort_by_key(|p| !p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case("libc.prx")).unwrap_or(false));
    out
}

/// Loads only the executable (no game modules).
pub fn load(exe: Executable) -> Result<Loaded, String> {
    load_with_modules(exe, None)
}

/// Loads the executable and the modules in its folder, links them together and to Orbis.
pub fn load_with_modules(main_exe: Executable, main_path: Option<&std::path::Path>) -> Result<Loaded, String> {
    let mut images = vec![map("eboot.bin", main_exe)?];
    if let Some(path) = main_path {
        for m in game_modules(path) {
            let name = m.file_name().unwrap().to_string_lossy().to_string();
            match std::fs::read(&m).map_err(|e| e.to_string()).and_then(|b| Executable::parse(&b).map_err(|e| e.to_string())) {
                Ok(exe) => images.push(map(&name, exe)?),
                Err(e) => eprintln!("[orbis] module {name} skipped: {e}"),
            }
        }
    }

    // Exports of the game's own modules (libc, fmod, …), by NID.
    let mut exports: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for img in images.iter().skip(1) {
        for (nid, value) in img.exe.exports() {
            exports.entry(nid).or_insert(img.base as u64 + value);
        }
    }

    *EXPORTS.lock().unwrap() = exports.clone();
    let table = hle::table();
    let total_relocs: usize = images.iter().map(|i| i.exe.relocations().len()).sum();
    let stubs = alloc_rwx(total_relocs.max(1) * 32);
    let mut stub_count = 0usize;
    let mut bound = 0usize;
    let mut stub_of: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut data_of: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    // TLS ids: 1 = the main executable's static block below the thread pointer; modules get
    // their own blocks (dynamic model, __tls_get_addr).
    *TLS_BLOCKS.lock().unwrap() = vec![0, 0];

    let mut inits = vec![];
    for (index, img) in images.iter().enumerate() {
        let base = img.base;
        let exe = &img.exe;
        let tls_id = if index == 0 {
            1u64
        } else if let Some(t) = exe.segment(PT_TLS) {
            let block = Box::leak(vec![0u8; t.memsz as usize + 0x40].into_boxed_slice()).as_mut_ptr();
            unsafe { std::ptr::copy_nonoverlapping(base.add(t.vaddr as usize), block, t.filesz as usize) };
            let mut b = TLS_BLOCKS.lock().unwrap();
            b.push(block as usize);
            let mut tt = TLS_TEMPLATES.lock().unwrap();
            tt.resize(b.len() - 1, (vec![], 0));
            tt.push((unsafe { std::slice::from_raw_parts(base.add(t.vaddr as usize), t.filesz as usize) }.to_vec(), t.memsz as usize));
            (b.len() - 1) as u64
        } else {
            0
        };
        let mut unsupported = std::collections::BTreeMap::new();
        for (offset, kind, sym, addend) in exe.relocations() {
            let target = unsafe { base.add(offset as usize) as *mut u64 };
            let value = match kind {
                R_X86_64_RELATIVE => base as u64 + addend as u64,
                R_X86_64_DTPMOD64 => tls_id,
                R_X86_64_DTPOFF64 => exe.symbol(sym).2.wrapping_add(addend as u64),
                R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                    let add = if kind == R_X86_64_64 { addend as u64 } else { 0 };
                    let (info, shndx, st_value) = exe.symbol(sym);
                    if shndx != 0 {
                        base as u64 + st_value + add // defined in this image
                    } else {
                        let name = exe.symbol_name(sym).unwrap_or_default();
                        let nid = name.split('#').next().unwrap_or("").to_string();
                        if let Some(&(_, addr)) = table.get(&nid).filter(|_| hle::forced(&nid)) {
                            bound += 1;
                            addr as u64 + add
                        } else if let Some(&addr) = exports.get(&nid) {
                            bound += 1;
                            addr + add
                        } else if let Some(&(_, addr)) = table.get(&nid) {
                            bound += 1;
                            addr as u64 + add
                        } else if info & 0xF == 1 {
                            // Data object nobody provides: zeroed memory, never code.
                            let block = *data_of.entry(nid.clone()).or_insert_with(|| {
                                let label = crate::known::names().get(&nid).map(|n| n.to_string()).unwrap_or_else(|| nid.clone());
                                eprintln!("[orbis] {}: data import {label} not provided, zeroed", img.name);
                                Box::leak(vec![0u64; 0x200].into_boxed_slice()).as_mut_ptr() as u64
                            });
                            block + add
                        } else {
                            let stub = *stub_of.entry(nid.clone()).or_insert_with(|| {
                                let label = crate::known::names().get(&nid).map(|n| format!("{n} ({nid})")).unwrap_or(nid.clone());
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
                            });
                            stub + add
                        }
                    }
                }
                other => {
                    *unsupported.entry(other).or_insert(0usize) += 1;
                    continue;
                }
            };
            unsafe { target.write_unaligned(value) };
        }
        if !unsupported.is_empty() {
            eprintln!("[orbis] {}: unsupported relocation types {:?}", img.name, unsupported);
        }
        let fs_patched = crate::tls::patch_fs_accesses(exe, base);
        if index == 0 {
            *GAME.lock().unwrap() = (base as usize, img.size);
            if let Some(p) = exe.segment(PT_SCE_PROCPARAM) {
                unsafe { crate::kernel::PROC_PARAM = base as usize + p.vaddr as usize };
            }
            let tls_size = crate::tls::setup(exe, base)?;
            TLS_BLOCKS.lock().unwrap()[1] = crate::tls::THREAD_POINTER - tls_size;
            eprintln!("[orbis] thread-local storage: {tls_size} bytes at {:#x}, {fs_patched} fs: accesses rewritten", crate::tls::THREAD_POINTER);
        } else {
            eprintln!("[orbis] module {} at {:p}: {} exports, TLS id {tls_id}, {fs_patched} fs: accesses rewritten", img.name, base, exe.exports().len());
            if let Some(init) = exe.dyn_value(DT_INIT) {
                inits.push((img.name.clone(), base as u64 + init));
            }
        }
    }
    let main = &images[0];
    Ok(Loaded {
        base: main.base,
        entry: main.base as u64 + main.exe.entry,
        bound,
        stubbed: stub_count,
        modules: images.iter().skip(1).map(|i| i.name.clone()).collect(),
        inits,
    })
}

extern "sysv64" fn exit_handler() {}

/// Runs the modules' initialisers, then the entry point, on a thread with a console-sized
/// stack (the PC main thread has only 1 MB).
pub fn run(loaded: &Loaded) -> i32 {
    struct P(usize);
    unsafe impl Send for P {}
    let p = P(loaded as *const Loaded as usize);
    std::thread::Builder::new()
        .name("main".into())
        .stack_size(64 << 20)
        .spawn(move || {
            let p = p;
            install_thread_guard();
            crate::tls::set_current(crate::tls::THREAD_POINTER);
            run_here(unsafe { &*(p.0 as *const Loaded) })
        })
        .map_err(|e| e.to_string())
        .and_then(|h| h.join().map_err(|_| "the game thread panicked".to_string()))
        .unwrap_or(-1)
}

fn run_here(loaded: &Loaded) -> i32 {
    for (name, init) in &loaded.inits {
        eprintln!("[orbis] initialising {name}");
        let f: extern "sysv64" fn(u64, u64, u64) -> i32 = unsafe { std::mem::transmute(*init as usize) };
        f(0, 0, 0);
    }
    eprintln!("[orbis] starting the game at its entry point");
    let mut args = [0u64; 32];
    let entry: extern "sysv64" fn(*mut u64, extern "sysv64" fn()) -> i32 = unsafe { std::mem::transmute(loaded.entry as usize) };
    entry(args.as_mut_ptr(), exit_handler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulate_sse4a_insertq_and_extrq() {
        let mut xmm = [(0u64, 0u64); 16];

        // Exact Minecraft PS5 instruction: f2 0f 78 c0 08 08 -> INSERTQ xmm0, xmm0, len=8, idx=8
        xmm[0] = (0x1122_3344_5566_77AB, 0xAAAA_BBBB_CCCC_DDDD);
        assert_eq!(emulate_sse4a(&[0xF2, 0x0F, 0x78, 0xC0, 0x08, 0x08], &mut xmm), Some(6));
        assert_eq!(xmm[0].0, 0x1122_3344_5566_ABAB);
        assert_eq!(xmm[0].1, 0xAAAA_BBBB_CCCC_DDDD);

        // INSERTQ with REX (REX.R | REX.B = 0x45): xmm9, xmm10, len=16, idx=16
        xmm[9] = (0xFFFF_FFFF_0000_1234, 0);
        xmm[10] = (0x0000_0000_0000_CAFE, 0);
        // modrm: mod=3, reg=1 (xmm9), rm=2 (xmm10) -> 0xCA
        assert_eq!(emulate_sse4a(&[0xF2, 0x45, 0x0F, 0x78, 0xCA, 16, 16], &mut xmm), Some(7));
        assert_eq!(xmm[9].0, 0xFFFF_FFFF_CAFE_1234);

        // INSERTQ register form: F2 0F 79 C1 -> xmm0, xmm1 (len=8 in hi[5:0], idx=24 in hi[13:8])
        xmm[0] = (0x0000_0000_1122_3344, 0);
        xmm[1] = (0x0000_0000_0000_00EE, (24u64 << 8) | 8u64);
        assert_eq!(emulate_sse4a(&[0xF2, 0x0F, 0x79, 0xC1], &mut xmm), Some(4));
        assert_eq!(xmm[0].0, 0x0000_0000_EE22_3344);

        // EXTRQ immediate form with REX.B=0x41: 66 41 0F 78 C2 0C 04 -> xmm10, len=12, idx=4
        xmm[10] = (0x1234_5678_9ABC_DEF0, 0x9999);
        assert_eq!(emulate_sse4a(&[0x66, 0x41, 0x0F, 0x78, 0xC2, 12, 4], &mut xmm), Some(7));
        assert_eq!(xmm[10].0, 0xDEF);
        assert_eq!(xmm[10].1, 0x9999);

        // EXTRQ register form: 66 0F 79 C1 -> xmm0, xmm1 (len=0 => 64 bits, idx=0)
        xmm[0] = (0x0123_4567_89AB_CDEF, 0);
        xmm[1] = (0, 0); // len=0 (64), idx=0
        assert_eq!(emulate_sse4a(&[0x66, 0x0F, 0x79, 0xC1], &mut xmm), Some(4));
        assert_eq!(xmm[0].0, 0x0123_4567_89AB_CDEF);
    }
}
