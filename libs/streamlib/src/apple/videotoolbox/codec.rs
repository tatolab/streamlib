// Video Codec Abstraction
//
// Defines video codec types and profiles for VideoToolbox encoding.
// Currently only H.264 is implemented, but the structure is designed
// to support future codecs (H.265/HEVC, AV1, VP9).

use super::ffi;
use serde::{Deserialize, Serialize};

/// Video codec type with profile configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    /// H.264/AVC codec (fully implemented)
    H264(H264Profile),
    // Future codecs (not yet implemented):
    // H265(H265Profile),
    // AV1,
    // VP9,
}

/// H.264 encoding profile
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264Profile {
    /// Baseline profile - most compatible, lowest features
    Baseline,
    /// Main profile - good balance of compatibility and features
    Main,
    /// High profile - advanced features, requires newer decoders
    High,
}

/// Codec information trait for encoding parameters
#[allow(dead_code)] // Methods kept for future WebRTC/RTP implementation
pub trait CodecInfo {
    /// FourCC code for the codec (e.g., 'avc1' for H.264)
    fn fourcc(&self) -> u32;

    /// MIME type for the codec (e.g., "video/h264")
    fn mime_type(&self) -> &'static str;

    /// RTP payload type (96-127 for dynamic types)
    fn rtp_payload_type(&self) -> u8;

    /// SDP format-specific parameters (fmtp line)
    fn sdp_fmtp_params(&self) -> String;
}

impl CodecInfo for VideoCodec {
    fn fourcc(&self) -> u32 {
        match self {
            VideoCodec::H264(_) => ffi::K_CMVIDEO_CODEC_TYPE_H264,
        }
    }

    fn mime_type(&self) -> &'static str {
        match self {
            VideoCodec::H264(_) => "video/h264",
        }
    }

    fn rtp_payload_type(&self) -> u8 {
        match self {
            // H.264 uses dynamic payload type 96-127
            VideoCodec::H264(_) => 96,
        }
    }

    fn sdp_fmtp_params(&self) -> String {
        match self {
            VideoCodec::H264(profile) => {
                // Generate profile-level-id from H.264 profile
                let profile_idc = match profile {
                    H264Profile::Baseline => "42", // Baseline = 66 (0x42)
                    H264Profile::Main => "4d",     // Main = 77 (0x4d)
                    H264Profile::High => "64",     // High = 100 (0x64)
                };

                // constraint_set_flags and level_idc
                // For WebRTC compatibility:
                // - Baseline: 42e01f (Level 3.1)
                // - Main: 4d001f (Level 3.1)
                // - High: 64001f (Level 3.1)
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

impl Default for VideoCodec {
    fn default() -> Self {
        // Main profile is the best balance for WebRTC
        VideoCodec::H264(H264Profile::Main)
    }
}
