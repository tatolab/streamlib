// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in camera source: frames from the engine's video device seam,
//! published as `VideoFrame` bags.
//!
//! The platform's capture arm — V4L2 on Linux — hands off every frame already
//! converted into a pooled `Rgba32` pixel buffer, so this processor opens the
//! stream, names the frame, and publishes it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, RuntimeContextFullAccess,
    VideoCaptureStream, VideoDeviceStreamRequest, probe_video_device_backend,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::media_clock::MediaClock;
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::schemars::JsonSchema;

use crate::h273_color_vui_translation::h273_color_vui_to_color_info;
use crate::video_frame::VideoFrame;

/// Resolution cap when the config names none; preserves the real-time
/// encoding guardrail, and high-resolution use cases opt in by raising it.
const DEFAULT_MAX_WIDTH: u32 = 1920;
const DEFAULT_MAX_HEIGHT: u32 = 1080;

/// Configuration for [`CameraSource`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct CameraSourceConfig {
    /// The capture backend's name for the device — a V4L2 device path
    /// (`/dev/video0`) on Linux. Absent: the first capture-capable device
    /// found.
    #[serde(default)]
    pub device_id: Option<String>,
    /// Resolution cap; the negotiated format is clamped to fit. Default 1920.
    #[serde(default)]
    pub max_width: Option<u32>,
    /// Resolution cap; the negotiated format is clamped to fit. Default 1080.
    #[serde(default)]
    pub max_height: Option<u32>,
}

#[streamlib::sdk::processor(
    description = "Captures live video from the platform's camera — V4L2 on Linux (zero-copy DMA-BUF when the device exports it, CPU upload otherwise)",
    execution = manual,
    scheduling = high,
    config = crate::camera_source::CameraSourceConfig,
    output("video", description = "Live camera video frames"),
)]
pub struct CameraSource {
    capture_stream: Option<Box<dyn VideoCaptureStream>>,
    camera_name: String,
    frame_counter: Arc<AtomicU64>,
}

impl ManualProcessor for CameraSource::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let backend = probe_video_device_backend();
        let capture_stream = backend.open_capture_stream(&VideoDeviceStreamRequest {
            device_id: self.config.device_id.clone(),
            max_width: self.config.max_width.unwrap_or(DEFAULT_MAX_WIDTH),
            max_height: self.config.max_height.unwrap_or(DEFAULT_MAX_HEIGHT),
            gpu_context: ctx.gpu_limited_access().clone(),
        })?;
        let stream_format = capture_stream.stream_format();
        let opened_device = capture_stream.opened_device();
        tracing::info!(
            video_backend = backend.backend_name(),
            device_id = %opened_device.id,
            device_name = %opened_device.name,
            width = stream_format.width,
            height = stream_format.height,
            frames_per_second = ?stream_format.frames_per_second,
            "CameraSource: capture stream opened"
        );
        self.camera_name = opened_device.name.clone();
        self.capture_stream = Some(capture_stream);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            camera = %self.camera_name,
            frames = self.frame_counter.load(Ordering::Relaxed),
            capture_device_failure = ?self
                .capture_stream
                .as_ref()
                .and_then(|stream| stream.liveness_report().failure_that_ended_the_stream()),
            "CameraSource: teardown"
        );
        self.stop_delivering();
        self.capture_stream = None;
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let Some(capture_stream) = self.capture_stream.as_mut() else {
            return Err(Error::Configuration(
                "CameraSource: no capture stream is open. setup() must run first.".into(),
            ));
        };
        let frames_per_second = capture_stream.stream_format().frames_per_second;
        capture_stream.start_delivering_to(video_frame_hand_off_publishing_to(
            self.outputs.clone(),
            Arc::clone(&self.frame_counter),
            self.camera_name.clone(),
            frames_per_second,
        ))
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_delivering();
        tracing::info!(
            camera = %self.camera_name,
            frames = self.frame_counter.load(Ordering::Relaxed),
            "CameraSource: stopped"
        );
        Ok(())
    }
}

impl CameraSource::Processor {
    fn stop_delivering(&mut self) {
        if let Some(capture_stream) = self.capture_stream.as_mut()
            && let Err(e) = capture_stream.stop_delivering()
        {
            tracing::warn!(error = %e, "CameraSource: capture stream failed to stop");
        }
    }
}

