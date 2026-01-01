// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::frames::AudioFrame;
use crate::core::utils::audio_utils::AudioRechunker;
use crate::core::{LinkInput, LinkOutput, Result, RuntimeContext};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
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

#[crate::processor(
    execution = Reactive,
    description = "Rechunks variable-sized audio buffers into fixed-size chunks (works with any channel count)"
)]
pub struct BufferRechunkerProcessor {
    #[crate::input(description = "Variable-sized audio input")]
    audio_in: LinkInput<AudioFrame>,

    #[crate::output(description = "Fixed-size audio output at target buffer size")]
    audio_out: LinkOutput<AudioFrame>,

    #[crate::config]
    config: BufferRechunkerConfig,

    rechunker: Option<AudioRechunker>,
}

impl crate::core::ReactiveProcessor for BufferRechunkerProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "[BufferRechunker] Initialized with target buffer size: {} samples per channel",
            self.config.target_buffer_size
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("[BufferRechunker] Stopped");
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        if let Some(input_frame) = self.audio_in.read() {
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
