// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFmpeg MP4 muxer using libavformat.

use crate::core::codec::Mp4MuxerConfig;
use crate::core::{EncodedAudioFrame, EncodedVideoFrame, Result, RuntimeContext, StreamError};

/// FFmpeg MP4 muxer using libavformat.
///
/// Muxes pre-encoded H.264 video and AAC/Opus audio into MP4 container.
pub struct FFmpegMuxer {
    config: Mp4MuxerConfig,
    // TODO: FFmpeg fields
    // format_context: *mut AVFormatContext,
    // video_stream: *mut AVStream,
    // audio_stream: Option<*mut AVStream>,
}

impl FFmpegMuxer {
    /// Create a new FFmpeg MP4 muxer.
    pub fn new(config: Mp4MuxerConfig, _ctx: &RuntimeContext) -> Result<Self> {
        // TODO: Initialize libavformat muxer
        // - Create AVFormatContext with output URL
        // - Create video stream with codec parameters
        // - Create audio stream (if audio configured)
        // - Write header
        Err(StreamError::Configuration(
            "FFmpeg MP4 muxer not yet implemented".into(),
        ))
    }

    /// Write an encoded video frame.
    pub fn write_video(&mut self, _frame: &EncodedVideoFrame) -> Result<()> {
        // TODO: Write encoded video packet
        // - Create AVPacket from frame.data
        // - Set stream index, pts, dts
        // - Write packet to container
        Err(StreamError::Configuration(
            "FFmpeg MP4 muxer not yet implemented".into(),
        ))
    }

    /// Write an encoded audio frame.
    pub fn write_audio(&mut self, _frame: &EncodedAudioFrame) -> Result<()> {
        // TODO: Write encoded audio packet
        // - Create AVPacket from frame.data
        // - Set stream index, pts, dts
        // - Write packet to container
        Err(StreamError::Configuration(
            "FFmpeg MP4 muxer not yet implemented".into(),
        ))
    }

    /// Finalize and close the MP4 file.
    pub fn finalize(&mut self) -> Result<()> {
        // TODO: Finish writing
        // - Write trailer
        // - Close output file
        // - Free format context
        Err(StreamError::Configuration(
            "FFmpeg MP4 muxer not yet implemented".into(),
        ))
    }

    /// Get the muxer configuration.
    pub fn config(&self) -> &Mp4MuxerConfig {
        &self.config
    }
}

// FFmpegMuxer is Send because FFmpeg context is used from a single thread
unsafe impl Send for FFmpegMuxer {}
