// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The AVFoundation arm of the video device seam: enumeration, open, 4:2:0
//! biplanar negotiation, and the capture session whose every frame lands in a
//! pooled `Rgba32` pixel buffer before it is handed off.
//!
//! Camera→GPU transport is [`Biplanar420IOSurfaceToPooledRgbaConversion`]'s.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak, mpsc};
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceFormat,
    AVCaptureDeviceInput, AVCaptureDevicePosition, AVCaptureDeviceTypeBuiltInWideAngleCamera,
    AVCaptureDeviceWasDisconnectedNotification, AVCaptureOutput, AVCaptureSession,
    AVCaptureSessionRuntimeErrorNotification, AVCaptureVideoDataOutput,
    AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{
    CMClock, CMFormatDescription, CMSampleBuffer, CMSyncConvertTime, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionGetDimensions,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetIOSurface, CVPixelBufferGetWidth,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSNotification, NSNotificationCenter, NSNotificationName, NSNumber,
    NSObject, NSObjectProtocol, NSString,
};
use parking_lot::Mutex;

use crate::apple::biplanar_420_iosurface_to_pooled_rgba_conversion::Biplanar420IOSurfaceToPooledRgbaConversion;
use crate::apple::core_video_pixel_buffer_color::core_video_pixel_buffer_color_to_h273_color_vui;
use crate::apple::core_video_pixel_format_dictionary::ensure_core_video_pixel_format_dictionary_is_initialised;
use crate::apple::permissions::{
    AvFoundationCaptureDeviceAuthorizationAuthority, CaptureDeviceAuthorizationAtOpen,
    CaptureDeviceAuthorizationAuthority, CaptureDeviceRefusal, PrivacyGatedCaptureDevice,
    authorize_the_capture_device_without_waiting_for_the_user, capture_device_refusal_for_the_user,
};
use crate::core::color::{ColorSpaceKind, H273ColorVui};
use crate::core::context::captured_video_frame_to_pooled_rgba_conversion_stage::CapturedVideoFrameDeliveryTally;
use crate::core::context::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, DeviceReportedCaptureStamp,
    DeviceStreamFailureReason, DeviceStreamFailureRecorder, DeviceStreamLivenessReport,
    GpuContextLimitedAccess, VideoCaptureDevice, VideoCaptureInstantResolver, VideoCaptureStream,
    VideoCaptureStreamFormat, VideoDeviceBackend, VideoDeviceStreamRequest,
    refusal_for_a_named_camera_that_is_not_attached,
};
use crate::core::media_clock::MediaClock;
use crate::core::rhi::{PixelBuffer, PixelFormat, PublishedPixelBufferFrameId};
use crate::core::{Error, Result};

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
            capture_instant_resolver: Arc::clone(&self.capture_instant_resolver),
            capture_progress: Mutex::new(AvFoundationCaptureProgress::default()),
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
        self.session_control_queue.exec_async(move || {
            match session_request.configure_and_start() {
                Ok(configured) => *configured_session_slot.lock() = configured,
                Err(start_error) => {
                    tracing::error!(
                        camera = %session_request.camera_name,
                        error = %start_error,
                        "AVFoundation camera: capture could not start"
                    );
                    session_request
                        .failure_recorder
                        .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
                            start_error.to_string(),
                        ));
                }
            }
        });

        self.running_delivery = Some(AvFoundationRunningDelivery {
            frame_delivery,
            sample_buffer_queue,
            configured_session,
        });
    }
}

/// What the control queue needs to configure and start one session.
struct AvFoundationCaptureSessionRequest {
    capture_device: CaptureDeviceConfiguredOnItsControlQueue,
    pixel_format_to_deliver: PixelFormat,
    stream_format: VideoCaptureStreamFormat,
    frame_delivery: Arc<AvFoundationFrameDelivery>,
    sample_buffer_queue: DispatchRetained<DispatchQueue>,
    failure_recorder: DeviceStreamFailureRecorder,
    camera_name: String,
}

