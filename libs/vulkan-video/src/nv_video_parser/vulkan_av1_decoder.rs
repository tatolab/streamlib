// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `VulkanAV1Decoder.h` + `VulkanAV1Decoder.cpp` + `VulkanAV1GlobalMotionDec.cpp`.
//!
//! AV1 bitstream parser: sequence header (OBU) parsing, frame header parsing,
//! tile info, quantization, segmentation, loop filter parameters, CDEF parameters,
//! loop restoration, global motion parameter decoding, reference frame management
//! (8 reference frames), and film grain parameters.

// Many constants and helpers are ported for completeness even if not yet
// referenced by the current parsing paths.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const BUFFER_POOL_MAX_SIZE: usize = 10;

pub const MAX_NUM_TEMPORAL_LAYERS: usize = 8;
pub const MAX_NUM_SPATIAL_LAYERS: usize = 4;
pub const MAX_NUM_OPERATING_POINTS: usize = MAX_NUM_TEMPORAL_LAYERS * MAX_NUM_SPATIAL_LAYERS;

pub const LEVEL_MAJOR_BITS: u32 = 3;
pub const LEVEL_MINOR_BITS: u32 = 2;
pub const LEVEL_BITS: u32 = LEVEL_MAJOR_BITS + LEVEL_MINOR_BITS;

pub const LEVEL_MAJOR_MIN: u32 = 2;
pub const LEVEL_MAJOR_MAX: u32 = (1 << LEVEL_MAJOR_BITS) - 1 + LEVEL_MAJOR_MIN;
pub const LEVEL_MINOR_MIN: u32 = 0;
pub const LEVEL_MINOR_MAX: u32 = (1 << LEVEL_MINOR_BITS) - 1;
pub const OP_POINTS_CNT_MINUS_1_BITS: u32 = 5;
pub const OP_POINTS_IDC_BITS: u32 = 12;

pub const REF_FRAMES_BITS: u32 = 3;

/// AV1 spec: 8 reference frame slots.
pub const NUM_REF_FRAMES: usize = 8;
/// AV1 spec: 7 reference names used per frame (LAST..ALTREF).
pub const REFS_PER_FRAME: usize = 7;

pub const GM_GLOBAL_MODELS_PER_FRAME: usize = 7;
pub const SUPERRES_NUM: u32 = 8;
pub const SUPERRES_DENOM_MIN: u32 = 9;
pub const SUPERRES_DENOM_BITS: u32 = 3;

pub const MAX_TILE_COLS: usize = 64;
pub const MAX_TILE_ROWS: usize = 64;
pub const MAX_TILE_WIDTH: u32 = (MAX_TILE_COLS * MAX_TILE_ROWS) as u32;
pub const MAX_TILE_AREA: u32 = MAX_TILE_WIDTH * 2304;
pub const MAX_TILES: usize = 512;
pub const MIN_TILE_SIZE_BYTES: usize = 1;

pub const MAX_SEGMENTS: usize = 8;
pub const SEG_LVL_MAX: usize = 8;

pub const TOTAL_REFS_PER_FRAME: usize = 8;
pub const LOOP_FILTER_ADJUSTMENTS: usize = 2;

pub const MAX_NUM_Y_POINTS: usize = 14;
pub const MAX_NUM_CB_POINTS: usize = 10;
pub const MAX_NUM_CR_POINTS: usize = 10;
pub const MAX_NUM_POS_LUMA: usize = 24;
pub const MAX_NUM_POS_CHROMA: usize = 25;

pub const BIT32_MAX: u32 = 0xFFFF_FFFF;

const PRIMARY_REF_NONE: u32 = 7;

// ---------------------------------------------------------------------------
// Global motion constants (from VulkanAV1GlobalMotionDec.cpp)
// ---------------------------------------------------------------------------

const DIV_LUT_PREC_BITS: i32 = 14;
const DIV_LUT_BITS: i32 = 8;
const DIV_LUT_NUM: usize = 1 << DIV_LUT_BITS;

pub const WARPEDMODEL_PREC_BITS: i32 = 16;
const WARPEDMODEL_ROW3HOMO_PREC_BITS: i32 = 16;
const WARPEDPIXEL_PREC_BITS: i32 = 6;
const WARPEDPIXEL_PREC_SHIFTS: i32 = 1 << WARPEDPIXEL_PREC_BITS;

const SUBEXPFIN_K: u16 = 3;
const GM_TRANS_PREC_BITS: i32 = 6;
const GM_ABS_TRANS_BITS: i32 = 12;
const GM_ABS_TRANS_ONLY_BITS: i32 = GM_ABS_TRANS_BITS - GM_TRANS_PREC_BITS + 3;
const GM_TRANS_PREC_DIFF: i32 = WARPEDMODEL_PREC_BITS - GM_TRANS_PREC_BITS;
const GM_TRANS_ONLY_PREC_DIFF: i32 = WARPEDMODEL_PREC_BITS - 3;
const GM_TRANS_DECODE_FACTOR: i32 = 1 << GM_TRANS_PREC_DIFF;
const GM_TRANS_ONLY_DECODE_FACTOR: i32 = 1 << GM_TRANS_ONLY_PREC_DIFF;

const GM_ALPHA_PREC_BITS: i32 = 15;
const GM_ABS_ALPHA_BITS: i32 = 12;
const GM_ALPHA_PREC_DIFF: i32 = WARPEDMODEL_PREC_BITS - GM_ALPHA_PREC_BITS;
const GM_ALPHA_DECODE_FACTOR: i32 = 1 << GM_ALPHA_PREC_DIFF;
const GM_ALPHA_MAX: i32 = 1 << GM_ABS_ALPHA_BITS;

const GM_TRANS_MAX: i32 = 1 << GM_ABS_TRANS_BITS;

const WARP_PARAM_REDUCE_BITS: i32 = 6;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// OBU types (AV1 spec section 5.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Av1ObuType {
    SequenceHeader = 1,
    TemporalDelimiter = 2,
    FrameHeader = 3,
    TileGroup = 4,
    Metadata = 5,
    Frame = 6,
    RedundantFrameHeader = 7,
    TileList = 8,
    Padding = 15,
}

impl Av1ObuType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::SequenceHeader),
            2 => Some(Self::TemporalDelimiter),
            3 => Some(Self::FrameHeader),
            4 => Some(Self::TileGroup),
            5 => Some(Self::Metadata),
            6 => Some(Self::Frame),
            7 => Some(Self::RedundantFrameHeader),
            8 => Some(Self::TileList),
            15 => Some(Self::Padding),
            _ => None,
        }
    }
}

/// Transformation types for global motion (AV1 spec section 5.9.24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1TransformationType {
    #[default]
    Identity = 0,
    Translation = 1,
    Rotzoom = 2,
    Affine = 3,
}

impl Av1TransformationType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Identity),
            1 => Some(Self::Translation),
            2 => Some(Self::Rotzoom),
            3 => Some(Self::Affine),
            _ => None,
        }
    }
}

/// AV1 frame types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1FrameType {
    #[default]
    Key = 0,
    Inter = 1,
    IntraOnly = 2,
    Switch = 3,
}

impl Av1FrameType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Key),
            1 => Some(Self::Inter),
            2 => Some(Self::IntraOnly),
            3 => Some(Self::Switch),
            _ => None,
        }
    }
}

/// AV1 profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1Profile {
    #[default]
    Main = 0,
    High = 1,
    Professional = 2,
}

impl Av1Profile {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Main),
            1 => Some(Self::High),
            2 => Some(Self::Professional),
            _ => None,
        }
    }
}

/// AV1 interpolation filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1InterpolationFilter {
    EightTap = 0,
    EightTapSmooth = 1,
    EightTapSharp = 2,
    Bilinear = 3,
    #[default]
    Switchable = 4,
}

impl Av1InterpolationFilter {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::EightTap),
            1 => Some(Self::EightTapSmooth),
            2 => Some(Self::EightTapSharp),
            3 => Some(Self::Bilinear),
            4 => Some(Self::Switchable),
            _ => None,
        }
    }
}

/// AV1 TX mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1TxMode {
    Only4x4 = 0,
    Largest = 1,
    #[default]
    Select = 2,
}

/// AV1 frame restoration type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1FrameRestorationType {
    #[default]
    None = 0,
    Switchable = 1,
    Wiener = 2,
    Sgrproj = 3,
}

/// AV1 color primaries — subset used for sRGB detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1ColorPrimaries {
    Bt709 = 1,
    #[default]
    Unspecified = 2,
}

/// AV1 transfer characteristics — subset used for sRGB detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1TransferCharacteristics {
    #[default]
    Unspecified = 2,
    Srgb = 13,
}

/// AV1 matrix coefficients — subset used for identity detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Av1MatrixCoefficients {
    Identity = 0,
    #[default]
    Unspecified = 2,
}

/// AV1 reference name indices (section 6.10.24).
/// These are 1-based in the spec; LAST_FRAME = 1.
pub const REFERENCE_NAME_LAST_FRAME: usize = 1;
pub const REFERENCE_NAME_LAST2_FRAME: usize = 2;
pub const REFERENCE_NAME_LAST3_FRAME: usize = 3;
pub const REFERENCE_NAME_GOLDEN_FRAME: usize = 4;
pub const REFERENCE_NAME_BWDREF_FRAME: usize = 5;
pub const REFERENCE_NAME_ALTREF2_FRAME: usize = 6;
pub const REFERENCE_NAME_ALTREF_FRAME: usize = 7;

// ---------------------------------------------------------------------------
// Structures
// ---------------------------------------------------------------------------

/// Warped motion parameters for a single reference frame.
///
/// Corresponds to `AV1WarpedMotionParams` in the C++ source.
#[derive(Debug, Clone)]
pub struct Av1WarpedMotionParams {
    pub wmtype: Av1TransformationType,
    pub wmmat: [i32; 6],
    pub invalid: bool,
}

impl Default for Av1WarpedMotionParams {
    fn default() -> Self {
        default_warp_params()
    }
}

/// Returns the default (identity) warp parameters.
pub fn default_warp_params() -> Av1WarpedMotionParams {
    Av1WarpedMotionParams {
        wmtype: Av1TransformationType::Identity,
        wmmat: [
            0,
            0,
            1 << WARPEDMODEL_PREC_BITS,
            0,
            0,
            1 << WARPEDMODEL_PREC_BITS,
        ],
        invalid: false,
    }
}

/// OBU header.
///
/// Corresponds to `AV1ObuHeader` in the C++ source.
#[derive(Debug, Clone, Default)]
pub struct Av1ObuHeader {
    pub header_size: u32,
    pub payload_size: u32,
    pub obu_type: Option<Av1ObuType>,
    pub has_size_field: bool,
    pub has_extension: bool,
    pub temporal_id: i32,
    pub spatial_id: i32,
}

/// Color configuration from sequence header.
#[derive(Debug, Clone, Default)]
pub struct Av1ColorConfig {
    pub bit_depth: u8,
    pub mono_chrome: bool,
    pub color_description_present_flag: bool,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub color_range: bool,
    pub subsampling_x: u8,
    pub subsampling_y: u8,
    pub chroma_sample_position: u8,
    pub separate_uv_delta_q: bool,
}

/// Timing info from sequence header.
#[derive(Debug, Clone, Default)]
pub struct Av1TimingInfo {
    pub num_units_in_display_tick: u32,
    pub time_scale: u32,
    pub equal_picture_interval: bool,
    pub num_ticks_per_picture: u32,
}

/// Decoder model info.
#[derive(Debug, Clone, Default)]
pub struct Av1DecoderModelInfo {
    pub num_units_in_decoding_tick: u32,
    pub encoder_decoder_buffer_delay_length: i32,
    pub buffer_removal_time_length: i32,
    pub frame_presentation_time_length: i32,
}

/// Decoder model operating point parameters.
#[derive(Debug, Clone, Default)]
pub struct Av1DecoderModelOpParams {
    pub decoder_model_param_present: bool,
    pub bitrate: u32,
    pub buffer_size: u32,
    pub cbr_flag: i32,
    pub decoder_buffer_delay: i32,
    pub encoder_buffer_delay: i32,
    pub low_delay_mode_flag: i32,
    pub display_model_param_present: bool,
    pub initial_display_delay: i32,
}

/// Sequence header parameters.
///
/// Corresponds to `av1_seq_param_s` in the C++ source.
#[derive(Debug, Clone, Default)]
pub struct Av1SequenceHeader {
    pub seq_profile: u32,
    pub still_picture: bool,
    pub reduced_still_picture_header: bool,

    pub frame_width_bits_minus_1: u32,
    pub frame_height_bits_minus_1: u32,
    pub max_frame_width_minus_1: u32,
    pub max_frame_height_minus_1: u32,

    pub frame_id_numbers_present_flag: bool,
    pub use_128x128_superblock: bool,
    pub enable_filter_intra: bool,
    pub enable_intra_edge_filter: bool,
    pub enable_interintra_compound: bool,
    pub enable_masked_compound: bool,
    pub enable_warped_motion: bool,
    pub enable_dual_filter: bool,
    pub enable_order_hint: bool,
    pub enable_jnt_comp: bool,
    pub enable_ref_frame_mvs: bool,
    pub seq_force_screen_content_tools: u32,
    pub seq_force_integer_mv: u32,
    pub order_hint_bits_minus_1: u32,
    pub enable_superres: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,
    pub film_grain_params_present: bool,

    // Operating point info
    pub operating_points_cnt_minus_1: i32,
    pub operating_point_idc: [i32; MAX_NUM_OPERATING_POINTS],
    pub display_model_info_present: bool,
    pub decoder_model_info_present: bool,
    pub level: [u32; MAX_NUM_OPERATING_POINTS],
    pub tier: [u8; MAX_NUM_OPERATING_POINTS],

    pub color_config: Av1ColorConfig,
    pub timing_info: Av1TimingInfo,

    pub update_sequence_count: u64,
}

/// Select screen content tools constant (AV1 spec).
pub const SELECT_SCREEN_CONTENT_TOOLS: u32 = 2;
/// Select integer MV constant (AV1 spec).
pub const SELECT_INTEGER_MV: u32 = 2;

/// Quantization parameters.
#[derive(Debug, Clone, Default)]
pub struct Av1Quantization {
    pub base_q_idx: u32,
    pub delta_q_y_dc: i32,
    pub delta_q_u_dc: i32,
    pub delta_q_u_ac: i32,
    pub delta_q_v_dc: i32,
    pub delta_q_v_ac: i32,
    pub using_qmatrix: bool,
    pub qm_y: u8,
    pub qm_u: u8,
    pub qm_v: u8,
}

/// Segmentation data.
#[derive(Debug, Clone, Default)]
pub struct Av1Segmentation {
    pub feature_enabled: [u8; MAX_SEGMENTS],
    pub feature_data: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
}

/// Loop filter parameters.
#[derive(Debug, Clone, Default)]
pub struct Av1LoopFilter {
    pub loop_filter_level: [u8; 4],
    pub loop_filter_sharpness: u8,
    pub loop_filter_delta_enabled: bool,
    pub loop_filter_delta_update: bool,
    pub loop_filter_ref_deltas: [i8; TOTAL_REFS_PER_FRAME],
    pub loop_filter_mode_deltas: [i8; LOOP_FILTER_ADJUSTMENTS],
}

/// CDEF parameters.
#[derive(Debug, Clone, Default)]
pub struct Av1Cdef {
    pub cdef_damping_minus_3: u8,
    pub cdef_bits: u8,
    pub cdef_y_pri_strength: [u8; 8],
    pub cdef_y_sec_strength: [u8; 8],
    pub cdef_uv_pri_strength: [u8; 8],
    pub cdef_uv_sec_strength: [u8; 8],
}

/// Loop restoration parameters.
#[derive(Debug, Clone, Default)]
pub struct Av1LoopRestoration {
    pub frame_restoration_type: [Av1FrameRestorationType; 3],
    pub loop_restoration_size: [u8; 3],
}

/// Film grain parameters.
#[derive(Debug, Clone, Default)]
pub struct Av1FilmGrain {
    pub apply_grain: bool,
    pub grain_seed: u16,
    pub update_grain: bool,
    pub film_grain_params_ref_idx: u8,
    pub num_y_points: u8,
    pub point_y_value: [u8; MAX_NUM_Y_POINTS],
    pub point_y_scaling: [u8; MAX_NUM_Y_POINTS],
    pub chroma_scaling_from_luma: bool,
    pub num_cb_points: u8,
    pub point_cb_value: [u8; MAX_NUM_CB_POINTS],
    pub point_cb_scaling: [u8; MAX_NUM_CB_POINTS],
    pub num_cr_points: u8,
    pub point_cr_value: [u8; MAX_NUM_CR_POINTS],
    pub point_cr_scaling: [u8; MAX_NUM_CR_POINTS],
    pub grain_scaling_minus_8: u8,
    pub ar_coeff_lag: u8,
    pub ar_coeffs_y_plus_128: [u8; MAX_NUM_POS_LUMA],
    pub ar_coeffs_cb_plus_128: [u8; MAX_NUM_POS_CHROMA],
    pub ar_coeffs_cr_plus_128: [u8; MAX_NUM_POS_CHROMA],
    pub ar_coeff_shift_minus_6: u8,
    pub grain_scale_shift: u8,
    pub cb_mult: u8,
    pub cb_luma_mult: u8,
    pub cb_offset: u16,
    pub cr_mult: u8,
    pub cr_luma_mult: u8,
    pub cr_offset: u16,
    pub overlap_flag: bool,
    pub clip_to_restricted_range: bool,
}

