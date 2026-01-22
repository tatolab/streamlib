// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_audio_resampler_config::Quality;
use crate::_generated_::{Audioframe1Ch, Audioframe2Ch};
use crate::core::frames::AudioChannelCount;
use crate::core::utils::audio_resample::{AudioResampler, ResamplingQuality};
use crate::core::{Result, RuntimeContext};

/// Convert generated Quality enum to internal ResamplingQuality.
fn quality_to_resampling_quality(quality: &Quality) -> ResamplingQuality {
    match quality {
        Quality::High => ResamplingQuality::High,
        Quality::Medium => ResamplingQuality::Medium,
        Quality::Low => ResamplingQuality::Low,
    }
}

// =============================================================================
// Mono (1-channel) Resampler
// =============================================================================

#[crate::processor("src/core/processors/audio_resampler_1ch.yaml")]
pub struct AudioResampler1chProcessor {
    resampler: Option<AudioResampler>,
    output_sample_rate: u32,
    frame_counter: u64,
}

impl crate::core::ReactiveProcessor for AudioResampler1chProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.output_sample_rate = self.config.target_sample_rate;

        tracing::info!(
            "[AudioResampler1ch] Initialized - will create resampler when first frame arrives (target: {}Hz, quality: {:?})",
            self.output_sample_rate,
            self.config.quality
        );

        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "[AudioResampler1ch] Stopped (processed {} output frames)",
            self.frame_counter
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }

        let input_frame: Audioframe1Ch = self.inputs.read("audio_in")?;

        if self.resampler.is_none() {
            let input_sample_rate = input_frame.sample_rate;

            if input_sample_rate != self.output_sample_rate {
                let chunk_size = input_frame.samples.len();
                let quality = quality_to_resampling_quality(&self.config.quality);

                tracing::info!(
                    "[AudioResampler1ch] Initializing: {}Hz → {}Hz ({:?}, 1 channel, chunk_size={})",
                    input_sample_rate,
                    self.output_sample_rate,
                    self.config.quality,
                    chunk_size
                );

                let resampler = AudioResampler::new(
                    input_sample_rate,
                    self.output_sample_rate,
                    AudioChannelCount::One,
                    chunk_size,
                    quality,
                )?;

                self.resampler = Some(resampler);
            } else {
                tracing::info!(
                    "[AudioResampler1ch] Sample rates match ({}Hz) - passthrough mode",
                    input_sample_rate
                );
            }
        }

        let output_samples = if let Some(ref mut resampler) = self.resampler {
            resampler.resample(&input_frame.samples)?
        } else {
            input_frame.samples.clone()
        };

        let output_frame = Audioframe1Ch {
            samples: output_samples,
            sample_rate: self.output_sample_rate,
            timestamp_ns: input_frame.timestamp_ns.clone(),
            frame_index: self.frame_counter.to_string(),
        };

        self.outputs.write("audio_out", &output_frame)?;
        self.frame_counter += 1;

        tracing::debug!("[AudioResampler1ch] Processed frame {}", self.frame_counter);

        Ok(())
    }
}

// =============================================================================
// Stereo (2-channel) Resampler
// =============================================================================

#[crate::processor("src/core/processors/audio_resampler_2ch.yaml")]
pub struct AudioResampler2chProcessor {
    resampler: Option<AudioResampler>,
    output_sample_rate: u32,
    frame_counter: u64,
}

impl crate::core::ReactiveProcessor for AudioResampler2chProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.output_sample_rate = self.config.target_sample_rate;

        tracing::info!(
            "[AudioResampler2ch] Initialized - will create resampler when first frame arrives (target: {}Hz, quality: {:?})",
            self.output_sample_rate,
            self.config.quality
        );

        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "[AudioResampler2ch] Stopped (processed {} output frames)",
            self.frame_counter
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }

        let input_frame: Audioframe2Ch = self.inputs.read("audio_in")?;

        if self.resampler.is_none() {
            let input_sample_rate = input_frame.sample_rate;

            if input_sample_rate != self.output_sample_rate {
                // For stereo, chunk_size is samples.len() / 2 (interleaved)
                let chunk_size = input_frame.samples.len() / 2;
                let quality = quality_to_resampling_quality(&self.config.quality);

                tracing::info!(
                    "[AudioResampler2ch] Initializing: {}Hz → {}Hz ({:?}, 2 channels, chunk_size={})",
                    input_sample_rate,
                    self.output_sample_rate,
                    self.config.quality,
                    chunk_size
                );

                let resampler = AudioResampler::new(
                    input_sample_rate,
                    self.output_sample_rate,
                    AudioChannelCount::Two,
                    chunk_size,
                    quality,
                )?;

                self.resampler = Some(resampler);
            } else {
                tracing::info!(
                    "[AudioResampler2ch] Sample rates match ({}Hz) - passthrough mode",
                    input_sample_rate
                );
            }
        }

        let output_samples = if let Some(ref mut resampler) = self.resampler {
            resampler.resample(&input_frame.samples)?
        } else {
            input_frame.samples.clone()
        };

        let output_frame = Audioframe2Ch {
            samples: output_samples,
            sample_rate: self.output_sample_rate,
            timestamp_ns: input_frame.timestamp_ns.clone(),
            frame_index: self.frame_counter.to_string(),
        };

        self.outputs.write("audio_out", &output_frame)?;
        self.frame_counter += 1;

        tracing::debug!("[AudioResampler2ch] Processed frame {}", self.frame_counter);

        Ok(())
    }
}
