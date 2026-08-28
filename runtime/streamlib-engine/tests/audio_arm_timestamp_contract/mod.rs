// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every audio backend arm owes the capture seam, asserted once.
//!
//! The claims are about `AudioCaptureStream`, not about any one arm, so they
//! live beside the arms rather than inside either: a change to the seam's
//! contract has to change one copy, and a third arm inherits the suite instead
//! of copying it.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use streamlib_engine::core::context::{
    AudioCaptureStream, AudioStreamFormat, AudioDeviceStreamRequest, AudioClockConfig,
    AudioDeviceBackend, CapturedAudioBlockFromDevice, SharedAudioClock, SoftwareAudioClock,
};
use streamlib_engine::core::media_clock::MediaClock;

/// Enough blocks to see the cadence without making the test slow: at a 512
/// sample period this is well under a second of audio.
const BLOCKS_TO_OBSERVE: usize = 12;

/// A capture that has produced nothing in this long is a broken device, not a
/// slow one.
const CAPTURE_DEADLINE: Duration = Duration::from_secs(10);

/// How far a block-to-block gap may stray from `sample_count / sample_rate`.
/// A device's own clock is regular to well under a microsecond; this is loose
/// enough that a busy machine cannot fail it and tight enough that a lost or
/// duplicated block cannot pass.
const CADENCE_TOLERANCE_NS: i64 = 2_000_000;

/// Long enough that a hand-off which is still being called would have been.
const HOW_LONG_A_STOPPED_STREAM_IS_WATCHED: Duration = Duration::from_millis(250);

/// One block as it was handed over, plus when the hand-off actually ran.
#[derive(Debug, Clone, Copy)]
struct ObservedAudioCaptureBlock {
    sample_count: u32,
    first_sample_timestamp_ns: i64,
    /// `CLOCK_MONOTONIC` at the moment the hand-off carrying this block ran —
    /// the value a stamp-at-delivery implementation would have used.
    handed_off_at_ns: i64,
}

fn monotonic_now_ns() -> i64 {
    MediaClock::now().as_nanos() as i64
}

/// The seam requires a pacing clock; an arm whose device provides the cadence
/// ignores it, which is the point — a device-paced graph never starts the timer.
fn an_unused_deviceless_pacing_clock() -> SharedAudioClock {
    Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)))
}

fn open_capture_stream_on(
    backend: &dyn AudioDeviceBackend,
    device_id: Option<String>,
) -> Box<dyn AudioCaptureStream> {
    backend
        .open_capture_stream(&AudioDeviceStreamRequest {
            device_id,
            deviceless_pacing_clock: an_unused_deviceless_pacing_clock(),
        })
        .expect("a device that answered the probe opens a capture stream")
}

