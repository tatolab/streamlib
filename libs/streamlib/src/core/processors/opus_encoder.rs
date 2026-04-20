// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Opus Encoder Processor
//
// Encodes AudioFrame to EncodedAudioFrame using libopus.

use crate::_generated_::{Audioframe, Encodedaudioframe};
use crate::core::streaming::{AudioEncoderConfig, AudioEncoderOpus, OpusEncoder};
use crate::core::{Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamError};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.opus_encoder")]
pub struct OpusEncoderProcessor {
    /// Opus encoder.
    opus_encoder: Option<OpusEncoder>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl crate::core::ReactiveProcessor for OpusEncoderProcessor::Processor {
    async fn setup<'a>(&'a mut self, _ctx: &'a RuntimeContextFullAccess<'a>) -> Result<()> {
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

    async fn teardown<'a>(&'a mut self, _ctx: &'a RuntimeContextFullAccess<'a>) -> Result<()> {
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
        let frame: Audioframe = self.inputs.read("audio_in")?;

        let encoder = self
            .opus_encoder
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Opus encoder not initialized".into()))?;

        let encoded: Encodedaudioframe = encoder.encode(&frame)?;
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
