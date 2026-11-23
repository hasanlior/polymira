//! Audio output (`libSceAudioOut` and PS5 `libSceAudioOut2`) backed by Windows `winmm.dll` (`waveOut*`).
//!
//! All `winmm.dll` calls run on a dedicated native OS worker thread (with a full Windows stack)
//! fed by a bounded channel so PS5 guest audio threads (e.g. `libfmod.prx`) never risk stack
//! overflow, COM/TLS corruption, or `WAVEHDR` use-after-free.
//! When `ORBIS_OFFSCREEN`, `ORBIS_HIDDEN`, or `ORBIS_HEADLESS` is set, audio stays silent
//! and paces cleanly to avoid burning CPU on audio threads.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

fn is_silent_mode() -> bool {
    std::env::var_os("ORBIS_OFFSCREEN").is_some()
        || std::env::var_os("ORBIS_HIDDEN").is_some()
        || std::env::var_os("ORBIS_HEADLESS").is_some()
}

fn lock_unpoison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

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
    reset: unsafe extern "system" fn(usize) -> u32,
    close: unsafe extern "system" fn(usize) -> u32,
    time_begin_period: Option<unsafe extern "system" fn(u32) -> u32>,
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
        let time_begin_period = GetProcAddress(h, b"timeBeginPeriod\0".as_ptr())
            .map(|f| std::mem::transmute::<_, unsafe extern "system" fn(u32) -> u32>(f));
        Some(WinMm {
            open: std::mem::transmute(GetProcAddress(h, b"waveOutOpen\0".as_ptr())?),
            prepare: std::mem::transmute(GetProcAddress(h, b"waveOutPrepareHeader\0".as_ptr())?),
            unprepare: std::mem::transmute(GetProcAddress(h, b"waveOutUnprepareHeader\0".as_ptr())?),
            write: std::mem::transmute(GetProcAddress(h, b"waveOutWrite\0".as_ptr())?),
            reset: std::mem::transmute(GetProcAddress(h, b"waveOutReset\0".as_ptr())?),
            close: std::mem::transmute(GetProcAddress(h, b"waveOutClose\0".as_ptr())?),
            time_begin_period,
        })
    })
    .as_ref()
}

#[cfg(windows)]
fn ensure_high_res_timer() {
    static SET: OnceLock<()> = OnceLock::new();
    SET.get_or_init(|| {
        if let Some(mm) = winmm() {
            if let Some(f) = mm.time_begin_period {
                unsafe {
                    let _ = f(1); // 1 ms Windows multimedia timer resolution
                }
            }
        }
    });
}

enum AudioCmd {
    Pcm(Vec<u8>),
    Close,
}

