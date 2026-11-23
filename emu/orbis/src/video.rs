//! Video out: the game's display buffers, shown in a window on every flip.
//!
//! ORBIS_HIDDEN=1 keeps the window hidden; ORBIS_DUMP=<folder> writes every 60th frame as a
//! BMP file there (for testing without a visible window).

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Default, Debug)]
struct Buffer {
    addr: usize,
    width: u32,
    height: u32,
    pitch: u32,
    format: u64,
    tiling: u32,
}

static BUFFERS: Mutex<[Buffer; 16]> = Mutex::new([Buffer { addr: 0, width: 0, height: 0, pitch: 0, format: 0, tiling: 0 }; 16]);
static FLIPS: AtomicU64 = AtomicU64::new(0);
static LAST_ARG: AtomicU64 = AtomicU64::new(0);
static CURRENT: AtomicU64 = AtomicU64::new(0);

/// The picture the window shows: BGRA pixels, width, height.
static FRAME: Mutex<(Vec<u32>, u32, u32)> = Mutex::new((Vec::new(), 0, 0));

/// sceVideoOutOpen(user, bus, index, param)
pub extern "sysv64" fn open(_user: i32, _bus: i32, _index: i32, _param: *const c_void) -> i32 {
    eprintln!("[orbis] video out opened (display handle 1)");
    start_window();
    1
}

/// sceVideoOutSetBufferAttribute2(attr, pixel_format, tiling, width, height, option, dcc_control, dcc_clear)
pub extern "sysv64" fn set_attribute2(attr: *mut u8, format: u64, tiling: u32, width: u32, height: u32, option: u64, dcc_control: u32, dcc_clear: u64) -> i32 {
    if attr.is_null() {
        return 0x8029_0002u32 as i32;
    }
    unsafe {
        std::ptr::write_bytes(attr, 0, 0x50);
        *(attr.add(0x04) as *mut u32) = tiling;
        *(attr.add(0x0C) as *mut u32) = width;
        *(attr.add(0x10) as *mut u32) = height;
        *(attr.add(0x18) as *mut u64) = option;
        *(attr.add(0x20) as *mut u64) = format;
        *(attr.add(0x28) as *mut u64) = dcc_clear;
        *(attr.add(0x30) as *mut u32) = dcc_control;
    }
    0
}

/// sceVideoOutSetBufferAttribute(attr, format, tiling, aspect, width, height, pitch) — PS4 style.
pub extern "sysv64" fn set_attribute(attr: *mut u32, format: u32, tiling: u32, aspect: u32, width: u32, height: u32, pitch: u32) -> i32 {
    unsafe {
        std::ptr::write_bytes(attr as *mut u8, 0, 0x40);
        *attr = format;
        *attr.add(1) = tiling;
        *attr.add(2) = aspect;
        *attr.add(3) = width;
        *attr.add(4) = height;
        *attr.add(5) = pitch;
    }
    0
}

/// sceVideoOutRegisterBuffers2(handle, set, start, buffers, count, attr, category, option)
pub extern "sysv64" fn register_buffers2(_handle: i32, set: i32, start: i32, buffers: *const u64, count: i32, attr: *const u8) -> i32 {
    let (tiling, width, height, format) = unsafe { (*(attr.add(4) as *const u32), *(attr.add(0xC) as *const u32), *(attr.add(0x10) as *const u32), *(attr.add(0x20) as *const u64)) };
    let mut b = BUFFERS.lock().unwrap();
    for i in 0..count.max(0) as usize {
        let addr = unsafe { *buffers.add(i * 4) } as usize;
        if let Some(slot) = b.get_mut(start as usize + i) {
            *slot = Buffer { addr, width, height, pitch: width, format, tiling };
        }
    }
    eprintln!("[orbis] video out: {count} display buffers {width}x{height} (format {format:#x}, tiling {tiling})");
    set
}

/// sceVideoOutRegisterBuffers(handle, start, addresses, count, attr) — PS4 style.
pub extern "sysv64" fn register_buffers(_handle: i32, start: i32, addrs: *const usize, count: i32, attr: *const u32) -> i32 {
    let (format, tiling, width, height, pitch) = unsafe { (*attr as u64, *attr.add(1), *attr.add(3), *attr.add(4), *attr.add(5)) };
    let mut b = BUFFERS.lock().unwrap();
    for i in 0..count.max(0) as usize {
        if let Some(slot) = b.get_mut(start as usize + i) {
            *slot = Buffer { addr: unsafe { *addrs.add(i) }, width, height, pitch: pitch.max(width), format, tiling };
        }
    }
    eprintln!("[orbis] video out: {count} display buffers {width}x{height}");
    0
}

