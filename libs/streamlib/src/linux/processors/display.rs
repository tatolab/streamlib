// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
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

                let event_loop = match EventLoop::new() {
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

                event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

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
                    swapchain_state: None,
                };

                if let Err(e) = event_loop.run_app(&mut app) {
                    tracing::error!("Display {}: Event loop error: {}", window_id, e);
                }

                // Clean up swapchain resources before exiting
                if let Some(state) = app.swapchain_state.take() {
                    destroy_swapchain_state(&app.vulkan_device, &state);
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
    image_available_semaphore: vk::Semaphore,
    render_finished_semaphore: vk::Semaphore,
    in_flight_fence: vk::Fence,
    surface_loader: ash::khr::surface::Instance,
    swapchain_loader: ash::khr::swapchain::Device,
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
    swapchain_state: Option<SwapchainState>,
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

        // Create Vulkan surface and swapchain
        match create_swapchain_state(
            &self.vulkan_device,
            &window,
            self.width,
            self.height,
            self.vsync,
        ) {
            Ok(state) => {
                tracing::info!(
                    "Display {}: Vulkan swapchain created ({}x{}, {:?})",
                    self.window_id,
                    state.swapchain_extent.width,
                    state.swapchain_extent.height,
                    state.swapchain_format
                );
                self.swapchain_state = Some(state);
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
                        destroy_swapchain_resources_only(&self.vulkan_device, &old_state);

                        match recreate_swapchain(
                            &self.vulkan_device,
                            window,
                            &old_state,
                            new_size.width,
                            new_size.height,
                            self.vsync,
                        ) {
                            Ok(new_state) => {
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
            window.request_redraw();
        }
    }
}

impl DisplayEventLoopHandler {
    fn render_frame(&mut self) {
        let Some(ref state) = self.swapchain_state else {
            return;
        };

        // Check if input frame is available
        if !self.inputs.has_data("video") {
            std::thread::sleep(Duration::from_micros(500));
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

        // Wait for previous frame to finish
        unsafe {
            let _ = device.wait_for_fences(
                &[state.in_flight_fence],
                true,
                u64::MAX,
            );
            let _ = device.reset_fences(&[state.in_flight_fence]);
        }

        // Acquire next swapchain image
        let image_index = match unsafe {
            state.swapchain_loader.acquire_next_image(
                state.swapchain,
                u64::MAX,
                state.image_available_semaphore,
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
        let vulkan_pixel_buffer = &buffer.buffer_ref().inner;
        let src_buffer = vulkan_pixel_buffer.buffer();
        let src_width = vulkan_pixel_buffer.width();
        let src_height = vulkan_pixel_buffer.height();

        // Record command buffer: copy staging buffer → swapchain image
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(state.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);

        let command_buffer = match unsafe { device.allocate_command_buffers(&alloc_info) } {
            Ok(bufs) => bufs[0],
            Err(e) => {
                tracing::warn!(
                    "Display {}: Failed to allocate command buffer: {}",
                    self.window_id,
                    e
                );
                return;
            }
        };

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        unsafe {
            if device
                .begin_command_buffer(command_buffer, &begin_info)
                .is_err()
            {
                device.free_command_buffers(state.command_pool, &[command_buffer]);
                return;
            }

            // Transition swapchain image: UNDEFINED → TRANSFER_DST_OPTIMAL
            let barrier_to_transfer = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(swapchain_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);

            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_to_transfer],
            );

            // Copy buffer to image
            let copy_width = src_width.min(state.swapchain_extent.width);
            let copy_height = src_height.min(state.swapchain_extent.height);

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
                    width: copy_width,
                    height: copy_height,
                    depth: 1,
                });

            device.cmd_copy_buffer_to_image(
                command_buffer,
                src_buffer,
                swapchain_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );

            // Transition swapchain image: TRANSFER_DST_OPTIMAL → PRESENT_SRC_KHR
            let barrier_to_present = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(swapchain_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::empty());

            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_to_present],
            );

            if device.end_command_buffer(command_buffer).is_err() {
                device.free_command_buffers(state.command_pool, &[command_buffer]);
                return;
            }

            // Submit
            let wait_semaphores = [state.image_available_semaphore];
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            let signal_semaphores = [state.render_finished_semaphore];
            let command_buffers = [command_buffer];

            let submit_info = vk::SubmitInfo::default()
                .wait_semaphores(&wait_semaphores)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&command_buffers)
                .signal_semaphores(&signal_semaphores);

            if let Err(e) =
                device.queue_submit(queue, &[submit_info], state.in_flight_fence)
            {
                tracing::warn!(
                    "Display {}: Failed to submit render command: {}",
                    self.window_id,
                    e
                );
                device.free_command_buffers(state.command_pool, &[command_buffer]);
                return;
            }

            // Present
            let swapchains = [state.swapchain];
            let image_indices = [image_index];
            let present_info = vk::PresentInfoKHR::default()
                .wait_semaphores(&signal_semaphores)
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

            // Wait for GPU before releasing the source buffer.
            // Without this, the camera can reacquire and overwrite the buffer while
            // the GPU is still reading from it, causing corruption artifacts.
            let _ = device.wait_for_fences(&[state.in_flight_fence], true, u64::MAX);

            device.free_command_buffers(state.command_pool, &[command_buffer]);
        }

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
) -> Result<SwapchainState> {
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

    // Create synchronization primitives
    let semaphore_info = vk::SemaphoreCreateInfo::default();
    let image_available_semaphore =
        unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;

    let render_finished_semaphore =
        unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;

    let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
    let in_flight_fence = unsafe { device.create_fence(&fence_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create fence: {}", e)))?;

    tracing::info!(
        "Swapchain created: {}x{}, format {:?}, present mode {:?}, {} images",
        extent.width,
        extent.height,
        surface_format.format,
        present_mode,
        swapchain_images.len()
    );

    Ok(SwapchainState {
        surface,
        swapchain,
        swapchain_images,
        swapchain_format: surface_format.format,
        swapchain_extent: extent,
        command_pool,
        image_available_semaphore,
        render_finished_semaphore,
        in_flight_fence,
        surface_loader,
        swapchain_loader,
    })
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

    let present_mode = if vsync {
        vk::PresentModeKHR::FIFO
    } else {
        vk::PresentModeKHR::MAILBOX
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

    // Create new sync primitives
    let semaphore_info = vk::SemaphoreCreateInfo::default();
    let image_available_semaphore =
        unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;

    let render_finished_semaphore =
        unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;

    let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
    let in_flight_fence = unsafe { device.create_fence(&fence_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create fence: {}", e)))?;

    Ok(SwapchainState {
        surface,
        swapchain,
        swapchain_images,
        swapchain_format: old_state.swapchain_format,
        swapchain_extent: extent,
        command_pool,
        image_available_semaphore,
        render_finished_semaphore,
        in_flight_fence,
        surface_loader,
        swapchain_loader,
    })
}

/// Destroy swapchain-related resources only (not the surface).
fn destroy_swapchain_resources_only(
    vulkan_device: &crate::vulkan::rhi::VulkanDevice,
    state: &SwapchainState,
) {
    let device = vulkan_device.device();
    unsafe {
        device.destroy_fence(state.in_flight_fence, None);
        device.destroy_semaphore(state.render_finished_semaphore, None);
        device.destroy_semaphore(state.image_available_semaphore, None);
        device.destroy_command_pool(state.command_pool, None);
        state
            .swapchain_loader
            .destroy_swapchain(state.swapchain, None);
    }
}

/// Destroy all swapchain state including the surface.
fn destroy_swapchain_state(
    vulkan_device: &crate::vulkan::rhi::VulkanDevice,
    state: &SwapchainState,
) {
    let device = vulkan_device.device();
    unsafe {
        let _ = device.device_wait_idle();
        device.destroy_fence(state.in_flight_fence, None);
        device.destroy_semaphore(state.render_finished_semaphore, None);
        device.destroy_semaphore(state.image_available_semaphore, None);
        device.destroy_command_pool(state.command_pool, None);
        state
            .swapchain_loader
            .destroy_swapchain(state.swapchain, None);
        state
            .surface_loader
            .destroy_surface(state.surface, None);
    }
}
