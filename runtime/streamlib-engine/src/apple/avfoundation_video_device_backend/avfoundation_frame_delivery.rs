// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Delivering each sample buffer into a pooled pixel buffer, stamped on the
//! device's own clock, through the sample buffer delegate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutputSampleBufferDelegate,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMClock, CMSampleBuffer, CMSyncConvertTime, CMTimeFlags};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetIOSurface, CVPixelBufferGetWidth,
};
use objc2_foundation::{NSObject, NSObjectProtocol};
use parking_lot::Mutex;

use crate::apple::biplanar_420_iosurface_to_pooled_rgba_conversion::Biplanar420IOSurfaceToPooledRgbaConversion;
use crate::apple::core_video_pixel_buffer_color::core_video_pixel_buffer_color_to_h273_color_vui;
use crate::core::color::{ColorSpaceKind, H273ColorVui, ResolveH273ColorVuiDefaults};
use crate::core::context::capture_and_playback_devices::captured_video_frame_to_pooled_rgba_conversion_stage::CapturedVideoFrameDeliveryTally;
use crate::core::context::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, DeviceReportedCaptureStamp,
    GpuContextLimitedAccess, VideoCaptureInstantResolver, VideoCaptureStreamFormat,
};
use crate::core::logging::{
    ProjectLogRouteEntry, run_in_the_project_log_route_entry_when_there_is_one,
};
use crate::core::media_clock::MediaClock;
use crate::core::rhi::{PixelBuffer, PixelFormat, PublishedPixelBufferFrameId};
use crate::core::{Error, Result};

use super::DELIVERY_DRAIN_TIMEOUT;
use super::avfoundation_capture_session_request::AvFoundationConfiguredCaptureSession;

/// One delivery: the frame delivery its hand-off rides on, the queue it runs
/// on, and the session the control queue configures for it — absent until the
/// control queue has, and after it could not.
pub(super) struct AvFoundationRunningDelivery {
    pub(super) frame_delivery: Arc<AvFoundationFrameDelivery>,
    pub(super) sample_buffer_queue: DispatchRetained<DispatchQueue>,
    pub(super) configured_session: Arc<Mutex<Option<AvFoundationConfiguredCaptureSession>>>,
}

impl AvFoundationRunningDelivery {
    /// End the delivery: no hand-off follows an `Ok`. The session stops on the
    /// control queue behind any start still powering the camera up, and this
    /// does not wait for it — only, bounded, for a frame already on its way
    /// through the sample-buffer queue.
    pub(super) fn stop(
        self,
        session_control_queue: &DispatchQueue,
        camera_name: &str,
    ) -> Result<()> {
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
pub(super) struct CaptureSessionReadForItsClock(pub(super) objc2::rc::Weak<AVCaptureSession>);

// SAFETY: loaded only on the session's own sample-buffer queue, and only to
// read its synchronization clock — a read-only property AVFoundation serves
// from any thread.
unsafe impl Send for CaptureSessionReadForItsClock {}
// SAFETY: as above.
unsafe impl Sync for CaptureSessionReadForItsClock {}

/// What the sample-buffer delegate delivers each frame through.
pub(super) struct AvFoundationFrameDelivery {
    pub(super) hand_off: CapturedVideoFrameHandOff,
    pub(super) is_delivering: AtomicBool,
    pub(super) capture_session: OnceLock<CaptureSessionReadForItsClock>,
    pub(super) synchronization_clock: OnceLock<CFRetained<CMClock>>,
    pub(super) camera_name: String,
    pub(super) stream_format: VideoCaptureStreamFormat,
    pub(super) pixel_format_delivered: PixelFormat,
    pub(super) gpu_context: GpuContextLimitedAccess,
    pub(super) node_run_id_of_the_capturing_node: String,
    pub(super) capture_instant_resolver: Arc<VideoCaptureInstantResolver>,
    pub(super) capture_progress: Mutex<AvFoundationCaptureProgress>,
    /// The route of the thread that started the delivery, which each frame's
    /// delivery runs in.
    pub(super) project_log_route_entry_of_the_starting_thread: Option<Arc<ProjectLogRouteEntry>>,
}

impl AvFoundationFrameDelivery {
    /// Land one sample buffer in a pooled pixel buffer and hand it off. Runs
    /// on the session's serial sample-buffer queue.
    fn deliver(&self, sample_buffer: &CMSampleBuffer) {
        run_in_the_project_log_route_entry_when_there_is_one(
            self.project_log_route_entry_of_the_starting_thread.as_ref(),
            || self.deliver_in_the_starting_threads_log_route(sample_buffer),
        );
    }

    fn deliver_in_the_starting_threads_log_route(&self, sample_buffer: &CMSampleBuffer) {
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
pub(super) struct AvFoundationCaptureProgress {
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
                    &delivery.node_run_id_of_the_capturing_node,
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
    pub(super) struct AvFoundationSampleBufferDelegate;

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
    pub(super) fn delivering_into(
        frame_delivery: Arc<AvFoundationFrameDelivery>,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(frame_delivery);
        // SAFETY: `init` is `NSObject`'s designated initializer; the ivars are
        // set above.
        unsafe { msg_send![super(this), init] }
    }
}
