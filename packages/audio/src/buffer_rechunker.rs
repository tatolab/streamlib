// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::AudioFrame;
use streamlib_plugin_sdk::sdk::error::Result;
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/audio/BufferRechunker",
    description = "Rechunks audio buffers to a fixed sample count",
    execution = reactive,
    scheduling = realtime,
    config = crate::_generated_::BufferRechunkerConfig,
    input("audio_in", "@tatolab/core/AudioFrame", read_mode = "read_next_in_order", buffer_size = 32, description = "Variable-size audio frame"),
    output("audio_out", "@tatolab/core/AudioFrame", description = "Fixed-size audio frame"),
)]
pub struct BufferRechunkerProcessor {
    buffer: Vec<f32>,
    sample_rate: u32,
    frame_counter: u64,
    channels: u8,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for BufferRechunkerProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let target_size = self.config.target_buffer_size as usize;
        self.buffer = Vec::with_capacity(target_size * 16);
        tracing::info!(
            "[BufferRechunker] Initialized with target buffer size: {} samples per channel",
            target_size
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("[BufferRechunker] Stopped");
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }

        let input_frame: AudioFrame = self.inputs.read("audio_in")?;

        if self.sample_rate == 0 {
            self.sample_rate = input_frame.sample_rate;
            self.channels = input_frame.channels;
        }

        self.buffer.extend_from_slice(&input_frame.samples);

        let target_interleaved_size =
            (self.config.target_buffer_size as usize) * (self.channels as usize);

        while self.buffer.len() >= target_interleaved_size {
            let chunk: Vec<f32> = self.buffer.drain(..target_interleaved_size).collect();

            let output_frame = AudioFrame {
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
