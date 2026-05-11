// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux display processor — winit window + Vulkan presentation via
//! [`VulkanPresentTarget`].
//!
//! Owns no raw Vulkan handles itself; every GPU resource flows through
//! the host RHI public surface (`VulkanPresentTarget`,
//! `VulkanGraphicsKernel`, `RhiCommandRecorder`, `VulkanTextureReadback`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::engine::host_rhi::{
    HostVulkanDevice, PresentFrame, VulkanAccess, VulkanGraphicsKernel, VulkanPresentTarget,
    VulkanStage, VulkanTextureReadback,
};
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    PrimitiveTopology, RasterizationState, ScissorRect, Texture, TextureReadbackDescriptor,
    TextureSourceLayout, VertexInputState, Viewport,
};
use streamlib_consumer_rhi::VulkanLayout;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes};

use crate::_generated_::tatolab__display::display_config::ScalingMode;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct LinuxWindowId(pub u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

/// Compiled-in display-blit SPIR-V (built by this package's `build.rs`).
const DISPLAY_BLIT_VERT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.vert.spv"));
const DISPLAY_BLIT_FRAG_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.frag.spv"));

#[streamlib::sdk::processor("Display")]
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

impl streamlib::sdk::processors::ManualProcessor for LinuxDisplayProcessor::Processor {
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
            "Display {}: start() called — spawning render thread",
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
            .ok_or_else(|| Error::Configuration("GPU context not initialized".into()))?;

        // Engine-bridge: reach the underlying `HostVulkanDevice` via the
        // `HostGpuDeviceExt` trait. The Sandbox clone is what the render
        // thread keeps for steady-state frame resolution.
        let vulkan_device = Arc::clone(ctx.gpu_full_access().device().vulkan_device());

        running.store(true, Ordering::Release);

        let render_thread = std::thread::Builder::new()
            .name(format!("display-{}-render", window_id))
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tracing::debug!("Display {}: Render thread started", window_id);

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
                    present_target: None,
                    graphics_kernel: None,
                    frame_limit,
                    png_sample_dir,
                    png_sample_every,
                    png_samples_saved: 0,
                    png_texture_readback: None,
                };

                if let Err(e) = event_loop.run_app(&mut app) {
                    tracing::error!("Display {}: Event loop error: {}", window_id, e);
                }

                // If the loop exited on its own (frame_limit, close, error),
                // publish RuntimeShutdown so the runtime stops. Skip when stop()
                // triggered the exit — the runtime is already tearing down.
                if !stop_called.load(Ordering::Acquire) {
                    use streamlib::sdk::pubsub::{Event, RuntimeEvent, PUBSUB};
                    tracing::info!(
                        "Display {}: Event loop exited, requesting runtime shutdown",
                        window_id
                    );
                    let shutdown_event =
                        Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
                    PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);
                }

                // Present target + kernel drop in reverse construction order via
                // App field destruction; both clean up their GPU resources.
                drop(app);

                tracing::debug!("Display {}: Render thread exiting", window_id);
            })
            .map_err(|e| Error::Runtime(format!("Failed to spawn render thread: {}", e)))?;

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
                    .map_err(|_| Error::Runtime("Render thread panicked".into()))?;
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
// Event loop handler — owns the window + present target + per-frame draws
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct DisplayEventLoopHandler {
    window: Option<Window>,
    vulkan_device: Arc<HostVulkanDevice>,
    gpu_context: GpuContextLimitedAccess,
    inputs: streamlib::sdk::iceoryx2::InputMailboxes,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    window_id: u64,
    width: u32,
    height: u32,
    window_title: String,
    vsync: bool,
    scaling_mode: ScalingMode,
    present_target: Option<VulkanPresentTarget>,
    graphics_kernel: Option<Arc<VulkanGraphicsKernel>>,
    frame_limit: Option<u64>,
    png_sample_dir: Option<std::path::PathBuf>,
    png_sample_every: u64,
    png_samples_saved: u64,
    png_texture_readback: Option<Arc<VulkanTextureReadback>>,
}

impl ApplicationHandler for DisplayEventLoopHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
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

        // Build the present target + graphics kernel for steady-state rendering.
        let present_target = match VulkanPresentTarget::new(
            &self.vulkan_device,
            &window,
            self.width,
            self.height,
            self.vsync,
        ) {
            Ok(pt) => pt,
            Err(e) => {
                tracing::error!(
                    "Display {}: Failed to construct VulkanPresentTarget: {}",
                    self.window_id,
                    e
                );
                event_loop.exit();
                return;
            }
        };

        let color_format = present_target.color_format();
        let kernel = match build_display_kernel(&self.vulkan_device, color_format) {
            Ok(k) => Arc::new(k),
            Err(e) => {
                tracing::error!(
                    "Display {}: Failed to construct display graphics kernel: {}",
                    self.window_id,
                    e
                );
                event_loop.exit();
                return;
            }
        };

        tracing::info!(
            "Display {}: Vulkan present target ready ({}x{}, {:?})",
            self.window_id,
            present_target.current_extent().0,
            present_target.current_extent().1,
            color_format
        );

        self.present_target = Some(present_target);
        self.graphics_kernel = Some(kernel);
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
                    return;
                }
                tracing::debug!(
                    "Display {}: Resized to {}x{}",
                    self.window_id,
                    new_size.width,
                    new_size.height
                );
                self.width = new_size.width;
                self.height = new_size.height;

                if let Some(pt) = self.present_target.as_mut() {
                    if let Err(e) = pt.recreate(new_size.width, new_size.height) {
                        tracing::error!(
                            "Display {}: Failed to recreate present target: {}",
                            self.window_id,
                            e
                        );
                        event_loop.exit();
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
                window.request_redraw();
            } else {
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(1),
                ));
            }
        }
    }
}

