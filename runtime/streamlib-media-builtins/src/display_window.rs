// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in display: one window's present loop, fed from an input port.
//!
//! The loop itself is the engine's — [`WindowPresentLoopForOwningProcessor`]
//! mints the window and its present target, resolves each named surface and
//! composes it onto the swapchain. What is this processor's is the policy:
//! title, size, scaling, which frame to name next, what a close means, and
//! what to do when no window can be had.
//!
//! This display is the app-process driver of that one machinery: it names
//! frames from its own thread rather than the pump's, so each window paces on
//! its own vsync and no display can stall another.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use streamlib::sdk::color::HdrStaticMetadata;
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::engine::host_rhi::PresentScalingMode;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::InputMailboxes;
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::window_present_loop::{
    NamedSurfacePresentationOutcome, SurfaceNamedForPresentationOnOwnedWindow,
    WindowPresentLoopForOwningProcessor, WindowPresentLoopRequestFromOwningProcessor,
};

use crate::video_frame::{ColorInfo, VideoFrame};

/// How long the render thread parks when the input has no frame to show. The
/// display is a `latest`-profile sink, so this is the worst-case lateness of a
/// frame that arrives just after a poll, not a frame budget.
const DISPLAY_RENDER_THREAD_IDLE_PARK_INTERVAL: Duration = Duration::from_millis(1);

/// The same park for a display with no window: it still drains, so upstream
/// sees a live consumer, but nothing is racing a vsync deadline.
const DEGRADED_DISPLAY_DRAIN_PARK_INTERVAL: Duration = Duration::from_millis(2);

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

