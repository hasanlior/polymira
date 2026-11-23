//! High-level emulation: PS4 system functions implemented natively.
//!
//! Games call these through their import tables. PS4 code uses the System V calling
//! convention, so every function here is `extern "sysv64"`; Rust generates the bridge to
//! Windows' own convention for us.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

use crate::nid::nid;

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn start() -> &'static Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    T.get_or_init(Instant::now)
}

// ── libkernel ────────────────────────────────────────────────────────

extern "sysv64" fn sce_kernel_debug_out_text(_channel: i32, msg: *const c_char) -> i32 {
    print!("{}", text(msg));
    let _ = std::io::stdout().flush();
    0
}

extern "sysv64" fn sce_kernel_usleep(us: u32) -> i32 {
    std::thread::sleep(std::time::Duration::from_micros(us as u64));
    0
}

extern "sysv64" fn sce_kernel_get_process_time() -> u64 {
    start().elapsed().as_micros() as u64
}

extern "sysv64" fn sce_kernel_get_cpumode() -> i32 {
    0
}

extern "sysv64" fn sce_kernel_is_neo_mode() -> i32 {
    0
}

// ── libc ─────────────────────────────────────────────────────────────

extern "sysv64" fn puts(s: *const c_char) -> i32 {
    println!("{}", text(s));
    0
}

extern "sysv64" fn strlen(s: *const c_char) -> usize {
    text(s).len()
}

extern "sysv64" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe { std::ptr::copy_nonoverlapping(src, dst, n) };
    dst
}

extern "sysv64" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    unsafe { std::ptr::write_bytes(dst, c as u8, n) };
    dst
}

/// malloc/free: a size header in front of each block so free() knows the layout.
pub extern "sysv64" fn malloc(size: usize) -> *mut c_void {
    let layout = std::alloc::Layout::from_size_align(size + 16, 16).unwrap();
    unsafe {
        let p = std::alloc::alloc(layout);
        if p.is_null() {
            return std::ptr::null_mut();
        }
        *(p as *mut usize) = size;
        p.add(16) as *mut c_void
    }
}

pub extern "sysv64" fn free(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    unsafe {
        let base = (p as *mut u8).sub(16);
        let size = *(base as *mut usize);
        std::alloc::dealloc(base, std::alloc::Layout::from_size_align(size + 16, 16).unwrap());
    }
}

// ── system services ──────────────────────────────────────────────────

extern "sysv64" fn sce_user_service_initialize(_params: *const c_void) -> i32 {
    0
}

extern "sysv64" fn sce_user_service_get_initial_user(user: *mut i32) -> i32 {
    if !user.is_null() {
        unsafe { *user = 1 };
    }
    0
}

extern "sysv64" fn sce_sysmodule_load_module(_id: u16) -> i32 {
    0
}

extern "sysv64" fn sce_pad_init() -> i32 {
    0
}

extern "sysv64" fn sce_pad_open(_user: i32, _kind: i32, _index: i32, _param: *const c_void) -> i32 {
    1
}

/// Implemented functions by name. The loader matches imports by NID.
pub fn implemented() -> Vec<(&'static str, usize)> {
    vec![
    ("sceKernelDebugOutText", sce_kernel_debug_out_text as *const () as usize),
    ("sceKernelUsleep", sce_kernel_usleep as *const () as usize),
    ("sceKernelGetProcessTime", sce_kernel_get_process_time as *const () as usize),
    ("sceKernelGetCpumode", sce_kernel_get_cpumode as *const () as usize),
    ("sceKernelIsNeoMode", sce_kernel_is_neo_mode as *const () as usize),
    ("puts", puts as *const () as usize),
    ("strlen", strlen as *const () as usize),
    ("memcpy", memcpy as *const () as usize),
    ("memset", memset as *const () as usize),
    ("malloc", malloc as *const () as usize),
    ("free", crate::hle_rt::free_any as *const () as usize),
    ("sceUserServiceInitialize", sce_user_service_initialize as *const () as usize),
    ("sceUserServiceGetInitialUser", sce_user_service_get_initial_user as *const () as usize),
    ("sceSysmoduleLoadModule", sce_sysmodule_load_module as *const () as usize),
    ("scePadInit", sce_pad_init as *const () as usize),
    ("scePadOpen", sce_pad_open as *const () as usize),
    ]
}

/// Functions Orbis must provide even when a game module exports the same name.
pub fn forced(nid_: &str) -> bool {
    static F: OnceLock<Vec<String>> = OnceLock::new();
    static MSPACE: OnceLock<Vec<String>> = OnceLock::new();
    F.get_or_init(|| ["__tls_get_addr"].iter().map(|n| nid(n)).collect()).iter().any(|n| n == nid_) || MSPACE.get_or_init(|| crate::kernel::functions().iter().filter(|(n, _)| n.starts_with("sceLibcMspace")).map(|(n, _)| nid(n)).collect::<Vec<_>>()).iter().any(|n| n == nid_)
}

/// NID → (name, address) for every implemented function.
pub fn table() -> &'static HashMap<String, (&'static str, usize)> {
    static T: OnceLock<HashMap<String, (&'static str, usize)>> = OnceLock::new();
    T.get_or_init(|| crate::hle_rt::functions().into_iter().chain(implemented()).chain(crate::kernel::functions()).chain(crate::thread::functions()).chain(crate::system::functions()).chain(crate::gpu::functions()).chain(crate::agc::functions()).chain(crate::equeue::functions()).chain(crate::video::functions()).map(|(name, addr)| (name.strip_prefix('#').map(str::to_string).unwrap_or_else(|| nid(name)), (name.trim_start_matches('#'), addr))).collect())
}
