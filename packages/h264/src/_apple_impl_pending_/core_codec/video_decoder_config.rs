// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform-agnostic video decoder configuration.

use super::VideoCodec;
use serde::{Deserialize, Serialize};

/// Configuration for video decoding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoDecoderConfig {
    /// Expected video width (may be updated from SPS).
    pub width: u32,
    /// Expected video height (may be updated from SPS).
    pub height: u32,
    /// Video codec to decode.
    pub codec: VideoCodec,
}

impl Default for VideoDecoderConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            codec: VideoCodec::default(),
        }
    }
}

impl VideoDecoderConfig {
    /// Create a new decoder config with the specified dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            ..Default::default()
        }
    }

    /// Create a decoder config with a specific codec.
    pub fn with_codec(mut self, codec: VideoCodec) -> Self {
        self.codec = codec;
        self
    }
}
