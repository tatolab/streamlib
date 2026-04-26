// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_display_config::ScalingMode;
use crate::core::{GpuContextLimitedAccess, Result, RuntimeContextFullAccess, StreamError};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrSurfaceExtensionInstanceCommands as _;
use vulkanalia::vk::KhrSwapchainExtensionDeviceCommands as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes};

/// Maximum CPU/GPU frames in flight at once.
///
/// Per-frame resources (semaphores, command buffers, descriptor sets, camera
/// textures) are sized to this constant — independent of swapchain image count.
/// This is the conventional Vulkan pattern: swapchain image count is a
/// presentation concern (driven by the compositor's preferred mode), while
/// frames-in-flight is a CPU/GPU pipelining concern. Decoupling them avoids
/// over-allocating per-frame resources and keeps input latency low.
///
/// 2 is the standard choice — CPU runs at most 1 frame ahead of GPU.
const MAX_FRAMES_IN_FLIGHT: usize = 2;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct LinuxWindowId(pub u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

#[crate::processor("com.tatolab.display")]
pub struct LinuxDisplayProcessor {
    gpu_context: Option<GpuContextLimitedAccess>,
    window_id: LinuxWindowId,
    window_title: String,
    width: u32,
    height: u32,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    render_thread: Option<JoinHandle<()>>,
    event_loop_proxy: Arc<OnceLock<EventLoopProxy<()>>>,
    stop_called: Arc<AtomicBool>,
}

impl crate::core::ManualProcessor for LinuxDisplayProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            tracing::trace!("Display: setup() called");
            self.gpu_context = Some(ctx.gpu_limited_access().clone());
            self.window_id = LinuxWindowId(NEXT_WINDOW_ID.fetch_add(1, Ordering::SeqCst));
            self.width = self.config.width;
            self.height = self.config.height;
            self.window_title = self
                .config
                .title
                .clone()
                .unwrap_or_else(|| "streamlib Display".to_string());

            self.running = Arc::new(AtomicBool::new(false));
            self.event_loop_proxy = Arc::new(OnceLock::new());
            self.stop_called = Arc::new(AtomicBool::new(false));

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

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("Display {}: Teardown", self.window_title);
        std::future::ready(Ok(()))
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::trace!(
            "Display {}: start() called — spawning Vulkan + winit render thread",
            self.window_id.0
        );

        let inputs = std::mem::take(&mut self.inputs);
        let running = Arc::clone(&self.running);
        let frame_counter = Arc::clone(&self.frame_counter);
        let event_loop_proxy = Arc::clone(&self.event_loop_proxy);
        let stop_called = Arc::clone(&self.stop_called);
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

        // Pre-warm the tiled DMA-BUF VMA pool BEFORE spawning the render
        // thread that creates the swapchain. NVIDIA caps DMA-BUF
        // exportable allocations after swapchain creation; pre-warming the
        // pool now ensures the first VMA block lands while DMA-BUF
        // allocations are still freely available.
        // See `docs/learnings/nvidia-dma-buf-after-swapchain.md` and
        // `docs/learnings/nvidia-egl-dmabuf-render-target.md`.
        // Failure here is non-fatal: the EGL probe may have returned no
        // RT-capable modifiers in headless / no-display environments, in
        // which case render-target adapters will fail loudly with a clear
        // message at acquire time rather than silently fall back to
        // sampler-only LINEAR.
        if let Err(e) = gpu_context
            .pre_warm_render_target_dma_buf_pool(crate::core::rhi::TextureFormat::Bgra8Unorm)
        {
            tracing::warn!(
                "Display {}: render-target DMA-BUF pool pre-warm failed: {} \
                 — surface adapter consumers will see allocation errors",
                self.window_id.0, e
            );
        }

        // Pull the Vulkan device handle from the FullAccess lifecycle ctx.
        // The render thread uses it to build its swapchain and rendering
        // pipeline at startup; the Sandbox clone is what the thread keeps
        // for steady-state frame resolution.
        let vulkan_device = Arc::clone(&ctx.gpu_full_access().device().inner);

        running.store(true, Ordering::Release);

        let render_thread = std::thread::Builder::new()
            .name(format!("display-{}-render", window_id))
            .stack_size(8 * 1024 * 1024) // 8 MB — FramePayload is 64KB+ on the stack
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

                // Store proxy so stop() can wake the event loop from another thread.
                event_loop_proxy.set(event_loop.create_proxy()).ok();

                let frame_limit = std::env::var("STREAMLIB_DISPLAY_FRAME_LIMIT")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok());
                let png_sample_dir = std::env::var("STREAMLIB_DISPLAY_PNG_SAMPLE_DIR")
                    .ok()
                    .map(std::path::PathBuf::from);
                let png_sample_every = std::env::var("STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(30);

                if let Some(ref dir) = png_sample_dir {
                    if let Err(e) = std::fs::create_dir_all(dir) {
                        tracing::warn!(
                            "Display {}: failed to create PNG sample dir {:?}: {}",
                            window_id, dir, e
                        );
                    } else {
                        tracing::info!(
                            "Display {}: PNG sampling enabled — saving every {} frames to {:?}",
                            window_id, png_sample_every, dir
                        );
                    }
                }
                if let Some(limit) = frame_limit {
                    tracing::info!(
                        "Display {}: frame limit enabled — will exit after {} frames",
                        window_id, limit
                    );
                }

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
                    frame_limit,
                    png_sample_dir,
                    png_sample_every,
                    png_samples_saved: 0,
                };

                if let Err(e) = event_loop.run_app(&mut app) {
                    tracing::error!("Display {}: Event loop error: {}", window_id, e);
                }

                // If the event loop exited on its own (frame_limit, window close,
                // error), publish RuntimeShutdown so the runtime stops. Skip when
                // stop() triggered the exit — the runtime is already shutting down
                // and iceoryx2 may be mid-teardown.
                if !stop_called.load(Ordering::Acquire) {
                    use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
                    tracing::info!(
                        "Display {}: Event loop exited, requesting runtime shutdown",
                        window_id
                    );
                    let shutdown_event =
                        Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
                    PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);
                }

                // Camera textures are owned by the texture_cache / VulkanTexture Drop.

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

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::trace!("Display {}: stop() called", self.window_id.0);

        self.running.store(false, Ordering::Release);
        self.stop_called.store(true, Ordering::Release);

        // Wake the event loop so it observes running=false and calls exit().
        // Without this, the loop may be blocked in a platform event wait
        // (X11/Wayland) and never reach about_to_wait() to check the flag.
        if let Some(proxy) = self.event_loop_proxy.get() {
            let _ = proxy.send_event(());
        }

        if let Some(handle) = self.render_thread.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                handle
                    .join()
                    .map_err(|_| StreamError::Runtime("Render thread panicked".into()))?;
            } else {
                tracing::warn!(
                    "Display {}: Render thread did not exit within 2s, detaching",
                    self.window_id.0
                );
            }
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
    /// Per-swapchain-image VkImageView for dynamic rendering color attachment.
    swapchain_image_views: Vec<vk::ImageView>,
}

