// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::frames::AudioChannelCount;
use crate::core::utils::audio_resample::{AudioResampler, ResamplingQuality};
use crate::core::{Result, RuntimeContext, StreamError};
use crate::schemas::{Audioframe1ch, Audioframe2ch};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
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

// =============================================================================
// Mono (1-channel) Resampler
// =============================================================================

#[crate::processor(
    execution = Reactive,
    description = "Resamples mono audio from source to target sample rate",
    inputs = [input("audio_in", schema = "com.tatolab.audioframe.1ch@1.0.0")],
    outputs = [output("audio_out", schema = "com.tatolab.audioframe.1ch@1.0.0")]
)]
pub struct AudioResampler1chProcessor {
    #[crate::config]
    config: AudioResamplerConfig,

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
        if let Some(payload) = self.inputs.get("audio_in") {
            let input_frame = Audioframe1ch::from_msgpack(payload.data())
                .map_err(|e| StreamError::Runtime(format!("msgpack decode: {}", e)))?;

            if self.resampler.is_none() {
                let input_sample_rate = input_frame.sample_rate;

                if input_sample_rate != self.output_sample_rate {
                    let chunk_size = input_frame.samples.len();

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
                        self.config.quality,
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

            let output_frame = Audioframe1ch {
                samples: output_samples,
                sample_rate: self.output_sample_rate,
                timestamp_ns: input_frame.timestamp_ns,
                frame_index: self.frame_counter,
            };

            let bytes = output_frame.to_msgpack()
                .map_err(|e| StreamError::Runtime(format!("msgpack encode: {}", e)))?;
            self.outputs.write("audio_out", &bytes)?;
            self.frame_counter += 1;

            tracing::debug!("[AudioResampler1ch] Processed frame {}", self.frame_counter);
        }

        Ok(())
    }
}

// =============================================================================
// Stereo (2-channel) Resampler
// =============================================================================

#[crate::processor(
    execution = Reactive,
    description = "Resamples stereo audio from source to target sample rate",
    inputs = [input("audio_in", schema = "com.tatolab.audioframe.2ch@1.0.0")],
    outputs = [output("audio_out", schema = "com.tatolab.audioframe.2ch@1.0.0")]
)]
pub struct AudioResampler2chProcessor {
    #[crate::config]
    config: AudioResamplerConfig,

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
        if let Some(payload) = self.inputs.get("audio_in") {
            let input_frame = Audioframe2ch::from_msgpack(payload.data())
                .map_err(|e| StreamError::Runtime(format!("msgpack decode: {}", e)))?;

            if self.resampler.is_none() {
                let input_sample_rate = input_frame.sample_rate;

                if input_sample_rate != self.output_sample_rate {
                    // For stereo, chunk_size is samples.len() / 2 (interleaved)
                    let chunk_size = input_frame.samples.len() / 2;

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
                        self.config.quality,
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

            let output_frame = Audioframe2ch {
                samples: output_samples,
                sample_rate: self.output_sample_rate,
                timestamp_ns: input_frame.timestamp_ns,
                frame_index: self.frame_counter,
            };

            let bytes = output_frame.to_msgpack()
                .map_err(|e| StreamError::Runtime(format!("msgpack encode: {}", e)))?;
            self.outputs.write("audio_out", &bytes)?;
            self.frame_counter += 1;

            tracing::debug!("[AudioResampler2ch] Processed frame {}", self.frame_counter);
        }

        Ok(())
    }
}
