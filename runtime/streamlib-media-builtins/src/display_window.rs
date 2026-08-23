// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in display: one window registered with the engine's event pump, one
//! present-composition call per frame.
//!
//! The window is minted by [`process_wide_window_event_pump`] — winit permits
//! one event loop per process, so N displays share one — but every policy
//! decision about it is this processor's: title, size, what a resize means,
//! when to redraw, what closing does. The engine mints the present target from
//! the raw window handle and owns every swapchain and acquire detail. The draw
//! step is [`VulkanPresentCompositor::compose_to_present_frame`] — the display
//! never records Vulkan work of its own.
//!
//! Rendering runs on this processor's own thread rather than the pump's, so
//! each window paces on its own vsync and no display can stall another.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use streamlib::sdk::rhi::pool_slot_key_of_surface_id;
use streamlib::sdk::window_event_pump::{
    WindowRegisteredWithEventPump, WindowRegistrationRequestFromOwningProcessor,
    process_wide_window_event_pump,
};

use crate::video_frame::{ColorInfo, VideoFrame};

/// How long the render thread parks when the input has no frame to show. The
/// display is a `latest`-profile sink, so this is the worst-case lateness of a
/// frame that arrives just after a poll, not a frame budget.
const DISPLAY_RENDER_THREAD_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// The same park for a display with no window: it still drains, so upstream
/// sees a live consumer, but nothing is racing a vsync deadline.
const DEGRADED_DISPLAY_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(2);

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
    description = "Shows video frames in a window with vsync",
    execution = manual,
    scheduling = high,
    config = crate::display_window::DisplayWindowConfig,
    input("video", delivery_profile = "latest", description = "Video frames to show in the window"),
)]
pub struct DisplayWindow {
    gpu_context: Option<GpuContextLimitedAccess>,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    render_thread: Option<JoinHandle<()>>,
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
        let inputs: InputMailboxes = self.inputs.clone();
        let config = self.config.clone();

