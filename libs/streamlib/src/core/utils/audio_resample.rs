use crate::core::frames::AudioChannelCount;
use crate::core::{Result, StreamError};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde::{Deserialize, Serialize};

/// Quality presets for audio resampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResamplingQuality {
    High,
    Medium,
    Low,
}

impl ResamplingQuality {
    /// Convert to rubato interpolation parameters.
    pub fn to_parameters(&self) -> SincInterpolationParameters {
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

/// Multi-channel audio resampler (1-8 channels).
pub struct AudioResampler {
    inner: ResamplerInner,
    source_sample_rate: u32,
    target_sample_rate: u32,
    channels: AudioChannelCount,
    quality: ResamplingQuality,
}

enum ResamplerInner {
    One(SincFixedIn<f32>),
    Two(SincFixedIn<f32>),
    Three(SincFixedIn<f32>),
    Four(SincFixedIn<f32>),
    Five(SincFixedIn<f32>),
    Six(SincFixedIn<f32>),
    Seven(SincFixedIn<f32>),
    Eight(SincFixedIn<f32>),
}

impl AudioResampler {
    /// Create a new multi-channel audio resampler.
    pub fn new(
        source_rate: u32,
        target_rate: u32,
        channels: AudioChannelCount,
        chunk_size: usize,
        quality: ResamplingQuality,
    ) -> Result<Self> {
        let ratio = target_rate as f64 / source_rate as f64;
        let params = quality.to_parameters();

        // Create channel-specific resampler
        let inner = match channels {
            AudioChannelCount::One => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 1)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 1-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::One(resampler)
            }
            AudioChannelCount::Two => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 2)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 2-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::Two(resampler)
            }
            AudioChannelCount::Three => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 3)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 3-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::Three(resampler)
            }
            AudioChannelCount::Four => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 4)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 4-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::Four(resampler)
            }
            AudioChannelCount::Five => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 5)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 5-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::Five(resampler)
            }
            AudioChannelCount::Six => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 6)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 6-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::Six(resampler)
            }
            AudioChannelCount::Seven => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 7)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 7-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::Seven(resampler)
            }
            AudioChannelCount::Eight => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 8)
                    .map_err(|e| {
                        StreamError::Runtime(format!(
                            "Failed to create 8-channel resampler: {:?}",
                            e
                        ))
                    })?;
                ResamplerInner::Eight(resampler)
            }
        };

        Ok(Self {
            inner,
            source_sample_rate: source_rate,
            target_sample_rate: target_rate,
            channels,
            quality,
        })
    }

    /// Resample interleaved multi-channel audio.
    pub fn resample(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        let channels_usize = self.channels.as_usize();
        // Convert interleaved to planar (separate channels)
        let samples_per_channel = input.len() / channels_usize;
        let mut planar_input: Vec<Vec<f32>> =
            vec![Vec::with_capacity(samples_per_channel); channels_usize];

        for chunk in input.chunks_exact(channels_usize) {
            for (ch_idx, &sample) in chunk.iter().enumerate() {
                planar_input[ch_idx].push(sample);
            }
        }

        // Resample using the appropriate inner resampler
        let planar_output = match &mut self.inner {
            ResamplerInner::One(r) => r.process(&planar_input, None),
            ResamplerInner::Two(r) => r.process(&planar_input, None),
            ResamplerInner::Three(r) => r.process(&planar_input, None),
            ResamplerInner::Four(r) => r.process(&planar_input, None),
            ResamplerInner::Five(r) => r.process(&planar_input, None),
            ResamplerInner::Six(r) => r.process(&planar_input, None),
            ResamplerInner::Seven(r) => r.process(&planar_input, None),
            ResamplerInner::Eight(r) => r.process(&planar_input, None),
        }
        .map_err(|e| StreamError::Runtime(format!("Resampling failed: {:?}", e)))?;

        // Convert back to interleaved
        let output_samples_per_channel = planar_output[0].len();
        let mut interleaved_output =
            Vec::with_capacity(output_samples_per_channel * channels_usize);

        for i in 0..output_samples_per_channel {
            for channel in planar_output.iter().take(channels_usize) {
                interleaved_output.push(channel[i]);
            }
        }

        Ok(interleaved_output)
    }

    pub fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }

    pub fn target_sample_rate(&self) -> u32 {
        self.target_sample_rate
    }

    pub fn channels(&self) -> AudioChannelCount {
        self.channels
    }

    pub fn quality(&self) -> ResamplingQuality {
        self.quality
    }
}

