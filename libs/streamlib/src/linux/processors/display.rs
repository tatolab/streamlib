// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_display_config::ScalingMode;
use crate::core::{GpuContextLimitedAccess, Result, RuntimeContextFullAccess, StreamError};
use streamlib_consumer_rhi::VulkanLayout;
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
                    png_texture_readback: None,
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

                // Camera textures are owned by the texture_cache / HostVulkanTexture Drop.

                // Clean up swapchain resources before exiting
                if let Some(state) = app.swapchain_state.take() {
                    destroy_swapchain_state(&app.vulkan_device, &state);
                }

                // Clean up persistent pipeline resources — VulkanGraphicsKernel
                // owns its pipeline, layout, descriptor pool, and default
                // sampler; dropping the kernel tears them down.
                drop(app.pipeline_state.take());

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

/// Persistent render pipeline state that survives swapchain recreation.
///
/// The single graphics-pipeline VkPipeline + VkPipelineLayout +
/// VkDescriptorSetLayout + VkDescriptorPool + per-frame VkDescriptorSet ring
/// + default sampler are all owned by [`VulkanGraphicsKernel`]; dropping the
/// kernel tears them down in the right order.
struct PersistentPipelineState {
    graphics_kernel: Arc<crate::vulkan::rhi::VulkanGraphicsKernel>,
}

