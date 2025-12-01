use super::audio_resample::{AudioResampler, ResamplingQuality};
use crate::core::frames::{AudioChannelCount, AudioFrame};
use crate::core::Result;

/// Convert audio frame to a different channel count.
pub fn convert_channels(frame: &AudioFrame, target_channels: AudioChannelCount) -> AudioFrame {
    let source_channels = frame.channels;

    // Fast path: same channel count
    if source_channels == target_channels {
        return frame.clone();
    }

    let source_count = source_channels.as_usize();
    let target_count = target_channels.as_usize();
    let sample_count = frame.sample_count();

    let mut output_samples = Vec::with_capacity(sample_count * target_count);

    match (source_channels, target_channels) {
        // Mono → Stereo: duplicate to both channels
        (AudioChannelCount::One, AudioChannelCount::Two) => {
            for i in 0..sample_count {
                let sample = frame.samples[i];
                output_samples.push(sample);
                output_samples.push(sample);
            }
        }

        // Stereo → Mono: average L and R
        (AudioChannelCount::Two, AudioChannelCount::One) => {
            for i in 0..sample_count {
                let left = frame.samples[i * 2];
                let right = frame.samples[i * 2 + 1];
                output_samples.push((left + right) * 0.5);
            }
        }

        // Mono → Multichannel: duplicate to all channels
        (AudioChannelCount::One, _) => {
            for i in 0..sample_count {
                let sample = frame.samples[i];
                for _ in 0..target_count {
                    output_samples.push(sample);
                }
            }
        }

        // Multichannel → Mono: average all channels
        (_, AudioChannelCount::One) => {
            for i in 0..sample_count {
                let mut sum = 0.0;
                for ch in 0..source_count {
                    sum += frame.samples[i * source_count + ch];
                }
                output_samples.push(sum / source_count as f32);
            }
        }

        // Stereo → Multichannel: L/R to front, zero-fill rest
        (AudioChannelCount::Two, _) => {
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
        (_, AudioChannelCount::Two) => {
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

    AudioFrame::new(
        output_samples,
        target_channels,
        frame.timestamp_ns,
        frame.frame_number,
        frame.sample_rate,
    )
}

/// Resample audio frame to a different sample rate.
pub fn resample_frame(
    frame: &AudioFrame,
    target_sample_rate: u32,
    quality: ResamplingQuality,
) -> Result<AudioFrame> {
    // Fast path: same sample rate
    if frame.sample_rate == target_sample_rate {
        return Ok(frame.clone());
    }

    let chunk_size = frame.sample_count();
    let mut resampler = AudioResampler::new(
        frame.sample_rate,
        target_sample_rate,
        frame.channels,
        chunk_size,
        quality,
    )?;

    let resampled_samples = resampler.resample(&frame.samples)?;

    Ok(AudioFrame::new(
        resampled_samples,
        frame.channels,
        frame.timestamp_ns,
        frame.frame_number,
        target_sample_rate,
    ))
}

/// Convert audio frame to match target configuration (channels + sample rate).
pub fn convert_audio_frame(
    frame: &AudioFrame,
    target_channels: AudioChannelCount,
    target_sample_rate: u32,
    quality: ResamplingQuality,
) -> Result<AudioFrame> {
    // Step 1: Convert channels
    let frame = convert_channels(frame, target_channels);

    // Step 2: Resample
    resample_frame(&frame, target_sample_rate, quality)
}

/// Accumulates samples and outputs frames with exact target sample count.
pub struct AudioRechunker {
    channels: AudioChannelCount,
    target_sample_count: usize,
    buffer: Vec<f32>,
    next_frame_number: u64,
}

impl AudioRechunker {
    /// Create a new audio rechunker.
    pub fn new(channels: AudioChannelCount, target_sample_count: usize) -> Self {
        Self {
            channels,
            target_sample_count,
            buffer: Vec::new(),
            next_frame_number: 0,
        }
    }

    /// Process an input frame, returning an output frame if buffer is full.
    pub fn process(&mut self, input: &AudioFrame) -> Option<AudioFrame> {
        // Validate channel count
        if input.channels != self.channels {
            tracing::warn!(
                "AudioRechunker: channel mismatch (expected {:?}, got {:?})",
                self.channels,
                input.channels
            );
            return None;
        }

        // Accumulate samples
        self.buffer.extend_from_slice(&input.samples);

        let channels_usize = self.channels.as_usize();
        let target_total_samples = self.target_sample_count * channels_usize;

        // Check if we have enough for output
        if self.buffer.len() >= target_total_samples {
            // Extract exact amount needed
            let output_samples = self.buffer.drain(..target_total_samples).collect();

            let frame = AudioFrame::new(
                output_samples,
                self.channels,
                input.timestamp_ns, // Use input timestamp (first sample timestamp)
                self.next_frame_number,
                input.sample_rate,
            );

            self.next_frame_number += 1;
            Some(frame)
        } else {
            None
        }
    }

    /// Flush remaining samples as a potentially shorter frame.
    pub fn flush(&mut self, timestamp_ns: i64, sample_rate: u32) -> Option<AudioFrame> {
        let channels_usize = self.channels.as_usize();

        if self.buffer.is_empty() {
            return None;
        }

        // Ensure buffer is aligned to channel count
        let aligned_len = (self.buffer.len() / channels_usize) * channels_usize;
        if aligned_len == 0 {
            return None;
        }

        let output_samples = self.buffer.drain(..aligned_len).collect();

        let frame = AudioFrame::new(
            output_samples,
            self.channels,
            timestamp_ns,
            self.next_frame_number,
            sample_rate,
        );

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
        let samples = vec![1.0, 2.0, 3.0];
        let frame = AudioFrame::new(samples, AudioChannelCount::One, 0, 0, 48000);

        let stereo = convert_channels(&frame, AudioChannelCount::Two);

        assert_eq!(stereo.channels(), 2);
        assert_eq!(stereo.sample_count(), 3);
        assert_eq!(&*stereo.samples, &[1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn test_convert_stereo_to_mono() {
        let samples = vec![1.0, 2.0, 3.0, 4.0];
        let frame = AudioFrame::new(samples, AudioChannelCount::Two, 0, 0, 48000);

        let mono = convert_channels(&frame, AudioChannelCount::One);

        assert_eq!(mono.channels(), 1);
        assert_eq!(mono.sample_count(), 2);
        assert_eq!(&*mono.samples, &[1.5, 3.5]); // Average of L/R
    }

    #[test]
    fn test_rechunker_exact_fit() {
        let mut rechunker = AudioRechunker::new(AudioChannelCount::Two, 2);

        // Input frame with exactly 2 samples per channel (4 total)
        let frame = AudioFrame::new(
            vec![1.0, 2.0, 3.0, 4.0],
            AudioChannelCount::Two,
            0,
            0,
            48000,
        );

        let output = rechunker.process(&frame).expect("Should output frame");
        assert_eq!(output.sample_count(), 2);
        assert_eq!(&*output.samples, &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_rechunker_accumulation() {
        let mut rechunker = AudioRechunker::new(AudioChannelCount::Two, 4);

        // First frame: 2 samples per channel (not enough)
        let frame1 = AudioFrame::new(
            vec![1.0, 2.0, 3.0, 4.0],
            AudioChannelCount::Two,
            0,
            0,
            48000,
        );
        assert!(rechunker.process(&frame1).is_none());

        // Second frame: 2 more samples per channel (now enough)
        let frame2 = AudioFrame::new(
            vec![5.0, 6.0, 7.0, 8.0],
            AudioChannelCount::Two,
            100,
            1,
            48000,
        );
        let output = rechunker.process(&frame2).expect("Should output frame");

        assert_eq!(output.sample_count(), 4);
        assert_eq!(&*output.samples, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }
}
