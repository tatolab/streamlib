// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Configuring and starting a capture session on its control queue, watching
//! the ways it ends, and stopping it.

use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureDeviceWasDisconnectedNotification,
    AVCaptureSession, AVCaptureSessionRuntimeErrorNotification, AVCaptureVideoDataOutput,
};
use objc2_core_foundation::CFString;
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};
use objc2_foundation::{
    NSDictionary, NSNotification, NSNotificationCenter, NSNotificationName, NSNumber,
    NSObjectProtocol, NSString,
};

use crate::apple::core_video_pixel_format_dictionary::ensure_core_video_pixel_format_dictionary_is_initialised;
use crate::core::context::{
    DeviceStreamFailureReason, DeviceStreamFailureRecorder, VideoCaptureStreamFormat,
};
use crate::core::logging::{
    run_in_the_project_log_route_entry_when_there_is_one,
    the_project_log_route_entry_of_this_thread,
};
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};

use super::CaptureDeviceConfiguredOnItsControlQueue;
use super::avfoundation_frame_delivery::{
    AvFoundationFrameDelivery, AvFoundationSampleBufferDelegate, CaptureSessionReadForItsClock,
};

/// What the control queue needs to configure and start one session.
pub(super) struct AvFoundationCaptureSessionRequest {
    pub(super) capture_device: CaptureDeviceConfiguredOnItsControlQueue,
    pub(super) pixel_format_to_deliver: PixelFormat,
    pub(super) stream_format: VideoCaptureStreamFormat,
    pub(super) frame_delivery: Arc<AvFoundationFrameDelivery>,
    pub(super) sample_buffer_queue: DispatchRetained<DispatchQueue>,
    pub(super) failure_recorder: DeviceStreamFailureRecorder,
    pub(super) camera_name: String,
}

impl AvFoundationCaptureSessionRequest {
    /// Configure a session on the device and format, point its output at the
    /// frame delivery, and start it — unless the delivery was stopped while
    /// this waited its turn, when powering the camera up would only be torn
    /// down again. Runs on the device's control queue.
    pub(super) fn configure_and_start(
        &self,
    ) -> Result<Option<AvFoundationConfiguredCaptureSession>> {
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
        let project_log_route_entry_of_the_observing_thread =
            the_project_log_route_entry_of_this_thread();
        let on_notification = block2::RcBlock::new(move |notification: NonNull<NSNotification>| {
            // SAFETY: NSNotificationCenter hands the block a live notification.
            let notification = unsafe { notification.as_ref() };
            let detail = notification
                .userInfo()
                .map(|user_info| user_info.description().to_string())
                .unwrap_or_default();
            run_in_the_project_log_route_entry_when_there_is_one(
                project_log_route_entry_of_the_observing_thread.as_ref(),
                || {
                    tracing::error!(
                        camera = %camera_name,
                        %detail,
                        "AVFoundation camera: {what_happened}"
                    );
                },
            );
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
pub(super) struct AvFoundationConfiguredCaptureSession {
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
    pub(super) fn stop(self) {
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