/// The hand-off the capture stream delivers into: each frame counted, named
/// as a `VideoFrame` bag and written on `video`.
fn video_frame_hand_off_publishing_to(
    outputs: OutputWriter,
    frame_counter: Arc<AtomicU64>,
    camera_name: String,
    frames_per_second: Option<u32>,
) -> CapturedVideoFrameHandOff {
    Box::new(move |captured: CapturedVideoFrameFromDevice<'_>| {
        frame_counter.fetch_add(1, Ordering::Relaxed);
        let frame = video_frame_bag_for(
            &captured,
            frames_per_second,
            MediaClock::now().as_nanos() as i64,
        );
        if let Err(e) = outputs.write("video", &frame) {
            tracing::error!(camera = camera_name, error = %e, "failed to write frame");
        }
    })
}

fn video_frame_bag_for(
    captured: &CapturedVideoFrameFromDevice<'_>,
    frames_per_second: Option<u32>,
    timestamp_ns: i64,
) -> VideoFrame {
    VideoFrame {
        surface_id: captured.published_pixel_buffer_frame_id.to_string(),
        width: captured.width,
        height: captured.height,
        timestamp_ns,
        fps: frames_per_second,
        // Present even when the device described no axis: an empty map is how
        // a camera's bag says every axis is unspecified.
        color_info: Some(h273_color_vui_to_color_info(&captured.color).unwrap_or_default()),
        // No capture device surfaces ST.2086 / CLLI; HDR-aware sources only.
        mastering_display: None,
        content_light: None,
        texture_layout: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgpack_wire_test_support::{
        decode_msgpack_named_map_entries, wire_map_entry_named,
    };
    use crate::video_frame::{ColorInfo, Matrix, Primaries, Range, Transfer};
    use streamlib::sdk::color::H273ColorVui;
    use streamlib::sdk::color::h273_color_vui::{matrix, primaries, transfer};
    use streamlib::sdk::rhi::{PixelBufferPoolSlotId, PublishedPixelBufferFrameId};

    fn a_published_frame_id() -> PublishedPixelBufferFrameId {
        PublishedPixelBufferFrameId::new(PixelBufferPoolSlotId::from_str("pooled-slot"), 7)
    }

    fn a_captured_frame(
        published_pixel_buffer_frame_id: &PublishedPixelBufferFrameId,
        color: H273ColorVui,
    ) -> CapturedVideoFrameFromDevice<'_> {
        CapturedVideoFrameFromDevice {
            published_pixel_buffer_frame_id,
            width: 1280,
            height: 720,
            color,
        }
    }

    #[test]
    fn config_defaults_to_no_device_and_uncapped() {
        let config: CameraSourceConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config.device_id, None);
        assert_eq!((config.max_width, config.max_height), (None, None));
    }

    #[test]
    fn a_captured_frame_is_published_as_the_bag_a_camera_has_always_published() {
        let published_frame_id = a_published_frame_id();
        let frame = video_frame_bag_for(
            &a_captured_frame(
                &published_frame_id,
                H273ColorVui {
                    primaries: Some(primaries::SMPTE170M),
                    transfer: Some(transfer::BT709),
                    matrix: Some(matrix::SMPTE170M),
                    full_range: Some(false),
                },
            ),
            Some(30),
            1_234_567,
        );
        assert_eq!(
            frame,
            VideoFrame {
                surface_id: "pooled-slot#7".to_string(),
                width: 1280,
                height: 720,
                timestamp_ns: 1_234_567,
                color_info: Some(ColorInfo {
                    primaries: Some(Primaries::Smpte170m),
                    transfer: Some(Transfer::Bt709),
                    matrix: Some(Matrix::Smpte170m),
                    range: Some(Range::Limited),
                }),
                content_light: None,
                fps: Some(30),
                mastering_display: None,
                texture_layout: None,
            }
        );
    }

    /// A device that describes no colour axis — `V4L2_COLORSPACE_DEFAULT`, or
    /// a format query that failed — publishes `color_info` as an empty map,
    /// never an absent key.
    #[test]
    fn a_device_that_describes_no_colour_still_publishes_an_empty_colour_map() {
        let published_frame_id = a_published_frame_id();
        let frame = video_frame_bag_for(
            &a_captured_frame(&published_frame_id, H273ColorVui::default()),
            None,
            0,
        );
        let bag = rmp_serde::to_vec_named(&frame).expect("a frame serialises");
        let entries = decode_msgpack_named_map_entries(&bag);
        assert_eq!(
            wire_map_entry_named(&entries, "color_info"),
            rmpv::Value::Map(Vec::new()),
            "an unknown colour is an empty map on the wire, never an absent key"
        );
    }
}
