// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux display processor — winit window + Vulkan presentation via the
//! engine-free plugin SDK's [`PresentTarget`] PluginAbiObject.
//!
//! Owns no raw Vulkan handles and never names the host device: every GPU
//! resource is minted host-side through the FullAccess surface reached via
//! [`GpuContextLimitedAccess::escalate`] ([`PresentTarget`],
//! [`VulkanGraphicsKernel`], [`TextureReadback`](streamlib_plugin_sdk::sdk::rhi::TextureReadback))
//! and driven per frame through each resource's own plugin-ABI methods.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use streamlib_plugin_sdk::sdk::context::{
    GpuContextFullAccess, GpuContextLimitedAccess, RuntimeContextFullAccess,
};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    PresentTarget, PrimitiveTopology, RasterizationState, ScissorRect, Texture, TextureFormat,
    TextureSourceLayout, VertexInputState, Viewport, VulkanAccess, VulkanGraphicsKernel,
    VulkanLayout, VulkanStage,
};
use streamlib_plugin_abi::{ColorTraitsRepr, HdrStaticMetadataRepr, RawWindowHandleRepr};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes};

use crate::_generated_::tatolab__display::display_config::ScalingMode;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct LinuxWindowId(pub u64);

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

/// Descriptor-set ring depth for the display blit kernel. The engine's
/// `MAX_FRAMES_IN_FLIGHT` lives in engine-internal code an engine-free
/// package cannot name, so the display owns its own frames-in-flight
/// policy for the blit kernel's descriptor sets (matched to the
/// swapchain's 2-deep present ring).
const DISPLAY_BLIT_FRAMES_IN_FLIGHT: u32 = 2;

/// Compiled-in display-blit SPIR-V (built by this package's `build.rs`).
const DISPLAY_BLIT_VERT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.vert.spv"));
const DISPLAY_BLIT_FRAG_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.frag.spv"));

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/display/Display",
    description = "Displays video frames in a window with vsync",
    execution = manual,
    scheduling = high,
    config = crate::_generated_::DisplayConfig,
    input("video", "@tatolab/core/VideoFrame", description = "Video frames to display in the window"),
)]
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

