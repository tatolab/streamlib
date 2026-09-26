// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The H.265 sequence parameter set, read from its RBSP (ITU-T H.265 §7.3.2.2).
//!
//! Ported from NVIDIA's `NvVideoParser` (`VulkanH265Parser.cpp`); the field
//! shapes mirror the `StdVideoH265*` structures the Vulkan Video decoder fills.

use crate::core::nal_unit_raw_byte_sequence_payload::RbspBitstreamReader;

pub const MAX_NUM_SPS: usize = 16;
pub const MAX_NUM_SUB_LAYERS: usize = 7;
pub const MAX_NUM_STRPS: usize = 64;
pub const MAX_NUM_LTRP: usize = 32;
pub const MAX_NUM_STRPS_ENTRIES: usize = 16;
/// Sublayers list size (mirrors `STD_VIDEO_H265_SUBLAYERS_LIST_SIZE`).
pub const STD_VIDEO_H265_SUBLAYERS_LIST_SIZE: usize = 7;

// ---------------------------------------------------------------------------
// H.265 Level IDC mapping
// ---------------------------------------------------------------------------

/// H.265 level identifiers (Vulkan standard video enum values).
///
/// `general_level_idc` is 30 * level_number per Table A.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum H265LevelIdc {
    #[default]
    Level1_0 = 0,
    Level2_0 = 1,
    Level2_1 = 2,
    Level3_0 = 3,
    Level3_1 = 4,
    Level4_0 = 5,
    Level4_1 = 6,
    Level5_0 = 7,
    Level5_1 = 8,
    Level5_2 = 9,
    Level6_0 = 10,
    Level6_1 = 11,
    Level6_2 = 12,
    MaxEnum = 0x7FFF_FFFF,
}

/// Convert `general_level_idc` byte to the Vulkan enum.
///
/// Accepts two formats:
/// - Raw H.265 spec bytes (Table A.4): general_level_idc = 30 * level
///   (e.g. 90 for Level 3.0, 150 for Level 5.0). Used by spec-compliant
///   encoders like ffmpeg/libx265.
/// - Vulkan StdVideoH265LevelIdc enum indices (0–12). The NVIDIA Vulkan
///   Video encoder driver writes these into the bitstream SPS instead of
///   the raw spec bytes.
pub fn general_level_idc_to_vulkan(general_level_idc: u8) -> H265LevelIdc {
    match general_level_idc as u32 {
        // Vulkan enum indices (StdVideoH265LevelIdc values 0–12)
        0 => H265LevelIdc::Level1_0,
        1 => H265LevelIdc::Level2_0,
        2 => H265LevelIdc::Level2_1,
        3 => H265LevelIdc::Level3_0,
        4 => H265LevelIdc::Level3_1,
        5 => H265LevelIdc::Level4_0,
        6 => H265LevelIdc::Level4_1,
        7 => H265LevelIdc::Level5_0,
        8 => H265LevelIdc::Level5_1,
        9 => H265LevelIdc::Level5_2,
        10 => H265LevelIdc::Level6_0,
        11 => H265LevelIdc::Level6_1,
        12 => H265LevelIdc::Level6_2,
        // Raw H.265 spec bytes (general_level_idc = 30 * level_number)
        30 => H265LevelIdc::Level1_0,
        60 => H265LevelIdc::Level2_0,
        63 => H265LevelIdc::Level2_1,
        90 => H265LevelIdc::Level3_0,
        93 => H265LevelIdc::Level3_1,
        120 => H265LevelIdc::Level4_0,
        123 => H265LevelIdc::Level4_1,
        150 => H265LevelIdc::Level5_0,
        153 => H265LevelIdc::Level5_1,
        156 => H265LevelIdc::Level5_2,
        180 => H265LevelIdc::Level6_0,
        183 => H265LevelIdc::Level6_1,
        186 => H265LevelIdc::Level6_2,
        _ => {
            tracing::error!("Invalid h.265 IDC Level: {}", general_level_idc);
            H265LevelIdc::Level6_2
        }
    }
}

// ---------------------------------------------------------------------------
// Scaling list (referenced from nv_vulkan_h265_scaling_list)
// ---------------------------------------------------------------------------

/// Single scaling list entry, ported from C++ `scaling_list_entry_s`.
#[derive(Debug, Clone)]
pub struct ScalingListEntry {
    pub scaling_list_pred_mode_flag: bool,
    pub scaling_list_pred_matrix_id_delta: i32,
    pub scaling_list_dc_coef_minus8: i32,
    pub scaling_list_delta_coef: [i8; 64],
}

impl Default for ScalingListEntry {
    fn default() -> Self {
        Self {
            scaling_list_pred_mode_flag: false,
            scaling_list_pred_matrix_id_delta: 0,
            scaling_list_dc_coef_minus8: 0,
            scaling_list_delta_coef: [0; 64],
        }
    }
}

/// Full scaling list data, ported from C++ `scaling_list_s`.
/// Indexed as `entry[sizeId][matrixId]`.
#[derive(Debug, Clone, Default)]
pub struct ScalingList {
    pub entry: [[ScalingListEntry; 6]; 4],
}

// ---------------------------------------------------------------------------
// Short-term reference picture set
// ---------------------------------------------------------------------------