/// Persistent render pipeline objects that survive swapchain recreation.
struct PersistentPipelineState {
    graphics_pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_sets: Vec<vk::DescriptorSet>,
    sampler: vk::Sampler,
}

/// Device-local VkImage used as the camera texture for fragment shader sampling.
// ---------------------------------------------------------------------------
// Event loop handler — owns the window and drives frame rendering
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct DisplayEventLoopHandler {
    window: Option<Window>,
    vulkan_device: Arc<crate::vulkan::rhi::VulkanDevice>,
    gpu_context: GpuContextLimitedAccess,
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
    /// Debug feature: auto-exit after N frames rendered (env: STREAMLIB_DISPLAY_FRAME_LIMIT).
    frame_limit: Option<u64>,
    /// Debug feature: directory to save sampled PNGs (env: STREAMLIB_DISPLAY_PNG_SAMPLE_DIR).
    png_sample_dir: Option<std::path::PathBuf>,
    /// Debug feature: save every Nth frame as PNG (env: STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY, default 30).
    png_sample_every: u64,
    /// Internal counter for next PNG sample.
    png_samples_saved: u64,
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: ()) {
        if !self.running.load(Ordering::Acquire) {
            event_loop.exit();
        }
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

        // Debug feature: auto-exit after frame_limit frames rendered.
        if let Some(limit) = self.frame_limit {
            let current = self.frame_counter.load(Ordering::Relaxed);
            if current >= limit {
                tracing::info!(
                    "Display {}: frame limit ({}) reached — exiting",
                    self.window_id, limit
                );
                self.running.store(false, Ordering::Release);
                event_loop.exit();
                return;
            }
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

        // Unified texture resolution — GpuContext picks the fastest path:
        // 1. Same-process texture cache (zero-copy ring texture)
        // 2. Cross-process DMA-BUF VkImage import via surface-share service (GPU-to-GPU)
        let camera_texture = match self.gpu_context.resolve_videoframe_texture(&ipc_frame) {
            Ok(tex) => tex,
            Err(e) => {
                tracing::warn!(
                    "Display {}: Failed to resolve texture for '{}': {}",
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

        let camera_image_view = match camera_texture.inner.image_view() {
            Ok(view) => view,
            Err(e) => {
                tracing::warn!("Display {}: camera texture image view error: {}", self.window_id, e);
                return;
            }
        };
        let src_width = camera_texture.width();
        let src_height = camera_texture.height();

        // Parse camera timeline semaphore value from frame_index
        let camera_timeline_wait_value: u64 = ipc_frame
            .frame_index
            .parse()
            .unwrap_or(0);

        // Get camera timeline semaphore handle for GPU-GPU sync (same-process only)
        let camera_timeline_raw = self.gpu_context.camera_timeline_semaphore();
        let camera_timeline_sem: Option<vk::Semaphore> = if camera_timeline_raw != 0 {
            Some(unsafe { std::mem::transmute(camera_timeline_raw) })
        } else {
            None
        };

        // Update descriptor set with camera texture
        let desc_image_info = vk::DescriptorImageInfo::builder()
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image_view(camera_image_view)
            .sampler(ps.sampler)
            .build();
        let desc_image_infos = [desc_image_info];
        let descriptor_write = vk::WriteDescriptorSet::builder()
            .dst_set(ps.descriptor_sets[frame_index])
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&desc_image_infos)
            .build();
        unsafe { device.update_descriptor_sets(&[descriptor_write], &[] as &[vk::CopyDescriptorSet]) };

        // Timeline semaphore wait: ensure frame N-MAX_FRAMES_IN_FLIGHT completed
        // before reusing slot N. On the first MAX_FRAMES_IN_FLIGHT frames,
        // wait_value is 0 and the semaphore starts at 0, so the wait returns
        // immediately (equivalent to fences starting signaled).
        state.frame_timeline_value += 1;
        let wait_value = state
            .frame_timeline_value
            .saturating_sub(MAX_FRAMES_IN_FLIGHT as u64);
        if wait_value > 0 {
            let semaphores = [state.frame_timeline_semaphore];
            let values = [wait_value];
            let wait_info = vk::SemaphoreWaitInfo::builder()
                .semaphores(&semaphores)
                .values(&values)
                .build();
            unsafe {
                let _ = device.wait_semaphores(&wait_info, u64::MAX);
            }
        }

        let image_available_semaphore = state.image_available_semaphores[frame_index];
        let command_buffer = state.command_buffers[frame_index];

        // Acquire next swapchain image
        let image_index = match unsafe {
            device.acquire_next_image_khr(
                state.swapchain,
                u64::MAX,
                image_available_semaphore,
                vk::Fence::null(),
            )
        } {
            Ok((index, _)) => index,
            Err(vk::ErrorCode::OUT_OF_DATE_KHR) => {
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
        // render_finished is per-swapchain-image: the present engine holds the
        // binary semaphore until the image is released, so signal/wait must be
        // keyed by image_index rather than frame_index.
        let render_finished_semaphore = state.render_finished_semaphores[image_index as usize];

        // Reset and re-record the pre-allocated command buffer
        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();

        let color_subresource_range = vk::ImageSubresourceRange::builder()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .base_mip_level(0)
            .level_count(1)
            .base_array_layer(0)
            .layer_count(1)
            .build();

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

            // Camera texture is already in SHADER_READ_ONLY_OPTIMAL (set by camera
            // after compute dispatch). Only need swapchain barrier.
            // Camera timeline semaphore wait in queue_submit2 ensures the texture is ready.
            // UNDEFINED old_layout: on first use each swapchain image is in UNDEFINED,
            // and on subsequent uses the previous contents are unconditionally discarded
            // by the CLEAR load_op below — so declaring UNDEFINED (which permits any
            // current layout) is valid for every frame and avoids a VUID-vkCmdDraw-None-09600
            // mismatch on the first submit for each image.
            let swapchain_barrier = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::NONE)
                .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(swapchain_image)
                .subresource_range(color_subresource_range)
                .build();
            let swapchain_barriers = [swapchain_barrier];
            let dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&swapchain_barriers)
                .build();
            device.cmd_pipeline_barrier2(command_buffer, &dep);

            // Begin dynamic rendering on swapchain image
            let color_attachment = vk::RenderingAttachmentInfo::builder()
                .image_view(state.swapchain_image_views[image_index as usize])
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue {
                        float32: [0.0, 0.0, 0.0, 1.0],
                    },
                })
                .build();
            let color_attachments = [color_attachment];
            let rendering_info = vk::RenderingInfo::builder()
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: state.swapchain_extent,
                })
                .layer_count(1)
                .color_attachments(&color_attachments)
                .build();

            device.cmd_begin_rendering(command_buffer, &rendering_info);

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
                &[ps.descriptor_sets[frame_index]],
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

            device.cmd_end_rendering(command_buffer);

            // sync2: Swapchain COLOR_ATTACHMENT_OPTIMAL → PRESENT_SRC_KHR
            let present_barrier = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::NONE)
                .dst_access_mask(vk::AccessFlags2::NONE)
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(swapchain_image)
                .subresource_range(color_subresource_range)
                .build();

            let present_barriers = [present_barrier];
            let post_render_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&present_barriers)
                .build();
            device.cmd_pipeline_barrier2(command_buffer, &post_render_dep);

            if device.end_command_buffer(command_buffer).is_err() {
                return;
            }

            // queue_submit2: wait on image_available + camera timeline, signal render_finished + display timeline
            let mut wait_semaphore_infos = vec![
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(image_available_semaphore)
                    .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                    .build(),
            ];

            // GPU-wait on camera's timeline semaphore at FRAGMENT_SHADER stage
            if let Some(cam_sem) = camera_timeline_sem {
                if camera_timeline_wait_value > 0 {
                    wait_semaphore_infos.push(
                        vk::SemaphoreSubmitInfo::builder()
                            .semaphore(cam_sem)
                            .value(camera_timeline_wait_value)
                            .stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                            .build(),
                    );
                }
            }

            let signal_semaphore_infos = [
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(render_finished_semaphore)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build(),
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(state.frame_timeline_semaphore)
                    .value(state.frame_timeline_value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build(),
            ];

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(command_buffer)
                .build();
            let cmd_infos = [cmd_info];

            let submit = vk::SubmitInfo2::builder()
                .wait_semaphore_infos(&wait_semaphore_infos)
                .signal_semaphore_infos(&signal_semaphore_infos)
                .command_buffer_infos(&cmd_infos)
                .build();

            if let Err(e) =
                self.vulkan_device.submit_to_queue(queue, &[submit], vk::Fence::null())
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
            let present_info = vk::PresentInfoKHR::builder()
                .wait_semaphores(&present_wait_semaphores)
                .swapchains(&swapchains)
                .image_indices(&image_indices)
                .build();

            match self.vulkan_device.present_to_queue(queue, &present_info) {
                Ok(_) => {}
                Err(vk::ErrorCode::OUT_OF_DATE_KHR) => {
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

        state.current_frame = (frame_index + 1) % MAX_FRAMES_IN_FLIGHT;
        let frame_idx = self.frame_counter.fetch_add(1, Ordering::Relaxed);

        // Debug feature: sample frame to PNG from HOST_VISIBLE pixel buffer.
        // Resolve pixel buffer on-demand for sampling (not in hot path).
        //
        // Filename carries BOTH the displayed-frame counter (`frame_idx`)
        // and the INPUT frame index (from `ipc_frame.frame_index`). The
        // input-frame index lets downstream tooling (PSNR rig, #305)
        // pair each decoded PNG with its reference input; the
        // displayed-frame counter guarantees filenames are unique even
        // when the display's `skip_to_latest` input mailbox re-reads
        // the same decoded frame while the decoder is stalled.
        if let Some(ref dir) = self.png_sample_dir {
            if frame_idx % self.png_sample_every == 0 {
                if let Ok(buf) = self.gpu_context.resolve_videoframe_buffer(&ipc_frame) {
                    let vk_buf = &buf.buffer_ref().inner;
                    let mapped_ptr = vk_buf.mapped_ptr();
                    if !mapped_ptr.is_null() {
                        let input_frame_index: u64 = ipc_frame.frame_index.parse().unwrap_or(frame_idx);
                        let path = dir.join(format!(
                            "display_{:03}_frame_{:06}_input_{:06}.png",
                            self.window_id, frame_idx, input_frame_index
                        ));
                        let len = (src_width as usize) * (src_height as usize) * 4;
                        let rgba = unsafe { std::slice::from_raw_parts(mapped_ptr, len) };
                        if let Err(e) = write_png_rgba(&path, src_width, src_height, rgba) {
                            tracing::warn!(
                                "Display {}: PNG sample save failed for input frame {}: {}",
                                self.window_id, input_frame_index, e
                            );
                        } else {
                            self.png_samples_saved += 1;
                            tracing::info!(
                                "Display {}: saved PNG sample {:?} (input {}, displayed {}, total saved {})",
                                self.window_id, path, input_frame_index, frame_idx, self.png_samples_saved
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Minimal PNG writer for 8-bit RGBA images. No dependencies, deflate via uncompressed blocks.
fn write_png_rgba(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;

    // PNG signature
    file.write_all(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])?;

    // IHDR chunk
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut file, b"IHDR", &ihdr)?;

    // Build raw image data with filter byte per row
    let stride = (width as usize) * 4;
    let mut raw = Vec::with_capacity((stride + 1) * (height as usize));
    for y in 0..height as usize {
        raw.push(0); // filter type: None
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    // zlib-wrapped uncompressed deflate (no compression, just framed)
    let zlib = build_zlib_uncompressed(&raw);
    write_chunk(&mut file, b"IDAT", &zlib)?;

    // IEND chunk
    write_chunk(&mut file, b"IEND", &[])?;

    Ok(())
}

fn write_chunk<W: std::io::Write>(w: &mut W, kind: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(kind)?;
    w.write_all(data)?;
    let crc = crc32(kind, data);
    w.write_all(&crc.to_be_bytes())?;
    Ok(())
}

fn build_zlib_uncompressed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    // zlib header: deflate, 32K window, no preset dict, fastest
    out.push(0x78);
    out.push(0x01);

    // Deflate stored blocks (max 65535 bytes each)
    let mut offset = 0;
    while offset < data.len() {
        let chunk_len = (data.len() - offset).min(65535);
        let is_last = offset + chunk_len == data.len();
        out.push(if is_last { 0x01 } else { 0x00 }); // BFINAL/BTYPE bits
        out.extend_from_slice(&(chunk_len as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk_len as u16)).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + chunk_len]);
        offset += chunk_len;
    }

    // Adler-32 checksum of the uncompressed data
    let adler = adler32(data);
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &b in kind.iter().chain(data.iter()) {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    crc ^ 0xFFFFFFFF
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
    let instance = vulkan_device.instance();
    let device = vulkan_device.device();
    let physical_device = vulkan_device.physical_device();
    let queue_family_index = vulkan_device.queue_family_index();

    // Create surface via vulkanalia window integration
    let surface = unsafe {
        vulkanalia::window::create_surface(instance, window, window)
    }
    .map_err(|e| StreamError::GpuError(format!("Failed to create Vulkan surface: {}", e)))?;

    // Check surface support for this queue family
    let surface_supported = unsafe {
        instance.get_physical_device_surface_support_khr(
            physical_device,
            queue_family_index,
            surface,
        )
    }
    .map_err(|e| StreamError::GpuError(format!("Failed to check surface support: {}", e)))?;

    if !surface_supported {
        unsafe { instance.destroy_surface_khr(surface, None) };
        return Err(StreamError::GpuError(
            "Graphics queue family does not support presentation to this surface".into(),
        ));
    }

    // Query surface capabilities
    let capabilities = unsafe {
        instance.get_physical_device_surface_capabilities_khr(physical_device, surface)
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to query surface capabilities: {}", e))
    })?;

    let surface_formats = unsafe {
        instance.get_physical_device_surface_formats_khr(physical_device, surface)
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to query surface formats: {}", e))
    })?;

    let present_modes = unsafe {
        instance.get_physical_device_surface_present_modes_khr(physical_device, surface)
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

    let swapchain_info = vk::SwapchainCreateInfoKHR::builder()
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
        .clipped(true)
        .build();

    let swapchain = unsafe { device.create_swapchain_khr(&swapchain_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create swapchain: {}", e)))?;

    let swapchain_images = unsafe { device.get_swapchain_images_khr(swapchain) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to get swapchain images: {}", e))
        })?;

    // Create command pool for this thread
    let pool_info = vk::CommandPoolCreateInfo::builder()
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .queue_family_index(queue_family_index)
        .build();

    let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {}", e)))?;

    // image_available: per-frame-in-flight (CPU↔GPU pipelining).
    // render_finished: per-swapchain-image — the present engine keeps a hold
    // on the binary semaphore until it releases the image for reuse, so the
    // signal must match the acquired image_index (not the CPU frame_index)
    // to avoid VUID-vkQueueSubmit2-semaphore-03868.
    let image_count = swapchain_images.len();
    let semaphore_info = vk::SemaphoreCreateInfo::builder().build();

    let mut image_available_semaphores = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
    for _ in 0..MAX_FRAMES_IN_FLIGHT {
        let image_available = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;
        image_available_semaphores.push(image_available);
    }

    let mut render_finished_semaphores = Vec::with_capacity(image_count);
    for _ in 0..image_count {
        let render_finished = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;
        render_finished_semaphores.push(render_finished);
    }

    // Timeline semaphore for multi-flight frame synchronization (Vulkan 1.2 core).
    // One semaphore tracks all frames — wait for value N-MAX_FRAMES_IN_FLIGHT
    // before reusing slot N.
    let mut timeline_type_info = vk::SemaphoreTypeCreateInfo::builder()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0)
        .build();
    let timeline_semaphore_info = vk::SemaphoreCreateInfo::builder()
        .push_next(&mut timeline_type_info)
        .build();
    let frame_timeline_semaphore =
        unsafe { device.create_semaphore(&timeline_semaphore_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create timeline semaphore: {}", e))
            })?;

    // Pre-allocate command buffers (one per in-flight frame).
    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(MAX_FRAMES_IN_FLIGHT as u32)
        .build();

    let command_buffers = unsafe { device.allocate_command_buffers(&alloc_info) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to allocate command buffers: {}", e))
        })?;

    // Create swapchain image views for dynamic rendering color attachments
    let mut swapchain_image_views = Vec::with_capacity(image_count);
    for &image in &swapchain_images {
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
            .format(surface_format.format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let view = unsafe { device.create_image_view(&view_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create swapchain image view: {}", e))
            })?;
        swapchain_image_views.push(view);
    }

    // Create sampler for camera texture sampling in fragment shader
    let sampler_info = vk::SamplerCreateInfo::builder()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .build();
    let sampler = unsafe { device.create_sampler(&sampler_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create sampler: {}", e)))?;

    // Descriptor set layout: binding 0 = combined image sampler (fragment stage)
    let ds_binding = vk::DescriptorSetLayoutBinding::builder()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        .build();
    let ds_bindings = [ds_binding];
    let ds_layout_info = vk::DescriptorSetLayoutCreateInfo::builder()
        .bindings(&ds_bindings)
        .build();
    let descriptor_set_layout =
        unsafe { device.create_descriptor_set_layout(&ds_layout_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!(
                    "Failed to create descriptor set layout: {}",
                    e
                ))
            })?;

    // Pipeline layout: push constant for scale (vec2) + offset (vec2) = 16 bytes
    let push_constant_range = vk::PushConstantRange::builder()
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(16)
        .build();
    let set_layouts = [descriptor_set_layout];
    let pipeline_layout_info = vk::PipelineLayoutCreateInfo::builder()
        .set_layouts(&set_layouts)
        .push_constant_ranges(std::slice::from_ref(&push_constant_range))
        .build();
    let pipeline_layout = unsafe { device.create_pipeline_layout(&pipeline_layout_info, None) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to create pipeline layout: {}", e))
        })?;

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

    let vert_module_info = vk::ShaderModuleCreateInfo::builder()
        .code(&vert_code)
        .build();
    let frag_module_info = vk::ShaderModuleCreateInfo::builder()
        .code(&frag_code)
        .build();

    let vert_module = unsafe { device.create_shader_module(&vert_module_info, None) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to create vertex shader module: {}", e))
        })?;
    let frag_module = unsafe { device.create_shader_module(&frag_module_info, None) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to create fragment shader module: {}", e))
        })?;

    let shader_stages = [
        vk::PipelineShaderStageCreateInfo::builder()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert_module)
            .name(b"main\0")
            .build(),
        vk::PipelineShaderStageCreateInfo::builder()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag_module)
            .name(b"main\0")
            .build(),
    ];

    // No vertex input — fullscreen triangle derives UVs from gl_VertexIndex
    let vertex_input_info = vk::PipelineVertexInputStateCreateInfo::builder().build();
    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::builder()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST)
        .build();

    // Dynamic viewport/scissor — set per-frame, no pipeline recreation on resize
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state_info = vk::PipelineDynamicStateCreateInfo::builder()
        .dynamic_states(&dynamic_states)
        .build();

    let viewports = [vk::Viewport::default()];
    let scissors = [vk::Rect2D::default()];
    let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
        .viewports(&viewports)
        .scissors(&scissors)
        .build();

    let rasterizer = vk::PipelineRasterizationStateCreateInfo::builder()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0)
        .build();

    let multisampling = vk::PipelineMultisampleStateCreateInfo::builder()
        .rasterization_samples(vk::SampleCountFlags::_1)
        .build();

    let color_blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        )
        .blend_enable(false)
        .build();
    let color_blend_attachments = [color_blend_attachment];
    let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
        .attachments(&color_blend_attachments)
        .build();

    // Dynamic rendering: specify color attachment format via pNext
    let color_attachment_formats = [surface_format.format];
    let mut pipeline_rendering_info = vk::PipelineRenderingCreateInfo::builder()
        .color_attachment_formats(&color_attachment_formats)
        .build();

    let pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
        .stages(&shader_stages)
        .vertex_input_state(&vertex_input_info)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterizer)
        .multisample_state(&multisampling)
        .color_blend_state(&color_blend_state)
        .dynamic_state(&dynamic_state_info)
        .layout(pipeline_layout)
        .push_next(&mut pipeline_rendering_info)
        .build();

    let graphics_pipeline = unsafe {
        device.create_graphics_pipelines(
            vk::PipelineCache::null(),
            &[pipeline_info],
            None,
        )
    }
    .map_err(|e| {
        StreamError::GpuError(format!("Failed to create graphics pipeline: {}", e))
    })?
    .0[0];

    // Shader modules no longer needed after pipeline creation
    unsafe {
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);
    }

    // Descriptor pool and sets — one per in-flight frame to avoid updating
    // a descriptor set while a previous frame's command buffer is still pending.
    let pool_size = vk::DescriptorPoolSize::builder()
        .type_(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(MAX_FRAMES_IN_FLIGHT as u32)
        .build();
    let pool_sizes = [pool_size];
    let descriptor_pool_info = vk::DescriptorPoolCreateInfo::builder()
        .max_sets(MAX_FRAMES_IN_FLIGHT as u32)
        .pool_sizes(&pool_sizes)
        .build();
    let descriptor_pool =
        unsafe { device.create_descriptor_pool(&descriptor_pool_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create descriptor pool: {}", e))
            })?;

    let set_layouts_alloc: Vec<vk::DescriptorSetLayout> =
        vec![descriptor_set_layout; MAX_FRAMES_IN_FLIGHT];
    let ds_alloc_info = vk::DescriptorSetAllocateInfo::builder()
        .descriptor_pool(descriptor_pool)
        .set_layouts(&set_layouts_alloc)
        .build();
    let descriptor_sets = unsafe { device.allocate_descriptor_sets(&ds_alloc_info) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to allocate descriptor sets: {}", e))
        })?;

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
            swapchain_image_views,
        },
        PersistentPipelineState {
            graphics_pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_sets,
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
    let instance = vulkan_device.instance();
    let device = vulkan_device.device();
    let physical_device = vulkan_device.physical_device();
    let queue_family_index = vulkan_device.queue_family_index();

    let surface = old_state.surface;

    let capabilities = unsafe {
        instance.get_physical_device_surface_capabilities_khr(physical_device, surface)
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
        instance.get_physical_device_surface_present_modes_khr(physical_device, surface)
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

    let swapchain_info = vk::SwapchainCreateInfoKHR::builder()
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
        .old_swapchain(old_state.swapchain)
        .build();

    let swapchain = unsafe { device.create_swapchain_khr(&swapchain_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to recreate swapchain: {}", e)))?;

    let swapchain_images = unsafe { device.get_swapchain_images_khr(swapchain) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to get swapchain images: {}", e))
        })?;

    // Create new command pool
    let pool_info = vk::CommandPoolCreateInfo::builder()
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .queue_family_index(queue_family_index)
        .build();

    let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
        .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {}", e)))?;

    // image_available: per-frame-in-flight. render_finished: per-swapchain-image
    // (see create path for rationale).
    let new_image_count = swapchain_images.len();
    let semaphore_info = vk::SemaphoreCreateInfo::builder().build();

    let mut image_available_semaphores = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
    for _ in 0..MAX_FRAMES_IN_FLIGHT {
        let image_available = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;
        image_available_semaphores.push(image_available);
    }

    let mut render_finished_semaphores = Vec::with_capacity(new_image_count);
    for _ in 0..new_image_count {
        let render_finished = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {}", e)))?;
        render_finished_semaphores.push(render_finished);
    }

    // Timeline semaphore for multi-flight frame synchronization
    let mut timeline_type_info = vk::SemaphoreTypeCreateInfo::builder()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0)
        .build();
    let timeline_semaphore_info = vk::SemaphoreCreateInfo::builder()
        .push_next(&mut timeline_type_info)
        .build();
    let frame_timeline_semaphore =
        unsafe { device.create_semaphore(&timeline_semaphore_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create timeline semaphore: {}", e))
            })?;

    // Pre-allocate command buffers (one per in-flight frame).
    let alloc_info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(MAX_FRAMES_IN_FLIGHT as u32)
        .build();

    let command_buffers = unsafe { device.allocate_command_buffers(&alloc_info) }
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to allocate command buffers: {}", e))
        })?;

    // Create swapchain image views for dynamic rendering color attachments
    let mut swapchain_image_views = Vec::with_capacity(new_image_count);
    for &image in &swapchain_images {
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image)
            .view_type(vk::ImageViewType::_2D)
            .format(old_state.swapchain_format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .build();
        let view = unsafe { device.create_image_view(&view_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create swapchain image view: {}", e))
            })?;
        swapchain_image_views.push(view);
    }

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
        swapchain_image_views,
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
        device.destroy_swapchain_khr(state.swapchain, None);
    }
}

/// Destroy all swapchain state including the surface (but not persistent pipeline objects).
fn destroy_swapchain_state(
    vulkan_device: &crate::vulkan::rhi::VulkanDevice,
    state: &SwapchainState,
) {
    let instance = vulkan_device.instance();
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
        device.destroy_swapchain_khr(state.swapchain, None);
        instance.destroy_surface_khr(state.surface, None);
    }
}
