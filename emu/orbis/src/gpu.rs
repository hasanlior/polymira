//! PS5 graphics (libSceAgc): the start of the GPU layer.
//!
//! For now this accepts the game's set-up calls so it can continue; command buffers are not
//! executed yet. The next stage is translating AGC command buffers and shaders to Vulkan.

/// Register-defaults table the game searches by hash: entries at +0x30, count at +0x38.
#[repr(C)]
struct RegisterDefaults {
    header: [u64; 6],
    entries: usize,
    count: u32,
    pad: u32,
}

static mut DEFAULTS: RegisterDefaults = RegisterDefaults { header: [0; 6], entries: 0, count: 0, pad: 0 };
static mut DEFAULT_ENTRY: [u32; 3] = [0; 3];

/// sceAgcInit(state*, version): { version, 0 }.
pub extern "sysv64" fn init(state: *mut u32, version: u32) -> i32 {
    eprintln!("[orbis] GPU: AGC initialised (version {version:#x})");
    if !state.is_null() {
        unsafe {
            *state = version;
            *state.add(1) = 0;
        }
    }
    0
}

/// sceAgcGetRegisterDefaults2(version): an empty table (no overrides).
pub extern "sysv64" fn register_defaults(_version: u32) -> usize {
    unsafe {
        let d = std::ptr::addr_of_mut!(DEFAULTS);
        (*d).entries = std::ptr::addr_of!(DEFAULT_ENTRY) as usize;
        d as usize
    }
}

/// Shaders the game created: (header, code, type).
pub static SHADERS: std::sync::Mutex<Vec<(usize, usize, u8)>> = std::sync::Mutex::new(Vec::new());

/// Turns a self-relative pointer field into an absolute one (0 stays 0).
unsafe fn relocate(field: usize) {
    let p = field as *mut u64;
    let rel = *p;
    if rel != 0 {
        *p = field as u64 + rel;
    }
}

/// sceAgcCreateShader(out, header, code): the header's pointer fields are stored relative to
/// themselves; resolve them, record the code address, hand the header back as the shader.
pub extern "sysv64" fn create_shader(out: *mut usize, header: usize, code: usize) -> i32 {
    const INVALID: i32 = 0x8000_0001u32 as i32;
    if header == 0 || code == 0 {
        return INVALID;
    }
    unsafe {
        let magic = *(header as *const u32);
        if magic != 0x3433_3231 {
            eprintln!("[orbis] GPU: shader with unknown header {magic:#x}");
            return INVALID;
        }
        for off in [0x08, 0x18, 0x20, 0x28, 0x30, 0x38] {
            relocate(header + off);
        }
        *((header + 0x10) as *mut u64) = code as u64;
        let user = *((header + 0x08) as *const u64) as usize;
        if user != 0 {
            for off in [0, 8, 0x10, 0x18, 0x20] {
                relocate(user + off);
            }
        }
        let kind = *((header + 0x5A) as *const u8);
        SHADERS.lock().unwrap().push((header, code, kind));
        if !out.is_null() {
            *out = header;
        }
    }
    0
}

/// sceAgcDriverRegisterOwner(owner*, name): an id for a group of GPU resources.
pub extern "sysv64" fn register_owner(owner: *mut u32, _name: *const u8) -> i32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    if !owner.is_null() {
        unsafe { *owner = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst) };
    }
    0
}

/// sceAgcDriverRegisterResource(handle*, owner, addr, size, name, type, flags)
pub extern "sysv64" fn register_resource(handle: *mut u32, _owner: u32, _addr: usize, _size: usize) -> i32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    if !handle.is_null() {
        unsafe { *handle = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst) };
    }
    0
}

pub extern "sysv64" fn zero() -> u64 {
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("#23LRUSvYu1M", init as *const () as usize),              // sceAgcInit
        ("#2JtWUUiYBXs", register_defaults as *const () as usize), // sceAgcGetRegisterDefaults2
        ("#wRbq6ZjNop4", register_defaults as *const () as usize), // sceAgcGetRegisterDefaults2Internal
        ("#BfBDZGbti7A", zero as *const () as usize),              // sceAgcGetIsTrinityMode
        ("#f3dg2CSgRKY", create_shader as *const () as usize),     // sceAgcCreateShader
        ("#X-Nm5KLREeg", register_owner as *const () as usize),    // sceAgcDriverRegisterOwner
        ("#W5z4eZrjEas", register_resource as *const () as usize), // sceAgcDriverRegisterResource
        ("#ZLJk9r2+2Aw", zero as *const () as usize),              // sceAgcDriverUnregisterOwnerAndResources
        ("#pWLG7WOpVcw", zero as *const () as usize),              // sceAgcDriverUnregisterResource
    ]
}
