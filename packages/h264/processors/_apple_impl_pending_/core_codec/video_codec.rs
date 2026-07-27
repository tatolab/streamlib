// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Video codec types and profiles.

use serde::{Deserialize, Serialize};

/// FourCC code for H.264/AVC ('avc1').
pub const FOURCC_H264: u32 = 0x61766331; // 'avc1' in ASCII

/// Video codec type with profile configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    /// H.264/AVC codec.
    H264(H264Profile),
    // Future codecs:
    // H265(H265Profile),
    // AV1,
    // VP9,
}

/// H.264 encoding profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264Profile {
    /// Baseline profile - most compatible, lowest features.
    Baseline,
    /// Main profile - good balance of compatibility and features.
    #[default]
    Main,
    /// High profile - advanced features, requires newer decoders.
    High,
}

impl Default for VideoCodec {
    fn default() -> Self {
        VideoCodec::H264(H264Profile::Main)
    }
}

impl VideoCodec {
    /// FourCC code for this codec.
    pub fn fourcc(&self) -> u32 {
        match self {
            VideoCodec::H264(_) => FOURCC_H264,
        }
    }

    /// MIME type for this codec.
    pub fn mime_type(&self) -> &'static str {
        match self {
            VideoCodec::H264(_) => "video/h264",
        }
    }

    /// RTP payload type (96-127 for dynamic types).
    pub fn rtp_payload_type(&self) -> u8 {
        match self {
            VideoCodec::H264(_) => 96,
        }
    }

    /// SDP format-specific parameters (fmtp line).
    pub fn sdp_fmtp_params(&self) -> String {
        match self {
            VideoCodec::H264(profile) => {
                let profile_idc = match profile {
                    H264Profile::Baseline => "42", // 66 (0x42)
                    H264Profile::Main => "4d",     // 77 (0x4d)
                    H264Profile::High => "64",     // 100 (0x64)
                };

                let constraint_level = match profile {
                    H264Profile::Baseline => "e01f", // constraint_set1_flag=1, Level 3.1
                    H264Profile::Main => "001f",     // no constraints, Level 3.1
                    H264Profile::High => "001f",     // no constraints, Level 3.1
                };

                format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}{}",
                    profile_idc, constraint_level
                )
            }
        }
    }
}

impl H264Profile {
    /// Get the profile_idc value for H.264.
    pub fn profile_idc(&self) -> u8 {
        match self {
            H264Profile::Baseline => 66,
            H264Profile::Main => 77,
            H264Profile::High => 100,
        }
    }
}
