// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_display_config::ScalingMode;
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use ash::vk;
use gpu_allocator::vulkan::Allocation;
use gpu_allocator::MemoryLocation;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct LinuxWindowId(pub u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

#[crate::processor("com.tatolab.display")]
pub struct LinuxDisplayProcessor {
    gpu_context: Option<GpuContext>,
    window_id: LinuxWindowId,
    window_title: String,
    width: u32,
    height: u32,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    render_thread: Option<JoinHandle<()>>,
}

impl crate::core::ManualProcessor for LinuxDisplayProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::trace!("Display: setup() called");
            self.gpu_context = Some(ctx.gpu.clone());
            self.window_id = LinuxWindowId(NEXT_WINDOW_ID.fetch_add(1, Ordering::SeqCst));
            self.width = self.config.width;
            self.height = self.config.height;
            self.window_title = self
                .config
                .title
                .clone()
                .unwrap_or_else(|| "streamlib Display".to_string());

            self.running = Arc::new(AtomicBool::new(false));

            tracing::info!(
                "Display {}: Setup complete ({}x{})",
                self.window_title,
                self.width,
                self.height
            );

            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("Display {}: Teardown", self.window_title);
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        tracing::trace!(
            "Display {}: start() called — spawning Vulkan + winit render thread",
            self.window_id.0
        );

        let inputs = std::mem::take(&mut self.inputs);
        let running = Arc::clone(&self.running);
        let frame_counter = Arc::clone(&self.frame_counter);
        let window_id = self.window_id.0;
        let width = self.width;
        let height = self.height;
        let window_title = self.window_title.clone();
        let vsync = self.config.vsync.unwrap_or(true);
        let scaling_mode = self
            .config
            .scaling_mode
            .clone()
            .unwrap_or(ScalingMode::Stretch);

        let gpu_context = self
            .gpu_context
            .clone()
            .ok_or_else(|| StreamError::Configuration("GPU context not initialized".into()))?;

        // Get Vulkan handles from the GpuDevice
        let vulkan_device = Arc::clone(&gpu_context.device().inner);

        running.store(true, Ordering::Release);

        let render_thread = std::thread::Builder::new()
            .name(format!("display-{}-render", window_id))
            .spawn(move || {
                tracing::debug!("Display {}: Render thread started", window_id);

                // Use any_thread() to allow event loop on non-main thread.
                let event_loop = match {
                    use winit::platform::x11::EventLoopBuilderExtX11;
                    EventLoop::builder().with_any_thread(true).build()
                } {
                    Ok(el) => el,
                    Err(e) => {
                        tracing::error!(
                            "Display {}: Failed to create event loop: {}",
                            window_id,
                            e
                        );
                        return;
                    }
                };

                let mut app = DisplayEventLoopHandler {
                    window: None,
                    vulkan_device,
                    gpu_context,
                    inputs,
                    running,
                    frame_counter,
                    window_id,
                    width,
                    height,
                    window_title,
                    vsync,
                    scaling_mode,
                    swapchain_state: None,
                    pipeline_state: None,
                    camera_texture_ring: Vec::new(),
                };

                if let Err(e) = event_loop.run_app(&mut app) {
                    tracing::error!("Display {}: Event loop error: {}", window_id, e);
                }

                // Clean up camera texture ring resources
                if !app.camera_texture_ring.is_empty() {
                    let device = app.vulkan_device.device();
                    for tex in app.camera_texture_ring.drain(..) {
                        unsafe {
                            device.destroy_image_view(tex.image_view, None);
                            device.destroy_image(tex.image, None);
                        }
                        let _ = app.vulkan_device.free_gpu_memory(tex.gpu_memory_allocation);
                    }
                }

                // Clean up swapchain resources before exiting
                if let Some(state) = app.swapchain_state.take() {
                    destroy_swapchain_state(&app.vulkan_device, &state);
                }

                // Clean up persistent pipeline resources
                if let Some(ps) = app.pipeline_state.take() {
                    let device = app.vulkan_device.device();
                    unsafe {
                        device.destroy_pipeline(ps.graphics_pipeline, None);
                        device.destroy_pipeline_layout(ps.pipeline_layout, None);
                        device.destroy_descriptor_pool(ps.descriptor_pool, None);
                        device.destroy_descriptor_set_layout(ps.descriptor_set_layout, None);
                        device.destroy_sampler(ps.sampler, None);
                    }
                }

                tracing::debug!("Display {}: Render thread exiting", window_id);
            })
            .map_err(|e| StreamError::Runtime(format!("Failed to spawn render thread: {}", e)))?;

        self.render_thread = Some(render_thread);

        tracing::info!(
            "Display {}: Vulkan + winit rendering started",
            self.window_id.0
        );

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::trace!("Display {}: stop() called", self.window_id.0);

        self.running.store(false, Ordering::Release);

        if let Some(handle) = self.render_thread.take() {
            handle
                .join()
                .map_err(|_| StreamError::Runtime("Render thread panicked".into()))?;
        }

        tracing::info!("Display {}: Stopped", self.window_id.0);

        Ok(())
    }
}

impl LinuxDisplayProcessor::Processor {
    pub fn window_id(&self) -> LinuxWindowId {
        self.window_id
    }

