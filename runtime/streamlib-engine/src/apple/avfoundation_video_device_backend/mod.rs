// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The AVFoundation arm of the video device seam: enumeration, open, 4:2:0
//! biplanar negotiation, and the capture session whose every frame lands in a
//! pooled `Rgba32` pixel buffer before it is handed off.
//!
//! Camera→GPU transport is [`Biplanar420IOSurfaceToPooledRgbaConversion`]'s.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceFormat,
    AVCaptureDevicePosition, AVCaptureDeviceTypeBuiltInWideAngleCamera, AVMediaTypeVideo,
};
use objc2_core_media::{CMFormatDescription, CMTime, CMVideoFormatDescriptionGetDimensions};
use objc2_foundation::NSArray;
use parking_lot::Mutex;

use crate::apple::permissions::{
    AvFoundationCaptureDeviceAuthorizationAuthority, CaptureDeviceAuthorizationAtOpen,
    CaptureDeviceAuthorizationAuthority, CaptureDeviceRefusal, PrivacyGatedCaptureDevice,
    authorize_the_capture_device_without_waiting_for_the_user, capture_device_refusal_for_the_user,
};
use crate::core::context::{
    CapturedVideoFrameHandOff, DeviceStreamFailureReason, DeviceStreamFailureRecorder,
    DeviceStreamLivenessReport, GpuContextLimitedAccess, VideoCaptureDevice,
    VideoCaptureInstantResolver, VideoCaptureStream, VideoCaptureStreamFormat, VideoDeviceBackend,
    VideoDeviceStreamRequest, refusal_for_a_named_camera_that_is_not_attached,
};
use crate::core::logging::{
    carrying_this_threads_project_log_route_entry, the_project_log_route_entry_of_this_thread,
};
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};
use avfoundation_capture_session_request::AvFoundationCaptureSessionRequest;
use avfoundation_frame_delivery::{
    AvFoundationCaptureProgress, AvFoundationFrameDelivery, AvFoundationRunningDelivery,
};

mod avfoundation_capture_session_request;
mod avfoundation_frame_delivery;
#[cfg(test)]
mod tests;

/// How long a stop waits for a frame already on the sample-buffer queue to
/// finish before saying it could not confirm delivery stopped — well inside
/// the engine's budget for a processor to stop.
const DELIVERY_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// The AVFoundation backend. The framework is always present, so this arm
/// always opens; a Mac with no camera enumerates no devices.
pub(crate) struct AvFoundationVideoDeviceBackend;

impl VideoDeviceBackend for AvFoundationVideoDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "avfoundation"
    }

    fn list_capture_devices(&self) -> Result<Vec<VideoCaptureDevice>> {
        Ok(avfoundation_capture_devices_in_default_order()
            .iter()
            .map(|device| video_capture_device_describing(device))
            .collect())
    }

    fn open_capture_stream(
        &self,
        request: &VideoDeviceStreamRequest,
    ) -> Result<Box<dyn VideoCaptureStream>> {
        Ok(Box::new(AvFoundationVideoCaptureStream::open(
            request,
            &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Camera),
        )?))
    }
}

/// Every video capture device AVFoundation lists: built-in cameras first, then
/// external ones — USB cameras and camera extensions alike.
///
/// Not `defaultDeviceWithMediaType`, which can answer with an idle camera
/// extension over the built-in camera, and not discovery order alone, which
/// lists extensions first.
fn avfoundation_capture_devices_in_default_order() -> Vec<Retained<AVCaptureDevice>> {
    // SAFETY: `AVMediaTypeVideo` is an AVFoundation-exported constant.
    let Some(video) = (unsafe { AVMediaTypeVideo }) else {
        return Vec::new();
    };
    // The deprecated synonym rather than `AVCaptureDeviceTypeExternal`, which
    // exists only from macOS 14: the macOS floor is the wheel's to set.
    #[allow(deprecated)]
    // SAFETY: both are AVFoundation-exported constants.
    let device_types = unsafe {
        NSArray::from_slice(&[
            AVCaptureDeviceTypeBuiltInWideAngleCamera,
            objc2_av_foundation::AVCaptureDeviceTypeExternalUnknown,
        ])
    };
    // SAFETY: a discovery query, callable from any thread.
    let mut devices = unsafe {
        AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &device_types,
            Some(video),
            AVCaptureDevicePosition::Unspecified,
        )
        .devices()
    }
    .to_vec();
    // SAFETY: an AVFoundation-exported constant.
    let built_in_camera_type = unsafe { AVCaptureDeviceTypeBuiltInWideAngleCamera }.to_string();
    // Compared as Rust strings: a camera extension written in Swift answers
    // with a string whose `isEqualToString:` objc2 refuses to call.
    // SAFETY: a property read on a live device.
    devices
        .sort_by_key(|device| unsafe { device.deviceType() }.to_string() != built_in_camera_type);
    devices
}

