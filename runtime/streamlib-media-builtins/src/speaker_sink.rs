// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in audio playback: read `AudioBlock` → bounded hand-off → device
//! callback.
//!
//! `MicrophoneSource` mirrored. Written against the engine's audio device seam,
//! so it reaches hardware the way every other built-in reaches its device class
//! and runs unchanged on a machine with no audio libraries at all — there it
//! discards rather than failing to start.
//!
//! Playback is plain on this rung: no immediate cancel and no played-up-to
//! timestamps. Those are the barge-in door and the AEC reference, they are one
//! mechanism, and the plan lands them together on the conditioning rung
//! (`docs/plan/ARCHITECTURE.md` §Media I/O `[audio-subsystem]`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioDeviceStreamRequest,
    AudioPlaybackStream, AudioSampleFormat, AudioStreamFormat, RuntimeContextFullAccess,
    probe_audio_device_backend,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::InputMailboxes;
use streamlib::sdk::processors::ManualProcessor;

use crate::audio_block::{AudioBlock, AudioSampleDtype};
use crate::audio_samples_awaiting_playback_ring::{
    AudioSamplesAwaitingPlaybackRing, AudioSamplesHandOffOutcome,
};
use crate::consecutive_failure_report_schedule::ConsecutiveFailureReportSchedule;

/// Device periods the ring holds before the drain thread waits for room.
/// Matches the ring depth a sample-stream link itself carries
/// (`DeliveryProfile::STREAM_DEPTH`), so the buffering either side of the
/// device edge is the same order of magnitude — roughly 170 ms at the default
/// 48 kHz / 512-sample quantum.
const DEVICE_PERIODS_THE_RING_HOLDS: usize = 16;

/// A period at 48 kHz stereo `f32`, used only to size the ring when a stream
/// reports a rate a period count cannot be derived from. The ring's size is a
/// buffering choice — a block larger than it is queued in pieces, never
/// refused — so this floor bounds nothing a graph may publish.
const A_DEVICE_PERIOD_WORTH_OF_BYTES: usize = 512 * 2 * 4;

/// How long the drain thread parks when the input has no block to play. Short
/// against a device quantum, so a block that arrives just after a poll is not
/// late by anything the device would notice.
const DRAIN_IDLE_PARK_INTERVAL: Duration = Duration::from_millis(1);

/// How long the drain thread sits inside a wait for ring room before it comes
/// up for air. Stopping ends playback, which releases it for good; this only
/// bounds a wait that nothing woke.
const ROOM_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Refused blocks between reports once the first has been logged. A misconfigured
/// graph refuses every block — roughly 94 a second at the default quantum — and
/// the first line already says what the next thousand would.
const REFUSED_BLOCKS_BETWEEN_REPORTS: u64 = 300;

/// Failed reads between reports, on the same reasoning.
const FAILED_READS_BETWEEN_REPORTS: u64 = 300;

/// Ten-millisecond periods of silence between underrun warnings — roughly
/// three seconds. A graph that is not keeping up underruns at device cadence,
/// and saying so once per period would bury the reason under the symptom.
const SILENT_PERIODS_BETWEEN_UNDERRUN_WARNINGS: u64 = 300;

/// The one port a block to play arrives on.
const AUDIO_INPUT_PORT: &str = "audio";

/// Bound on the wait for the drain thread to exit.
const DRAIN_THREAD_EXIT_GRACE: Duration = Duration::from_secs(2);

/// Configuration for [`SpeakerSink`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SpeakerSinkConfig {
    /// Backend-named playback device. Absent: the backend's default device.
    /// A name the backend cannot open raises rather than landing on a
    /// different device — a wrong device id is a wiring error.
    #[serde(default)]
    pub device_id: Option<String>,
}

#[streamlib::sdk::processor(
    description = "Plays timestamped blocks of interleaved samples on the machine's audio backend (discarding where no backend exists)",
    execution = manual,
    scheduling = realtime,
    config = crate::speaker_sink::SpeakerSinkConfig,
    input(
        "audio",
        delivery_profile = "lossless",
        description = "Timestamped blocks of interleaved samples to play"
    ),
)]
pub struct SpeakerSink {
    playback_stream: Option<Box<dyn AudioPlaybackStream>>,
    samples_awaiting_playback: Option<Arc<AudioSamplesAwaitingPlaybackRing>>,
    /// Minted per drain thread, so a thread that had to be detached can never
    /// be revived by a later `start()` into an endless spin.
    is_draining: Option<Arc<AtomicBool>>,
    played_block_counter: Arc<AtomicU64>,
    drain_thread_handle: Option<JoinHandle<()>>,
}

