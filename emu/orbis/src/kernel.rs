//! libkernel: the functions the console's own C library (libc.prx, shipped with games) and
//! games call for memory, time, files and process information.
//!
//! Return values follow the console: 0 on success, an SCE error code (0x8002_xxxx) on failure
//! for `sceKernel*`, and -1 with errno for POSIX-style names.

use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;

const SCE_KERNEL_ERROR_ENOMEM: i32 = 0x8002_000Cu32 as i32;
const SCE_KERNEL_ERROR_ENOENT: i32 = 0x8002_0002u32 as i32;
const SCE_KERNEL_ERROR_EBADF: i32 = 0x8002_0009u32 as i32;

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

// ── memory ────────────────────────────────────────────────────────────

/// Regions handed to the game: (start, end, direct?, name).
static REGIONS: Mutex<Vec<(usize, usize, bool)>> = Mutex::new(Vec::new());

fn record(start: usize, len: usize, direct: bool) {
    if start != 0 {
        let mut r = REGIONS.lock().unwrap();
        r.push((start, start + len, direct));
        r.sort_by_key(|x| x.0);
    }
}

#[repr(C)]
pub struct VirtualQueryInfo {
    start: usize,
    end: usize,
    offset: u64,
    protection: i32,
    memory_type: i32,
    flags: u8,
    name: [u8; 32],
}

/// sceKernelVirtualQuery(addr, flags, info, size): flags & 1 = find the next region.
pub extern "sysv64" fn virtual_query(addr: usize, flags: i32, info: *mut VirtualQueryInfo, _size: usize) -> i32 {
    const EACCES: i32 = 0x8002_000Du32 as i32;
    let mut found = None;
    for &(s, e, direct) in REGIONS.lock().unwrap().iter() {
        if addr >= s && addr < e {
            found = Some((s, e, direct));
            break;
        }
        if flags & 1 != 0 && s > addr {
            found = Some((s, e, direct));
            break;
        }
    }
    if found.is_none() {
        for (_, s, size) in crate::loader::images() {
            if addr >= s && addr < s + size || (flags & 1 != 0 && s > addr) {
                found = Some((s, s + size, false));
                break;
            }
        }
    }
    let Some((s, e, direct)) = found else { return EACCES };
    if info.is_null() {
        return 0;
    }
    unsafe {
        std::ptr::write_bytes(info as *mut u8, 0, std::mem::size_of::<VirtualQueryInfo>());
        (*info).start = s;
        (*info).end = e;
        (*info).protection = 0x7;
        (*info).flags = if direct { 0b10010 } else { 0b10001 }; // direct/flexible + committed
    }
    0
}

/// Commits [addr, addr+len), reserving any free parts first (the range may span several
/// Windows reservations). Returns false if some part belongs to something else.
#[cfg(windows)]
fn commit_range(addr: usize, len: usize) -> bool {
    use windows_sys::Win32::System::Memory::{VirtualAlloc, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_FREE, MEM_RESERVE, PAGE_EXECUTE_READWRITE};
    let end = addr + len;
    let mut at = addr;
    while at < end {
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { VirtualQuery(at as *const c_void, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            return false;
        }
        let region_end = (mbi.BaseAddress as usize + mbi.RegionSize).min(end);
        let n = region_end - at;
        let ok = unsafe {
            if mbi.State == MEM_FREE {
                // Reservations must start on 64 KB boundaries.
                let base = (at & !0xFFFF).max(mbi.BaseAddress as usize);
                !VirtualAlloc(base as *const c_void, region_end - base, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE).is_null()
                    || !VirtualAlloc(at as *const c_void, n, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE).is_null()
            } else {
                !VirtualAlloc(at as *const c_void, n, MEM_COMMIT, PAGE_EXECUTE_READWRITE).is_null()
            }
        };
        if !ok {
            return false;
        }
        at = region_end;
    }
    true
}

/// Maps memory for the game. `fixed`: exactly at `addr` or fail. `commit`: usable now
/// (otherwise only reserved, committed later by mprotect/map).
#[cfg(windows)]
fn vmap(addr: usize, len: usize, fixed: bool, commit: bool) -> usize {
    use windows_sys::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_NOACCESS};
    let len = (len + 0x3FFF) & !0x3FFF;
    unsafe {
        if addr != 0 {
            if commit {
                if commit_range(addr, len) {
                    record(addr, len, false);
                    return addr;
                }
            } else {
                let p = VirtualAlloc(addr as *const c_void, len, MEM_RESERVE, PAGE_NOACCESS);
                if !p.is_null() || fixed {
                    // Already reserved by the game counts as success for a fixed request.
                    record(addr, len, false);
                    return addr;
                }
            }
            if fixed {
                return 0;
            }
        }
        let flags = if commit { MEM_COMMIT | MEM_RESERVE } else { MEM_RESERVE };
        let p = VirtualAlloc(std::ptr::null(), len, flags, if commit { PAGE_EXECUTE_READWRITE } else { PAGE_NOACCESS }) as usize;
        record(p, len, false);
        p
    }
}
#[cfg(not(windows))]
fn vmap(_addr: usize, _len: usize, _fixed: bool, _commit: bool) -> usize {
    0
}

fn vmem(addr: usize, len: usize) -> usize {
    vmap(addr, len, false, true)
}

/// sceKernelMapFlexibleMemory(void** addr, size_t len, int prot, int flags)
pub extern "sysv64" fn map_flexible(addr: *mut usize, len: usize, prot: i32, flags: i32) -> i32 {
    if addr.is_null() {
        return SCE_KERNEL_ERROR_ENOMEM;
    }
    let p = vmap(unsafe { *addr }, len, flags & 0x10 != 0, true);
    if std::env::var_os("ORBIS_TRACE_MEM").is_some() {
        eprintln!("[orbis] map flexible({:#x}, {len:#x}, prot {prot:#x}, flags {flags:#x}) -> {p:#x}", unsafe { *addr });
    }
    if p == 0 {
        eprintln!("[orbis] ENOMEM in map_flexible(want={:#x}, len={len:#x}, flags={flags:#x})", unsafe { *addr });
        return SCE_KERNEL_ERROR_ENOMEM;
    }
    unsafe { *addr = p };
    0
}

/// sceKernelMapNamedFlexibleMemory(void** addr, size_t len, int prot, int flags, const char* name)
pub extern "sysv64" fn map_named_flexible(addr: *mut usize, len: usize, prot: i32, flags: i32, _name: *const c_char) -> i32 {
    map_flexible(addr, len, prot, flags)
}

/// mmap(addr, len, prot, flags, fd, offset): anonymous and file-backed mappings.
pub extern "sysv64" fn mmap(addr: usize, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> usize {
    let p = vmap(addr, len, flags & 0x10 != 0, prot != 0);
    if p != 0 && prot != 0 && len > 0 && fd >= 3 && flags & 0x1000 == 0 {
        // File mapping: copy the file's contents in.
        use std::io::{Read, Seek, SeekFrom};
        let buf = unsafe { std::slice::from_raw_parts_mut(p as *mut u8, len) };
        with_file(fd, |f| {
            let _ = f.seek(SeekFrom::Start(off.max(0) as u64));
            let _ = f.read(buf);
        });
    }
    if std::env::var_os("ORBIS_TRACE_MEM").is_some() {
        eprintln!("[orbis] mmap({addr:#x}, {len:#x}, prot {prot:#x}, flags {flags:#x}, fd {fd}, off {off:#x}) -> {p:#x}");
    }
    if p == 0 {
        usize::MAX
    } else {
        p
    }
}

pub extern "sysv64" fn munmap(addr: usize, len: usize) -> i32 {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Memory::{VirtualFree, MEM_DECOMMIT};
        VirtualFree(addr as *mut c_void, len, MEM_DECOMMIT);
    }
    let _ = (addr, len);
    0
}

