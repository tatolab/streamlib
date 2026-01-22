// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::com_tatolab_audio_mixer_config::Strategy;
use crate::_generated_::{Audioframe1Ch, Audioframe2Ch};
use crate::core::{Result, RuntimeContext};

#[crate::processor("src/core/processors/audio_mixer.yaml")]
pub struct AudioMixerProcessor {
    sample_rate: u32,
    buffer_size: usize,
    frame_counter: u64,
}

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

        // Check both inputs have data
        if !self.inputs.has_data("left") || !self.inputs.has_data("right") {
            tracing::debug!("[AudioMixer] Waiting for both inputs");
            return Ok(());
        }

        let left_frame: Audioframe1Ch = self.inputs.read("left")?;
        let right_frame: Audioframe1Ch = self.inputs.read("right")?;

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

        // Parse timestamp strings to compare (use lexicographic comparison for now)
        let timestamp_ns = if left_frame.timestamp_ns > right_frame.timestamp_ns {
            left_frame.timestamp_ns.clone()
        } else {
            right_frame.timestamp_ns.clone()
        };

        // Both inputs are mono, interleave into stereo
        let mut stereo_samples = Vec::with_capacity(self.buffer_size * 2);

        for i in 0..self.buffer_size {
            let left_sample = left_frame.samples[i];
            let right_sample = right_frame.samples[i];

            let (final_left, final_right) = match self.config.strategy {
                Strategy::Sum => (left_sample, right_sample),
                Strategy::SumNormalized => (left_sample, right_sample),
                Strategy::SumClipped => {
                    (left_sample.clamp(-1.0, 1.0), right_sample.clamp(-1.0, 1.0))
                }
            };

            stereo_samples.push(final_left);
            stereo_samples.push(final_right);
        }

        let output_frame = Audioframe2Ch {
            samples: stereo_samples,
            sample_rate: self.sample_rate,
            timestamp_ns,
            frame_index: self.frame_counter.to_string(),
        };

        self.outputs.write("audio", &output_frame)?;

        tracing::debug!("[AudioMixer] Wrote mixed stereo frame");
        self.frame_counter += 1;

        Ok(())
    }
}
