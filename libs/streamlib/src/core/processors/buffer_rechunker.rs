// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Result, RuntimeContext, StreamError};
use crate::schemas::{Audioframe1ch, Audioframe2ch};
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

// =============================================================================
// Mono (1-channel) Buffer Rechunker
// =============================================================================

#[crate::processor(
    execution = Reactive,
    description = "Rechunks variable-sized mono audio buffers into fixed-size chunks",
    inputs = [input("audio_in", schema = "com.tatolab.audioframe.1ch@1.0.0")],
    outputs = [output("audio_out", schema = "com.tatolab.audioframe.1ch@1.0.0")]
)]
pub struct BufferRechunker1chProcessor {
    #[crate::config]
    config: BufferRechunkerConfig,

    buffer: Vec<f32>,
    sample_rate: u32,
    frame_counter: u64,
}

impl crate::core::ReactiveProcessor for BufferRechunker1chProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.buffer = Vec::with_capacity(self.config.target_buffer_size * 2);
        tracing::info!(
            "[BufferRechunker1ch] Initialized with target buffer size: {} samples",
            self.config.target_buffer_size
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("[BufferRechunker1ch] Stopped");
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        if let Some(payload) = self.inputs.get("audio_in") {
            let input_frame = Audioframe1ch::from_msgpack(payload.data())
                .map_err(|e| StreamError::Runtime(format!("msgpack decode: {}", e)))?;

            // Store sample rate from first frame
            if self.sample_rate == 0 {
                self.sample_rate = input_frame.sample_rate;
            }

            // Add samples to buffer
            self.buffer.extend_from_slice(&input_frame.samples);

            // Output chunks when we have enough samples
            while self.buffer.len() >= self.config.target_buffer_size {
                let chunk: Vec<f32> = self.buffer.drain(..self.config.target_buffer_size).collect();

                let output_frame = Audioframe1ch {
                    samples: chunk,
                    sample_rate: self.sample_rate,
                    timestamp_ns: input_frame.timestamp_ns,
                    frame_index: self.frame_counter,
                };

                let bytes = output_frame.to_msgpack()
                    .map_err(|e| StreamError::Runtime(format!("msgpack encode: {}", e)))?;
                self.outputs.write("audio_out", &bytes)?;
                self.frame_counter += 1;

                tracing::debug!(
                    "[BufferRechunker1ch] Output frame {} with {} samples",
                    self.frame_counter,
                    self.config.target_buffer_size
                );
            }
        }

        Ok(())
    }
}

// =============================================================================
// Stereo (2-channel) Buffer Rechunker
// =============================================================================

#[crate::processor(
    execution = Reactive,
    description = "Rechunks variable-sized stereo audio buffers into fixed-size chunks",
    inputs = [input("audio_in", schema = "com.tatolab.audioframe.2ch@1.0.0")],
    outputs = [output("audio_out", schema = "com.tatolab.audioframe.2ch@1.0.0")]
)]
pub struct BufferRechunker2chProcessor {
    #[crate::config]
    config: BufferRechunkerConfig,

    buffer: Vec<f32>,
    sample_rate: u32,
    frame_counter: u64,
}

impl crate::core::ReactiveProcessor for BufferRechunker2chProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        // For stereo, buffer size is target * 2 (interleaved)
        self.buffer = Vec::with_capacity(self.config.target_buffer_size * 4);
        tracing::info!(
            "[BufferRechunker2ch] Initialized with target buffer size: {} samples per channel",
            self.config.target_buffer_size
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("[BufferRechunker2ch] Stopped");
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        if let Some(payload) = self.inputs.get("audio_in") {
            let input_frame = Audioframe2ch::from_msgpack(payload.data())
                .map_err(|e| StreamError::Runtime(format!("msgpack decode: {}", e)))?;

            // Store sample rate from first frame
            if self.sample_rate == 0 {
                self.sample_rate = input_frame.sample_rate;
            }

            // Add samples to buffer
            self.buffer.extend_from_slice(&input_frame.samples);

            // For stereo, we need target_buffer_size * 2 samples (interleaved L,R,L,R,...)
            let target_interleaved_size = self.config.target_buffer_size * 2;

            // Output chunks when we have enough samples
            while self.buffer.len() >= target_interleaved_size {
                let chunk: Vec<f32> = self.buffer.drain(..target_interleaved_size).collect();

                let output_frame = Audioframe2ch {
                    samples: chunk,
                    sample_rate: self.sample_rate,
                    timestamp_ns: input_frame.timestamp_ns,
                    frame_index: self.frame_counter,
                };

                let bytes = output_frame.to_msgpack()
                    .map_err(|e| StreamError::Runtime(format!("msgpack encode: {}", e)))?;
                self.outputs.write("audio_out", &bytes)?;
                self.frame_counter += 1;

                tracing::debug!(
                    "[BufferRechunker2ch] Output frame {} with {} samples per channel",
                    self.frame_counter,
                    self.config.target_buffer_size
                );
            }
        }

        Ok(())
    }
}
