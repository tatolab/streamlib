// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The capture thread's dequeue loop: it takes each frame from the driver,
//! converts it into a pooled pixel buffer, gives the driver its buffer back,
//! and hands the frame to the node.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use v4l::FourCC;
use v4l::io::traits::CaptureStream;

use crate::core::color::{H273ColorVui, ResolvedColorInfo};
use crate::core::context::capture_and_playback_devices::captured_video_frame_to_pooled_rgba_conversion_stage::{
    CapturedVideoFrameBytesInAStorageBuffer, CapturedVideoFrameDeliveryTally,
    CapturedVideoFrameToPooledRgbaConversionStage,
};
use crate::core::context::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, DeviceReportedCaptureStamp,
    OneOffAcquisitionCountedForItsNode, VideoCaptureInstantResolver,
};
use crate::core::media_clock::MediaClock;
use crate::core::rhi::{PixelBuffer, PublishedPixelBufferFrameId, SourceLayoutInfo, StorageBuffer};

/// What the dequeue loop reads each frame from, converts it on, and hands it
/// to.
pub(super) struct V4l2CaptureDequeueLoopInputs<'a> {
    pub(super) device_fd: RawFd,
    pub(super) is_capturing: &'a AtomicBool,
    pub(super) hand_off: &'a CapturedVideoFrameHandOff,
    pub(super) camera_name: &'a str,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) fourcc: FourCC,
    pub(super) capture_instant_resolver: &'a VideoCaptureInstantResolver,
    pub(super) conversion_stage: &'a mut CapturedVideoFrameToPooledRgbaConversionStage,
    pub(super) input_storage_buffers: &'a [OneOffAcquisitionCountedForItsNode<StorageBuffer>],
    pub(super) input_mapped_ptrs: [*mut u8; 2],
    pub(super) use_dmabuf: bool,
    pub(super) dmabuf_imported_buffers: &'a [StorageBuffer],
    pub(super) input_byte_size: usize,
    pub(super) src_layout: SourceLayoutInfo,
    pub(super) resolved_color: &'a ResolvedColorInfo,
    pub(super) cached_color: H273ColorVui,
}