impl AvFoundationCaptureSessionRequest {
    /// Configure a session on the device and format, point its output at the
    /// frame delivery, and start it — unless the delivery was stopped while
    /// this waited its turn, when powering the camera up would only be torn
    /// down again. Runs on the device's control queue.
    fn configure_and_start(&self) -> Result<Option<AvFoundationConfiguredCaptureSession>> {
        if !self.frame_delivery.is_delivering.load(Ordering::Acquire) {
            return Ok(None);
        }
        let refused = |what_failed: String| {
            Error::Configuration(format!(
                "AVFoundation camera '{}': {what_failed}",
                self.camera_name
            ))
        };
        let CaptureDeviceConfiguredOnItsControlQueue {
            device,
            device_format,
            shortest_frame_duration,
        } = &self.capture_device;

        ensure_core_video_pixel_format_dictionary_is_initialised();
        // SAFETY: AVFoundation's documented configuration sequence, on objects
        // this stream owns, on its serial control queue.
        let (session, output) = unsafe {
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(device).map_err(|e| {
                refused(format!(
                    "the device could not be opened: {}",
                    e.localizedDescription()
                ))
            })?;
            let session = AVCaptureSession::new();
            session.beginConfiguration();
            if !session.canAddInput(&input) {
                session.commitConfiguration();
                return Err(refused(
                    "the session cannot take the device as an input".into(),
                ));
            }
            session.addInput(&input);

            let output = AVCaptureVideoDataOutput::new();
            output.setVideoSettings(Some(&video_settings_delivering(
                self.pixel_format_to_deliver,
                self.stream_format,
            )));
            output.setAlwaysDiscardsLateVideoFrames(true);
            if !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(refused(
                    "the session cannot take a video data output".into(),
                ));
            }
            session.addOutput(&output);

            // The format is set after the input joins the session, which would
            // otherwise reset it to its preset's, and the lock is held until the
            // session runs so it cannot.
            if let Err(e) = device.lockForConfiguration() {
                session.commitConfiguration();
                return Err(refused(format!(
                    "the device could not be locked to set its format: {}",
                    e.localizedDescription()
                )));
            }
            device.setActiveFormat(device_format);
            if let Some(shortest_frame_duration) = shortest_frame_duration {
                device.setActiveVideoMinFrameDuration(*shortest_frame_duration);
            }
            session.commitConfiguration();
            (session, output)
        };

        let _ = self
            .frame_delivery
            .capture_session
            .set(CaptureSessionReadForItsClock(
                objc2::rc::Weak::from_retained(&session),
            ));
        let sample_buffer_delegate =
            AvFoundationSampleBufferDelegate::delivering_into(Arc::clone(&self.frame_delivery));
        // SAFETY: a delegate the output keeps weakly and this session keeps
        // strongly, on the serial queue the delivery expects.
        unsafe {
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*sample_buffer_delegate)),
                Some(&*self.sample_buffer_queue),
            );
        }
        let notification_observers = observe_the_ways_a_session_ends(
            &session,
            device,
            &self.failure_recorder,
            &self.camera_name,
        );
        // SAFETY: starting the session configured above, then releasing the
        // configuration lock it held so the format could not be reset.
        unsafe {
            session.startRunning();
            device.unlockForConfiguration();
        }
        tracing::info!(
            camera = %self.camera_name,
            width = self.stream_format.width,
            height = self.stream_format.height,
            "AVFoundation camera: capture started"
        );
        Ok(Some(AvFoundationConfiguredCaptureSession {
            session,
            output,
            _sample_buffer_delegate: sample_buffer_delegate,
            notification_observers,
        }))
    }
}

/// The output settings that make every delivered buffer IOSurface-backed, in
/// `pixel_format`, at the stream's extent — AVFoundation scales to it when the
/// device's format does not match exactly.
fn video_settings_delivering(
    pixel_format: PixelFormat,
    stream_format: VideoCaptureStreamFormat,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: the keys are CoreVideo-exported constants, and a `CFString` is
    // toll-free bridged to `NSString`.
    let key = |cf_key: &CFString| -> &NSString { unsafe { &*(cf_key as *const CFString).cast() } };
    // SAFETY: as above.
    let (pixel_format_key, width_key, height_key, iosurface_properties_key) = unsafe {
        (
            key(kCVPixelBufferPixelFormatTypeKey),
            key(kCVPixelBufferWidthKey),
            key(kCVPixelBufferHeightKey),
            key(kCVPixelBufferIOSurfacePropertiesKey),
        )
    };
    let pixel_format = NSNumber::new_u32(pixel_format.as_cv_pixel_format_type());
    let width = NSNumber::new_u32(stream_format.width);
    let height = NSNumber::new_u32(stream_format.height);
    let no_iosurface_properties = NSDictionary::<NSString, AnyObject>::new();
    NSDictionary::from_slices(
        &[
            pixel_format_key,
            width_key,
            height_key,
            iosurface_properties_key,
        ],
        &[
            pixel_format.as_ref(),
            width.as_ref(),
            height.as_ref(),
            no_iosurface_properties.as_ref(),
        ],
    )
}

