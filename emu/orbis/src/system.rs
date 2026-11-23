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

// ── gamepad (libScePad: XInput + keyboard/mouse) ──────────────────────

// PS4/PS5 ScePadButtonDataOffset bitmask constants.
const SCE_PAD_BUTTON_L3: u32 = 1 << 1;         // 0x0000_0002
const SCE_PAD_BUTTON_R3: u32 = 1 << 2;         // 0x0000_0004
const SCE_PAD_BUTTON_OPTIONS: u32 = 1 << 3;    // 0x0000_0008
const SCE_PAD_BUTTON_UP: u32 = 1 << 4;         // 0x0000_0010
const SCE_PAD_BUTTON_RIGHT: u32 = 1 << 5;      // 0x0000_0020
const SCE_PAD_BUTTON_DOWN: u32 = 1 << 6;       // 0x0000_0040
const SCE_PAD_BUTTON_LEFT: u32 = 1 << 7;       // 0x0000_0080
const SCE_PAD_BUTTON_L2: u32 = 1 << 8;         // 0x0000_0100
const SCE_PAD_BUTTON_R2: u32 = 1 << 9;         // 0x0000_0200
const SCE_PAD_BUTTON_L1: u32 = 1 << 10;        // 0x0000_0400
const SCE_PAD_BUTTON_R1: u32 = 1 << 11;        // 0x0000_0800
const SCE_PAD_BUTTON_TRIANGLE: u32 = 1 << 12;  // 0x0000_1000
const SCE_PAD_BUTTON_CIRCLE: u32 = 1 << 13;    // 0x0000_2000
const SCE_PAD_BUTTON_CROSS: u32 = 1 << 14;     // 0x0000_4000
const SCE_PAD_BUTTON_SQUARE: u32 = 1 << 15;    // 0x0000_8000
const SCE_PAD_BUTTON_TOUCH_PAD: u32 = 1 << 20; // 0x0010_0000

const PAD_STICK_CENTER: u8 = 0x80;
const XINPUT_STICK_DEADZONE: f32 = 0.15;

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

fn is_offscreen_or_headless() -> bool {
    std::env::var_os("ORBIS_OFFSCREEN").is_some()
        || std::env::var_os("ORBIS_HIDDEN").is_some()
        || std::env::var_os("ORBIS_HEADLESS").is_some()
}

/// Converts an XInput (x, y) thumbstick pair (i16, +Y up) into PS4/PS5 ScePadAnalogStick
/// bytes (0x00..=0xFF, 0x80 center, 0x00 up / 0xFF down) with a smooth radial deadzone.
fn xinput_stick_pair_to_ps(x: i16, y: i16, deadzone: f32) -> (u8, u8) {
    let fx = (x as f32 / 32768.0).clamp(-1.0, 1.0);
    let fy = (y as f32 / 32768.0).clamp(-1.0, 1.0);
    let mag = (fx * fx + fy * fy).sqrt();
    if mag <= deadzone {
        return (PAD_STICK_CENTER, PAD_STICK_CENTER);
    }
    let norm = ((mag - deadzone) / (1.0 - deadzone)).clamp(0.0, 1.0);
    let sx = (fx / mag) * norm;
    let sy = -(fy / mag) * norm; // PS4/PS5 Y axis: 0x00 = up, 0xFF = down
    let to_u8 = |v: f32| -> u8 {
        let s = if v < 0.0 { 128.0 + v * 128.0 } else { 128.0 + v * 127.0 };
        s.round().clamp(0.0, 255.0) as u8
    };
    (to_u8(sx), to_u8(sy))
}

#[cfg(windows)]
fn foreground_orbis_hwnd() -> Option<windows_sys::Win32::Foundation::HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() {
        return None;
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(fg, &mut pid) };
    if pid == std::process::id() {
        Some(fg)
    } else {
        None
    }
}

