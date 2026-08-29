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
    AudioPlaybackStream, AudioStreamFormat, AudioStreamLivenessReport, RuntimeContextFullAccess,
    probe_audio_device_backend,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{AudioWindowContractMatchingADeviceStream, InputMailboxes};
use streamlib::sdk::processors::ManualProcessor;

use crate::audio_block::AudioBlock;
use crate::audio_samples_awaiting_playback_ring::{
    AudioSamplesAwaitingPlaybackRing, AudioSamplesHandOffOutcome,
};
use crate::consecutive_failure_report_schedule::ConsecutiveFailureReportSchedule;
use crate::cumulative_count_report_threshold::CumulativeCountReportThreshold;
use crate::processor_thread_join::join_within_grace_or_detach;

/// Device periods the ring holds before the drain thread waits for room.
/// Matches the ring depth a sample-stream link itself carries
/// (`DeliveryProfile::ORDERED_DEPTH`), so the buffering either side of the
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

/// Failed reads between reports. A graph publishing frames this cannot decode
/// fails every read — roughly 94 a second at the default quantum — and the
/// first line already says what the next thousand would.
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
        delivery_profile = "ordered",
        audio_window = match_device,
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
        // The port declared `audio_window = match_device`, and this is the
        // format it was waiting for: from here the read-side stage resamples,
        // converts channels and re-encodes every block into what this device
        // opened at, so a mono-preferring microphone drives a stereo-preferring
        // speaker with nothing between them. Window and hop are one device
        // period because the sink wants format conversion, not framing — under
        // an all-or-nothing contract that is how a converter is spelled.
        let one_device_period = a_device_periods_worth_of_per_channel_samples(stream_format);
        self.inputs
            .settle_a_ports_device_matched_audio_window_contract(
                ctx,
                AUDIO_INPUT_PORT,
                &AudioWindowContractMatchingADeviceStream {
                    device_stream_format: stream_format,
                    window_size_in_per_channel_samples: one_device_period,
                    hop_in_per_channel_samples: one_device_period,
                },
            )?;

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
        // The one `setup` stored, not a second one off the stream: on every arm
        // these are the same report, and depending on that is how it stays
        // true.
        let Some(playback_stream_liveness_report) = self.playback_stream_liveness_report.clone()
        else {
            return Err(Error::Configuration(
                "SpeakerSink: no playback stream liveness report. setup() must run first.".into(),
            ));
        };

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
            playback_device_failure = ?self
                .playback_stream_liveness_report
                .as_ref()
                .and_then(|report| report.failure_that_ended_the_stream()),
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

/// Ten milliseconds of this format in per-channel samples — the device period
/// this assumes where it has not yet been told one.
///
/// Written once because three callers depend on it moving together: the ring's
/// size, the interval between underrun reports, and the window the port's
/// `match_device` contract settles to are all stated in periods. Floored at one
/// sample so a stream reporting an absurd rate still settles a contract the
/// stage can honour rather than one refused for a zero window.
fn a_device_periods_worth_of_per_channel_samples(stream_format: AudioStreamFormat) -> u32 {
    (stream_format.sample_rate / 100).max(1)
}