fn video_capture_device_describing(device: &AVCaptureDevice) -> VideoCaptureDevice {
    // SAFETY: property reads on a live device.
    unsafe {
        VideoCaptureDevice {
            id: device.uniqueID().to_string(),
            name: device.localizedName().to_string(),
        }
    }
}

/// The device `device_id` names — an AVFoundation `uniqueID` — or the first
/// camera listed. A named device that is not attached is refused naming it,
/// never replaced by a different one.
fn open_requested_avfoundation_device(
    device_id: Option<&str>,
) -> Result<Retained<AVCaptureDevice>> {
    let devices = avfoundation_capture_devices_in_default_order();
    match device_id {
        None => devices.into_iter().next().ok_or_else(|| {
            Error::Configuration(
                "No camera found: AVFoundation lists no video capture device. Check the camera \
                 is connected, or use TestPatternSource to run without one."
                    .into(),
            )
        }),
        Some(device_id) => devices
            .iter()
            // SAFETY: a property read on a live device.
            .find(|device| unsafe { device.uniqueID() }.to_string() == device_id)
            .cloned()
            .ok_or_else(|| {
                let attached: Vec<VideoCaptureDevice> = devices
                    .iter()
                    .map(|device| video_capture_device_describing(device))
                    .collect();
                Error::Configuration(refusal_for_a_named_camera_that_is_not_attached(
                    device_id,
                    &attached,
                    "Check the camera is connected",
                ))
            }),
    }
}

/// One of a device's formats, as the negotiation weighs it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AvFoundationFormatCandidate {
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    highest_frames_per_second: f64,
    /// The frame duration of that highest rate, which the device is set to.
    shortest_frame_duration: Option<CMTime>,
}

/// Which candidate to capture in: the most pixels within the cap, then the
/// highest frame rate, then a biplanar 4:2:0 format — `420v` before `420f` —
/// over one AVFoundation would have to convert. A device with nothing within
/// the cap captures in its smallest format.
fn index_of_the_format_to_capture_in(
    candidates: &[AvFoundationFormatCandidate],
    max_width: u32,
    max_height: u32,
) -> Option<usize> {
    let biplanar_preference = |pixel_format: PixelFormat| match pixel_format {
        PixelFormat::Nv12VideoRange => 2,
        PixelFormat::Nv12FullRange => 1,
        _ => 0,
    };
    let pixels = |candidate: &AvFoundationFormatCandidate| {
        u64::from(candidate.width) * u64::from(candidate.height)
    };
    let within_the_cap = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.width <= max_width && candidate.height <= max_height)
        .max_by(|(_, a), (_, b)| {
            pixels(a)
                .cmp(&pixels(b))
                .then(
                    a.highest_frames_per_second
                        .total_cmp(&b.highest_frames_per_second),
                )
                .then(biplanar_preference(a.pixel_format).cmp(&biplanar_preference(b.pixel_format)))
        })
        .map(|(index, _)| index);
    within_the_cap.or_else(|| {
        candidates
            .iter()
            .enumerate()
            .min_by_key(|(_, candidate)| pixels(candidate))
            .map(|(index, _)| index)
    })
}

/// The pixel format a stream asks AVFoundation to deliver in: the device's own
/// when it is biplanar 4:2:0, else `420v`, which AVFoundation converts into.
fn pixel_format_to_deliver_for(device_pixel_format: PixelFormat) -> PixelFormat {
    match device_pixel_format {
        PixelFormat::Nv12FullRange => PixelFormat::Nv12FullRange,
        _ => PixelFormat::Nv12VideoRange,
    }
}

fn format_candidate_describing(format: &AVCaptureDeviceFormat) -> AvFoundationFormatCandidate {
    // SAFETY: property reads on a live format, whose description is a video
    // format description because the device captures video.
    unsafe {
        let description: Retained<CMFormatDescription> = format.formatDescription();
        let dimensions = CMVideoFormatDescriptionGetDimensions(&description);
        let fastest_range = format
            .videoSupportedFrameRateRanges()
            .iter()
            .max_by(|a, b| a.maxFrameRate().total_cmp(&b.maxFrameRate()));
        AvFoundationFormatCandidate {
            width: dimensions.width.max(0) as u32,
            height: dimensions.height.max(0) as u32,
            pixel_format: PixelFormat::from_cv_pixel_format_type(description.media_sub_type()),
            highest_frames_per_second: fastest_range
                .as_ref()
                .map_or(0.0, |range| range.maxFrameRate()),
            shortest_frame_duration: fastest_range.map(|range| range.minFrameDuration()),
        }
    }
}

