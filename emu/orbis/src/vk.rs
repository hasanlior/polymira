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
    /// DB_DEPTH_CONTROL (bit 1 test, bit 2 write, bits 4..6 compare function).
    pub depth: u32,
    /// DB_DEPTH_CLEAR: value a cleared depth buffer starts with.
    pub zclear: f32,
    /// Optional GPU-translated SPIR-V vertex shader (`rdna::vs::translate`).
    pub vs_spirv: Option<std::sync::Arc<Vec<u32>>>,
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
    /// Guest VB cache: addr -> (buf, mem, cap, last_checksum, last_frame_used).
    pub guest_vb_cache: HashMap<usize, (vk::Buffer, vk::DeviceMemory, usize, u64, u64)>,
    pub guest_vb_checked: HashMap<usize, u64>,
    pub vs_dsets: HashMap<[u64; 4], vk::DescriptorSet>,
}

unsafe impl Send for VkRenderer {}

static RENDERER: OnceLock<Mutex<Option<VkRenderer>>> = OnceLock::new();

pub fn with_renderer<R>(f: impl FnOnce(&mut VkRenderer) -> R) -> Option<R> {
    let m = RENDERER.get()?;
    let mut g = m.lock().ok()?;
    g.as_mut().map(f)
}

/// Initializes Vulkan on the given Win32 HWND (idempotent; no-op when ORBIS_HIDDEN=1 or no Vulkan).
#[cfg(windows)]
pub fn init_window(hwnd: *mut c_void, hinstance: *mut c_void, width: u32, height: u32) {
    if std::env::var_os("ORBIS_HIDDEN").is_some() || std::env::var_os("ORBIS_NO_VK").is_some() {
        return;
    }
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
        eprintln!("[orbis] Vulkan: using {gpu_name} (queue family {queue_family})");

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
            .present_mode(vk::PresentModeKHR::FIFO)
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

        eprintln!("[orbis] Vulkan: swapchain {}x{} ({} images, {:?})", swap_extent.width, swap_extent.height, swap_images.len(), fmt.format);
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
            vs_dsets: HashMap::new(),
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
        self.blit_ready = true;
        eprintln!(
            "[orbis] Vulkan: SPIR-V fullscreen blit + indexed quad pipelines compiled on GPU (quad={}, fbs={})",
            self.quad_pipe != vk::Pipeline::null(),
            self.swap_fbs.len()
        );
        Ok(())
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
                .present_mode(vk::PresentModeKHR::FIFO)
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
            Err(_) => None,
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
            let end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
            end.saturating_sub(addr).min(max_len)
        }
    }

    #[cfg(not(windows))]
    fn readable_bytes(_addr: usize, _max_len: usize) -> usize {
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
        let sum = {
            let words = avail / 8;
            let step = if bpp == 1 { 1 } else { (words / 512).max(1) };
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
                        .format(vk::Format::D32_SFLOAT)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .load_op(if clear { vk::AttachmentLoadOp::CLEAR } else { vk::AttachmentLoadOp::LOAD })
                        .store_op(vk::AttachmentStoreOp::STORE)
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
            let avail = Self::readable_bytes(addr, want_src);
            if avail >= 65536 {
                let words = avail / 8;
                let step = (words / 512).max(1);
                let p = addr as *const u64;
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
                    eprintln!("[orbis] ATLAS-SYNC(rt_target) {addr:#x} {w}x{h} avail={avail} nz={nz_cnt}/512");
                }
                if nz_cnt > 0 {
                    let src = std::slice::from_raw_parts(addr as *const u8, avail);
                    let pixels = crate::tiling::untile_32bpp(src, w, h, 27 | (56 << 8) | (0xFAC << 20));
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
                            .format(vk::Format::D32_SFLOAT)
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
                            .format(vk::Format::D32_SFLOAT)
                            .subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::DEPTH, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 }),
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
    unsafe fn ensure_guest_vb(&mut self, addr: usize, need_bytes: usize) -> vk::Buffer {
        if addr < 0x1_0000_0000 || need_bytes == 0 || need_bytes > (64 << 20) {
            return self.vs_dummy_buf;
        }
        let need = (need_bytes + 15) & !15;
        let avail = Self::readable_bytes(addr, need);
        if avail < 16 {
            return self.vs_dummy_buf;
        }
        let upload_len = avail.min(need);
        if self.guest_vb_checked.get(&addr) == Some(&self.frame_no) {
            if let Some(entry) = self.guest_vb_cache.get_mut(&addr) {
                if entry.2 >= upload_len {
                    entry.4 = self.frame_no;
                    return entry.0;
                }
            }
        }
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
                entry.4 = self.frame_no;
                if entry.3 != sum {
                    let mem = entry.1;
                    let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
                    if let Ok(p) = self.device.map_memory(mem, 0, upload_len as u64, vk::MemoryMapFlags::empty()) {
                        std::ptr::copy_nonoverlapping(addr as *const u8, p as *mut u8, upload_len);
                        self.device.unmap_memory(mem);
                    }
                    entry.3 = sum;
                }
                return entry.0;
            }
            // Need larger capacity: free old buffer.
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if let Some((ob, om, _, _, _)) = self.guest_vb_cache.remove(&addr) {
                use ash::vk::Handle;
                let raw = ob.as_raw();
                self.vs_dsets.retain(|k, _| !k.contains(&raw));
                self.device.destroy_buffer(ob, None);
                self.device.free_memory(om, None);
            }
        }
        // Evict stale chunk VBs if cache grows beyond 2048 buffers.
        if self.guest_vb_cache.len() >= 2048 {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            let mut old: Vec<(u64, usize)> = self.guest_vb_cache.iter().map(|(&a, e)| (e.4, a)).collect();
            old.sort_by_key(|&(f, _)| f);
            for &(_, a) in old.iter().take(256) {
                if let Some((ob, om, _, _, _)) = self.guest_vb_cache.remove(&a) {
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
            return self.vs_dummy_buf;
        };
        if let Ok(p) = self.device.map_memory(mem, 0, upload_len as u64, vk::MemoryMapFlags::empty()) {
            std::ptr::copy_nonoverlapping(addr as *const u8, p as *mut u8, upload_len);
            self.device.unmap_memory(mem);
        }
        self.guest_vb_cache.insert(addr, (buf, mem, alloc_sz, sum, self.frame_no));
        buf
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
            // Evict least-recently-used offscreen RTs if cache grows beyond 24 targets (prevents VRAM OOM).
            if self.targets.len() > 24 {
                let mut candidates: Vec<(u64, usize)> = self
                    .targets
                    .keys()
                    .copied()
                    .filter(|&a| !used.iter().any(|&(u, _, _)| u == a))
                    .map(|a| (self.rt_last_used.get(&a).copied().unwrap_or(0), a))
                    .collect();
                candidates.sort_by_key(|&(t, _)| t);
                let to_evict = self.targets.len().saturating_sub(24);
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

            // Resolve every draw before recording: textures, pipelines, targets.
            struct Item {
                rt: (usize, u32, u32),
                first: u32,
                count: u32,
                indexed: bool,
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
                    if self.gvbuf == vk::Buffer::null()
                        || gd.first_vertex.saturating_add(gd.vertex_count) > g_max
                        || (gd.index_count > 0 && (self.gibuf == vk::Buffer::null() || gd.first_index.saturating_add(gd.index_count) > gi_max))
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
                                    let avail = Self::readable_bytes(a, want_src);
                                    if avail >= 64 {
                                        let words = avail / 8;
                                        let step = (words / 512).max(1);
                                        let p = a as *const u64;
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
                                                    eprintln!("[orbis] ATLAS-SYNC(sample) {a:#x} target={tw}x{th} tex={w}x{h} sw={sw_mode} nz={nz_cnt}/512");
                                                }
                                                if tw == w && th == h {
                                                    let src = std::slice::from_raw_parts(a as *const u8, avail);
                                                    let pixels = crate::tiling::untile_32bpp(src, w, h, sw);
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
                    if gd.extra_texs[0].is_some() && gd.vertex_count >= 100 && std::env::var_os("ORBIS_FRAME_STATS").is_some() {
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
                        let vbs = [
                            self.ensure_guest_vb(gd.vs_vbs[0].0, gd.vs_vbs[0].1),
                            self.ensure_guest_vb(gd.vs_vbs[1].0, gd.vs_vbs[1].1),
                            self.ensure_guest_vb(gd.vs_vbs[2].0, gd.vs_vbs[2].1),
                            self.ensure_guest_vb(gd.vs_vbs[3].0, gd.vs_vbs[3].1),
                        ];
                        let Some(vds) = self.vs_dset_for(vbs) else { continue };
                        (vds, gd.vs_param_off)
                    } else {
                        (vk::DescriptorSet::null(), 0)
                    };
                    let [x, y, w, h] = gd.scissor;
                    let scissor = vk::Rect2D { offset: vk::Offset2D { x, y }, extent: vk::Extent2D { width: w.max(0) as u32, height: h.max(0) as u32 } };
                    let (first, count, indexed) = if gd.index_count > 0 {
                        (gd.first_index, gd.index_count, true)
                    } else {
                        (gd.first_vertex, gd.vertex_count, false)
                    };
                    items.push(Item { rt, first, count, indexed, ds, vs_ds, vs_off, pipe, gen: g as usize, scissor, sampled_rts });
                } else if b.index_count > 0 && b.first_index.saturating_add(b.index_count) <= max_idx {
                    let ds = self.ensure_sampled_texture(b.tex_addr, b.tex_w, b.tex_h, b.tex_swiz);
                    items.push(Item { rt: display, first: b.first_index, count: b.index_count, indexed: true, ds, vs_ds: vk::DescriptorSet::null(), vs_off: 0, pipe: vk::Pipeline::null(), gen: 0, scissor: full(display.1, display.2), sampled_rts: [0; 4] });
                }
            }
            if items.is_empty() {
                return false;
            }

            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
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
            let Some(img_idx) = self.acquire_swapchain_image() else {
                return false;
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
                    if a != display.0 && is_cohtml_clear {
                        self.persistent_rts.insert(a);
                    }
                    let has_prev = *self.rt_layouts.get(&a).unwrap_or(&vk::ImageLayout::UNDEFINED) != vk::ImageLayout::UNDEFINED;
                    let is_persistent = a != display.0 && (is_cohtml_clear || self.persistent_rts.contains(&a));
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
                    let ib = if generic { self.gibuf } else { self.ibuf };
                    if ib != cur_ib && ib != vk::Buffer::null() {
                        self.device.cmd_bind_index_buffer(self.cmd_buf, ib, 0, vk::IndexType::UINT32);
                        cur_ib = ib;
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
                    let bytes = std::slice::from_raw_parts(gens[it.gen].slots.as_ptr() as *const u8, 256);
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

            // Show the display buffer.
            let swap_img = self.swap_images[img_idx as usize];
            let sub = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
            let layers = vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 };
            if !cleared.contains(&display.0) && *self.rt_layouts.get(&display.0).unwrap_or(&vk::ImageLayout::UNDEFINED) == vk::ImageLayout::UNDEFINED {
                self.rt_barrier(display.0, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
                let img = self.targets[&display.0].image;
                let c = vk::ClearColorValue { float32: clear_rgba };
                self.device.cmd_clear_color_image(self.cmd_buf, img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &c, &[sub]);
            }
            self.rt_barrier(display.0, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
            let to_dst = [vk::ImageMemoryBarrier::default()
                .image(swap_img)
                .subresource_range(sub)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
            self.device.cmd_pipeline_barrier(self.cmd_buf, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_dst);
            let (dw, dh) = (display.1 as i32, display.2 as i32);
            let blit = vk::ImageBlit {
                src_subresource: layers,
                src_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: dw, y: dh, z: 1 }],
                dst_subresource: layers,
                dst_offsets: [vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: self.swap_extent.width as i32, y: self.swap_extent.height as i32, z: 1 }],
            };
            let disp_img = self.targets[&display.0].image;
            self.device.cmd_blit_image(self.cmd_buf, disp_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::LINEAR);

            // ORBIS_VK_DUMP=<dir>: copy every 120th shown frame back for checking.
            static DUMP_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dump_dir = std::env::var("ORBIS_VK_DUMP").ok();
            let n = DUMP_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (ew, eh) = (self.swap_extent.width, self.swap_extent.height);
            let dump = dump_dir.is_some()
                && (n % 120 == 60 || n % 120 == 61 || ((gverts.len() > 5000 || gi_max > 5000) && n % 30 == 0))
                && self.ensure_staging((ew * eh * 4) as usize);
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
            let _ = self.device.end_command_buffer(self.cmd_buf);

            let wait_sems = [self.image_avail];
            let sig_sems = [self.render_done];
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            let cmds = [self.cmd_buf];
            let submit = [vk::SubmitInfo::default().wait_semaphores(&wait_sems).wait_dst_stage_mask(&wait_stages).command_buffers(&cmds).signal_semaphores(&sig_sems)];
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

    /// Pipeline for a generic draw: pass-through vertex shader + the translated pixel shader.
    /// Failures are cached as a null pipeline (the draw is then skipped).
    unsafe fn ensure_gen_pipeline(&mut self, gd: &GenDraw) -> vk::Pipeline {
        if let Some(&p) = self.gen_pipes.get(&gd.key) {
            return p;
        }
        let p = self.create_gen_pipeline(gd).unwrap_or_else(|e| {
            eprintln!("[orbis] Vulkan: generic pipeline for ps {:#x}: {e}", gd.key.0);
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
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
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
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
        let dc = gd.depth;
        let zop = match (dc >> 4) & 7 {
            0 => vk::CompareOp::NEVER,
            1 => vk::CompareOp::LESS,
            2 => vk::CompareOp::EQUAL,
            3 => vk::CompareOp::LESS_OR_EQUAL,
            4 => vk::CompareOp::GREATER,
            5 => vk::CompareOp::NOT_EQUAL,
            6 => vk::CompareOp::GREATER_OR_EQUAL,
            _ => vk::CompareOp::ALWAYS,
        };
        let dss = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(dc & 2 != 0)
            .depth_write_enable(dc & 2 != 0 && dc & 4 != 0)
            .depth_compare_op(zop);
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
