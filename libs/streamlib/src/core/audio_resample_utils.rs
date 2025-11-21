//! Shared audio resampling utilities
//!
//! This module provides reusable resampling logic used by both AudioResamplerProcessor
//! and AppleAudioOutputProcessor to ensure consistent behavior and reduce code duplication.

use crate::core::{Result, StreamError};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde::{Deserialize, Serialize};

/// Quality presets for audio resampling
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResamplingQuality {
    High,
    Medium,
    Low,
}

impl ResamplingQuality {
    /// Convert quality preset to rubato interpolation parameters
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

/// Multi-channel audio resampler that supports any channel count
///
/// This resampler handles interleaved audio with any number of channels (1-8)
/// and provides adaptive sample rate conversion for use in audio pipelines.
///
/// Internally dispatches to the appropriate channel-specific resampler.
pub struct AudioResampler {
    inner: ResamplerInner,
    source_sample_rate: u32,
    target_sample_rate: u32,
    channels: usize,
    quality: ResamplingQuality,
}

/// Internal resampler variants for different channel counts
enum ResamplerInner {
    Mono(SincFixedIn<f32>),
    Stereo(SincFixedIn<f32>),
    Surround4(SincFixedIn<f32>),   // Quadraphonic
    Surround5_1(SincFixedIn<f32>), // 5.1 surround
    Surround7_1(SincFixedIn<f32>), // 7.1 surround
}

impl AudioResampler {
    /// Create a new multi-channel audio resampler
    ///
    /// # Arguments
    /// * `source_rate` - Input sample rate (Hz)
    /// * `target_rate` - Output sample rate (Hz)
    /// * `channels` - Number of audio channels (1=mono, 2=stereo, 4=quad, 6=5.1, 8=7.1)
    /// * `chunk_size` - Number of samples per channel to process at once
    /// * `quality` - Quality preset for interpolation
    ///
    /// # Returns
    /// A configured resampler ready to process multi-channel audio
    ///
    /// # Supported Channel Counts
    /// - 1: Mono
    /// - 2: Stereo
    /// - 4: Quadraphonic
    /// - 6: 5.1 Surround
    /// - 8: 7.1 Surround
    pub fn new(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        chunk_size: usize,
        quality: ResamplingQuality,
    ) -> Result<Self> {
        // Validate channel count
        if ![1, 2, 4, 6, 8].contains(&channels) {
            return Err(StreamError::Configuration(format!(
                "Unsupported channel count: {} (supported: 1, 2, 4, 6, 8)",
                channels
            )));
        }

        let ratio = target_rate as f64 / source_rate as f64;
        let params = quality.to_parameters();

        // Create channel-specific resampler
        let inner = match channels {
            1 => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 1)
                    .map_err(|e| {
                        StreamError::Runtime(format!("Failed to create mono resampler: {:?}", e))
                    })?;
                ResamplerInner::Mono(resampler)
            }
            2 => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 2)
                    .map_err(|e| {
                        StreamError::Runtime(format!("Failed to create stereo resampler: {:?}", e))
                    })?;
                ResamplerInner::Stereo(resampler)
            }
            4 => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 4)
                    .map_err(|e| {
                        StreamError::Runtime(format!("Failed to create quad resampler: {:?}", e))
                    })?;
                ResamplerInner::Surround4(resampler)
            }
            6 => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 6)
                    .map_err(|e| {
                        StreamError::Runtime(format!("Failed to create 5.1 resampler: {:?}", e))
                    })?;
                ResamplerInner::Surround5_1(resampler)
            }
            8 => {
                let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 8)
                    .map_err(|e| {
                        StreamError::Runtime(format!("Failed to create 7.1 resampler: {:?}", e))
                    })?;
                ResamplerInner::Surround7_1(resampler)
            }
            _ => unreachable!("Validated above"),
        };

        Ok(Self {
            inner,
            source_sample_rate: source_rate,
            target_sample_rate: target_rate,
            channels,
            quality,
        })
    }

    /// Resample interleaved multi-channel audio
    ///
    /// # Arguments
    /// * `input` - Interleaved samples [ch0, ch1, ..., chN, ch0, ch1, ...]
    ///
    /// # Returns
    /// Resampled interleaved samples at target sample rate
    ///
    /// # Performance
    /// This function converts between interleaved and planar formats internally.
    /// For real-time audio, the conversion overhead is negligible compared to
    /// the resampling computation.
    pub fn resample(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        // Convert interleaved to planar (separate channels)
        let samples_per_channel = input.len() / self.channels;
        let mut planar_input: Vec<Vec<f32>> =
            vec![Vec::with_capacity(samples_per_channel); self.channels];

        for chunk in input.chunks_exact(self.channels) {
            for (ch_idx, &sample) in chunk.iter().enumerate() {
                planar_input[ch_idx].push(sample);
            }
        }

        // Resample using the appropriate inner resampler
        let planar_output = match &mut self.inner {
            ResamplerInner::Mono(r) => r.process(&planar_input, None),
            ResamplerInner::Stereo(r) => r.process(&planar_input, None),
            ResamplerInner::Surround4(r) => r.process(&planar_input, None),
            ResamplerInner::Surround5_1(r) => r.process(&planar_input, None),
            ResamplerInner::Surround7_1(r) => r.process(&planar_input, None),
        }
        .map_err(|e| StreamError::Runtime(format!("Resampling failed: {:?}", e)))?;

        // Convert back to interleaved
        let output_samples_per_channel = planar_output[0].len();
        let mut interleaved_output = Vec::with_capacity(output_samples_per_channel * self.channels);

        for i in 0..output_samples_per_channel {
            for channel in planar_output.iter().take(self.channels) {
                interleaved_output.push(channel[i]);
            }
        }

        Ok(interleaved_output)
    }

    /// Get the source sample rate
    pub fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }

    /// Get the target sample rate
    pub fn target_sample_rate(&self) -> u32 {
        self.target_sample_rate
    }

    /// Get the number of channels
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Get the quality setting
    pub fn quality(&self) -> ResamplingQuality {
        self.quality
    }
}

