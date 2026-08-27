// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in audio capture: device callback → bounded hand-off → published
//! `AudioBlock`.
//!
//! Written against the engine's audio device seam, so it reaches hardware the
//! way every other built-in reaches its device class and runs unchanged on a
//! machine with no audio libraries at all — there it captures silence rather
//! than failing to start.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    AudioCaptureSampleFormat, AudioCaptureStream, AudioCaptureStreamFormat,
    AudioCaptureStreamRequest, CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
    RuntimeContextFullAccess, probe_audio_device_backend,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::processors::ManualProcessor;

use crate::audio_block::{AudioBlock, AudioSampleDtype};
use crate::captured_audio_block_hand_off_ring::{
    CapturedAudioBlockAwaitingPublish, CapturedAudioBlockHandOffRing,
    NextCapturedAudioBlockToPublish,
};

/// Blocks the hand-off ring holds before it starts dropping the oldest.
/// Matches the ring depth a sample-stream link itself carries
/// (`DeliveryProfile::STREAM_DEPTH`), so the buffering either side of the
/// publish is the same order of magnitude — roughly 170 ms at the default
/// 48 kHz / 512-sample quantum.
const MAX_CAPTURED_BLOCKS_AWAITING_PUBLISH: usize = 16;

/// How often the publishing thread comes up for air to notice a stop while no
/// block is arriving.
const PUBLISH_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Drops between warnings once the first one has been logged — the counter is
/// cumulative, so a sustained overrun says so periodically instead of once per
/// lost block.
const DROPPED_BLOCKS_BETWEEN_WARNINGS: u64 = 300;

/// Failed writes between reports once the first has been logged. An output
/// port nobody connected fails every block — roughly 94 a second at the
/// default quantum — and the first line already says what the next thousand
/// would.
const FAILED_WRITES_BETWEEN_REPORTS: u64 = 300;

/// The one port a captured block is published on.
const AUDIO_OUTPUT_PORT: &str = "audio";

/// Bound on the wait for the publishing thread to exit. A consumer whose port
/// declares `lossless` can hold that thread inside `write` for as long as it
/// likes; detaching keeps the runtime's shutdown chain moving.
const PUBLISH_THREAD_EXIT_GRACE: Duration = Duration::from_secs(2);

/// Configuration for [`MicrophoneSource`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MicrophoneSourceConfig {
    /// Backend-named capture device. Absent: the backend's default device.
    /// A name the backend cannot open raises rather than landing on a
    /// different device — a wrong device id is a wiring error.
    ///
    /// On the PipeWire backend, `<sink>.monitor` captures what that sink is
    /// playing — how a graph records its own output, or a test closes a loop
    /// against a known signal.
    #[serde(default)]
    pub device_id: Option<String>,
}

#[streamlib::sdk::processor(
    description = "Captures audio from the machine's audio backend as timestamped sample blocks (silence where no backend exists)",
    execution = manual,
    scheduling = realtime,
    config = crate::microphone_source::MicrophoneSourceConfig,
    output("audio", description = "Timestamped blocks of interleaved capture samples"),
)]
pub struct MicrophoneSource {
    capture_stream: Option<Box<dyn AudioCaptureStream>>,
    hand_off_ring: Option<Arc<CapturedAudioBlockHandOffRing>>,
    /// Minted per publishing thread, so a thread that had to be detached can
    /// never be revived by a later `start()` into an endless spin.
    is_publishing: Option<Arc<AtomicBool>>,
    published_block_counter: Arc<AtomicU64>,
    publish_thread_handle: Option<JoinHandle<()>>,
}