impl ManualProcessor for SpeakerSink::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let backend = probe_audio_device_backend();
        let playback_stream = backend.open_playback_stream(&AudioDeviceStreamRequest {
            device_id: self.config.device_id.clone(),
            deviceless_pacing_clock: Arc::clone(ctx.audio_clock()),
        })?;
        let stream_format = playback_stream.stream_format();
        tracing::info!(
            audio_backend = backend.backend_name(),
            device_id = self.config.device_id.as_deref().unwrap_or("<default>"),
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            sample_format = ?stream_format.sample_format,
            "SpeakerSink: playback stream opened"
        );
        self.playback_stream = Some(playback_stream);
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let Some(playback_stream) = self.playback_stream.as_mut() else {
            return Err(Error::Configuration(
                "SpeakerSink: no playback stream is open. setup() must run first.".into(),
            ));
        };
        let stream_format = playback_stream.stream_format();

        let samples_awaiting_playback =
            Arc::new(AudioSamplesAwaitingPlaybackRing::with_byte_capacity(
                ring_byte_capacity_for(stream_format),
            ));
        playback_stream.start_requesting_from(device_callback_filling_from(Arc::clone(
            &samples_awaiting_playback,
        )))?;

        let is_draining = Arc::new(AtomicBool::new(true));
        let handle = std::thread::Builder::new()
            .name("audio-playback-drain".to_string())
            .spawn({
                let inputs: InputMailboxes = self.inputs.clone();
                let samples_awaiting_playback = Arc::clone(&samples_awaiting_playback);
                let is_draining = Arc::clone(&is_draining);
                let played_block_counter = Arc::clone(&self.played_block_counter);
                move || {
                    drain_blocks_into_playback(
                        &inputs,
                        &samples_awaiting_playback,
                        &is_draining,
                        &played_block_counter,
                        stream_format,
                    );
                }
            })
            .map_err(|e| {
                Error::Runtime(format!(
                    "SpeakerSink: failed to spawn the drain thread: {e}"
                ))
            })?;

        self.samples_awaiting_playback = Some(samples_awaiting_playback);
        self.is_draining = Some(is_draining);
        self.drain_thread_handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_draining();
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_draining();
        let samples_awaiting_playback = self.samples_awaiting_playback.as_ref();
        tracing::info!(
            played_blocks = self.played_block_counter.load(Ordering::Relaxed),
            underrun_bytes = samples_awaiting_playback.map_or(0, |ring| ring.underrun_byte_count()),
            // Reported beside the underruns rather than folded into them: this
            // is the cost of starting, and a reader comparing two runs needs to
            // see which of the two moved.
            silence_before_playback_began_bytes = samples_awaiting_playback.map_or(0, |ring| ring
                .silence_played_before_the_cushion_filled_byte_count()),
            "SpeakerSink: teardown"
        );
        self.playback_stream = None;
        Ok(())
    }
}

impl SpeakerSink::Processor {
    fn stop_draining(&mut self) {
        if let Some(playback_stream) = self.playback_stream.as_mut()
            && let Err(e) = playback_stream.stop_requesting()
        {
            tracing::warn!(error = %e, "SpeakerSink: playback stream failed to stop");
        }
        if let Some(is_draining) = self.is_draining.take() {
            is_draining.store(false, Ordering::Release);
        }
        if let Some(samples_awaiting_playback) = &self.samples_awaiting_playback {
            samples_awaiting_playback.end_playback();
        }

        let Some(handle) = self.drain_thread_handle.take() else {
            return;
        };
        let deadline = Instant::now() + DRAIN_THREAD_EXIT_GRACE;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if handle.is_finished() {
            let _ = handle.join();
        } else {
            tracing::warn!(
                "SpeakerSink: drain thread did not exit within {:?}, detaching",
                DRAIN_THREAD_EXIT_GRACE
            );
        }
    }
}

/// How many bytes of queued samples sit between the graph and the device.
fn ring_byte_capacity_for(stream_format: AudioStreamFormat) -> usize {
    let device_period_bytes = stream_format
        .interleaved_byte_count_for(stream_format.sample_rate / 100)
        .max(A_DEVICE_PERIOD_WORTH_OF_BYTES);
    device_period_bytes * DEVICE_PERIODS_THE_RING_HOLDS
}

