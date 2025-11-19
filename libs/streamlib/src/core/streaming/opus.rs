// Opus Audio Encoding
//
// Provides Opus encoding for real-time audio streaming.

use crate::core::{AudioFrame, StreamError, Result};
use serde::{Deserialize, Serialize};

// ============================================================================
// ENCODED AUDIO FRAME
// ============================================================================

/// Internal representation of encoded Opus frame
#[derive(Clone, Debug)]
pub struct EncodedAudioFrame {
    pub data: Vec<u8>,
    pub timestamp_ns: i64,
    pub sample_count: usize,
}

// ============================================================================
// OPUS ENCODING CONFIGURATION
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioEncoderConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate_bps: u32,
    pub frame_duration_ms: u32,
    pub complexity: u32,
    pub vbr: bool,
}

impl Default for AudioEncoderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            bitrate_bps: 128_000,
            frame_duration_ms: 20,
            complexity: 10,
            vbr: true,
        }
    }
}

// ============================================================================
// AUDIO ENCODER TRAIT
// ============================================================================

pub trait AudioEncoderOpus: Send {
    fn encode(&mut self, frame: &AudioFrame<2>) -> Result<EncodedAudioFrame>;
    fn config(&self) -> &AudioEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}

// ============================================================================
// OPUS ENCODER IMPLEMENTATION
// ============================================================================

/// Opus audio encoder for real-time WebRTC streaming.
///
/// # Requirements
/// - Input must be stereo (`AudioFrame<2>`)
/// - Sample rate must be 48kHz
/// - Frame size must be exactly 960 samples (20ms @ 48kHz)
///
/// # Pipeline Setup
/// Typical pipeline for Opus encoding requires preprocessing:
/// - AudioSource → AudioResamplerProcessor(48kHz) → BufferRechunkerProcessor(960) → OpusEncoder
///
/// # Configuration
/// - **Bitrate**: 128 kbps default (adjust with `set_bitrate()`)
/// - **VBR**: Enabled by default for better quality
/// - **FEC**: Forward error correction enabled for packet loss resilience
#[derive(Debug)]
pub struct OpusEncoder {
    config: AudioEncoderConfig,
    encoder: opus::Encoder,
    frame_size: usize,  // 960 samples per channel @ 48kHz (20ms)
}

impl OpusEncoder {
    pub fn new(config: AudioEncoderConfig) -> Result<Self> {
        // Validate config
        if config.sample_rate != 48000 {
            return Err(StreamError::Configuration(
                format!("Opus encoder only supports 48kHz sample rate, got {}Hz", config.sample_rate)
            ));
        }
        if config.channels != 2 {
            return Err(StreamError::Configuration(
                format!("Opus encoder only supports stereo (2 channels), got {}", config.channels)
            ));
        }

        // Calculate frame size (20ms @ 48kHz = 960 samples per channel)
        let frame_size = (config.sample_rate as usize * config.frame_duration_ms as usize) / 1000;

        // Create opus encoder
        let mut encoder = opus::Encoder::new(
            config.sample_rate,
            opus::Channels::Stereo,
            opus::Application::Audio,  // Use Audio for best quality (music/broadcast)
        ).map_err(|e| StreamError::Configuration(format!("Failed to create Opus encoder: {:?}", e)))?;

        // Configure encoder
        encoder.set_bitrate(opus::Bitrate::Bits(config.bitrate_bps as i32))
            .map_err(|e| StreamError::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        encoder.set_vbr(config.vbr)
            .map_err(|e| StreamError::Configuration(format!("Failed to set VBR: {:?}", e)))?;

        // Enable FEC (Forward Error Correction) for better packet loss resilience
        encoder.set_inband_fec(true)
            .map_err(|e| StreamError::Configuration(format!("Failed to set FEC: {:?}", e)))?;

        tracing::info!(
            "OpusEncoder initialized: {}Hz, {} channels, {} kbps, {}ms frames, VBR={}",
            config.sample_rate,
            config.channels,
            config.bitrate_bps / 1000,
            config.frame_duration_ms,
            config.vbr
        );

        Ok(Self {
            config,
            encoder,
            frame_size,
        })
    }
}

impl AudioEncoderOpus for OpusEncoder {
    fn encode(&mut self, frame: &AudioFrame<2>) -> Result<EncodedAudioFrame> {
        // Validate sample rate
        if frame.sample_rate != 48000 {
            return Err(StreamError::Configuration(
                format!(
                    "Expected 48kHz, got {}Hz. Use AudioResamplerProcessor upstream to convert to 48kHz.",
                    frame.sample_rate
                )
            ));
        }

        // Validate frame size (should be exactly 960 samples per channel for 20ms @ 48kHz)
        let expected_samples = self.frame_size;  // 960
        let actual_samples = frame.sample_count();

        if actual_samples != expected_samples {
            return Err(StreamError::Configuration(
                format!(
                    "Expected {} samples (20ms @ 48kHz), got {}. Use BufferRechunkerProcessor(960) upstream.",
                    expected_samples, actual_samples
                )
            ));
        }

        // Encode (opus expects interleaved f32, which is what AudioFrame uses)
        // Max packet size ~4KB is enough for worst case Opus output
        let encoded_data = self.encoder
            .encode_vec_float(&frame.samples, 4000)
            .map_err(|e| StreamError::Runtime(format!("Opus encoding failed: {:?}", e)))?;

        tracing::trace!(
            "Encoded audio frame: {} samples → {} bytes (compression: {:.2}x)",
            actual_samples * 2,  // Total samples (stereo)
            encoded_data.len(),
            (actual_samples * 2 * 4) as f32 / encoded_data.len() as f32  // f32 = 4 bytes per sample
        );

        Ok(EncodedAudioFrame {
            data: encoded_data,
            timestamp_ns: frame.timestamp_ns,  // Preserve timestamp exactly
            sample_count: actual_samples,
        })
    }

    fn config(&self) -> &AudioEncoderConfig {
        &self.config
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.encoder
            .set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))
            .map_err(|e| StreamError::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        self.config.bitrate_bps = bitrate_bps;

        tracing::info!("Opus bitrate changed to {} kbps", bitrate_bps / 1000);
        Ok(())
    }
}
