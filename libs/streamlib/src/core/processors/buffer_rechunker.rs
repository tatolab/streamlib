// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::Audioframe;
use crate::core::{Result, RuntimeContext};

#[crate::processor("src/core/processors/buffer_rechunker.yaml")]
pub struct BufferRechunkerProcessor {
    buffer: Vec<f32>,
    sample_rate: u32,
    frame_counter: u64,
    channels: u8,
}

impl crate::core::ReactiveProcessor for BufferRechunkerProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let target_size = self.config.target_buffer_size as usize;
        // Pre-allocate buffer with extra capacity for any channel count
        self.buffer = Vec::with_capacity(target_size * 16);
        tracing::info!(
            "[BufferRechunker] Initialized with target buffer size: {} samples per channel",
            target_size
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("[BufferRechunker] Stopped");
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }

        let input_frame: Audioframe = self.inputs.read("audio_in")?;

        // Store sample rate and channels from first frame
        if self.sample_rate == 0 {
            self.sample_rate = input_frame.sample_rate;
            self.channels = input_frame.channels;
        }

        // Add samples to buffer
        self.buffer.extend_from_slice(&input_frame.samples);

        // Target size in total samples (accounting for interleaved channels)
        let target_interleaved_size =
            (self.config.target_buffer_size as usize) * (self.channels as usize);

        // Output chunks when we have enough samples
        while self.buffer.len() >= target_interleaved_size {
            let chunk: Vec<f32> = self.buffer.drain(..target_interleaved_size).collect();

            let output_frame = Audioframe {
                samples: chunk,
                channels: self.channels,
                sample_rate: self.sample_rate,
                timestamp_ns: input_frame.timestamp_ns.clone(),
                frame_index: self.frame_counter.to_string(),
            };

            self.outputs.write("audio_out", &output_frame)?;
            self.frame_counter += 1;

            tracing::debug!(
                "[BufferRechunker] Output frame {} with {} samples per channel ({} channels)",
                self.frame_counter,
                self.config.target_buffer_size,
                self.channels
            );
        }

        Ok(())
    }
}