/// Spawns a dedicated native OS worker thread that exclusively owns the `winmm.dll`
/// `HWAVEOUT` handle and 16-slot `WaveHdr` ring buffer.
fn spawn_wave_worker(rate: u32, channels: u16, is_float: bool, buf_bytes: usize) -> Option<SyncSender<AudioCmd>> {
    #[cfg(not(windows))]
    {
        let _ = (rate, channels, is_float, buf_bytes);
        return None;
    }

    #[cfg(windows)]
    {
        if is_silent_mode() {
            return None;
        }
        let mm = winmm()?;
        ensure_high_res_timer();
        let (tx, rx) = mpsc::sync_channel::<AudioCmd>(16);
        let init_bytes = buf_bytes.max(1024);
        let spawned = std::thread::Builder::new()
            .name("orbis-audio".into())
            .spawn(move || {
                let bps: u16 = if is_float { 32 } else { 16 };
                let wf = WaveFormatEx {
                    w_format_tag: if is_float { 3 } else { 1 },
                    n_channels: channels,
                    n_samples_per_sec: rate,
                    n_avg_bytes_per_sec: rate * (channels as u32) * (bps as u32 / 8),
                    n_block_align: channels * (bps / 8),
                    w_bits_per_sample: bps,
                    cb_size: 0,
                };
                let mut wave = 0usize;
                if unsafe { (mm.open)(&mut wave, 0xFFFF_FFFF, &wf, 0, 0, 0) } != 0 || wave == 0 {
                    return;
                }

                const SLOTS: usize = 16;
                const WHDR_DONE: u32 = 1;
                const WHDR_PREPARED: u32 = 2;
                let mut ring: Vec<(Box<WaveHdr>, Vec<u8>)> = Vec::with_capacity(SLOTS);
                for _ in 0..SLOTS {
                    let mut data = vec![0u8; init_bytes];
                    let hdr = Box::new(WaveHdr {
                        lp_data: data.as_mut_ptr(),
                        dw_buffer_length: init_bytes as u32,
                        dw_bytes_recorded: 0,
                        dw_user: 0,
                        dw_flags: 0,
                        dw_loops: 0,
                        lp_next: std::ptr::null_mut(),
                        reserved: 0,
                    });
                    ring.push((hdr, data));
                }
                let mut cursor = 0usize;

                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        AudioCmd::Close => break,
                        AudioCmd::Pcm(src) => {
                            if src.is_empty() {
                                continue;
                            }
                            let mut chosen = None;
                            for offset in 0..SLOTS {
                                let idx = (cursor + offset) % SLOTS;
                                let flags = unsafe { std::ptr::read_volatile(&ring[idx].0.dw_flags) };
                                if (flags & WHDR_PREPARED) == 0 || (flags & WHDR_DONE) != 0 {
                                    chosen = Some(idx);
                                    break;
                                }
                            }
                            let Some(slot) = chosen else {
                                continue;
                            };
                            cursor = (slot + 1) % SLOTS;
                            let (hdr, data) = &mut ring[slot];
                            let hdr_ptr: *mut WaveHdr = &mut **hdr;
                            unsafe {
                                if hdr.dw_flags & WHDR_PREPARED != 0 {
                                    if (mm.unprepare)(wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32) != 0 {
                                        continue;
                                    }
                                }
                                if data.len() < src.len() {
                                    data.resize(src.len(), 0);
                                }
                                std::ptr::copy_nonoverlapping(src.as_ptr(), data.as_mut_ptr(), src.len());
                                hdr.lp_data = data.as_mut_ptr();
                                hdr.dw_buffer_length = src.len() as u32;
                                hdr.dw_bytes_recorded = 0;
                                hdr.dw_flags = 0;
                                hdr.dw_loops = 0;
                                if (mm.prepare)(wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32) == 0 {
                                    if (mm.write)(wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32) != 0 {
                                        let _ = (mm.unprepare)(wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32);
                                        hdr.dw_flags = 0;
                                    }
                                }
                            }
                        }
                    }
                }

                // Clean teardown: reset stops DMA and marks all queued headers WHDR_DONE before unprepare/close.
                unsafe {
                    let _ = (mm.reset)(wave);
                    for (hdr, _) in &mut ring {
                        let hdr_ptr: *mut WaveHdr = &mut **hdr;
                        if (std::ptr::read_volatile(&hdr.dw_flags) & WHDR_PREPARED) != 0 {
                            let _ = (mm.unprepare)(wave, hdr_ptr, std::mem::size_of::<WaveHdr>() as u32);
                        }
                    }
                    let _ = (mm.close)(wave);
                }
            });
        if spawned.is_ok() {
            Some(tx)
        } else {
            None
        }
    }
}

/// Downmixes 8-channel (7.1 SMPTE: FL, FR, C, LFE, BL, BR, SL, SR) little-endian F32 PCM bytes
/// into clamped interleaved stereo S16 PCM (4 bytes/frame) without alignment assumptions.
fn downmix_71_f32_bytes_to_stereo_s16(src_bytes: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let frames = src_bytes.len() / 32;
    out.reserve(frames * 4);
    let read_f = |chunk: &[u8], idx: usize| -> f32 {
        let off = idx * 4;
        let v = f32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
        if v.is_finite() { v } else { 0.0 }
    };
    for ch in src_bytes.chunks_exact(32) {
        let fl = read_f(ch, 0);
        let fr = read_f(ch, 1);
        let c = read_f(ch, 2);
        let lfe = read_f(ch, 3);
        let bl = read_f(ch, 4);
        let br = read_f(ch, 5);
        let sl = read_f(ch, 6);
        let sr = read_f(ch, 7);
        let l = (fl + 0.7071 * (c + bl + sl) + 0.5 * lfe).clamp(-1.0, 1.0);
        let r = (fr + 0.7071 * (c + br + sr) + 0.5 * lfe).clamp(-1.0, 1.0);
        let ls = (l * 32767.0).round() as i16;
        let rs = (r * 32767.0).round() as i16;
        out.extend_from_slice(&ls.to_le_bytes());
        out.extend_from_slice(&rs.to_le_bytes());
    }
}