/// Legacy stereo-only resampler (kept for backward compatibility)
///
/// For new code, prefer `AudioResampler` which supports any channel count.
pub struct StereoResampler {
    inner: AudioResampler,
}

impl StereoResampler {
    /// Create a new stereo resampler
    pub fn new(
        source_rate: u32,
        target_rate: u32,
        chunk_size: usize,
        quality: ResamplingQuality,
    ) -> Result<Self> {
        let inner = AudioResampler::new(source_rate, target_rate, 2, chunk_size, quality)?;
        Ok(Self { inner })
    }

    /// Resample interleaved stereo audio
    pub fn resample(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        self.inner.resample(input)
    }

    /// Get the source sample rate
    pub fn source_sample_rate(&self) -> u32 {
        self.inner.source_sample_rate()
    }

    /// Get the target sample rate
    pub fn target_sample_rate(&self) -> u32 {
        self.inner.target_sample_rate()
    }

    /// Get the quality setting
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
        let mut resampler = AudioResampler::new(48000, 24000, 1, 960, ResamplingQuality::Medium)
            .expect("Failed to create mono resampler");

        assert_eq!(resampler.source_sample_rate(), 48000);
        assert_eq!(resampler.target_sample_rate(), 24000);
        assert_eq!(resampler.channels(), 1);

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
        let mut resampler = AudioResampler::new(48000, 24000, 2, 960, ResamplingQuality::Medium)
            .expect("Failed to create stereo resampler");

        assert_eq!(resampler.channels(), 2);

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
        let mut resampler = AudioResampler::new(48000, 24000, 4, 960, ResamplingQuality::Medium)
            .expect("Failed to create quad resampler");

        assert_eq!(resampler.channels(), 4);

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
        let mut resampler = AudioResampler::new(48000, 24000, 6, 960, ResamplingQuality::Medium)
            .expect("Failed to create 5.1 resampler");

        assert_eq!(resampler.channels(), 6);

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
        let mut resampler = AudioResampler::new(48000, 24000, 8, 960, ResamplingQuality::Medium)
            .expect("Failed to create 7.1 resampler");

        assert_eq!(resampler.channels(), 8);

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
        let mut resampler = AudioResampler::new(24000, 48000, 2, 480, ResamplingQuality::Medium)
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
        // Try to create resampler with unsupported channel count
        let result = AudioResampler::new(48000, 24000, 3, 960, ResamplingQuality::Medium);
        assert!(result.is_err());

        let result = AudioResampler::new(48000, 24000, 5, 960, ResamplingQuality::Medium);
        assert!(result.is_err());
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