/// Device-local VkImage used as the camera texture for fragment shader sampling.
// ---------------------------------------------------------------------------
// Event loop handler — owns the window and drives frame rendering
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct DisplayEventLoopHandler {
    window: Option<Window>,
    vulkan_device: Arc<crate::vulkan::rhi::HostVulkanDevice>,
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
    /// Lazily-built readback handle for the texture-surface PNG sampling path.
    /// Pinned to the first sampled texture's format/extent; samples whose
    /// texture disagrees are skipped with a warning.
    png_texture_readback: Option<Arc<crate::vulkan::rhi::VulkanTextureReadback>>,
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
        //
        // We resolve the full registration (not just the texture) so we can
        // read `current_layout` and issue a correct source-layout barrier
        // before sampling — adapter-output textures (OpenGL, Skia, Vulkan
        // adapter outputs) may arrive in a layout other than the descriptor
        // binding's claimed `SHADER_READ_ONLY_OPTIMAL`, and the wrong
        // claim is the bug #616 fixes.
        let registration = match self.gpu_context.resolve_videoframe_registration(&ipc_frame) {
            Ok(reg) => reg,
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
        let camera_texture = registration.texture().clone();

        let device = self.vulkan_device.device();
        let queue = self.vulkan_device.queue();

        let frame_index = state.current_frame;

        let camera_image = match camera_texture.inner.image() {
            Some(img) => img,
            None => {
                tracing::warn!(
                    "Display {}: camera texture has no VkImage handle",
                    self.window_id
                );
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

        // Stage the camera texture binding for this frame's descriptor-set
        // ring slot. The kernel flushes the write at cmd_bind_and_draw time.
        if let Err(e) = ps.graphics_kernel.set_sampled_texture(
            frame_index as u32,
            0,
            &camera_texture,
        ) {
            tracing::warn!(
                "Display {}: failed to set camera texture binding: {}",
                self.window_id,
                e
            );
            return;
        }

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

            // Swapchain image: UNDEFINED → COLOR_ATTACHMENT_OPTIMAL.
            // UNDEFINED old_layout: on first use each swapchain image is in UNDEFINED,
            // and on subsequent uses the previous contents are unconditionally discarded
            // by the CLEAR load_op below — so declaring UNDEFINED (which permits any
            // current layout) is valid for every frame and avoids a VUID-vkCmdDraw-None-09600
            // mismatch on the first submit for each image.
            //
            // Camera-input texture: source layout depends on producer. Camera ring
            // textures are left in SHADER_READ_ONLY_OPTIMAL after every compute submit;
            // adapter-output textures (e.g. AvatarCharacter via streamlib-adapter-opengl)
            // arrive in whatever layout the registration declares (typically UNDEFINED
            // for adapters that don't transition the Vulkan tracker, or whatever the
            // last consumer's `update_layout` set). The barrier here transitions from
            // the registered current layout → SHADER_READ_ONLY_OPTIMAL so the
            // descriptor binding's claimed `image_layout` matches reality. The camera
            // timeline-semaphore wait in queue_submit2 covers the producer-finished
            // GPU sync (independent of layout); this barrier covers the layout
            // contract Vulkan validation enforces. Skipped when already in
            // SHADER_READ_ONLY_OPTIMAL to avoid a no-op submission.
            let camera_current_layout = registration.current_layout();
            let mut image_barriers: Vec<vk::ImageMemoryBarrier2> = Vec::with_capacity(2);
            image_barriers.push(
                vk::ImageMemoryBarrier2::builder()
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
                    .build(),
            );
            if camera_current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
                image_barriers.push(
                    vk::ImageMemoryBarrier2::builder()
                        // ALL_COMMANDS / MEMORY_WRITE in the source masks is the
                        // tolerant pattern that covers any producer (camera compute,
                        // OpenGL adapter glFinish, future Vulkan/Skia adapters).
                        // Mirrors the source-side masks `VulkanTextureReadback`
                        // uses for the same producer-agnostic purpose.
                        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                        .old_layout(camera_current_layout.as_vk())
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(camera_image)
                        .subresource_range(color_subresource_range)
                        .build(),
                );
            }
            let dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&image_barriers)
                .build();
            device.cmd_pipeline_barrier2(command_buffer, &dep);
            if camera_current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
                registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
            }

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

            // Compute aspect-ratio-aware scale per the configured mode.
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
            if let Err(e) = ps.graphics_kernel.set_push_constants_value(
                frame_index as u32,
                &push_constants,
            ) {
                tracing::warn!(
                    "Display {}: failed to stage push constants: {}",
                    self.window_id,
                    e
                );
                return;
            }

            // Bind pipeline + descriptor set + push constants + draw, with
            // dynamic viewport/scissor matching the swapchain extent.
            let draw = crate::core::rhi::DrawCall {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
                viewport: Some(crate::core::rhi::Viewport::full(
                    state.swapchain_extent.width,
                    state.swapchain_extent.height,
                )),
                scissor: Some(crate::core::rhi::ScissorRect::full(
                    state.swapchain_extent.width,
                    state.swapchain_extent.height,
                )),
            };
            if let Err(e) = ps.graphics_kernel.cmd_bind_and_draw(
                command_buffer,
                frame_index as u32,
                &draw,
            ) {
                tracing::warn!(
                    "Display {}: graphics kernel draw failed: {}",
                    self.window_id,
                    e
                );
                return;
            }

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

        // Debug feature: sample frame to PNG. Two paths:
        //   1. HOST_VISIBLE pixel-buffer fast path — zero-copy from CPU's
        //      perspective, fires for surfaces registered as pixel buffers
        //      (decoded frames, BgraFileSource).
        //   2. Texture-surface fallback — for surfaces registered via
        //      `surface_store.register_texture` (DMA-BUF VkImages from
        //      adapter outputs: AvatarCharacter, Skia, Glitch). Routes
        //      through the canonical `VulkanTextureReadback` RHI primitive
        //      (Granite-style `copy_image_to_buffer + vkSemaphoreWaitKHR`,
        //      reusable persistent staging buffer + command pool).
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
                let input_frame_index: u64 = ipc_frame.frame_index.parse().unwrap_or(frame_idx);
                let path = dir.join(format!(
                    "display_{:03}_frame_{:06}_input_{:06}.png",
                    self.window_id, frame_idx, input_frame_index
                ));
                if let Ok(buf) = self.gpu_context.resolve_videoframe_buffer(&ipc_frame) {
                    let vk_buf = &buf.buffer_ref().inner;
                    let mapped_ptr = vk_buf.mapped_ptr();
                    if !mapped_ptr.is_null() {
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
                } else {
                    self.sample_texture_to_png(
                        &camera_texture,
                        &path,
                        frame_idx,
                        input_frame_index,
                    );
                }
            }
        }
    }

    /// Capture an adapter-output texture into a PNG via the RHI's
    /// `VulkanTextureReadback` primitive. Lazily constructs (and caches)
    /// the readback handle on first use; format/extent are pinned to the
    /// first texture sampled. Subsequent samples whose texture disagrees
    /// are skipped with a warning rather than rebuilding the handle.
    fn sample_texture_to_png(
        &mut self,
        texture: &crate::core::rhi::StreamTexture,
        path: &std::path::Path,
        frame_idx: u64,
        input_frame_index: u64,
    ) {
        use crate::core::rhi::{TextureReadbackDescriptor, TextureSourceLayout};

        let format = texture.format();
        let width = texture.width();
        let height = texture.height();

        // PNG writer assumes 4-byte RGBA-shape pixels. Skip exotic formats
        // (NV12, fp16, fp32) — these are valid texture formats but the
        // handwritten PNG writer can't render them.
        if format.bytes_per_pixel() != 4 {
            tracing::warn!(
                "Display {}: skip PNG sample for input frame {} — texture format {:?} \
                 (bpp={}) not supported by PNG writer",
                self.window_id,
                input_frame_index,
                format,
                format.bytes_per_pixel()
            );
            return;
        }

        // Lazy: build the readback handle on first sample.
        if self.png_texture_readback.is_none() {
            let descriptor = TextureReadbackDescriptor {
                label: "display-png-sample",
                format,
                width,
                height,
            };
            match crate::vulkan::rhi::VulkanTextureReadback::new_into_stream_error(
                &self.vulkan_device,
                &descriptor,
            ) {
                Ok(handle) => {
                    tracing::info!(
                        "Display {}: created texture-readback handle for PNG sampling \
                         ({:?}, {}x{}, {} bytes)",
                        self.window_id,
                        format,
                        width,
                        height,
                        descriptor.staging_size()
                    );
                    self.png_texture_readback = Some(Arc::new(handle));
                }
                Err(e) => {
                    tracing::warn!(
                        "Display {}: PNG texture-readback handle creation failed: {}",
                        self.window_id,
                        e
                    );
                    return;
                }
            }
        }
        let readback = self.png_texture_readback.as_ref().unwrap().clone();

        // `TextureSourceLayout::General` covers adapter-output textures
        // (OpenGL post-glFinish, Skia, compute kernels) — the canonical
        // case this fallback exists to support. Native ring textures from
        // the camera processor register as pixel buffers and take the
        // fast path above, so they don't reach here in practice.
        let ticket = match readback.submit(texture, TextureSourceLayout::General) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "Display {}: PNG texture-readback submit failed for input frame {}: {}",
                    self.window_id,
                    input_frame_index,
                    e
                );
                return;
            }
        };
        let result = readback.wait_and_read_with(ticket, u64::MAX, |bytes| {
            let rgba = maybe_swizzle_bgra_to_rgba(format, bytes);
            write_png_rgba(path, width, height, &rgba)
        });
        match result {
            Ok(Ok(())) => {
                self.png_samples_saved += 1;
                tracing::info!(
                    "Display {}: saved PNG sample (texture path) {:?} \
                     (input {}, displayed {}, total saved {})",
                    self.window_id,
                    path,
                    input_frame_index,
                    frame_idx,
                    self.png_samples_saved
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "Display {}: PNG sample (texture path) save failed for input frame {}: {}",
                    self.window_id,
                    input_frame_index,
                    e
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Display {}: PNG texture-readback wait failed for input frame {}: {}",
                    self.window_id,
                    input_frame_index,
                    e
                );
            }
        }
    }
}