/// Record, as the failure that ended the stream, a runtime error the session
/// reports or the device disconnecting.
fn observe_the_ways_a_session_ends(
    session: &AVCaptureSession,
    device: &AVCaptureDevice,
    failure_recorder: &DeviceStreamFailureRecorder,
    camera_name: &str,
) -> Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>> {
    let notification_center = NSNotificationCenter::defaultCenter();
    let observe = |name: &NSNotificationName, object: &AnyObject, what_happened: &'static str| {
        let failure_recorder = failure_recorder.clone();
        let camera_name = camera_name.to_owned();
        let on_notification = block2::RcBlock::new(move |notification: NonNull<NSNotification>| {
            // SAFETY: NSNotificationCenter hands the block a live notification.
            let notification = unsafe { notification.as_ref() };
            let detail = notification
                .userInfo()
                .map(|user_info| user_info.description().to_string())
                .unwrap_or_default();
            tracing::error!(camera = %camera_name, %detail, "AVFoundation camera: {what_happened}");
            failure_recorder.record_the_failure_that_ended_the_stream(
                DeviceStreamFailureReason::of(format!("{what_happened} {detail}")),
            );
        });
        // SAFETY: the observer is removed before the object it watches is
        // released, when the session stops.
        unsafe {
            notification_center.addObserverForName_object_queue_usingBlock(
                Some(name),
                Some(object),
                None,
                &on_notification,
            )
        }
    };
    // SAFETY: AVFoundation-exported notification names.
    let (runtime_error, disconnected) = unsafe {
        (
            AVCaptureSessionRuntimeErrorNotification,
            AVCaptureDeviceWasDisconnectedNotification,
        )
    };
    vec![
        observe(
            runtime_error,
            session.as_ref(),
            "the capture session stopped on a runtime error",
        ),
        observe(disconnected, device.as_ref(), "the camera was disconnected"),
    ]
}

