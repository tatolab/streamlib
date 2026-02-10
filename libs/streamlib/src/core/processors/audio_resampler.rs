// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_audio_resampler_config::Quality;
use crate::_generated_::Audioframe;
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

#[crate::processor("com.tatolab.audio_resampler")]
pub struct AudioResamplerProcessor {
    resampler: Option<AudioResampler>,
    output_sample_rate: u32,
    frame_counter: u64,
    channels: u8,
}

impl crate::core::ReactiveProcessor for AudioResamplerProcessor::Processor {
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
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }

        let input_frame: Audioframe = self.inputs.read("audio_in")?;

        // Store channels from first frame
        if self.channels == 0 {
            self.channels = input_frame.channels;
        }

        if self.resampler.is_none() {
            let input_sample_rate = input_frame.sample_rate;

            if input_sample_rate != self.output_sample_rate {
                // Chunk size is samples per channel
                let chunk_size = input_frame.samples.len() / input_frame.channels as usize;
                let quality = quality_to_resampling_quality(&self.config.quality);

                tracing::info!(
                    "[AudioResampler] Initializing: {}Hz → {}Hz ({:?}, {} channels, chunk_size={})",
                    input_sample_rate,
                    self.output_sample_rate,
                    self.config.quality,
                    input_frame.channels,
                    chunk_size
                );

                let resampler = AudioResampler::new(
                    input_sample_rate,
                    self.output_sample_rate,
                    input_frame.channels,
                    chunk_size,
                    quality,
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
            input_frame.samples.clone()
        };

        let output_frame = Audioframe {
            samples: output_samples,
            channels: self.channels,
            sample_rate: self.output_sample_rate,
            timestamp_ns: input_frame.timestamp_ns.clone(),
            frame_index: self.frame_counter.to_string(),
        };

        self.outputs.write("audio_out", &output_frame)?;
        self.frame_counter += 1;

        tracing::debug!("[AudioResampler] Processed frame {}", self.frame_counter);

        Ok(())
    }
}