pub extern "sysv64" fn mprotect(addr: usize, len: usize, prot: i32) -> i32 {
    #[cfg(windows)]
    if prot != 0 {
        commit_range(addr & !0x3FFF, (len + (addr & 0x3FFF) + 0x3FFF) & !0x3FFF);
    }
    let _ = (addr, len, prot);
    0
}

/// Direct memory: one counter, the "physical" offset handed back to the game.
static DIRECT: Mutex<u64> = Mutex::new(0);
const DIRECT_SIZE: u64 = 16 << 30;

/// sceKernelAllocateDirectMemory(start, end, len, align, type, out_offset)
pub extern "sysv64" fn allocate_direct(_start: i64, _end: i64, len: u64, align: u64, _kind: i32, out: *mut u64) -> i32 {
    let mut d = DIRECT.lock().unwrap();
    let a = align.max(0x10000);
    let off = (*d + a - 1) / a * a;
    if off + len > DIRECT_SIZE {
        eprintln!("[orbis] ENOMEM in allocate_direct(off={off:#x}, len={len:#x}, max={DIRECT_SIZE:#x})");
        return SCE_KERNEL_ERROR_ENOMEM;
    }
    *d = off + len;
    unsafe { *out = off };
    0
}

pub extern "sysv64" fn allocate_main_direct(len: u64, align: u64, kind: i32, out: *mut u64) -> i32 {
    allocate_direct(0, DIRECT_SIZE as i64, len, align, kind, out)
}

/// sceKernelMapDirectMemory(void** addr, len, prot, flags, offset, align)
pub extern "sysv64" fn map_direct(addr: *mut usize, len: usize, prot: i32, flags: i32, _off: i64, _align: u64) -> i32 {
    map_flexible(addr, len, prot, flags)
}

pub extern "sysv64" fn map_named_direct(addr: *mut usize, len: usize, prot: i32, flags: i32, off: i64, align: u64, _name: *const c_char) -> i32 {
    map_direct(addr, len, prot, flags, off, align)
}

pub extern "sysv64" fn direct_memory_size() -> u64 {
    DIRECT_SIZE
}

/// sceKernelAvailableFlexibleMemorySize(size_t* out)
pub extern "sysv64" fn available_flexible(out: *mut u64) -> i32 {
    if !out.is_null() {
        unsafe { *out = 8 << 30 }; // 8 GiB PS5-class flexible RAM for 3D world chunks & structures
    }
    0
}

/// sceKernelAvailableDirectMemorySize(start, end, align, out_start, out_size)
pub extern "sysv64" fn available_direct(_s: i64, _e: i64, _a: u64, out_start: *mut u64, out_size: *mut u64) -> i32 {
    let used = *DIRECT.lock().unwrap();
    unsafe {
        if !out_start.is_null() {
            *out_start = used;
        }
        if !out_size.is_null() {
            *out_size = DIRECT_SIZE - used;
        }
    }
    0
}

/// sceKernelReserveVirtualRange(void** addr, len, flags, align)
pub extern "sysv64" fn reserve_virtual_range(addr: *mut usize, len: usize, _flags: i32, align: usize) -> i32 {
    if addr.is_null() {
        return SCE_KERNEL_ERROR_ENOMEM;
    }
    let want = unsafe { *addr };
    // Over-allocate to honour the alignment.
    let a = align.max(0x10000);
    let p = vmap(want, len + if want == 0 { a } else { 0 }, false, false);
    if std::env::var_os("ORBIS_TRACE_MEM").is_some() {
        eprintln!("[orbis] reserve({want:#x}, {len:#x}, align {align:#x}) -> {p:#x}");
    }
    if p == 0 {
        eprintln!("[orbis] ENOMEM in reserve_virtual_range(want={want:#x}, len={len:#x}, align={align:#x})");
        return SCE_KERNEL_ERROR_ENOMEM;
    }
    unsafe { *addr = if want == 0 { (p + a - 1) / a * a } else { p } };
    0
}

/// pthread_once / scePthreadOnce: { int state; mutex }.
pub extern "sysv64" fn pthread_once(ctrl: *mut i32, f: extern "sysv64" fn()) -> i32 {
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        if *ctrl == 0 {
            *ctrl = 1;
            f();
            *ctrl = 2;
        }
    }
    0
}

/// sceKernelLoadStartModule(path, argc, argv, flags, opt, result): modules from the game folder
/// are already loaded and linked at start-up; return their handle.
pub extern "sysv64" fn load_start_module(path: *const c_char, _argc: usize, _argv: *const c_void, _flags: u32, _opt: *const c_void, result: *mut i32) -> i32 {
    let p = text(path);
    let file = p.rsplit('/').next().unwrap_or(&p).to_string();
    let found = crate::loader::images().iter().position(|(n, _, _)| n.eq_ignore_ascii_case(&file));
    eprintln!("[orbis] load module {p}: {}", if found.is_some() { "already loaded" } else { "not available" });
    if !result.is_null() {
        unsafe { *result = 0 };
    }
    match found {
        Some(i) => i as i32 + 1,
        None => SCE_KERNEL_ERROR_ENOENT,
    }
}

// ── mspaces (sceLibcMspace*): heaps a game creates over its own memory ──
// Orbis serves them from the PC heap; the region the game offers is left alone.

use crate::hle_rt::{free_any, memalign as rt_memalign};

fn usable(p: *mut c_void) -> usize {
    if p.is_null() {
        return 0;
    }
    unsafe {
        let tag = *((p as *mut u8).sub(16) as *const usize);
        if tag == usize::MAX {
            let raw = *((p as *mut u8).sub(8) as *const usize);
            let raw_size = *((raw - 16) as *const usize);
            raw + raw_size - p as usize
        } else {
            tag
        }
    }
}