impl streamlib_plugin_sdk::sdk::processors::ManualProcessor for LinuxDisplayProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
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
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("Display {}: Teardown", self.window_title);
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
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

        // Engine-free GPU access: the render thread mints every GPU resource
        // host-side via `gpu_context.escalate(|full| full.create_*(..))`, never
        // a raw host device. `GpuContextLimitedAccess` is `Clone` + `Send`, so
        // the clone rides into the render thread.
        let gpu_context = self
            .gpu_context
            .clone()
            .ok_or_else(|| Error::Configuration("GPU context not initialized".into()))?;

        running.store(true, Ordering::Release);

        let render_thread = std::thread::Builder::new()
            .name(format!("display-{}-render", window_id))
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tracing::debug!("Display {}: Render thread started", window_id);

                // Headless degradation seam. A graph may include a display
                // processor on a box with no display server (a headless
                // drone, a CI container). Rather than failing, the display
                // degrades to a drain: it keeps consuming its wired input
                // and discards every frame, presenting nothing, so the same
                // graph runs unchanged on a desktop and a headless host.
                //
                // `STREAMLIB_DISPLAY_FORCE_HEADLESS=1` forces this path even
                // when a display IS available — a test hook and an ops
                // override for containers that should never open a window.
                let force_headless = std::env::var("STREAMLIB_DISPLAY_FORCE_HEADLESS")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                let frame_limit = std::env::var("STREAMLIB_DISPLAY_FRAME_LIMIT")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok());

                let event_loop = if force_headless {
                    tracing::warn!(
                        "Display {}: STREAMLIB_DISPLAY_FORCE_HEADLESS set — entering headless drain mode",
                        window_id
                    );
                    None
                } else {
                    match {
                        use winit::platform::x11::EventLoopBuilderExtX11;
                        EventLoop::builder().with_any_thread(true).build()
                    } {
                        Ok(el) => Some(el),
                        Err(e) => {
                            // No display server (no X11/Wayland) — the common
                            // headless case. Degrade to a drain, don't die.
                            tracing::warn!(
                                "Display {}: no display server available ({}) — degrading to headless drain mode",
                                window_id,
                                e
                            );
                            None
                        }
                    }
                };

                match event_loop {
                    None => {
                        run_headless_drain_loop(
                            &inputs,
                            &running,
                            &frame_counter,
                            frame_limit,
                            window_id,
                        );
                    }
                    Some(event_loop) => {
                        event_loop_proxy.set(event_loop.create_proxy()).ok();

                        let png_sample_dir = std::env::var("STREAMLIB_DISPLAY_PNG_SAMPLE_DIR")
                            .ok()
                            .map(std::path::PathBuf::from);
                        let png_sample_every = std::env::var("STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY")
                            .ok()
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(30);
                        // Test-only: when set, the render body deliberately
                        // returns `Err` at the given frame counter, exercising
                        // the present target's error-path drain (the host
                        // `end_frame` closes any dangling render pass and
                        // consumes the `image_available_semaphore` so the next
                        // slot reuse doesn't trip
                        // `VUID-vkQueueSubmit2-semaphore-03868`). Run with
                        // `VK_LOADER_LAYERS_ENABLE=*validation*` to surface any
                        // drain bug as a VUID at the next acquire.
                        let inject_error_at_frame =
                            std::env::var("STREAMLIB_DISPLAY_INJECT_ERROR_AT_FRAME")
                                .ok()
                                .and_then(|s| s.parse::<u64>().ok());

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
                            current_frame_color_info: None,
                            inactive: false,
                            frame_limit,
                            png_sample_dir,
                            png_sample_every,
                            png_samples_saved: 0,
                            png_texture_readback: None,
                            inject_error_at_frame,
                        };

                        if let Err(e) = event_loop.run_app(&mut app) {
                            tracing::error!("Display {}: Event loop error: {}", window_id, e);
                        }

                        // Present target + kernel drop in reverse construction
                        // order via App field destruction; both dispatch their
                        // host-side GPU cleanup through the plugin ABI.
                        drop(app);
                    }
                }

                // The render thread exited on its own (frame_limit, window
                // close, error, or a headless drain reaching its frame limit):
                // request a runtime shutdown so the whole graph stops. The
                // engine-free `request_runtime_shutdown` publishes the reason
                // string on the host's reserved control topic and the host maps
                // it to the shutdown — no engine `Event` type crosses the plugin
                // ABI. Skipped when `stop()` triggered the exit (the runtime is
                // already tearing down); a headless drain with no frame limit
                // only reaches here via `stop()`, so it won't spuriously shut the
                // runtime down. Best-effort: a failed request only warns.
                if !stop_called.load(Ordering::Acquire) {
                    tracing::info!(
                        "Display {}: render thread exited, requesting runtime shutdown",
                        window_id
                    );
                    if let Err(e) =
                        streamlib_plugin_sdk::sdk::runtime_control::request_runtime_shutdown(
                            "display render thread exited (window close / frame limit / error)",
                        )
                    {
                        tracing::warn!(
                            "Display {}: request_runtime_shutdown failed: {}",
                            window_id, e
                        );
                    }
                }

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
    gpu_context: GpuContextLimitedAccess,
    inputs: streamlib_plugin_sdk::sdk::iceoryx2::InputMailboxes,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    window_id: u64,
    width: u32,
    height: u32,
    window_title: String,
    vsync: bool,
    scaling_mode: ScalingMode,
    present_target: Option<PresentTarget>,
    graphics_kernel: Option<VulkanGraphicsKernel>,
    /// Last-applied frame `color_info` (package-local serialized form).
    /// When a new frame arrives with a different value the swapchain is
    /// recreated against the new `VkColorSpaceKHR` priority pick. `None`
    /// covers both "no frame seen yet" and "every frame so far has had
    /// `color_info: None`" — both stay on the legacy SDR pick.
    current_frame_color_info: Option<crate::_generated_::ColorInfo>,
    /// Set when no display surface could be created (window or present
    /// target creation failed) even though the event loop built. The
    /// handler keeps running purely to drain its input — every tick reads
    /// and discards queued frames, presents nothing.
    inactive: bool,
    frame_limit: Option<u64>,
    png_sample_dir: Option<std::path::PathBuf>,
    png_sample_every: u64,
    png_samples_saved: u64,
    /// Single-in-flight GPU→CPU readback for the PNG-sample fallback path,
    /// minted host-side (`escalate` + `create_texture_readback`) once and
    /// cached. `!Clone` — the primitive owns its staging resources.
    png_texture_readback: Option<streamlib_plugin_sdk::sdk::rhi::TextureReadback>,
    /// Test-only: when set, the render body returns `Err` once the
    /// displayed-frame counter hits this value. Exercises the present
    /// target's error-path semaphore drain (host `end_frame`).
    inject_error_at_frame: Option<u64>,
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
                // No display surface — degrade to a drain rather than tear
                // the runtime down. `about_to_wait` keeps reading and
                // discarding frames; the event loop stays alive without a
                // window.
                tracing::warn!(
                    "Display {}: no display surface (window creation failed: {}) — degrading to headless drain mode",
                    self.window_id,
                    e
                );
                self.inactive = true;
                return;
            }
        };

        // Project the winit window into the plugin ABI's flattened window
        // handle. The host reconstructs the native handles and owns the
        // `VkSurfaceKHR` from creation; the window must outlive the present
        // target (it lives in `self.window` for the handler's lifetime).
        let window_repr = match raw_window_handle_repr_from_window(&window) {
            Ok(repr) => repr,
            Err(e) => {
                tracing::warn!(
                    "Display {}: no display surface (window-handle projection failed: {}) — degrading to headless drain mode",
                    self.window_id,
                    e
                );
                self.inactive = true;
                return;
            }
        };

        // Escalate ONCE to mint the swapchain present target + blit kernel on
        // the host device; both are cached and driven per frame through their
        // own scope-free plugin-ABI methods (no per-frame escalate). Initial
        // colorspace pick is `None` (legacy SDR); the first frame's
        // `color_info`, if non-`None`, drives a recreate in `render_frame`.
        let built = self
            .gpu_context
            .escalate(|full| -> Result<(PresentTarget, VulkanGraphicsKernel)> {
                let present_target =
                    full.create_present_target(&window_repr, self.width, self.height, self.vsync, None)?;
                let color_format_raw = present_target.color_format_raw();
                let color_format = texture_format_from_raw(color_format_raw).ok_or_else(|| {
                    Error::GpuError(format!(
                        "present target reported unknown swapchain color-format discriminant {color_format_raw}"
                    ))
                })?;
                let kernel = build_display_kernel(full, color_format)?;
                Ok((present_target, kernel))
            });

        let (present_target, kernel) = match built {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                // Surface creation or the blit pipeline failed. Either a
                // benign headless box (`DisplaySurfaceUnavailable`) or an
                // unexpected GPU error — degrade to a drain rather than
                // crashing the runtime; the window local drops here.
                tracing::warn!(
                    "Display {}: display GPU setup failed ({}) — degrading to headless drain mode",
                    self.window_id,
                    e
                );
                self.inactive = true;
                return;
            }
            Err(e) => {
                tracing::warn!(
                    "Display {}: escalate for display GPU setup failed ({}) — degrading to headless drain mode",
                    self.window_id,
                    e
                );
                self.inactive = true;
                return;
            }
        };

        tracing::info!(
            "Display {}: Vulkan present target ready ({}x{}, color-format discriminant {})",
            self.window_id,
            self.width,
            self.height,
            present_target.color_format_raw()
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

                let color_traits =
                    package_color_info_to_traits_repr(self.current_frame_color_info.as_ref());
                if let Some(pt) = self.present_target.as_mut() {
                    // Resize-driven recreate keeps the current colorspace pick.
                    if let Err(e) = pt.recreate(new_size.width, new_size.height, color_traits) {
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

        // Degraded path: a surface couldn't be created, so there's no
        // window to present into. Keep draining the input every tick and
        // discard the frames — the display behaves as a sink so upstream
        // sees a live consumer. `frame_counter` advances per drained frame
        // so a configured frame limit still self-terminates the run.
        if self.inactive {
            let drained = drain_and_discard_video(&self.inputs);
            if drained > 0 {
                self.frame_counter.fetch_add(drained, Ordering::Relaxed);
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(2),
            ));
            return;
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
        if self.present_target.is_none() || self.graphics_kernel.is_none() {
            return;
        }

        let window_id = self.window_id;
        let current_width = self.width;
        let current_height = self.height;
        let scaling_mode = self.scaling_mode.clone();
        let inject_error_at_frame = self.inject_error_at_frame;

        let ipc_frame: crate::_generated_::VideoFrame = match self.inputs.read("video") {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!("Display {}: Failed to read frame: {}", window_id, e);
                return;
            }
        };

        // Colorspace negotiation — recreate the swapchain if this frame's
        // `color_info` differs from the last-applied value. First-frame
        // inspection: when the very first frame arrives with non-`None`
        // color_info, the present target was constructed in `resumed()`
        // with `None` (legacy SDR pick) and is now upgraded to whatever
        // the priority walk picks. On NVIDIA + X11 (today's dev box) the
        // surface only exposes SRGB_NONLINEAR so the pick stays SDR — the
        // recreate is a no-op-cost path that only fires when the WSI
        // exposes wider colorspaces.
        if ipc_frame.color_info != self.current_frame_color_info {
            let new_color_traits = package_color_info_to_traits_repr(ipc_frame.color_info.as_ref());
            // Scope the present-target borrow to the recreate; read the
            // before/after color format so a format flip can rebuild the kernel.
            let recreate_outcome: Result<(u32, u32)> = {
                let present_target = match self.present_target.as_mut() {
                    Some(pt) => pt,
                    None => return,
                };
                let prior_color_format_raw = present_target.color_format_raw();
                present_target
                    .recreate(current_width, current_height, new_color_traits)
                    .map(|()| (prior_color_format_raw, present_target.color_format_raw()))
            };
            match recreate_outcome {
                Err(e) => {
                    tracing::warn!(
                        "Display {}: ColorInfo recreate failed (keeping previous swapchain): {}",
                        window_id, e
                    );
                }
                Ok((prior_color_format_raw, new_color_format_raw)) => {
                    self.current_frame_color_info = ipc_frame.color_info.clone();
                    // A recreate can flip SDR BGRA8 → HDR10 A2B10G10R10; the
                    // cached blit kernel was built against the prior attachment
                    // format and must be rebuilt before the next draw.
                    if new_color_format_raw != prior_color_format_raw {
                        let new_color_format = match texture_format_from_raw(new_color_format_raw) {
                            Some(f) => f,
                            None => {
                                tracing::error!(
                                    "Display {}: recreate reported unknown color-format discriminant {}",
                                    window_id, new_color_format_raw
                                );
                                self.frame_counter.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                        };
                        match self
                            .gpu_context
                            .escalate(|full| build_display_kernel(full, new_color_format))
                        {
                            Ok(Ok(new_kernel)) => {
                                tracing::info!(
                                    "Display {}: rebuilt display blit kernel for new color format \
                                     {:?} (discriminant {}, was {})",
                                    window_id, new_color_format, new_color_format_raw,
                                    prior_color_format_raw
                                );
                                self.graphics_kernel = Some(new_kernel);
                                // Skip this frame's draw — the next frame uses
                                // the new kernel.
                                self.frame_counter.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                            Ok(Err(e)) => {
                                tracing::error!(
                                    "Display {}: failed to rebuild display blit kernel: {}",
                                    window_id, e
                                );
                                self.frame_counter.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Display {}: escalate to rebuild display blit kernel failed: {}",
                                    window_id, e
                                );
                                self.frame_counter.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                        }
                    }
                }
            }
        }

        // HDR static metadata push — fires only when the picked colorspace
        // is one of the PQ/HLG variants (gated by `set_hdr_metadata` itself
        // host-side) and the frame carries the sidecar metadata. Subsequent
        // frames with byte-identical payload short-circuit inside the host.
        if let Some(hdr_metadata) = package_hdr_metadata_to_repr(
            ipc_frame.mastering_display.as_ref(),
            ipc_frame.content_light.as_ref(),
        ) && let Some(present_target) = self.present_target.as_mut()
            && let Err(e) = present_target.set_hdr_metadata(&hdr_metadata)
        {
            tracing::warn!("Display {}: set_hdr_metadata failed: {}", window_id, e);
        }

        // Resolve the texture + registration via the engine's blessed API.
        let registration = match self.gpu_context.resolve_texture_registration_by_surface_id(
            &ipc_frame.surface_id,
            ipc_frame.texture_layout,
            ipc_frame.width,
            ipc_frame.height,
        ) {
            Ok(reg) => reg,
            Err(e) => {
                tracing::warn!(
                    "Display {}: Failed to resolve texture for '{}': {}",
                    window_id,
                    ipc_frame.surface_id,
                    e
                );
                return;
            }
        };
        let camera_texture: Texture = registration.texture().clone();
        let src_width = camera_texture.width();
        let src_height = camera_texture.height();

        // Snapshot the current blit kernel (a host Arc bump). A color-format
        // change above rebuilds `self.graphics_kernel` and returns early, so
        // this snapshot is only ever used on a non-format-change frame.
        let blit_kernel = match self.graphics_kernel.as_ref() {
            Some(k) => k.clone(),
            None => return,
        };

        let inject_error_this_frame = inject_error_at_frame
            .map(|n| self.frame_counter.load(Ordering::Relaxed) == n)
            .unwrap_or(false);

        // Acquire the swapchain frame and draw. `begin_frame` borrows the
        // present target mutably for the RAII `PresentTargetFrame`; the borrow
        // (and the whole block) is scoped so `self` is free for the PNG-sample
        // path below.
        let present_completed = {
            let present_target = match self.present_target.as_mut() {
                Some(pt) => pt,
                None => return,
            };
            match present_target.begin_frame() {
                Err(e) => {
                    tracing::warn!("Display {}: begin_frame failed: {}", window_id, e);
                    false
                }
                Ok(None) => {
                    // OUT_OF_DATE_KHR: no frame stashed in flight; the caller
                    // recreates on the next resize. Falls through to the
                    // frame-counter + PNG-sample path (parity with the prior
                    // "swapchain out of date" branch).
                    tracing::debug!(
                        "Display {}: swapchain out of date — will recreate on next resize",
                        window_id
                    );
                    true
                }
                Ok(Some(mut frame)) => {
                    let frame_index = frame.frame_index;
                    let extent = frame.extent;
                    let image_view_raw = frame.image_view_raw;

                    let draw_result: Result<()> = (|| {
                        // Stage the camera texture as the kernel's binding-0
                        // sampled texture for this descriptor-ring slot.
                        blit_kernel.set_sampled_texture(frame_index, 0, &camera_texture)?;

                        // Transition the camera texture into
                        // SHADER_READ_ONLY_OPTIMAL if it isn't already there.
                        // Producers may publish in different layouts; the
                        // registration declares what's current.
                        let camera_current_layout = registration.current_layout();
                        if camera_current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
                            frame.recorder().record_image_barrier(
                                &camera_texture,
                                camera_current_layout,
                                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                                VulkanStage::ALL_COMMANDS,
                                VulkanStage::FRAGMENT_SHADER,
                                VulkanAccess::MEMORY_WRITE,
                                VulkanAccess::SHADER_SAMPLED_READ,
                            )?;
                            registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
                        }

                        // No GPU-wait on a producer timeline here. Every
                        // in-tree producer drains its GPU work synchronously
                        // before sending the iceoryx2 VideoFrame, so the
                        // iceoryx2 receipt itself is the "GPU writes are
                        // visible" signal — no separate timeline wait is
                        // required. A future async producer that genuinely
                        // needs Display-side timeline sync carries an explicit
                        // (timeline, wait_value) pair on the VideoFrame
                        // protocol, folded into `frame.end`'s extra-waits.

                        // Compute aspect-ratio-aware scale per configured mode.
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
                        blit_kernel.set_push_constants_value(frame_index, &push_constants)?;

                        // Begin dynamic rendering on the acquired swapchain
                        // image view with CLEAR.
                        frame.recorder().cmd_begin_dynamic_rendering(
                            image_view_raw,
                            extent,
                            Some([0.0, 0.0, 0.0, 1.0]),
                        )?;

                        // Test-only error injection AFTER begin (deliberately
                        // leaves a dangling render pass): the host `end_frame`
                        // closes it and drains the acquire semaphore, so
                        // validation layers catch any drain bug at the next
                        // acquire as VUID-vkQueueSubmit2-semaphore-03868.
                        if inject_error_this_frame {
                            return Err(Error::GpuError(
                                "STREAMLIB_DISPLAY_INJECT_ERROR_AT_FRAME: forced closure error after cmd_begin_dynamic_rendering".into(),
                            ));
                        }

                        let draw = DrawCall {
                            vertex_count: 3,
                            instance_count: 1,
                            first_vertex: 0,
                            first_instance: 0,
                            viewport: Some(Viewport::full(extent.0, extent.1)),
                            scissor: Some(ScissorRect::full(extent.0, extent.1)),
                        };
                        frame
                            .recorder()
                            .record_draw(&blit_kernel, frame_index, &draw)?;
                        frame.recorder().cmd_end_dynamic_rendering()?;
                        Ok(())
                    })();

                    // Always complete the frame: the host barriers + submits +
                    // presents on `end` (even after a draw error, closing any
                    // dangling render pass and draining the acquire semaphore)
                    // so the swapchain keeps making forward progress. No
                    // producer-finished timeline waits are folded in (empty
                    // slice) — see the timeline note above.
                    let end_result = frame.end(&[]);

                    match (draw_result, end_result) {
                        (Ok(()), Ok(())) => true,
                        (Err(e), _) => {
                            tracing::warn!("Display {}: render frame draw failed: {}", window_id, e);
                            false
                        }
                        (Ok(()), Err(e)) => {
                            tracing::warn!("Display {}: present-frame end failed: {}", window_id, e);
                            false
                        }
                    }
                }
            }
        };

        if !present_completed {
            // Advance the counter so frame-counter-keyed logic (frame limit,
            // fault-injection trigger) makes progress on every render attempt,
            // not only on success. The host already completed the swapchain
            // submit + present on the error path, so the frame is "done" from
            // the swapchain's perspective regardless.
            self.frame_counter.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let frame_idx = self.frame_counter.fetch_add(1, Ordering::Relaxed);

        if let Some(ref dir) = self.png_sample_dir {
            if frame_idx % self.png_sample_every == 0 {
                let path = dir.join(format!(
                    "display_{:03}_frame_{:06}.png",
                    self.window_id, frame_idx
                ));
                if let Ok(buf) = self
                    .gpu_context
                    .resolve_pixel_buffer_by_surface_id(&ipc_frame.surface_id)
                {
                    let mapped_ptr = buf.plane_base_address(0);
                    if !mapped_ptr.is_null() {
                        let len = (src_width as usize) * (src_height as usize) * 4;
                        // SAFETY: the host maps the pixel buffer's plane 0 for
                        // the surface; `len` matches the tightly-packed RGBA
                        // extent the resolver validated.
                        let rgba = unsafe { std::slice::from_raw_parts(mapped_ptr, len) };
                        if let Err(e) = write_png_rgba(&path, src_width, src_height, rgba) {
                            tracing::warn!(
                                "Display {}: PNG sample save failed at frame {}: {}",
                                self.window_id, frame_idx, e
                            );
                        } else {
                            self.png_samples_saved += 1;
                            tracing::info!(
                                "Display {}: saved PNG sample {:?} (displayed {}, total saved {})",
                                self.window_id,
                                path,
                                frame_idx,
                                self.png_samples_saved
                            );
                        }
                    }
                } else {
                    self.sample_texture_to_png(&camera_texture, &path, frame_idx);
                }
            }
        }
    }

    fn sample_texture_to_png(
        &mut self,
        texture: &Texture,
        path: &std::path::Path,
        frame_idx: u64,
    ) {
        let format = texture.format();
        let width = texture.width();
        let height = texture.height();

        if format.bytes_per_pixel() != 4 {
            tracing::warn!(
                "Display {}: skip PNG sample at frame {} — texture format {:?} \
                 (bpp={}) not supported by PNG writer",
                self.window_id,
                frame_idx,
                format,
                format.bytes_per_pixel()
            );
            return;
        }

        if self.png_texture_readback.is_none() {
            // Privileged host-side creation: `escalate` opens a FullAccess
            // window just long enough to build the readback on the host device,
            // then drains + releases it. The handle is cached; per-sample
            // `submit` / `wait_and_read` run scope-free (no second escalate).
            let readback = match self.gpu_context.escalate(|full| {
                full.create_texture_readback("display-png-sample", width, height, format)
            }) {
                Ok(Ok(rb)) => rb,
                Ok(Err(e)) => {
                    tracing::warn!(
                        "Display {}: PNG texture-readback handle creation failed: {}",
                        self.window_id, e
                    );
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        "Display {}: escalate for PNG texture-readback creation failed: {}",
                        self.window_id, e
                    );
                    return;
                }
            };
            tracing::info!(
                "Display {}: created texture-readback handle for PNG sampling \
                 ({:?}, {}x{}, {} bytes)",
                self.window_id,
                format,
                width,
                height,
                readback.staging_size()
            );
            self.png_texture_readback = Some(readback);
        }

        // Scope the immutable borrow of the cached readback (`!Clone`, stored
        // inline) so the byte slice it lends is dropped before the mutable
        // `self.png_samples_saved` bookkeeping below.
        let write_outcome: std::result::Result<(), String> = {
            let readback = match self.png_texture_readback.as_ref() {
                Some(rb) => rb,
                None => return,
            };
            let ticket = match readback.submit(texture, TextureSourceLayout::General) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(
                        "Display {}: PNG texture-readback submit failed at frame {}: {}",
                        self.window_id, frame_idx, e
                    );
                    return;
                }
            };
            match readback.wait_and_read(ticket, u64::MAX) {
                Ok(bytes) => {
                    let rgba = maybe_swizzle_bgra_to_rgba(format, bytes);
                    write_png_rgba(path, width, height, &rgba).map_err(|e| e.to_string())
                }
                Err(e) => {
                    tracing::warn!(
                        "Display {}: PNG texture-readback wait failed at frame {}: {}",
                        self.window_id, frame_idx, e
                    );
                    return;
                }
            }
        };

        match write_outcome {
            Ok(()) => {
                self.png_samples_saved += 1;
                tracing::info!(
                    "Display {}: saved PNG sample (texture path) {:?} \
                     (displayed {}, total saved {})",
                    self.window_id,
                    path,
                    frame_idx,
                    self.png_samples_saved
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Display {}: PNG sample (texture path) save failed at frame {}: {}",
                    self.window_id,
                    frame_idx,
                    e
                );
            }
        }
    }
}

