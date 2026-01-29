// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFmpeg-based H.264 encoder for Linux.

use crate::_generated_::Encodedvideoframe;
use crate::core::{
    GpuContext, Result, RuntimeContext, StreamError, VideoEncoderConfig, VideoFrame,
};

/// FFmpeg-based hardware encoder.
///
/// Uses FFmpeg's libavcodec for H.264 encoding on Linux.
/// TODO: Implement actual FFmpeg integration.
pub struct FFmpegEncoder {
    config: VideoEncoderConfig,
}

impl FFmpegEncoder {
    /// Create a new FFmpeg encoder.
    pub fn new(
        config: VideoEncoderConfig,
        _gpu_context: Option<GpuContext>,
        _ctx: &RuntimeContext,
    ) -> Result<Self> {
        // TODO: Initialize FFmpeg encoder
        // - Create AVCodecContext for libx264 or h264_nvenc
        // - Configure encoder settings from VideoEncoderConfig
        // - Set up pixel format conversion if needed
        Err(StreamError::Configuration(
            "FFmpeg encoder not yet implemented".into(),
        ))
    }

    /// Encode a video frame.
    pub fn encode(&mut self, _frame: &VideoFrame) -> Result<Encodedvideoframe> {
        // TODO: Implement encoding
        // - Convert VideoFrame to AVFrame
        // - Send frame to encoder
        // - Receive encoded packet
        // - Convert to Encodedvideoframe
        Err(StreamError::Configuration(
            "FFmpeg encoder not yet implemented".into(),
        ))
    }

    /// Set the target bitrate.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.config.bitrate_bps = bitrate_bps;
        // TODO: Update FFmpeg encoder bitrate dynamically
        Ok(())
    }

    /// Force the next frame to be a keyframe.
    pub fn force_keyframe(&mut self) {
        // TODO: Set flag to force IDR frame on next encode
    }

    /// Get the encoder configuration.
    pub fn config(&self) -> &VideoEncoderConfig {
        &self.config
    }
}

// FFmpegEncoder is Send because FFmpeg contexts can be used from any thread
// (with proper synchronization, which we handle internally)
unsafe impl Send for FFmpegEncoder {}