/// The serial queue every stream on the device named `device_unique_id`
/// configures, starts and stops its sessions on, for the life of the process.
fn session_control_queue_for_device(device_unique_id: &str) -> DispatchRetained<DispatchQueue> {
    static SESSION_CONTROL_QUEUE_BY_DEVICE: OnceLock<
        Mutex<HashMap<String, DispatchRetained<DispatchQueue>>>,
    > = OnceLock::new();
    SESSION_CONTROL_QUEUE_BY_DEVICE
        .get_or_init(Mutex::default)
        .lock()
        .entry(device_unique_id.to_owned())
        .or_insert_with(|| {
            DispatchQueue::new(
                &format!("com.streamlib.avfoundation-session-control.{device_unique_id}"),
                None,
            )
        })
        .clone()
}

/// A capture stream on one AVFoundation device: negotiated at open, and
/// delivering from its own capture session between `start_delivering_to` and
/// `stop_delivering` once the camera is allowed.
struct AvFoundationVideoCaptureStream {
    opened_device: VideoCaptureDevice,
    stream_format: VideoCaptureStreamFormat,
    liveness_report: DeviceStreamLivenessReport,
    capture_instant_resolver: Arc<VideoCaptureInstantResolver>,
    capture_session_control: Arc<Mutex<AvFoundationCaptureSessionControl>>,
}

/// Where camera access stands for one stream.
enum CameraAccessForTheStream {
    Granted,
    /// The user is being asked; a delivery started meanwhile waits here.
    AwaitingTheUsersAnswer {
        parked_hand_off: Option<CapturedVideoFrameHandOff>,
    },
    Refused(String),
}

/// The camera a stream opened, in the format it negotiated. Messaged only on
/// the device's serial control queue; elsewhere it is only retained and
/// released.
#[derive(Clone)]
struct CaptureDeviceConfiguredOnItsControlQueue {
    device: Retained<AVCaptureDevice>,
    device_format: Retained<AVCaptureDeviceFormat>,
    shortest_frame_duration: Option<CMTime>,
}

// SAFETY: AVFoundation's capture devices and formats may be configured from any
// thread, one caller at a time; every message this wrapper's objects receive is
// sent from the device's serial control queue — one per device, shared by every
// stream that opens it — which is that one caller.
unsafe impl Send for CaptureDeviceConfiguredOnItsControlQueue {}

/// Everything that starts and stops the stream's capture, behind one lock: the
/// processor's start and stop and the user's permission answer all reach it,
/// from different threads.
struct AvFoundationCaptureSessionControl {
    capture_device: CaptureDeviceConfiguredOnItsControlQueue,
    pixel_format_to_deliver: PixelFormat,
    opened_device: VideoCaptureDevice,
    stream_format: VideoCaptureStreamFormat,
    gpu_context: GpuContextLimitedAccess,
    processor_id_of_the_capturing_node: String,
    failure_recorder: DeviceStreamFailureRecorder,
    capture_instant_resolver: Arc<VideoCaptureInstantResolver>,
    camera_access: CameraAccessForTheStream,
    /// The device's serial control queue, shared by every stream on the
    /// device: every session is configured, started and stopped on it, in the
    /// order asked, so neither a restart nor a stream reopened on the device
    /// opens it beside a session still stopping.
    session_control_queue: DispatchRetained<DispatchQueue>,
    running_delivery: Option<AvFoundationRunningDelivery>,
}