/// Tile info.
#[derive(Debug, Clone, Default)]
pub struct Av1TileInfo {
    pub tile_cols: u32,
    pub tile_rows: u32,
    pub context_update_tile_id: u32,
    pub tile_size_bytes_minus_1: u8,
    pub uniform_tile_spacing_flag: bool,
}

/// Per-frame picture info flags.
#[derive(Debug, Clone, Default)]
pub struct Av1PictureInfoFlags {
    pub error_resilient_mode: bool,
    pub disable_cdf_update: bool,
    pub allow_screen_content_tools: bool,
    pub force_integer_mv: bool,
    pub allow_intrabc: bool,
    pub allow_high_precision_mv: bool,
    pub is_filter_switchable: bool,
    pub is_motion_mode_switchable: bool,
    pub use_ref_frame_mvs: bool,
    pub disable_frame_end_update_cdf: bool,
    pub allow_warped_motion: bool,
    pub reduced_tx_set: bool,
    pub reference_select: bool,
    pub skip_mode_present: bool,
    pub delta_q_present: bool,
    pub delta_lf_present: bool,
    pub delta_lf_multi: bool,
    pub segmentation_enabled: bool,
    pub segmentation_update_map: bool,
    pub segmentation_update_data: bool,
    pub segmentation_temporal_update: bool,
    pub use_superres: bool,
    pub render_and_frame_size_different: bool,
    pub buffer_removal_time_present_flag: bool,
    pub frame_size_override_flag: bool,
    pub frame_refs_short_signaling: bool,
    pub uses_lr: bool,
}

/// Decoded AV1 picture info (per-frame state).
///
/// Corresponds to `VkParserAv1PictureData` / `StdVideoDecodeAV1PictureInfo` in C++.
#[derive(Debug, Clone, Default)]
pub struct Av1PictureInfo {
    pub flags: Av1PictureInfoFlags,
    pub frame_type: Av1FrameType,
    pub current_frame_id: u32,
    pub order_hint: u8,
    pub primary_ref_frame: u32,
    pub refresh_frame_flags: u8,
    pub interpolation_filter: Av1InterpolationFilter,
    pub tx_mode: Av1TxMode,
    pub delta_q_res: u8,
    pub delta_lf_res: u8,
    pub skip_mode_frame: [i32; 2],
    pub coded_denom: u8,
    pub order_hints: [u8; NUM_REF_FRAMES],
}

/// Per-frame picture data assembled during parsing.
///
/// Corresponds to the `m_PicData` aggregate in the C++ source.
#[derive(Debug, Clone)]
pub struct Av1PictureData {
    pub std_info: Av1PictureInfo,
    pub show_frame: bool,
    pub needs_session_reset: bool,
    pub tile_info: Av1TileInfo,
    pub quantization: Av1Quantization,
    pub segmentation: Av1Segmentation,
    pub loop_filter: Av1LoopFilter,
    pub cdef: Av1Cdef,
    pub loop_restoration: Av1LoopRestoration,
    pub film_grain: Av1FilmGrain,
    pub global_motion_types: [u32; NUM_REF_FRAMES],
    pub gm_params: [[i32; 6]; NUM_REF_FRAMES],

    // Tile group data
    pub tile_count: u32,
    pub tile_offsets: [u32; MAX_TILES],
    pub tile_sizes: [u32; MAX_TILES],

    // Superblock column/row starts and width/height for non-uniform tiles
    pub mi_col_starts: [u32; MAX_TILE_COLS + 1],
    pub mi_row_starts: [u32; MAX_TILE_ROWS + 1],
    pub width_in_sbs_minus_1: [u32; MAX_TILE_COLS],
    pub height_in_sbs_minus_1: [u32; MAX_TILE_ROWS],

    pub ref_frame_idx: [i32; REFS_PER_FRAME],

    pub upscaled_width: u16,
    pub frame_width: u16,
    pub frame_height: u16,
}

impl Default for Av1PictureData {
    fn default() -> Self {
        Self {
            std_info: Av1PictureInfo::default(),
            show_frame: false,
            needs_session_reset: false,
            tile_info: Av1TileInfo::default(),
            quantization: Av1Quantization::default(),
            segmentation: Av1Segmentation::default(),
            loop_filter: Av1LoopFilter::default(),
            cdef: Av1Cdef::default(),
            loop_restoration: Av1LoopRestoration::default(),
            film_grain: Av1FilmGrain::default(),
            global_motion_types: [0; NUM_REF_FRAMES],
            gm_params: [[0; 6]; NUM_REF_FRAMES],
            tile_count: 0,
            tile_offsets: [0; MAX_TILES],
            tile_sizes: [0; MAX_TILES],
            mi_col_starts: [0; MAX_TILE_COLS + 1],
            mi_row_starts: [0; MAX_TILE_ROWS + 1],
            width_in_sbs_minus_1: [0; MAX_TILE_COLS],
            height_in_sbs_minus_1: [0; MAX_TILE_ROWS],
            ref_frame_idx: [0; REFS_PER_FRAME],
            upscaled_width: 0,
            frame_width: 0,
            frame_height: 0,
        }
    }
}

/// State for a single reference frame buffer slot.
///
/// Corresponds to `av1_ref_frames_s` in the C++ source.
#[derive(Debug, Clone)]
pub struct Av1RefFrameState {
    pub buffer_valid: bool,
    pub frame_type: Av1FrameType,
    pub film_grain_params: Av1FilmGrain,
    pub global_models: [Av1WarpedMotionParams; GM_GLOBAL_MODELS_PER_FRAME],
    pub lf_ref_delta: [i8; TOTAL_REFS_PER_FRAME],
    pub lf_mode_delta: [i8; LOOP_FILTER_ADJUSTMENTS],
    pub showable_frame: bool,
    pub seg_feature_enabled: [u8; MAX_SEGMENTS],
    pub seg_feature_data: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
    pub primary_ref_frame: u32,
    pub base_q_index: u32,
    pub disable_frame_end_update_cdf: bool,
    pub segmentation_enabled: bool,
    pub ref_frame_sign_bias: [i8; NUM_REF_FRAMES],
    pub saved_order_hints: [u8; NUM_REF_FRAMES],
    pub order_hint: u8,
}

impl Default for Av1RefFrameState {
    fn default() -> Self {
        Self {
            buffer_valid: false,
            frame_type: Av1FrameType::default(),
            film_grain_params: Av1FilmGrain::default(),
            global_models: std::array::from_fn(|_| default_warp_params()),
            lf_ref_delta: [0; TOTAL_REFS_PER_FRAME],
            lf_mode_delta: [0; LOOP_FILTER_ADJUSTMENTS],
            showable_frame: false,
            seg_feature_enabled: [0; MAX_SEGMENTS],
            seg_feature_data: [[0; SEG_LVL_MAX]; MAX_SEGMENTS],
            primary_ref_frame: PRIMARY_REF_NONE,
            base_q_index: 0,
            disable_frame_end_update_cdf: false,
            segmentation_enabled: false,
            ref_frame_sign_bias: [0; NUM_REF_FRAMES],
            saved_order_hints: [0; NUM_REF_FRAMES],
            order_hint: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Bitstream reader (minimal — matches the `u()` / bit-reading in C++ base)
// ---------------------------------------------------------------------------

/// A simple bitstream reader that reads from a byte slice.
///
/// Corresponds to the bit-reading facilities inherited from `VulkanVideoDecoder`
/// (the `u(n)`, `init_dbits()`, etc. methods). In the C++ source these are
/// member functions of the base class; here we use a standalone struct that
/// the decoder holds.
#[derive(Debug)]
pub struct BitstreamReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitstreamReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit_offset: 0 }
    }

    /// Read `n` bits as an unsigned value (MSB-first). Corresponds to `u(n)`.
    pub fn u(&mut self, n: u32) -> u32 {
        assert!(n <= 32);
        if n == 0 {
            return 0;
        }
        let mut val: u32 = 0;
        for _ in 0..n {
            let byte_idx = self.bit_offset / 8;
            let bit_idx = 7 - (self.bit_offset % 8);
            if byte_idx < self.data.len() {
                val = (val << 1) | (((self.data[byte_idx] >> bit_idx) & 1) as u32);
            } else {
                val <<= 1;
            }
            self.bit_offset += 1;
        }
        val
    }

    /// Current consumed bits count.
    pub fn consumed_bits(&self) -> usize {
        self.bit_offset
    }

    /// Align to the next byte boundary.
    pub fn byte_alignment(&mut self) {
        let remainder = self.bit_offset % 8;
        if remainder != 0 {
            self.bit_offset += 8 - remainder;
        }
    }

    /// Skip `n` bits.
    pub fn skip_bits(&mut self, n: u32) {
        self.bit_offset += n as usize;
    }

    /// Read `n` bytes in little-endian order (corresponds to `le(n)`).
    pub fn le(&mut self, n: usize) -> u64 {
        let mut t: u64 = 0;
        for i in 0..n {
            let byte = self.u(8) as u64;
            t += byte << (i * 8);
        }
        t
    }

