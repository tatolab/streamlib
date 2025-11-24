use crate::core::audio_resample_utils::{AudioResampler, ResamplingQuality};
use crate::core::frames::AudioFrame;
use crate::core::{Result, StreamInput, StreamOutput};
use serde::{Deserialize, Serialize};
use streamlib_macros::StreamProcessor;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(StreamProcessor)]
#[processor(
    mode = Push,
    description = "Resamples audio from source to target sample rate (supports any channel count)"
)]
pub struct AudioResamplerProcessor {
    #[input(description = "Audio input at source sample rate")]
    audio_in: StreamInput<AudioFrame>,

    #[output(description = "Audio output at target sample rate")]
    audio_out: StreamOutput<AudioFrame>,

    #[config]
    config: AudioResamplerConfig,

    // Runtime state - resampler is created lazily when first frame arrives
    resampler: Option<AudioResampler>,
    output_sample_rate: u32,
    frame_counter: u64,
}

impl AudioResamplerProcessor {
    fn setup(&mut self, _ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.output_sample_rate = self.config.target_sample_rate;

        tracing::info!(
            "[AudioResampler] Initialized - will create resampler when first frame arrives (target: {}Hz, quality: {:?})",
            self.output_sample_rate,
            self.config.quality
        );

        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            "[AudioResampler] Stopped (processed {} output frames)",
            self.frame_counter
        );
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if let Some(input_frame) = self.audio_in.read() {
            // Lazy initialize resampler on first frame
            if self.resampler.is_none() {
                let input_sample_rate = input_frame.sample_rate;

                // Only create resampler if sample rates differ
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

            // Process the audio
            let output_samples = if let Some(ref mut resampler) = self.resampler {
                // Resample
                resampler.resample(&input_frame.samples)?
            } else {
                // Passthrough mode: no resampling needed
                input_frame.samples.to_vec()
            };

            // Create output frame with resampled audio
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
