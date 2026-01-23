// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_audio_channel_converter_config::Mode;
use crate::_generated_::{Audioframe1Ch, Audioframe2Ch};
use crate::core::{Result, RuntimeContext};

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

        // Read mono input frame from iceoryx2
        let input_frame: Audioframe1Ch = self.inputs.read("audio_in")?;

        // Convert mono samples to stereo based on mode
        let stereo_samples: Vec<f32> = match self.config.mode {
            Mode::Duplicate => {
                // Duplicate each mono sample to both L and R channels
                input_frame
                    .samples
                    .iter()
                    .flat_map(|&sample| [sample, sample])
                    .collect()
            }
            Mode::LeftOnly => {
                // Place mono signal in left channel, silence in right
                input_frame
                    .samples
                    .iter()
                    .flat_map(|&sample| [sample, 0.0])
                    .collect()
            }
            Mode::RightOnly => {
                // Silence in left channel, mono signal in right
                input_frame
                    .samples
                    .iter()
                    .flat_map(|&sample| [0.0, sample])
                    .collect()
            }
        };

        let output_frame = Audioframe2Ch {
            samples: stereo_samples,
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
