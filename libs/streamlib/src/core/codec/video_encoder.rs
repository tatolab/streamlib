// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform-agnostic video encoder wrapper.

use crate::core::{EncodedVideoFrame, GpuContext, Result, RuntimeContext, VideoFrame};

use super::VideoEncoderConfig;

/// Platform-agnostic video encoder.
///
/// Uses VideoToolbox on macOS/iOS, FFmpeg on Linux.
pub struct VideoEncoder {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) inner: crate::apple::videotoolbox::VideoToolboxEncoder,

    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub(crate) inner: crate::linux::ffmpeg::FFmpegEncoder,

    // Fallback for platforms without encoder support
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    _marker: std::marker::PhantomData<()>,

    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    config: VideoEncoderConfig,
}

impl VideoEncoder {
    /// Create a new video encoder.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn new(
        config: VideoEncoderConfig,
        gpu_context: Option<GpuContext>,
        ctx: &RuntimeContext,
    ) -> Result<Self> {
        let inner = crate::apple::videotoolbox::VideoToolboxEncoder::new(config, gpu_context, ctx)?;
        Ok(Self { inner })
    }

    /// Create a new video encoder.
    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub fn new(
        config: VideoEncoderConfig,
        gpu_context: Option<GpuContext>,
        ctx: &RuntimeContext,
    ) -> Result<Self> {
        let inner = crate::linux::ffmpeg::FFmpegEncoder::new(config, gpu_context, ctx)?;
        Ok(Self { inner })
    }

    /// Create a new video encoder (unsupported platform).
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    pub fn new(
        config: VideoEncoderConfig,
        _gpu_context: Option<GpuContext>,
        _ctx: &RuntimeContext,
    ) -> Result<Self> {
        Ok(Self {
            _marker: std::marker::PhantomData,
            config,
        })
    }

    /// Encode a video frame.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedVideoFrame> {
        self.inner.encode(frame)
    }

    /// Encode a video frame.
    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedVideoFrame> {
        self.inner.encode(frame)
    }

    /// Encode a video frame (unsupported platform).
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    pub fn encode(&mut self, _frame: &VideoFrame) -> Result<EncodedVideoFrame> {
        Err(StreamError::Configuration(
            "Video encoding not supported on this platform".into(),
        ))
    }

    /// Set the target bitrate.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.inner.set_bitrate(bitrate_bps)
    }

    /// Set the target bitrate.
    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.inner.set_bitrate(bitrate_bps)
    }

    /// Set the target bitrate (unsupported platform).
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    pub fn set_bitrate(&mut self, _bitrate_bps: u32) -> Result<()> {
        Err(StreamError::Configuration(
            "Video encoding not supported on this platform".into(),
        ))
    }

    /// Force the next frame to be a keyframe.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn force_keyframe(&mut self) {
        self.inner.force_keyframe();
    }

    /// Force the next frame to be a keyframe.
    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub fn force_keyframe(&mut self) {
        self.inner.force_keyframe();
    }

    /// Force the next frame to be a keyframe (unsupported platform).
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    pub fn force_keyframe(&mut self) {
        // No-op on unsupported platforms
    }

    /// Get the encoder configuration.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn config(&self) -> &VideoEncoderConfig {
        self.inner.config()
    }

    /// Get the encoder configuration.
    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub fn config(&self) -> &VideoEncoderConfig {
        self.inner.config()
    }

    /// Get the encoder configuration (unsupported platform).
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    pub fn config(&self) -> &VideoEncoderConfig {
        &self.config
    }
}

// VideoEncoder is Send because the inner encoders are Send
#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe impl Send for VideoEncoder {}

#[cfg(all(target_os = "linux", feature = "ffmpeg"))]
unsafe impl Send for VideoEncoder {}
