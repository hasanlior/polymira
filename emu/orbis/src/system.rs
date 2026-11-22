//! System services: the signed-in user, system settings, random numbers, video output.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const SCE_USER_SERVICE_ERROR_NO_EVENT: i32 = 0x8096_0007u32 as i32;
const USER: i32 = 1;

/// sceUserServiceGetEvent(event*): { int type; int user }. One "user logged in", then none.
pub extern "sysv64" fn user_get_event(ev: *mut [i32; 2]) -> i32 {
    static SENT: AtomicBool = AtomicBool::new(false);
    if SENT.swap(true, Ordering::SeqCst) {
        return SCE_USER_SERVICE_ERROR_NO_EVENT;
    }
    if !ev.is_null() {
        unsafe { *ev = [0, USER] };
    }
    0
}

/// sceUserServiceGetLoginUserIdList(list*): four user ids, -1 = empty.
pub extern "sysv64" fn user_login_list(list: *mut [i32; 4]) -> i32 {
    if !list.is_null() {
        unsafe { *list = [USER, -1, -1, -1] };
    }
    0
}

pub extern "sysv64" fn user_initial(user: *mut i32) -> i32 {
    if !user.is_null() {
        unsafe { *user = USER };
    }
    0
}

/// sceUserServiceGetUserName(user, buf, size)
pub extern "sysv64" fn user_name(_user: i32, buf: *mut u8, size: usize) -> i32 {
    let name = b"Player";
    if !buf.is_null() && size > name.len() {
        unsafe {
            std::ptr::copy_nonoverlapping(name.as_ptr(), buf, name.len());
            *buf.add(name.len()) = 0;
        }
    }
    0
}

/// sceSystemServiceParamGetInt(id, value*): language 1 = English (US), others 0.
pub extern "sysv64" fn param_get_int(id: i32, value: *mut i32) -> i32 {
    let v = match id {
        1 => 1,   // language: English (US)
        2 => 0,   // date format
        3 => 1,   // time format: 24 h
        4 => 180, // time zone offset (minutes): UTC+3
        5 => 0,   // summer time
        7 => 1,   // enter button: cross
        _ => 0,
    };
    if !value.is_null() {
        unsafe { *value = v };
    }
    0
}

/// sceRandomGetRandomNumber(buf, size)
pub extern "sysv64" fn random(buf: *mut u8, size: usize) -> i32 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    }
    for i in 0..size {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        unsafe { *buf.add(i) = (x >> 24) as u8 };
    }
    STATE.store(x, Ordering::Relaxed);
    0
}

pub extern "sysv64" fn ok() -> i32 {
    0
}

// ── video out ─────────────────────────────────────────────────────────

/// sceVideoOutOpen(user, bus, index, param): a handle for the main display.
pub extern "sysv64" fn video_open(_user: i32, _bus: i32, _index: i32, _param: *const c_void) -> i32 {
    eprintln!("[orbis] video out opened (display handle 1)");
    1
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("sceUserServiceGetEvent", user_get_event as *const () as usize),
        ("sceUserServiceGetLoginUserIdList", user_login_list as *const () as usize),
        ("sceUserServiceGetInitialUser", user_initial as *const () as usize),
        ("sceUserServiceGetUserName", user_name as *const () as usize),
        ("sceUserServiceInitialize", ok as *const () as usize),
        ("sceUserServiceInitialize2", ok as *const () as usize),
        ("sceSystemServiceParamGetInt", param_get_int as *const () as usize),
        ("sceSystemServiceHideSplashScreen", ok as *const () as usize),
        ("sceRandomGetRandomNumber", random as *const () as usize),
        ("sceKernelReleaseDirectMemory", ok as *const () as usize),
        ("sceKernelCheckedReleaseDirectMemory", ok as *const () as usize),
        ("sceNpCheckCallback", ok as *const () as usize),
        ("sceNetCtlCheckCallback", ok as *const () as usize),
        ("sceVideoOutOpen", video_open as *const () as usize),
    ]
}