/// Short-term reference picture set, ported from C++ `short_term_ref_pic_set_s`.
#[derive(Debug, Clone, Default)]
pub struct ShortTermRefPicSet {
    pub num_negative_pics: u8,
    pub num_positive_pics: u8,
    pub inter_ref_pic_set_prediction_flag: u8,
    pub delta_idx_minus1: u8,
    pub used_by_curr_pic_s0: [u8; MAX_NUM_STRPS_ENTRIES],
    pub used_by_curr_pic_s1: [u8; MAX_NUM_STRPS_ENTRIES],
    pub delta_poc_s0: [i32; MAX_NUM_STRPS_ENTRIES],
    pub delta_poc_s1: [i32; MAX_NUM_STRPS_ENTRIES],
}

/// Std-video compatible short-term ref pic set (bitmask-based).
/// Corresponds to `StdVideoH265ShortTermRefPicSet` in Vulkan headers.
#[derive(Debug, Clone, Default)]
pub struct StdShortTermRefPicSet {
    pub flags: StdShortTermRefPicSetFlags,
    pub delta_idx_minus1: u32,
    pub use_delta_flag: u32,
    pub abs_delta_rps_minus1: u32,
    pub used_by_curr_pic_flag: u32,
    pub used_by_curr_pic_s0_flag: u32,
    pub used_by_curr_pic_s1_flag: u32,
    pub num_negative_pics: u32,
    pub num_positive_pics: u32,
    pub delta_poc_s0_minus1: [u16; MAX_NUM_STRPS_ENTRIES],
    pub delta_poc_s1_minus1: [i32; MAX_NUM_STRPS_ENTRIES],
}

#[derive(Debug, Clone, Default)]
pub struct StdShortTermRefPicSetFlags {
    pub inter_ref_pic_set_prediction_flag: bool,
    pub delta_rps_sign: bool,
}

// ---------------------------------------------------------------------------
// HRD parameters
// ---------------------------------------------------------------------------

/// Sub-layer HRD parameters, ported from `StdVideoH265SubLayerHrdParameters`.
#[derive(Debug, Clone, Default)]
pub struct SubLayerHrdParameters {
    pub bit_rate_value_minus1: [u32; 32],
    pub cpb_size_value_minus1: [u32; 32],
    pub cpb_size_du_value_minus1: [u32; 32],
    pub bit_rate_du_value_minus1: [u32; 32],
    /// Bitmask: bit `i` set => CBR for CPB index `i`.
    pub cbr_flag: u32,
}

/// HRD parameters flags.
#[derive(Debug, Clone, Default)]
pub struct HrdParametersFlags {
    pub nal_hrd_parameters_present_flag: bool,
    pub vcl_hrd_parameters_present_flag: bool,
    pub sub_pic_hrd_params_present_flag: bool,
    pub sub_pic_cpb_params_in_pic_timing_sei_flag: bool,
    pub fixed_pic_rate_general_flag: u32,
    pub fixed_pic_rate_within_cvs_flag: u32,
    pub low_delay_hrd_flag: u32,
}

/// Video HRD parameters, ported from C++ `hevc_video_hrd_param_s`.
#[derive(Debug, Clone, Default)]
pub struct VideoHrdParameters {
    pub flags: HrdParametersFlags,
    pub tick_divisor_minus2: u8,
    pub du_cpb_removal_delay_increment_length_minus1: u8,
    pub dpb_output_delay_du_length_minus1: u8,
    pub bit_rate_scale: u8,
    pub cpb_size_scale: u8,
    pub cpb_size_du_scale: u8,
    pub initial_cpb_removal_delay_length_minus1: u8,
    pub au_cpb_removal_delay_length_minus1: u8,
    pub dpb_output_delay_length_minus1: u8,
    pub cpb_cnt_minus1: [u8; STD_VIDEO_H265_SUBLAYERS_LIST_SIZE],
    pub elemental_duration_in_tc_minus1: [u16; STD_VIDEO_H265_SUBLAYERS_LIST_SIZE],
    pub max_num_sub_layers: u32,
    pub sub_layer_hrd_parameters_nal: [SubLayerHrdParameters; STD_VIDEO_H265_SUBLAYERS_LIST_SIZE],
    pub sub_layer_hrd_parameters_vcl: [SubLayerHrdParameters; STD_VIDEO_H265_SUBLAYERS_LIST_SIZE],
}

// ---------------------------------------------------------------------------
// Profile / Tier / Level
// ---------------------------------------------------------------------------

/// Profile-tier-level info, ported from `StdVideoH265ProfileTierLevel`.
#[derive(Debug, Clone, Default)]
pub struct ProfileTierLevel {
    pub general_profile_idc: u32,
    pub general_level_idc: H265LevelIdc,
}

// ---------------------------------------------------------------------------
// Decoded Picture Buffer Management
// ---------------------------------------------------------------------------

/// Dec-pic-buf management parameters, ported from `StdVideoH265DecPicBufMgr`.
#[derive(Debug, Clone, Default)]
pub struct DecPicBufMgr {
    pub max_dec_pic_buffering_minus1: [u8; STD_VIDEO_H265_SUBLAYERS_LIST_SIZE],
    pub max_num_reorder_pics: [u8; STD_VIDEO_H265_SUBLAYERS_LIST_SIZE],
    pub max_latency_increase_plus1: [u8; STD_VIDEO_H265_SUBLAYERS_LIST_SIZE],
}

// ---------------------------------------------------------------------------
// VUI parameters
// ---------------------------------------------------------------------------

