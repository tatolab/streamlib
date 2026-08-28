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
use std::time::Duration;

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioDeviceStreamRequest,
    AudioPlaybackStream, AudioSampleFormat, AudioStreamFormat, AudioStreamLivenessReport,
    RuntimeContextFullAccess, probe_audio_device_backend,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::InputMailboxes;
use streamlib::sdk::processors::ManualProcessor;

use crate::audio_block::{AudioBlock, AudioSampleDtype};
use crate::audio_device_serving_state::{
    AudioDeviceServingState, ask_whether_the_device_is_still_serving,
};
use crate::audio_samples_awaiting_playback_ring::{
    AudioSamplesAwaitingPlaybackRing, AudioSamplesHandOffOutcome,
};
use crate::consecutive_failure_report_schedule::ConsecutiveFailureReportSchedule;
use crate::cumulative_count_report_threshold::CumulativeCountReportThreshold;
use crate::processor_thread_join::join_within_grace_or_detach;

/// What a playback device that stopped means for this sink, said once when it
/// happens.
const WHAT_A_STOPPED_PLAYBACK_DEVICE_MEANS: &str = "SpeakerSink: the playback device stopped serving this processor, so nothing further      will be played. Blocks still arriving on the input port are read and discarded by the      link's own ring, not queued for a device that is gone.";

/// Device periods the ring holds before the drain thread waits for room.
/// Matches the ring depth a sample-stream link itself carries
/// (`DeliveryProfile::STREAM_DEPTH`), so the buffering either side of the
/// device edge is the same order of magnitude — roughly 170 ms at the default
/// 48 kHz / 512-sample quantum.
const DEVICE_PERIODS_THE_RING_HOLDS: usize = 16;

/// The smallest ring worth having, whatever a stream reports — one large
/// device period, so a ring can always hold at least one.
///
/// A floor on the whole ring rather than on the per-period estimate: at every
/// format the arms actually negotiate the estimate is the larger term and this
/// never binds, and a floor that quietly won at 48 kHz would be the real size
/// while reading as a fallback. It exists for a stream reporting a rate no
/// period count derives from. The ring's size is a buffering choice — a block
/// larger than it is queued in pieces, never refused — so it bounds nothing a
/// graph may publish.
const SMALLEST_USABLE_RING_BYTES: usize = 8 * 1024;

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
    /// Taken at open and kept beside the stream rather than read off it: the
    /// drain thread is what has to notice a device that stopped, and it never
    /// holds the stream.
    playback_stream_liveness_report: Option<AudioStreamLivenessReport>,
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
        self.playback_stream_liveness_report = Some(playback_stream.liveness_report());
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
        let playback_stream_liveness_report = playback_stream.liveness_report();

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
                        &playback_stream_liveness_report,
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
            // Beside the counts rather than only in the line that fired when
            // it happened: a run that ended early and a run that ended are the
            // same handful of numbers otherwise.
            playback_device_failure = self
                .playback_stream_liveness_report
                .as_ref()
                .and_then(|report| report.failure_that_ended_the_stream())
                .map_or_else(|| "none".to_string(), |reason| reason.to_string()),
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

        if let Some(handle) = self.drain_thread_handle.take() {
            join_within_grace_or_detach(
                handle,
                DRAIN_THREAD_EXIT_GRACE,
                "SpeakerSink: the drain thread",
            );
        }
    }
}

/// Ten milliseconds of this format, in bytes — the device period this assumes
/// where it has not yet been told one.
///
/// Written once because two callers depend on it moving together: the ring's
/// size and the interval between underrun reports are both stated in periods.
fn a_device_periods_worth_of_bytes(stream_format: AudioStreamFormat) -> usize {
    stream_format.interleaved_byte_count_for(stream_format.sample_rate / 100)
}

