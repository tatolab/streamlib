// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `VulkanH265Decoder.h` + `VulkanH265Parser.cpp`.
//!
//! H.265/HEVC bitstream parser implementing:
//! - VPS (Video Parameter Set) parsing
//! - SPS/PPS parsing (HEVC variants)
//! - Slice segment header parsing
//! - CTU-based DPB management
//! - Short-term and long-term reference picture sets (RPS)
//! - POC calculation
//! - Reference picture list construction with modification
//!
//! C++ reference:
//!   `vk_video_decoder/libs/NvVideoParser/include/VulkanH265Decoder.h`
//!   `vk_video_decoder/libs/NvVideoParser/src/VulkanH265Parser.cpp`

// ---------------------------------------------------------------------------
// Constants — direct ports of the C++ #defines
// ---------------------------------------------------------------------------

pub const MAX_NUM_VPS: usize = 16;
pub const MAX_NUM_SPS: usize = 16;
pub const MAX_NUM_PPS: usize = 64;
pub const MAX_NUM_SUB_LAYERS: usize = 7;
pub const MAX_NUM_STRPS: usize = 64;
pub const MAX_NUM_LTRP: usize = 32;
pub const MAX_NUM_STRPS_ENTRIES: usize = 16;
pub const MAX_NUM_REF_PICS: usize = 16;
pub const MAX_NUM_TILE_COLUMNS: usize = 20;
pub const MAX_NUM_TILE_ROWS: usize = 22;
pub const HEVC_DPB_SIZE: usize = 16;

pub const MAX_VPS_LAYERS: usize = 64;
pub const MAX_NUM_LAYER_IDS: usize = 64;
pub const MAX_VPS_LAYER_SETS: usize = 1024;
pub const MAX_NUM_SCALABILITY_TYPES: usize = 16;
pub const MAX_VPS_OP_SETS_PLUS1: usize = 1024;
pub const MAX_VPS_OUTPUTLAYER_SETS: usize = 1024;
pub const MAX_SUB_LAYERS: usize = 7;

/// Sublayers list size (mirrors `STD_VIDEO_H265_SUBLAYERS_LIST_SIZE`).
pub const STD_VIDEO_H265_SUBLAYERS_LIST_SIZE: usize = 7;
/// Max short-term ref pic sets (mirrors `STD_VIDEO_H265_MAX_SHORT_TERM_REF_PIC_SETS`).
pub const STD_VIDEO_H265_MAX_SHORT_TERM_REF_PIC_SETS: usize = 64;

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// H.265 profile identifiers.
///
/// Corresponds to `profile_e` in the C++ source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Profile {
    #[default]
    Invalid = 0,
    Main = 1,
    Main10 = 2,
    MainStillPic = 3,
    Main12 = 4,
    MainMvc = 5,
}

/// H.265 NAL unit type codes.
///
/// Corresponds to `nal_unit_type_e` in the C++ source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum NalUnitType {
    TrailN = 0,
    TrailR = 1,
    TsaN = 2,
    TsaR = 3,
    StsaN = 4,
    StsaR = 5,
    RadlN = 6,
    RadlR = 7,
    RaslN = 8,
    RaslR = 9,
    BlaWLp = 16,
    BlaWRadl = 17,
    BlaNLp = 18,
    IdrWRadl = 19,
    IdrNLp = 20,
    CraNut = 21,
    VpsNut = 32,
    SpsNut = 33,
    PpsNut = 34,
    AudNut = 35,
    EosNut = 36,
    EobNut = 37,
    FdNut = 38,
    PrefixSeiNut = 39,
    SuffixSeiNut = 40,
}

impl NalUnitType {
    /// Attempt to convert from a raw `u8` value.
    pub fn from_raw(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::TrailN),
            1 => Some(Self::TrailR),
            2 => Some(Self::TsaN),
            3 => Some(Self::TsaR),
            4 => Some(Self::StsaN),
            5 => Some(Self::StsaR),
            6 => Some(Self::RadlN),
            7 => Some(Self::RadlR),
            8 => Some(Self::RaslN),
            9 => Some(Self::RaslR),
            16 => Some(Self::BlaWLp),
            17 => Some(Self::BlaWRadl),
            18 => Some(Self::BlaNLp),
            19 => Some(Self::IdrWRadl),
            20 => Some(Self::IdrNLp),
            21 => Some(Self::CraNut),
            32 => Some(Self::VpsNut),
            33 => Some(Self::SpsNut),
            34 => Some(Self::PpsNut),
            35 => Some(Self::AudNut),
            36 => Some(Self::EosNut),
            37 => Some(Self::EobNut),
            38 => Some(Self::FdNut),
            39 => Some(Self::PrefixSeiNut),
            40 => Some(Self::SuffixSeiNut),
            _ => None,
        }
    }

    /// Returns `true` if this NAL unit type is a VCL slice type.
    pub fn is_slice(self) -> bool {
        let v = self as u8;
        (v <= NalUnitType::RaslR as u8)
            || (v >= NalUnitType::BlaWLp as u8 && v <= NalUnitType::CraNut as u8)
    }

    /// Returns `true` if this is a RAP (Random Access Point) picture.
    pub fn is_rap(self) -> bool {
        let v = self as u8;
        v >= NalUnitType::BlaWLp as u8 && v <= NalUnitType::CraNut as u8
    }

    /// Returns `true` if this is an IRAP (IDR/BLA/CRA) picture.
    /// The C++ code uses `<= 23` to include 2 reserved values.
    pub fn is_irap(self) -> bool {
        let v = self as u8;
        v >= NalUnitType::BlaWLp as u8 && v <= 23
    }

    /// Returns `true` if this is an IDR picture.
    pub fn is_idr(self) -> bool {
        self == NalUnitType::IdrWRadl || self == NalUnitType::IdrNLp
    }
}

/// H.265 slice types.
///
/// Corresponds to `hevc_slice_type_e` in the C++ source.
/// Note: HEVC uses B=0, P=1, I=2 (different from H.264).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HevcSliceType {
    B = 0,
    P = 1,
    I = 2,
}

