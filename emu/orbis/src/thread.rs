//! Threads and synchronisation: scePthread* and the POSIX names libc.prx imports.
//!
//! Every game thread is a real Windows thread with its own thread pointer (see tls.rs).
//! Game-side objects (pthread_t, mutexes, conditions, rwlocks, attributes) are pointers to
//! Orbis-owned structures. Static initialisers (a zero or small value in the game's variable)
//! are turned into a real object on first use.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;

use windows_sys::Win32::System::Threading::{
    AcquireSRWLockExclusive, AcquireSRWLockShared, DeleteCriticalSection, EnterCriticalSection, InitializeConditionVariable,
    InitializeCriticalSection, InitializeSRWLock, LeaveCriticalSection, ReleaseSRWLockExclusive, ReleaseSRWLockShared,
    SleepConditionVariableCS, SwitchToThread, TryAcquireSRWLockExclusive, TryAcquireSRWLockShared, TryEnterCriticalSection,
    WakeAllConditionVariable, WakeConditionVariable, CONDITION_VARIABLE, CRITICAL_SECTION, SRWLOCK,
};

const EBUSY: i32 = 16;
const ETIMEDOUT: i32 = 60;
const EINVAL: i32 = 22;
const SCE_KERNEL_ERROR_EBUSY: i32 = 0x8002_0010u32 as i32;
const SCE_KERNEL_ERROR_ETIMEDOUT: i32 = 0x8002_003Cu32 as i32;

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Objects created lazily from static initialisers: one lock for all of them.
static LAZY: Mutex<()> = Mutex::new(());

fn lazy<T>(slot: *mut usize, make: impl FnOnce() -> *mut T) -> *mut T {
    unsafe {
        let v = *slot;
        if v > 0x10000 {
            return v as *mut T;
        }
        let _g = LAZY.lock().unwrap_or_else(|e| e.into_inner());
        if *slot <= 0x10000 {
            *slot = make() as usize;
        }
        *slot as *mut T
    }
}

// ── threads ───────────────────────────────────────────────────────────

#[repr(C)]
pub struct Thread {
    /// First field: games sometimes read their thread id from here.
    id: u64,
    tp: usize,
    name: String,
    handle: Option<std::thread::JoinHandle<()>>,
    result: usize,
}

static THREADS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

thread_local! {
    static SELF: RefCell<usize> = const { RefCell::new(0) };
    static SPECIFIC: RefCell<HashMap<u32, usize>> = RefCell::new(HashMap::new());
}

fn new_thread(tp: usize, name: String) -> *mut Thread {
    let t = Box::leak(Box::new(Thread { id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst), tp, name, handle: None, result: 0 }));
    THREADS.lock().unwrap().push(t as *mut Thread as usize);
    t
}

/// The current game thread (the main thread gets one on first use).
fn me() -> *mut Thread {
    SELF.with(|s| {
        let mut s = s.borrow_mut();
        if *s == 0 {
            *s = new_thread(crate::tls::current(), "main".into()) as usize;
        }
        *s as *mut Thread
    })
}

#[repr(C)]
pub struct Attr {
    stack_size: usize,
    detached: i32,
    priority: i32,
    affinity: u64,
}

pub extern "sysv64" fn attr_init(attr: *mut usize) -> i32 {
    let a = Box::leak(Box::new(Attr { stack_size: 0x10_0000, detached: 0, priority: 700, affinity: 0x7F }));
    unsafe { *attr = a as *mut Attr as usize };
    0
}

pub extern "sysv64" fn attr_destroy(_attr: *mut usize) -> i32 {
    0
}

fn attr_of(attr: *mut usize) -> *mut Attr {
    if attr.is_null() {
        return std::ptr::null_mut();
    }
    lazy(attr, || Box::leak(Box::new(Attr { stack_size: 0x10_0000, detached: 0, priority: 700, affinity: 0x7F })) as *mut Attr)
}

pub extern "sysv64" fn attr_setstacksize(attr: *mut usize, size: usize) -> i32 {
    unsafe { (*attr_of(attr)).stack_size = size };
    0
}

pub extern "sysv64" fn attr_getstacksize(attr: *mut usize, size: *mut usize) -> i32 {
    unsafe { *size = (*attr_of(attr)).stack_size };
    0
}

pub extern "sysv64" fn attr_setdetachstate(attr: *mut usize, state: i32) -> i32 {
    unsafe { (*attr_of(attr)).detached = state };
    0
}

