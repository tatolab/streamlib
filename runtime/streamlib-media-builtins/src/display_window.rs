// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in display: window lifecycle + event pump, one present-composition
//! call per frame.
//!
//! The window and event pump belong to this processor; the engine mints the
//! present target from the raw window handle and owns every swapchain and
//! acquire detail. The draw step is
//! [`VulkanPresentCompositor::compose_to_present_frame`] — the display never
//! records Vulkan work of its own.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use streamlib::sdk::color::ColorTraits;
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::engine::host_rhi::{
    PresentScalingMode, VulkanPresentCompositor, VulkanPresentTarget,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::InputMailboxes;
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::VulkanLayout;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes};

use crate::video_frame::{ColorInfo, VideoFrame};

/// How the frame maps onto the window, as configuration vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DisplayScaling {
    /// Whole frame visible, black bars fill the rest.
    #[default]
    Fit,
    /// Window covered, frame overflow cropped.
    Fill,
    /// Frame stretched to the window exactly.
    Stretch,
}

impl DisplayScaling {
    fn present_scaling_mode(self) -> PresentScalingMode {
        match self {
            DisplayScaling::Fit => PresentScalingMode::Fit,
            DisplayScaling::Fill => PresentScalingMode::Fill,
            DisplayScaling::Stretch => PresentScalingMode::Stretch,
        }
    }
}

/// Configuration for [`DisplayWindow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayWindowConfig {
    /// Window title.
    #[serde(default = "default_title")]
    pub title: String,
    /// Initial window width in pixels.
    #[serde(default = "default_window_width")]
    pub width: u32,
    /// Initial window height in pixels.
    #[serde(default = "default_window_height")]
    pub height: u32,
    /// How the frame maps onto the window.
    #[serde(default)]
    pub scaling: DisplayScaling,
}

fn default_title() -> String {
    "StreamLib".to_string()
}

fn default_window_width() -> u32 {
    1280
}

fn default_window_height() -> u32 {
    720
}

impl Default for DisplayWindowConfig {
    fn default() -> Self {
        Self {
            title: default_title(),
            width: default_window_width(),
            height: default_window_height(),
            scaling: DisplayScaling::default(),
        }
    }
}

#[streamlib::sdk::processor(
    "@tatolab/media-builtins/DisplayWindow",
    description = "Shows video frames in a window with vsync",
    execution = manual,
    scheduling = high,
    config = crate::display_window::DisplayWindowConfig,
    input("video", any, delivery_profile = "latest", description = "Video frames to show in the window"),
)]
pub struct DisplayWindow {
    gpu_context: Option<GpuContextLimitedAccess>,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    render_thread: Option<JoinHandle<()>>,
    event_loop_proxy: Arc<OnceLock<EventLoopProxy<()>>>,
}

impl ManualProcessor for DisplayWindow::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // The render thread needs an owned handle that escapes into the
        // thread closure, so the clone at setup is load-bearing.
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_render_thread();
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            Error::Configuration("GPU context not initialized. Call setup() first.".into())
        })?;

        self.running.store(true, Ordering::Release);
        let running = Arc::clone(&self.running);
        let frame_counter = Arc::clone(&self.frame_counter);
        let event_loop_proxy = Arc::clone(&self.event_loop_proxy);
        let inputs: InputMailboxes = self.inputs.clone();
        let window_title = self.config.title.clone();
        let window_width = self.config.width;
        let window_height = self.config.height;
        let scaling = self.config.scaling;

        let handle = std::thread::Builder::new()
            .name("display-window".to_string())
            .spawn(move || {
                let event_loop = {
                    // The event loop runs on this render thread, not the
                    // process main thread; X11 permits that with the
                    // any-thread opt-in. (Wayland winit permits it too via
                    // its own extension; the X11 builder path covers today's
                    // rig.)
                    use winit::platform::x11::EventLoopBuilderExtX11;
                    EventLoop::builder().with_any_thread(true).build()
                };
                let event_loop = match event_loop {
                    Ok(el) => el,
                    Err(e) => {
                        tracing::error!(error = %e, "DisplayWindow: failed to build event loop");
                        running.store(false, Ordering::Release);
                        return;
                    }
                };
                let _ = event_loop_proxy.set(event_loop.create_proxy());

                let mut handler = DisplayWindowEventLoopHandler {
                    gpu_context,
                    inputs,
                    running: Arc::clone(&running),
                    frame_counter,
                    window: None,
                    present_target: None,
                    compositor: None,
                    window_title,
                    width: window_width,
                    height: window_height,
                    scaling: scaling.present_scaling_mode(),
                    current_frame_color_info: None,
                    inactive: false,
                };
                if let Err(e) = event_loop.run_app(&mut handler) {
                    tracing::error!(error = %e, "DisplayWindow: event loop exited with error");
                }
                running.store(false, Ordering::Release);
            })
            .map_err(|e| Error::Configuration(format!("Failed to spawn render thread: {}", e)))?;

        self.render_thread = Some(handle);
        tracing::info!("DisplayWindow: render thread started");
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_render_thread();
        tracing::info!(
            "DisplayWindow: stopped ({} frames)",
            self.frame_counter.load(Ordering::Relaxed)
        );
        Ok(())
    }
}

