// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderStateH265.h
//!
//! H.265 encoder state tracking: VPS, SPS (with all sub-structures),
//! and rate control state.
//!
//! Divergence from C++: The H.265 standard types are represented as Rust
//! structs mirroring the C++ `StdVideoH265*` types. The actual ash/Vulkan
//! bindings will replace these when stabilized.

// ---------------------------------------------------------------------------
// VpsH265
// ---------------------------------------------------------------------------

/// H.265 Video Parameter Set.
///
/// Equivalent to the C++ `VpsH265` struct.
#[derive(Debug, Clone, Default)]
pub struct VpsH265 {
    pub vps_info: H265Vps,
}

/// Placeholder for `StdVideoH265VideoParameterSet`.
#[derive(Debug, Clone, Default)]
pub struct H265Vps {
    pub vps_video_parameter_set_id: u8,
    pub vps_max_sub_layers_minus1: u8,
    pub vps_num_units_in_tick: u32,
    pub vps_time_scale: u32,
    pub vps_num_ticks_poc_diff_one_minus1: u32,
    // Flags
    pub vps_temporal_id_nesting_flag: bool,
    pub vps_sub_layer_ordering_info_present_flag: bool,
    pub vps_timing_info_present_flag: bool,
    pub vps_poc_proportional_to_timing_flag: bool,
}

// ---------------------------------------------------------------------------
// SpsH265
// ---------------------------------------------------------------------------

/// H.265 Sequence Parameter Set with all sub-structures.
///
/// Equivalent to the C++ `SpsH265` struct. The constructor wires up the
/// internal pointers (pProfileTierLevel, pDecPicBufMgr, etc.) as field
/// references -- in Rust these are simply owned fields.
#[derive(Debug, Clone, Default)]
pub struct SpsH265 {
    pub sps: H265Sps,
    pub dec_pic_buf_mgr: H265DecPicBufMgr,
    pub hrd_parameters: H265HrdParameters,
    pub profile_tier_level: H265ProfileTierLevel,
    pub short_term_ref_pic_set: H265ShortTermRefPicSet,
    pub long_term_ref_pics_sps: H265LongTermRefPicsSps,
    pub vui_info: H265Vui,
    pub sub_layer_hrd_parameters_nal: H265SubLayerHrdParameters,
}

/// Placeholder for `StdVideoH265SequenceParameterSet`.
#[derive(Debug, Clone, Default)]
pub struct H265Sps {
    pub chroma_format_idc: u32,
    pub pic_width_in_luma_samples: u32,
    pub pic_height_in_luma_samples: u32,
    pub sps_video_parameter_set_id: u8,
    pub sps_max_sub_layers_minus1: u8,
    pub sps_seq_parameter_set_id: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    pub log2_min_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_luma_coding_block_size: u8,
    pub log2_min_luma_transform_block_size_minus2: u8,
    pub log2_diff_max_min_luma_transform_block_size: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub pcm_sample_bit_depth_luma_minus1: u8,
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    pub num_short_term_ref_pic_sets: u32,
    pub num_long_term_ref_pics_sps: u32,
    pub conf_win_left_offset: u32,
    pub conf_win_right_offset: u32,
    pub conf_win_top_offset: u32,
    pub conf_win_bottom_offset: u32,
    // Flags (mirrors StdVideoH265SpsFlags)
    pub sps_temporal_id_nesting_flag: bool,
    pub separate_colour_plane_flag: bool,
    pub conformance_window_flag: bool,
    pub sps_sub_layer_ordering_info_present_flag: bool,
    pub scaling_list_enabled_flag: bool,
    pub sps_scaling_list_data_present_flag: bool,
    pub amp_enabled_flag: bool,
    pub sample_adaptive_offset_enabled_flag: bool,
    pub pcm_enabled_flag: bool,
    pub pcm_loop_filter_disabled_flag: bool,
    pub long_term_ref_pics_present_flag: bool,
    pub sps_temporal_mvp_enabled_flag: bool,
    pub strong_intra_smoothing_enabled_flag: bool,
    pub vui_parameters_present_flag: bool,
    pub sps_extension_present_flag: bool,
    pub sps_range_extension_flag: bool,
    pub transform_skip_rotation_enabled_flag: bool,
    pub transform_skip_context_enabled_flag: bool,
    pub implicit_rdpcm_enabled_flag: bool,
    pub explicit_rdpcm_enabled_flag: bool,
    pub extended_precision_processing_flag: bool,
    pub intra_smoothing_disabled_flag: bool,
    pub high_precision_offsets_enabled_flag: bool,
    pub persistent_rice_adaptation_enabled_flag: bool,
    pub cabac_bypass_alignment_enabled_flag: bool,
    pub sps_scc_extension_flag: bool,
    pub sps_curr_pic_ref_enabled_flag: bool,
    pub palette_mode_enabled_flag: bool,
    pub sps_palette_predictor_initializers_present_flag: bool,
    pub intra_boundary_filtering_disabled_flag: bool,
}