    pub fn set_window_title(&mut self, title: &str) {
        self.window_title = title.to_string();
    }
}

// ---------------------------------------------------------------------------
// Swapchain state — all Vulkan objects for the current swapchain
// ---------------------------------------------------------------------------

struct SwapchainState {
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_format: vk::Format,
    swapchain_extent: vk::Extent2D,
    command_pool: vk::CommandPool,
    /// Per-swapchain-image binary semaphores for acquire/present sync.
    image_available_semaphores: Vec<vk::Semaphore>,
    render_finished_semaphores: Vec<vk::Semaphore>,
    /// Timeline semaphore for multi-flight frame synchronization.
    /// Replaces per-image fences — a single semaphore tracks all in-flight frames.
    frame_timeline_semaphore: vk::Semaphore,
    frame_timeline_value: u64,
    /// Pre-allocated command buffers, one per swapchain image.
    command_buffers: Vec<vk::CommandBuffer>,
    /// Current frame index cycling through sync sets.
    current_frame: usize,
    surface_loader: ash::khr::surface::Instance,
    swapchain_loader: ash::khr::swapchain::Device,
    /// Per-swapchain-image VkImageView for dynamic rendering color attachment.
    swapchain_image_views: Vec<vk::ImageView>,
    dynamic_rendering_loader: ash::khr::dynamic_rendering::Device,
}

/// Persistent render pipeline objects that survive swapchain recreation.
struct PersistentPipelineState {
    graphics_pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    sampler: vk::Sampler,
}

/// Device-local VkImage used as the camera texture for fragment shader sampling.
struct CameraTextureState {
    image: vk::Image,
    image_view: vk::ImageView,
    gpu_memory_allocation: Allocation,
    width: u32,
    height: u32,
}

// ---------------------------------------------------------------------------
// Event loop handler — owns the window and drives frame rendering
// ---------------------------------------------------------------------------

struct DisplayEventLoopHandler {
    window: Option<Window>,
    vulkan_device: Arc<crate::vulkan::rhi::VulkanDevice>,
    gpu_context: GpuContext,
    inputs: crate::iceoryx2::InputMailboxes,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    window_id: u64,
    width: u32,
    height: u32,
    window_title: String,
    vsync: bool,
    scaling_mode: ScalingMode,
    swapchain_state: Option<SwapchainState>,
    pipeline_state: Option<PersistentPipelineState>,
    camera_texture_ring: Vec<CameraTextureState>,
}

impl ApplicationHandler for DisplayEventLoopHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Already initialized
        }

        let attrs = WindowAttributes::default()
            .with_title(&self.window_title)
            .with_inner_size(PhysicalSize::new(self.width, self.height));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(
                    "Display {}: Failed to create window: {}",
                    self.window_id,
                    e
                );
                event_loop.exit();
                return;
            }
        };

        // Create Vulkan surface, swapchain, and persistent pipeline objects
        match create_swapchain_state(
            &self.vulkan_device,
            &window,
            self.width,
            self.height,
            self.vsync,
        ) {
            Ok((state, pipeline_state)) => {
                tracing::info!(
                    "Display {}: Vulkan swapchain created ({}x{}, {:?})",
                    self.window_id,
                    state.swapchain_extent.width,
                    state.swapchain_extent.height,
                    state.swapchain_format
                );
                self.swapchain_state = Some(state);
                self.pipeline_state = Some(pipeline_state);
            }
            Err(e) => {
                tracing::error!(
                    "Display {}: Failed to create swapchain: {}",
                    self.window_id,
                    e
                );
                event_loop.exit();
                return;
            }
        }

        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("Display {}: Window close requested", self.window_id);
                self.running.store(false, Ordering::Release);
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if new_size.width == 0 || new_size.height == 0 {
                    return; // Minimized
                }
                tracing::debug!(
                    "Display {}: Resized to {}x{}",
                    self.window_id,
                    new_size.width,
                    new_size.height
                );
                self.width = new_size.width;
                self.height = new_size.height;

                // Recreate swapchain for new size
                if let Some(window) = &self.window {
                    // Wait for GPU idle before destroying old swapchain
                    unsafe {
                        let _ = self.vulkan_device.device().device_wait_idle();
                    }
                    if let Some(old_state) = self.swapchain_state.take() {
                        // Recreate FIRST (old swapchain must be alive for old_swapchain param),
                        // then destroy old resources on success.
                        match recreate_swapchain(
                            &self.vulkan_device,
                            window,
                            &old_state,
                            new_size.width,
                            new_size.height,
                            self.vsync,
                        ) {
                            Ok(new_state) => {
                                destroy_swapchain_resources_only(
                                    &self.vulkan_device,
                                    &old_state,
                                );
                                tracing::debug!(
                                    "Display {}: Swapchain recreated ({}x{})",
                                    self.window_id,
                                    new_state.swapchain_extent.width,
                                    new_state.swapchain_extent.height
                                );
                                self.swapchain_state = Some(new_state);
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Display {}: Failed to recreate swapchain: {}",
                                    self.window_id,
                                    e
                                );
                                event_loop.exit();
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.running.load(Ordering::Acquire) {
            event_loop.exit();
            return;
        }
        if let Some(ref window) = self.window {
            if self.inputs.has_data("video") {
                // New frame available — render it immediately
                window.request_redraw();
            } else {
                // No frame available — poll again after 1ms instead of spinning.
                // This keeps CPU idle between frames while still detecting new data
                // promptly. Each frame is rendered exactly once since read() consumes
                // the IPC message, causing has_data() to return false until the next frame.
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(1),
                ));
            }
        }
    }
}