#[cfg(windows)]
fn poll_host_pad() -> (u32, u8, u8, u8, u8, u8, u8) {
    use std::sync::{Mutex, OnceLock};
    type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XInputState) -> u32;
    type GetAsyncKeyStateFn = unsafe extern "system" fn(i32) -> i16;

    static XINPUT: OnceLock<Option<XInputGetStateFn>> = OnceLock::new();
    static KEYSTATE: OnceLock<Option<GetAsyncKeyStateFn>> = OnceLock::new();
    static LAST_MOUSE: Mutex<Option<(i32, i32)>> = Mutex::new(None);

    // Never inject host input in offscreen/hidden/headless mode or when our window is unfocused.
    if is_offscreen_or_headless() {
        if let Ok(mut last) = LAST_MOUSE.lock() {
            *last = None;
        }
        return (0, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, 0, 0);
    }
    let Some(fg_hwnd) = foreground_orbis_hwnd() else {
        if let Ok(mut last) = LAST_MOUSE.lock() {
            *last = None;
        }
        return (0, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, 0, 0);
    };

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
    let (mut lx, mut ly, mut rx, mut ry, mut l2, mut r2) = (
        PAD_STICK_CENTER,
        PAD_STICK_CENTER,
        PAD_STICK_CENTER,
        PAD_STICK_CENTER,
        0u8,
        0u8,
    );

    if let Some(f) = xget {
        for slot in 0..4u32 {
            let mut st = XInputState::default();
            if unsafe { f(slot, &mut st) } == 0 {
                let g = &st.gamepad;
                if g.buttons & 0x0001 != 0 { btns |= SCE_PAD_BUTTON_UP; }
                if g.buttons & 0x0002 != 0 { btns |= SCE_PAD_BUTTON_DOWN; }
                if g.buttons & 0x0004 != 0 { btns |= SCE_PAD_BUTTON_LEFT; }
                if g.buttons & 0x0008 != 0 { btns |= SCE_PAD_BUTTON_RIGHT; }
                if g.buttons & 0x0010 != 0 { btns |= SCE_PAD_BUTTON_OPTIONS; }
                if g.buttons & 0x0020 != 0 { btns |= SCE_PAD_BUTTON_TOUCH_PAD; }
                if g.buttons & 0x0040 != 0 { btns |= SCE_PAD_BUTTON_L3; }
                if g.buttons & 0x0080 != 0 { btns |= SCE_PAD_BUTTON_R3; }
                if g.buttons & 0x0100 != 0 { btns |= SCE_PAD_BUTTON_L1; }
                if g.buttons & 0x0200 != 0 { btns |= SCE_PAD_BUTTON_R1; }
                if g.buttons & 0x1000 != 0 { btns |= SCE_PAD_BUTTON_CROSS; }
                if g.buttons & 0x2000 != 0 { btns |= SCE_PAD_BUTTON_CIRCLE; }
                if g.buttons & 0x4000 != 0 { btns |= SCE_PAD_BUTTON_SQUARE; }
                if g.buttons & 0x8000 != 0 { btns |= SCE_PAD_BUTTON_TRIANGLE; }
                l2 = if g.left_trigger > 10 { g.left_trigger } else { 0 };
                r2 = if g.right_trigger > 10 { g.right_trigger } else { 0 };
                if l2 > 30 { btns |= SCE_PAD_BUTTON_L2; }
                if r2 > 30 { btns |= SCE_PAD_BUTTON_R2; }
                let (nlx, nly) = xinput_stick_pair_to_ps(g.thumb_lx, g.thumb_ly, XINPUT_STICK_DEADZONE);
                let (nrx, nry) = xinput_stick_pair_to_ps(g.thumb_rx, g.thumb_ry, XINPUT_STICK_DEADZONE);
                lx = nlx;
                ly = nly;
                rx = nrx;
                ry = nry;
                break;
            }
        }
    }

    if let Some(k) = key {
        let down = |vk: i32| unsafe { (k(vk) as u16) & 0x8000 != 0 };

        // Arrow keys -> D-pad (crisp single-step menu navigation without double-stepping Left Stick)
        if down(0x26) { btns |= SCE_PAD_BUTTON_UP; }
        if down(0x28) { btns |= SCE_PAD_BUTTON_DOWN; }
        if down(0x25) { btns |= SCE_PAD_BUTTON_LEFT; }
        if down(0x27) { btns |= SCE_PAD_BUTTON_RIGHT; }

        // WASD -> Left Stick (movement; opposing keys cancel cleanly to center)
        let k_lx = (down(b'D' as i32) as i32) - (down(b'A' as i32) as i32);
        let k_ly = (down(b'S' as i32) as i32) - (down(b'W' as i32) as i32);
        if k_lx != 0 {
            lx = if k_lx < 0 { 0x00 } else { 0xFF };
        }
        if k_ly != 0 {
            ly = if k_ly < 0 { 0x00 } else { 0xFF };
        }

        // IJKL -> Right Stick (keyboard camera look)
        let k_rx = (down(b'L' as i32) as i32) - (down(b'J' as i32) as i32);
        let k_ry = (down(b'K' as i32) as i32) - (down(b'I' as i32) as i32);
        if k_rx != 0 {
            rx = if k_rx < 0 { 0x20 } else { 0xE0 };
        }
        if k_ry != 0 {
            ry = if k_ry < 0 { 0x20 } else { 0xE0 };
        }

        // Face, shoulder, and system buttons
        if down(0x0D) || down(0x20) { btns |= SCE_PAD_BUTTON_CROSS; }      // Enter / Space
        if down(0x1B) || down(0x08) { btns |= SCE_PAD_BUTTON_CIRCLE; }     // Esc / Backspace
        if down(b'E' as i32) { btns |= SCE_PAD_BUTTON_TRIANGLE; }          // E -> Inventory
        if down(b'Q' as i32) { btns |= SCE_PAD_BUTTON_SQUARE; }            // Q -> Crafting
        if down(0x09) || down(b'P' as i32) { btns |= SCE_PAD_BUTTON_OPTIONS; } // Tab / P -> Pause
        if down(b'M' as i32) { btns |= SCE_PAD_BUTTON_TOUCH_PAD; }         // M -> TouchPad
        if down(0x10) { btns |= SCE_PAD_BUTTON_L3; }                       // Shift -> Sprint
        if down(0x11) { btns |= SCE_PAD_BUTTON_R3; }                       // Ctrl -> Crouch
        if down(b'1' as i32) || down(b'Z' as i32) { btns |= SCE_PAD_BUTTON_L1; } // Prev hotbar
        if down(b'2' as i32) || down(b'X' as i32) { btns |= SCE_PAD_BUTTON_R1; } // Next hotbar
        if down(b'F' as i32) { btns |= SCE_PAD_BUTTON_L2; l2 = 0xFF; }     // Place / Use
        if down(b'R' as i32) { btns |= SCE_PAD_BUTTON_R2; r2 = 0xFF; }     // Mine / Attack

        // Mouse look & triggers only when cursor is inside the Orbis window client area
        let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        let mut rc = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let in_client = unsafe {
            use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
            use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, GetCursorPos};
            GetCursorPos(&mut pt) != 0
                && ScreenToClient(fg_hwnd, &mut pt) != 0
                && GetClientRect(fg_hwnd, &mut rc) != 0
                && pt.x >= rc.left
                && pt.x < rc.right
                && pt.y >= rc.top
                && pt.y < rc.bottom
        };
        if in_client {
            if down(0x01) { btns |= SCE_PAD_BUTTON_R2; r2 = 0xFF; } // LMB -> Mine / Attack
            if down(0x02) { btns |= SCE_PAD_BUTTON_L2; l2 = 0xFF; } // RMB -> Place / Use
            if let Ok(mut last) = LAST_MOUSE.lock() {
                if let Some((lx_m, ly_m)) = *last {
                    let dx = pt.x - lx_m;
                    let dy = pt.y - ly_m;
                    if (dx != 0 || dy != 0) && dx.abs() < 200 && dy.abs() < 200 {
                        if k_rx == 0 && rx == PAD_STICK_CENTER {
                            rx = (128 + (dx * 6).clamp(-128, 127)) as u8;
                        }
                        if k_ry == 0 && ry == PAD_STICK_CENTER {
                            ry = (128 + (dy * 6).clamp(-128, 127)) as u8;
                        }
                    }
                }
                *last = Some((pt.x, pt.y));
            }
        } else if let Ok(mut last) = LAST_MOUSE.lock() {
            *last = None;
        }
    }

    (btns, lx, ly, rx, ry, l2, r2)
}

