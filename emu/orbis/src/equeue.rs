//! Event queues (sceKernel*Equeue, FreeBSD kqueue style): user events and video-out flips.

use std::sync::{Condvar, Mutex};

/// struct kevent: ident, filter, flags, fflags, data, udata (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Event {
    ident: u64,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: i64,
    udata: u64,
}

const EVFILT_USER: i16 = -11;
const EVFILT_VIDEO_OUT: i16 = -13;
const SCE_KERNEL_ERROR_ETIMEDOUT: i32 = 0x8002_003Cu32 as i32;
const SCE_KERNEL_ERROR_EBADF: i32 = 0x8002_0009u32 as i32;

pub struct Queue {
    /// Registered events (ident, filter, udata) and how many pending firings each has.
    registered: Mutex<Vec<(Event, u32)>>,
    signal: Condvar,
}

static QUEUES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn queue(eq: usize) -> Option<&'static Queue> {
    if QUEUES.lock().unwrap().contains(&eq) {
        Some(unsafe { &*(eq as *const Queue) })
    } else {
        None
    }
}

pub extern "sysv64" fn create(out: *mut usize, _name: *const u8) -> i32 {
    let q = Box::leak(Box::new(Queue { registered: Mutex::new(vec![]), signal: Condvar::new() })) as *mut Queue as usize;
    QUEUES.lock().unwrap().push(q);
    unsafe { *out = q };
    0
}

pub extern "sysv64" fn delete(eq: usize) -> i32 {
    QUEUES.lock().unwrap().retain(|&q| q != eq);
    0
}

fn add(eq: usize, ident: u64, filter: i16, udata: u64) -> i32 {
    let Some(q) = queue(eq) else { return SCE_KERNEL_ERROR_EBADF };
    let mut r = q.registered.lock().unwrap();
    r.retain(|(e, _)| !(e.ident == ident && e.filter == filter));
    r.push((Event { ident, filter, udata, ..Default::default() }, 0));
    0
}

pub extern "sysv64" fn add_user_event(eq: usize, id: i32) -> i32 {
    add(eq, id as u64, EVFILT_USER, 0)
}

pub extern "sysv64" fn add_user_event_edge(eq: usize, id: i32) -> i32 {
    add(eq, id as u64, EVFILT_USER, 0)
}

pub extern "sysv64" fn trigger_user_event(eq: usize, id: i32, udata: u64) -> i32 {
    let Some(q) = queue(eq) else { return SCE_KERNEL_ERROR_EBADF };
    let mut r = q.registered.lock().unwrap();
    for (e, fired) in r.iter_mut() {
        if e.filter == EVFILT_USER && e.ident == id as u64 {
            e.udata = udata;
            *fired = fired.saturating_add(1);
        }
    }
    q.signal.notify_all();
    0
}

pub extern "sysv64" fn delete_user_event(eq: usize, id: i32) -> i32 {
    if let Some(q) = queue(eq) {
        q.registered.lock().unwrap().retain(|(e, _)| !(e.filter == EVFILT_USER && e.ident == id as u64));
    }
    0
}

/// sceVideoOutAddFlipEvent(eq, handle, udata)
pub extern "sysv64" fn add_flip_event(eq: usize, _handle: i32, udata: u64) -> i32 {
    add(eq, 6, EVFILT_VIDEO_OUT, udata)
}

const EVFILT_GRAPHICS: i16 = -14;

/// sceAgcDriverAddEqEvent(eq, id, udata): GPU end-of-pipe notifications.
pub extern "sysv64" fn add_gpu_event(eq: usize, id: i32, udata: u64) -> i32 {
    if std::env::var_os("ORBIS_TRACE_WAIT").is_some() {
        eprintln!("[orbis] equeue add_gpu_event eq={eq:#x} id={id} udata={udata:#x}");
    }
    add(eq, id as u64, EVFILT_GRAPHICS, udata)
}

pub extern "sysv64" fn delete_gpu_event(eq: usize, id: i32) -> i32 {
    if let Some(q) = queue(eq) {
        q.registered.lock().unwrap().retain(|(e, _)| !(e.filter == EVFILT_GRAPHICS && e.ident == id as u64));
    }
    0
}

