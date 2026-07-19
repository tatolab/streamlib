// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Opus audio encoder — libopus codec + reactive processor wrapper.

use serde::{Deserialize, Serialize};
use crate::_generated_::{AudioFrame, EncodedAudioFrame};
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::{Error, Result};

// ============================================================================
// OPUS ENCODING CONFIGURATION
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
            complexity: 5,
            vbr: true,
        }
    }
}

// ============================================================================
// AUDIO ENCODER TRAIT
// ============================================================================

pub trait AudioEncoderOpus: Send {
    fn encode(&mut self, frame: &AudioFrame) -> Result<EncodedAudioFrame>;
    fn config(&self) -> &AudioEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}

// ============================================================================
// OPUS ENCODER IMPLEMENTATION
// ============================================================================

/// Opus audio encoder for real-time WebRTC streaming.
///
/// # Requirements
/// - Input must be stereo (`AudioFrame`)
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
    frame_size: usize, // 960 samples per channel @ 48kHz (20ms)
}

impl OpusEncoder {
    pub fn new(config: AudioEncoderConfig) -> Result<Self> {
        // Validate config
        if config.sample_rate != 48000 {
            return Err(Error::Configuration(format!(
                "Opus encoder only supports 48kHz sample rate, got {}Hz",
                config.sample_rate
            )));
        }
        if config.channels != 2 {
            return Err(Error::Configuration(format!(
                "Opus encoder only supports stereo (2 channels), got {}",
                config.channels
            )));
        }

        // Calculate frame size (20ms @ 48kHz = 960 samples per channel)
        let frame_size = (config.sample_rate as usize * config.frame_duration_ms as usize) / 1000;

        // Create opus encoder
        let mut encoder = opus::Encoder::new(
            config.sample_rate,
            opus::Channels::Stereo,
            opus::Application::Audio, // Use Audio for best quality (music/broadcast)
        )
        .map_err(|e| {
            Error::Configuration(format!("Failed to create Opus encoder: {:?}", e))
        })?;

        // Configure encoder
        encoder
            .set_bitrate(opus::Bitrate::Bits(config.bitrate_bps as i32))
            .map_err(|e| Error::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        encoder
            .set_vbr(config.vbr)
            .map_err(|e| Error::Configuration(format!("Failed to set VBR: {:?}", e)))?;

        // Enable FEC (Forward Error Correction) for better packet loss resilience
        encoder
            .set_inband_fec(true)
            .map_err(|e| Error::Configuration(format!("Failed to set FEC: {:?}", e)))?;

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
    fn encode(&mut self, frame: &AudioFrame) -> Result<EncodedAudioFrame> {
        // Validate sample rate
        if frame.sample_rate != 48000 {
            return Err(Error::Configuration(
                format!(
                    "Expected 48kHz, got {}Hz. Use AudioResamplerProcessor upstream to convert to 48kHz.",
                    frame.sample_rate
                )
            ));
        }

        // Validate frame size (should be exactly 960 samples per channel for 20ms @ 48kHz)
        let expected_samples = self.frame_size; // 960
        let actual_samples = frame.samples.len() / frame.channels as usize;

        if actual_samples != expected_samples {
            return Err(Error::Configuration(
                format!(
                    "Expected {} samples (20ms @ 48kHz), got {}. Use BufferRechunkerProcessor(960) upstream.",
                    expected_samples, actual_samples
                )
            ));
        }

        // Encode (opus expects interleaved f32, which is what AudioFrame uses)
        // Max packet size ~4KB is enough for worst case Opus output
        let encoded_data = self
            .encoder
            .encode_vec_float(&frame.samples, 4000)
            .map_err(|e| Error::Runtime(format!("Opus encoding failed: {:?}", e)))?;

        tracing::trace!(
            "Encoded audio frame: {} samples → {} bytes (compression: {:.2}x)",
            actual_samples * 2, // Total samples (stereo)
            encoded_data.len(),
            (actual_samples * 2 * 4) as f32 / encoded_data.len() as f32 // f32 = 4 bytes per sample
        );

        Ok(EncodedAudioFrame {
            data: encoded_data,
            timestamp_ns: frame.timestamp_ns.clone(),
            sample_count: actual_samples as u32,
        })
    }

    fn config(&self) -> &AudioEncoderConfig {
        &self.config
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.encoder
            .set_bitrate(opus::Bitrate::Bits(bitrate_bps as i32))
            .map_err(|e| Error::Configuration(format!("Failed to set bitrate: {:?}", e)))?;

        self.config.bitrate_bps = bitrate_bps;

        tracing::info!("Opus bitrate changed to {} kbps", bitrate_bps / 1000);
        Ok(())
    }
}

// ============================================================================
// PROCESSOR
// ============================================================================

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/opus/OpusEncoder",
    execution = reactive,
    scheduling = realtime,
    config = crate::_generated_::OpusEncoderConfig,
    input("audio_in", "@tatolab/core/AudioFrame", read_mode = "read_next_in_order", buffer_size = 32),
    output("encoded_audio_out", "@tatolab/core/EncodedAudioFrame"),
)]
pub struct OpusEncoderProcessor {
    /// Opus encoder.
    opus_encoder: Option<OpusEncoder>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for OpusEncoderProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let encoder_config = AudioEncoderConfig {
            sample_rate: 48000,
            channels: 2,
            bitrate_bps: self.config.bitrate_bps.unwrap_or(128_000),
            frame_duration_ms: 20,
            complexity: 5,
            vbr: true,
        };

        let encoder = OpusEncoder::new(encoder_config)?;

        tracing::info!("[OpusEncoder] Initialized");

        self.opus_encoder = Some(encoder);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[OpusEncoder] Shutting down"
        );
        self.opus_encoder.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("audio_in") {
            return Ok(());
        }
        let frame: AudioFrame = self.inputs.read("audio_in")?;

        let encoder = self
            .opus_encoder
            .as_mut()
            .ok_or_else(|| Error::Runtime("Opus encoder not initialized".into()))?;

        let encoded: EncodedAudioFrame = encoder.encode(&frame)?;
        self.outputs.write("encoded_audio_out", &encoded)?;

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("[OpusEncoder] First frame encoded");
        } else if self.frames_encoded % 500 == 0 {
            tracing::info!(frames = self.frames_encoded, "[OpusEncoder] Encode progress");
        }

        Ok(())
    }
}
