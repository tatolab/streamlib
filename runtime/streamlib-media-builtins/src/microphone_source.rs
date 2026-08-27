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
    while is_publishing.load(Ordering::Acquire) {
        match hand_off_ring.wait_for_next_block_to_publish(PUBLISH_WAIT_POLL_INTERVAL) {
            NextCapturedAudioBlockToPublish::Block(captured) => {
                publish_one_captured_block(
                    captured,
                    outputs,
                    published_block_counter,
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
        publish_one_captured_block(captured, outputs, published_block_counter, stream_format);
    }
}

fn publish_one_captured_block(
    captured: CapturedAudioBlockAwaitingPublish,
    outputs: &OutputWriter,
    published_block_counter: &AtomicU64,
    stream_format: AudioCaptureStreamFormat,
) {
    let block = audio_block_captured_as(captured, stream_format);
    // The device stamped this block, and the engine must not re-stamp it:
    // `write`'s implicit `MediaClock::now()` would name the instant of
    // publication rather than the instant of capture, and A/V sync is the
    // subtraction of two capture instants.
    if let Err(e) = outputs.write_with_timestamp("audio", &block, block.first_sample_timestamp_ns) {
        tracing::error!(error = %e, "MicrophoneSource: failed to write an audio block");
        return;
    }
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
    use std::sync::Mutex;
    use streamlib::sdk::context::{
        AudioClock, AudioClockConfig, AudioTickCallback, AudioTickContext, SharedAudioClock,
    };

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
                config: AudioClockConfig::new(48_000, 512),
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
        let mut capture_stream = probe_audio_device_backend()
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

        let mut survived = 0;
        while let NextCapturedAudioBlockToPublish::Block(block) =
            hand_off_ring.wait_for_next_block_to_publish(Duration::ZERO)
        {
            assert_eq!(block.sample_count, 512);
            survived += 1;
        }
        assert_eq!(survived, BLOCKS_THE_RING_HOLDS, "the newest audio survives");
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
