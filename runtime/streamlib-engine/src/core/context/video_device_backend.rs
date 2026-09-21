// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video device seam: the one path anything opens a video capture stream
//! through.
//!
//! It sits beside the audio device seam and composes the same pieces — the
//! device-stream liveness report and the first-arm-that-opens walk — so video
//! reaches a camera the way audio reaches a microphone: through a
//! handle-shaped primitive the built-ins are written against, with each
//! platform's capture API an arm behind it.

use std::sync::{Arc, OnceLock};

use super::GpuContextLimitedAccess;
use super::device_backend_probe_chain::{
    DeviceBackendArm, first_device_backend_arm_that_opens_among,
};
use super::device_stream_liveness_report::DeviceStreamLivenessReport;
use super::refusing_null_video_device_backend::RefusingNullVideoDeviceBackend;
use crate::core::Result;
use crate::core::color::H273ColorVui;
use crate::core::rhi::PublishedPixelBufferFrameId;

/// A capture device a backend can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCaptureDevice {
    /// The backend's name for the device — what a `device_id` names.
    pub id: String,
    /// The device's human-readable name, as its driver reports it.
    pub name: String,
}

/// What a capture stream negotiated with its device when it opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoCaptureStreamFormat {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// The device's frame rate, when it reports one.
    pub frames_per_second: Option<u32>,
}

/// One frame as a capture stream delivered it, borrowed for the length of the
/// hand-off.
///
/// The frame is already on the GPU, in the pooled `Rgba32` pixel buffer the
/// arm converted the device's pixels into. The arm holds that slot until the
/// hand-off returns, so a callee that publishes the frame's id publishes a
/// live slot, and one that keeps the frame past the hand-off must have
/// published it first.
#[derive(Debug, Clone, Copy)]
pub struct CapturedVideoFrameFromDevice<'a> {
    /// The id the pooled pixel buffer holding the frame publishes it under —
    /// what a bag's `surface_id` carries.
    pub published_pixel_buffer_frame_id: &'a PublishedPixelBufferFrameId,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// The frame's colour as the device described it. An axis the device left
    /// unspecified is absent.
    pub color: H273ColorVui,
    /// The instant the device captured the frame, in nanoseconds on the
    /// machine's monotonic clock — the device's own stamp where it is usable,
    /// never the instant of hand-off.
    pub capture_timestamp_ns: i64,
}

