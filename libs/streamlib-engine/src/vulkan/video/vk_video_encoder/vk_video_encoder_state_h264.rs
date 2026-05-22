// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderStateH264.h
//!
//! H.264 encoder state tracking: `VideoSessionParametersInfo` for creating
//! Vulkan video session parameters (SPS/PPS), and `EncoderH264State` for
//! holding the current SPS, PPS, VUI, HRD, and rate control state.
//!
//! Divergence from C++: The Vulkan struct chains (sType/pNext) are represented
//! as Rust structs with builder-style initialization. The actual Vulkan types
//! (`VkVideoEncodeH264SessionParametersAddInfoKHR`, etc.) will be wired up
//! when the `ash` video encode extensions are stabilized.

use vulkanalia::vk;
#[allow(unused_imports)] // Needed for VideoSessionKHR::null()
use vulkanalia::vk::Handle;

// ---------------------------------------------------------------------------
// VideoSessionParametersInfo
// ---------------------------------------------------------------------------

/// Configuration for creating H.264 video session parameters.
///
/// Holds the SPS, PPS, quality level, and optional QP map configuration
/// needed to create `VkVideoSessionParametersKHR`.
///
/// Equivalent to the C++ `VideoSessionParametersInfo` class.
#[derive(Debug, Clone)]
pub struct VideoSessionParametersInfoH264 {
    pub video_session: vk::VideoSessionKHR,
    pub sps_count: u32,
    pub pps_count: u32,
    pub max_sps_count: u32,
    pub max_pps_count: u32,
    pub quality_level: u32,
    pub enable_qp_map: bool,
    pub qp_map_texel_size: vk::Extent2D,
}

impl VideoSessionParametersInfoH264 {
    /// Create a new session parameters info.
    ///
    /// Equivalent to the C++ `VideoSessionParametersInfo` constructor.
    pub fn new(
        video_session: vk::VideoSessionKHR,
        quality_level: u32,
        enable_qp_map: bool,
        qp_map_texel_size: vk::Extent2D,
    ) -> Self {
        Self {
            video_session,
            sps_count: 1,
            pps_count: 1,
            max_sps_count: 1,
            max_pps_count: 1,
            quality_level,
            enable_qp_map,
            qp_map_texel_size,
        }
    }
}

// ---------------------------------------------------------------------------
// EncoderH264State
// ---------------------------------------------------------------------------

/// H.264 encoder state: SPS, PPS, VUI, HRD parameters, and rate control info.
///
/// Equivalent to the C++ `EncoderH264State` struct.
///
/// The H.264 standard types (`StdVideoH264SequenceParameterSet`, etc.) are
/// represented as placeholder structs until the `ash` video standard types
/// are available. The fields mirror the C++ member variables exactly.
#[derive(Debug, Clone, Default)]
pub struct EncoderH264State {
    /// Sequence Parameter Set info.
    pub sps_info: H264Sps,
    /// Picture Parameter Set info.
    pub pps_info: H264Pps,
    /// VUI (Video Usability Information) parameters.
    pub vui_info: H264Vui,
    /// HRD (Hypothetical Reference Decoder) parameters.
    pub hrd_parameters: H264HrdParameters,
}

/// Placeholder for `StdVideoH264SequenceParameterSet`.
///
/// Fields will be expanded when the full SPS is needed for bitstream writing.
#[derive(Debug, Clone, Default)]
pub struct H264Sps {
    pub profile_idc: u32,
    pub level_idc: u32,
    pub seq_parameter_set_id: u8,
    pub chroma_format_idc: u32,
    pub pic_width_in_mbs_minus1: u32,
    pub pic_height_in_map_units_minus1: u32,
    pub max_num_ref_frames: u8,
    pub pic_order_cnt_type: u32,
    pub log2_max_frame_num_minus4: u8,
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    pub frame_crop_right_offset: u32,
    pub frame_crop_bottom_offset: u32,
    // Flags
    pub frame_mbs_only_flag: bool,
    pub frame_cropping_flag: bool,
    pub direct_8x8_inference_flag: bool,
    pub vui_parameters_present_flag: bool,
    pub qpprime_y_zero_transform_bypass_flag: bool,
    pub constraint_set0_flag: bool,
    pub constraint_set1_flag: bool,
    pub constraint_set4_flag: bool,
    pub constraint_set5_flag: bool,
}

/// Placeholder for `StdVideoH264PictureParameterSet`.
#[derive(Debug, Clone, Default)]
pub struct H264Pps {
    pub seq_parameter_set_id: u8,
    pub pic_parameter_set_id: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub weighted_bipred_idc: u32,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    // Flags
    pub transform_8x8_mode_flag: bool,
    pub entropy_coding_mode_flag: bool,
    pub deblocking_filter_control_present_flag: bool,
    pub constrained_intra_pred_flag: bool,
}

/// Placeholder for `StdVideoH264SequenceParameterSetVui`.
#[derive(Debug, Clone, Default)]
pub struct H264Vui {
    pub aspect_ratio_idc: u32,
    pub sar_width: u16,
    pub sar_height: u16,
    pub video_format: u8,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub time_scale: u32,
    pub num_units_in_tick: u32,
    pub max_num_reorder_frames: u8,
    // Flags
    pub aspect_ratio_info_present_flag: bool,
    pub overscan_info_present_flag: bool,
    pub overscan_appropriate_flag: bool,
    pub video_signal_type_present_flag: bool,
    pub video_full_range_flag: bool,
    pub color_description_present_flag: bool,
    pub chroma_loc_info_present_flag: bool,
    pub timing_info_present_flag: bool,
    pub fixed_frame_rate_flag: bool,
    pub nal_hrd_parameters_present_flag: bool,
    pub vcl_hrd_parameters_present_flag: bool,
    pub bitstream_restriction_flag: bool,
}

/// Placeholder for `StdVideoH264HrdParameters`.
#[derive(Debug, Clone, Default)]
pub struct H264HrdParameters {
    pub cpb_cnt_minus1: u32,
    pub bit_rate_scale: u8,
    pub cpb_size_scale: u8,
    pub bit_rate_value_minus1: [u32; 32],
    pub cpb_size_value_minus1: [u32; 32],
    pub cbr_flag: [bool; 32],
    pub initial_cpb_removal_delay_length_minus1: u8,
    pub cpb_removal_delay_length_minus1: u8,
    pub dpb_output_delay_length_minus1: u8,
    pub time_offset_length: u8,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_state_default() {
        let state = EncoderH264State::default();
        assert_eq!(state.sps_info.profile_idc, 0);
        assert_eq!(state.pps_info.pic_parameter_set_id, 0);
        assert!(!state.vui_info.timing_info_present_flag);
    }

    #[test]
    fn test_session_params_info() {
        let info = VideoSessionParametersInfoH264::new(
            vk::VideoSessionKHR::null(),
            0,
            false,
            vk::Extent2D { width: 0, height: 0 },
        );
        assert_eq!(info.sps_count, 1);
        assert_eq!(info.pps_count, 1);
        assert_eq!(info.quality_level, 0);
        assert!(!info.enable_qp_map);
    }

    #[test]
    fn test_session_params_with_qp_map() {
        let info = VideoSessionParametersInfoH264::new(
            vk::VideoSessionKHR::null(),
            2,
            true,
            vk::Extent2D {
                width: 16,
                height: 16,
            },
        );
        assert!(info.enable_qp_map);
        assert_eq!(info.qp_map_texel_size.width, 16);
        assert_eq!(info.quality_level, 2);
    }
}
