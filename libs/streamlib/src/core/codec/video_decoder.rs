// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform-agnostic video decoder wrapper.
//!
//! Delegates to platform-specific implementations:
//! - macOS/iOS: VideoToolbox (hardware accelerated)
//! - Linux: FFmpeg (software or VAAPI/NVDEC)

use super::VideoDecoderConfig;
use crate::_generated_::Videoframe;
use crate::core::{GpuContext, Result, RuntimeContext};

/// Platform-agnostic video decoder.
///
/// Wraps platform-specific decoders behind a unified API.
/// Use [`VideoDecoder::new`] to create an instance, then call
/// [`VideoDecoder::update_format`] with SPS/PPS before decoding.
pub struct VideoDecoder {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) inner: crate::apple::videotoolbox::VideoToolboxDecoder,

    #[cfg(all(target_os = "linux", feature = "ffmpeg"))]
    pub(crate) inner: crate::linux::ffmpeg::FFmpegDecoder,

    // Fallback for unsupported platforms
    #[cfg(not(any(
        any(target_os = "macos", target_os = "ios"),
        all(target_os = "linux", feature = "ffmpeg")
    )))]
    _marker: std::marker::PhantomData<VideoDecoderConfig>,
}

// macOS/iOS implementation using VideoToolbox
#[cfg(any(target_os = "macos", target_os = "ios"))]
impl VideoDecoder {
    /// Create a new video decoder.
    pub fn new(config: VideoDecoderConfig, ctx: &RuntimeContext) -> Result<Self> {
        // VideoToolboxDecoder uses core VideoDecoderConfig
        let inner = crate::apple::videotoolbox::VideoToolboxDecoder::new(config, ctx)?;
        Ok(Self { inner })
    }

    /// Update decoder format with SPS/PPS parameter sets.
    ///
    /// Must be called before [`VideoDecoder::decode`] when receiving H.264 stream.
    pub fn update_format(&mut self, sps: &[u8], pps: &[u8]) -> Result<()> {
        self.inner.update_format(sps, pps)
    }

    /// Decode H.264 NAL units to a video frame.
    ///
    /// Returns `Ok(Some(frame))` when a frame is decoded, `Ok(None)` when buffering.
    pub fn decode(
        &mut self,
        nal_units_annex_b: &[u8],
        timestamp_ns: i64,
        gpu: &GpuContext,
    ) -> Result<Option<Videoframe>> {
        self.inner.decode(nal_units_annex_b, timestamp_ns, gpu)
    }
}

// Linux implementation using FFmpeg
#[cfg(all(target_os = "linux", feature = "ffmpeg"))]
impl VideoDecoder {
    /// Create a new video decoder.
    pub fn new(config: VideoDecoderConfig, ctx: &RuntimeContext) -> Result<Self> {
        let inner = crate::linux::ffmpeg::FFmpegDecoder::new(config, ctx)?;
        Ok(Self { inner })
    }

    /// Update decoder format with SPS/PPS parameter sets.
    pub fn update_format(&mut self, sps: &[u8], pps: &[u8]) -> Result<()> {
        self.inner.update_format(sps, pps)
    }

    /// Decode H.264 NAL units to a video frame.
    pub fn decode(
        &mut self,
        nal_units_annex_b: &[u8],
        timestamp_ns: i64,
        gpu: &GpuContext,
    ) -> Result<Option<Videoframe>> {
        self.inner.decode(nal_units_annex_b, timestamp_ns, gpu)
    }
}

// Fallback for unsupported platforms
#[cfg(not(any(
    any(target_os = "macos", target_os = "ios"),
    all(target_os = "linux", feature = "ffmpeg")
)))]
impl VideoDecoder {
    /// Create a new video decoder (unsupported platform).
    pub fn new(_config: VideoDecoderConfig, _ctx: &RuntimeContext) -> Result<Self> {
        Err(crate::core::StreamError::Configuration(
            "Video decoding not supported on this platform".into(),
        ))
    }

    /// Update decoder format (unsupported platform).
    pub fn update_format(&mut self, _sps: &[u8], _pps: &[u8]) -> Result<()> {
        Err(crate::core::StreamError::Configuration(
            "Video decoding not supported on this platform".into(),
        ))
    }

    /// Decode NAL units (unsupported platform).
    pub fn decode(
        &mut self,
        _nal_units_annex_b: &[u8],
        _timestamp_ns: i64,
        _gpu: &GpuContext,
    ) -> Result<Option<Videoframe>> {
        Err(crate::core::StreamError::Configuration(
            "Video decoding not supported on this platform".into(),
        ))
    }
}

// SAFETY: Platform-specific decoders are Send
#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe impl Send for VideoDecoder {}

#[cfg(all(target_os = "linux", feature = "ffmpeg"))]
unsafe impl Send for VideoDecoder {}