/// The callback the playback stream asks for samples through.
///
/// Named rather than written inline at its one call site so a test can hold
/// what the device edge actually promises: the callback takes what is queued
/// and returns, and a graph that stops supplying costs counted silence instead
/// of stalling the device's own thread.
fn device_callback_filling_from(
    samples_awaiting_playback: Arc<AudioSamplesAwaitingPlaybackRing>,
) -> AudioBlockForPlaybackHandOff {
    Box::new(move |requested: AudioBlockRequestedByDevice<'_>| {
        samples_awaiting_playback
            .fill_one_device_period(requested.interleaved_sample_bytes_to_fill);
    })
}

/// How a wire dtype is spelled as a stream's scalar encoding.
fn stream_sample_format_for(dtype: AudioSampleDtype) -> AudioSampleFormat {
    match dtype {
        AudioSampleDtype::F32 => AudioSampleFormat::F32,
        AudioSampleDtype::I16 => AudioSampleFormat::I16,
    }
}

/// Why the device cannot play a block as it stands, in the words the refusal
/// carries.
///
/// A `Result<(), String>` rather than a bare bool because a refusal that does
/// not name both the block's format and the device's sends a reader to the
/// wrong end of the graph — and there is no resampler on this rung, so the
/// wiring is the only thing that can be fixed.
fn refuse_a_block_the_device_cannot_play(
    block: &AudioBlock,
    stream_format: AudioStreamFormat,
) -> std::result::Result<(), String> {
    let block_sample_format = stream_sample_format_for(block.dtype);
    if block.sample_rate == stream_format.sample_rate
        && block.channels == stream_format.channels
        && block_sample_format == stream_format.sample_format
    {
        return Ok(());
    }
    Err(format!(
        "a block of {} Hz / {} channels / {:?} cannot be played on a device running at {} Hz \
         / {} channels / {:?}. There is no resampler on this rung, so the block is refused \
         rather than adapted — silently playing it would be indistinguishable from working \
         code. Publish blocks in the device's format, or name a device that matches.",
        block.sample_rate,
        block.channels,
        block_sample_format,
        stream_format.sample_rate,
        stream_format.channels,
        stream_format.sample_format,
    ))
}

/// The bytes of one block, refused rather than adapted when the device cannot
/// play them as they stand.
///
/// Also refuses a block whose payload is not `sample_count × channels × width`:
/// playing it would push every later sample out of phase by the shortfall,
/// which sounds like a device fault rather than a producer one.
fn samples_of_a_block_the_device_can_play(
    block: &AudioBlock,
    stream_format: AudioStreamFormat,
) -> std::result::Result<&[u8], String> {
    refuse_a_block_the_device_cannot_play(block, stream_format)?;

    let expected_byte_count = stream_format.interleaved_byte_count_for(block.sample_count);
    if block.interleaved_sample_bytes.len() != expected_byte_count {
        return Err(format!(
            "a block declaring {} samples of {} channels at {:?} carries {} bytes where {} \
             describes it — the payload and the header disagree, and playing either reading \
             would put every later sample out of phase",
            block.sample_count,
            block.channels,
            block.dtype,
            block.interleaved_sample_bytes.len(),
            expected_byte_count,
        ));
    }
    Ok(&block.interleaved_sample_bytes)
}