/// What a capture stream calls with each frame it captures.
///
/// Runs on the backend's own capture thread with the frame's device buffer and
/// pool slot held, so it must hand the frame on and return rather than block.
/// It must not re-enter the stream it was installed on: calling
/// [`VideoCaptureStream::stop_delivering`] from here waits on the thread that
/// is running it, and never succeeds.
pub type CapturedVideoFrameHandOff = Box<dyn Fn(CapturedVideoFrameFromDevice<'_>) + Send + Sync>;

/// What a caller asks a backend to open a capture stream for.
pub struct VideoDeviceStreamRequest {
    /// Backend-named device. `None` takes the first capture device the backend
    /// finds; a name the backend cannot open is refused by name, never a quiet
    /// landing on a different device.
    pub device_id: Option<String>,
    /// Resolution cap in pixels; the negotiated format is clamped to fit.
    pub max_width: u32,
    /// Resolution cap in pixels; the negotiated format is clamped to fit.
    pub max_height: u32,
    /// The GPU the stream converts its frames on.
    pub gpu_context: GpuContextLimitedAccess,
}

/// A capture stream a backend opened.
pub trait VideoCaptureStream: Send {
    /// The extent and frame rate every frame this stream delivers carries.
    fn stream_format(&self) -> VideoCaptureStreamFormat;

    /// The device this stream opened.
    fn opened_device(&self) -> &VideoCaptureDevice;

    /// Whether this stream is still capturing, readable from whatever thread
    /// the owner does its work on.
    ///
    /// The report belongs to the stream and outlives any one delivery, so a
    /// failure it names outlives [`Self::start_delivering_to`] too:
    /// restarting delivery does not bring a device back.
    fn liveness_report(&self) -> DeviceStreamLivenessReport;

    /// How many of this stream's device stamps were ahead of the instant their
    /// frame was dequeued and were clamped to it.
    fn future_capture_stamps_clamped_to_dequeue(&self) -> u64;

    /// Begin delivering captured frames to `hand_off`, replacing any delivery
    /// an earlier call started.
    fn start_delivering_to(&mut self, hand_off: CapturedVideoFrameHandOff) -> Result<()>;

    /// Stop delivering. Once this returns `Ok` the hand-off is not called
    /// again; an `Err` names a delivery that could not be confirmed stopped.
    fn stop_delivering(&mut self) -> Result<()>;
}

/// The video device seam every capture stream is opened through.
pub trait VideoDeviceBackend: Send + Sync {
    /// The arm's name, for the one probe log line and for error text.
    fn backend_name(&self) -> &'static str;

    /// The capture devices this backend can open, in the order its default
    /// picks from.
    fn list_capture_devices(&self) -> Result<Vec<VideoCaptureDevice>>;

    /// Open a capture stream against the named device, or the first capture
    /// device found when none is named.
    fn open_capture_stream(
        &self,
        request: &VideoDeviceStreamRequest,
    ) -> Result<Box<dyn VideoCaptureStream>>;
}

/// Why a camera named by `device_id` cannot be opened when it is not attached:
/// naming it and listing the cameras that are, or — when none is — saying how
/// to check for one.
pub(crate) fn refusal_for_a_named_camera_that_is_not_attached(
    device_id: &str,
    attached: &[VideoCaptureDevice],
    how_to_check_a_camera_is_attached: &str,
) -> String {
    if attached.is_empty() {
        return format!(
            "Camera '{device_id}' does not exist and no other camera is attached. \
             {how_to_check_a_camera_is_attached}, or use TestPatternSource to run without one."
        );
    }
    let attached = attached
        .iter()
        .map(|device| format!("{} ({})", device.id, device.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Camera '{device_id}' does not exist. Attached cameras: {attached}. Fix device_id, or \
         omit it to use the first camera found."
    )
}

/// Shared handle to the backend the chain probed.
pub type SharedVideoDeviceBackend = Arc<dyn VideoDeviceBackend>;

/// One arm of the video chain.
type VideoDeviceBackendArm = DeviceBackendArm<SharedVideoDeviceBackend>;

static PROBED_VIDEO_DEVICE_BACKEND: OnceLock<SharedVideoDeviceBackend> = OnceLock::new();

/// Probe the video backend chain once per process and log the arm it chose.
///
/// No configuration dial selects an arm and no environment variable overrides
/// the probe. A platform with no capture arm lands on a backend that refuses
/// every open by name, so a camera asked for there fails at `setup()` saying
/// why rather than never producing a frame.
pub fn probe_video_device_backend() -> SharedVideoDeviceBackend {
    Arc::clone(PROBED_VIDEO_DEVICE_BACKEND.get_or_init(|| {
        let backend = first_video_device_backend_arm_that_opens();
        tracing::info!(
            video_backend = backend.backend_name(),
            "video device backend chain probed"
        );
        backend
    }))
}

/// Take the first arm that opens, logging each demotion with the reason that
/// caused it, or the refusing backend once every arm has declined.
fn first_video_device_backend_arm_that_opens() -> SharedVideoDeviceBackend {
    first_device_backend_arm_that_opens_among(
        platform_video_device_backend_arms(),
        |backend_name, reason| {
            tracing::info!(
                video_backend = backend_name,
                %reason,
                "video device backend chain: demoting to the next arm"
            );
        },
    )
    .unwrap_or_else(|| Arc::new(RefusingNullVideoDeviceBackend))
}

/// The chain's real arms: V4L2, else — once it has declined — the refusing
/// backend the walk falls through to.
#[cfg(target_os = "linux")]
fn platform_video_device_backend_arms() -> Vec<VideoDeviceBackendArm> {
    use crate::linux::v4l2_video_device_backend::V4l2VideoDeviceBackend;

    vec![VideoDeviceBackendArm::named("v4l2", || {
        Ok(Arc::new(V4l2VideoDeviceBackend) as SharedVideoDeviceBackend)
    })]
}

/// No capture arm serves this platform yet; the walk falls through to the
/// refusing backend.
#[cfg(not(target_os = "linux"))]
fn platform_video_device_backend_arms() -> Vec<VideoDeviceBackendArm> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_camera_that_is_not_attached_is_refused_listing_the_ones_that_are() {
        let refusal = refusal_for_a_named_camera_that_is_not_attached(
            "/dev/video9",
            &[VideoCaptureDevice {
                id: "/dev/video0".into(),
                name: "FaceTime HD Camera".into(),
            }],
            "Check the camera is plugged in",
        );
        assert!(
            refusal.contains("'/dev/video9' does not exist"),
            "{refusal}"
        );
        assert!(
            refusal.contains("/dev/video0 (FaceTime HD Camera)"),
            "{refusal}"
        );
    }

    #[test]
    fn a_named_camera_with_nothing_attached_says_how_to_check_and_how_to_run_without_one() {
        let refusal = refusal_for_a_named_camera_that_is_not_attached(
            "a-camera",
            &[],
            "Check the camera is plugged in",
        );
        assert!(
            refusal.contains("Check the camera is plugged in"),
            "{refusal}"
        );
        assert!(refusal.contains("TestPatternSource"), "{refusal}");
    }

    #[test]
    fn the_video_chain_is_probed_once_and_hands_back_the_same_backend_every_time() {
        let first = probe_video_device_backend();
        let second = probe_video_device_backend();
        assert!(
            Arc::ptr_eq(&first, &second),
            "the chain is probed once per process, so every caller shares one backend"
        );
    }

    #[test]
    fn the_video_chain_always_lands_on_an_arm_whether_or_not_the_platform_captures() {
        let backend = probe_video_device_backend();
        assert!(
            ["v4l2", "refusing-null"].contains(&backend.backend_name()),
            "the chain resolved to an arm nothing declares: {}",
            backend.backend_name()
        );
    }

    /// Read off the list the probe itself walks rather than restated beside
    /// it, so an arm inserted in the wrong place fails here.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_video_chain_offers_v4l2_before_falling_through_to_the_refusing_backend() {
        let arm_names: Vec<&str> = platform_video_device_backend_arms()
            .iter()
            .map(|arm| arm.backend_name)
            .collect();
        assert_eq!(arm_names, ["v4l2"]);
    }
}
