// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The ALSA arm against a real capture device: blocks arrive, and their
//! timestamps are the device's rather than the moment of delivery.
//!
//! The distinction is the whole of block-level A/V sync, and it is invisible
//! without an assertion — a stamp taken at delivery looks perfectly plausible
//! and is wrong by a device period, every block, forever. So the test measures
//! the one thing a delivery-time stamp cannot satisfy: the block's first sample
//! was captured *before* the callback that carried it ran.
//!
//! The arm is constructed directly rather than reached through
//! `probe_audio_device_backend`. The chain takes the first arm that opens and
//! no dial overrides it — which is the decided behaviour — so on any machine
//! with a PipeWire session the probe answers "pipewire" and this arm would
//! never be exercised.
//!
//! Audio tier — needs `/dev/snd` and a capture device ALSA can open.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use streamlib_engine::core::context::{
    AudioCaptureStreamFormat, AudioCaptureStreamRequest, AudioClockConfig, AudioDeviceBackend,
    CapturedAudioBlockFromDevice, SharedAudioClock, SoftwareAudioClock,
};
use streamlib_engine::core::media_clock::MediaClock;
use streamlib_engine::linux_alsa_audio_device_backend::AlsaAudioDeviceBackend;

/// Enough blocks to see the cadence without making the test slow: at a 512
/// sample period this is well under a second of audio.
const BLOCKS_TO_OBSERVE: usize = 12;

/// A capture that has produced nothing in this long is a broken device, not a
/// slow one.
const CAPTURE_DEADLINE: Duration = Duration::from_secs(10);

/// How far a block-to-block gap may stray from `sample_count / sample_rate`.
/// A sound card's own clock is regular to well under a microsecond; this is
/// loose enough that a busy machine cannot fail it and tight enough that a lost
/// or duplicated block cannot pass.
const CADENCE_TOLERANCE_NS: i64 = 2_000_000;

/// One block as it was handed over, plus when the hand-off actually ran.
#[derive(Debug, Clone, Copy)]
struct ObservedBlock {
    sample_count: u32,
    first_sample_timestamp_ns: i64,
    /// `CLOCK_MONOTONIC` at the moment the hand-off carrying this block ran —
    /// the value a stamp-at-delivery implementation would have used.
    handed_off_at_ns: i64,
}

fn monotonic_now_ns() -> i64 {
    MediaClock::now().as_nanos() as i64
}

/// Blocks from a real device through the ALSA arm, or `None` when this machine
/// has no `libasound` or no capture device behind it.
///
/// `None` rather than a panic, so the tier stays well-behaved when the feature
/// is on but the runner has no sound card — the same shape
/// `try_vulkan_device()` gives the GPU tier (`docs/testing-hardware.md`). A
/// machine with no audio is a supported environment; it just cannot answer the
/// question these tests ask.
fn observe_blocks_from_a_real_device() -> Option<(AudioCaptureStreamFormat, Vec<ObservedBlock>)> {
    let backend = AlsaAudioDeviceBackend::load_and_open().ok()?;
    assert_eq!(
        backend.backend_name(),
        "alsa",
        "the arm names itself the way the chain's probe log line reports it"
    );

    // Handed over because the seam requires one; the ALSA arm ignores it, which
    // is the point — a device-paced graph never starts the timer.
    let unused_deviceless_clock: SharedAudioClock =
        Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)));
    let mut capture_stream = backend
        .open_capture_stream(&AudioCaptureStreamRequest {
            device_id: None,
            deviceless_pacing_clock: unused_deviceless_clock,
        })
        .expect("a device that answered the probe opens its default capture PCM");
    let capture_stream_format = capture_stream.stream_format();

    let (observed_sender, observed_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = observed_sender.send(ObservedBlock {
                sample_count: block.sample_count,
                first_sample_timestamp_ns: block.first_sample_timestamp_ns,
                handed_off_at_ns: monotonic_now_ns(),
            });
        }))
        .expect("delivery starts");

    let mut observed = Vec::with_capacity(BLOCKS_TO_OBSERVE);
    while observed.len() < BLOCKS_TO_OBSERVE {
        let block = observed_receiver
            .recv_timeout(CAPTURE_DEADLINE)
            .expect("an open capture stream delivers blocks");
        observed.push(block);
    }
    capture_stream.stop_delivering().expect("stop");

    Some((capture_stream_format, observed))
}

