//! Vulkan GPU backend for POLYMIRA Orbis (runs on the host GPU via `vulkan-1.dll`).
//!
//! Creates a Vulkan 1.2 device (preferring discrete GPUs), attaches a Win32 swapchain to
//! the Orbis window, manages offscreen render targets keyed by guest virtual address, and
//! executes graphics pipelines compiled from PS5 RDNA2 shaders.

use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::sync::{Mutex, OnceLock};

use ash::vk;

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
    pub swap_extent: vk::Extent2D,
    pub swap_format: vk::Format,
    pub image_avail: vk::Semaphore,
    pub render_done: vk::Semaphore,
    pub mem_props: vk::PhysicalDeviceMemoryProperties,
    pub targets: HashMap<usize, VkTarget>,
    pub pipelines: HashMap<(usize, usize), (vk::PipelineLayout, vk::Pipeline, vk::RenderPass)>,
    pub blit_ready: bool,
    pub blit_rp: vk::RenderPass,
    pub blit_layout: vk::PipelineLayout,
    pub blit_pipe: vk::Pipeline,
    pub blit_dset_layout: vk::DescriptorSetLayout,
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
            swap_extent,
            swap_format: fmt.format,
            image_avail,
            render_done,
            mem_props,
            targets: HashMap::new(),
            pipelines: HashMap::new(),
            blit_ready: false,
            blit_rp: vk::RenderPass::null(),
            blit_layout: vk::PipelineLayout::null(),
            blit_pipe: vk::Pipeline::null(),
            blit_dset_layout: vk::DescriptorSetLayout::null(),
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
        self.blit_rp = rp;
        self.blit_layout = pl;
        self.blit_pipe = pipe;
        self.blit_dset_layout = dsl;
        self.blit_ready = true;
        eprintln!("[orbis] Vulkan: SPIR-V fullscreen blit pipeline compiled on GPU");
        Ok(())
    }

    pub fn find_mem_type(&self, type_bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
        for i in 0..self.mem_props.memory_type_count {
            if (type_bits & (1 << i)) != 0 && self.mem_props.memory_types[i as usize].property_flags.contains(flags) {
                return Some(i);
            }
        }
        None
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