    /// Remaining bytes from current bit position (rounded down).
    pub fn remaining_bytes(&self) -> usize {
        let remaining_bits = if self.data.len() * 8 > self.bit_offset {
            self.data.len() * 8 - self.bit_offset
        } else {
            0
        };
        remaining_bits / 8
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Clamp a value to `[low, high]`.
fn clamp<T: Ord>(value: T, low: T, high: T) -> T {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

/// Compute `tile_log2`: the minimum k such that `(blk_size << k) >= target`.
fn tile_log2(blk_size: u32, target: u32) -> u32 {
    let mut k = 0u32;
    while (blk_size << k) < target {
        k += 1;
    }
    k
}

/// Floor of log2 for a non-zero value.
fn floor_log2(x: u32) -> u32 {
    assert!(x > 0);
    let mut val = x;
    let mut s: i32 = 0;
    while val != 0 {
        val >>= 1;
        s += 1;
    }
    (s - 1) as u32
}

/// Get the position of the most significant bit (0-indexed).
fn get_msb(n: u32) -> i32 {
    assert!(n != 0);
    let mut log = 0i32;
    let mut value = n;
    for i in (0..=4).rev() {
        let shift = 1 << i;
        let x = value >> shift;
        if x != 0 {
            value = x;
            log += shift;
        }
    }
    log
}

/// Inverse recenter a non-negative literal `v` around reference `r`.
fn inv_recenter_nonneg(r: u16, v: u16) -> u16 {
    if v > (r << 1) {
        v
    } else if (v & 1) == 0 {
        (v >> 1) + r
    } else {
        r - ((v + 1) >> 1)
    }
}

/// Inverse recenter a non-negative literal `v` in `[0, n-1]` around `r` in `[0, n-1]`.
fn inv_recenter_finite_nonneg(n: u16, r: u16, v: u16) -> u16 {
    if (r << 1) <= n {
        inv_recenter_nonneg(r, v)
    } else {
        n - 1 - inv_recenter_nonneg(n - 1 - r, v)
    }
}

fn read_u16_le(data: &[u8]) -> u32 {
    ((data[1] as u32) << 8) | (data[0] as u32)
}

fn read_u24_le(data: &[u8]) -> u32 {
    ((data[2] as u32) << 16) | ((data[1] as u32) << 8) | (data[0] as u32)
}

fn read_u32_le(data: &[u8]) -> u32 {
    ((data[3] as u32) << 24) | ((data[2] as u32) << 16) | ((data[1] as u32) << 8) | (data[0] as u32)
}

fn read_tile_group_size(src: &[u8], size: usize) -> Option<u64> {
    match size {
        1 => Some(src[0] as u64),
        2 => Some(read_u16_le(src) as u64),
        3 => Some(read_u24_le(src) as u64),
        4 => Some(read_u32_le(src) as u64),
        _ => None,
    }
}

/// Segmentation feature data: whether each level is signed.
const SEG_FEATURE_DATA_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
/// Segmentation feature data: bit width per level.
const SEG_FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
/// Segmentation feature data: max absolute value per level.
const SEG_FEATURE_DATA_MAX: [i32; SEG_LVL_MAX] = [255, 63, 63, 63, 63, 7, 0, 0];

/// Default loop filter reference deltas.
const LF_REF_DELTA_DEFAULT: [i8; TOTAL_REFS_PER_FRAME] = [1, 0, 0, 0, -1, 0, -1, -1];

// ---------------------------------------------------------------------------
// VulkanAV1Decoder
// ---------------------------------------------------------------------------

/// AV1 decoder state machine and bitstream parser.
///
/// Corresponds to `class VulkanAV1Decoder` in the C++ source.
pub struct VulkanAv1Decoder {
    // Sequence parameter set
    pub sps: Option<Av1SequenceHeader>,

    // Picture data being assembled for the current frame
    pub pic_data: Av1PictureData,

    // Common params
    pub temporal_id: i32,
    pub spatial_id: i32,
    pub sps_received: bool,
    pub sps_changed: bool,
    pub obu_annex_b: bool,
    pub timing_info_present: bool,
    pub timing_info: Av1TimingInfo,
    pub buffer_model: Av1DecoderModelInfo,
    pub op_params: [Av1DecoderModelOpParams; MAX_NUM_OPERATING_POINTS + 1],
    pub op_frame_timing: [u32; MAX_NUM_OPERATING_POINTS + 1],

    pub delta_frame_id_length: u8,
    pub frame_id_length: u8,
    pub last_frame_type: Av1FrameType,
    pub last_intra_only: bool,
    pub coded_lossless: bool,
    pub all_lossless: bool,

    // Frame header
    pub upscaled_width: u16,
    pub frame_width: u16,
    pub frame_height: u16,
    pub render_width: i32,
    pub render_height: i32,

    pub intra_only: bool,
    pub showable_frame: bool,
    pub last_show_frame: bool,
    pub show_existing_frame: bool,
    pub tu_presentation_delay: i32,

    pub lossless: [bool; MAX_SEGMENTS],

    pub tile_size_bytes_minus_1: u8,
    pub log2_tile_cols: u32,
    pub log2_tile_rows: u32,

    // Global motion
    pub global_motions: [Av1WarpedMotionParams; GM_GLOBAL_MODELS_PER_FRAME],

    pub ref_frame_id: [i32; NUM_REF_FRAMES],
    pub ref_valid: [bool; NUM_REF_FRAMES],
    pub ref_frame_idx: [i32; REFS_PER_FRAME],

    // Reference order hints
    pub ref_order_hint: [i32; BUFFER_POOL_MAX_SIZE],
    pub buffers: [Av1RefFrameState; BUFFER_POOL_MAX_SIZE],

    pub output_all_layers: bool,
    pub operating_point_idc_active: i32,
}

impl Default for VulkanAv1Decoder {
    fn default() -> Self {
        Self::new(false)
    }
}

impl VulkanAv1Decoder {
    /// Create a new AV1 decoder. `annex_b` selects Annex B OBU framing.
    ///
    /// Corresponds to the `VulkanAV1Decoder` constructor in C++.
    pub fn new(annex_b: bool) -> Self {
        let mut decoder = Self {
            sps: None,
            pic_data: Av1PictureData::default(),
            temporal_id: 0,
            spatial_id: 0,
            sps_received: false,
            sps_changed: false,
            obu_annex_b: annex_b,
            timing_info_present: false,
            timing_info: Av1TimingInfo::default(),
            buffer_model: Av1DecoderModelInfo::default(),
            op_params: std::array::from_fn(|_| Av1DecoderModelOpParams::default()),
            op_frame_timing: [0; MAX_NUM_OPERATING_POINTS + 1],
            delta_frame_id_length: 0,
            frame_id_length: 0,
            last_frame_type: Av1FrameType::Key,
            last_intra_only: false,
            coded_lossless: false,
            all_lossless: false,
            upscaled_width: 0,
            frame_width: 0,
            frame_height: 0,
            render_width: 0,
            render_height: 0,
            intra_only: false,
            showable_frame: false,
            last_show_frame: false,
            show_existing_frame: false,
            tu_presentation_delay: 0,
            lossless: [false; MAX_SEGMENTS],
            tile_size_bytes_minus_1: 3,
            log2_tile_cols: 0,
            log2_tile_rows: 0,
            global_motions: std::array::from_fn(|_| default_warp_params()),
            ref_frame_id: [-1; NUM_REF_FRAMES],
            ref_valid: [false; NUM_REF_FRAMES],
            ref_frame_idx: [0; REFS_PER_FRAME],
            ref_order_hint: [0; BUFFER_POOL_MAX_SIZE],
            buffers: std::array::from_fn(|_| Av1RefFrameState::default()),
            output_all_layers: false,
            operating_point_idc_active: 0,
        };

        decoder.pic_data.std_info.primary_ref_frame = PRIMARY_REF_NONE;
        decoder.pic_data.std_info.refresh_frame_flags = ((1u16 << NUM_REF_FRAMES) - 1) as u8;

        decoder
    }

    /// Returns true if the current frame is intra (KEY or INTRA_ONLY).
    pub fn is_frame_intra(&self) -> bool {
        matches!(
            self.pic_data.std_info.frame_type,
            Av1FrameType::IntraOnly | Av1FrameType::Key
        )
    }

    /// Compute relative distance between two order hints.
    ///
    /// Corresponds to `GetRelativeDist` / `GetRelativeDist1` in C++.
    pub fn get_relative_dist(&self, a: i32, b: i32) -> i32 {
        let sps = match &self.sps {
            Some(s) => s,
            None => return 0,
        };
        if !sps.enable_order_hint {
            return 0;
        }
        let bits = sps.order_hint_bits_minus_1 as i32 + 1;
        debug_assert!(bits >= 1);
        let diff = a - b;
        let m = 1 << (bits - 1);
        (diff & (m - 1)) - (diff & m)
    }

    // -----------------------------------------------------------------------
    // OBU header parsing
    // -----------------------------------------------------------------------

    /// Read OBU size from LEB128 encoding.
    ///
    /// Returns `Some((obu_size, length_field_size))` on success.
    pub fn read_obu_size(data: &[u8]) -> Option<(u32, u32)> {
        let mut obu_size: u64 = 0;
        for i in 0..8.min(data.len()) {
            let decoded_byte = (data[i] & 0x7f) as u64;
            obu_size |= decoded_byte << (i * 7);
            if (data[i] >> 7) == 0 {
                if obu_size > BIT32_MAX as u64 {
                    return None;
                }
                return Some((obu_size as u32, (i + 1) as u32));
            }
        }
        None
    }

    /// Parse an OBU header from raw bytes.
    pub fn read_obu_header(data: &[u8]) -> Option<Av1ObuHeader> {
        if data.is_empty() {
            return None;
        }

        let mut hdr = Av1ObuHeader::default();
        hdr.header_size = 1;

        // Forbidden bit
        if (data[0] >> 7) & 1 != 0 {
            return None;
        }

        let type_val = (data[0] >> 3) & 0xf;
        hdr.obu_type = Av1ObuType::from_u8(type_val);
        if hdr.obu_type.is_none() {
            return None;
        }

        hdr.has_extension = ((data[0] >> 2) & 1) != 0;
        hdr.has_size_field = ((data[0] >> 1) & 1) != 0;

        // Reserved bit
        if (data[0] & 1) != 0 {
            return None;
        }

        if hdr.has_extension {
            if data.len() < 2 {
                return None;
            }
            hdr.header_size += 1;
            hdr.temporal_id = ((data[1] >> 5) & 0x7) as i32;
            hdr.spatial_id = ((data[1] >> 3) & 0x3) as i32;
            if (data[1] & 0x7) != 0 {
                return None;
            }
        }

        Some(hdr)
    }

    /// Parse OBU header and determine payload size.
    pub fn parse_obu_header_and_size(&self, data: &[u8]) -> Option<Av1ObuHeader> {
        if data.is_empty() {
            return None;
        }

        let mut annexb_obu_length: u32 = 0;
        let mut annexb_uleb_length: u32 = 0;

        if self.obu_annex_b {
            let (size, len) = Self::read_obu_size(data)?;
            annexb_obu_length = size;
            annexb_uleb_length = len;
        }

        let offset = annexb_uleb_length as usize;
        if offset >= data.len() {
            return None;
        }
        let mut hdr = Self::read_obu_header(&data[offset..])?;

        if !hdr.has_size_field && !self.obu_annex_b {
            return None;
        }

        if self.obu_annex_b {
            if annexb_obu_length < hdr.header_size {
                return None;
            }
            hdr.payload_size = annexb_obu_length - hdr.header_size;
            hdr.header_size += annexb_uleb_length;

            if hdr.has_size_field {
                let off = hdr.header_size as usize;
                if off >= data.len() {
                    return None;
                }
                let (obu_size, size_field_len) = Self::read_obu_size(&data[off..])?;
                hdr.header_size += size_field_len;
                hdr.payload_size = obu_size;
            }
        } else {
            let off = hdr.header_size as usize;
            if off >= data.len() {
                return None;
            }
            let (obu_size, size_field_len) = Self::read_obu_size(&data[off..])?;
            hdr.payload_size = obu_size;
            hdr.header_size += size_field_len;
        }

        Some(hdr)
    }

    // -----------------------------------------------------------------------
    // Bitstream element readers
    // -----------------------------------------------------------------------

    /// Read UVLC (unsigned variable length code).
    fn read_uvlc(bs: &mut BitstreamReader) -> u32 {
        let mut lz = 0u32;
        while bs.u(1) == 0 {
            lz += 1;
            if lz >= 32 {
                return BIT32_MAX;
            }
        }
        if lz >= 32 {
            return BIT32_MAX;
        }
        let v = bs.u(lz);
        v + (1 << lz) - 1
    }

    /// Read a signed value of `bits+1` total bits.
    fn read_signed_bits(bs: &mut BitstreamReader, bits: u32) -> i32 {
        let nbits = 32 - bits as i32 - 1;
        let v = (bs.u(bits + 1) as i32) << nbits;
        v >> nbits
    }

    /// Read delta_q: if flag bit is 1, read a signed value; else 0.
    fn read_delta_q(bs: &mut BitstreamReader, bits: u32) -> i32 {
        if bs.u(1) != 0 {
            Self::read_signed_bits(bs, bits)
        } else {
            0
        }
    }

    /// Read ns() (non-symmetric) / SwGetUniform.
    fn sw_get_uniform(bs: &mut BitstreamReader, max_value: u32) -> u32 {
        let w = floor_log2(max_value) + 1;
        let m = (1u32 << w) - max_value;
        let v = bs.u(w - 1);
        if v < m {
            v
        } else {
            let extra_bit = bs.u(1);
            (v << 1) - m + extra_bit
        }
    }

    // -----------------------------------------------------------------------
    // Global motion sub-expression readers (from VulkanAV1GlobalMotionDec.cpp)
    // -----------------------------------------------------------------------

    fn read_primitive_quniform(bs: &mut BitstreamReader, n: u16) -> u16 {
        if n <= 1 {
            return 0;
        }
        let l = get_msb((n - 1) as u32) + 1;
        let m = (1i32 << l) - n as i32;
        let v = bs.u(l as u32 - 1) as i32;
        if v < m {
            v as u16
        } else {
            ((v << 1) - m + bs.u(1) as i32) as u16
        }
    }

    fn read_primitive_subexpfin(bs: &mut BitstreamReader, n: u16, k: u16) -> u16 {
        let mut i: u16 = 0;
        let mut mk: u16 = 0;

        loop {
            let b = if i != 0 { k + i - 1 } else { k };
            let a = 1u16 << b;

            if n <= mk + 3 * a {
                return Self::read_primitive_quniform(bs, n - mk) + mk;
            }

            if bs.u(1) == 0 {
                return bs.u(b as u32) as u16 + mk;
            }

            i += 1;
            mk += a;
        }
    }

    fn read_primitive_refsubexpfin(bs: &mut BitstreamReader, n: u16, k: u16, reference: u16) -> u16 {
        inv_recenter_finite_nonneg(n, reference, Self::read_primitive_subexpfin(bs, n, k))
    }

    fn read_signed_primitive_refsubexpfin(
        bs: &mut BitstreamReader,
        n: u16,
        k: u16,
        reference: i16,
    ) -> i16 {
        let ref_val = reference + n as i16 - 1;
        let scaled_n = (n << 1) - 1;
        Self::read_primitive_refsubexpfin(bs, scaled_n, k, ref_val as u16) as i16 - n as i16 + 1
    }

    /// Read global motion parameters for a single reference frame.
    ///
    /// Corresponds to `ReadGlobalMotionParams` in the C++ source.
    /// Matches the C++ logic exactly.
    fn read_gm_params(
        bs: &mut BitstreamReader,
        ref_params: &Av1WarpedMotionParams,
        allow_hp: bool,
    ) -> Av1WarpedMotionParams {
        let mut params = default_warp_params();

        // Determine transformation type
        let type_bit = bs.u(1);
        let gm_type = if type_bit == 0 {
            Av1TransformationType::Identity
        } else if bs.u(1) != 0 {
            Av1TransformationType::Rotzoom
        } else if bs.u(1) != 0 {
            Av1TransformationType::Translation
        } else {
            Av1TransformationType::Affine
        };

        params.wmtype = gm_type;

        if gm_type as u32 >= Av1TransformationType::Rotzoom as u32 {
            params.wmmat[2] = Self::read_signed_primitive_refsubexpfin(
                bs,
                GM_ALPHA_MAX as u16 + 1,
                SUBEXPFIN_K,
                ((ref_params.wmmat[2] >> GM_ALPHA_PREC_DIFF) - (1 << GM_ALPHA_PREC_BITS)) as i16,
            ) as i32
                * GM_ALPHA_DECODE_FACTOR
                + (1 << WARPEDMODEL_PREC_BITS);

            params.wmmat[3] = Self::read_signed_primitive_refsubexpfin(
                bs,
                GM_ALPHA_MAX as u16 + 1,
                SUBEXPFIN_K,
                (ref_params.wmmat[3] >> GM_ALPHA_PREC_DIFF) as i16,
            ) as i32
                * GM_ALPHA_DECODE_FACTOR;
        }

        if gm_type as u32 >= Av1TransformationType::Affine as u32 {
            params.wmmat[4] = Self::read_signed_primitive_refsubexpfin(
                bs,
                GM_ALPHA_MAX as u16 + 1,
                SUBEXPFIN_K,
                (ref_params.wmmat[4] >> GM_ALPHA_PREC_DIFF) as i16,
            ) as i32
                * GM_ALPHA_DECODE_FACTOR;

            params.wmmat[5] = Self::read_signed_primitive_refsubexpfin(
                bs,
                GM_ALPHA_MAX as u16 + 1,
                SUBEXPFIN_K,
                ((ref_params.wmmat[5] >> GM_ALPHA_PREC_DIFF) - (1 << GM_ALPHA_PREC_BITS)) as i16,
            ) as i32
                * GM_ALPHA_DECODE_FACTOR
                + (1 << WARPEDMODEL_PREC_BITS);
        } else {
            params.wmmat[4] = -params.wmmat[3];
            params.wmmat[5] = params.wmmat[2];
        }

        if gm_type as u32 >= Av1TransformationType::Translation as u32 {
            let allow_hp_i = if allow_hp { 1i32 } else { 0 };
            let trans_bits = if gm_type == Av1TransformationType::Translation {
                GM_ABS_TRANS_ONLY_BITS - (1 - allow_hp_i)
            } else {
                GM_ABS_TRANS_BITS
            };

            let trans_dec_factor = if gm_type == Av1TransformationType::Translation {
                GM_TRANS_ONLY_DECODE_FACTOR * (1 << if allow_hp { 0 } else { 1 })
            } else {
                GM_TRANS_DECODE_FACTOR
            };

            let trans_prec_diff = if gm_type == Av1TransformationType::Translation {
                GM_TRANS_ONLY_PREC_DIFF + (1 - allow_hp_i)
            } else {
                GM_TRANS_PREC_DIFF
            };

            params.wmmat[0] = Self::read_signed_primitive_refsubexpfin(
                bs,
                ((1i32 << trans_bits) + 1) as u16,
                SUBEXPFIN_K,
                (ref_params.wmmat[0] >> trans_prec_diff) as i16,
            ) as i32
                * trans_dec_factor;

            params.wmmat[1] = Self::read_signed_primitive_refsubexpfin(
                bs,
                ((1i32 << trans_bits) + 1) as u16,
                SUBEXPFIN_K,
                (ref_params.wmmat[1] >> trans_prec_diff) as i16,
            ) as i32
                * trans_dec_factor;
        }

        params
    }

    /// Decode global motion parameters for all reference frames.
    ///
    /// Corresponds to `DecodeGlobalMotionParams` in the C++ source.
    pub fn decode_global_motion_params(&mut self, bs: &mut BitstreamReader) {
        let mut prev_models: [Av1WarpedMotionParams; GM_GLOBAL_MODELS_PER_FRAME] =
            std::array::from_fn(|_| default_warp_params());

        if self.pic_data.std_info.primary_ref_frame != PRIMARY_REF_NONE {
            let prim_idx = self.pic_data.std_info.primary_ref_frame as usize;
            if prim_idx < REFS_PER_FRAME {
                let buf_idx = self.ref_frame_idx[prim_idx];
                if buf_idx >= 0 && (buf_idx as usize) < BUFFER_POOL_MAX_SIZE {
                    let buf = &self.buffers[buf_idx as usize];
                    if buf.buffer_valid {
                        for i in 0..GM_GLOBAL_MODELS_PER_FRAME {
                            prev_models[i] = buf.global_models[i].clone();
                        }
                    }
                }
            }
        }

        let allow_hp = self.pic_data.std_info.flags.allow_high_precision_mv;
        for frame in 0..GM_GLOBAL_MODELS_PER_FRAME {
            let ref_params = &prev_models[frame];
            let params = Self::read_gm_params(bs, ref_params, allow_hp);
            self.global_motions[frame] = params;
        }
    }

    // -----------------------------------------------------------------------
    // Sequence header parsing
    // -----------------------------------------------------------------------

    /// Parse timing info header.
    fn read_timing_info_header(&mut self, bs: &mut BitstreamReader) {
        self.timing_info.num_units_in_display_tick = bs.u(32);
        self.timing_info.time_scale = bs.u(32);
        self.timing_info.equal_picture_interval = bs.u(1) != 0;
        if self.timing_info.equal_picture_interval {
            self.timing_info.num_ticks_per_picture = Self::read_uvlc(bs) + 1;
        }
    }

    /// Parse decoder model info.
    fn read_decoder_model_info(&mut self, bs: &mut BitstreamReader) {
        self.buffer_model.encoder_decoder_buffer_delay_length = bs.u(5) as i32 + 1;
        self.buffer_model.num_units_in_decoding_tick = bs.u(32);
        self.buffer_model.buffer_removal_time_length = bs.u(5) as i32 + 1;
        self.buffer_model.frame_presentation_time_length = bs.u(5) as i32 + 1;
    }

    /// Parse the OBU sequence header.
    ///
    /// Corresponds to `ParseObuSequenceHeader` in the C++ source.
    pub fn parse_obu_sequence_header(&mut self, bs: &mut BitstreamReader) -> bool {
        let mut sps = Av1SequenceHeader::default();

        sps.seq_profile = bs.u(3);
        if sps.seq_profile > Av1Profile::Professional as u32 {
            return false;
        }

        sps.still_picture = bs.u(1) != 0;
        sps.reduced_still_picture_header = bs.u(1) != 0;

        if !sps.still_picture && sps.reduced_still_picture_header {
            return false;
        }

        if sps.reduced_still_picture_header {
            self.timing_info_present = false;
            sps.decoder_model_info_present = false;
            sps.display_model_info_present = false;
            sps.operating_points_cnt_minus_1 = 0;
            sps.operating_point_idc[0] = 0;
            sps.level[0] = bs.u(5);
            sps.tier[0] = 0;
            self.op_params[0].decoder_model_param_present = false;
            self.op_params[0].display_model_param_present = false;
        } else {
            self.timing_info_present = bs.u(1) != 0;
            if self.timing_info_present {
                self.read_timing_info_header(bs);
                sps.decoder_model_info_present = bs.u(1) != 0;
                if sps.decoder_model_info_present {
                    self.read_decoder_model_info(bs);
                }
            } else {
                sps.decoder_model_info_present = false;
            }

            sps.display_model_info_present = bs.u(1) != 0;
            sps.operating_points_cnt_minus_1 = bs.u(5) as i32;

            for i in 0..=(sps.operating_points_cnt_minus_1 as usize) {
                sps.operating_point_idc[i] = bs.u(12) as i32;
                sps.level[i] = bs.u(5);

                if sps.level[i] > 7 {
                    // level > 3.3
                    sps.tier[i] = bs.u(1) as u8;
                } else {
                    sps.tier[i] = 0;
                }

                if sps.decoder_model_info_present {
                    self.op_params[i].decoder_model_param_present = bs.u(1) != 0;
                    if self.op_params[i].decoder_model_param_present {
                        let n = self.buffer_model.encoder_decoder_buffer_delay_length as u32;
                        self.op_params[i].decoder_buffer_delay = bs.u(n) as i32;
                        self.op_params[i].encoder_buffer_delay = bs.u(n) as i32;
                        self.op_params[i].low_delay_mode_flag = bs.u(1) as i32;
                    }
                } else {
                    self.op_params[i].decoder_model_param_present = false;
                }

                if sps.display_model_info_present {
                    self.op_params[i].display_model_param_present = bs.u(1) != 0;
                    if self.op_params[i].display_model_param_present {
                        self.op_params[i].initial_display_delay = bs.u(4) as i32 + 1;
                    } else {
                        self.op_params[i].initial_display_delay = 10;
                    }
                } else {
                    self.op_params[i].display_model_param_present = false;
                    self.op_params[i].initial_display_delay = 10;
                }
            }
        }

        sps.frame_width_bits_minus_1 = bs.u(4);
        sps.frame_height_bits_minus_1 = bs.u(4);
        sps.max_frame_width_minus_1 = bs.u(sps.frame_width_bits_minus_1 + 1);
        sps.max_frame_height_minus_1 = bs.u(sps.frame_height_bits_minus_1 + 1);

        if sps.reduced_still_picture_header {
            sps.frame_id_numbers_present_flag = false;
        } else {
            sps.frame_id_numbers_present_flag = bs.u(1) != 0;
        }

        if sps.frame_id_numbers_present_flag {
            self.delta_frame_id_length = bs.u(4) as u8 + 2;
            self.frame_id_length = bs.u(3) as u8 + self.delta_frame_id_length + 1;
            if self.frame_id_length > 16 {
                return false;
            }
        }

        sps.use_128x128_superblock = bs.u(1) != 0;
        sps.enable_filter_intra = bs.u(1) != 0;
        sps.enable_intra_edge_filter = bs.u(1) != 0;

        if sps.reduced_still_picture_header {
            sps.enable_interintra_compound = false;
            sps.enable_masked_compound = false;
            sps.enable_warped_motion = false;
            sps.enable_dual_filter = false;
            sps.enable_order_hint = false;
            sps.enable_jnt_comp = false;
            sps.enable_ref_frame_mvs = false;
            sps.seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS;
            sps.seq_force_integer_mv = SELECT_INTEGER_MV;
            sps.order_hint_bits_minus_1 = 0;
        } else {
            sps.enable_interintra_compound = bs.u(1) != 0;
            sps.enable_masked_compound = bs.u(1) != 0;
            sps.enable_warped_motion = bs.u(1) != 0;
            sps.enable_dual_filter = bs.u(1) != 0;
            sps.enable_order_hint = bs.u(1) != 0;

            if sps.enable_order_hint {
                sps.enable_jnt_comp = bs.u(1) != 0;
                sps.enable_ref_frame_mvs = bs.u(1) != 0;
            } else {
                sps.enable_jnt_comp = false;
                sps.enable_ref_frame_mvs = false;
            }

            if bs.u(1) != 0 {
                sps.seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS;
            } else {
                sps.seq_force_screen_content_tools = bs.u(1);
            }

            if sps.seq_force_screen_content_tools > 0 {
                if bs.u(1) != 0 {
                    sps.seq_force_integer_mv = SELECT_INTEGER_MV;
                } else {
                    sps.seq_force_integer_mv = bs.u(1);
                }
            } else {
                sps.seq_force_integer_mv = SELECT_INTEGER_MV;
            }

            sps.order_hint_bits_minus_1 = if sps.enable_order_hint { bs.u(3) } else { 0 };
        }

        sps.enable_superres = bs.u(1) != 0;
        sps.enable_cdef = bs.u(1) != 0;
        sps.enable_restoration = bs.u(1) != 0;

        // Color config
        let high_bitdepth = bs.u(1) != 0;
        if sps.seq_profile == Av1Profile::Professional as u32 && high_bitdepth {
            let twelve_bit = bs.u(1) != 0;
            sps.color_config.bit_depth = if twelve_bit { 12 } else { 10 };
        } else if sps.seq_profile <= Av1Profile::Professional as u32 {
            sps.color_config.bit_depth = if high_bitdepth { 10 } else { 8 };
        } else {
            return false;
        }

        sps.color_config.mono_chrome =
            if sps.seq_profile != Av1Profile::High as u32 { bs.u(1) != 0 } else { false };
        sps.color_config.color_description_present_flag = bs.u(1) != 0;

        if sps.color_config.color_description_present_flag {
            sps.color_config.color_primaries = bs.u(8) as u8;
            sps.color_config.transfer_characteristics = bs.u(8) as u8;
            sps.color_config.matrix_coefficients = bs.u(8) as u8;
        } else {
            sps.color_config.color_primaries = 2; // UNSPECIFIED
            sps.color_config.transfer_characteristics = 2;
            sps.color_config.matrix_coefficients = 2;
        }

        if sps.color_config.mono_chrome {
            sps.color_config.color_range = bs.u(1) != 0;
            sps.color_config.subsampling_x = 1;
            sps.color_config.subsampling_y = 1;
            sps.color_config.separate_uv_delta_q = false;
        } else if sps.color_config.color_primaries == 1
            && sps.color_config.transfer_characteristics == 13
            && sps.color_config.matrix_coefficients == 0
        {
            // sRGB
            sps.color_config.subsampling_x = 0;
            sps.color_config.subsampling_y = 0;
            sps.color_config.color_range = true;
        } else {
            sps.color_config.color_range = bs.u(1) != 0;
            if sps.seq_profile == Av1Profile::Main as u32 {
                sps.color_config.subsampling_x = 1;
                sps.color_config.subsampling_y = 1;
            } else if sps.seq_profile == Av1Profile::High as u32 {
                sps.color_config.subsampling_x = 0;
                sps.color_config.subsampling_y = 0;
            } else {
                // Professional
                if sps.color_config.bit_depth == 12 {
                    sps.color_config.subsampling_x = bs.u(1) as u8;
                    if sps.color_config.subsampling_x != 0 {
                        sps.color_config.subsampling_y = bs.u(1) as u8;
                    } else {
                        sps.color_config.subsampling_y = 0;
                    }
                } else {
                    sps.color_config.subsampling_x = 1;
                    sps.color_config.subsampling_y = 0;
                }
            }
            if sps.color_config.subsampling_x != 0 && sps.color_config.subsampling_y != 0 {
                sps.color_config.chroma_sample_position = bs.u(2) as u8;
            }

            sps.color_config.separate_uv_delta_q = bs.u(1) != 0;
        }

        sps.film_grain_params_present = bs.u(1) != 0;

        // check_trailing_bits
        let bits_before_byte_alignment = 8 - (bs.consumed_bits() % 8);
        let trailing = bs.u(bits_before_byte_alignment as u32);
        if trailing != (1 << (bits_before_byte_alignment - 1)) as u32 {
            return false;
        }

        if self.sps_received {
            // Simplified difference check
            self.sps_changed = true;
        } else {
            self.sps_changed = true;
        }

        self.sps_received = true;
        self.sps = Some(sps);

        true
    }

    // -----------------------------------------------------------------------
    // Frame size
    // -----------------------------------------------------------------------

    /// Parse frame size.
    pub fn setup_frame_size(&mut self, bs: &mut BitstreamReader, frame_size_override_flag: bool) {
        let sps = self.sps.as_ref().expect("SPS required");

        if frame_size_override_flag {
            self.frame_width = bs.u(sps.frame_width_bits_minus_1 + 1) as u16 + 1;
            self.frame_height = bs.u(sps.frame_height_bits_minus_1 + 1) as u16 + 1;
        } else {
            self.frame_width = sps.max_frame_width_minus_1 as u16 + 1;
            self.frame_height = sps.max_frame_height_minus_1 as u16 + 1;
        }

        // superres_params
        self.upscaled_width = self.frame_width;
        self.pic_data.std_info.coded_denom = 0;
        let mut superres_scale_denominator: u32 = 8;
        self.pic_data.std_info.flags.use_superres = false;

        if sps.enable_superres && bs.u(1) != 0 {
            self.pic_data.std_info.flags.use_superres = true;
            let denom_coded = bs.u(3);
            self.pic_data.std_info.coded_denom = denom_coded as u8;
            superres_scale_denominator = denom_coded + SUPERRES_DENOM_MIN;
            self.frame_width = ((self.upscaled_width as u32 * SUPERRES_NUM
                + superres_scale_denominator / 2)
                / superres_scale_denominator) as u16;
        }

        // render size
        self.pic_data.std_info.flags.render_and_frame_size_different = bs.u(1) != 0;
        if self.pic_data.std_info.flags.render_and_frame_size_different {
            self.render_width = bs.u(16) as i32 + 1;
            self.render_height = bs.u(16) as i32 + 1;
        } else {
            self.render_width = self.upscaled_width as i32;
            self.render_height = self.frame_height as i32;
        }
    }

    /// Parse frame size with refs.
    pub fn setup_frame_size_with_refs(&mut self, bs: &mut BitstreamReader) -> bool {
        let sps = self.sps.as_ref().expect("SPS required");
        let enable_superres = sps.enable_superres;

        let mut found = false;
        for _i in 0..REFS_PER_FRAME {
            if bs.u(1) != 0 {
                found = true;
                // In full implementation, would get dimensions from reference buffer.
                // For now, leave as-is (frame_width/height from ref is not available
                // without the full buffer management with VkPicIf).
                break;
            }
        }

        if !found {
            self.setup_frame_size(bs, true);
        } else {
            // superres_params
            let mut superres_scale_denominator: u32 = SUPERRES_NUM;
            self.pic_data.std_info.coded_denom = 0;
            self.pic_data.std_info.flags.use_superres = false;

            if enable_superres && bs.u(1) != 0 {
                self.pic_data.std_info.flags.use_superres = true;
                let denom_coded = bs.u(SUPERRES_DENOM_BITS);
                self.pic_data.std_info.coded_denom = denom_coded as u8;
                superres_scale_denominator = denom_coded + SUPERRES_DENOM_MIN;
            }

            self.frame_width = ((self.upscaled_width as u32 * SUPERRES_NUM
                + superres_scale_denominator / 2)
                / superres_scale_denominator) as u16;
        }

        true
    }

    // -----------------------------------------------------------------------
    // Tile info
    // -----------------------------------------------------------------------

    /// Parse tile info.
    pub fn decode_tile_info(&mut self, bs: &mut BitstreamReader) -> bool {
        let sps = self.sps.as_ref().expect("SPS required");
        let use_128x128_superblock = sps.use_128x128_superblock;

        let mi_cols = 2 * ((self.frame_width as u32 + 7) >> 3);
        let mi_rows = 2 * ((self.frame_height as u32 + 7) >> 3);

        let sb_cols = if use_128x128_superblock {
            (mi_cols + 31) >> 5
        } else {
            (mi_cols + 15) >> 4
        };
        let sb_rows = if use_128x128_superblock {
            (mi_rows + 31) >> 5
        } else {
            (mi_rows + 15) >> 4
        };
        let num_superblocks = sb_cols * sb_rows;
        let sb_shift: u32 = if use_128x128_superblock { 5 } else { 4 };
        let sb_size = sb_shift + 2;

        let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
        let max_tile_area_sb_init = MAX_TILE_AREA >> (2 * sb_size);
        let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
        let max_log2_tile_cols = tile_log2(1, sb_cols.min(MAX_TILE_COLS as u32));
        let max_log2_tile_rows = tile_log2(1, sb_rows.min(MAX_TILE_ROWS as u32));
        let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb_init, sb_rows * sb_cols));

        self.pic_data.tile_info.uniform_tile_spacing_flag = bs.u(1) != 0;
        self.pic_data.mi_col_starts = [0; MAX_TILE_COLS + 1];
        self.pic_data.mi_row_starts = [0; MAX_TILE_ROWS + 1];
        self.pic_data.width_in_sbs_minus_1 = [0; MAX_TILE_COLS];
        self.pic_data.height_in_sbs_minus_1 = [0; MAX_TILE_ROWS];

        if self.pic_data.tile_info.uniform_tile_spacing_flag {
            self.log2_tile_cols = min_log2_tile_cols;
            while self.log2_tile_cols < max_log2_tile_cols {
                if bs.u(1) == 0 {
                    break;
                }
                self.log2_tile_cols += 1;
            }

            let tile_width_sb = (sb_cols + (1 << self.log2_tile_cols) - 1) >> self.log2_tile_cols;
            self.pic_data.tile_info.tile_cols = (sb_cols + tile_width_sb - 1) / tile_width_sb;

            let min_log2_tile_rows_val = if min_log2_tiles > self.log2_tile_cols {
                min_log2_tiles - self.log2_tile_cols
            } else {
                0
            };
            self.log2_tile_rows = min_log2_tile_rows_val;
            while self.log2_tile_rows < max_log2_tile_rows {
                if bs.u(1) == 0 {
                    break;
                }
                self.log2_tile_rows += 1;
            }

            let tile_height_sb = (sb_rows + (1 << self.log2_tile_rows) - 1) >> self.log2_tile_rows;
            self.pic_data.tile_info.tile_rows = (sb_rows + tile_height_sb - 1) / tile_height_sb;

            // Derive width_in_sbs_minus_1
            let tc = self.pic_data.tile_info.tile_cols;
            for col in 0..(tc - 1) {
                self.pic_data.width_in_sbs_minus_1[col as usize] = tile_width_sb - 1;
            }
            self.pic_data.width_in_sbs_minus_1[(tc - 1) as usize] =
                sb_cols - (tc - 1) * tile_width_sb - 1;

            let tr = self.pic_data.tile_info.tile_rows;
            for row in 0..(tr - 1) {
                self.pic_data.height_in_sbs_minus_1[row as usize] = tile_height_sb - 1;
            }
            self.pic_data.height_in_sbs_minus_1[(tr - 1) as usize] =
                sb_rows - (tr - 1) * tile_height_sb - 1;

            // Derive superblock column/row starts
            let mut idx = 0usize;
            let mut start_sb = 0u32;
            while start_sb < sb_cols {
                self.pic_data.mi_col_starts[idx] = start_sb;
                start_sb += tile_width_sb;
                idx += 1;
            }
            self.pic_data.mi_col_starts[idx] = sb_cols;

            idx = 0;
            start_sb = 0;
            while start_sb < sb_rows {
                self.pic_data.mi_row_starts[idx] = start_sb;
                start_sb += tile_height_sb;
                idx += 1;
            }
            self.pic_data.mi_row_starts[idx] = sb_rows;
        } else {
            // Non-uniform tile spacing
            let mut widest_tile_sb = 0u32;
            let mut start_sb = 0u32;
            let mut i = 0usize;

            while start_sb < sb_cols && i < MAX_TILE_COLS {
                self.pic_data.mi_col_starts[i] = start_sb;
                let max_width = (sb_cols - start_sb).min(max_tile_width_sb);
                self.pic_data.width_in_sbs_minus_1[i] =
                    if max_width > 1 { Self::sw_get_uniform(bs, max_width) } else { 0 };
                let size_sb = self.pic_data.width_in_sbs_minus_1[i] + 1;
                widest_tile_sb = widest_tile_sb.max(size_sb);
                start_sb += size_sb;
                i += 1;
            }
            self.log2_tile_cols = tile_log2(1, i as u32);
            self.pic_data.tile_info.tile_cols = i as u32;

            let mut max_tile_area_sb = if min_log2_tiles > 0 {
                num_superblocks >> (min_log2_tiles + 1)
            } else {
                num_superblocks
            };
            let max_tile_height_sb = (max_tile_area_sb / widest_tile_sb).max(1);

            start_sb = 0;
            i = 0;
            while start_sb < sb_rows && i < MAX_TILE_ROWS {
                self.pic_data.mi_row_starts[i] = start_sb;
                let max_height = (sb_rows - start_sb).min(max_tile_height_sb);
                self.pic_data.height_in_sbs_minus_1[i] =
                    if max_height > 1 { Self::sw_get_uniform(bs, max_height) } else { 0 };
                let size_sb = self.pic_data.height_in_sbs_minus_1[i] + 1;
                start_sb += size_sb;
                i += 1;
            }
            self.log2_tile_rows = tile_log2(1, i as u32);
            self.pic_data.tile_info.tile_rows = i as u32;
        }

        self.pic_data.tile_info.context_update_tile_id = 0;
        self.tile_size_bytes_minus_1 = 3;
        let num_tiles = self.pic_data.tile_info.tile_rows * self.pic_data.tile_info.tile_cols;
        if num_tiles > 1 {
            self.pic_data.tile_info.context_update_tile_id =
                bs.u(self.log2_tile_rows + self.log2_tile_cols);
            self.tile_size_bytes_minus_1 = bs.u(2) as u8;
            self.pic_data.tile_info.tile_size_bytes_minus_1 = self.tile_size_bytes_minus_1;
        }

        true
    }

    // -----------------------------------------------------------------------
    // Quantization
    // -----------------------------------------------------------------------

    pub fn decode_quantization_data(&mut self, bs: &mut BitstreamReader) {
        let sps = self.sps.as_ref().expect("SPS required");
        let mono_chrome = sps.color_config.mono_chrome;
        let separate_uv_delta_q = sps.color_config.separate_uv_delta_q;

        self.pic_data.quantization.base_q_idx = bs.u(8);
        self.pic_data.quantization.delta_q_y_dc = Self::read_delta_q(bs, 6);

        if !mono_chrome {
            let diff_uv_delta = if separate_uv_delta_q { bs.u(1) != 0 } else { false };
            self.pic_data.quantization.delta_q_u_dc = Self::read_delta_q(bs, 6);
            self.pic_data.quantization.delta_q_u_ac = Self::read_delta_q(bs, 6);
            if diff_uv_delta {
                self.pic_data.quantization.delta_q_v_dc = Self::read_delta_q(bs, 6);
                self.pic_data.quantization.delta_q_v_ac = Self::read_delta_q(bs, 6);
            } else {
                self.pic_data.quantization.delta_q_v_dc = self.pic_data.quantization.delta_q_u_dc;
                self.pic_data.quantization.delta_q_v_ac = self.pic_data.quantization.delta_q_u_ac;
            }
        }

        self.pic_data.quantization.using_qmatrix = bs.u(1) != 0;
        if self.pic_data.quantization.using_qmatrix {
            self.pic_data.quantization.qm_y = bs.u(4) as u8;
            self.pic_data.quantization.qm_u = bs.u(4) as u8;
            if !separate_uv_delta_q {
                self.pic_data.quantization.qm_v = self.pic_data.quantization.qm_u;
            } else {
                self.pic_data.quantization.qm_v = bs.u(4) as u8;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Segmentation
    // -----------------------------------------------------------------------

    pub fn decode_segmentation_data(&mut self, bs: &mut BitstreamReader) {
        let flags = &mut self.pic_data.std_info.flags;
        flags.segmentation_enabled = bs.u(1) != 0;

        if !flags.segmentation_enabled {
            self.pic_data.segmentation = Av1Segmentation::default();
            return;
        }

        let primary_ref_frame = self.pic_data.std_info.primary_ref_frame;

        if primary_ref_frame == PRIMARY_REF_NONE {
            self.pic_data.std_info.flags.segmentation_update_map = true;
            self.pic_data.std_info.flags.segmentation_update_data = true;
            self.pic_data.std_info.flags.segmentation_temporal_update = false;
        } else {
            self.pic_data.std_info.flags.segmentation_update_map = bs.u(1) != 0;
            if self.pic_data.std_info.flags.segmentation_update_map {
                self.pic_data.std_info.flags.segmentation_temporal_update = bs.u(1) != 0;
            } else {
                self.pic_data.std_info.flags.segmentation_temporal_update = false;
            }
            self.pic_data.std_info.flags.segmentation_update_data = bs.u(1) != 0;
        }

        if self.pic_data.std_info.flags.segmentation_update_data {
            for i in 0..MAX_SEGMENTS {
                self.pic_data.segmentation.feature_enabled[i] = 0;
                for j in 0..SEG_LVL_MAX {
                    let mut feature_value: i32 = 0;
                    let enabled = bs.u(1) != 0;
                    if enabled {
                        self.pic_data.segmentation.feature_enabled[i] |= 1 << j;
                    }
                    if enabled {
                        let data_max = SEG_FEATURE_DATA_MAX[j];
                        if SEG_FEATURE_DATA_SIGNED[j] {
                            feature_value = Self::read_signed_bits(bs, SEG_FEATURE_BITS[j]);
                            feature_value = clamp(feature_value, -data_max, data_max);
                        } else {
                            feature_value = bs.u(SEG_FEATURE_BITS[j]) as i32;
                            feature_value = clamp(feature_value, 0, data_max);
                        }
                    }
                    self.pic_data.segmentation.feature_data[i][j] = feature_value as i16;
                }
            }
        } else if primary_ref_frame != PRIMARY_REF_NONE {
            let prim_buf_idx = self.ref_frame_idx[primary_ref_frame as usize];
            if prim_buf_idx >= 0 && (prim_buf_idx as usize) < BUFFER_POOL_MAX_SIZE {
                let buf = &self.buffers[prim_buf_idx as usize];
                if buf.buffer_valid {
                    self.pic_data.segmentation.feature_enabled = buf.seg_feature_enabled;
                    self.pic_data.segmentation.feature_data = buf.seg_feature_data;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Loop filter
    // -----------------------------------------------------------------------

    pub fn decode_loop_filter_data(&mut self, bs: &mut BitstreamReader) {
        let sps = self.sps.as_ref().expect("SPS required");
        let mono_chrome = sps.color_config.mono_chrome;

        self.pic_data.loop_filter.loop_filter_level[2] = 0;
        self.pic_data.loop_filter.loop_filter_level[3] = 0;
        self.pic_data.loop_filter.loop_filter_ref_deltas = LF_REF_DELTA_DEFAULT;
        self.pic_data.loop_filter.loop_filter_mode_deltas = [0; LOOP_FILTER_ADJUSTMENTS];

        if self.pic_data.std_info.flags.allow_intrabc || self.coded_lossless {
            self.pic_data.loop_filter.loop_filter_level[0] = 0;
            self.pic_data.loop_filter.loop_filter_level[1] = 0;
            return;
        }

        let primary_ref_frame = self.pic_data.std_info.primary_ref_frame;
        if primary_ref_frame != PRIMARY_REF_NONE {
            let prim_buf_idx = self.ref_frame_idx[primary_ref_frame as usize];
            if prim_buf_idx >= 0 && (prim_buf_idx as usize) < BUFFER_POOL_MAX_SIZE {
                let buf = &self.buffers[prim_buf_idx as usize];
                if buf.buffer_valid {
                    self.pic_data.loop_filter.loop_filter_ref_deltas = buf.lf_ref_delta;
                    self.pic_data.loop_filter.loop_filter_mode_deltas = buf.lf_mode_delta;
                }
            }
        }

        self.pic_data.loop_filter.loop_filter_level[0] = bs.u(6) as u8;
        self.pic_data.loop_filter.loop_filter_level[1] = bs.u(6) as u8;
        if !mono_chrome
            && (self.pic_data.loop_filter.loop_filter_level[0] != 0
                || self.pic_data.loop_filter.loop_filter_level[1] != 0)
        {
            self.pic_data.loop_filter.loop_filter_level[2] = bs.u(6) as u8;
            self.pic_data.loop_filter.loop_filter_level[3] = bs.u(6) as u8;
        }
        self.pic_data.loop_filter.loop_filter_sharpness = bs.u(3) as u8;

        self.pic_data.loop_filter.loop_filter_delta_enabled = bs.u(1) != 0;
        if self.pic_data.loop_filter.loop_filter_delta_enabled {
            let lf_mode_ref_delta_update = bs.u(1) != 0;
            self.pic_data.loop_filter.loop_filter_delta_update = lf_mode_ref_delta_update;
            if lf_mode_ref_delta_update {
                for i in 0..TOTAL_REFS_PER_FRAME {
                    if bs.u(1) != 0 {
                        self.pic_data.loop_filter.loop_filter_ref_deltas[i] =
                            Self::read_signed_bits(bs, 6) as i8;
                    }
                }
                for i in 0..LOOP_FILTER_ADJUSTMENTS {
                    if bs.u(1) != 0 {
                        self.pic_data.loop_filter.loop_filter_mode_deltas[i] =
                            Self::read_signed_bits(bs, 6) as i8;
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // CDEF
    // -----------------------------------------------------------------------

    pub fn decode_cdef_data(&mut self, bs: &mut BitstreamReader) {
        let sps = self.sps.as_ref().expect("SPS required");
        let mono_chrome = sps.color_config.mono_chrome;

        if self.pic_data.std_info.flags.allow_intrabc {
            return;
        }

        self.pic_data.cdef.cdef_damping_minus_3 = bs.u(2) as u8;
        self.pic_data.cdef.cdef_bits = bs.u(2) as u8;

        for i in 0..8usize {
            if i == (1 << self.pic_data.cdef.cdef_bits) as usize {
                break;
            }
            self.pic_data.cdef.cdef_y_pri_strength[i] = bs.u(4) as u8;
            self.pic_data.cdef.cdef_y_sec_strength[i] = bs.u(2) as u8;
            if !mono_chrome {
                self.pic_data.cdef.cdef_uv_pri_strength[i] = bs.u(4) as u8;
                self.pic_data.cdef.cdef_uv_sec_strength[i] = bs.u(2) as u8;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Loop restoration
    // -----------------------------------------------------------------------

    pub fn decode_loop_restoration_data(&mut self, bs: &mut BitstreamReader) {
        let sps = self.sps.as_ref().expect("SPS required");
        let mono_chrome = sps.color_config.mono_chrome;
        let use_128x128_superblock = sps.use_128x128_superblock;
        let subsampling_x = sps.color_config.subsampling_x;
        let subsampling_y = sps.color_config.subsampling_y;

        if self.pic_data.std_info.flags.allow_intrabc {
            return;
        }

        let n_planes = if mono_chrome { 1 } else { 3 };
        let mut use_lr = false;
        let mut use_chroma_lr = false;

        let remap_lr_type = [
            Av1FrameRestorationType::None,
            Av1FrameRestorationType::Switchable,
            Av1FrameRestorationType::Wiener,
            Av1FrameRestorationType::Sgrproj,
        ];

        for pl in 0..n_planes {
            let lr_type = bs.u(2) as usize;
            self.pic_data.loop_restoration.frame_restoration_type[pl] = remap_lr_type[lr_type];
            if self.pic_data.loop_restoration.frame_restoration_type[pl]
                != Av1FrameRestorationType::None
            {
                use_lr = true;
                if pl > 0 {
                    use_chroma_lr = true;
                }
            }
        }

        self.pic_data.std_info.flags.uses_lr = use_lr;
        let mut lr_uv_shift: u8 = 0;

        if use_lr {
            let sb_size: u8 = if use_128x128_superblock { 2 } else { 1 };
            for pl in 0..n_planes {
                self.pic_data.loop_restoration.loop_restoration_size[pl] = sb_size;
            }

            let lr_unit_shift = if use_128x128_superblock {
                1 + bs.u(1) as u8
            } else {
                let mut shift = bs.u(1) as u8;
                if shift != 0 {
                    shift += bs.u(1) as u8;
                }
                shift
            };
            self.pic_data.loop_restoration.loop_restoration_size[0] = 1 + lr_unit_shift;
        } else {
            for pl in 0..n_planes {
                self.pic_data.loop_restoration.loop_restoration_size[pl] = 3;
            }
        }

        if !mono_chrome {
            if use_chroma_lr && (subsampling_x != 0 && subsampling_y != 0) {
                lr_uv_shift = bs.u(1) as u8;
                self.pic_data.loop_restoration.loop_restoration_size[1] =
                    self.pic_data.loop_restoration.loop_restoration_size[0] - lr_uv_shift;
                self.pic_data.loop_restoration.loop_restoration_size[2] =
                    self.pic_data.loop_restoration.loop_restoration_size[1];
            } else {
                self.pic_data.loop_restoration.loop_restoration_size[1] =
                    self.pic_data.loop_restoration.loop_restoration_size[0];
                self.pic_data.loop_restoration.loop_restoration_size[2] =
                    self.pic_data.loop_restoration.loop_restoration_size[0];
            }
        }
        // C++ has a replicated assignment at the end:
        self.pic_data.loop_restoration.loop_restoration_size[1] =
            self.pic_data.loop_restoration.loop_restoration_size[0] >> lr_uv_shift;
    }

    // -----------------------------------------------------------------------
    // Film grain
    // -----------------------------------------------------------------------

    pub fn read_film_grain_params(&mut self, bs: &mut BitstreamReader) -> bool {
        let sps = self.sps.as_ref().expect("SPS required");
        let film_grain_params_present = sps.film_grain_params_present;
        let mono_chrome = sps.color_config.mono_chrome;
        let subsampling_x = sps.color_config.subsampling_x;
        let subsampling_y = sps.color_config.subsampling_y;

        if film_grain_params_present && (self.pic_data.show_frame || self.showable_frame) {
            self.pic_data.std_info.flags.delta_q_present = self.pic_data.std_info.flags.delta_q_present; // no-op, preserve
            self.pic_data.film_grain.apply_grain = bs.u(1) != 0;

            if !self.pic_data.film_grain.apply_grain {
                self.pic_data.film_grain = Av1FilmGrain::default();
                return true;
            }

            self.pic_data.film_grain.grain_seed = bs.u(16) as u16;
            let frame_type = self.pic_data.std_info.frame_type;
            self.pic_data.film_grain.update_grain =
                if frame_type == Av1FrameType::Inter { bs.u(1) != 0 } else { true };

            if !self.pic_data.film_grain.update_grain {
                let buf_idx = bs.u(3) as u8;
                let random_seed = self.pic_data.film_grain.grain_seed;
                if (buf_idx as usize) < BUFFER_POOL_MAX_SIZE
                    && self.buffers[buf_idx as usize].buffer_valid
                {
                    self.pic_data.film_grain =
                        self.buffers[buf_idx as usize].film_grain_params.clone();
                }
                self.pic_data.film_grain.grain_seed = random_seed;
                self.pic_data.film_grain.film_grain_params_ref_idx = buf_idx;
                return true;
            }

            // Scaling functions parameters
            self.pic_data.film_grain.num_y_points = bs.u(4) as u8;
            for i in 0..self.pic_data.film_grain.num_y_points as usize {
                self.pic_data.film_grain.point_y_value[i] = bs.u(8) as u8;
                self.pic_data.film_grain.point_y_scaling[i] = bs.u(8) as u8;
            }

            self.pic_data.film_grain.chroma_scaling_from_luma =
                if !mono_chrome { bs.u(1) != 0 } else { false };

            if mono_chrome
                || self.pic_data.film_grain.chroma_scaling_from_luma
                || (subsampling_x == 1
                    && subsampling_y == 1
                    && self.pic_data.film_grain.num_y_points == 0)
            {
                self.pic_data.film_grain.num_cb_points = 0;
                self.pic_data.film_grain.num_cr_points = 0;
            } else {
                self.pic_data.film_grain.num_cb_points = bs.u(4) as u8;
                for i in 0..self.pic_data.film_grain.num_cb_points as usize {
                    self.pic_data.film_grain.point_cb_value[i] = bs.u(8) as u8;
                    self.pic_data.film_grain.point_cb_scaling[i] = bs.u(8) as u8;
                }
                self.pic_data.film_grain.num_cr_points = bs.u(4) as u8;
                for i in 0..self.pic_data.film_grain.num_cr_points as usize {
                    self.pic_data.film_grain.point_cr_value[i] = bs.u(8) as u8;
                    self.pic_data.film_grain.point_cr_scaling[i] = bs.u(8) as u8;
                }
            }

            self.pic_data.film_grain.grain_scaling_minus_8 = bs.u(2) as u8;
            self.pic_data.film_grain.ar_coeff_lag = bs.u(2) as u8;

            let num_pos_luma =
                2 * self.pic_data.film_grain.ar_coeff_lag as usize
                    * (self.pic_data.film_grain.ar_coeff_lag as usize + 1);
            let mut num_pos_chroma = num_pos_luma;
            if self.pic_data.film_grain.num_y_points > 0 {
                num_pos_chroma += 1;
            }

            if self.pic_data.film_grain.num_y_points > 0 {
                for i in 0..num_pos_luma {
                    self.pic_data.film_grain.ar_coeffs_y_plus_128[i] = bs.u(8) as u8;
                }
            }

            if self.pic_data.film_grain.num_cb_points > 0
                || self.pic_data.film_grain.chroma_scaling_from_luma
            {
                for i in 0..num_pos_chroma {
                    self.pic_data.film_grain.ar_coeffs_cb_plus_128[i] = bs.u(8) as u8;
                }
            }

            if self.pic_data.film_grain.num_cr_points > 0
                || self.pic_data.film_grain.chroma_scaling_from_luma
            {
                for i in 0..num_pos_chroma {
                    self.pic_data.film_grain.ar_coeffs_cr_plus_128[i] = bs.u(8) as u8;
                }
            }

            self.pic_data.film_grain.ar_coeff_shift_minus_6 = bs.u(2) as u8;
            self.pic_data.film_grain.grain_scale_shift = bs.u(2) as u8;

            if self.pic_data.film_grain.num_cb_points > 0 {
                self.pic_data.film_grain.cb_mult = bs.u(8) as u8;
                self.pic_data.film_grain.cb_luma_mult = bs.u(8) as u8;
                self.pic_data.film_grain.cb_offset = bs.u(9) as u16;
            }

            if self.pic_data.film_grain.num_cr_points > 0 {
                self.pic_data.film_grain.cr_mult = bs.u(8) as u8;
                self.pic_data.film_grain.cr_luma_mult = bs.u(8) as u8;
                self.pic_data.film_grain.cr_offset = bs.u(9) as u16;
            }

            self.pic_data.film_grain.overlap_flag = bs.u(1) != 0;
            self.pic_data.film_grain.clip_to_restricted_range = bs.u(1) != 0;
        } else {
            self.pic_data.film_grain = Av1FilmGrain::default();
        }

        true
    }

    // -----------------------------------------------------------------------
    // Reference frame management (SetFrameRefs, IsSkipModeAllowed)
    // -----------------------------------------------------------------------

    /// Automatic reference frame assignment (AV1 spec 7.8).
    ///
    /// Corresponds to `SetFrameRefs` in the C++ source.
    pub fn set_frame_refs(&mut self, last_frame_idx: i32, gold_frame_idx: i32) {
        let sps = self.sps.as_ref().expect("SPS required");
        debug_assert!(sps.enable_order_hint);

        let cur_frame_hint = 1i32 << sps.order_hint_bits_minus_1;

        let mut shifted_order_hints = [0i32; NUM_REF_FRAMES];
        let mut used_frame = [false; NUM_REF_FRAMES];

        for i in 0..REFS_PER_FRAME {
            self.ref_frame_idx[i] = -1;
        }

        // LAST_FRAME index = 0, GOLDEN_FRAME index = 3
        self.ref_frame_idx[REFERENCE_NAME_LAST_FRAME - REFERENCE_NAME_LAST_FRAME] = last_frame_idx;
        self.ref_frame_idx[REFERENCE_NAME_GOLDEN_FRAME - REFERENCE_NAME_LAST_FRAME] = gold_frame_idx;
        if last_frame_idx >= 0 {
            used_frame[last_frame_idx as usize] = true;
        }
        if gold_frame_idx >= 0 {
            used_frame[gold_frame_idx as usize] = true;
        }

        let order_hint = self.pic_data.std_info.order_hint;
        for i in 0..NUM_REF_FRAMES {
            let ref_order_hint = self.ref_order_hint[i];
            shifted_order_hints[i] =
                cur_frame_hint + self.get_relative_dist(ref_order_hint, order_hint as i32);
        }

        // ALTREF_FRAME
        {
            let mut ref_idx: i32 = -1;
            let mut latest_order_hint: i32 = -1;
            for i in 0..NUM_REF_FRAMES {
                let hint = shifted_order_hints[i];
                if !used_frame[i]
                    && hint >= cur_frame_hint
                    && (ref_idx < 0 || hint >= latest_order_hint)
                {
                    ref_idx = i as i32;
                    latest_order_hint = hint;
                }
            }
            if ref_idx >= 0 {
                self.ref_frame_idx[REFERENCE_NAME_ALTREF_FRAME - REFERENCE_NAME_LAST_FRAME] = ref_idx;
                used_frame[ref_idx as usize] = true;
            }
        }

        // BWDREF_FRAME
        {
            let mut ref_idx: i32 = -1;
            let mut earliest_order_hint: i32 = -1;
            for i in 0..NUM_REF_FRAMES {
                let hint = shifted_order_hints[i];
                if !used_frame[i]
                    && hint >= cur_frame_hint
                    && (ref_idx < 0 || hint < earliest_order_hint)
                {
                    ref_idx = i as i32;
                    earliest_order_hint = hint;
                }
            }
            if ref_idx >= 0 {
                self.ref_frame_idx[REFERENCE_NAME_BWDREF_FRAME - REFERENCE_NAME_LAST_FRAME] = ref_idx;
                used_frame[ref_idx as usize] = true;
            }
        }

        // ALTREF2_FRAME
        {
            let mut ref_idx: i32 = -1;
            let mut earliest_order_hint: i32 = -1;
            for i in 0..NUM_REF_FRAMES {
                let hint = shifted_order_hints[i];
                if !used_frame[i]
                    && hint >= cur_frame_hint
                    && (ref_idx < 0 || hint < earliest_order_hint)
                {
                    ref_idx = i as i32;
                    earliest_order_hint = hint;
                }
            }
            if ref_idx >= 0 {
                self.ref_frame_idx[REFERENCE_NAME_ALTREF2_FRAME - REFERENCE_NAME_LAST_FRAME] = ref_idx;
                used_frame[ref_idx as usize] = true;
            }
        }

        // Remaining frames
        let ref_frame_list: [usize; 5] = [
            REFERENCE_NAME_LAST2_FRAME,
            REFERENCE_NAME_LAST3_FRAME,
            REFERENCE_NAME_BWDREF_FRAME,
            REFERENCE_NAME_ALTREF2_FRAME,
            REFERENCE_NAME_ALTREF_FRAME,
        ];

        for &ref_frame in &ref_frame_list {
            let slot = ref_frame - REFERENCE_NAME_LAST_FRAME;
            if self.ref_frame_idx[slot] < 0 {
                let mut ref_idx: i32 = -1;
                let mut latest_order_hint: i32 = -1;
                for i in 0..NUM_REF_FRAMES {
                    let hint = shifted_order_hints[i];
                    if !used_frame[i]
                        && hint < cur_frame_hint
                        && (ref_idx < 0 || hint >= latest_order_hint)
                    {
                        ref_idx = i as i32;
                        latest_order_hint = hint;
                    }
                }
                if ref_idx >= 0 {
                    self.ref_frame_idx[slot] = ref_idx;
                    used_frame[ref_idx as usize] = true;
                }
            }
        }

        // Fill remaining with earliest
        {
            let mut ref_idx: i32 = -1;
            let mut earliest_order_hint: i32 = -1;
            for i in 0..NUM_REF_FRAMES {
                let hint = shifted_order_hints[i];
                if ref_idx < 0 || hint < earliest_order_hint {
                    ref_idx = i as i32;
                    earliest_order_hint = hint;
                }
            }
            for i in 0..REFS_PER_FRAME {
                if self.ref_frame_idx[i] < 0 {
                    self.ref_frame_idx[i] = ref_idx;
                }
            }
        }
    }

    /// Check if skip mode is allowed.
    ///
    /// Corresponds to `IsSkipModeAllowed` in the C++ source.
    pub fn is_skip_mode_allowed(&self) -> bool {
        let sps = match &self.sps {
            Some(s) => s,
            None => return false,
        };

        if !sps.enable_order_hint || self.is_frame_intra() || !self.pic_data.std_info.flags.reference_select {
            return false;
        }

        let mut ref0: i32 = -1;
        let mut ref1: i32 = -1;
        let mut ref0_off: i32 = -1;
        let mut ref1_off: i32 = -1;

        let order_hint = self.pic_data.std_info.order_hint;

        for i in 0..REFS_PER_FRAME {
            let frame_idx = self.ref_frame_idx[i];
            if frame_idx >= 0 && (frame_idx as usize) < BUFFER_POOL_MAX_SIZE {
                let ref_frame_offset = self.ref_order_hint[frame_idx as usize];
                let rel_off = self.get_relative_dist(ref_frame_offset, order_hint as i32);

                // Forward reference
                if rel_off < 0
                    && (ref0_off == -1 || self.get_relative_dist(ref_frame_offset, ref0_off) > 0)
                {
                    ref0 = i as i32 + REFERENCE_NAME_LAST_FRAME as i32;
                    ref0_off = ref_frame_offset;
                }
                // Backward reference
                if rel_off > 0
                    && (ref1_off == -1 || self.get_relative_dist(ref_frame_offset, ref1_off) < 0)
                {
                    ref1 = i as i32 + REFERENCE_NAME_LAST_FRAME as i32;
                    ref1_off = ref_frame_offset;
                }
            }
        }

        if ref0 != -1 && ref1 != -1 {
            self.set_skip_mode_frames(ref0, ref1);
            return true;
        } else if ref0 != -1 {
            // Forward prediction only — find second nearest forward reference
            for i in 0..REFS_PER_FRAME {
                let frame_idx = self.ref_frame_idx[i];
                if frame_idx >= 0 && (frame_idx as usize) < BUFFER_POOL_MAX_SIZE {
                    let ref_frame_offset = self.ref_order_hint[frame_idx as usize];
                    if self.get_relative_dist(ref_frame_offset, ref0_off) < 0
                        && (ref1_off == -1
                            || self.get_relative_dist(ref_frame_offset, ref1_off) > 0)
                    {
                        ref1 = i as i32 + REFERENCE_NAME_LAST_FRAME as i32;
                        ref1_off = ref_frame_offset;
                    }
                }
            }
            if ref1 != -1 {
                self.set_skip_mode_frames(ref0, ref1);
                return true;
            }
        }

        false
    }

    /// Helper: set skip mode frame indices (requires interior mutability
    /// since `is_skip_mode_allowed` takes `&self`). In the C++ code this
    /// modifies `pStd->SkipModeFrame` which is part of the picture data.
    /// We work around the borrow by making this a no-op that returns the
    /// values — the caller applies them. This is a slight divergence from
    /// C++ where the method writes directly.
    fn set_skip_mode_frames(&self, ref0: i32, ref1: i32) {
        // In practice the caller (parse_obu_frame_header) checks the bool return
        // and then sets SkipModeFrame. This is a structural divergence documented here.
        // The actual assignment happens in parse_obu_frame_header.
        let _ = (ref0, ref1);
    }

    /// Compute skip mode frames and return them.
    pub fn compute_skip_mode_frames(&self) -> Option<[i32; 2]> {
        let sps = match &self.sps {
            Some(s) => s,
            None => return None,
        };

        if !sps.enable_order_hint || self.is_frame_intra() || !self.pic_data.std_info.flags.reference_select {
            return None;
        }

        let mut ref0: i32 = -1;
        let mut ref1: i32 = -1;
        let mut ref0_off: i32 = -1;
        let mut ref1_off: i32 = -1;
        let order_hint = self.pic_data.std_info.order_hint;

        for i in 0..REFS_PER_FRAME {
            let frame_idx = self.ref_frame_idx[i];
            if frame_idx >= 0 && (frame_idx as usize) < BUFFER_POOL_MAX_SIZE {
                let ref_frame_offset = self.ref_order_hint[frame_idx as usize];
                let rel_off = self.get_relative_dist(ref_frame_offset, order_hint as i32);
                if rel_off < 0
                    && (ref0_off == -1 || self.get_relative_dist(ref_frame_offset, ref0_off) > 0)
                {
                    ref0 = i as i32 + REFERENCE_NAME_LAST_FRAME as i32;
                    ref0_off = ref_frame_offset;
                }
                if rel_off > 0
                    && (ref1_off == -1 || self.get_relative_dist(ref_frame_offset, ref1_off) < 0)
                {
                    ref1 = i as i32 + REFERENCE_NAME_LAST_FRAME as i32;
                    ref1_off = ref_frame_offset;
                }
            }
        }

        if ref0 != -1 && ref1 != -1 {
            return Some([ref0.min(ref1), ref0.max(ref1)]);
        } else if ref0 != -1 {
            for i in 0..REFS_PER_FRAME {
                let frame_idx = self.ref_frame_idx[i];
                if frame_idx >= 0 && (frame_idx as usize) < BUFFER_POOL_MAX_SIZE {
                    let ref_frame_offset = self.ref_order_hint[frame_idx as usize];
                    if self.get_relative_dist(ref_frame_offset, ref0_off) < 0
                        && (ref1_off == -1
                            || self.get_relative_dist(ref_frame_offset, ref1_off) > 0)
                    {
                        ref1 = i as i32 + REFERENCE_NAME_LAST_FRAME as i32;
                        ref1_off = ref_frame_offset;
                    }
                }
            }
            if ref1 != -1 {
                return Some([ref0.min(ref1), ref0.max(ref1)]);
            }
        }

        None
    }

    // -----------------------------------------------------------------------
    // Frame header parsing
    // -----------------------------------------------------------------------

    /// Parse AV1 frame header (uncompressed header).
    ///
    /// Corresponds to `ParseObuFrameHeader` in the C++ source.
    pub fn parse_obu_frame_header(&mut self, bs: &mut BitstreamReader) -> bool {
        let sps = match &self.sps {
            Some(s) => s.clone(),
            None => return false,
        };

        self.pic_data.std_info.flags.frame_size_override_flag = false;
        self.last_frame_type = self.pic_data.std_info.frame_type;
        self.last_intra_only = self.intra_only;

        if sps.reduced_still_picture_header {
            self.show_existing_frame = false;
            self.showable_frame = false;
            self.pic_data.show_frame = true;
            self.pic_data.std_info.frame_type = Av1FrameType::Key;
            self.pic_data.std_info.flags.error_resilient_mode = true;
        } else {
            self.show_existing_frame = bs.u(1) != 0;

            if self.show_existing_frame {
                let frame_to_show_map_idx = bs.u(3) as usize;

                if sps.decoder_model_info_present && !self.timing_info.equal_picture_interval {
                    self.tu_presentation_delay =
                        bs.u(self.buffer_model.frame_presentation_time_length as u32) as i32;
                }

                if sps.frame_id_numbers_present_flag {
                    let _display_frame_id = bs.u(self.frame_id_length as u32);
                }

                if frame_to_show_map_idx < BUFFER_POOL_MAX_SIZE {
                    let reset_decoder_state =
                        self.buffers[frame_to_show_map_idx].frame_type == Av1FrameType::Key;

                    self.pic_data.loop_filter.loop_filter_level[0] = 0;
                    self.pic_data.loop_filter.loop_filter_level[1] = 0;
                    self.pic_data.show_frame = true;
                    self.showable_frame = self.buffers[frame_to_show_map_idx].showable_frame;

                    if sps.film_grain_params_present {
                        self.pic_data.film_grain =
                            self.buffers[frame_to_show_map_idx].film_grain_params.clone();
                    }

                    if reset_decoder_state {
                        self.showable_frame = false;
                        self.pic_data.std_info.frame_type = Av1FrameType::Key;
                        self.pic_data.std_info.refresh_frame_flags = ((1u16 << NUM_REF_FRAMES) - 1) as u8;

                        self.pic_data.loop_filter.loop_filter_ref_deltas =
                            self.buffers[frame_to_show_map_idx].lf_ref_delta;
                        self.pic_data.loop_filter.loop_filter_mode_deltas =
                            self.buffers[frame_to_show_map_idx].lf_mode_delta;

                        for i in 0..GM_GLOBAL_MODELS_PER_FRAME {
                            self.global_motions[i] =
                                self.buffers[frame_to_show_map_idx].global_models[i].clone();
                        }

                        self.pic_data.segmentation.feature_enabled =
                            self.buffers[frame_to_show_map_idx].seg_feature_enabled;
                        self.pic_data.segmentation.feature_data =
                            self.buffers[frame_to_show_map_idx].seg_feature_data;

                        self.pic_data.std_info.order_hint =
                            self.ref_order_hint[frame_to_show_map_idx] as u8;
                        self.update_frame_pointers(frame_to_show_map_idx);
                    } else {
                        self.pic_data.std_info.refresh_frame_flags = 0;
                    }
                }

                return true;
            }

            self.pic_data.std_info.frame_type =
                Av1FrameType::from_u32(bs.u(2)).unwrap_or(Av1FrameType::Key);
            self.intra_only = self.pic_data.std_info.frame_type == Av1FrameType::IntraOnly;

            self.pic_data.show_frame = bs.u(1) != 0;
            if self.pic_data.show_frame {
                if sps.decoder_model_info_present && !self.timing_info.equal_picture_interval {
                    self.tu_presentation_delay =
                        bs.u(self.buffer_model.frame_presentation_time_length as u32) as i32;
                }
                self.showable_frame = self.pic_data.std_info.frame_type != Av1FrameType::Key;
            } else {
                self.showable_frame = bs.u(1) != 0;
            }

            self.pic_data.std_info.flags.error_resilient_mode =
                if self.pic_data.std_info.frame_type == Av1FrameType::Switch
                    || (self.pic_data.std_info.frame_type == Av1FrameType::Key
                        && self.pic_data.show_frame)
                {
                    true
                } else {
                    bs.u(1) != 0
                };
        }

        if self.pic_data.std_info.frame_type == Av1FrameType::Key && self.pic_data.show_frame {
            for i in 0..NUM_REF_FRAMES {
                self.ref_valid[i] = false;
                self.ref_order_hint[i] = 0;
            }
        }

        self.pic_data.std_info.flags.disable_cdf_update = bs.u(1) != 0;

        if sps.seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS {
            self.pic_data.std_info.flags.allow_screen_content_tools = bs.u(1) != 0;
        } else {
            self.pic_data.std_info.flags.allow_screen_content_tools =
                sps.seq_force_screen_content_tools != 0;
        }

        if self.pic_data.std_info.flags.allow_screen_content_tools {
            if sps.seq_force_integer_mv == SELECT_INTEGER_MV {
                self.pic_data.std_info.flags.force_integer_mv = bs.u(1) != 0;
            } else {
                self.pic_data.std_info.flags.force_integer_mv = sps.seq_force_integer_mv != 0;
            }
        } else {
            self.pic_data.std_info.flags.force_integer_mv = false;
        }

        if self.is_frame_intra() {
            self.pic_data.std_info.flags.force_integer_mv = true;
        }

        self.pic_data.std_info.flags.frame_refs_short_signaling = false;
        self.pic_data.std_info.flags.allow_intrabc = false;
        self.pic_data.std_info.primary_ref_frame = PRIMARY_REF_NONE;
        self.pic_data.std_info.flags.frame_size_override_flag = false;

        if !sps.reduced_still_picture_header {
            if sps.frame_id_numbers_present_flag {
                self.pic_data.std_info.current_frame_id = bs.u(self.frame_id_length as u32);

                // Frame ID validation (simplified — full validation omitted for brevity)
                for i in 0..NUM_REF_FRAMES {
                    if self.pic_data.std_info.frame_type == Av1FrameType::Key
                        && self.pic_data.show_frame
                    {
                        self.ref_valid[i] = false;
                    }
                }
            } else {
                self.pic_data.std_info.current_frame_id = 0;
            }

            self.pic_data.std_info.flags.frame_size_override_flag =
                if self.pic_data.std_info.frame_type == Av1FrameType::Switch {
                    true
                } else {
                    bs.u(1) != 0
                };

            // order_hint
            self.pic_data.std_info.order_hint = if sps.enable_order_hint {
                bs.u(sps.order_hint_bits_minus_1 + 1) as u8
            } else {
                0
            };

            if !self.pic_data.std_info.flags.error_resilient_mode && !self.is_frame_intra() {
                self.pic_data.std_info.primary_ref_frame = bs.u(3);
            }
        }

        if sps.decoder_model_info_present {
            self.pic_data.std_info.flags.buffer_removal_time_present_flag = bs.u(1) != 0;
            if self.pic_data.std_info.flags.buffer_removal_time_present_flag {
                for op_num in 0..=(sps.operating_points_cnt_minus_1 as usize) {
                    if self.op_params[op_num].decoder_model_param_present {
                        let op_pt_idc = sps.operating_point_idc[op_num];
                        let in_temporal_layer = (op_pt_idc >> self.temporal_id) & 1;
                        let in_spatial_layer = (op_pt_idc >> (self.spatial_id + 8)) & 1;
                        if op_pt_idc == 0 || (in_temporal_layer != 0 && in_spatial_layer != 0) {
                            self.op_frame_timing[op_num] =
                                bs.u(self.buffer_model.buffer_removal_time_length as u32);
                        }
                    }
                }
            }
        }

        // refresh_frame_flags
        if self.pic_data.std_info.frame_type == Av1FrameType::Key {
            if !self.pic_data.show_frame {
                self.pic_data.std_info.refresh_frame_flags = bs.u(8) as u8;
            } else {
                self.pic_data.std_info.refresh_frame_flags = ((1u16 << NUM_REF_FRAMES) - 1) as u8;
            }
            for i in 0..REFS_PER_FRAME {
                self.ref_frame_idx[i] = 0;
            }
        } else {
            if self.intra_only || self.pic_data.std_info.frame_type != Av1FrameType::Switch {
                self.pic_data.std_info.refresh_frame_flags = bs.u(NUM_REF_FRAMES as u32) as u8;
            } else {
                self.pic_data.std_info.refresh_frame_flags = ((1u16 << NUM_REF_FRAMES) - 1) as u8;
            }
        }

        // error_resilient + enable_order_hint: read ref_order_hint[]
        if (!self.is_frame_intra() || self.pic_data.std_info.refresh_frame_flags != 0xFF)
            && self.pic_data.std_info.flags.error_resilient_mode
            && sps.enable_order_hint
        {
            for _buf_idx in 0..NUM_REF_FRAMES {
                let _offset = bs.u(sps.order_hint_bits_minus_1 + 1);
            }
        }

        if self.is_frame_intra() {
            self.setup_frame_size(bs, self.pic_data.std_info.flags.frame_size_override_flag);
            if self.pic_data.std_info.flags.allow_screen_content_tools
                && self.frame_width == self.upscaled_width
            {
                self.pic_data.std_info.flags.allow_intrabc = bs.u(1) != 0;
            }
            self.pic_data.std_info.flags.use_ref_frame_mvs = false;
        } else {
            self.pic_data.std_info.flags.use_ref_frame_mvs = false;

            if sps.enable_order_hint {
                self.pic_data.std_info.flags.frame_refs_short_signaling = bs.u(1) != 0;
            }

            if self.pic_data.std_info.flags.frame_refs_short_signaling {
                let lst_ref = bs.u(REF_FRAMES_BITS) as i32;
                let gld_ref = bs.u(REF_FRAMES_BITS) as i32;
                self.set_frame_refs(lst_ref, gld_ref);
            }

            for i in 0..REFS_PER_FRAME {
                if !self.pic_data.std_info.flags.frame_refs_short_signaling {
                    let ref_frame_index = bs.u(REF_FRAMES_BITS) as i32;
                    self.ref_frame_idx[i] = ref_frame_index;
                }

                if sps.frame_id_numbers_present_flag {
                    let _delta_frame_id_minus_1 = bs.u(self.delta_frame_id_length as u32);
                }
            }

            if !self.pic_data.std_info.flags.error_resilient_mode
                && self.pic_data.std_info.flags.frame_size_override_flag
            {
                self.setup_frame_size_with_refs(bs);
            } else {
                self.setup_frame_size(bs, self.pic_data.std_info.flags.frame_size_override_flag);
            }

            if self.pic_data.std_info.flags.force_integer_mv {
                self.pic_data.std_info.flags.allow_high_precision_mv = false;
            } else {
                self.pic_data.std_info.flags.allow_high_precision_mv = bs.u(1) != 0;
            }

            // interpolation_filter
            let tmp = bs.u(1);
            self.pic_data.std_info.flags.is_filter_switchable = tmp != 0;
            if tmp != 0 {
                self.pic_data.std_info.interpolation_filter = Av1InterpolationFilter::Switchable;
            } else {
                self.pic_data.std_info.interpolation_filter =
                    Av1InterpolationFilter::from_u32(bs.u(2)).unwrap_or(Av1InterpolationFilter::EightTap);
            }

            self.pic_data.std_info.flags.is_motion_mode_switchable = bs.u(1) != 0;

            if !self.pic_data.std_info.flags.error_resilient_mode
                && sps.enable_ref_frame_mvs
                && sps.enable_order_hint
                && !self.is_frame_intra()
            {
                self.pic_data.std_info.flags.use_ref_frame_mvs = bs.u(1) != 0;
            }

            // Set OrderHints for references
            for i in 0..REFS_PER_FRAME {
                let idx = self.ref_frame_idx[i];
                if idx >= 0 && (idx as usize) < BUFFER_POOL_MAX_SIZE {
                    self.pic_data.std_info.order_hints[i + REFERENCE_NAME_LAST_FRAME] =
                        self.ref_order_hint[idx as usize] as u8;
                }
            }
        }

        // Update reference frame IDs
        if sps.frame_id_numbers_present_flag {
            let tmp_flags = self.pic_data.std_info.refresh_frame_flags;
            for i in 0..NUM_REF_FRAMES {
                if ((tmp_flags >> i) & 1) != 0 {
                    self.ref_frame_id[i] = self.pic_data.std_info.current_frame_id as i32;
                    self.ref_valid[i] = true;
                }
            }
        }

        if !sps.reduced_still_picture_header && !self.pic_data.std_info.flags.disable_cdf_update {
            self.pic_data.std_info.flags.disable_frame_end_update_cdf = bs.u(1) != 0;
        } else {
            self.pic_data.std_info.flags.disable_frame_end_update_cdf = true;
        }

        // tile_info
        self.decode_tile_info(bs);
        self.decode_quantization_data(bs);
        self.decode_segmentation_data(bs);

        // delta_q / delta_lf
        self.pic_data.std_info.delta_q_res = 0;
        self.pic_data.std_info.delta_lf_res = 0;
        self.pic_data.std_info.flags.delta_lf_present = false;
        self.pic_data.std_info.flags.delta_lf_multi = false;
        self.pic_data.std_info.flags.delta_q_present =
            if self.pic_data.quantization.base_q_idx > 0 { bs.u(1) != 0 } else { false };

        if self.pic_data.std_info.flags.delta_q_present {
            self.pic_data.std_info.delta_q_res = bs.u(2) as u8;
            if !self.pic_data.std_info.flags.allow_intrabc {
                self.pic_data.std_info.flags.delta_lf_present = bs.u(1) != 0;
            }
            if self.pic_data.std_info.flags.delta_lf_present {
                self.pic_data.std_info.delta_lf_res = bs.u(2) as u8;
                self.pic_data.std_info.flags.delta_lf_multi = bs.u(1) != 0;
            }
        }

        // Compute lossless per segment
        for i in 0..MAX_SEGMENTS {
            let qindex = self.pic_data.quantization.base_q_idx as i32;
            // Simplified: the C++ code has a bug/quirk with (FeatureEnabled[i] & 0) which
            // is always 0 — so qindex is always base_q_idx.
            let qindex = clamp(qindex, 0, 255);
            self.lossless[i] = qindex == 0
                && self.pic_data.quantization.delta_q_y_dc == 0
                && self.pic_data.quantization.delta_q_u_dc == 0
                && self.pic_data.quantization.delta_q_u_ac == 0
                && self.pic_data.quantization.delta_q_v_dc == 0
                && self.pic_data.quantization.delta_q_v_ac == 0;
        }

        self.coded_lossless = self.lossless[0];
        if self.pic_data.std_info.flags.segmentation_enabled {
            for i in 1..MAX_SEGMENTS {
                self.coded_lossless = self.coded_lossless && self.lossless[i];
            }
        }

        self.all_lossless = self.coded_lossless && (self.frame_width == self.upscaled_width);

        if self.coded_lossless {
            self.pic_data.loop_filter.loop_filter_level[0] = 0;
            self.pic_data.loop_filter.loop_filter_level[1] = 0;
        }
        if self.coded_lossless || !sps.enable_cdef {
            self.pic_data.cdef.cdef_bits = 0;
        }
        if self.all_lossless || !sps.enable_restoration {
            self.pic_data.loop_restoration.frame_restoration_type =
                [Av1FrameRestorationType::None; 3];
        }

        self.decode_loop_filter_data(bs);

        if !self.coded_lossless
            && sps.enable_cdef
            && !self.pic_data.std_info.flags.allow_intrabc
        {
            self.decode_cdef_data(bs);
        }
        if !self.all_lossless
            && sps.enable_restoration
            && !self.pic_data.std_info.flags.allow_intrabc
        {
            self.decode_loop_restoration_data(bs);
        }

        // TxMode
        self.pic_data.std_info.tx_mode = if self.coded_lossless {
            Av1TxMode::Only4x4
        } else if bs.u(1) != 0 {
            Av1TxMode::Select
        } else {
            Av1TxMode::Largest
        };

        if !self.is_frame_intra() {
            self.pic_data.std_info.flags.reference_select = bs.u(1) != 0;
        } else {
            self.pic_data.std_info.flags.reference_select = false;
        }

        // skip_mode_present
        let skip_mode_allowed = self.compute_skip_mode_frames();
        self.pic_data.std_info.flags.skip_mode_present = if skip_mode_allowed.is_some() {
            bs.u(1) != 0
        } else {
            false
        };
        if let Some(frames) = skip_mode_allowed {
            self.pic_data.std_info.skip_mode_frame = frames;
        }

        if !self.is_frame_intra()
            && !self.pic_data.std_info.flags.error_resilient_mode
            && sps.enable_warped_motion
        {
            self.pic_data.std_info.flags.allow_warped_motion = bs.u(1) != 0;
        } else {
            self.pic_data.std_info.flags.allow_warped_motion = false;
        }

        self.pic_data.std_info.flags.reduced_tx_set = bs.u(1) != 0;

        // Reset global motions
        for i in 0..GM_GLOBAL_MODELS_PER_FRAME {
            self.global_motions[i] = default_warp_params();
        }

        if !self.is_frame_intra() {
            self.decode_global_motion_params(bs);
        }

        self.read_film_grain_params(bs);

        true
    }

    // -----------------------------------------------------------------------
    // Update frame pointers (reference buffer management)
    // -----------------------------------------------------------------------

    /// Update reference frame buffers after decoding a frame.
    ///
    /// Corresponds to `UpdateFramePointers` in the C++ source.
    pub fn update_frame_pointers(&mut self, _source_buffer_idx: usize) {
        let refresh_flags = self.pic_data.std_info.refresh_frame_flags;
        let frame_type = self.pic_data.std_info.frame_type;
        let order_hint = self.pic_data.std_info.order_hint;

        let mut ref_index = 0u32;
        let mut mask = refresh_flags;
        while mask != 0 {
            if (mask & 1) != 0 {
                let idx = ref_index as usize;
                if idx < BUFFER_POOL_MAX_SIZE {
                    self.buffers[idx].buffer_valid = true;
                    self.buffers[idx].showable_frame = self.showable_frame;
                    self.buffers[idx].frame_type = frame_type;
                    self.buffers[idx].order_hint = order_hint;

                    for ref_name in REFERENCE_NAME_LAST_FRAME..NUM_REF_FRAMES {
                        let ref_order = self.pic_data.std_info.order_hints[ref_name];
                        self.buffers[idx].saved_order_hints[ref_name] = ref_order;
                        self.buffers[idx].ref_frame_sign_bias[ref_name] =
                            self.get_relative_dist(order_hint as i32, ref_order as i32) as i8;
                    }

                    self.buffers[idx].film_grain_params = self.pic_data.film_grain.clone();

                    for gm in 0..GM_GLOBAL_MODELS_PER_FRAME {
                        self.buffers[idx].global_models[gm] = self.global_motions[gm].clone();
                    }

                    self.buffers[idx].lf_ref_delta =
                        self.pic_data.loop_filter.loop_filter_ref_deltas;
                    self.buffers[idx].lf_mode_delta =
                        self.pic_data.loop_filter.loop_filter_mode_deltas;

                    self.buffers[idx].seg_feature_enabled =
                        self.pic_data.segmentation.feature_enabled;
                    self.buffers[idx].seg_feature_data = self.pic_data.segmentation.feature_data;

                    self.buffers[idx].primary_ref_frame =
                        self.pic_data.std_info.primary_ref_frame;
                    self.buffers[idx].base_q_index = self.pic_data.quantization.base_q_idx;
                    self.buffers[idx].disable_frame_end_update_cdf =
                        self.pic_data.std_info.flags.disable_frame_end_update_cdf;

                    self.ref_order_hint[idx] = order_hint as i32;
                }
            }
            ref_index += 1;
            mask >>= 1;
        }
    }

    // -----------------------------------------------------------------------
    // OBU-level frame parsing
    // -----------------------------------------------------------------------

    /// Check if an OBU is in the current operating point.
    fn is_obu_in_current_operating_point(current_op: i32, hdr: &Av1ObuHeader) -> bool {
        if current_op == 0 {
            return true;
        }
        ((current_op >> hdr.temporal_id) & 0x1) != 0
            && ((current_op >> (hdr.spatial_id + 8)) & 0x1) != 0
    }

    /// Parse a single AV1 temporal unit (one frame).
    ///
    /// Corresponds to `ParseOneFrame` in the C++ source.
    /// Returns `true` on success.
    pub fn parse_one_frame(&mut self, data: &[u8]) -> bool {
        self.sps_changed = false;

        let mut offset = 0usize;
        let frame_size = data.len();

        while offset < frame_size {
            let remaining = &data[offset..];
            let hdr = match self.parse_obu_header_and_size(remaining) {
                Some(h) => h,
                None => return false,
            };

            let total_obu_size = (hdr.header_size + hdr.payload_size) as usize;
            if total_obu_size > remaining.len() {
                return false;
            }

            let obu_type = match hdr.obu_type {
                Some(t) => t,
                None => {
                    offset += total_obu_size;
                    continue;
                }
            };

            self.temporal_id = hdr.temporal_id;
            self.spatial_id = hdr.spatial_id;

            // Check operating point
            if obu_type != Av1ObuType::TemporalDelimiter
                && obu_type != Av1ObuType::SequenceHeader
                && obu_type != Av1ObuType::Padding
            {
                if !Self::is_obu_in_current_operating_point(
                    self.operating_point_idc_active,
                    &hdr,
                ) {
                    offset += total_obu_size;
                    continue;
                }
            }

            let payload_start = offset + hdr.header_size as usize;
            let payload_end = payload_start + hdr.payload_size as usize;
            let payload = if payload_end <= data.len() {
                &data[payload_start..payload_end]
            } else {
                return false;
            };

            let mut bs = BitstreamReader::new(payload);

            match obu_type {
                Av1ObuType::TemporalDelimiter => {
                    self.pic_data.tile_offsets = [0; MAX_TILES];
                    self.pic_data.tile_sizes = [0; MAX_TILES];
                    self.pic_data.tile_count = 0;
                }
                Av1ObuType::SequenceHeader => {
                    self.parse_obu_sequence_header(&mut bs);
                }
                Av1ObuType::FrameHeader | Av1ObuType::Frame => {
                    self.pic_data.tile_offsets = [0; MAX_TILES];
                    self.pic_data.tile_count = 0;
                    self.pic_data.tile_sizes = [0; MAX_TILES];

                    self.parse_obu_frame_header(&mut bs);

                    if self.show_existing_frame {
                        offset += total_obu_size;
                        continue;
                    }

                    if obu_type == Av1ObuType::Frame {
                        bs.byte_alignment();
                        // Tile group parsing would follow here in full implementation
                    }
                }
                Av1ObuType::TileGroup => {
                    // Tile group parsing (simplified)
                }
                _ => {}
            }

            offset += total_obu_size;
        }

        true
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Global motion parameter calculations
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_warp_params() {
        let p = default_warp_params();
        assert_eq!(p.wmtype, Av1TransformationType::Identity);
        assert_eq!(p.wmmat[0], 0);
        assert_eq!(p.wmmat[1], 0);
        assert_eq!(p.wmmat[2], 1 << WARPEDMODEL_PREC_BITS);
        assert_eq!(p.wmmat[3], 0);
        assert_eq!(p.wmmat[4], 0);
        assert_eq!(p.wmmat[5], 1 << WARPEDMODEL_PREC_BITS);
        assert!(!p.invalid);
    }

    #[test]
    fn test_identity_global_motion() {
        // A bitstream with a single 0 bit => IDENTITY transformation
        let data = [0x00u8]; // All zeros
        let mut bs = BitstreamReader::new(&data);
        let ref_params = default_warp_params();
        let result = VulkanAv1Decoder::read_gm_params(&mut bs, &ref_params, true);
        assert_eq!(result.wmtype, Av1TransformationType::Identity);
        assert_eq!(result.wmmat, default_warp_params().wmmat);
        assert!(!result.invalid);
    }

    #[test]
    fn test_inv_recenter_nonneg() {
        assert_eq!(inv_recenter_nonneg(5, 0), 5);
        assert_eq!(inv_recenter_nonneg(5, 1), 4);
        assert_eq!(inv_recenter_nonneg(5, 2), 6);
        assert_eq!(inv_recenter_nonneg(5, 11), 11);
        assert_eq!(inv_recenter_nonneg(0, 0), 0);
    }

    #[test]
    fn test_inv_recenter_finite_nonneg() {
        // When r*2 <= n
        assert_eq!(inv_recenter_finite_nonneg(10, 3, 0), 3);
        assert_eq!(inv_recenter_finite_nonneg(10, 3, 1), 2);
        assert_eq!(inv_recenter_finite_nonneg(10, 3, 2), 4);

        // When r*2 > n
        assert_eq!(inv_recenter_finite_nonneg(10, 8, 0), 8);
    }

    #[test]
    fn test_get_msb() {
        assert_eq!(get_msb(1), 0);
        assert_eq!(get_msb(2), 1);
        assert_eq!(get_msb(3), 1);
        assert_eq!(get_msb(4), 2);
        assert_eq!(get_msb(255), 7);
        assert_eq!(get_msb(256), 8);
    }

    // -----------------------------------------------------------------------
    // OBU parsing basics
    // -----------------------------------------------------------------------

    #[test]
    fn test_obu_type_from_u8() {
        assert_eq!(Av1ObuType::from_u8(1), Some(Av1ObuType::SequenceHeader));
        assert_eq!(Av1ObuType::from_u8(2), Some(Av1ObuType::TemporalDelimiter));
        assert_eq!(Av1ObuType::from_u8(3), Some(Av1ObuType::FrameHeader));
        assert_eq!(Av1ObuType::from_u8(4), Some(Av1ObuType::TileGroup));
        assert_eq!(Av1ObuType::from_u8(6), Some(Av1ObuType::Frame));
        assert_eq!(Av1ObuType::from_u8(15), Some(Av1ObuType::Padding));
        assert_eq!(Av1ObuType::from_u8(0), None);
        assert_eq!(Av1ObuType::from_u8(9), None);
    }

    #[test]
    fn test_read_obu_size_single_byte() {
        // 0x05 => value 5, no continuation bit
        let data = [0x05u8];
        let result = VulkanAv1Decoder::read_obu_size(&data);
        assert_eq!(result, Some((5, 1)));
    }

    #[test]
    fn test_read_obu_size_multi_byte() {
        // LEB128: 0x80, 0x01 => (0 | (1 << 7)) = 128
        let data = [0x80u8, 0x01];
        let result = VulkanAv1Decoder::read_obu_size(&data);
        assert_eq!(result, Some((128, 2)));
    }

    #[test]
    fn test_read_obu_header_sequence() {
        // OBU type = 1 (SequenceHeader), has_size_field = 1
        // Byte: 0_0001_0_1_0 = 0b00001010 = 0x0A
        let data = [0x0Au8, 0x00];
        let hdr = VulkanAv1Decoder::read_obu_header(&data).unwrap();
        assert_eq!(hdr.obu_type, Some(Av1ObuType::SequenceHeader));
        assert!(hdr.has_size_field);
        assert!(!hdr.has_extension);
        assert_eq!(hdr.header_size, 1);
    }

    #[test]
    fn test_read_obu_header_with_extension() {
        // OBU type = 3 (FrameHeader), has_extension = 1, has_size_field = 1
        // Byte 0: 0_0011_1_1_0 = 0b00011110 = 0x1E
        // Byte 1: temporal_id=2, spatial_id=1 => 010_01_000 = 0b01001000 = 0x48
        let data = [0x1Eu8, 0x48];
        let hdr = VulkanAv1Decoder::read_obu_header(&data).unwrap();
        assert_eq!(hdr.obu_type, Some(Av1ObuType::FrameHeader));
        assert!(hdr.has_size_field);
        assert!(hdr.has_extension);
        assert_eq!(hdr.temporal_id, 2);
        assert_eq!(hdr.spatial_id, 1);
        assert_eq!(hdr.header_size, 2);
    }

    #[test]
    fn test_read_obu_header_forbidden_bit() {
        // Forbidden bit set (MSB = 1)
        let data = [0x80u8];
        assert!(VulkanAv1Decoder::read_obu_header(&data).is_none());
    }

    #[test]
    fn test_read_obu_header_reserved_bit() {
        // Reserved bit set (LSB = 1)
        // OBU type = 1, has_size_field = 1, reserved = 1
        // 0_0001_0_1_1 = 0b00001011 = 0x0B
        let data = [0x0Bu8];
        assert!(VulkanAv1Decoder::read_obu_header(&data).is_none());
    }

    #[test]
    fn test_parse_obu_header_and_size() {
        let decoder = VulkanAv1Decoder::new(false);
        // OBU type = 1 (SequenceHeader), has_size_field = 1
        // Header byte: 0x0A
        // Size byte: 0x03 (payload = 3 bytes)
        // Payload: 3 zero bytes
        let data = [0x0Au8, 0x03, 0x00, 0x00, 0x00];
        let hdr = decoder.parse_obu_header_and_size(&data).unwrap();
        assert_eq!(hdr.obu_type, Some(Av1ObuType::SequenceHeader));
        assert_eq!(hdr.header_size, 2); // 1 byte header + 1 byte size
        assert_eq!(hdr.payload_size, 3);
    }

    // -----------------------------------------------------------------------
    // Reference frame selection logic
    // -----------------------------------------------------------------------

    #[test]
    fn test_relative_dist_no_order_hint() {
        let mut decoder = VulkanAv1Decoder::new(false);
        let mut sps = Av1SequenceHeader::default();
        sps.enable_order_hint = false;
        decoder.sps = Some(sps);
        // Without order hint, relative dist is always 0
        assert_eq!(decoder.get_relative_dist(10, 5), 0);
    }

    #[test]
    fn test_relative_dist_with_order_hint() {
        let mut decoder = VulkanAv1Decoder::new(false);
        let mut sps = Av1SequenceHeader::default();
        sps.enable_order_hint = true;
        sps.order_hint_bits_minus_1 = 6; // 7 bits
        decoder.sps = Some(sps);

        // Simple forward distance
        assert_eq!(decoder.get_relative_dist(10, 5), 5);
        // Simple backward distance
        assert_eq!(decoder.get_relative_dist(5, 10), -5);
        // Wrap-around (7 bits => max 128)
        assert_eq!(decoder.get_relative_dist(2, 126), 4); // wraps
    }

    #[test]
    fn test_set_frame_refs_basic() {
        let mut decoder = VulkanAv1Decoder::new(false);
        let mut sps = Av1SequenceHeader::default();
        sps.enable_order_hint = true;
        sps.order_hint_bits_minus_1 = 6;
        decoder.sps = Some(sps);

        // Set up some reference order hints
        for i in 0..BUFFER_POOL_MAX_SIZE {
            decoder.ref_order_hint[i] = i as i32;
        }
        decoder.pic_data.std_info.order_hint = 5;

        decoder.set_frame_refs(0, 3);

        // LAST_FRAME should be index 0
        assert_eq!(
            decoder.ref_frame_idx[REFERENCE_NAME_LAST_FRAME - REFERENCE_NAME_LAST_FRAME],
            0
        );
        // GOLDEN_FRAME should be index 3
        assert_eq!(
            decoder.ref_frame_idx[REFERENCE_NAME_GOLDEN_FRAME - REFERENCE_NAME_LAST_FRAME],
            3
        );
        // All slots should be filled (>= 0)
        for i in 0..REFS_PER_FRAME {
            assert!(decoder.ref_frame_idx[i] >= 0, "ref_frame_idx[{}] = {}", i, decoder.ref_frame_idx[i]);
        }
    }

    #[test]
    fn test_is_frame_intra() {
        let mut decoder = VulkanAv1Decoder::new(false);
        decoder.pic_data.std_info.frame_type = Av1FrameType::Key;
        assert!(decoder.is_frame_intra());

        decoder.pic_data.std_info.frame_type = Av1FrameType::IntraOnly;
        assert!(decoder.is_frame_intra());

        decoder.pic_data.std_info.frame_type = Av1FrameType::Inter;
        assert!(!decoder.is_frame_intra());

        decoder.pic_data.std_info.frame_type = Av1FrameType::Switch;
        assert!(!decoder.is_frame_intra());
    }

    #[test]
    fn test_skip_mode_not_allowed_for_intra() {
        let mut decoder = VulkanAv1Decoder::new(false);
        let mut sps = Av1SequenceHeader::default();
        sps.enable_order_hint = true;
        decoder.sps = Some(sps);
        decoder.pic_data.std_info.frame_type = Av1FrameType::Key;
        decoder.pic_data.std_info.flags.reference_select = true;
        assert!(!decoder.is_skip_mode_allowed());
    }

    // -----------------------------------------------------------------------
    // Bitstream reader
    // -----------------------------------------------------------------------

    #[test]
    fn test_bitstream_reader_u() {
        let data = [0b10110100u8, 0b11000000];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(bs.u(1), 1);
        assert_eq!(bs.u(1), 0);
        assert_eq!(bs.u(1), 1);
        assert_eq!(bs.u(1), 1);
        assert_eq!(bs.u(4), 0b0100);
        assert_eq!(bs.u(2), 0b11);
    }

    #[test]
    fn test_bitstream_reader_byte_alignment() {
        let data = [0xFF, 0xAA];
        let mut bs = BitstreamReader::new(&data);
        bs.u(3); // consume 3 bits
        bs.byte_alignment(); // skip to bit 8
        assert_eq!(bs.consumed_bits(), 8);
        assert_eq!(bs.u(8), 0xAA);
    }

    #[test]
    fn test_read_uvlc() {
        // UVLC for value 0: leading zeros = 0, then 1 bit "1" => value = 0
        // That is: bit "1" => lz=0, val = u(0) + (1<<0) - 1 = 0
        let data = [0b10000000u8];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanAv1Decoder::read_uvlc(&mut bs), 0);

        // UVLC for value 1: "010" => lz=1, val = u(1)=0 + 2 - 1 = 1
        let data = [0b01000000u8];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanAv1Decoder::read_uvlc(&mut bs), 1);

        // UVLC for value 2: "011" => lz=1, val = u(1)=1 + 2 - 1 = 2
        let data = [0b01100000u8];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanAv1Decoder::read_uvlc(&mut bs), 2);
    }

    #[test]
    fn test_read_signed_bits() {
        // 3 bits signed: value bits=2, sign bit
        // "011" => (0b011 << (32-3)) >> (32-3) = 3
        let data = [0b01100000u8];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanAv1Decoder::read_signed_bits(&mut bs, 2), 3);

        // "111" => negative
        let data = [0b11100000u8];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanAv1Decoder::read_signed_bits(&mut bs, 2), -1);
    }

    // -----------------------------------------------------------------------
    // Helper functions
    // -----------------------------------------------------------------------

    #[test]
    fn test_tile_log2() {
        assert_eq!(tile_log2(64, 64), 0);
        assert_eq!(tile_log2(64, 65), 1);
        assert_eq!(tile_log2(64, 128), 1);
        assert_eq!(tile_log2(64, 129), 2);
        assert_eq!(tile_log2(1, 1), 0);
        assert_eq!(tile_log2(1, 4), 2);
    }

    #[test]
    fn test_floor_log2() {
        assert_eq!(floor_log2(1), 0);
        assert_eq!(floor_log2(2), 1);
        assert_eq!(floor_log2(3), 1);
        assert_eq!(floor_log2(4), 2);
        assert_eq!(floor_log2(7), 2);
        assert_eq!(floor_log2(8), 3);
    }

    #[test]
    fn test_clamp() {
        assert_eq!(clamp(5, 0, 10), 5);
        assert_eq!(clamp(-1, 0, 10), 0);
        assert_eq!(clamp(11, 0, 10), 10);
        assert_eq!(clamp(0, 0, 0), 0);
    }

    #[test]
    fn test_decoder_construction() {
        let decoder = VulkanAv1Decoder::new(false);
        assert!(!decoder.obu_annex_b);
        assert!(!decoder.sps_received);
        assert_eq!(decoder.pic_data.std_info.primary_ref_frame, PRIMARY_REF_NONE);
        assert_eq!(decoder.pic_data.std_info.refresh_frame_flags, 0xFF);
        assert_eq!(decoder.ref_frame_id, [-1; NUM_REF_FRAMES]);
        assert_eq!(decoder.tile_size_bytes_minus_1, 3);
    }

    #[test]
    fn test_decoder_annex_b() {
        let decoder = VulkanAv1Decoder::new(true);
        assert!(decoder.obu_annex_b);
    }

    #[test]
    fn test_frame_type_conversions() {
        assert_eq!(Av1FrameType::from_u32(0), Some(Av1FrameType::Key));
        assert_eq!(Av1FrameType::from_u32(1), Some(Av1FrameType::Inter));
        assert_eq!(Av1FrameType::from_u32(2), Some(Av1FrameType::IntraOnly));
        assert_eq!(Av1FrameType::from_u32(3), Some(Av1FrameType::Switch));
        assert_eq!(Av1FrameType::from_u32(4), None);
    }

    #[test]
    fn test_transformation_type_conversions() {
        assert_eq!(
            Av1TransformationType::from_u32(0),
            Some(Av1TransformationType::Identity)
        );
        assert_eq!(
            Av1TransformationType::from_u32(1),
            Some(Av1TransformationType::Translation)
        );
        assert_eq!(
            Av1TransformationType::from_u32(2),
            Some(Av1TransformationType::Rotzoom)
        );
        assert_eq!(
            Av1TransformationType::from_u32(3),
            Some(Av1TransformationType::Affine)
        );
        assert_eq!(Av1TransformationType::from_u32(4), None);
    }

    #[test]
    fn test_le_bytes_reader() {
        let data = [0x01, 0x02, 0x03, 0x04];
        let mut bs = BitstreamReader::new(&data);
        // le(2) reads 2 bytes in little-endian: 0x01 + (0x02 << 8) = 0x0201
        assert_eq!(bs.le(2), 0x0201);
    }

    #[test]
    fn test_read_tile_group_size() {
        assert_eq!(read_tile_group_size(&[42], 1), Some(42));
        assert_eq!(read_tile_group_size(&[0x01, 0x02], 2), Some(0x0201));
        assert_eq!(
            read_tile_group_size(&[0x01, 0x02, 0x03], 3),
            Some(0x030201)
        );
        assert_eq!(
            read_tile_group_size(&[0x01, 0x02, 0x03, 0x04], 4),
            Some(0x04030201)
        );
        assert_eq!(read_tile_group_size(&[0x00], 5), None);
    }
}
