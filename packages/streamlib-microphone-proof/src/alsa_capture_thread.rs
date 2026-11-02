// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The device thread: one ALSA period read, stamped, and handed to the ring, forever.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::alsa_capture_library::{
    ALSA_BROKEN_PIPE, CapturePeriodWait, OpenedCapturePcm, OpenedCapturePcmFormat,
};
use crate::captured_audio_block_ring::{CapturedAudioBlock, CapturedAudioBlockRing};
use crate::monotonic_clock::monotonic_now_ns;
use crate::realtime_thread_priority::make_the_calling_thread_realtime;

/// How long one wait for a period lasts before the thread looks at its stop flag.
const PERIOD_WAIT_TIMEOUT_MS: i32 = 100;

const BYTES_PER_F32_SAMPLE: usize = 4;

/// What the device thread reports while it runs and after it ends.
#[derive(Default)]
pub(crate) struct AlsaCaptureThreadReport {
    pub(crate) device_overrun_count: AtomicU64,
    pub(crate) captured_block_count: AtomicU64,
    pub(crate) device_thread_priority: Mutex<Option<String>>,
    pub(crate) failure_that_ended_the_capture: Mutex<Option<String>>,
}

impl AlsaCaptureThreadReport {
    fn record_the_failure_that_ended_the_capture(&self, failure: String) {
        if let Ok(mut slot) = self.failure_that_ended_the_capture.lock() {
            *slot = Some(failure);
        }
    }
}

/// Read blocks of `block_frames` until `stop_requested`, then end the ring's hand-off.
pub(crate) fn capture_into_the_ring_until_stopped(
    mut pcm: OpenedCapturePcm,
    format: OpenedCapturePcmFormat,
    block_frames: u32,
    ring: &CapturedAudioBlockRing,
    stop_requested: &AtomicBool,
    report: &AlsaCaptureThreadReport,
) {
    let priority = make_the_calling_thread_realtime();
    if let Ok(mut slot) = report.device_thread_priority.lock() {
        *slot = Some(priority.description());
    }
    if let Some(failure) =
        read_blocks_until_stopped(&mut pcm, format, block_frames, ring, stop_requested, report)
    {
        report.record_the_failure_that_ended_the_capture(failure);
    }
    ring.end_hand_off();
}

fn read_blocks_until_stopped(
    pcm: &mut OpenedCapturePcm,
    format: OpenedCapturePcmFormat,
    block_frames: u32,
    ring: &CapturedAudioBlockRing,
    stop_requested: &AtomicBool,
    report: &AlsaCaptureThreadReport,
) -> Option<String> {
    let bytes_per_frame = format.channels as usize * BYTES_PER_F32_SAMPLE;
    let sample_rate = i64::from(format.sample_rate);
    if let Err(error_code) = pcm.start() {
        return Some(format!(
            "snd_pcm_start refused: {}",
            pcm.library().error_text(error_code)
        ));
    }
    while !stop_requested.load(Ordering::Acquire) {
        match pcm.wait_for_a_period(PERIOD_WAIT_TIMEOUT_MS) {
            CapturePeriodWait::TimedOut => continue,
            CapturePeriodWait::Ready => {}
            CapturePeriodWait::Failed(error_code) => {
                if let Err(unrecoverable) = recover(pcm, error_code, report) {
                    return Some(unrecoverable);
                }
                continue;
            }
        }
        let mut interleaved_f32_sample_bytes = vec![0u8; block_frames as usize * bytes_per_frame];
        let sample_count = match pcm.read_interleaved(
            &mut interleaved_f32_sample_bytes,
            block_frames,
            bytes_per_frame,
        ) {
            Ok(0) => continue,
            Ok(sample_count) => sample_count,
            Err(error_code) => {
                if let Err(unrecoverable) = recover(pcm, error_code, report) {
                    return Some(unrecoverable);
                }
                continue;
            }
        };
        let read_returned_at_ns = monotonic_now_ns();
        // The block's last sample is everything still waiting in the device behind it,
        // so its first is that plus the block itself before the read returned.
        let frames_since_the_first_sample =
            pcm.frames_waiting_to_be_read() + i64::from(sample_count);
        let first_sample_timestamp_ns =
            read_returned_at_ns - frames_since_the_first_sample * 1_000_000_000 / sample_rate;
        interleaved_f32_sample_bytes.truncate(sample_count as usize * bytes_per_frame);
        ring.hand_off_from_device_thread(CapturedAudioBlock {
            interleaved_f32_sample_bytes,
            sample_count,
            first_sample_timestamp_ns,
        });
        report.captured_block_count.fetch_add(1, Ordering::Relaxed);
    }
    None
}

fn recover(
    pcm: &mut OpenedCapturePcm,
    error_code: i32,
    report: &AlsaCaptureThreadReport,
) -> Result<(), String> {
    if error_code == ALSA_BROKEN_PIPE {
        report.device_overrun_count.fetch_add(1, Ordering::Relaxed);
    }
    pcm.recover(error_code).map_err(|unrecoverable| {
        format!(
            "the capture stream could not be recovered from {}: {}",
            pcm.library().error_text(error_code),
            pcm.library().error_text(unrecoverable)
        )
    })?;
    // `snd_pcm_recover` re-prepares a capture stream without starting it.
    pcm.start().map_err(|error_code| {
        format!(
            "snd_pcm_start after a recovery refused: {}",
            pcm.library().error_text(error_code)
        )
    })
}
