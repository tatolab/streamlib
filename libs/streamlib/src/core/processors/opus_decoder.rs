// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Opus Decoder Processor
//
// Decodes EncodedAudioFrame (Opus bitstream) to AudioFrame using libopus.

use crate::_generated_::{Audioframe, Encodedaudioframe};
use crate::core::streaming::OpusDecoder;
use crate::core::{Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamError};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.opus_decoder")]
pub struct OpusDecoderProcessor {
    /// Opus decoder.
    opus_decoder: Option<OpusDecoder>,

    /// Frames decoded counter.
    frames_decoded: u64,
}

impl crate::core::ReactiveProcessor for OpusDecoderProcessor::Processor {
    async fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
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

    async fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
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
        let encoded: Encodedaudioframe = self.inputs.read("encoded_audio_in")?;

        let decoder = self
            .opus_decoder
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Opus decoder not initialized".into()))?;

        let timestamp_ns: i64 = encoded.timestamp_ns.parse().unwrap_or(0);
        let frame: Audioframe = decoder.decode_to_audio_frame(&encoded.data, timestamp_ns)?;
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
