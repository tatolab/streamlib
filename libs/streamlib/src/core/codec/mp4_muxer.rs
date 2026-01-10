// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform-agnostic MP4 muxer wrapper.
//!
//! Delegates to platform-specific implementations:
//! - macOS/iOS: AVAssetWriter with passthrough video
//! - Linux: FFmpeg libavformat

use super::Mp4MuxerConfig;
use crate::core::{EncodedAudioFrame, EncodedVideoFrame, Result, RuntimeContext};

/// Platform-agnostic MP4 muxer.
///
/// Muxes pre-encoded video and audio frames into an MP4 container.
/// This is different from Mp4WriterProcessor which encodes raw frames.
pub struct Mp4Muxer {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) inner: crate::apple::muxer::AppleMp4Muxer,

    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub(crate) inner: crate::linux::ffmpeg::FFmpegMuxer,

    // Fallback for unsupported platforms
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    _marker: std::marker::PhantomData<Mp4MuxerConfig>,
}

// macOS/iOS implementation using AVAssetWriter
#[cfg(any(target_os = "macos", target_os = "ios"))]
impl Mp4Muxer {
    /// Create a new MP4 muxer.
    pub fn new(config: Mp4MuxerConfig, ctx: &RuntimeContext) -> Result<Self> {
        let inner = crate::apple::muxer::AppleMp4Muxer::new(config, ctx)?;
        Ok(Self { inner })
    }

    /// Write an encoded video frame.
    pub fn write_video(&mut self, frame: &EncodedVideoFrame) -> Result<()> {
        self.inner.write_video(frame)
    }

    /// Write an encoded audio frame.
    pub fn write_audio(&mut self, frame: &EncodedAudioFrame) -> Result<()> {
        self.inner.write_audio(frame)
    }

    /// Finalize and close the MP4 file.
    pub fn finalize(&mut self) -> Result<()> {
        self.inner.finalize()
    }
}

// Linux implementation using FFmpeg
#[cfg(all(target_os = "linux", feature = "ffmpeg"))]
impl Mp4Muxer {
    /// Create a new MP4 muxer.
    pub fn new(config: Mp4MuxerConfig, ctx: &RuntimeContext) -> Result<Self> {
        let inner = crate::linux::ffmpeg::FFmpegMuxer::new(config, ctx)?;
        Ok(Self { inner })
    }

    /// Write an encoded video frame.
    pub fn write_video(&mut self, frame: &EncodedVideoFrame) -> Result<()> {
        self.inner.write_video(frame)
    }

    /// Write an encoded audio frame.
    pub fn write_audio(&mut self, frame: &EncodedAudioFrame) -> Result<()> {
        self.inner.write_audio(frame)
    }

    /// Finalize and close the MP4 file.
    pub fn finalize(&mut self) -> Result<()> {
        self.inner.finalize()
    }
}

// Fallback for unsupported platforms
#[cfg(not(any(
    any(target_os = "macos", target_os = "ios"),
    all(target_os = "linux", feature = "ffmpeg")
)))]
impl Mp4Muxer {
    /// Create a new MP4 muxer (unsupported platform).
    pub fn new(_config: Mp4MuxerConfig, _ctx: &RuntimeContext) -> Result<Self> {
        Err(crate::core::StreamError::Configuration(
            "MP4 muxing not supported on this platform".into(),
        ))
    }

    /// Write an encoded video frame (unsupported platform).
    pub fn write_video(&mut self, _frame: &EncodedVideoFrame) -> Result<()> {
        Err(crate::core::StreamError::Configuration(
            "MP4 muxing not supported on this platform".into(),
        ))
    }

    /// Write an encoded audio frame (unsupported platform).
    pub fn write_audio(&mut self, _frame: &EncodedAudioFrame) -> Result<()> {
        Err(crate::core::StreamError::Configuration(
            "MP4 muxing not supported on this platform".into(),
        ))
    }

    /// Finalize (unsupported platform).
    pub fn finalize(&mut self) -> Result<()> {
        Err(crate::core::StreamError::Configuration(
            "MP4 muxing not supported on this platform".into(),
        ))
    }
}

// SAFETY: Platform-specific muxers are Send
#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe impl Send for Mp4Muxer {}

#[cfg(all(target_os = "linux", feature = "ffmpeg"))]
unsafe impl Send for Mp4Muxer {}