/// VUI flags, ported from `StdVideoH265SequenceParameterSetVui.flags`.
#[derive(Debug, Clone, Default)]
pub struct VuiFlags {
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

/// VUI parameters, ported from `StdVideoH265SequenceParameterSetVui`.
#[derive(Debug, Clone, Default)]
pub struct VuiParameters {
    pub flags: VuiFlags,
    pub aspect_ratio_idc: u8,
    pub sar_width: u16,
    pub sar_height: u16,
    pub video_format: u8,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coeffs: u8,
    pub chroma_sample_loc_type_top_field: u32,
    pub chroma_sample_loc_type_bottom_field: u32,
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
    pub log2_max_mv_length_horizontal: u32,
    pub log2_max_mv_length_vertical: u32,
}

// ---------------------------------------------------------------------------
// Long-term reference pictures SPS
// ---------------------------------------------------------------------------

/// Long-term ref pics SPS info, ported from `StdVideoH265LongTermRefPicsSps`.
#[derive(Debug, Clone, Default)]
pub struct LongTermRefPicsSps {
    /// Bitmask of `used_by_curr_pic_lt_sps_flag` per index.
    pub used_by_curr_pic_lt_sps_flag: u32,
    pub lt_ref_pic_poc_lsb_sps: [u32; MAX_NUM_LTRP],
}

// ---------------------------------------------------------------------------
// SPS flags
// ---------------------------------------------------------------------------

/// SPS flags, matching the C++ `StdVideoH265SpsFlags` bitfield.
#[derive(Debug, Clone, Default)]
pub struct SpsFlags {
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
}

// ---------------------------------------------------------------------------
// Sequence Parameter Set (SPS)
// ---------------------------------------------------------------------------

/// H.265 Sequence Parameter Set, ported from C++ `hevc_seq_param_s`.
#[derive(Debug, Clone)]
pub struct HevcSeqParam {
    pub flags: SpsFlags,
    pub profile_tier_level: ProfileTierLevel,
    pub dec_pic_buf_mgr: DecPicBufMgr,
    pub vui: VuiParameters,
    pub hrd_parameters: VideoHrdParameters,
    pub long_term_ref_pics_sps: LongTermRefPicsSps,
    pub scaling_lists: ScalingList,

    pub sps_video_parameter_set_id: u8,
    pub sps_max_sub_layers_minus1: u8,
    pub sps_seq_parameter_set_id: u8,
    pub chroma_format_idc: u8,
    pub pic_width_in_luma_samples: u32,
    pub pic_height_in_luma_samples: u32,
    pub conf_win_left_offset: u8,
    pub conf_win_right_offset: u8,
    pub conf_win_top_offset: u8,
    pub conf_win_bottom_offset: u8,
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
    pub num_short_term_ref_pic_sets: u8,
    pub num_long_term_ref_pics_sps: u8,

    pub max_dec_pic_buffering: u8,
    pub max_num_reorder_pics: u8,
    pub sps_rep_format_idx: u8,

