// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::frames::{AudioChannelCount, AudioFrame};
use crate::core::{LinkInput, LinkOutput, Result, RuntimeContext};
use dasp::Signal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[crate::processor(
    execution = Reactive,
    description = "Mixes two mono signals (left and right) into a stereo signal"
)]
pub struct AudioMixerProcessor {
    #[crate::input(description = "Left channel mono audio input")]
    left: LinkInput<AudioFrame>,

    #[crate::input(description = "Right channel mono audio input")]
    right: LinkInput<AudioFrame>,

    #[crate::output(description = "Mixed stereo audio output")]
    audio: LinkOutput<AudioFrame>,

    #[crate::config]
    config: AudioMixerConfig,

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

        let left_frame = match self.left.read() {
            Some(f) => f,
            None => {
                tracing::debug!("[AudioMixer] Left input has no data");
                return Ok(());
            }
        };

        let right_frame = match self.right.read() {
            Some(f) => f,
            None => {
                tracing::debug!("[AudioMixer] Right input has no data");
                return Ok(());
            }
        };

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

        let mut left_signal = left_frame.read();
        let mut right_signal = right_frame.read();

        let mut stereo_samples = Vec::with_capacity(self.buffer_size * 2);

        for _ in 0..self.buffer_size {
            let left_sample = left_signal.next().as_slice()[0];
            let right_sample = right_signal.next().as_slice()[0];

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

        let output_frame = AudioFrame::new(
            stereo_samples,
            AudioChannelCount::Two,
            timestamp_ns,
            self.frame_counter,
            self.sample_rate,
        );
        self.audio.write(output_frame);

        tracing::debug!("[AudioMixer] Wrote mixed stereo frame");
        self.frame_counter += 1;

        Ok(())
    }
}