impl DisplayEventLoopHandler {
    fn render_frame(&mut self) {
        if !self.inputs.has_data("video") {
            return;
        }
        let Some(present_target) = self.present_target.as_mut() else {
            return;
        };
        let Some(graphics_kernel) = self.graphics_kernel.as_ref().cloned() else {
            return;
        };

        let ipc_frame: streamlib::sdk::_generated_::VideoFrame = match self.inputs.read("video") {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!("Display {}: Failed to read frame: {}", self.window_id, e);
                return;
            }
        };

        // Resolve the texture + registration via the engine's blessed API.
        let registration = match self.gpu_context.resolve_video_frame_registration(&ipc_frame) {
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
        let camera_texture: Texture = registration.texture().clone();
        let src_width = camera_texture.width();
        let src_height = camera_texture.height();

        // Producer timeline value parsed from frame_index — the producer
        // signals this on writeback completion; we GPU-wait it at FRAGMENT_SHADER.
        let video_source_timeline_wait_value: u64 = ipc_frame.frame_index.parse().unwrap_or(0);
        let video_source_timeline =
            self.gpu_context.video_source_timeline_semaphore();

        let scaling_mode = self.scaling_mode.clone();
        let kernel_for_draw = Arc::clone(&graphics_kernel);
        let window_id = self.window_id;

        let result = present_target.render_frame(|frame: &mut PresentFrame<'_>| {
            let frame_index = frame.frame_index;
            let extent = frame.extent;

            // Stage the camera texture as the kernel's binding-0 sampled texture
            // for this descriptor-ring slot.
            kernel_for_draw.set_sampled_texture(frame_index, 0, &camera_texture)?;

            // Transition the camera texture into SHADER_READ_ONLY_OPTIMAL if it
            // isn't already there. Producers (camera ring textures, adapter
            // outputs) may publish in different layouts; the registration
            // declares what's current.
            let camera_current_layout = registration.current_layout();
            if camera_current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
                frame.recorder.record_image_barrier(
                    &camera_texture,
                    camera_current_layout,
                    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::FRAGMENT_SHADER,
                    VulkanAccess::MEMORY_WRITE,
                    VulkanAccess::SHADER_READ,
                )?;
                registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
            }

            // GPU-wait on the producer's timeline for content-finished sync.
            if let Some(ref timeline) = video_source_timeline {
                if video_source_timeline_wait_value > 0 {
                    frame.add_timeline_wait(
                        timeline,
                        video_source_timeline_wait_value,
                        VulkanStage::FRAGMENT_SHADER,
                    );
                }
            }

            // Compute aspect-ratio-aware scale per the configured mode.
            let src_aspect = src_width as f32 / src_height as f32;
            let dst_aspect = extent.0 as f32 / extent.1 as f32;
            let (scale_x, scale_y) = match scaling_mode {
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
            kernel_for_draw.set_push_constants_value(frame_index, &push_constants)?;

            // Begin render pass on the acquired swapchain image with CLEAR.
            frame.begin_rendering(Some([0.0, 0.0, 0.0, 1.0]))?;

            // Draw — bind pipeline + descriptors + push consts, viewport/scissor
            // matching the swapchain extent.
            let draw = DrawCall {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
                viewport: Some(Viewport::full(extent.0, extent.1)),
                scissor: Some(ScissorRect::full(extent.0, extent.1)),
            };
            frame
                .recorder
                .record_draw(&kernel_for_draw, frame_index, &draw)?;

            frame.end_rendering()?;
            Ok(())
        });