pub extern "sysv64" fn mspace_create(name: *const c_char, base: usize, cap: usize, _flag: u32) -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0x5EED_0000);
    let id = NEXT.fetch_add(0x100, std::sync::atomic::Ordering::SeqCst);
    if std::env::var_os("ORBIS_TRACE").is_some() {
        eprintln!("[orbis] mspace {} ({cap:#x} bytes at {base:#x})", text(name));
    }
    // A distinct non-null handle per mspace.
    Box::leak(Box::new([id; 8])).as_ptr() as usize
}
pub extern "sysv64" fn mspace_destroy(_m: usize) -> i32 {
    0
}
pub extern "sysv64" fn mspace_malloc(_m: usize, size: usize) -> *mut c_void {
    rt_memalign(16, size.max(1))
}
pub extern "sysv64" fn mspace_free(_m: usize, p: *mut c_void) -> i32 {
    free_any(p);
    0
}
pub extern "sysv64" fn mspace_calloc(_m: usize, n: usize, size: usize) -> *mut c_void {
    let total = n.saturating_mul(size).max(1);
    let p = rt_memalign(16, total);
    if !p.is_null() {
        unsafe { std::ptr::write_bytes(p as *mut u8, 0, total) };
    }
    p
}
pub extern "sysv64" fn mspace_memalign(_m: usize, align: usize, size: usize) -> *mut c_void {
    rt_memalign(align, size.max(1))
}
pub extern "sysv64" fn mspace_realloc(_m: usize, p: *mut c_void, size: usize) -> *mut c_void {
    let n = rt_memalign(16, size.max(1));
    if !p.is_null() && !n.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(p as *const u8, n as *mut u8, usable(p).min(size)) };
        free_any(p);
    }
    n
}
pub extern "sysv64" fn mspace_reallocalign(m: usize, p: *mut c_void, size: usize, align: usize) -> *mut c_void {
    let n = mspace_memalign(m, align, size);
    if !p.is_null() && !n.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(p as *const u8, n as *mut u8, usable(p).min(size)) };
        free_any(p);
    }
    n
}
pub extern "sysv64" fn mspace_posix_memalign(m: usize, out: *mut *mut c_void, align: usize, size: usize) -> i32 {
    let p = mspace_memalign(m, align, size);
    unsafe { *out = p };
    if p.is_null() {
        12
    } else {
        0
    }
}
pub extern "sysv64" fn mspace_usable(p: *mut c_void) -> usize {
    usable(p)
}
pub extern "sysv64" fn mspace_stats(_m: usize, stats: *mut u64) -> i32 {
    if !stats.is_null() {
        unsafe { std::ptr::write_bytes(stats as *mut u8, 0, 32) };
    }
    0
}

// ── process ───────────────────────────────────────────────────────────

pub static mut PROC_PARAM: usize = 0;

pub extern "sysv64" fn get_proc_param() -> usize {
    unsafe { PROC_PARAM }
}

/// `__progname`: data symbol, a pointer to the program name.
pub static mut PROGNAME: [usize; 2] = [0; 2];

pub fn progname() -> usize {
    unsafe {
        PROGNAME[0] = b"eboot.bin\0".as_ptr() as usize;
        std::ptr::addr_of!(PROGNAME) as usize
    }
}

pub extern "sysv64" fn zero() -> u64 {
    0
}

/// _sceKernelRtldSetApplicationHeapAPI(table): libc hands over its malloc family.
pub static mut HEAP_API: usize = 0;
pub extern "sysv64" fn set_application_heap_api(api: usize) -> i32 {
    unsafe { HEAP_API = api };
    0
}

/// sceKernelGetModuleInfoFromAddr(addr, flags, info): enough for unwinding to find nothing.
pub extern "sysv64" fn module_info_from_addr(_addr: usize, _flags: i32, _info: *mut c_void) -> i32 {
    SCE_KERNEL_ERROR_ENOENT
}

pub extern "sysv64" fn debug_raise_exception(code: u32, arg: u64) -> i32 {
    eprintln!("[orbis] the game raised a debug exception {code:#x} ({arg:#x})");
    0
}

// ── time ──────────────────────────────────────────────────────────────

fn start() -> &'static std::time::Instant {
    static T: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    T.get_or_init(std::time::Instant::now)
}

#[repr(C)]
pub struct Timespec {
    sec: i64,
    nsec: i64,
}

/// clock_gettime(clock, ts): realtime (0) from the system clock, every other clock monotonic.
pub extern "sysv64" fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32 {
    if ts.is_null() {
        return -1;
    }
    let d = if clock == 0 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
    } else {
        start().elapsed()
    };
    unsafe {
        (*ts).sec = d.as_secs() as i64;
        (*ts).nsec = d.subsec_nanos() as i64;
    }
    0
}

#[repr(C)]
pub struct Timeval {
    sec: i64,
    usec: i64,
}

pub extern "sysv64" fn gettimeofday(tv: *mut Timeval, _tz: *mut c_void) -> i32 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    if !tv.is_null() {
        unsafe {
            (*tv).sec = d.as_secs() as i64;
            (*tv).usec = d.subsec_micros() as i64;
        }
    }
    0
}

/// Host time zone: (seconds west of UTC, daylight-saving seconds currently in effect).
fn host_timezone() -> (i64, i64) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let r = GetTimeZoneInformation(&mut tz);
        let dst = if r == 2 { -(tz.DaylightBias as i64) * 60 } else { 0 };
        return (tz.Bias as i64 * 60, dst);
    }
    #[allow(unreachable_code)]
    (0, 0)
}

#[repr(C)]
pub struct KernelTimesec {
    t: i64,
    west_sec: u32,
    dst_sec: u32,
}

/// sceKernelConvertUtcToLocaltime(utc, *local, *timesec, *dst_sec)
pub extern "sysv64" fn convert_utc_to_localtime(utc: i64, local: *mut i64, ts: *mut KernelTimesec, dst: *mut u64) -> i32 {
    let (west, dst_s) = host_timezone();
    unsafe {
        if !local.is_null() {
            *local = utc - west + dst_s;
        }
        if !ts.is_null() {
            (*ts).t = utc;
            (*ts).west_sec = west as u32;
            (*ts).dst_sec = dst_s as u32;
        }
        if !dst.is_null() {
            *dst = dst_s as u64;
        }
    }
    0
}

/// sceKernelConvertLocaltimeToUtc(local, unused, *utc, *timezone{west, dst}, *dst_sec)
pub extern "sysv64" fn convert_localtime_to_utc(local: i64, _x: i64, utc: *mut i64, tz: *mut [i32; 2], dst: *mut i32) -> i32 {
    let (west, dst_s) = host_timezone();
    unsafe {
        if !utc.is_null() {
            *utc = local + west - dst_s;
        }
        if !tz.is_null() {
            (*tz)[0] = (west / 60) as i32;
            (*tz)[1] = (dst_s / 60) as i32;
        }
        if !dst.is_null() {
            *dst = dst_s as i32;
        }
    }
    0
}

pub extern "sysv64" fn nanosleep(req: *const Timespec, _rem: *mut Timespec) -> i32 {
    if !req.is_null() {
        let (s, n) = unsafe { ((*req).sec, (*req).nsec) };
        std::thread::sleep(std::time::Duration::new(s.max(0) as u64, n.clamp(0, 999_999_999) as u32));
    }
    0
}

pub fn monotonic_now() -> std::time::Duration {
    start().elapsed()
}

pub extern "sysv64" fn read_tsc() -> u64 {
    start().elapsed().as_nanos() as u64
}

pub extern "sysv64" fn tsc_frequency() -> u64 {
    1_000_000_000
}

// ── files ─────────────────────────────────────────────────────────────
// Console paths: /app0 is the game folder; /data, /temp0, /download0 map to a folder under
// POLYMIRA's data folder (see data_root).

pub static GAME_DIR: Mutex<String> = Mutex::new(String::new());

/// Where Orbis keeps a game's files (saves, settings, downloads): POLYMIRA's own data folder,
/// %APPDATA%\POLYMIRA\orbis, or ORBIS_DATA when POLYMIRA passes another one.
pub fn data_root() -> String {
    if let Ok(d) = std::env::var("ORBIS_DATA") {
        if !d.is_empty() {
            return d.replace('\\', "/");
        }
    }
    std::env::var("APPDATA").map(|d| format!("{}/POLYMIRA/orbis", d.replace('\\', "/"))).unwrap_or_else(|_| "orbis-data".into())
}

