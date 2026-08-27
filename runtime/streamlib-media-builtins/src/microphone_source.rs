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
    AudioCaptureStreamRequest, CapturedAudioBlockFromDevice, RuntimeContextFullAccess,
    probe_audio_device_backend,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::processors::ManualProcessor;

use crate::audio_block::{AudioBlock, AudioSampleDtype};
use crate::captured_audio_block_hand_off_ring::{
    CapturedAudioBlockAwaitingPublish, CapturedAudioBlockHandOffRing,
};

/// Blocks the hand-off ring holds before it starts dropping the oldest.
/// Matches the ring depth a sample-stream link itself carries
/// (`DeliveryProfile::STREAM_DEPTH`), so the buffering either side of the
/// publish is the same order of magnitude — roughly 170 ms at the default
/// 48 kHz / 512-sample quantum.
const CAPTURED_BLOCKS_AWAITING_PUBLISH: usize = 16;

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
    stream_format: Option<AudioCaptureStreamFormat>,
    is_publishing: Arc<AtomicBool>,
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
        self.stream_format = Some(stream_format);
        self.capture_stream = Some(capture_stream);
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let (Some(capture_stream), Some(stream_format)) =
            (self.capture_stream.as_mut(), self.stream_format)
        else {
            return Err(Error::Configuration(
                "MicrophoneSource: no capture stream is open. setup() must run first.".into(),
            ));
        };

        let hand_off_ring = Arc::new(CapturedAudioBlockHandOffRing::with_capacity(
            CAPTURED_BLOCKS_AWAITING_PUBLISH,
        ));
        let hand_off_ring_for_device_callback = Arc::clone(&hand_off_ring);
        capture_stream.start_delivering_to(Box::new(
            move |captured: CapturedAudioBlockFromDevice<'_>| {
                hand_off_ring_for_device_callback.hand_off_from_device_callback(
                    CapturedAudioBlockAwaitingPublish {
                        interleaved_sample_bytes: captured.interleaved_sample_bytes.to_vec(),
                        sample_count: captured.sample_count,
                        first_sample_timestamp_ns: captured.first_sample_timestamp_ns,
                    },
                );
            },
        ))?;

        self.is_publishing.store(true, Ordering::Release);
        let is_publishing = Arc::clone(&self.is_publishing);
        let published_block_counter = Arc::clone(&self.published_block_counter);
        let outputs: OutputWriter = self.outputs.clone();
        let hand_off_ring_for_publishing = Arc::clone(&hand_off_ring);

        let handle = std::thread::Builder::new()
            .name("audio-capture-publish".to_string())
            .spawn(move || {
                publish_captured_blocks(
                    &hand_off_ring_for_publishing,
                    &is_publishing,
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
        self.is_publishing.store(false, Ordering::Release);
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

fn publish_captured_blocks(
    hand_off_ring: &CapturedAudioBlockHandOffRing,
    is_publishing: &AtomicBool,
    published_block_counter: &AtomicU64,
    outputs: &OutputWriter,
    stream_format: AudioCaptureStreamFormat,
) {
    let mut warn_at_dropped_block_count = 1u64;
    while is_publishing.load(Ordering::Acquire) {
        let Some(captured) =
            hand_off_ring.wait_for_next_block_to_publish(PUBLISH_WAIT_POLL_INTERVAL)
        else {
            continue;
        };

        let block = audio_block_captured_as(captured, stream_format);
        // The device stamped this block, and the engine must not re-stamp it:
        // `write`'s implicit `MediaClock::now()` would name the instant of
        // publication rather than the instant of capture, and A/V sync is the
        // subtraction of two capture instants.
        if let Err(e) =
            outputs.write_with_timestamp("audio", &block, block.first_sample_timestamp_ns)
        {
            tracing::error!(error = %e, "MicrophoneSource: failed to write an audio block");
            continue;
        }
        published_block_counter.fetch_add(1, Ordering::Relaxed);

        let dropped_block_count = hand_off_ring.dropped_block_count();
        if dropped_block_count >= warn_at_dropped_block_count {
            tracing::warn!(
                dropped_blocks = dropped_block_count,
                "MicrophoneSource: blocks dropped at the device edge — the consumer is not \
                 keeping up. The gap is derivable from the timestamps and sample counts of \
                 the blocks either side of it."
            );
            warn_at_dropped_block_count = dropped_block_count + DROPPED_BLOCKS_BETWEEN_WARNINGS;
        }
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
        dtype: match stream_format.sample_format {
            AudioCaptureSampleFormat::F32 => AudioSampleDtype::F32,
            AudioCaptureSampleFormat::I16 => AudioSampleDtype::I16,
        },
        first_sample_timestamp_ns: captured.first_sample_timestamp_ns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