/// A session the control queue configured and started, and what it hung off
/// it. Created, stopped and released only on the device's control queue.
struct AvFoundationConfiguredCaptureSession {
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    _sample_buffer_delegate: Retained<AvFoundationSampleBufferDelegate>,
    notification_observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

// SAFETY: every AVFoundation object here is messaged only on the device's
// serial control queue; the stopping thread only moves this value there.
unsafe impl Send for AvFoundationConfiguredCaptureSession {}

impl AvFoundationConfiguredCaptureSession {
    /// Stop the session and detach everything hung off it. Runs on the
    /// stream's control queue.
    fn stop(self) {
        let notification_center = NSNotificationCenter::defaultCenter();
        for observer in &self.notification_observers {
            // SAFETY: an observer this session added, removed once.
            unsafe { notification_center.removeObserver(observer.as_ref()) };
        }
        // SAFETY: stopping a session this stream owns, then clearing its data
        // output's delegate — a nil delegate with a nil queue is the documented
        // way to stop an output calling back.
        unsafe {
            self.session.stopRunning();
            self.output.setSampleBufferDelegate_queue(None, None);
        }
    }
}

/// One delivery: the frame delivery its hand-off rides on, the queue it runs
/// on, and the session the control queue configures for it — absent until the
/// control queue has, and after it could not.
struct AvFoundationRunningDelivery {
    frame_delivery: Arc<AvFoundationFrameDelivery>,
    sample_buffer_queue: DispatchRetained<DispatchQueue>,
    configured_session: Arc<Mutex<Option<AvFoundationConfiguredCaptureSession>>>,
}

impl AvFoundationRunningDelivery {
    /// End the delivery: no hand-off follows an `Ok`. The session stops on the
    /// control queue behind any start still powering the camera up, and this
    /// does not wait for it — only, bounded, for a frame already on its way
    /// through the sample-buffer queue.
    fn stop(self, session_control_queue: &DispatchQueue, camera_name: &str) -> Result<()> {
        self.frame_delivery
            .is_delivering
            .store(false, Ordering::Release);
        let configured_session = Arc::clone(&self.configured_session);
        session_control_queue.exec_async(move || {
            if let Some(configured_session) = configured_session.lock().take() {
                configured_session.stop();
            }
        });

        let (drained, drained_by) = mpsc::sync_channel::<()>(1);
        self.sample_buffer_queue.exec_async(move || {
            let _ = drained.send(());
        });
        drained_by
            .recv_timeout(DELIVERY_DRAIN_TIMEOUT)
            .map_err(|_| {
                Error::Runtime(format!(
                    "AVFoundation camera {camera_name}: a frame still on its way through the \
                     sample-buffer queue did not finish within {}s",
                    DELIVERY_DRAIN_TIMEOUT.as_secs()
                ))
            })
    }
}

/// The running session, held weakly by the frame delivery its output calls
/// into — the session holds the output, which holds the delegate, which holds
/// the delivery.
struct CaptureSessionReadForItsClock(objc2::rc::Weak<AVCaptureSession>);

// SAFETY: loaded only on the session's own sample-buffer queue, and only to
// read its synchronization clock — a read-only property AVFoundation serves
// from any thread.
unsafe impl Send for CaptureSessionReadForItsClock {}
// SAFETY: as above.
unsafe impl Sync for CaptureSessionReadForItsClock {}

/// What the sample-buffer delegate delivers each frame through.
struct AvFoundationFrameDelivery {
    hand_off: CapturedVideoFrameHandOff,
    is_delivering: AtomicBool,
    capture_session: OnceLock<CaptureSessionReadForItsClock>,
    synchronization_clock: OnceLock<CFRetained<CMClock>>,
    camera_name: String,
    stream_format: VideoCaptureStreamFormat,
    pixel_format_delivered: PixelFormat,
    gpu_context: GpuContextLimitedAccess,
    capture_instant_resolver: Arc<VideoCaptureInstantResolver>,
    capture_progress: Mutex<AvFoundationCaptureProgress>,
}

impl AvFoundationFrameDelivery {
    /// Land one sample buffer in a pooled pixel buffer and hand it off. Runs
    /// on the session's serial sample-buffer queue.
    fn deliver(&self, sample_buffer: &CMSampleBuffer) {
        let dequeued_at_ns = MediaClock::now().as_nanos() as i64;
        if !self.is_delivering.load(Ordering::Acquire) {
            return;
        }
        // SAFETY: a live sample buffer the output handed the delegate.
        let Some(pixel_buffer) = (unsafe { sample_buffer.image_buffer() }) else {
            return;
        };
        let capture_timestamp_ns = self.capture_instant_resolver.resolve_capture_timestamp_ns(
            self.device_capture_stamp_of(sample_buffer),
            dequeued_at_ns,
        );
        let color = core_video_pixel_buffer_color_to_h273_color_vui(&pixel_buffer);

        let mut capture_progress = self.capture_progress.lock();
        let (published_pixel_buffer_frame_id, pooled_pixel_buffer) =
            match capture_progress.convert_into_pooled_pixel_buffer(self, &pixel_buffer, &color) {
                Ok(converted) => converted,
                Err(frame_error) => {
                    capture_progress
                        .delivery_tally
                        .record_a_dropped_frame(&self.camera_name, &frame_error);
                    return;
                }
            };

        // A stop that arrived during this frame's GPU work ends delivery here.
        if !self.is_delivering.load(Ordering::Acquire) {
            return;
        }
        (self.hand_off)(CapturedVideoFrameFromDevice {
            published_pixel_buffer_frame_id: &published_pixel_buffer_frame_id,
            width: self.stream_format.width,
            height: self.stream_format.height,
            color,
            capture_timestamp_ns,
        });
        // The pool reclaims the slot when this drops, after the hand-off has
        // written the frame out.
        drop(pooled_pixel_buffer);

        if capture_progress.delivery_tally.record_a_delivered_frame() == 1 {
            tracing::info!(
                camera = %self.camera_name,
                transport = capture_progress
                    .conversion
                    .as_ref()
                    .map(Biplanar420IOSurfaceToPooledRgbaConversion::describe_transport),
                width = self.stream_format.width,
                height = self.stream_format.height,
                pixel_format = ?self.pixel_format_delivered,
                "first frame captured via GPU compute"
            );
        }
    }