impl ManualProcessor for MicrophoneSource::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let backend = probe_audio_device_backend();
        let capture_stream = backend.open_capture_stream(&AudioCaptureStreamRequest {
            device_id: self.config.device_id.clone(),
            deviceless_pacing_clock: Arc::clone(ctx.audio_clock()),
        })?;
        let stream_format = capture_stream.stream_format();
        tracing::info!(
            audio_backend = backend.backend_name(),
            device_id = self.config.device_id.as_deref().unwrap_or("<default>"),
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            sample_format = ?stream_format.sample_format,
            "MicrophoneSource: capture stream opened"
        );
        self.capture_stream = Some(capture_stream);
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let Some(capture_stream) = self.capture_stream.as_mut() else {
            return Err(Error::Configuration(
                "MicrophoneSource: no capture stream is open. setup() must run first.".into(),
            ));
        };
        let stream_format = capture_stream.stream_format();

        let hand_off_ring = Arc::new(CapturedAudioBlockHandOffRing::with_capacity(
            MAX_CAPTURED_BLOCKS_AWAITING_PUBLISH,
        ));
        capture_stream
            .start_delivering_to(device_callback_handing_off_into(Arc::clone(&hand_off_ring)))?;

        let is_publishing = Arc::new(AtomicBool::new(true));
        let is_publishing_in_the_thread = Arc::clone(&is_publishing);
        let published_block_counter = Arc::clone(&self.published_block_counter);
        let outputs: OutputWriter = self.outputs.clone();
        let hand_off_ring_for_publishing = Arc::clone(&hand_off_ring);

        let handle = std::thread::Builder::new()
            .name("audio-capture-publish".to_string())
            .spawn(move || {
                publish_captured_blocks(
                    &hand_off_ring_for_publishing,
                    &is_publishing_in_the_thread,
                    &published_block_counter,
                    &outputs,
                    stream_format,
                );
            })
            .map_err(|e| {
                Error::Runtime(format!(
                    "MicrophoneSource: failed to spawn the publishing thread: {e}"
                ))
            })?;

        self.hand_off_ring = Some(hand_off_ring);
        self.is_publishing = Some(is_publishing);
        self.publish_thread_handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_publishing();
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_publishing();
        tracing::info!(
            published_blocks = self.published_block_counter.load(Ordering::Relaxed),
            dropped_blocks = self
                .hand_off_ring
                .as_ref()
                .map_or(0, |ring| ring.dropped_block_count()),
            "MicrophoneSource: teardown"
        );
        self.capture_stream = None;
        Ok(())
    }
}

impl MicrophoneSource::Processor {
    fn stop_publishing(&mut self) {
        if let Some(capture_stream) = self.capture_stream.as_mut()
            && let Err(e) = capture_stream.stop_delivering()
        {
            tracing::warn!(error = %e, "MicrophoneSource: capture stream failed to stop");
        }
        if let Some(is_publishing) = self.is_publishing.take() {
            is_publishing.store(false, Ordering::Release);
        }
        if let Some(hand_off_ring) = &self.hand_off_ring {
            hand_off_ring.end_hand_off();
        }

        let Some(handle) = self.publish_thread_handle.take() else {
            return;
        };
        let deadline = Instant::now() + PUBLISH_THREAD_EXIT_GRACE;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if handle.is_finished() {
            let _ = handle.join();
        } else {
            tracing::warn!(
                "MicrophoneSource: publishing thread did not exit within {:?}, detaching",
                PUBLISH_THREAD_EXIT_GRACE
            );
        }
    }
}

/// The callback the capture stream delivers into.
///
/// Named rather than written inline at its one call site so a test can hold
/// what the device edge actually promises: the callback copies and returns,
/// and a consumer that stops draining costs blocks here instead of stalling
/// the device's own thread.
fn device_callback_handing_off_into(
    hand_off_ring: Arc<CapturedAudioBlockHandOffRing>,
) -> CapturedAudioBlockHandOff {
    Box::new(move |captured: CapturedAudioBlockFromDevice<'_>| {
        hand_off_ring.hand_off_from_device_callback(CapturedAudioBlockAwaitingPublish {
            interleaved_sample_bytes: captured.interleaved_sample_bytes.to_vec(),
            sample_count: captured.sample_count,
            first_sample_timestamp_ns: captured.first_sample_timestamp_ns,
        });
    })
}