/// Project a winit [`Window`]'s raw window + display handles into the plugin
/// ABI's flattened [`RawWindowHandleRepr`] (`0 = Xlib`, `1 = Xcb`,
/// `2 = Wayland`). The host reconstructs the native handles and owns the
/// `VkSurfaceKHR` from creation; the window must outlive the minted present
/// target. Widths widen to `u64` on the wire; the host narrows them back.
fn raw_window_handle_repr_from_window(window: &Window) -> Result<RawWindowHandleRepr> {
    let window_handle = window
        .window_handle()
        .map_err(|e| Error::GpuError(format!("window handle unavailable: {e}")))?
        .as_raw();
    let display_handle = window
        .display_handle()
        .map_err(|e| Error::GpuError(format!("display handle unavailable: {e}")))?
        .as_raw();
    match (window_handle, display_handle) {
        (RawWindowHandle::Xlib(w), RawDisplayHandle::Xlib(d)) => Ok(RawWindowHandleRepr {
            kind: 0,
            _reserved_padding: 0,
            // Xlib `window` is `c_ulong`; on Linux (the only target this
            // module compiles for) that is `u64`, so no cast is needed.
            window_or_surface: w.window,
            display_or_connection: d.display.map(|p| p.as_ptr() as u64).unwrap_or(0),
            screen: d.screen as u32,
            _reserved_tail: 0,
        }),
        (RawWindowHandle::Xcb(w), RawDisplayHandle::Xcb(d)) => Ok(RawWindowHandleRepr {
            kind: 1,
            _reserved_padding: 0,
            window_or_surface: w.window.get() as u64,
            display_or_connection: d.connection.map(|p| p.as_ptr() as u64).unwrap_or(0),
            screen: d.screen as u32,
            _reserved_tail: 0,
        }),
        (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => Ok(RawWindowHandleRepr {
            kind: 2,
            _reserved_padding: 0,
            window_or_surface: w.surface.as_ptr() as u64,
            display_or_connection: d.display.as_ptr() as u64,
            screen: 0,
            _reserved_tail: 0,
        }),
        (window_handle, _) => Err(Error::GpuError(format!(
            "unsupported window-handle backend for present target: {window_handle:?}"
        ))),
    }
}

/// Decode a plugin-ABI `TextureFormat` `#[repr(u32)]` discriminant (as
/// carried by the present target's cached swapchain color format) back to a
/// [`TextureFormat`]. `None` for a discriminant this build doesn't know —
/// the caller degrades rather than silently building a kernel against the
/// wrong attachment format. The discriminant values are the wire contract
/// pinned by `streamlib-consumer-rhi`'s `texture_format_discriminants_are_pinned`.
fn texture_format_from_raw(raw: u32) -> Option<TextureFormat> {
    Some(match raw {
        0 => TextureFormat::Rgba8Unorm,
        1 => TextureFormat::Rgba8UnormSrgb,
        2 => TextureFormat::Bgra8Unorm,
        3 => TextureFormat::Bgra8UnormSrgb,
        4 => TextureFormat::Rgba16Float,
        5 => TextureFormat::Rgba32Float,
        6 => TextureFormat::Nv12,
        _ => return None,
    })
}

/// Drain and discard every queued frame on the display's `"video"`
/// input, returning the number drained. Used by the headless / inactive
/// degradation path: the display still owns a wired input it must
/// consume, but produces no presentation. `read_raw` pulls pending
/// iceoryx2 samples into the mailbox and pops it (no msgpack decode);
/// for the `video` port's `SkipToLatest` read mode one call empties the
/// buffer, but the loop keeps the helper correct for any read mode.
fn drain_and_discard_video(inputs: &streamlib_plugin_sdk::sdk::iceoryx2::InputMailboxes) -> u64 {
    let mut drained = 0u64;
    while let Ok(Some(_)) = inputs.read_raw("video") {
        drained += 1;
    }
    drained
}

/// Headless drain loop — runs in place of the winit event loop when no
/// display server is available (or `STREAMLIB_DISPLAY_FORCE_HEADLESS` is
/// set). Reads and discards frames every tick so the display behaves as
/// a live sink, and honors `frame_limit` so automated runs still
/// self-terminate. With no frame limit it runs until `stop()` clears
/// `running`. The fixed ~2 ms tick bounds the wasted work while staying
/// responsive to `stop()` (which waits up to 2 s for this thread).
fn run_headless_drain_loop(
    inputs: &streamlib_plugin_sdk::sdk::iceoryx2::InputMailboxes,
    running: &Arc<AtomicBool>,
    frame_counter: &Arc<AtomicU64>,
    frame_limit: Option<u64>,
    window_id: u64,
) {
    tracing::info!(
        "Display {}: headless drain mode active — reading and discarding frames, presenting nothing",
        window_id
    );
    while running.load(Ordering::Acquire) {
        if let Some(limit) = frame_limit {
            if frame_counter.load(Ordering::Relaxed) >= limit {
                tracing::info!(
                    "Display {}: frame limit ({}) reached in headless mode — exiting",
                    window_id, limit
                );
                running.store(false, Ordering::Release);
                break;
            }
        }
        let drained = drain_and_discard_video(inputs);
        if drained > 0 {
            frame_counter.fetch_add(drained, Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    tracing::debug!("Display {}: headless drain loop exiting", window_id);
}

/// Project this package's `_generated_::ColorInfo` into the plugin ABI's
/// [`ColorTraitsRepr`] (`primaries_raw` / `transfer_raw`, `u32::MAX` = the
/// axis is unset). Each consumer translates its own generated schema flavor
/// at the boundary; the wire format is the contract across packages, not
/// Rust type equality. Returns `None` when the frame has no color metadata
/// — the host stays on the SDR fallback pick.
fn package_color_info_to_traits_repr(
    pkg: Option<&crate::_generated_::ColorInfo>,
) -> Option<ColorTraitsRepr> {
    use crate::_generated_::tatolab__core::color_info::{Primaries, Transfer};
    use streamlib_plugin_sdk::sdk::color::{PrimariesId, TransferId};
    let pkg = pkg?;
    let primaries_raw = pkg
        .primaries
        .as_ref()
        .map(|p| {
            let id = match p {
                Primaries::Bt709 => PrimariesId::Bt709,
                Primaries::Bt470M => PrimariesId::Bt470M,
                Primaries::Bt470Bg => PrimariesId::Bt470Bg,
                Primaries::Smpte170m => PrimariesId::Smpte170m,
                Primaries::Smpte240m => PrimariesId::Smpte240m,
                Primaries::Film => PrimariesId::Film,
                Primaries::Bt2020 => PrimariesId::Bt2020,
                Primaries::Smpte428 => PrimariesId::Smpte428,
                Primaries::Smpte431 => PrimariesId::Smpte431,
                Primaries::Smpte432 => PrimariesId::Smpte432,
                Primaries::Ebu3213 => PrimariesId::Ebu3213,
            };
            id as u32
        })
        .unwrap_or(u32::MAX);
    let transfer_raw = pkg
        .transfer
        .as_ref()
        .map(|t| {
            let id = match t {
                Transfer::Srgb => TransferId::Srgb,
                Transfer::Bt709
                | Transfer::Smpte170m
                | Transfer::Bt2020TenBit
                | Transfer::Bt2020TwelveBit => TransferId::Bt709,
                Transfer::Smpte2084 => TransferId::Pq,
                Transfer::AribStdB67 => TransferId::Hlg,
                Transfer::Linear => TransferId::Linear,
                _ => TransferId::Linear,
            };
            id as u32
        })
        .unwrap_or(u32::MAX);
    Some(ColorTraitsRepr {
        primaries_raw,
        transfer_raw,
    })
}

/// Project the per-frame mastering-display + content-light schema pair
/// into the plugin ABI's [`HdrStaticMetadataRepr`]. Returns `None` when
/// either sidecar is absent — the present target's `set_hdr_metadata` is
/// gated on `Some`, keeping HDR signaling off for SDR frames.
///
/// Schema unit scaling: chromaticity at 1/50000 increments (CIE 1931)
/// → `[0, 1]`; mastering luminance at 0.0001 cd/m² increments → cd/m²;
/// content-light fields are integer cd/m² cast to f32 directly.
fn package_hdr_metadata_to_repr(
    mastering_pkg: Option<&crate::_generated_::MasteringDisplay>,
    content_light_pkg: Option<&crate::_generated_::ContentLight>,
) -> Option<HdrStaticMetadataRepr> {
    let m = mastering_pkg?;
    let cl = content_light_pkg?;
    const CHROMA_SCALE: f32 = 1.0 / 50_000.0;
    const LUM_SCALE: f32 = 1.0 / 10_000.0;
    Some(HdrStaticMetadataRepr {
        display_primary_red: [
            m.display_primaries_r_x as f32 * CHROMA_SCALE,
            m.display_primaries_r_y as f32 * CHROMA_SCALE,
        ],
        display_primary_green: [
            m.display_primaries_g_x as f32 * CHROMA_SCALE,
            m.display_primaries_g_y as f32 * CHROMA_SCALE,
        ],
        display_primary_blue: [
            m.display_primaries_b_x as f32 * CHROMA_SCALE,
            m.display_primaries_b_y as f32 * CHROMA_SCALE,
        ],
        white_point: [
            m.white_point_x as f32 * CHROMA_SCALE,
            m.white_point_y as f32 * CHROMA_SCALE,
        ],
        min_luminance_cd_m2: m.min_luminance as f32 * LUM_SCALE,
        max_luminance_cd_m2: m.max_luminance as f32 * LUM_SCALE,
        max_content_light_level: cl.max_cll as f32,
        max_frame_average_light_level: cl.max_fall as f32,
    })
}

fn build_display_kernel(
    full: &GpuContextFullAccess,
    attachment_format: TextureFormat,
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
        descriptor_sets_in_flight: DISPLAY_BLIT_FRAMES_IN_FLIGHT,
    };
    full.create_graphics_kernel(&descriptor)
}

// ---------------------------------------------------------------------------
// Minimal PNG writer (no external deps)
// ---------------------------------------------------------------------------

fn maybe_swizzle_bgra_to_rgba(
    format: TextureFormat,
    bytes: &[u8],
) -> std::borrow::Cow<'_, [u8]> {
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
    use super::{maybe_swizzle_bgra_to_rgba, texture_format_from_raw};
    use streamlib_plugin_sdk::sdk::rhi::TextureFormat;

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

    /// Every known plugin-ABI `TextureFormat` discriminant round-trips
    /// through the present target's raw color-format decode, and an
    /// out-of-range discriminant is rejected (rather than silently
    /// aliasing onto `Rgba8Unorm`). Mentally renumbering a variant, or
    /// swapping the `None` arm for a fallback, fails this — the decode
    /// must stay in lock step with `TextureFormat`'s pinned discriminants.
    #[test]
    fn texture_format_from_raw_round_trips_known_and_rejects_unknown() {
        for format in [
            TextureFormat::Rgba8Unorm,
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Rgba16Float,
            TextureFormat::Rgba32Float,
            TextureFormat::Nv12,
        ] {
            assert_eq!(texture_format_from_raw(format as u32), Some(format));
        }
        assert_eq!(texture_format_from_raw(7), None);
        assert_eq!(texture_format_from_raw(u32::MAX), None);
    }

    // --- headless drain-and-drop (#1104) ---

    use super::drain_and_discard_video;
    use streamlib_plugin_sdk::sdk::iceoryx2::InputMailboxes;

    /// Draining an unwired (empty) input is a clean no-op returning zero
    /// — locks that the `while let Ok(Some(_))` loop terminates on
    /// `Ok(None)` rather than spinning. An engine-free package cannot
    /// construct a host-backed mailbox with buffered frames (the real
    /// `InputMailboxesInner` lives in the engine, not the SDK), so the
    /// multi-frame FIFO / skip-to-latest drain semantics are covered by
    /// the engine's own iceoryx2 tests and the host `read_raw` contract,
    /// not re-testable here. `InputMailboxes::empty()` reports no data.
    #[test]
    fn drain_on_unwired_input_returns_zero() {
        let inputs = InputMailboxes::empty();
        assert!(!inputs.has_data("video"));
        assert_eq!(drain_and_discard_video(&inputs), 0);
    }
}