fn host_path(p: &str) -> std::path::PathBuf {
    if let Some(saved) = crate::savedata::resolve(p) {
        return saved;
    }
    let game = GAME_DIR.lock().unwrap().clone();
    let data = data_root();
    for (prefix, root) in [("/app0", game.clone()), ("/hostapp", game), ("/data", format!("{data}/data")), ("/temp0", format!("{data}/temp0")), ("/temp", format!("{data}/temp0")), ("/download0", format!("{data}/download0"))] {
        if let Some(rest) = p.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with('/') {
                return std::path::PathBuf::from(format!("{root}{rest}"));
            }
        }
    }
    std::path::PathBuf::from(format!("{data}/root{p}"))
}

enum FdEntry {
    File(std::fs::File),
    Dir {
        path: std::path::PathBuf,
        entries: Vec<(String, bool)>,
        pos: usize,
    },
}

static FILES: Mutex<Vec<Option<FdEntry>>> = Mutex::new(Vec::new());

const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_APPEND: i32 = 8;
const O_CREAT: i32 = 0x200;
const O_TRUNC: i32 = 0x400;
const O_EXCL: i32 = 0x800;

const SCE_KERNEL_ERROR_EEXIST: i32 = 0x8002_0011u32 as i32;
const SCE_KERNEL_ERROR_EINVAL: i32 = 0x8002_0016u32 as i32;
const SCE_KERNEL_ERROR_ENOTEMPTY: i32 = 0x8002_0042u32 as i32;

fn alloc_fd(entry: FdEntry) -> i32 {
    let mut f = FILES.lock().unwrap();
    if let Some((idx, slot)) = f.iter_mut().enumerate().find(|(_, s)| s.is_none()) {
        *slot = Some(entry);
        (idx + 3) as i32
    } else {
        f.push(Some(entry));
        (f.len() + 2) as i32
    }
}

fn open_file(path: &str, flags: i32) -> Result<i32, i32> {
    let host = if path == "/dev/urandom" || path == "/dev/random" { random_device() } else { host_path(path) };
    let mut o = std::fs::OpenOptions::new();
    let acc = flags & 3;
    let want_write = acc == O_WRONLY || acc == O_RDWR || (flags & (O_TRUNC | O_APPEND)) != 0;
    o.read(acc != O_WRONLY)
        .write(want_write)
        .append(flags & O_APPEND != 0)
        .truncate(flags & O_TRUNC != 0);
    if flags & O_CREAT != 0 {
        if flags & O_EXCL != 0 {
            o.create_new(true);
        } else {
            o.create(true);
        }
        if let Some(parent) = host.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    if host.is_dir() {
        let mut entries = vec![(".".to_string(), true), ("..".to_string(), true)];
        if let Ok(rd) = std::fs::read_dir(&host) {
            for ent in rd.flatten() {
                if let Some(name) = ent.file_name().to_str() {
                    let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    entries.push((name.to_string(), is_dir));
                }
            }
        }
        return Ok(alloc_fd(FdEntry::Dir { path: host, entries, pos: 0 }));
    }
    match o.open(&host) {
        Ok(file) => {
            let fd = alloc_fd(FdEntry::File(file));
            eprintln!("[orbis] open {path} → fd {fd}");
            Ok(fd)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(SCE_KERNEL_ERROR_EEXIST),
        Err(_) => {
            eprintln!("[orbis] open {path}: not found ({})", host.display());
            Err(SCE_KERNEL_ERROR_ENOENT)
        }
    }
}

pub extern "sysv64" fn sce_open(path: *const c_char, flags: i32, _mode: i32) -> i32 {
    match open_file(&text(path), flags) {
        Ok(fd) => fd,
        Err(e) => e,
    }
}

pub extern "sysv64" fn posix_open(path: *const c_char, flags: i32, _mode: i32) -> i32 {
    match open_file(&text(path), flags) {
        Ok(fd) => fd,
        Err(SCE_KERNEL_ERROR_EEXIST) => {
            crate::hle_rt::set_errno(17); // EEXIST
            -1
        }
        Err(_) => {
            crate::hle_rt::set_errno(2); // ENOENT
            -1
        }
    }
}

fn with_file<R>(fd: i32, f: impl FnOnce(&mut std::fs::File) -> R) -> Option<R> {
    let mut files = FILES.lock().unwrap();
    match files.get_mut((fd - 3).max(0) as usize).and_then(|x| x.as_mut()) {
        Some(FdEntry::File(file)) => Some(f(file)),
        _ => None,
    }
}

pub extern "sysv64" fn sce_close(fd: i32) -> i32 {
    if fd < 3 {
        return 0;
    }
    let mut files = FILES.lock().unwrap();
    match files.get_mut((fd - 3) as usize) {
        Some(slot) => {
            *slot = None;
            0
        }
        None => SCE_KERNEL_ERROR_EBADF,
    }
}

pub extern "sysv64" fn sce_read(fd: i32, buf: *mut u8, len: usize) -> i64 {
    use std::io::Read;
    if fd < 3 || len == 0 {
        return 0;
    }
    if buf.is_null() {
        return SCE_KERNEL_ERROR_EINVAL as i64;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, len) };
    with_file(fd, |f| f.read(slice).map(|n| n as i64).unwrap_or(-1)).unwrap_or(SCE_KERNEL_ERROR_EBADF as i64)
}

pub extern "sysv64" fn sce_write(fd: i32, buf: *const u8, len: usize) -> i64 {
    use std::io::Write;
    if len == 0 {
        return 0;
    }
    if buf.is_null() {
        return SCE_KERNEL_ERROR_EINVAL as i64;
    }
    let slice = unsafe { std::slice::from_raw_parts(buf, len) };
    if fd == 1 || fd == 2 {
        let mut out = std::io::stdout();
        let _ = out.write_all(slice);
        let _ = out.flush();
        return len as i64;
    }
    with_file(fd, |f| f.write(slice).map(|n| n as i64).unwrap_or(-1)).unwrap_or(SCE_KERNEL_ERROR_EBADF as i64)
}

pub extern "sysv64" fn sce_lseek(fd: i32, off: i64, whence: i32) -> i64 {
    use std::io::{Seek, SeekFrom};
    let pos = match whence {
        0 => SeekFrom::Start(off.max(0) as u64),
        1 => SeekFrom::Current(off),
        _ => SeekFrom::End(off),
    };
    with_file(fd, |f| f.seek(pos).map(|n| n as i64).unwrap_or(-1)).unwrap_or(SCE_KERNEL_ERROR_EBADF as i64)
}

pub extern "sysv64" fn sce_pread(fd: i32, buf: *mut u8, len: usize, offset: i64) -> i64 {
    use std::io::{Read, Seek, SeekFrom};
    if fd < 3 || buf.is_null() {
        return SCE_KERNEL_ERROR_EBADF as i64;
    }
    if offset < 0 {
        return SCE_KERNEL_ERROR_EINVAL as i64;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, len) };
    with_file(fd, |f| {
        let orig = f.stream_position().unwrap_or(0);
        let res = f
            .seek(SeekFrom::Start(offset as u64))
            .and_then(|_| f.read(slice))
            .map(|n| n as i64)
            .unwrap_or(-1);
        let _ = f.seek(SeekFrom::Start(orig));
        res
    })
    .unwrap_or(SCE_KERNEL_ERROR_EBADF as i64)
}