fn publish_captured_blocks(
    hand_off_ring: &CapturedAudioBlockHandOffRing,
    is_publishing: &AtomicBool,
    published_block_counter: &AtomicU64,
    outputs: &OutputWriter,
    stream_format: AudioCaptureStreamFormat,
) {
    let mut warn_at_dropped_block_count = 1u64;
    let mut consecutive_write_failures = 0u64;
    while is_publishing.load(Ordering::Acquire) {
        match hand_off_ring.wait_for_next_block_to_publish(PUBLISH_WAIT_POLL_INTERVAL) {
            NextCapturedAudioBlockToPublish::Block(captured) => {
                publish_one_captured_block(
                    captured,
                    outputs,
                    published_block_counter,
                    &mut consecutive_write_failures,
                    stream_format,
                );
                warn_about_any_new_drops(hand_off_ring, &mut warn_at_dropped_block_count);
            }
            NextCapturedAudioBlockToPublish::WaitTimedOut => {}
            NextCapturedAudioBlockToPublish::HandOffEnded => break,
        }
    }

    // Stopping is not a discard: what the device captured before the stop is
    // already accounted for in the timestamps, so dropping it here would open
    // a gap nothing counted.
    while let NextCapturedAudioBlockToPublish::Block(captured) =
        hand_off_ring.wait_for_next_block_to_publish(Duration::ZERO)
    {
        publish_one_captured_block(
            captured,
            outputs,
            published_block_counter,
            &mut consecutive_write_failures,
            stream_format,
        );
    }
    warn_about_any_new_drops(hand_off_ring, &mut warn_at_dropped_block_count);
}

/// Whether a failure at this point in a run of them is one to report.
///
/// A write failure is not a passing condition: an output port with no link
/// fails every block for as long as the graph runs, so reporting each one
/// buries the rest of the log rather than telling anyone anything new.
fn write_failure_is_worth_reporting(consecutive_write_failures: u64) -> bool {
    consecutive_write_failures > 0
        && (consecutive_write_failures == 1
            || consecutive_write_failures.is_multiple_of(FAILED_WRITES_BETWEEN_REPORTS))
}

/// Fold one publish attempt into the run of failures, and say whether this one
/// is the one to report.
///
/// The reset lives here rather than at the success path's tail so that the
/// whole rule — a success ends a run, a failure extends it, and only some
/// failures are spoken about — is one testable thing.
fn publish_attempt_is_worth_reporting(
    published: bool,
    consecutive_write_failures: &mut u64,
) -> bool {
    if published {
        *consecutive_write_failures = 0;
        return false;
    }
    *consecutive_write_failures += 1;
    write_failure_is_worth_reporting(*consecutive_write_failures)
}

fn publish_one_captured_block(
    captured: CapturedAudioBlockAwaitingPublish,
    outputs: &OutputWriter,
    published_block_counter: &AtomicU64,
    consecutive_write_failures: &mut u64,
    stream_format: AudioCaptureStreamFormat,
) {
    // Asked before the block is built, because serializing a device quantum
    // into a port with no link is thousands of allocations a second on the
    // publishing thread for a value nothing can receive.
    if !outputs.has_port(AUDIO_OUTPUT_PORT) {
        if publish_attempt_is_worth_reporting(false, consecutive_write_failures) {
            // A warning rather than an error: connect() is a runtime operation,
            // so an output with no link yet is a state the engine permits
            // rather than a defect. What makes it worth saying at all is that
            // it persists — the count is how a reader tells the two apart.
            tracing::warn!(
                blocks_not_published = *consecutive_write_failures,
                "MicrophoneSource: the audio output port has no link, so captured \
                 blocks are going nowhere. Connect it to a consumer."
            );
        }
        return;
    }

    let block = audio_block_captured_as(captured, stream_format);
    // The device stamped this block, and the engine must not re-stamp it:
    // `write`'s implicit `MediaClock::now()` would name the instant of
    // publication rather than the instant of capture, and A/V sync is the
    // subtraction of two capture instants.
    if let Err(e) =
        outputs.write_with_timestamp(AUDIO_OUTPUT_PORT, &block, block.first_sample_timestamp_ns)
    {
        // A linked port that still refused the write: a payload over the
        // channel ceiling, an exhausted publisher segment behind a slow
        // consumer, a serialize failure. The error says which; this must not
        // guess, because naming the wrong cause sends a reader after a link
        // that is already there.
        if publish_attempt_is_worth_reporting(false, consecutive_write_failures) {
            tracing::error!(
                consecutive_failures = *consecutive_write_failures,
                error = %e,
                "MicrophoneSource: failed to write an audio block"
            );
        }
        return;
    }
    publish_attempt_is_worth_reporting(true, consecutive_write_failures);
    published_block_counter.fetch_add(1, Ordering::Relaxed);
}