        let handle = std::thread::Builder::new()
            .name("display-window".to_string())
            .spawn(move || {
                DisplayWindowRenderLoop::new(gpu_context, inputs, running, frame_counter, config)
                    .run();
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
        // Bounded wait: a stalled GPU / driver state can wedge the render
        // thread; detaching after the grace window keeps the runtime's
        // shutdown chain moving. The detached thread is reaped at process
        // exit.
        if let Some(handle) = self.render_thread.take() {
            // Cut the idle park short so shutdown does not wait out a poll.
            handle.thread().unpark();
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

/// The display's own render thread: acquire a window, then show frames on it
/// until the graph stops or the user closes it.
struct DisplayWindowRenderLoop {
    gpu_context: GpuContextLimitedAccess,
    inputs: InputMailboxes,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    window_title: String,
    scaling: PresentScalingMode,
    width: u32,
    height: u32,
    // Declared ahead of `registered_window`: the present target's surface was
    // minted from that window's raw handle, and fields drop in declaration
    // order, so the surface must go first.
    present_target: Option<VulkanPresentTarget>,
    compositor: Option<VulkanPresentCompositor>,
    registered_window: Option<WindowRegisteredWithEventPump>,
    /// Last-applied per-frame color description; a change triggers a
    /// swapchain recreate with the new colorspace pick.
    current_frame_color_info: Option<ColorInfo>,
    /// Degraded mode: no window could be had. The display then behaves as a
    /// sink — drains and discards — so upstream sees a live consumer.
    inactive: bool,
    /// The last surface id that failed to resolve, so the failure warns once
    /// per surface instead of once per redraw.
    last_unresolved_surface_id: Option<String>,
    /// The last `color_info` whose swapchain recreate failed, so the failure
    /// warns once per description instead of once per frame (each frame
    /// retries the recreate).
    last_failed_recreate_color_info: Option<Option<ColorInfo>>,
}

impl DisplayWindowRenderLoop {
    fn new(
        gpu_context: GpuContextLimitedAccess,
        inputs: InputMailboxes,
        running: Arc<AtomicBool>,
        frame_counter: Arc<AtomicU64>,
        config: DisplayWindowConfig,
    ) -> Self {
        Self {
            gpu_context,
            inputs,
            running,
            frame_counter,
            scaling: config.scaling.present_scaling_mode(),
            width: config.width,
            height: config.height,
            window_title: config.title,
            present_target: None,
            compositor: None,
            registered_window: None,
            current_frame_color_info: None,
            inactive: false,
            last_unresolved_surface_id: None,
            last_failed_recreate_color_info: None,
        }
    }

    fn run(&mut self) {
        if let Err(reason) = self.acquire_window_and_present_target() {
            self.degrade_to_drain_and_discard(&reason);
        }

        while self.running.load(Ordering::Acquire) {
            self.apply_window_events_from_event_pump();
            if !self.running.load(Ordering::Acquire) {
                break;
            }

            if self.inactive {
                self.drain_and_discard_so_upstream_sees_a_live_consumer();
                std::thread::park_timeout(DEGRADED_DISPLAY_DRAIN_POLL_INTERVAL);
                continue;
            }

            if self.inputs.has_data("video") {
                self.render_frame();
            } else {
                std::thread::park_timeout(DISPLAY_RENDER_THREAD_IDLE_POLL_INTERVAL);
            }
        }

        self.running.store(false, Ordering::Release);
    }

    fn acquire_window_and_present_target(&mut self) -> Result<()> {
        let registered_window = process_wide_window_event_pump()?
            .request_window_for_owning_processor(WindowRegistrationRequestFromOwningProcessor {
                window_title: self.window_title.clone(),
                initial_width_in_physical_pixels: self.width,
                initial_height_in_physical_pixels: self.height,
            })?;

        let (width, height) = registered_window.current_physical_size();
        self.width = width;
        self.height = height;

        let (present_target, compositor) = self.gpu_context.escalate(|full| {
            let present_target = full.create_present_target(
                registered_window.window_shared_with_event_pump().as_ref(),
                width,
                height,
                true,
                None,
            )?;
            let compositor = full.create_present_compositor(present_target.color_format())?;
            Ok((present_target, compositor))
        })?;

        self.present_target = Some(present_target);
        self.compositor = Some(compositor);
        self.registered_window = Some(registered_window);
        tracing::info!(
            width,
            height,
            window_title = %self.window_title,
            "DisplayWindow: window + present target ready"
        );
        Ok(())
    }

    fn degrade_to_drain_and_discard(&mut self, reason: &Error) {
        tracing::error!(
            error = %reason,
            window_title = %self.window_title,
            "DisplayWindow: no window — running degraded (frames drained, nothing shown)"
        );
        self.inactive = true;
    }

    fn apply_window_events_from_event_pump(&mut self) {
        let Some(events) = self
            .registered_window
            .as_ref()
            .map(|registered_window| registered_window.drain_window_events_from_event_pump())
        else {
            return;
        };

        if let Some((width, height)) = events.resized_to_physical_pixels {
            self.recreate_swapchain_for_new_extent(width, height);
        }
        if events.close_requested_by_user {
            tracing::info!(window_title = %self.window_title, "DisplayWindow: window close requested");
            self.running.store(false, Ordering::Release);
        }
    }

    fn recreate_swapchain_for_new_extent(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let color_traits = self
            .current_frame_color_info
            .as_ref()
            .map(ColorInfo::engine_color_traits);
        if let Some(present_target) = self.present_target.as_mut()
            && let Err(e) = present_target.recreate(width, height, color_traits.as_ref())
        {
            tracing::error!(error = %e, "DisplayWindow: swapchain recreate on resize failed");
            self.running.store(false, Ordering::Release);
        }
    }

    fn drain_and_discard_so_upstream_sees_a_live_consumer(&mut self) {
        let mut drained = 0u64;
        while let Ok(Some(_)) = self.inputs.read_raw("video") {
            drained += 1;
        }
        if drained > 0 {
            self.frame_counter.fetch_add(drained, Ordering::Relaxed);
        }
    }

    fn render_frame(&mut self) {
        // Runs before the destructive `latest` read below, so a frame is not
        // consumed when there is nothing to render it into.
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
            let Some(present_target) = self.present_target.as_mut() else {
                return;
            };
            match present_target.recreate(self.width, self.height, color_traits.as_ref()) {
                Ok(()) => {
                    self.current_frame_color_info = frame_bag.color_info.clone();
                    self.last_failed_recreate_color_info = None;
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
                    if self.last_failed_recreate_color_info.as_ref() != Some(&frame_bag.color_info)
                    {
                        tracing::warn!(
                            error = %e,
                            "DisplayWindow: colorspace recreate failed (keeping previous \
                             swapchain; warning once per color description)"
                        );
                        self.last_failed_recreate_color_info = Some(frame_bag.color_info.clone());
                    }
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
            Ok(registration) => {
                self.last_unresolved_surface_id = None;
                registration
            }
            Err(e) => {
                // Redraws retry at frame rate; warn once per underlying
                // surface, not once per attempt — and a pool surface
                // publishes a fresh id per frame, so the dedup keys on the
                // slot or a lagging display would warn at source cadence.
                let unresolved_surface_key =
                    pool_slot_key_of_surface_id(&frame_bag.surface_id).to_string();
                if self.last_unresolved_surface_id.as_deref()
                    != Some(unresolved_surface_key.as_str())
                {
                    tracing::warn!(
                        surface_id = %frame_bag.surface_id,
                        error = %e,
                        "DisplayWindow: failed to resolve frame texture (warning once per surface)"
                    );
                    self.last_unresolved_surface_id = Some(unresolved_surface_key);
                }
                return;
            }
        };
        let scaling = self.scaling;
        let (Some(present_target), Some(compositor)) =
            (self.present_target.as_mut(), self.compositor.as_ref())
        else {
            return;
        };
        // The compositor owns the source's layout bookkeeping via the
        // registration, so a draw error after the barrier cannot leave the
        // registration stale.
        let present_result = present_target.render_frame(|frame| {
            compositor.compose_to_present_frame(frame, &registration, scaling)
        });
        if let Err(e) = present_result {
            tracing::warn!(error = %e, "DisplayWindow: present failed");
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
