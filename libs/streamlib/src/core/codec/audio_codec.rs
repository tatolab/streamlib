// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Audio codec types.

use serde::{Deserialize, Serialize};

/// Audio codec type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    /// AAC codec (common for MP4).
    #[default]
    Aac,
    /// Opus codec (common for WebRTC).
    Opus,
}

impl AudioCodec {
    /// MIME type for this codec.
    pub fn mime_type(&self) -> &'static str {
        match self {
            AudioCodec::Aac => "audio/aac",
            AudioCodec::Opus => "audio/opus",
        }
    }

    /// RTP payload type (96-127 for dynamic types).
    pub fn rtp_payload_type(&self) -> u8 {
        match self {
            AudioCodec::Aac => 97,
            AudioCodec::Opus => 111,
        }
    }
}
