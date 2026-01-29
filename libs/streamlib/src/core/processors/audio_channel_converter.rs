// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_audio_channel_converter_config::Mode;
use crate::_generated_::Audioframe;
use crate::core::{Result, RuntimeContext, StreamError};

#[crate::processor("src/core/processors/audio_channel_converter.yaml")]
pub struct AudioChannelConverterProcessor {
    frame_counter: u64,
}

impl crate::core::ReactiveProcessor for AudioChannelConverterProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "[AudioChannelConverter] setup() - mode: {:?}",
            self.config.mode
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "[AudioChannelConverter] Stopped (processed {} frames)",
            self.frame_counter
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        // Check if data available before trying to read
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }

        // Read input frame from iceoryx2
        let input_frame: Audioframe = self.inputs.read("audio_in")?;

        // Validate input is mono (1 channel)
        if input_frame.channels != 1 {
            return Err(StreamError::Configuration(format!(
                "AudioChannelConverter expects mono input (1 channel), got {} channels",
                input_frame.channels
            )));
        }

        // Get output channel count from config (default: 2 for stereo)
        let output_channels = self.config.output_channels.unwrap_or(2);

        // Convert mono samples to output channels based on mode
        let output_samples: Vec<f32> = match self.config.mode {
            Mode::Duplicate => {
                // Duplicate each mono sample to all output channels
                input_frame
                    .samples
                    .iter()
                    .flat_map(|&sample| std::iter::repeat_n(sample, output_channels as usize))
                    .collect()
            }
            Mode::LeftOnly => {
                // Place mono signal in first channel, silence in others
                input_frame
                    .samples
                    .iter()
                    .flat_map(|&sample| {
                        std::iter::once(sample)
                            .chain(std::iter::repeat_n(0.0, output_channels as usize - 1))
                    })
                    .collect()
            }
            Mode::RightOnly => {
                // Silence in first channel(s), mono signal in last channel
                input_frame
                    .samples
                    .iter()
                    .flat_map(|&sample| {
                        std::iter::repeat_n(0.0, output_channels as usize - 1)
                            .chain(std::iter::once(sample))
                    })
                    .collect()
            }
        };

        let output_frame = Audioframe {
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