fn warn_about_any_new_drops(
    hand_off_ring: &CapturedAudioBlockHandOffRing,
    warn_at_dropped_block_count: &mut u64,
) {
    let dropped_block_count = hand_off_ring.dropped_block_count();
    if dropped_block_count < *warn_at_dropped_block_count {
        return;
    }
    tracing::warn!(
        dropped_blocks = dropped_block_count,
        "MicrophoneSource: blocks dropped at the device edge — the consumer is not keeping \
         up. The gap is derivable from the timestamps and sample counts of the blocks \
         either side of it."
    );
    *warn_at_dropped_block_count = dropped_block_count + DROPPED_BLOCKS_BETWEEN_WARNINGS;
}

/// How a captured stream's scalar encoding is spelled on the wire.
fn wire_dtype_for(sample_format: AudioCaptureSampleFormat) -> AudioSampleDtype {
    match sample_format {
        AudioCaptureSampleFormat::F32 => AudioSampleDtype::F32,
        AudioCaptureSampleFormat::I16 => AudioSampleDtype::I16,
    }
}

/// Assemble the bag a captured block publishes as: the stream describes the
/// samples, and the device's own timestamp rides through untouched.
fn audio_block_captured_as(
    captured: CapturedAudioBlockAwaitingPublish,
    stream_format: AudioCaptureStreamFormat,
) -> AudioBlock {
    AudioBlock {
        interleaved_sample_bytes: captured.interleaved_sample_bytes,
        sample_rate: stream_format.sample_rate,
        channels: stream_format.channels,
        sample_count: captured.sample_count,
        dtype: wire_dtype_for(stream_format.sample_format),
        first_sample_timestamp_ns: captured.first_sample_timestamp_ns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;
    use streamlib::sdk::context::{
        AudioClock, AudioClockConfig, AudioDeviceBackend, AudioTickCallback, AudioTickContext,
        SharedAudioClock, SilentNullAudioDeviceBackend,
    };

    const TEST_SAMPLE_RATE: u32 = 48_000;
    const TEST_QUANTUM_SAMPLES: usize = 512;
    const TEST_TICK_TIMESTAMP_NS: i64 = 1_000_000_000;

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
                timestamp_ns: TEST_TICK_TIMESTAMP_NS,
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

    /// The device edge's whole contract, held where it is claimed: the
    /// callback the source installs hands off into its ring and returns, so a
    /// consumer that never drains costs blocks and a counter rather than the
    /// device's own thread.
    ///
    /// Mental revert: publish straight from the callback instead of handing
    /// off, and under `delivery_profile = "lossless"` the capture thread waits
    /// on the consumer — the shape the ring exists to make impossible.
    #[test]
    fn the_device_callback_hands_off_into_the_ring_and_the_loss_lands_there() {
        const BLOCKS_THE_RING_HOLDS: usize = 4;
        const BLOCKS_NOBODY_DRAINS: usize = 10;

        let clock = Arc::new(HandFiredTestAudioClock::new());
        // The null arm by name rather than through the chain's probe: what is
        // under test is the ring at the device edge, and the probe would hand
        // back whatever audio server the machine running this happens to have —
        // a device that paces itself and ignores a hand-fired clock.
        let mut capture_stream = SilentNullAudioDeviceBackend
            .open_capture_stream(&AudioCaptureStreamRequest {
                device_id: None,
                deviceless_pacing_clock: Arc::clone(&clock) as SharedAudioClock,
            })
            .expect("the null backend opens its default device on any machine");

        let hand_off_ring = Arc::new(CapturedAudioBlockHandOffRing::with_capacity(
            BLOCKS_THE_RING_HOLDS,
        ));
        capture_stream
            .start_delivering_to(device_callback_handing_off_into(Arc::clone(&hand_off_ring)))
            .expect("start delivering");

        let capturing_began = Instant::now();
        for _ in 0..BLOCKS_NOBODY_DRAINS {
            clock.fire_one_tick();
        }
        let capturing_took = capturing_began.elapsed();

        assert!(
            capturing_took < PUBLISH_WAIT_POLL_INTERVAL,
            "{BLOCKS_NOBODY_DRAINS} captures into a ring nobody drains took \
             {capturing_took:?} — a device callback waited"
        );
        assert_eq!(
            hand_off_ring.dropped_block_count(),
            (BLOCKS_NOBODY_DRAINS - BLOCKS_THE_RING_HOLDS) as u64,
            "every block the ring could not hold is counted at the device edge"
        );

        let survived: Vec<i64> = std::iter::from_fn(|| {
            match hand_off_ring.wait_for_next_block_to_publish(Duration::ZERO) {
                NextCapturedAudioBlockToPublish::Block(block) => {
                    Some(block.first_sample_timestamp_ns)
                }
                _ => None,
            }
        })
        .collect();
        let newest_captured: Vec<i64> = (BLOCKS_NOBODY_DRAINS - BLOCKS_THE_RING_HOLDS
            ..BLOCKS_NOBODY_DRAINS)
            .map(|block_index| {
                TEST_TICK_TIMESTAMP_NS
                    + (block_index * TEST_QUANTUM_SAMPLES) as i64 * 1_000_000_000
                        / i64::from(TEST_SAMPLE_RATE)
            })
            .collect();
        assert_eq!(
            survived, newest_captured,
            "the oldest block is what a full ring gives up, so the newest audio is what \
             reaches the link"
        );
    }

    #[test]
    fn a_captured_streams_scalar_encoding_maps_onto_the_wires_dtype() {
        assert_eq!(
            wire_dtype_for(AudioCaptureSampleFormat::F32),
            AudioSampleDtype::F32
        );
        assert_eq!(
            wire_dtype_for(AudioCaptureSampleFormat::I16),
            AudioSampleDtype::I16
        );
    }

    fn captured_block(interleaved_sample_bytes: Vec<u8>) -> CapturedAudioBlockAwaitingPublish {
        CapturedAudioBlockAwaitingPublish {
            sample_count: 2,
            first_sample_timestamp_ns: 123_456_789,
            interleaved_sample_bytes,
        }
    }

    /// A block's timestamp is the device's, and the stream — not the block —
    /// is what says how to read the samples.
    #[test]
    fn a_published_block_carries_the_streams_format_and_the_devices_timestamp() {
        let block = audio_block_captured_as(
            captured_block(vec![0u8; 16]),
            AudioCaptureStreamFormat {
                sample_rate: 48_000,
                channels: 2,
                sample_format: AudioCaptureSampleFormat::F32,
            },
        );
        assert_eq!(block.sample_rate, 48_000);
        assert_eq!(block.channels, 2);
        assert_eq!(block.sample_count, 2);
        assert_eq!(block.dtype, AudioSampleDtype::F32);
        assert_eq!(block.first_sample_timestamp_ns, 123_456_789);
        assert_eq!(
            block.interleaved_sample_bytes.len(),
            block.sample_count as usize * block.channels as usize * 4
        );
    }

    #[test]
    fn a_stream_capturing_i16_publishes_blocks_that_say_so() {
        let block = audio_block_captured_as(
            captured_block(vec![0u8; 4]),
            AudioCaptureStreamFormat {
                sample_rate: 16_000,
                channels: 1,
                sample_format: AudioCaptureSampleFormat::I16,
            },
        );
        assert_eq!(block.dtype, AudioSampleDtype::I16);
        assert_eq!(
            block.interleaved_sample_bytes.len(),
            block.sample_count as usize * block.channels as usize * 2
        );
    }

    /// Counts the lines a body actually emitted and at what level, so a test
    /// can hold the log rate itself rather than a predicate the log site is
    /// free to ignore — and can tell the two failure paths apart.
    #[derive(Default)]
    struct EmittedLines {
        warnings: AtomicU64,
        errors: AtomicU64,
    }

    struct CountingTracingSubscriber(Arc<EmittedLines>);

    impl tracing::Subscriber for CountingTracingSubscriber {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let counter = match *event.metadata().level() {
                tracing::Level::ERROR => &self.0.errors,
                _ => &self.0.warnings,
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    fn a_captured_block() -> CapturedAudioBlockAwaitingPublish {
        CapturedAudioBlockAwaitingPublish {
            interleaved_sample_bytes: vec![0u8; 8],
            sample_count: 2,
            first_sample_timestamp_ns: 0,
        }
    }

    fn a_stream_format() -> AudioCaptureStreamFormat {
        AudioCaptureStreamFormat {
            sample_rate: 48_000,
            channels: 1,
            sample_format: AudioCaptureSampleFormat::F32,
        }
    }

    /// The defect this fixes, held by counting the lines rather than by
    /// re-deriving the rule the log site is supposed to follow: an output with
    /// no link failed every block and reported every one, burying an observed
    /// run under 18 059 lines.
    #[test]
    fn a_thousand_blocks_into_an_unlinked_port_report_a_handful_of_times() {
        // Never wired, so it has no port and every block fails the way an
        // output nobody connected does.
        let unwired_outputs = OutputWriter::empty();
        let published_block_counter = AtomicU64::new(0);
        let mut consecutive_write_failures = 0u64;
        let lines = Arc::new(EmittedLines::default());

        tracing::subscriber::with_default(CountingTracingSubscriber(Arc::clone(&lines)), || {
            for _ in 0..1000 {
                publish_one_captured_block(
                    a_captured_block(),
                    &unwired_outputs,
                    &published_block_counter,
                    &mut consecutive_write_failures,
                    a_stream_format(),
                );
            }
        });

        assert_eq!(consecutive_write_failures, 1000, "every block failed");
        assert_eq!(
            published_block_counter.load(Ordering::Relaxed),
            0,
            "nothing reached a link that does not exist"
        );
        assert_eq!(
            lines.warnings.load(Ordering::Relaxed),
            4,
            "the first failure and every {FAILED_WRITES_BETWEEN_REPORTS}th, not one per block"
        );
        assert_eq!(
            lines.errors.load(Ordering::Relaxed),
            0,
            "an output nobody connected is a state the engine permits, and is \
             reported as such rather than as a write that failed for some \
             unknown reason"
        );
    }

    /// A success ends a run, so the next failure is a first failure again — a
    /// link that comes up must not keep the source quiet about the next
    /// stretch.
    #[test]
    fn a_success_ends_the_run_and_the_next_failure_is_reported_again() {
        let mut consecutive_write_failures = 0u64;

        assert!(publish_attempt_is_worth_reporting(
            false,
            &mut consecutive_write_failures
        ));
        for _ in 0..50 {
            publish_attempt_is_worth_reporting(false, &mut consecutive_write_failures);
        }
        assert_eq!(consecutive_write_failures, 51);

        assert!(!publish_attempt_is_worth_reporting(
            true,
            &mut consecutive_write_failures
        ));
        assert_eq!(consecutive_write_failures, 0, "a success ends the run");

        assert!(
            publish_attempt_is_worth_reporting(false, &mut consecutive_write_failures),
            "the first failure of a new run is reported"
        );
    }

    /// Nothing has failed yet, so there is nothing to report.
    #[test]
    fn no_failures_at_all_is_not_worth_reporting() {
        assert!(!write_failure_is_worth_reporting(0));
    }

    /// `rt.add(MicrophoneSource)` sends `{}` to the engine, and every field of
    /// a built-in's config has to deserialize from it — the spelling the plan
    /// blesses for a block that needs no configuration.
    #[test]
    fn a_config_given_no_fields_at_all_takes_the_backends_default_device() {
        let config: MicrophoneSourceConfig =
            serde_json::from_str("{}").expect("an empty config object deserializes");
        assert_eq!(config, MicrophoneSourceConfig { device_id: None });
    }
}