impl DisplayEventLoopHandler {
    fn render_frame(&mut self) {
        let Some(ref mut state) = self.swapchain_state else {
            return;
        };
        let Some(ref ps) = self.pipeline_state else {
            return;
        };

        // Check if input frame is available — return immediately if not.
        // Frame pacing is driven by the swapchain present mode, not sleep loops.
        if !self.inputs.has_data("video") {
            return;
        }

        let ipc_frame: crate::_generated_::Videoframe = match self.inputs.read("video") {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!("Display {}: Failed to read frame: {}", self.window_id, e);
                return;
            }
        };

        // Resolve pixel buffer from surface_id
        let buffer = match self.gpu_context.resolve_videoframe_buffer(&ipc_frame) {
            Ok(buf) => buf,
            Err(e) => {
                tracing::warn!(
                    "Display {}: Failed to resolve buffer for '{}': {}",
                    self.window_id,
                    ipc_frame.surface_id,
                    e
                );
                return;
            }
        };

        let device = self.vulkan_device.device();
        let queue = self.vulkan_device.queue();

        let frame_index = state.current_frame;
        let image_count = state.swapchain_images.len();

        let vulkan_pixel_buffer = &buffer.buffer_ref().inner;
        let src_buffer = vulkan_pixel_buffer.buffer();
        let src_width = vulkan_pixel_buffer.width();
        let src_height = vulkan_pixel_buffer.height();

        // Create or recreate camera texture ring if dimensions changed or ring
        // size no longer matches the swapchain image count (e.g. after resize).
        // Each in-flight frame gets its own device-local texture, avoiding
        // write-after-read hazards between buffer copy and fragment shader.
        let need_camera_texture_ring = if self.camera_texture_ring.is_empty() {
            true
        } else {
            let existing = &self.camera_texture_ring[0];
            existing.width != src_width
                || existing.height != src_height
                || self.camera_texture_ring.len() != image_count
        };

        if need_camera_texture_ring {
            // Destroy all existing camera textures
            if !self.camera_texture_ring.is_empty() {
                unsafe {
                    let _ = device.device_wait_idle();
                }
                for old_tex in self.camera_texture_ring.drain(..) {
                    unsafe {
                        device.destroy_image_view(old_tex.image_view, None);
                        device.destroy_image(old_tex.image, None);
                    }
                    let _ = self.vulkan_device.free_gpu_memory(old_tex.gpu_memory_allocation);
                }
            }

            // Allocate one camera texture per swapchain image (ring buffer).
            for ring_idx in 0..image_count {
                let image_info = vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::B8G8R8A8_UNORM)
                    .extent(vk::Extent3D {
                        width: src_width,
                        height: src_height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED);

                let image = match unsafe { device.create_image(&image_info, None) } {
                    Ok(img) => img,
                    Err(e) => {
                        tracing::warn!(
                            "Display {}: Failed to create camera texture [{}]: {}",
                            self.window_id,
                            ring_idx,
                            e
                        );
                        return;
                    }
                };

                let mem_requirements = unsafe { device.get_image_memory_requirements(image) };

                // Sub-allocate through gpu-allocator so camera textures share
                // the same DEVICE_LOCAL memory blocks as other GPU resources.
                // Raw vkAllocateMemory was hitting the driver's per-process
                // allocation count limit (maxMemoryAllocationCount ≈ 4096 on
                // NVIDIA), causing DEVICE_LOCAL to fail despite free VRAM.
                let allocation = match self.vulkan_device.allocate_gpu_memory(
                    "camera_texture",
                    mem_requirements,
                    MemoryLocation::GpuOnly,
                    false, // OPTIMAL tiling = non-linear
                ) {
                    Ok(alloc) => alloc,
                    Err(e) => {
                        tracing::warn!(
                            "Display {}: Failed to allocate camera texture memory [{}]: {}",
                            self.window_id,
                            ring_idx,
                            e
                        );
                        unsafe { device.destroy_image(image, None) };
                        return;
                    }
                };

                if ring_idx == 0 {
                    tracing::info!(
                        "Display {}: Camera texture allocated via gpu-allocator, size={} bytes",
                        self.window_id,
                        mem_requirements.size
                    );
                }

                if unsafe {
                    device.bind_image_memory(image, allocation.memory(), allocation.offset())
                }
                .is_err()
                {
                    let _ = self.vulkan_device.free_gpu_memory(allocation);
                    unsafe { device.destroy_image(image, None) };
                    return;
                }

                let view_info = vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::B8G8R8A8_UNORM)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .base_mip_level(0)
                            .level_count(1)
                            .base_array_layer(0)
                            .layer_count(1),
                    );

