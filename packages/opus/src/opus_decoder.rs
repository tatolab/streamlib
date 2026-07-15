// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Opus audio decoder — libopus codec + reactive processor wrapper.

use crate::_generated_::{AudioFrame, EncodedAudioFrame};
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::{Error, Result};

// ============================================================================
// OPUS DECODER IMPLEMENTATION
// ============================================================================

/// Opus audio decoder for real-time WebRTC streaming.
#[derive(Debug)]
pub struct OpusDecoder {
    decoder: opus::Decoder,
    sample_rate: u32,
    input_channels: usize,
    frame_size: usize,
}

impl OpusDecoder {
    /// Create a new Opus decoder.
    pub fn new(sample_rate: u32, input_channels: usize) -> Result<Self> {
        // Opus supports: 8000, 12000, 16000, 24000, 48000 Hz
        // WebRTC typically uses 48000 Hz
        if ![8000, 12000, 16000, 24000, 48000].contains(&sample_rate) {
            return Err(Error::Configuration(format!(
                "Opus decoder requires sample rate of 8/12/16/24/48 kHz, got {}Hz",
                sample_rate
            )));
        }

        if input_channels != 1 && input_channels != 2 {
            return Err(Error::Configuration(format!(
                "Opus decoder supports 1 (mono) or 2 (stereo) channels, got {}",
                input_channels
            )));
        }

        // Create opus decoder with the stream's channel count
        let channels = match input_channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => unreachable!(),
        };

        let decoder = opus::Decoder::new(sample_rate, channels)
            .map_err(|e| Error::Runtime(format!("Failed to create Opus decoder: {}", e)))?;

        // Calculate frame size based on sample rate
        // WebRTC typically uses 20ms frames: (sample_rate * 20) / 1000
        let frame_size = (sample_rate * 20 / 1000) as usize;

        tracing::info!(
            "[Opus Decoder] Created decoder: {}Hz, {} input channels → stereo output, {} samples/frame",
            sample_rate,
            input_channels,
            frame_size
        );

