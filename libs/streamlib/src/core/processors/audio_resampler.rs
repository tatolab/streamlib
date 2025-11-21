use crate::core::frames::AudioFrame;
use crate::core::{Result, StreamError, StreamInput, StreamOutput};
use rubato::{
    Resampler, SincFixedOut, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib_macros::StreamProcessor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResamplingQuality {
    High,
    Medium,
    Low,
}

impl ResamplingQuality {
    fn to_parameters(&self) -> SincInterpolationParameters {
        match self {
            ResamplingQuality::High => SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Cubic,
                oversampling_factor: 256,
                window: WindowFunction::BlackmanHarris2,
            },
            ResamplingQuality::Medium => SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            },
            ResamplingQuality::Low => SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.90,
                interpolation: SincInterpolationType::Nearest,
                oversampling_factor: 64,
                window: WindowFunction::Blackman,
            },
        }
    }
}

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
    description = "Resamples mono audio from source sample rate to target sample rate using high-quality sinc interpolation"
)]
pub struct AudioResamplerProcessor {
    #[input(description = "Mono audio input at source sample rate")]
    audio_in: StreamInput<AudioFrame<1>>,

    #[output(description = "Mono audio output at target sample rate")]
    audio_out: Arc<StreamOutput<AudioFrame<1>>>,

    #[config]
    config: AudioResamplerConfig,

    // Runtime state fields - auto-detected (no attribute needed)
    resampler: Option<SincFixedOut<f32>>,
    input_sample_rate: u32,
    output_sample_rate: u32,
    frame_counter: u64,
    input_buffer: Vec<f32>,
}

impl AudioResamplerProcessor {
    fn setup(&mut self, _ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.output_sample_rate = self.config.target_sample_rate;

        tracing::info!(
            "[AudioResampler] setup() - will initialize resampler when first frame arrives"
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
        // Read input frame
        if let Some(input_frame) = self.audio_in.read() {
            // Initialize resampler on first frame (use actual frame sample rate, not config)
            if self.resampler.is_none() {
                self.input_sample_rate = input_frame.sample_rate;

                // Only create resampler if sample rates differ
                if self.input_sample_rate != self.output_sample_rate {
                    let ratio = self.output_sample_rate as f64 / self.input_sample_rate as f64;
                    let params = self.config.quality.to_parameters();

                    // Calculate fixed output chunk size based on input frame size
                    let input_chunk_size = input_frame.samples.len();
                    let output_chunk_size = (input_chunk_size as f64 * ratio).round() as usize;

                    tracing::info!(
                        "[AudioResampler] Initializing resampler: {}Hz → {}Hz (ratio: {:.4}, quality: {:?})",
                        self.input_sample_rate,
                        self.output_sample_rate,
                        ratio,
                        self.config.quality
                    );
                    tracing::info!(
                        "[AudioResampler] Chunk sizes: input={} samples, output={} samples",
                        input_chunk_size,
                        output_chunk_size
                    );

                    let resampler = SincFixedOut::<f32>::new(
                        ratio,
                        2.0, // max relative ratio change
                        params,
                        output_chunk_size,
                        1, // mono channel
                    )
                    .map_err(|e| {
                        StreamError::Configuration(format!("Failed to create resampler: {}", e))
                    })?;

                    self.resampler = Some(resampler);
                } else {
                    tracing::info!(
                        "[AudioResampler] Sample rates match ({}Hz), resampling disabled (passthrough mode)",
                        self.input_sample_rate
                    );
                }
            }

            // Process the audio
            let output_samples = if let Some(ref mut resampler) = self.resampler {
                // Get how many input frames the resampler needs
                let frames_needed = resampler.input_frames_next();

                // Accumulate input samples in buffer (convert Arc<Vec> to Vec)
                self.input_buffer.extend_from_slice(&input_frame.samples);

                let available_frames = self.input_buffer.len();

                // Check if we have enough samples to resample
                if available_frames < frames_needed {
                    tracing::debug!(
                        "[AudioResampler] Buffering: have {} frames, need {} frames",
                        available_frames,
                        frames_needed
                    );
                    return Ok(());
                }

                // Take exactly the number of frames needed
                let input_samples: Vec<f32> = self.input_buffer.drain(..frames_needed).collect();

                // Resample (input is already mono, output will be mono)
                let waves_in = vec![input_samples];
                match resampler.process(&waves_in, None) {
                    Ok(waves_out) => waves_out[0].clone(),
                    Err(e) => {
                        tracing::error!("[AudioResampler] Resampling failed: {}", e);
                        return Err(StreamError::Configuration(format!(
                            "Resampling failed: {}",
                            e
                        )));
                    }
                }
            } else {
                // Passthrough mode: no resampling needed
                input_frame.samples.to_vec()
            };

            // Create output frame with resampled audio
            let output_frame = AudioFrame::<1>::new(
                output_samples,
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