/// The display's own render thread: acquire a window, then name frames onto
/// the engine's present loop until the graph stops or the user closes it.
struct DisplayWindowRenderLoop {
    gpu_context: GpuContextLimitedAccess,
    inputs: InputMailboxes,
    running: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    window_present_loop_request: WindowPresentLoopRequestFromOwningProcessor,
    /// The engine's present loop for this display's window. `None` is the
    /// degraded mode: no window could be had, so the display behaves as a
    /// sink — drains and discards — and upstream still sees a live consumer.
    window_present_loop: Option<WindowPresentLoopForOwningProcessor>,
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
            window_present_loop_request: WindowPresentLoopRequestFromOwningProcessor {
                window_title: config.title,
                initial_width_in_physical_pixels: config.width,
                initial_height_in_physical_pixels: config.height,
                scaling_mode_for_frame_in_window: config.scaling.present_scaling_mode(),
            },
            window_present_loop: None,
        }
    }

    fn run(&mut self) {
        if let Err(reason) = self.open_the_engines_present_loop_for_this_window() {
            self.degrade_to_drain_and_discard(&reason);
        }

        while self.running.load(Ordering::Acquire) {
            self.apply_window_events_from_event_pump();
            if !self.running.load(Ordering::Acquire) {
                break;
            }

            if self.window_present_loop.is_none() {
                self.drain_and_discard_so_upstream_sees_a_live_consumer();
                std::thread::park_timeout(DEGRADED_DISPLAY_DRAIN_PARK_INTERVAL);
                continue;
            }

            if self.inputs.has_data("video") {
                self.show_next_frame_on_the_window();
            } else {
                std::thread::park_timeout(DISPLAY_RENDER_THREAD_IDLE_PARK_INTERVAL);
            }
        }

        self.running.store(false, Ordering::Release);
    }

    fn open_the_engines_present_loop_for_this_window(&mut self) -> Result<()> {
        let request = self.window_present_loop_request.clone();
        let window_present_loop = self.gpu_context.escalate(|full| {
            WindowPresentLoopForOwningProcessor::open_on_the_process_wide_window_event_pump(
                full, request,
            )
        })?;
        self.window_present_loop = Some(window_present_loop);
        Ok(())
    }

    fn degrade_to_drain_and_discard(&mut self, reason: &Error) {
        tracing::error!(
            error = %reason,
            window_title = %self.window_present_loop_request.window_title,
            "DisplayWindow: no window — running degraded (frames drained, nothing shown)"
        );
    }

    fn apply_window_events_from_event_pump(&mut self) {
        let Some(window_present_loop) = self.window_present_loop.as_mut() else {
            return;
        };
        let events = match window_present_loop.apply_pending_window_events() {
            Ok(events) => events,
            Err(e) => {
                tracing::error!(error = %e, "DisplayWindow: swapchain recreate on resize failed");
                self.running.store(false, Ordering::Release);
                return;
            }
        };
        if events.close_requested_by_user {
            tracing::info!(
                window_title = %self.window_present_loop_request.window_title,
                "DisplayWindow: window close requested"
            );
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

    fn show_next_frame_on_the_window(&mut self) {
        // Taken before the destructive `latest` read below, so a frame is not
        // consumed when there is no window to show it on.
        let Some(window_present_loop) = self.window_present_loop.as_mut() else {
            return;
        };
        let frame_bag: VideoFrame = match self.inputs.read("video") {
            Ok(frame) => frame,
            Err(e) => {
                tracing::warn!(error = %e, "DisplayWindow: failed to read frame");
                return;
            }
        };

        let hdr_static_metadata = frame_bag.mastering_display.as_ref().map(|mastering| {
            hdr_static_metadata_from_bag(mastering, frame_bag.content_light.as_ref())
        });
        let named_surface = named_surface_of_video_frame(&frame_bag, hdr_static_metadata.as_ref());

        match window_present_loop.show_named_surface(&named_surface) {
            Ok(NamedSurfacePresentationOutcome::SurfaceIdDidNotResolve) => {}
            Ok(_) => {
                self.frame_counter.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                tracing::warn!(error = %e, "DisplayWindow: present failed");
                self.frame_counter.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Name a bag's frame for the engine's present loop: the bag's color
/// description projected onto the engine's colorspace-pick input, and its HDR
/// sidecar pre-translated by the caller (the metadata outlives the borrow).
fn named_surface_of_video_frame<'a>(
    frame_bag: &'a VideoFrame,
    hdr_static_metadata: Option<&'a HdrStaticMetadata>,
) -> SurfaceNamedForPresentationOnOwnedWindow<'a> {
    SurfaceNamedForPresentationOnOwnedWindow {
        surface_id: &frame_bag.surface_id,
        source_width_in_pixels: frame_bag.width,
        source_height_in_pixels: frame_bag.height,
        producer_published_texture_layout: frame_bag.texture_layout,
        color_traits_of_frame: frame_bag
            .color_info
            .as_ref()
            .map(ColorInfo::engine_color_traits),
        hdr_static_metadata_of_frame: hdr_static_metadata,
    }
}

/// Translate the bag's HDR sidecar integers to the engine's f32 metadata:
/// chromaticities are 1/50000 increments → CIE xy, luminances 0.0001 cd/m²
/// increments → cd/m².
fn hdr_static_metadata_from_bag(
    mastering: &crate::video_frame::MasteringDisplay,
    content_light: Option<&crate::video_frame::ContentLight>,
) -> HdrStaticMetadata {
    let chromaticity = |v: u32| v as f32 / 50_000.0;
    HdrStaticMetadata {
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

    /// The fold's one lossy step: the bag's four-tuple reaches the engine's
    /// present loop as the two axes the swapchain pick actually reads, and
    /// every other field travels verbatim.
    #[test]
    fn a_frame_bag_names_its_surface_with_the_axes_the_swapchain_pick_reads() {
        let frame_bag = VideoFrame {
            surface_id: "pool-slot-7#12".to_string(),
            width: 1920,
            height: 1080,
            texture_layout: Some(1000001002),
            color_info: Some(crate::video_frame::ColorInfo {
                primaries: Some(crate::video_frame::Primaries::Bt2020),
                transfer: Some(crate::video_frame::Transfer::Smpte2084),
                matrix: Some(crate::video_frame::Matrix::Bt2020Ncl),
                range: Some(crate::video_frame::Range::Limited),
            }),
            ..VideoFrame::default()
        };

        let named_surface = named_surface_of_video_frame(&frame_bag, None);

        assert_eq!(named_surface.surface_id, "pool-slot-7#12");
        assert_eq!(named_surface.source_width_in_pixels, 1920);
        assert_eq!(named_surface.source_height_in_pixels, 1080);
        assert_eq!(
            named_surface.producer_published_texture_layout,
            Some(1000001002)
        );
        assert!(named_surface.hdr_static_metadata_of_frame.is_none());
        let color_traits = named_surface
            .color_traits_of_frame
            .expect("a frame carrying color_info names its traits");
        assert_eq!(
            color_traits,
            frame_bag
                .color_info
                .as_ref()
                .map(ColorInfo::engine_color_traits)
                .expect("the same projection the loop compares against"),
        );
    }

    /// A bag with no color description names none: the present loop must see
    /// the absence, not a default, or the first frame would renegotiate the
    /// swapchain toward a pick nobody asked for.
    #[test]
    fn a_frame_bag_without_color_info_names_no_color_traits() {
        let frame_bag = VideoFrame {
            surface_id: "surface".to_string(),
            width: 640,
            height: 480,
            ..VideoFrame::default()
        };
        let named_surface = named_surface_of_video_frame(&frame_bag, None);
        assert!(named_surface.color_traits_of_frame.is_none());
        assert!(named_surface.producer_published_texture_layout.is_none());
    }

    /// The HDR sidecar is translated once per frame by the caller and named
    /// by reference, so the loop signals the driver the same numbers the bag
    /// carried.
    #[test]
    fn a_frame_bags_hdr_sidecar_reaches_the_named_surface() {
        let frame_bag = VideoFrame {
            surface_id: "surface".to_string(),
            mastering_display: Some(crate::video_frame::MasteringDisplay {
                display_primaries_r_x: 35_400,
                display_primaries_r_y: 14_600,
                display_primaries_g_x: 8_500,
                display_primaries_g_y: 39_850,
                display_primaries_b_x: 6_550,
                display_primaries_b_y: 2_300,
                white_point_x: 15_635,
                white_point_y: 16_450,
                max_luminance: 10_000_000,
                min_luminance: 50,
            }),
            content_light: Some(crate::video_frame::ContentLight {
                max_cll: 1_000,
                max_fall: 400,
            }),
            ..VideoFrame::default()
        };
        let metadata = hdr_static_metadata_from_bag(
            frame_bag
                .mastering_display
                .as_ref()
                .expect("the sidecar under test"),
            frame_bag.content_light.as_ref(),
        );

        let named_surface = named_surface_of_video_frame(&frame_bag, Some(&metadata));

        let named_metadata = named_surface
            .hdr_static_metadata_of_frame
            .expect("a frame carrying a mastering display names its metadata");
        assert_eq!(named_metadata.max_content_light_level, 1_000.0);
        assert!((named_metadata.max_luminance_cd_m2 - 1_000.0).abs() < 1e-3);
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
