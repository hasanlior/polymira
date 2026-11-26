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

const AUTO_KEY_HOLD_SUBMITS: u64 = 12;

fn parse_auto_keys(spec: &str) -> Vec<(u64, u32)> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let Some((at_s, btn_s)) = part.trim().split_once(':') else { continue };
        let Ok(at) = at_s.trim().parse::<u64>() else { continue };
        let bit = match btn_s.trim().to_ascii_lowercase().as_str() {
            "up" => SCE_PAD_BUTTON_UP,
            "down" => SCE_PAD_BUTTON_DOWN,
            "left" => SCE_PAD_BUTTON_LEFT,
            "right" => SCE_PAD_BUTTON_RIGHT,
            "cross" | "x" | "a" => SCE_PAD_BUTTON_CROSS,
            "circle" | "o" | "b" => SCE_PAD_BUTTON_CIRCLE,
            "square" => SCE_PAD_BUTTON_SQUARE,
            "triangle" => SCE_PAD_BUTTON_TRIANGLE,
            "l1" => SCE_PAD_BUTTON_L1,
            "r1" => SCE_PAD_BUTTON_R1,
            "l2" => SCE_PAD_BUTTON_L2,
            "r2" => SCE_PAD_BUTTON_R2,
            "l3" => SCE_PAD_BUTTON_L3,
            "r3" => SCE_PAD_BUTTON_R3,
            "options" | "start" => SCE_PAD_BUTTON_OPTIONS,
            "touchpad" | "touch" => SCE_PAD_BUTTON_TOUCH_PAD,
            _ => continue,
        };
        out.push((at, bit));
    }
    out
}

/// Evaluates scheduled `ORBIS_AUTO_KEYS="800:cross,1100:down,..."` against `submit_count`.
/// Returns `(buttons, l2, r2)` if any scheduled press is active at `submit_count`.
fn eval_auto_keys(schedule: &[(u64, u32)], submit_count: u64) -> (u32, u8, u8) {
    let mut btns = 0u32;
    for &(start, bit) in schedule {
        if submit_count >= start && submit_count < start.saturating_add(AUTO_KEY_HOLD_SUBMITS) {
            btns |= bit;
        }
    }
    let l2 = if btns & SCE_PAD_BUTTON_L2 != 0 { 0xFF } else { 0 };
    let r2 = if btns & SCE_PAD_BUTTON_R2 != 0 { 0xFF } else { 0 };
    (btns, l2, r2)
}