impl DisplayWindow::Processor {
    fn stop_render_thread(&mut self) {
        self.running.store(false, Ordering::Release);
        // Wake the event pump so `about_to_wait` observes `running == false`
        // without waiting for its next timeout tick.
        if let Some(proxy) = self.event_loop_proxy.get() {
            let _ = proxy.send_event(());
        }
        // Bounded wait: a stalled GPU / driver state can wedge the render
        // thread; detaching after the grace window keeps the runtime's
        // shutdown chain moving. The detached thread is reaped at process
        // exit.
        if let Some(handle) = self.render_thread.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                tracing::warn!("DisplayWindow: render thread did not exit within 2s, detaching");
            }
        }
    }
}

struct DisplayWindowEventLoopHandler {
    gpu_context: GpuContextLimitedAccess,
    inputs: InputMailboxes,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    window: Option<Window>,
    present_target: Option<VulkanPresentTarget>,
    compositor: Option<VulkanPresentCompositor>,
    window_title: String,
    width: u32,
    height: u32,
    scaling: PresentScalingMode,
    /// Last-applied per-frame color description; a change triggers a
    /// swapchain recreate with the new colorspace pick.
    current_frame_color_info: Option<ColorInfo>,
    /// Degraded mode: no surface could be created. The display then behaves
    /// as a sink — drains and discards — so upstream sees a live consumer.
    inactive: bool,
}