/// Nanoseconds one block of `sample_count` samples occupies at the stream's
/// rate — the gap consecutive blocks are expected to be, and the distance a
/// block's stamp sits before the hand-off that carried it.
fn block_duration_ns(capture_stream_format: AudioCaptureStreamFormat, sample_count: u32) -> i64 {
    i64::from(sample_count) * 1_000_000_000 / i64::from(capture_stream_format.sample_rate)
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a capture device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_real_device_stamps_its_blocks_before_the_callback_that_carries_them() {
    let Some((capture_stream_format, observed)) = observe_blocks_from_a_real_device() else {
        return;
    };

    assert!(
        capture_stream_format.sample_rate > 0 && capture_stream_format.channels > 0,
        "a negotiated stream reports a real rate and channel count: {capture_stream_format:?}"
    );

    for block in &observed {
        let block_duration_ns = block_duration_ns(capture_stream_format, block.sample_count);
        let stamped_before_delivery_by = block.handed_off_at_ns - block.first_sample_timestamp_ns;

        // The assertion a `MediaClock::now()`-at-delivery stamp cannot pass:
        // it would put this at roughly zero. The device's own timing puts the
        // first sample at least one block in the past, because the block had
        // to be captured before it could be delivered.
        assert!(
            stamped_before_delivery_by >= block_duration_ns,
            "a block stamped {stamped_before_delivery_by} ns before its hand-off ran is not \
             the device's timing — one block is {block_duration_ns} ns, and a stamp taken at \
             delivery would read about zero: {block:?}"
        );
    }
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a capture device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_real_devices_blocks_advance_by_one_block_with_no_gap_across_a_run() {
    let Some((capture_stream_format, observed)) = observe_blocks_from_a_real_device() else {
        return;
    };

    for pair in observed.windows(2) {
        let [earlier, later] = pair else {
            unreachable!("windows(2) yields pairs");
        };
        let expected_gap_ns = block_duration_ns(capture_stream_format, earlier.sample_count);
        let actual_gap_ns = later.first_sample_timestamp_ns - earlier.first_sample_timestamp_ns;
        assert!(
            (actual_gap_ns - expected_gap_ns).abs() <= CADENCE_TOLERANCE_NS,
            "consecutive blocks are {actual_gap_ns} ns apart where {expected_gap_ns} ns of \
             samples separate them — audio went missing, or a block claimed an instant it \
             did not cover"
        );
    }
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a capture device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_real_devices_stamps_land_in_the_kernel_monotonic_domain() {
    let before_ns = monotonic_now_ns();
    let Some((_, observed)) = observe_blocks_from_a_real_device() else {
        return;
    };
    let after_ns = monotonic_now_ns();

    // The bracket is the whole claim, and on this arm it is what
    // `SND_PCM_TSTAMP_TYPE_MONOTONIC` buys: a stamp inside it is directly
    // subtractable from a `VideoFrame.timestamp_ns`, which is what makes
    // joining audio to camera frames arithmetic rather than machinery. A
    // `CLOCK_REALTIME` stamp would be five decades outside it.
    for block in &observed {
        assert!(
            block.first_sample_timestamp_ns > before_ns - 1_000_000_000
                && block.first_sample_timestamp_ns < after_ns,
            "a device stamp of {} lies outside the CLOCK_MONOTONIC bracket \
             [{before_ns}, {after_ns}]",
            block.first_sample_timestamp_ns
        );
    }
}