pub extern "sysv64" fn attr_getdetachstate(attr: *mut usize, state: *mut i32) -> i32 {
    unsafe { *state = (*attr_of(attr)).detached };
    0
}

pub extern "sysv64" fn attr_setschedparam(attr: *mut usize, param: *const i32) -> i32 {
    if !param.is_null() {
        unsafe { (*attr_of(attr)).priority = *param };
    }
    0
}

pub extern "sysv64" fn attr_getschedparam(attr: *mut usize, param: *mut i32) -> i32 {
    if !param.is_null() {
        unsafe { *param = (*attr_of(attr)).priority };
    }
    0
}

pub extern "sysv64" fn attr_setaffinity(attr: *mut usize, mask: u64) -> i32 {
    unsafe { (*attr_of(attr)).affinity = mask };
    0
}

pub extern "sysv64" fn attr_getaffinity(attr: *mut usize, mask: *mut u64) -> i32 {
    if !mask.is_null() {
        unsafe { *mask = (*attr_of(attr)).affinity };
    }
    0
}

/// scePthreadAttrGet(thread, attr*): the attributes of a running thread.
pub extern "sysv64" fn attr_get(_thread: usize, attr: *mut usize) -> i32 {
    attr_of(attr);
    0
}

pub extern "sysv64" fn ok() -> i32 {
    0
}

struct SendPtr(usize, usize, usize);
unsafe impl Send for SendPtr {}

/// scePthreadCreate(thread*, attr*, entry, arg, name)
pub extern "sysv64" fn create(out: *mut usize, attr: *mut usize, entry: usize, arg: usize, name: *const c_char) -> i32 {
    let a = attr_of(attr);
    // Guarantee at least 2 MiB stack for PS5 worker threads (world-gen / LevelDB + Windows HLE frames).
    let stack = if a.is_null() { 0x20_0000 } else { unsafe { (*a).stack_size } }.max(0x20_0000) + 0x2_0000;
    let label = text(name);
    let tp = crate::tls::new_thread_block();
    let t = new_thread(tp, label.clone());
    unsafe { *out = t as usize };
    let pass = SendPtr(entry, arg, t as usize);
    eprintln!("[orbis] thread created: {} ({} KB stack)", if label.is_empty() { "unnamed" } else { &label }, stack / 1024);
    let handle = std::thread::Builder::new().name(label).stack_size(stack).spawn(move || {
        let p = pass;
        crate::loader::install_thread_guard();
        crate::tls::set_current(unsafe { (*(p.2 as *mut Thread)).tp });
        SELF.with(|s| *s.borrow_mut() = p.2);
        let f: extern "sysv64" fn(usize) -> usize = unsafe { std::mem::transmute(p.0) };
        let r = f(p.1);
        unsafe { (*(p.2 as *mut Thread)).result = r };
    });
    match handle {
        Ok(h) => {
            unsafe { (*t).handle = Some(h) };
            0
        }
        Err(_) => 0x8002_000Cu32 as i32,
    }
}

pub extern "sysv64" fn posix_create(out: *mut usize, attr: *mut usize, entry: usize, arg: usize) -> i32 {
    create(out, attr, entry, arg, std::ptr::null())
}

pub extern "sysv64" fn join(thread: usize, result: *mut usize) -> i32 {
    if thread == 0 {
        return EINVAL;
    }
    let t = thread as *mut Thread;
    if let Some(h) = unsafe { (*t).handle.take() } {
        let _ = h.join();
    }
    if !result.is_null() {
        unsafe { *result = (*t).result };
    }
    0
}

pub extern "sysv64" fn self_() -> usize {
    me() as usize
}

pub extern "sysv64" fn equal(a: usize, b: usize) -> i32 {
    (a == b) as i32
}

pub extern "sysv64" fn exit(value: usize) -> ! {
    unsafe {
        (*me()).result = value;
        windows_sys::Win32::System::Threading::ExitThread(0);
    }
}

pub extern "sysv64" fn yield_() -> i32 {
    unsafe { SwitchToThread() };
    0
}

