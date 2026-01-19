// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Result, RuntimeContext, StreamError};
use crate::schemas::{Audioframe1ch, Audioframe2ch};
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
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
    description = "Converts mono audio to stereo using configurable channel mapping",
    inputs = [input("audio_in", schema = "com.tatolab.audioframe.1ch@1.0.0")],
    outputs = [output("audio_out", schema = "com.tatolab.audioframe.2ch@1.0.0")]
)]
pub struct AudioChannelConverterProcessor {
    #[crate::config]
    config: AudioChannelConverterConfig,

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
        // Read mono input frame from iceoryx2
        if let Some(payload) = self.inputs.get("audio_in") {
            let input_frame = Audioframe1ch::from_msgpack(payload.data())
                .map_err(|e| StreamError::Runtime(format!("msgpack decode: {}", e)))?;

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

            let output_frame = Audioframe2ch {
                samples: stereo_samples,
                sample_rate: input_frame.sample_rate,
                timestamp_ns: input_frame.timestamp_ns,
                frame_index: self.frame_counter,
            };

            let bytes = output_frame.to_msgpack()
                .map_err(|e| StreamError::Runtime(format!("msgpack encode: {}", e)))?;
            self.outputs.write("audio_out", &bytes)?;
            self.frame_counter += 1;

            tracing::debug!(
                "[AudioChannelConverter] Processed frame {}",
                self.frame_counter
            );
        }

        Ok(())
    }
}