/// Legacy stereo-only resampler. Prefer [`AudioResampler`].
pub struct StereoResampler {
    inner: AudioResampler,
}

impl StereoResampler {
    pub fn new(
        source_rate: u32,
        target_rate: u32,
        chunk_size: usize,
        quality: ResamplingQuality,
    ) -> Result<Self> {
        let inner = AudioResampler::new(
            source_rate,
            target_rate,
            AudioChannelCount::Two,
            chunk_size,
            quality,
        )?;
        Ok(Self { inner })
    }

    pub fn resample(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        self.inner.resample(input)
    }

    pub fn source_sample_rate(&self) -> u32 {
        self.inner.source_sample_rate()
    }

    pub fn target_sample_rate(&self) -> u32 {
        self.inner.target_sample_rate()
    }

    pub fn quality(&self) -> ResamplingQuality {
        self.inner.quality()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_resampler_mono() {
        // Create mono resampler: 48kHz → 24kHz
        let mut resampler = AudioResampler::new(
            48000,
            24000,
            AudioChannelCount::One,
            960,
            ResamplingQuality::Medium,
        )
        .expect("Failed to create mono resampler");

        assert_eq!(resampler.source_sample_rate(), 48000);
        assert_eq!(resampler.target_sample_rate(), 24000);
        assert_eq!(resampler.channels(), AudioChannelCount::One);

        // Create test input: 960 mono samples
        let input: Vec<f32> = (0..960).map(|i| (i as f32) / 960.0).collect();

        // Resample
        let output = resampler.resample(&input).expect("Resampling failed");

        // Output should be ~half the length (2:1 downsampling)
        assert!(
            output.len() > 400 && output.len() < 600,
            "Output length: {}",
            output.len()
        );
    }

    #[test]
    fn test_audio_resampler_stereo() {
        // Create stereo resampler: 48kHz → 24kHz (2:1 ratio)
        let mut resampler = AudioResampler::new(
            48000,
            24000,
            AudioChannelCount::Two,
            960,
            ResamplingQuality::Medium,
        )
        .expect("Failed to create stereo resampler");

        assert_eq!(resampler.channels(), AudioChannelCount::Two);

        // Create test input: 960 samples per channel = 1920 total samples (interleaved)
        let input: Vec<f32> = (0..1920)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();

        // Resample
        let output = resampler.resample(&input).expect("Resampling failed");

        // Output should be ~half the length (2:1 downsampling)
        assert!(
            output.len() > 800 && output.len() < 1200,
            "Output length: {}",
            output.len()
        );

        // Output should still be interleaved stereo
        assert_eq!(output.len() % 2, 0);
    }

    #[test]
    fn test_audio_resampler_quad() {
        // Create quad resampler: 48kHz → 24kHz
        let mut resampler = AudioResampler::new(
            48000,
            24000,
            AudioChannelCount::Four,
            960,
            ResamplingQuality::Medium,
        )
        .expect("Failed to create quad resampler");

        assert_eq!(resampler.channels(), AudioChannelCount::Four);

        // Create test input: 960 samples per channel = 3840 total samples (interleaved)
        let input: Vec<f32> = (0..3840).map(|i| (i % 4) as f32 / 4.0).collect();

        // Resample
        let output = resampler.resample(&input).expect("Resampling failed");

        // Output should be ~half the length (2:1 downsampling)
        assert!(
            output.len() > 1600 && output.len() < 2400,
            "Output length: {}",
            output.len()
        );

        // Output should still be interleaved quad
        assert_eq!(output.len() % 4, 0);
    }

    #[test]
    fn test_audio_resampler_surround_5_1() {
        // Create 5.1 resampler: 48kHz → 24kHz
        let mut resampler = AudioResampler::new(
            48000,
            24000,
            AudioChannelCount::Six,
            960,
            ResamplingQuality::Medium,
        )
        .expect("Failed to create 5.1 resampler");

        assert_eq!(resampler.channels(), AudioChannelCount::Six);

        // Create test input: 960 samples per channel = 5760 total samples (interleaved)
        let input: Vec<f32> = (0..5760).map(|i| (i % 6) as f32 / 6.0).collect();

        // Resample
        let output = resampler.resample(&input).expect("Resampling failed");

        // Output should be ~half the length (2:1 downsampling)
        assert!(
            output.len() > 2400 && output.len() < 3600,
            "Output length: {}",
            output.len()
        );

        // Output should still be interleaved 5.1
        assert_eq!(output.len() % 6, 0);
    }

    #[test]
    fn test_audio_resampler_surround_7_1() {
        // Create 7.1 resampler: 48kHz → 24kHz
        let mut resampler = AudioResampler::new(
            48000,
            24000,
            AudioChannelCount::Eight,
            960,
            ResamplingQuality::Medium,
        )
        .expect("Failed to create 7.1 resampler");

        assert_eq!(resampler.channels(), AudioChannelCount::Eight);

        // Create test input: 960 samples per channel = 7680 total samples (interleaved)
        let input: Vec<f32> = (0..7680).map(|i| (i % 8) as f32 / 8.0).collect();

        // Resample
        let output = resampler.resample(&input).expect("Resampling failed");

        // Output should be ~half the length (2:1 downsampling)
        assert!(
            output.len() > 3200 && output.len() < 4800,
            "Output length: {}",
            output.len()
        );

        // Output should still be interleaved 7.1
        assert_eq!(output.len() % 8, 0);
    }

    #[test]
    fn test_audio_resampler_upsampling() {
        // Create resampler: 24kHz → 48kHz (1:2 ratio)
        let mut resampler = AudioResampler::new(
            24000,
            48000,
            AudioChannelCount::Two,
            480,
            ResamplingQuality::Medium,
        )
        .expect("Failed to create resampler");

        // Create test input: 480 samples per channel = 960 total samples (interleaved)
        let input: Vec<f32> = (0..960)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();

        // Resample
        let output = resampler.resample(&input).expect("Resampling failed");

        // Output should be ~double the length (1:2 upsampling)
        assert!(
            output.len() > 1600 && output.len() < 2400,
            "Output length: {}",
            output.len()
        );

        // Output should still be interleaved stereo
        assert_eq!(output.len() % 2, 0);
    }

    #[test]
    fn test_audio_resampler_invalid_channels() {
        // Try to create resampler with unsupported channel count (9 channels, outside 1-8 range)
        // Note: 3 and 5 channels are now supported, so we test with an actually unsupported count
        // Since AudioChannelCount only goes up to 8, we can't directly test invalid counts
        // through the API. This test now verifies that valid channel counts work.
        let result = AudioResampler::new(
            48000,
            24000,
            AudioChannelCount::Three,
            960,
            ResamplingQuality::Medium,
        );
        assert!(result.is_ok());

        let result = AudioResampler::new(
            48000,
            24000,
            AudioChannelCount::Five,
            960,
            ResamplingQuality::Medium,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_stereo_resampler_backward_compat() {
        // Test backward compatibility with StereoResampler
        let mut resampler = StereoResampler::new(48000, 24000, 960, ResamplingQuality::Medium)
            .expect("Failed to create resampler");

        assert_eq!(resampler.source_sample_rate(), 48000);
        assert_eq!(resampler.target_sample_rate(), 24000);

        // Create test input: 960 samples per channel = 1920 total samples (interleaved)
        let input: Vec<f32> = (0..1920)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();

        // Resample
        let output = resampler.resample(&input).expect("Resampling failed");

        // Output should be ~half the length (2:1 downsampling)
        assert!(
            output.len() > 800 && output.len() < 1200,
            "Output length: {}",
            output.len()
        );
        assert_eq!(output.len() % 2, 0);
    }
}
