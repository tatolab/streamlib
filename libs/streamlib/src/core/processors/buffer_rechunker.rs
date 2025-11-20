use crate::core::frames::AudioFrame;
use crate::core::{Result, RuntimeContext, StreamInput, StreamOutput};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib_macros::StreamProcessor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferRechunkerConfig {
    /// Target buffer size in samples (required - this is what the processor transforms)
    pub target_buffer_size: usize,
}

impl Default for BufferRechunkerConfig {
    fn default() -> Self {
        Self {
            target_buffer_size: 512,
        }
    }
}

#[derive(StreamProcessor)]
#[processor(
    mode = Push,
    description = "Rechunks variable-sized stereo audio buffers into fixed-size chunks matching the runtime's buffer size"
)]
pub struct BufferRechunkerProcessor {
    #[input(description = "Variable-sized stereo audio input")]
    audio_in: StreamInput<AudioFrame<2>>,

    #[output(description = "Fixed-size stereo audio output at target buffer size")]
    audio_out: Arc<StreamOutput<AudioFrame<2>>>,

    #[config]
    config: BufferRechunkerConfig,

    // Runtime state fields
    buffer: Vec<f32>,
    target_buffer_size: usize,
    frame_counter: u64,
    next_timestamp_ns: i64,
}

impl BufferRechunkerProcessor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        self.target_buffer_size = self.config.target_buffer_size;

        tracing::info!(
            "[BufferRechunker] Initialized with target buffer size: {} samples (sample_rate will be read from input frames)",
            self.target_buffer_size
        );

        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            "[BufferRechunker] Stopped (processed {} output frames, {} samples buffered)",
            self.frame_counter,
            self.buffer.len() / 2
        );
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        const CHANNELS: usize = 2;

        // Read input frame
        if let Some(input_frame) = self.audio_in.read() {
            let input_samples = &*input_frame.samples;
            let input_sample_count = input_samples.len() / CHANNELS;
            let sample_rate = input_frame.sample_rate; // Read from input frame!

            // Initialize next_timestamp_ns on first frame
            if self.frame_counter == 0 && self.next_timestamp_ns == 0 {
                self.next_timestamp_ns = input_frame.timestamp_ns;
            }

            tracing::debug!(
                "[BufferRechunker] Received {} samples at {}Hz (buffer has {} samples)",
                input_sample_count,
                sample_rate,
                self.buffer.len() / CHANNELS
            );

            // Append incoming samples to buffer
            self.buffer.extend_from_slice(input_samples);

            // Output as many fixed-size chunks as possible
            let target_samples_total = self.target_buffer_size * CHANNELS;

            while self.buffer.len() >= target_samples_total {
                // Extract exactly target_buffer_size samples
                let output_samples: Vec<f32> = self.buffer.drain(..target_samples_total).collect();

                // Create output frame (preserve sample_rate from input!)
                let output_frame = AudioFrame::<2>::new(
                    output_samples,
                    self.next_timestamp_ns,
                    self.frame_counter,
                    sample_rate, // Use sample_rate from input frame
                );

                // Calculate next timestamp based on the number of samples we're outputting
                let duration_ns =
                    (self.target_buffer_size as i64 * 1_000_000_000) / sample_rate as i64;
                self.next_timestamp_ns += duration_ns;

                self.audio_out.write(output_frame);
                self.frame_counter += 1;

                tracing::debug!(
                    "[BufferRechunker] Output frame {} ({} samples at {}Hz, {} samples remain buffered)",
                    self.frame_counter,
                    self.target_buffer_size,
                    sample_rate,
                    self.buffer.len() / CHANNELS
                );
            }
        }

        Ok(())
    }
}