fn auto_keys_schedule() -> &'static [(u64, u32)] {
    use std::sync::OnceLock;
    static SCHEDULE: OnceLock<Vec<(u64, u32)>> = OnceLock::new();
    SCHEDULE.get_or_init(|| {
        std::env::var("ORBIS_AUTO_KEYS")
            .ok()
            .map(|s| parse_auto_keys(&s))
            .unwrap_or_default()
    })
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

    // Test-only ORBIS_AUTO_KEYS overrides offscreen/focus gating only for scheduled submit windows.
    let auto_sched = auto_keys_schedule();
    let (auto_btns, auto_l2, auto_r2) = if !auto_sched.is_empty() {
        eval_auto_keys(auto_sched, crate::agc::stats().0)
    } else {
        (0, 0, 0)
    };

    // Never inject host input in offscreen/hidden/headless mode or when our window is unfocused.
    if is_offscreen_or_headless() {
        if let Ok(mut last) = LAST_MOUSE.lock() {
            *last = None;
        }
        return (
            auto_btns,
            PAD_STICK_CENTER,
            PAD_STICK_CENTER,
            PAD_STICK_CENTER,
            PAD_STICK_CENTER,
            auto_l2,
            auto_r2,
        );
    }
    let Some(fg_hwnd) = foreground_orbis_hwnd() else {
        if let Ok(mut last) = LAST_MOUSE.lock() {
            *last = None;
        }
        return (
            auto_btns,
            PAD_STICK_CENTER,
            PAD_STICK_CENTER,
            PAD_STICK_CENTER,
            PAD_STICK_CENTER,
            auto_l2,
            auto_r2,
        );
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

        // Face, shoulder, and system buttons (natural 3D Minecraft PS5 keyboard layout)
        if down(0x0D) || down(0x20) { btns |= SCE_PAD_BUTTON_CROSS; }      // Enter / Space -> Jump / Select
        if down(0x08) || down(b'Q' as i32) { btns |= SCE_PAD_BUTTON_CIRCLE; } // Backspace / Q -> Back / Drop
        if down(b'E' as i32) { btns |= SCE_PAD_BUTTON_TRIANGLE; }          // E -> Inventory
        if down(b'F' as i32) { btns |= SCE_PAD_BUTTON_SQUARE; }            // F -> Crafting
        if down(0x1B) || down(b'P' as i32) { btns |= SCE_PAD_BUTTON_OPTIONS; } // Esc / P -> Pause menu
        if down(0x09) || down(b'M' as i32) { btns |= SCE_PAD_BUTTON_TOUCH_PAD; } // Tab / M -> TouchPad
        if down(0x11) { btns |= SCE_PAD_BUTTON_L3; }                       // Ctrl -> Sprint
        if down(0x10) { btns |= SCE_PAD_BUTTON_R3; }                       // Shift -> Sneak / Crouch
        if down(b'1' as i32) || down(b'Z' as i32) { btns |= SCE_PAD_BUTTON_L1; } // 1 / Z -> Hotbar left
        if down(b'2' as i32) || down(b'X' as i32) || down(b'C' as i32) { btns |= SCE_PAD_BUTTON_R1; } // 2 / X / C -> Hotbar right
        if down(b'R' as i32) { btns |= SCE_PAD_BUTTON_R2; r2 = 0xFF; }     // R -> Mine / Attack (keyboard fallback)
        if down(b'V' as i32) { btns |= SCE_PAD_BUTTON_L2; l2 = 0xFF; }     // V -> Place / Use (keyboard fallback)

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

    (btns | auto_btns, lx, ly, rx, ry, l2.max(auto_l2), r2.max(auto_r2))
}