fn drain_blocks_into_playback(
    inputs: &InputMailboxes,
    samples_awaiting_playback: &AudioSamplesAwaitingPlaybackRing,
    is_draining: &AtomicBool,
    played_block_counter: &AtomicU64,
    stream_format: AudioStreamFormat,
) {
    let mut read_failures =
        ConsecutiveFailureReportSchedule::reporting_every(FAILED_READS_BETWEEN_REPORTS);
    let mut refused_blocks =
        ConsecutiveFailureReportSchedule::reporting_every(REFUSED_BLOCKS_BETWEEN_REPORTS);
    let mut warn_at_underrun_byte_count = 1u64;
    let underrun_bytes_between_warnings = stream_format
        .interleaved_byte_count_for(stream_format.sample_rate / 100)
        .max(1) as u64
        * SILENT_PERIODS_BETWEEN_UNDERRUN_WARNINGS;

    while is_draining.load(Ordering::Acquire) {
        if !inputs.has_data(AUDIO_INPUT_PORT) {
            std::thread::park_timeout(DRAIN_IDLE_PARK_INTERVAL);
            continue;
        }
        let block: AudioBlock = match inputs.read(AUDIO_INPUT_PORT) {
            Ok(block) => {
                read_failures.note_attempt_and_say_whether_to_report(true);
                block
            }
            Err(e) => {
                // A port that reported data and then refused to hand it over:
                // a payload that would not deserialize as an `AudioBlock`, or
                // one another reader took first. The error says which; this
                // must not guess.
                if read_failures.note_attempt_and_say_whether_to_report(false) {
                    tracing::error!(
                        consecutive_failures = read_failures.consecutive_failures(),
                        error = %e,
                        "SpeakerSink: failed to read an audio block"
                    );
                }
                continue;
            }
        };

        let samples = match samples_of_a_block_the_device_can_play(&block, stream_format) {
            Ok(samples) => {
                refused_blocks.note_attempt_and_say_whether_to_report(true);
                samples
            }
            Err(refusal) => {
                if refused_blocks.note_attempt_and_say_whether_to_report(false) {
                    tracing::error!(
                        refused_blocks = refused_blocks.consecutive_failures(),
                        "SpeakerSink: {refusal}"
                    );
                }
                continue;
            }
        };

        // The wait here is the backpressure the port's `lossless` profile
        // promises: a drain thread held for room is a drain thread not reading
        // its mailbox, so the producer blocks rather than anything being
        // dropped.
        if samples_awaiting_playback.hand_off_for_playback(samples, ROOM_WAIT_POLL_INTERVAL)
            == AudioSamplesHandOffOutcome::PlaybackEnded
        {
            break;
        }
        played_block_counter.fetch_add(1, Ordering::Relaxed);
        warn_about_any_new_underruns(
            samples_awaiting_playback,
            &mut warn_at_underrun_byte_count,
            underrun_bytes_between_warnings,
        );
    }
}

