// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFmpeg-based H.264 decoder for Linux.

use crate::core::{codec::VideoDecoderConfig, Result, RuntimeContext, StreamError, VideoFrame};

/// FFmpeg-based hardware decoder.
///
/// Uses FFmpeg's libavcodec for H.264 decoding on Linux.
/// TODO: Implement actual FFmpeg integration.
pub struct FFmpegDecoder {
    config: VideoDecoderConfig,
}

impl FFmpegDecoder {
    /// Create a new FFmpeg decoder.
    pub fn new(config: VideoDecoderConfig, _ctx: &RuntimeContext) -> Result<Self> {
        // TODO: Initialize FFmpeg decoder
        // - Create AVCodecContext for h264 decoder
        // - Configure decoder settings from VideoDecoderConfig
        // - Set up pixel format conversion if needed
        Err(StreamError::Configuration(
            "FFmpeg decoder not yet implemented".into(),
        ))
    }

    /// Update decoder format with SPS/PPS parameter sets.
    pub fn update_format(&mut self, _sps: &[u8], _pps: &[u8]) -> Result<()> {
        // TODO: Parse SPS/PPS to extract dimensions
        // - Update decoder context with extradata
        Err(StreamError::Configuration(
            "FFmpeg decoder not yet implemented".into(),
        ))
    }

    /// Decode H.264 NAL units to a video frame.
    pub fn decode(
        &mut self,
        _nal_units_annex_b: &[u8],
        _timestamp_ns: i64,
    ) -> Result<Option<VideoFrame>> {
        // TODO: Implement decoding
        // - Create AVPacket from NAL units
        // - Send packet to decoder
        // - Receive decoded frame
        // - Convert AVFrame to VideoFrame
        Err(StreamError::Configuration(
            "FFmpeg decoder not yet implemented".into(),
        ))
    }

    /// Get the decoder configuration.
    pub fn config(&self) -> &VideoDecoderConfig {
        &self.config
    }
}

// FFmpegDecoder is Send because FFmpeg contexts can be used from any thread
// (with proper synchronization, which we handle internally)
unsafe impl Send for FFmpegDecoder {}
