// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! AVFoundation capture with no CPU copy: each IOSurface the capture session
//! delivers is adopted once through the module host table and converted on the
//! GPU into the node's output texture, on the session's own sample-buffer queue.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceDiscoverySession,
    AVCaptureDeviceFormat, AVCaptureDeviceInput, AVCaptureDevicePosition,
    AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{
    CMClock, CMFormatDescription, CMSampleBuffer, CMSyncConvertTime, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionGetDimensions,
};
use objc2_core_video::{
    CVPixelBufferGetHeight, CVPixelBufferGetIOSurface, CVPixelBufferGetWidth,
    CVPixelFormatDescriptionArrayCreateWithAllPixelFormatTypes, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use objc2_io_surface::{IOSurfaceID, IOSurfaceRef};
use tatolab_module_sdk::{
    ModuleHost, ModuleHostColorInfo, ModuleHostForeignIOSurface, ModuleHostLogLevel,
    ModuleHostNodeOutputTextureRequest, ModuleHostOutputPort, ModuleHostSurface,
};

use crate::camera_frame_publication::{
    CAMERA_LOG_TARGET, CONVERTED_FRAME_POOL_KEY, CONVERTED_FRAME_TEXTURE_FORMAT,
    CONVERTED_FRAME_TEXTURE_USAGE, CameraCaptureRefusal, CaptureLoopReportingWindow,
    FRAMES_BETWEEN_REPORTS,
};
use crate::capture_timestamp::{monotonic_now_ns, resolve_a_monotonic_device_stamp};
use crate::captured_frame_colour_description::{
    CapturedFrameColourDescription, matrix_name_or_the_players_fallback,
};
use crate::core_video_colour_description::colour_description_of_core_video_pixel_buffer;
use crate::video_frame_record::PublishedVideoFrame;

/// `'420v'`: biplanar 4:2:0 8-bit, video range.
const CORE_VIDEO_NV12_VIDEO_RANGE: u32 = u32::from_be_bytes(*b"420v");
/// `'420f'`: biplanar 4:2:0 8-bit, full range.
const CORE_VIDEO_NV12_FULL_RANGE: u32 = u32::from_be_bytes(*b"420f");

/// How many distinct IOSurfaces stay adopted. AVFoundation recycles a handful
/// from its own pool, so a capture past this is churning surfaces and starts
/// over rather than growing.
const MOST_ADOPTED_IOSURFACES_KEPT: usize = 16;

/// The capture-device access policy the host states under this key in its
/// capabilities.
const CAPTURE_DEVICE_ACCESS_PROMPTING_CAPABILITY: &str = "capture_device_access_prompting";
/// The policy value that lets this node ask the user for camera access.
const CAPTURE_DEVICE_ACCESS_MAY_ASK_THE_USER: &str = "may_ask_the_user";

/// Every video capture device AVFoundation lists, as `(unique id, name)`:
/// built-in cameras first, then external ones.
pub fn list_avfoundation_capture_devices() -> Vec<(String, String)> {
    avfoundation_capture_devices_in_default_order()
        .iter()
        .map(|device| unique_id_and_name_of(device))
        .collect()
}

/// Built-in cameras first, then external ones — USB cameras and camera
/// extensions alike. Not `defaultDeviceWithMediaType`, which can answer with an
/// idle camera extension over the built-in camera.
fn avfoundation_capture_devices_in_default_order() -> Vec<Retained<AVCaptureDevice>> {
    // SAFETY: `AVMediaTypeVideo` is an AVFoundation-exported constant.
    let Some(video) = (unsafe { AVMediaTypeVideo }) else {
        return Vec::new();
    };
    // The deprecated synonym rather than `AVCaptureDeviceTypeExternal`, which
    // exists only from macOS 14.
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

fn unique_id_and_name_of(device: &AVCaptureDevice) -> (String, String) {
    // SAFETY: property reads on a live device.
    unsafe {
        (
            device.uniqueID().to_string(),
            device.localizedName().to_string(),
        )
    }
}

/// The device `device_id` names — an AVFoundation `uniqueID` — or the first
/// camera listed. A named device that is not attached is refused naming the
/// ones that are, never replaced by a different one.
fn open_requested_avfoundation_device(
    device_id: Option<&str>,
) -> Result<Retained<AVCaptureDevice>, CameraCaptureRefusal> {
    let devices = avfoundation_capture_devices_in_default_order();
    match device_id {
        None => devices.into_iter().next().ok_or_else(|| {
            CameraCaptureRefusal(
                "no camera found: AVFoundation lists no video capture device".to_string(),
            )
        }),
        Some(device_id) => devices
            .iter()
            // SAFETY: a property read on a live device.
            .find(|device| unsafe { device.uniqueID() }.to_string() == device_id)
            .cloned()
            .ok_or_else(|| {
                let attached: Vec<String> = devices
                    .iter()
                    .map(|device| {
                        let (id, name) = unique_id_and_name_of(device);
                        format!("{name} ({id})")
                    })
                    .collect();
                CameraCaptureRefusal(format!(
                    "camera {device_id:?} is not attached; attached: [{}]",
                    attached.join(", ")
                ))
            }),
    }
}

/// One of a device's formats, as the negotiation weighs it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AvFoundationFormatCandidate {
    width: u32,
    height: u32,
    core_video_pixel_format: u32,
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
    let biplanar_preference = |core_video_pixel_format: u32| match core_video_pixel_format {
        CORE_VIDEO_NV12_VIDEO_RANGE => 2,
        CORE_VIDEO_NV12_FULL_RANGE => 1,
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
                .then(
                    biplanar_preference(a.core_video_pixel_format)
                        .cmp(&biplanar_preference(b.core_video_pixel_format)),
                )
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

/// The pixel format a capture asks AVFoundation to deliver in: the device's own
/// when it is biplanar 4:2:0, else `420v`, which AVFoundation converts into.
fn core_video_pixel_format_to_deliver_for(device_core_video_pixel_format: u32) -> u32 {
    match device_core_video_pixel_format {
        CORE_VIDEO_NV12_FULL_RANGE => CORE_VIDEO_NV12_FULL_RANGE,
        _ => CORE_VIDEO_NV12_VIDEO_RANGE,
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
            core_video_pixel_format: description.media_sub_type(),
            highest_frames_per_second: fastest_range
                .as_ref()
                .map_or(0.0, |range| range.maxFrameRate()),
            shortest_frame_duration: fastest_range.map(|range| range.minFrameDuration()),
        }
    }
}

/// What the device negotiated, fixed for the life of the capture.
#[derive(Debug, Clone)]
pub struct NegotiatedAvFoundationCaptureFormat {
    pub device_id: String,
    pub device_name: String,
    /// The CoreVideo pixel format every delivered buffer is in.
    pub core_video_pixel_format: u32,
    pub width: u32,
    pub height: u32,
    pub nominal_frames_per_second: Option<u32>,
}

impl NegotiatedAvFoundationCaptureFormat {
    /// The engine's pixel format wire name for what the session delivers.
    pub fn pixel_format_wire_name(&self) -> &'static str {
        match self.core_video_pixel_format {
            CORE_VIDEO_NV12_FULL_RANGE => "nv12_full_range",
            _ => "nv12_video_range",
        }
    }

    /// The delivered format's CoreVideo fourcc, as text.
    pub fn fourcc_name(&self) -> &'static str {
        match self.core_video_pixel_format {
            CORE_VIDEO_NV12_FULL_RANGE => "420f",
            _ => "420v",
        }
    }
}