/// Reorder bytes to RGBA-in-memory before PNG encode.
///
/// `VulkanTextureReadback` hands the staging bytes back in the texture's
/// native channel order. The PNG writer treats every input as
/// RGBA-in-memory, so source textures whose memory layout is BGRA need
/// an R↔B swap before the encode — otherwise blue and red swap and a
/// dark blue wall renders as orange. The OpenGL adapter picks
/// `Bgra8Unorm` for its render-target DMA-BUFs to match the swapchain's
/// native format and avoid a swizzle in the display path; that
/// optimization just doesn't carry to the PNG export.
fn maybe_swizzle_bgra_to_rgba(
    format: crate::core::rhi::TextureFormat,
    bytes: &[u8],
) -> std::borrow::Cow<'_, [u8]> {
    use crate::core::rhi::TextureFormat;
    let needs_swizzle = matches!(
        format,
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb,
    );
    if needs_swizzle {
        let mut rgba = bytes.to_vec();
        for chunk in rgba.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }
        std::borrow::Cow::Owned(rgba)
    } else {
        std::borrow::Cow::Borrowed(bytes)
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
    vulkan_device: &Arc<crate::vulkan::rhi::HostVulkanDevice>,
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

    // Surface-format-derived pipeline target. Convert the raw vk::Format the
    // surface picked into a TextureFormat for the kernel descriptor; if the
    // compositor handed us a format the kernel doesn't recognize as a color
    // attachment, fail loudly here rather than at first draw.
    let attachment_format = match surface_format.format {
        vk::Format::B8G8R8A8_UNORM => crate::core::rhi::TextureFormat::Bgra8Unorm,
        vk::Format::B8G8R8A8_SRGB => crate::core::rhi::TextureFormat::Bgra8UnormSrgb,
        vk::Format::R8G8B8A8_UNORM => crate::core::rhi::TextureFormat::Rgba8Unorm,
        vk::Format::R8G8B8A8_SRGB => crate::core::rhi::TextureFormat::Rgba8UnormSrgb,
        other => {
            return Err(StreamError::GpuError(format!(
                "Display swapchain surface format {other:?} not mapped to TextureFormat"
            )));
        }
    };

    // Build the graphics kernel (replaces the hand-rolled vertex+fragment
    // pipeline, descriptor set layout, descriptor pool, descriptor sets,
    // pipeline layout, and sampler — all owned by the kernel now). The
    // descriptor-set ring is sized to MAX_FRAMES_IN_FLIGHT so callers can
    // index by frame_index without races against in-flight rendering.
    use crate::core::rhi::{
        AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState,
        GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor,
        GraphicsPipelineState, GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage,
        MultisampleState, PrimitiveTopology, RasterizationState, VertexInputState,
    };
    let display_blit_vert = include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.vert.spv"));
    let display_blit_frag = include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.frag.spv"));
    let stages = [
        GraphicsStage::vertex(display_blit_vert),
        GraphicsStage::fragment(display_blit_frag),
    ];
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let kernel_descriptor = GraphicsKernelDescriptor {
        label: "display_blit",
        stages: &stages,
        bindings: &bindings,
        push_constants: GraphicsPushConstants {
            size: 16, // vec2 scale + vec2 offset
            stages: GraphicsShaderStageFlags::FRAGMENT,
        },
        pipeline_state: GraphicsPipelineState {
            topology: PrimitiveTopology::TriangleList,
            vertex_input: VertexInputState::None,
            rasterization: RasterizationState::default(),
            multisample: MultisampleState::default(),
            depth_stencil: DepthStencilState::Disabled,
            color_blend: ColorBlendState::Disabled {
                color_write_mask: ColorWriteMask::RGBA,
            },
            attachment_formats: AttachmentFormats::color_only(attachment_format),
            dynamic_state: GraphicsDynamicState::ViewportScissor,
        },
        descriptor_sets_in_flight: MAX_FRAMES_IN_FLIGHT as u32,
    };
    let graphics_kernel = Arc::new(crate::vulkan::rhi::VulkanGraphicsKernel::new(
        vulkan_device,
        &kernel_descriptor,
    )?);

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
        PersistentPipelineState { graphics_kernel },
    ))
}

