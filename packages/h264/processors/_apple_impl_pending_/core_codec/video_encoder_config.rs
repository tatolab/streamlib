// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Video encoder configuration.

use serde::{Deserialize, Serialize};

use super::VideoCodec;

/// Video encoder configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoEncoderConfig {
    /// Video width in pixels.
    pub width: u32,
    /// Video height in pixels.
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
    /// Target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Keyframe interval in frames.
    pub keyframe_interval_frames: u32,
    /// Video codec to use.
    pub codec: VideoCodec,
    /// Enable low-latency mode for real-time streaming.
    pub low_latency: bool,
}

impl Default for VideoEncoderConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_bps: 2_500_000,
            keyframe_interval_frames: 60,
            codec: VideoCodec::default(),
            low_latency: true,
        }
    }
}

impl VideoEncoderConfig {
    /// Create a new config with specified dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            ..Default::default()
        }
    }

    /// Set the frames per second.
    pub fn with_fps(mut self, fps: u32) -> Self {
        self.fps = fps;
        self
    }

    /// Set the target bitrate in bits per second.
    pub fn with_bitrate(mut self, bitrate_bps: u32) -> Self {
        self.bitrate_bps = bitrate_bps;
        self
    }

    /// Set the keyframe interval in frames.
    pub fn with_keyframe_interval(mut self, frames: u32) -> Self {
        self.keyframe_interval_frames = frames;
        self
    }

    /// Set the video codec.
    pub fn with_codec(mut self, codec: VideoCodec) -> Self {
        self.codec = codec;
        self
    }

    /// Enable or disable low-latency mode.
    pub fn with_low_latency(mut self, enabled: bool) -> Self {
        self.low_latency = enabled;
        self
    }
}