/// Dequeue, convert, requeue and deliver frames until the capture stops or
/// the device fails.
pub(super) fn run_the_v4l2_dequeue_loop(
    stream: &mut v4l::io::mmap::Stream,
    dequeue_loop_inputs: V4l2CaptureDequeueLoopInputs<'_>,
    record_that_capture_ended_on_its_own: &dyn Fn(String),
) {
    let V4l2CaptureDequeueLoopInputs {
        device_fd,
        is_capturing,
        hand_off,
        camera_name,
        width,
        height,
        fourcc,
        capture_instant_resolver,
        conversion_stage,
        input_storage_buffers,
        input_mapped_ptrs,
        use_dmabuf,
        dmabuf_imported_buffers,
        input_byte_size,
        src_layout,
        resolved_color,
        cached_color,
    } = dequeue_loop_inputs;

    let mut ping_pong_index: usize = 0;
    let mut delivery_tally = CapturedVideoFrameDeliveryTally::default();

    while is_capturing.load(Ordering::Acquire) {
        // ---- Step 1: Acquire frame and select input SSBO ----
        let mut v4l2_requeue_buf: Option<v4l::v4l_sys::v4l2_buffer> = None;
        let frame_sequence: u32;
        let input_ssbo_index: usize;
        let capture_timestamp_ns: i64;

        if use_dmabuf {
            unsafe {
                let mut pollfd = libc::pollfd {
                    fd: device_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let poll_result = libc::poll(&mut pollfd, 1, 1000);
                if poll_result == 0 {
                    continue;
                }
                if poll_result < 0 {
                    if is_capturing.load(Ordering::Acquire) {
                        let poll_error = std::io::Error::last_os_error();
                        tracing::error!(camera = camera_name, "V4L2 poll error");
                        record_that_capture_ended_on_its_own(format!(
                            "polling the device failed: {poll_error}"
                        ));
                    }
                    break;
                }

                let mut v4l2_buf: v4l::v4l_sys::v4l2_buffer = std::mem::zeroed();
                v4l2_buf.type_ = v4l::buffer::Type::VideoCapture as u32;
                v4l2_buf.memory = v4l::memory::Memory::Mmap as u32;

                if libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_DQBUF as libc::c_ulong,
                    &mut v4l2_buf,
                ) != 0
                {
                    if is_capturing.load(Ordering::Acquire) {
                        tracing::error!(camera = camera_name, "DQBUF failed");
                    }
                    continue;
                }

                capture_timestamp_ns = capture_instant_resolver.resolve_capture_timestamp_ns(
                    v4l2_buffer_capture_stamp(
                        v4l2_buf.flags,
                        v4l2_buf.timestamp.tv_sec,
                        v4l2_buf.timestamp.tv_usec,
                    ),
                    MediaClock::now().as_nanos() as i64,
                );
                input_ssbo_index = v4l2_buf.index as usize;
                frame_sequence = v4l2_buf.sequence;
                v4l2_requeue_buf = Some(v4l2_buf);
            }
        } else {
            // MMAP path: stream.next() issues VIDIOC_QBUF + VIDIOC_STREAMON
            // on its first call, then blocks on VIDIOC_DQBUF with the poll
            // timeout applied in start_delivering_to(). Do NOT poll the fd before
            // stream.next() — strict-conformance drivers (v4l2loopback) only
            // signal POLLIN after STREAMON, so an earlier poll hangs.
            let (buf, meta) = match stream.next() {
                Ok(frame) => frame,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => {
                    if is_capturing.load(Ordering::Acquire) {
                        tracing::error!(camera = camera_name, error = %e, "V4L2 stream error");
                        record_that_capture_ended_on_its_own(format!(
                            "the device's stream failed: {e}"
                        ));
                    }
                    break;
                }
            };
            if !is_capturing.load(Ordering::Acquire) {
                break;
            }
            capture_timestamp_ns = capture_instant_resolver.resolve_capture_timestamp_ns(
                v4l2_buffer_capture_stamp(
                    meta.flags.bits(),
                    meta.timestamp.sec,
                    meta.timestamp.usec,
                ),
                MediaClock::now().as_nanos() as i64,
            );
            frame_sequence = meta.sequence;
            input_ssbo_index = ping_pong_index;

            let copy_len = buf.len().min(input_byte_size);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    buf.as_ptr(),
                    input_mapped_ptrs[input_ssbo_index],
                    copy_len,
                );
            }
        }

        let input_buffer = if use_dmabuf {
            &dmabuf_imported_buffers[input_ssbo_index]
        } else {
            &*input_storage_buffers[input_ssbo_index]
        };
        let frame_result = conversion_stage.convert_into_pooled_pixel_buffer(
            CapturedVideoFrameBytesInAStorageBuffer {
                storage_buffer: input_buffer,
                layout: src_layout,
                written_by_another_device: use_dmabuf,
            },
            resolved_color,
        );

        // The V4L2 buffer goes back to the driver on success and failure
        // alike — a skipped requeue starves the DMA-BUF queue after
        // V4L2_BUFFER_COUNT drops.
        requeue_a_v4l2_buffer_to_the_driver(device_fd, v4l2_requeue_buf);

        let (published_pixel_buffer_frame_id, pooled_buffer) = match frame_result {
            Ok(frame_surfaces) => frame_surfaces,
            Err(frame_error) => {
                delivery_tally.record_a_dropped_frame(camera_name, &frame_error);
                continue;
            }
        };

        if !deliver_a_converted_v4l2_frame(
            is_capturing,
            hand_off,
            camera_name,
            width,
            height,
            fourcc,
            cached_color,
            use_dmabuf,
            &mut delivery_tally,
            published_pixel_buffer_frame_id,
            pooled_buffer,
            capture_timestamp_ns,
            frame_sequence,
        ) {
            break;
        }

        if !use_dmabuf {
            ping_pong_index = 1 - ping_pong_index;
        }
    }
}