#[cfg(not(windows))]
fn poll_host_pad() -> (u32, u8, u8, u8, u8, u8, u8) {
    (0, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, 0, 0)
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
            *(info as *mut f32) = 44.86;        // 0x00: touchPadInfo.pixelDensity
            *(info.add(4) as *mut u16) = 1920;  // 0x04: touchPadInfo.resolution.x
            *(info.add(6) as *mut u16) = 943;   // 0x06: touchPadInfo.resolution.y
            *info.add(8) = 0x0D;                // 0x08: stickInfo.deadZoneLeft
            *info.add(9) = 0x0D;                // 0x09: stickInfo.deadZoneRight
            *info.add(0x0A) = 0;                // 0x0A: connectionType = LOCAL (0)
            *info.add(0x0B) = 1;                // 0x0B: connectedCount = 1
            *info.add(0x0C) = 1;                // 0x0C: connected = true
            *(info.add(0x10) as *mut i32) = 0;  // 0x10: deviceClass = STANDARD (0)
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

pub extern "sysv64" fn net_getsockopt(_fd: i32, _level: i32, _opt: i32, val: *mut u8, len: *mut u32) -> i32 {
    if !val.is_null() && !len.is_null() {
        unsafe {
            let n = (*len as usize).min(64);
            std::ptr::write_bytes(val, 0, n);
        }
    }
    0
}

pub extern "sysv64" fn savedata_not_found() -> i32 {
    0x809F_0008u32 as i32 // SCE_SAVE_DATA_ERROR_NOT_FOUND
}

// ── libSceAppContent (temp/download storage space & mount points) ─────

/// sceAppContentTemporaryDataMount2(option, mountPoint*): mounts /temp0 and creates the folder.
pub extern "sysv64" fn app_content_temp_mount(_option: u32, mount_point: *mut u8) -> i32 {
    let dir = format!("{}/temp0", crate::kernel::data_root());
    let _ = std::fs::create_dir_all(&dir);
    if !mount_point.is_null() {
        let mp = b"/temp0";
        unsafe {
            std::ptr::write_bytes(mount_point, 0, 16);
            std::ptr::copy_nonoverlapping(mp.as_ptr(), mount_point, mp.len());
        }
    }
    0
}

/// sceAppContentTemporaryDataGetAvailableSpaceKb / DownloadDataGetAvailableSpaceKb(mp*, outKb*)
/// Reports 8 GiB (8_388_608 KiB) of free temporary/download storage.
pub extern "sysv64" fn app_content_available_space_kb(_mp: *const u8, out_kb: *mut usize) -> i32 {
    if !out_kb.is_null() {
        unsafe { *out_kb = 8 * 1024 * 1024 }; // 8 GiB in KiB
    }
    0
}

/// sceAppContentGetAddcontInfoList(serviceLabel, list*, listNum, hitNum*)
pub extern "sysv64" fn app_content_addcont_list(_label: u32, _list: *mut u8, _num: u32, hit_num: *mut u32) -> i32 {
    if !hit_num.is_null() {
        unsafe { *hit_num = 0 };
    }
    0
}

/// sceAppContentAppParamGetInt(paramId, value*)
pub extern "sysv64" fn app_content_param_get_int(_id: u32, value: *mut i32) -> i32 {
    if !value.is_null() {
        unsafe { *value = 0 };
    }
    0
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

pub extern "sysv64" fn hide_splash_screen() -> i32 {
    crate::video::hide_os_splash();
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("sceSystemServiceReceiveEvent", system_receive_event as *const () as usize),
        ("sceSystemServiceGetStatus", system_get_status as *const () as usize),
        ("sceSystemServiceReportAbnormalTermination", ok as *const () as usize),
        ("scePadReadState", pad_read_state as *const () as usize),
        ("scePadRead", pad_read as *const () as usize),
        ("scePadGetControllerInformation", pad_get_info as *const () as usize),
        ("scePadClose", ok as *const () as usize),
        ("sceSaveDataTerminate", ok as *const () as usize),
        ("sceSaveDataLoadIcon", savedata_not_found as *const () as usize),
        ("sceSaveDataTransferringMount", savedata_not_found as *const () as usize),
        ("sceAppContentInitialize", ok as *const () as usize),
        ("sceAppContentTemporaryDataMount2", app_content_temp_mount as *const () as usize),
        ("sceAppContentTemporaryDataUnmount", ok as *const () as usize),
        ("sceAppContentTemporaryDataFormat", ok as *const () as usize),
        ("sceAppContentTemporaryDataGetAvailableSpaceKb", app_content_available_space_kb as *const () as usize),
        ("sceAppContentDownloadDataFormat", ok as *const () as usize),
        ("sceAppContentDownloadDataGetAvailableSpaceKb", app_content_available_space_kb as *const () as usize),
        ("sceAppContentGetAddcontInfoList", app_content_addcont_list as *const () as usize),
        ("sceAppContentAddcontMount", ok as *const () as usize),
        ("sceAppContentAddcontUnmount", ok as *const () as usize),
        ("sceAppContentAppParamGetInt", app_content_param_get_int as *const () as usize),
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
        ("listen", ok as *const () as usize),
        ("accept", net_fail as *const () as usize),
        ("shutdown", ok as *const () as usize),
        ("select", ok as *const () as usize),
        ("getsockname", net_getsockname as *const () as usize),
        ("getpeername", net_getsockname as *const () as usize),
        ("setsockopt", ok as *const () as usize),
        ("getsockopt", net_getsockopt as *const () as usize),
        ("sendto", net_send as *const () as usize),
        ("send", net_send as *const () as usize),
        ("recvfrom", net_fail as *const () as usize),
        ("recv", net_fail as *const () as usize),
        ("connect", net_fail as *const () as usize),
        ("inet_ntop", net_inet_ntop as *const () as usize),
        ("inet_pton", net_inet_pton as *const () as usize),
        ("sceNetInetPton", net_inet_pton as *const () as usize),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_button_bits_match_ps_sdk() {
        assert_eq!(SCE_PAD_BUTTON_L3, 0x0000_0002);
        assert_eq!(SCE_PAD_BUTTON_R3, 0x0000_0004);
        assert_eq!(SCE_PAD_BUTTON_OPTIONS, 0x0000_0008);
        assert_eq!(SCE_PAD_BUTTON_UP, 0x0000_0010);
        assert_eq!(SCE_PAD_BUTTON_RIGHT, 0x0000_0020);
        assert_eq!(SCE_PAD_BUTTON_DOWN, 0x0000_0040);
        assert_eq!(SCE_PAD_BUTTON_LEFT, 0x0000_0080);
        assert_eq!(SCE_PAD_BUTTON_L2, 0x0000_0100);
        assert_eq!(SCE_PAD_BUTTON_R2, 0x0000_0200);
        assert_eq!(SCE_PAD_BUTTON_L1, 0x0000_0400);
        assert_eq!(SCE_PAD_BUTTON_R1, 0x0000_0800);
        assert_eq!(SCE_PAD_BUTTON_TRIANGLE, 0x0000_1000);
        assert_eq!(SCE_PAD_BUTTON_CIRCLE, 0x0000_2000);
        assert_eq!(SCE_PAD_BUTTON_CROSS, 0x0000_4000);
        assert_eq!(SCE_PAD_BUTTON_SQUARE, 0x0000_8000);
        assert_eq!(SCE_PAD_BUTTON_TOUCH_PAD, 0x0010_0000);
    }

    #[test]
    fn xinput_stick_center_and_deadzone_are_exact() {
        // Exact center must be 0x80 on both axes (no off-by-one 0x7F on Y).
        assert_eq!(xinput_stick_pair_to_ps(0, 0, XINPUT_STICK_DEADZONE), (0x80, 0x80));
        // Small resting drift inside deadzone snaps cleanly to 0x80.
        assert_eq!(xinput_stick_pair_to_ps(2000, -2500, XINPUT_STICK_DEADZONE), (0x80, 0x80));
        // Cardinal extremes: +X = 0xFF (right), -X = 0x00 (left), +Y = 0x00 (up), -Y = 0xFF (down).
        assert_eq!(xinput_stick_pair_to_ps(i16::MAX, 0, XINPUT_STICK_DEADZONE), (0xFF, 0x80));
        assert_eq!(xinput_stick_pair_to_ps(i16::MIN, 0, XINPUT_STICK_DEADZONE), (0x00, 0x80));
        assert_eq!(xinput_stick_pair_to_ps(0, i16::MAX, XINPUT_STICK_DEADZONE), (0x80, 0x00));
        assert_eq!(xinput_stick_pair_to_ps(0, i16::MIN, XINPUT_STICK_DEADZONE), (0x80, 0xFF));
    }

    #[test]
    fn pad_read_state_is_neutral_without_focus_and_matches_abi() {
        let mut buf = [0xAAu8; 0x78];
        let rc = pad_read_state(1, buf.as_mut_ptr());
        assert_eq!(rc, 0);
        // No window is created/focused in unit tests, so buttons must be 0 and sticks 0x80.
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), 0);
        assert_eq!(&buf[4..8], &[0x80, 0x80, 0x80, 0x80]);
        assert_eq!(&buf[8..10], &[0, 0]);
        assert_eq!(f32::from_le_bytes(buf[0x18..0x1C].try_into().unwrap()), 1.0);
        assert_eq!(buf[0x4C], 1); // connected
        assert_eq!(buf[0x68], 1); // connectedCount
    }

    #[test]
    fn pad_get_info_matches_ps_abi_offsets() {
        let mut info = [0xFFu8; 0x20];
        assert_eq!(pad_get_info(1, info.as_mut_ptr()), 0);
        assert_eq!(u16::from_le_bytes(info[4..6].try_into().unwrap()), 1920);
        assert_eq!(u16::from_le_bytes(info[6..8].try_into().unwrap()), 943);
        assert_eq!(info[8], 0x0D);    // deadZoneLeft
        assert_eq!(info[9], 0x0D);    // deadZoneRight
        assert_eq!(info[0x0A], 0);    // connectionType = LOCAL
        assert_eq!(info[0x0B], 1);    // connectedCount
        assert_eq!(info[0x0C], 1);    // connected = true
        assert_eq!(i32::from_le_bytes(info[0x10..0x14].try_into().unwrap()), 0); // deviceClass = STANDARD
    }

    #[test]
    fn app_content_reports_temp_mount_and_free_space() {
        let mut mp = [0xAAu8; 16];
        assert_eq!(app_content_temp_mount(0, mp.as_mut_ptr()), 0);
        assert_eq!(&mp[..7], b"/temp0\0");
        let mut kb = 0usize;
        assert_eq!(app_content_available_space_kb(mp.as_ptr(), &mut kb), 0);
        assert_eq!(kb, 8 * 1024 * 1024);
    }
}
