// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderStateAV1.h
//!
//! AV1 encoder state tracking.
//! Contains the VideoSessionParametersInfoAV1 helper for creating
//! VkVideoSessionParametersKHR, and the EncoderAV1State struct that
//! holds the sequence header, timing info, decoder model info,
//! operating points, and AV1-specific rate control parameters.

// ---------------------------------------------------------------------------
// VideoSessionParametersInfoAV1
// ---------------------------------------------------------------------------

/// Helper for building AV1 video session parameters create info.
///
/// In C++ this class chains several Vulkan structures together.
/// In Rust we store the relevant fields and provide a builder pattern.
#[derive(Debug, Clone, Default)]
pub struct VideoSessionParametersInfoAV1 {
    pub quality_level: u32,
    pub enable_qp_map: bool,
    pub quantization_map_texel_size_width: u32,
    pub quantization_map_texel_size_height: u32,
}

impl VideoSessionParametersInfoAV1 {
    pub fn new(
        quality_level: u32,
        enable_qp_map: bool,
        texel_size_w: u32,
        texel_size_h: u32,
    ) -> Self {
        Self {
            quality_level,
            enable_qp_map,
            quantization_map_texel_size_width: texel_size_w,
            quantization_map_texel_size_height: texel_size_h,
        }
    }
}

// ---------------------------------------------------------------------------
// AV1 Sequence Header (simplified)
// ---------------------------------------------------------------------------

/// Simplified AV1 sequence header flags.
#[derive(Debug, Clone, Default)]
pub struct Av1SequenceHeaderFlags {
    pub frame_id_numbers_present_flag: bool,
    pub enable_order_hint: bool,
}

/// Simplified AV1 color config.
#[derive(Debug, Clone, Default)]
pub struct Av1ColorConfig {
    pub mono_chrome: bool,
    pub separate_uv_delta_q: bool,
}

/// Simplified AV1 timing info.
#[derive(Debug, Clone, Default)]
pub struct Av1TimingInfo {
    pub equal_picture_interval: bool,
}

/// Simplified AV1 decoder model info.
#[derive(Debug, Clone, Default)]
pub struct Av1DecoderModelInfo {
    pub frame_presentation_time_length_minus_1: u32,
}

/// Simplified AV1 operating point info.
#[derive(Debug, Clone, Default)]
pub struct Av1OperatingPointInfo {
    pub operating_point_idc: u16,
    pub seq_level_idx: u8,
    pub seq_tier: u8,
}

// ---------------------------------------------------------------------------
// EncoderAV1State
// ---------------------------------------------------------------------------

/// AV1 encoder state — holds sequence-level parameters and rate control info.
#[derive(Debug, Clone, Default)]
pub struct EncoderAV1State {
    // Sequence header fields
    pub delta_frame_id_length_minus_2: u32,
    pub additional_frame_id_length_minus_1: u32,
    pub order_hint_bits_minus_1: u32,
    pub seq_flags: Av1SequenceHeaderFlags,
    pub color_config: Option<Av1ColorConfig>,
    pub timing_info: Option<Av1TimingInfo>,

    // Decoder model
    pub decoder_model_info: Av1DecoderModelInfo,

    // Operating points
    pub operating_points_count: u32,
    pub operating_points_info: Vec<Av1OperatingPointInfo>,

    // Flags
    pub timing_info_present_flag: bool,
    pub decoder_model_info_present_flag: bool,
}

impl EncoderAV1State {
    pub fn new() -> Self {
        Self::default()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_params_info() {
        let info = VideoSessionParametersInfoAV1::new(2, true, 16, 16);
        assert_eq!(info.quality_level, 2);
        assert!(info.enable_qp_map);
    }

    #[test]
    fn test_encoder_av1_state_default() {
        let state = EncoderAV1State::new();
        assert_eq!(state.operating_points_count, 0);
        assert!(!state.timing_info_present_flag);
        assert!(!state.decoder_model_info_present_flag);
    }

    #[test]
    fn test_sequence_header_flags() {
        let flags = Av1SequenceHeaderFlags {
            frame_id_numbers_present_flag: true,
            enable_order_hint: true,
        };
        assert!(flags.frame_id_numbers_present_flag);
        assert!(flags.enable_order_hint);
    }

    #[test]
    fn test_color_config() {
        let config = Av1ColorConfig { mono_chrome: true, separate_uv_delta_q: false };
        assert!(config.mono_chrome);
    }
}