/// Give a dequeued buffer back to the driver, on the DMA-BUF path; the MMAP
/// path has none to give back.
fn requeue_a_v4l2_buffer_to_the_driver(device_fd: RawFd, buf: Option<v4l::v4l_sys::v4l2_buffer>) {
    if let Some(mut v4l2_buf) = buf {
        let result = unsafe {
            libc::ioctl(
                device_fd,
                v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                &mut v4l2_buf,
            )
        };
        if result != 0 {
            // Each failed requeue permanently removes one buffer from the
            // driver queue; after V4L2_BUFFER_COUNT of them the DMA-BUF
            // path starves silently.
            tracing::error!(
                buffer_index = v4l2_buf.index,
                errno = std::io::Error::last_os_error().raw_os_error(),
                "VIDIOC_QBUF requeue failed — one capture buffer lost"
            );
        }
    }
}

/// Hand one converted frame to the node unless a stop arrived during its GPU
/// work, and tally it; returns whether the capture is still running.
fn deliver_a_converted_v4l2_frame(
    is_capturing: &AtomicBool,
    hand_off: &CapturedVideoFrameHandOff,
    camera_name: &str,
    width: u32,
    height: u32,
    fourcc: FourCC,
    cached_color: H273ColorVui,
    use_dmabuf: bool,
    delivery_tally: &mut CapturedVideoFrameDeliveryTally,
    published_pixel_buffer_frame_id: PublishedPixelBufferFrameId,
    pooled_buffer: PixelBuffer,
    capture_timestamp_ns: i64,
    frame_sequence: u32,
) -> bool {
    // A stop that arrived during this frame's GPU work ends delivery here,
    // so a stop that returned `Ok` is never followed by a hand-off.
    if !is_capturing.load(Ordering::Acquire) {
        return false;
    }
    hand_off(CapturedVideoFrameFromDevice {
        published_pixel_buffer_frame_id: &published_pixel_buffer_frame_id,
        width,
        height,
        color: cached_color,
        capture_timestamp_ns,
    });

    // The pooled pixel buffer must stay alive until the hand-off has
    // written the frame out; the pool reclaims the slot when this handle
    // drops after the write has been delivered into the link's ring.
    drop(pooled_buffer);

    let delivered_frames = delivery_tally.record_a_delivered_frame();
    if delivered_frames == 1 {
        let mode = if use_dmabuf {
            "DMA-BUF zero-copy"
        } else {
            "MMAP + memcpy"
        };
        tracing::info!(
            camera = camera_name,
            mode,
            seq = frame_sequence,
            width,
            height,
            ?fourcc,
            "first frame captured via GPU compute",
        );
    } else if delivered_frames.is_multiple_of(300) {
        tracing::debug!(
            camera = camera_name,
            frame = delivered_frames,
            "frame milestone"
        );
    }
    true
}

/// What a dequeued V4L2 buffer says about when its frame was captured.
///
/// The buffer's timestamp flags name the clock its stamp was taken on; only
/// `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC` is the machine's monotonic clock. The
/// others — `UNKNOWN`, and `COPY` from an output queue — say nothing the
/// engine can join to its own clock.
pub(super) fn v4l2_buffer_capture_stamp(
    buffer_flags: u32,
    timestamp_seconds: i64,
    timestamp_microseconds: i64,
) -> DeviceReportedCaptureStamp {
    let timestamp_type = buffer_flags & v4l::buffer::Flags::TIMESTAMP_MASK.bits();
    if timestamp_type != v4l::buffer::Flags::TIMESTAMP_MONOTONIC.bits() {
        return DeviceReportedCaptureStamp::OffTheMachineMonotonicClock;
    }
    DeviceReportedCaptureStamp::OnTheMachineMonotonicClock {
        capture_timestamp_ns: timestamp_seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(timestamp_microseconds.saturating_mul(1_000)),
    }
}
