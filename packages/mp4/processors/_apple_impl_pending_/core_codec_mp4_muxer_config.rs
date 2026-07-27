// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MP4 muxer configuration.

use super::{AudioCodec, VideoCodec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for MP4 muxer.
///
/// The muxer takes pre-encoded video and audio frames and writes them to an MP4 file.
/// Unlike Mp4WriterProcessor which encodes raw frames, this muxer only performs muxing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mp4MuxerConfig {
    /// Output file path.
    pub output_path: PathBuf,
    /// Video codec (must match the encoded frames).
    pub video_codec: VideoCodec,
    /// Video width in pixels.
    pub video_width: u32,
    /// Video height in pixels.
    pub video_height: u32,
    /// Video frame rate.
    pub video_fps: u32,
    /// Optional audio codec (None for video-only).
    pub audio_codec: Option<AudioCodec>,
    /// Audio sample rate (required if audio_codec is Some).
    pub audio_sample_rate: Option<u32>,
    /// Audio channel count (required if audio_codec is Some).
    pub audio_channels: Option<u16>,
}

impl Default for Mp4MuxerConfig {
    fn default() -> Self {
        Self {
            output_path: PathBuf::from("/tmp/output.mp4"),
            video_codec: VideoCodec::default(),
            video_width: 1920,
            video_height: 1080,
            video_fps: 30,
            audio_codec: None,
            audio_sample_rate: None,
            audio_channels: None,
        }
    }
}

impl Mp4MuxerConfig {
    /// Create a video-only muxer config.
    pub fn video_only(output_path: PathBuf, width: u32, height: u32, fps: u32) -> Self {
        Self {
            output_path,
            video_width: width,
            video_height: height,
            video_fps: fps,
            ..Default::default()
        }
    }

    /// Add audio configuration.
    pub fn with_audio(mut self, codec: AudioCodec, sample_rate: u32, channels: u16) -> Self {
        self.audio_codec = Some(codec);
        self.audio_sample_rate = Some(sample_rate);
        self.audio_channels = Some(channels);
        self
    }
}