/// How the host lets this node treat a camera nobody has answered for yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraAccessPrompting {
    /// Someone loaded or started the stream, so macOS may ask the user.
    MayAskTheUser,
    /// The runtime re-loaded the stream on its own, so nobody is asked out of
    /// nowhere.
    RefuseWhatWasNeverAskedFor,
}

impl CameraAccessPrompting {
    /// The policy the host's capabilities state; one that states none, or one
    /// this module does not know, never asks.
    fn stated_by(module_host: &ModuleHost) -> Self {
        let stated =
            serde_json::from_str::<serde_json::Value>(module_host.host_capabilities_json())
                .ok()
                .and_then(|capabilities| {
                    capabilities
                        .get(CAPTURE_DEVICE_ACCESS_PROMPTING_CAPABILITY)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
        match stated.as_deref() {
            Some(CAPTURE_DEVICE_ACCESS_MAY_ASK_THE_USER) => Self::MayAskTheUser,
            _ => Self::RefuseWhatWasNeverAskedFor,
        }
    }
}

/// Where camera access stands for one capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraAccess {
    Granted,
    /// macOS is asking the user; a started capture's session starts once they
    /// allow it.
    AwaitingTheUsersAnswer,
}

/// Why camera access refuses this capture, naming what to change.
fn camera_access_refusal(status: AVAuthorizationStatus) -> CameraCaptureRefusal {
    let launched_by =
        "the application that launched the runtime, such as your terminal, never Python";
    CameraCaptureRefusal(match status {
        AVAuthorizationStatus::Denied => format!(
            "Camera access is denied for {launched_by}. Turn it on in System Settings › Privacy \
             & Security › Camera and run again. An application macOS never lets ask, such as a \
             command-line tool built without the camera entitlement, is denied with no prompt \
             and appears in no list: launch the runtime from Terminal or another application \
             that can ask."
        ),
        AVAuthorizationStatus::NotDetermined => format!(
            "Camera access has not been asked for yet. The runtime re-loaded this stream on its \
             own as it started, and asks for the camera only when you load or start a stream \
             yourself: restart this stream, or reload its project, and macOS will ask whether \
             {launched_by} may use the camera."
        ),
        _ => format!(
            "Camera access is restricted on this Mac — a device-management profile or Screen \
             Time forbids it — so {launched_by} cannot be allowed the camera here."
        ),
    })
}

/// The camera a capture opened, in the format it negotiated. Messaged only on
/// the capture's serial control queue; elsewhere it is only retained and
/// released.
struct CaptureDeviceConfiguredOnItsControlQueue {
    device: Retained<AVCaptureDevice>,
    device_format: Retained<AVCaptureDeviceFormat>,
    shortest_frame_duration: Option<CMTime>,
}

// SAFETY: AVFoundation's capture devices and formats may be configured from any
// thread, one caller at a time; every message this wrapper's objects receive is
// sent from the capture's serial control queue, which is that one caller.
unsafe impl Send for CaptureDeviceConfiguredOnItsControlQueue {}

/// A session the control queue configured and started, and what hangs off it.
/// Created, stopped and released only on the capture's control queue.
struct AvFoundationRunningCaptureSession {
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    _sample_buffer_delegate: Retained<TatolabCameraSampleBufferDelegate>,
}

// SAFETY: every AVFoundation object here is messaged only on the capture's
// serial control queue; other threads only move this value there.
unsafe impl Send for AvFoundationRunningCaptureSession {}

/// Everything that starts and stops the capture's session, behind one lock:
/// the node's start and stop and the user's permission answer all reach it,
/// from different threads.
struct AvFoundationCaptureSessionControl {
    capture_device: CaptureDeviceConfiguredOnItsControlQueue,
    frame_delivery: Arc<AvFoundationFrameDelivery>,
    session_control_queue: DispatchRetained<DispatchQueue>,
    sample_buffer_queue: DispatchRetained<DispatchQueue>,
    running_session: Option<AvFoundationRunningCaptureSession>,
    camera_access: CameraAccess,
    start_requested: bool,
    stopped: bool,
}

impl AvFoundationCaptureSessionControl {
    /// Configure a session on the device and format, point its output at the
    /// frame delivery and start it, on the control queue, returning once it
    /// runs. `startRunning` may take seconds while the camera powers up.
    fn configure_and_start_the_session(&mut self) -> Result<(), CameraCaptureRefusal> {
        if self.stopped
            || !self.start_requested
            || self.camera_access != CameraAccess::Granted
            || self.running_session.is_some()
        {
            return Ok(());
        }
        let session_request = SendToTheControlQueue(AvFoundationCaptureSessionRequest {
            capture_device: &self.capture_device,
            frame_delivery: &self.frame_delivery,
            sample_buffer_queue: &self.sample_buffer_queue,
        });
        let mut started = None;
        self.session_control_queue.exec_sync(|| {
            started = Some(session_request.into_value().configure_and_start());
        });
        let running_session = started.unwrap_or_else(|| {
            Err(CameraCaptureRefusal(
                "the camera's control queue never ran the session's start".to_string(),
            ))
        })?;
        self.running_session = Some(running_session);
        Ok(())
    }