/// Placeholder for `StdVideoH265DecPicBufMgr`.
#[derive(Debug, Clone)]
pub struct H265DecPicBufMgr {
    pub max_latency_increase_plus1: [u32; 7],
    pub max_dec_pic_buffering_minus1: [u8; 7],
    pub max_num_reorder_pics: [u8; 7],
}

impl Default for H265DecPicBufMgr {
    fn default() -> Self {
        Self {
            max_latency_increase_plus1: [0; 7],
            max_dec_pic_buffering_minus1: [0; 7],
            max_num_reorder_pics: [0; 7],
        }
    }
}

/// Placeholder for `StdVideoH265ProfileTierLevel`.
#[derive(Debug, Clone, Default)]
pub struct H265ProfileTierLevel {
    pub general_profile_idc: u32,
    pub general_level_idc: u32,
    // Flags
    pub general_tier_flag: bool,
    pub general_progressive_source_flag: bool,
    pub general_interlaced_source_flag: bool,
    pub general_non_packed_constraint_flag: bool,
    pub general_frame_only_constraint_flag: bool,
}

/// Placeholder for `StdVideoH265ShortTermRefPicSet`.
#[derive(Debug, Clone, Default)]
pub struct H265ShortTermRefPicSet {
    pub num_negative_pics: u8,
    pub num_positive_pics: u8,
    pub used_by_curr_pic_s0_flag: u16,
    pub used_by_curr_pic_s1_flag: u16,
    pub used_by_curr_pic_flag: u16,
    pub delta_idx_minus1: u32,
    pub use_delta_flag: u16,
    pub abs_delta_rps_minus1: u32,
    pub delta_poc_s0_minus1: [u32; 16],
    pub delta_poc_s1_minus1: [u32; 16],
    // Flags
    pub inter_ref_pic_set_prediction_flag: bool,
    pub delta_rps_sign: bool,
}

/// Placeholder for `StdVideoH265LongTermRefPicsSps`.
#[derive(Debug, Clone, Default)]
pub struct H265LongTermRefPicsSps {
    pub used_by_curr_pic_lt_sps_flag: u32,
    pub lt_ref_pic_poc_lsb_sps: [u32; 32],
}

