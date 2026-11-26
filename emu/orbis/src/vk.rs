//! Vulkan GPU backend for POLYMIRA Orbis (runs on the host GPU via `vulkan-1.dll`).
//!
//! Creates a Vulkan 1.2 device (preferring discrete GPUs), attaches a Win32 swapchain to
//! the Orbis window, manages offscreen render targets keyed by guest virtual address, and
//! executes graphics pipelines compiled from PS5 RDNA2 shaders.

use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::sync::{Mutex, OnceLock};

use ash::vk;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Vertex {
    pub pos: [f32; 4],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct DrawBatch {
    pub first_index: u32,
    pub index_count: u32,
    pub tex_addr: usize,
    pub tex_w: u32,
    pub tex_h: u32,
    pub tex_swiz: u32,
    /// Index into the frame's generic draws (CPU-run vertex shader + translated pixel shader).
    pub gen: Option<u32>,
}

/// Vertex of a generic draw: clip position plus the pixel shader's 8 interpolants.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GVertex {
    pub pos: [f32; 4],
    pub attr: [[f32; 4]; 8],
}

/// A draw whose pixel shader was translated to SPIR-V (`rdna::ps`).
#[derive(Clone)]
pub struct GenDraw {
    /// Pipeline cache key (pixel shader address + blend state).
    pub key: (usize, u32, u32),
    /// CB_TARGET_MASK bits for MRT0 (R, G, B, A).
    pub wmask: u32,
    pub spirv: std::sync::Arc<Vec<u32>>,
    pub blend: u32,
    pub first_vertex: u32,
    pub vertex_count: u32,
    /// When `index_count > 0`, draw indexed from `gibuf` (`first_index..first_index+index_count`).
    pub first_index: u32,
    pub index_count: u32,
    pub slots: [u32; 64],
    pub tex: Option<(usize, u32, u32, u32)>,
    /// Additional textures (bindings 1..3) for multi-texture pixel shaders (e.g. terrain + lightmap).
    pub extra_texs: [Option<(usize, u32, u32, u32)>; 3],
    /// Array layers of the texture (6 for a cube map, 1 otherwise).
    pub layers: u32,
    /// Colour target: guest address, width, height.
    pub rt: (usize, u32, u32),
    /// Scissor rectangle in the target: x, y, width, height.
    pub scissor: [i32; 4],
    /// The sampler descriptor (S#) the shader uses with the texture.
    pub samp: [u32; 4],
    /// Samplers for bindings 1..3.
    pub extra_samps: [[u32; 4]; 3],
    /// DB_DEPTH_CONTROL (bit 0 stencil, bit 1 test, bit 2 write, bits 4..6 zfunc, bits 8..10 sfunc).
    pub depth: u32,
    /// DB_DEPTH_CLEAR: value a cleared depth buffer starts with.
    pub zclear: f32,
    /// DB_STENCIL_CONTROL (cx 0x10B), DB_STENCILREFMASK (cx 0x10C), DB_STENCILREFMASK_BF (cx 0x10D).
    pub stencil_ctl: u32,
    pub stencil_ref: u32,
    pub stencil_ref_bf: u32,
    /// VGT_PRIMITIVE_TYPE (1=POINT_LIST, 2=LINE_LIST, 3=LINE_STRIP, 4=TRIANGLE_LIST, 5=TRIANGLE_STRIP).
    pub prim: u8,
    /// Optional direct guest index buffer `(addr, byte_len, is_32bit)` bound via VRAM `guest_ib_cache`.
    pub guest_ib: Option<(usize, usize, bool)>,
    /// Optional GPU-translated SPIR-V vertex shader (`rdna::vs::translate`).
    pub vs_spirv: Option<std::sync::Arc<Vec<u32>>>,
    /// Guest VA of the vertex shader (for runtime auto-heal quarantine if GPU-VS stalls/faults).
    pub vs_addr: usize,
    /// Byte offset (256-byte aligned) into per-frame `vs_ubuf` (`STORAGE_BUFFER_DYNAMIC`).
    pub vs_param_off: u32,
    /// Guest vertex buffers `(base_addr, byte_len)` for Set 1 bindings 1..=4.
    pub vs_vbs: [(usize, usize); 4],
}

const GVBUF_INIT_BYTES: usize = 32 << 20;
const GIBUF_INIT_BYTES: usize = 8 << 20;
const VS_UBUF_INIT_BYTES: usize = 4 << 20;

static READY_GIDXS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static READY_VS_PARAMS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static QUARANTINED_GPU_VS: std::sync::LazyLock<Mutex<std::collections::HashSet<usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
static HAS_QUARANTINED_GPU_VS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SAVED_WINDOW: Mutex<Option<(usize, usize, u32, u32)>> = Mutex::new(None);
static RENDER_MODE_LABEL: OnceLock<String> = OnceLock::new();

/// Returns a short badge describing the active hardware upscaler / FrameGen mode
/// (e.g. `"DLSS Quality | FG 3x"`, `"FSR Quality | FG 2x"`, or `""` when native).
pub fn render_mode_label() -> &'static str {
    RENDER_MODE_LABEL.get().map(|s| s.as_str()).unwrap_or("")
}

/// Detects hardware-appropriate Vulkan upscaler (DLSS/FSR via RCAS) and Optical-Flow FrameGen
/// settings from the GPU vendor/name and `ORBIS_UPSCALER`, `ORBIS_UPSCALE_QUALITY`, `ORBIS_FRAMEGEN`.
fn detect_render_mode(vendor_id: u32, gpu_name: &str) -> (f32, u32, String) {
    let up_env = std::env::var("ORBIS_UPSCALER").unwrap_or_else(|_| "auto".into()).to_ascii_lowercase();
    let qual_env = std::env::var("ORBIS_UPSCALE_QUALITY").unwrap_or_else(|_| "quality".into()).to_ascii_lowercase();
    let fg_env = std::env::var("ORBIS_FRAMEGEN").unwrap_or_else(|_| "auto".into()).to_ascii_lowercase();

    let name_up = gpu_name.to_ascii_uppercase();
    let is_nvidia_rtx = vendor_id == 0x10DE && name_up.contains("RTX");
    let is_rtx_40_50 = is_nvidia_rtx
        && (name_up.contains("RTX 40")
            || name_up.contains("RTX 50")
            || name_up.contains(" 40")
            || name_up.contains(" 50"));
    let is_modern_gpu = matches!(vendor_id, 0x10DE | 0x1002 | 0x8086);

    let upscaler_tag = match up_env.as_str() {
        "off" | "none" | "0" => "",
        "dlss" => "DLSS",
        "fsr" => "FSR",
        _ => {
            if is_nvidia_rtx {
                "DLSS"
            } else {
                "FSR"
            }
        }
    };

    let (rcas_sharpness, qual_tag) = if upscaler_tag.is_empty() {
        (0.0f32, "")
    } else {
        match qual_env.as_str() {
            "balanced" => (0.65f32, "Balanced"),
            "performance" | "perf" => (0.55f32, "Perf"),
            "native" => (0.35f32, "Native"),
            _ => (0.75f32, "Quality"),
        }
    };

    let disable_fg = std::env::var_os("ORBIS_OFFSCREEN").is_some()
        || std::env::var_os("ORBIS_VK_DUMP").is_some()
        || std::env::var_os("ORBIS_HIDDEN").is_some();
    let fg_mult: u32 = if disable_fg {
        1
    } else {
        match fg_env.as_str() {
            "off" | "none" | "1x" | "0" => 1,
            "2x" | "2" => 2,
            "3x" | "3" => 3,
            _ => {
                if is_rtx_40_50 {
                    3
                } else if is_modern_gpu {
                    2
                } else {
                    1
                }
            }
        }
    };

    let mut parts: Vec<String> = Vec::new();
    if !upscaler_tag.is_empty() {
        parts.push(format!("{upscaler_tag} {qual_tag}"));
    }
    if fg_mult > 1 {
        parts.push(format!("FG {fg_mult}x"));
    }
    let label = parts.join(" | ");
    (rcas_sharpness, fg_mult, label)
}

unsafe fn choose_present_mode(
    surface_loader: &ash::khr::surface::Instance,
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> vk::PresentModeKHR {
    let modes = surface_loader
        .get_physical_device_surface_present_modes(physical, surface)
        .unwrap_or_default();
    if modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else if modes.contains(&vk::PresentModeKHR::FIFO_RELAXED) {
        vk::PresentModeKHR::FIFO_RELAXED
    } else {
        vk::PresentModeKHR::FIFO
    }
}

/// Quarantines a faulty GPU-VS shader at runtime so subsequent frames automatically fall back
/// to the CPU hoisted vertex shader without crashing or stalling the GPU.
pub fn quarantine_gpu_vs(addr: usize, reason: &str) {
    if addr == 0 {
        return;
    }
    let mut g = QUARANTINED_GPU_VS.lock().unwrap_or_else(|e| e.into_inner());
    if g.insert(addr) {
        HAS_QUARANTINED_GPU_VS.store(true, std::sync::atomic::Ordering::Release);
        eprintln!("[orbis] AUTO-HEAL: quarantined GPU-VS {addr:#x} ({reason}) -> falling back to CPU hoisted VS");
    }
}

#[inline(always)]
pub fn is_gpu_vs_quarantined(addr: usize) -> bool {
    if addr == 0 || !HAS_QUARANTINED_GPU_VS.load(std::sync::atomic::Ordering::Acquire) {
        return false;
    }
    QUARANTINED_GPU_VS.lock().unwrap_or_else(|e| e.into_inner()).contains(&addr)
}

/// Swaps the current frame's generic draw index buffer into the renderer handoff slot
/// (preserves capacity on both sides without touching `video.rs`).
pub fn swap_gen_indices(src: &mut Vec<u32>) {
    let mut g = READY_GIDXS.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::swap(&mut *g, src);
    src.clear();
}

/// Swaps the current frame's GPU-VS uniform parameter buffer into the renderer handoff slot.
pub fn swap_vs_params(src: &mut Vec<u32>) {
    let mut g = READY_VS_PARAMS.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::swap(&mut *g, src);
    src.clear();
}

pub struct VkTarget {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub width: u32,
    pub height: u32,
}

pub struct VkRenderer {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub cmd_pool: vk::CommandPool,
    pub cmd_buf: vk::CommandBuffer,
    pub fence: vk::Fence,
    pub surface_loader: ash::khr::surface::Instance,
    pub swapchain_loader: ash::khr::swapchain::Device,
    pub surface: vk::SurfaceKHR,
    pub swapchain: vk::SwapchainKHR,
    pub swap_images: Vec<vk::Image>,
    pub swap_views: Vec<vk::ImageView>,
    pub swap_fbs: Vec<vk::Framebuffer>,
    pub swap_extent: vk::Extent2D,
    pub swap_format: vk::Format,
    pub depth_format: vk::Format,
    pub image_avail: vk::Semaphore,
    pub render_done: vk::Semaphore,
    pub mem_props: vk::PhysicalDeviceMemoryProperties,
    pub targets: HashMap<usize, VkTarget>,
    pub pipelines: HashMap<(usize, usize), (vk::PipelineLayout, vk::Pipeline, vk::RenderPass)>,
    pub blit_ready: bool,
    pub blit_rp: vk::RenderPass,
    pub swap_rp: vk::RenderPass,
    pub blit_layout: vk::PipelineLayout,
    pub blit_pipe: vk::Pipeline,
    pub quad_pipe: vk::Pipeline,
    pub blit_dset_layout: vk::DescriptorSetLayout,
    pub sampler: vk::Sampler,
    pub desc_pool: vk::DescriptorPool,
    pub desc_set: vk::DescriptorSet,
    pub staging_buf: vk::Buffer,
    pub staging_mem: vk::DeviceMemory,
    pub staging_cap: usize,
    pub vbuf: vk::Buffer,
    pub vmem: vk::DeviceMemory,
    pub ibuf: vk::Buffer,
    pub imem: vk::DeviceMemory,
    pub upload_tex: Option<VkTarget>,
    pub tex_cache: HashMap<usize, (VkTarget, vk::DescriptorSet)>,
    pub tex_sums: HashMap<usize, u64>,
    pub array_cache: HashMap<usize, (VkTarget, vk::DescriptorSet, u64)>,
    pub rt_rp_clear: vk::RenderPass,
    pub rt_rp_load: vk::RenderPass,
    pub rt_fbs: HashMap<usize, vk::Framebuffer>,
    pub rt_dsets: HashMap<usize, vk::DescriptorSet>,
    pub rt_layouts: HashMap<usize, vk::ImageLayout>,
    pub tex_checked: HashMap<usize, u64>,
    pub frame_no: u64,
    pub samplers: HashMap<u32, vk::Sampler>,
    pub rt_depth: HashMap<usize, (vk::Image, vk::DeviceMemory, vk::ImageView)>,
    pub rt_last_used: HashMap<usize, u64>,
    pub persistent_rts: std::collections::HashSet<usize>,
    pub rt_guest_sums: HashMap<usize, u64>,
    pub frame_seq: u64,
    pub rt_quad_pipe: vk::Pipeline,
    pub view_dsets: HashMap<(u64, u64), vk::DescriptorSet>,
    pub gen_dset_layout: vk::DescriptorSetLayout,
    pub gen_dsets: HashMap<[(u64, u64); 4], vk::DescriptorSet>,
    pub gen_layout: vk::PipelineLayout,
    pub gen_vs: vk::ShaderModule,
    pub gen_pipes: HashMap<(usize, u32, u32), vk::Pipeline>,
    pub gvbuf: vk::Buffer,
    pub gvmem: vk::DeviceMemory,
    pub gvbuf_cap: usize,
    pub gibuf: vk::Buffer,
    pub gimem: vk::DeviceMemory,
    pub gibuf_cap: usize,
    // GPU-VS offload resources (Set 1: dynamic VS params + 4 cached guest VB storage buffers).
    pub vs_dset_layout: vk::DescriptorSetLayout,
    pub gpu_vs_layout: vk::PipelineLayout,
    pub vs_ubuf: vk::Buffer,
    pub vs_umem: vk::DeviceMemory,
    pub vs_ubuf_cap: usize,
    pub vs_dummy_buf: vk::Buffer,
    pub vs_dummy_mem: vk::DeviceMemory,
    pub vs_modules: HashMap<usize, vk::ShaderModule>,
    /// Guest VB cache: addr -> (buf, mem, cap, uploaded_len, last_checksum, last_frame_used).
    pub guest_vb_cache: HashMap<usize, (vk::Buffer, vk::DeviceMemory, usize, usize, u64, u64)>,
    pub guest_vb_checked: HashMap<usize, u64>,
    /// Guest IB cache: addr -> (buf, mem, cap, uploaded_len, last_checksum, last_frame_used).
    pub guest_ib_cache: HashMap<usize, (vk::Buffer, vk::DeviceMemory, usize, usize, u64, u64)>,
    pub guest_ib_checked: HashMap<usize, u64>,
    pub vs_dsets: HashMap<[u64; 4], vk::DescriptorSet>,
    /// Runtime self-healing state: GPU-VS shaders active on the in-flight frame, shaders proven
    /// healthy (<120ms GPU completion), and device-lost recovery flag.
    pub last_frame_gpu_vs: Vec<usize>,
    pub healthy_gpu_vs: std::collections::HashSet<usize>,
    pub device_lost: bool,
    /// Hardware-aware Vulkan upscaler (RCAS) & Optical-Flow FrameGen (2x/3x) state.
    pub rcas_sharpness: f32,
    pub fg_mult: u32,
    pub rcas_layout: vk::PipelineLayout,
    pub rcas_pipe: vk::Pipeline,
    pub fg_dset_layout: vk::DescriptorSetLayout,
    pub fg_layout: vk::PipelineLayout,
    pub fg_pipe: vk::Pipeline,
    pub fg_hist: [Option<VkTarget>; 2],
    pub fg_dsets: [vk::DescriptorSet; 2],
    pub fg_idx: usize,
    pub fg_has_prev: bool,
    pub fg_size: (u32, u32),
}

unsafe impl Send for VkRenderer {}

impl Drop for VkRenderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader.destroy_swapchain(self.swapchain, None);
                self.swapchain = vk::SwapchainKHR::null();
            }
            if self.surface != vk::SurfaceKHR::null() {
                self.surface_loader.destroy_surface(self.surface, None);
                self.surface = vk::SurfaceKHR::null();
            }
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

static RENDERER: OnceLock<Mutex<Option<VkRenderer>>> = OnceLock::new();

pub fn with_renderer<R>(f: impl FnOnce(&mut VkRenderer) -> R) -> Option<R> {
    let m = RENDERER.get()?;
    let mut g = m.lock().ok()?;
    #[cfg(windows)]
    if g.as_ref().map(|r| r.device_lost).unwrap_or(false) {
        if let Some((hwnd, hinst, w, h)) = *SAVED_WINDOW.lock().unwrap_or_else(|e| e.into_inner()) {
            *g = None;
            match unsafe { VkRenderer::new_win32(hwnd as *mut c_void, hinst as *mut c_void, w, h) } {
                Ok(r) => {
                    eprintln!("[orbis] AUTO-HEAL: Vulkan device & swapchain recreated cleanly after DEVICE_LOST!");
                    *g = Some(r);
                }
                Err(e) => {
                    eprintln!("[orbis] AUTO-HEAL: Vulkan device recovery failed ({e})");
                }
            }
        }
    }
    g.as_mut().map(f)
}

/// Initializes Vulkan on the given Win32 HWND (idempotent; no-op when ORBIS_HIDDEN=1 or no Vulkan).
#[cfg(windows)]
pub fn init_window(hwnd: *mut c_void, hinstance: *mut c_void, width: u32, height: u32) {
    if std::env::var_os("ORBIS_HIDDEN").is_some() || std::env::var_os("ORBIS_NO_VK").is_some() {
        return;
    }
    *SAVED_WINDOW.lock().unwrap_or_else(|e| e.into_inner()) = Some((hwnd as usize, hinstance as usize, width, height));
    let cell = RENDERER.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap();
    if guard.is_some() {
        return;
    }
    match unsafe { VkRenderer::new_win32(hwnd, hinstance, width, height) } {
        Ok(r) => {
            *guard = Some(r);
        }
        Err(e) => {
            eprintln!("[orbis] Vulkan unavailable ({e}), falling back to GDI");
        }
    }
}

#[cfg(not(windows))]
pub fn init_window(_hwnd: *mut c_void, _hinstance: *mut c_void, _width: u32, _height: u32) {}

impl VkRenderer {
    #[cfg(windows)]
    unsafe fn new_win32(hwnd: *mut c_void, hinstance: *mut c_void, width: u32, height: u32) -> Result<Self, String> {
        let entry = ash::Entry::load().map_err(|e| format!("load vulkan-1.dll: {e}"))?;
        let app_name = c"POLYMIRA Orbis";
        let app_info = vk::ApplicationInfo::default()
            .application_name(app_name)
            .engine_name(app_name)
            .api_version(vk::make_api_version(0, 1, 2, 0));

        let exts = [
            ash::khr::surface::NAME.as_ptr(),
            ash::khr::win32_surface::NAME.as_ptr(),
        ];
        let inst_ci = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&exts);
        let instance = entry.create_instance(&inst_ci, None).map_err(|e| format!("create_instance: {e}"))?;

        let win32_surface = ash::khr::win32_surface::Instance::new(&entry, &instance);
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let surf_ci = vk::Win32SurfaceCreateInfoKHR::default()
            .hinstance(hinstance as vk::HINSTANCE)
            .hwnd(hwnd as vk::HWND);
        let surface = win32_surface
            .create_win32_surface(&surf_ci, None)
            .map_err(|e| format!("create_win32_surface: {e}"))?;

