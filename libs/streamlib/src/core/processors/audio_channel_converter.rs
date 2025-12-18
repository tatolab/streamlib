// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::frames::{AudioChannelCount, AudioFrame};
use crate::core::{LinkInput, LinkOutput, Result, RuntimeContext};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ChannelConversionMode {
    /// Duplicate mono signal to both left and right channels
    #[default]
    Duplicate,
    /// Place mono signal only in left channel, silence right
    LeftOnly,
    /// Place mono signal only in right channel, silence left
    RightOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioChannelConverterConfig {
    pub mode: ChannelConversionMode,
}

impl Default for AudioChannelConverterConfig {
    fn default() -> Self {
        Self {
            mode: ChannelConversionMode::Duplicate,
        }
    }
}

#[crate::processor(
    execution = Reactive,
    description = "Converts mono audio to stereo using configurable channel mapping"
)]
pub struct AudioChannelConverterProcessor {
    #[crate::input(description = "Mono audio input")]
    audio_in: LinkInput<AudioFrame>,

    #[crate::output(description = "Stereo audio output")]
    audio_out: LinkOutput<AudioFrame>,

    #[crate::config]
    config: AudioChannelConverterConfig,

    frame_counter: u64,
}

impl crate::core::Processor for AudioChannelConverterProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        tracing::info!(
            "[AudioChannelConverter] setup() - mode: {:?}",
            self.config.mode
        );
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            "[AudioChannelConverter] Stopped (processed {} frames)",
            self.frame_counter
        );
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Read mono input frame
        if let Some(input_frame) = self.audio_in.read() {
            // Convert mono samples to stereo based on mode
            let stereo_samples: Vec<f32> = match self.config.mode {
                ChannelConversionMode::Duplicate => {
                    // Duplicate each mono sample to both L and R channels
                    input_frame
                        .samples
                        .iter()
                        .flat_map(|&sample| [sample, sample])
                        .collect()
                }
                ChannelConversionMode::LeftOnly => {
                    // Place mono signal in left channel, silence in right
                    input_frame
                        .samples
                        .iter()
                        .flat_map(|&sample| [sample, 0.0])
                        .collect()
                }
                ChannelConversionMode::RightOnly => {
                    // Silence in left channel, mono signal in right
                    input_frame
                        .samples
                        .iter()
                        .flat_map(|&sample| [0.0, sample])
                        .collect()
                }
            };

            // Create stereo output frame
            let output_frame = AudioFrame::new(
                stereo_samples,
                AudioChannelCount::Two,
                input_frame.timestamp_ns,
                self.frame_counter,
                input_frame.sample_rate,
            );

            self.audio_out.write(output_frame);
            self.frame_counter += 1;

            tracing::debug!(
                "[AudioChannelConverter] Processed frame {}",
                self.frame_counter
            );
        }

        Ok(())
    }
}