    /// Stop delivering, stop the session on the control queue, and wait for a
    /// frame already on the sample-buffer queue to finish. Idempotent.
    fn stop(&mut self) {
        self.stopped = true;
        self.frame_delivery
            .is_delivering
            .store(false, Ordering::Release);
        if let Some(running_session) = self.running_session.take() {
            let running_session = SendToTheControlQueue(running_session);
            self.session_control_queue.exec_sync(move || {
                let running_session = running_session.into_value();
                // SAFETY: stopping a session this capture owns, then clearing
                // its data output's delegate — a nil delegate with a nil queue
                // is the documented way to stop an output calling back.
                unsafe {
                    running_session.session.stopRunning();
                    running_session
                        .output
                        .setSampleBufferDelegate_queue(None, None);
                }
            });
        }
        self.sample_buffer_queue.exec_sync(|| {});
        self.frame_delivery.give_every_adopted_surface_back();
    }
}

/// A value moved to the control queue, where it is used and dropped.
struct SendToTheControlQueue<Value>(Value);

// SAFETY: the value is used only on the serial control queue it is moved to.
unsafe impl<Value> Send for SendToTheControlQueue<Value> {}

impl<Value> SendToTheControlQueue<Value> {
    /// The value, taken out on the control queue. A method rather than a
    /// pattern, so a closure captures the whole wrapper and not its field.
    fn into_value(self) -> Value {
        self.0
    }
}

/// What the control queue needs to configure and start one session.
struct AvFoundationCaptureSessionRequest<'control> {
    capture_device: &'control CaptureDeviceConfiguredOnItsControlQueue,
    frame_delivery: &'control Arc<AvFoundationFrameDelivery>,
    sample_buffer_queue: &'control DispatchRetained<DispatchQueue>,
}

