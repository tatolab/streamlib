// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::tatolab__audio::audio_channel_converter_config::Mode;
use crate::_generated_::AudioFrame;
use streamlib_plugin_sdk::sdk::error::{Result, Error};
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};

#[streamlib_plugin_sdk::sdk::processor("AudioChannelConverter")]
pub struct AudioChannelConverterProcessor {
    frame_counter: u64,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for AudioChannelConverterProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "[AudioChannelConverter] setup() - mode: {:?}",
            self.config.mode
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "[AudioChannelConverter] Stopped (processed {} frames)",
            self.frame_counter
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }

        let input_frame: AudioFrame = self.inputs.read("audio_in")?;

        if input_frame.channels != 1 {
            return Err(Error::Configuration(format!(
                "AudioChannelConverter expects mono input (1 channel), got {} channels",
                input_frame.channels
            )));
        }

        let output_channels = self.config.output_channels.unwrap_or(2);

        let output_samples: Vec<f32> = match self.config.mode {
            Mode::Duplicate => input_frame
                .samples
                .iter()
                .flat_map(|&sample| std::iter::repeat_n(sample, output_channels as usize))
                .collect(),
            Mode::LeftOnly => input_frame
                .samples
                .iter()
                .flat_map(|&sample| {
                    std::iter::once(sample)
                        .chain(std::iter::repeat_n(0.0, output_channels as usize - 1))
                })
                .collect(),
            Mode::RightOnly => input_frame
                .samples
                .iter()
                .flat_map(|&sample| {
                    std::iter::repeat_n(0.0, output_channels as usize - 1)
                        .chain(std::iter::once(sample))
                })
                .collect(),
        };

        let output_frame = AudioFrame {
            samples: output_samples,
            channels: output_channels,
            sample_rate: input_frame.sample_rate,
            timestamp_ns: input_frame.timestamp_ns,
            frame_index: self.frame_counter.to_string(),
        };

        self.outputs.write("audio_out", &output_frame)?;
        self.frame_counter += 1;

        tracing::debug!(
            "[AudioChannelConverter] Processed frame {}",
            self.frame_counter
        );

        Ok(())
    }
}