struct Port {
    tx: Option<SyncSender<AudioCmd>>,
    samples: u32,
    rate: u32,
    channels: u16,
    is_float: bool,
}

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
    let buf_bytes = (samples as usize) * (channels as usize) * (bps as usize / 8);
    let tx = spawn_wave_worker(rate, channels, is_float, buf_bytes);

    let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
    let idx = (handle - 1) as usize;
    let mut ports = lock_unpoison(&PORTS);
    if ports.len() <= idx {
        ports.resize_with(idx + 1, || None);
    }
    ports[idx] = Some(Port { tx, samples, rate, channels, is_float });
    eprintln!("[orbis] audio out opened: handle {handle} ({rate} Hz, {channels}ch, {}bit, {samples} samples)", bps);
    handle
}

/// sceAudioOutOutput(handle, ptr)
pub extern "sysv64" fn output(handle: i32, ptr: *const c_void) -> i32 {
    if handle <= 0 || ptr.is_null() {
        return 0;
    }
    let (samples, pace_ms) = {
        let ports = lock_unpoison(&PORTS);
        let Some(Some(p)) = ports.get((handle - 1) as usize) else {
            return 0;
        };
        let samples = p.samples as i32;
        let bytes = (p.samples as usize) * (p.channels as usize) * if p.is_float { 4 } else { 2 };
        if let Some(ref tx) = p.tx {
            if is_readable_range(ptr as *const u8, bytes) {
                let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, bytes) };
                let _ = tx.try_send(AudioCmd::Pcm(slice.to_vec()));
            }
            (samples, 0u64)
        } else {
            let ms = ((p.samples as u64) * 1000 / (p.rate.max(1) as u64)).clamp(4, 16);
            (samples, ms)
        }
    };
    if pace_ms > 0 {
        std::thread::sleep(Duration::from_millis(pace_ms));
    }
    samples
}

pub extern "sysv64" fn close(handle: i32) -> i32 {
    if handle <= 0 {
        return 0;
    }
    let mut ports = lock_unpoison(&PORTS);
    if let Some(Some(p)) = ports.get_mut((handle - 1) as usize).map(|x| x.take()) {
        if let Some(tx) = p.tx {
            let _ = tx.try_send(AudioCmd::Close);
        }
    }
    0
}

// ── PS5 libSceAudioOut2 (used by libfmod.prx) ────────────────────────

struct Audio2Context {
    sample_rate: u32,
    grain_samples: u32,
    next_tick: Option<Instant>,
}

struct Audio2Port {
    ctx: u64,
    port: Port,
    pending_pcm: Vec<u8>,
}

static A2_NEXT_CTX: AtomicI32 = AtomicI32::new(1);
static A2_NEXT_USER: AtomicI32 = AtomicI32::new(1);
static A2_NEXT_PORT: AtomicI32 = AtomicI32::new(1);
static A2_CONTEXTS: Mutex<Vec<Option<Audio2Context>>> = Mutex::new(Vec::new());
static A2_PORTS: Mutex<Vec<Option<Audio2Port>>> = Mutex::new(Vec::new());

#[repr(C)]
pub struct Audio2Attribute {
    attr_id: u32,
    _pad: u32,
    value: *const u8,
    value_size: usize,
}

