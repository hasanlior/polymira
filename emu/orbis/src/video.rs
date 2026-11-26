//! Video out: the game's display buffers, shown in a window on every flip.
//!
//! ORBIS_HIDDEN=1 keeps the window hidden; ORBIS_DUMP=<folder> writes every 60th frame as a
//! BMP file there (for testing without a visible window).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
static CLEAR_RGBA: Mutex<[f32; 4]> = Mutex::new([0.12, 0.12, 0.12, 1.0]);
static SPLASH_ACTIVE: AtomicBool = AtomicBool::new(false);
static SPLASH_IMG: Mutex<Option<(Vec<u32>, u32, u32)>> = Mutex::new(None);
static SPLASH_START: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Cubic ease-out brightness factor (0.0 -> 1.0 over 450ms) for PS5-style boot splash fade-in.
fn splash_fade_factor() -> f32 {
    let start = SPLASH_START.lock().ok().and_then(|g| *g);
    let Some(t0) = start else { return 1.0 };
    let t = (t0.elapsed().as_secs_f32() / 0.45).clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

fn fade_bgra(src: &[u32], factor: f32) -> Vec<u32> {
    if factor >= 0.999 {
        return src.to_vec();
    }
    let scale = (factor.clamp(0.0, 1.0) * 256.0) as u32;
    src.iter()
        .map(|&px| {
            let b = ((px & 0xFF) * scale) >> 8;
            let g = (((px >> 8) & 0xFF) * scale) >> 8;
            let r = (((px >> 16) & 0xFF) * scale) >> 8;
            0xFF00_0000 | (r << 16) | (g << 8) | b
        })
        .collect()
}

pub fn set_clear_color(rgba: [f32; 4]) {
    if let Ok(mut c) = CLEAR_RGBA.lock() {
        *c = rgba;
    }
}

/// POLYMIRA OS Splash Screen: loads `<game_dir>/sce_sys/pic0.png` (like PS5 OS boot splash)
/// and displays it immediately on Vulkan until `sceSystemServiceHideSplashScreen` + first game draw.
pub fn show_os_splash(game_dir: &str) {
    if std::env::var_os("ORBIS_HIDDEN").is_some() || std::env::var_os("ORBIS_HEADLESS").is_some() {
        return;
    }
    let candidates = [
        format!("{game_dir}/sce_sys/pic0.png"),
        format!("{game_dir}/sce_sys/pic1.png"),
        format!("{game_dir}/sce_sys/icon0.png"),
    ];
    for p in &candidates {
        let Ok(file) = std::fs::File::open(p) else { continue };
        let mut dec = png::Decoder::new(std::io::BufReader::new(file));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let Ok(mut reader) = dec.read_info() else { continue };
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let Ok(info) = reader.next_frame(&mut buf) else { continue };
        let (w, h) = (info.width, info.height);
        let mut bgra = Vec::with_capacity((w * h) as usize);
        match info.color_type {
            png::ColorType::Rgba => {
                for ch in buf[..info.buffer_size()].chunks_exact(4) {
                    bgra.push(u32::from_le_bytes([ch[2], ch[1], ch[0], ch[3]]));
                }
            }
            png::ColorType::Rgb => {
                for ch in buf[..info.buffer_size()].chunks_exact(3) {
                    bgra.push(u32::from_le_bytes([ch[2], ch[1], ch[0], 0xFF]));
                }
            }
            _ => continue,
        }
        if bgra.len() == (w * h) as usize {
            eprintln!("[orbis] POLYMIRA OS splash: loaded {p} ({w}x{h})");
            *FRAME.lock().unwrap() = (vec![0xFF00_0000u32; (w * h) as usize], w, h);
            *SPLASH_IMG.lock().unwrap() = Some((bgra, w, h));
            *SPLASH_START.lock().unwrap() = Some(std::time::Instant::now());
            SPLASH_ACTIVE.store(true, Ordering::SeqCst);
            start_window();
            #[cfg(windows)]
            std::thread::spawn(|| {
                for _ in 0..32 {
                    if !SPLASH_ACTIVE.load(Ordering::SeqCst) {
                        break;
                    }
                    let factor = splash_fade_factor();
                    let faded = {
                        let sp = SPLASH_IMG.lock().unwrap();
                        sp.as_ref().map(|(px, sw, sh)| (fade_bgra(px, factor), *sw, *sh))
                    };
                    if let Some((fpx, sw, sh)) = faded {
                        let vk_ok = crate::vk::with_renderer(|r| r.upload_and_present_rgba(&fpx, sw, sh)).unwrap_or(false);
                        *FRAME.lock().unwrap() = (fpx, sw, sh);
                        if !vk_ok {
                            let hwnd = WINDOW.load(Ordering::SeqCst) as windows_sys::Win32::Foundation::HWND;
                            if !hwnd.is_null() {
                                unsafe { windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 0); }
                            }
                        }
                    }
                    if factor >= 0.999 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(16));
                }
            });
            break;
        }
    }
}