                let image_view = match unsafe { device.create_image_view(&view_info, None) } {
                    Ok(view) => view,
                    Err(e) => {
                        tracing::warn!(
                            "Display {}: Failed to create camera texture view [{}]: {}",
                            self.window_id,
                            ring_idx,
                            e
                        );
                        let _ = self.vulkan_device.free_gpu_memory(allocation);
                        unsafe { device.destroy_image(image, None) };
                        return;
                    }
                };

                self.camera_texture_ring.push(CameraTextureState {
                    image,
                    image_view,
                    gpu_memory_allocation: allocation,
                    width: src_width,
                    height: src_height,
                });
            }

            tracing::debug!(
                "Display {}: Camera texture ring created ({} textures, {}x{})",
                self.window_id,
                image_count,
                src_width,
                src_height
            );
        }

        let camera_tex = &self.camera_texture_ring[frame_index];

        // Update descriptor set with camera texture every frame.
        // This is a single descriptor write — negligible CPU cost — and ensures
        // correctness after swapchain recreation (which creates a new descriptor set).
        let desc_image_info = vk::DescriptorImageInfo::default()
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image_view(camera_tex.image_view)
            .sampler(ps.sampler);
        let desc_image_infos = [desc_image_info];
        let descriptor_write = vk::WriteDescriptorSet::default()
            .dst_set(ps.descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&desc_image_infos);
        unsafe { device.update_descriptor_sets(&[descriptor_write], &[]) };

        // Timeline semaphore wait: ensure frame N-image_count completed before reusing slot N.
        // On the first image_count frames, wait_value is 0 and the semaphore starts at 0,
        // so the wait returns immediately (equivalent to fences starting signaled).
        state.frame_timeline_value += 1;
        let wait_value = state.frame_timeline_value.saturating_sub(image_count as u64);
        if wait_value > 0 {
            let semaphores = [state.frame_timeline_semaphore];
            let values = [wait_value];
            let wait_info = vk::SemaphoreWaitInfo::default()
                .semaphores(&semaphores)
                .values(&values);
            unsafe {
                let _ = device.wait_semaphores(&wait_info, u64::MAX);
            }
        }

        let image_available_semaphore = state.image_available_semaphores[frame_index];
        let render_finished_semaphore = state.render_finished_semaphores[frame_index];
        let command_buffer = state.command_buffers[frame_index];

        // Acquire next swapchain image
        let image_index = match unsafe {
            state.swapchain_loader.acquire_next_image(
                state.swapchain,
                u64::MAX,
                image_available_semaphore,
                vk::Fence::null(),
            )
        } {
            Ok((index, _suboptimal)) => index,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                tracing::debug!("Display {}: Swapchain out of date", self.window_id);
                return;
            }
            Err(e) => {
                tracing::warn!(
                    "Display {}: Failed to acquire swapchain image: {}",
                    self.window_id,
                    e
                );
                return;
            }
        };

        let swapchain_image = state.swapchain_images[image_index as usize];

        // Reset and re-record the pre-allocated command buffer
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        let color_subresource_range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .base_mip_level(0)
            .level_count(1)
            .base_array_layer(0)
            .layer_count(1);

        unsafe {
            if device
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                .is_err()
            {
                return;
            }

            if device
                .begin_command_buffer(command_buffer, &begin_info)
                .is_err()
            {
                return;
            }

            // Transition camera texture: UNDEFINED → TRANSFER_DST_OPTIMAL
            let barrier_camera_to_transfer = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(camera_tex.image)
                .subresource_range(color_subresource_range)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);

            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_camera_to_transfer],
            );

            // Copy source pixel buffer into device-local camera texture
            let region = vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(src_width)
                .buffer_image_height(src_height)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width: src_width,
                    height: src_height,
                    depth: 1,
                });

            device.cmd_copy_buffer_to_image(
                command_buffer,
                src_buffer,
                camera_tex.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );

            // Transition camera texture: TRANSFER_DST → SHADER_READ_ONLY
            // Transition swapchain image: UNDEFINED → COLOR_ATTACHMENT_OPTIMAL
            let barrier_camera_to_shader_read = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(camera_tex.image)
                .subresource_range(color_subresource_range)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ);

            let barrier_swapchain_to_color_attachment = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(swapchain_image)
                .subresource_range(color_subresource_range)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);

            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER
                    | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_camera_to_shader_read, barrier_swapchain_to_color_attachment],
            );

            // Begin dynamic rendering on swapchain image
            let color_attachment = vk::RenderingAttachmentInfo::default()
                .image_view(state.swapchain_image_views[image_index as usize])
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue {
                        float32: [0.0, 0.0, 0.0, 1.0],
                    },
                });
            let color_attachments = [color_attachment];
            let rendering_info = vk::RenderingInfo::default()
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: state.swapchain_extent,
                })
                .layer_count(1)
                .color_attachments(&color_attachments);

            state
                .dynamic_rendering_loader
                .cmd_begin_rendering(command_buffer, &rendering_info);

            // Set dynamic viewport and scissor
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: state.swapchain_extent.width as f32,
                height: state.swapchain_extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            let scissor = vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: state.swapchain_extent,
            };
            device.cmd_set_viewport(command_buffer, 0, &[viewport]);
            device.cmd_set_scissor(command_buffer, 0, &[scissor]);

            // Bind graphics pipeline and descriptor set
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                ps.graphics_pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                ps.pipeline_layout,
                0,
                &[ps.descriptor_set],
                &[],
            );

            // Push constants for scaling based on configured mode
            let src_aspect = src_width as f32 / src_height as f32;
            let dst_aspect =
                state.swapchain_extent.width as f32 / state.swapchain_extent.height as f32;

            let (scale_x, scale_y) = match self.scaling_mode {
                ScalingMode::Stretch => (1.0f32, 1.0f32),
                ScalingMode::Letterbox => {
                    if src_aspect > dst_aspect {
                        (1.0f32, dst_aspect / src_aspect)
                    } else {
                        (src_aspect / dst_aspect, 1.0f32)
                    }
                }
                ScalingMode::Crop => {
                    if src_aspect > dst_aspect {
                        (src_aspect / dst_aspect, 1.0f32)
                    } else {
                        (1.0f32, dst_aspect / src_aspect)
                    }
                }
            };

            let push_constants: [f32; 4] = [scale_x, scale_y, 0.0, 0.0];
            let push_constant_bytes = std::slice::from_raw_parts(
                push_constants.as_ptr() as *const u8,
                std::mem::size_of::<[f32; 4]>(),
            );
            device.cmd_push_constants(
                command_buffer,
                ps.pipeline_layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                push_constant_bytes,
            );

            // Draw fullscreen triangle (3 vertices from gl_VertexIndex, no vertex buffer)
            device.cmd_draw(command_buffer, 3, 1, 0, 0);

            state
                .dynamic_rendering_loader
                .cmd_end_rendering(command_buffer);

            // Transition swapchain image: COLOR_ATTACHMENT_OPTIMAL → PRESENT_SRC_KHR
            let barrier_to_present = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(swapchain_image)
                .subresource_range(color_subresource_range)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::empty());

            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_to_present],
            );

            if device.end_command_buffer(command_buffer).is_err() {
                return;
            }

            // Submit — signal both binary semaphore (for present) and timeline (for sync)
            let wait_semaphores = [image_available_semaphore];
            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let signal_semaphores =
                [render_finished_semaphore, state.frame_timeline_semaphore];
            let command_buffers = [command_buffer];

            // TimelineSemaphoreSubmitInfo: value 0 is ignored for binary semaphores
            let signal_values = [0u64, state.frame_timeline_value];
            let wait_values = [0u64];
            let mut timeline_submit_info = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);

            let submit_info = vk::SubmitInfo::default()
                .wait_semaphores(&wait_semaphores)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&command_buffers)
                .signal_semaphores(&signal_semaphores)
                .push_next(&mut timeline_submit_info);

            if let Err(e) =
                device.queue_submit(queue, &[submit_info], vk::Fence::null())
            {
                tracing::warn!(
                    "Display {}: Failed to submit render command: {}",
                    self.window_id,
                    e
                );
                return;
            }

            // Present — wait only on the binary render_finished semaphore
            let present_wait_semaphores = [render_finished_semaphore];
            let swapchains = [state.swapchain];
            let image_indices = [image_index];
            let present_info = vk::PresentInfoKHR::default()
                .wait_semaphores(&present_wait_semaphores)
                .swapchains(&swapchains)
                .image_indices(&image_indices);

            match state.swapchain_loader.queue_present(queue, &present_info) {
                Ok(_) | Err(vk::Result::SUBOPTIMAL_KHR) => {}
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    tracing::debug!(
                        "Display {}: Swapchain out of date at present",
                        self.window_id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Display {}: Failed to present: {}",
                        self.window_id,
                        e
                    );
                }
            }

            // No second wait here — the timeline semaphore wait at the top of the
            // next render_frame() call for this sync slot handles synchronization.
        }

        state.current_frame = (frame_index + 1) % image_count;
        self.frame_counter.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Swapchain creation / destruction helpers