    fn running_sessions_synchronization_clock(&self) -> Option<&CMClock> {
        if let Some(synchronization_clock) = self.synchronization_clock.get() {
            return Some(synchronization_clock);
        }
        let capture_session = self.capture_session.get()?.0.load()?;
        // `synchronizationClock` arrived in macOS 12.3; asking an older session
        // for it would raise.
        if !capture_session.respondsToSelector(sel!(synchronizationClock)) {
            return None;
        }
        // SAFETY: the selector exists, checked above; the session has no clock
        // until it runs, and then `None` here is retried on the next frame.
        let synchronization_clock = unsafe { capture_session.synchronizationClock() }?;
        Some(
            self.synchronization_clock
                .get_or_init(|| CFRetained::from(&*synchronization_clock)),
        )
    }

    /// What the sample buffer says about when its frame was captured: its
    /// presentation stamp on the session's clock, converted to host time — the
    /// clock `mach_absolute_time` reads.
    fn device_capture_stamp_of(
        &self,
        sample_buffer: &CMSampleBuffer,
    ) -> DeviceReportedCaptureStamp {
        let Some(synchronization_clock) = self.running_sessions_synchronization_clock() else {
            return DeviceReportedCaptureStamp::OffTheMachineMonotonicClock;
        };
        // SAFETY: a live sample buffer; both clocks are live CMClocks.
        let host_time = unsafe {
            let presentation_time = sample_buffer.presentation_time_stamp();
            if !presentation_time.flags.contains(CMTimeFlags::Valid) {
                return DeviceReportedCaptureStamp::OffTheMachineMonotonicClock;
            }
            CMSyncConvertTime(
                presentation_time,
                synchronization_clock,
                &CMClock::host_time_clock(),
            )
        };
        if !host_time.flags.contains(CMTimeFlags::Valid) {
            return DeviceReportedCaptureStamp::OffTheMachineMonotonicClock;
        }
        // SAFETY: a valid time on the host clock, which is what the conversion
        // to mach ticks takes.
        let raw_host_ticks = unsafe { CMClock::convert_host_time_to_system_units(host_time) };
        DeviceReportedCaptureStamp::OnTheMachineMonotonicClock {
            capture_timestamp_ns: MediaClock::nanos_from_raw_timestamp(raw_host_ticks).as_nanos()
                as i64,
        }
    }
}

/// A stream's per-frame state, touched only from its sample-buffer queue.
#[derive(Default)]
struct AvFoundationCaptureProgress {
    /// Created on the stream's first frame.
    conversion: Option<Biplanar420IOSurfaceToPooledRgbaConversion>,
    delivery_tally: CapturedVideoFrameDeliveryTally,
}

impl AvFoundationCaptureProgress {
    fn convert_into_pooled_pixel_buffer(
        &mut self,
        delivery: &AvFoundationFrameDelivery,
        pixel_buffer: &CVPixelBuffer,
        color: &H273ColorVui,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        let VideoCaptureStreamFormat { width, height, .. } = delivery.stream_format;
        let delivered_extent = (
            CVPixelBufferGetWidth(pixel_buffer),
            CVPixelBufferGetHeight(pixel_buffer),
        );
        if delivered_extent != (width as usize, height as usize) {
            return Err(Error::Runtime(format!(
                "frame delivered at {}x{}, the stream negotiated {width}x{height}",
                delivered_extent.0, delivered_extent.1
            )));
        }
        let iosurface = CVPixelBufferGetIOSurface(Some(pixel_buffer)).ok_or_else(|| {
            Error::Runtime("AVFoundation delivered a pixel buffer with no IOSurface".into())
        })?;

        let conversion = match &mut self.conversion {
            Some(conversion) => conversion,
            None => self
                .conversion
                .insert(Biplanar420IOSurfaceToPooledRgbaConversion::create(
                    &delivery.gpu_context,
                    &delivery.camera_name,
                    delivery.pixel_format_delivered,
                    width,
                    height,
                )?),
        };
        conversion.convert_into_pooled_pixel_buffer(
            &delivery.gpu_context,
            &iosurface,
            &color.resolve_defaults(ColorSpaceKind::Yuv),
        )
    }
}

define_class!(
    /// The data output's delegate: hands every sample buffer to the stream's
    /// frame delivery.
    #[unsafe(super(NSObject))]
    #[name = "StreamlibAvFoundationSampleBufferDelegate"]
    #[ivars = Arc<AvFoundationFrameDelivery>]
    struct AvFoundationSampleBufferDelegate;

    unsafe impl NSObjectProtocol for AvFoundationSampleBufferDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for AvFoundationSampleBufferDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn capture_output_did_output_sample_buffer(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.ivars().deliver(sample_buffer);
        }
    }
);

