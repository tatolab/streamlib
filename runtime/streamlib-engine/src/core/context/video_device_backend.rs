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

/// A capture device a backend can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCaptureDevice {
    /// The backend's name for the device — what a `device_id` names.
    pub id: String,
    /// The device's human-readable name, as its driver reports it.
    pub name: String,
}

/// What a capture stream negotiated with its device, fixed for its lifetime.
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
/// The frame is already on the GPU: `surface_id` names the pooled `Rgba32`
/// pixel buffer the arm converted the device's pixels into, and the arm holds
/// that slot until the hand-off returns — so a callee that publishes the id
/// publishes a live slot, and one that keeps the frame past the hand-off must
/// have published it first.
#[derive(Debug, Clone, Copy)]
pub struct CapturedVideoFrameFromDevice<'a> {
    /// Surface id of the pooled `Rgba32` pixel buffer holding the frame.
    pub surface_id: &'a str,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// The frame's colour as the device described it. An axis the device left
    /// unspecified is absent.
    pub color: H273ColorVui,
}

/// What a capture stream calls with each frame it captures.
///
/// Runs on the backend's own capture thread with the frame's device buffer and
/// pool slot held, so it must hand the frame on and return rather than block.
/// It must not re-enter the stream it was installed on: calling
/// [`VideoCaptureStream::stop_delivering`] from here waits on the thread that
/// is running it.
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

    /// Begin delivering captured frames to `hand_off`, replacing any delivery
    /// an earlier call started.
    fn start_delivering_to(&mut self, hand_off: CapturedVideoFrameHandOff) -> Result<()>;

    /// Stop delivering. The hand-off is not called again once this returns.
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

fn first_video_device_backend_arm_that_opens() -> SharedVideoDeviceBackend {
    first_video_device_backend_arm_that_opens_among(platform_video_device_backend_arms())
        .unwrap_or_else(|| Arc::new(RefusingNullVideoDeviceBackend))
}

/// Take the first arm that opens, logging each demotion with the reason that
/// caused it. Separate from the platform arm list so the walk is exercised by
/// arms that fail on purpose.
fn first_video_device_backend_arm_that_opens_among(
    arms: impl IntoIterator<Item = VideoDeviceBackendArm>,
) -> Option<SharedVideoDeviceBackend> {
    first_device_backend_arm_that_opens_among(arms, |backend_name, reason| {
        tracing::info!(
            video_backend = backend_name,
            %reason,
            "video device backend chain: demoting to the next arm"
        );
    })
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
    use crate::core::context::DeviceBackendArmUnavailableReason;

    /// An arm that opens, standing in for a real backend so the walk can be
    /// driven without a camera.
    struct ArmThatOpened(&'static str);

    impl VideoDeviceBackend for ArmThatOpened {
        fn backend_name(&self) -> &'static str {
            self.0
        }

        fn list_capture_devices(&self) -> Result<Vec<VideoCaptureDevice>> {
            unreachable!("the walk only ever opens the arm, never enumerates through it")
        }

        fn open_capture_stream(
            &self,
            _request: &VideoDeviceStreamRequest,
        ) -> Result<Box<dyn VideoCaptureStream>> {
            unreachable!("the walk only ever opens the arm, never a stream on it")
        }
    }

    fn an_arm_that_opens(backend_name: &'static str) -> VideoDeviceBackendArm {
        VideoDeviceBackendArm::named(backend_name, move || {
            Ok(Arc::new(ArmThatOpened(backend_name)) as SharedVideoDeviceBackend)
        })
    }

    fn an_arm_that_declines(backend_name: &'static str) -> VideoDeviceBackendArm {
        VideoDeviceBackendArm::named(backend_name, move || {
            Err(DeviceBackendArmUnavailableReason::of(format!(
                "{backend_name} was made to decline by this test"
            )))
        })
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

    #[test]
    fn the_video_walk_takes_the_first_arm_that_opens_and_asks_no_arm_behind_it() {
        let chosen = first_video_device_backend_arm_that_opens_among([
            an_arm_that_declines("first"),
            an_arm_that_opens("second"),
            VideoDeviceBackendArm::named("third", || {
                unreachable!("an arm behind one that opened is never asked")
            }),
        ])
        .expect("an arm opened");
        assert_eq!(chosen.backend_name(), "second");
    }

    #[test]
    fn a_video_chain_whose_arms_all_decline_yields_nothing_for_the_refusing_backend_to_answer() {
        assert!(
            first_video_device_backend_arm_that_opens_among([
                an_arm_that_declines("first"),
                an_arm_that_declines("second"),
            ])
            .is_none()
        );
    }
}