        let pdevs = instance.enumerate_physical_devices().map_err(|e| format!("enumerate_physical_devices: {e}"))?;
        let mut chosen = None;
        for pdev in pdevs {
            let props = instance.get_physical_device_properties(pdev);
            let qprops = instance.get_physical_device_queue_family_properties(pdev);
            let mut gfx_q = None;
            for (idx, qp) in qprops.iter().enumerate() {
                let present = surface_loader
                    .get_physical_device_surface_support(pdev, idx as u32, surface)
                    .unwrap_or(false);
                if qp.queue_flags.contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE) && present {
                    gfx_q = Some(idx as u32);
                    break;
                }
            }
            if let Some(qf) = gfx_q {
                let score = match props.device_type {
                    vk::PhysicalDeviceType::DISCRETE_GPU => 100,
                    vk::PhysicalDeviceType::INTEGRATED_GPU => 50,
                    _ => 10,
                };
                if chosen.map(|(_, _, s)| score > s).unwrap_or(true) {
                    chosen = Some((pdev, qf, score));
                }
            }
        }
        let (physical, queue_family, _) = chosen.ok_or_else(|| "no Vulkan graphics+present GPU".to_string())?;
        let props = instance.get_physical_device_properties(physical);
        let gpu_name = CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy();
        let (rcas_sharpness, fg_mult, mode_label) = detect_render_mode(props.vendor_id, &gpu_name);
        let _ = RENDER_MODE_LABEL.set(mode_label.clone());
        eprintln!(
            "[orbis] Vulkan: using {gpu_name} (vendor={:#06x}, queue family {queue_family}) -> mode={} (rcas={rcas_sharpness:.2}, fg={}x)",
            props.vendor_id,
            if mode_label.is_empty() { "Native" } else { &mode_label },
            fg_mult
        );

        let prio = [1.0f32];
        let q_ci = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&prio)];
        let dev_exts = [ash::khr::swapchain::NAME.as_ptr()];
        let dev_ci = vk::DeviceCreateInfo::default()
            .queue_create_infos(&q_ci)
            .enabled_extension_names(&dev_exts);
        let device = instance.create_device(physical, &dev_ci, None).map_err(|e| format!("create_device: {e}"))?;
        let queue = device.get_device_queue(queue_family, 0);
        let mem_props = instance.get_physical_device_memory_properties(physical);

        let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);
        let caps = surface_loader
            .get_physical_device_surface_capabilities(physical, surface)
            .map_err(|e| format!("surface_caps: {e}"))?;
        let formats = surface_loader
            .get_physical_device_surface_formats(physical, surface)
            .map_err(|e| format!("surface_formats: {e}"))?;
        let fmt = formats
            .iter()
            .copied()
            .find(|f| f.format == vk::Format::B8G8R8A8_UNORM || f.format == vk::Format::R8G8B8A8_UNORM)
            .unwrap_or(formats[0]);
        let swap_extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let min_images = (caps.min_image_count + 1).min(if caps.max_image_count > 0 { caps.max_image_count } else { 3 });
        let present_mode = choose_present_mode(&surface_loader, physical, surface);
        let sc_ci = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(min_images)
            .image_format(fmt.format)
            .image_color_space(fmt.color_space)
            .image_extent(swap_extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(present_mode)
            .clipped(true);
        let swapchain = swapchain_loader
            .create_swapchain(&sc_ci, None)
            .map_err(|e| format!("create_swapchain: {e}"))?;
        let swap_images = swapchain_loader
            .get_swapchain_images(swapchain)
            .map_err(|e| format!("get_swapchain_images: {e}"))?;

        let pool_ci = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let cmd_pool = device.create_command_pool(&pool_ci, None).map_err(|e| format!("create_command_pool: {e}"))?;
        let alloc_ci = vk::CommandBufferAllocateInfo::default()
            .command_pool(cmd_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd_buf = device.allocate_command_buffers(&alloc_ci).map_err(|e| format!("alloc_cmd: {e}"))?[0];
        let fence = device
            .create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)
            .map_err(|e| format!("create_fence: {e}"))?;
        let image_avail = device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            .map_err(|e| format!("create_sem: {e}"))?;
        let render_done = device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            .map_err(|e| format!("create_sem: {e}"))?;

        let d32s8 = instance.get_physical_device_format_properties(physical, vk::Format::D32_SFLOAT_S8_UINT);
        let depth_format = if d32s8.optimal_tiling_features.contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT) {
            vk::Format::D32_SFLOAT_S8_UINT
        } else {
            vk::Format::D24_UNORM_S8_UINT
        };
        eprintln!("[orbis] Vulkan: swapchain {}x{} ({} images, {:?}, depth={:?})", swap_extent.width, swap_extent.height, swap_images.len(), fmt.format, depth_format);
        let mut r = Self {
            entry,
            instance,
            physical,
            device,
            queue,
            queue_family,
            cmd_pool,
            cmd_buf,
            fence,
            surface_loader,
            swapchain_loader,
            surface,
            swapchain,
            swap_images,
            swap_views: Vec::new(),
            swap_fbs: Vec::new(),
            swap_extent,
            swap_format: fmt.format,
            depth_format,
            image_avail,
            render_done,
            mem_props,
            targets: HashMap::new(),
            pipelines: HashMap::new(),
            blit_ready: false,
            blit_rp: vk::RenderPass::null(),
            swap_rp: vk::RenderPass::null(),
            blit_layout: vk::PipelineLayout::null(),
            blit_pipe: vk::Pipeline::null(),
            quad_pipe: vk::Pipeline::null(),
            blit_dset_layout: vk::DescriptorSetLayout::null(),
            sampler: vk::Sampler::null(),
            desc_pool: vk::DescriptorPool::null(),
            desc_set: vk::DescriptorSet::null(),
            staging_buf: vk::Buffer::null(),
            staging_mem: vk::DeviceMemory::null(),
            staging_cap: 0,
            vbuf: vk::Buffer::null(),
            vmem: vk::DeviceMemory::null(),
            ibuf: vk::Buffer::null(),
            imem: vk::DeviceMemory::null(),
            upload_tex: None,
            tex_cache: HashMap::new(),
            tex_sums: HashMap::new(),
            array_cache: HashMap::new(),
            rt_rp_clear: vk::RenderPass::null(),
            rt_rp_load: vk::RenderPass::null(),
            rt_fbs: HashMap::new(),
            rt_dsets: HashMap::new(),
            rt_layouts: HashMap::new(),
            tex_checked: HashMap::new(),
            frame_no: 0,
            samplers: HashMap::new(),
            rt_depth: HashMap::new(),
            rt_last_used: HashMap::new(),
            persistent_rts: std::collections::HashSet::new(),
            rt_guest_sums: HashMap::new(),
            frame_seq: 0,
            rt_quad_pipe: vk::Pipeline::null(),
            view_dsets: HashMap::new(),
            gen_dset_layout: vk::DescriptorSetLayout::null(),
            gen_dsets: HashMap::new(),
            gen_layout: vk::PipelineLayout::null(),
            gen_vs: vk::ShaderModule::null(),
            gen_pipes: HashMap::new(),
            gvbuf: vk::Buffer::null(),
            gvmem: vk::DeviceMemory::null(),
            gvbuf_cap: 0,
            gibuf: vk::Buffer::null(),
            gimem: vk::DeviceMemory::null(),
            gibuf_cap: 0,
            vs_dset_layout: vk::DescriptorSetLayout::null(),
            gpu_vs_layout: vk::PipelineLayout::null(),
            vs_ubuf: vk::Buffer::null(),
            vs_umem: vk::DeviceMemory::null(),
            vs_ubuf_cap: 0,
            vs_dummy_buf: vk::Buffer::null(),
            vs_dummy_mem: vk::DeviceMemory::null(),
            vs_modules: HashMap::new(),
            guest_vb_cache: HashMap::new(),
            guest_vb_checked: HashMap::new(),
            guest_ib_cache: HashMap::new(),
            guest_ib_checked: HashMap::new(),
            vs_dsets: HashMap::new(),
            last_frame_gpu_vs: Vec::new(),
            healthy_gpu_vs: std::collections::HashSet::new(),
            device_lost: false,
            rcas_sharpness,
            fg_mult,
            rcas_layout: vk::PipelineLayout::null(),
            rcas_pipe: vk::Pipeline::null(),
            fg_dset_layout: vk::DescriptorSetLayout::null(),
            fg_layout: vk::PipelineLayout::null(),
            fg_pipe: vk::Pipeline::null(),
            fg_hist: [None, None],
            fg_dsets: [vk::DescriptorSet::null(), vk::DescriptorSet::null()],
            fg_idx: 0,
            fg_has_prev: false,
            fg_size: (0, 0),
        };
        if let Err(e) = r.init_blit_pipeline() {
            eprintln!("[orbis] Vulkan: blit pipeline init warning: {e}");
        }
        Ok(r)
    }

    unsafe fn init_blit_pipeline(&mut self) -> Result<(), String> {
        let vs_code = crate::shader::spv_fullscreen_vs();
        let fs_code = crate::shader::spv_blit_fs();
        let vs_mod = self
            .device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vs_code), None)
            .map_err(|e| format!("vs module: {e}"))?;
        let fs_mod = self
            .device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&fs_code), None)
            .map_err(|e| format!("fs module: {e}"))?;

        let att = [vk::AttachmentDescription::default()
            .format(vk::Format::B8G8R8A8_UNORM)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let color_ref = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let rp = self
            .device
            .create_render_pass(&vk::RenderPassCreateInfo::default().attachments(&att).subpasses(&subpass), None)
            .map_err(|e| format!("render_pass: {e}"))?;

        let binding = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let dsl = self
            .device
            .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&binding), None)
            .map_err(|e| format!("dset_layout: {e}"))?;
        let dsls = [dsl];
        let pl = self
            .device
            .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls), None)
            .map_err(|e| format!("pipeline_layout: {e}"))?;

        let entry = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs_mod).name(entry),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs_mod).name(entry),
        ];
        let vi = vk::PipelineVertexInputStateCreateInfo::default();
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let cba = [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);

        let gp_ci = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .color_blend_state(&cb)
            .dynamic_state(&ds)
            .layout(pl)
            .render_pass(rp)];
        let pipe = self
            .device
            .create_graphics_pipelines(vk::PipelineCache::null(), &gp_ci, None)
            .map_err(|(_, e)| format!("graphics_pipeline: {e}"))?[0];

        self.device.destroy_shader_module(vs_mod, None);
        self.device.destroy_shader_module(fs_mod, None);
        let sampler = self
            .device
            .create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )
            .map_err(|e| format!("sampler: {e}"))?;
        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 65536,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: 65536,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER_DYNAMIC,
                descriptor_count: 16384,
            },
        ];
        let desc_pool = self
            .device
            .create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default().max_sets(49152).pool_sizes(&pool_sizes),
                None,
            )
            .map_err(|e| format!("desc_pool: {e}"))?;
        let dsls_alloc = [dsl];
        let desc_set = self
            .device
            .allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(desc_pool).set_layouts(&dsls_alloc))
            .map_err(|e| format!("desc_set: {e}"))?[0];

        self.blit_rp = rp;
        self.blit_layout = pl;
        self.blit_pipe = pipe;
        self.blit_dset_layout = dsl;
        self.sampler = sampler;
        self.desc_pool = desc_pool;
        self.desc_set = desc_set;

        // Swapchain render pass (transitions UNDEFINED -> PRESENT_SRC_KHR) + per-image framebuffers
        let s_att = [vk::AttachmentDescription::default()
            .format(self.swap_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
        let s_ref = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
        let s_sub = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&s_ref)];
        let swap_rp = self
            .device
            .create_render_pass(&vk::RenderPassCreateInfo::default().attachments(&s_att).subpasses(&s_sub), None)
            .map_err(|e| format!("swap_rp: {e}"))?;
        self.swap_rp = swap_rp;
        for &img in &self.swap_images {
            let view = self
                .device
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(img)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(self.swap_format)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        }),
                    None,
                )
                .map_err(|e| format!("swap_view: {e}"))?;
            let atts = [view];
            let fb = self
                .device
                .create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(swap_rp)
                        .attachments(&atts)
                        .width(self.swap_extent.width)
                        .height(self.swap_extent.height)
                        .layers(1),
                    None,
                )
                .map_err(|e| format!("swap_fb: {e}"))?;
            self.swap_views.push(view);
            self.swap_fbs.push(fb);
        }

        // Dynamic host-visible vertex (512 KB) + index (128 KB) buffers for indexed draws
        if let Some((vb, vm)) = self.alloc_host_buffer(524288, vk::BufferUsageFlags::VERTEX_BUFFER) {
            self.vbuf = vb;
            self.vmem = vm;
        }
        if let Some((ib, im)) = self.alloc_host_buffer(131072, vk::BufferUsageFlags::INDEX_BUFFER) {
            self.ibuf = ib;
            self.imem = im;
        }
        // Upload a 1x1 white fallback texture so desc_set binding 0 is always valid for quad_pipe.
        self.init_white_texture();

        self.quad_pipe = self.create_quad_pipeline(swap_rp, pl).unwrap_or(vk::Pipeline::null());
        self.init_post_pipelines(swap_rp, dsl);
        self.blit_ready = true;
        eprintln!(
            "[orbis] Vulkan: SPIR-V fullscreen blit + indexed quad + RCAS({}) + FrameGen({}) pipelines compiled on GPU (quad={}, fbs={})",
            self.rcas_pipe != vk::Pipeline::null(),
            self.fg_pipe != vk::Pipeline::null(),
            self.quad_pipe != vk::Pipeline::null(),
            self.swap_fbs.len()
        );
        Ok(())
    }

    unsafe fn init_post_pipelines(&mut self, swap_rp: vk::RenderPass, blit_dsl: vk::DescriptorSetLayout) {
        let vs_code = crate::shader::spv_fullscreen_vs();
        let Ok(vs_mod) = self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vs_code), None) else {
            return;
        };
        let entry = c"main";
        let vi = vk::PipelineVertexInputStateCreateInfo::default();
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let cba = [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
        let pc_ranges = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: 16,
        }];

        // 1. RCAS Contrast-Adaptive Sharpening / Upscale pipeline
        let rcas_code = crate::shader::spv_rcas_upscale_fs();
        if let Ok(rcas_fs) = self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&rcas_code), None) {
            let dsls = [blit_dsl];
            if let Ok(pl) = self.device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls).push_constant_ranges(&pc_ranges),
                None,
            ) {
                let stages = [
                    vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs_mod).name(entry),
                    vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(rcas_fs).name(entry),
                ];
                let gp_ci = [vk::GraphicsPipelineCreateInfo::default()
                    .stages(&stages)
                    .vertex_input_state(&vi)
                    .input_assembly_state(&ia)
                    .viewport_state(&vp)
                    .rasterization_state(&rs)
                    .multisample_state(&ms)
                    .color_blend_state(&cb)
                    .dynamic_state(&ds)
                    .layout(pl)
                    .render_pass(swap_rp)];
                if let Ok(pipes) = self.device.create_graphics_pipelines(vk::PipelineCache::null(), &gp_ci, None) {
                    self.rcas_layout = pl;
                    self.rcas_pipe = pipes[0];
                } else {
                    self.device.destroy_pipeline_layout(pl, None);
                }
            }
            self.device.destroy_shader_module(rcas_fs, None);
        }

        // 2. Optical-Flow FrameGen (2x/3x) pipeline (2 combined image samplers: prev + curr)
        let fg_bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        ];
        if let Ok(fg_dsl) = self.device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&fg_bindings),
            None,
        ) {
            let fg_code = crate::shader::spv_framegen_fs();
            if let Ok(fg_fs) = self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&fg_code), None) {
                let dsls = [fg_dsl];
                if let Ok(pl) = self.device.create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls).push_constant_ranges(&pc_ranges),
                    None,
                ) {
                    let stages = [
                        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs_mod).name(entry),
                        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fg_fs).name(entry),
                    ];
                    let gp_ci = [vk::GraphicsPipelineCreateInfo::default()
                        .stages(&stages)
                        .vertex_input_state(&vi)
                        .input_assembly_state(&ia)
                        .viewport_state(&vp)
                        .rasterization_state(&rs)
                        .multisample_state(&ms)
                        .color_blend_state(&cb)
                        .dynamic_state(&ds)
                        .layout(pl)
                        .render_pass(swap_rp)];
                    if let Ok(pipes) = self.device.create_graphics_pipelines(vk::PipelineCache::null(), &gp_ci, None) {
                        let alloc_dsls = [fg_dsl, fg_dsl];
                        if let Ok(sets) = self.device.allocate_descriptor_sets(
                            &vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&alloc_dsls),
                        ) {
                            self.fg_dset_layout = fg_dsl;
                            self.fg_layout = pl;
                            self.fg_pipe = pipes[0];
                            self.fg_dsets = [sets[0], sets[1]];
                        } else {
                            self.device.destroy_pipeline(pipes[0], None);
                            self.device.destroy_pipeline_layout(pl, None);
                            self.device.destroy_descriptor_set_layout(fg_dsl, None);
                        }
                    } else {
                        self.device.destroy_pipeline_layout(pl, None);
                        self.device.destroy_descriptor_set_layout(fg_dsl, None);
                    }
                } else {
                    self.device.destroy_descriptor_set_layout(fg_dsl, None);
                }
                self.device.destroy_shader_module(fg_fs, None);
            } else {
                self.device.destroy_descriptor_set_layout(fg_dsl, None);
            }
        }
        self.device.destroy_shader_module(vs_mod, None);
    }

    unsafe fn ensure_fg_history(&mut self, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 || self.fg_pipe == vk::Pipeline::null() {
            return false;
        }
        if self.fg_size == (width, height) && self.fg_hist[0].is_some() && self.fg_hist[1].is_some() {
            return true;
        }
        for slot in &mut self.fg_hist {
            if let Some(old) = slot.take() {
                self.device.destroy_image_view(old.view, None);
                self.device.destroy_image(old.image, None);
                self.device.free_memory(old.memory, None);
            }
        }
        self.fg_has_prev = false;
        self.fg_idx = 0;
        self.fg_size = (0, 0);
        let mut created: [Option<VkTarget>; 2] = [None, None];
        for slot in &mut created {
            let img_ci = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let Ok(image) = self.device.create_image(&img_ci, None) else {
                return false;
            };
            let req = self.device.get_image_memory_requirements(image);
            let Some(mi) = self.find_mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL) else {
                self.device.destroy_image(image, None);
                return false;
            };
            let Ok(memory) = self.device.allocate_memory(
                &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mi),
                None,
            ) else {
                self.device.destroy_image(image, None);
                return false;
            };
            if self.device.bind_image_memory(image, memory, 0).is_err() {
                self.device.destroy_image(image, None);
                self.device.free_memory(memory, None);
                return false;
            }
            let Ok(view) = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::B8G8R8A8_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }),
                None,
            ) else {
                self.device.destroy_image(image, None);
                self.device.free_memory(memory, None);
                return false;
            };
            *slot = Some(VkTarget { image, memory, view, width, height });
        }
        let v0 = created[0].as_ref().unwrap().view;
        let v1 = created[1].as_ref().unwrap().view;
        // fg_dsets[0]: curr=0 (binding 1), prev=1 (binding 0)
        // fg_dsets[1]: curr=1 (binding 1), prev=0 (binding 0)
        let pairs = [(self.fg_dsets[0], v1, v0), (self.fg_dsets[1], v0, v1)];
        for (ds, prev_v, curr_v) in pairs {
            let i0 = [vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(prev_v)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let i1 = [vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(curr_v)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(ds)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&i0),
                vk::WriteDescriptorSet::default()
                    .dst_set(ds)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&i1),
            ];
            self.device.update_descriptor_sets(&writes, &[]);
        }
        self.fg_hist = created;
        self.fg_size = (width, height);
        true
    }

    pub fn recreate_swapchain(&mut self, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 {
            return false;
        }
        unsafe {
            let _ = self.device.device_wait_idle();
            let Ok(caps) = self.surface_loader.get_physical_device_surface_capabilities(self.physical, self.surface) else {
                return false;
            };
            let swap_extent = if caps.current_extent.width != u32::MAX && caps.current_extent.height != u32::MAX {
                caps.current_extent
            } else {
                vk::Extent2D {
                    width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                    height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
                }
            };
            if swap_extent.width == 0 || swap_extent.height == 0 {
                return false;
            }
            let color_space = self
                .surface_loader
                .get_physical_device_surface_formats(self.physical, self.surface)
                .ok()
                .and_then(|fmts| fmts.iter().find(|f| f.format == self.swap_format).map(|f| f.color_space))
                .unwrap_or(vk::ColorSpaceKHR::SRGB_NONLINEAR);
            let min_images = (caps.min_image_count + 1).min(if caps.max_image_count > 0 { caps.max_image_count } else { 3 });
            let present_mode = choose_present_mode(&self.surface_loader, self.physical, self.surface);
            let old_swapchain = self.swapchain;
            let sc_ci = vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(min_images)
                .image_format(self.swap_format)
                .image_color_space(color_space)
                .image_extent(swap_extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(present_mode)
                .clipped(true)
                .old_swapchain(old_swapchain);
            let Ok(new_sc) = self.swapchain_loader.create_swapchain(&sc_ci, None) else {
                return false;
            };
            let Ok(new_images) = self.swapchain_loader.get_swapchain_images(new_sc) else {
                self.swapchain_loader.destroy_swapchain(new_sc, None);
                return false;
            };
            for fb in self.swap_fbs.drain(..) {
                self.device.destroy_framebuffer(fb, None);
            }
            for view in self.swap_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            self.swapchain_loader.destroy_swapchain(old_swapchain, None);
            self.swapchain = new_sc;
            self.swap_images = new_images;
            self.swap_extent = swap_extent;
            self.fg_has_prev = false;
            if self.swap_rp != vk::RenderPass::null() {
                for &img in &self.swap_images {
                    let Ok(view) = self.device.create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(img)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(self.swap_format)
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0,
                                level_count: 1,
                                base_array_layer: 0,
                                layer_count: 1,
                            }),
                        None,
                    ) else {
                        return false;
                    };
                    let atts = [view];
                    let Ok(fb) = self.device.create_framebuffer(
                        &vk::FramebufferCreateInfo::default()
                            .render_pass(self.swap_rp)
                            .attachments(&atts)
                            .width(self.swap_extent.width)
                            .height(self.swap_extent.height)
                            .layers(1),
                        None,
                    ) else {
                        self.device.destroy_image_view(view, None);
                        return false;
                    };
                    self.swap_views.push(view);
                    self.swap_fbs.push(fb);
                }
            }
            eprintln!(
                "[orbis] Vulkan: swapchain recreated {}x{} ({} images)",
                self.swap_extent.width,
                self.swap_extent.height,
                self.swap_images.len()
            );
            true
        }
    }

    unsafe fn acquire_swapchain_image(&mut self) -> Option<u32> {
        if let Some((w, h)) = crate::video::take_resize() {
            if w == 0 || h == 0 {
                return None;
            }
            if w != self.swap_extent.width || h != self.swap_extent.height {
                if !self.recreate_swapchain(w, h) {
                    return None;
                }
            }
        }
        let (cur_w, cur_h) = crate::video::window_size();
        if cur_w == 0 || cur_h == 0 {
            return None;
        }
        match self.swapchain_loader.acquire_next_image(self.swapchain, 100_000_000, self.image_avail, vk::Fence::null()) {
            Ok((idx, _suboptimal)) => Some(idx),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                let (w, h) = crate::video::window_size();
                if !self.recreate_swapchain(w, h) {
                    return None;
                }
                self.swapchain_loader
                    .acquire_next_image(self.swapchain, 100_000_000, self.image_avail, vk::Fence::null())
                    .ok()
                    .map(|(idx, _)| idx)
            }
            Err(e) => {
                if e == vk::Result::ERROR_DEVICE_LOST {
                    eprintln!("[orbis] AUTO-HEAL: acquire_next_image returned VK_ERROR_DEVICE_LOST -> scheduling live Vulkan recovery");
                    self.quarantine_active_gpu_vs("acquire DEVICE_LOST");
                    self.device_lost = true;
                } else {
                    static ACQ_ERR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    if ACQ_ERR.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
                        eprintln!("[orbis] Vulkan: acquire_next_image failed ({e:?})");
                    }
                }
                None
            }
        }
    }

    fn quarantine_active_gpu_vs(&mut self, reason: &str) {
        let suspects: Vec<usize> = self
            .last_frame_gpu_vs
            .iter()
            .copied()
            .filter(|a| !self.healthy_gpu_vs.contains(a))
            .collect();
        let targets = if suspects.is_empty() { &self.last_frame_gpu_vs } else { &suspects };
        for &addr in targets {
            quarantine_gpu_vs(addr, reason);
        }
    }

    unsafe fn present_swapchain_image(&mut self, img_idx: u32) {
        let sig_sems = [self.render_done];
        let scs = [self.swapchain];
        let idxs = [img_idx];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&sig_sems)
            .swapchains(&scs)
            .image_indices(&idxs);
        match self.swapchain_loader.queue_present(self.queue, &present) {
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                let (w, h) = crate::video::window_size();
                if w > 0 && h > 0 {
                    let _ = self.recreate_swapchain(w, h);
                }
            }
            _ => {}
        }
    }

    unsafe fn init_white_texture(&mut self) {
        if !self.ensure_staging(4) {
            return;
        }
        if let Ok(p) = self.device.map_memory(self.staging_mem, 0, 4, vk::MemoryMapFlags::empty()) {
            *(p as *mut u32) = 0xFFFF_FFFF;
            self.device.unmap_memory(self.staging_mem);
        }
        let Ok(img) = self.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D { width: 1, height: 1, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        ) else {
            return;
        };
        let req = self.device.get_image_memory_requirements(img);
        let Some(mi) = self.find_mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL) else {
            return;
        };
        let Ok(mem) = self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mi),
            None,
        ) else {
            return;
        };
        let _ = self.device.bind_image_memory(img, mem, 0);
        let Ok(view) = self.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(img)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                }),
            None,
        ) else {
            return;
        };
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
        let _ = self.device.reset_fences(&[self.fence]);
        let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
        let _ = self.device.begin_command_buffer(
            self.cmd_buf,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        );
        let sub = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let to_dst = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
        self.device.cmd_pipeline_barrier(
            self.cmd_buf,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &to_dst,
        );
        let region = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D { width: 1, height: 1, depth: 1 })];
        self.device.cmd_copy_buffer_to_image(self.cmd_buf, self.staging_buf, img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &region);
        let to_read = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)];
        self.device.cmd_pipeline_barrier(
            self.cmd_buf,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &to_read,
        );
        let _ = self.device.end_command_buffer(self.cmd_buf);
        let cmds = [self.cmd_buf];
        let submit = [vk::SubmitInfo::default().command_buffers(&cmds)];
        let _ = self.device.queue_submit(self.queue, &submit, self.fence);
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);

        let img_info = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(self.desc_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&img_info)];
        self.device.update_descriptor_sets(&write, &[]);
        self.upload_tex = Some(VkTarget { image: img, memory: mem, view, width: 1, height: 1 });
    }

    unsafe fn alloc_host_buffer(&self, bytes: usize, usage: vk::BufferUsageFlags) -> Option<(vk::Buffer, vk::DeviceMemory)> {
        let buf = self
            .device
            .create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(bytes as u64)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
            .ok()?;
        let req = self.device.get_buffer_memory_requirements(buf);
        let mi = self.find_mem_type(
            req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        let mem = self
            .device
            .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mi), None)
            .ok()?;
        self.device.bind_buffer_memory(buf, mem, 0).ok()?;
        Some((buf, mem))
    }

    #[cfg(windows)]
    fn readable_bytes(addr: usize, max_len: usize) -> usize {
        if addr < 0x1_0000_0000 || max_len == 0 {
            return 0;
        }
        thread_local! {
            static VK_REGIONS: std::cell::RefCell<[(usize, usize); 64]> = const { std::cell::RefCell::new([(0, 0); 64]) };
            static VK_NEXT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }
        if let Some(end) = VK_REGIONS.with(|r| {
            let arr = r.borrow();
            for &(lo, hi) in arr.iter() {
                if addr >= lo && addr < hi {
                    return Some(hi);
                }
            }
            None
        }) {
            return end.saturating_sub(addr).min(max_len);
        }
        unsafe {
            let mut mbi: windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if windows_sys::Win32::System::Memory::VirtualQuery(
                addr as *const c_void,
                &mut mbi,
                std::mem::size_of::<windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION>(),
            ) == 0
                || mbi.State != windows_sys::Win32::System::Memory::MEM_COMMIT
            {
                return 0;
            }
            let base = mbi.BaseAddress as usize;
            let end = base.saturating_add(mbi.RegionSize);
            VK_REGIONS.with(|r| {
                let mut arr = r.borrow_mut();
                let slot = VK_NEXT.with(|s| {
                    let i = s.get();
                    s.set((i + 1) & 63);
                    i
                });
                arr[slot] = (base, end);
            });
            end.saturating_sub(addr).min(max_len)
        }
    }

    #[cfg(not(windows))]
    fn readable_bytes(_addr: usize, _max_len: usize) -> usize {
        0
    }

    /// Universal Self-Healing 2.0 Auto Mip-0 Calibrator for PS5 Oberon GFX10.3 `SW_64KB_R_X`
    /// (`sw=27`) mipmapped 32bpp render-target atlases. Computes the exact mip-chain tile prefix
    /// (e.g. `2048x1024 -> 42 tiles = 0x2A0000`, `1024x1024 -> 21 tiles = 0x150000`,
    /// `2048x2048 -> 85 tiles = 0x550000`) and verifies non-zero occupancy at `addr + prefix`.
    unsafe fn detect_sw27_mip0_offset(addr: usize, w: u32, h: u32, want_src: usize) -> usize {
        if w < 256 || h < 256 || !w.is_power_of_two() || !h.is_power_of_two() {
            return 0;
        }
        let mut prefix_tiles = 0usize;
        let mut mw = w / 2;
        let mut mh = h / 2;
        while mw >= 128 && mh >= 128 {
            prefix_tiles += (((mw as usize) + 127) / 128) * (((mh as usize) + 127) / 128);
            mw /= 2;
            mh /= 2;
        }
        if prefix_tiles == 0 {
            return 0;
        }
        let off = prefix_tiles * 65536;
        let mip0_addr = addr.wrapping_add(off);
        if Self::readable_bytes(mip0_addr, want_src) < want_src {
            return 0;
        }
        // Sample 64 points across the full Mip-0 footprint (never only the last tile,
        // which can be empty on partially-filled atlases like Minecraft's 2048x1024!).
        let words = want_src / 8;
        let step = (words / 64).max(1);
        let p = mip0_addr as *const u64;
        let mut k = 0usize;
        while k < words {
            if p.add(k).read_unaligned() != 0 {
                return off;
            }
            k += step;
        }
        0
    }

    /// Untiles and uploads a PS5 GFX10 8/16/32-bpp texture to a cached VkImage + VkDescriptorSet.
    unsafe fn ensure_sampled_texture(&mut self, addr: usize, width: u32, height: u32, swiz: u32) -> vk::DescriptorSet {
        if addr == 0 || width == 0 || height == 0 || width > 4096 || height > 4096 {
            return self.desc_set;
        }
        // Memory is compared at most once per frame per texture.
        if self.tex_checked.get(&addr) == Some(&self.frame_no) {
            if let Some((t, ds)) = self.tex_cache.get(&addr) {
                if t.width == width && t.height == height {
                    return *ds;
                }
            }
        }
        self.tex_checked.insert(addr, self.frame_no);
        let sw_mode = swiz & 0x1F;
        let fmt = (swiz >> 8) & 0x1FF;
        let (bpp, tile_w, tile_h): (usize, usize, usize) = match fmt {
            1..=14 => (1, 256, 256),
            15..=31 => (2, 256, 128),
            _ => (4, 128, 128),
        };
        let want_src = if sw_mode == 0 {
            (width as usize) * (height as usize) * bpp
        } else {
            (((width as usize) + tile_w - 1) / tile_w) * (((height as usize) + tile_h - 1) / tile_h) * 65536
        };
        let avail = Self::readable_bytes(addr, want_src);
        if avail < 64 {
            return self.desc_set;
        }
        // The game may fill or reuse a texture after we first uploaded it: compare a sampled
        // checksum and upload again into the same image when it changed.
        // For small dynamic textures (e.g. 16x16 terrain lightmap), hash every word of the
        // actual pixel footprint so lighting updates are never missed!
        let sum = {
            let pix_bytes = (width as usize).saturating_mul(height as usize).saturating_mul(bpp);
            let hash_bytes = if width <= 64 && height <= 64 { avail.min(pix_bytes.max(64)) } else { avail };
            let words = hash_bytes / 8;
            let step = if bpp == 1 || (width <= 64 && height <= 64) { 1 } else { (words / 512).max(1) };
            let p = addr as *const u64;
            let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (width as u64) << 32 ^ height as u64 ^ ((swiz as u64) << 48);
            let mut k = 0;
            while k < words {
                h = (h ^ p.add(k).read_unaligned()).wrapping_mul(0x100_0000_01b3);
                k += step;
            }
            h
        };
        if let Some((t, ds)) = self.tex_cache.get(&addr) {
            if t.width == width && t.height == height {
                let (ds, img) = (*ds, t.image);
                if self.tex_sums.get(&addr) != Some(&sum) {
                    let src = std::slice::from_raw_parts(addr as *const u8, avail);
                    let pixels = crate::tiling::untile_32bpp(src, width, height, swiz);
                    self.reupload_texture(img, width, height, &pixels);
                    self.tex_sums.insert(addr, sum);
                }
                return ds;
            }
            // Same address reused at a different size: free old VkImage/VkDeviceMemory.
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if let Some((old, _)) = self.tex_cache.remove(&addr) {
                use ash::vk::Handle;
                let old_raw = old.view.as_raw();
                self.view_dsets.retain(|&(v, _), _| v != old_raw);
                self.device.destroy_image_view(old.view, None);
                self.device.destroy_image(old.image, None);
                self.device.free_memory(old.memory, None);
                self.tex_sums.remove(&addr);
            }
        }
        if self.tex_cache.len() >= 4096 {
            return self.desc_set;
        }
        let src = std::slice::from_raw_parts(addr as *const u8, avail);
        let pixels = crate::tiling::untile_32bpp(src, width, height, swiz);
        if let Ok(dir) = std::env::var("ORBIS_DUMP_TEX") {
            let _ = std::fs::create_dir_all(&dir);
            let mut bmp = Vec::with_capacity(54 + pixels.len() * 4);
            let sz = (54 + pixels.len() * 4) as u32;
            bmp.extend_from_slice(b"BM");
            bmp.extend_from_slice(&sz.to_le_bytes());
            bmp.extend_from_slice(&0u32.to_le_bytes());
            bmp.extend_from_slice(&54u32.to_le_bytes());
            bmp.extend_from_slice(&40u32.to_le_bytes());
            bmp.extend_from_slice(&(width as i32).to_le_bytes());
            bmp.extend_from_slice(&(-(height as i32)).to_le_bytes());
            bmp.extend_from_slice(&1u16.to_le_bytes());
            bmp.extend_from_slice(&32u16.to_le_bytes());
            bmp.extend_from_slice(&[0u8; 24]);
            for &p in &pixels {
                // RGBA -> BGRA for BMP
                let bgra = (p & 0xFF00_FF00) | ((p & 0xFF) << 16) | ((p >> 16) & 0xFF);
                bmp.extend_from_slice(&bgra.to_le_bytes());
            }
            let _ = std::fs::write(format!("{dir}/tex_{width}x{height}_sw{swiz}_{addr:x}.bmp"), &bmp);
        }
        let byte_len = pixels.len() * 4;
        if byte_len == 0 || !self.ensure_staging(byte_len) {
            return self.desc_set;
        }
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
        let Ok(mapped) = self.device.map_memory(self.staging_mem, 0, byte_len as u64, vk::MemoryMapFlags::empty()) else {
            return self.desc_set;
        };
        std::ptr::copy_nonoverlapping(pixels.as_ptr() as *const u8, mapped as *mut u8, byte_len);
        self.device.unmap_memory(self.staging_mem);

        let Ok(img) = self.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        ) else {
            return self.desc_set;
        };
        let req = self.device.get_image_memory_requirements(img);
        let Some(mi) = self.find_mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL) else {
            self.device.destroy_image(img, None);
            return self.desc_set;
        };
        let Ok(mem) = self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mi),
            None,
        ) else {
            self.device.destroy_image(img, None);
            return self.desc_set;
        };
        if self.device.bind_image_memory(img, mem, 0).is_err() {
            self.device.destroy_image(img, None);
            self.device.free_memory(mem, None);
            return self.desc_set;
        }
        let Ok(view) = self.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(img)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                }),
            None,
        ) else {
            self.device.destroy_image(img, None);
            self.device.free_memory(mem, None);
            return self.desc_set;
        };
        let dsls = [self.blit_dset_layout];
        let Ok(sets) = self
            .device
            .allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&dsls))
        else {
            self.device.destroy_image_view(view, None);
            self.device.destroy_image(img, None);
            self.device.free_memory(mem, None);
            return self.desc_set;
        };
        let dset = sets[0];

        let _ = self.device.reset_fences(&[self.fence]);
        let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
        let _ = self.device.begin_command_buffer(
            self.cmd_buf,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        );
        let sub = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let to_dst = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
        self.device.cmd_pipeline_barrier(
            self.cmd_buf,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &to_dst,
        );
        let region = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D { width, height, depth: 1 })];
        self.device.cmd_copy_buffer_to_image(self.cmd_buf, self.staging_buf, img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &region);
        let to_read = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)];
        self.device.cmd_pipeline_barrier(
            self.cmd_buf,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &to_read,
        );
        let _ = self.device.end_command_buffer(self.cmd_buf);
        let cmds = [self.cmd_buf];
        let submit = [vk::SubmitInfo::default().command_buffers(&cmds)];
        let _ = self.device.queue_submit(self.queue, &submit, self.fence);
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);

        let img_info = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(dset)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&img_info)];
        self.device.update_descriptor_sets(&write, &[]);

        eprintln!("[orbis] Vulkan: uploaded PS5 texture @{addr:#x} ({width}x{height}, swiz={sw_mode}, fmt={fmt}) to GPU");
        self.tex_cache.insert(addr, (VkTarget { image: img, memory: mem, view, width, height }, dset));
        self.tex_sums.insert(addr, sum);
        dset
    }

    /// A layered texture (cube map faces) sampled as a 2D array; faces are stored one after
    /// another. Re-uploaded when the memory changes.
    unsafe fn ensure_array_texture(&mut self, addr: usize, width: u32, height: u32, swiz: u32, layers: u32) -> vk::DescriptorSet {
        if addr == 0 || width == 0 || height == 0 || width > 4096 || height > 4096 || layers > 6 {
            return self.desc_set;
        }
        let sw_mode = swiz & 0x1F;
        let slice = if sw_mode == 0 {
            (width as usize) * (height as usize) * 4
        } else {
            (((width as usize) + 127) / 128) * (((height as usize) + 127) / 128) * 65536
        };
        let total = slice * layers as usize;
        if Self::readable_bytes(addr, total) < total {
            return self.desc_set;
        }
        let sum = {
            let words = total / 8;
            let step = (words / 2048).max(1);
            let p = addr as *const u64;
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            let mut k = 0;
            while k < words {
                h = (h ^ p.add(k).read_unaligned()).wrapping_mul(0x100_0000_01b3);
                k += step;
            }
            h
        };
        let existing = self.array_cache.get(&addr).map(|(t, ds, old)| (t.image, *ds, *old, t.width == width && t.height == height));
        if let Some((_, ds, old, true)) = existing {
            if old == sum {
                return ds;
            }
        }
        let mut pixels: Vec<u32> = Vec::with_capacity((width * height * layers) as usize);
        for l in 0..layers as usize {
            let src = std::slice::from_raw_parts((addr + l * slice) as *const u8, slice);
            pixels.extend(crate::tiling::untile_32bpp(src, width, height, swiz));
        }
        let (img, ds, fresh) = match existing {
            Some((img, ds, _, true)) => (img, ds, false),
            _ => {
                let Some((t, ds)) = self.create_array_image(width, height, layers) else { return self.desc_set };
                let img = t.image;
                self.array_cache.insert(addr, (t, ds, 0));
                (img, ds, true)
            }
        };
        self.upload_layers(img, width, height, layers, &pixels, fresh);
        if let Some(e) = self.array_cache.get_mut(&addr) {
            e.2 = sum;
        }
        eprintln!("[orbis] Vulkan: uploaded PS5 layered texture @{addr:#x} ({width}x{height}x{layers}, swiz={swiz})");
        ds
    }

    unsafe fn create_array_image(&mut self, width: u32, height: u32, layers: u32) -> Option<(VkTarget, vk::DescriptorSet)> {
        let img = self
            .device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(layers)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .ok()?;
        let req = self.device.get_image_memory_requirements(img);
        let mi = self.find_mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
        let mem = self.device.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mi), None).ok()?;
        self.device.bind_image_memory(img, mem, 0).ok()?;
        let range = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: layers };
        let view = self
            .device
            .create_image_view(
                &vk::ImageViewCreateInfo::default().image(img).view_type(vk::ImageViewType::TYPE_2D_ARRAY).format(vk::Format::R8G8B8A8_UNORM).subresource_range(range),
                None,
            )
            .ok()?;
        let dsls = [self.blit_dset_layout];
        let ds = self.device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&dsls)).ok()?[0];
        let img_info = [vk::DescriptorImageInfo::default().sampler(self.sampler).image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default().dst_set(ds).dst_binding(0).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&img_info)];
        self.device.update_descriptor_sets(&write, &[]);
        Some((VkTarget { image: img, memory: mem, view, width, height }, ds))
    }

    unsafe fn upload_layers(&mut self, img: vk::Image, width: u32, height: u32, layers: u32, pixels: &[u32], fresh: bool) {
        let byte_len = pixels.len() * 4;
        if byte_len < (width * height * 4 * layers) as usize || !self.ensure_staging(byte_len) {
            return;
        }
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
        let Ok(mapped) = self.device.map_memory(self.staging_mem, 0, byte_len as u64, vk::MemoryMapFlags::empty()) else { return };
        std::ptr::copy_nonoverlapping(pixels.as_ptr() as *const u8, mapped as *mut u8, byte_len);
        self.device.unmap_memory(self.staging_mem);
        let _ = self.device.reset_fences(&[self.fence]);
        let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
        let _ = self.device.begin_command_buffer(self.cmd_buf, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
        let sub = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: layers };
        let old = if fresh { vk::ImageLayout::UNDEFINED } else { vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL };
        let to_dst = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(old)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
        self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_dst);
        let region = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: layers })
            .image_extent(vk::Extent3D { width, height, depth: 1 })];
        self.device.cmd_copy_buffer_to_image(self.cmd_buf, self.staging_buf, img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &region);
        let to_read = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)];
        self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::DependencyFlags::empty(), &[], &[], &to_read);
        let _ = self.device.end_command_buffer(self.cmd_buf);
        let cmds = [self.cmd_buf];
        let submit = [vk::SubmitInfo::default().command_buffers(&cmds)];
        let _ = self.device.queue_submit(self.queue, &submit, self.fence);
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
    }

    /// Copies new pixels into an already sampled texture image.
    unsafe fn reupload_texture(&mut self, img: vk::Image, width: u32, height: u32, pixels: &[u32]) {
        let byte_len = pixels.len() * 4;
        if byte_len < (width * height * 4) as usize || !self.ensure_staging(byte_len) {
            return;
        }
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
        let Ok(mapped) = self.device.map_memory(self.staging_mem, 0, byte_len as u64, vk::MemoryMapFlags::empty()) else {
            return;
        };
        std::ptr::copy_nonoverlapping(pixels.as_ptr() as *const u8, mapped as *mut u8, byte_len);
        self.device.unmap_memory(self.staging_mem);
        let _ = self.device.reset_fences(&[self.fence]);
        let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
        let _ = self.device.begin_command_buffer(self.cmd_buf, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
        let sub = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
        let to_dst = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
        self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_dst);
        let region = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
            .image_extent(vk::Extent3D { width, height, depth: 1 })];
        self.device.cmd_copy_buffer_to_image(self.cmd_buf, self.staging_buf, img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &region);
        let to_read = [vk::ImageMemoryBarrier::default()
            .image(img)
            .subresource_range(sub)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)];
        self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::DependencyFlags::empty(), &[], &[], &to_read);
        let _ = self.device.end_command_buffer(self.cmd_buf);
        let cmds = [self.cmd_buf];
        let submit = [vk::SubmitInfo::default().command_buffers(&cmds)];
        let _ = self.device.queue_submit(self.queue, &submit, self.fence);
        let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
    }

    /// Renders per-draw indexed batches (each with its own PS5 texture) directly into the swapchain framebuffer on the RTX 4060.
    pub fn present_indexed_draws(
        &mut self,
        clear_rgba: [f32; 4],
        vertices: &[Vertex],
        indices: &[u32],
        batches: &[DrawBatch],
        gverts: &[GVertex],
        gens: &[GenDraw],
    ) -> bool {
        if !self.blit_ready || self.quad_pipe == vk::Pipeline::null() || self.vbuf == vk::Buffer::null() || self.ibuf == vk::Buffer::null() {
            return false;
        }
        let v_bytes = (vertices.len() * std::mem::size_of::<Vertex>()).min(524288);
        let i_bytes = (indices.len() * std::mem::size_of::<u32>()).min(131072);
        let max_idx_count = (i_bytes / 4) as u32;
        if (v_bytes == 0 || max_idx_count == 0) && gens.is_empty() {
            return false;
        }
        let g_bytes = (gverts.len() * std::mem::size_of::<GVertex>()).min(self.gvbuf_cap.max(GVBUF_INIT_BYTES));
        let g_max = (g_bytes / std::mem::size_of::<GVertex>()) as u32;
        unsafe {
            if !gens.is_empty() && self.gvbuf == vk::Buffer::null() {
                if let Some((b, m)) = self.alloc_host_buffer(GVBUF_INIT_BYTES, vk::BufferUsageFlags::VERTEX_BUFFER) {
                    self.gvbuf = b;
                    self.gvmem = m;
                    self.gvbuf_cap = GVBUF_INIT_BYTES;
                }
            }
            // Pre-upload newly seen PS5 textures and build generic pipelines before beginning the
            // swapchain render pass. A generic item carries its pipeline (non-null) and GenDraw index.
            let mut resolved: Vec<(u32, u32, vk::DescriptorSet, vk::Pipeline, usize)> = Vec::with_capacity(batches.len().max(1));
            if batches.is_empty() {
                resolved.push((0, max_idx_count, self.desc_set, vk::Pipeline::null(), 0));
            } else {
                for b in batches {
                    if let Some(g) = b.gen {
                        let Some(gd) = gens.get(g as usize) else { continue };
                        if self.gvbuf == vk::Buffer::null() || gd.first_vertex.saturating_add(gd.vertex_count) > g_max || gd.vertex_count == 0 {
                            continue;
                        }
                        let pipe = self.ensure_gen_pipeline(gd);
                        if pipe == vk::Pipeline::null() {
                            continue;
                        }
                        let ds = match gd.tex {
                            Some((a, w, h, sw)) if gd.layers > 1 => self.ensure_array_texture(a, w, h, sw, gd.layers),
                            Some((a, w, h, sw)) => self.ensure_sampled_texture(a, w, h, sw),
                            None => self.desc_set,
                        };
                        resolved.push((gd.first_vertex, gd.vertex_count, ds, pipe, g as usize));
                    } else if b.first_index.saturating_add(b.index_count) <= max_idx_count && b.index_count > 0 {
                        let ds = self.ensure_sampled_texture(b.tex_addr, b.tex_w, b.tex_h, b.tex_swiz);
                        resolved.push((b.first_index, b.index_count, ds, vk::Pipeline::null(), 0));
                    }
                }
            }
            if resolved.is_empty() {
                return false;
            }

            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if g_bytes > 0 && self.gvbuf != vk::Buffer::null() {
                if let Ok(gp) = self.device.map_memory(self.gvmem, 0, g_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(gverts.as_ptr() as *const u8, gp as *mut u8, g_bytes);
                    self.device.unmap_memory(self.gvmem);
                }
            }
            if v_bytes > 0 {
                if let Ok(vp) = self.device.map_memory(self.vmem, 0, v_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(vertices.as_ptr() as *const u8, vp as *mut u8, v_bytes);
                    self.device.unmap_memory(self.vmem);
                }
            }
            if i_bytes > 0 {
                if let Ok(ip) = self.device.map_memory(self.imem, 0, i_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(indices.as_ptr() as *const u8, ip as *mut u8, i_bytes);
                    self.device.unmap_memory(self.imem);
                }
            }
            let Some(img_idx) = self.acquire_swapchain_image() else {
                return false;
            };
            let _ = self.device.reset_fences(&[self.fence]);
            let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
            let _ = self.device.begin_command_buffer(
                self.cmd_buf,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            );
            let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: clear_rgba } }];
            let rp_bi = vk::RenderPassBeginInfo::default()
                .render_pass(self.swap_rp)
                .framebuffer(self.swap_fbs[img_idx as usize])
                .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.swap_extent })
                .clear_values(&clear);
            self.device.cmd_begin_render_pass(self.cmd_buf, &rp_bi, vk::SubpassContents::INLINE);
            self.device.cmd_bind_pipeline(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.quad_pipe);
            let vp = [vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: self.swap_extent.width as f32,
                height: self.swap_extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            }];
            let sc = [vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.swap_extent }];
            self.device.cmd_set_viewport(self.cmd_buf, 0, &vp);
            self.device.cmd_set_scissor(self.cmd_buf, 0, &sc);
            self.device.cmd_bind_vertex_buffers(self.cmd_buf, 0, &[self.vbuf], &[0]);
            self.device.cmd_bind_index_buffer(self.cmd_buf, self.ibuf, 0, vk::IndexType::UINT32);
            let mut last_ds = vk::DescriptorSet::null();
            let mut cur_pipe = self.quad_pipe;
            for &(first, cnt, ds, pipe, g) in &resolved {
                let generic = pipe != vk::Pipeline::null();
                let want = if generic { pipe } else { self.quad_pipe };
                if want != cur_pipe {
                    self.device.cmd_bind_pipeline(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, want);
                    if generic != (cur_pipe != self.quad_pipe) {
                        let vb = if generic { self.gvbuf } else { self.vbuf };
                        self.device.cmd_bind_vertex_buffers(self.cmd_buf, 0, &[vb], &[0]);
                        last_ds = vk::DescriptorSet::null();
                    }
                    cur_pipe = want;
                }
                let layout = if generic { self.gen_layout } else { self.blit_layout };
                if ds != last_ds {
                    self.device.cmd_bind_descriptor_sets(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, layout, 0, &[ds], &[]);
                    last_ds = ds;
                }
                if generic {
                    let slots = &gens[g].slots;
                    let bytes = std::slice::from_raw_parts(slots.as_ptr() as *const u8, 256);
                    self.device.cmd_push_constants(self.cmd_buf, self.gen_layout, vk::ShaderStageFlags::FRAGMENT, 0, bytes);
                    self.device.cmd_draw(self.cmd_buf, cnt, 1, first, 0);
                } else {
                    self.device.cmd_draw_indexed(self.cmd_buf, cnt, 1, first, 0, 0);
                }
            }
            self.device.cmd_end_render_pass(self.cmd_buf);
            // ORBIS_VK_DUMP=<dir>: copy every 120th presented frame back for checking.
            static DUMP_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dump_dir = std::env::var("ORBIS_VK_DUMP").ok();
            let n = DUMP_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (ew, eh) = (self.swap_extent.width, self.swap_extent.height);
            let dump = dump_dir.is_some() && (n % 120 == 60 || n % 120 == 61) && self.ensure_staging((ew * eh * 4) as usize);
            if dump {
                let img = self.swap_images[img_idx as usize];
                let range = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
                let to_src = [vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .image(img)
                    .subresource_range(range)];
                self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_src);
                let region = [vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                    .image_extent(vk::Extent3D { width: ew, height: eh, depth: 1 })];
                self.device.cmd_copy_image_to_buffer(self.cmd_buf, img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, self.staging_buf, &region);
                let to_present = [vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                    .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .image(img)
                    .subresource_range(range)];
                self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[], &to_present);
            }
            let _ = self.device.end_command_buffer(self.cmd_buf);

            let wait_sems = [self.image_avail];
            let sig_sems = [self.render_done];
            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let cmds = [self.cmd_buf];
            let submit = [vk::SubmitInfo::default()
                .wait_semaphores(&wait_sems)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&cmds)
                .signal_semaphores(&sig_sems)];
            if self.device.queue_submit(self.queue, &submit, self.fence).is_err() {
                return false;
            }
            self.present_swapchain_image(img_idx);
            if dump {
                let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                let bytes = (ew * eh * 4) as usize;
                if let Ok(p) = self.device.map_memory(self.staging_mem, 0, bytes as u64, vk::MemoryMapFlags::empty()) {
                    let px = std::slice::from_raw_parts(p as *const u8, bytes);
                    let mut bmp = Vec::with_capacity(54 + bytes);
                    bmp.extend_from_slice(b"BM");
                    bmp.extend_from_slice(&((54 + bytes) as u32).to_le_bytes());
                    bmp.extend_from_slice(&0u32.to_le_bytes());
                    bmp.extend_from_slice(&54u32.to_le_bytes());
                    bmp.extend_from_slice(&40u32.to_le_bytes());
                    bmp.extend_from_slice(&(ew as i32).to_le_bytes());
                    bmp.extend_from_slice(&(-(eh as i32)).to_le_bytes());
                    bmp.extend_from_slice(&1u16.to_le_bytes());
                    bmp.extend_from_slice(&32u16.to_le_bytes());
                    bmp.extend_from_slice(&[0u8; 24]);
                    bmp.extend_from_slice(px);
                    self.device.unmap_memory(self.staging_mem);
                    let dir = dump_dir.unwrap_or_default();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::write(format!("{dir}/vk{n:05}.bmp"), bmp);
                }
            }
            true
        }
    }

    /// Render passes for offscreen targets: one that clears, one that keeps the contents.
    unsafe fn rt_passes(&mut self) -> Option<(vk::RenderPass, vk::RenderPass)> {
        if self.rt_rp_load == vk::RenderPass::null() {
            for clear in [true, false] {
                let att = [
                    vk::AttachmentDescription::default()
                        .format(vk::Format::B8G8R8A8_UNORM)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .load_op(if clear { vk::AttachmentLoadOp::CLEAR } else { vk::AttachmentLoadOp::LOAD })
                        .store_op(vk::AttachmentStoreOp::STORE)
                        .initial_layout(if clear { vk::ImageLayout::UNDEFINED } else { vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL })
                        .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL),
                    vk::AttachmentDescription::default()
                        .format(self.depth_format)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .load_op(if clear { vk::AttachmentLoadOp::CLEAR } else { vk::AttachmentLoadOp::LOAD })
                        .store_op(vk::AttachmentStoreOp::STORE)
                        .stencil_load_op(if clear { vk::AttachmentLoadOp::CLEAR } else { vk::AttachmentLoadOp::LOAD })
                        .stencil_store_op(vk::AttachmentStoreOp::STORE)
                        .initial_layout(if clear { vk::ImageLayout::UNDEFINED } else { vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL })
                        .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
                ];
                let color_ref = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
                let depth_ref = vk::AttachmentReference { attachment: 1, layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
                let subpass = [vk::SubpassDescription::default()
                    .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                    .color_attachments(&color_ref)
                    .depth_stencil_attachment(&depth_ref)];
                let rp = self.device.create_render_pass(&vk::RenderPassCreateInfo::default().attachments(&att).subpasses(&subpass), None).ok()?;
                if clear {
                    self.rt_rp_clear = rp;
                } else {
                    self.rt_rp_load = rp;
                }
            }
        }
        if self.rt_quad_pipe == vk::Pipeline::null() {
            self.rt_quad_pipe = self.create_quad_pipeline(self.rt_rp_load, self.blit_layout).unwrap_or(vk::Pipeline::null());
        }
        Some((self.rt_rp_clear, self.rt_rp_load))
    }

    /// Destroys a cached offscreen render target (framebuffer, depth, color image & memory).
    unsafe fn destroy_rt(&mut self, addr: usize) {
        if let Some(fb) = self.rt_fbs.remove(&addr) {
            self.device.destroy_framebuffer(fb, None);
        }
        if let Some((img, mem, dview)) = self.rt_depth.remove(&addr) {
            self.device.destroy_image_view(dview, None);
            self.device.destroy_image(img, None);
            self.device.free_memory(mem, None);
        }
        if let Some(old) = self.targets.remove(&addr) {
            use ash::vk::Handle;
            let old_raw = old.view.as_raw();
            self.view_dsets.retain(|&(v, _), _| v != old_raw);
            self.gen_dsets.retain(|k, _| !k.iter().any(|&(v, _)| v == old_raw));
            self.device.destroy_image_view(old.view, None);
            self.device.destroy_image(old.image, None);
            self.device.free_memory(old.memory, None);
        }
        self.rt_dsets.remove(&addr);
        self.rt_layouts.remove(&addr);
        self.rt_last_used.remove(&addr);
        self.persistent_rts.remove(&addr);
        self.rt_guest_sums.remove(&addr);
    }

    /// Offscreen colour target for a guest render-target address (created on first use).
    unsafe fn rt_target(&mut self, addr: usize, w: u32, h: u32) -> Option<(vk::Image, vk::Framebuffer)> {
        let (_, rp_load) = self.rt_passes()?;
        // If this guest address was previously uploaded as a CPU texture (e.g. terrain atlas
        // that later receives animated flipbook draws), preserve its pixels when promoting to RT.
        let promoted_tex = if self.tex_cache.contains_key(&addr) {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            self.tex_sums.remove(&addr);
            self.tex_cache.remove(&addr).map(|(old, _)| old)
        } else {
            None
        };
        let fresh = !matches!(self.targets.get(&addr), Some(t) if t.width == w && t.height == h);
        if fresh {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if let Some(old) = self.targets.get(&addr) {
                use ash::vk::Handle;
                let old_raw = old.view.as_raw();
                self.view_dsets.retain(|&(v, _), _| v != old_raw);
            }
            if let Some(fb) = self.rt_fbs.remove(&addr) {
                self.device.destroy_framebuffer(fb, None);
            }
            if let Some((img, mem, dview)) = self.rt_depth.remove(&addr) {
                self.device.destroy_image_view(dview, None);
                self.device.destroy_image(img, None);
                self.device.free_memory(mem, None);
            }
            self.rt_dsets.remove(&addr);
            self.rt_layouts.insert(addr, vk::ImageLayout::UNDEFINED);
        }
        let t = self.ensure_target(addr, w, h)?;
        let (image, view) = (t.image, t.view);
        if let Some(old) = promoted_tex {
            if old.width == w && old.height == h {
                let _ = self.device.reset_fences(&[self.fence]);
                let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
                let _ = self.device.begin_command_buffer(
                    self.cmd_buf,
                    &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                );
                let sub = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
                let to_copy = [
                    vk::ImageMemoryBarrier::default()
                        .image(old.image)
                        .subresource_range(sub)
                        .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .src_access_mask(vk::AccessFlags::SHADER_READ)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ),
                    vk::ImageMemoryBarrier::default()
                        .image(image)
                        .subresource_range(sub)
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE),
                ];
                self.device.cmd_pipeline_barrier(
                    self.cmd_buf,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &to_copy,
                );
                let copy = [vk::ImageCopy {
                    src_subresource: vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 },
                    src_offset: vk::Offset3D::default(),
                    dst_subresource: vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 },
                    dst_offset: vk::Offset3D::default(),
                    extent: vk::Extent3D { width: w, height: h, depth: 1 },
                }];
                self.device.cmd_copy_image(
                    self.cmd_buf,
                    old.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &copy,
                );
                let to_read = [vk::ImageMemoryBarrier::default()
                    .image(image)
                    .subresource_range(sub)
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)];
                self.device.cmd_pipeline_barrier(
                    self.cmd_buf,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &to_read,
                );
                let _ = self.device.end_command_buffer(self.cmd_buf);
                let cmds = [self.cmd_buf];
                let submit = [vk::SubmitInfo::default().command_buffers(&cmds)];
                let _ = self.device.queue_submit(self.queue, &submit, self.fence);
                let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                self.rt_layouts.insert(addr, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                if w < 3840 && h < 2160 {
                    self.persistent_rts.insert(addr);
                }
            }
            use ash::vk::Handle;
            let old_raw = old.view.as_raw();
            self.view_dsets.retain(|&(v, _), _| v != old_raw);
            self.gen_dsets.retain(|k, _| !k.iter().any(|&(v, _)| v == old_raw));
            self.device.destroy_image_view(old.view, None);
            self.device.destroy_image(old.image, None);
            self.device.free_memory(old.memory, None);
        } else if fresh && w < 3840 && h < 2160 && w > 0 && h > 0 && w <= 4096 && h <= 4096 {
            // If guest memory already contains a CPU-uploaded texture atlas at `addr`
            // (e.g. Minecraft's 2048x1024 terrain atlas drawn into by Flipbook before
            // being sampled for the first time), upload it into the new RT and mark persistent.
            let want_src = (((w as usize) + 127) / 128) * (((h as usize) + 127) / 128) * 65536;
            let mip0_off = Self::detect_sw27_mip0_offset(addr, w, h, want_src);
            let src_addr = addr.wrapping_add(mip0_off);
            let avail = Self::readable_bytes(src_addr, want_src);
            if avail >= 65536 {
                let words = avail / 8;
                let step = (words / 512).max(1);
                let p = src_addr as *const u64;
                let mut hsum = 0xcbf2_9ce4_8422_2325u64;
                let mut nz_cnt = 0u32;
                let mut k = 0;
                while k < words {
                    let v = p.add(k).read_unaligned();
                    if v != 0 {
                        nz_cnt += 1;
                    }
                    hsum = (hsum ^ v).wrapping_mul(0x100_0000_01b3);
                    k += step;
                }
                if std::env::var_os("ORBIS_FRAME_STATS").is_some() {
                    eprintln!("[orbis] ATLAS-SYNC(rt_target) {addr:#x}+{mip0_off:#x} {w}x{h} avail={avail} nz={nz_cnt}/512");
                }
                if nz_cnt > 0 {
                    let src = std::slice::from_raw_parts(src_addr as *const u8, avail);
                    let mut pixels = crate::tiling::untile_32bpp(src, w, h, 27 | (56 << 8) | (0xFAC << 20));
                    if w == 2048 && h == 1024 {
                        if let Ok(dir) = std::env::var("ORBIS_VK_DUMP") {
                            let bytes = (w as usize) * (h as usize) * 4;
                            let mut bmp = Vec::with_capacity(54 + bytes);
                            bmp.extend_from_slice(b"BM");
                            bmp.extend_from_slice(&((54 + bytes) as u32).to_le_bytes());
                            bmp.extend_from_slice(&0u32.to_le_bytes());
                            bmp.extend_from_slice(&54u32.to_le_bytes());
                            bmp.extend_from_slice(&40u32.to_le_bytes());
                            bmp.extend_from_slice(&(w as i32).to_le_bytes());
                            bmp.extend_from_slice(&(-(h as i32)).to_le_bytes());
                            bmp.extend_from_slice(&1u16.to_le_bytes());
                            bmp.extend_from_slice(&32u16.to_le_bytes());
                            bmp.extend_from_slice(&[0u8; 24]);
                            for &px in &pixels {
                                let [r, g, b, a] = px.to_le_bytes();
                                bmp.extend_from_slice(&[b, g, r, a]);
                            }
                            let _ = std::fs::create_dir_all(&dir);
                            let _ = std::fs::write(format!("{dir}/atlas_{w}x{h}.bmp"), bmp);
                        }
                    }
                    // Render targets in `self.targets` use `VK_FORMAT_B8G8R8A8_UNORM`, while
                    // `untile_32bpp` returns `[R, G, B, A]` u32 pixels -> swap R <-> B before upload!
                    for px in &mut pixels {
                        let [r, g, b, a] = px.to_le_bytes();
                        *px = u32::from_le_bytes([b, g, r, a]);
                    }
                    self.reupload_texture(image, w, h, &pixels);
                    self.rt_layouts.insert(addr, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                    self.persistent_rts.insert(addr);
                    self.rt_guest_sums.insert(addr, hsum);
                }
            }
        }
        if !self.rt_fbs.contains_key(&addr) {
            if !self.rt_depth.contains_key(&addr) {
                let img = self
                    .device
                    .create_image(
                        &vk::ImageCreateInfo::default()
                            .image_type(vk::ImageType::TYPE_2D)
                            .format(self.depth_format)
                            .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                            .mip_levels(1)
                            .array_layers(1)
                            .samples(vk::SampleCountFlags::TYPE_1)
                            .tiling(vk::ImageTiling::OPTIMAL)
                            .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
                            .initial_layout(vk::ImageLayout::UNDEFINED),
                        None,
                    )
                    .ok()?;
                let req = self.device.get_image_memory_requirements(img);
                let mi = self.find_mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
                let mem = self.device.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mi), None).ok()?;
                self.device.bind_image_memory(img, mem, 0).ok()?;
                let dview = self
                    .device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(img)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(self.depth_format)
                            .subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 }),
                        None,
                    )
                    .ok()?;
                self.rt_depth.insert(addr, (img, mem, dview));
            }
            let atts = [view, self.rt_depth[&addr].2];
            let fb = self
                .device
                .create_framebuffer(&vk::FramebufferCreateInfo::default().render_pass(rp_load).attachments(&atts).width(w).height(h).layers(1), None)
                .ok()?;
            self.rt_fbs.insert(addr, fb);
        }
        Some((image, self.rt_fbs[&addr]))
    }

    /// Vulkan sampler for an S#: filter (word 2) and address modes (word 0).
    unsafe fn sampler_for(&mut self, s: [u32; 4]) -> vk::Sampler {
        let key = (s[0] & 0x1FF) | (((s[2] >> 20) & 0xF) << 9);
        if let Some(&x) = self.samplers.get(&key) {
            return x;
        }
        let addr = |m: u32| match m {
            0 => vk::SamplerAddressMode::REPEAT,
            1 | 3 | 5 | 7 => vk::SamplerAddressMode::MIRRORED_REPEAT,
            6 => vk::SamplerAddressMode::CLAMP_TO_BORDER,
            _ => vk::SamplerAddressMode::CLAMP_TO_EDGE,
        };
        let filt = |f: u32| if f == 0 { vk::Filter::NEAREST } else { vk::Filter::LINEAR };
        let mag = filt((s[2] >> 20) & 3);
        let min = filt((s[2] >> 22) & 3);
        let u = addr(s[0] & 7);
        let v = addr((s[0] >> 3) & 7);
        let w = addr((s[0] >> 6) & 7);
        if mag == vk::Filter::LINEAR
            && min == vk::Filter::LINEAR
            && u == vk::SamplerAddressMode::CLAMP_TO_EDGE
            && v == vk::SamplerAddressMode::CLAMP_TO_EDGE
            && w == vk::SamplerAddressMode::CLAMP_TO_EDGE
        {
            self.samplers.insert(key, self.sampler);
            return self.sampler;
        }
        let ci = vk::SamplerCreateInfo::default()
            .mag_filter(mag)
            .min_filter(min)
            .address_mode_u(u)
            .address_mode_v(v)
            .address_mode_w(w)
            .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK);
        let x = self.device.create_sampler(&ci, None).unwrap_or(self.sampler);
        self.samplers.insert(key, x);
        x
    }

    /// Descriptor set for an image view with the sampler an S# describes.
    unsafe fn dset_for(&mut self, view: vk::ImageView, s: [u32; 4]) -> Option<vk::DescriptorSet> {
        use ash::vk::Handle;
        let sampler = self.sampler_for(s);
        let key = (view.as_raw(), sampler.as_raw());
        if let Some(&d) = self.view_dsets.get(&key) {
            return Some(d);
        }
        let dsls = [self.blit_dset_layout];
        let ds = self.device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&dsls)).ok()?[0];
        let info = [vk::DescriptorImageInfo::default().sampler(sampler).image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default().dst_set(ds).dst_binding(0).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&info)];
        self.device.update_descriptor_sets(&write, &[]);
        self.view_dsets.insert(key, ds);
        Some(ds)
    }

    /// Descriptor set with 4 combined image samplers (bindings 0..3) for generic pixel shaders.
    unsafe fn gen_dset_for(&mut self, slots: [(vk::ImageView, vk::Sampler); 4]) -> Option<vk::DescriptorSet> {
        use ash::vk::Handle;
        if self.gen_dset_layout == vk::DescriptorSetLayout::null() {
            return None;
        }
        let key = slots.map(|(v, s)| (v.as_raw(), s.as_raw()));
        if let Some(&d) = self.gen_dsets.get(&key) {
            return Some(d);
        }
        let dsls = [self.gen_dset_layout];
        let ds = self.device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&dsls)).ok()?[0];
        let infos = slots.map(|(v, s)| [vk::DescriptorImageInfo::default().sampler(s).image_view(v).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]);
        let writes: [vk::WriteDescriptorSet; 4] = std::array::from_fn(|k| {
            vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(k as u32)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&infos[k])
        });
        self.device.update_descriptor_sets(&writes, &[]);
        self.gen_dsets.insert(key, ds);
        Some(ds)
    }

    /// Ensures a cached host-visible GPU `STORAGE_BUFFER` holds up-to-date raw bytes of a
    /// guest vertex buffer `(addr, need_bytes)`. Static Minecraft chunk VBs upload once and
    /// hit cache on every subsequent frame with zero PCIe copy.
    /// Returns `Some(vs_dummy_buf)` ONLY for unused slots (`need_bytes == 0`), and `None` if a
    /// real VB cannot be read or allocated (never binds a 256B dummy buffer for a large VB!).
    unsafe fn ensure_guest_vb(&mut self, addr: usize, need_bytes: usize) -> Option<vk::Buffer> {
        if need_bytes == 0 || addr == 0 {
            return Some(self.vs_dummy_buf);
        }
        if addr < 0x1_0000_0000 || need_bytes > (64 << 20) {
            return None;
        }
        let need = (need_bytes + 15) & !15;
        // Fast path: already checksum-verified this frame (64-u64 checksum takes ~100ns,
        // so checking once per frame is instant and never serves stale reused ring-buffer VBs!).
        if let Some(entry) = self.guest_vb_cache.get_mut(&addr) {
            if entry.3 >= need {
                let last_chk = self.guest_vb_checked.get(&addr).copied().unwrap_or(0);
                if last_chk == self.frame_no {
                    entry.5 = self.frame_no;
                    return Some(entry.0);
                }
            }
        }
        let avail = Self::readable_bytes(addr, need);
        if avail < 16 {
            return None;
        }
        let upload_len = avail.min(need);
        self.guest_vb_checked.insert(addr, self.frame_no);
        // Fast sampled checksum (64 u64s) to detect dynamic VB rewrites.
        let sum = {
            let words = upload_len / 8;
            let step = (words / 64).max(1);
            let p = addr as *const u64;
            let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (upload_len as u64);
            let mut k = 0;
            while k < words {
                h = (h ^ p.add(k).read_unaligned()).wrapping_mul(0x100_0000_01b3);
                k += step;
            }
            h
        };
        if let Some(entry) = self.guest_vb_cache.get_mut(&addr) {
            if entry.2 >= upload_len {
                entry.5 = self.frame_no;
                if entry.3 < upload_len || entry.4 != sum {
                    let mem = entry.1;
                    let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                    if let Ok(p) = self.device.map_memory(mem, 0, upload_len as u64, vk::MemoryMapFlags::empty()) {
                        std::ptr::copy_nonoverlapping(addr as *const u8, p as *mut u8, upload_len);
                        self.device.unmap_memory(mem);
                    }
                    entry.3 = upload_len;
                    entry.4 = sum;
                }
                return Some(entry.0);
            }
            // Need larger capacity: free old buffer.
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if let Some((ob, om, _, _, _, _)) = self.guest_vb_cache.remove(&addr) {
                use ash::vk::Handle;
                let raw = ob.as_raw();
                self.vs_dsets.retain(|k, _| !k.contains(&raw));
                self.device.destroy_buffer(ob, None);
                self.device.free_memory(om, None);
            }
        }
        // Evict stale chunk VBs from PREVIOUS frames if cache grows beyond 8192 buffers
        // (NEVER evict a buffer already bound in the current frame!).
        if self.guest_vb_cache.len() >= 8192 {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            let cur_f = self.frame_no;
            let mut old: Vec<(u64, usize)> = self
                .guest_vb_cache
                .iter()
                .filter(|(_, e)| e.5 < cur_f)
                .map(|(&a, e)| (e.5, a))
                .collect();
            old.sort_by_key(|&(f, _)| f);
            for &(_, a) in old.iter().take(256) {
                if let Some((ob, om, _, _, _, _)) = self.guest_vb_cache.remove(&a) {
                    use ash::vk::Handle;
                    let raw = ob.as_raw();
                    self.vs_dsets.retain(|k, _| !k.contains(&raw));
                    self.device.destroy_buffer(ob, None);
                    self.device.free_memory(om, None);
                }
            }
        }
        let alloc_sz = upload_len.max(4096).next_power_of_two();
        let Some((buf, mem)) = self.alloc_host_buffer(alloc_sz, vk::BufferUsageFlags::STORAGE_BUFFER) else {
            return None;
        };
        if let Ok(p) = self.device.map_memory(mem, 0, upload_len as u64, vk::MemoryMapFlags::empty()) {
            std::ptr::copy_nonoverlapping(addr as *const u8, p as *mut u8, upload_len);
            self.device.unmap_memory(mem);
        }
        self.guest_vb_cache.insert(addr, (buf, mem, alloc_sz, upload_len, sum, self.frame_no));
        Some(buf)
    }

    /// Ensures a cached host-visible GPU `INDEX_BUFFER` holds up-to-date raw bytes of a
    /// guest index buffer `(addr, need_bytes)`. Eliminates CPU index scanning & u16->u32 copies!
    unsafe fn ensure_guest_ib(&mut self, addr: usize, need_bytes: usize) -> Option<vk::Buffer> {
        if addr < 0x1_0000_0000 || need_bytes == 0 || need_bytes > (64 << 20) {
            return None;
        }
        let need = (need_bytes + 15) & !15;
        if let Some(entry) = self.guest_ib_cache.get_mut(&addr) {
            if entry.3 >= need {
                let last_chk = self.guest_ib_checked.get(&addr).copied().unwrap_or(0);
                if last_chk == self.frame_no {
                    entry.5 = self.frame_no;
                    return Some(entry.0);
                }
            }
        }
        let avail = Self::readable_bytes(addr, need);
        if avail < need_bytes {
            return None;
        }
        let upload_len = avail.min(need);
        self.guest_ib_checked.insert(addr, self.frame_no);
        let sum = {
            let words = upload_len / 8;
            let step = (words / 64).max(1);
            let p = addr as *const u64;
            let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (upload_len as u64);
            let mut k = 0;
            while k < words {
                h = (h ^ p.add(k).read_unaligned()).wrapping_mul(0x100_0000_01b3);
                k += step;
            }
            h
        };
        if let Some(entry) = self.guest_ib_cache.get_mut(&addr) {
            if entry.2 >= upload_len {
                entry.5 = self.frame_no;
                if entry.3 < upload_len || entry.4 != sum {
                    let mem = entry.1;
                    let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                    if let Ok(p) = self.device.map_memory(mem, 0, upload_len as u64, vk::MemoryMapFlags::empty()) {
                        std::ptr::copy_nonoverlapping(addr as *const u8, p as *mut u8, upload_len);
                        self.device.unmap_memory(mem);
                    }
                    entry.3 = upload_len;
                    entry.4 = sum;
                }
                return Some(entry.0);
            }
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if let Some((ob, om, _, _, _, _)) = self.guest_ib_cache.remove(&addr) {
                self.device.destroy_buffer(ob, None);
                self.device.free_memory(om, None);
            }
        }
        if self.guest_ib_cache.len() >= 8192 {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            let cur_f = self.frame_no;
            let mut old: Vec<(u64, usize)> = self
                .guest_ib_cache
                .iter()
                .filter(|(_, e)| e.5 < cur_f)
                .map(|(&a, e)| (e.5, a))
                .collect();
            old.sort_by_key(|&(f, _)| f);
            for &(_, a) in old.iter().take(512) {
                if let Some((ob, om, _, _, _, _)) = self.guest_ib_cache.remove(&a) {
                    self.device.destroy_buffer(ob, None);
                    self.device.free_memory(om, None);
                }
            }
        }
        let alloc_sz = upload_len.max(4096).next_power_of_two();
        let Some((buf, mem)) = self.alloc_host_buffer(alloc_sz, vk::BufferUsageFlags::INDEX_BUFFER) else {
            return None;
        };
        if let Ok(p) = self.device.map_memory(mem, 0, upload_len as u64, vk::MemoryMapFlags::empty()) {
            std::ptr::copy_nonoverlapping(addr as *const u8, p as *mut u8, upload_len);
            self.device.unmap_memory(mem);
        }
        self.guest_ib_cache.insert(addr, (buf, mem, alloc_sz, upload_len, sum, self.frame_no));
        Some(buf)
    }

    /// Descriptor set (Set 1) for GPU-VS: binding 0 = dynamic `vs_ubuf`, bindings 1..=4 = VBs.
    unsafe fn vs_dset_for(&mut self, vbs: [vk::Buffer; 4]) -> Option<vk::DescriptorSet> {
        use ash::vk::Handle;
        if self.vs_dset_layout == vk::DescriptorSetLayout::null() || self.vs_ubuf == vk::Buffer::null() {
            return None;
        }
        let key = vbs.map(|b| b.as_raw());
        if let Some(&d) = self.vs_dsets.get(&key) {
            return Some(d);
        }
        let dsls = [self.vs_dset_layout];
        let ds = self
            .device
            .allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&dsls))
            .ok()?[0];
        let u_info = [vk::DescriptorBufferInfo::default().buffer(self.vs_ubuf).offset(0).range(16384)];
        let vb_infos = vbs.map(|b| [vk::DescriptorBufferInfo::default().buffer(b).offset(0).range(vk::WHOLE_SIZE)]);
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                .buffer_info(&u_info),
            vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&vb_infos[0]),
            vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&vb_infos[1]),
            vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&vb_infos[2]),
            vk::WriteDescriptorSet::default()
                .dst_set(ds)
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&vb_infos[3]),
        ];
        self.device.update_descriptor_sets(&writes, &[]);
        self.vs_dsets.insert(key, ds);
        Some(ds)
    }

    /// Descriptor set sampling a render target.
    unsafe fn rt_dset(&mut self, addr: usize) -> Option<vk::DescriptorSet> {
        if let Some(&d) = self.rt_dsets.get(&addr) {
            return Some(d);
        }
        let view = self.targets.get(&addr)?.view;
        let dsls = [self.blit_dset_layout];
        let ds = self.device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.desc_pool).set_layouts(&dsls)).ok()?[0];
        let info = [vk::DescriptorImageInfo::default().sampler(self.sampler).image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = [vk::WriteDescriptorSet::default().dst_set(ds).dst_binding(0).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&info)];
        self.device.update_descriptor_sets(&write, &[]);
        self.rt_dsets.insert(addr, ds);
        Some(ds)
    }

    unsafe fn rt_barrier(&mut self, addr: usize, to: vk::ImageLayout) {
        let Some(t) = self.targets.get(&addr) else { return };
        let from = *self.rt_layouts.get(&addr).unwrap_or(&vk::ImageLayout::UNDEFINED);
        if from == to {
            return;
        }
        let sub = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
        let b = [vk::ImageMemoryBarrier::default()
            .image(t.image)
            .subresource_range(sub)
            .old_layout(from)
            .new_layout(to)
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::TRANSFER_READ)];
        self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::ALL_COMMANDS, vk::PipelineStageFlags::ALL_COMMANDS, vk::DependencyFlags::empty(), &[], &[], &b);
        self.rt_layouts.insert(addr, to);
    }

    /// Renders a frame the way the game does: every draw into its own colour target (offscreen
    /// images keyed by guest address, sampled when the game reads them as textures), then
    /// shows the display buffer `display` in the window.
    pub fn present_frame(
        &mut self,
        clear_rgba: [f32; 4],
        vertices: &[Vertex],
        indices: &[u32],
        batches: &[DrawBatch],
        gverts: &[GVertex],
        gens: &[GenDraw],
        display: (usize, u32, u32),
    ) -> bool {
        if !self.blit_ready || self.vbuf == vk::Buffer::null() || self.ibuf == vk::Buffer::null() || display.0 == 0 {
            return false;
        }
        self.frame_no += 1;
        unsafe {
            if self.rt_passes().is_none() {
                return false;
            }
            let gidxs_guard = READY_GIDXS.lock().unwrap_or_else(|e| e.into_inner());
            let gidxs: &[u32] = &gidxs_guard;
            let vs_params_guard = READY_VS_PARAMS.lock().unwrap_or_else(|e| e.into_inner());
            let vs_params: &[u32] = &vs_params_guard;
            let need_g = gverts.len() * std::mem::size_of::<GVertex>();
            let need_gi = gidxs.len() * 4;
            let need_vsu = (vs_params.len() * 4).saturating_add(16384);
            // Dynamically grow GVBUF / GIBUF / VS_UBUF so 1,200+ draw frames NEVER clip HUD or white out!
            if !gens.is_empty() && (self.gvbuf == vk::Buffer::null() || need_g > self.gvbuf_cap) {
                let new_cap = need_g.max(GVBUF_INIT_BYTES).next_power_of_two().min(512 << 20);
                if self.gvbuf != vk::Buffer::null() {
                    let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                    self.device.destroy_buffer(self.gvbuf, None);
                    self.device.free_memory(self.gvmem, None);
                    self.gvbuf = vk::Buffer::null();
                    self.gvmem = vk::DeviceMemory::null();
                    self.gvbuf_cap = 0;
                }
                if let Some((b, m)) = self.alloc_host_buffer(new_cap, vk::BufferUsageFlags::VERTEX_BUFFER) {
                    self.gvbuf = b;
                    self.gvmem = m;
                    self.gvbuf_cap = new_cap;
                }
            }
            if !gens.is_empty() && (self.gibuf == vk::Buffer::null() || need_gi > self.gibuf_cap) {
                let new_cap = need_gi.max(GIBUF_INIT_BYTES).next_power_of_two().min(128 << 20);
                if self.gibuf != vk::Buffer::null() {
                    let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                    self.device.destroy_buffer(self.gibuf, None);
                    self.device.free_memory(self.gimem, None);
                    self.gibuf = vk::Buffer::null();
                    self.gimem = vk::DeviceMemory::null();
                    self.gibuf_cap = 0;
                }
                if let Some((b, m)) = self.alloc_host_buffer(new_cap, vk::BufferUsageFlags::INDEX_BUFFER) {
                    self.gibuf = b;
                    self.gimem = m;
                    self.gibuf_cap = new_cap;
                }
            }
            if !vs_params.is_empty() && (self.vs_ubuf == vk::Buffer::null() || need_vsu > self.vs_ubuf_cap) {
                let new_cap = need_vsu.max(VS_UBUF_INIT_BYTES).next_power_of_two().min(64 << 20);
                if self.vs_ubuf != vk::Buffer::null() {
                    let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                    self.vs_dsets.clear();
                    self.device.destroy_buffer(self.vs_ubuf, None);
                    self.device.free_memory(self.vs_umem, None);
                    self.vs_ubuf = vk::Buffer::null();
                    self.vs_umem = vk::DeviceMemory::null();
                    self.vs_ubuf_cap = 0;
                }
                if let Some((b, m)) = self.alloc_host_buffer(new_cap, vk::BufferUsageFlags::STORAGE_BUFFER) {
                    self.vs_ubuf = b;
                    self.vs_umem = m;
                    self.vs_ubuf_cap = new_cap;
                }
            }
            if !vs_params.is_empty() && self.vs_dummy_buf == vk::Buffer::null() {
                if let Some((b, m)) = self.alloc_host_buffer(256, vk::BufferUsageFlags::STORAGE_BUFFER) {
                    self.vs_dummy_buf = b;
                    self.vs_dummy_mem = m;
                }
            }
            let gi_bytes = need_gi.min(self.gibuf_cap);
            let gi_max = (gi_bytes / 4) as u32;
            let vsu_bytes = (vs_params.len() * 4).min(self.vs_ubuf_cap);
            let v_bytes = (vertices.len() * std::mem::size_of::<Vertex>()).min(524288);
            let i_bytes = (indices.len() * 4).min(131072);
            let max_idx = (i_bytes / 4) as u32;
            let g_bytes = need_g.min(self.gvbuf_cap);
            let g_max = (g_bytes / std::mem::size_of::<GVertex>()) as u32;

            // Pre-create all render targets used this frame BEFORE resolving sampled textures,
            // so a freshly allocated offscreen RT drawn and composited in the same frame resolves
            // to its GPU VkImageView instead of falling through to a CPU RAM swiz=27 upload.
            self.frame_seq = self.frame_seq.wrapping_add(1);
            let seq = self.frame_seq;
            let mut used: Vec<(usize, u32, u32)> = Vec::with_capacity(8);
            used.push(display);
            for b in batches {
                if let Some(g) = b.gen {
                    if let Some(gd) = gens.get(g as usize) {
                        if self.gvbuf != vk::Buffer::null()
                            && gd.first_vertex.saturating_add(gd.vertex_count) <= g_max
                            && ((gd.index_count > 0 && gd.first_index.saturating_add(gd.index_count) <= gi_max)
                                || (gd.index_count == 0 && gd.vertex_count > 0))
                        {
                            let rt = if gd.rt.0 != 0 && gd.rt.1 <= 8192 && gd.rt.2 <= 8192 { gd.rt } else { display };
                            used.push(rt);
                        }
                    }
                }
            }
            used.sort();
            used.dedup();
            // Evict least-recently-used offscreen RTs if cache grows beyond 48 targets (prevents VRAM OOM
            // while keeping multi-layer Cohtml/OreUI screens like Create New World intact).
            if self.targets.len() > 48 {
                let mut candidates: Vec<(u64, usize)> = self
                    .targets
                    .keys()
                    .copied()
                    .filter(|&a| !used.iter().any(|&(u, _, _)| u == a))
                    .map(|a| (self.rt_last_used.get(&a).copied().unwrap_or(0), a))
                    .collect();
                candidates.sort_by_key(|&(t, _)| t);
                let to_evict = self.targets.len().saturating_sub(48);
                if to_evict > 0 && !candidates.is_empty() {
                    let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                    for &(_, addr) in candidates.iter().take(to_evict) {
                        self.destroy_rt(addr);
                    }
                }
            }
            for &(a, w, h) in &used {
                if self.rt_target(a, w, h).is_none() {
                    return false;
                }
                self.rt_last_used.insert(a, seq);
            }

            // Pre-size and upload all GPU-VS guest VBs to their per-frame maximum needed length
            // BEFORE building any descriptor sets — prevents destroying a shared chunk VB arena
            // mid-frame while an earlier draw's `vs_dset` already references it!
            if !vs_params.is_empty() {
                let mut vb_max: HashMap<usize, usize> = HashMap::new();
                for b in batches {
                    if let Some(g) = b.gen {
                        if let Some(gd) = gens.get(g as usize) {
                            if gd.vs_spirv.is_some() {
                                for &(addr, len) in &gd.vs_vbs {
                                    if addr >= 0x1_0000_0000 && len > 0 {
                                        let e = vb_max.entry(addr).or_insert(0);
                                        if len > *e {
                                            *e = len;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let mut ib_max: HashMap<usize, usize> = HashMap::new();
                for b in batches {
                    if let Some(g) = b.gen {
                        if let Some(gd) = gens.get(g as usize) {
                            if let Some((ib_addr, ib_len, _)) = gd.guest_ib {
                                if ib_addr != 0 && ib_len > 0 {
                                    let e = ib_max.entry(ib_addr).or_insert(0);
                                    if ib_len > *e {
                                        *e = ib_len;
                                    }
                                }
                            }
                        }
                    }
                }
                for (addr, max_len) in vb_max {
                    let _ = self.ensure_guest_vb(addr, max_len);
                }
                for (addr, max_len) in ib_max {
                    let _ = self.ensure_guest_ib(addr, max_len);
                }
            }

            // Resolve every draw before recording: textures, pipelines, targets.
            struct Item {
                rt: (usize, u32, u32),
                first: u32,
                count: u32,
                indexed: bool,
                ib_buf: vk::Buffer,
                ib_32: bool,
                ds: vk::DescriptorSet,
                vs_ds: vk::DescriptorSet,
                vs_off: u32,
                pipe: vk::Pipeline,
                gen: usize,
                scissor: vk::Rect2D,
                sampled_rts: [usize; 4],
            }
            let full = |w: u32, h: u32| vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: w, height: h } };
            let fallback_view = self.upload_tex.as_ref().map(|t| t.view).unwrap_or(vk::ImageView::null());
            let mut items: Vec<Item> = Vec::with_capacity(batches.len());
            for b in batches {
                if let Some(g) = b.gen {
                    let Some(gd) = gens.get(g as usize) else { continue };
                    let has_guest_ib = gd.guest_ib.is_some();
                    if self.gvbuf == vk::Buffer::null()
                        || gd.first_vertex.saturating_add(gd.vertex_count) > g_max
                        || (gd.index_count > 0 && !has_guest_ib && (self.gibuf == vk::Buffer::null() || gd.first_index.saturating_add(gd.index_count) > gi_max))
                        || (gd.index_count == 0 && gd.vertex_count == 0)
                    {
                        continue;
                    }
                    let rt = if gd.rt.0 != 0 && gd.rt.1 <= 8192 && gd.rt.2 <= 8192 { gd.rt } else { display };
                    let pipe = self.ensure_gen_pipeline(gd);
                    if pipe == vk::Pipeline::null() {
                        continue;
                    }
                    let mut sampled_rts = [0usize; 4];
                    let tex_list = [gd.tex, gd.extra_texs[0], gd.extra_texs[1], gd.extra_texs[2]];
                    let samp_list = [gd.samp, gd.extra_samps[0], gd.extra_samps[1], gd.extra_samps[2]];
                    let mut slot_vs = [(fallback_view, self.sampler); 4];
                    for k in 0..4 {
                        let layers = if k == 0 { gd.layers } else { 1 };
                        let (v, ok) = match tex_list[k] {
                            Some((a, w, h, sw)) if self.targets.contains_key(&a) && a != rt.0 => {
                                // If guest memory was populated after RT creation (e.g. terrain atlas
                                // uploaded via CPU/compute), sync non-zero guest pixels into the RT once.
                                if w < 3840 && h < 2160 && self.tex_checked.get(&a) != Some(&self.frame_no) {
                                    self.tex_checked.insert(a, self.frame_no);
                                    let sw_mode = sw & 0x1F;
                                    let want_src = if sw_mode == 0 {
                                        (w as usize) * (h as usize) * 4
                                    } else {
                                        (((w as usize) + 127) / 128) * (((h as usize) + 127) / 128) * 65536
                                    };
                                    let mip0_off = if sw_mode == 27 {
                                        Self::detect_sw27_mip0_offset(a, w, h, want_src)
                                    } else {
                                        0
                                    };
                                    let src_a = a.wrapping_add(mip0_off);
                                    let avail = Self::readable_bytes(src_a, want_src);
                                    if avail >= 64 {
                                        let words = avail / 8;
                                        let step = (words / 512).max(1);
                                        let p = src_a as *const u64;
                                        let mut hsum = 0xcbf2_9ce4_8422_2325u64;
                                        let mut nz_cnt = 0u32;
                                        let mut j = 0;
                                        while j < words {
                                            let val = p.add(j).read_unaligned();
                                            if val != 0 {
                                                nz_cnt += 1;
                                            }
                                            hsum = (hsum ^ val).wrapping_mul(0x100_0000_01b3);
                                            j += step;
                                        }
                                        if nz_cnt > 0 && self.rt_guest_sums.get(&a) != Some(&hsum) {
                                            if let Some(t) = self.targets.get(&a) {
                                                let img = t.image;
                                                let (tw, th) = (t.width, t.height);
                                                if std::env::var_os("ORBIS_FRAME_STATS").is_some() {
                                                    eprintln!("[orbis] ATLAS-SYNC(sample) {a:#x}+{mip0_off:#x} target={tw}x{th} tex={w}x{h} sw={sw_mode} nz={nz_cnt}/512");
                                                }
                                                if tw == w && th == h {
                                                    let src = std::slice::from_raw_parts(src_a as *const u8, avail);
                                                    let mut pixels = crate::tiling::untile_32bpp(src, w, h, sw);
                                                    // `self.targets` uses `VK_FORMAT_B8G8R8A8_UNORM` -> swap R <-> B.
                                                    for px in &mut pixels {
                                                        let [r, g, b, a] = px.to_le_bytes();
                                                        *px = u32::from_le_bytes([b, g, r, a]);
                                                    }
                                                    self.reupload_texture(img, w, h, &pixels);
                                                    self.rt_layouts.insert(a, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                                                    self.persistent_rts.insert(a);
                                                    self.rt_guest_sums.insert(a, hsum);
                                                }
                                            }
                                        }
                                    }
                                }
                                sampled_rts[k] = a;
                                self.rt_last_used.insert(a, seq);
                                (self.targets.get(&a).map(|t| t.view), true)
                            }
                            Some((a, _, _, _)) if a == rt.0 => (None, false),
                            Some((a, w, h, sw)) if layers > 1 => {
                                let d = self.ensure_array_texture(a, w, h, sw, layers);
                                (if d != self.desc_set { self.array_cache.get(&a).map(|t| t.0.view) } else { None }, d != self.desc_set)
                            }
                            Some((a, w, h, sw)) => {
                                let d = self.ensure_sampled_texture(a, w, h, sw);
                                (if d != self.desc_set { self.tex_cache.get(&a).map(|t| t.0.view) } else { None }, d != self.desc_set)
                            }
                            None => (None, false),
                        };
                        if let (Some(view), true) = (v, ok) {
                            let sampler = if samp_list[k] == [0; 4] { self.sampler } else { self.sampler_for(samp_list[k]) };
                            slot_vs[k] = (view, sampler);
                        }
                    }
                    if gd.extra_texs[0].is_some() && (gd.vertex_count >= 100 || gd.index_count >= 100) && std::env::var_os("ORBIS_FRAME_STATS").is_some() {
                        static MT_SEEN: std::sync::LazyLock<Mutex<std::collections::HashSet<usize>>> = std::sync::LazyLock::new(|| Mutex::new(Default::default()));
                        if MT_SEEN.lock().unwrap_or_else(|e| e.into_inner()).insert(gd.key.0) {
                            let mut desc = String::new();
                            for k in 0..4 {
                                if let Some((a, w, h, sw)) = tex_list[k] {
                                    let is_rt = self.targets.contains_key(&a);
                                    let fmt = (sw >> 8) & 0x1FF;
                                    let swm = sw & 0x1F;
                                    let dsel = (sw >> 20) & 0xFFF;
                                    let sample_u32 = if Self::readable_bytes(a, 65536) >= 64 {
                                        let p = a as *const u32;
                                        (0..1024).map(|i| p.add(i * 16).read_unaligned()).find(|&x| x != 0).unwrap_or(0)
                                    } else {
                                        0
                                    };
                                    desc += &format!(" t{k}=({a:#x} {w}x{h} fmt={fmt} sw={swm} sel={dsel:#x} rt={is_rt} px={sample_u32:#x})");
                                }
                            }
                            eprintln!("[orbis] MULTITEX ps={:#x} verts={}:{desc}", gd.key.0, gd.vertex_count);
                        }
                    }
                    let ds = self.gen_dset_for(slot_vs).unwrap_or(self.desc_set);
                    let (vs_ds, vs_off) = if gd.vs_spirv.is_some() {
                        let (Some(b0), Some(b1), Some(b2), Some(b3)) = (
                            self.ensure_guest_vb(gd.vs_vbs[0].0, gd.vs_vbs[0].1),
                            self.ensure_guest_vb(gd.vs_vbs[1].0, gd.vs_vbs[1].1),
                            self.ensure_guest_vb(gd.vs_vbs[2].0, gd.vs_vbs[2].1),
                            self.ensure_guest_vb(gd.vs_vbs[3].0, gd.vs_vbs[3].1),
                        ) else {
                            quarantine_gpu_vs(gd.vs_addr, "guest VB alloc/read failed");
                            continue;
                        };
                        let Some(vds) = self.vs_dset_for([b0, b1, b2, b3]) else {
                            quarantine_gpu_vs(gd.vs_addr, "vs_dset_for failed");
                            continue;
                        };
                        (vds, gd.vs_param_off)
                    } else {
                        (vk::DescriptorSet::null(), 0)
                    };
                    let (ib_buf, ib_32) = if let Some((ib_addr, ib_len, is_32)) = gd.guest_ib {
                        let Some(buf) = self.ensure_guest_ib(ib_addr, ib_len) else {
                            continue;
                        };
                        (buf, is_32)
                    } else {
                        (vk::Buffer::null(), true)
                    };
                    let [x, y, w, h] = gd.scissor;
                    let scissor = vk::Rect2D { offset: vk::Offset2D { x, y }, extent: vk::Extent2D { width: w.max(0) as u32, height: h.max(0) as u32 } };
                    let (first, count, indexed) = if gd.index_count > 0 {
                        (gd.first_index, gd.index_count, true)
                    } else {
                        (gd.first_vertex, gd.vertex_count, false)
                    };
                    items.push(Item { rt, first, count, indexed, ib_buf, ib_32, ds, vs_ds, vs_off, pipe, gen: g as usize, scissor, sampled_rts });
                } else if b.index_count > 0 && b.first_index.saturating_add(b.index_count) <= max_idx {
                    let ds = self.ensure_sampled_texture(b.tex_addr, b.tex_w, b.tex_h, b.tex_swiz);
                    items.push(Item { rt: display, first: b.first_index, count: b.index_count, indexed: true, ib_buf: vk::Buffer::null(), ib_32: true, ds, vs_ds: vk::DescriptorSet::null(), vs_off: 0, pipe: vk::Pipeline::null(), gen: 0, scissor: full(display.1, display.2), sampled_rts: [0; 4] });
                }
            }
            if items.is_empty() {
                return false;
            }

            // Runtime GPU watchdog: detect slow/hanging GPU-VS shaders BEFORE Windows 2s TDR!
            let t_fence = std::time::Instant::now();
            match self.device.wait_for_fences(&[self.fence], true, 400_000_000) {
                Ok(()) => {
                    let wait_ms = t_fence.elapsed().as_secs_f64() * 1000.0;
                    if !self.last_frame_gpu_vs.is_empty() {
                        if wait_ms > 350.0 {
                            self.quarantine_active_gpu_vs(&format!("GPU stall {wait_ms:.0}ms"));
                        } else if wait_ms < 150.0 {
                            for &a in &self.last_frame_gpu_vs {
                                self.healthy_gpu_vs.insert(a);
                            }
                        }
                    }
                }
                Err(vk::Result::TIMEOUT) => {
                    self.quarantine_active_gpu_vs("GPU fence timeout >400ms");
                    if let Err(e2) = self.device.wait_for_fences(&[self.fence], true, 2_000_000_000) {
                        if e2 == vk::Result::ERROR_DEVICE_LOST {
                            eprintln!("[orbis] AUTO-HEAL: wait_for_fences DEVICE_LOST -> scheduling live Vulkan recovery");
                            self.device_lost = true;
                            return false;
                        }
                    }
                }
                Err(vk::Result::ERROR_DEVICE_LOST) => {
                    eprintln!("[orbis] AUTO-HEAL: wait_for_fences DEVICE_LOST -> scheduling live Vulkan recovery");
                    self.quarantine_active_gpu_vs("wait_for_fences DEVICE_LOST");
                    self.device_lost = true;
                    return false;
                }
                Err(e) => {
                    eprintln!("[orbis] Vulkan: wait_for_fences error ({e:?})");
                }
            }
            if g_bytes > 0 {
                if let Ok(p) = self.device.map_memory(self.gvmem, 0, g_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(gverts.as_ptr() as *const u8, p as *mut u8, g_bytes);
                    self.device.unmap_memory(self.gvmem);
                }
            }
            if gi_bytes > 0 && self.gimem != vk::DeviceMemory::null() {
                if let Ok(p) = self.device.map_memory(self.gimem, 0, gi_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(gidxs.as_ptr() as *const u8, p as *mut u8, gi_bytes);
                    self.device.unmap_memory(self.gimem);
                }
            }
            if vsu_bytes > 0 && self.vs_umem != vk::DeviceMemory::null() {
                if let Ok(p) = self.device.map_memory(self.vs_umem, 0, vsu_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(vs_params.as_ptr() as *const u8, p as *mut u8, vsu_bytes);
                    self.device.unmap_memory(self.vs_umem);
                }
            }
            drop(gidxs_guard);
            drop(vs_params_guard);
            if v_bytes > 0 {
                if let Ok(p) = self.device.map_memory(self.vmem, 0, v_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(vertices.as_ptr() as *const u8, p as *mut u8, v_bytes);
                    self.device.unmap_memory(self.vmem);
                }
            }
            if i_bytes > 0 {
                if let Ok(p) = self.device.map_memory(self.imem, 0, i_bytes as u64, vk::MemoryMapFlags::empty()) {
                    std::ptr::copy_nonoverlapping(indices.as_ptr() as *const u8, p as *mut u8, i_bytes);
                    self.device.unmap_memory(self.imem);
                }
            }
            // ORBIS_VK_DUMP=<dir>: copy every 120th shown frame back for checking.
            static DUMP_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dump_dir = std::env::var("ORBIS_VK_DUMP").ok();
            let n = DUMP_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (ew, eh) = (self.swap_extent.width, self.swap_extent.height);
            let dump = dump_dir.is_some()
                && (n % 120 == 60 || n % 120 == 61 || ((gverts.len() > 5000 || gi_max > 5000) && n % 30 == 0))
                && self.ensure_staging((ew * eh * 4) as usize);

            // ORBIS_OREUI_DIAG=1: 1-shot draw list dump on OreUI screens (e.g. Create New World).
            if std::env::var_os("ORBIS_OREUI_DIAG").is_some() && n > 2500 && gens.len() >= 10 && gens.len() <= 200 && n % 60 == 0 {
                let has_oreui = gens.iter().any(|gd| {
                    gd.tex.map(|(_, w, h, sw)| w == 1024 && h == 1024 && ((sw >> 8) & 0x1FF) == 1).unwrap_or(false)
                        || (gd.scissor[0] > 0 || gd.scissor[1] > 0 || (gd.scissor[2] > 0 && (gd.scissor[2] as u32) < gd.rt.1))
                });
                static OREUI_DUMPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if has_oreui && OREUI_DUMPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
                    eprintln!("[orbis] OREUI-DIAG frame={n} draws={} display={:#x}({}x{})", gens.len(), display.0, display.1, display.2);
                    for (i, gd) in gens.iter().take(80).enumerate() {
                        eprintln!(
                            "[orbis]   #{i:02}: rt={:#x}({}x{}) ps={:#x} blend={:#x} dctl={:#x} wm={:#x} prim={} sc={:?} v={} i={} tex={:x?}",
                            gd.rt.0, gd.rt.1, gd.rt.2, gd.key.0, gd.blend, gd.depth, gd.wmask, gd.prim, gd.scissor, gd.vertex_count, gd.index_count, gd.tex
                        );
                    }
                }
            }

            // Auto-bypass FrameGen on 2D UI / OreUI / Menu / Inventory screens so UI animations,
            // Inventory slots, and Create New World tabs NEVER bounce or flicker!
            let mut draws_3d_depth = 0usize;
            let mut draws_ui_overlay = 0usize;
            for gd in gens {
                if (gd.depth & 6) == 6 && (gd.index_count >= 100 || gd.vertex_count >= 100) {
                    draws_3d_depth += 1;
                } else if gd.rt.0 == display.0 && (gd.depth & 2) == 0 {
                    draws_ui_overlay += 1;
                }
            }
            let is_active_3d_gameplay = draws_3d_depth >= 4 && draws_ui_overlay < 45;
            if !is_active_3d_gameplay {
                self.fg_has_prev = false;
            }

            let use_fg = !dump
                && is_active_3d_gameplay
                && self.fg_mult > 1
                && self.fg_pipe != vk::Pipeline::null()
                && self.swap_rp != vk::RenderPass::null()
                && !self.swap_fbs.is_empty()
                && self.ensure_fg_history(display.1, display.2);

            let disp_dset = if !dump && self.rcas_sharpness > 0.0 && self.rcas_pipe != vk::Pipeline::null() && self.swap_rp != vk::RenderPass::null() && !self.swap_fbs.is_empty() {
                let view = self.targets[&display.0].view;
                self.dset_for(view, [0, 0, (1 << 20) | (1 << 22), 0])
            } else {
                None
            };

            let initial_img_idx = if use_fg {
                None
            } else {
                let Some(idx) = self.acquire_swapchain_image() else {
                    return false;
                };
                Some(idx)
            };

            let _ = self.device.reset_fences(&[self.fence]);
            let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
            let _ = self.device.begin_command_buffer(self.cmd_buf, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));

            let (rp_clear, rp_load) = (self.rt_rp_clear, self.rt_rp_load);
            let mut cleared: Vec<usize> = vec![];
            let mut sampled_this_frame: Vec<usize> = vec![];
            let mut in_pass = false;
            let mut cur_rt = 0usize;
            let mut cur_pipe = vk::Pipeline::null();
            let mut cur_vb = vk::Buffer::null();
            let mut cur_ib = vk::Buffer::null();
            let mut cur_ib_ty = vk::IndexType::UINT32;
            let mut last_ds = vk::DescriptorSet::null();
            for it in &items {
                for &sr in &it.sampled_rts {
                    if sr != 0 {
                        sampled_this_frame.push(sr);
                        if *self.rt_layouts.get(&sr).unwrap_or(&vk::ImageLayout::UNDEFINED) != vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL {
                            if in_pass {
                                self.device.cmd_end_render_pass(self.cmd_buf);
                                in_pass = false;
                            }
                            self.rt_barrier(sr, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                        }
                    }
                }
                if !in_pass || cur_rt != it.rt.0 {
                    if in_pass {
                        self.device.cmd_end_render_pass(self.cmd_buf);
                    }
                    let (a, w, h) = it.rt;
                    let fb = self.rt_fbs[&a];
                    // The display buffer starts each frame cleared; persistent offscreen targets
                    // (Cohtml/OreUI RTs and promoted CPU texture atlases) keep their contents
                    // across partial dirty-rect updates.
                    let is_cohtml_clear = it.pipe != vk::Pipeline::null()
                        && gens[it.gen].depth == 0x701
                        && gens[it.gen].blend == 0x60010001
                        && gens[it.gen].tex.is_none();
                    let is_2d_partial_dirty = it.pipe != vk::Pipeline::null()
                        && (gens[it.gen].depth & 2) == 0
                        && (it.scissor.offset.x > 0
                            || it.scissor.offset.y > 0
                            || it.scissor.extent.width < w
                            || it.scissor.extent.height < h);
                    if a != display.0 && (is_cohtml_clear || is_2d_partial_dirty) {
                        self.persistent_rts.insert(a);
                    }
                    let has_prev = *self.rt_layouts.get(&a).unwrap_or(&vk::ImageLayout::UNDEFINED) != vk::ImageLayout::UNDEFINED;
                    let is_persistent = a != display.0 && (is_cohtml_clear || is_2d_partial_dirty || self.persistent_rts.contains(&a));
                    let clear_now = !cleared.contains(&a)
                        && !sampled_this_frame.contains(&a)
                        && !(is_persistent && has_prev);
                    if clear_now {
                        cleared.push(a);
                        self.rt_layouts.insert(a, vk::ImageLayout::UNDEFINED);
                    } else {
                        if is_persistent && !cleared.contains(&a) {
                            cleared.push(a);
                        }
                        self.rt_barrier(a, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
                    }
                    let cv = if a == display.0 { clear_rgba } else { [0.0, 0.0, 0.0, 0.0] };
                    let zc = if clear_now {
                        items
                            .iter()
                            .find(|x| x.rt.0 == a && x.pipe != vk::Pipeline::null() && (gens[x.gen].depth & 2 != 0 || gens[x.gen].depth == 0x701))
                            .map(|x| gens[x.gen].zclear)
                            .unwrap_or(1.0)
                    } else {
                        1.0
                    };
                    let clear = [
                        vk::ClearValue { color: vk::ClearColorValue { float32: cv } },
                        vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: zc, stencil: 0 } },
                    ];
                    let bi = vk::RenderPassBeginInfo::default()
                        .render_pass(if clear_now { rp_clear } else { rp_load })
                        .framebuffer(fb)
                        .render_area(full(w, h))
                        .clear_values(&clear);
                    self.device.cmd_begin_render_pass(self.cmd_buf, &bi, vk::SubpassContents::INLINE);
                    self.rt_layouts.insert(a, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
                    let vp = [vk::Viewport { x: 0.0, y: 0.0, width: w as f32, height: h as f32, min_depth: 0.0, max_depth: 1.0 }];
                    self.device.cmd_set_viewport(self.cmd_buf, 0, &vp);
                    in_pass = true;
                    cur_rt = a;
                    cur_pipe = vk::Pipeline::null();
                    cur_vb = vk::Buffer::null();
                    last_ds = vk::DescriptorSet::null();
                    cur_ib = vk::Buffer::null();
                }
                let generic = it.pipe != vk::Pipeline::null();
                let want = if generic { it.pipe } else { self.rt_quad_pipe };
                if want == vk::Pipeline::null() {
                    continue;
                }
                if want != cur_pipe {
                    self.device.cmd_bind_pipeline(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, want);
                    cur_pipe = want;
                    last_ds = vk::DescriptorSet::null();
                }
                let gpu_vs = it.vs_ds != vk::DescriptorSet::null();
                if !gpu_vs {
                    let vb = if generic { self.gvbuf } else { self.vbuf };
                    if vb != cur_vb {
                        self.device.cmd_bind_vertex_buffers(self.cmd_buf, 0, &[vb], &[0]);
                        cur_vb = vb;
                    }
                }
                if it.indexed {
                    let (ib, ity) = if it.ib_buf != vk::Buffer::null() {
                        (it.ib_buf, if it.ib_32 { vk::IndexType::UINT32 } else { vk::IndexType::UINT16 })
                    } else if generic {
                        (self.gibuf, vk::IndexType::UINT32)
                    } else {
                        (self.ibuf, vk::IndexType::UINT32)
                    };
                    if (ib != cur_ib || ity != cur_ib_ty) && ib != vk::Buffer::null() {
                        self.device.cmd_bind_index_buffer(self.cmd_buf, ib, 0, ity);
                        cur_ib = ib;
                        cur_ib_ty = ity;
                    }
                }
                let layout = if gpu_vs {
                    self.gpu_vs_layout
                } else if generic {
                    self.gen_layout
                } else {
                    self.blit_layout
                };
                if it.ds != last_ds {
                    self.device.cmd_bind_descriptor_sets(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, layout, 0, &[it.ds], &[]);
                    last_ds = it.ds;
                }
                if gpu_vs {
                    self.device.cmd_bind_descriptor_sets(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.gpu_vs_layout, 1, &[it.vs_ds], &[it.vs_off]);
                }
                self.device.cmd_set_scissor(self.cmd_buf, 0, &[it.scissor]);
                if generic {
                    let gd = &gens[it.gen];
                    if (gd.depth & 1) != 0 {
                        let back_en = (gd.depth & 0x80) != 0;
                        let f_ref = gd.stencil_ref & 0xFF;
                        let f_cmp = (gd.stencil_ref >> 8) & 0xFF;
                        let f_wrt = (gd.stencil_ref >> 16) & 0xFF;
                        if back_en {
                            self.device.cmd_set_stencil_reference(self.cmd_buf, vk::StencilFaceFlags::FRONT, f_ref);
                            self.device.cmd_set_stencil_compare_mask(self.cmd_buf, vk::StencilFaceFlags::FRONT, f_cmp);
                            self.device.cmd_set_stencil_write_mask(self.cmd_buf, vk::StencilFaceFlags::FRONT, f_wrt);
                            let b_ref = gd.stencil_ref_bf & 0xFF;
                            let b_cmp = (gd.stencil_ref_bf >> 8) & 0xFF;
                            let b_wrt = (gd.stencil_ref_bf >> 16) & 0xFF;
                            self.device.cmd_set_stencil_reference(self.cmd_buf, vk::StencilFaceFlags::BACK, b_ref);
                            self.device.cmd_set_stencil_compare_mask(self.cmd_buf, vk::StencilFaceFlags::BACK, b_cmp);
                            self.device.cmd_set_stencil_write_mask(self.cmd_buf, vk::StencilFaceFlags::BACK, b_wrt);
                        } else {
                            self.device.cmd_set_stencil_reference(self.cmd_buf, vk::StencilFaceFlags::FRONT_AND_BACK, f_ref);
                            self.device.cmd_set_stencil_compare_mask(self.cmd_buf, vk::StencilFaceFlags::FRONT_AND_BACK, f_cmp);
                            self.device.cmd_set_stencil_write_mask(self.cmd_buf, vk::StencilFaceFlags::FRONT_AND_BACK, f_wrt);
                        }
                    }
                    let bytes = std::slice::from_raw_parts(gd.slots.as_ptr() as *const u8, 256);
                    self.device.cmd_push_constants(self.cmd_buf, layout, vk::ShaderStageFlags::FRAGMENT, 0, bytes);
                    if it.indexed {
                        self.device.cmd_draw_indexed(self.cmd_buf, it.count, 1, it.first, 0, 0);
                    } else {
                        self.device.cmd_draw(self.cmd_buf, it.count, 1, it.first, 0);
                    }
                } else {
                    self.device.cmd_draw_indexed(self.cmd_buf, it.count, 1, it.first, 0, 0);
                }
            }
            if in_pass {
                self.device.cmd_end_render_pass(self.cmd_buf);
            }

            // Record active GPU-VS shaders in this frame for the next frame's GPU watchdog.
            self.last_frame_gpu_vs.clear();
            for it in &items {
                if it.vs_ds != vk::DescriptorSet::null() {
                    let va = gens[it.gen].vs_addr;
                    if va != 0 && !self.last_frame_gpu_vs.contains(&va) {
                        self.last_frame_gpu_vs.push(va);
                    }
                }
            }

            // Show the display buffer.
            let sub = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
            let layers = vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 };
            if !cleared.contains(&display.0) && *self.rt_layouts.get(&display.0).unwrap_or(&vk::ImageLayout::UNDEFINED) == vk::ImageLayout::UNDEFINED {
                self.rt_barrier(display.0, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
                let img = self.targets[&display.0].image;
                let c = vk::ClearColorValue { float32: clear_rgba };
                self.device.cmd_clear_color_image(self.cmd_buf, img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &c, &[sub]);
            }
            let (dw, dh) = (display.1 as i32, display.2 as i32);
            let disp_img = self.targets[&display.0].image;

            if use_fg {
                // Step 1: Copy display buffer into current FrameGen history slot & finish offscreen render.
                let hist_img = self.fg_hist[self.fg_idx].as_ref().unwrap().image;
                self.rt_barrier(display.0, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
                let hist_to_dst = [vk::ImageMemoryBarrier::default()
                    .image(hist_img)
                    .subresource_range(sub)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
                self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &hist_to_dst);
                let hist_blit = vk::ImageBlit {
                    src_subresource: layers,
                    src_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: dw, y: dh, z: 1 }],
                    dst_subresource: layers,
                    dst_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: dw, y: dh, z: 1 }],
                };
                self.device.cmd_blit_image(self.cmd_buf, disp_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, hist_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[hist_blit], vk::Filter::NEAREST);
                let hist_to_read = [vk::ImageMemoryBarrier::default()
                    .image(hist_img)
                    .subresource_range(sub)
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)];
                self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::DependencyFlags::empty(), &[], &[], &hist_to_read);
                if disp_dset.is_some() {
                    self.rt_barrier(display.0, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                }
                let _ = self.device.end_command_buffer(self.cmd_buf);
                let cmds = [self.cmd_buf];
                let submit = [vk::SubmitInfo::default().command_buffers(&cmds)];
                if let Err(e) = self.device.queue_submit(self.queue, &submit, self.fence) {
                    eprintln!("[orbis] AUTO-HEAL: fg offscreen queue_submit failed ({e:?})");
                    self.quarantine_active_gpu_vs(&format!("fg submit {e:?}"));
                    if e == vk::Result::ERROR_DEVICE_LOST {
                        self.device_lost = true;
                    }
                    return false;
                }
                if let Err(e) = self.device.wait_for_fences(&[self.fence], true, 400_000_000) {
                    eprintln!("[orbis] AUTO-HEAL: fg offscreen wait_for_fences failed ({e:?})");
                    self.quarantine_active_gpu_vs(&format!("fg fence {e:?}"));
                    if e == vk::Result::ERROR_DEVICE_LOST {
                        self.device_lost = true;
                    }
                    return false;
                }

                let vp = [vk::Viewport { x: 0.0, y: 0.0, width: ew as f32, height: eh as f32, min_depth: 0.0, max_depth: 1.0 }];
                let sc = [vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.swap_extent }];
                let clear_val = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] } }];

                // Step 2: Present (fg_mult - 1) motion-compensated interpolated frames between prev & curr.
                if self.fg_has_prev {
                    let fg_ds = self.fg_dsets[self.fg_idx];
                    for k in 1..self.fg_mult {
                        let Some(inter_idx) = self.acquire_swapchain_image() else {
                            break;
                        };
                        let _ = self.device.reset_fences(&[self.fence]);
                        let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
                        let _ = self.device.begin_command_buffer(self.cmd_buf, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
                        let rp_bi = vk::RenderPassBeginInfo::default()
                            .render_pass(self.swap_rp)
                            .framebuffer(self.swap_fbs[inter_idx as usize])
                            .render_area(sc[0])
                            .clear_values(&clear_val);
                        self.device.cmd_begin_render_pass(self.cmd_buf, &rp_bi, vk::SubpassContents::INLINE);
                        self.device.cmd_set_viewport(self.cmd_buf, 0, &vp);
                        self.device.cmd_set_scissor(self.cmd_buf, 0, &sc);
                        self.device.cmd_bind_pipeline(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.fg_pipe);
                        self.device.cmd_bind_descriptor_sets(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.fg_layout, 0, &[fg_ds], &[]);
                        let t_blend = k as f32 / self.fg_mult as f32;
                        let params = [1.0 / (display.1.max(1) as f32), 1.0 / (display.2.max(1) as f32), t_blend, 0.0f32];
                        let pbytes = std::slice::from_raw_parts(params.as_ptr() as *const u8, 16);
                        self.device.cmd_push_constants(self.cmd_buf, self.fg_layout, vk::ShaderStageFlags::FRAGMENT, 0, pbytes);
                        self.device.cmd_draw(self.cmd_buf, 3, 1, 0, 0);
                        self.device.cmd_end_render_pass(self.cmd_buf);
                        let _ = self.device.end_command_buffer(self.cmd_buf);
                        let wait_sems = [self.image_avail];
                        let sig_sems = [self.render_done];
                        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
                        let cmds = [self.cmd_buf];
                        let submit = [vk::SubmitInfo::default().wait_semaphores(&wait_sems).wait_dst_stage_mask(&wait_stages).command_buffers(&cmds).signal_semaphores(&sig_sems)];
                        if self.device.queue_submit(self.queue, &submit, self.fence).is_err() {
                            break;
                        }
                        self.present_swapchain_image(inter_idx);
                        let _ = self.device.wait_for_fences(&[self.fence], true, 400_000_000);
                    }
                }

                // Step 3: Present the real curr frame (through RCAS if enabled, else linear blit).
                let Some(real_idx) = self.acquire_swapchain_image() else {
                    self.fg_idx ^= 1;
                    self.fg_has_prev = true;
                    return false;
                };
                let _ = self.device.reset_fences(&[self.fence]);
                let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
                let _ = self.device.begin_command_buffer(self.cmd_buf, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
                let wait_stage = if let Some(ds) = disp_dset {
                    let rp_bi = vk::RenderPassBeginInfo::default()
                        .render_pass(self.swap_rp)
                        .framebuffer(self.swap_fbs[real_idx as usize])
                        .render_area(sc[0])
                        .clear_values(&clear_val);
                    self.device.cmd_begin_render_pass(self.cmd_buf, &rp_bi, vk::SubpassContents::INLINE);
                    self.device.cmd_set_viewport(self.cmd_buf, 0, &vp);
                    self.device.cmd_set_scissor(self.cmd_buf, 0, &sc);
                    self.device.cmd_bind_pipeline(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.rcas_pipe);
                    self.device.cmd_bind_descriptor_sets(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.rcas_layout, 0, &[ds], &[]);
                    let params = [1.0 / (display.1.max(1) as f32), 1.0 / (display.2.max(1) as f32), self.rcas_sharpness, 0.0f32];
                    let pbytes = std::slice::from_raw_parts(params.as_ptr() as *const u8, 16);
                    self.device.cmd_push_constants(self.cmd_buf, self.rcas_layout, vk::ShaderStageFlags::FRAGMENT, 0, pbytes);
                    self.device.cmd_draw(self.cmd_buf, 3, 1, 0, 0);
                    self.device.cmd_end_render_pass(self.cmd_buf);
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                } else {
                    let swap_img = self.swap_images[real_idx as usize];
                    let to_dst = [vk::ImageMemoryBarrier::default()
                        .image(swap_img)
                        .subresource_range(sub)
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
                    self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_dst);
                    let blit = vk::ImageBlit {
                        src_subresource: layers,
                        src_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: dw, y: dh, z: 1 }],
                        dst_subresource: layers,
                        dst_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: ew as i32, y: eh as i32, z: 1 }],
                    };
                    self.device.cmd_blit_image(self.cmd_buf, disp_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::LINEAR);
                    let to_present = [vk::ImageMemoryBarrier::default()
                        .image(swap_img)
                        .subresource_range(sub)
                        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
                    self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[], &to_present);
                    vk::PipelineStageFlags::TRANSFER
                };
                let _ = self.device.end_command_buffer(self.cmd_buf);
                let wait_sems = [self.image_avail];
                let sig_sems = [self.render_done];
                let wait_stages = [wait_stage];
                let cmds = [self.cmd_buf];
                let submit = [vk::SubmitInfo::default().wait_semaphores(&wait_sems).wait_dst_stage_mask(&wait_stages).command_buffers(&cmds).signal_semaphores(&sig_sems)];
                if let Err(e) = self.device.queue_submit(self.queue, &submit, self.fence) {
                    eprintln!("[orbis] AUTO-HEAL: fg final queue_submit failed ({e:?})");
                    if e == vk::Result::ERROR_DEVICE_LOST {
                        self.device_lost = true;
                    }
                    return false;
                }
                self.present_swapchain_image(real_idx);
                self.fg_idx ^= 1;
                self.fg_has_prev = true;
                return true;
            }

            let img_idx = initial_img_idx.unwrap();
            let swap_img = self.swap_images[img_idx as usize];
            let wait_stage = if let Some(ds) = disp_dset {
                self.rt_barrier(display.0, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                let vp = [vk::Viewport { x: 0.0, y: 0.0, width: ew as f32, height: eh as f32, min_depth: 0.0, max_depth: 1.0 }];
                let sc = [vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.swap_extent }];
                let clear_val = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] } }];
                let rp_bi = vk::RenderPassBeginInfo::default()
                    .render_pass(self.swap_rp)
                    .framebuffer(self.swap_fbs[img_idx as usize])
                    .render_area(sc[0])
                    .clear_values(&clear_val);
                self.device.cmd_begin_render_pass(self.cmd_buf, &rp_bi, vk::SubpassContents::INLINE);
                self.device.cmd_set_viewport(self.cmd_buf, 0, &vp);
                self.device.cmd_set_scissor(self.cmd_buf, 0, &sc);
                self.device.cmd_bind_pipeline(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.rcas_pipe);
                self.device.cmd_bind_descriptor_sets(self.cmd_buf, vk::PipelineBindPoint::GRAPHICS, self.rcas_layout, 0, &[ds], &[]);
                let params = [1.0 / (display.1.max(1) as f32), 1.0 / (display.2.max(1) as f32), self.rcas_sharpness, 0.0f32];
                let pbytes = std::slice::from_raw_parts(params.as_ptr() as *const u8, 16);
                self.device.cmd_push_constants(self.cmd_buf, self.rcas_layout, vk::ShaderStageFlags::FRAGMENT, 0, pbytes);
                self.device.cmd_draw(self.cmd_buf, 3, 1, 0, 0);
                self.device.cmd_end_render_pass(self.cmd_buf);
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
            } else {
                self.rt_barrier(display.0, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
                let to_dst = [vk::ImageMemoryBarrier::default()
                    .image(swap_img)
                    .subresource_range(sub)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
                self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_dst);
                let blit = vk::ImageBlit {
                    src_subresource: layers,
                    src_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: dw, y: dh, z: 1 }],
                    dst_subresource: layers,
                    dst_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: ew as i32, y: eh as i32, z: 1 }],
                };
                self.device.cmd_blit_image(self.cmd_buf, disp_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::LINEAR);
                let mut swap_layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
                if dump {
                    let b = [vk::ImageMemoryBarrier::default()
                        .image(swap_img)
                        .subresource_range(sub)
                        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)];
                    self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &b);
                    let region = [vk::BufferImageCopy::default().image_subresource(layers).image_extent(vk::Extent3D { width: ew, height: eh, depth: 1 })];
                    self.device.cmd_copy_image_to_buffer(self.cmd_buf, swap_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, self.staging_buf, &region);
                    swap_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
                }
                let to_present = [vk::ImageMemoryBarrier::default()
                    .image(swap_img)
                    .subresource_range(sub)
                    .old_layout(swap_layout)
                    .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
                self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[], &to_present);
                vk::PipelineStageFlags::TRANSFER
            };
            let _ = self.device.end_command_buffer(self.cmd_buf);

            let wait_sems = [self.image_avail];
            let sig_sems = [self.render_done];
            let wait_stages = [wait_stage];
            let cmds = [self.cmd_buf];
            let submit = [vk::SubmitInfo::default().wait_semaphores(&wait_sems).wait_dst_stage_mask(&wait_stages).command_buffers(&cmds).signal_semaphores(&sig_sems)];
            if let Err(e) = self.device.queue_submit(self.queue, &submit, self.fence) {
                eprintln!("[orbis] AUTO-HEAL: queue_submit failed ({e:?})");
                self.quarantine_active_gpu_vs(&format!("queue_submit {e:?}"));
                if e == vk::Result::ERROR_DEVICE_LOST {
                    self.device_lost = true;
                }
                return false;
            }
            self.present_swapchain_image(img_idx);
            if dump {
                if let Err(e) = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000) {
                    eprintln!("[orbis] AUTO-HEAL: dump wait_for_fences failed ({e:?})");
                    self.quarantine_active_gpu_vs(&format!("dump fence {e:?}"));
                    if e == vk::Result::ERROR_DEVICE_LOST {
                        self.device_lost = true;
                    }
                    return false;
                }
                let bytes = (ew * eh * 4) as usize;
                if let Ok(p) = self.device.map_memory(self.staging_mem, 0, bytes as u64, vk::MemoryMapFlags::empty()) {
                    let px = std::slice::from_raw_parts(p as *const u8, bytes);
                    let mut bmp = Vec::with_capacity(54 + bytes);
                    bmp.extend_from_slice(b"BM");
                    bmp.extend_from_slice(&((54 + bytes) as u32).to_le_bytes());
                    bmp.extend_from_slice(&0u32.to_le_bytes());
                    bmp.extend_from_slice(&54u32.to_le_bytes());
                    bmp.extend_from_slice(&40u32.to_le_bytes());
                    bmp.extend_from_slice(&(ew as i32).to_le_bytes());
                    bmp.extend_from_slice(&(-(eh as i32)).to_le_bytes());
                    bmp.extend_from_slice(&1u16.to_le_bytes());
                    bmp.extend_from_slice(&32u16.to_le_bytes());
                    bmp.extend_from_slice(&[0u8; 24]);
                    bmp.extend_from_slice(px);
                    self.device.unmap_memory(self.staging_mem);
                    let dir = dump_dir.unwrap_or_default();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::write(format!("{dir}/vk{n:05}.bmp"), bmp);
                }
            }
            true
        }
    }

    /// Pipeline for a generic draw: pass-through vertex shader + the translated pixel shader.
    /// Failures are cached as a null pipeline (the draw is then skipped).
    unsafe fn ensure_gen_pipeline(&mut self, gd: &GenDraw) -> vk::Pipeline {
        if let Some(&p) = self.gen_pipes.get(&gd.key) {
            return p;
        }
        let p = self.create_gen_pipeline(gd).unwrap_or_else(|e| {
            eprintln!("[orbis] Vulkan: generic pipeline for ps {:#x}: {e}", gd.key.0);
            if gd.vs_spirv.is_some() {
                quarantine_gpu_vs(gd.vs_addr, &format!("pipeline compile failed: {e}"));
            }
            vk::Pipeline::null()
        });
        self.gen_pipes.insert(gd.key, p);
        p
    }

    unsafe fn create_gen_pipeline(&mut self, gd: &GenDraw) -> Result<vk::Pipeline, String> {
        if self.gen_dset_layout == vk::DescriptorSetLayout::null() {
            let bindings: [vk::DescriptorSetLayoutBinding; 4] = std::array::from_fn(|k| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(k as u32)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            });
            self.gen_dset_layout = self
                .device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
                .map_err(|e| format!("gen_dset_layout: {e}"))?;
        }
        if self.gen_layout == vk::PipelineLayout::null() {
            let dsls = [self.gen_dset_layout];
            let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::FRAGMENT, offset: 0, size: 256 }];
            self.gen_layout = self
                .device
                .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls).push_constant_ranges(&pcr), None)
                .map_err(|e| format!("layout: {e}"))?;
        }
        if self.vs_dset_layout == vk::DescriptorSetLayout::null() {
            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(3)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(4)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX),
            ];
            self.vs_dset_layout = self
                .device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
                .map_err(|e| format!("vs_dset_layout: {e}"))?;
        }
        if self.gpu_vs_layout == vk::PipelineLayout::null() {
            let dsls = [self.gen_dset_layout, self.vs_dset_layout];
            let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::FRAGMENT, offset: 0, size: 256 }];
            self.gpu_vs_layout = self
                .device
                .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls).push_constant_ranges(&pcr), None)
                .map_err(|e| format!("gpu_vs_layout: {e}"))?;
        }
        if self.gen_vs == vk::ShaderModule::null() {
            let code = crate::rdna::ps::passthrough_vs();
            self.gen_vs = self
                .device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)
                .map_err(|e| format!("vs: {e}"))?;
        }
        let is_gpu_vs = gd.vs_spirv.is_some();
        let vs_mod = if let Some(spv) = gd.vs_spirv.as_ref() {
            let key = std::sync::Arc::as_ptr(spv) as usize;
            if let Some(&m) = self.vs_modules.get(&key) {
                m
            } else {
                let m = self
                    .device
                    .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(spv), None)
                    .map_err(|e| format!("gpu_vs: {e}"))?;
                self.vs_modules.insert(key, m);
                m
            }
        } else {
            self.gen_vs
        };
        let fs_mod = self
            .device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&gd.spirv), None)
            .map_err(|e| format!("fs: {e}"))?;
        let entry = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs_mod).name(entry),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs_mod).name(entry),
        ];
        let vbind = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<GVertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let vattr: Vec<vk::VertexInputAttributeDescription> = (0..9u32)
            .map(|k| vk::VertexInputAttributeDescription { location: k, binding: 0, format: vk::Format::R32G32B32A32_SFLOAT, offset: k * 16 })
            .collect();
        let vi = if is_gpu_vs {
            vk::PipelineVertexInputStateCreateInfo::default()
        } else {
            vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&vbind)
                .vertex_attribute_descriptions(&vattr)
        };
        let topology = match gd.prim {
            1 => vk::PrimitiveTopology::POINT_LIST,
            2 => vk::PrimitiveTopology::LINE_LIST,
            3 => vk::PrimitiveTopology::LINE_STRIP,
            5 => vk::PrimitiveTopology::TRIANGLE_STRIP,
            _ => vk::PrimitiveTopology::TRIANGLE_LIST,
        };
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(topology);
        let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // CB_BLEND0_CONTROL → Vulkan blend state.
        let bc = gd.blend;
        let fac = |f: u32| match f {
            0 => vk::BlendFactor::ZERO,
            1 => vk::BlendFactor::ONE,
            2 => vk::BlendFactor::SRC_COLOR,
            3 => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
            4 => vk::BlendFactor::SRC_ALPHA,
            5 => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
            6 => vk::BlendFactor::DST_ALPHA,
            7 => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
            8 => vk::BlendFactor::DST_COLOR,
            9 => vk::BlendFactor::ONE_MINUS_DST_COLOR,
            10 => vk::BlendFactor::SRC_ALPHA_SATURATE,
            _ => vk::BlendFactor::ONE,
        };
        let op = |f: u32| match f {
            1 => vk::BlendOp::SUBTRACT,
            2 => vk::BlendOp::MIN,
            3 => vk::BlendOp::MAX,
            4 => vk::BlendOp::REVERSE_SUBTRACT,
            _ => vk::BlendOp::ADD,
        };
        let enable = bc & (1 << 30) != 0;
        let separate = bc & (1 << 29) != 0;
        let (cs, cf, cd) = (bc & 0x1F, (bc >> 5) & 7, (bc >> 8) & 0x1F);
        let (a_s, a_f, a_d) = if separate { ((bc >> 16) & 0x1F, (bc >> 21) & 7, (bc >> 24) & 0x1F) } else { (cs, cf, cd) };
        let cba = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(enable)
            .src_color_blend_factor(fac(cs))
            .dst_color_blend_factor(fac(cd))
            .color_blend_op(op(cf))
            .src_alpha_blend_factor(fac(a_s))
            .dst_alpha_blend_factor(fac(a_d))
            .alpha_blend_op(op(a_f))
            .color_write_mask(vk::ColorComponentFlags::from_raw(gd.wmask & 0xF))];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
        let dc = gd.depth;
        let stencil_en = (dc & 1) != 0;
        let dyn_states_base = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dyn_states_stencil = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::STENCIL_COMPARE_MASK,
            vk::DynamicState::STENCIL_WRITE_MASK,
            vk::DynamicState::STENCIL_REFERENCE,
        ];
        let ds = vk::PipelineDynamicStateCreateInfo::default()
            .dynamic_states(if stencil_en { &dyn_states_stencil[..] } else { &dyn_states_base[..] });
        let cmp_op = |v: u32| match v & 7 {
            0 => vk::CompareOp::NEVER,
            1 => vk::CompareOp::LESS,
            2 => vk::CompareOp::EQUAL,
            3 => vk::CompareOp::LESS_OR_EQUAL,
            4 => vk::CompareOp::GREATER,
            5 => vk::CompareOp::NOT_EQUAL,
            6 => vk::CompareOp::GREATER_OR_EQUAL,
            _ => vk::CompareOp::ALWAYS,
        };
        let sop = |v: u32| match v & 0xF {
            0 => vk::StencilOp::KEEP,
            1 => vk::StencilOp::ZERO,
            2..=4 => vk::StencilOp::REPLACE,
            5 => vk::StencilOp::INCREMENT_AND_CLAMP,
            6 => vk::StencilOp::DECREMENT_AND_CLAMP,
            7 => vk::StencilOp::INVERT,
            8 => vk::StencilOp::INCREMENT_AND_WRAP,
            9 => vk::StencilOp::DECREMENT_AND_WRAP,
            _ => vk::StencilOp::KEEP,
        };
        let zop = cmp_op(dc >> 4);
        let sc = gd.stencil_ctl;
        let front_s = vk::StencilOpState {
            fail_op: sop(sc),
            pass_op: sop(sc >> 4),
            depth_fail_op: sop(sc >> 8),
            compare_op: cmp_op(dc >> 8),
            compare_mask: 0xFF,
            write_mask: 0xFF,
            reference: 0,
        };
        let back_s = if (dc & 0x80) != 0 {
            vk::StencilOpState {
                fail_op: sop(sc >> 12),
                pass_op: sop(sc >> 16),
                depth_fail_op: sop(sc >> 20),
                compare_op: cmp_op(dc >> 20),
                compare_mask: 0xFF,
                write_mask: 0xFF,
                reference: 0,
            }
        } else {
            front_s
        };
        let dss = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(dc & 2 != 0)
            .depth_write_enable(dc & 2 != 0 && dc & 4 != 0)
            .depth_compare_op(zop)
            .stencil_test_enable(stencil_en)
            .front(front_s)
            .back(back_s);
        let gp_ci = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .color_blend_state(&cb)
            .depth_stencil_state(&dss)
            .dynamic_state(&ds)
            .layout(if is_gpu_vs { self.gpu_vs_layout } else { self.gen_layout })
            .render_pass(self.rt_passes().map(|p| p.1).unwrap_or(self.swap_rp))];
        let r = self.device.create_graphics_pipelines(vk::PipelineCache::null(), &gp_ci, None).map_err(|(_, e)| format!("pipeline: {e}"));
        self.device.destroy_shader_module(fs_mod, None);
        Ok(r?[0])
    }

    unsafe fn create_quad_pipeline(&self, rp: vk::RenderPass, pl: vk::PipelineLayout) -> Result<vk::Pipeline, String> {
        let vs_code = crate::shader::spv_vertex_vs();
        let fs_code = crate::shader::spv_textured_color_fs();
        let vs_mod = self
            .device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vs_code), None)
            .map_err(|e| format!("quad vs: {e}"))?;
        let fs_mod = self
            .device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&fs_code), None)
            .map_err(|e| format!("quad fs: {e}"))?;
        let entry = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs_mod).name(entry),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs_mod).name(entry),
        ];
        let vbind = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<Vertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let vattr = [
            vk::VertexInputAttributeDescription { location: 0, binding: 0, format: vk::Format::R32G32B32A32_SFLOAT, offset: 0 },
            vk::VertexInputAttributeDescription { location: 1, binding: 0, format: vk::Format::R32G32_SFLOAT, offset: 16 },
            vk::VertexInputAttributeDescription { location: 2, binding: 0, format: vk::Format::R32G32B32A32_SFLOAT, offset: 24 },
        ];
        let vi = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&vbind)
            .vertex_attribute_descriptions(&vattr);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let cba = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
        let no_depth = vk::PipelineDepthStencilStateCreateInfo::default();
        let gp_ci = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .color_blend_state(&cb)
            .depth_stencil_state(&no_depth)
            .dynamic_state(&ds)
            .layout(pl)
            .render_pass(rp)];
        let pipe = self.device.create_graphics_pipelines(vk::PipelineCache::null(), &gp_ci, None).map_err(|(_, e)| format!("quad pipe: {e}"))?[0];
        self.device.destroy_shader_module(vs_mod, None);
        self.device.destroy_shader_module(fs_mod, None);
        Ok(pipe)
    }

    unsafe fn ensure_staging(&mut self, bytes: usize) -> bool {
        if self.staging_cap >= bytes && self.staging_buf != vk::Buffer::null() {
            return true;
        }
        if self.staging_buf != vk::Buffer::null() {
            self.device.destroy_buffer(self.staging_buf, None);
            self.device.free_memory(self.staging_mem, None);
            self.staging_buf = vk::Buffer::null();
            self.staging_mem = vk::DeviceMemory::null();
            self.staging_cap = 0;
        }
        let Ok(buf) = self.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(bytes as u64)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        ) else {
            return false;
        };
        let req = self.device.get_buffer_memory_requirements(buf);
        let Some(mem_idx) = self.find_mem_type(
            req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) else {
            self.device.destroy_buffer(buf, None);
            return false;
        };
        let Ok(mem) = self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mem_idx),
            None,
        ) else {
            self.device.destroy_buffer(buf, None);
            return false;
        };
        if self.device.bind_buffer_memory(buf, mem, 0).is_err() {
            self.device.destroy_buffer(buf, None);
            self.device.free_memory(mem, None);
            return false;
        }
        self.staging_buf = buf;
        self.staging_mem = mem;
        self.staging_cap = bytes;
        true
    }

    /// Uploads a host/guest RGBA8/BGRA8 texture to a VkImage and blits it via the SPIR-V pipeline.
    pub fn upload_and_present_rgba(&mut self, pixels: &[u32], width: u32, height: u32) -> bool {
        if !self.blit_ready || width == 0 || height == 0 || pixels.len() < (width as usize * height as usize) {
            return false;
        }
        let byte_len = (width as usize) * (height as usize) * 4;
        unsafe {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if !self.ensure_staging(byte_len) {
                return false;
            }
            let Ok(mapped) = self.device.map_memory(self.staging_mem, 0, byte_len as u64, vk::MemoryMapFlags::empty()) else {
                return false;
            };
            std::ptr::copy_nonoverlapping(pixels.as_ptr() as *const u8, mapped as *mut u8, byte_len);
            self.device.unmap_memory(self.staging_mem);

            let need_tex = match &self.upload_tex {
                Some(t) => t.width != width || t.height != height,
                None => true,
            };
            if need_tex {
                let Ok(img) = self.device.create_image(
                    &vk::ImageCreateInfo::default()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(vk::Format::B8G8R8A8_UNORM)
                        .extent(vk::Extent3D { width, height, depth: 1 })
                        .mip_levels(1)
                        .array_layers(1)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
                        .initial_layout(vk::ImageLayout::UNDEFINED),
                    None,
                ) else {
                    return false;
                };
                let req = self.device.get_image_memory_requirements(img);
                let Some(mi) = self.find_mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL) else {
                    return false;
                };
                let Ok(mem) = self.device.allocate_memory(
                    &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mi),
                    None,
                ) else {
                    return false;
                };
                let _ = self.device.bind_image_memory(img, mem, 0);
                let Ok(view) = self.device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(img)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(vk::Format::B8G8R8A8_UNORM)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        }),
                    None,
                ) else {
                    return false;
                };
                self.upload_tex = Some(VkTarget { image: img, memory: mem, view, width, height });
            }
            let tex_img = self.upload_tex.as_ref().unwrap().image;

            let Some(img_idx) = self.acquire_swapchain_image() else {
                return false;
            };
            let _ = self.device.reset_fences(&[self.fence]);
            let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
            let _ = self.device.begin_command_buffer(
                self.cmd_buf,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            );
            let swap_img = self.swap_images[img_idx as usize];
            let sub = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let to_tex_dst = vk::ImageMemoryBarrier::default()
                .image(tex_img)
                .subresource_range(sub)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            let to_swap_dst = vk::ImageMemoryBarrier::default()
                .image(swap_img)
                .subresource_range(sub)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            self.device.cmd_pipeline_barrier(
                self.cmd_buf,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_tex_dst, to_swap_dst],
            );
            let region = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D { width, height, depth: 1 })];
            self.device.cmd_copy_buffer_to_image(
                self.cmd_buf,
                self.staging_buf,
                tex_img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &region,
            );
            let to_tex_src = vk::ImageMemoryBarrier::default()
                .image(tex_img)
                .subresource_range(sub)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
            self.device.cmd_pipeline_barrier(
                self.cmd_buf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_tex_src],
            );
            let blit = [vk::ImageBlit {
                src_subresource: vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 },
                src_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: width as i32, y: height as i32, z: 1 }],
                dst_subresource: vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 },
                dst_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: self.swap_extent.width as i32, y: self.swap_extent.height as i32, z: 1 }],
            }];
            self.device.cmd_blit_image(
                self.cmd_buf,
                tex_img,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                swap_img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &blit,
                vk::Filter::LINEAR,
            );
            let to_present = vk::ImageMemoryBarrier::default()
                .image(swap_img)
                .subresource_range(sub)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            self.device.cmd_pipeline_barrier(
                self.cmd_buf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            let _ = self.device.end_command_buffer(self.cmd_buf);

            let wait_sems = [self.image_avail];
            let sig_sems = [self.render_done];
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            let cmds = [self.cmd_buf];
            let submit = [vk::SubmitInfo::default()
                .wait_semaphores(&wait_sems)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&cmds)
                .signal_semaphores(&sig_sems)];
            if self.device.queue_submit(self.queue, &submit, self.fence).is_err() {
                return false;
            }
            self.present_swapchain_image(img_idx);
            true
        }
    }

    pub fn find_mem_type(&self, type_bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
        for i in 0..self.mem_props.memory_type_count {
            if (type_bits & (1 << i)) != 0 && self.mem_props.memory_types[i as usize].property_flags.contains(flags) {
                return Some(i);
            }
        }
        None
    }

    /// Compiles and caches a Vulkan graphics pipeline for a bound PS5 (VS, PS) shader pair.
    pub fn ensure_shader_pipeline(&mut self, vs_addr: usize, ps_addr: usize, vs_spv: &[u32], ps_spv: &[u32], has_tex: bool) -> bool {
        if !self.blit_ready || vs_addr == 0 || ps_addr == 0 {
            return false;
        }
        if self.pipelines.contains_key(&(vs_addr, ps_addr)) {
            return true;
        }
        unsafe {
            let Ok(vs_mod) = self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(vs_spv), None) else {
                return false;
            };
            let Ok(fs_mod) = self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(ps_spv), None) else {
                self.device.destroy_shader_module(vs_mod, None);
                return false;
            };
            let pc_ranges = [vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::FRAGMENT,
                offset: 0,
                size: 16,
            }];
            let dsls = [self.blit_dset_layout];
            let pl_ci = if has_tex {
                vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls)
            } else {
                vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&pc_ranges)
            };
            let Ok(pl) = self.device.create_pipeline_layout(&pl_ci, None) else {
                self.device.destroy_shader_module(vs_mod, None);
                self.device.destroy_shader_module(fs_mod, None);
                return false;
            };
            let entry = c"main";
            let stages = [
                vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs_mod).name(entry),
                vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs_mod).name(entry),
            ];
            let vi = vk::PipelineVertexInputStateCreateInfo::default();
            let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
            let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
            let rs = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .line_width(1.0);
            let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let cba = [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
            let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
            let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
            let gp_ci = [vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vi)
                .input_assembly_state(&ia)
                .viewport_state(&vp)
                .rasterization_state(&rs)
                .multisample_state(&ms)
                .color_blend_state(&cb)
                .dynamic_state(&ds)
                .layout(pl)
                .render_pass(self.blit_rp)];
            let pipe = self.device.create_graphics_pipelines(vk::PipelineCache::null(), &gp_ci, None).ok().map(|v| v[0]);
            self.device.destroy_shader_module(vs_mod, None);
            self.device.destroy_shader_module(fs_mod, None);
            if let Some(p) = pipe {
                self.pipelines.insert((vs_addr, ps_addr), (pl, p, self.blit_rp));
                true
            } else {
                self.device.destroy_pipeline_layout(pl, None);
                false
            }
        }
    }

    pub fn ensure_target(&mut self, addr: usize, width: u32, height: u32) -> Option<&VkTarget> {
        if addr == 0 || width == 0 || height == 0 {
            return None;
        }
        let recreate = match self.targets.get(&addr) {
            Some(t) => t.width != width || t.height != height,
            None => true,
        };
        if recreate {
            unsafe {
                let img_ci = vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::B8G8R8A8_UNORM)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                    .initial_layout(vk::ImageLayout::UNDEFINED);
                let image = self.device.create_image(&img_ci, None).ok()?;
                let req = self.device.get_image_memory_requirements(image);
                let mem_idx = self.find_mem_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
                let memory = self
                    .device
                    .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(mem_idx), None)
                    .ok()?;
                self.device.bind_image_memory(image, memory, 0).ok()?;
                let view = self
                    .device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(vk::Format::B8G8R8A8_UNORM)
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0,
                                level_count: 1,
                                base_array_layer: 0,
                                layer_count: 1,
                            }),
                        None,
                    )
                    .ok()?;
                if let Some(old) = self.targets.insert(addr, VkTarget { image, memory, view, width, height }) {
                    self.device.destroy_image_view(old.view, None);
                    self.device.destroy_image(old.image, None);
                    self.device.free_memory(old.memory, None);
                }
            }
        }
        self.targets.get(&addr)
    }

    /// Presents either a Vulkan render target (if drawn to) or clears the swapchain image.
    pub fn present_target(&mut self, target_addr: usize, clear_rgba: [f32; 4]) -> bool {
        unsafe {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            let Some(img_idx) = self.acquire_swapchain_image() else {
                return false;
            };
            let _ = self.device.reset_fences(&[self.fence]);
            let _ = self.device.reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty());
            let _ = self.device.begin_command_buffer(
                self.cmd_buf,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            );
            let swap_img = self.swap_images[img_idx as usize];
            let sub = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            // Transition swapchain image UNDEFINED -> TRANSFER_DST_OPTIMAL
            let to_dst = vk::ImageMemoryBarrier::default()
                .image(swap_img)
                .subresource_range(sub)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            self.device.cmd_pipeline_barrier(
                self.cmd_buf,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_dst],
            );

            if let Some(t) = self.targets.get(&target_addr) {
                let to_src = vk::ImageMemoryBarrier::default()
                    .image(t.image)
                    .subresource_range(sub)
                    .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
                self.device.cmd_pipeline_barrier(
                    self.cmd_buf,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_src],
                );
                let blit = vk::ImageBlit {
                    src_subresource: vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 },
                    src_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: t.width as i32, y: t.height as i32, z: 1 }],
                    dst_subresource: vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 },
                    dst_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: self.swap_extent.width as i32, y: self.swap_extent.height as i32, z: 1 }],
                };
                self.device.cmd_blit_image(
                    self.cmd_buf,
                    t.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    swap_img,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[blit],
                    vk::Filter::LINEAR,
                );
            } else {
                let clear = vk::ClearColorValue { float32: clear_rgba };
                self.device.cmd_clear_color_image(self.cmd_buf, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &clear, &[sub]);
            }

            // Transition swapchain image TRANSFER_DST_OPTIMAL -> PRESENT_SRC_KHR
            let to_present = vk::ImageMemoryBarrier::default()
                .image(swap_img)
                .subresource_range(sub)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            self.device.cmd_pipeline_barrier(
                self.cmd_buf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            let _ = self.device.end_command_buffer(self.cmd_buf);

            let wait_sems = [self.image_avail];
            let sig_sems = [self.render_done];
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            let cmds = [self.cmd_buf];
            let submit = [vk::SubmitInfo::default()
                .wait_semaphores(&wait_sems)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&cmds)
                .signal_semaphores(&sig_sems)];
            if self.device.queue_submit(self.queue, &submit, self.fence).is_err() {
                return false;
            }
            self.present_swapchain_image(img_idx);
            true
        }
    }
}