/// Fires GPU events with the given id (after a submitted command buffer completes).
pub fn trigger_gpu(id: u64) {
    for &eq in QUEUES.lock().unwrap().iter() {
        let q = unsafe { &*(eq as *const Queue) };
        let mut r = q.registered.lock().unwrap();
        for (e, fired) in r.iter_mut() {
            if e.filter == EVFILT_GRAPHICS && e.ident == id {
                *fired = fired.saturating_add(1);
            }
        }
        q.signal.notify_all();
    }
}

/// Called on every flip: fires each queue's flip event.
pub fn trigger_flip(arg: u64) {
    for &eq in QUEUES.lock().unwrap().iter() {
        let q = unsafe { &*(eq as *const Queue) };
        let mut r = q.registered.lock().unwrap();
        for (e, fired) in r.iter_mut() {
            if e.filter == EVFILT_VIDEO_OUT {
                e.data = arg as i64;
                *fired = fired.saturating_add(1);
            }
        }
        q.signal.notify_all();
    }
}

/// sceKernelWaitEqueue(eq, events, num, out, timeout_usec*)
pub extern "sysv64" fn wait(eq: usize, events: *mut Event, num: i32, out: *mut i32, timeout: *const u32) -> i32 {
    let Some(q) = queue(eq) else { return SCE_KERNEL_ERROR_EBADF };
    let deadline = if timeout.is_null() { None } else { Some(std::time::Instant::now() + std::time::Duration::from_micros(unsafe { *timeout } as u64)) };
    let mut r = q.registered.lock().unwrap();
    loop {
        let mut n = 0;
        for (e, fired) in r.iter_mut() {
            if *fired > 0 && n < num {
                unsafe { *events.add(n as usize) = *e };
                *fired -= 1;
                n += 1;
            }
        }
        if n > 0 {
            if !out.is_null() {
                unsafe { *out = n };
            }
            return 0;
        }
        match deadline {
            None => {
                if std::env::var_os("ORBIS_TRACE_WAIT").is_some() {
                    let ids: Vec<(i16, u64)> = r.iter().map(|(e, _)| (e.filter, e.ident)).collect();
                    eprintln!("[orbis] equeue wait blocking on {ids:?}");
                }
                r = q.signal.wait(r).unwrap();
            }
            Some(d) => {
                let now = std::time::Instant::now();
                if now >= d {
                    if !out.is_null() {
                        unsafe { *out = 0 };
                    }
                    return SCE_KERNEL_ERROR_ETIMEDOUT;
                }
                r = q.signal.wait_timeout(r, d - now).unwrap().0;
            }
        }
    }
}

pub extern "sysv64" fn event_id(e: *const Event) -> u64 {
    unsafe { (*e).ident }
}
pub extern "sysv64" fn event_filter(e: *const Event) -> i32 {
    unsafe { (*e).filter as i32 }
}
pub extern "sysv64" fn event_data(e: *const Event) -> i64 {
    unsafe { (*e).data }
}
pub extern "sysv64" fn event_user_data(e: *const Event) -> u64 {
    unsafe { (*e).udata }
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("sceKernelCreateEqueue", create as *const () as usize),
        ("sceKernelDeleteEqueue", delete as *const () as usize),
        ("sceKernelWaitEqueue", wait as *const () as usize),
        ("sceKernelAddUserEvent", add_user_event as *const () as usize),
        ("sceKernelAddUserEventEdge", add_user_event_edge as *const () as usize),
        ("sceKernelTriggerUserEvent", trigger_user_event as *const () as usize),
        ("sceKernelDeleteUserEvent", delete_user_event as *const () as usize),
        ("sceKernelGetEventId", event_id as *const () as usize),
        ("sceKernelGetEventFilter", event_filter as *const () as usize),
        ("sceKernelGetEventData", event_data as *const () as usize),
        ("sceKernelGetEventUserData", event_user_data as *const () as usize),
        ("#w2rJhmD+dsE", add_gpu_event as *const () as usize), // sceAgcDriverAddEqEvent
        ("sceAgcDriverAddEqEvent", add_gpu_event as *const () as usize),
        ("sceAgcDriverDeleteEqEvent", delete_gpu_event as *const () as usize),
        ("sceVideoOutGetEventId", event_id as *const () as usize),
        ("sceVideoOutGetEventData", event_data as *const () as usize),
    ]
}
