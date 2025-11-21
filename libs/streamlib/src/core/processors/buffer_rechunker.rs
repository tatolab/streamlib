use crate::core::audio_frame_utils::AudioRechunker;
use crate::core::frames::AudioFrame;
use crate::core::{Result, RuntimeContext, StreamInput, StreamOutput};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib_macros::StreamProcessor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferRechunkerConfig {
    /// Target buffer size in samples per channel
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
    description = "Rechunks variable-sized audio buffers into fixed-size chunks (works with any channel count)"
)]
pub struct BufferRechunkerProcessor {
    #[input(description = "Variable-sized audio input")]
    audio_in: StreamInput<AudioFrame>,

    #[output(description = "Fixed-size audio output at target buffer size")]
    audio_out: Arc<StreamOutput<AudioFrame>>,

    #[config]
    config: BufferRechunkerConfig,

    // Runtime state - rechunker is created lazily when first frame arrives
    rechunker: Option<AudioRechunker>,
}

impl BufferRechunkerProcessor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        tracing::info!(
            "[BufferRechunker] Initialized with target buffer size: {} samples per channel",
            self.config.target_buffer_size
        );
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        tracing::info!("[BufferRechunker] Stopped");
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if let Some(input_frame) = self.audio_in.read() {
            // Lazy initialize rechunker on first frame
            if self.rechunker.is_none() {
                let rechunker =
                    AudioRechunker::new(input_frame.channels, self.config.target_buffer_size);
                tracing::info!(
                    "[BufferRechunker] Initialized for {:?} ({} channels)",
                    input_frame.channels,
                    input_frame.channels()
                );
                self.rechunker = Some(rechunker);
            }

            // Process frame through rechunker
            if let Some(ref mut rechunker) = self.rechunker {
                if let Some(output_frame) = rechunker.process(&input_frame) {
                    tracing::debug!(
                        "[BufferRechunker] Output frame with {} samples per channel",
                        output_frame.sample_count()
                    );
                    self.audio_out.write(output_frame);
                }
            }
        }

        Ok(())
    }
}