#[cfg(not(windows))]
fn poll_host_pad() -> (u32, u8, u8, u8, u8, u8, u8) {
    let auto_sched = auto_keys_schedule();
    let (btns, l2, r2) = if !auto_sched.is_empty() {
        eval_auto_keys(auto_sched, crate::agc::stats().0)
    } else {
        (0, 0, 0)
    };
    (btns, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, PAD_STICK_CENTER, l2, r2)
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

const SCE_NP_ERROR_SIGNED_OUT: i32 = 0x8055_0006u32 as i32;

pub extern "sysv64" fn np_get_state(_user: i32, state: *mut i32) -> i32 {
    if !state.is_null() {
        unsafe { *state = 1 }; // SCE_NP_STATE_SIGNED_OUT
    }
    0
}

/// sceNpGetAccountIdA(userId, accountId*): offline, write 0 and report signed out.
pub extern "sysv64" fn np_get_account_id(_user: i32, account_id: *mut u64) -> i32 {
    if !account_id.is_null() {
        unsafe { *account_id = 0 };
    }
    SCE_NP_ERROR_SIGNED_OUT
}

/// sceNpGetAccountCountryA(userId, countryCode*): benign "us" SceNpCountryCode (4 bytes).
pub extern "sysv64" fn np_get_account_country(_user: i32, country: *mut u8) -> i32 {
    if !country.is_null() {
        unsafe {
            *country = b'u';
            *country.add(1) = b's';
            *country.add(2) = 0;
            *country.add(3) = 0;
        }
    }
    0
}

/// sceNpAuthCreateRequest(): offline, return signed-out error so caller does not use a bogus handle.
pub extern "sysv64" fn np_auth_create_request() -> i32 {
    SCE_NP_ERROR_SIGNED_OUT
}

// ── libSceNpTrophy2 / libSceNpUniversalDataSystem (local offline HLE) ─

static NEXT_TROPHY_HANDLE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
static UNLOCKED_TROPHIES: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

pub extern "sysv64" fn np_trophy_create_ctx(out_ctx: *mut i32, _user: i32, _label: u32, _opts: u64) -> i32 {
    if !out_ctx.is_null() {
        unsafe { *out_ctx = NEXT_TROPHY_HANDLE.fetch_add(1, Ordering::SeqCst) };
    }
    0
}

pub extern "sysv64" fn np_trophy_create_handle(out_handle: *mut i32) -> i32 {
    if !out_handle.is_null() {
        unsafe { *out_handle = NEXT_TROPHY_HANDLE.fetch_add(1, Ordering::SeqCst) };
    }
    0
}

pub extern "sysv64" fn np_trophy_unlock(_ctx: i32, _handle: i32, trophy_id: i32, out_plat: *mut i32) -> i32 {
    let mut list = UNLOCKED_TROPHIES.lock().unwrap_or_else(|e| e.into_inner());
    if !list.contains(&trophy_id) {
        list.push(trophy_id);
        eprintln!("[orbis] trophy unlocked: #{trophy_id}");
    }
    if !out_plat.is_null() {
        unsafe { *out_plat = -1 };
    }
    0
}

pub extern "sysv64" fn np_trophy_get_unlock_state(_ctx: i32, _handle: i32, flags: *mut u32, count: *mut u32) -> i32 {
    let list = UNLOCKED_TROPHIES.lock().unwrap_or_else(|e| e.into_inner());
    if !flags.is_null() {
        unsafe {
            std::ptr::write_bytes(flags as *mut u8, 0, 16);
            for &id in list.iter() {
                if (0..128).contains(&id) {
                    let word = (id / 32) as usize;
                    let bit = (id % 32) as u32;
                    *flags.add(word) |= 1u32 << bit;
                }
            }
        }
    }
    if !count.is_null() {
        unsafe { *count = list.len() as u32 };
    }
    0
}

pub extern "sysv64" fn np_uds_create_out(out: *mut i32) -> i32 {
    if !out.is_null() {
        unsafe { *out = NEXT_TROPHY_HANDLE.fetch_add(1, Ordering::SeqCst) };
    }
    0
}

// ── System Dialogs (MsgDialog / SaveDataDialog / ImeDialog) ──────────

/// SceCommonDialogStatus: FINISHED = 3 (auto-confirm prevents early-boot dialog soft-locks).
pub extern "sysv64" fn dialog_status_finished() -> i32 {
    3
}

/// MsgDialog / SaveDataDialog GetResult(res*): { i32 mode = 0, i32 result = 0 (OK), i32 buttonId = 1 (OK) }
pub extern "sysv64" fn dialog_get_result(res: *mut i32) -> i32 {
    if !res.is_null() {
        unsafe {
            std::ptr::write_bytes(res as *mut u8, 0, 32);
            *res = 0;         // mode
            *res.add(1) = 0;  // SCE_COMMON_DIALOG_RESULT_OK
            *res.add(2) = 1;  // buttonId = OK
        }
    }
    0
}

/// SceImeDialogStatus: FINISHED = 2.
pub extern "sysv64" fn ime_dialog_status_finished() -> i32 {
    2
}

/// SceImeDialogResult: { i32 endstatus = 0 (OK) }.
pub extern "sysv64" fn ime_dialog_get_result(res: *mut i32) -> i32 {
    if !res.is_null() {
        unsafe { std::ptr::write_bytes(res as *mut u8, 0, 16) };
    }
    0
}

// ── libSceNetCtl & libSceAudio3d stubs ───────────────────────────────

pub extern "sysv64" fn netctl_register_callback(_cb: usize, _arg: usize, cid: *mut i32) -> i32 {
    if !cid.is_null() {
        unsafe { *cid = 1 };
    }
    0
}

pub extern "sysv64" fn audio3d_port_open(_user: i32, _param: *const u8, out_id: *mut u32) -> i32 {
    if !out_id.is_null() {
        unsafe { *out_id = 1 };
    }
    0
}

// ── libSceRtc (real-time clock for UE4 / Stray / PS5 titles) ─────────

/// SceRtcDateTime (16 bytes): { u16 year, month, day, hour, minute, second; u32 microsecond }
pub extern "sysv64" fn rtc_get_current_clock(dt: *mut u8, _tz_minutes: i32) -> i32 {
    if !dt.is_null() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let days = (secs / 86400) as i64;
        let rem = secs % 86400;
        let hour = (rem / 3600) as u16;
        let min = ((rem % 3600) / 60) as u16;
        let sec = (rem % 60) as u16;
        let usec = now.subsec_micros();
        // Howard Hinnant civil-from-days algorithm (exact Gregorian calendar)
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = (yoe as i64) + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u16;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u16;
        let year = (y + if month <= 2 { 1 } else { 0 }) as u16;
        unsafe {
            *(dt as *mut u16) = year;
            *(dt.add(2) as *mut u16) = month;
            *(dt.add(4) as *mut u16) = day;
            *(dt.add(6) as *mut u16) = hour;
            *(dt.add(8) as *mut u16) = min;
            *(dt.add(10) as *mut u16) = sec;
            *(dt.add(12) as *mut u32) = usec;
        }
    }
    0
}