// ---------------------------------------------------------------------------

fn create_swapchain_state(
    vulkan_device: &crate::vulkan::rhi::VulkanDevice,
    window: &Window,
    width: u32,
    height: u32,
    vsync: bool,
) -> Result<(SwapchainState, PersistentPipelineState)> {
    let entry = vulkan_device.entry();
    let instance = vulkan_device.instance();
    let device = vulkan_device.device();
    let physical_device = vulkan_device.physical_device();
    let queue_family_index = vulkan_device.queue_family_index();

    // Create surface via ash-window
    let display_handle = window.display_handle().map_err(|e| {
        StreamError::GpuError(format!("Failed to get display handle: {}", e))
    })?;
    let window_handle = window.window_handle().map_err(|e| {
        StreamError::GpuError(format!("Failed to get window handle: {}", e))
    })?;

    let surface = unsafe {
        ash_window::create_surface(
            entry,
            instance,
            display_handle.as_raw(),
            window_handle.as_raw(),
            None,
        )
    }
    .map_err(|e| StreamError::GpuError(format!("Failed to create Vulkan surface: {}", e)))?;

    let surface_loader = ash::khr::surface::Instance::new(entry, instance);
    let swapchain_loader = ash::khr::swapchain::Device::new(instance, device);

    // Check surface support for this queue family
    let surface_supported = unsafe {
        surface_loader.get_physical_device_surface_support(
            physical_device,
            queue_family_index,
            surface,
        )
    }
    .map_err(|e| StreamError::GpuError(format!("Failed to check surface support: {}", e)))?;

    if !surface_supported {
        unsafe { surface_loader.destroy_surface(surface, None) };
        return Err(StreamError::GpuError(
            "Graphics queue family does not support presentation to this surface".into(),
        ));
    }

    // Query surface capabilities
    let capabilities = unsafe {
        surface_loader.get_physical_device_surface_capabilities(physical_device, surface)
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to query surface capabilities: {}", e))
    })?;

    let surface_formats = unsafe {
        surface_loader.get_physical_device_surface_formats(physical_device, surface)
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to query surface formats: {}", e))
    })?;

    let present_modes = unsafe {
        surface_loader.get_physical_device_surface_present_modes(physical_device, surface)
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to query present modes: {}", e))
    })?;

    // Choose surface format — prefer B8G8R8A8_UNORM + SRGB_NONLINEAR
    let surface_format = surface_formats
        .iter()
        .find(|f| {
            f.format == vk::Format::B8G8R8A8_UNORM
                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .unwrap_or(&surface_formats[0]);

    // Choose present mode
    let present_mode = if vsync {
        // FIFO is always supported and provides vsync
        vk::PresentModeKHR::FIFO
    } else if present_modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    };

    // Choose extent
    let extent = if capabilities.current_extent.width != u32::MAX {
        capabilities.current_extent
    } else {
        vk::Extent2D {
            width: width.clamp(
                capabilities.min_image_extent.width,
                capabilities.max_image_extent.width,
            ),
            height: height.clamp(
                capabilities.min_image_extent.height,
                capabilities.max_image_extent.height,
            ),
        }
    };

    // Choose image count (prefer min + 1 for triple buffering)
    let mut image_count = capabilities.min_image_count + 1;
    if capabilities.max_image_count > 0 && image_count > capabilities.max_image_count {
        image_count = capabilities.max_image_count;
    }

    let swapchain_info = vk::SwapchainCreateInfoKHR::default()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(surface_format.format)
        .image_color_space(surface_format.color_space)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .pre_transform(capabilities.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(present_mode)
        .clipped(true);

    let swapchain = unsafe { swapchain_loader.create_swapchain(&swapchain_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create swapchain: {}", e)))?;

    let swapchain_images = unsafe { swapchain_loader.get_swapchain_images(swapchain) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to get swapchain images: {}", e))
        })?;

    // Create command pool for this thread
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .queue_family_index(queue_family_index);

    let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {}", e)))?;

    // Create per-swapchain-image binary semaphores for acquire/present
    let image_count = swapchain_images.len();
    let semaphore_info = vk::SemaphoreCreateInfo::default();

    let mut image_available_semaphores = Vec::with_capacity(image_count);
    let mut render_finished_semaphores = Vec::with_capacity(image_count);

    for _ in 0..image_count {
        let image_available = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;
        let render_finished = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;

        image_available_semaphores.push(image_available);
        render_finished_semaphores.push(render_finished);
    }

    // Timeline semaphore for multi-flight frame synchronization (Vulkan 1.2 core).
    // One semaphore tracks all frames — wait for value N-image_count before reusing slot N.
    let mut timeline_type_info = vk::SemaphoreTypeCreateInfo::default()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let timeline_semaphore_info = vk::SemaphoreCreateInfo::default()
        .push_next(&mut timeline_type_info);
    let frame_timeline_semaphore = unsafe { device.create_semaphore(&timeline_semaphore_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create timeline semaphore: {}", e)))?;

    // Pre-allocate command buffers (one per swapchain image)
    let alloc_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(image_count as u32);

    let command_buffers = unsafe { device.allocate_command_buffers(&alloc_info) }
        .map_err(|e| StreamError::GpuError(format!("Failed to allocate command buffers: {}", e)))?;

    // Create swapchain image views for dynamic rendering color attachments
    let mut swapchain_image_views = Vec::with_capacity(image_count);
    for &image in &swapchain_images {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(surface_format.format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1),
            );
        let view = unsafe { device.create_image_view(&view_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create swapchain image view: {}", e)))?;
        swapchain_image_views.push(view);
    }

    // Create sampler for camera texture sampling in fragment shader
    let sampler_info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
    let sampler = unsafe { device.create_sampler(&sampler_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create sampler: {}", e)))?;

    // Descriptor set layout: binding 0 = combined image sampler (fragment stage)
    let ds_binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT);
    let ds_bindings = [ds_binding];
    let ds_layout_info = vk::DescriptorSetLayoutCreateInfo::default()
        .bindings(&ds_bindings);
    let descriptor_set_layout = unsafe { device.create_descriptor_set_layout(&ds_layout_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create descriptor set layout: {}", e)))?;

    // Pipeline layout: push constant for scale (vec2) + offset (vec2) = 16 bytes
    let push_constant_range = vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(16);
    let set_layouts = [descriptor_set_layout];
    let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&set_layouts)
        .push_constant_ranges(std::slice::from_ref(&push_constant_range));
    let pipeline_layout = unsafe { device.create_pipeline_layout(&pipeline_layout_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create pipeline layout: {}", e)))?;

    // Load compiled SPIR-V shaders
    let vert_spv = include_bytes!("shaders/fullscreen.vert.spv");
    let frag_spv = include_bytes!("shaders/fullscreen.frag.spv");

    let vert_code: Vec<u32> = vert_spv
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let frag_code: Vec<u32> = frag_spv
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let vert_module_info = vk::ShaderModuleCreateInfo::default().code(&vert_code);
    let frag_module_info = vk::ShaderModuleCreateInfo::default().code(&frag_code);

    let vert_module = unsafe { device.create_shader_module(&vert_module_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create vertex shader module: {}", e)))?;
    let frag_module = unsafe { device.create_shader_module(&frag_module_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create fragment shader module: {}", e)))?;

    let entry_point = c"main";

    let shader_stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert_module)
            .name(entry_point),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag_module)
            .name(entry_point),
    ];

    // No vertex input — fullscreen triangle derives UVs from gl_VertexIndex
    let vertex_input_info = vk::PipelineVertexInputStateCreateInfo::default();
    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);

    // Dynamic viewport/scissor — set per-frame, no pipeline recreation on resize
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state_info = vk::PipelineDynamicStateCreateInfo::default()
        .dynamic_states(&dynamic_states);

    let viewports = [vk::Viewport::default()];
    let scissors = [vk::Rect2D::default()];
    let viewport_state = vk::PipelineViewportStateCreateInfo::default()
        .viewports(&viewports)
        .scissors(&scissors);

    let rasterizer = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);

    let multisampling = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);

    let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        )
        .blend_enable(false);
    let color_blend_attachments = [color_blend_attachment];
    let color_blend_state = vk::PipelineColorBlendStateCreateInfo::default()
        .attachments(&color_blend_attachments);

    // Dynamic rendering: specify color attachment format via pNext
    let color_attachment_formats = [surface_format.format];
    let mut pipeline_rendering_info = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&color_attachment_formats);

    let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&shader_stages)
        .vertex_input_state(&vertex_input_info)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterizer)
        .multisample_state(&multisampling)
        .color_blend_state(&color_blend_state)
        .dynamic_state(&dynamic_state_info)
        .layout(pipeline_layout)
        .push_next(&mut pipeline_rendering_info);

    let graphics_pipeline = unsafe {
        device.create_graphics_pipelines(
            vk::PipelineCache::null(),
            &[pipeline_info],
            None,
        )
    }
    .map_err(|(_pipelines, e)| StreamError::GpuError(format!("Failed to create graphics pipeline: {}", e)))?[0];

    // Shader modules no longer needed after pipeline creation
    unsafe {
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);
    }

    // Descriptor pool and set for camera texture binding
    let pool_size = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1);
    let pool_sizes = [pool_size];
    let descriptor_pool_info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(1)
        .pool_sizes(&pool_sizes);
    let descriptor_pool = unsafe { device.create_descriptor_pool(&descriptor_pool_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create descriptor pool: {}", e)))?;

    let set_layouts_alloc = [descriptor_set_layout];
    let ds_alloc_info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(descriptor_pool)
        .set_layouts(&set_layouts_alloc);
    let descriptor_set = unsafe { device.allocate_descriptor_sets(&ds_alloc_info) }
        .map_err(|e| StreamError::GpuError(format!("Failed to allocate descriptor set: {}", e)))?[0];

    // Dynamic rendering loader
    let dynamic_rendering_loader = ash::khr::dynamic_rendering::Device::new(instance, device);

    tracing::info!(
        "Swapchain created: {}x{}, format {:?}, present mode {:?}, {} images",
        extent.width,
        extent.height,
        surface_format.format,
        present_mode,
        image_count
    );

    Ok((
        SwapchainState {
            surface,
            swapchain,
            swapchain_images,
            swapchain_format: surface_format.format,
            swapchain_extent: extent,
            command_pool,
            image_available_semaphores,
            render_finished_semaphores,
            frame_timeline_semaphore,
            frame_timeline_value: 0,
            command_buffers,
            current_frame: 0,
            surface_loader,
            swapchain_loader,
            swapchain_image_views,
            dynamic_rendering_loader,
        },
        PersistentPipelineState {
            graphics_pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            sampler,
        },
    ))
}

fn recreate_swapchain(
    vulkan_device: &crate::vulkan::rhi::VulkanDevice,
    _window: &Window,
    old_state: &SwapchainState,
    width: u32,
    height: u32,
    vsync: bool,
) -> Result<SwapchainState> {
    let entry = vulkan_device.entry();
    let instance = vulkan_device.instance();
    let device = vulkan_device.device();
    let physical_device = vulkan_device.physical_device();
    let queue_family_index = vulkan_device.queue_family_index();

    let surface = old_state.surface;
    let surface_loader = ash::khr::surface::Instance::new(entry, instance);
    let swapchain_loader = ash::khr::swapchain::Device::new(instance, device);

    let capabilities = unsafe {
        surface_loader.get_physical_device_surface_capabilities(physical_device, surface)
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to query surface capabilities: {}", e))
    })?;

    let extent = if capabilities.current_extent.width != u32::MAX {
        capabilities.current_extent
    } else {
        vk::Extent2D {
            width: width.clamp(
                capabilities.min_image_extent.width,
                capabilities.max_image_extent.width,
            ),
            height: height.clamp(
                capabilities.min_image_extent.height,
                capabilities.max_image_extent.height,
            ),
        }
    };

    let present_modes = unsafe {
        surface_loader.get_physical_device_surface_present_modes(physical_device, surface)
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to query present modes: {}", e))
    })?;

    let present_mode = if vsync {
        vk::PresentModeKHR::FIFO
    } else if present_modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    };

    let mut image_count = capabilities.min_image_count + 1;
    if capabilities.max_image_count > 0 && image_count > capabilities.max_image_count {
        image_count = capabilities.max_image_count;
    }

    let swapchain_info = vk::SwapchainCreateInfoKHR::default()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(old_state.swapchain_format)
        .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .pre_transform(capabilities.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(present_mode)
        .clipped(true)
        .old_swapchain(old_state.swapchain);

    let swapchain = unsafe { swapchain_loader.create_swapchain(&swapchain_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to recreate swapchain: {}", e)))?;

    let swapchain_images = unsafe { swapchain_loader.get_swapchain_images(swapchain) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to get swapchain images: {}", e))
        })?;

    // Create new command pool
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .queue_family_index(queue_family_index);

    let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {}", e)))?;

    // Create per-swapchain-image binary semaphores for acquire/present
    let new_image_count = swapchain_images.len();
    let semaphore_info = vk::SemaphoreCreateInfo::default();

    let mut image_available_semaphores = Vec::with_capacity(new_image_count);
    let mut render_finished_semaphores = Vec::with_capacity(new_image_count);

    for _ in 0..new_image_count {
        let image_available = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;
        let render_finished = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;

        image_available_semaphores.push(image_available);
        render_finished_semaphores.push(render_finished);
    }

    // Timeline semaphore for multi-flight frame synchronization
    let mut timeline_type_info = vk::SemaphoreTypeCreateInfo::default()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let timeline_semaphore_info = vk::SemaphoreCreateInfo::default()
        .push_next(&mut timeline_type_info);
    let frame_timeline_semaphore = unsafe { device.create_semaphore(&timeline_semaphore_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create timeline semaphore: {}", e)))?;

    // Pre-allocate command buffers (one per swapchain image)
    let alloc_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(new_image_count as u32);

    let command_buffers = unsafe { device.allocate_command_buffers(&alloc_info) }
        .map_err(|e| StreamError::GpuError(format!("Failed to allocate command buffers: {}", e)))?;

    // Create swapchain image views for dynamic rendering color attachments
    let mut swapchain_image_views = Vec::with_capacity(new_image_count);
    for &image in &swapchain_images {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(old_state.swapchain_format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1),
            );
        let view = unsafe { device.create_image_view(&view_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create swapchain image view: {}", e)))?;
        swapchain_image_views.push(view);
    }

    let dynamic_rendering_loader = ash::khr::dynamic_rendering::Device::new(instance, device);

    Ok(SwapchainState {
        surface,
        swapchain,
        swapchain_images,
        swapchain_format: old_state.swapchain_format,
        swapchain_extent: extent,
        command_pool,
        image_available_semaphores,
        render_finished_semaphores,
        frame_timeline_semaphore,
        frame_timeline_value: 0,
        command_buffers,
        current_frame: 0,
        surface_loader,
        swapchain_loader,
        swapchain_image_views,
        dynamic_rendering_loader,
    })
}

/// Destroy swapchain-related resources only (not the surface or persistent pipeline objects).
fn destroy_swapchain_resources_only(
    vulkan_device: &crate::vulkan::rhi::VulkanDevice,
    state: &SwapchainState,
) {
    let device = vulkan_device.device();
    unsafe {
        for &view in &state.swapchain_image_views {
            device.destroy_image_view(view, None);
        }
        device.destroy_semaphore(state.frame_timeline_semaphore, None);
        for &semaphore in &state.render_finished_semaphores {
            device.destroy_semaphore(semaphore, None);
        }
        for &semaphore in &state.image_available_semaphores {
            device.destroy_semaphore(semaphore, None);
        }
        // Command buffers are freed when the command pool is destroyed
        device.destroy_command_pool(state.command_pool, None);
        state
            .swapchain_loader
            .destroy_swapchain(state.swapchain, None);
    }
}

/// Destroy all swapchain state including the surface (but not persistent pipeline objects).
fn destroy_swapchain_state(
    vulkan_device: &crate::vulkan::rhi::VulkanDevice,
    state: &SwapchainState,
) {
    let device = vulkan_device.device();
    unsafe {
        let _ = device.device_wait_idle();
        for &view in &state.swapchain_image_views {
            device.destroy_image_view(view, None);
        }
        device.destroy_semaphore(state.frame_timeline_semaphore, None);
        for &semaphore in &state.render_finished_semaphores {
            device.destroy_semaphore(semaphore, None);
        }
        for &semaphore in &state.image_available_semaphores {
            device.destroy_semaphore(semaphore, None);
        }
        // Command buffers are freed when the command pool is destroyed
        device.destroy_command_pool(state.command_pool, None);
        state
            .swapchain_loader
            .destroy_swapchain(state.swapchain, None);
        state
            .surface_loader
            .destroy_surface(state.surface, None);
    }
}
