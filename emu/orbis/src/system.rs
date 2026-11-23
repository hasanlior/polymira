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

/// sceSystemServiceReceiveEvent(event*): no system events (resume, URL open, ...) are ever pending.
pub extern "sysv64" fn system_receive_event(_ev: *mut c_void) -> i32 {
    0x80A1_0003u32 as i32 // SCE_SYSTEM_SERVICE_ERROR_NO_EVENT
}

/// sceSystemServiceGetStatus(status*): { int eventNum; bool overlaid, background, cpuMode7, ... }.
pub extern "sysv64" fn system_get_status(st: *mut u8) -> i32 {
    if !st.is_null() {
        unsafe { std::ptr::write_bytes(st, 0, 16) };
    }
    0
}

// ── gamepad (libScePad: XInput + keyboard) ────────────────────────────

#[repr(C)]
#[derive(Default)]
struct XInputGamepad {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    thumb_lx: i16,
    thumb_ly: i16,
    thumb_rx: i16,
    thumb_ry: i16,
}

#[repr(C)]
#[derive(Default)]
struct XInputState {
    packet_number: u32,
    gamepad: XInputGamepad,
}

#[cfg(windows)]
fn poll_host_pad() -> (u32, u8, u8, u8, u8, u8, u8) {
    use std::sync::OnceLock;
    type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XInputState) -> u32;
    type GetAsyncKeyStateFn = unsafe extern "system" fn(i32) -> i16;

    static XINPUT: OnceLock<Option<XInputGetStateFn>> = OnceLock::new();
    static KEYSTATE: OnceLock<Option<GetAsyncKeyStateFn>> = OnceLock::new();

    let xget = *XINPUT.get_or_init(|| unsafe {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
        for dll in [b"xinput1_4.dll\0".as_ptr(), b"xinput9_1_0.dll\0".as_ptr()] {
            let h = LoadLibraryA(dll);
            if !h.is_null() {
                if let Some(f) = GetProcAddress(h, b"XInputGetState\0".as_ptr()) {
                    return Some(std::mem::transmute(f));
                }
            }
        }
        None
    });
    let key = *KEYSTATE.get_or_init(|| unsafe {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
        let h = LoadLibraryA(b"user32.dll\0".as_ptr());
        if !h.is_null() {
            GetProcAddress(h, b"GetAsyncKeyState\0".as_ptr()).map(|f| std::mem::transmute(f))
        } else {
            None
        }
    });

    let mut btns = 0u32;
    let (mut lx, mut ly, mut rx, mut ry, mut l2, mut r2) = (0x80u8, 0x80u8, 0x80u8, 0x80u8, 0u8, 0u8);

    if let Some(f) = xget {
        let mut st = XInputState::default();
        if unsafe { f(0, &mut st) } == 0 {
            let g = &st.gamepad;
            if g.buttons & 0x0001 != 0 { btns |= 1 << 4; }  // UP
            if g.buttons & 0x0002 != 0 { btns |= 1 << 6; }  // DOWN
            if g.buttons & 0x0004 != 0 { btns |= 1 << 7; }  // LEFT
            if g.buttons & 0x0008 != 0 { btns |= 1 << 5; }  // RIGHT
            if g.buttons & 0x0010 != 0 { btns |= 1 << 3; }  // OPTIONS (Start)
            if g.buttons & 0x0040 != 0 { btns |= 1 << 1; }  // L3
            if g.buttons & 0x0080 != 0 { btns |= 1 << 2; }  // R3
            if g.buttons & 0x0100 != 0 { btns |= 1 << 10; } // L1
            if g.buttons & 0x0200 != 0 { btns |= 1 << 11; } // R1
            if g.buttons & 0x1000 != 0 { btns |= 1 << 14; } // CROSS (A)
            if g.buttons & 0x2000 != 0 { btns |= 1 << 13; } // CIRCLE (B)
            if g.buttons & 0x4000 != 0 { btns |= 1 << 15; } // SQUARE (X)
            if g.buttons & 0x8000 != 0 { btns |= 1 << 12; } // TRIANGLE (Y)
            l2 = g.left_trigger;
            r2 = g.right_trigger;
            if l2 > 30 { btns |= 1 << 8; }
            if r2 > 30 { btns |= 1 << 9; }
            lx = (((g.thumb_lx as i32) + 32768) >> 8) as u8;
            ly = (((-(g.thumb_ly as i32) - 1) + 32768) >> 8) as u8;
            rx = (((g.thumb_rx as i32) + 32768) >> 8) as u8;
            ry = (((-(g.thumb_ry as i32) - 1) + 32768) >> 8) as u8;
        }
    }

    if let Some(k) = key {
        let down = |vk: i32| unsafe { (k(vk) as u16) & 0x8000 != 0 };
        if down(0x26) || down(b'W' as i32) { btns |= 1 << 4; ly = 0x10; }  // UP
        if down(0x28) || down(b'S' as i32) { btns |= 1 << 6; ly = 0xF0; }  // DOWN
        if down(0x25) || down(b'A' as i32) { btns |= 1 << 7; lx = 0x10; }  // LEFT
        if down(0x27) || down(b'D' as i32) { btns |= 1 << 5; lx = 0xF0; }  // RIGHT
        if down(0x0D) || down(0x20) { btns |= 1 << 14; } // Enter/Space -> CROSS
        if down(0x1B) || down(0x08) { btns |= 1 << 13; } // Esc/Backspace -> CIRCLE
        if down(b'E' as i32) { btns |= 1 << 12; }        // E -> TRIANGLE
        if down(b'Q' as i32) { btns |= 1 << 15; }        // Q -> SQUARE
    }

    (btns, lx, ly, rx, ry, l2, r2)
}