impl AvFoundationVideoCaptureStream {
    fn open(
        request: &VideoDeviceStreamRequest,
        camera_authorization_authority: &dyn CaptureDeviceAuthorizationAuthority,
    ) -> Result<Self> {
        let device = open_requested_avfoundation_device(request.device_id.as_deref())?;
        let opened_device = video_capture_device_describing(&device);

        // SAFETY: a property read on a live device.
        let device_formats = unsafe { device.formats() }.to_vec();
        let candidates: Vec<AvFoundationFormatCandidate> = device_formats
            .iter()
            .map(|format| format_candidate_describing(format))
            .collect();
        let chosen =
            index_of_the_format_to_capture_in(&candidates, request.max_width, request.max_height)
                .ok_or_else(|| {
                Error::Configuration(format!(
                    "Camera '{}' ({}) offers no video format to capture in.",
                    opened_device.name, opened_device.id
                ))
            })?;
        let chosen_candidate = candidates[chosen];
        if chosen_candidate.width > request.max_width
            || chosen_candidate.height > request.max_height
        {
            tracing::warn!(
                camera = %opened_device.name,
                width = chosen_candidate.width,
                height = chosen_candidate.height,
                max_width = request.max_width,
                max_height = request.max_height,
                "AVFoundation camera offers no format within the configured cap; capturing in \
                 its smallest"
            );
        }
        let stream_format = VideoCaptureStreamFormat {
            width: chosen_candidate.width,
            height: chosen_candidate.height,
            frames_per_second: Some(chosen_candidate.highest_frames_per_second.floor() as u32)
                .filter(|&frames_per_second| frames_per_second > 0),
        };
        let pixel_format_to_deliver = pixel_format_to_deliver_for(chosen_candidate.pixel_format);
        tracing::info!(
            camera = %opened_device.name,
            device_id = %opened_device.id,
            width = stream_format.width,
            height = stream_format.height,
            frames_per_second = ?stream_format.frames_per_second,
            device_pixel_format = ?chosen_candidate.pixel_format,
            delivered_pixel_format = ?pixel_format_to_deliver,
            "AVFoundation camera: format negotiated"
        );

        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
        let capture_instant_resolver = Arc::new(VideoCaptureInstantResolver::for_device(
            opened_device.name.clone(),
        ));
        let device_format = device_formats[chosen].clone();
        let session_control_queue = session_control_queue_for_device(&opened_device.id);
        let capture_session_control = Arc::new(Mutex::new(AvFoundationCaptureSessionControl {
            capture_device: CaptureDeviceConfiguredOnItsControlQueue {
                device,
                device_format,
                shortest_frame_duration: chosen_candidate.shortest_frame_duration,
            },
            pixel_format_to_deliver,
            opened_device: opened_device.clone(),
            stream_format,
            gpu_context: request.gpu_context.clone(),
            processor_id_of_the_capturing_node: request.processor_id_of_the_capturing_node.clone(),
            failure_recorder,
            capture_instant_resolver: Arc::clone(&capture_instant_resolver),
            camera_access: CameraAccessForTheStream::AwaitingTheUsersAnswer {
                parked_hand_off: None,
            },
            session_control_queue,
            running_delivery: None,
        }));

        let answer_reaches = Arc::downgrade(&capture_session_control);
        match authorize_the_capture_device_without_waiting_for_the_user(
            camera_authorization_authority,
            request.capture_device_access_prompting,
            Box::new(move |granted| the_users_camera_answer_arrived(&answer_reaches, granted)),
        )? {
            CaptureDeviceAuthorizationAtOpen::Granted => {
                capture_session_control.lock().camera_access = CameraAccessForTheStream::Granted;
            }
            CaptureDeviceAuthorizationAtOpen::AwaitingTheUsersAnswer => {}
        }

        Ok(Self {
            opened_device,
            stream_format,
            liveness_report,
            capture_instant_resolver,
            capture_session_control,
        })
    }
}

/// What a stream does with the user's answer to its camera request, whenever
/// and on whatever thread it arrives.
fn the_users_camera_answer_arrived(
    capture_session_control: &Weak<Mutex<AvFoundationCaptureSessionControl>>,
    granted: bool,
) {
    let Some(capture_session_control) = capture_session_control.upgrade() else {
        return;
    };
    let mut control = capture_session_control.lock();
    let parked_hand_off = match &mut control.camera_access {
        CameraAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } => {
            parked_hand_off.take()
        }
        _ => None,
    };
    if granted {
        tracing::info!(camera = %control.opened_device.name, "camera access allowed");
        control.camera_access = CameraAccessForTheStream::Granted;
        if let Some(hand_off) = parked_hand_off {
            control.start_delivering_into(hand_off);
        }
        return;
    }
    let refusal = capture_device_refusal_for_the_user(
        PrivacyGatedCaptureDevice::Camera,
        CaptureDeviceRefusal::DeniedByTheUser,
    );
    tracing::error!(camera = %control.opened_device.name, "{refusal}");
    control
        .failure_recorder
        .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(refusal.clone()));
    control.camera_access = CameraAccessForTheStream::Refused(refusal);
}