/// sceVideoOutSubmitFlip(handle, index, mode, arg)
pub extern "sysv64" fn submit_flip(_handle: i32, index: i32, _mode: i32, arg: i64) -> i32 {
    let n = FLIPS.fetch_add(1, Ordering::SeqCst) + 1;
    LAST_ARG.store(arg as u64, Ordering::SeqCst);
    CURRENT.store(index as u64, Ordering::SeqCst);
    if index >= 0 {
        let buf = BUFFERS.lock().unwrap().get(index as usize).copied().unwrap_or_default();
        if buf.addr != 0 && buf.width > 0 {
            present(&buf, n);
        }
    }
    if n == 1 {
        eprintln!("[orbis] video out: first frame submitted");
    }
    crate::equeue::trigger_flip(arg as u64);
    0
}

/// sceVideoOutGetFlipStatus(handle, status)
pub extern "sysv64" fn flip_status(_handle: i32, st: *mut u8) -> i32 {
    unsafe {
        std::ptr::write_bytes(st, 0, 0x48);
        let n = FLIPS.load(Ordering::SeqCst);
        *(st as *mut u64) = n;
        *(st.add(0x18) as *mut u64) = LAST_ARG.load(Ordering::SeqCst);
        *(st.add(0x38) as *mut i32) = CURRENT.load(Ordering::SeqCst) as i32;
    }
    0
}

/// sceVideoOutGetResolutionStatus / GetOutputStatus: 1920x1080 at 60 Hz.
pub extern "sysv64" fn output_status(_handle: i32, st: *mut u32) -> i32 {
    if !st.is_null() {
        unsafe {
            std::ptr::write_bytes(st as *mut u8, 0, 0x20);
            *st = 1920;
            *st.add(1) = 1080;
            *st.add(2) = 1920;
            *st.add(3) = 1080;
        }
    }
    0
}

pub extern "sysv64" fn is_flip_pending(_handle: i32) -> i32 {
    0
}

pub extern "sysv64" fn ok() -> i32 {
    0
}

/// sceVideoOutWaitVblank: ~60 Hz.
pub extern "sysv64" fn wait_vblank(_handle: i32) -> i32 {
    std::thread::sleep(std::time::Duration::from_millis(16));
    0
}

// ── presenting ────────────────────────────────────────────────────────

fn present(buf: &Buffer, n: u64) {
    let (w, h) = (buf.width as usize, buf.height as usize);
    let pitch = buf.pitch.max(buf.width) as usize;
    let src = unsafe { std::slice::from_raw_parts(buf.addr as *const u32, pitch * h) };
    // R and B swap for ABGR-ordered formats; others are shown as stored (BGRA).
    let swap = (buf.format >> 8) & 0xFF == 0x22 || buf.format & 0xFFFF_FFFF == 0x8000_2200;
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let v = src[y * pitch + x];
            px[y * w + x] = if swap { (v & 0xFF00FF00) | ((v & 0xFF) << 16) | ((v >> 16) & 0xFF) } else { v };
        }
    }
    if let Ok(dir) = std::env::var("ORBIS_DUMP") {
        if n == 1 || n % 60 == 0 {
            let _ = write_bmp(&format!("{dir}/frame{n:05}.bmp"), &px, w, h);
        }
    }
    *FRAME.lock().unwrap() = (px, w as u32, h as u32);
    #[cfg(windows)]
    unsafe {
        let hwnd = WINDOW.load(Ordering::SeqCst) as windows_sys::Win32::Foundation::HWND;
        if !hwnd.is_null() {
            if n == 1 || n % 15 == 0 {
                let (_, draws, dispatches) = crate::agc::stats();
                let shaders = crate::gpu::SHADERS.lock().map(|s| s.len()).unwrap_or(0);
                let name = std::env::var("ORBIS_TITLE").unwrap_or_else(|_| "PS5".into());
                let title: Vec<u16> = format!(
                    "POLYMIRA Orbis — {name} | {w}x{h} | Frame #{n} | {draws} draws, {dispatches} dispatches, {shaders} shaders\0"
                ).encode_utf16().collect();
                windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, title.as_ptr());
            }
            windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 0);
        }
    }
}