pub extern "sysv64" fn get_name(thread: usize, out: *mut u8) -> i32 {
    let t = if thread == 0 { me() } else { thread as *mut Thread };
    let name = unsafe { (*t).name.clone() };
    let n = name.len().min(31);
    unsafe {
        std::ptr::copy_nonoverlapping(name.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    0
}

pub extern "sysv64" fn thread_id() -> i32 {
    unsafe { (*me()).id as i32 }
}

pub extern "sysv64" fn get_prio(_t: usize, prio: *mut i32) -> i32 {
    if !prio.is_null() {
        unsafe { *prio = 700 };
    }
    0
}

// ── thread-specific data ──────────────────────────────────────────────

static KEY_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

pub extern "sysv64" fn key_create(key: *mut u32, _dtor: usize) -> i32 {
    unsafe { *key = KEY_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst) };
    0
}

pub extern "sysv64" fn key_delete(_key: u32) -> i32 {
    0
}

pub extern "sysv64" fn getspecific(key: u32) -> usize {
    SPECIFIC.with(|m| m.borrow().get(&key).copied().unwrap_or(0))
}

pub extern "sysv64" fn setspecific(key: u32, value: usize) -> i32 {
    SPECIFIC.with(|m| m.borrow_mut().insert(key, value));
    0
}

// ── mutexes (recursive critical sections) ─────────────────────────────

fn new_cs() -> *mut CRITICAL_SECTION {
    let cs = Box::leak(Box::new(unsafe { std::mem::zeroed::<CRITICAL_SECTION>() }));
    unsafe { InitializeCriticalSection(cs) };
    cs
}

fn cs_of(m: *mut usize) -> *mut CRITICAL_SECTION {
    lazy(m, new_cs)
}

pub extern "sysv64" fn mutex_init(m: *mut usize, _attr: *const usize, _name: *const c_char) -> i32 {
    if m.is_null() {
        return EINVAL;
    }
    unsafe { *m = new_cs() as usize };
    0
}

pub extern "sysv64" fn mutex_destroy(m: *mut usize) -> i32 {
    unsafe {
        if !m.is_null() && *m > 0x10000 {
            DeleteCriticalSection(*m as *mut CRITICAL_SECTION);
            *m = 0;
        }
    }
    0
}

pub extern "sysv64" fn mutex_lock(m: *mut usize) -> i32 {
    if m.is_null() {
        return EINVAL;
    }
    unsafe { EnterCriticalSection(cs_of(m)) };
    0
}

pub extern "sysv64" fn mutex_unlock(m: *mut usize) -> i32 {
    if m.is_null() {
        return EINVAL;
    }
    unsafe { LeaveCriticalSection(cs_of(m)) };
    0
}

pub extern "sysv64" fn mutex_trylock(m: *mut usize) -> i32 {
    if unsafe { TryEnterCriticalSection(cs_of(m)) } != 0 {
        0
    } else {
        EBUSY
    }
}

pub extern "sysv64" fn sce_mutex_trylock(m: *mut usize) -> i32 {
    if mutex_trylock(m) == 0 {
        0
    } else {
        SCE_KERNEL_ERROR_EBUSY
    }
}

/// scePthreadMutexTimedlock(m, usec): try until the time is up.
pub extern "sysv64" fn mutex_timedlock(m: *mut usize, usec: u32) -> i32 {
    let end = std::time::Instant::now() + std::time::Duration::from_micros(usec as u64);
    loop {
        if mutex_trylock(m) == 0 {
            return 0;
        }
        if std::time::Instant::now() >= end {
            return SCE_KERNEL_ERROR_ETIMEDOUT;
        }
        std::thread::yield_now();
    }
}

pub extern "sysv64" fn mutexattr_init(a: *mut usize) -> i32 {
    unsafe { *a = Box::leak(Box::new([0u32; 4])).as_mut_ptr() as usize };
    0
}

pub extern "sysv64" fn mutexattr_set(_a: *mut usize, _v: i32) -> i32 {
    0
}

// ── condition variables ───────────────────────────────────────────────

fn cv_of(c: *mut usize) -> *mut CONDITION_VARIABLE {
    lazy(c, || {
        let cv = Box::leak(Box::new(unsafe { std::mem::zeroed::<CONDITION_VARIABLE>() }));
        unsafe { InitializeConditionVariable(cv) };
        cv as *mut CONDITION_VARIABLE
    })
}

pub extern "sysv64" fn cond_init(c: *mut usize, _attr: *const usize, _name: *const c_char) -> i32 {
    unsafe { *c = 0 };
    cv_of(c);
    0
}

pub extern "sysv64" fn cond_wait(c: *mut usize, m: *mut usize) -> i32 {
    unsafe { SleepConditionVariableCS(cv_of(c), cs_of(m), u32::MAX) };
    0
}

fn wait_ms(c: *mut usize, m: *mut usize, ms: u32) -> bool {
    unsafe { SleepConditionVariableCS(cv_of(c), cs_of(m), ms) != 0 }
}

/// scePthreadCondTimedwait(c, m, usec): relative timeout in microseconds.
pub extern "sysv64" fn sce_cond_timedwait(c: *mut usize, m: *mut usize, usec: u32) -> i32 {
    if wait_ms(c, m, usec.div_ceil(1000)) {
        0
    } else {
        SCE_KERNEL_ERROR_ETIMEDOUT
    }
}

/// pthread_cond_timedwait(c, m, abstime): supports both CLOCK_REALTIME and CLOCK_MONOTONIC.
pub extern "sysv64" fn cond_timedwait(c: *mut usize, m: *mut usize, abs: *const [i64; 2]) -> i32 {
    let (s, n) = if abs.is_null() { (0, 0) } else { unsafe { ((*abs)[0], (*abs)[1]) } };
    let now = if s < 1_000_000_000 {
        crate::kernel::monotonic_now()
    } else {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
    };
    let target = std::time::Duration::new(s.max(0) as u64, n.clamp(0, 999_999_999) as u32);
    let ms = target.saturating_sub(now).as_millis().min(u32::MAX as u128 - 1) as u32;
    if wait_ms(c, m, ms) {
        0
    } else {
        ETIMEDOUT
    }
}

pub extern "sysv64" fn cond_signal(c: *mut usize) -> i32 {
    unsafe { WakeConditionVariable(cv_of(c)) };
    0
}

pub extern "sysv64" fn cond_broadcast(c: *mut usize) -> i32 {
    unsafe { WakeAllConditionVariable(cv_of(c)) };
    0
}

pub extern "sysv64" fn cond_destroy(_c: *mut usize) -> i32 {
    0
}

// ── reader/writer locks ───────────────────────────────────────────────

#[repr(C)]
struct RwLock {
    lock: SRWLOCK,
    /// Thread holding it exclusively (0 = none/shared), so unlock knows which release to use.
    writer: std::sync::atomic::AtomicU64,
}

fn rw_of(r: *mut usize) -> *mut RwLock {
    lazy(r, || {
        let l = Box::leak(Box::new(RwLock { lock: unsafe { std::mem::zeroed() }, writer: std::sync::atomic::AtomicU64::new(0) }));
        unsafe { InitializeSRWLock(&mut l.lock) };
        l as *mut RwLock
    })
}

fn tid() -> u64 {
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() as u64 }
}

