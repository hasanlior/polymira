//! Audio output (`libSceAudioOut`) backed by Windows `winmm.dll` (`waveOut*`).
//!
//! Supports 48 kHz stereo/8ch S16 and F32 PCM buffers submitted by `libfmod.prx` and games.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

#[repr(C)]
struct WaveFormatEx {
    w_format_tag: u16,
    n_channels: u16,
    n_samples_per_sec: u32,
    n_avg_bytes_per_sec: u32,
    n_block_align: u16,
    w_bits_per_sample: u16,
    cb_size: u16,
}

#[repr(C)]
struct WaveHdr {
    lp_data: *mut u8,
    dw_buffer_length: u32,
    dw_bytes_recorded: u32,
    dw_user: usize,
    dw_flags: u32,
    dw_loops: u32,
    lp_next: *mut WaveHdr,
    reserved: usize,
}

#[cfg(windows)]
struct WinMm {
    open: unsafe extern "system" fn(*mut usize, u32, *const WaveFormatEx, usize, usize, u32) -> u32,
    prepare: unsafe extern "system" fn(usize, *mut WaveHdr, u32) -> u32,
    unprepare: unsafe extern "system" fn(usize, *mut WaveHdr, u32) -> u32,
    write: unsafe extern "system" fn(usize, *mut WaveHdr, u32) -> u32,
    close: unsafe extern "system" fn(usize) -> u32,
}

#[cfg(windows)]
fn winmm() -> Option<&'static WinMm> {
    static W: OnceLock<Option<WinMm>> = OnceLock::new();
    W.get_or_init(|| unsafe {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
        let h = LoadLibraryA(b"winmm.dll\0".as_ptr());
        if h.is_null() {
            return None;
        }
        Some(WinMm {
            open: std::mem::transmute(GetProcAddress(h, b"waveOutOpen\0".as_ptr())?),
            prepare: std::mem::transmute(GetProcAddress(h, b"waveOutPrepareHeader\0".as_ptr())?),
            unprepare: std::mem::transmute(GetProcAddress(h, b"waveOutUnprepareHeader\0".as_ptr())?),
            write: std::mem::transmute(GetProcAddress(h, b"waveOutWrite\0".as_ptr())?),
            close: std::mem::transmute(GetProcAddress(h, b"waveOutClose\0".as_ptr())?),
        })
    })
    .as_ref()
}

struct Port {
    wave: usize,
    samples: u32,
    channels: u16,
    is_float: bool,
    ring: Vec<(Box<WaveHdr>, Vec<u8>)>,
    cursor: usize,
}

unsafe impl Send for Port {}

static PORTS: Mutex<Vec<Option<Port>>> = Mutex::new(Vec::new());
static NEXT_HANDLE: AtomicI32 = AtomicI32::new(1);

pub extern "sysv64" fn init() -> i32 {
    0
}

/// sceAudioOutOpen(userId, type, index, len, freq, param)
pub extern "sysv64" fn open(_user: i32, _kind: i32, _index: i32, len: u32, freq: u32, param: u32) -> i32 {
    let fmt = param & 0xFF;
    let is_float = matches!(fmt, 2 | 3 | 4 | 6);
    let channels: u16 = match fmt {
        0 | 2 => 1,
        1 | 3 => 2,
        _ => 2,
    };
    let samples = if len == 0 { 256 } else { len };
    let rate = if freq == 0 { 48000 } else { freq };
    let bps: u16 = if is_float { 32 } else { 16 };

    let mut wave = 0usize;
    #[cfg(windows)]
    if std::env::var_os("ORBIS_HIDDEN").is_none() {
        if let Some(mm) = winmm() {
            let wf = WaveFormatEx {
                w_format_tag: if is_float { 3 } else { 1 }, // IEEE_FLOAT or PCM
                n_channels: channels,
                n_samples_per_sec: rate,
                n_avg_bytes_per_sec: rate * (channels as u32) * (bps as u32 / 8),
                n_block_align: channels * (bps / 8),
                w_bits_per_sample: bps,
                cb_size: 0,
            };
            let _ = unsafe { (mm.open)(&mut wave, 0xFFFF_FFFF, &wf, 0, 0, 0) };
        }
    }

    let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
    let idx = (handle - 1) as usize;
    let mut ports = PORTS.lock().unwrap();
    if ports.len() <= idx {
        ports.resize_with(idx + 1, || None);
    }
    let buf_bytes = (samples as usize) * (channels as usize) * (bps as usize / 8);
    let mut ring = Vec::with_capacity(4);
    for _ in 0..4 {
        let mut data = vec![0u8; buf_bytes];
        let hdr = Box::new(WaveHdr {
            lp_data: data.as_mut_ptr(),
            dw_buffer_length: buf_bytes as u32,
            dw_bytes_recorded: 0,
            dw_user: 0,
            dw_flags: 0,
            dw_loops: 0,
            lp_next: std::ptr::null_mut(),
            reserved: 0,
        });
        ring.push((hdr, data));
    }
    ports[idx] = Some(Port { wave, samples, channels, is_float, ring, cursor: 0 });
    eprintln!("[orbis] audio out opened: handle {handle} ({rate} Hz, {channels}ch, {}bit, {samples} samples)", bps);
    handle
}

/// sceAudioOutOutput(handle, ptr)
pub extern "sysv64" fn output(handle: i32, ptr: *const c_void) -> i32 {
    if handle <= 0 || ptr.is_null() {
        return 0;
    }
    let mut ports = PORTS.lock().unwrap();
    let Some(Some(p)) = ports.get_mut((handle - 1) as usize) else {
        return 0;
    };
    let samples = p.samples as i32;
    let bytes = (p.samples as usize) * (p.channels as usize) * if p.is_float { 4 } else { 2 };
    #[cfg(windows)]
    if p.wave != 0 {
        if let Some(mm) = winmm() {
            let slot = p.cursor % p.ring.len();
            p.cursor = p.cursor.wrapping_add(1);
            let (hdr, data) = &mut p.ring[slot];
            let hdr_ptr: *mut WaveHdr = &mut **hdr;
            unsafe {
                if hdr.dw_flags & 2 != 0 {
                    let _ = (mm.unprepare)(p.wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32);
                }
                if data.len() >= bytes {
                    std::ptr::copy_nonoverlapping(ptr as *const u8, data.as_mut_ptr(), bytes);
                    hdr.lp_data = data.as_mut_ptr();
                    hdr.dw_buffer_length = bytes as u32;
                    hdr.dw_flags = 0;
                    let _ = (mm.prepare)(p.wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32);
                    let _ = (mm.write)(p.wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32);
                }
            }
        }
    }
    samples
}

pub extern "sysv64" fn close(handle: i32) -> i32 {
    if handle <= 0 {
        return 0;
    }
    let mut ports = PORTS.lock().unwrap();
    if let Some(Some(p)) = ports.get_mut((handle - 1) as usize).map(|x| x.take()) {
        #[cfg(windows)]
        if p.wave != 0 {
            if let Some(mm) = winmm() {
                unsafe { let _ = (mm.close)(p.wave); }
            }
        }
    }
    0
}

pub extern "sysv64" fn ok() -> i32 {
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("sceAudioOutInit", init as *const () as usize),
        ("sceAudioOutOpen", open as *const () as usize),
        ("sceAudioOutOutput", output as *const () as usize),
        ("sceAudioOutOutputs", ok as *const () as usize),
        ("sceAudioOutClose", close as *const () as usize),
        ("sceAudioOutSetVolume", ok as *const () as usize),
        ("sceAudioOutGetPortState", ok as *const () as usize),
    ]
}
