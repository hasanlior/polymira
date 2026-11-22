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

pub extern "sysv64" fn zero() -> u64 {
    0
}

pub fn functions() -> Vec<(&'static str, usize)> {
    vec![
        ("#23LRUSvYu1M", init as *const () as usize),              // sceAgcInit
        ("#2JtWUUiYBXs", register_defaults as *const () as usize), // sceAgcGetRegisterDefaults2
        ("#wRbq6ZjNop4", register_defaults as *const () as usize), // sceAgcGetRegisterDefaults2Internal
        ("#BfBDZGbti7A", zero as *const () as usize),              // sceAgcGetIsTrinityMode
    ]
}