pub extern "sysv64" fn rw_init(r: *mut usize, _attr: *const usize, _name: *const c_char) -> i32 {
    unsafe { *r = 0 };
    rw_of(r);
    0
}

pub extern "sysv64" fn rw_rdlock(r: *mut usize) -> i32 {
    unsafe { AcquireSRWLockShared(&mut (*rw_of(r)).lock) };
    0
}

pub extern "sysv64" fn rw_wrlock(r: *mut usize) -> i32 {
    let l = rw_of(r);
    unsafe {
        AcquireSRWLockExclusive(&mut (*l).lock);
        (*l).writer.store(tid(), std::sync::atomic::Ordering::SeqCst);
    }
    0
}

pub extern "sysv64" fn rw_tryrdlock(r: *mut usize) -> i32 {
    if unsafe { TryAcquireSRWLockShared(&mut (*rw_of(r)).lock) } != 0 {
        0
    } else {
        SCE_KERNEL_ERROR_EBUSY
    }
}

pub extern "sysv64" fn rw_trywrlock(r: *mut usize) -> i32 {
    let l = rw_of(r);
    unsafe {
        if TryAcquireSRWLockExclusive(&mut (*l).lock) != 0 {
            (*l).writer.store(tid(), std::sync::atomic::Ordering::SeqCst);
            0
        } else {
            SCE_KERNEL_ERROR_EBUSY
        }
    }
}