        match result {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!("Display {}: swapchain out of date — will recreate on next resize", window_id);
            }
            Err(e) => {
                tracing::warn!("Display {}: render frame failed: {}", window_id, e);
                return;
            }
        }

        let frame_idx = self.frame_counter.fetch_add(1, Ordering::Relaxed);

        if let Some(ref dir) = self.png_sample_dir {
            if frame_idx % self.png_sample_every == 0 {
                let input_frame_index: u64 =
                    ipc_frame.frame_index.parse().unwrap_or(frame_idx);
                let path = dir.join(format!(
                    "display_{:03}_frame_{:06}_input_{:06}.png",
                    self.window_id, frame_idx, input_frame_index
                ));
                if let Ok(buf) = self.gpu_context.resolve_video_frame_buffer(&ipc_frame) {
                    use streamlib::sdk::engine::HostPixelBufferRefExt;
                    let vk_buf = buf.buffer_ref().vulkan_inner();
                    let mapped_ptr = vk_buf.mapped_ptr();
                    if !mapped_ptr.is_null() {
                        let len = (src_width as usize) * (src_height as usize) * 4;
                        let rgba =
                            unsafe { std::slice::from_raw_parts(mapped_ptr, len) };
                        if let Err(e) = write_png_rgba(&path, src_width, src_height, rgba) {
                            tracing::warn!(
                                "Display {}: PNG sample save failed for input frame {}: {}",
                                self.window_id, input_frame_index, e
                            );
                        } else {
                            self.png_samples_saved += 1;
                            tracing::info!(
                                "Display {}: saved PNG sample {:?} (input {}, displayed {}, total saved {})",
                                self.window_id,
                                path,
                                input_frame_index,
                                frame_idx,
                                self.png_samples_saved
                            );
                        }
                    }
                } else {
                    self.sample_texture_to_png(&camera_texture, &path, frame_idx, input_frame_index);
                }
            }
        }
    }

    fn sample_texture_to_png(
        &mut self,
        texture: &Texture,
        path: &std::path::Path,
        frame_idx: u64,
        input_frame_index: u64,
    ) {
        let format = texture.format();
        let width = texture.width();
        let height = texture.height();

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

        if self.png_texture_readback.is_none() {
            let descriptor = TextureReadbackDescriptor {
                label: "display-png-sample",
                format,
                width,
                height,
            };
            match VulkanTextureReadback::new_into_stream_error(
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

fn build_display_kernel(
    device: &Arc<HostVulkanDevice>,
    attachment_format: streamlib::sdk::rhi::TextureFormat,
) -> Result<VulkanGraphicsKernel> {
    let stages = [
        GraphicsStage::vertex(DISPLAY_BLIT_VERT_SPV),
        GraphicsStage::fragment(DISPLAY_BLIT_FRAG_SPV),
    ];
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let descriptor = GraphicsKernelDescriptor {
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
        descriptor_sets_in_flight:
            streamlib::sdk::engine::host_rhi::MAX_FRAMES_IN_FLIGHT as u32,
    };
    VulkanGraphicsKernel::new(device, &descriptor)
}

// ---------------------------------------------------------------------------
// Minimal PNG writer (no external deps)
// ---------------------------------------------------------------------------

fn maybe_swizzle_bgra_to_rgba(
    format: streamlib::sdk::rhi::TextureFormat,
    bytes: &[u8],
) -> std::borrow::Cow<'_, [u8]> {
    use streamlib::sdk::rhi::TextureFormat;
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

fn write_png_rgba(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;

    file.write_all(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])?;

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6);
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    write_chunk(&mut file, b"IHDR", &ihdr)?;

    let stride = (width as usize) * 4;
    let mut raw = Vec::with_capacity((stride + 1) * (height as usize));
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    let zlib = build_zlib_uncompressed(&raw);
    write_chunk(&mut file, b"IDAT", &zlib)?;
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
    out.push(0x78);
    out.push(0x01);

    let mut offset = 0;
    while offset < data.len() {
        let chunk_len = (data.len() - offset).min(65535);
        let is_last = offset + chunk_len == data.len();
        out.push(if is_last { 0x01 } else { 0x00 });
        out.extend_from_slice(&(chunk_len as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk_len as u16)).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + chunk_len]);
        offset += chunk_len;
    }

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

#[cfg(test)]
mod tests {
    use super::maybe_swizzle_bgra_to_rgba;
    use streamlib::sdk::rhi::TextureFormat;

    /// `Rgba8Unorm` is already RGBA-in-memory; helper must borrow input
    /// unchanged so the no-swizzle branch stays a zero-copy pass-through.
    #[test]
    fn rgba_source_passes_through_unchanged() {
        let bytes = [0x10, 0x20, 0x30, 0xFF, 0x40, 0x50, 0x60, 0x80];
        let out = maybe_swizzle_bgra_to_rgba(TextureFormat::Rgba8Unorm, &bytes);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*out, &bytes);
    }

    /// `Bgra8Unorm` memory holds bytes in B,G,R,A order. After swizzle
    /// they must come out in R,G,B,A order so the PNG encode (which
    /// treats input as RGBA-in-memory) channels-correctly.
    #[test]
    fn bgra_source_swaps_red_and_blue_per_pixel() {
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

    /// Counter-test: feeding BGRA-in-memory bytes through the no-swizzle
    /// branch leaves them BGRA-ordered — locks in that the swizzle is
    /// actually doing the swap, not silently coincidental.
    #[test]
    fn unsorted_branch_does_not_swap_proves_swizzle_is_load_bearing() {
        let bgra_bytes = [0xFF, 0x00, 0x00, 0xFF];
        let out = maybe_swizzle_bgra_to_rgba(TextureFormat::Rgba8Unorm, &bgra_bytes);
        assert_eq!(&*out, &bgra_bytes);
        assert_ne!(&*out, &[0x00, 0x00, 0xFF, 0xFF]);
    }
}