impl VideoCaptureStream for AvFoundationVideoCaptureStream {
    fn stream_format(&self) -> VideoCaptureStreamFormat {
        self.stream_format
    }

    fn opened_device(&self) -> &VideoCaptureDevice {
        &self.opened_device
    }

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn future_capture_stamps_clamped_to_dequeue(&self) -> u64 {
        self.capture_instant_resolver
            .future_capture_stamps_clamped_to_dequeue()
    }

    fn start_delivering_to(&mut self, hand_off: CapturedVideoFrameHandOff) -> Result<()> {
        self.stop_delivering()?;
        let mut control = self.capture_session_control.lock();
        match &mut control.camera_access {
            CameraAccessForTheStream::Granted => {
                control.start_delivering_into(hand_off);
                Ok(())
            }
            CameraAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } => {
                *parked_hand_off = Some(hand_off);
                Ok(())
            }
            CameraAccessForTheStream::Refused(refusal) => {
                Err(Error::Configuration(refusal.clone()))
            }
        }
    }

    fn stop_delivering(&mut self) -> Result<()> {
        let mut control = self.capture_session_control.lock();
        if let CameraAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } =
            &mut control.camera_access
        {
            *parked_hand_off = None;
        }
        match control.running_delivery.take() {
            Some(running_delivery) => {
                running_delivery.stop(&control.session_control_queue, &control.opened_device.name)
            }
            None => Ok(()),
        }
    }
}

impl Drop for AvFoundationVideoCaptureStream {
    fn drop(&mut self) {
        if let Err(stop_error) = self.stop_delivering() {
            tracing::warn!(
                error = %stop_error,
                "AVFoundation capture stream dropped while delivering"
            );
        }
    }
}

impl AvFoundationCaptureSessionControl {
    /// Begin delivering into `hand_off`: the frame delivery is ready at once,
    /// and the session is configured and started on the control queue, where
    /// `startRunning` may take seconds while the camera powers up. A session
    /// that cannot start ends the stream's liveness, naming why.
    fn start_delivering_into(&mut self, hand_off: CapturedVideoFrameHandOff) {
        debug_assert!(
            self.running_delivery.is_none(),
            "a delivery is stopped before another starts"
        );
        let frame_delivery = Arc::new(AvFoundationFrameDelivery {
            hand_off,
            is_delivering: AtomicBool::new(true),
            capture_session: OnceLock::new(),
            synchronization_clock: OnceLock::new(),
            camera_name: self.opened_device.name.clone(),
            stream_format: self.stream_format,
            pixel_format_delivered: self.pixel_format_to_deliver,
            gpu_context: self.gpu_context.clone(),
            processor_id_of_the_capturing_node: self.processor_id_of_the_capturing_node.clone(),
            capture_instant_resolver: Arc::clone(&self.capture_instant_resolver),
            capture_progress: Mutex::new(AvFoundationCaptureProgress::default()),
            project_log_route_entry_of_the_starting_thread:
                the_project_log_route_entry_of_this_thread(),
        });
        let sample_buffer_queue = DispatchQueue::new(
            &format!(
                "com.streamlib.avfoundation-capture.{}",
                self.opened_device.id
            ),
            None,
        );
        let configured_session = Arc::new(Mutex::new(None));

        let session_request = AvFoundationCaptureSessionRequest {
            capture_device: self.capture_device.clone(),
            pixel_format_to_deliver: self.pixel_format_to_deliver,
            stream_format: self.stream_format,
            frame_delivery: Arc::clone(&frame_delivery),
            sample_buffer_queue: sample_buffer_queue.clone(),
            failure_recorder: self.failure_recorder.clone(),
            camera_name: self.opened_device.name.clone(),
        };
        let configured_session_slot = Arc::clone(&configured_session);
        self.session_control_queue
            .exec_async(carrying_this_threads_project_log_route_entry(
                move || match session_request.configure_and_start() {
                    Ok(configured) => *configured_session_slot.lock() = configured,
                    Err(start_error) => {
                        tracing::error!(
                            camera = %session_request.camera_name,
                            error = %start_error,
                            "AVFoundation camera: capture could not start"
                        );
                        session_request
                            .failure_recorder
                            .record_the_failure_that_ended_the_stream(
                                DeviceStreamFailureReason::of(start_error.to_string()),
                            );
                    }
                },
            ));

        self.running_delivery = Some(AvFoundationRunningDelivery {
            frame_delivery,
            sample_buffer_queue,
            configured_session,
        });
    }
}