impl AvFoundationCaptureSessionRequest<'_> {
    fn configure_and_start(
        &self,
    ) -> Result<AvFoundationRunningCaptureSession, CameraCaptureRefusal> {
        let format = &self.frame_delivery.format;
        let refused = |what_failed: String| {
            CameraCaptureRefusal(format!("camera {:?}: {what_failed}", format.device_name))
        };
        let CaptureDeviceConfiguredOnItsControlQueue {
            device,
            device_format,
            shortest_frame_duration,
        } = self.capture_device;

        ensure_core_video_pixel_format_dictionary_is_built();
        // SAFETY: AVFoundation's documented configuration sequence, on objects
        // this capture owns, on its serial control queue.
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
            output.setVideoSettings(Some(&video_settings_delivering(format)));
            output.setAlwaysDiscardsLateVideoFrames(true);
            if !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(refused(
                    "the session cannot take a video data output".into(),
                ));
            }
            session.addOutput(&output);

            // The format is set after the input joins the session, which would
            // otherwise reset it to its preset's, and the lock is held until
            // the session runs so it cannot.
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

        let _ =
            self.frame_delivery
                .capture_session_for_its_clock
                .set(CaptureSessionReadForItsClock(
                    objc2::rc::Weak::from_retained(&session),
                ));
        let sample_buffer_delegate =
            TatolabCameraSampleBufferDelegate::delivering_into(Arc::clone(self.frame_delivery));
        // SAFETY: a delegate the output keeps weakly and the running session
        // keeps strongly, on the serial queue the delivery expects.
        unsafe {
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*sample_buffer_delegate)),
                Some(self.sample_buffer_queue),
            );
        }
        // SAFETY: starting the session configured above, then releasing the
        // configuration lock it held so the format could not be reset.
        unsafe {
            session.startRunning();
            device.unlockForConfiguration();
        }
        Ok(AvFoundationRunningCaptureSession {
            session,
            output,
            _sample_buffer_delegate: sample_buffer_delegate,
        })
    }
}

/// Build CoreVideo's pixel-format dictionary once, before any session asks
/// for a format: CoreVideo builds it lazily and unguarded, and two threads
/// building it at once crash.
fn ensure_core_video_pixel_format_dictionary_is_built() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let _every_pixel_format_description =
            CVPixelFormatDescriptionArrayCreateWithAllPixelFormatTypes(None);
    });
}