        Ok(Self {
            decoder,
            sample_rate,
            input_channels,
            frame_size,
        })
    }

    /// Decode Opus packet to raw PCM samples.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>> {
        // Track decode calls for debugging
        static DECODE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let decode_num = DECODE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Allocate output buffer
        // If input is mono, we'll decode to mono then convert to stereo
        let mut output = vec![0.0f32; self.frame_size * self.input_channels];

        if decode_num == 0 {
            tracing::info!(
                "[Opus Decoder] FIRST DECODE: packet_size={} bytes, expected_frame_size={} samples, input_channels={}, output_buffer_size={} floats",
                packet.len(),
                self.frame_size,
                self.input_channels,
                output.len()
            );
        }

        // Decode to PCM
        let decoded_samples = self.decoder
            .decode_float(packet, &mut output, false)
            .map_err(|e| {
                tracing::error!(
                    "[Opus Decoder] Decode failed (packet #{}): {} (packet_size={} bytes, input_channels={})",
                    decode_num,
                    e,
                    packet.len(),
                    self.input_channels
                );
                Error::Runtime(format!("Opus decode failed: {}", e))
            })?;

        if decode_num == 0 {
            tracing::info!(
                "[Opus Decoder] FIRST DECODE RESULT: decoded_samples={} (per channel), total_output_samples={}",
                decoded_samples,
                decoded_samples * self.input_channels
            );
        } else if decode_num.is_multiple_of(100) {
            tracing::debug!(
                "[Opus Decoder] Decode #{}: {} samples per channel, {} total samples",
                decode_num,
                decoded_samples,
                decoded_samples * self.input_channels
            );
        }

        // Trim to actual decoded length
        output.truncate(decoded_samples * self.input_channels);

        // Convert mono to stereo if needed
        if self.input_channels == 1 {
            if decode_num == 0 {
                tracing::info!(
                    "[Opus Decoder] Converting MONO to STEREO: {} mono samples → {} stereo samples",
                    output.len(),
                    output.len() * 2
                );
            }
            // Duplicate mono to both channels
            let stereo = output.iter().flat_map(|&sample| [sample, sample]).collect();
            Ok(stereo)
        } else {
            if decode_num == 0 {
                tracing::info!(
                    "[Opus Decoder] Already STEREO: {} samples (interleaved L,R,L,R...)",
                    output.len()
                );
            }
            // Already stereo
            Ok(output)
        }
    }

    /// Decode Opus packet directly to [`AudioFrame`].
    pub fn decode_to_audio_frame(
        &mut self,
        packet: &[u8],
        timestamp_ns: i64,
    ) -> Result<AudioFrame> {
        let samples = self.decode(packet)?;

        // Samples are already interleaved stereo [L,R,L,R,...]
        // AudioFrame expects Vec<f32> with interleaved samples
        Ok(AudioFrame {
            samples,
            channels: 2, // stereo
            timestamp_ns: timestamp_ns.to_string(),
            frame_index: "0".to_string(), // frame_number (will be set by caller if needed)
            sample_rate: self.sample_rate,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn input_channels(&self) -> usize {
        self.input_channels
    }

    pub fn frame_size(&self) -> usize {
        self.frame_size
    }
}

// ============================================================================
// PROCESSOR
// ============================================================================

#[streamlib_plugin_sdk::sdk::processor("OpusDecoder")]
pub struct OpusDecoderProcessor {
    /// Opus decoder.
    opus_decoder: Option<OpusDecoder>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for OpusDecoderProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let sample_rate = self.config.sample_rate.unwrap_or(48000);
        let channels = self.config.channels.unwrap_or(2) as usize;

        let decoder = OpusDecoder::new(sample_rate, channels)?;

        tracing::info!(
            sample_rate,
            channels,
            "[OpusDecoder] Initialized"
        );

        self.opus_decoder = Some(decoder);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[OpusDecoder] Shutting down"
        );
        self.opus_decoder.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_audio_in") {
            return Ok(());
        }
        let encoded: EncodedAudioFrame = self.inputs.read("encoded_audio_in")?;

        let decoder = self
            .opus_decoder
            .as_mut()
            .ok_or_else(|| Error::Runtime("Opus decoder not initialized".into()))?;

        let timestamp_ns: i64 = encoded.timestamp_ns.parse().unwrap_or(0);
        let frame: AudioFrame = decoder.decode_to_audio_frame(&encoded.data, timestamp_ns)?;
        self.outputs.write("audio_out", &frame)?;

        self.frames_decoded += 1;
        if self.frames_decoded == 1 {
            tracing::info!("[OpusDecoder] First frame decoded");
        } else if self.frames_decoded % 500 == 0 {
            tracing::info!(frames = self.frames_decoded, "[OpusDecoder] Decode progress");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoder_creation() {
        // Valid configuration
        let decoder = OpusDecoder::new(48000, 2);
        assert!(decoder.is_ok());

        // Invalid sample rate
        let decoder = OpusDecoder::new(44100, 2);
        assert!(decoder.is_err());

        // Invalid channel count
        let decoder = OpusDecoder::new(48000, 3);
        assert!(decoder.is_err());
    }

    #[test]
    fn test_mono_decoder() {
        let decoder = OpusDecoder::new(48000, 1);
        assert!(decoder.is_ok());

        let decoder = decoder.unwrap();
        assert_eq!(decoder.input_channels(), 1);
        assert_eq!(decoder.sample_rate(), 48000);
    }

    #[test]
    fn test_stereo_decoder() {
        let decoder = OpusDecoder::new(48000, 2);
        assert!(decoder.is_ok());

        let decoder = decoder.unwrap();
        assert_eq!(decoder.input_channels(), 2);
        assert_eq!(decoder.sample_rate(), 48000);
    }
}