pub extern "sysv64" fn posix_pread(fd: i32, buf: *mut u8, len: usize, offset: i64) -> i64 {
    let r = sce_pread(fd, buf, len, offset);
    if r < 0 {
        crate::hle_rt::set_errno(if offset < 0 { 22 } else { 9 }); // EINVAL / EBADF
        -1
    } else {
        r
    }
}

pub extern "sysv64" fn sce_pwrite(fd: i32, buf: *const u8, len: usize, offset: i64) -> i64 {
    use std::io::{Seek, SeekFrom, Write};
    if fd < 3 || buf.is_null() {
        return SCE_KERNEL_ERROR_EBADF as i64;
    }
    if offset < 0 {
        return SCE_KERNEL_ERROR_EINVAL as i64;
    }
    let slice = unsafe { std::slice::from_raw_parts(buf, len) };
    with_file(fd, |f| {
        let orig = f.stream_position().unwrap_or(0);
        let res = f
            .seek(SeekFrom::Start(offset as u64))
            .and_then(|_| f.write(slice))
            .map(|n| n as i64)
            .unwrap_or(-1);
        let _ = f.seek(SeekFrom::Start(orig));
        res
    })
    .unwrap_or(SCE_KERNEL_ERROR_EBADF as i64)
}

pub extern "sysv64" fn posix_pwrite(fd: i32, buf: *const u8, len: usize, offset: i64) -> i64 {
    let r = sce_pwrite(fd, buf, len, offset);
    if r < 0 {
        crate::hle_rt::set_errno(if offset < 0 { 22 } else { 9 }); // EINVAL / EBADF
        -1
    } else {
        r
    }
}

pub extern "sysv64" fn sce_ftruncate(fd: i32, length: i64) -> i32 {
    if length < 0 {
        return SCE_KERNEL_ERROR_EINVAL;
    }
    with_file(fd, |f| match f.set_len(length as u64) {
        Ok(()) => 0,
        Err(_) => SCE_KERNEL_ERROR_EINVAL,
    })
    .unwrap_or(SCE_KERNEL_ERROR_EBADF)
}

pub extern "sysv64" fn posix_ftruncate(fd: i32, length: i64) -> i32 {
    let r = sce_ftruncate(fd, length);
    if r == 0 {
        0
    } else {
        crate::hle_rt::set_errno(if r == SCE_KERNEL_ERROR_EINVAL { 22 } else { 9 });
        -1
    }
}

pub extern "sysv64" fn sce_truncate(path: *const c_char, length: i64) -> i32 {
    if length < 0 {
        return SCE_KERNEL_ERROR_EINVAL;
    }
    let p = host_path(&text(path));
    match std::fs::OpenOptions::new().write(true).open(&p) {
        Ok(f) => match f.set_len(length as u64) {
            Ok(()) => 0,
            Err(_) => SCE_KERNEL_ERROR_EINVAL,
        },
        Err(_) => SCE_KERNEL_ERROR_ENOENT,
    }
}

pub extern "sysv64" fn posix_truncate(path: *const c_char, length: i64) -> i32 {
    let r = sce_truncate(path, length);
    if r == 0 {
        0
    } else {
        crate::hle_rt::set_errno(if r == SCE_KERNEL_ERROR_EINVAL { 22 } else { 2 });
        -1
    }
}

/// FreeBSD struct stat (the fields games read: mode, size, blocks).
#[repr(C)]
pub struct Stat {
    dev: u32,
    ino: u32,
    mode: u16,
    nlink: u16,
    uid: u32,
    gid: u32,
    rdev: u32,
    atime: [i64; 2],
    mtime: [i64; 2],
    ctime: [i64; 2],
    size: i64,
    blocks: i64,
    blksize: u32,
    flags: u32,
    gen: u32,
    lspare: i32,
    birthtime: [i64; 2],
}

fn sys_time_to_timespec(t: std::io::Result<std::time::SystemTime>) -> [i64; 2] {
    match t.ok().and_then(|st| st.duration_since(std::time::UNIX_EPOCH).ok()) {
        Some(d) => [d.as_secs() as i64, d.subsec_nanos() as i64],
        None => [0, 0],
    }
}

fn fill_stat(meta: &std::fs::Metadata, st: *mut Stat) {
    unsafe {
        std::ptr::write_bytes(st as *mut u8, 0, std::mem::size_of::<Stat>());
        (*st).mode = if meta.is_dir() { 0o040755 } else { 0o100644 };
        (*st).size = meta.len() as i64;
        (*st).blksize = 0x10000;
        (*st).blocks = (meta.len() as i64 + 511) / 512;
        (*st).nlink = 1;
        (*st).atime = sys_time_to_timespec(meta.accessed());
        (*st).mtime = sys_time_to_timespec(meta.modified());
        (*st).ctime = (*st).mtime;
        (*st).birthtime = sys_time_to_timespec(meta.created());
    }
}

pub extern "sysv64" fn sce_stat(path: *const c_char, st: *mut Stat) -> i32 {
    if st.is_null() {
        return SCE_KERNEL_ERROR_EINVAL;
    }
    let p = text(path);
    match std::fs::metadata(host_path(&p)) {
        Ok(m) => {
            fill_stat(&m, st);
            0
        }
        Err(_) => SCE_KERNEL_ERROR_ENOENT,
    }
}

pub extern "sysv64" fn posix_stat(path: *const c_char, st: *mut Stat) -> i32 {
    if sce_stat(path, st) == 0 {
        0
    } else {
        crate::hle_rt::set_errno(2);
        -1
    }
}

pub extern "sysv64" fn sce_fstat(fd: i32, st: *mut Stat) -> i32 {
    if st.is_null() {
        return SCE_KERNEL_ERROR_EINVAL;
    }
    let mut files = FILES.lock().unwrap();
    match files.get_mut((fd - 3).max(0) as usize).and_then(|x| x.as_mut()) {
        Some(FdEntry::File(f)) => match f.metadata() {
            Ok(m) => {
                fill_stat(&m, st);
                0
            }
            Err(_) => SCE_KERNEL_ERROR_EBADF,
        },
        Some(FdEntry::Dir { path, .. }) => match std::fs::metadata(path) {
            Ok(m) => {
                fill_stat(&m, st);
                0
            }
            Err(_) => {
                unsafe {
                    std::ptr::write_bytes(st as *mut u8, 0, std::mem::size_of::<Stat>());
                    (*st).mode = 0o040755;
                }
                0
            }
        },
        None => {
            unsafe {
                std::ptr::write_bytes(st as *mut u8, 0, std::mem::size_of::<Stat>());
                (*st).mode = 0o020666; // character device (console streams)
            }
            0
        }
    }
}

