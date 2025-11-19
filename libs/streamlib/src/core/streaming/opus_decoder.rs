// Opus Audio Decoding
//
// Provides Opus decoding for real-time audio streaming from WebRTC.

use crate::core::{AudioFrame, StreamError, Result};

// ============================================================================
// OPUS DECODER IMPLEMENTATION
// ============================================================================

/// Opus audio decoder for real-time WebRTC streaming.
///
/// # Features
/// - Decodes Opus packets to PCM audio
/// - Supports mono and stereo input
/// - Always outputs stereo `AudioFrame<2>` (mono input is duplicated to both channels)
/// - Sample rate: 48kHz (WebRTC standard)
///
/// # Usage
/// ```ignore
/// let decoder = OpusDecoder::new(48000, 2)?; // 48kHz, stereo
/// let audio_frame = decoder.decode_to_audio_frame(opus_packet, timestamp_ns)?;
/// ```
#[derive(Debug)]
pub struct OpusDecoder {
    decoder: opus::Decoder,
    sample_rate: u32,
    input_channels: usize,  // Channels in the input stream (1 or 2)
    frame_size: usize,      // Expected frame size in samples per channel
}

impl OpusDecoder {
    /// Create a new Opus decoder
    ///
    /// # Arguments
    /// * `sample_rate` - Sample rate in Hz (must be 48000 for WebRTC)
    /// * `input_channels` - Number of channels in the Opus stream (1=mono, 2=stereo)
    ///
    /// # Returns
    /// Configured Opus decoder that outputs stereo frames
    pub fn new(sample_rate: u32, input_channels: usize) -> Result<Self> {
        if sample_rate != 48000 {
            return Err(StreamError::Configuration(
                format!("Opus decoder requires 48kHz sample rate, got {}Hz", sample_rate)
            ));
        }

        if input_channels != 1 && input_channels != 2 {
            return Err(StreamError::Configuration(
                format!("Opus decoder supports 1 (mono) or 2 (stereo) channels, got {}", input_channels)
            ));
        }

        // Create opus decoder with the stream's channel count
        let channels = match input_channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => unreachable!(),
        };

        let decoder = opus::Decoder::new(sample_rate, channels)
            .map_err(|e| StreamError::Runtime(format!("Failed to create Opus decoder: {}", e)))?;

        // WebRTC typically uses 20ms frames (960 samples @ 48kHz)
        let frame_size = 960;

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

    /// Decode Opus packet to raw PCM samples
    ///
    /// # Arguments
    /// * `packet` - Compressed Opus packet data
    ///
    /// # Returns
    /// Vec of f32 samples (interleaved stereo: [L, R, L, R, ...])
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>> {
        // Allocate output buffer
        // If input is mono, we'll decode to mono then convert to stereo
        let mut output = vec![0.0f32; self.frame_size * self.input_channels];

        // Decode to PCM
        let decoded_samples = self.decoder
            .decode_float(packet, &mut output, false)
            .map_err(|e| StreamError::Runtime(format!("Opus decode failed: {}", e)))?;

        // Trim to actual decoded length
        output.truncate(decoded_samples * self.input_channels);

        // Convert mono to stereo if needed
        if self.input_channels == 1 {
            // Duplicate mono to both channels
            let stereo = output
                .iter()
                .flat_map(|&sample| [sample, sample])
                .collect();
            Ok(stereo)
        } else {
            // Already stereo
            Ok(output)
        }
    }

    /// Decode Opus packet directly to `AudioFrame<2>`
    ///
    /// # Arguments
    /// * `packet` - Compressed Opus packet data
    /// * `timestamp_ns` - Presentation timestamp in nanoseconds (from MediaClock)
    ///
    /// # Returns
    /// Stereo audio frame ready to be sent to audio output
    pub fn decode_to_audio_frame(&mut self, packet: &[u8], timestamp_ns: i64) -> Result<AudioFrame<2>> {
        let samples = self.decode(packet)?;

        // Samples are already interleaved stereo [L,R,L,R,...]
        // AudioFrame expects Arc<Vec<f32>> with interleaved samples
        Ok(AudioFrame::new(
            samples,
            timestamp_ns,
            0, // frame_number (will be set by caller if needed)
            self.sample_rate,
        ))
    }

    /// Get the configured sample rate
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get the input channel count (from stream)
    pub fn input_channels(&self) -> usize {
        self.input_channels
    }

    /// Get the expected frame size in samples per channel
    pub fn frame_size(&self) -> usize {
        self.frame_size
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

        let mut decoder = decoder.unwrap();
        assert_eq!(decoder.input_channels(), 1);
        assert_eq!(decoder.sample_rate(), 48000);
    }

    #[test]
    fn test_stereo_decoder() {
        let decoder = OpusDecoder::new(48000, 2);
        assert!(decoder.is_ok());

        let mut decoder = decoder.unwrap();
        assert_eq!(decoder.input_channels(), 2);
        assert_eq!(decoder.sample_rate(), 48000);
    }
}