#[cfg(not(windows))]
fn poll_host_pad() -> (u32, u8, u8, u8, u8, u8, u8) {
    (0, 0x80, 0x80, 0x80, 0x80, 0, 0)
}

/// scePadReadState(handle, data*): fills ScePadData (0x78 = 120 bytes) with live XInput/keyboard state.
pub extern "sysv64" fn pad_read_state(_handle: i32, data: *mut u8) -> i32 {
    if data.is_null() {
        return 0x8092_0001u32 as i32;
    }
    let (btns, lx, ly, rx, ry, l2, r2) = poll_host_pad();
    let ts = crate::kernel::monotonic_now().as_micros() as u64;
    unsafe {
        std::ptr::write_bytes(data, 0, 0x78);
        *(data as *mut u32) = btns;
        *data.add(4) = lx;
        *data.add(5) = ly;
        *data.add(6) = rx;
        *data.add(7) = ry;
        *data.add(8) = l2;
        *data.add(9) = r2;
        *(data.add(0x18) as *mut f32) = 1.0; // orientation.w
        *data.add(0x4C) = 1; // connected = true (offset 0x4C)
        *(data.add(0x50) as *mut u64) = ts; // timestamp (offset 0x50)
        *data.add(0x68) = 1; // connectedCount = 1 (offset 104 = 0x68)
    }
    0
}

pub extern "sysv64" fn pad_read(handle: i32, data: *mut u8, count: i32) -> i32 {
    if count <= 0 || data.is_null() {
        return 0;
    }
    pad_read_state(handle, data);
    1
}

pub extern "sysv64" fn pad_get_info(_handle: i32, info: *mut u8) -> i32 {
    if !info.is_null() {
        unsafe {
            std::ptr::write_bytes(info, 0, 0x20);
            *(info as *mut f32) = 44.86;        // touchPadInfo.pixelDensity
            *(info.add(4) as *mut u16) = 1920;  // touchPadInfo.resolution.x
            *(info.add(6) as *mut u16) = 943;   // touchPadInfo.resolution.y
            *info.add(8) = 0x04;                // stickInfo.deadZoneLeft
            *info.add(9) = 0x04;                // stickInfo.deadZoneRight
            *info.add(0x0C) = 0;                // connectionType = LOCAL (0)
            *info.add(0x0D) = 1;                // connectedCount = 1
            *info.add(0x0E) = 1;                // connected = true
            *info.add(0x0F) = 0;                // deviceClass = STANDARD (0)
        }
    }
    0
}

pub extern "sysv64" fn np_get_state(_user: i32, state: *mut i32) -> i32 {
    if !state.is_null() {
        unsafe { *state = 1 }; // SCE_NP_STATE_SIGNED_OUT
    }
    0
}

pub extern "sysv64" fn netctl_get_state(state: *mut i32) -> i32 {
    if !state.is_null() {
        unsafe { *state = 0 }; // SCE_NET_CTL_STATE_DISCONNECTED
    }
    0
}

pub extern "sysv64" fn http2_offline() -> i32 {
    0x8043_1001u32 as i32 // offline / network unavailable
}

static NEXT_SOCK: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1000);