/// The output settings that make every delivered buffer IOSurface-backed, in
/// the negotiated pixel format, at the negotiated extent.
fn video_settings_delivering(
    format: &NegotiatedAvFoundationCaptureFormat,
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
    let pixel_format = NSNumber::new_u32(format.core_video_pixel_format);
    let width = NSNumber::new_u32(format.width);
    let height = NSNumber::new_u32(format.height);
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

/// An IOSurface the session delivered and the surface its adoption made.
struct IOSurfaceAdoptedAsSurface {
    adopted_surface: ModuleHostSurface,
    /// Held so the id the adoption is keyed by names this surface while the
    /// adoption does.
    _iosurface: CFRetained<IOSurfaceRef>,
}

// SAFETY: the retained IOSurface is only kept alive here, never messaged, and
// IOSurface references may be released from any thread.
unsafe impl Send for IOSurfaceAdoptedAsSurface {}

/// What changes per frame, touched only from the sample-buffer queue and the
/// stop that drained it.
#[derive(Default)]
struct AvFoundationCaptureProgress {
    adopted_by_iosurface_id: HashMap<IOSurfaceID, IOSurfaceAdoptedAsSurface>,
    reporting_window: CaptureLoopReportingWindow,
    frames_published_in_total: u64,
    first_refusal_logged: bool,
}

/// What the sample-buffer delegate delivers each frame through.
struct AvFoundationFrameDelivery {
    module_host: ModuleHost,
    video_output_port: ModuleHostOutputPort,
    format: NegotiatedAvFoundationCaptureFormat,
    output_pool_rotation_depth: u32,
    is_delivering: AtomicBool,
    capture_session_for_its_clock: OnceLock<CaptureSessionReadForItsClock>,
    synchronization_clock: OnceLock<SynchronizationClockOfTheSession>,
    capture_progress: Mutex<AvFoundationCaptureProgress>,
}

/// The running session's synchronization clock, kept once read.
struct SynchronizationClockOfTheSession(CFRetained<CMClock>);

// SAFETY: a CMClock is a thread-safe CoreFoundation object; it is only read.
unsafe impl Send for SynchronizationClockOfTheSession {}
// SAFETY: as above.
unsafe impl Sync for SynchronizationClockOfTheSession {}

impl AvFoundationFrameDelivery {
    /// Adopt one sample buffer's IOSurface, convert it into a texture from the
    /// node's output pool and publish it. Runs on the session's serial
    /// sample-buffer queue.
    fn deliver(&self, sample_buffer: &CMSampleBuffer) {
        let dequeue_instant_ns = monotonic_now_ns();
        if !self.is_delivering.load(Ordering::Acquire) || self.module_host.stop_requested() {
            return;
        }
        let timestamp = resolve_a_monotonic_device_stamp(
            self.device_capture_stamp_ns_of(sample_buffer),
            dequeue_instant_ns,
        );
        let Ok(mut capture_progress) = self.capture_progress.lock() else {
            return;
        };
        match self.publish_one_frame(
            &mut capture_progress,
            sample_buffer,
            timestamp.timestamp_ns(),
        ) {
            Ok(()) => {
                capture_progress.reporting_window.frames_published += 1;
                capture_progress.frames_published_in_total += 1;
                capture_progress
                    .reporting_window
                    .capture_to_publish_ns
                    .push(monotonic_now_ns() - timestamp.timestamp_ns());
                if capture_progress.frames_published_in_total == 1 {
                    self.module_host.log(
                        ModuleHostLogLevel::Info,
                        CAMERA_LOG_TARGET,
                        &format!("first frame published (timestamp {timestamp:?})"),
                    );
                }
            }
            Err(refusal) => {
                capture_progress.reporting_window.frames_dropped += 1;
                if !capture_progress.first_refusal_logged {
                    capture_progress.first_refusal_logged = true;
                    self.module_host.log(
                        ModuleHostLogLevel::Warn,
                        CAMERA_LOG_TARGET,
                        &format!("dropping a frame: {refusal}"),
                    );
                }
            }
        }
        let reporting_window = &mut capture_progress.reporting_window;
        if reporting_window.frames_published + reporting_window.frames_dropped
            >= FRAMES_BETWEEN_REPORTS
        {
            self.module_host.log(
                ModuleHostLogLevel::Info,
                CAMERA_LOG_TARGET,
                &reporting_window.summary(),
            );
            *reporting_window = CaptureLoopReportingWindow::default();
        }
    }

    fn publish_one_frame(
        &self,
        capture_progress: &mut AvFoundationCaptureProgress,
        sample_buffer: &CMSampleBuffer,
        timestamp_ns: i64,
    ) -> Result<(), String> {
        // SAFETY: a live sample buffer the output handed the delegate.
        let pixel_buffer =
            unsafe { sample_buffer.image_buffer() }.ok_or("the sample buffer carries no image")?;
        let delivered_extent = (
            CVPixelBufferGetWidth(&pixel_buffer),
            CVPixelBufferGetHeight(&pixel_buffer),
        );
        if delivered_extent != (self.format.width as usize, self.format.height as usize) {
            return Err(format!(
                "a frame arrived at {}x{}, and the capture negotiated {}x{}",
                delivered_extent.0, delivered_extent.1, self.format.width, self.format.height
            ));
        }
        let iosurface = CVPixelBufferGetIOSurface(Some(&pixel_buffer))
            .ok_or("AVFoundation delivered a pixel buffer with no IOSurface")?;
        let colour = colour_description_of_core_video_pixel_buffer(&pixel_buffer);
        let adopted_surface = self.adopted_surface_of(capture_progress, iosurface)?;
        let converted_frame_texture = self
            .module_host
            .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
                pool_key: CONVERTED_FRAME_POOL_KEY,
                rotation_depth: self.output_pool_rotation_depth,
                width: self.format.width,
                height: self.format.height,
                texture_format: CONVERTED_FRAME_TEXTURE_FORMAT,
                usage: &CONVERTED_FRAME_TEXTURE_USAGE,
            })
            .map_err(|refusal| refusal.to_string())?;
        let conversion_colour = CapturedFrameColourDescription {
            matrix: Some(matrix_name_or_the_players_fallback(
                colour.matrix,
                self.format.height,
            )),
            ..colour
        };
        self.module_host
            .convert_surface_into(
                adopted_surface,
                &converted_frame_texture,
                &ModuleHostColorInfo {
                    primaries: conversion_colour.primaries,
                    transfer: conversion_colour.transfer,
                    matrix: conversion_colour.matrix,
                    range: conversion_colour.range,
                },
            )
            .map_err(|refusal| refusal.to_string())?;
        if !self.is_delivering.load(Ordering::Acquire) {
            return Ok(());
        }
        let record = PublishedVideoFrame {
            surface_id: converted_frame_texture.surface_id(),
            width: self.format.width,
            height: self.format.height,
            timestamp_ns,
            nominal_frames_per_second: self.format.nominal_frames_per_second,
            colour,
        }
        .to_msgpack();
        self.video_output_port
            .write(&record, Some(timestamp_ns), true)
            .map_err(|refusal| refusal.to_string())
    }

    /// The surface `iosurface` was adopted as, adopting it on the first frame
    /// it carries. An adoption past the bound starts the set over.
    fn adopted_surface_of<'progress>(
        &self,
        capture_progress: &'progress mut AvFoundationCaptureProgress,
        iosurface: CFRetained<IOSurfaceRef>,
    ) -> Result<&'progress ModuleHostSurface, String> {
        let iosurface_id = iosurface.id();
        if !capture_progress
            .adopted_by_iosurface_id
            .contains_key(&iosurface_id)
        {
            let adopted_surface = self
                .module_host
                .import_iosurface(&ModuleHostForeignIOSurface {
                    iosurface: CFRetained::as_ptr(&iosurface).as_ptr().cast_const().cast(),
                    pixel_format: self.format.pixel_format_wire_name(),
                    width: self.format.width,
                    height: self.format.height,
                })
                .map_err(|refusal| {
                    format!("the runtime could not adopt the IOSurface: {refusal}")
                })?;
            if capture_progress.adopted_by_iosurface_id.len() >= MOST_ADOPTED_IOSURFACES_KEPT {
                capture_progress.adopted_by_iosurface_id.clear();
            }
            capture_progress.adopted_by_iosurface_id.insert(
                iosurface_id,
                IOSurfaceAdoptedAsSurface {
                    adopted_surface,
                    _iosurface: iosurface,
                },
            );
        }
        capture_progress
            .adopted_by_iosurface_id
            .get(&iosurface_id)
            .map(|adopted| &adopted.adopted_surface)
            .ok_or_else(|| format!("IOSurface {iosurface_id} is missing from the adopted set"))
    }

    /// Give back every adoption, after the sample-buffer queue drained.
    fn give_every_adopted_surface_back(&self) {
        if let Ok(mut capture_progress) = self.capture_progress.lock() {
            capture_progress.adopted_by_iosurface_id.clear();
        }
    }

    fn running_sessions_synchronization_clock(&self) -> Option<&CMClock> {
        if let Some(synchronization_clock) = self.synchronization_clock.get() {
            return Some(&synchronization_clock.0);
        }
        let capture_session = self.capture_session_for_its_clock.get()?.0.load()?;
        // `synchronizationClock` arrived in macOS 12.3; asking an older session
        // for it would raise.
        if !capture_session.respondsToSelector(sel!(synchronizationClock)) {
            return None;
        }
        // SAFETY: the selector exists, checked above; the session has no clock
        // until it runs, and then `None` here is retried on the next frame.
        let synchronization_clock = unsafe { capture_session.synchronizationClock() }?;
        Some(
            &self
                .synchronization_clock
                .get_or_init(|| {
                    SynchronizationClockOfTheSession(CFRetained::from(&*synchronization_clock))
                })
                .0,
        )
    }

    /// The sample buffer's presentation stamp on the session's clock,
    /// converted to host time — the clock `mach_absolute_time` reads — in
    /// nanoseconds; `None` when it carries none that converts.
    fn device_capture_stamp_ns_of(&self, sample_buffer: &CMSampleBuffer) -> Option<i64> {
        let synchronization_clock = self.running_sessions_synchronization_clock()?;
        // SAFETY: a live sample buffer; both clocks are live CMClocks.
        let host_time = unsafe {
            let presentation_time = sample_buffer.presentation_time_stamp();
            if !presentation_time.flags.contains(CMTimeFlags::Valid) {
                return None;
            }
            CMSyncConvertTime(
                presentation_time,
                synchronization_clock,
                &CMClock::host_time_clock(),
            )
        };
        nanoseconds_of_a_host_time(host_time)
    }
}

