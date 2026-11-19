// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The capture thread's dequeue loop: it takes each frame from the driver,
//! converts it into a pooled pixel buffer, gives the driver its buffer back,
//! and hands the frame to the node.

use std::sync::atomic::{AtomicBool, Ordering};

use v4l::FourCC;

use crate::core::color::{H273ColorVui, ResolvedColorInfo};
use crate::core::context::capture_and_playback_devices::captured_video_frame_to_pooled_rgba_conversion_stage::{
    CapturedVideoFrameBytesInAStorageBuffer, CapturedVideoFrameDeliveryTally,
    CapturedVideoFrameToPooledRgbaConversionStage,
};
use crate::core::context::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, DeviceReportedCaptureStamp,
    OneOffAcquisitionCountedForItsNode, VideoCaptureInstantResolver,
};
use crate::core::Error;
use crate::core::media_clock::MediaClock;
use crate::core::rhi::{PixelBuffer, PublishedPixelBufferFrameId, SourceLayoutInfo, StorageBuffer};

use super::v4l2_capture_buffer_queue::{
    HeldV4l2Buffer, NextFilledV4l2Buffer, V4l2CaptureBufferQueue, V4l2DeviceCaptureBufferQueue,
};
use super::v4l2_driver_buffers_mapped_for_reading::V4l2DriverBuffersMappedForReading;

/// Where each frame's bytes reach the converter from.
pub(super) enum V4l2CaptureInput<'a> {
    /// The driver's buffers, imported into the GPU and converted in place.
    DmaBuf {
        imported_buffers: &'a [StorageBuffer],
    },
    /// Each frame copied out of the driver's buffer into one of two storage
    /// buffers, in turn.
    Mmap {
        mapped_driver_buffers: &'a V4l2DriverBuffersMappedForReading<'a>,
        storage_buffers: &'a [OneOffAcquisitionCountedForItsNode<StorageBuffer>],
        storage_buffer_mappings: [*mut u8; 2],
    },
}

/// What the dequeue loop reads each frame from, converts it on, and hands it
/// to.
pub(super) struct V4l2CaptureDequeueLoopInputs<'a> {
    pub(super) is_capturing: &'a AtomicBool,
    pub(super) hand_off: &'a CapturedVideoFrameHandOff,
    pub(super) camera_name: &'a str,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) fourcc: FourCC,
    pub(super) capture_instant_resolver: &'a VideoCaptureInstantResolver,
    pub(super) conversion_stage: &'a mut CapturedVideoFrameToPooledRgbaConversionStage,
    pub(super) input: V4l2CaptureInput<'a>,
    pub(super) input_byte_size: usize,
    pub(super) src_layout: SourceLayoutInfo,
    pub(super) resolved_color: &'a ResolvedColorInfo,
    pub(super) cached_color: H273ColorVui,
}

/// Dequeue, convert, requeue and deliver frames until the capture stops or
/// the device fails.
pub(super) fn run_the_v4l2_dequeue_loop(
    capture_buffer_queue: &mut V4l2DeviceCaptureBufferQueue<'_>,
    dequeue_loop_inputs: V4l2CaptureDequeueLoopInputs<'_>,
    record_that_capture_ended_on_its_own: &dyn Fn(String),
) {
    let V4l2CaptureDequeueLoopInputs {
        is_capturing,
        hand_off,
        camera_name,
        width,
        height,
        fourcc,
        capture_instant_resolver,
        conversion_stage,
        input,
        input_byte_size,
        src_layout,
        resolved_color,
        cached_color,
    } = dequeue_loop_inputs;

    let use_dmabuf = matches!(input, V4l2CaptureInput::DmaBuf { .. });
    let mut ping_pong_index: usize = 0;
    let mut delivery_tally = CapturedVideoFrameDeliveryTally::default();

    while is_capturing.load(Ordering::Acquire) {
        let filled_buffer =
            match capture_buffer_queue.wait_for_the_next_filled_buffer(camera_name, is_capturing) {
                NextFilledV4l2Buffer::Filled(filled_buffer) => filled_buffer,
                NextFilledV4l2Buffer::CaptureStopped => break,
                NextFilledV4l2Buffer::DeviceFailed(reason) => {
                    if is_capturing.load(Ordering::Acquire) {
                        tracing::error!(camera = camera_name, %reason, "V4L2 capture failed");
                        record_that_capture_ended_on_its_own(reason);
                    }
                    break;
                }
            };
        let capture_timestamp_ns = capture_instant_resolver.resolve_capture_timestamp_ns(
            v4l2_buffer_capture_stamp(
                filled_buffer.flags(),
                filled_buffer.timestamp().tv_sec,
                filled_buffer.timestamp().tv_usec,
            ),
            MediaClock::now().as_nanos() as i64,
        );
        let frame_sequence = filled_buffer.sequence();
        let (input_buffer, buffer_held_through_the_conversion) = match &input {
            V4l2CaptureInput::DmaBuf { imported_buffers } => {
                let Some(imported_buffer) = imported_buffers.get(filled_buffer.index() as usize)
                else {
                    drop_a_frame_from_a_buffer_never_granted(
                        capture_buffer_queue,
                        &mut delivery_tally,
                        camera_name,
                        filled_buffer,
                    );
                    continue;
                };
                (imported_buffer, Some(filled_buffer))
            }
            V4l2CaptureInput::Mmap {
                mapped_driver_buffers,
                storage_buffers,
                storage_buffer_mappings,
            } => {
                let Some(frame_bytes) = mapped_driver_buffers.bytes_of(&filled_buffer) else {
                    drop_a_frame_from_a_buffer_never_granted(
                        capture_buffer_queue,
                        &mut delivery_tally,
                        camera_name,
                        filled_buffer,
                    );
                    continue;
                };
                let copy_len = frame_bytes.len().min(input_byte_size);
                // SAFETY: each storage buffer's mapping is at least
                // `input_byte_size` long, and `copy_len` is no longer.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        frame_bytes.as_ptr(),
                        storage_buffer_mappings[ping_pong_index],
                        copy_len,
                    );
                }
                // The frame is out of the driver's buffer, so the driver can
                // fill it again during the conversion.
                capture_buffer_queue.requeue_a_read_buffer(camera_name, filled_buffer);
                (&*storage_buffers[ping_pong_index], None)
            }
        };
        let frame_result = conversion_stage.convert_into_pooled_pixel_buffer(
            CapturedVideoFrameBytesInAStorageBuffer {
                storage_buffer: input_buffer,
                layout: src_layout,
                written_by_another_device: use_dmabuf,
            },
            resolved_color,
        );

        // A converted DMA-BUF frame goes back to the driver on success and
        // failure alike — a skipped requeue starves the queue.
        if let Some(converted_buffer) = buffer_held_through_the_conversion {
            capture_buffer_queue.requeue_a_read_buffer(camera_name, converted_buffer);
        }

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

/// Drop the frame of a buffer the driver named but never granted, and give
/// the buffer back.
fn drop_a_frame_from_a_buffer_never_granted(
    capture_buffer_queue: &mut V4l2DeviceCaptureBufferQueue<'_>,
    delivery_tally: &mut CapturedVideoFrameDeliveryTally,
    camera_name: &str,
    buffer: HeldV4l2Buffer,
) {
    delivery_tally.record_a_dropped_frame(
        camera_name,
        &Error::Runtime(format!(
            "the driver filled buffer {}, which it never granted",
            buffer.index()
        )),
    );
    capture_buffer_queue.requeue_a_read_buffer(camera_name, buffer);
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
