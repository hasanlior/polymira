//! C and C++ runtime functions games call during start-up: process exit, static initialisers,
//! memory, strings, and locks (single-threaded for now).

use std::ffi::{c_char, c_void};
use std::io::Write;

use crate::hle::{free, malloc};

// ── process ───────────────────────────────────────────────────────────

pub extern "sysv64" fn init_env() {}

pub extern "sysv64" fn atexit(_f: *const c_void) -> i32 {
    0
}

pub extern "sysv64" fn cxa_atexit(_f: *const c_void, _arg: *const c_void, _dso: *const c_void) -> i32 {
    0
}

pub extern "sysv64" fn exit(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    eprintln!("[orbis] the game called exit({code})");
    std::process::exit(code)
}

pub extern "sysv64" fn abort() -> ! {
    eprintln!("[orbis] the game called abort()");
    std::process::exit(134)
}

pub extern "sysv64" fn stack_chk_fail() -> ! {
    eprintln!("[orbis] stack protector: the game detected stack corruption");
    std::process::exit(134)
}

/// Data symbol: the stack protector canary.
pub static mut STACK_CHK_GUARD: u64 = 0x2A9F_5D3C_71E4_0B86;
static mut ERRNO: i32 = 0;

pub extern "sysv64" fn error() -> *mut i32 {
    std::ptr::addr_of_mut!(ERRNO)
}

/// Dinkumware _Xtime_get_ticks: 100 ns units since 1970.
pub extern "sysv64" fn xtime_get_ticks() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| (d.as_nanos() / 100) as i64).unwrap_or(0)
}

// ── C++ static initialisation guards ──────────────────────────────────

/// Returns 1 when the guarded static still has to be constructed.
pub extern "sysv64" fn cxa_guard_acquire(guard: *mut u8) -> i32 {
    if unsafe { *guard } == 0 { 1 } else { 0 }
}

pub extern "sysv64" fn cxa_guard_release(guard: *mut u8) {
    unsafe { *guard = 1 };
}

pub extern "sysv64" fn cxa_guard_abort(_guard: *mut u8) {}

pub extern "sysv64" fn cxa_pure_virtual() -> ! {
    eprintln!("[orbis] pure virtual function called");
    std::process::exit(134)
}

// ── memory ────────────────────────────────────────────────────────────

pub extern "sysv64" fn calloc(n: usize, size: usize) -> *mut c_void {
    let total = n.saturating_mul(size);
    let p = malloc(total);
    if !p.is_null() {
        unsafe { std::ptr::write_bytes(p as *mut u8, 0, total) };
    }
    p
}

pub extern "sysv64" fn realloc(p: *mut c_void, size: usize) -> *mut c_void {
    if p.is_null() {
        return malloc(size);
    }
    let old = unsafe { *((p as *mut u8).sub(16) as *mut usize) };
    let n = malloc(size);
    if !n.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(p as *const u8, n as *mut u8, old.min(size)) };
        free(p);
    }
    n
}

/// Aligned allocations: over-allocate and keep the real block pointer just before the result.
pub extern "sysv64" fn memalign(align: usize, size: usize) -> *mut c_void {
    let align = align.max(16).next_power_of_two();
    let raw = malloc(size + align + 16) as usize;
    if raw == 0 {
        return std::ptr::null_mut();
    }
    let user = (raw + 16 + align - 1) & !(align - 1);
    unsafe { *((user - 8) as *mut usize) = raw };
    // Mark as aligned so free() via free_any knows.
    unsafe { *((user - 16) as *mut usize) = usize::MAX };
    user as *mut c_void
}

pub extern "sysv64" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> i32 {
    let p = memalign(align, size);
    unsafe { *out = p };
    if p.is_null() { 12 } else { 0 }
}

pub extern "sysv64" fn aligned_alloc(align: usize, size: usize) -> *mut c_void {
    memalign(align, size)
}

/// free() that also understands blocks from memalign().
pub extern "sysv64" fn free_any(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    unsafe {
        if *((p as *mut u8).sub(16) as *mut usize) == usize::MAX {
            free(*((p as *mut u8).sub(8) as *mut usize) as *mut c_void);
        } else {
            free(p);
        }
    }
}

pub extern "sysv64" fn op_new(size: usize) -> *mut c_void {
    malloc(size.max(1))
}

pub extern "sysv64" fn op_new_nothrow(size: usize, _tag: *const c_void) -> *mut c_void {
    malloc(size.max(1))
}

pub extern "sysv64" fn op_new_aligned(size: usize, align: usize) -> *mut c_void {
    memalign(align, size.max(1))
}

pub extern "sysv64" fn op_delete(p: *mut c_void) {
    free_any(p)
}

