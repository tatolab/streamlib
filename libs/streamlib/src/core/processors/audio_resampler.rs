// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::frames::AudioFrame;
use crate::core::utils::audio_resample::{AudioResampler, ResamplingQuality};
use crate::core::{LinkInput, LinkOutput, Result, RuntimeContext};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioResamplerConfig {
    pub source_sample_rate: u32,
    pub target_sample_rate: u32,
    pub quality: ResamplingQuality,
}

impl Default for AudioResamplerConfig {
    fn default() -> Self {
        Self {
            source_sample_rate: 48000,
            target_sample_rate: 48000,
            quality: ResamplingQuality::High,
        }
    }
}

#[crate::processor(
    execution = Reactive,
    description = "Resamples audio from source to target sample rate (supports any channel count)"
)]
pub struct AudioResamplerProcessor {
    #[crate::input(description = "Audio input at source sample rate")]
    audio_in: LinkInput<AudioFrame>,

    #[crate::output(description = "Audio output at target sample rate")]
    audio_out: LinkOutput<AudioFrame>,

    #[crate::config]
    config: AudioResamplerConfig,

    resampler: Option<AudioResampler>,
    output_sample_rate: u32,
    frame_counter: u64,
}

impl crate::core::Processor for AudioResamplerProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.output_sample_rate = self.config.target_sample_rate;

        tracing::info!(
            "[AudioResampler] Initialized - will create resampler when first frame arrives (target: {}Hz, quality: {:?})",
            self.output_sample_rate,
            self.config.quality
        );

        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "[AudioResampler] Stopped (processed {} output frames)",
            self.frame_counter
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        if let Some(input_frame) = self.audio_in.read() {
            if self.resampler.is_none() {
                let input_sample_rate = input_frame.sample_rate;

                if input_sample_rate != self.output_sample_rate {
                    let chunk_size = input_frame.sample_count();

                    tracing::info!(
                        "[AudioResampler] Initializing: {}Hz → {}Hz ({:?}, {} channels, chunk_size={})",
                        input_sample_rate,
                        self.output_sample_rate,
                        input_frame.channels,
                        input_frame.channels(),
                        chunk_size
                    );

                    let resampler = AudioResampler::new(
                        input_sample_rate,
                        self.output_sample_rate,
                        input_frame.channels,
                        chunk_size,
                        self.config.quality,
                    )?;

                    self.resampler = Some(resampler);
                } else {
                    tracing::info!(
                        "[AudioResampler] Sample rates match ({}Hz) - passthrough mode",
                        input_sample_rate
                    );
                }
            }

            let output_samples = if let Some(ref mut resampler) = self.resampler {
                resampler.resample(&input_frame.samples)?
            } else {
                input_frame.samples.to_vec()
            };

            let output_frame = AudioFrame::new(
                output_samples,
                input_frame.channels,
                input_frame.timestamp_ns,
                self.frame_counter,
                self.output_sample_rate,
            );

            self.audio_out.write(output_frame);
            self.frame_counter += 1;

            tracing::debug!("[AudioResampler] Processed frame {}", self.frame_counter);
        }

        Ok(())
    }
}