impl HevcSliceType {
    pub fn from_raw(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::B),
            1 => Some(Self::P),
            2 => Some(Self::I),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Return values from ParseNalUnit
// ---------------------------------------------------------------------------

/// NAL unit parsing result codes.
pub const NALU_DISCARD: i32 = 0;
pub const NALU_SLICE: i32 = 1;

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Integer log2 for values up to 2^31.
/// Equivalent to C++ `Log2U31`: counts the number of bits needed to represent n.
/// Returns 0 for n=0, 1 for n=1, 2 for n=2..3, 3 for n=4..7, etc.
/// This is `floor(log2(n)) + 1` for n > 0.
fn log2_u31(n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    32 - n.leading_zeros()
}

/// Ceiling of log2(n), matching the C++ `CeilLog2` inline function.
/// `CeilLog2(n) = (n > 0) ? Log2U31(n-1) : 0`
/// Returns 0 for n <= 0.
pub fn ceil_log2(n: i32) -> u32 {
    if n > 0 {
        log2_u31((n - 1) as u32)
    } else {
        0
    }
}

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
// Representation format (VPS extension)
// ---------------------------------------------------------------------------

/// VPS representation format, ported from C++ `repFormat_t`.
#[derive(Debug, Clone, Default)]
pub struct RepFormat {
    pub chroma_and_bit_depth_vps_present_flag: bool,
    pub chroma_format_vps_idc: u32,
    pub separate_colour_plane_vps_flag: bool,
    pub pic_width_vps_in_luma_samples: u32,
    pub pic_height_vps_in_luma_samples: u32,
    pub bit_depth_vps_luma_minus8: u32,
    pub bit_depth_vps_chroma_minus8: u32,
    pub conformance_window_vps_flag: bool,
    pub conf_win_vps_left_offset: u32,
    pub conf_win_vps_right_offset: u32,
    pub conf_win_vps_top_offset: u32,
    pub conf_win_vps_bottom_offset: u32,
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
// PPS flags
// ---------------------------------------------------------------------------

/// PPS flags, matching the C++ `StdVideoH265PpsFlags` bitfield.
#[derive(Debug, Clone, Default)]
pub struct PpsFlags {
    pub dependent_slice_segments_enabled_flag: bool,
    pub output_flag_present_flag: bool,
    pub sign_data_hiding_enabled_flag: bool,
    pub cabac_init_present_flag: bool,
    pub constrained_intra_pred_flag: bool,
    pub transform_skip_enabled_flag: bool,
    pub cu_qp_delta_enabled_flag: bool,
    pub pps_slice_chroma_qp_offsets_present_flag: bool,
    pub weighted_pred_flag: bool,
    pub weighted_bipred_flag: bool,
    pub transquant_bypass_enabled_flag: bool,
    pub tiles_enabled_flag: bool,
    pub entropy_coding_sync_enabled_flag: bool,
    pub uniform_spacing_flag: bool,
    pub loop_filter_across_tiles_enabled_flag: bool,
    pub pps_loop_filter_across_slices_enabled_flag: bool,
    pub deblocking_filter_control_present_flag: bool,
    pub deblocking_filter_override_enabled_flag: bool,
    pub pps_deblocking_filter_disabled_flag: bool,
    pub pps_scaling_list_data_present_flag: bool,
    pub lists_modification_present_flag: bool,
    pub slice_segment_header_extension_present_flag: bool,
    pub pps_extension_present_flag: bool,
    pub pps_range_extension_flag: bool,
    pub cross_component_prediction_enabled_flag: bool,
    pub chroma_qp_offset_list_enabled_flag: bool,
}

// ---------------------------------------------------------------------------
// Picture Parameter Set (PPS)
// ---------------------------------------------------------------------------

/// H.265 Picture Parameter Set, ported from C++ `hevc_pic_param_s`.
#[derive(Debug, Clone)]
pub struct HevcPicParam {
    pub flags: PpsFlags,
    pub pps_pic_parameter_set_id: u8,
    pub pps_seq_parameter_set_id: u8,
    pub sps_video_parameter_set_id: u8,
    pub num_extra_slice_header_bits: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub init_qp_minus26: i8,
    pub diff_cu_qp_delta_depth: u8,
    pub pps_cb_qp_offset: i8,
    pub pps_cr_qp_offset: i8,
    pub num_tile_columns_minus1: u8,
    pub num_tile_rows_minus1: u8,
    pub column_width_minus1: [u16; MAX_NUM_TILE_COLUMNS],
    pub row_height_minus1: [u16; MAX_NUM_TILE_ROWS],
    pub pps_beta_offset_div2: i8,
    pub pps_tc_offset_div2: i8,
    pub log2_parallel_merge_level_minus2: u8,
    pub log2_max_transform_skip_block_size_minus2: u8,
    pub diff_cu_chroma_qp_offset_depth: u8,
    pub chroma_qp_offset_list_len_minus1: u8,
    pub cb_qp_offset_list: [i8; 6],
    pub cr_qp_offset_list: [i8; 6],
    pub log2_sao_offset_scale_luma: u8,
    pub log2_sao_offset_scale_chroma: u8,
    pub pps_scaling_list: ScalingList,
}

impl Default for HevcPicParam {
    fn default() -> Self {
        Self {
            flags: PpsFlags {
                uniform_spacing_flag: true,
                loop_filter_across_tiles_enabled_flag: true,
                ..Default::default()
            },
            pps_pic_parameter_set_id: 0,
            pps_seq_parameter_set_id: 0,
            sps_video_parameter_set_id: 0,
            num_extra_slice_header_bits: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            init_qp_minus26: 0,
            diff_cu_qp_delta_depth: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            column_width_minus1: [0; MAX_NUM_TILE_COLUMNS],
            row_height_minus1: [0; MAX_NUM_TILE_ROWS],
            pps_beta_offset_div2: 0,
            pps_tc_offset_div2: 0,
            log2_parallel_merge_level_minus2: 0,
            log2_max_transform_skip_block_size_minus2: 0,
            diff_cu_chroma_qp_offset_depth: 0,
            chroma_qp_offset_list_len_minus1: 0,
            cb_qp_offset_list: [0; 6],
            cr_qp_offset_list: [0; 6],
            log2_sao_offset_scale_luma: 0,
            log2_sao_offset_scale_chroma: 0,
            pps_scaling_list: ScalingList::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Video Parameter Set — VPS private flags
// ---------------------------------------------------------------------------

/// VPS-extension bitfield flags, ported from C++ `hevc_video_param_flags`.
#[derive(Debug, Clone, Default)]
pub struct VpsPrivFlags {
    pub vps_base_layer_internal_flag: bool,
    pub vps_base_layer_available_flag: bool,
    pub vps_extension_flag: bool,
    pub splitting_flag: bool,
    pub vps_nuh_layer_id_present_flag: bool,
    pub vps_sub_layers_max_minus1_present_flag: bool,
    pub max_tid_ref_present_flag: bool,
    pub default_ref_layers_active_flag: bool,
    pub rep_format_idx_present_flag: bool,
    pub max_one_active_ref_layer_flag: bool,
    pub vps_poc_lsb_aligned_flag: bool,
}

/// VPS flags from the base `StdVideoH265VpsFlags`.
#[derive(Debug, Clone, Default)]
pub struct VpsBaseFlags {
    pub vps_temporal_id_nesting_flag: bool,
    pub vps_sub_layer_ordering_info_present_flag: bool,
    pub vps_timing_info_present_flag: bool,
    pub vps_poc_proportional_to_timing_flag: bool,
}

// ---------------------------------------------------------------------------
// Video Parameter Set (VPS)
// ---------------------------------------------------------------------------

/// H.265 Video Parameter Set, ported from C++ `hevc_video_param_s`.
#[derive(Debug, Clone)]
pub struct HevcVideoParam {
    pub base_flags: VpsBaseFlags,
    pub priv_flags: VpsPrivFlags,
    pub profile_tier_level: ProfileTierLevel,
    pub dec_pic_buf_mgr: DecPicBufMgr,
    pub hrd_parameters: Option<Vec<VideoHrdParameters>>,

    pub vps_video_parameter_set_id: u32,
    pub vps_max_layers_minus1: u32,
    pub vps_max_sub_layers_minus1: u32,
    pub vps_max_layer_id: u32,
    pub vps_num_layer_sets: u32,
    pub vps_num_units_in_tick: u32,
    pub vps_time_scale: u32,
    pub vps_num_ticks_poc_diff_one_minus1: u32,
    pub vps_num_hrd_parameters: u32,

    pub layer_id_included_flag: Vec<Vec<u8>>,
    pub num_layers_in_id_list: Vec<u32>,
    pub layer_set_layer_id_list: Vec<Vec<u8>>,
    pub hrd_layer_set_idx: Vec<u32>,
    pub cprms_present_flag: Vec<u8>,

    // VPS Extension fields
    pub scalability_mask_flag: [u8; MAX_NUM_SCALABILITY_TYPES],
    pub num_scalability_types: u32,
    pub dimension_id_len: [u8; MAX_NUM_SCALABILITY_TYPES],
    pub layer_id_in_nuh: [u8; MAX_NUM_LAYER_IDS],
    pub layer_idx_in_vps: [u8; MAX_NUM_LAYER_IDS],
    pub dimension_id: [[u8; MAX_NUM_SCALABILITY_TYPES]; MAX_NUM_LAYER_IDS],
    pub num_views: u32,
    pub view_order_idx: [u8; MAX_NUM_LAYER_IDS],
    pub view_id_len: u32,
    pub view_id_val: [u8; MAX_NUM_LAYER_IDS],
    pub direct_dependency_flag: [[u8; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
    pub dependency_flag: [[u8; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
    pub num_direct_ref_layers: [u8; MAX_NUM_LAYER_IDS],
    pub id_direct_ref_layer: [[u8; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
    pub num_ref_layers: [u8; MAX_NUM_LAYER_IDS],
    pub id_ref_layer: [[u8; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
    pub num_predicted_layers: [u8; MAX_NUM_LAYER_IDS],
    pub id_predicted_layer: [[u8; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
    pub layer_id_in_list_flag: [u8; MAX_NUM_LAYER_IDS],
    pub num_layers_in_tree_partition: [u32; MAX_NUM_LAYER_IDS],
    pub tree_partition_layer_id_list: [[u8; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
    pub num_independent_layers: u32,
    pub num_add_layer_sets: u32,
    pub highest_layer_idx_plus1: Vec<Vec<u8>>,

    pub sub_layers_vps_max_minus1: [u8; MAX_NUM_LAYER_IDS],
    pub max_tid_il_ref_pics_plus1: [[u8; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],

    pub vps_num_profile_tier_level_minus1: u32,
    pub vps_profile_present_flag: Vec<u8>,

    // Operation Points
    pub num_add_olss: u32,
    pub num_output_layer_sets: u32,
    pub default_output_layer_idc: u32,
    pub layer_set_idx_for_ols_minus1: Vec<u32>,
    pub output_layer_flag: Vec<Vec<u32>>,
    pub num_necessary_layers: Vec<u8>,
    pub necessary_layer_flag: Vec<Vec<u8>>,
    pub num_output_layers_in_output_layer_set: Vec<u8>,
    pub ols_highest_output_layer_id: Vec<u8>,
    pub profile_tier_level_idx: Vec<Vec<u8>>,

    // Output Format
    pub vps_num_rep_formats_minus1: u32,
    pub rep_format: Vec<RepFormat>,
    pub vps_rep_format_idx: [u8; MAX_NUM_LAYER_IDS],
    pub poc_lsb_not_present_flag: [u8; MAX_NUM_LAYER_IDS],

    // DPB size
    pub sub_layer_flag_info_present_flag: Vec<u8>,
    pub sub_layer_dpb_info_present_flag: Vec<Vec<u8>>,
    pub max_vps_dec_pic_buffering_minus1: Vec<Vec<Vec<u8>>>,
    pub max_vps_num_reorder_pics: Vec<Vec<u8>>,
    pub max_vps_latency_increase_plus1: Vec<Vec<u8>>,

    pub vps_extension2_flag: u32,
}

impl Default for HevcVideoParam {
    fn default() -> Self {
        Self {
            base_flags: VpsBaseFlags::default(),
            priv_flags: VpsPrivFlags::default(),
            profile_tier_level: ProfileTierLevel::default(),
            dec_pic_buf_mgr: DecPicBufMgr::default(),
            hrd_parameters: None,
            vps_video_parameter_set_id: 0,
            vps_max_layers_minus1: 0,
            vps_max_sub_layers_minus1: 0,
            vps_max_layer_id: 0,
            vps_num_layer_sets: 0,
            vps_num_units_in_tick: 0,
            vps_time_scale: 0,
            vps_num_ticks_poc_diff_one_minus1: 0,
            vps_num_hrd_parameters: 0,
            layer_id_included_flag: Vec::new(),
            num_layers_in_id_list: Vec::new(),
            layer_set_layer_id_list: Vec::new(),
            hrd_layer_set_idx: Vec::new(),
            cprms_present_flag: Vec::new(),
            scalability_mask_flag: [0; MAX_NUM_SCALABILITY_TYPES],
            num_scalability_types: 0,
            dimension_id_len: [0; MAX_NUM_SCALABILITY_TYPES],
            layer_id_in_nuh: [0; MAX_NUM_LAYER_IDS],
            layer_idx_in_vps: [0; MAX_NUM_LAYER_IDS],
            dimension_id: [[0; MAX_NUM_SCALABILITY_TYPES]; MAX_NUM_LAYER_IDS],
            num_views: 0,
            view_order_idx: [0; MAX_NUM_LAYER_IDS],
            view_id_len: 0,
            view_id_val: [0; MAX_NUM_LAYER_IDS],
            direct_dependency_flag: [[0; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
            dependency_flag: [[0; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
            num_direct_ref_layers: [0; MAX_NUM_LAYER_IDS],
            id_direct_ref_layer: [[0; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
            num_ref_layers: [0; MAX_NUM_LAYER_IDS],
            id_ref_layer: [[0; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
            num_predicted_layers: [0; MAX_NUM_LAYER_IDS],
            id_predicted_layer: [[0; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
            layer_id_in_list_flag: [0; MAX_NUM_LAYER_IDS],
            num_layers_in_tree_partition: [0; MAX_NUM_LAYER_IDS],
            tree_partition_layer_id_list: [[0; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
            num_independent_layers: 0,
            num_add_layer_sets: 0,
            highest_layer_idx_plus1: Vec::new(),
            sub_layers_vps_max_minus1: [0; MAX_NUM_LAYER_IDS],
            max_tid_il_ref_pics_plus1: [[0; MAX_NUM_LAYER_IDS]; MAX_NUM_LAYER_IDS],
            vps_num_profile_tier_level_minus1: 0,
            vps_profile_present_flag: Vec::new(),
            num_add_olss: 0,
            num_output_layer_sets: 0,
            default_output_layer_idc: 0,
            layer_set_idx_for_ols_minus1: Vec::new(),
            output_layer_flag: Vec::new(),
            num_necessary_layers: Vec::new(),
            necessary_layer_flag: Vec::new(),
            num_output_layers_in_output_layer_set: Vec::new(),
            ols_highest_output_layer_id: Vec::new(),
            profile_tier_level_idx: Vec::new(),
            vps_num_rep_formats_minus1: 0,
            rep_format: Vec::new(),
            vps_rep_format_idx: [0; MAX_NUM_LAYER_IDS],
            poc_lsb_not_present_flag: [0; MAX_NUM_LAYER_IDS],
            sub_layer_flag_info_present_flag: Vec::new(),
            sub_layer_dpb_info_present_flag: Vec::new(),
            max_vps_dec_pic_buffering_minus1: Vec::new(),
            max_vps_num_reorder_pics: Vec::new(),
            max_vps_latency_increase_plus1: Vec::new(),
            vps_extension2_flag: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Slice header
// ---------------------------------------------------------------------------

/// H.265 slice header, ported from C++ `hevc_slice_header_s`.
#[derive(Debug, Clone)]
pub struct HevcSliceHeader {
    pub nal_unit_type: u8,
    pub nuh_temporal_id_plus1: u8,
    pub pic_output_flag: u8,
    pub collocated_from_l0_flag: u8,

    pub first_slice_segment_in_pic_flag: u8,
    pub no_output_of_prior_pics_flag: u8,
    pub pic_parameter_set_id: u8,
    pub slice_type: u8,

    pub slice_segment_address: u32,

    pub colour_plane_id: u8,
    pub short_term_ref_pic_set_sps_flag: u8,
    pub short_term_ref_pic_set_idx: u8,
    pub num_long_term_sps: u8,

    pub pic_order_cnt_lsb: u16,
    pub num_long_term_pics: u8,

    pub num_bits_for_short_term_rps_in_slice: u32,
    /// Bitmask for `used_by_curr_pic_lt` per ref pic index.
    pub used_by_curr_pic_lt_flags: u32,
    /// Bitmask for `delta_poc_msb_present_flag` per ref pic index.
    pub delta_poc_msb_present_flags: u32,

    pub lt_idx_sps: [u8; MAX_NUM_REF_PICS],
    pub poc_lsb_lt: [u16; MAX_NUM_REF_PICS],
    pub delta_poc_msb_cycle_lt: [i32; MAX_NUM_REF_PICS],

    pub slice_temporal_mvp_enabled_flag: u8,
    pub inter_layer_pred_enabled_flag: u8,
    pub num_inter_layer_ref_pics_minus1: u8,
    pub num_active_ref_layer_pics: u8,

    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub inter_layer_pred_layer_idc: [u8; MAX_VPS_LAYERS],

    /// Short-term ref pic set parsed in slice header (when not from SPS).
    pub strps: ShortTermRefPicSet,
}

impl Default for HevcSliceHeader {
    fn default() -> Self {
        Self {
            nal_unit_type: 0,
            nuh_temporal_id_plus1: 0,
            pic_output_flag: 1,
            collocated_from_l0_flag: 1,
            first_slice_segment_in_pic_flag: 0,
            no_output_of_prior_pics_flag: 0,
            pic_parameter_set_id: 0,
            slice_type: 0,
            slice_segment_address: 0,
            colour_plane_id: 0,
            short_term_ref_pic_set_sps_flag: 0,
            short_term_ref_pic_set_idx: 0,
            num_long_term_sps: 0,
            pic_order_cnt_lsb: 0,
            num_long_term_pics: 0,
            num_bits_for_short_term_rps_in_slice: 0,
            used_by_curr_pic_lt_flags: 0,
            delta_poc_msb_present_flags: 0,
            lt_idx_sps: [0; MAX_NUM_REF_PICS],
            poc_lsb_lt: [0; MAX_NUM_REF_PICS],
            delta_poc_msb_cycle_lt: [0; MAX_NUM_REF_PICS],
            slice_temporal_mvp_enabled_flag: 0,
            inter_layer_pred_enabled_flag: 0,
            num_inter_layer_ref_pics_minus1: 0,
            num_active_ref_layer_pics: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            inter_layer_pred_layer_idc: [0; MAX_VPS_LAYERS],
            strps: ShortTermRefPicSet::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// DPB entry
// ---------------------------------------------------------------------------

/// DPB entry state constants.
pub const DPB_STATE_EMPTY: i32 = 0;
pub const DPB_STATE_IN_USE: i32 = 1;

/// DPB marking constants.
pub const DPB_MARKING_UNUSED: i32 = 0;
pub const DPB_MARKING_SHORT_TERM: i32 = 1;
pub const DPB_MARKING_LONG_TERM: i32 = 2;

/// H.265 DPB entry, ported from C++ `hevc_dpb_entry_s`.
#[derive(Debug, Clone, Default)]
pub struct HevcDpbEntry {
    /// 0: empty, 1: in use
    pub state: i32,
    /// 0: unused, 1: short-term, 2: long-term
    pub marking: i32,
    /// 0: not needed for output, 1: needed for output
    pub output: i32,
    pub pic_order_cnt_val: i32,
    pub layer_id: i32,
    /// Picture buffer index (opaque identifier for the external allocator).
    pub pic_buf_idx: Option<usize>,
}

// ---------------------------------------------------------------------------
// Mastering display colour volume SEI
// ---------------------------------------------------------------------------

/// Mastering display colour volume SEI message (H.265 Annex D.2.27).
#[derive(Debug, Clone, Default)]
pub struct MasteringDisplayColourVolume {
    pub display_primaries_x: [u16; 3],
    pub display_primaries_y: [u16; 3],
    pub white_point_x: u16,
    pub white_point_y: u16,
    pub max_display_mastering_luminance: u32,
    pub min_display_mastering_luminance: u32,
}

// ---------------------------------------------------------------------------
// Parser private data
// ---------------------------------------------------------------------------

/// Per-decoder tracking data, ported from C++ `H265ParserData`.
#[derive(Debug, Clone)]
pub struct H265ParserData {
    pub sps_client_update_count: [u64; MAX_NUM_SPS],
    pub pps_client_update_count: [u64; MAX_NUM_PPS],
    pub vps_client_update_count: [u64; MAX_NUM_VPS],
    pub display: MasteringDisplayColourVolume,
}

impl Default for H265ParserData {
    fn default() -> Self {
        Self {
            sps_client_update_count: [0; MAX_NUM_SPS],
            pps_client_update_count: [0; MAX_NUM_PPS],
            vps_client_update_count: [0; MAX_NUM_VPS],
            display: MasteringDisplayColourVolume::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// VulkanH265Decoder — main decoder structure
// ---------------------------------------------------------------------------

/// H.265/HEVC decoder, ported from C++ class `VulkanH265Decoder`.
///
/// This struct holds all the state required for H.265 bitstream parsing
/// including parameter set stores, DPB, reference picture set management,
/// and POC calculation state.
#[derive(Debug)]
pub struct VulkanH265Decoder {
    // Parser private data
    pub parser_data: H265ParserData,

    // DPB state
    pub max_dpb_size: i32,
    pub picture_started: bool,
    pub prev_pic_order_cnt_msb: i32,
    pub prev_pic_order_cnt_lsb: i32,
    pub intra_pic_flag: bool,
    pub no_rasl_output_flag: bool,
    pub num_bits_for_short_term_rps_in_slice: i32,
    pub num_delta_pocs_of_ref_rps_idx: i32,
    pub num_poc_total_curr: i32,
    pub num_poc_st_curr_before: i32,
    pub num_poc_st_curr_after: i32,
    pub num_poc_lt_curr: i32,
    pub num_active_ref_layer_pics_0: i32,
    pub num_active_ref_layer_pics_1: i32,
    pub nuh_layer_id: i32,
    pub max_dec_pic_buffering: i32,

    // Reference picture set indices into DPB
    pub ref_pic_set_st_curr_before: [i8; 32],
    pub ref_pic_set_st_curr_after: [i8; 32],
    pub ref_pic_set_lt_curr: [i8; 32],
    pub ref_pic_set_inter_layer_0: [i8; 32],
    pub ref_pic_set_inter_layer_1: [i8; 32],

    // Current picture
    pub dpb_cur: Option<usize>, // index into dpb array, replaces raw pointer
    pub current_dpb_id: i8,
    pub dpb: [HevcDpbEntry; HEVC_DPB_SIZE],
    pub slh: HevcSliceHeader,

    // Parameter set stores
    pub active_sps: [Option<Box<HevcSeqParam>>; MAX_VPS_LAYERS],
    pub active_pps: [Option<Box<HevcPicParam>>; MAX_VPS_LAYERS],
    pub active_vps: Option<Box<HevcVideoParam>>,
    pub spss: [Option<Box<HevcSeqParam>>; MAX_NUM_SPS],
    pub ppss: [Option<Box<HevcPicParam>>; MAX_NUM_PPS],
    pub vpss: [Option<Box<HevcVideoParam>>; MAX_NUM_VPS],

    pub display: Option<MasteringDisplayColourVolume>,
}

// Use const arrays for Default - avoids issues with array of Option<Box<T>>
const NONE_SPS: Option<Box<HevcSeqParam>> = None;
const NONE_PPS: Option<Box<HevcPicParam>> = None;
const NONE_VPS: Option<Box<HevcVideoParam>> = None;

impl Default for VulkanH265Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VulkanH265Decoder {
    /// Create a new decoder. Corresponds to the C++ constructor.
    pub fn new() -> Self {
        Self {
            parser_data: H265ParserData::default(),
            max_dpb_size: 0,
            picture_started: false,
            prev_pic_order_cnt_msb: 0,
            prev_pic_order_cnt_lsb: -1,
            intra_pic_flag: false,
            no_rasl_output_flag: false,
            num_bits_for_short_term_rps_in_slice: 0,
            num_delta_pocs_of_ref_rps_idx: 0,
            num_poc_total_curr: 0,
            num_poc_st_curr_before: 0,
            num_poc_st_curr_after: 0,
            num_poc_lt_curr: 0,
            num_active_ref_layer_pics_0: 0,
            num_active_ref_layer_pics_1: 0,
            nuh_layer_id: 0,
            max_dec_pic_buffering: 0,
            ref_pic_set_st_curr_before: [-1; 32],
            ref_pic_set_st_curr_after: [-1; 32],
            ref_pic_set_lt_curr: [-1; 32],
            ref_pic_set_inter_layer_0: [-1; 32],
            ref_pic_set_inter_layer_1: [-1; 32],
            dpb_cur: None,
            current_dpb_id: -1,
            dpb: Default::default(),
            slh: HevcSliceHeader::default(),
            active_sps: [NONE_SPS; MAX_VPS_LAYERS],
            active_pps: [NONE_PPS; MAX_VPS_LAYERS],
            active_vps: None,
            spss: [NONE_SPS; MAX_NUM_SPS],
            ppss: [NONE_PPS; MAX_NUM_PPS],
            vpss: [NONE_VPS; MAX_NUM_VPS],
            display: None,
        }
    }

    // -----------------------------------------------------------------------
    // Initialization / End-of-stream
    // -----------------------------------------------------------------------

    /// Initialize the parser. Corresponds to C++ `InitParser`.
    pub fn init_parser(&mut self) {
        self.max_dpb_size = 0;
        self.picture_started = false;
        self.nuh_layer_id = 0;
        self.max_dec_pic_buffering = 0;
        self.end_of_stream();
    }

    /// End of stream handling. Corresponds to C++ `EndOfStream`.
    pub fn end_of_stream(&mut self) {
        self.flush_decoded_picture_buffer(false);
        self.slh = HevcSliceHeader::default();

        for vps in self.vpss.iter_mut() {
            *vps = None;
        }
        for sps in self.spss.iter_mut() {
            *sps = None;
        }
        for pps in self.ppss.iter_mut() {
            *pps = None;
        }
        for sps in self.active_sps.iter_mut() {
            *sps = None;
        }
        for pps in self.active_pps.iter_mut() {
            *pps = None;
        }
        self.active_vps = None;
        self.dpb = Default::default();
        self.dpb_cur = None;
        self.current_dpb_id = -1;
        self.picture_started = false;
        self.prev_pic_order_cnt_msb = 0;
        self.prev_pic_order_cnt_lsb = -1;
        self.display = None;
    }

    // -----------------------------------------------------------------------
    // DPB management — corresponds to the C++ DPB section
    // -----------------------------------------------------------------------

    /// Get the maximum DPB size based on level and picture size.
    /// Corresponds to C++ static function `GetMaxDpbSize`.
    pub fn get_max_dpb_size(sps: &HevcSeqParam) -> i32 {
        let max_luma_ps: i32 = match sps.profile_tier_level.general_level_idc {
            H265LevelIdc::Level1_0 => 36864,
            H265LevelIdc::Level2_0 => 122880,
            H265LevelIdc::Level2_1 => 245760,
            H265LevelIdc::Level3_0 => 552960,
            H265LevelIdc::Level3_1 => 983040,
            H265LevelIdc::Level4_0 | H265LevelIdc::Level4_1 => 2228224,
            H265LevelIdc::Level5_0 | H265LevelIdc::Level5_1 | H265LevelIdc::Level5_2 => 8912896,
            H265LevelIdc::Level6_0
            | H265LevelIdc::Level6_1
            | H265LevelIdc::Level6_2
            | H265LevelIdc::MaxEnum => 35651584,
        };

        let pic_size_in_samples_y =
            sps.pic_width_in_luma_samples as i32 * sps.pic_height_in_luma_samples as i32;
        let max_dpb_pic_buf = 6;

        let max_dpb_size = if pic_size_in_samples_y <= (max_luma_ps >> 2) {
            max_dpb_pic_buf * 4
        } else if pic_size_in_samples_y <= (max_luma_ps >> 1) {
            max_dpb_pic_buf * 2
        } else if pic_size_in_samples_y <= ((3 * max_luma_ps) >> 2) {
            (max_dpb_pic_buf * 4) / 3
        } else {
            max_dpb_pic_buf
        };

        max_dpb_size.min(HEVC_DPB_SIZE as i32)
    }

    /// Flush the decoded picture buffer.
    /// Corresponds to C++ `flush_decoded_picture_buffer`.
    pub fn flush_decoded_picture_buffer(&mut self, no_output_of_prior_pics: bool) {
        // Mark all reference pictures as "unused for reference"
        for i in 0..HEVC_DPB_SIZE {
            self.dpb[i].marking = DPB_MARKING_UNUSED;
            if no_output_of_prior_pics {
                self.dpb[i].output = 0;
            }
            if self.dpb[i].state == DPB_STATE_IN_USE
                && self.dpb[i].output == 0
                && self.dpb[i].marking == DPB_MARKING_UNUSED
            {
                self.dpb[i].state = DPB_STATE_EMPTY;
                self.dpb[i].pic_buf_idx = None;
            }
        }

        while !self.dpb_empty() {
            if !self.dpb_bumping(0) {
                break;
            }
        }

        // Release all frame buffers
        for i in 0..HEVC_DPB_SIZE {
            self.dpb[i].state = DPB_STATE_EMPTY;
            self.dpb[i].marking = DPB_MARKING_UNUSED;
            self.dpb[i].pic_buf_idx = None;
        }
    }

    /// Return the number of pictures currently in the DPB.
    /// Corresponds to C++ `dpb_fullness`.
    pub fn dpb_fullness(&self) -> i32 {
        self.dpb
            .iter()
            .filter(|e| e.state == DPB_STATE_IN_USE)
            .count() as i32
    }

    /// Return the number of pictures in the DPB that need output.
    /// Corresponds to C++ `dpb_reordering_delay`.
    pub fn dpb_reordering_delay(&self) -> i32 {
        self.dpb
            .iter()
            .filter(|e| {
                e.layer_id == self.nuh_layer_id
                    && e.state == DPB_STATE_IN_USE
                    && e.output != 0
            })
            .count() as i32
    }

    /// Returns `true` if the DPB is empty.
    pub fn dpb_empty(&self) -> bool {
        self.dpb_fullness() == 0
    }

    /// DPB bumping process: output the picture with the smallest POC.
    /// Corresponds to C++ `dpb_bumping`.
    ///
    /// Returns `true` if a picture was bumped, `false` if nothing could be done.
    pub fn dpb_bumping(&mut self, max_allowed_dpb_size: i32) -> bool {
        let mut i_min: i32 = -1;
        let mut i_min2: i32 = -1;
        let mut poc_min = 0i32;

        for i in 0..HEVC_DPB_SIZE {
            if self.dpb[i].state == DPB_STATE_IN_USE {
                if self.dpb[i].output != 0
                    && (i_min < 0
                        || self.dpb[i].pic_order_cnt_val < poc_min
                        || (self.dpb[i].pic_order_cnt_val == poc_min
                            && self.dpb[i].layer_id < self.dpb[i_min as usize].layer_id))
                {
                    poc_min = self.dpb[i].pic_order_cnt_val;
                    i_min = i as i32;
                } else if i_min2 < 0
                    || self.dpb[i].pic_order_cnt_val
                        < self.dpb[i_min2 as usize].pic_order_cnt_val
                {
                    i_min2 = i as i32;
                }
            }
        }

        if i_min < 0 {
            i_min = i_min2;
            // Allow exceeding DPB size up to max_allowed_dpb_size - 1
            if self.dpb_fullness() < max_allowed_dpb_size {
                return false;
            }
            if i_min < 0 {
                return false;
            }
            self.dpb[i_min as usize].marking = DPB_MARKING_UNUSED;
            tracing::warn!("DPB overflow");
        }

        if self.dpb[i_min as usize].output != 0 {
            self.output_picture(i_min as usize);
            self.dpb[i_min as usize].output = 0;
        }

        if self.dpb[i_min as usize].marking == DPB_MARKING_UNUSED {
            self.dpb[i_min as usize].state = DPB_STATE_EMPTY;
            self.dpb[i_min as usize].pic_buf_idx = None;
        }
        true
    }

    /// Output a picture from the DPB. Corresponds to C++ `output_picture`.
    ///
    /// Divergence from C++: the C++ version calls `display_picture` on a
    /// `VkPicIf*`. Here we simply log the output; the actual display callback
    /// would be provided by the integration layer.
    pub fn output_picture(&self, nframe: usize) {
        if self.dpb[nframe].pic_buf_idx.is_some() {
            tracing::debug!(
                "Output picture DPB[{}] POC={}",
                nframe,
                self.dpb[nframe].pic_order_cnt_val
            );
        }
    }

    /// Start a new picture in the DPB. Corresponds to C++ `dpb_picture_start`.
    pub fn dpb_picture_start(&mut self, pps: &HevcPicParam, slh: &HevcSliceHeader) {
        let nuh_layer_id = self.nuh_layer_id as usize;
        if nuh_layer_id < MAX_VPS_LAYERS {
            self.active_pps[nuh_layer_id] = Some(Box::new(pps.clone()));
        }
        self.picture_started = true;
        self.num_delta_pocs_of_ref_rps_idx = 0;

        if slh.strps.inter_ref_pic_set_prediction_flag != 0 {
            if let Some(ref active_sps) = self.active_sps[nuh_layer_id] {
                let r_idx = active_sps.num_short_term_ref_pic_sets as i32
                    - (slh.strps.delta_idx_minus1 as i32 + 1);
                if r_idx >= 0 && (r_idx as usize) < active_sps.strpss.len() {
                    self.num_delta_pocs_of_ref_rps_idx =
                        active_sps.strpss[r_idx as usize].num_negative_pics as i32
                            + active_sps.strpss[r_idx as usize].num_positive_pics as i32;
                }
            }
        }

        let is_irap_pic =
            slh.nal_unit_type >= NalUnitType::BlaWLp as u8 && slh.nal_unit_type <= 23;

        let pic_order_cnt_val = self.picture_order_count(slh);
        self.reference_picture_set(slh, pic_order_cnt_val);

        let pic_output_flag = if ((slh.nal_unit_type == NalUnitType::RaslN as u8)
            || (slh.nal_unit_type == NalUnitType::RaslR as u8))
            && self.no_rasl_output_flag
        {
            0
        } else {
            slh.pic_output_flag as i32
        };

        if is_irap_pic && self.no_rasl_output_flag {
            let no_output_of_prior_pics = if slh.nal_unit_type == NalUnitType::CraNut as u8 {
                true
            } else {
                slh.no_output_of_prior_pics_flag != 0
            };
            if no_output_of_prior_pics {
                for i in 0..HEVC_DPB_SIZE {
                    if self.dpb[i].layer_id == self.nuh_layer_id {
                        self.dpb[i].state = DPB_STATE_EMPTY;
                        self.dpb[i].marking = DPB_MARKING_UNUSED;
                        self.dpb[i].output = 0;
                    }
                }
            }
        }

        // Remove entries no longer needed
        for i in 0..HEVC_DPB_SIZE {
            if self.dpb[i].marking == DPB_MARKING_UNUSED && self.dpb[i].output == 0 {
                self.dpb[i].state = DPB_STATE_EMPTY;
                self.dpb[i].pic_buf_idx = None;
            }
        }

        // Make room in DPB
        let mut dpb_size =
            self.max_dec_pic_buffering.min(self.max_dpb_size);
        if dpb_size <= 0 {
            dpb_size = 1;
        }
        if dpb_size > HEVC_DPB_SIZE as i32 {
            dpb_size = HEVC_DPB_SIZE as i32;
        }
        let fullness_before = self.dpb_fullness();
        tracing::debug!(
            poc = pic_order_cnt_val,
            fullness = fullness_before,
            dpb_size,
            max_dec = self.max_dec_pic_buffering,
            max_dpb = self.max_dpb_size,
            "dpb_picture_start DPB check"
        );
        let mut bumped = false;
        while self.dpb_fullness() >= dpb_size {
            if !self.dpb_bumping(self.max_dpb_size - 1) {
                break;
            }
            bumped = true;
        }

        // Select a free DPB slot
        let mut i_cur = 0usize;
        let mut found_free = false;
        for i in 0..HEVC_DPB_SIZE {
            if self.dpb[i].state == DPB_STATE_EMPTY {
                i_cur = i;
                found_free = true;
                break;
            }
        }

        if bumped || fullness_before >= dpb_size {
            tracing::debug!(
                poc = pic_order_cnt_val,
                fullness_before,
                fullness_after = self.dpb_fullness(),
                dpb_size,
                bumped,
                i_cur,
                found_free,
                "DPB bumping at dpb_picture_start"
            );
        }

        // Initialize the DPB entry
        self.dpb[i_cur].pic_order_cnt_val = pic_order_cnt_val;
        self.dpb[i_cur].layer_id = self.nuh_layer_id;
        self.dpb[i_cur].output = pic_output_flag;
        if self.dpb[i_cur].pic_buf_idx.is_none() {
            self.dpb[i_cur].pic_buf_idx = Some(i_cur);
        }
        self.dpb_cur = Some(i_cur);
        self.current_dpb_id = i_cur as i8;
    }

    /// End of picture processing. Corresponds to C++ `dpb_picture_end`.
    pub fn dpb_picture_end(&mut self) {
        let cur_idx = match self.dpb_cur {
            Some(idx) => idx,
            None => return,
        };
        if !self.picture_started {
            return;
        }
        self.picture_started = false;

        self.dpb[cur_idx].state = DPB_STATE_IN_USE;
        self.dpb[cur_idx].marking = DPB_MARKING_SHORT_TERM;

        let nuh_layer_id = self.nuh_layer_id as usize;
        if nuh_layer_id < MAX_VPS_LAYERS {
            if let Some(ref active_sps) = self.active_sps[nuh_layer_id] {
                let max_reorder = active_sps.max_num_reorder_pics as i32;
                while self.dpb_reordering_delay() > max_reorder {
                    if !self.dpb_bumping(self.max_dpb_size - 1) {
                        break;
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // POC calculation — 8.3.1 Decoding process for picture order count
    // -----------------------------------------------------------------------

    /// Calculate the picture order count for the current picture.
    /// Corresponds to C++ `picture_order_count`.
    pub fn picture_order_count(&mut self, slh: &HevcSliceHeader) -> i32 {
        let nuh_layer_id = self.nuh_layer_id as usize;
        let sps = match &self.active_sps[nuh_layer_id.min(MAX_VPS_LAYERS - 1)] {
            Some(sps) => sps,
            None => return 0,
        };

        let is_irap_pic =
            slh.nal_unit_type >= NalUnitType::BlaWLp as u8 && slh.nal_unit_type <= 23;

        let pic_order_cnt_msb;
        if is_irap_pic && self.no_rasl_output_flag {
            pic_order_cnt_msb = 0;
        } else {
            let max_pic_order_cnt_lsb =
                1i32 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);

            if (slh.pic_order_cnt_lsb as i32) < self.prev_pic_order_cnt_lsb
                && (self.prev_pic_order_cnt_lsb - slh.pic_order_cnt_lsb as i32)
                    >= (max_pic_order_cnt_lsb / 2)
            {
                pic_order_cnt_msb = self.prev_pic_order_cnt_msb + max_pic_order_cnt_lsb;
            } else if (slh.pic_order_cnt_lsb as i32) > self.prev_pic_order_cnt_lsb
                && ((slh.pic_order_cnt_lsb as i32) - self.prev_pic_order_cnt_lsb)
                    > (max_pic_order_cnt_lsb / 2)
            {
                pic_order_cnt_msb = self.prev_pic_order_cnt_msb - max_pic_order_cnt_lsb;
            } else {
                pic_order_cnt_msb = self.prev_pic_order_cnt_msb;
            }
        }

        let pic_order_cnt_val = pic_order_cnt_msb + slh.pic_order_cnt_lsb as i32;

        let temporal_id = slh.nuh_temporal_id_plus1 as i32 - 1;
        let is_sub_layer_non_ref = matches!(
            slh.nal_unit_type,
            0 | 2 | 4 | 6 | 8 | 10 | 12 | 14 // all even values < 16
        );

        if temporal_id == 0
            && !(slh.nal_unit_type >= NalUnitType::RadlN as u8
                && slh.nal_unit_type <= NalUnitType::RaslR as u8)
            && !is_sub_layer_non_ref
        {
            self.prev_pic_order_cnt_lsb = slh.pic_order_cnt_lsb as i32;
            self.prev_pic_order_cnt_msb = pic_order_cnt_msb;
        }

        pic_order_cnt_val
    }

    // -----------------------------------------------------------------------
    // Reference picture set — 8.3.2
    // -----------------------------------------------------------------------

    /// Decoding process for reference picture set.
    /// Corresponds to C++ `reference_picture_set`.
    pub fn reference_picture_set(&mut self, slh: &HevcSliceHeader, pic_order_cnt_val: i32) {
        let mut poc_st_curr_before = [0i32; 16];
        let mut poc_st_curr_after = [0i32; 16];
        let mut poc_st_foll = [0i32; 16];
        let mut poc_lt_curr = [0i32; 16];
        let mut poc_lt_foll = [0i32; 16];
        let mut curr_delta_poc_msb_present_flag = [0i32; 16];
        let mut foll_delta_poc_msb_present_flag = [0i32; 16];

        let nuh_layer_id = self.nuh_layer_id as usize;
        let sps = match &self.active_sps[nuh_layer_id.min(MAX_VPS_LAYERS - 1)] {
            Some(s) => s.clone(),
            None => return,
        };

        let max_pic_order_cnt_lsb = 1i32 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
        let is_irap_pic =
            slh.nal_unit_type >= NalUnitType::BlaWLp as u8 && slh.nal_unit_type <= 23;

        if is_irap_pic && self.no_rasl_output_flag {
            for i in 0..HEVC_DPB_SIZE {
                if self.dpb[i].layer_id == self.nuh_layer_id {
                    self.dpb[i].marking = DPB_MARKING_UNUSED;
                }
            }
        }

        let num_poc_st_curr_before;
        let num_poc_st_curr_after;
        let num_poc_st_foll;
        let num_poc_lt_curr;
        let num_poc_lt_foll;

        if slh.nal_unit_type == NalUnitType::IdrWRadl as u8
            || slh.nal_unit_type == NalUnitType::IdrNLp as u8
        {
            // IDR picture
            num_poc_st_curr_before = 0;
            num_poc_st_curr_after = 0;
            num_poc_st_foll = 0;
            num_poc_lt_curr = 0;
            num_poc_lt_foll = 0;
        } else {
            let strps = if slh.short_term_ref_pic_set_sps_flag == 0 {
                &slh.strps
            } else {
                let idx = slh.short_term_ref_pic_set_idx as usize;
                if idx < sps.strpss.len() {
                    &sps.strpss[idx]
                } else {
                    return;
                }
            };

            let mut j = 0i32;
            let mut k = 0i32;
            for i in 0..strps.num_negative_pics as usize {
                if strps.used_by_curr_pic_s0[i] != 0 {
                    poc_st_curr_before[j as usize] =
                        pic_order_cnt_val + strps.delta_poc_s0[i];
                    j += 1;
                } else {
                    poc_st_foll[k as usize] = pic_order_cnt_val + strps.delta_poc_s0[i];
                    k += 1;
                }
            }
            num_poc_st_curr_before = j;

            j = 0;
            for i in 0..strps.num_positive_pics as usize {
                if strps.used_by_curr_pic_s1[i] != 0 {
                    poc_st_curr_after[j as usize] =
                        pic_order_cnt_val + strps.delta_poc_s1[i];
                    j += 1;
                } else {
                    poc_st_foll[k as usize] = pic_order_cnt_val + strps.delta_poc_s1[i];
                    k += 1;
                }
            }
            num_poc_st_curr_after = j;
            num_poc_st_foll = k;

            // Long-term references
            let mut poc_lsb_lt = [0i32; 16];
            let mut used_by_curr_pic_lt = [false; 16];
            let mut delta_poc_msb_cycle_lt = [0i32; 16];

            let lt_count = slh.num_long_term_sps as usize + slh.num_long_term_pics as usize;
            for i in 0..lt_count {
                if i < slh.num_long_term_sps as usize {
                    poc_lsb_lt[i] =
                        sps.long_term_ref_pics_sps.lt_ref_pic_poc_lsb_sps
                            [slh.lt_idx_sps[i] as usize] as i32;
                    used_by_curr_pic_lt[i] = (sps
                        .long_term_ref_pics_sps
                        .used_by_curr_pic_lt_sps_flag
                        >> slh.lt_idx_sps[i])
                        & 1
                        != 0;
                } else {
                    poc_lsb_lt[i] = slh.poc_lsb_lt[i] as i32;
                    used_by_curr_pic_lt[i] = (slh.used_by_curr_pic_lt_flags >> i) & 1 != 0;
                }
                if i == 0 || i == slh.num_long_term_sps as usize {
                    delta_poc_msb_cycle_lt[i] = slh.delta_poc_msb_cycle_lt[i];
                } else {
                    delta_poc_msb_cycle_lt[i] =
                        slh.delta_poc_msb_cycle_lt[i] + delta_poc_msb_cycle_lt[i - 1];
                }
            }

            j = 0;
            k = 0;
            for i in 0..lt_count {
                let mut poc_lt = poc_lsb_lt[i];
                if slh.delta_poc_msb_present_flags & (1 << i) != 0 {
                    poc_lt += pic_order_cnt_val
                        - delta_poc_msb_cycle_lt[i] * max_pic_order_cnt_lsb
                        - slh.pic_order_cnt_lsb as i32;
                }
                if used_by_curr_pic_lt[i] {
                    poc_lt_curr[j as usize] = poc_lt;
                    curr_delta_poc_msb_present_flag[j as usize] =
                        ((slh.delta_poc_msb_present_flags >> i) & 1) as i32;
                    j += 1;
                } else {
                    poc_lt_foll[k as usize] = poc_lt;
                    foll_delta_poc_msb_present_flag[k as usize] =
                        ((slh.delta_poc_msb_present_flags >> i) & 1) as i32;
                    k += 1;
                }
            }
            num_poc_lt_curr = j;
            num_poc_lt_foll = k;
        }

        let mut ref_pic_set_st_foll = [-1i8; 16];
        let mut ref_pic_set_lt_foll = [-1i8; 16];

        // Reset all ref pic set entries
        for i in 0..16 {
            self.ref_pic_set_st_curr_before[i] = -1;
            self.ref_pic_set_st_curr_after[i] = -1;
            ref_pic_set_st_foll[i] = -1;
            self.ref_pic_set_lt_curr[i] = -1;
            ref_pic_set_lt_foll[i] = -1;
            self.ref_pic_set_inter_layer_0[i] = -1;
            self.ref_pic_set_inter_layer_1[i] = -1;
        }

        self.num_poc_st_curr_before = num_poc_st_curr_before;
        self.num_poc_st_curr_after = num_poc_st_curr_after;
        self.num_poc_lt_curr = num_poc_lt_curr;

        // Find long-term current references in DPB
        for i in 0..num_poc_lt_curr as usize {
            let mask = if curr_delta_poc_msb_present_flag[i] == 0 {
                max_pic_order_cnt_lsb - 1
            } else {
                !0i32
            };
            for j in 0..HEVC_DPB_SIZE {
                if self.dpb[j].layer_id == self.nuh_layer_id
                    && self.dpb[j].state == DPB_STATE_IN_USE
                    && self.dpb[j].marking != DPB_MARKING_UNUSED
                    && (self.dpb[j].pic_order_cnt_val & mask) == poc_lt_curr[i]
                {
                    self.ref_pic_set_lt_curr[i] = j as i8;
                    break;
                }
            }
            if self.ref_pic_set_lt_curr[i] < 0 {
                tracing::warn!(
                    "long-term reference picture not available (POC={})",
                    poc_lt_curr[i]
                );
            }
        }

        // Find long-term follow references
        for i in 0..num_poc_lt_foll as usize {
            let mask = if foll_delta_poc_msb_present_flag[i] == 0 {
                max_pic_order_cnt_lsb - 1
            } else {
                !0i32
            };
            for j in 0..HEVC_DPB_SIZE {
                if self.dpb[j].layer_id == self.nuh_layer_id
                    && self.dpb[j].state == DPB_STATE_IN_USE
                    && self.dpb[j].marking != DPB_MARKING_UNUSED
                    && (self.dpb[j].pic_order_cnt_val & mask) == poc_lt_foll[i]
                {
                    ref_pic_set_lt_foll[i] = j as i8;
                    break;
                }
            }
        }

        // Mark long-term references
        for i in 0..num_poc_lt_curr as usize {
            if self.ref_pic_set_lt_curr[i] >= 0 {
                self.dpb[self.ref_pic_set_lt_curr[i] as usize].marking = DPB_MARKING_LONG_TERM;
            }
        }
        for i in 0..num_poc_lt_foll as usize {
            if ref_pic_set_lt_foll[i] >= 0 {
                self.dpb[ref_pic_set_lt_foll[i] as usize].marking = DPB_MARKING_LONG_TERM;
            }
        }

        // Find short-term current-before references
        for i in 0..num_poc_st_curr_before as usize {
            for j in 0..HEVC_DPB_SIZE {
                if self.dpb[j].layer_id == self.nuh_layer_id
                    && self.dpb[j].state == DPB_STATE_IN_USE
                    && self.dpb[j].marking == DPB_MARKING_SHORT_TERM
                    && self.dpb[j].pic_order_cnt_val == poc_st_curr_before[i]
                {
                    self.ref_pic_set_st_curr_before[i] = j as i8;
                    break;
                }
            }
            if self.ref_pic_set_st_curr_before[i] < 0 {
                tracing::warn!(
                    "short-term reference picture not available (POC={})",
                    poc_st_curr_before[i]
                );
                self.ref_pic_set_st_curr_before[i] =
                    self.create_lost_ref_pic(poc_st_curr_before[i], self.nuh_layer_id, 1);
            }
        }

        // Find short-term current-after references
        for i in 0..num_poc_st_curr_after as usize {
            for j in 0..HEVC_DPB_SIZE {
                if self.dpb[j].layer_id == self.nuh_layer_id
                    && self.dpb[j].state == DPB_STATE_IN_USE
                    && self.dpb[j].marking == DPB_MARKING_SHORT_TERM
                    && self.dpb[j].pic_order_cnt_val == poc_st_curr_after[i]
                {
                    self.ref_pic_set_st_curr_after[i] = j as i8;
                    break;
                }
            }
            if self.ref_pic_set_st_curr_after[i] < 0 {
                tracing::warn!(
                    "short-term reference picture not available (POC={})",
                    poc_st_curr_after[i]
                );
                self.ref_pic_set_st_curr_after[i] =
                    self.create_lost_ref_pic(poc_st_curr_after[i], self.nuh_layer_id, 1);
            }
        }

        // Find short-term follow references
        for i in 0..num_poc_st_foll as usize {
            for j in 0..HEVC_DPB_SIZE {
                if self.dpb[j].layer_id == self.nuh_layer_id
                    && self.dpb[j].state == DPB_STATE_IN_USE
                    && self.dpb[j].marking == DPB_MARKING_SHORT_TERM
                    && self.dpb[j].pic_order_cnt_val == poc_st_foll[i]
                {
                    ref_pic_set_st_foll[i] = j as i8;
                    break;
                }
            }
        }

        // Enhance layer (MV-HEVC)
        let mut num_active_ref_layer_pics_0 = 0i32;
        let mut num_active_ref_layer_pics_1 = 0i32;

        if self.nuh_layer_id > 0 {
            if let Some(ref vps) = self.active_vps {
                for i in 0..slh.num_active_ref_layer_pics as usize {
                    let layer_id_ref = slh.inter_layer_pred_layer_idc[i] as usize;
                    let view_id_cur = vps.view_id_val[self.nuh_layer_id as usize];
                    let view_id_zero = vps.view_id_val[0];
                    let view_id_ref = vps.view_id_val[layer_id_ref];

                    let mut found_j = None;
                    for j in 0..HEVC_DPB_SIZE {
                        if self.dpb[j].layer_id as usize == layer_id_ref
                            && self.dpb[j].state == DPB_STATE_IN_USE
                            && self.dpb[j].marking != DPB_MARKING_UNUSED
                            && self.dpb[j].pic_order_cnt_val == pic_order_cnt_val
                        {
                            found_j = Some(j);
                            break;
                        }
                    }

                    if let Some(j) = found_j {
                        if (view_id_cur <= view_id_zero && view_id_cur <= view_id_ref)
                            || (view_id_cur >= view_id_zero && view_id_cur >= view_id_ref)
                        {
                            self.ref_pic_set_inter_layer_0
                                [num_active_ref_layer_pics_0 as usize] = j as i8;
                            num_active_ref_layer_pics_0 += 1;
                        } else {
                            self.ref_pic_set_inter_layer_1
                                [num_active_ref_layer_pics_1 as usize] = j as i8;
                            num_active_ref_layer_pics_1 += 1;
                        }
                    } else {
                        tracing::warn!(
                            "InterLayer reference picture not available (POC={})",
                            pic_order_cnt_val
                        );
                    }
                }
            }
        }
        self.num_active_ref_layer_pics_0 = num_active_ref_layer_pics_0;
        self.num_active_ref_layer_pics_1 = num_active_ref_layer_pics_1;

        // Mark all pictures not in any ref pic set as "unused for reference"
        let mut in_use_mask: u32 = 0;
        for i in 0..num_poc_lt_curr as usize {
            if self.ref_pic_set_lt_curr[i] >= 0 {
                in_use_mask |= 1 << self.ref_pic_set_lt_curr[i];
            }
        }
        for i in 0..num_poc_lt_foll as usize {
            if ref_pic_set_lt_foll[i] >= 0 {
                in_use_mask |= 1 << ref_pic_set_lt_foll[i];
            }
        }
        for i in 0..num_poc_st_curr_before as usize {
            if self.ref_pic_set_st_curr_before[i] >= 0 {
                in_use_mask |= 1 << self.ref_pic_set_st_curr_before[i];
            }
        }
        for i in 0..num_poc_st_curr_after as usize {
            if self.ref_pic_set_st_curr_after[i] >= 0 {
                in_use_mask |= 1 << self.ref_pic_set_st_curr_after[i];
            }
        }
        for i in 0..num_poc_st_foll as usize {
            if ref_pic_set_st_foll[i] >= 0 {
                in_use_mask |= 1 << ref_pic_set_st_foll[i];
            }
        }

        let mut mask = in_use_mask;
        for i in 0..HEVC_DPB_SIZE {
            if self.dpb[i].layer_id == self.nuh_layer_id && (mask & 1) == 0 {
                self.dpb[i].marking = DPB_MARKING_UNUSED;
            }
            mask >>= 1;
        }
    }

    /// Create a "lost" reference picture by finding the closest POC in the DPB.
    /// Corresponds to C++ `create_lost_ref_pic`.
    pub fn create_lost_ref_pic(&self, lost_poc: i32, layer_id: i32, marking_flag: i32) -> i8 {
        let mut return_dpb_pos: i8 = -1;
        let mut closest_poc = i32::MAX;
        for i in 0..HEVC_DPB_SIZE {
            if self.dpb[i].layer_id == layer_id
                && self.dpb[i].state != DPB_STATE_EMPTY
                && self.dpb[i].marking == marking_flag
            {
                let diff = (self.dpb[i].pic_order_cnt_val - lost_poc).abs();
                if diff < closest_poc && diff != 0 {
                    closest_poc = diff;
                    return_dpb_pos = i as i8;
                }
            }
        }
        if return_dpb_pos >= 0 {
            tracing::warn!(
                "Generating reference picture {} instead of picture {}",
                self.dpb[return_dpb_pos as usize].pic_order_cnt_val,
                lost_poc
            );
        }
        return_dpb_pos
    }

    // -----------------------------------------------------------------------
    // Short-term ref pic set parsing
    // -----------------------------------------------------------------------

    /// Parse a short_term_ref_pic_set from the bitstream.
    /// Corresponds to C++ `short_term_ref_pic_set`.
    ///
    /// `reader` provides the bitstream reading methods (u, ue, se).
    /// Returns `None` on parse error.
    pub fn parse_short_term_ref_pic_set(
        reader: &mut BitstreamReader,
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
                    let d_poc =
                        rstrps.delta_poc_s1[j] + delta_rps;
                    if d_poc < 0
                        && use_delta_flag[rstrps.num_negative_pics as usize + j] != 0
                    {
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
                            [rstrps.num_negative_pics as usize
                                + rstrps.num_positive_pics as usize];
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
                            [rstrps.num_negative_pics as usize
                                + rstrps.num_positive_pics as usize];
                        if strps.used_by_curr_pic_s1[i] != 0 {
                            std_strps.used_by_curr_pic_s1_flag |= 1 << i;
                        }
                        i += 1;
                    }
                }
                for j in 0..rstrps.num_positive_pics as usize {
                    let d_poc = rstrps.delta_poc_s1[j] + delta_rps;
                    if d_poc > 0
                        && use_delta_flag[rstrps.num_negative_pics as usize + j] != 0
                    {
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
                strps.delta_poc_s0[i] = (if i == 0 {
                    0
                } else {
                    strps.delta_poc_s0[i - 1]
                }) - (delta_poc_s0_minus1[i] as i32 + 1);
                std_strps.delta_poc_s0_minus1[i] = strps.delta_poc_s0[i] as u16;
                strps.used_by_curr_pic_s0[i] = used_by_curr_pic_s0_flag[i];
                if strps.used_by_curr_pic_s0[i] != 0 {
                    std_strps.used_by_curr_pic_s0_flag |= 1 << i;
                }
            }
            for i in 0..num_positive_pics as usize {
                strps.delta_poc_s1[i] = (if i == 0 {
                    0
                } else {
                    strps.delta_poc_s1[i - 1]
                }) + (delta_poc_s1_minus1[i] as i32 + 1);
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
    pub fn parse_scaling_list_data(
        reader: &mut BitstreamReader,
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
                            tracing::warn!(
                                "Invalid scaling_list_dc_coef_minus8 ({})",
                                dc_coef
                            );
                            return None;
                        }
                        next_coef = scle.scaling_list_dc_coef_minus8 + 8;
                    }
                    for i in 0..coef_num as usize {
                        let delta_coef = reader.se()?;
                        scle.scaling_list_delta_coef[i] = delta_coef as i8;
                        if delta_coef < -128 || delta_coef > 127 {
                            tracing::warn!(
                                "Invalid scaling_list_delta_coef ({})",
                                delta_coef
                            );
                            return None;
                        }
                        next_coef = (next_coef + delta_coef) & 0xff;
                        if next_coef == 0 {
                            tracing::warn!(
                                "Invalid scaling_list_delta_coef: zero ScalingList entry"
                            );
                            return None;
                        }
                    }
                }
            }
        }
        Some(())
    }

    // -----------------------------------------------------------------------
    // VPS parsing — ported from C++ video_parameter_set_rbsp()
    // -----------------------------------------------------------------------

    /// Parse an H.265 Video Parameter Set from RBSP data (after EPB removal,
    /// after the 2-byte NAL header). Returns `None` on parse error.
    pub fn parse_vps(reader: &mut BitstreamReader) -> Option<HevcVideoParam> {
        let mut vps = HevcVideoParam::default();

        vps.vps_video_parameter_set_id = reader.u(4)?;
        let _base_layer_internal_flag = reader.u(1)?;
        let _base_layer_available_flag = reader.u(1)?;
        vps.vps_max_layers_minus1 = reader.u(6)?;
        vps.vps_max_sub_layers_minus1 = reader.u(3)?;
        if vps.vps_max_sub_layers_minus1 as usize >= MAX_NUM_SUB_LAYERS {
            return None;
        }
        vps.base_flags.vps_temporal_id_nesting_flag =
            reader.u(1)? != 0;
        let _reserved = reader.u(16)?; // reserved_0xffff_16bits

        vps.profile_tier_level = Self::parse_profile_tier_level(
            reader,
            vps.vps_max_sub_layers_minus1 as u8,
        )?;

        let sub_layer_ordering_present = reader.u(1)? != 0;
        vps.base_flags.vps_sub_layer_ordering_info_present_flag = sub_layer_ordering_present;
        let start = if sub_layer_ordering_present {
            0
        } else {
            vps.vps_max_sub_layers_minus1 as usize
        };
        for i in start..=vps.vps_max_sub_layers_minus1 as usize {
            vps.dec_pic_buf_mgr.max_dec_pic_buffering_minus1[i] = reader.ue()? as u8;
            vps.dec_pic_buf_mgr.max_num_reorder_pics[i] = reader.ue()? as u8;
            vps.dec_pic_buf_mgr.max_latency_increase_plus1[i] = reader.ue()? as u8;
        }

        vps.vps_max_layer_id = reader.u(6)?;
        vps.vps_num_layer_sets = reader.ue()? + 1;
        // Skip layer_id_included_flag for layer sets > 0
        for _ in 1..vps.vps_num_layer_sets {
            for _ in 0..=vps.vps_max_layer_id {
                reader.u(1)?; // layer_id_included_flag
            }
        }

        let timing_info_present = reader.u(1)? != 0;
        if timing_info_present {
            vps.base_flags.vps_timing_info_present_flag = true;
            vps.vps_num_units_in_tick = reader.u(32)?;
            vps.vps_time_scale = reader.u(32)?;
            let poc_proportional_to_timing = reader.u(1)? != 0;
            vps.base_flags.vps_poc_proportional_to_timing_flag = poc_proportional_to_timing;
            if poc_proportional_to_timing {
                vps.vps_num_ticks_poc_diff_one_minus1 = reader.ue()?;
            }
            // Skip HRD parameters
            vps.vps_num_hrd_parameters = reader.ue()?;
            // We don't parse HRD params — not needed for decode
        }

        Some(vps)
    }

    // -----------------------------------------------------------------------
    // SPS parsing — ported from C++ seq_parameter_set_rbsp()
    // -----------------------------------------------------------------------

    /// Parse an H.265 Sequence Parameter Set from RBSP data (after EPB removal,
    /// after the 2-byte NAL header). Returns `None` on parse error.
    ///
    /// Ported from C++ `VulkanH265Decoder::seq_parameter_set_rbsp()`.
    pub fn parse_sps(reader: &mut BitstreamReader) -> Option<HevcSeqParam> {
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
        sps.profile_tier_level =
            Self::parse_profile_tier_level(reader, sps.sps_max_sub_layers_minus1)?;

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
            if sps.dec_pic_buf_mgr.max_dec_pic_buffering_minus1[i] + 1 > sps.max_dec_pic_buffering
            {
                sps.max_dec_pic_buffering =
                    sps.dec_pic_buf_mgr.max_dec_pic_buffering_minus1[i] + 1;
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
                Self::parse_scaling_list_data(reader, &mut sps.scaling_lists)?;
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
            Self::parse_short_term_ref_pic_set(
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

        // VUI parameters — skip for now (not needed for decode correctness)
        sps.flags.vui_parameters_present_flag = reader.u(1)? != 0;
        // VUI parsing omitted; only needed for display timing, not decode

        // SPS extensions — skip
        // (not needed for basic decode correctness)

        Some(sps)
    }

    /// Parse profile_tier_level() from the bitstream.
    /// Returns a simplified ProfileTierLevel with profile_idc and level_idc.
    fn parse_profile_tier_level(
        reader: &mut BitstreamReader,
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

    // -----------------------------------------------------------------------
    // PPS parsing — ported from C++ pic_parameter_set_rbsp()
    // -----------------------------------------------------------------------

    /// Parse an H.265 Picture Parameter Set from RBSP data (after EPB removal,
    /// after the 2-byte NAL header). Returns `None` on parse error.
    ///
    /// `spss` provides the SPS store for resolving the referenced SPS.
    ///
    /// Ported from C++ `VulkanH265Decoder::pic_parameter_set_rbsp()`.
    pub fn parse_pps(
        reader: &mut BitstreamReader,
        spss: &[Option<Box<HevcSeqParam>>],
    ) -> Option<HevcPicParam> {
        let mut pps = HevcPicParam::default();

        let pic_parameter_set_id = reader.ue()?;
        let seq_parameter_set_id = reader.ue()?;
        if pic_parameter_set_id as usize >= MAX_NUM_PPS
            || seq_parameter_set_id as usize >= MAX_NUM_SPS
        {
            return None;
        }
        pps.pps_pic_parameter_set_id = pic_parameter_set_id as u8;
        pps.pps_seq_parameter_set_id = seq_parameter_set_id as u8;

        let sps = spss
            .get(seq_parameter_set_id as usize)
            .and_then(|s| s.as_ref());
        pps.sps_video_parameter_set_id = sps.map_or(0, |s| s.sps_video_parameter_set_id);

        pps.flags.dependent_slice_segments_enabled_flag = reader.u(1)? != 0;
        pps.flags.output_flag_present_flag = reader.u(1)? != 0;
        pps.num_extra_slice_header_bits = reader.u(3)? as u8;
        pps.flags.sign_data_hiding_enabled_flag = reader.u(1)? != 0;
        pps.flags.cabac_init_present_flag = reader.u(1)? != 0;

        let l0 = reader.ue()?;
        let l1 = reader.ue()?;
        if l0 > 15 || l1 > 15 {
            return None;
        }
        pps.num_ref_idx_l0_default_active_minus1 = l0 as u8;
        pps.num_ref_idx_l1_default_active_minus1 = l1 as u8;

        pps.init_qp_minus26 = reader.se()? as i8;
        let qp_bd_offset_y = sps.map_or(0i32, |s| 6 * s.bit_depth_luma_minus8 as i32);
        if (pps.init_qp_minus26 as i32) < -(26 + qp_bd_offset_y)
            || pps.init_qp_minus26 as i32 > 25
        {
            tracing::warn!("Invalid init_qp_minus26: {}", pps.init_qp_minus26);
            return None;
        }

        pps.flags.constrained_intra_pred_flag = reader.u(1)? != 0;
        pps.flags.transform_skip_enabled_flag = reader.u(1)? != 0;
        pps.flags.cu_qp_delta_enabled_flag = reader.u(1)? != 0;
        if pps.flags.cu_qp_delta_enabled_flag {
            pps.diff_cu_qp_delta_depth = reader.ue()? as u8;
        }

        pps.pps_cb_qp_offset = reader.se()? as i8;
        pps.pps_cr_qp_offset = reader.se()? as i8;

        pps.flags.pps_slice_chroma_qp_offsets_present_flag = reader.u(1)? != 0;
        pps.flags.weighted_pred_flag = reader.u(1)? != 0;
        pps.flags.weighted_bipred_flag = reader.u(1)? != 0;
        pps.flags.transquant_bypass_enabled_flag = reader.u(1)? != 0;
        pps.flags.tiles_enabled_flag = reader.u(1)? != 0;
        pps.flags.entropy_coding_sync_enabled_flag = reader.u(1)? != 0;

        if pps.flags.tiles_enabled_flag {
            let cols = reader.ue()?;
            let rows = reader.ue()?;
            if cols as usize >= MAX_NUM_TILE_COLUMNS || rows as usize >= MAX_NUM_TILE_ROWS {
                return None;
            }
            pps.num_tile_columns_minus1 = cols as u8;
            pps.num_tile_rows_minus1 = rows as u8;
            pps.flags.uniform_spacing_flag = reader.u(1)? != 0;
            if !pps.flags.uniform_spacing_flag {
                for i in 0..cols as usize {
                    pps.column_width_minus1[i] = reader.ue()? as u16;
                }
                for i in 0..rows as usize {
                    pps.row_height_minus1[i] = reader.ue()? as u16;
                }
            }
            pps.flags.loop_filter_across_tiles_enabled_flag = reader.u(1)? != 0;
        }

        pps.flags.pps_loop_filter_across_slices_enabled_flag = reader.u(1)? != 0;
        pps.flags.deblocking_filter_control_present_flag = reader.u(1)? != 0;
        if pps.flags.deblocking_filter_control_present_flag {
            pps.flags.deblocking_filter_override_enabled_flag = reader.u(1)? != 0;
            pps.flags.pps_deblocking_filter_disabled_flag = reader.u(1)? != 0;
            if !pps.flags.pps_deblocking_filter_disabled_flag {
                pps.pps_beta_offset_div2 = reader.se()? as i8;
                pps.pps_tc_offset_div2 = reader.se()? as i8;
            }
        }

        pps.flags.pps_scaling_list_data_present_flag = reader.u(1)? != 0;
        if pps.flags.pps_scaling_list_data_present_flag {
            Self::parse_scaling_list_data(reader, &mut pps.pps_scaling_list)?;
        }

        pps.flags.lists_modification_present_flag = reader.u(1)? != 0;
        pps.log2_parallel_merge_level_minus2 = reader.ue()? as u8;
        pps.flags.slice_segment_header_extension_present_flag = reader.u(1)? != 0;

        // PPS extensions — skip for basic decode correctness
        pps.flags.pps_extension_present_flag = reader.u(1)? != 0;
        if pps.flags.pps_extension_present_flag {
            pps.flags.pps_range_extension_flag = reader.u(1)? != 0;
            let _multilayer = reader.u(1)?;
            let _ext_6bits = reader.u(6)?;
            if pps.flags.pps_range_extension_flag {
                if pps.flags.transform_skip_enabled_flag {
                    pps.log2_max_transform_skip_block_size_minus2 = reader.ue()? as u8;
                }
                pps.flags.cross_component_prediction_enabled_flag = reader.u(1)? != 0;
                pps.flags.chroma_qp_offset_list_enabled_flag = reader.u(1)? != 0;
                if pps.flags.chroma_qp_offset_list_enabled_flag {
                    pps.diff_cu_chroma_qp_offset_depth = reader.ue()? as u8;
                    pps.chroma_qp_offset_list_len_minus1 = reader.ue()? as u8;
                    if pps.chroma_qp_offset_list_len_minus1 > 5 {
                        return None;
                    }
                    for i in 0..=pps.chroma_qp_offset_list_len_minus1 as usize {
                        pps.cb_qp_offset_list[i] = reader.se()? as i8;
                        pps.cr_qp_offset_list[i] = reader.se()? as i8;
                    }
                }
                pps.log2_sao_offset_scale_luma = reader.ue()? as u8;
                pps.log2_sao_offset_scale_chroma = reader.ue()? as u8;
            }
        }

        Some(pps)
    }

    // -----------------------------------------------------------------------
    // Slice header parsing — ported from C++ slice_header()
    // -----------------------------------------------------------------------

    /// Parse an H.265 slice header from RBSP data (after EPB removal, after
    /// the 2-byte NAL header). Returns the parsed `HevcSliceHeader` or `None`
    /// on error.
    ///
    /// Ported from C++ `VulkanH265Decoder::slice_header()`.
    pub fn parse_slice_header(
        reader: &mut BitstreamReader,
        nal_unit_type: u8,
        nuh_temporal_id_plus1: u8,
        sps: &HevcSeqParam,
        pps: &HevcPicParam,
    ) -> Option<HevcSliceHeader> {
        let rap_pic_flag = nal_unit_type >= NalUnitType::BlaWLp as u8 && nal_unit_type <= 23;
        let idr_pic_flag =
            nal_unit_type == NalUnitType::IdrWRadl as u8
                || nal_unit_type == NalUnitType::IdrNLp as u8;

        let mut slh = HevcSliceHeader::default();
        slh.nal_unit_type = nal_unit_type;
        slh.nuh_temporal_id_plus1 = nuh_temporal_id_plus1;

        slh.first_slice_segment_in_pic_flag = reader.u(1)? as u8;
        if rap_pic_flag {
            slh.no_output_of_prior_pics_flag = reader.u(1)? as u8;
        }

        let pps_id = reader.ue()?;
        slh.pic_parameter_set_id = pps_id as u8;
        if pps_id as usize >= MAX_NUM_PPS {
            return None;
        }

        let log2_ctb_size_y = sps.log2_min_luma_coding_block_size_minus3 as u32
            + 3
            + sps.log2_diff_max_min_luma_coding_block_size as u32;
        let pic_width_in_ctbs_y =
            (sps.pic_width_in_luma_samples + (1 << log2_ctb_size_y) - 1) >> log2_ctb_size_y;
        let pic_height_in_ctbs_y =
            (sps.pic_height_in_luma_samples + (1 << log2_ctb_size_y) - 1) >> log2_ctb_size_y;
        let pic_size_in_ctbs_y = pic_width_in_ctbs_y * pic_height_in_ctbs_y;

        let mut dependent_slice_segment_flag = false;
        if slh.first_slice_segment_in_pic_flag == 0 {
            if pps.flags.dependent_slice_segments_enabled_flag {
                dependent_slice_segment_flag = reader.u(1)? != 0;
            }
            slh.slice_segment_address = reader.u(ceil_log2(pic_size_in_ctbs_y as i32))?;
        }

        if !dependent_slice_segment_flag {
            if pps.num_extra_slice_header_bits > 0 {
                reader.u(pps.num_extra_slice_header_bits as u32)?;
            }

            let slice_type = reader.ue()?;
            if slice_type > 2 {
                return None;
            }
            slh.slice_type = slice_type as u8;

            if pps.flags.output_flag_present_flag {
                slh.pic_output_flag = reader.u(1)? as u8;
            }
            if sps.flags.separate_colour_plane_flag {
                slh.colour_plane_id = reader.u(2)? as u8;
            }

            if !idr_pic_flag {
                slh.pic_order_cnt_lsb =
                    reader.u(sps.log2_max_pic_order_cnt_lsb_minus4 as u32 + 4)? as u16;

                slh.short_term_ref_pic_set_sps_flag = reader.u(1)? as u8;
                if slh.short_term_ref_pic_set_sps_flag == 0 {
                    // STRPS signaled in slice header
                    let bitcnt_before = reader.consumed_bits();
                    let mut std_strps = StdShortTermRefPicSet::default();
                    // Clone existing SPS STRPS for the all_strpss parameter
                    let all_strpss = sps.strpss.clone();
                    Self::parse_short_term_ref_pic_set(
                        reader,
                        &mut std_strps,
                        &mut slh.strps,
                        &all_strpss,
                        sps.num_short_term_ref_pic_sets as usize,
                        sps.num_short_term_ref_pic_sets as usize,
                    )?;
                    slh.num_bits_for_short_term_rps_in_slice =
                        (reader.consumed_bits() - bitcnt_before) as u32;
                } else {
                    if sps.num_short_term_ref_pic_sets > 1 {
                        let bits = ceil_log2(sps.num_short_term_ref_pic_sets as i32);
                        slh.short_term_ref_pic_set_idx = reader.u(bits)? as u8;
                    }
                    if slh.short_term_ref_pic_set_idx >= sps.num_short_term_ref_pic_sets {
                        tracing::warn!(
                            "Invalid short_term_ref_pic_set_idx: {}/{}",
                            slh.short_term_ref_pic_set_idx,
                            sps.num_short_term_ref_pic_sets
                        );
                        return None;
                    }
                }

                // Long-term reference pictures
                if sps.flags.long_term_ref_pics_present_flag {
                    if sps.num_long_term_ref_pics_sps > 0 {
                        slh.num_long_term_sps = reader.ue()? as u8;
                    }
                    slh.num_long_term_pics = reader.ue()? as u8;
                    let lt_count =
                        slh.num_long_term_sps as usize + slh.num_long_term_pics as usize;
                    if lt_count > MAX_NUM_REF_PICS {
                        return None;
                    }
                    for i in 0..lt_count {
                        if i < slh.num_long_term_sps as usize {
                            if sps.num_long_term_ref_pics_sps > 1 {
                                let bits =
                                    ceil_log2(sps.num_long_term_ref_pics_sps as i32);
                                slh.lt_idx_sps[i] = reader.u(bits)? as u8;
                            }
                        } else {
                            slh.poc_lsb_lt[i] = reader
                                .u(sps.log2_max_pic_order_cnt_lsb_minus4 as u32 + 4)?
                                as u16;
                            if reader.u(1)? != 0 {
                                // used_by_curr_pic_lt_flag
                                slh.used_by_curr_pic_lt_flags |= 1 << i;
                            }
                        }
                        if reader.u(1)? != 0 {
                            // delta_poc_msb_present_flag
                            slh.delta_poc_msb_present_flags |= 1 << i;
                            slh.delta_poc_msb_cycle_lt[i] = reader.ue()? as i32;
                        }
                    }
                }

                if sps.flags.sps_temporal_mvp_enabled_flag {
                    slh.slice_temporal_mvp_enabled_flag = reader.u(1)? as u8;
                }
            }

            // SAO flags
            if sps.flags.sample_adaptive_offset_enabled_flag {
                reader.u(1)?; // slice_sao_luma_flag
                reader.u(1)?; // slice_sao_chroma_flag
            }

            // Reference list override
            if slh.slice_type == 1 || slh.slice_type == 0 {
                // P or B
                if reader.u(1)? != 0 {
                    // num_ref_idx_active_override_flag
                    slh.num_ref_idx_l0_active_minus1 = reader.ue()? as u8;
                    if slh.slice_type == 0 {
                        // B
                        slh.num_ref_idx_l1_active_minus1 = reader.ue()? as u8;
                    }
                } else {
                    slh.num_ref_idx_l0_active_minus1 = pps.num_ref_idx_l0_default_active_minus1;
                    slh.num_ref_idx_l1_active_minus1 = pps.num_ref_idx_l1_default_active_minus1;
                }
                if slh.slice_type != 0 {
                    // not B
                    slh.num_ref_idx_l1_active_minus1 = 0;
                }
            }
        }

        Some(slh)
    }
}

// ---------------------------------------------------------------------------
// BitstreamReader — minimal bitstream reading abstraction
// ---------------------------------------------------------------------------

/// Minimal bitstream reader for parsing NAL unit data.
///
/// Divergence from C++: The C++ code uses methods inherited from VulkanVideoDecoder
/// (`u()`, `ue()`, `se()`, etc.). We provide an equivalent standalone struct.
pub struct BitstreamReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitstreamReader<'a> {
    /// Create a new reader from a byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    /// Read `n` bits as a `u32`. Returns `None` if not enough bits available.
    pub fn u(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n > 32 {
            return None;
        }
        let n = n as usize;
        if self.bit_offset + n > self.data.len() * 8 {
            return None;
        }
        let mut val = 0u32;
        for _ in 0..n {
            let byte_idx = self.bit_offset / 8;
            let bit_idx = 7 - (self.bit_offset % 8);
            val = (val << 1) | ((self.data[byte_idx] >> bit_idx) as u32 & 1);
            self.bit_offset += 1;
        }
        Some(val)
    }

    /// Read an unsigned Exp-Golomb coded value (ue(v)).
    pub fn ue(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0u32;
        loop {
            let bit = self.u(1)?;
            if bit != 0 {
                break;
            }
            leading_zero_bits += 1;
            if leading_zero_bits > 31 {
                return None;
            }
        }
        if leading_zero_bits == 0 {
            return Some(0);
        }
        let suffix = self.u(leading_zero_bits)?;
        Some((1 << leading_zero_bits) - 1 + suffix)
    }

    /// Read a signed Exp-Golomb coded value (se(v)).
    pub fn se(&mut self) -> Option<i32> {
        let code_num = self.ue()?;
        let value = ((code_num + 1) >> 1) as i32;
        if code_num & 1 == 0 {
            Some(-value)
        } else {
            Some(value)
        }
    }

    /// Return the number of bits consumed so far.
    pub fn consumed_bits(&self) -> usize {
        self.bit_offset
    }

    /// Return the number of bits still available.
    pub fn available_bits(&self) -> usize {
        self.data.len() * 8 - self.bit_offset
    }

    /// Peek at the next `n` bits without consuming them.
    pub fn next_bits(&self, n: u32) -> Option<u32> {
        if n == 0 || n > 32 {
            return None;
        }
        let n = n as usize;
        if self.bit_offset + n > self.data.len() * 8 {
            return None;
        }
        let mut val = 0u32;
        for k in 0..n {
            let byte_idx = (self.bit_offset + k) / 8;
            let bit_idx = 7 - ((self.bit_offset + k) % 8);
            val = (val << 1) | ((self.data[byte_idx] >> bit_idx) as u32 & 1);
        }
        Some(val)
    }

    /// Skip `n` bits.
    pub fn skip_bits(&mut self, n: usize) {
        self.bit_offset += n;
    }

    /// Check if current position is byte-aligned.
    pub fn byte_aligned(&self) -> bool {
        self.bit_offset % 8 == 0
    }

    /// Read a fixed-pattern of `n` bits and verify it matches `expected`.
    pub fn f(&mut self, n: u32, expected: u32) -> Option<u32> {
        let val = self.u(n)?;
        if val != expected {
            tracing::warn!(
                "Fixed pattern mismatch: expected 0x{:x}, got 0x{:x}",
                expected,
                val
            );
        }
        Some(val)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper / utility tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_ceil_log2() {
        // CeilLog2(n) = Log2U31(n-1) where Log2U31 counts bit-width.
        // This matches ceil(log2(n)) for n >= 1.
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(-1), 0);
        assert_eq!(ceil_log2(1), 0);  // ceil(log2(1)) = 0
        assert_eq!(ceil_log2(2), 1);  // ceil(log2(2)) = 1
        assert_eq!(ceil_log2(3), 2);  // ceil(log2(3)) = 2
        assert_eq!(ceil_log2(4), 2);  // ceil(log2(4)) = 2
        assert_eq!(ceil_log2(5), 3);  // ceil(log2(5)) = 3
        assert_eq!(ceil_log2(8), 3);  // ceil(log2(8)) = 3
        assert_eq!(ceil_log2(9), 4);  // ceil(log2(9)) = 4
        assert_eq!(ceil_log2(16), 4); // ceil(log2(16)) = 4
        assert_eq!(ceil_log2(17), 5); // ceil(log2(17)) = 5
    }

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
    // NAL unit type tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_nal_unit_type_from_raw() {
        assert_eq!(NalUnitType::from_raw(0), Some(NalUnitType::TrailN));
        assert_eq!(NalUnitType::from_raw(19), Some(NalUnitType::IdrWRadl));
        assert_eq!(NalUnitType::from_raw(21), Some(NalUnitType::CraNut));
        assert_eq!(NalUnitType::from_raw(33), Some(NalUnitType::SpsNut));
        assert_eq!(NalUnitType::from_raw(10), None); // reserved
        assert_eq!(NalUnitType::from_raw(42), None);
    }

    #[test]
    fn test_nal_unit_type_is_irap() {
        assert!(NalUnitType::BlaWLp.is_irap());
        assert!(NalUnitType::IdrWRadl.is_irap());
        assert!(NalUnitType::CraNut.is_irap());
        assert!(!NalUnitType::TrailN.is_irap());
        assert!(!NalUnitType::SpsNut.is_irap());
    }

    #[test]
    fn test_nal_unit_type_is_idr() {
        assert!(NalUnitType::IdrWRadl.is_idr());
        assert!(NalUnitType::IdrNLp.is_idr());
        assert!(!NalUnitType::CraNut.is_idr());
        assert!(!NalUnitType::BlaWLp.is_idr());
    }

    #[test]
    fn test_nal_unit_type_is_slice() {
        assert!(NalUnitType::TrailN.is_slice());
        assert!(NalUnitType::RaslR.is_slice());
        assert!(NalUnitType::BlaWLp.is_slice());
        assert!(NalUnitType::CraNut.is_slice());
        assert!(!NalUnitType::VpsNut.is_slice());
        assert!(!NalUnitType::SpsNut.is_slice());
    }

    // -----------------------------------------------------------------------
    // POC calculation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_poc_idr_picture() {
        let mut decoder = VulkanH265Decoder::new();
        // Set up a minimal active SPS
        let mut sps = HevcSeqParam::default();
        sps.log2_max_pic_order_cnt_lsb_minus4 = 4; // MaxPicOrderCntLsb = 256
        decoder.active_sps[0] = Some(Box::new(sps));

        // IDR picture: POC should be just pic_order_cnt_lsb
        let slh = HevcSliceHeader {
            nal_unit_type: NalUnitType::IdrWRadl as u8,
            nuh_temporal_id_plus1: 1,
            pic_order_cnt_lsb: 0,
            ..Default::default()
        };
        decoder.no_rasl_output_flag = true;
        let poc = decoder.picture_order_count(&slh);
        assert_eq!(poc, 0);
    }

    #[test]
    fn test_poc_increment_sequence() {
        let mut decoder = VulkanH265Decoder::new();
        let mut sps = HevcSeqParam::default();
        sps.log2_max_pic_order_cnt_lsb_minus4 = 4; // MaxPicOrderCntLsb = 256
        decoder.active_sps[0] = Some(Box::new(sps));

        // First IDR
        decoder.no_rasl_output_flag = true;
        let slh_idr = HevcSliceHeader {
            nal_unit_type: NalUnitType::IdrWRadl as u8,
            nuh_temporal_id_plus1: 1,
            pic_order_cnt_lsb: 0,
            ..Default::default()
        };
        let poc = decoder.picture_order_count(&slh_idr);
        assert_eq!(poc, 0);

        // Subsequent non-IDR pictures
        decoder.no_rasl_output_flag = false;
        for expected_poc in 1..10i32 {
            let slh = HevcSliceHeader {
                nal_unit_type: NalUnitType::TrailR as u8,
                nuh_temporal_id_plus1: 1,
                pic_order_cnt_lsb: expected_poc as u16,
                ..Default::default()
            };
            let poc = decoder.picture_order_count(&slh);
            assert_eq!(poc, expected_poc);
        }
    }

    #[test]
    fn test_poc_wraparound() {
        let mut decoder = VulkanH265Decoder::new();
        let mut sps = HevcSeqParam::default();
        sps.log2_max_pic_order_cnt_lsb_minus4 = 0; // MaxPicOrderCntLsb = 16
        decoder.active_sps[0] = Some(Box::new(sps));

        // IDR at POC 0
        decoder.no_rasl_output_flag = true;
        let slh_idr = HevcSliceHeader {
            nal_unit_type: NalUnitType::IdrWRadl as u8,
            nuh_temporal_id_plus1: 1,
            pic_order_cnt_lsb: 0,
            ..Default::default()
        };
        let poc = decoder.picture_order_count(&slh_idr);
        assert_eq!(poc, 0);

        // Advance to LSB = 15
        decoder.no_rasl_output_flag = false;
        for lsb in 1..=15u16 {
            let slh = HevcSliceHeader {
                nal_unit_type: NalUnitType::TrailR as u8,
                nuh_temporal_id_plus1: 1,
                pic_order_cnt_lsb: lsb,
                ..Default::default()
            };
            let poc = decoder.picture_order_count(&slh);
            assert_eq!(poc, lsb as i32);
        }

        // Wraparound: LSB goes back to 0, POC should be 16
        let slh_wrap = HevcSliceHeader {
            nal_unit_type: NalUnitType::TrailR as u8,
            nuh_temporal_id_plus1: 1,
            pic_order_cnt_lsb: 0,
            ..Default::default()
        };
        let poc = decoder.picture_order_count(&slh_wrap);
        assert_eq!(poc, 16);
    }

    // -----------------------------------------------------------------------
    // DPB management tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_dpb_initially_empty() {
        let decoder = VulkanH265Decoder::new();
        assert!(decoder.dpb_empty());
        assert_eq!(decoder.dpb_fullness(), 0);
    }

    #[test]
    fn test_dpb_fullness_tracking() {
        let mut decoder = VulkanH265Decoder::new();
        decoder.dpb[0].state = DPB_STATE_IN_USE;
        decoder.dpb[0].marking = DPB_MARKING_SHORT_TERM;
        decoder.dpb[3].state = DPB_STATE_IN_USE;
        decoder.dpb[3].marking = DPB_MARKING_LONG_TERM;
        assert_eq!(decoder.dpb_fullness(), 2);
        assert!(!decoder.dpb_empty());
    }

    #[test]
    fn test_dpb_bumping_outputs_smallest_poc() {
        let mut decoder = VulkanH265Decoder::new();
        decoder.max_dpb_size = 4;

        // Add entries with different POCs
        for (i, poc) in [10, 5, 15, 3].iter().enumerate() {
            decoder.dpb[i].state = DPB_STATE_IN_USE;
            decoder.dpb[i].marking = DPB_MARKING_SHORT_TERM;
            decoder.dpb[i].output = 1;
            decoder.dpb[i].pic_order_cnt_val = *poc;
            decoder.dpb[i].layer_id = 0;
        }

        // Bump should output POC=3 (index 3)
        assert!(decoder.dpb_bumping(3));
        assert_eq!(decoder.dpb[3].output, 0); // output flag cleared
    }

    #[test]
    fn test_dpb_flush() {
        let mut decoder = VulkanH265Decoder::new();

        for i in 0..4 {
            decoder.dpb[i].state = DPB_STATE_IN_USE;
            decoder.dpb[i].marking = DPB_MARKING_SHORT_TERM;
            decoder.dpb[i].output = 1;
            decoder.dpb[i].pic_order_cnt_val = i as i32;
            decoder.dpb[i].layer_id = 0;
        }
        assert_eq!(decoder.dpb_fullness(), 4);

        decoder.flush_decoded_picture_buffer(false);
        assert!(decoder.dpb_empty());
    }

    #[test]
    fn test_dpb_flush_no_output() {
        let mut decoder = VulkanH265Decoder::new();

        for i in 0..4 {
            decoder.dpb[i].state = DPB_STATE_IN_USE;
            decoder.dpb[i].marking = DPB_MARKING_SHORT_TERM;
            decoder.dpb[i].output = 1;
            decoder.dpb[i].pic_order_cnt_val = i as i32;
            decoder.dpb[i].layer_id = 0;
        }

        decoder.flush_decoded_picture_buffer(true);
        assert!(decoder.dpb_empty());
    }

    #[test]
    fn test_dpb_reordering_delay() {
        let mut decoder = VulkanH265Decoder::new();
        decoder.nuh_layer_id = 0;

        decoder.dpb[0].state = DPB_STATE_IN_USE;
        decoder.dpb[0].output = 1;
        decoder.dpb[0].layer_id = 0;

        decoder.dpb[1].state = DPB_STATE_IN_USE;
        decoder.dpb[1].output = 0;
        decoder.dpb[1].layer_id = 0;

        decoder.dpb[2].state = DPB_STATE_IN_USE;
        decoder.dpb[2].output = 1;
        decoder.dpb[2].layer_id = 0;

        // Layer 1 entry should not be counted
        decoder.dpb[3].state = DPB_STATE_IN_USE;
        decoder.dpb[3].output = 1;
        decoder.dpb[3].layer_id = 1;

        assert_eq!(decoder.dpb_reordering_delay(), 2);
    }

    #[test]
    fn test_get_max_dpb_size() {
        let mut sps = HevcSeqParam::default();
        sps.profile_tier_level.general_level_idc = H265LevelIdc::Level4_0;
        sps.pic_width_in_luma_samples = 1920;
        sps.pic_height_in_luma_samples = 1080;
        let max_dpb = VulkanH265Decoder::get_max_dpb_size(&sps);
        // 1920*1080 = 2073600, MaxLumaPS for 4.0 = 2228224
        // PicSize <= MaxLumaPS, so MaxDpbSize = MaxDpbPicBuf = 6
        assert_eq!(max_dpb, 6);
    }

    #[test]
    fn test_get_max_dpb_size_small_picture() {
        let mut sps = HevcSeqParam::default();
        sps.profile_tier_level.general_level_idc = H265LevelIdc::Level4_0;
        sps.pic_width_in_luma_samples = 320;
        sps.pic_height_in_luma_samples = 240;
        let max_dpb = VulkanH265Decoder::get_max_dpb_size(&sps);
        // 320*240 = 76800. MaxLumaPS/4 = 557056. PicSize <= MaxLumaPS/4 => MaxDpbSize = 24
        // But capped at HEVC_DPB_SIZE = 16
        assert_eq!(max_dpb, 16);
    }

    // -----------------------------------------------------------------------
    // RPS management tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_reference_picture_set_idr() {
        let mut decoder = VulkanH265Decoder::new();
        let sps = HevcSeqParam::default();
        decoder.active_sps[0] = Some(Box::new(sps));

        let slh = HevcSliceHeader {
            nal_unit_type: NalUnitType::IdrWRadl as u8,
            ..Default::default()
        };

        decoder.no_rasl_output_flag = true;
        decoder.reference_picture_set(&slh, 0);

        // After IDR, all ref pic sets should be empty (-1)
        assert_eq!(decoder.num_poc_st_curr_before, 0);
        assert_eq!(decoder.num_poc_st_curr_after, 0);
        assert_eq!(decoder.num_poc_lt_curr, 0);
    }

    #[test]
    fn test_create_lost_ref_pic_found() {
        let mut decoder = VulkanH265Decoder::new();
        decoder.dpb[0].state = DPB_STATE_IN_USE;
        decoder.dpb[0].marking = DPB_MARKING_SHORT_TERM;
        decoder.dpb[0].pic_order_cnt_val = 5;
        decoder.dpb[0].layer_id = 0;

        decoder.dpb[3].state = DPB_STATE_IN_USE;
        decoder.dpb[3].marking = DPB_MARKING_SHORT_TERM;
        decoder.dpb[3].pic_order_cnt_val = 8;
        decoder.dpb[3].layer_id = 0;

        // Looking for POC 7, closest is POC 8 (DPB[3])
        let result = decoder.create_lost_ref_pic(7, 0, DPB_MARKING_SHORT_TERM);
        assert_eq!(result, 3);
    }

    #[test]
    fn test_create_lost_ref_pic_not_found() {
        let decoder = VulkanH265Decoder::new();
        let result = decoder.create_lost_ref_pic(42, 0, DPB_MARKING_SHORT_TERM);
        assert_eq!(result, -1);
    }

    // -----------------------------------------------------------------------
    // BitstreamReader tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_bitstream_reader_u() {
        // 0b10110001 = 0xB1
        let data = [0xB1];
        let mut reader = BitstreamReader::new(&data);
        assert_eq!(reader.u(1), Some(1)); // '1'
        assert_eq!(reader.u(3), Some(0b011)); // '011'
        assert_eq!(reader.u(4), Some(0b0001)); // '0001'
    }

    #[test]
    fn test_bitstream_reader_ue() {
        // ue(0) = '1' -> 0
        // ue(1) = '010' -> 1
        // ue(2) = '011' -> 2
        // ue(3) = '00100' -> 3
        // Concatenated: 1|010|011|00100 = 1010011 00100... pad
        // = 0b10100110 0100_0000 = 0xA6 0x40
        let data = [0xA6, 0x40];
        let mut reader = BitstreamReader::new(&data);
        assert_eq!(reader.ue(), Some(0));
        assert_eq!(reader.ue(), Some(1));
        assert_eq!(reader.ue(), Some(2));
        assert_eq!(reader.ue(), Some(3));
    }

    #[test]
    fn test_bitstream_reader_se() {
        // se(v): code_num 0 -> 0, 1 -> 1, 2 -> -1, 3 -> 2, 4 -> -2
        // ue: 0='1', 1='010', 2='011', 3='00100', 4='00101'
        // Concatenated: 1|010|011|00100|00101 = 10100110 01000010 1...
        // = 0xA6 0x42 0x80
        let data = [0xA6, 0x42, 0x80];
        let mut reader = BitstreamReader::new(&data);
        assert_eq!(reader.se(), Some(0));
        assert_eq!(reader.se(), Some(1));
        assert_eq!(reader.se(), Some(-1));
        assert_eq!(reader.se(), Some(2));
        assert_eq!(reader.se(), Some(-2));
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
        let mut reader = BitstreamReader::new(&data);
        let mut std_strps = StdShortTermRefPicSet::default();
        let mut strps = ShortTermRefPicSet::default();

        let result = VulkanH265Decoder::parse_short_term_ref_pic_set(
            &mut reader,
            &mut std_strps,
            &mut strps,
            &[],
            0,
            1,
        );

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