pub extern "sysv64" fn rtc_get_current_clock_local(dt: *mut u8) -> i32 {
    rtc_get_current_clock(dt, 0)
}

pub extern "sysv64" fn rtc_get_current_tick(tick: *mut u64) -> i32 {
    if !tick.is_null() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // PS4/PS5 RTC ticks are microseconds since 0001-01-01 (epoch offset = 62135596800s).
        let us = (now.as_secs() + 62_135_596_800) * 1_000_000 + (now.subsec_micros() as u64);
        unsafe { *tick = us };
    }
    0
}

pub extern "sysv64" fn rtc_get_tick_resolution() -> u32 {
    1_000_000
}

/// sceRtcGetTick(const SceRtcDateTime* dt, SceRtcTick* tick)
/// Converts Gregorian date/time to microseconds since 0001-01-01 00:00:00.
pub extern "sysv64" fn rtc_get_tick(dt: *const u8, tick: *mut u64) -> i32 {
    if dt.is_null() || tick.is_null() {
        return 0x80B5_0001u32 as i32; // SCE_RTC_ERROR_INVALID_POINTER
    }
    unsafe {
        let year = u16::from_le_bytes([*dt, *dt.add(1)]) as i64;
        let month = u16::from_le_bytes([*dt.add(2), *dt.add(3)]) as i64;
        let day = u16::from_le_bytes([*dt.add(4), *dt.add(5)]) as i64;
        let hour = u16::from_le_bytes([*dt.add(6), *dt.add(7)]) as i64;
        let min = u16::from_le_bytes([*dt.add(8), *dt.add(9)]) as i64;
        let sec = u16::from_le_bytes([*dt.add(10), *dt.add(11)]) as i64;
        let usec = u32::from_le_bytes([*dt.add(12), *dt.add(13), *dt.add(14), *dt.add(15)]) as i64;
        // Days since 1970-01-01 (Howard Hinnant's days_from_civil), then shift to 0001-01-01 (+719162).
        let y = year - if month <= 2 { 1 } else { 0 };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days_unix = era * 146_097 + doe - 719_468;
        let days_0001 = (days_unix + 719_162).max(0);
        let secs = days_0001 * 86_400 + hour * 3600 + min * 60 + sec;
        *tick = (secs as u64).saturating_mul(1_000_000).saturating_add(usec.max(0) as u64);
    }
    0
}

// ── libScePlayGo (all chunks treated as 100% locally installed) ───────

pub extern "sysv64" fn playgo_open(out_handle: *mut i32, _param: *const u8) -> i32 {
    if !out_handle.is_null() {
        unsafe { *out_handle = 1 };
    }
    0
}

pub extern "sysv64" fn playgo_get_locus(_handle: i32, _chunk_ids: *const u16, count: u32, out_loci: *mut u8) -> i32 {
    if !out_loci.is_null() && count > 0 {
        // SCE_PLAYGO_LOCUS_LOCAL_FAST = 3
        unsafe { std::ptr::write_bytes(out_loci, 3, count as usize) };
    }
    0
}