fn warn_about_any_new_underruns(
    samples_awaiting_playback: &AudioSamplesAwaitingPlaybackRing,
    warn_at_underrun_byte_count: &mut u64,
    underrun_bytes_between_warnings: u64,
) {
    let underrun_byte_count = samples_awaiting_playback.underrun_byte_count();
    if underrun_byte_count < *warn_at_underrun_byte_count {
        return;
    }
    tracing::warn!(
        underrun_bytes = underrun_byte_count,
        "SpeakerSink: the device was given silence because the graph had no samples ready. \
         The count is how much, and it is never filled without being counted."
    );
    *warn_at_underrun_byte_count = underrun_byte_count + underrun_bytes_between_warnings;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use streamlib::sdk::context::{
        AudioClock, AudioClockConfig, AudioDeviceBackend, AudioTickCallback, AudioTickContext,
        SharedAudioClock, SilentNullAudioDeviceBackend,
    };

    const TEST_SAMPLE_RATE: u32 = 48_000;
    const TEST_QUANTUM_SAMPLES: usize = 512;

    /// An [`AudioClock`] whose ticks the test fires by hand, so what is under
    /// test is the wiring rather than how promptly a timer thread woke.
    struct HandFiredTestAudioClock {
        config: AudioClockConfig,
        callbacks: Mutex<Vec<AudioTickCallback>>,
        next_tick_number: AtomicU64,
    }

    impl HandFiredTestAudioClock {
        fn new() -> Self {
            Self {
                config: AudioClockConfig::new(TEST_SAMPLE_RATE, TEST_QUANTUM_SAMPLES),
                callbacks: Mutex::new(Vec::new()),
                next_tick_number: AtomicU64::new(0),
            }
        }

        fn fire_one_tick(&self) {
            let tick = AudioTickContext {
                timestamp_ns: 1_000_000_000,
                samples_needed: self.config.buffer_size,
                sample_rate: self.config.sample_rate,
                tick_number: self.next_tick_number.fetch_add(1, Ordering::SeqCst),
            };
            for callback in self.callbacks.lock().expect("test clock lock").iter() {
                callback(tick);
            }
        }
    }

    impl AudioClock for HandFiredTestAudioClock {
        fn on_tick(&self, callback: AudioTickCallback) {
            self.callbacks
                .lock()
                .expect("test clock lock")
                .push(callback);
        }
        fn sample_rate(&self) -> u32 {
            self.config.sample_rate
        }
        fn buffer_size(&self) -> usize {
            self.config.buffer_size
        }
        fn start(&self) -> Result<()> {
            Ok(())
        }
        fn stop(&self) -> Result<()> {
            Ok(())
        }
        fn is_running(&self) -> bool {
            true
        }
    }

    fn a_stream_format(sample_rate: u32, channels: u32) -> AudioStreamFormat {
        AudioStreamFormat {
            sample_rate,
            channels,
            sample_format: AudioSampleFormat::F32,
        }
    }

    fn a_block(sample_rate: u32, channels: u32, dtype: AudioSampleDtype) -> AudioBlock {
        const SAMPLE_COUNT: u32 = 4;
        AudioBlock {
            interleaved_sample_bytes: vec![
                0u8;
                SAMPLE_COUNT as usize
                    * channels as usize
                    * dtype.bytes_per_sample()
            ],
            sample_rate,
            channels,
            sample_count: SAMPLE_COUNT,
            dtype,
            first_sample_timestamp_ns: 0,
        }
    }

    /// The device edge's whole contract, held where it is claimed: the callback
    /// the sink installs takes what is queued and returns, so a graph that
    /// never supplies costs counted silence rather than the device's own
    /// thread.
    ///
    /// Mental revert: have the callback wait for a block instead of taking what
    /// is there, and a stalled graph stops the sound card.
    #[test]
    fn the_device_callback_takes_what_is_queued_and_never_waits_for_the_graph() {
        const PERIODS_NOBODY_SUPPLIES: usize = 10;

        let clock = Arc::new(HandFiredTestAudioClock::new());
        // The null arm by name rather than through the chain's probe: what is
        // under test is the ring at the device edge, and the probe would hand
        // back whatever audio server the machine running this happens to have —
        // a device that paces itself and ignores a hand-fired clock.
        let mut playback_stream = SilentNullAudioDeviceBackend
            .open_playback_stream(&AudioDeviceStreamRequest {
                device_id: None,
                deviceless_pacing_clock: Arc::clone(&clock) as SharedAudioClock,
            })
            .expect("the null backend opens its default device on any machine");
        let stream_format = playback_stream.stream_format();

        let samples_awaiting_playback =
            Arc::new(AudioSamplesAwaitingPlaybackRing::with_byte_capacity(
                ring_byte_capacity_for(stream_format),
            ));
        playback_stream
            .start_requesting_from(device_callback_filling_from(Arc::clone(
                &samples_awaiting_playback,
            )))
            .expect("start requesting");

        let playing_began = Instant::now();
        for _ in 0..PERIODS_NOBODY_SUPPLIES {
            clock.fire_one_tick();
        }
        let playing_took = playing_began.elapsed();

        assert!(
            playing_took < ROOM_WAIT_POLL_INTERVAL,
            "{PERIODS_NOBODY_SUPPLIES} periods with nothing queued took {playing_took:?} — a \
             device callback waited on the graph"
        );
        assert_eq!(
            samples_awaiting_playback.silence_played_before_the_cushion_filled_byte_count(),
            (PERIODS_NOBODY_SUPPLIES
                * stream_format.interleaved_byte_count_for(TEST_QUANTUM_SAMPLES as u32))
                as u64,
            "every byte of silence the device was given is counted, whichever counter it \
             belongs on"
        );
        assert_eq!(
            samples_awaiting_playback.underrun_byte_count(),
            0,
            "a stream whose cushion never filled has not fallen behind — it never started"
        );
    }

    /// What the graph queued is what the device is given, period by period, on
    /// the arm a container runs.
    #[test]
    fn samples_the_graph_queued_reach_the_devices_own_callback() {
        let clock = Arc::new(HandFiredTestAudioClock::new());
        let mut playback_stream = SilentNullAudioDeviceBackend
            .open_playback_stream(&AudioDeviceStreamRequest {
                device_id: None,
                deviceless_pacing_clock: Arc::clone(&clock) as SharedAudioClock,
            })
            .expect("the null backend opens its default device on any machine");
        let stream_format = playback_stream.stream_format();

        let samples_awaiting_playback =
            Arc::new(AudioSamplesAwaitingPlaybackRing::with_byte_capacity(
                ring_byte_capacity_for(stream_format),
            ));
        playback_stream
            .start_requesting_from(device_callback_filling_from(Arc::clone(
                &samples_awaiting_playback,
            )))
            .expect("start requesting");

        // Enough to fill the cushion the ring starts on and serve a period
        // after it, so this is about serving rather than about starting.
        let one_period_of_bytes =
            stream_format.interleaved_byte_count_for(TEST_QUANTUM_SAMPLES as u32);
        samples_awaiting_playback
            .hand_off_for_playback(&vec![0x7Fu8; one_period_of_bytes * 3], Duration::ZERO);
        for _ in 0..3 {
            clock.fire_one_tick();
        }

        assert_eq!(
            samples_awaiting_playback.underrun_byte_count(),
            0,
            "periods the graph supplied in full are not underruns"
        );
    }

    /// The refusal the whole rung turns on: no resampler exists yet, so a block
    /// the device cannot play is named rather than adapted.
    ///
    /// Mental revert: accept a mismatched rate and the device plays it at the
    /// wrong speed — audible, but indistinguishable from a device fault, and
    /// exactly the wiring error the next rung's port window contract removes.
    #[test]
    fn a_block_at_the_wrong_rate_is_refused_naming_both_rates() {
        let refusal = refuse_a_block_the_device_cannot_play(
            &a_block(16_000, 1, AudioSampleDtype::F32),
            a_stream_format(48_000, 1),
        )
        .expect_err("16 kHz cannot be played on a 48 kHz device");
        assert!(
            refusal.contains("16000") && refusal.contains("48000"),
            "the refusal must name what it got and what the device wants: {refusal}"
        );
    }

    #[test]
    fn a_block_with_the_wrong_channel_count_is_refused_naming_both_counts() {
        let refusal = refuse_a_block_the_device_cannot_play(
            &a_block(48_000, 1, AudioSampleDtype::F32),
            a_stream_format(48_000, 2),
        )
        .expect_err("a mono block cannot be played as a stereo one");
        assert!(
            refusal.contains("1 channels") && refusal.contains("2 channels"),
            "the refusal must name both channel counts: {refusal}"
        );
    }

    /// A dtype mismatch reads every scalar at the wrong width, so it is the
    /// mismatch that produces noise rather than a wrong speed.
    #[test]
    fn a_block_in_the_wrong_dtype_is_refused_rather_than_reinterpreted() {
        let refusal = refuse_a_block_the_device_cannot_play(
            &a_block(48_000, 2, AudioSampleDtype::I16),
            a_stream_format(48_000, 2),
        )
        .expect_err("i16 scalars cannot be handed to an f32 device");
        assert!(
            refusal.contains("I16") && refusal.contains("F32"),
            "the refusal must name both encodings: {refusal}"
        );
    }

    #[test]
    fn a_block_the_device_can_play_is_handed_over_whole() {
        let block = a_block(48_000, 2, AudioSampleDtype::F32);
        let samples = samples_of_a_block_the_device_can_play(&block, a_stream_format(48_000, 2))
            .expect("a matching block plays");
        assert_eq!(samples.len(), block.interleaved_sample_bytes.len());
    }

    /// A header that disagrees with its payload would push every later sample
    /// out of phase by the shortfall, which sounds like a device fault rather
    /// than the producer bug it is.
    #[test]
    fn a_block_whose_payload_contradicts_its_header_is_refused_naming_both_lengths() {
        let mut block = a_block(48_000, 2, AudioSampleDtype::F32);
        block.interleaved_sample_bytes.truncate(4);
        let refusal = samples_of_a_block_the_device_can_play(&block, a_stream_format(48_000, 2))
            .expect_err("a truncated payload cannot describe 4 stereo samples");
        assert!(
            refusal.contains('4') && refusal.contains("32"),
            "the refusal must name what arrived and what was described: {refusal}"
        );
    }

    /// `rt.add(SpeakerSink)` sends `{}` to the engine, and every field of a
    /// built-in's config has to deserialize from it — the spelling the plan
    /// blesses for a block that needs no configuration.
    #[test]
    fn a_config_given_no_fields_at_all_takes_the_backends_default_device() {
        let config: SpeakerSinkConfig =
            serde_json::from_str("{}").expect("an empty config object deserializes");
        assert_eq!(config, SpeakerSinkConfig { device_id: None });
    }

    /// The ring has to hold more than one period, or the drain thread waits on
    /// the device for every block and the graph paces on the callback.
    #[test]
    fn the_ring_holds_several_device_periods_rather_than_one() {
        let stream_format = a_stream_format(48_000, 2);
        let one_period = stream_format.interleaved_byte_count_for(480);
        assert!(
            ring_byte_capacity_for(stream_format) >= one_period * DEVICE_PERIODS_THE_RING_HOLDS,
            "a ring of one period makes every hand-off wait for the device"
        );
    }

    /// A device reporting an implausibly low rate must still get a ring a real
    /// block fits in, because the ring's size is a buffering choice and never a
    /// limit on what a graph may publish.
    #[test]
    fn a_stream_reporting_a_tiny_rate_still_gets_a_usable_ring() {
        assert!(ring_byte_capacity_for(a_stream_format(1, 1)) >= A_DEVICE_PERIOD_WORTH_OF_BYTES);
    }
}