/// How many bytes of queued samples sit between the graph and the device.
fn ring_byte_capacity_for(stream_format: AudioStreamFormat) -> usize {
    (a_device_periods_worth_of_bytes(stream_format) * DEVICE_PERIODS_THE_RING_HOLDS)
        .max(SMALLEST_USABLE_RING_BYTES)
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
    playback_stream_liveness_report: &AudioStreamLivenessReport,
    played_block_counter: &AtomicU64,
    stream_format: AudioStreamFormat,
) {
    let mut read_failures =
        ConsecutiveFailureReportSchedule::reporting_every(FAILED_READS_BETWEEN_REPORTS);
    let mut refused_blocks =
        ConsecutiveFailureReportSchedule::reporting_every(REFUSED_BLOCKS_BETWEEN_REPORTS);
    let mut underrun_reports = CumulativeCountReportThreshold::reporting_every(
        a_device_periods_worth_of_bytes(stream_format).max(1) as u64
            * SILENT_PERIODS_BETWEEN_UNDERRUN_WARNINGS,
    );

    while is_draining.load(Ordering::Acquire) {
        // Asked ahead of the underrun check, because a device that stopped
        // underruns for the rest of the run: the underrun line would be the
        // loudest thing in the log and the one that says the least.
        if ask_whether_the_device_is_still_serving(
            playback_stream_liveness_report,
            WHAT_A_STOPPED_PLAYBACK_DEVICE_MEANS,
        ) == AudioDeviceServingState::StoppedServing
        {
            break;
        }
        // Judged on the idle path too, and this is the case that needs it
        // most: a producer that stopped entirely underruns the device for as
        // long as the graph runs, and a check reached only after a successful
        // hand-off would never speak again once the last block arrived.
        warn_about_any_new_underruns(samples_awaiting_playback, &mut underrun_reports);

        if !inputs.has_data(AUDIO_INPUT_PORT) {
            std::thread::park_timeout(DRAIN_IDLE_PARK_INTERVAL);
            continue;
        }
        let block: AudioBlock = match inputs.read(AUDIO_INPUT_PORT) {
            Ok(block) => {
                read_failures.note_success();
                block
            }
            Err(e) => {
                // A port that reported data and then refused to hand it over:
                // a payload that would not deserialize as an `AudioBlock`, or
                // one another reader took first. The error says which; this
                // must not guess.
                if read_failures.note_failure_and_say_whether_to_report() {
                    tracing::error!(
                        consecutive_failures = read_failures.consecutive_failures(),
                        error = %e,
                        "SpeakerSink: failed to read an audio block"
                    );
                }
                // Parked like an empty port rather than retried at once: a
                // producer publishing frames this cannot decode would
                // otherwise spin this thread at full tilt against a device
                // that only needs it every quantum.
                std::thread::park_timeout(DRAIN_IDLE_PARK_INTERVAL);
                continue;
            }
        };

        let samples = match samples_of_a_block_the_device_can_play(&block, stream_format) {
            Ok(samples) => {
                refused_blocks.note_success();
                samples
            }
            Err(refusal) => {
                if refused_blocks.note_failure_and_say_whether_to_report() {
                    tracing::error!(
                        refused_blocks = refused_blocks.consecutive_failures(),
                        "SpeakerSink: {refusal}"
                    );
                }
                continue;
            }
        };

        // The wait here is the backpressure the port's `lossless` profile asks
        // for: a drain thread held for room is a drain thread not reading its
        // mailbox. What that buys today is bounded queueing rather than the
        // guarantee the profile names — `PortMailbox::push` drops its oldest
        // entry whenever it is full, whatever the profile says — so a producer
        // that outruns this loop still loses blocks upstream of it.
        if samples_awaiting_playback.hand_off_for_playback(samples, ROOM_WAIT_POLL_INTERVAL)
            == AudioSamplesHandOffOutcome::PlaybackEnded
        {
            break;
        }
        played_block_counter.fetch_add(1, Ordering::Relaxed);
    }
}