pub fn hide_os_splash() {
    if SPLASH_ACTIVE.swap(false, Ordering::SeqCst) {
        eprintln!("[orbis] POLYMIRA OS splash: hidden by game (sceSystemServiceHideSplashScreen)");
    }
}

/// The picture the window shows: BGRA pixels, width, height.
static FRAME: Mutex<(Vec<u32>, u32, u32)> = Mutex::new((Vec::new(), 0, 0));

/// sceVideoOutOpen(user, bus, index, param)
pub extern "sysv64" fn open(_user: i32, _bus: i32, _index: i32, _param: *const c_void) -> i32 {
    eprintln!("[orbis] video out opened (display handle 1)");
    start_window();
    #[cfg(windows)]
    if std::env::var_os("ORBIS_HIDDEN").is_none() && std::env::var_os("ORBIS_HEADLESS").is_none() {
        for _ in 0..50 {
            let hwnd = WINDOW.load(Ordering::SeqCst) as *mut c_void;
            if !hwnd.is_null() {
                let inst = unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) } as *mut c_void;
                let (iw, ih) = (WIN_W.load(Ordering::Relaxed).max(1), WIN_H.load(Ordering::Relaxed).max(1));
                crate::vk::init_window(hwnd, inst, iw, ih);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
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
    let (verts, idxs, batches) = crate::agc::take_frame_draws();
    let (gverts, gens) = crate::agc::take_frame_gen();
    let has_draws = (!verts.is_empty() && !idxs.is_empty()) || !gens.is_empty();
    if std::env::var_os("ORBIS_FRAME_STATS").is_some() && (n <= 3 || n % 120 == 0) {
        eprintln!("[orbis] frame {n}: {} verts, {} indices, {} batches, {} generic ({} gverts) totals={:?}", verts.len(), idxs.len(), batches.len(), gens.len(), gverts.len(), crate::agc::gen_stats());
    }
    let dump_dir = std::env::var("ORBIS_DUMP").ok();
    let mut has_pixels = false;
    if !has_draws || dump_dir.is_some() {
        let pitch = buf.pitch.max(buf.width) as usize;
        let mut px = if buf.tiling != 0 {
            let bytes = unsafe { std::slice::from_raw_parts(buf.addr as *const u8, pitch * h * 4) };
            crate::tiling::untile_32bpp(bytes, buf.width, buf.height, buf.tiling)
        } else {
            let src = unsafe { std::slice::from_raw_parts(buf.addr as *const u32, pitch * h) };
            let mut out = vec![0u32; w * h];
            for y in 0..h {
                out[y * w..(y + 1) * w].copy_from_slice(&src[y * pitch..y * pitch + w]);
            }
            out
        };
        // R and B swap for ABGR-ordered formats; others are shown as stored (BGRA).
        let swap = (buf.format >> 8) & 0xFF == 0x22 || buf.format & 0xFFFF_FFFF == 0x8000_2200;
        if swap {
            for p in &mut px {
                let v = *p;
                *p = (v & 0xFF00FF00) | ((v & 0xFF) << 16) | ((v >> 16) & 0xFF);
            }
        }
        if let Some(dir) = dump_dir {
            if n == 1 || n % 60 == 0 {
                let _ = write_bmp(&format!("{dir}/frame{n:05}.bmp"), &px, w, h);
            }
        }
        has_pixels = px.iter().step_by(64).any(|&p| (p & 0x00FF_FFFF) != 0);
        *FRAME.lock().unwrap() = (px, w as u32, h as u32);
    }
    #[cfg(windows)]
    unsafe {
        let hwnd = WINDOW.load(Ordering::SeqCst) as windows_sys::Win32::Foundation::HWND;
        if !hwnd.is_null() {
            if n == 1 || n % 15 == 0 {
                let (_, draws, dispatches) = crate::agc::stats();
                let shaders = crate::gpu::SHADERS.lock().map(|s| s.len()).unwrap_or(0);
                let name = std::env::var("ORBIS_TITLE").unwrap_or_else(|_| "PS5".into());
                let badge = crate::vk::render_mode_label();
                let title: Vec<u16> = if badge.is_empty() {
                    format!("POLYMIRA Orbis — {name} | {w}x{h} | Frame #{n} | {draws} draws, {dispatches} dispatches, {shaders} shaders\0")
                } else {
                    format!("POLYMIRA Orbis — {name} | {badge} | {w}x{h} | Frame #{n}\0")
                }.encode_utf16().collect();
                windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, title.as_ptr());
            }
            let clear = CLEAR_RGBA.lock().map(|c| *c).unwrap_or([0.12, 0.12, 0.12, 1.0]);
            if has_draws {
                SPLASH_ACTIVE.store(false, Ordering::SeqCst);
            }
            let presented_vk = if has_draws {
                crate::vk::with_renderer(|r| r.present_frame(clear, &verts, &idxs, &batches, &gverts, &gens, (buf.addr, buf.width, buf.height))).unwrap_or(false)
            } else if SPLASH_ACTIVE.load(Ordering::SeqCst) {
                let sp = SPLASH_IMG.lock().unwrap();
                if let Some((ref px, sw, sh)) = *sp {
                    let factor = splash_fade_factor();
                    if factor < 0.999 {
                        let fpx = fade_bgra(px, factor);
                        crate::vk::with_renderer(|r| r.upload_and_present_rgba(&fpx, sw, sh)).unwrap_or(false)
                    } else {
                        crate::vk::with_renderer(|r| r.upload_and_present_rgba(px, sw, sh)).unwrap_or(false)
                    }
                } else {
                    crate::vk::with_renderer(|r| r.present_target(buf.addr, clear)).unwrap_or(false)
                }
            } else if has_pixels {
                let frame = FRAME.lock().unwrap();
                crate::vk::with_renderer(|r| r.upload_and_present_rgba(&frame.0, frame.1, frame.2)).unwrap_or(false)
            } else {
                crate::vk::with_renderer(|r| r.present_target(buf.addr, clear)).unwrap_or(false)
            };
            if !presented_vk && WIN_W.load(Ordering::Relaxed) > 0 && WIN_H.load(Ordering::Relaxed) > 0 {
                windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 0);
            }
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
static WIN_W: AtomicU32 = AtomicU32::new(1280);
static WIN_H: AtomicU32 = AtomicU32::new(720);
static WIN_RESIZED: AtomicBool = AtomicBool::new(false);
static FULLSCREEN: AtomicBool = AtomicBool::new(false);
static SAVED_STYLE: AtomicU32 = AtomicU32::new(0);
static SAVED_RECT: Mutex<[i32; 4]> = Mutex::new([100, 100, 1380, 820]);

pub fn window_size() -> (u32, u32) {
    (WIN_W.load(Ordering::Relaxed), WIN_H.load(Ordering::Relaxed))
}

pub fn take_resize() -> Option<(u32, u32)> {
    if WIN_RESIZED.swap(false, Ordering::AcqRel) {
        Some(window_size())
    } else {
        None
    }
}

#[cfg(windows)]
unsafe fn toggle_fullscreen(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let entering = !FULLSCREEN.load(Ordering::Relaxed);
    if entering {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        SAVED_STYLE.store(style, Ordering::Relaxed);
        let mut rc: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rc) != 0 {
            *SAVED_RECT.lock().unwrap_or_else(|e| e.into_inner()) = [rc.left, rc.top, rc.right, rc.bottom];
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) != 0 {
            let r = mi.rcMonitor;
            let borderless = (style & !(WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU)) | WS_POPUP;
            SetWindowLongW(hwnd, GWL_STYLE, borderless as i32);
            SetWindowPos(
                hwnd,
                HWND_TOP,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
            );
            FULLSCREEN.store(true, Ordering::Relaxed);
        }
    } else {
        let style = SAVED_STYLE.load(Ordering::Relaxed);
        let restore_style = if style != 0 { style } else { WS_OVERLAPPEDWINDOW | WS_VISIBLE };
        let [l, t, r, b] = *SAVED_RECT.lock().unwrap_or_else(|e| e.into_inner());
        SetWindowLongW(hwnd, GWL_STYLE, restore_style as i32);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            l,
            t,
            (r - l).max(320),
            (b - t).max(240),
            SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
        FULLSCREEN.store(false, Ordering::Relaxed);
    }
}

#[cfg(windows)]
unsafe extern "system" fn wndproc(hwnd: windows_sys::Win32::Foundation::HWND, msg: u32, wp: usize, lp: isize) -> isize {
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    match msg {
        WM_SIZE => {
            let w = (lp as usize & 0xFFFF) as u32;
            let h = ((lp as usize >> 16) & 0xFFFF) as u32;
            WIN_W.store(w, Ordering::Relaxed);
            WIN_H.store(h, Ordering::Relaxed);
            WIN_RESIZED.store(true, Ordering::Release);
            0
        }
        WM_KEYDOWN if wp == 0x7A && (lp & (1 << 30)) == 0 => {
            // F11: borderless fullscreen toggle
            toggle_fullscreen(hwnd);
            0
        }
        WM_SYSKEYDOWN if wp == 0x0D && (lp & (1 << 29)) != 0 && (lp & (1 << 30)) == 0 => {
            // Alt+Enter: borderless fullscreen toggle
            toggle_fullscreen(hwnd);
            0
        }
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
            crate::kernel::flush_all_files();
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
            // ORBIS_OFFSCREEN=1: a real window (so Vulkan presents) placed off every monitor,
            // never activated and kept off the taskbar, for automated test runs.
            let offscreen = std::env::var_os("ORBIS_OFFSCREEN").is_some();
            let want_fs = !offscreen
                && std::env::var("ORBIS_FULLSCREEN")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
            let (wx, wy, ww, wh, style) = if offscreen {
                (-30000, -30000, 1296, 759, WS_POPUP | WS_VISIBLE)
            } else if want_fs {
                use windows_sys::Win32::Foundation::POINT;
                use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
                let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
                let mut mi: MONITORINFO = std::mem::zeroed();
                mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                let (mx, my, mw, mh) = if GetMonitorInfoW(mon, &mut mi) != 0 {
                    let r = mi.rcMonitor;
                    (r.left, r.top, (r.right - r.left).max(640), (r.bottom - r.top).max(480))
                } else {
                    (0, 0, GetSystemMetrics(SM_CXSCREEN).max(1280), GetSystemMetrics(SM_CYSCREEN).max(720))
                };
                // Save a centered 1280x720 windowed rect so F11 / Alt+Enter cleanly restores windowed mode.
                let cx = mx + ((mw - 1296) / 2).max(0);
                let cy = my + ((mh - 759) / 2).max(0);
                SAVED_STYLE.store(WS_OVERLAPPEDWINDOW | WS_VISIBLE, Ordering::Relaxed);
                *SAVED_RECT.lock().unwrap_or_else(|e| e.into_inner()) = [cx, cy, cx + 1296, cy + 759];
                FULLSCREEN.store(true, Ordering::Relaxed);
                WIN_W.store(mw as u32, Ordering::Relaxed);
                WIN_H.store(mh as u32, Ordering::Relaxed);
                (mx, my, mw, mh, WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN)
            } else {
                (CW_USEDEFAULT, CW_USEDEFAULT, 1296, 759, WS_OVERLAPPEDWINDOW | WS_VISIBLE)
            };
            let hwnd = CreateWindowExW(
                if offscreen { WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE } else { 0 },
                class.as_ptr(),
                title.as_ptr(),
                style,
                wx,
                wy,
                ww,
                wh,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                inst,
                std::ptr::null(),
            );
            WINDOW.store(hwnd as u64, Ordering::SeqCst);
            if std::env::var_os("ORBIS_HIDDEN").is_none() && std::env::var_os("ORBIS_HEADLESS").is_none() {
                // First ShowWindow obeys STARTUPINFO.wShowWindow (which may be SW_HIDE);
                // calling ShowWindow twice guarantees the window becomes visible.
                if offscreen {
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                } else {
                    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
                    let fg = GetForegroundWindow();
                    let fg_tid = if !fg.is_null() { GetWindowThreadProcessId(fg, std::ptr::null_mut()) } else { 0 };
                    let cur_tid = GetCurrentThreadId();
                    let attached = fg_tid != 0 && fg_tid != cur_tid && AttachThreadInput(fg_tid, cur_tid, 1) != 0;
                    if want_fs {
                        SetWindowPos(hwnd, HWND_TOPMOST, wx, wy, ww, wh, SWP_SHOWWINDOW | SWP_FRAMECHANGED);
                        ShowWindow(hwnd, SW_SHOW);
                        BringWindowToTop(hwnd);
                        SetForegroundWindow(hwnd);
                        SetWindowPos(hwnd, HWND_NOTOPMOST, wx, wy, ww, wh, SWP_SHOWWINDOW);
                    } else {
                        SetWindowPos(hwnd, HWND_TOPMOST, wx, wy, ww, wh, SWP_SHOWWINDOW);
                        ShowWindow(hwnd, SW_SHOW);
                        ShowWindow(hwnd, SW_SHOWNORMAL);
                        BringWindowToTop(hwnd);
                        SetForegroundWindow(hwnd);
                        SetWindowPos(hwnd, HWND_NOTOPMOST, wx, wy, ww, wh, SWP_SHOWWINDOW);
                    }
                    if attached {
                        AttachThreadInput(fg_tid, cur_tid, 0);
                    }
                }
                windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 0);
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