pub extern "sysv64" fn playgo_get_progress(_handle: i32, _chunk_ids: *const u16, _count: u32, out_progress: *mut u64) -> i32 {
    if !out_progress.is_null() {
        // ScePlayGoProgress { u64 progressSize, u64 totalSize } -> 100% installed
        unsafe {
            *out_progress = 1;
            *out_progress.add(1) = 1;
        }
    }
    0
}

pub extern "sysv64" fn playgo_get_empty_list(_handle: i32, _out_list: *mut u8, _max: u32, out_count: *mut u32) -> i32 {
    if !out_count.is_null() {
        unsafe { *out_count = 0 };
    }
    0
}

pub extern "sysv64" fn playgo_get_install_speed(_handle: i32, out_speed: *mut i32) -> i32 {
    if !out_speed.is_null() {
        unsafe { *out_speed = 2 }; // SCE_PLAYGO_INSTALL_SPEED_TRICKLE/FAST
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
        ("scePadSetVibration", ok as *const () as usize),
        ("scePadSetLightBar", ok as *const () as usize),
        ("scePadResetLightBar", ok as *const () as usize),
        ("scePadSetMotionSensorState", ok as *const () as usize),
        ("scePadSetTiltCorrectionState", ok as *const () as usize),
        ("scePadSetAngularVelocityDeadbandState", ok as *const () as usize),
        ("scePadResetOrientation", ok as *const () as usize),
        ("scePadSetTriggerEffect", ok as *const () as usize),
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
        ("sceNpCheckCallback", ok as *const () as usize),
        ("sceNetCtlCheckCallback", ok as *const () as usize),
        ("sceNpGetState", np_get_state as *const () as usize),
        ("sceNpGetAccountIdA", np_get_account_id as *const () as usize),
        ("#rbknaUjpqWo", np_get_account_id as *const () as usize), // sceNpGetAccountIdA
        ("sceNpGetAccountCountryA", np_get_account_country as *const () as usize),
        ("sceNpAuthCreateRequest", np_auth_create_request as *const () as usize),
        // libSceNpTrophy2 + PS4 libSceNpTrophy
        ("sceNpTrophy2CreateContext", np_trophy_create_ctx as *const () as usize),
        ("sceNpTrophy2CreateHandle", np_trophy_create_handle as *const () as usize),
        ("sceNpTrophy2DestroyContext", ok as *const () as usize),
        ("sceNpTrophy2DestroyHandle", ok as *const () as usize),
        ("sceNpTrophy2RegisterContext", ok as *const () as usize),
        ("sceNpTrophy2UnlockTrophy", np_trophy_unlock as *const () as usize),
        ("sceNpTrophy2GetTrophyUnlockState", np_trophy_get_unlock_state as *const () as usize),
        ("sceNpTrophyCreateContext", np_trophy_create_ctx as *const () as usize),
        ("sceNpTrophyCreateHandle", np_trophy_create_handle as *const () as usize),
        ("sceNpTrophyDestroyContext", ok as *const () as usize),
        ("sceNpTrophyDestroyHandle", ok as *const () as usize),
        ("sceNpTrophyRegisterContext", ok as *const () as usize),
        ("sceNpTrophyUnlockTrophy", np_trophy_unlock as *const () as usize),
        // libSceNpUniversalDataSystem
        ("sceNpUniversalDataSystemInitialize", ok as *const () as usize),
        ("sceNpUniversalDataSystemTerminate", ok as *const () as usize),
        ("sceNpUniversalDataSystemCreateContext", np_uds_create_out as *const () as usize),
        ("sceNpUniversalDataSystemDestroyContext", ok as *const () as usize),
        ("sceNpUniversalDataSystemCreateHandle", np_uds_create_out as *const () as usize),
        ("sceNpUniversalDataSystemDestroyHandle", ok as *const () as usize),
        ("sceNpUniversalDataSystemRegisterContext", ok as *const () as usize),
        ("sceNpUniversalDataSystemPostEvent", ok as *const () as usize),
        // System Dialogs (CommonDialog / MsgDialog / SaveDataDialog / ImeDialog)
        ("sceCommonDialogInitialize", ok as *const () as usize),
        ("sceMsgDialogInitialize", ok as *const () as usize),
        ("sceMsgDialogOpen", ok as *const () as usize),
        ("sceMsgDialogUpdateStatus", dialog_status_finished as *const () as usize),
        ("sceMsgDialogGetStatus", dialog_status_finished as *const () as usize),
        ("sceMsgDialogGetResult", dialog_get_result as *const () as usize),
        ("sceMsgDialogClose", ok as *const () as usize),
        ("sceMsgDialogTerminate", ok as *const () as usize),
        ("sceSaveDataDialogInitialize", ok as *const () as usize),
        ("sceSaveDataDialogOpen", ok as *const () as usize),
        ("sceSaveDataDialogUpdateStatus", dialog_status_finished as *const () as usize),
        ("sceSaveDataDialogGetStatus", dialog_status_finished as *const () as usize),
        ("sceSaveDataDialogGetResult", dialog_get_result as *const () as usize),
        ("sceSaveDataDialogProgressBarSetValue", ok as *const () as usize),
        ("sceSaveDataDialogClose", ok as *const () as usize),
        ("sceSaveDataDialogTerminate", ok as *const () as usize),
        ("sceImeDialogInit", ok as *const () as usize),
        ("sceImeDialogGetStatus", ime_dialog_status_finished as *const () as usize),
        ("sceImeDialogGetResult", ime_dialog_get_result as *const () as usize),
        ("sceImeDialogTerm", ok as *const () as usize),
        // libSceNetCtl
        ("sceNetCtlInit", ok as *const () as usize),
        ("sceNetCtlTerm", ok as *const () as usize),
        ("sceNetCtlGetState", netctl_get_state as *const () as usize),
        ("sceNetCtlRegisterCallback", netctl_register_callback as *const () as usize),
        ("sceNetCtlUnregisterCallback", ok as *const () as usize),
        // libSceAudio3d
        ("sceAudio3dInitialize", ok as *const () as usize),
        ("sceAudio3dTerminate", ok as *const () as usize),
        ("sceAudio3dPortOpen", audio3d_port_open as *const () as usize),
        ("sceAudio3dPortClose", ok as *const () as usize),
        ("sceAudio3dPortAdvance", ok as *const () as usize),
        ("sceAudio3dPortPush", ok as *const () as usize),
        ("sceAudio3dPortFlush", ok as *const () as usize),
        ("sceAudio3dPortSetAttribute", ok as *const () as usize),
        ("sceAudio3dObjectReserve", ok as *const () as usize),
        ("sceAudio3dObjectUnreserve", ok as *const () as usize),
        ("sceAudio3dObjectSetAttributes", ok as *const () as usize),
        // libSceRtc & libSceCoredump & libScePlayGo
        ("sceRtcGetCurrentClock", rtc_get_current_clock as *const () as usize),
        ("sceRtcGetCurrentClockLocalTime", rtc_get_current_clock_local as *const () as usize),
        ("sceRtcGetCurrentTick", rtc_get_current_tick as *const () as usize),
        ("sceRtcGetTick", rtc_get_tick as *const () as usize),
        ("sceRtcGetTickResolution", rtc_get_tick_resolution as *const () as usize),
        ("sceCoredumpRegisterCoredumpHandler", ok as *const () as usize),
        ("sceCoredumpUnregisterCoredumpHandler", ok as *const () as usize),
        ("scePlayGoInitialize", ok as *const () as usize),
        ("scePlayGoTerminate", ok as *const () as usize),
        ("scePlayGoOpen", playgo_open as *const () as usize),
        ("scePlayGoClose", ok as *const () as usize),
        ("scePlayGoGetLocus", playgo_get_locus as *const () as usize),
        ("scePlayGoGetProgress", playgo_get_progress as *const () as usize),
        ("scePlayGoGetToDoList", playgo_get_empty_list as *const () as usize),
        ("scePlayGoSetToDoList", ok as *const () as usize),
        ("scePlayGoGetChunkIdList", playgo_get_empty_list as *const () as usize),
        ("scePlayGoGetInstallSpeed", playgo_get_install_speed as *const () as usize),
        ("scePlayGoSetInstallSpeed", ok as *const () as usize),
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

    #[test]
    fn auto_keys_parses_and_holds_for_window() {
        let sched = parse_auto_keys("800:cross, 1100:down, 1400:r2");
        assert_eq!(sched.len(), 3);
        assert_eq!(eval_auto_keys(&sched, 799), (0, 0, 0));
        assert_eq!(eval_auto_keys(&sched, 800), (SCE_PAD_BUTTON_CROSS, 0, 0));
        assert_eq!(eval_auto_keys(&sched, 811), (SCE_PAD_BUTTON_CROSS, 0, 0));
        assert_eq!(eval_auto_keys(&sched, 812), (0, 0, 0));
        assert_eq!(eval_auto_keys(&sched, 1105), (SCE_PAD_BUTTON_DOWN, 0, 0));
        assert_eq!(eval_auto_keys(&sched, 1400), (SCE_PAD_BUTTON_R2, 0, 0xFF));
    }

    #[test]
    fn np_offline_stubs_are_safe() {
        let mut acct = 123u64;
        assert_eq!(np_get_account_id(1, &mut acct), SCE_NP_ERROR_SIGNED_OUT);
        assert_eq!(acct, 0);
        let mut cc = [0xFFu8; 4];
        assert_eq!(np_get_account_country(1, cc.as_mut_ptr()), 0);
        assert_eq!(&cc, b"us\0\0");
        assert_eq!(np_auth_create_request(), SCE_NP_ERROR_SIGNED_OUT);
    }

    #[test]
    fn wave1_trophy_dialog_audio3d_stubs_work() {
        let mut ctx = 0i32;
        let mut h = 0i32;
        assert_eq!(np_trophy_create_ctx(&mut ctx, 1, 0, 0), 0);
        assert_eq!(np_trophy_create_handle(&mut h), 0);
        assert!(ctx > 0 && h > 0);
        let mut plat = 0i32;
        assert_eq!(np_trophy_unlock(ctx, h, 5, &mut plat), 0);
        assert_eq!(plat, -1);
        let mut flags = [0u32; 4];
        let mut count = 0u32;
        assert_eq!(np_trophy_get_unlock_state(ctx, h, flags.as_mut_ptr(), &mut count), 0);
        assert_ne!(flags[0] & (1 << 5), 0);
        assert!(count >= 1);

        assert_eq!(dialog_status_finished(), 3);
        let mut res = [-1i32; 8];
        assert_eq!(dialog_get_result(res.as_mut_ptr()), 0);
        assert_eq!(res[1], 0);
        assert_eq!(res[2], 1);

        assert_eq!(ime_dialog_status_finished(), 2);
        let mut port_id = 0u32;
        assert_eq!(audio3d_port_open(1, std::ptr::null(), &mut port_id), 0);
        assert_eq!(port_id, 1);

        let mut dt = [0u8; 16];
        assert_eq!(rtc_get_current_clock_local(dt.as_mut_ptr()), 0);
        let year = u16::from_le_bytes([dt[0], dt[1]]);
        let month = u16::from_le_bytes([dt[2], dt[3]]);
        let day = u16::from_le_bytes([dt[4], dt[5]]);
        assert!(year >= 2024 && (1..=12).contains(&month) && (1..=31).contains(&day));
        assert_eq!(rtc_get_tick_resolution(), 1_000_000);
        let mut tick = 0u64;
        assert_eq!(rtc_get_tick(dt.as_ptr(), &mut tick), 0);
        assert!(tick > 63_000_000_000_000_000);

        let mut pg_h = 0i32;
        assert_eq!(playgo_open(&mut pg_h, std::ptr::null()), 0);
        assert_eq!(pg_h, 1);
        let mut loci = [0u8; 4];
        assert_eq!(playgo_get_locus(pg_h, std::ptr::null(), 4, loci.as_mut_ptr()), 0);
        assert_eq!(loci, [3, 3, 3, 3]);
    }
}