#[cfg(windows)]
fn is_readable_range(ptr: *const u8, len: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS,
    };
    if ptr.is_null() || (ptr as usize) < 0x10000 || len == 0 {
        return false;
    }
    let mut mbi = std::mem::MaybeUninit::<MEMORY_BASIC_INFORMATION>::zeroed();
    let ok = unsafe {
        VirtualQuery(
            ptr as *const c_void,
            mbi.as_mut_ptr(),
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if ok == 0 {
        return false;
    }
    let mbi = unsafe { mbi.assume_init() };
    let start = ptr as usize;
    let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
    mbi.State == MEM_COMMIT
        && (mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD)) == 0
        && (mbi.Protect & 0xFF) != 0
        && start.saturating_add(len) <= region_end
}

#[cfg(windows)]
fn is_writable_range(ptr: *mut u8, len: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READWRITE,
        PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READWRITE, PAGE_WRITECOPY,
    };
    if ptr.is_null() || (ptr as usize) < 0x10000 || len == 0 {
        return false;
    }
    let mut mbi = std::mem::MaybeUninit::<MEMORY_BASIC_INFORMATION>::zeroed();
    let ok = unsafe {
        VirtualQuery(
            ptr as *const c_void,
            mbi.as_mut_ptr(),
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if ok == 0 {
        return false;
    }
    let mbi = unsafe { mbi.assume_init() };
    let start = ptr as usize;
    let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
    let writable = (mbi.Protect & (PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)) != 0;
    mbi.State == MEM_COMMIT
        && (mbi.Protect & PAGE_GUARD) == 0
        && writable
        && start.saturating_add(len) <= region_end
}

#[cfg(not(windows))]
fn is_readable_range(ptr: *const u8, len: usize) -> bool {
    !ptr.is_null() && (ptr as usize) >= 0x10000 && len > 0
}

#[cfg(not(windows))]
fn is_writable_range(ptr: *mut u8, len: usize) -> bool {
    !ptr.is_null() && (ptr as usize) >= 0x10000 && len > 0
}

pub extern "sysv64" fn a2_init() -> i32 {
    0
}

/// sceAudioOut2ContextResetParam(SceAudioOut2ContextParam* param)
pub extern "sysv64" fn a2_context_reset_param(param: *mut u8) -> i32 {
    if is_writable_range(param, 64) {
        unsafe {
            std::ptr::write_bytes(param, 0, 64);
        }
    }
    0
}

/// sceAudioOut2ContextQueryMemory(const SceAudioOut2ContextParam* param, size_t* outSize)
pub extern "sysv64" fn a2_context_query_memory(_param: *const u8, out_size: *mut usize) -> i32 {
    if is_writable_range(out_size as *mut u8, std::mem::size_of::<usize>()) {
        unsafe {
            *out_size = 0x10000; // 64 KiB context workspace
        }
    }
    0
}

/// sceAudioOut2ContextCreate(const SceAudioOut2ContextParam* param, void* mem, size_t memSize, SceAudioOut2ContextHandle* outHandle)
pub extern "sysv64" fn a2_context_create(
    _param: *const u8,
    _mem: *mut c_void,
    _mem_size: usize,
    out_handle: *mut u64,
) -> i32 {
    let handle = A2_NEXT_CTX.fetch_add(1, Ordering::SeqCst) as u64;
    let idx = (handle - 1) as usize;
    let mut ctxs = lock_unpoison(&A2_CONTEXTS);
    if ctxs.len() <= idx {
        ctxs.resize_with(idx + 1, || None);
    }
    ctxs[idx] = Some(Audio2Context {
        sample_rate: 48000,
        grain_samples: 256,
        next_tick: None,
    });
    if is_writable_range(out_handle as *mut u8, std::mem::size_of::<u64>()) {
        unsafe { *out_handle = handle };
    }
    0
}

/// sceAudioOut2ContextDestroy(SceAudioOut2ContextHandle ctx)
pub extern "sysv64" fn a2_context_destroy(ctx: u64) -> i32 {
    if ctx > 0 && ctx <= 4096 {
        if let Some(slot) = lock_unpoison(&A2_CONTEXTS).get_mut((ctx - 1) as usize) {
            *slot = None;
        }
        let mut ports = lock_unpoison(&A2_PORTS);
        for slot in ports.iter_mut() {
            if slot.as_ref().map(|p| p.ctx == ctx).unwrap_or(false) {
                if let Some(p) = slot.take() {
                    if let Some(tx) = p.port.tx {
                        let _ = tx.try_send(AudioCmd::Close);
                    }
                }
            }
        }
    }
    0
}

/// sceAudioOut2UserCreate(SceUserServiceUserId userId, SceAudioOut2UserHandle* outUser)
pub extern "sysv64" fn a2_user_create(_user_id: i32, out_user: *mut u64) -> i32 {
    let handle = A2_NEXT_USER.fetch_add(1, Ordering::SeqCst) as u64;
    if is_writable_range(out_user as *mut u8, std::mem::size_of::<u64>()) {
        unsafe { *out_user = handle };
    }
    0
}

/// sceAudioOut2UserDestroy(SceAudioOut2UserHandle user)
pub extern "sysv64" fn a2_user_destroy(_user: u64) -> i32 {
    0
}

/// sceAudioOut2PortCreate(SceAudioOut2ContextHandle ctx, const SceAudioOut2PortParam* param, SceAudioOut2PortHandle* outPort)
pub extern "sysv64" fn a2_port_create(ctx: u64, param: *const u8, out_port: *mut u64) -> i32 {
    let mut rate = 48000u32;
    let mut is_main_71 = true;
    if is_readable_range(param, 12) {
        let flag = unsafe { *(param.add(4) as *const u32) };
        let r = unsafe { *(param.add(8) as *const u32) };
        if (8000..=192000).contains(&r) {
            rate = r;
        }
        // Main 7.1 bus sets param+4 = 0x800 (2048); auxiliary/object ports use smaller values.
        is_main_71 = flag == 0 || flag == 0x800;
    }
    let channels = 2u16;
    let is_float = false; // Output clean stereo S16 PCM after 7.1 F32 -> stereo downmix
    let samples = 256u32;
    let buf_bytes = (samples as usize) * (channels as usize) * 2;
    let tx = if is_main_71 {
        spawn_wave_worker(rate, channels, is_float, buf_bytes)
    } else {
        None
    };

    let handle = A2_NEXT_PORT.fetch_add(1, Ordering::SeqCst) as u64;
    let idx = (handle - 1) as usize;
    let mut ports = lock_unpoison(&A2_PORTS);
    if ports.len() <= idx {
        ports.resize_with(idx + 1, || None);
    }
    ports[idx] = Some(Audio2Port {
        ctx,
        port: Port { tx, samples, rate, channels, is_float },
        pending_pcm: Vec::with_capacity(buf_bytes),
    });
    if is_writable_range(out_port as *mut u8, std::mem::size_of::<u64>()) {
        unsafe { *out_port = handle };
    }
    eprintln!("[orbis] audio out2 port opened: handle {handle} (ctx {ctx}, {rate} Hz, 7.1->2ch S16, main={is_main_71})");
    0
}

/// sceAudioOut2PortDestroy(SceAudioOut2PortHandle port)
pub extern "sysv64" fn a2_port_destroy(port: u64) -> i32 {
    if port > 0 && port <= 4096 {
        let mut ports = lock_unpoison(&A2_PORTS);
        if let Some(Some(p)) = ports.get_mut((port - 1) as usize).map(|x| x.take()) {
            if let Some(tx) = p.port.tx {
                let _ = tx.try_send(AudioCmd::Close);
            }
        }
    }
    0
}

/// sceAudioOut2PortSetAttributes(SceAudioOut2PortHandle port, const SceAudioOut2Attribute* attrs, size_t count)
pub extern "sysv64" fn a2_port_set_attributes(port: u64, attrs: *const Audio2Attribute, count: usize) -> i32 {
    if port == 0 || port > 4096 || count == 0 || count > 64 {
        return 0;
    }
    if (attrs as usize) % std::mem::align_of::<Audio2Attribute>() != 0
        || !is_readable_range(attrs as *const u8, count * std::mem::size_of::<Audio2Attribute>())
    {
        return 0;
    }
    let mut ports = lock_unpoison(&A2_PORTS);
    let Some(Some(p)) = ports.get_mut((port - 1) as usize) else {
        return 0;
    };
    if p.port.tx.is_none() {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(attrs, count) };
    // libfmod.prx mixes 256 samples * 8 channels (7.1 F32) = 8192 bytes per grain at attr_id == 0.
    let f32_71_bytes = (p.port.samples as usize) * 8 * 4;
    for attr in slice {
        if attr.attr_id != 0 {
            continue;
        }
        if attr.value_size == 8 && is_readable_range(attr.value, 8) {
            let pcm_ptr = unsafe { std::ptr::read_unaligned(attr.value as *const *const u8) };
            if is_readable_range(pcm_ptr, f32_71_bytes) {
                let bytes = unsafe { std::slice::from_raw_parts(pcm_ptr, f32_71_bytes) };
                downmix_71_f32_bytes_to_stereo_s16(bytes, &mut p.pending_pcm);
            }
        }
    }
    0
}

/// sceAudioOut2ContextPush(SceAudioOut2ContextHandle ctx, uint32_t flags): queues staged PCM to the OS audio worker.
pub extern "sysv64" fn a2_context_push(ctx: u64, _flags: u32) -> i32 {
    if !is_silent_mode() {
        let mut ports = lock_unpoison(&A2_PORTS);
        for p in ports.iter_mut().flatten() {
            if (ctx == 0 || p.ctx == ctx) && !p.pending_pcm.is_empty() {
                if let Some(ref tx) = p.port.tx {
                    let pcm = std::mem::take(&mut p.pending_pcm);
                    let _ = tx.try_send(AudioCmd::Pcm(pcm));
                }
            }
        }
    }
    0
}

/// sceAudioOut2ContextAdvance(SceAudioOut2ContextHandle ctx): paces the FMOD audio thread at 1 grain
/// (256 samples @ 48 kHz = 5.33 ms), or 8 ms in offscreen/hidden/headless mode so background runs stay silent.
pub extern "sysv64" fn a2_context_advance(ctx: u64) -> i32 {
    let sleep_for = {
        let grain = if is_silent_mode() {
            Duration::from_millis(8)
        } else {
            Duration::from_micros(5333)
        };
        let now = Instant::now();
        let mut ctxs = lock_unpoison(&A2_CONTEXTS);
        if ctx > 0 && ctx <= 4096 {
            if let Some(Some(c)) = ctxs.get_mut((ctx - 1) as usize) {
                let step = if is_silent_mode() {
                    grain
                } else {
                    Duration::from_micros((c.grain_samples as u64) * 1_000_000 / (c.sample_rate.max(1) as u64))
                };
                match c.next_tick {
                    Some(t) if t > now => {
                        c.next_tick = Some(t + step);
                        t.saturating_duration_since(now)
                    }
                    Some(t) if now.saturating_duration_since(t) < Duration::from_millis(40) => {
                        c.next_tick = Some(t + step);
                        Duration::ZERO
                    }
                    _ => {
                        c.next_tick = Some(now + step);
                        Duration::ZERO
                    }
                }
            } else {
                grain
            }
        } else {
            grain
        }
    };
    if !sleep_for.is_zero() {
        std::thread::sleep(sleep_for);
    }
    0
}

/// Legacy fallback pacing stub for unmapped AudioOut2 poll NIDs.
pub extern "sysv64" fn a2_pace() -> i32 {
    std::thread::sleep(Duration::from_millis(if is_silent_mode() { 8 } else { 5 }));
    0
}

pub extern "sysv64" fn ok() -> i32 {
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        // libSceAudioOut
        ("sceAudioOutInit", init as *const () as usize),
        ("sceAudioOutOpen", open as *const () as usize),
        ("sceAudioOutOutput", output as *const () as usize),
        ("sceAudioOutOutputs", ok as *const () as usize),
        ("sceAudioOutClose", close as *const () as usize),
        ("sceAudioOutSetVolume", ok as *const () as usize),
        ("sceAudioOutGetPortState", ok as *const () as usize),
        // PS5 libSceAudioOut2 (libfmod.prx)
        ("sceAudioOut2Initialize", a2_init as *const () as usize),
        ("sceAudioOut2Terminate", ok as *const () as usize),
        ("sceAudioOut2ContextResetParam", a2_context_reset_param as *const () as usize),
        ("sceAudioOut2ContextQueryMemory", a2_context_query_memory as *const () as usize),
        ("sceAudioOut2ContextCreate", a2_context_create as *const () as usize),
        ("sceAudioOut2ContextDestroy", a2_context_destroy as *const () as usize),
        ("sceAudioOut2UserCreate", a2_user_create as *const () as usize),
        ("sceAudioOut2UserDestroy", a2_user_destroy as *const () as usize),
        ("sceAudioOut2PortCreate", a2_port_create as *const () as usize),
        ("sceAudioOut2PortDestroy", a2_port_destroy as *const () as usize),
        ("sceAudioOut2PortSetAttributes", a2_port_set_attributes as *const () as usize),
        ("sceAudioOut2ContextAdvance", a2_context_advance as *const () as usize),
        ("sceAudioOut2ContextPush", a2_context_push as *const () as usize),
        ("#MmpF1XsQiHw", a2_pace as *const () as usize),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio2_lifecycle_matches_libfmod_abi_and_guards_writes() {
        std::env::set_var("ORBIS_HEADLESS", "1");
        assert_eq!(a2_init(), 0);

        let mut param = [0xAAu8; 64];
        assert_eq!(a2_context_reset_param(param.as_mut_ptr()), 0);
        assert_eq!(param, [0u8; 64]);

        let mut mem_and_canary = [0usize, 0xDEAD_BEEF_CAFE_BABEu64 as usize];
        assert_eq!(a2_context_query_memory(param.as_ptr(), mem_and_canary.as_mut_ptr()), 0);
        assert_eq!(mem_and_canary[0], 0x10000);
        assert_eq!(mem_and_canary[1], 0xDEAD_BEEF_CAFE_BABEu64 as usize);

        let mut ctx = 0u64;
        assert_eq!(a2_context_create(param.as_ptr(), std::ptr::null_mut(), mem_and_canary[0], &mut ctx), 0);
        assert!(ctx > 0);

        let mut user = 0u64;
        assert_eq!(a2_user_create(0xFF, &mut user), 0);
        assert!(user > 0);

        let mut port_param = [0u8; 64];
        port_param[8..12].copy_from_slice(&48000u32.to_le_bytes());
        let mut port = 0u64;
        assert_eq!(a2_port_create(ctx, port_param.as_ptr(), &mut port), 0);
        assert!(port > 0);

        let pcm = [0.0f32; 256 * 8];
        let pcm_ptr = pcm.as_ptr();
        let attr = Audio2Attribute {
            attr_id: 0,
            _pad: 0,
            value: (&pcm_ptr as *const *const f32) as *const u8,
            value_size: 8,
        };
        assert_eq!(a2_port_set_attributes(port, &attr, 1), 0);
        assert_eq!(a2_context_advance(ctx), 0);
        assert_eq!(a2_context_push(ctx, 1), 0);

        assert_eq!(a2_user_create(0xFF, 0xFFusize as *mut u64), 0);

        assert_eq!(a2_port_destroy(port), 0);
        assert_eq!(a2_user_destroy(user), 0);
        assert_eq!(a2_context_destroy(ctx), 0);
        std::env::remove_var("ORBIS_HEADLESS");
    }

    #[test]
    fn downmix_71_to_stereo_s16_preserves_channels_and_clamps() {
        let src: [f32; 32] = [
            0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            0.0,  0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            2.0, -2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            f32::NAN, f32::INFINITY, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        let mut bytes = Vec::with_capacity(src.len() * 4);
        for v in src {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        downmix_71_f32_bytes_to_stereo_s16(&bytes, &mut out);
        assert_eq!(out.len(), 16);
        let s: Vec<i16> = out.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(s[0], 16384);
        assert_eq!(s[1], -16384);
        assert!((s[2] - 23170).abs() <= 2);
        assert_eq!(s[2], s[3]);
        assert_eq!(s[4], 32767);
        assert_eq!(s[5], -32767);
        assert_eq!(s[6], 0);
        assert_eq!(s[7], 0);
    }
}