/// Collect blocks from a real device, then stop.
fn observe_blocks_from(
    backend: &dyn AudioDeviceBackend,
    device_id: Option<String>,
) -> (AudioStreamFormat, Vec<ObservedAudioCaptureBlock>) {
    let mut capture_stream = open_capture_stream_on(backend, device_id);
    let capture_stream_format = capture_stream.stream_format();

    let (observed_sender, observed_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = observed_sender.send(ObservedAudioCaptureBlock {
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

    (capture_stream_format, observed)
}

/// Nanoseconds one block of `sample_count` samples occupies at the stream's
/// rate — the gap consecutive blocks are expected to be, and the distance a
/// block's stamp sits before the hand-off that carried it.
fn block_duration_ns(capture_stream_format: AudioStreamFormat, sample_count: u32) -> i64 {
    i64::from(sample_count) * 1_000_000_000 / i64::from(capture_stream_format.sample_rate)
}

fn assert_blocks_are_stamped_before_the_hand_off_that_carries_them(
    capture_stream_format: AudioStreamFormat,
    observed: &[ObservedAudioCaptureBlock],
) {
    assert!(
        capture_stream_format.sample_rate > 0 && capture_stream_format.channels > 0,
        "a negotiated stream reports a real rate and channel count: {capture_stream_format:?}"
    );

    for block in observed {
        let block_duration_ns = block_duration_ns(capture_stream_format, block.sample_count);
        let stamped_before_delivery_by = block.handed_off_at_ns - block.first_sample_timestamp_ns;

        // The assertion a `MediaClock::now()`-at-delivery stamp cannot pass: it
        // would put this at roughly zero. The device's own timing puts the
        // first sample at least one block in the past, because the block had to
        // be captured before it could be delivered.
        assert!(
            stamped_before_delivery_by >= block_duration_ns,
            "a block stamped {stamped_before_delivery_by} ns before its hand-off ran is not \
             the device's timing — one block is {block_duration_ns} ns, and a stamp taken at \
             delivery would read about zero: {block:?}"
        );
    }
}

fn assert_blocks_advance_by_one_block_with_no_gap(
    capture_stream_format: AudioStreamFormat,
    observed: &[ObservedAudioCaptureBlock],
) {
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

fn assert_stamps_land_in_the_kernel_monotonic_domain(
    observed: &[ObservedAudioCaptureBlock],
    before_ns: i64,
    after_ns: i64,
) {
    // The bracket is the whole claim: a stamp inside it is directly
    // subtractable from a `VideoFrame.timestamp_ns`, which is what makes
    // joining audio to camera frames arithmetic rather than machinery. A
    // wall-clock stamp would be five decades outside it.
    for block in observed {
        assert!(
            block.first_sample_timestamp_ns > before_ns - 1_000_000_000
                && block.first_sample_timestamp_ns < after_ns,
            "a device stamp of {} lies outside the CLOCK_MONOTONIC bracket \
             [{before_ns}, {after_ns}]",
            block.first_sample_timestamp_ns
        );
    }
}

/// Every timestamp claim the seam makes, over whichever device path an arm
/// hands in.
///
/// The composition is the contract as much as the individual assertions are —
/// a fourth claim belongs here, once, not in each arm's file.
pub fn assert_the_timestamp_contract_holds_on(
    backend: &dyn AudioDeviceBackend,
    device_id: Option<String>,
) {
    let before_ns = monotonic_now_ns();
    let (capture_stream_format, observed) = observe_blocks_from(backend, device_id);
    let after_ns = monotonic_now_ns();

    assert_blocks_are_stamped_before_the_hand_off_that_carries_them(
        capture_stream_format,
        &observed,
    );
    assert_blocks_advance_by_one_block_with_no_gap(capture_stream_format, &observed);
    assert_stamps_land_in_the_kernel_monotonic_domain(&observed, before_ns, after_ns);
}

/// The two clauses `AudioCaptureStream` states about delivery: a stopped
/// stream calls nothing more, and starting again replaces the hand-off rather
/// than adding to it.
///
/// The restart is also where a stream that left its device running would
/// surface — `snd_pcm_prepare` against a running PCM is `EBUSY`.
pub fn assert_a_stopped_stream_is_silent_and_a_restart_replaces_the_hand_off(
    backend: &dyn AudioDeviceBackend,
    device_id: Option<String>,
) {
    let mut capture_stream = open_capture_stream_on(backend, device_id);

    let (first_sender, first_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = first_sender.send(block.sample_count);
        }))
        .expect("delivery starts");
    first_receiver
        .recv_timeout(CAPTURE_DEADLINE)
        .expect("the first hand-off receives blocks while it is installed");

    capture_stream.stop_delivering().expect("stop");
    while first_receiver.try_recv().is_ok() {
        // Blocks already queued before the stop are not a contract violation;
        // what follows the drain is.
    }
    std::thread::sleep(HOW_LONG_A_STOPPED_STREAM_IS_WATCHED);
    assert!(
        first_receiver.try_recv().is_err(),
        "a hand-off was called after stop_delivering returned, which is the one thing the \
         seam promises cannot happen"
    );

    let (second_sender, second_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = second_sender.send(block.sample_count);
        }))
        .expect("a stopped stream restarts — a device left running would refuse with EBUSY");
    second_receiver
        .recv_timeout(CAPTURE_DEADLINE)
        .expect("the replacing hand-off receives blocks");
    assert!(
        first_receiver.try_recv().is_err(),
        "the replaced hand-off received a block after start_delivering_to installed another"
    );

    capture_stream.stop_delivering().expect("stop");
}