pub extern "sysv64" fn rw_unlock(r: *mut usize) -> i32 {
    let l = rw_of(r);
    unsafe {
        if (*l).writer.load(std::sync::atomic::Ordering::SeqCst) == tid() {
            (*l).writer.store(0, std::sync::atomic::Ordering::SeqCst);
            ReleaseSRWLockExclusive(&mut (*l).lock);
        } else {
            ReleaseSRWLockShared(&mut (*l).lock);
        }
    }
    0
}

pub extern "sysv64" fn rw_destroy(_r: *mut usize) -> i32 {
    0
}

// ── once ──────────────────────────────────────────────────────────────

/// pthread_once: { int state; mutex }. 0 = not run, 1 = running, 2 = done.
pub extern "sysv64" fn once(ctrl: *mut i32, f: extern "sysv64" fn()) -> i32 {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    unsafe {
        if std::ptr::read_volatile(ctrl) == 2 {
            return 0;
        }
        // Reentrant-safe enough: the init function runs outside the lock if it re-enters.
        let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if *ctrl == 0 {
            *ctrl = 1;
            drop(g);
            f();
            std::ptr::write_volatile(ctrl, 2);
        }
    }
    0
}

/// (name, address) for everything here.
// ── kernel semaphores ─────────────────────────────────────────────────

struct Sema {
    state: std::sync::Mutex<(i32, i32)>, // (count, max_count)
    cv: std::sync::Condvar,
}

const SCE_ETIMEDOUT: i32 = 0x8002_003Cu32 as i32;
const SCE_EBUSY: i32 = 0x8002_0010u32 as i32;
const SCE_EINVAL: i32 = 0x8002_0016u32 as i32;

pub extern "sysv64" fn sema_create(
    out: *mut usize,
    _name: *const c_char,
    _attr: u32,
    init_count: i32,
    max_count: i32,
    _opt: *const c_void,
) -> i32 {
    if out.is_null() || init_count < 0 || max_count <= 0 || init_count > max_count {
        return SCE_EINVAL;
    }
    let s = Box::leak(Box::new(Sema {
        state: std::sync::Mutex::new((init_count, max_count)),
        cv: std::sync::Condvar::new(),
    }));
    unsafe { *out = s as *mut Sema as usize };
    0
}

pub extern "sysv64" fn sema_delete(_sem: usize) -> i32 {
    0
}

pub extern "sysv64" fn sema_wait(sem: usize, need: i32, timeout_us: *mut u32) -> i32 {
    if sem < 0x10000 || need <= 0 {
        return SCE_EINVAL;
    }
    let s = unsafe { &*(sem as *const Sema) };
    let mut g = s.state.lock().unwrap_or_else(|e| e.into_inner());
    if timeout_us.is_null() {
        while g.0 < need {
            g = s.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.0 -= need;
        return 0;
    }
    let us = unsafe { *timeout_us };
    let deadline = std::time::Instant::now() + std::time::Duration::from_micros(us as u64);
    while g.0 < need {
        let now = std::time::Instant::now();
        if now >= deadline {
            unsafe { *timeout_us = 0 };
            return SCE_ETIMEDOUT;
        }
        let (ng, res) = s.cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner());
        g = ng;
        if res.timed_out() && g.0 < need {
            unsafe { *timeout_us = 0 };
            return SCE_ETIMEDOUT;
        }
    }
    g.0 -= need;
    0
}

pub extern "sysv64" fn sema_poll(sem: usize, need: i32) -> i32 {
    if sem < 0x10000 || need <= 0 {
        return SCE_EINVAL;
    }
    let s = unsafe { &*(sem as *const Sema) };
    let mut g = s.state.lock().unwrap_or_else(|e| e.into_inner());
    if g.0 < need {
        return SCE_EBUSY;
    }
    g.0 -= need;
    0
}

pub extern "sysv64" fn sema_signal(sem: usize, count: i32) -> i32 {
    if sem < 0x10000 || count <= 0 {
        return SCE_EINVAL;
    }
    let s = unsafe { &*(sem as *const Sema) };
    let mut g = s.state.lock().unwrap_or_else(|e| e.into_inner());
    g.0 = g.0.saturating_add(count).min(g.1);
    s.cv.notify_all();
    0
}