/// sceKernelGetdents(fd, buf, nbytes): FreeBSD dirent records (fileno: u32, reclen: u16, type: u8, namlen: u8, name[]).
pub extern "sysv64" fn sce_getdents(fd: i32, buf: *mut u8, nbytes: usize) -> i32 {
    if fd < 3 || buf.is_null() {
        return SCE_KERNEL_ERROR_EBADF;
    }
    let mut files = FILES.lock().unwrap();
    let Some(Some(FdEntry::Dir { entries, pos, .. })) = files.get_mut((fd - 3) as usize) else {
        return SCE_KERNEL_ERROR_EBADF;
    };
    let mut written = 0usize;
    while *pos < entries.len() {
        let (ref name, is_dir) = entries[*pos];
        let nb = name.as_bytes();
        let namlen = nb.len().min(255);
        let reclen = (8 + namlen + 1 + 3) & !3;
        if written + reclen > nbytes {
            break;
        }
        unsafe {
            let p = buf.add(written);
            std::ptr::write_bytes(p, 0, reclen);
            *(p as *mut u32) = (*pos as u32) + 1; // d_fileno
            *(p.add(4) as *mut u16) = reclen as u16; // d_reclen
            *p.add(6) = if is_dir { 4 } else { 8 }; // DT_DIR / DT_REG
            *p.add(7) = namlen as u8; // d_namlen
            std::ptr::copy_nonoverlapping(nb.as_ptr(), p.add(8), namlen);
        }
        written += reclen;
        *pos += 1;
    }
    written as i32
}

pub extern "sysv64" fn sce_check_reachability(path: *const c_char) -> i32 {
    if host_path(&text(path)).exists() {
        0
    } else {
        SCE_KERNEL_ERROR_ENOENT
    }
}

pub extern "sysv64" fn sce_dlsym(_handle: i32, symbol: *const c_char, out: *mut usize) -> i32 {
    const ESRCH: i32 = 0x8002_0003u32 as i32;
    let name = text(symbol);
    let addr = crate::loader::export_addr(&name)
        .or_else(|| crate::hle::table().get(&crate::nid::nid(&name)).map(|&(_, a)| a));
    if let Some(a) = addr {
        if !out.is_null() {
            unsafe { *out = a };
        }
        0
    } else {
        ESRCH
    }
}

pub extern "sysv64" fn sce_mkdir(path: *const c_char, _mode: i32) -> i32 {
    let _ = std::fs::create_dir_all(host_path(&text(path)));
    0
}

pub extern "sysv64" fn sce_unlink(path: *const c_char) -> i32 {
    let p = host_path(&text(path));
    match std::fs::remove_file(&p) {
        Ok(()) => 0,
        Err(_) => SCE_KERNEL_ERROR_ENOENT,
    }
}

pub extern "sysv64" fn posix_unlink(path: *const c_char) -> i32 {
    let p = host_path(&text(path));
    if std::fs::remove_file(&p).is_ok() {
        0
    } else {
        crate::hle_rt::set_errno(2); // ENOENT
        -1
    }
}

pub extern "sysv64" fn sce_rmdir(path: *const c_char) -> i32 {
    let p = host_path(&text(path));
    match std::fs::remove_dir(&p) {
        Ok(()) => 0,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SCE_KERNEL_ERROR_ENOENT,
        Err(_) => SCE_KERNEL_ERROR_ENOTEMPTY,
    }
}

pub extern "sysv64" fn posix_rmdir(path: *const c_char) -> i32 {
    match sce_rmdir(path) {
        0 => 0,
        SCE_KERNEL_ERROR_ENOENT => {
            crate::hle_rt::set_errno(2); // ENOENT
            -1
        }
        _ => {
            crate::hle_rt::set_errno(66); // FreeBSD ENOTEMPTY
            -1
        }
    }
}

// ── rename, fsync, uuid, scheduler, process id, random device ────────

pub extern "sysv64" fn sce_rename(from: *const c_char, to: *const c_char) -> i32 {
    let (f, t) = (host_path(&text(from)), host_path(&text(to)));
    if let Some(parent) = t.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let res = match std::fs::rename(&f, &t) {
        Ok(()) => Ok(()),
        Err(_) if f.exists() && t.is_file() => {
            // POSIX/PS5 rename replaces an existing target file; Windows rename fails if it exists.
            let _ = std::fs::remove_file(&t);
            std::fs::rename(&f, &t)
        }
        Err(e) => Err(e),
    };
    match res {
        Ok(()) => 0,
        Err(_) => SCE_KERNEL_ERROR_ENOENT,
    }
}

pub extern "sysv64" fn posix_rename(from: *const c_char, to: *const c_char) -> i32 {
    if sce_rename(from, to) == 0 {
        0
    } else {
        crate::hle_rt::set_errno(2);
        -1
    }
}

/// sceKernelUuidCreate(SceKernelUuid* out): 16-byte RFC 4122 v4 UUID.
pub extern "sysv64" fn sce_uuid_create(out: *mut u8) -> i32 {
    if out.is_null() {
        return SCE_KERNEL_ERROR_EINVAL;
    }
    crate::system::random(out, 16);
    unsafe {
        *out.add(6) = (*out.add(6) & 0x0F) | 0x40; // version 4
        *out.add(8) = (*out.add(8) & 0x3F) | 0x80; // variant RFC 4122
    }
    0
}

pub extern "sysv64" fn sched_get_priority_min(_policy: i32) -> i32 {
    0
}

pub extern "sysv64" fn sched_get_priority_max(_policy: i32) -> i32 {
    31
}

pub extern "sysv64" fn getpid() -> i32 {
    1000
}

/// /dev/urandom and /dev/random: a fresh file of random bytes in the data folder.
fn random_device() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(format!("{}/dev_random.bin", data_root()));
    let mut bytes = vec![0u8; 1 << 16];
    crate::system::random(bytes.as_mut_ptr(), bytes.len());
    let _ = std::fs::create_dir_all(data_root());
    let _ = std::fs::write(&path, &bytes);
    path
}

// ── POSIX semaphores (sem_t holds a pointer to an Orbis semaphore) ────

struct Sem {
    count: Mutex<u32>,
    cv: std::sync::Condvar,
}

fn sem_of(s: *mut usize) -> Option<&'static Sem> {
    if s.is_null() {
        return None;
    }
    let p = unsafe { *s };
    if p == 0 {
        return None;
    }
    Some(unsafe { &*(p as *const Sem) })
}

pub extern "sysv64" fn sem_init(s: *mut usize, _pshared: i32, value: u32) -> i32 {
    if s.is_null() {
        return -1;
    }
    let sem = Box::leak(Box::new(Sem { count: Mutex::new(value), cv: std::sync::Condvar::new() }));
    unsafe { *s = sem as *const Sem as usize };
    0
}

pub extern "sysv64" fn sem_destroy(_s: *mut usize) -> i32 {
    0
}

pub extern "sysv64" fn sem_post(s: *mut usize) -> i32 {
    let Some(sem) = sem_of(s) else { return -1 };
    *sem.count.lock().unwrap() += 1;
    sem.cv.notify_one();
    0
}

pub extern "sysv64" fn sem_wait(s: *mut usize) -> i32 {
    let Some(sem) = sem_of(s) else { return -1 };
    let mut c = sem.count.lock().unwrap();
    while *c == 0 {
        c = sem.cv.wait(c).unwrap();
    }
    *c -= 1;
    0
}

pub extern "sysv64" fn sem_trywait(s: *mut usize) -> i32 {
    let Some(sem) = sem_of(s) else { return -1 };
    let mut c = sem.count.lock().unwrap();
    if *c == 0 {
        crate::hle_rt::set_errno(35); // EAGAIN
        return -1;
    }
    *c -= 1;
    0
}