fn write_bmp(path: &str, px: &[u32], w: usize, h: usize) -> std::io::Result<()> {
    let mut f = Vec::with_capacity(54 + w * h * 4);
    let size = (54 + w * h * 4) as u32;
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&size.to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(&54u32.to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes());
    f.extend_from_slice(&(w as i32).to_le_bytes());
    f.extend_from_slice(&(-(h as i32)).to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&32u16.to_le_bytes());
    f.extend_from_slice(&[0u8; 24]);
    for p in px {
        f.extend_from_slice(&p.to_le_bytes());
    }
    std::fs::write(path, f)
}

static WINDOW: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
unsafe extern "system" fn wndproc(hwnd: windows_sys::Win32::Foundation::HWND, msg: u32, wp: usize, lp: isize) -> isize {
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let frame = FRAME.lock().unwrap();
            if frame.1 > 0 {
                let mut bi: BITMAPINFO = std::mem::zeroed();
                bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = frame.1 as i32;
                bi.bmiHeader.biHeight = -(frame.2 as i32);
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                SetStretchBltMode(dc, HALFTONE);
                StretchDIBits(dc, 0, 0, rc.right, rc.bottom, 0, 0, frame.1 as i32, frame.2 as i32, frame.0.as_ptr() as *const c_void, &bi, DIB_RGB_COLORS, SRCCOPY);
            } else {
                FillRect(dc, &rc, GetStockObject(BLACK_BRUSH) as HBRUSH);
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_DESTROY => {
            std::process::exit(0);
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn start_window() {
    #[cfg(windows)]
    {
        if WINDOW.load(Ordering::SeqCst) != 0 {
            return;
        }
        std::thread::spawn(|| unsafe {
            use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            let class: Vec<u16> = "PolymiraOrbis\0".encode_utf16().collect();
            let title: Vec<u16> = format!("POLYMIRA Orbis — {}\0", std::env::var("ORBIS_TITLE").unwrap_or_else(|_| "PS5".into())).encode_utf16().collect();
            let inst = GetModuleHandleW(std::ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = inst;
            wc.lpszClassName = class.as_ptr();
            wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 1296, 759, std::ptr::null_mut(), std::ptr::null_mut(), inst, std::ptr::null());
            WINDOW.store(hwnd as u64, Ordering::SeqCst);
            if std::env::var_os("ORBIS_HIDDEN").is_none() {
                ShowWindow(hwnd, SW_SHOW);
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    }
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("sceVideoOutOpen", open as *const () as usize),
        ("sceVideoOutClose", ok as *const () as usize),
        ("sceVideoOutSetBufferAttribute2", set_attribute2 as *const () as usize),
        ("#PjS5uASwcV8", set_attribute2 as *const () as usize),
        ("sceVideoOutSetBufferAttribute", set_attribute as *const () as usize),
        ("sceVideoOutRegisterBuffers2", register_buffers2 as *const () as usize),
        ("#rKBUtgRrtbk", register_buffers2 as *const () as usize),
        ("sceVideoOutRegisterBuffers", register_buffers as *const () as usize),
        ("sceVideoOutSubmitFlip", submit_flip as *const () as usize),
        ("sceVideoOutGetFlipStatus", flip_status as *const () as usize),
        ("sceVideoOutIsFlipPending", is_flip_pending as *const () as usize),
        ("sceVideoOutGetResolutionStatus", output_status as *const () as usize),
        ("sceVideoOutGetOutputStatus", output_status as *const () as usize),
        ("#utPrVdxio-8", output_status as *const () as usize),
        ("sceVideoOutSetFlipRate", ok as *const () as usize),
        ("sceVideoOutWaitVblank", wait_vblank as *const () as usize),
        ("sceVideoOutAddFlipEvent", crate::equeue::add_flip_event as *const () as usize),
        ("sceVideoOutAddVblankEvent", ok as *const () as usize),
        ("sceVideoOutConfigureOutput", ok as *const () as usize),
        ("sceVideoOutInitializeOutputOptions", ok as *const () as usize),
        ("sceVideoOutSetWindowModeMargins", ok as *const () as usize),
        ("sceVideoOutAdjustColor_", ok as *const () as usize),
        ("sceVideoOutColorSettingsSetGamma_", ok as *const () as usize),
        ("sceVideoOutIsOutputSupported", ok as *const () as usize),
    ]
}