fn warn_about_any_new_underruns(
    samples_awaiting_playback: &AudioSamplesAwaitingPlaybackRing,
    underrun_reports: &mut CumulativeCountReportThreshold,
) {
    let underrun_byte_count = samples_awaiting_playback.underrun_byte_count();
    if !underrun_reports.count_is_worth_reporting(underrun_byte_count) {
        return;
    }
    tracing::warn!(
        underrun_bytes = underrun_byte_count,
        "SpeakerSink: the device was given silence because the graph had no samples ready. \
         The count is how much, and it is never filled without being counted."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;
    use streamlib::sdk::context::{
        AudioClock, AudioClockConfig, AudioDeviceBackend, AudioStreamFailureReason,
        AudioTickCallback, AudioTickContext, SharedAudioClock, SilentNullAudioDeviceBackend,
    };

    use crate::emitted_log_line_test_support::{CountingTracingSubscriber, EmittedLines};

    const TEST_SAMPLE_RATE: u32 = 48_000;
    const TEST_QUANTUM_SAMPLES: usize = 512;

    /// Many idle turns of the drain loop, so a thread that only notices its
    /// device on some later condition has had every chance to.
    const HOW_LONG_A_DRAIN_THREAD_IS_WATCHED: Duration = Duration::from_millis(50);

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

    /// What the graph queued is what the device is *given* — read back out of
    /// the buffer the arm filled, not inferred from a counter that stayed at
    /// zero.
    ///
    /// Mental revert: hand the stream a no-op callback instead of one that
    /// fills from the ring, and this reddens on the bytes. An assertion on the
    /// underrun count alone would not have: zero is also what a device that
    /// never asked reports.
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
        let _ = samples_awaiting_playback
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
        assert_eq!(
            ring_byte_capacity_for(a_stream_format(1, 1)),
            SMALLEST_USABLE_RING_BYTES,
            "a rate no period derives from falls back to the floor"
        );
    }

    /// And at the format the arms actually negotiate, the floor does not bind
    /// — otherwise it would be the ring's real size while reading as a
    /// fallback nobody expects to reach.
    #[test]
    fn at_a_real_format_the_ring_is_sized_by_the_device_period_not_the_floor() {
        let stream_format = a_stream_format(48_000, 2);
        assert_eq!(
            ring_byte_capacity_for(stream_format),
            a_device_periods_worth_of_bytes(stream_format) * DEVICE_PERIODS_THE_RING_HOLDS
        );
        assert!(ring_byte_capacity_for(stream_format) > SMALLEST_USABLE_RING_BYTES);
    }

    /// The sink's half of the seam's point: a drain thread comes back from a
    /// device that died instead of feeding a ring nothing will ever play, and
    /// says why once.
    ///
    /// Mental revert: drop the liveness check from the loop and this parks on
    /// a 1 ms interval for the rest of the run, warning about underruns that
    /// are the consequence rather than the cause — the loudest line in the log
    /// and the one that says the least.
    #[test]
    fn a_drain_thread_whose_device_died_comes_back_and_says_why() {
        let samples_awaiting_playback =
            AudioSamplesAwaitingPlaybackRing::with_byte_capacity(SMALLEST_USABLE_RING_BYTES);
        let is_draining = AtomicBool::new(true);
        let played_block_counter = AtomicU64::new(0);

        let liveness_report = AudioStreamLivenessReport::of_a_stream_that_has_not_failed();
        liveness_report.record_the_failure_that_ended_the_stream(AudioStreamFailureReason::of(
            "the PipeWire stream stopped serving its device: node destroyed",
        ));

        let lines = Arc::new(EmittedLines::default());
        let draining_began = Instant::now();
        tracing::subscriber::with_default(CountingTracingSubscriber(Arc::clone(&lines)), || {
            drain_blocks_into_playback(
                &InputMailboxes::empty(),
                &samples_awaiting_playback,
                &is_draining,
                &liveness_report,
                &played_block_counter,
                a_stream_format(48_000, 2),
            );
        });
        let draining_took = draining_began.elapsed();

        assert!(
            draining_took < HOW_LONG_A_DRAIN_THREAD_IS_WATCHED,
            "the drain thread took {draining_took:?} to notice a device that had already \
             died — it went on parking against a device that is gone"
        );
        assert!(
            is_draining.load(Ordering::Acquire),
            "the loop left on the device's account, not because it was told to stop — the \
             two have to stay distinguishable"
        );
        assert_eq!(
            lines.errors.load(Ordering::Relaxed),
            1,
            "a device that stopped is said once, at error, not once per 1 ms turn"
        );
        assert_eq!(
            lines.warnings.load(Ordering::Relaxed),
            0,
            "the underruns a dead device causes are the consequence — reporting them here \
             sends a reader after the producer"
        );
    }

    /// The other half, and the one that keeps the first honest: a sink whose
    /// device is fine is not talked out of its own loop.
    #[test]
    fn a_drain_thread_whose_device_is_healthy_keeps_draining() {
        let samples_awaiting_playback = Arc::new(
            AudioSamplesAwaitingPlaybackRing::with_byte_capacity(SMALLEST_USABLE_RING_BYTES),
        );
        let is_draining = Arc::new(AtomicBool::new(true));
        let played_block_counter = Arc::new(AtomicU64::new(0));
        let liveness_report = AudioStreamLivenessReport::of_a_stream_that_has_not_failed();

        let draining = std::thread::spawn({
            let samples_awaiting_playback = Arc::clone(&samples_awaiting_playback);
            let is_draining = Arc::clone(&is_draining);
            let played_block_counter = Arc::clone(&played_block_counter);
            let liveness_report = liveness_report.clone();
            move || {
                drain_blocks_into_playback(
                    &InputMailboxes::empty(),
                    &samples_awaiting_playback,
                    &is_draining,
                    &liveness_report,
                    &played_block_counter,
                    a_stream_format(48_000, 2),
                );
            }
        });

        std::thread::sleep(HOW_LONG_A_DRAIN_THREAD_IS_WATCHED);
        assert!(
            !draining.is_finished(),
            "a healthy device was reported as dead, which makes the signal worth nothing"
        );

        is_draining.store(false, Ordering::Release);
        samples_awaiting_playback.end_playback();
        draining.join().expect("the drain thread ends");
    }
}