/// sem_timedwait(s, abstime): absolute CLOCK_REALTIME.
pub extern "sysv64" fn sem_timedwait(s: *mut usize, abs: *const [i64; 2]) -> i32 {
    let Some(sem) = sem_of(s) else { return -1 };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let (sec, ns) = if abs.is_null() { (0, 0) } else { unsafe { ((*abs)[0], (*abs)[1]) } };
    let target = std::time::Duration::new(sec.max(0) as u64, ns.clamp(0, 999_999_999) as u32);
    let deadline = std::time::Instant::now() + target.saturating_sub(now);
    let mut c = sem.count.lock().unwrap();
    while *c == 0 {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            crate::hle_rt::set_errno(60); // ETIMEDOUT
            return -1;
        }
        c = sem.cv.wait_timeout(c, left).unwrap().0;
    }
    *c -= 1;
    0
}

pub extern "sysv64" fn sem_getvalue(s: *mut usize, out: *mut i32) -> i32 {
    let Some(sem) = sem_of(s) else { return -1 };
    if !out.is_null() {
        unsafe { *out = *sem.count.lock().unwrap() as i32 };
    }
    0
}

/// (name, address) for everything here.
pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("__tls_get_addr", crate::loader::tls_get_addr as *const () as usize),
        ("sceKernelMapFlexibleMemory", map_flexible as *const () as usize),
        ("#4h6F1LLbTiw", map_flexible as *const () as usize), // PS5 6.xx variant
        ("sceKernelMapNamedFlexibleMemory", map_named_flexible as *const () as usize),
        ("sceKernelMapNamedSystemFlexibleMemory", map_named_flexible as *const () as usize),
        ("sceKernelAllocateDirectMemory", allocate_direct as *const () as usize),
        ("sceKernelAllocateMainDirectMemory", allocate_main_direct as *const () as usize),
        ("sceKernelMapDirectMemory", map_direct as *const () as usize),
        ("sceKernelMapNamedDirectMemory", map_named_direct as *const () as usize),
        ("sceKernelGetDirectMemorySize", direct_memory_size as *const () as usize),
        ("sceKernelAvailableFlexibleMemorySize", available_flexible as *const () as usize),
        ("sceKernelAvailableDirectMemorySize", available_direct as *const () as usize),
        ("sceLibcMspaceCreate", mspace_create as *const () as usize),
        ("sceLibcMspaceDestroy", mspace_destroy as *const () as usize),
        ("sceLibcMspaceMalloc", mspace_malloc as *const () as usize),
        ("sceLibcMspaceFree", mspace_free as *const () as usize),
        ("sceLibcMspaceCalloc", mspace_calloc as *const () as usize),
        ("sceLibcMspaceMemalign", mspace_memalign as *const () as usize),
        ("sceLibcMspaceRealloc", mspace_realloc as *const () as usize),
        ("sceLibcMspaceReallocalign", mspace_reallocalign as *const () as usize),
        ("sceLibcMspacePosixMemalign", mspace_posix_memalign as *const () as usize),
        ("sceLibcMspaceMallocUsableSize", mspace_usable as *const () as usize),
        ("sceLibcMspaceMallocStats", mspace_stats as *const () as usize),
        ("sceLibcMspaceMallocStatsFast", mspace_stats as *const () as usize),
        ("sceKernelLoadStartModule", load_start_module as *const () as usize),
        ("sceKernelDlsym", sce_dlsym as *const () as usize),
        ("sceKernelGetdents", sce_getdents as *const () as usize),
        ("sceKernelCheckReachability", sce_check_reachability as *const () as usize),
        ("sceKernelVirtualQuery", virtual_query as *const () as usize),
        ("sceKernelReserveVirtualRange", reserve_virtual_range as *const () as usize),
        ("scePthreadOnce", pthread_once as *const () as usize),
        ("pthread_once", pthread_once as *const () as usize),
        ("sceLibcHeapGetTraceInfo", zero as *const () as usize),
        ("mmap", mmap as *const () as usize),
        ("sceKernelMmap", mmap as *const () as usize),
        ("munmap", munmap as *const () as usize),
        ("sceKernelMunmap", munmap as *const () as usize),
        ("mprotect", mprotect as *const () as usize),
        ("sceKernelMprotect", mprotect as *const () as usize),
        ("sceKernelMlock", zero as *const () as usize),
        ("sceKernelGetProcParam", get_proc_param as *const () as usize),
        ("__progname", progname()),
        ("_sceKernelRtldSetApplicationHeapAPI", set_application_heap_api as *const () as usize),
        ("_sceKernelRtldThreadAtexitIncrement", zero as *const () as usize),
        ("_sceKernelRtldThreadAtexitDecrement", zero as *const () as usize),
        ("_sceKernelSetThreadAtexitCount", zero as *const () as usize),
        ("_sceKernelSetThreadAtexitReport", zero as *const () as usize),
        ("_sceKernelSetThreadDtors", zero as *const () as usize),
        ("sceKernelIsAddressSanitizerEnabled", zero as *const () as usize),
        ("sceKernelGetSanitizerMallocReplaceExternal", zero as *const () as usize),
        ("sceKernelGetSanitizerNewReplaceExternal", zero as *const () as usize),
        ("sceKernelGetModuleInfoFromAddr", module_info_from_addr as *const () as usize),
        ("sceKernelGetModuleInfoForUnwind", module_info_from_addr as *const () as usize),
        ("sceSysmoduleGetModuleInfoForUnwind", module_info_from_addr as *const () as usize),
        ("sceKernelDebugRaiseException", debug_raise_exception as *const () as usize),
        ("sceKernelDebugRaiseExceptionOnReleaseMode", debug_raise_exception as *const () as usize),
        ("#cfwBSQyr5Ys", zero as *const () as usize),
        ("_sigprocmask", zero as *const () as usize),
        ("signal", zero as *const () as usize),
        ("_is_signal_return", zero as *const () as usize),
        ("clock_gettime", clock_gettime as *const () as usize),
        ("sceKernelClockGettime", clock_gettime as *const () as usize),
        ("gettimeofday", gettimeofday as *const () as usize),
        ("sceKernelGettimeofday", gettimeofday as *const () as usize),
        ("sceKernelConvertUtcToLocaltime", convert_utc_to_localtime as *const () as usize),
        ("sceKernelConvertLocaltimeToUtc", convert_localtime_to_utc as *const () as usize),
        ("nanosleep", nanosleep as *const () as usize),
        ("_nanosleep", nanosleep as *const () as usize),
        ("sceKernelNanosleep", nanosleep as *const () as usize),
        ("sceKernelReadTsc", read_tsc as *const () as usize),
        ("sceKernelGetTscFrequency", tsc_frequency as *const () as usize),
        ("sceKernelGetProcessTimeCounter", read_tsc as *const () as usize),
        ("sceKernelGetProcessTimeCounterFrequency", tsc_frequency as *const () as usize),
        ("sceKernelOpen", sce_open as *const () as usize),
        ("_open", posix_open as *const () as usize),
        ("open", posix_open as *const () as usize),
        ("sceKernelClose", sce_close as *const () as usize),
        ("_close", sce_close as *const () as usize),
        ("close", sce_close as *const () as usize),
        ("sceKernelRead", sce_read as *const () as usize),
        ("_read", sce_read as *const () as usize),
        ("read", sce_read as *const () as usize),
        ("sceKernelWrite", sce_write as *const () as usize),
        ("_write", sce_write as *const () as usize),
        ("write", sce_write as *const () as usize),
        ("sceKernelLseek", sce_lseek as *const () as usize),
        ("lseek", sce_lseek as *const () as usize),
        ("sceKernelStat", sce_stat as *const () as usize),
        ("stat", posix_stat as *const () as usize),
        ("sceKernelFstat", sce_fstat as *const () as usize),
        ("fstat", sce_fstat as *const () as usize),
        ("sceKernelMkdir", sce_mkdir as *const () as usize),
        ("mkdir", sce_mkdir as *const () as usize),
        ("sceKernelRename", sce_rename as *const () as usize),
        ("rename", posix_rename as *const () as usize),
        ("_rename", posix_rename as *const () as usize),
        ("sceKernelFsync", zero as *const () as usize),
        ("fsync", zero as *const () as usize),
        ("_fsync", zero as *const () as usize),
        ("fcntl", zero as *const () as usize),
        ("_fcntl", zero as *const () as usize),
        ("getpid", getpid as *const () as usize),
        ("_getpid", getpid as *const () as usize),
        ("sceKernelGetpid", getpid as *const () as usize),
        ("pthread_condattr_setclock", zero as *const () as usize),
        ("sceKernelAddReadEvent", zero as *const () as usize),
        ("sem_init", sem_init as *const () as usize),
        ("sem_destroy", sem_destroy as *const () as usize),
        ("sem_post", sem_post as *const () as usize),
        ("sem_wait", sem_wait as *const () as usize),
        ("sem_trywait", sem_trywait as *const () as usize),
        ("sem_timedwait", sem_timedwait as *const () as usize),
        ("sem_getvalue", sem_getvalue as *const () as usize),
        ("sceKernelUnlink", sce_unlink as *const () as usize),
        ("unlink", posix_unlink as *const () as usize),
        ("sceKernelRmdir", sce_rmdir as *const () as usize),
        ("rmdir", posix_rmdir as *const () as usize),
        ("_rmdir", posix_rmdir as *const () as usize),
        ("sceKernelPread", sce_pread as *const () as usize),
        ("pread", posix_pread as *const () as usize),
        ("_pread", posix_pread as *const () as usize),
        ("sceKernelPwrite", sce_pwrite as *const () as usize),
        ("pwrite", posix_pwrite as *const () as usize),
        ("_pwrite", posix_pwrite as *const () as usize),
        ("sceKernelFtruncate", sce_ftruncate as *const () as usize),
        ("ftruncate", posix_ftruncate as *const () as usize),
        ("_ftruncate", posix_ftruncate as *const () as usize),
        ("sceKernelTruncate", sce_truncate as *const () as usize),
        ("truncate", posix_truncate as *const () as usize),
        ("sceKernelUuidCreate", sce_uuid_create as *const () as usize),
        ("sceKernelUuidcreate", sce_uuid_create as *const () as usize),
        ("sceKernelStopUnloadModule", zero as *const () as usize),
        ("sceKernelDeleteReadEvent", zero as *const () as usize),
        ("sceKernelDeleteWriteEvent", zero as *const () as usize),
        ("sceKernelAddWriteEvent", zero as *const () as usize),
        ("sched_get_priority_min", sched_get_priority_min as *const () as usize),
        ("sched_get_priority_max", sched_get_priority_max as *const () as usize),
        ("pthread_setschedparam", zero as *const () as usize),
        ("sceKernelSync", zero as *const () as usize),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn world_save_fs_syscalls_work_end_to_end() {
        let base = std::env::temp_dir().join(format!("orbis_fs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("ORBIS_DATA", base.to_str().unwrap());

        let dir_c = CString::new("/data/world_test").unwrap();
        assert_eq!(sce_mkdir(dir_c.as_ptr(), 0o755), 0);

        let file_a = CString::new("/data/world_test/CURRENT.tmp").unwrap();
        let file_b = CString::new("/data/world_test/CURRENT").unwrap();

        // Create and write via sce_open + sce_write + sce_pwrite
        let fd = sce_open(file_a.as_ptr(), O_RDWR | O_CREAT | O_TRUNC, 0o644);
        assert!(fd >= 3);
        let payload = b"MANIFEST-000001\n";
        assert_eq!(sce_write(fd, payload.as_ptr(), payload.len()), payload.len() as i64);

        // Positional pwrite at offset 9 does not disturb seek position (which is at 16)
        assert_eq!(sce_pwrite(fd, b"000042".as_ptr(), 6, 9), 6);
        assert_eq!(sce_lseek(fd, 0, 1), payload.len() as i64);

        // Positional pread reads updated bytes and preserves seek position
        let mut read_buf = [0u8; 16];
        assert_eq!(sce_pread(fd, read_buf.as_mut_ptr(), 16, 0), 16);
        assert_eq!(&read_buf, b"MANIFEST-000042\n");
        assert_eq!(sce_lseek(fd, 0, 1), payload.len() as i64);

        // ftruncate shrinks file to 8 bytes
        assert_eq!(sce_ftruncate(fd, 8), 0);
        let mut st = unsafe { std::mem::zeroed::<Stat>() };
        assert_eq!(sce_fstat(fd, &mut st), 0);
        assert_eq!(st.size, 8);
        assert!(st.mtime[0] > 0);
        assert_eq!(sce_close(fd), 0);

        // Rename over an existing destination (POSIX/LevelDB semantics on Windows)
        let fd_b = sce_open(file_b.as_ptr(), O_RDWR | O_CREAT | O_TRUNC, 0o644);
        assert!(fd_b >= 3);
        assert_eq!(sce_write(fd_b, b"OLD".as_ptr(), 3), 3);
        assert_eq!(sce_close(fd_b), 0);
        assert_eq!(sce_rename(file_a.as_ptr(), file_b.as_ptr()), 0);

        // Verify destination now has the truncated 8-byte content
        let fd_check = sce_open(file_b.as_ptr(), 0, 0);
        assert!(fd_check >= 3);
        let mut check_buf = [0u8; 8];
        assert_eq!(sce_read(fd_check, check_buf.as_mut_ptr(), 8), 8);
        assert_eq!(&check_buf, b"MANIFEST");
        assert_eq!(sce_close(fd_check), 0);

        // Cleanup via unlink + rmdir
        assert_eq!(sce_unlink(file_b.as_ptr()), 0);
        assert_eq!(sce_rmdir(dir_c.as_ptr()), 0);
        // Removing already-deleted dir returns ENOENT
        assert_eq!(sce_rmdir(dir_c.as_ptr()), SCE_KERNEL_ERROR_ENOENT);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn uuid_create_generates_valid_v4_uuids() {
        let mut u1 = [0u8; 16];
        let mut u2 = [0u8; 16];
        assert_eq!(sce_uuid_create(u1.as_mut_ptr()), 0);
        assert_eq!(sce_uuid_create(u2.as_mut_ptr()), 0);
        assert_eq!(u1[6] & 0xF0, 0x40); // version 4
        assert_eq!(u1[8] & 0xC0, 0x80); // RFC 4122 variant
        assert_ne!(u1, u2);
        assert_eq!(sce_uuid_create(std::ptr::null_mut()), SCE_KERNEL_ERROR_EINVAL);
    }
}