fn recreate_swapchain(
    vulkan_device: &crate::vulkan::rhi::HostVulkanDevice,
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
    vulkan_device: &crate::vulkan::rhi::HostVulkanDevice,
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
    vulkan_device: &crate::vulkan::rhi::HostVulkanDevice,
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

#[cfg(test)]
mod tests {
    use super::maybe_swizzle_bgra_to_rgba;
    use crate::core::rhi::TextureFormat;

    /// `Rgba8Unorm` is already RGBA-in-memory; helper must borrow input
    /// unchanged so the no-swizzle branch stays a zero-copy pass-through.
    #[test]
    fn rgba_source_passes_through_unchanged() {
        let bytes = [0x10, 0x20, 0x30, 0xFF, 0x40, 0x50, 0x60, 0x80];
        let out = maybe_swizzle_bgra_to_rgba(TextureFormat::Rgba8Unorm, &bytes);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*out, &bytes);
    }

    /// `Rgba8UnormSrgb` follows the same no-swizzle branch as the linear
    /// variant — the sRGB tag affects sampling, not memory layout.
    #[test]
    fn rgba_srgb_source_passes_through_unchanged() {
        let bytes = [0x10, 0x20, 0x30, 0xFF];
        let out = maybe_swizzle_bgra_to_rgba(TextureFormat::Rgba8UnormSrgb, &bytes);
        assert_eq!(&*out, &bytes);
    }

    /// `Bgra8Unorm` memory holds bytes in B,G,R,A order. After swizzle
    /// they must come out in R,G,B,A order so the PNG encode (which
    /// treats input as RGBA-in-memory) channels-correctly.
    #[test]
    fn bgra_source_swaps_red_and_blue_per_pixel() {
        // Two pixels: pure-blue then pure-red, both in BGRA-in-memory.
        let bgra_bytes = [
            0xFF, 0x00, 0x00, 0xFF, // B=255, G=0, R=0, A=255 → blue
            0x00, 0x00, 0xFF, 0xFF, // B=0, G=0, R=255, A=255 → red
        ];
        let out = maybe_swizzle_bgra_to_rgba(TextureFormat::Bgra8Unorm, &bgra_bytes);
        let expected = [
            0x00, 0x00, 0xFF, 0xFF, // R=0, G=0, B=255, A=255 (RGBA blue)
            0xFF, 0x00, 0x00, 0xFF, // R=255, G=0, B=0, A=255 (RGBA red)
        ];
        assert_eq!(&*out, &expected);
    }

    /// `Bgra8UnormSrgb` must take the same swizzle path as the linear
    /// variant — both are BGRA-in-memory and the PNG writer can't
    /// disambiguate sRGB from linear bytes anyway.
    #[test]
    fn bgra_srgb_source_swaps_red_and_blue() {
        let bgra_bytes = [0xFF, 0x00, 0x00, 0xFF];
        let out = maybe_swizzle_bgra_to_rgba(TextureFormat::Bgra8UnormSrgb, &bgra_bytes);
        assert_eq!(&*out, &[0x00, 0x00, 0xFF, 0xFF]);
    }

    /// Counter-test: feeding BGRA-in-memory bytes through the no-swizzle
    /// branch leaves them BGRA-ordered. Mentally reverting the swizzle
    /// (or dropping `Bgra8Unorm` from the `matches!`) routes BGRA bytes
    /// through this branch — the output bytes match the input verbatim,
    /// which would mis-channel the PNG. Locks in that the swizzle is
    /// actually doing the swap, not silently coincidental.
    #[test]
    fn unsorted_branch_does_not_swap_proves_swizzle_is_load_bearing() {
        let bgra_bytes = [0xFF, 0x00, 0x00, 0xFF];
        let out = maybe_swizzle_bgra_to_rgba(TextureFormat::Rgba8Unorm, &bgra_bytes);
        // Bytes preserved verbatim — would render blue as red in PNG.
        assert_eq!(&*out, &bgra_bytes);
        // Sanity: the would-be-correct RGBA encoding does NOT match.
        assert_ne!(&*out, &[0x00, 0x00, 0xFF, 0xFF]);
    }
}
