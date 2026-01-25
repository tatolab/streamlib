// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::audio_resample::{AudioResampler, ResamplingQuality};
use crate::_generated_::Audioframe;
use crate::core::Result;

/// Convert audio frame to a different channel count.
pub fn convert_channels(frame: &Audioframe, target_channels: u8) -> Audioframe {
    let source_channels = frame.channels;

    // Fast path: same channel count
    if source_channels == target_channels {
        return frame.clone();
    }

    let source_count = source_channels as usize;
    let target_count = target_channels as usize;
    let sample_count = frame.samples.len() / source_count;

    let mut output_samples = Vec::with_capacity(sample_count * target_count);

    match (source_channels, target_channels) {
        // Mono → Stereo: duplicate to both channels
        (1, 2) => {
            for i in 0..sample_count {
                let sample = frame.samples[i];
                output_samples.push(sample);
                output_samples.push(sample);
            }
        }

        // Stereo → Mono: average L and R
        (2, 1) => {
            for i in 0..sample_count {
                let left = frame.samples[i * 2];
                let right = frame.samples[i * 2 + 1];
                output_samples.push((left + right) * 0.5);
            }
        }

        // Mono → Multichannel: duplicate to all channels
        (1, _) => {
            for i in 0..sample_count {
                let sample = frame.samples[i];
                for _ in 0..target_count {
                    output_samples.push(sample);
                }
            }
        }

        // Multichannel → Mono: average all channels
        (_, 1) => {
            for i in 0..sample_count {
                let mut sum = 0.0;
                for ch in 0..source_count {
                    sum += frame.samples[i * source_count + ch];
                }
                output_samples.push(sum / source_count as f32);
            }
        }

        // Stereo → Multichannel: L/R to front, zero-fill rest
        (2, _) => {
            for i in 0..sample_count {
                let left = frame.samples[i * 2];
                let right = frame.samples[i * 2 + 1];

                output_samples.push(left); // Front left
                if target_count > 1 {
                    output_samples.push(right); // Front right
                }
                // Zero-fill remaining channels
                output_samples.extend(std::iter::repeat_n(0.0, target_count - 2));
            }
        }

        // Multichannel → Stereo: take front L/R
        (_, 2) => {
            for i in 0..sample_count {
                let left = frame.samples[i * source_count]; // Channel 0
                let right = if source_count > 1 {
                    frame.samples[i * source_count + 1] // Channel 1
                } else {
                    left // Duplicate if source is mono
                };
                output_samples.push(left);
                output_samples.push(right);
            }
        }

        // General multichannel conversion: copy what fits, zero-fill or truncate
        _ => {
            for i in 0..sample_count {
                for target_ch in 0..target_count {
                    let sample = if target_ch < source_count {
                        frame.samples[i * source_count + target_ch]
                    } else {
                        0.0 // Zero-fill if target has more channels
                    };
                    output_samples.push(sample);
                }
            }
        }
    }

    Audioframe {
        samples: output_samples,
        channels: target_channels,
        timestamp_ns: frame.timestamp_ns.clone(),
        frame_index: frame.frame_index.clone(),
        sample_rate: frame.sample_rate,
    }
}

/// Resample audio frame to a different sample rate.
pub fn resample_frame(
    frame: &Audioframe,
    target_sample_rate: u32,
    quality: ResamplingQuality,
) -> Result<Audioframe> {
    // Fast path: same sample rate
    if frame.sample_rate == target_sample_rate {
        return Ok(frame.clone());
    }

    let chunk_size = frame.samples.len() / frame.channels as usize;
    let mut resampler = AudioResampler::new(
        frame.sample_rate,
        target_sample_rate,
        frame.channels,
        chunk_size,
        quality,
    )?;

    let resampled_samples = resampler.resample(&frame.samples)?;

    Ok(Audioframe {
        samples: resampled_samples,
        channels: frame.channels,
        timestamp_ns: frame.timestamp_ns.clone(),
        frame_index: frame.frame_index.clone(),
        sample_rate: target_sample_rate,
    })
}

/// Convert audio frame to match target configuration (channels + sample rate).
pub fn convert_audio_frame(
    frame: &Audioframe,
    target_channels: u8,
    target_sample_rate: u32,
    quality: ResamplingQuality,
) -> Result<Audioframe> {
    // Step 1: Convert channels
    let frame = convert_channels(frame, target_channels);

    // Step 2: Resample
    resample_frame(&frame, target_sample_rate, quality)
}

/// Accumulates samples and outputs frames with exact target sample count.
pub struct AudioRechunker {
    channels: u8,
    target_sample_count: usize,
    buffer: Vec<f32>,
    next_frame_number: u64,
}

