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
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST)
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
        let pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 128,
        }];
        let desc_pool = self
            .device
            .create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default().max_sets(128).pool_sizes(&pool_sizes),
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
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
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

    /// Untiles and uploads a PS5 GFX10 32-bpp RGBA8 texture to a cached VkImage + VkDescriptorSet.
    unsafe fn ensure_sampled_texture(&mut self, addr: usize, width: u32, height: u32, swiz: u32) -> vk::DescriptorSet {
        if addr == 0 || width == 0 || height == 0 || width > 4096 || height > 4096 {
            return self.desc_set;
        }
        if let Some((t, ds)) = self.tex_cache.get(&addr) {
            if t.width == width && t.height == height {
                return *ds;
            }
        }
        if self.tex_cache.len() >= 120 {
            return self.desc_set;
        }
        let want_src = if swiz == 0 {
            (width as usize) * (height as usize) * 4
        } else {
            (((width as usize) + 127) / 128) * (((height as usize) + 127) / 128) * 65536
        };
        let avail = Self::readable_bytes(addr, want_src);
        if avail < 64 {
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
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
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

        eprintln!("[orbis] Vulkan: uploaded PS5 texture @{addr:#x} ({width}x{height}, swiz={swiz}) to GPU");
        self.tex_cache.insert(addr, (VkTarget { image: img, memory: mem, view, width, height }, dset));
        dset
    }

    /// Renders per-draw indexed batches (each with its own PS5 texture) directly into the swapchain framebuffer on the RTX 4060.
    pub fn present_indexed_draws(
        &mut self,
        clear_rgba: [f32; 4],
        vertices: &[Vertex],
        indices: &[u32],
        batches: &[DrawBatch],
    ) -> bool {
        if !self.blit_ready || self.quad_pipe == vk::Pipeline::null() || self.vbuf == vk::Buffer::null() || self.ibuf == vk::Buffer::null() {
            return false;
        }
        let v_bytes = (vertices.len() * std::mem::size_of::<Vertex>()).min(524288);
        let i_bytes = (indices.len() * std::mem::size_of::<u32>()).min(131072);
        let max_idx_count = (i_bytes / 4) as u32;
        if v_bytes == 0 || max_idx_count == 0 {
            return false;
        }
        unsafe {
            // Pre-upload any newly seen PS5 textures before beginning the swapchain render pass
            let mut resolved: Vec<(u32, u32, vk::DescriptorSet)> = Vec::with_capacity(batches.len().max(1));
            if batches.is_empty() {
                resolved.push((0, max_idx_count, self.desc_set));
            } else {
                for b in batches {
                    if b.first_index.saturating_add(b.index_count) <= max_idx_count && b.index_count > 0 {
                        let ds = self.ensure_sampled_texture(b.tex_addr, b.tex_w, b.tex_h, b.tex_swiz);
                        resolved.push((b.first_index, b.index_count, ds));
                    }
                }
            }
            if resolved.is_empty() {
                return false;
            }

            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            if let Ok(vp) = self.device.map_memory(self.vmem, 0, v_bytes as u64, vk::MemoryMapFlags::empty()) {
                std::ptr::copy_nonoverlapping(vertices.as_ptr() as *const u8, vp as *mut u8, v_bytes);
                self.device.unmap_memory(self.vmem);
            }
            if let Ok(ip) = self.device.map_memory(self.imem, 0, i_bytes as u64, vk::MemoryMapFlags::empty()) {
                std::ptr::copy_nonoverlapping(indices.as_ptr() as *const u8, ip as *mut u8, i_bytes);
                self.device.unmap_memory(self.imem);
            }
            let Ok((img_idx, _)) = self.swapchain_loader.acquire_next_image(self.swapchain, 100_000_000, self.image_avail, vk::Fence::null()) else {
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
            for &(first_idx, cnt, ds) in &resolved {
                if ds != last_ds {
                    self.device.cmd_bind_descriptor_sets(
                        self.cmd_buf,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.blit_layout,
                        0,
                        &[ds],
                        &[],
                    );
                    last_ds = ds;
                }
                self.device.cmd_draw_indexed(self.cmd_buf, cnt, 1, first_idx, 0, 0);
            }
            self.device.cmd_end_render_pass(self.cmd_buf);
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
            let scs = [self.swapchain];
            let idxs = [img_idx];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&sig_sems)
                .swapchains(&scs)
                .image_indices(&idxs);
            let _ = self.swapchain_loader.queue_present(self.queue, &present);
            true
        }
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
            let tex_view = self.upload_tex.as_ref().unwrap().view;
            let img_info = [vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(tex_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let write = [vk::WriteDescriptorSet::default()
                .dst_set(self.desc_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&img_info)];
            self.device.update_descriptor_sets(&write, &[]);

            let Ok((img_idx, _)) = self.swapchain_loader.acquire_next_image(self.swapchain, 100_000_000, self.image_avail, vk::Fence::null()) else {
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
            let scs = [self.swapchain];
            let idxs = [img_idx];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&sig_sems)
                .swapchains(&scs)
                .image_indices(&idxs);
            let _ = self.swapchain_loader.queue_present(self.queue, &present);
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
                    .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST)
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
                self.targets.insert(addr, VkTarget { image, memory, view, width, height });
            }
        }
        self.targets.get(&addr)
    }

    /// Presents either a Vulkan render target (if drawn to) or clears the swapchain image.
    pub fn present_target(&mut self, target_addr: usize, clear_rgba: [f32; 4]) -> bool {
        unsafe {
            let _ = self.device.wait_for_fences(&[self.fence], true, 1_000_000_000);
            let Ok((img_idx, _)) = self.swapchain_loader.acquire_next_image(self.swapchain, 100_000_000, self.image_avail, vk::Fence::null()) else {
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
            let scs = [self.swapchain];
            let idxs = [img_idx];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&sig_sems)
                .swapchains(&scs)
                .image_indices(&idxs);
            let _ = self.swapchain_loader.queue_present(self.queue, &present);
            true
        }
    }
}
