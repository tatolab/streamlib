// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every audio backend arm owes the playback seam, asserted once.
//!
//! The claims are about `AudioPlaybackStream`, not about any one arm, so they
//! live beside the arms rather than inside either: a change to the seam's
//! contract has to change one copy, and a third arm inherits the suite instead
//! of copying it. Sister to `audio_arm_timestamp_contract`, which does the
//! same for capture.
//!
//! Nothing here listens to what came out. A device that asked for samples,
//! took the ones it was given and stopped asking when told is everything the
//! seam promises; whether the sound is right is a loopback question and lives
//! in `tests/fixtures/verify_audio_loopback.sh`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use streamlib_engine::core::context::{
    AudioBlockRequestedByDevice, AudioClockConfig, AudioDeviceBackend, AudioDeviceStreamRequest,
    AudioPlaybackStream, AudioStreamFormat, SharedAudioClock, SoftwareAudioClock,
};

/// Enough periods to see the device asking repeatedly rather than once.
const PERIODS_TO_OBSERVE: usize = 8;

/// A device that has asked for nothing in this long is not playing.
const PLAYBACK_DEADLINE: Duration = Duration::from_secs(10);

/// Long enough that a hand-off which is still being called would have been.
const HOW_LONG_A_STOPPED_STREAM_IS_WATCHED: Duration = Duration::from_millis(250);

/// The seam requires a pacing clock; an arm whose device provides the cadence
/// ignores it, which is the point — a device-paced graph never starts the timer.
fn an_unused_deviceless_pacing_clock() -> SharedAudioClock {
    Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)))
}

fn open_playback_stream_on(
    backend: &dyn AudioDeviceBackend,
    device_id: Option<String>,
) -> Box<dyn AudioPlaybackStream> {
    backend
        .open_playback_stream(&AudioDeviceStreamRequest {
            device_id,
            deviceless_pacing_clock: an_unused_deviceless_pacing_clock(),
        })
        .expect("a device that answered the probe opens a playback stream")
}

/// One request as the device made it.
#[derive(Debug, Clone, Copy)]
struct ObservedPlaybackRequest {
    sample_count: u32,
    interleaved_sample_byte_count: usize,
}

/// The device asks, repeatedly, for buffers the negotiated format describes.
///
/// The byte count is the assertion that earns its keep: a buffer that is not a
/// whole number of frames means a caller would write partial frames and every
/// later sample would land in the wrong channel — silently, and only on the
/// arm that got it wrong.
pub fn assert_the_device_asks_for_whole_periods_of_its_own_format(
    backend: &dyn AudioDeviceBackend,
    device_id: Option<String>,
) {
    let mut playback_stream = open_playback_stream_on(backend, device_id);
    let stream_format = playback_stream.stream_format();
    assert!(
        stream_format.sample_rate > 0 && stream_format.channels > 0,
        "a negotiated stream reports a real rate and channel count: {stream_format:?}"
    );

    let (observed_sender, observed_receiver) = mpsc::channel();
    playback_stream
        .start_requesting_from(Box::new(
            move |requested: AudioBlockRequestedByDevice<'_>| {
                let _ = observed_sender.send(ObservedPlaybackRequest {
                    sample_count: requested.sample_count,
                    interleaved_sample_byte_count: requested.interleaved_sample_bytes_to_fill.len(),
                });
                // Silence rather than a tone: nothing here listens, and a rig
                // running this suite should not be made to make noise.
                requested.interleaved_sample_bytes_to_fill.fill(0);
            },
        ))
        .expect("requesting starts");

    for _ in 0..PERIODS_TO_OBSERVE {
        let requested = observed_receiver
            .recv_timeout(PLAYBACK_DEADLINE)
            .expect("an open playback stream asks for samples");
        assert!(
            requested.sample_count > 0,
            "a device asked for a period of no samples: {requested:?}"
        );
        assert_eq!(
            requested.interleaved_sample_byte_count,
            byte_count_of(stream_format, requested.sample_count),
            "the buffer is not {} whole frames of the negotiated format {stream_format:?} — a \
             caller filling it would put every later sample in the wrong channel",
            requested.sample_count
        );
    }

    playback_stream.stop_requesting().expect("stop");
}

fn byte_count_of(stream_format: AudioStreamFormat, sample_count: u32) -> usize {
    stream_format.interleaved_byte_count_for(sample_count)
}

/// The two clauses `AudioPlaybackStream` states about requesting: a stopped
/// stream asks nothing more, and starting again replaces the hand-off rather
/// than adding to it.
///
/// The restart is also where a stream that left its device running would
/// surface — `snd_pcm_prepare` against a running PCM is `EBUSY`.
pub fn assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off(
    backend: &dyn AudioDeviceBackend,
    device_id: Option<String>,
) {
    let mut playback_stream = open_playback_stream_on(backend, device_id);

    let first_request_count = Arc::new(AtomicU64::new(0));
    let (first_sender, first_receiver) = mpsc::channel();
    playback_stream
        .start_requesting_from(Box::new({
            let first_request_count = Arc::clone(&first_request_count);
            move |requested: AudioBlockRequestedByDevice<'_>| {
                requested.interleaved_sample_bytes_to_fill.fill(0);
                // Release, not Relaxed: the assertions below read this with
                // Acquire to decide whether a callback ran after the stop, and
                // a Relaxed increment pairs with nothing — the load could
                // observe the pre-stop count and pass over exactly the
                // violation it is watching for.
                first_request_count.fetch_add(1, Ordering::Release);
                let _ = first_sender.send(());
            }
        }))
        .expect("requesting starts");
    first_receiver
        .recv_timeout(PLAYBACK_DEADLINE)
        .expect("the first hand-off is asked for samples while it is installed");

    playback_stream.stop_requesting().expect("stop");
    let asked_by_the_stop = first_request_count.load(Ordering::Acquire);
    std::thread::sleep(HOW_LONG_A_STOPPED_STREAM_IS_WATCHED);
    assert_eq!(
        first_request_count.load(Ordering::Acquire),
        asked_by_the_stop,
        "a hand-off was asked for samples after stop_requesting returned, which is the one \
         thing the seam promises cannot happen"
    );

    let (second_sender, second_receiver) = mpsc::channel();
    playback_stream
        .start_requesting_from(Box::new(
            move |requested: AudioBlockRequestedByDevice<'_>| {
                requested.interleaved_sample_bytes_to_fill.fill(0);
                let _ = second_sender.send(());
            },
        ))
        .expect("a stopped stream restarts — a device left running would refuse with EBUSY");
    second_receiver
        .recv_timeout(PLAYBACK_DEADLINE)
        .expect("the replacing hand-off is asked for samples");

    let asked_by_the_restart = first_request_count.load(Ordering::Acquire);
    std::thread::sleep(HOW_LONG_A_STOPPED_STREAM_IS_WATCHED);
    assert_eq!(
        first_request_count.load(Ordering::Acquire),
        asked_by_the_restart,
        "the replaced hand-off was asked for samples after start_requesting_from installed \
         another"
    );

    playback_stream.stop_requesting().expect("stop");
}