pub extern "sysv64" fn net_socket(_domain: i32, _ty: i32, _proto: i32) -> i32 {
    NEXT_SOCK.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

pub extern "sysv64" fn net_getsockname(_fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    if !addr.is_null() {
        unsafe {
            let cap = if !addrlen.is_null() { (*addrlen as usize).min(16) } else { 16 };
            if cap >= 16 {
                std::ptr::write_bytes(addr, 0, 16);
                *addr.add(0) = 16; // sin_len (FreeBSD/PS5)
                *addr.add(1) = 2;  // AF_INET
                *addr.add(2) = 0x4A; // htons(19132) = 0x4ABC
                *addr.add(3) = 0xBC;
                *addr.add(4) = 127;
                *addr.add(5) = 0;
                *addr.add(6) = 0;
                *addr.add(7) = 1;
                if !addrlen.is_null() {
                    *addrlen = 16;
                }
            }
        }
    }
    0
}

pub extern "sysv64" fn net_send(_fd: i32, _buf: *const u8, len: usize) -> isize {
    len as isize
}

pub extern "sysv64" fn net_fail() -> isize {
    crate::hle_rt::set_errno(35); // EAGAIN
    -1
}

pub extern "sysv64" fn net_inet_ntop(af: i32, src: *const u8, dst: *mut u8, size: u32) -> *mut u8 {
    if src.is_null() || dst.is_null() || size < 8 {
        return std::ptr::null_mut();
    }
    let s = if af == 2 {
        unsafe { format!("{}.{}.{}.{}", *src, *src.add(1), *src.add(2), *src.add(3)) }
    } else {
        "::1".to_string()
    };
    let b = s.as_bytes();
    let n = b.len().min((size as usize).saturating_sub(1));
    unsafe {
        std::ptr::copy_nonoverlapping(b.as_ptr(), dst, n);
        *dst.add(n) = 0;
    }
    dst
}

pub extern "sysv64" fn net_inet_pton(af: i32, src: *const i8, dst: *mut u8) -> i32 {
    if src.is_null() || dst.is_null() {
        return 0;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(src) }.to_string_lossy();
    if af == 2 {
        let parts: Vec<u8> = s.split('.').filter_map(|p| p.parse::<u8>().ok()).collect();
        if parts.len() == 4 {
            unsafe { std::ptr::copy_nonoverlapping(parts.as_ptr(), dst, 4) };
            return 1;
        }
    }
    0
}

pub extern "sysv64" fn audio2_pace() -> i32 {
    std::thread::sleep(std::time::Duration::from_millis(8));
    0
}

pub extern "sysv64" fn hide_splash_screen() -> i32 {
    crate::video::hide_os_splash();
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("sceSystemServiceReceiveEvent", system_receive_event as *const () as usize),
        ("sceSystemServiceGetStatus", system_get_status as *const () as usize),
        ("scePadReadState", pad_read_state as *const () as usize),
        ("scePadRead", pad_read as *const () as usize),
        ("scePadGetControllerInformation", pad_get_info as *const () as usize),
        ("#ie7qhZ4X0Cc", ok as *const () as usize), // sceSaveDataCommit
        ("#lJUQuaKqoKY", ok as *const () as usize), // sceSaveDataDeleteTransactionResource
        ("#uW4vfTwMQVo", ok as *const () as usize), // sceSaveDataUmount2
        ("#85zul--eGXs", ok as *const () as usize), // sceSaveDataSetParam
        ("#65VH0Qaaz6s", ok as *const () as usize), // sceSaveDataGetMountInfo
        ("sceUserServiceGetEvent", user_get_event as *const () as usize),
        ("sceUserServiceGetLoginUserIdList", user_login_list as *const () as usize),
        ("sceUserServiceGetInitialUser", user_initial as *const () as usize),
        ("sceUserServiceGetUserName", user_name as *const () as usize),
        ("sceUserServiceInitialize", ok as *const () as usize),
        ("sceUserServiceInitialize2", ok as *const () as usize),
        ("sceSystemServiceParamGetInt", param_get_int as *const () as usize),
        ("sceSystemServiceHideSplashScreen", hide_splash_screen as *const () as usize),
        ("sceRandomGetRandomNumber", random as *const () as usize),
        ("sceKernelReleaseDirectMemory", ok as *const () as usize),
        ("sceKernelCheckedReleaseDirectMemory", ok as *const () as usize),
        ("sceNpCheckCallback", ok as *const () as usize),
        ("sceNetCtlCheckCallback", ok as *const () as usize),
        ("sceNpGetState", np_get_state as *const () as usize),
        ("sceNetCtlGetState", netctl_get_state as *const () as usize),
        ("sceHttp2CreateRequestWithURL", http2_offline as *const () as usize),
        ("sceHttp2SendRequestAsync", http2_offline as *const () as usize),
        ("socket", net_socket as *const () as usize),
        ("bind", ok as *const () as usize),
        ("getsockname", net_getsockname as *const () as usize),
        ("setsockopt", ok as *const () as usize),
        ("sendto", net_send as *const () as usize),
        ("send", net_send as *const () as usize),
        ("recvfrom", net_fail as *const () as usize),
        ("recv", net_fail as *const () as usize),
        ("connect", net_fail as *const () as usize),
        ("inet_ntop", net_inet_ntop as *const () as usize),
        ("inet_pton", net_inet_pton as *const () as usize),
        ("sceNetInetPton", net_inet_pton as *const () as usize),
        ("#MmpF1XsQiHw", audio2_pace as *const () as usize), // libSceAudioOut2 frame pacing
        ("#8XTArSPyWHk", audio2_pace as *const () as usize), // libSceAudioOut2 poll pacing
    ]
}