impl AvFoundationSampleBufferDelegate {
    fn delivering_into(frame_delivery: Arc<AvFoundationFrameDelivery>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(frame_delivery);
        // SAFETY: `init` is `NSObject`'s designated initializer; the ivars are
        // set above.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_format(
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        highest_frames_per_second: f64,
    ) -> AvFoundationFormatCandidate {
        AvFoundationFormatCandidate {
            width,
            height,
            pixel_format,
            highest_frames_per_second,
            shortest_frame_duration: None,
        }
    }

    #[test]
    fn the_largest_format_within_the_cap_is_chosen() {
        let formats = [
            a_format(640, 480, PixelFormat::Nv12VideoRange, 30.0),
            a_format(1920, 1080, PixelFormat::Nv12VideoRange, 30.0),
            a_format(3840, 2160, PixelFormat::Nv12VideoRange, 30.0),
            a_format(1280, 720, PixelFormat::Nv12VideoRange, 60.0),
        ];
        assert_eq!(
            index_of_the_format_to_capture_in(&formats, 1920, 1080),
            Some(1)
        );
    }

    #[test]
    fn at_one_extent_the_faster_format_wins_and_then_video_range_biplanar() {
        let formats = [
            a_format(1280, 720, PixelFormat::Bgra32, 30.0),
            a_format(1280, 720, PixelFormat::Nv12FullRange, 30.0),
            a_format(1280, 720, PixelFormat::Nv12VideoRange, 30.0),
            a_format(1280, 720, PixelFormat::Yuyv422, 60.0),
        ];
        assert_eq!(
            index_of_the_format_to_capture_in(&formats, 1920, 1080),
            Some(3)
        );
        assert_eq!(
            index_of_the_format_to_capture_in(&formats[..3], 1920, 1080),
            Some(2)
        );
    }

    #[test]
    fn a_device_with_nothing_within_the_cap_captures_in_its_smallest_format() {
        let formats = [
            a_format(3840, 2160, PixelFormat::Nv12VideoRange, 30.0),
            a_format(1920, 1080, PixelFormat::Nv12VideoRange, 30.0),
        ];
        assert_eq!(
            index_of_the_format_to_capture_in(&formats, 640, 480),
            Some(1)
        );
    }

    #[test]
    fn a_device_that_offers_no_format_has_nothing_to_capture_in() {
        assert_eq!(index_of_the_format_to_capture_in(&[], 1920, 1080), None);
    }

    #[test]
    fn biplanar_formats_are_delivered_as_they_are_and_anything_else_as_video_range() {
        assert_eq!(
            pixel_format_to_deliver_for(PixelFormat::Nv12FullRange),
            PixelFormat::Nv12FullRange
        );
        assert_eq!(
            pixel_format_to_deliver_for(PixelFormat::Nv12VideoRange),
            PixelFormat::Nv12VideoRange
        );
        assert_eq!(
            pixel_format_to_deliver_for(PixelFormat::Bgra32),
            PixelFormat::Nv12VideoRange
        );
    }

    #[test]
    fn listing_cameras_succeeds_with_or_without_one_attached() {
        let devices = AvFoundationVideoDeviceBackend
            .list_capture_devices()
            .expect("enumeration must not error");
        let unique: std::collections::HashSet<&str> =
            devices.iter().map(|device| device.id.as_str()).collect();
        assert_eq!(
            unique.len(),
            devices.len(),
            "a camera is listed once: {devices:?}"
        );
    }
}