pub extern "sysv64" fn sema_cancel(sem: usize, set_count: i32, _num_waiters: *mut i32) -> i32 {
    if sem < 0x10000 {
        return SCE_EINVAL;
    }
    let s = unsafe { &*(sem as *const Sema) };
    let mut g = s.state.lock().unwrap_or_else(|e| e.into_inner());
    g.0 = set_count.clamp(0, g.1);
    s.cv.notify_all();
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    let mut v: Vec<(&'static str, usize)> = vec![
        ("scePthreadCreate", create as *const () as usize),
        ("pthread_create", posix_create as *const () as usize),
        ("pthread_create_name_np", create as *const () as usize),
        ("scePthreadJoin", join as *const () as usize),
        ("pthread_join", join as *const () as usize),
        ("scePthreadDetach", ok as *const () as usize),
        ("pthread_detach", ok as *const () as usize),
        ("scePthreadSelf", self_ as *const () as usize),
        ("pthread_self", self_ as *const () as usize),
        ("scePthreadEqual", equal as *const () as usize),
        ("pthread_equal", equal as *const () as usize),
        ("scePthreadExit", exit as *const () as usize),
        ("pthread_exit", exit as *const () as usize),
        ("scePthreadYield", yield_ as *const () as usize),
        ("pthread_yield", yield_ as *const () as usize),
        ("sched_yield", yield_ as *const () as usize),
        ("scePthreadGetname", get_name as *const () as usize),
        ("scePthreadRename", ok as *const () as usize),
        ("pthread_setname_np", ok as *const () as usize),
        ("scePthreadGetthreadid", thread_id as *const () as usize),
        ("pthread_getthreadid_np", thread_id as *const () as usize),
        ("#EI-5-jlq2dE", thread_id as *const () as usize),
        ("scePthreadGetprio", get_prio as *const () as usize),
        ("scePthreadSetprio", ok as *const () as usize),
        ("scePthreadSetaffinity", ok as *const () as usize),
        ("scePthreadGetaffinity", attr_getaffinity as *const () as usize),
        ("scePthreadAttrInit", attr_init as *const () as usize),
        ("pthread_attr_init", attr_init as *const () as usize),
        ("scePthreadAttrDestroy", attr_destroy as *const () as usize),
        ("pthread_attr_destroy", attr_destroy as *const () as usize),
        ("scePthreadAttrGet", attr_get as *const () as usize),
        ("pthread_getattr_np", attr_get as *const () as usize),
        ("scePthreadAttrSetstacksize", attr_setstacksize as *const () as usize),
        ("pthread_attr_setstacksize", attr_setstacksize as *const () as usize),
        ("scePthreadAttrGetstacksize", attr_getstacksize as *const () as usize),
        ("pthread_attr_getstacksize", attr_getstacksize as *const () as usize),
        ("scePthreadAttrSetdetachstate", attr_setdetachstate as *const () as usize),
        ("pthread_attr_setdetachstate", attr_setdetachstate as *const () as usize),
        ("scePthreadAttrGetdetachstate", attr_getdetachstate as *const () as usize),
        ("pthread_attr_getdetachstate", attr_getdetachstate as *const () as usize),
        ("scePthreadAttrSetschedparam", attr_setschedparam as *const () as usize),
        ("scePthreadAttrGetschedparam", attr_getschedparam as *const () as usize),
        ("pthread_attr_setschedparam", attr_setschedparam as *const () as usize),
        ("pthread_attr_getschedparam", attr_getschedparam as *const () as usize),
        ("scePthreadAttrSetaffinity", attr_setaffinity as *const () as usize),
        ("scePthreadAttrGetaffinity", attr_getaffinity as *const () as usize),
        ("scePthreadAttrSetinheritsched", mutexattr_set as *const () as usize),
        ("scePthreadAttrSetschedpolicy", mutexattr_set as *const () as usize),
        ("scePthreadAttrSetguardsize", mutexattr_set as *const () as usize),
        ("pthread_attr_setinheritsched", mutexattr_set as *const () as usize),
        ("pthread_attr_setguardsize", mutexattr_set as *const () as usize),
        ("scePthreadKeyCreate", key_create as *const () as usize),
        ("pthread_key_create", key_create as *const () as usize),
        ("scePthreadKeyDelete", key_delete as *const () as usize),
        ("pthread_key_delete", key_delete as *const () as usize),
        ("scePthreadGetspecific", getspecific as *const () as usize),
        ("pthread_getspecific", getspecific as *const () as usize),
        ("scePthreadSetspecific", setspecific as *const () as usize),
        ("pthread_setspecific", setspecific as *const () as usize),
        ("scePthreadMutexInit", mutex_init as *const () as usize),
        ("pthread_mutex_init", mutex_init as *const () as usize),
        ("scePthreadMutexDestroy", mutex_destroy as *const () as usize),
        ("pthread_mutex_destroy", mutex_destroy as *const () as usize),
        ("scePthreadMutexLock", mutex_lock as *const () as usize),
        ("pthread_mutex_lock", mutex_lock as *const () as usize),
        ("scePthreadMutexUnlock", mutex_unlock as *const () as usize),
        ("pthread_mutex_unlock", mutex_unlock as *const () as usize),
        ("scePthreadMutexTrylock", sce_mutex_trylock as *const () as usize),
        ("pthread_mutex_trylock", mutex_trylock as *const () as usize),
        ("scePthreadMutexTimedlock", mutex_timedlock as *const () as usize),
        ("scePthreadMutexattrInit", mutexattr_init as *const () as usize),
        ("pthread_mutexattr_init", mutexattr_init as *const () as usize),
        ("scePthreadMutexattrDestroy", ok as *const () as usize),
        ("pthread_mutexattr_destroy", ok as *const () as usize),
        ("scePthreadMutexattrSettype", mutexattr_set as *const () as usize),
        ("pthread_mutexattr_settype", mutexattr_set as *const () as usize),
        ("scePthreadMutexattrSetprotocol", mutexattr_set as *const () as usize),
        ("pthread_mutexattr_setprotocol", mutexattr_set as *const () as usize),
        ("scePthreadCondInit", cond_init as *const () as usize),
        ("pthread_cond_init", cond_init as *const () as usize),
        ("scePthreadCondDestroy", cond_destroy as *const () as usize),
        ("pthread_cond_destroy", cond_destroy as *const () as usize),
        ("scePthreadCondWait", cond_wait as *const () as usize),
        ("pthread_cond_wait", cond_wait as *const () as usize),
        ("scePthreadCondTimedwait", sce_cond_timedwait as *const () as usize),
        ("pthread_cond_timedwait", cond_timedwait as *const () as usize),
        ("scePthreadCondSignal", cond_signal as *const () as usize),
        ("pthread_cond_signal", cond_signal as *const () as usize),
        ("scePthreadCondBroadcast", cond_broadcast as *const () as usize),
        ("pthread_cond_broadcast", cond_broadcast as *const () as usize),
        ("scePthreadCondattrInit", mutexattr_init as *const () as usize),
        ("scePthreadCondattrDestroy", ok as *const () as usize),
        ("pthread_condattr_init", mutexattr_init as *const () as usize),
        ("pthread_condattr_destroy", ok as *const () as usize),
        ("scePthreadRwlockInit", rw_init as *const () as usize),
        ("pthread_rwlock_init", rw_init as *const () as usize),
        ("scePthreadRwlockDestroy", rw_destroy as *const () as usize),
        ("pthread_rwlock_destroy", rw_destroy as *const () as usize),
        ("scePthreadRwlockRdlock", rw_rdlock as *const () as usize),
        ("pthread_rwlock_rdlock", rw_rdlock as *const () as usize),
        ("scePthreadRwlockWrlock", rw_wrlock as *const () as usize),
        ("pthread_rwlock_wrlock", rw_wrlock as *const () as usize),
        ("scePthreadRwlockTryrdlock", rw_tryrdlock as *const () as usize),
        ("scePthreadRwlockTrywrlock", rw_trywrlock as *const () as usize),
        ("scePthreadRwlockUnlock", rw_unlock as *const () as usize),
        ("pthread_rwlock_unlock", rw_unlock as *const () as usize),
        ("scePthreadRwlockattrInit", mutexattr_init as *const () as usize),
        ("scePthreadRwlockattrDestroy", ok as *const () as usize),
        ("scePthreadOnce", once as *const () as usize),
        ("pthread_once", once as *const () as usize),
        ("sceKernelCreateSema", sema_create as *const () as usize),
        ("sceKernelDeleteSema", sema_delete as *const () as usize),
        ("sceKernelWaitSema", sema_wait as *const () as usize),
        ("sceKernelPollSema", sema_poll as *const () as usize),
        ("sceKernelSignalSema", sema_signal as *const () as usize),
        ("sceKernelCancelSema", sema_cancel as *const () as usize),
    ];
    let _ = (&mut v, std::ptr::null::<c_void>());
    v
}