impl ApplicationHandler for DisplayWindowEventLoopHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = WindowAttributes::default()
            .with_title(&self.window_title)
            .with_inner_size(PhysicalSize::new(self.width, self.height));
        let window = match event_loop.create_window(attributes) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "DisplayWindow: window creation failed — running degraded (frames drained, nothing shown)");
                self.inactive = true;
                return;
            }
        };

        let inner_size = window.inner_size();
        self.width = inner_size.width.max(1);
        self.height = inner_size.height.max(1);

        let created = self.gpu_context.escalate(|full| {
            let present_target =
                full.create_present_target(&window, self.width, self.height, true, None)?;
            let compositor = full.create_present_compositor(present_target.color_format())?;
            Ok((present_target, compositor))
        });
        match created {
            Ok((present_target, compositor)) => {
                self.present_target = Some(present_target);
                self.compositor = Some(compositor);
                self.window = Some(window);
                tracing::info!(
                    width = self.width,
                    height = self.height,
                    "DisplayWindow: window + present target ready"
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "DisplayWindow: present-target creation failed — running degraded (frames drained, nothing shown)");
                self.inactive = true;
            }
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
                tracing::info!("DisplayWindow: window close requested");
                self.running.store(false, Ordering::Release);
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if new_size.width == 0 || new_size.height == 0 {
                    return;
                }
                self.width = new_size.width;
                self.height = new_size.height;
                let color_traits = self
                    .current_frame_color_info
                    .as_ref()
                    .map(ColorInfo::engine_color_traits);
                if let Some(present_target) = self.present_target.as_mut()
                    && let Err(e) = present_target.recreate(
                        new_size.width,
                        new_size.height,
                        color_traits.as_ref(),
                    )
                {
                    tracing::error!(error = %e, "DisplayWindow: swapchain recreate on resize failed");
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: ()) {
        if !self.running.load(Ordering::Acquire) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.running.load(Ordering::Acquire) {
            event_loop.exit();
            return;
        }

        if self.inactive {
            // Drain and discard so upstream sees a live consumer; the frame
            // counter still advances per drained frame.
            let mut drained = 0u64;
            while let Ok(Some(_)) = self.inputs.read_raw("video") {
                drained += 1;
            }
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

impl DisplayWindowEventLoopHandler {
    fn render_frame(&mut self) {
        if !self.inputs.has_data("video") {
            return;
        }
        if self.present_target.is_none() || self.compositor.is_none() {
            return;
        }

        let frame_bag: VideoFrame = match self.inputs.read("video") {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!(error = %e, "DisplayWindow: failed to read frame");
                return;
            }
        };

        // Colorspace negotiation — recreate the swapchain when this frame's
        // `color_info` differs from the last-applied value. First-frame
        // inspection: the present target was constructed with `None` (legacy
        // SDR pick) and upgrades to whatever the priority walk picks. A
        // recreate can flip the attachment format (SDR BGRA8 → HDR10
        // A2B10G10R10); `ensure_attachment_format` rebuilds the compositor's
        // kernel when it does.
        if frame_bag.color_info != self.current_frame_color_info {
            let color_traits: Option<ColorTraits> = frame_bag
                .color_info
                .as_ref()
                .map(ColorInfo::engine_color_traits);
            let present_target = self.present_target.as_mut().expect("checked above");
            match present_target.recreate(self.width, self.height, color_traits.as_ref()) {
                Ok(()) => {
                    self.current_frame_color_info = frame_bag.color_info.clone();
                    let new_format = present_target.color_format();
                    if let Some(compositor) = self.compositor.as_mut() {
                        match compositor.ensure_attachment_format(new_format) {
                            Ok(true) => {
                                tracing::info!(
                                    ?new_format,
                                    "DisplayWindow: rebuilt compositor for new attachment format"
                                );
                                // Skip this frame's draw; the next frame uses
                                // the rebuilt kernel against the new swapchain.
                                self.frame_counter.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                            Ok(false) => {}
                            Err(e) => {
                                tracing::error!(error = %e, "DisplayWindow: compositor rebuild failed");
                                self.frame_counter.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "DisplayWindow: colorspace recreate failed (keeping previous swapchain)"
                    );
                }
            }
        }

        // HDR static metadata — only meaningful when the picked colorspace
        // is PQ/HLG (gated inside `set_hdr_metadata`) and the frame carries
        // the sidecar.
        if let (Some(mastering), Some(present_target)) = (
            frame_bag.mastering_display.as_ref(),
            self.present_target.as_mut(),
        ) {
            let metadata =
                hdr_static_metadata_from_bag(mastering, frame_bag.content_light.as_ref());
            if let Err(e) = present_target.set_hdr_metadata(&metadata) {
                tracing::warn!(error = %e, "DisplayWindow: set_hdr_metadata failed");
            }
        }

        // Resolve the frame's texture: same-process texture cache, then
        // cross-process DMA-BUF import, then pixel-buffer upload — the
        // engine's blessed resolution order.
        let registration = match self.gpu_context.resolve_texture_registration_by_surface_id(
            &frame_bag.surface_id,
            frame_bag.texture_layout,
            frame_bag.width,
            frame_bag.height,
        ) {
            Ok(registration) => registration,
            Err(e) => {
                tracing::warn!(
                    surface_id = %frame_bag.surface_id,
                    error = %e,
                    "DisplayWindow: failed to resolve frame texture"
                );
                return;
            }
        };
        let frame_texture = registration.texture().clone();
        let source_layout = registration.current_layout();

        let compositor = self.compositor.as_ref().expect("checked above");
        let scaling = self.scaling;
        let present_target = self.present_target.as_mut().expect("checked above");
        let present_result = present_target.render_frame(|frame| {
            compositor.compose_to_present_frame(frame, &frame_texture, source_layout, scaling)
        });
        match present_result {
            Ok(_presented) => {
                // The compositor left the source in SHADER_READ_ONLY_OPTIMAL.
                registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
            }
            Err(e) => {
                tracing::warn!(error = %e, "DisplayWindow: present failed");
            }
        }
        self.frame_counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Translate the bag's HDR sidecar integers to the engine's f32 metadata:
/// chromaticities are 1/50000 increments → CIE xy, luminances 0.0001 cd/m²
/// increments → cd/m².
fn hdr_static_metadata_from_bag(
    mastering: &crate::video_frame::MasteringDisplay,
    content_light: Option<&crate::video_frame::ContentLight>,
) -> streamlib::sdk::color::HdrStaticMetadata {
    let chromaticity = |v: u32| v as f32 / 50_000.0;
    streamlib::sdk::color::HdrStaticMetadata {
        display_primary_red: [
            chromaticity(mastering.display_primaries_r_x),
            chromaticity(mastering.display_primaries_r_y),
        ],
        display_primary_green: [
            chromaticity(mastering.display_primaries_g_x),
            chromaticity(mastering.display_primaries_g_y),
        ],
        display_primary_blue: [
            chromaticity(mastering.display_primaries_b_x),
            chromaticity(mastering.display_primaries_b_y),
        ],
        white_point: [
            chromaticity(mastering.white_point_x),
            chromaticity(mastering.white_point_y),
        ],
        min_luminance_cd_m2: mastering.min_luminance as f32 * 0.0001,
        max_luminance_cd_m2: mastering.max_luminance as f32 * 0.0001,
        max_content_light_level: content_light.map(|cl| cl.max_cll as f32).unwrap_or(0.0),
        max_frame_average_light_level: content_light.map(|cl| cl.max_fall as f32).unwrap_or(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_720p_fit() {
        let config: DisplayWindowConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config.title, "StreamLib");
        assert_eq!((config.width, config.height), (1280, 720));
        assert_eq!(config.scaling, DisplayScaling::Fit);
    }

    #[test]
    fn scaling_vocabulary_is_snake_case() {
        let config: DisplayWindowConfig =
            serde_json::from_str(r#"{"scaling": "fill"}"#).expect("fill");
        assert_eq!(config.scaling, DisplayScaling::Fill);
        assert!(
            serde_json::from_str::<DisplayWindowConfig>(r#"{"scaling": "Letterbox"}"#).is_err(),
            "old vocabulary is not silently accepted"
        );
    }

    #[test]
    fn hdr_metadata_translation_scales_the_wire_integers() {
        let mastering = crate::video_frame::MasteringDisplay {
            display_primaries_r_x: 35_400, // 0.708 in 1/50000
            display_primaries_r_y: 14_600,
            display_primaries_g_x: 8_500,
            display_primaries_g_y: 39_850,
            display_primaries_b_x: 6_550,
            display_primaries_b_y: 2_300,
            white_point_x: 15_635,
            white_point_y: 16_450,
            max_luminance: 10_000_000, // 1000 cd/m² in 0.0001 increments
            min_luminance: 50,         // 0.005 cd/m²
        };
        let content_light = crate::video_frame::ContentLight {
            max_cll: 1_000,
            max_fall: 400,
        };
        let metadata = hdr_static_metadata_from_bag(&mastering, Some(&content_light));
        assert!((metadata.display_primary_red[0] - 0.708).abs() < 1e-6);
        assert!((metadata.max_luminance_cd_m2 - 1_000.0).abs() < 1e-3);
        assert!((metadata.min_luminance_cd_m2 - 0.005).abs() < 1e-6);
        assert_eq!(metadata.max_content_light_level, 1_000.0);
        assert_eq!(metadata.max_frame_average_light_level, 400.0);
    }
}