pub extern "sysv64" fn op_delete_sized(p: *mut c_void, _size: usize) {
    free_any(p)
}

// ── strings and memory blocks ─────────────────────────────────────────

pub extern "sysv64" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe { std::ptr::copy(src, dst, n) };
    dst
}

pub extern "sysv64" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (*a.add(i), *b.add(i)) };
        if x != y {
            return x as i32 - y as i32;
        }
    }
    0
}

pub extern "sysv64" fn memchr(s: *const u8, c: i32, n: usize) -> *const u8 {
    for i in 0..n {
        if unsafe { *s.add(i) } == c as u8 {
            return unsafe { s.add(i) };
        }
    }
    std::ptr::null()
}

pub extern "sysv64" fn strcmp(a: *const c_char, b: *const c_char) -> i32 {
    strncmp(a, b, usize::MAX)
}

pub extern "sysv64" fn strncmp(a: *const c_char, b: *const c_char, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = unsafe { (*a.add(i) as u8, *b.add(i) as u8) };
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
    }
    0
}

pub extern "sysv64" fn strcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char {
    let mut i = 0;
    loop {
        let c = unsafe { *src.add(i) };
        unsafe { *dst.add(i) = c };
        if c == 0 {
            return dst;
        }
        i += 1;
    }
}

pub extern "sysv64" fn strncpy(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char {
    let mut end = false;
    for i in 0..n {
        let c = if end { 0 } else { unsafe { *src.add(i) } };
        end |= c == 0;
        unsafe { *dst.add(i) = c };
    }
    dst
}

pub extern "sysv64" fn strchr(s: *const c_char, c: i32) -> *const c_char {
    let mut i = 0;
    loop {
        let ch = unsafe { *s.add(i) };
        if ch as u8 == c as u8 {
            return unsafe { s.add(i) };
        }
        if ch == 0 {
            return std::ptr::null();
        }
        i += 1;
    }
}

pub extern "sysv64" fn strrchr(s: *const c_char, c: i32) -> *const c_char {
    let mut last = std::ptr::null();
    let mut i = 0;
    loop {
        let ch = unsafe { *s.add(i) };
        if ch as u8 == c as u8 {
            last = unsafe { s.add(i) };
        }
        if ch == 0 {
            return last;
        }
        i += 1;
    }
}

// ── locks (single-threaded for now: always succeed) ───────────────────

pub extern "sysv64" fn ok0() -> i32 {
    0
}

pub extern "sysv64" fn ok1(_a: *const c_void) -> i32 {
    0
}

pub extern "sysv64" fn ok2(_a: *const c_void, _b: *const c_void) -> i32 {
    0
}

// ── C++ library mutexes (Dinkumware _Mtx_*) ──────────────────────────
// _Mtx_init creates the mutex object and stores its address in *mtx; the other calls take
// that address. Single-threaded for now, so locking always succeeds.

pub extern "sysv64" fn mtx_init(mtx: *mut usize, _kind: i32) -> i32 {
    let obj = calloc(1, 64) as usize;
    if !mtx.is_null() {
        unsafe { *mtx = obj };
    }
    0
}

pub extern "sysv64" fn mtx_init_with_name(mtx: *mut usize, kind: i32, _name: *const c_char) -> i32 {
    mtx_init(mtx, kind)
}

pub extern "sysv64" fn mtx_destroy(_mtx: usize) {}

pub extern "sysv64" fn thread_id() -> i32 {
    1
}

// ── thread-specific data (keys) ───────────────────────────────────────

static KEYS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

pub extern "sysv64" fn key_create(key: *mut u32, _dtor: *const c_void) -> i32 {
    let mut k = KEYS.lock().unwrap();
    k.push(0);
    unsafe { *key = (k.len() - 1) as u32 };
    0
}

pub extern "sysv64" fn key_delete(_key: u32) -> i32 {
    0
}

pub extern "sysv64" fn getspecific(key: u32) -> usize {
    KEYS.lock().unwrap().get(key as usize).copied().unwrap_or(0)
}

pub extern "sysv64" fn setspecific(key: u32, value: usize) -> i32 {
    let mut k = KEYS.lock().unwrap();
    if let Some(slot) = k.get_mut(key as usize) {
        *slot = value;
        0
    } else {
        22
    }
}

pub extern "sysv64" fn thread_self() -> usize {
    crate::tls::THREAD_POINTER
}

/// (name, address) for everything above. Mutex/condition functions share the success stubs.
pub fn functions() -> Vec<(&'static str, usize)> {
    let mut v: Vec<(&'static str, usize)> = vec![
        ("_init_env", init_env as *const () as usize),
        ("atexit", atexit as *const () as usize),
        ("__cxa_atexit", cxa_atexit as *const () as usize),
        ("exit", exit as *const () as usize),
        ("_Exit", exit as *const () as usize),
        ("abort", abort as *const () as usize),
        ("__stack_chk_fail", stack_chk_fail as *const () as usize),
        ("__stack_chk_guard", std::ptr::addr_of!(STACK_CHK_GUARD) as usize),
        ("__error", error as *const () as usize),
        ("_Xtime_get_ticks", xtime_get_ticks as *const () as usize),
        ("__cxa_guard_acquire", cxa_guard_acquire as *const () as usize),
        ("__cxa_guard_release", cxa_guard_release as *const () as usize),
        ("__cxa_guard_abort", cxa_guard_abort as *const () as usize),
        ("__cxa_pure_virtual", cxa_pure_virtual as *const () as usize),
        ("calloc", calloc as *const () as usize),
        ("realloc", realloc as *const () as usize),
        ("memalign", memalign as *const () as usize),
        ("posix_memalign", posix_memalign as *const () as usize),
        ("aligned_alloc", aligned_alloc as *const () as usize),
        ("_Znwm", op_new as *const () as usize),
        ("_Znam", op_new as *const () as usize),
        ("_ZnwmRKSt9nothrow_t", op_new_nothrow as *const () as usize),
        ("_ZnamRKSt9nothrow_t", op_new_nothrow as *const () as usize),
        ("_ZnwmSt11align_val_t", op_new_aligned as *const () as usize),
        ("_ZdlPv", op_delete as *const () as usize),
        ("_ZdaPv", op_delete as *const () as usize),
        ("_ZdlPvm", op_delete_sized as *const () as usize),
        ("_ZdaPvm", op_delete_sized as *const () as usize),
        ("_ZdlPvRKSt9nothrow_t", op_delete_sized as *const () as usize),
        ("_ZdlPvSt11align_val_t", op_delete_sized as *const () as usize),
        ("memmove", memmove as *const () as usize),
        ("memcmp", memcmp as *const () as usize),
        ("memchr", memchr as *const () as usize),
        ("strcmp", strcmp as *const () as usize),
        ("strncmp", strncmp as *const () as usize),
        ("strcpy", strcpy as *const () as usize),
        ("strncpy", strncpy as *const () as usize),
        ("strchr", strchr as *const () as usize),
        ("strrchr", strrchr as *const () as usize),
        ("pthread_self", thread_self as *const () as usize),
        ("_Mtx_init", mtx_init as *const () as usize),
        ("_Mtx_init_with_name", mtx_init_with_name as *const () as usize),
        ("_Mtx_destroy", mtx_destroy as *const () as usize),
        ("pthread_getthreadid_np", thread_id as *const () as usize),
        // Known by NID only (name not in the list):
        ("#EI-5-jlq2dE", thread_id as *const () as usize), // pthread_getthreadid_np
        ("pthread_key_create", key_create as *const () as usize),
        ("pthread_key_delete", key_delete as *const () as usize),
        ("pthread_getspecific", getspecific as *const () as usize),
        ("pthread_setspecific", setspecific as *const () as usize),
        ("scePthreadKeyCreate", key_create as *const () as usize),
        ("scePthreadKeyDelete", key_delete as *const () as usize),
        ("scePthreadGetspecific", getspecific as *const () as usize),
        ("scePthreadSetspecific", setspecific as *const () as usize),
        ("scePthreadSelf", thread_self as *const () as usize),
    ];
    for name in [
        "_Mtx_lock", "_Mtx_unlock", "_Mtxinit", "_Mtxlock", "_Mtxunlock", "_Mtxdst", "_Cnd_init",
        "_Cnd_signal", "_Cnd_broadcast", "_Cnd_destroy", "_Locksyslock", "_Unlocksyslock", "pthread_mutex_init", "pthread_mutex_lock",
        "pthread_mutex_unlock", "pthread_mutex_trylock", "pthread_mutex_destroy", "pthread_mutexattr_init", "pthread_mutexattr_settype",
        "pthread_mutexattr_destroy", "pthread_cond_init", "pthread_cond_signal", "pthread_cond_broadcast", "pthread_cond_destroy",
        "scePthreadMutexInit", "scePthreadMutexLock", "scePthreadMutexUnlock", "scePthreadMutexTrylock", "scePthreadMutexDestroy",
        "scePthreadMutexattrInit", "scePthreadMutexattrSettype", "scePthreadMutexattrSetprotocol", "scePthreadMutexattrDestroy",
        "scePthreadCondInit", "scePthreadCondSignal", "scePthreadCondBroadcast", "scePthreadCondDestroy",
    ] {
        v.push((name, ok2 as *const () as usize));
    }
    let _ = (ok0 as *const () as usize, ok1 as *const () as usize);
    v
}