impl AudioRechunker {
    /// Create a new audio rechunker.
    pub fn new(channels: u8, target_sample_count: usize) -> Self {
        Self {
            channels,
            target_sample_count,
            buffer: Vec::new(),
            next_frame_number: 0,
        }
    }

    /// Process an input frame, returning an output frame if buffer is full.
    pub fn process(&mut self, input: &Audioframe) -> Option<Audioframe> {
        // Validate channel count
        if input.channels != self.channels {
            tracing::warn!(
                "AudioRechunker: channel mismatch (expected {}, got {})",
                self.channels,
                input.channels
            );
            return None;
        }

        // Accumulate samples
        self.buffer.extend_from_slice(&input.samples);

        let channels_usize = self.channels as usize;
        let target_total_samples = self.target_sample_count * channels_usize;

        // Check if we have enough for output
        if self.buffer.len() >= target_total_samples {
            // Extract exact amount needed
            let output_samples: Vec<f32> = self.buffer.drain(..target_total_samples).collect();

            let frame = Audioframe {
                samples: output_samples,
                channels: self.channels,
                timestamp_ns: input.timestamp_ns.clone(),
                frame_index: self.next_frame_number.to_string(),
                sample_rate: input.sample_rate,
            };

            self.next_frame_number += 1;
            Some(frame)
        } else {
            None
        }
    }

    /// Flush remaining samples as a potentially shorter frame.
    pub fn flush(&mut self, timestamp_ns: i64, sample_rate: u32) -> Option<Audioframe> {
        let channels_usize = self.channels as usize;

        if self.buffer.is_empty() {
            return None;
        }

        // Ensure buffer is aligned to channel count
        let aligned_len = (self.buffer.len() / channels_usize) * channels_usize;
        if aligned_len == 0 {
            return None;
        }

        let output_samples: Vec<f32> = self.buffer.drain(..aligned_len).collect();

        let frame = Audioframe {
            samples: output_samples,
            channels: self.channels,
            timestamp_ns: timestamp_ns.to_string(),
            frame_index: self.next_frame_number.to_string(),
            sample_rate,
        };

        self.next_frame_number += 1;
        Some(frame)
    }

    /// Clear internal buffer.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.next_frame_number = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_mono_to_stereo() {
        let frame = Audioframe {
            samples: vec![1.0, 2.0, 3.0],
            channels: 1,
            timestamp_ns: "0".to_string(),
            frame_index: "0".to_string(),
            sample_rate: 48000,
        };

        let stereo = convert_channels(&frame, 2);

        assert_eq!(stereo.channels, 2);
        assert_eq!(stereo.samples.len() / stereo.channels as usize, 3);
        assert_eq!(&stereo.samples, &[1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn test_convert_stereo_to_mono() {
        let frame = Audioframe {
            samples: vec![1.0, 2.0, 3.0, 4.0],
            channels: 2,
            timestamp_ns: "0".to_string(),
            frame_index: "0".to_string(),
            sample_rate: 48000,
        };

        let mono = convert_channels(&frame, 1);

        assert_eq!(mono.channels, 1);
        assert_eq!(mono.samples.len(), 2);
        assert_eq!(&mono.samples, &[1.5, 3.5]); // Average of L/R
    }

    #[test]
    fn test_rechunker_exact_fit() {
        let mut rechunker = AudioRechunker::new(2, 2);

        // Input frame with exactly 2 samples per channel (4 total)
        let frame = Audioframe {
            samples: vec![1.0, 2.0, 3.0, 4.0],
            channels: 2,
            timestamp_ns: "0".to_string(),
            frame_index: "0".to_string(),
            sample_rate: 48000,
        };

        let output = rechunker.process(&frame).expect("Should output frame");
        assert_eq!(output.samples.len() / output.channels as usize, 2);
        assert_eq!(&output.samples, &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_rechunker_accumulation() {
        let mut rechunker = AudioRechunker::new(2, 4);

        // First frame: 2 samples per channel (not enough)
        let frame1 = Audioframe {
            samples: vec![1.0, 2.0, 3.0, 4.0],
            channels: 2,
            timestamp_ns: "0".to_string(),
            frame_index: "0".to_string(),
            sample_rate: 48000,
        };
        assert!(rechunker.process(&frame1).is_none());

        // Second frame: 2 more samples per channel (now enough)
        let frame2 = Audioframe {
            samples: vec![5.0, 6.0, 7.0, 8.0],
            channels: 2,
            timestamp_ns: "100".to_string(),
            frame_index: "1".to_string(),
            sample_rate: 48000,
        };
        let output = rechunker.process(&frame2).expect("Should output frame");

        assert_eq!(output.samples.len() / output.channels as usize, 4);
        assert_eq!(&output.samples, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }
}