/// One device period of this format, in bytes.
fn a_device_periods_worth_of_bytes(stream_format: AudioStreamFormat) -> usize {
    stream_format
        .interleaved_byte_count_for(a_device_periods_worth_of_per_channel_samples(stream_format))
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
    let mut underrun_reports = CumulativeCountReportThreshold::reporting_every(
        a_device_periods_worth_of_bytes(stream_format).max(1) as u64
            * SILENT_PERIODS_BETWEEN_UNDERRUN_WARNINGS,
    );

    while is_draining.load(Ordering::Acquire) {
        // Asked ahead of the underrun check, because a device that stopped
        // underruns for the rest of the run: the underrun line would be the
        // loudest thing in the log and the one that says the least.
        if let Some(reason) = playback_stream_liveness_report.failure_that_ended_the_stream() {
            tracing::error!(
                %reason,
                "SpeakerSink: the playback device stopped serving this processor, so nothing \
                 further will be played. Blocks still arriving on the input port are dropped \
                 by the link's own ring rather than queued for a device that is gone."
            );
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

        // Handed to the device as it stands, with nothing checked on the way:
        // the port declared `audio_window = match_device` and its read-side
        // stage emits windows already in this device's rate, channel count and
        // scalar encoding — and refuses a bag it cannot read, by name, at the
        // read above. A second check here would be the same refusal spelled
        // twice, and one of the two spellings would eventually be wrong.
        let samples = block.interleaved_sample_bytes.as_slice();

        // The wait is this sink pacing itself against its own ring — the
        // device-stall envelope — not backpressure any profile asks for. A
        // drain thread held here is not reading its mailbox, so a producer
        // outrunning this loop loses blocks at the port, counted there against
        // the link they arrived on.
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
        AudioClock, AudioClockConfig, AudioDeviceBackend, AudioSampleFormat,
        AudioStreamFailureReason, AudioStreamFailureRecorder, AudioTickCallback, AudioTickContext,
        SharedAudioClock, SilentNullAudioDeviceBackend,
    };

    use crate::emitted_log_line_test_support::{CountingTracingSubscriber, EmittedLogLineCounts};
    use crate::worker_thread_test_support::a_thread_that_finishes_within;

    const TEST_SAMPLE_RATE: u32 = 48_000;
    const TEST_QUANTUM_SAMPLES: usize = 512;

    /// Many idle turns of the drain loop, so a thread that only notices its
    /// device on some later condition has had every chance to.
    const HOW_LONG_A_DRAIN_THREAD_IS_WATCHED: Duration = Duration::from_millis(50);

    /// Generous next to the loop's own 1 ms park, so a busy machine cannot
    /// fail this and a loop that never asks cannot pass it.
    const HOW_LONG_A_DEAD_DEVICE_MAY_GO_UNNOTICED: Duration = Duration::from_secs(2);

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

    /// The declaration the whole rung turns on. The sink's input port asks the
    /// engine to window every block into whatever its own playback device
    /// opened at, and nothing else in this file can compensate if it stops:
    /// with the sentinel gone the read-side stage does not run, the refusal
    /// that used to catch a mismatched block is deleted, and a 16 kHz mono
    /// capture reaches a 48 kHz stereo device to be played at the wrong speed —
    /// audible, and indistinguishable from a device fault.
    #[test]
    fn the_audio_port_asks_the_engine_to_match_whatever_device_this_sink_opens() {
        use streamlib::sdk::processors::GeneratedProcessor;

        let descriptor = <SpeakerSink::Processor as GeneratedProcessor>::descriptor()
            .expect("a `#[processor]` type carries a descriptor");
        let audio = descriptor
            .inputs
            .iter()
            .find(|port| port.name == AUDIO_INPUT_PORT)
            .expect("the sink declares the port a microphone wires to");

        assert_eq!(
            audio.audio_window,
            Some(streamlib::sdk::descriptors::AudioWindowContract::MatchDevice {}),
            "the port must declare `audio_window = match_device` — five written values \
             would be a guess at a format that varies by machine"
        );
        assert_eq!(
            audio.delivery_profile.as_deref(),
            Some("ordered"),
            "a window contract requires `ordered`; `newest` passes over bags by design"
        );
    }

    /// Window and hop are one device period, so the stage converts format
    /// without also re-framing: a sink wants what its device can play, at the
    /// cadence its device asks for it.
    #[test]
    fn the_window_this_sink_settles_is_one_device_period_at_the_devices_own_rate() {
        assert_eq!(
            a_device_periods_worth_of_per_channel_samples(a_stream_format(48_000, 2)),
            480,
            "ten milliseconds at 48 kHz"
        );
        assert_eq!(
            a_device_periods_worth_of_bytes(a_stream_format(48_000, 2)),
            480 * 2 * 4,
            "the two must move together — the ring is sized in the periods the contract \
             frames in"
        );
        assert_eq!(
            a_device_periods_worth_of_per_channel_samples(a_stream_format(1, 1)),
            1,
            "a rate no period divides still settles a contract the stage can honour"
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
    /// Run on a thread and watched, rather than called inline: on a mental
    /// revert this loop parks on a 1 ms interval for the rest of the run, and
    /// an inline call would hang `cargo test` where this fails it.
    #[test]
    fn a_drain_thread_whose_device_died_comes_back_and_says_why() {
        let samples_awaiting_playback = Arc::new(
            AudioSamplesAwaitingPlaybackRing::with_byte_capacity(SMALLEST_USABLE_RING_BYTES),
        );
        let is_draining = Arc::new(AtomicBool::new(true));
        let played_block_counter = Arc::new(AtomicU64::new(0));

        let (failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();
        failure_recorder.record_the_failure_that_ended_the_stream(AudioStreamFailureReason::of(
            "the PipeWire stream stopped serving its device: node destroyed",
        ));

        let lines = Arc::new(EmittedLogLineCounts::default());
        let draining = std::thread::spawn({
            let samples_awaiting_playback = Arc::clone(&samples_awaiting_playback);
            let is_draining = Arc::clone(&is_draining);
            let played_block_counter = Arc::clone(&played_block_counter);
            let lines = Arc::clone(&lines);
            move || {
                // Installed inside the thread because the subscriber is
                // thread-local, and the loop under test runs here.
                tracing::subscriber::with_default(CountingTracingSubscriber(lines), || {
                    drain_blocks_into_playback(
                        &InputMailboxes::empty(),
                        &samples_awaiting_playback,
                        &is_draining,
                        &liveness_report,
                        &played_block_counter,
                        a_stream_format(48_000, 2),
                    );
                });
            }
        });

        assert!(
            a_thread_that_finishes_within(&draining, HOW_LONG_A_DEAD_DEVICE_MAY_GO_UNNOTICED),
            "the drain thread did not notice a device that had already died — it went on \
             parking against a device that is gone"
        );
        draining.join().expect("the drain thread ends");

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
        let (_failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();

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