    /// Short-term ref pic set data (internal representation).
    pub strpss: Vec<ShortTermRefPicSet>,
    /// Std-video short-term ref pic sets.
    pub std_short_term_ref_pic_sets: Vec<StdShortTermRefPicSet>,
}

impl Default for HevcSeqParam {
    fn default() -> Self {
        Self {
            flags: SpsFlags::default(),
            profile_tier_level: ProfileTierLevel::default(),
            dec_pic_buf_mgr: DecPicBufMgr::default(),
            vui: VuiParameters::default(),
            hrd_parameters: VideoHrdParameters::default(),
            long_term_ref_pics_sps: LongTermRefPicsSps::default(),
            scaling_lists: ScalingList::default(),
            sps_video_parameter_set_id: 0,
            sps_max_sub_layers_minus1: 0,
            sps_seq_parameter_set_id: 0,
            chroma_format_idc: 0,
            pic_width_in_luma_samples: 0,
            pic_height_in_luma_samples: 0,
            conf_win_left_offset: 0,
            conf_win_right_offset: 0,
            conf_win_top_offset: 0,
            conf_win_bottom_offset: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            log2_min_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_luma_coding_block_size: 0,
            log2_min_luma_transform_block_size_minus2: 0,
            log2_diff_max_min_luma_transform_block_size: 0,
            max_transform_hierarchy_depth_inter: 0,
            max_transform_hierarchy_depth_intra: 0,
            pcm_sample_bit_depth_luma_minus1: 0,
            pcm_sample_bit_depth_chroma_minus1: 0,
            log2_min_pcm_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_pcm_luma_coding_block_size: 0,
            num_short_term_ref_pic_sets: 0,
            num_long_term_ref_pics_sps: 0,
            max_dec_pic_buffering: 1,
            max_num_reorder_pics: 0,
            sps_rep_format_idx: 0,
            strpss: Vec::new(),
            std_short_term_ref_pic_sets: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing — ported from the C++ `VulkanH265Decoder` members
// ---------------------------------------------------------------------------

// -----------------------------------------------------------------------
// Short-term ref pic set parsing
// -----------------------------------------------------------------------

/// Parse a short_term_ref_pic_set from the bitstream.
/// Corresponds to C++ `short_term_ref_pic_set`.
///
/// `reader` provides the bitstream reading methods (u, ue, se).
/// Returns `None` on parse error.
pub fn parse_h265_short_term_ref_pic_set(
    reader: &mut RbspBitstreamReader,
    std_strps: &mut StdShortTermRefPicSet,
    strps: &mut ShortTermRefPicSet,
    all_strpss: &[ShortTermRefPicSet],
    idx: usize,
    num_short_term_ref_pic_sets: usize,
) -> Option<()> {
    let inter_ref_pic_set_prediction_flag = if idx != 0 { reader.u(1)? } else { 0 };
    strps.inter_ref_pic_set_prediction_flag = inter_ref_pic_set_prediction_flag as u8;
    std_strps.flags.inter_ref_pic_set_prediction_flag = inter_ref_pic_set_prediction_flag != 0;

    if inter_ref_pic_set_prediction_flag != 0 {
        let mut used_by_curr_pic_flag = [0u8; MAX_NUM_STRPS_ENTRIES + 1];
        let mut use_delta_flag = [0u8; MAX_NUM_STRPS_ENTRIES + 1];

        let delta_idx_minus1 = if idx == num_short_term_ref_pic_sets {
            reader.ue()? as u32
        } else {
            0
        };
        if delta_idx_minus1 >= idx as u32 {
            tracing::warn!(
                "Invalid delta_idx_minus1 ({} > {})",
                delta_idx_minus1,
                idx - 1
            );
            return None;
        }
        strps.delta_idx_minus1 = delta_idx_minus1 as u8;
        std_strps.delta_idx_minus1 = delta_idx_minus1;

        let delta_rps_sign = reader.u(1)?;
        std_strps.flags.delta_rps_sign = delta_rps_sign != 0;
        let abs_delta_rps_minus1 = reader.ue()? as i32;
        std_strps.abs_delta_rps_minus1 = abs_delta_rps_minus1 as u32;

        let delta_rps = (1 - 2 * delta_rps_sign as i32) * (abs_delta_rps_minus1 + 1);
        let r_idx = idx as i32 - (delta_idx_minus1 as i32 + 1);
        if r_idx < 0 || r_idx as usize >= all_strpss.len() {
            return None;
        }
        let rstrps = &all_strpss[r_idx as usize];

        let total = rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize;
        for j in 0..=total {
            if j >= MAX_NUM_STRPS_ENTRIES + 1 {
                break;
            }
            used_by_curr_pic_flag[j] = reader.u(1)? as u8;
            if used_by_curr_pic_flag[j] != 0 {
                std_strps.used_by_curr_pic_flag |= 1 << j;
            }
            use_delta_flag[j] = if used_by_curr_pic_flag[j] == 0 {
                reader.u(1)? as u8
            } else {
                1
            };
            if use_delta_flag[j] != 0 {
                std_strps.use_delta_flag |= 1 << j;
            }
        }

        // Derive S0 (negative)
        {
            let mut i = 0usize;
            for j in (0..rstrps.num_positive_pics as usize).rev() {
                let d_poc = rstrps.delta_poc_s1[j] + delta_rps;
                if d_poc < 0 && use_delta_flag[rstrps.num_negative_pics as usize + j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s0[i] = d_poc;
                    std_strps.delta_poc_s0_minus1[i] = d_poc as u16;
                    strps.used_by_curr_pic_s0[i] =
                        used_by_curr_pic_flag[rstrps.num_negative_pics as usize + j];
                    if strps.used_by_curr_pic_s0[i] != 0 {
                        std_strps.used_by_curr_pic_s0_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            if delta_rps < 0
                && use_delta_flag
                    [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize]
                    != 0
            {
                if i < MAX_NUM_STRPS_ENTRIES {
                    strps.delta_poc_s0[i] = delta_rps;
                    std_strps.delta_poc_s0_minus1[i] = delta_rps as u16;
                    strps.used_by_curr_pic_s0[i] = used_by_curr_pic_flag
                        [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize];
                    if strps.used_by_curr_pic_s0[i] != 0 {
                        std_strps.used_by_curr_pic_s0_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            for j in 0..rstrps.num_negative_pics as usize {
                let d_poc = rstrps.delta_poc_s0[j] + delta_rps;
                if d_poc < 0 && use_delta_flag[j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s0[i] = d_poc;
                    std_strps.delta_poc_s0_minus1[i] = d_poc as u16;
                    strps.used_by_curr_pic_s0[i] = used_by_curr_pic_flag[j];
                    if strps.used_by_curr_pic_s0[i] != 0 {
                        std_strps.used_by_curr_pic_s0_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            strps.num_negative_pics = i as u8;
            std_strps.num_negative_pics = i as u32;
        }

        // Derive S1 (positive)
        {
            let mut i = 0usize;
            for j in (0..rstrps.num_negative_pics as usize).rev() {
                let d_poc = rstrps.delta_poc_s0[j] + delta_rps;
                if d_poc > 0 && use_delta_flag[j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s1[i] = d_poc;
                    std_strps.delta_poc_s1_minus1[i] = d_poc;
                    strps.used_by_curr_pic_s1[i] = used_by_curr_pic_flag[j];
                    if strps.used_by_curr_pic_s1[i] != 0 {
                        std_strps.used_by_curr_pic_s1_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            if delta_rps > 0
                && use_delta_flag
                    [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize]
                    != 0
            {
                if i < MAX_NUM_STRPS_ENTRIES {
                    strps.delta_poc_s1[i] = delta_rps;
                    std_strps.delta_poc_s1_minus1[i] = delta_rps;
                    strps.used_by_curr_pic_s1[i] = used_by_curr_pic_flag
                        [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize];
                    if strps.used_by_curr_pic_s1[i] != 0 {
                        std_strps.used_by_curr_pic_s1_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            for j in 0..rstrps.num_positive_pics as usize {
                let d_poc = rstrps.delta_poc_s1[j] + delta_rps;
                if d_poc > 0 && use_delta_flag[rstrps.num_negative_pics as usize + j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s1[i] = d_poc;
                    std_strps.delta_poc_s1_minus1[i] = d_poc;
                    strps.used_by_curr_pic_s1[i] =
                        used_by_curr_pic_flag[rstrps.num_negative_pics as usize + j];
                    if strps.used_by_curr_pic_s1[i] != 0 {
                        std_strps.used_by_curr_pic_s1_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            strps.num_positive_pics = i as u8;
            std_strps.num_positive_pics = i as u32;
        }

        if strps.num_negative_pics as usize + strps.num_positive_pics as usize
            > MAX_NUM_STRPS_ENTRIES
        {
            tracing::warn!(
                "Invalid NumNegativePics+NumPositivePics ({}+{})",
                strps.num_negative_pics,
                strps.num_positive_pics
            );
            return None;
        }
    } else {
        // Direct coding (no inter-prediction)
        let num_negative_pics = reader.ue()? as u32;
        let num_positive_pics = reader.ue()? as u32;
        if num_negative_pics as usize > MAX_NUM_STRPS_ENTRIES
            || num_positive_pics as usize > MAX_NUM_STRPS_ENTRIES
            || (num_negative_pics + num_positive_pics) as usize > MAX_NUM_STRPS_ENTRIES
        {
            tracing::warn!(
                "Invalid num_negative_pics+num_positive_pics ({}+{})",
                num_negative_pics,
                num_positive_pics
            );
            return None;
        }

        let mut delta_poc_s0_minus1 = [0i16; MAX_NUM_STRPS_ENTRIES];
        let mut used_by_curr_pic_s0_flag = [0u8; MAX_NUM_STRPS_ENTRIES];
        let mut delta_poc_s1_minus1 = [0i16; MAX_NUM_STRPS_ENTRIES];
        let mut used_by_curr_pic_s1_flag = [0u8; MAX_NUM_STRPS_ENTRIES];

        for i in 0..num_negative_pics as usize {
            delta_poc_s0_minus1[i] = reader.ue()? as i16;
            used_by_curr_pic_s0_flag[i] = reader.u(1)? as u8;
        }
        for i in 0..num_positive_pics as usize {
            delta_poc_s1_minus1[i] = reader.ue()? as i16;
            used_by_curr_pic_s1_flag[i] = reader.u(1)? as u8;
        }

        strps.num_negative_pics = num_negative_pics as u8;
        std_strps.num_negative_pics = num_negative_pics;
        strps.num_positive_pics = num_positive_pics as u8;
        std_strps.num_positive_pics = num_positive_pics;

        for i in 0..num_negative_pics as usize {
            strps.delta_poc_s0[i] = (if i == 0 { 0 } else { strps.delta_poc_s0[i - 1] })
                - (delta_poc_s0_minus1[i] as i32 + 1);
            std_strps.delta_poc_s0_minus1[i] = strps.delta_poc_s0[i] as u16;
            strps.used_by_curr_pic_s0[i] = used_by_curr_pic_s0_flag[i];
            if strps.used_by_curr_pic_s0[i] != 0 {
                std_strps.used_by_curr_pic_s0_flag |= 1 << i;
            }
        }
        for i in 0..num_positive_pics as usize {
            strps.delta_poc_s1[i] = (if i == 0 { 0 } else { strps.delta_poc_s1[i - 1] })
                + (delta_poc_s1_minus1[i] as i32 + 1);
            std_strps.delta_poc_s1_minus1[i] = strps.delta_poc_s1[i];
            strps.used_by_curr_pic_s1[i] = used_by_curr_pic_s1_flag[i];
            if strps.used_by_curr_pic_s1[i] != 0 {
                std_strps.used_by_curr_pic_s1_flag |= 1 << i;
            }
        }
    }

    Some(())
}

// -----------------------------------------------------------------------
// Scaling list data parsing
// -----------------------------------------------------------------------

/// Parse scaling_list_data. Corresponds to C++ `scaling_list_data`.
pub fn parse_h265_scaling_list_data(
    reader: &mut RbspBitstreamReader,
    scl: &mut ScalingList,
) -> Option<()> {
    for size_id in 0..4u32 {
        let matrix_count = if size_id == 3 { 2 } else { 6 };
        for matrix_id in 0..matrix_count {
            let scle = &mut scl.entry[size_id as usize][matrix_id];
            scle.scaling_list_pred_mode_flag = reader.u(1)? != 0;
            if !scle.scaling_list_pred_mode_flag {
                let pred_matrix_id_delta = reader.ue()? as i32;
                let ref_matrix_id = matrix_id as i32 - pred_matrix_id_delta;
                scle.scaling_list_pred_matrix_id_delta = pred_matrix_id_delta;
                if ref_matrix_id < 0 {
                    tracing::warn!(
                        "Invalid scaling_list_pred_matrix_id_delta (refMatrixId = {})",
                        ref_matrix_id
                    );
                    return None;
                }
            } else {
                let coef_num = 64i32.min(1 << (4 + (size_id << 1)));
                let mut next_coef = 8i32;
                if size_id > 1 {
                    let dc_coef = reader.se()?;
                    scle.scaling_list_dc_coef_minus8 = dc_coef;
                    if dc_coef < -7 || dc_coef > 247 {
                        tracing::warn!("Invalid scaling_list_dc_coef_minus8 ({})", dc_coef);
                        return None;
                    }
                    next_coef = scle.scaling_list_dc_coef_minus8 + 8;
                }
                for i in 0..coef_num as usize {
                    let delta_coef = reader.se()?;
                    scle.scaling_list_delta_coef[i] = delta_coef as i8;
                    if delta_coef < -128 || delta_coef > 127 {
                        tracing::warn!("Invalid scaling_list_delta_coef ({})", delta_coef);
                        return None;
                    }
                    next_coef = (next_coef + delta_coef) & 0xff;
                    if next_coef == 0 {
                        tracing::warn!("Invalid scaling_list_delta_coef: zero ScalingList entry");
                        return None;
                    }
                }
            }
        }
    }
    Some(())
}

// -----------------------------------------------------------------------
// SPS parsing — ported from C++ seq_parameter_set_rbsp()
// -----------------------------------------------------------------------

/// Parse an H.265 Sequence Parameter Set from RBSP data (after EPB removal,
/// after the 2-byte NAL header). Returns `None` on parse error.
///
/// Ported from C++ `VulkanH265Decoder::seq_parameter_set_rbsp()`.
pub fn parse_h265_sequence_parameter_set(reader: &mut RbspBitstreamReader) -> Option<HevcSeqParam> {
    let mut sps = HevcSeqParam::default();

    sps.sps_video_parameter_set_id = reader.u(4)? as u8;

    // For single-layer (nuh_layer_id == 0) streams
    sps.sps_max_sub_layers_minus1 = reader.u(3)? as u8;
    if sps.sps_max_sub_layers_minus1 as usize >= MAX_NUM_SUB_LAYERS {
        tracing::warn!("Too many sub-layers: {}", sps.sps_max_sub_layers_minus1);
        return None;
    }

    sps.flags.sps_temporal_id_nesting_flag = reader.u(1)? != 0;

    // profile_tier_level(true, sps_max_sub_layers_minus1)
    sps.profile_tier_level = parse_h265_profile_tier_level(reader, sps.sps_max_sub_layers_minus1)?;

    sps.sps_seq_parameter_set_id = reader.ue()? as u8;
    if sps.sps_seq_parameter_set_id as usize >= MAX_NUM_SPS {
        return None;
    }

    sps.chroma_format_idc = reader.ue()? as u8;
    if sps.chroma_format_idc > 3 {
        return None;
    }
    if sps.chroma_format_idc == 3 {
        sps.flags.separate_colour_plane_flag = reader.u(1)? != 0;
    }

    sps.pic_width_in_luma_samples = reader.ue()?;
    sps.pic_height_in_luma_samples = reader.ue()?;

    // conformance_window_flag
    if reader.u(1)? != 0 {
        sps.flags.conformance_window_flag = true;
        let left = reader.ue()?;
        let right = reader.ue()?;
        let top = reader.ue()?;
        let bottom = reader.ue()?;
        sps.conf_win_left_offset = left.min(255) as u8;
        sps.conf_win_right_offset = right.min(255) as u8;
        sps.conf_win_top_offset = top.min(255) as u8;
        sps.conf_win_bottom_offset = bottom.min(255) as u8;
    }

    sps.bit_depth_luma_minus8 = reader.ue()? as u8;
    sps.bit_depth_chroma_minus8 = reader.ue()? as u8;

    sps.log2_max_pic_order_cnt_lsb_minus4 = reader.ue()? as u8;
    if sps.log2_max_pic_order_cnt_lsb_minus4 > 12 {
        tracing::warn!(
            "Invalid log2_max_pic_order_cnt_lsb_minus4: {}",
            sps.log2_max_pic_order_cnt_lsb_minus4
        );
        return None;
    }

    // sps_sub_layer_ordering_info_present_flag
    let sub_layer_ordering_present = reader.u(1)? != 0;
    sps.max_dec_pic_buffering = 1;
    sps.max_num_reorder_pics = 0;
    let start = if sub_layer_ordering_present {
        0
    } else {
        sps.sps_max_sub_layers_minus1 as usize
    };
    for i in start..=sps.sps_max_sub_layers_minus1 as usize {
        sps.dec_pic_buf_mgr.max_dec_pic_buffering_minus1[i] = reader.ue()? as u8;
        sps.dec_pic_buf_mgr.max_num_reorder_pics[i] = reader.ue()? as u8;
        sps.dec_pic_buf_mgr.max_latency_increase_plus1[i] = reader.ue()? as u8;
        if sps.dec_pic_buf_mgr.max_dec_pic_buffering_minus1[i] + 1 > sps.max_dec_pic_buffering {
            sps.max_dec_pic_buffering = sps.dec_pic_buf_mgr.max_dec_pic_buffering_minus1[i] + 1;
        }
        if sps.dec_pic_buf_mgr.max_num_reorder_pics[i] > sps.max_num_reorder_pics {
            sps.max_num_reorder_pics = sps.dec_pic_buf_mgr.max_num_reorder_pics[i];
        }
    }

    sps.log2_min_luma_coding_block_size_minus3 = reader.ue()? as u8;
    sps.log2_diff_max_min_luma_coding_block_size = reader.ue()? as u8;
    sps.log2_min_luma_transform_block_size_minus2 = reader.ue()? as u8;
    sps.log2_diff_max_min_luma_transform_block_size = reader.ue()? as u8;
    sps.max_transform_hierarchy_depth_inter = reader.ue()? as u8;
    sps.max_transform_hierarchy_depth_intra = reader.ue()? as u8;

    // scaling_list_enabled_flag
    sps.flags.scaling_list_enabled_flag = reader.u(1)? != 0;
    if sps.flags.scaling_list_enabled_flag {
        sps.flags.sps_scaling_list_data_present_flag = reader.u(1)? != 0;
        if sps.flags.sps_scaling_list_data_present_flag {
            parse_h265_scaling_list_data(reader, &mut sps.scaling_lists)?;
        }
    }

    sps.flags.amp_enabled_flag = reader.u(1)? != 0;
    sps.flags.sample_adaptive_offset_enabled_flag = reader.u(1)? != 0;
    sps.flags.pcm_enabled_flag = reader.u(1)? != 0;
    if sps.flags.pcm_enabled_flag {
        sps.pcm_sample_bit_depth_luma_minus1 = reader.u(4)? as u8;
        sps.pcm_sample_bit_depth_chroma_minus1 = reader.u(4)? as u8;
        sps.log2_min_pcm_luma_coding_block_size_minus3 = reader.ue()? as u8;
        sps.log2_diff_max_min_pcm_luma_coding_block_size = reader.ue()? as u8;
        sps.flags.pcm_loop_filter_disabled_flag = reader.u(1)? != 0;
    }

    let num_short_term_ref_pic_sets = reader.ue()?;
    if num_short_term_ref_pic_sets as usize > MAX_NUM_STRPS {
        tracing::warn!(
            "Invalid num_short_term_ref_pic_sets: {}",
            num_short_term_ref_pic_sets
        );
        return None;
    }
    sps.num_short_term_ref_pic_sets = num_short_term_ref_pic_sets as u8;
    sps.strpss = vec![ShortTermRefPicSet::default(); num_short_term_ref_pic_sets as usize];
    sps.std_short_term_ref_pic_sets =
        vec![StdShortTermRefPicSet::default(); num_short_term_ref_pic_sets as usize];

    for i in 0..num_short_term_ref_pic_sets as usize {
        // We need a temporary copy of strpss for the all_strpss parameter
        // because parse_short_term_ref_pic_set needs read access to earlier entries.
        let all_strpss: Vec<ShortTermRefPicSet> = sps.strpss[..i].to_vec();
        parse_h265_short_term_ref_pic_set(
            reader,
            &mut sps.std_short_term_ref_pic_sets[i],
            &mut sps.strpss[i],
            &all_strpss,
            i,
            num_short_term_ref_pic_sets as usize,
        )?;
    }

    sps.flags.long_term_ref_pics_present_flag = reader.u(1)? != 0;
    if sps.flags.long_term_ref_pics_present_flag {
        let num_lt = reader.ue()?;
        if num_lt as usize > MAX_NUM_LTRP {
            return None;
        }
        sps.num_long_term_ref_pics_sps = num_lt as u8;
        sps.long_term_ref_pics_sps.used_by_curr_pic_lt_sps_flag = 0;
        for i in 0..num_lt as usize {
            sps.long_term_ref_pics_sps.lt_ref_pic_poc_lsb_sps[i] =
                reader.u(sps.log2_max_pic_order_cnt_lsb_minus4 as u32 + 4)? as u32;
            if reader.u(1)? != 0 {
                sps.long_term_ref_pics_sps.used_by_curr_pic_lt_sps_flag |= 1 << i;
            }
        }
    }

    sps.flags.sps_temporal_mvp_enabled_flag = reader.u(1)? != 0;
    sps.flags.strong_intra_smoothing_enabled_flag = reader.u(1)? != 0;

    // VUI parameters — parse only the color-relevant subset
    // (aspect-ratio + video_signal_type → colour_description).
    // The rest of the VUI is left unparsed; full H.265 VUI parsing
    // is huge and not needed for decode correctness. Color fields
    // are surfaced via `SimpleDecoder::current_color_vui()`.
    sps.flags.vui_parameters_present_flag = reader.u(1)? != 0;
    if sps.flags.vui_parameters_present_flag {
        // aspect_ratio_info_present_flag
        let aspect_ratio_info_present = reader.u(1)? != 0;
        if aspect_ratio_info_present {
            let aspect_ratio_idc = reader.u(8)?;
            if aspect_ratio_idc == 255 {
                // EXTENDED_SAR
                reader.u(16)?; // sar_width
                reader.u(16)?; // sar_height
            }
        }
        // overscan_info_present_flag
        let overscan_info_present = reader.u(1)? != 0;
        if overscan_info_present {
            reader.u(1)?; // overscan_appropriate_flag
        }
        // video_signal_type_present_flag
        sps.vui.flags.video_signal_type_present_flag = reader.u(1)? != 0;
        if sps.vui.flags.video_signal_type_present_flag {
            sps.vui.video_format = reader.u(3)? as u8;
            sps.vui.flags.video_full_range_flag = reader.u(1)? != 0;
            sps.vui.flags.colour_description_present_flag = reader.u(1)? != 0;
            if sps.vui.flags.colour_description_present_flag {
                sps.vui.colour_primaries = reader.u(8)? as u8;
                sps.vui.transfer_characteristics = reader.u(8)? as u8;
                sps.vui.matrix_coeffs = reader.u(8)? as u8;
            }
        }
        // Stop here — remaining VUI fields (chroma_loc_info, neutral_chroma,
        // field_seq, frame_field_info, default_display_window, timing, HRD,
        // bitstream_restriction) are not consumed by streamlib today.
    }

    // SPS extensions — skip
    // (not needed for basic decode correctness)

    Some(sps)
}

/// Parse profile_tier_level() from the bitstream.
/// Returns a simplified ProfileTierLevel with profile_idc and level_idc.
pub fn parse_h265_profile_tier_level(
    reader: &mut RbspBitstreamReader,
    max_sub_layers_minus1: u8,
) -> Option<ProfileTierLevel> {
    // general_profile_space(2), general_tier_flag(1), general_profile_idc(5)
    let _profile_space = reader.u(2)?;
    let _tier_flag = reader.u(1)?;
    let general_profile_idc = reader.u(5)?;

    // general_profile_compatibility_flags[32]
    reader.u(32)?;

    // progressive_source_flag, interlaced_source_flag, non_packed_constraint_flag,
    // frame_only_constraint_flag = 4 bits
    reader.u(4)?;
    // 44 reserved zero bits
    reader.u(32)?;
    reader.u(12)?;

    let general_level_idc = reader.u(8)? as u8;

    // Sub-layer profile/level presence flags
    let mut sub_layer_profile_present = [false; 6];
    let mut sub_layer_level_present = [false; 6];
    for i in 0..max_sub_layers_minus1 as usize {
        sub_layer_profile_present[i] = reader.u(1)? != 0;
        sub_layer_level_present[i] = reader.u(1)? != 0;
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            reader.u(2)?; // reserved_zero_2bits
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if sub_layer_profile_present[i] {
            // 2+1+5+32+4+44 = 88 bits
            reader.u(32)?;
            reader.u(32)?;
            reader.u(24)?;
        }
        if sub_layer_level_present[i] {
            reader.u(8)?; // sub_layer_level_idc
        }
    }

    Some(ProfileTierLevel {
        general_profile_idc,
        general_level_idc: general_level_idc_to_vulkan(general_level_idc),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_general_level_idc_to_vulkan_raw_spec_bytes() {
        // Raw H.265 spec bytes (general_level_idc = 30 * level)
        assert_eq!(general_level_idc_to_vulkan(30), H265LevelIdc::Level1_0);
        assert_eq!(general_level_idc_to_vulkan(60), H265LevelIdc::Level2_0);
        assert_eq!(general_level_idc_to_vulkan(63), H265LevelIdc::Level2_1);
        assert_eq!(general_level_idc_to_vulkan(90), H265LevelIdc::Level3_0);
        assert_eq!(general_level_idc_to_vulkan(120), H265LevelIdc::Level4_0);
        assert_eq!(general_level_idc_to_vulkan(150), H265LevelIdc::Level5_0);
        assert_eq!(general_level_idc_to_vulkan(186), H265LevelIdc::Level6_2);
        // Invalid should map to Level6_2
        assert_eq!(general_level_idc_to_vulkan(255), H265LevelIdc::Level6_2);
    }

    #[test]
    fn test_general_level_idc_to_vulkan_enum_indices() {
        // Vulkan StdVideoH265LevelIdc enum indices (0–12), as written
        // by the NVIDIA encoder driver into the bitstream SPS.
        assert_eq!(general_level_idc_to_vulkan(0), H265LevelIdc::Level1_0);
        assert_eq!(general_level_idc_to_vulkan(3), H265LevelIdc::Level3_0);
        assert_eq!(general_level_idc_to_vulkan(5), H265LevelIdc::Level4_0);
        assert_eq!(general_level_idc_to_vulkan(7), H265LevelIdc::Level5_0);
        assert_eq!(general_level_idc_to_vulkan(12), H265LevelIdc::Level6_2);
    }

    // -----------------------------------------------------------------------
    // Short-term ref pic set parsing test
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_short_term_ref_pic_set_direct() {
        // Encode: inter_ref_pic_set_prediction_flag = 0 (implicit for idx=0)
        // num_negative_pics = ue(2) = '011'
        // num_positive_pics = ue(0) = '1'
        // For each negative pic:
        //   delta_poc_s0_minus1[0] = ue(0) = '1', used_by_curr_pic_s0_flag[0] = 1 = '1'
        //   delta_poc_s0_minus1[1] = ue(1) = '010', used_by_curr_pic_s0_flag[1] = 1 = '1'
        //
        // MSB-first bit sequence:
        //   0 1 1 | 1 | 1 | 1 | 0 1 0 | 1
        //   byte 0: 0111_1101 = 0x7D
        //   byte 1: 01xx_xxxx = 0x40
        let data = [0x7D, 0x40];
        let mut reader = RbspBitstreamReader::new(&data);
        let mut std_strps = StdShortTermRefPicSet::default();
        let mut strps = ShortTermRefPicSet::default();

        let result =
            parse_h265_short_term_ref_pic_set(&mut reader, &mut std_strps, &mut strps, &[], 0, 1);

        assert!(result.is_some());
        assert_eq!(strps.num_negative_pics, 2);
        assert_eq!(strps.num_positive_pics, 0);
        // DeltaPocS0[0] = 0 - (0 + 1) = -1
        assert_eq!(strps.delta_poc_s0[0], -1);
        // DeltaPocS0[1] = -1 - (1 + 1) = -3
        assert_eq!(strps.delta_poc_s0[1], -3);
        assert_eq!(strps.used_by_curr_pic_s0[0], 1);
        assert_eq!(strps.used_by_curr_pic_s0[1], 1);
    }
}