/// A valid host-clock time in nanoseconds. The host clock's times are
/// `mach_absolute_time` scaled to seconds, so this is the instant
/// `CLOCK_UPTIME_RAW` reads.
fn nanoseconds_of_a_host_time(host_time: CMTime) -> Option<i64> {
    if !host_time.flags.contains(CMTimeFlags::Valid) || host_time.timescale <= 0 {
        return None;
    }
    let nanoseconds = i128::from(host_time.value) * 1_000_000_000 / i128::from(host_time.timescale);
    i64::try_from(nanoseconds).ok()
}

define_class!(
    /// The data output's delegate: hands every sample buffer to the capture's
    /// frame delivery.
    #[unsafe(super(NSObject))]
    #[name = "TatolabCameraSampleBufferDelegate"]
    #[ivars = Arc<AvFoundationFrameDelivery>]
    struct TatolabCameraSampleBufferDelegate;

    unsafe impl NSObjectProtocol for TatolabCameraSampleBufferDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for TatolabCameraSampleBufferDelegate {
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

impl TatolabCameraSampleBufferDelegate {
    fn delivering_into(frame_delivery: Arc<AvFoundationFrameDelivery>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(frame_delivery);
        // SAFETY: `init` is `NSObject`'s designated initializer; the ivars are
        // set above.
        unsafe { msg_send![super(this), init] }
    }
}

/// The camera streaming from AVFoundation's own queues, each frame published
/// as a `VideoFrame` on the lent output port.
pub struct AvFoundationZeroCopyCameraCapture {
    format: NegotiatedAvFoundationCaptureFormat,
    capture_session_control: Arc<Mutex<AvFoundationCaptureSessionControl>>,
}

impl AvFoundationZeroCopyCameraCapture {
    /// Open `device_id`, or the first camera listed, negotiate its format and
    /// settle camera access — refusing one the user denied, or one never asked
    /// for when the host may not ask — publishing nothing until [`Self::start`].
    pub fn open(
        module_host: ModuleHost,
        video_output_port_name: &str,
        device_id: Option<&str>,
        max_width: u32,
        max_height: u32,
        output_pool_rotation_depth: u32,
    ) -> Result<Self, CameraCaptureRefusal> {
        let device = open_requested_avfoundation_device(device_id)?;
        let (device_id, device_name) = unique_id_and_name_of(&device);
        // SAFETY: a property read on a live device.
        let device_formats = unsafe { device.formats() }.to_vec();
        let candidates: Vec<AvFoundationFormatCandidate> = device_formats
            .iter()
            .map(|format| format_candidate_describing(format))
            .collect();
        let chosen = index_of_the_format_to_capture_in(&candidates, max_width, max_height)
            .ok_or_else(|| {
                CameraCaptureRefusal(format!(
                    "camera {device_name:?} ({device_id}) offers no video format to capture in"
                ))
            })?;
        let chosen_candidate = candidates[chosen];
        let format = NegotiatedAvFoundationCaptureFormat {
            device_id,
            device_name,
            core_video_pixel_format: core_video_pixel_format_to_deliver_for(
                chosen_candidate.core_video_pixel_format,
            ),
            width: chosen_candidate.width,
            height: chosen_candidate.height,
            nominal_frames_per_second: Some(
                chosen_candidate.highest_frames_per_second.floor() as u32
            )
            .filter(|&frames_per_second| frames_per_second > 0),
        };
        let video_output_port = module_host
            .open_output_port(video_output_port_name)
            .map_err(|refusal| CameraCaptureRefusal(refusal.to_string()))?;
        let camera_access_prompting = CameraAccessPrompting::stated_by(&module_host);

        let frame_delivery = Arc::new(AvFoundationFrameDelivery {
            module_host: module_host.clone(),
            video_output_port,
            format: format.clone(),
            output_pool_rotation_depth,
            is_delivering: AtomicBool::new(true),
            capture_session_for_its_clock: OnceLock::new(),
            synchronization_clock: OnceLock::new(),
            capture_progress: Mutex::new(AvFoundationCaptureProgress::default()),
        });
        let capture_session_control = Arc::new(Mutex::new(AvFoundationCaptureSessionControl {
            capture_device: CaptureDeviceConfiguredOnItsControlQueue {
                device,
                device_format: device_formats[chosen].clone(),
                shortest_frame_duration: chosen_candidate.shortest_frame_duration,
            },
            frame_delivery,
            session_control_queue: DispatchQueue::new(
                &format!("com.tatolab.camera.session-control.{}", format.device_id),
                None,
            ),
            sample_buffer_queue: DispatchQueue::new(
                &format!("com.tatolab.camera.capture.{}", format.device_id),
                None,
            ),
            running_session: None,
            camera_access: CameraAccess::AwaitingTheUsersAnswer,
            start_requested: false,
            stopped: false,
        }));

        let camera_access = authorize_the_camera_without_waiting_for_the_user(
            &module_host,
            camera_access_prompting,
            Arc::downgrade(&capture_session_control),
        )?;
        if camera_access == CameraAccess::Granted {
            capture_session_control
                .lock()
                .map_err(|_| poisoned_control_lock())?
                .camera_access = CameraAccess::Granted;
        }
        Ok(Self {
            format,
            capture_session_control,
        })
    }

    /// Start the session at once when camera access is allowed, or once the
    /// user allows it when macOS is still asking. A started or stopped capture
    /// stays as it is.
    pub fn start(&mut self) -> Result<(), CameraCaptureRefusal> {
        let mut control = self
            .capture_session_control
            .lock()
            .map_err(|_| poisoned_control_lock())?;
        control.start_requested = true;
        control.configure_and_start_the_session()
    }

    /// What the device negotiated.
    pub fn format(&self) -> &NegotiatedAvFoundationCaptureFormat {
        &self.format
    }

    /// Stop the session, give the adopted surfaces back and wait for a frame
    /// on its way. Idempotent.
    pub fn stop(&mut self) {
        if let Ok(mut control) = self.capture_session_control.lock() {
            control.stop();
        }
    }
}

impl Drop for AvFoundationZeroCopyCameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn poisoned_control_lock() -> CameraCaptureRefusal {
    CameraCaptureRefusal("the capture's control lock is poisoned".to_string())
}

/// Make sure camera access has been requested, never waiting on the user: a
/// request still with the user answers only when they do, so the session
/// starts from the answer, whenever it arrives. A refusal already on record
/// is refused here, and so is an access never asked for when the host may not
/// ask.
fn authorize_the_camera_without_waiting_for_the_user(
    module_host: &ModuleHost,
    camera_access_prompting: CameraAccessPrompting,
    capture_session_control: Weak<Mutex<AvFoundationCaptureSessionControl>>,
) -> Result<CameraAccess, CameraCaptureRefusal> {
    // SAFETY: `AVMediaTypeVideo` is an AVFoundation-exported constant.
    let Some(video) = (unsafe { AVMediaTypeVideo }) else {
        return Err(camera_access_refusal(AVAuthorizationStatus::Restricted));
    };
    // SAFETY: a status read, callable from any thread.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(video) };
    match status {
        AVAuthorizationStatus::Authorized => Ok(CameraAccess::Granted),
        AVAuthorizationStatus::NotDetermined
            if camera_access_prompting == CameraAccessPrompting::MayAskTheUser =>
        {
            module_host.log(
                ModuleHostLogLevel::Info,
                CAMERA_LOG_TARGET,
                "Camera access requested: macOS is asking whether the application that \
                 launched the runtime may use the camera. Frames start as soon as you allow it.",
            );
            let answering_module_host = module_host.clone();
            let answer_reaches = Mutex::new(Some(capture_session_control));
            let completion_handler = block2::RcBlock::new(move |granted: Bool| {
                let Some(capture_session_control) =
                    answer_reaches.lock().ok().and_then(|mut weak| weak.take())
                else {
                    return;
                };
                the_users_camera_answer_arrived(
                    &answering_module_host,
                    &capture_session_control,
                    granted.as_bool(),
                );
            });
            // SAFETY: documented not to block while the user is asked, and to
            // run the handler once on an arbitrary queue, which it is `Send` for.
            unsafe {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(
                    video,
                    &completion_handler,
                )
            };
            Ok(CameraAccess::AwaitingTheUsersAnswer)
        }
        refused => Err(camera_access_refusal(refused)),
    }
}

/// Record a grant and start the session if the capture was started and has
/// not stopped meanwhile; log a denial naming what to change.
fn the_users_camera_answer_arrived(
    module_host: &ModuleHost,
    capture_session_control: &Weak<Mutex<AvFoundationCaptureSessionControl>>,
    granted: bool,
) {
    if !granted {
        module_host.log(
            ModuleHostLogLevel::Error,
            CAMERA_LOG_TARGET,
            &camera_access_refusal(AVAuthorizationStatus::Denied).0,
        );
        return;
    }
    let Some(capture_session_control) = capture_session_control.upgrade() else {
        return;
    };
    let Ok(mut control) = capture_session_control.lock() else {
        return;
    };
    module_host.log(
        ModuleHostLogLevel::Info,
        CAMERA_LOG_TARGET,
        "camera access allowed",
    );
    control.camera_access = CameraAccess::Granted;
    if let Err(start_refusal) = control.configure_and_start_the_session() {
        module_host.log(
            ModuleHostLogLevel::Error,
            CAMERA_LOG_TARGET,
            &format!("the camera could not start: {start_refusal}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_format(
        width: u32,
        height: u32,
        core_video_pixel_format: u32,
        highest_frames_per_second: f64,
    ) -> AvFoundationFormatCandidate {
        AvFoundationFormatCandidate {
            width,
            height,
            core_video_pixel_format,
            highest_frames_per_second,
            shortest_frame_duration: None,
        }
    }

    const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
    const YUYV: u32 = u32::from_be_bytes(*b"yuvs");

    #[test]
    fn the_largest_format_within_the_cap_is_chosen() {
        let formats = [
            a_format(640, 480, CORE_VIDEO_NV12_VIDEO_RANGE, 30.0),
            a_format(1920, 1080, CORE_VIDEO_NV12_VIDEO_RANGE, 30.0),
            a_format(3840, 2160, CORE_VIDEO_NV12_VIDEO_RANGE, 30.0),
            a_format(1280, 720, CORE_VIDEO_NV12_VIDEO_RANGE, 60.0),
        ];
        assert_eq!(
            index_of_the_format_to_capture_in(&formats, 1920, 1080),
            Some(1)
        );
    }

    #[test]
    fn at_one_extent_the_faster_format_wins_and_then_video_range_biplanar() {
        let formats = [
            a_format(1280, 720, BGRA, 30.0),
            a_format(1280, 720, CORE_VIDEO_NV12_FULL_RANGE, 30.0),
            a_format(1280, 720, CORE_VIDEO_NV12_VIDEO_RANGE, 30.0),
            a_format(1280, 720, YUYV, 60.0),
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
            a_format(3840, 2160, CORE_VIDEO_NV12_VIDEO_RANGE, 30.0),
            a_format(1920, 1080, CORE_VIDEO_NV12_VIDEO_RANGE, 30.0),
        ];
        assert_eq!(
            index_of_the_format_to_capture_in(&formats, 640, 480),
            Some(1)
        );
    }

    #[test]
    fn biplanar_formats_are_delivered_as_they_are_and_anything_else_as_video_range() {
        assert_eq!(
            core_video_pixel_format_to_deliver_for(CORE_VIDEO_NV12_FULL_RANGE),
            CORE_VIDEO_NV12_FULL_RANGE
        );
        assert_eq!(
            core_video_pixel_format_to_deliver_for(BGRA),
            CORE_VIDEO_NV12_VIDEO_RANGE
        );
    }

    #[test]
    fn a_host_time_reads_back_in_nanoseconds() {
        let host_time = CMTime {
            value: 1_500_000,
            timescale: 1_000_000,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        assert_eq!(nanoseconds_of_a_host_time(host_time), Some(1_500_000_000));
        let invalid = CMTime {
            flags: CMTimeFlags::empty(),
            ..host_time
        };
        assert_eq!(nanoseconds_of_a_host_time(invalid), None);
    }

    #[test]
    fn listing_cameras_succeeds_with_or_without_one_attached() {
        let devices = list_avfoundation_capture_devices();
        let unique: std::collections::HashSet<&str> =
            devices.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(unique.len(), devices.len());
    }
}
