// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Result, RuntimeContext, StreamError};
use crate::schemas::{Audioframe1ch, Audioframe2ch};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
pub struct AudioMixerConfig {
    pub strategy: MixingStrategy,
}

impl Default for AudioMixerConfig {
    fn default() -> Self {
        Self {
            strategy: MixingStrategy::SumNormalized,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum MixingStrategy {
    Sum,
    #[default]
    SumNormalized,
    SumClipped,
}

#[crate::processor("schemas/processors/audio_mixer.yaml")]
pub struct AudioMixerProcessor;

impl crate::core::ReactiveProcessor for AudioMixerProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.sample_rate = 0;
        self.buffer_size = 0;
        self.frame_counter = 0;

        tracing::info!(
            "AudioMixer: Starting (sample_rate and buffer_size will be inferred from first input, strategy: {:?})",
            self.config.strategy
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("AudioMixer: Stopped");
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        tracing::debug!("[AudioMixer] process() called");

        let left_payload = match self.inputs.get("left") {
            Some(p) => p,
            None => {
                tracing::debug!("[AudioMixer] Left input has no data");
                return Ok(());
            }
        };

        let right_payload = match self.inputs.get("right") {
            Some(p) => p,
            None => {
                tracing::debug!("[AudioMixer] Right input has no data");
                return Ok(());
            }
        };

        let left_frame = Audioframe1ch::from_msgpack(left_payload.data())
            .map_err(|e| StreamError::Runtime(format!("left msgpack decode: {}", e)))?;
        let right_frame = Audioframe1ch::from_msgpack(right_payload.data())
            .map_err(|e| StreamError::Runtime(format!("right msgpack decode: {}", e)))?;

        if self.sample_rate == 0 {
            self.sample_rate = left_frame.sample_rate;
            self.buffer_size = left_frame.samples.len();
            tracing::info!(
                "[AudioMixer] Inferred config from first frame: {}Hz, {} samples",
                self.sample_rate,
                self.buffer_size
            );
        }

        if left_frame.sample_rate != self.sample_rate {
            tracing::warn!(
                "[AudioMixer] Dropping left frame with mismatched sample_rate {}Hz (expected {}Hz)",
                left_frame.sample_rate,
                self.sample_rate
            );
            return Ok(());
        }
        if left_frame.samples.len() != self.buffer_size {
            tracing::warn!(
                "[AudioMixer] Dropping left frame with mismatched buffer_size {} (expected {})",
                left_frame.samples.len(),
                self.buffer_size
            );
            return Ok(());
        }

        if right_frame.sample_rate != self.sample_rate {
            tracing::warn!(
                "[AudioMixer] Dropping right frame with mismatched sample_rate {}Hz (expected {}Hz)",
                right_frame.sample_rate,
                self.sample_rate
            );
            return Ok(());
        }
        if right_frame.samples.len() != self.buffer_size {
            tracing::warn!(
                "[AudioMixer] Dropping right frame with mismatched buffer_size {} (expected {})",
                right_frame.samples.len(),
                self.buffer_size
            );
            return Ok(());
        }

        let timestamp_ns = left_frame.timestamp_ns.max(right_frame.timestamp_ns);

        // Both inputs are mono, interleave into stereo
        let mut stereo_samples = Vec::with_capacity(self.buffer_size * 2);

        for i in 0..self.buffer_size {
            let left_sample = left_frame.samples[i];
            let right_sample = right_frame.samples[i];

            let (final_left, final_right) = match self.config.strategy {
                MixingStrategy::Sum => (left_sample, right_sample),
                MixingStrategy::SumNormalized => (left_sample, right_sample),
                MixingStrategy::SumClipped => {
                    (left_sample.clamp(-1.0, 1.0), right_sample.clamp(-1.0, 1.0))
                }
            };

            stereo_samples.push(final_left);
            stereo_samples.push(final_right);
        }

        let output_frame = Audioframe2ch {
            samples: stereo_samples,
            sample_rate: self.sample_rate,
            timestamp_ns,
            frame_index: self.frame_counter,
        };

        let bytes = output_frame
            .to_msgpack()
            .map_err(|e| StreamError::Runtime(format!("msgpack encode: {}", e)))?;
        self.outputs.write("audio", &bytes)?;

        tracing::debug!("[AudioMixer] Wrote mixed stereo frame");
        self.frame_counter += 1;

        Ok(())
    }
}