/// Placeholder for `StdVideoH265SequenceParameterSetVui`.
#[derive(Debug, Clone, Default)]
pub struct H265Vui {
    pub aspect_ratio_idc: u32,
    pub sar_width: u16,
    pub sar_height: u16,
    pub video_format: u8,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coeffs: u8,
    pub chroma_sample_loc_type_top_field: u8,
    pub chroma_sample_loc_type_bottom_field: u8,
    pub def_disp_win_left_offset: u32,
    pub def_disp_win_right_offset: u32,
    pub def_disp_win_top_offset: u32,
    pub def_disp_win_bottom_offset: u32,
    pub vui_num_units_in_tick: u32,
    pub vui_time_scale: u32,
    pub vui_num_ticks_poc_diff_one_minus1: u32,
    pub min_spatial_segmentation_idc: u32,
    pub max_bytes_per_pic_denom: u32,
    pub max_bits_per_min_cu_denom: u32,
    pub log2_max_mv_length_horizontal: u8,
    pub log2_max_mv_length_vertical: u8,
    // Flags
    pub aspect_ratio_info_present_flag: bool,
    pub overscan_info_present_flag: bool,
    pub overscan_appropriate_flag: bool,
    pub video_signal_type_present_flag: bool,
    pub video_full_range_flag: bool,
    pub colour_description_present_flag: bool,
    pub chroma_loc_info_present_flag: bool,
    pub neutral_chroma_indication_flag: bool,
    pub field_seq_flag: bool,
    pub frame_field_info_present_flag: bool,
    pub default_display_window_flag: bool,
    pub vui_timing_info_present_flag: bool,
    pub vui_poc_proportional_to_timing_flag: bool,
    pub vui_hrd_parameters_present_flag: bool,
    pub bitstream_restriction_flag: bool,
    pub tiles_fixed_structure_flag: bool,
    pub motion_vectors_over_pic_boundaries_flag: bool,
    pub restricted_ref_pic_lists_flag: bool,
}

/// Placeholder for `StdVideoH265HrdParameters`.
#[derive(Debug, Clone, Default)]
pub struct H265HrdParameters {
    pub tick_divisor_minus2: u32,
    pub du_cpb_removal_delay_increment_length_minus1: u8,
    pub dpb_output_delay_du_length_minus1: u8,
    pub bit_rate_scale: u8,
    pub cpb_size_scale: u8,
    pub cpb_size_du_scale: u8,
    pub initial_cpb_removal_delay_length_minus1: u8,
    pub au_cpb_removal_delay_length_minus1: u8,
    pub dpb_output_delay_length_minus1: u8,
    pub cpb_cnt_minus1: [u32; 7],
    // Flags
    pub nal_hrd_parameters_present_flag: bool,
    pub vcl_hrd_parameters_present_flag: bool,
    pub sub_pic_hrd_params_present_flag: bool,
    pub sub_pic_cpb_params_in_pic_timing_sei_flag: bool,
    pub fixed_pic_rate_general_flag: bool,
    pub fixed_pic_rate_within_cvs_flag: bool,
    pub low_delay_hrd_flag: bool,
}

/// Placeholder for `StdVideoH265SubLayerHrdParameters`.
#[derive(Debug, Clone, Default)]
pub struct H265SubLayerHrdParameters {
    pub bit_rate_value_minus1: [u32; 32],
    pub cpb_size_value_minus1: [u32; 32],
    pub cpb_size_du_value_minus1: [u32; 32],
    pub bit_rate_du_value_minus1: [u32; 32],
    pub cbr_flag: u32,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sps_default() {
        let sps = SpsH265::default();
        assert_eq!(sps.sps.chroma_format_idc, 0);
        assert_eq!(sps.sps.pic_width_in_luma_samples, 0);
        assert!(!sps.sps.vui_parameters_present_flag);
    }

    #[test]
    fn test_vps_default() {
        let vps = VpsH265::default();
        assert_eq!(vps.vps_info.vps_video_parameter_set_id, 0);
        assert!(!vps.vps_info.vps_timing_info_present_flag);
    }

    #[test]
    fn test_dec_pic_buf_mgr_default() {
        let mgr = H265DecPicBufMgr::default();
        assert_eq!(mgr.max_dec_pic_buffering_minus1[0], 0);
        assert_eq!(mgr.max_num_reorder_pics[0], 0);
    }

    #[test]
    fn test_short_term_ref_pic_set() {
        let mut rps = H265ShortTermRefPicSet::default();
        rps.num_negative_pics = 4;
        rps.used_by_curr_pic_s0_flag = (1 << 4) - 1; // 0x0F
        assert_eq!(rps.used_by_curr_pic_s0_flag, 0x0F);
    }

    #[test]
    fn test_hrd_parameters_default() {
        let hrd = H265HrdParameters::default();
        assert!(!hrd.nal_hrd_parameters_present_flag);
        assert_eq!(hrd.bit_rate_scale, 0);
    }
}
