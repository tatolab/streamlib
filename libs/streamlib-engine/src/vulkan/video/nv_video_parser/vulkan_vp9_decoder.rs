// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `VulkanVP9Decoder.h` + `VulkanVP9Decoder.cpp` — VP9 bitstream parser.
//!
//! Implements VP9 uncompressed header parsing, reference frame management (8 slots),
//! segmentation, loop filter parameters, quantization parameters, tile info,
//! and superframe index parsing.

// ---------------------------------------------------------------------------
// Constants (from VulkanVP9Decoder.h)
// ---------------------------------------------------------------------------

/// VP9 frame marker — the 2-bit value 0b10 that must appear at the start of every frame.
pub const VP9_FRAME_MARKER: u32 = 2;

/// VP9 sync code — 24-bit value 0x498342 embedded in keyframes / intra-only frames.
pub const VP9_FRAME_SYNC_CODE: u32 = 0x498342;

/// Maximum probability value used in VP9 segmentation probability tables.
pub const VP9_MAX_PROBABILITY: u8 = 255;

/// Minimum number of 64×64 superblocks per tile column.
pub const VP9_MIN_TILE_WIDTH_B64: u32 = 4;

/// Maximum number of 64×64 superblocks per tile column.
pub const VP9_MAX_TILE_WIDTH_B64: u32 = 64;

/// Size of the VP9 buffer pool (reference frame slots + extra).
pub const VP9_BUFFER_POOL_MAX_SIZE: usize = 10;

/// Maximum number of spatial layers for VP9 SVC.
pub const VP9_MAX_NUM_SPATIAL_LAYERS: usize = 4;

/// Number of reference frames in VP9.
pub const VP9_NUM_REF_FRAMES: usize = 8;

/// Number of reference frames used per inter frame (LAST, GOLDEN, ALTREF).
pub const VP9_REFS_PER_FRAME: usize = 3;

/// Maximum number of segments.
pub const VP9_MAX_SEGMENTS: usize = 8;

/// Maximum reference frame count (INTRA, LAST, GOLDEN, ALTREF).
pub const VP9_MAX_REF_FRAMES: usize = 4;

/// Number of loop filter adjustments (mode deltas).
pub const VP9_LOOP_FILTER_ADJUSTMENTS: usize = 2;

/// Maximum number of segmentation tree probabilities.
pub const VP9_MAX_SEGMENTATION_TREE_PROBS: usize = 7;

/// Maximum number of segmentation prediction probabilities.
pub const VP9_MAX_SEGMENTATION_PRED_PROB: usize = 3;

/// Number of segment-level features.
pub const VP9_SEG_LVL_MAX: usize = 4;

// ---------------------------------------------------------------------------
// Segment level features (from VulkanVP9Decoder.h)
// ---------------------------------------------------------------------------

/// Segment-level features for VP9.
///
/// Corresponds to `SEG_LVL_FEATURES` in the C++ source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SegLvlFeature {
    /// Use alternate quantizer.
    AltQ = 0,
    /// Use alternate loop filter value.
    AltLf = 1,
    /// Optional segment reference frame.
    RefFrame = 2,
    /// Optional segment (0,0) + skip mode.
    Skip = 3,
}

// ---------------------------------------------------------------------------
// VP9 Frame Type
// ---------------------------------------------------------------------------

/// VP9 frame type.
///
/// Corresponds to `StdVideoVP9FrameType` in the Vulkan Video headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Vp9FrameType {
    #[default]
    Key = 0,
    Inter = 1,
}

// ---------------------------------------------------------------------------
// VP9 Profile
// ---------------------------------------------------------------------------

/// VP9 profile.
///
/// Corresponds to `StdVideoVP9Profile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(u32)]
pub enum Vp9Profile {
    #[default]
    Profile0 = 0,
    Profile1 = 1,
    Profile2 = 2,
    Profile3 = 3,
}

// ---------------------------------------------------------------------------
// VP9 Color Space
// ---------------------------------------------------------------------------

/// VP9 color space.
///
/// Corresponds to `StdVideoVP9ColorSpace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Vp9ColorSpace {
    #[default]
    Unknown = 0,
    Bt601 = 1,
    Bt709 = 2,
    Smpte170 = 3,
    Smpte240 = 4,
    Bt2020 = 5,
    Reserved = 6,
    Rgb = 7,
}

impl Vp9ColorSpace {
    /// Create from a raw 3-bit value read from the bitstream.
    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Unknown,
            1 => Self::Bt601,
            2 => Self::Bt709,
            3 => Self::Smpte170,
            4 => Self::Smpte240,
            5 => Self::Bt2020,
            6 => Self::Reserved,
            7 => Self::Rgb,
            _ => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// VP9 Interpolation Filter
// ---------------------------------------------------------------------------

/// VP9 interpolation filter type.
///
/// Corresponds to `StdVideoVP9InterpolationFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Vp9InterpolationFilter {
    #[default]
    EighttapSmooth = 0,
    Eighttap = 1,
    EighttapSharp = 2,
    Bilinear = 3,
    Switchable = 4,
}

// ---------------------------------------------------------------------------
// VP9 Color Config
// ---------------------------------------------------------------------------

/// VP9 color configuration.
///
/// Corresponds to `StdVideoVP9ColorConfig`.
#[derive(Debug, Clone, Default)]
pub struct Vp9ColorConfig {
    pub bit_depth: u8,
    pub color_space: Vp9ColorSpace,
    pub color_range: bool,
    pub subsampling_x: u8,
    pub subsampling_y: u8,
}

// ---------------------------------------------------------------------------
// VP9 Loop Filter
// ---------------------------------------------------------------------------

/// VP9 loop filter parameters.
///
/// Corresponds to `StdVideoVP9LoopFilter`.
#[derive(Debug, Clone, Default)]
pub struct Vp9LoopFilter {
    pub loop_filter_level: u8,
    pub loop_filter_sharpness: u8,
    pub loop_filter_delta_enabled: bool,
    pub loop_filter_delta_update: bool,
    pub update_ref_delta: u8,
    pub update_mode_delta: u8,
    pub loop_filter_ref_deltas: [i8; VP9_MAX_REF_FRAMES],
    pub loop_filter_mode_deltas: [i8; VP9_LOOP_FILTER_ADJUSTMENTS],
}

// ---------------------------------------------------------------------------
// VP9 Segmentation
// ---------------------------------------------------------------------------

/// VP9 segmentation flags.
///
/// Corresponds to segmentation flag bits in `StdVideoVP9Segmentation`.
#[derive(Debug, Clone, Default)]
pub struct Vp9SegmentationFlags {
    pub segmentation_update_map: bool,
    pub segmentation_temporal_update: bool,
    pub segmentation_update_data: bool,
    pub segmentation_abs_or_delta_update: bool,
}

/// VP9 segmentation parameters.
///
/// Corresponds to `StdVideoVP9Segmentation`.
#[derive(Debug, Clone, Default)]
pub struct Vp9Segmentation {
    pub flags: Vp9SegmentationFlags,
    pub segmentation_tree_probs: [u8; VP9_MAX_SEGMENTATION_TREE_PROBS],
    pub segmentation_pred_prob: [u8; VP9_MAX_SEGMENTATION_PRED_PROB],
    /// Per-segment feature enable bitmask (one byte per segment, each bit = one feature).
    pub feature_enabled: [u8; VP9_MAX_SEGMENTS],
    /// Per-segment feature data.
    pub feature_data: [[i16; VP9_SEG_LVL_MAX]; VP9_MAX_SEGMENTS],
}

// ---------------------------------------------------------------------------
// VP9 Picture Info Flags
// ---------------------------------------------------------------------------

/// Flags for `Vp9PictureInfo`.
///
/// Corresponds to `StdVideoDecodeVP9PictureInfoFlags`.
#[derive(Debug, Clone, Default)]
pub struct Vp9PictureInfoFlags {
    pub show_frame: bool,
    pub error_resilient_mode: bool,
    pub intra_only: bool,
    pub refresh_frame_context: bool,
    pub frame_parallel_decoding_mode: bool,
    pub segmentation_enabled: bool,
    pub allow_high_precision_mv: bool,
    pub use_prev_frame_mvs: bool,
}

// ---------------------------------------------------------------------------
// VP9 Standard Picture Info
// ---------------------------------------------------------------------------

/// VP9 standard picture info.
///
/// Corresponds to `StdVideoDecodeVP9PictureInfo`.
#[derive(Debug, Clone, Default)]
pub struct Vp9StdPictureInfo {
    pub flags: Vp9PictureInfoFlags,
    pub profile: Vp9Profile,
    pub frame_type: Vp9FrameType,
    pub base_q_idx: u8,
    pub delta_q_y_dc: i32,
    pub delta_q_uv_dc: i32,
    pub delta_q_uv_ac: i32,
    pub refresh_frame_flags: u8,
    pub ref_frame_sign_bias_mask: u8,
    pub interpolation_filter: Vp9InterpolationFilter,
    pub frame_context_idx: u8,
    pub reset_frame_context: u8,
    pub tile_cols_log2: u8,
    pub tile_rows_log2: u8,
}

// ---------------------------------------------------------------------------
// VP9 Picture Data  (codec-specific picture parameters)
// ---------------------------------------------------------------------------

/// VP9 picture data — full set of parameters extracted from the frame header.
///
/// Corresponds to `VkParserVp9PictureData`.
#[derive(Debug, Clone, Default)]
pub struct Vp9PictureData {
    pub std_picture_info: Vp9StdPictureInfo,
    pub std_color_config: Vp9ColorConfig,
    pub std_loop_filter: Vp9LoopFilter,
    pub std_segmentation: Vp9Segmentation,

    pub show_existing_frame: bool,
    pub frame_to_show_map_idx: u8,
    pub frame_is_intra: bool,

    /// Reference frame indices for LAST, GOLDEN, ALTREF (3 entries).
    pub ref_frame_idx: [u8; VP9_REFS_PER_FRAME],
    /// Picture buffer indices for all 8 reference slots.
    pub pic_idx: [i32; VP9_NUM_REF_FRAMES],

    pub frame_width: u32,
    pub frame_height: u32,
    pub render_width: u32,
    pub render_height: u32,

    pub mi_cols: u32,
    pub mi_rows: u32,
    pub sb64_cols: u32,
    pub sb64_rows: u32,

    pub chroma_format: u32,

    pub uncompressed_header_offset: u32,
    pub compressed_header_offset: u32,
    pub compressed_header_size: u32,
    pub tiles_offset: u32,
    pub num_tiles: u32,
}

// ---------------------------------------------------------------------------
// VP9 Reference Frame Slot
// ---------------------------------------------------------------------------

/// A reference frame buffer entry.
///
/// Corresponds to `vp9_ref_frames_s`.
#[derive(Debug, Clone, Default)]
pub struct Vp9RefFrame {
    /// Index into an external picture buffer pool, or `None` if empty.
    pub buffer_idx: Option<i32>,
    pub frame_type: Vp9FrameType,
    pub segmentation_enabled: bool,
    /// Decode width stored on the reference picture.
    pub decode_width: u32,
    /// Decode height stored on the reference picture.
    pub decode_height: u32,
}

// ---------------------------------------------------------------------------
// Bitstream Reader  (unsigned integer extraction — corresponds to `u()` calls
// in the C++ parser via the VulkanVideoDecoder base class)
// ---------------------------------------------------------------------------

/// A simple MSB-first bitstream reader for VP9 uncompressed header parsing.
///
/// In the C++ code, bitstream reading (`u(n)`) is provided by the
/// `VulkanVideoDecoder` base class. We provide a standalone reader here
/// so the VP9 parser can be unit-tested independently.
#[derive(Debug)]
pub struct BitstreamReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitstreamReader<'a> {
    /// Create a new reader over `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    /// Read `n` bits (1..=32) as an unsigned value, MSB first.
    ///
    /// Corresponds to the `u(n)` calls in the C++ code.
    pub fn u(&mut self, n: u32) -> u32 {
        debug_assert!(n >= 1 && n <= 32);
        let mut value: u32 = 0;
        for _ in 0..n {
            let byte_idx = self.bit_offset >> 3;
            let bit_idx = 7 - (self.bit_offset & 7);
            if byte_idx < self.data.len() {
                value = (value << 1) | (((self.data[byte_idx] >> bit_idx) & 1) as u32);
            } else {
                // Reading past the end — return zeros (matches C++ behavior of
                // reading from a zeroed buffer past the end).
                value <<= 1;
            }
            self.bit_offset += 1;
        }
        value
    }

    /// Return the number of bits consumed so far.
    pub fn consumed_bits(&self) -> u32 {
        self.bit_offset as u32
    }

    /// Return remaining bits.
    pub fn remaining_bits(&self) -> usize {
        self.data.len().saturating_mul(8).saturating_sub(self.bit_offset)
    }
}

// ---------------------------------------------------------------------------
// VulkanVP9Decoder  (the main decoder state machine)
// ---------------------------------------------------------------------------

/// VP9 decoder state — faithful port of the `VulkanVP9Decoder` C++ class.
///
/// The C++ class inherits from `VulkanVideoDecoder`; in Rust we store the
/// VP9-specific state here and reference shared base types through
/// `super::vulkan_video_decoder` (when available) or standalone equivalents.
#[derive(Debug)]
pub struct VulkanVp9Decoder {
    /// Current VP9 picture data being built up during header parsing.
    pub pic_data: Vp9PictureData,

    /// Current picture buffer index (or `None`).
    pub curr_pic: Option<i32>,

    /// Frame index counter (-1 = not started).
    pub frame_idx: i32,

    pub data_size: i32,
    pub frame_size: i32,
    pub frame_size_changed: bool,

    pub rt_orig_width: i32,
    pub rt_orig_height: i32,
    pub picture_started: bool,
    pub bitstream_complete: bool,

    // Parsing state for compute_image_size() side effects.
    pub last_frame_width: i32,
    pub last_frame_height: i32,
    pub last_show_frame: bool,

    /// Last used loop filter reference deltas (persisted across frames).
    pub loop_filter_ref_deltas: [i8; VP9_MAX_REF_FRAMES],
    /// Last used loop filter mode deltas (persisted across frames).
    pub loop_filter_mode_deltas: [i8; VP9_LOOP_FILTER_ADJUSTMENTS],

    /// Reference frame buffer pool (up to `VP9_BUFFER_POOL_MAX_SIZE`).
    pub buffers: [Vp9RefFrame; VP9_BUFFER_POOL_MAX_SIZE],
}

impl Default for VulkanVp9Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VulkanVp9Decoder {
    /// Create a new VP9 decoder with default state.
    ///
    /// Corresponds to `VulkanVP9Decoder::VulkanVP9Decoder(VkVideoCodecOperationFlagBitsKHR)`.
    pub fn new() -> Self {
        Self {
            pic_data: Vp9PictureData::default(),
            curr_pic: None,
            frame_idx: -1,
            data_size: 0,
            frame_size: 0,
            frame_size_changed: false,
            rt_orig_width: 0,
            rt_orig_height: 0,
            picture_started: false,
            bitstream_complete: true,
            last_frame_width: 0,
            last_frame_height: 0,
            last_show_frame: false,
            loop_filter_ref_deltas: [0i8; VP9_MAX_REF_FRAMES],
            loop_filter_mode_deltas: [0i8; VP9_LOOP_FILTER_ADJUSTMENTS],
            buffers: Default::default(),
        }
    }

    /// Reset the parser to its initial state.
    ///
    /// Corresponds to `VulkanVP9Decoder::InitParser()`.
    pub fn init_parser(&mut self) {
        self.curr_pic = None;
        self.bitstream_complete = true;
        self.picture_started = false;
        self.end_of_stream();
    }

    /// Release all reference buffers.
    ///
    /// Corresponds to `VulkanVP9Decoder::EndOfStream()`.
    pub fn end_of_stream(&mut self) {
        self.curr_pic = None;
        for i in 0..VP9_NUM_REF_FRAMES {
            self.buffers[i].buffer_idx = None;
        }
    }

    /// Update reference frame buffer pointers after decoding a frame.
    ///
    /// For each bit set in `refresh_frame_flags`, the corresponding reference
    /// slot is updated to point to `current_picture`.
    ///
    /// Corresponds to `VulkanVP9Decoder::UpdateFramePointers()`.
    pub fn update_frame_pointers(
        &mut self,
        current_picture: Option<i32>,
        decode_width: u32,
        decode_height: u32,
    ) {
        let refresh = self.pic_data.std_picture_info.refresh_frame_flags;
        let mut mask = refresh as u32;
        let mut ref_index: usize = 0;

        while mask != 0 {
            if (mask & 1) != 0 {
                self.buffers[ref_index].buffer_idx = current_picture;
                self.buffers[ref_index].decode_width = decode_width;
                self.buffers[ref_index].decode_height = decode_height;
            }
            mask >>= 1;
            ref_index += 1;
        }
    }

    // -----------------------------------------------------------------------
    // Uncompressed header parsing
    // -----------------------------------------------------------------------

    /// Parse the VP9 uncompressed header from the given bitstream reader.
    ///
    /// Populates `self.pic_data` with all header fields.
    /// Returns `true` on success, `false` on parse error.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseUncompressedHeader()`.
    pub fn parse_uncompressed_header(&mut self, bs: &mut BitstreamReader) -> bool {
        self.frame_size_changed = false;

        // VP9_CHECK_FRAME_MARKER
        if bs.u(2) != VP9_FRAME_MARKER {
            tracing::error!("Invalid VP9 frame marker");
            return false;
        }

        // Profile
        let mut profile = bs.u(1);
        profile |= bs.u(1) << 1;
        self.pic_data.std_picture_info.profile = match profile {
            0 => Vp9Profile::Profile0,
            1 => Vp9Profile::Profile1,
            2 => Vp9Profile::Profile2,
            3 => Vp9Profile::Profile3,
            _ => unreachable!(),
        };
        if self.pic_data.std_picture_info.profile == Vp9Profile::Profile3 {
            if bs.u(1) != 0 {
                tracing::error!("Invalid syntax: reserved bit must be 0 for profile 3");
                return false;
            }
        }

        // show_existing_frame
        self.pic_data.show_existing_frame = bs.u(1) != 0;
        if self.pic_data.show_existing_frame {
            self.pic_data.frame_to_show_map_idx = bs.u(3) as u8;
            self.pic_data.uncompressed_header_offset = (bs.consumed_bits() + 7) >> 3;
            self.pic_data.compressed_header_size = 0;
            self.pic_data.std_picture_info.refresh_frame_flags = 0;
            self.pic_data.std_loop_filter.loop_filter_level = 0;
            return true;
        }

        // frame_type
        self.pic_data.std_picture_info.frame_type = if bs.u(1) == 0 {
            Vp9FrameType::Key
        } else {
            Vp9FrameType::Inter
        };
        self.pic_data.std_picture_info.flags.show_frame = bs.u(1) != 0;
        self.pic_data.std_picture_info.flags.error_resilient_mode = bs.u(1) != 0;

        if self.pic_data.std_picture_info.frame_type == Vp9FrameType::Key {
            // VP9_CHECK_FRAME_SYNC_CODE
            if bs.u(24) != VP9_FRAME_SYNC_CODE {
                tracing::error!("Invalid VP9 frame sync code");
            }
            Self::parse_color_config_static(
                &mut self.pic_data.std_picture_info,
                &mut self.pic_data.std_color_config,
                bs,
            );
            Self::parse_frame_and_render_size_static(&mut self.pic_data, bs);
            self.compute_image_size();
            self.pic_data.std_picture_info.refresh_frame_flags = 0xFF; // (1 << NUM_REF_FRAMES) - 1
            self.pic_data.frame_is_intra = true;

            for i in 0..VP9_REFS_PER_FRAME {
                self.pic_data.ref_frame_idx[i] = 0;
            }
        } else {
            // Non-key frame
            self.pic_data.std_picture_info.flags.intra_only =
                if self.pic_data.std_picture_info.flags.show_frame {
                    false
                } else {
                    bs.u(1) != 0
                };
            self.pic_data.frame_is_intra = self.pic_data.std_picture_info.flags.intra_only;

            self.pic_data.std_picture_info.reset_frame_context =
                if self.pic_data.std_picture_info.flags.error_resilient_mode {
                    0
                } else {
                    bs.u(2) as u8
                };

            if self.pic_data.std_picture_info.flags.intra_only {
                // VP9_CHECK_FRAME_SYNC_CODE
                if bs.u(24) != VP9_FRAME_SYNC_CODE {
                    tracing::error!("Invalid VP9 frame sync code");
                }

                if self.pic_data.std_picture_info.profile > Vp9Profile::Profile0 {
                    Self::parse_color_config_static(
                        &mut self.pic_data.std_picture_info,
                        &mut self.pic_data.std_color_config,
                        bs,
                    );
                } else {
                    self.pic_data.std_color_config.color_space = Vp9ColorSpace::Bt601;
                    self.pic_data.std_color_config.subsampling_x = 1;
                    self.pic_data.std_color_config.subsampling_y = 1;
                    self.pic_data.std_color_config.bit_depth = 8;
                }

                self.pic_data.std_picture_info.refresh_frame_flags =
                    bs.u(VP9_NUM_REF_FRAMES as u32) as u8;

                Self::parse_frame_and_render_size_static(&mut self.pic_data, bs);
                self.compute_image_size();
            } else {
                // Inter frame
                self.pic_data.std_picture_info.refresh_frame_flags =
                    bs.u(VP9_NUM_REF_FRAMES as u32) as u8;

                self.pic_data.std_picture_info.ref_frame_sign_bias_mask = 0;
                for i in 0..VP9_REFS_PER_FRAME {
                    self.pic_data.ref_frame_idx[i] = bs.u(3) as u8;
                    let sign_bias = bs.u(1) as u8;
                    // STD_VIDEO_VP9_REFERENCE_NAME_LAST_FRAME = 1
                    self.pic_data.std_picture_info.ref_frame_sign_bias_mask |=
                        sign_bias << (1 + i);
                }

                self.parse_frame_and_render_size_with_refs(bs);

                self.pic_data.std_picture_info.flags.allow_high_precision_mv = bs.u(1) != 0;

                // Interpolation filter
                let is_filter_switchable = bs.u(1) != 0;
                if is_filter_switchable {
                    self.pic_data.std_picture_info.interpolation_filter =
                        Vp9InterpolationFilter::Switchable;
                } else {
                    let literal_to_filter = [
                        Vp9InterpolationFilter::EighttapSmooth,
                        Vp9InterpolationFilter::Eighttap,
                        Vp9InterpolationFilter::EighttapSharp,
                        Vp9InterpolationFilter::Bilinear,
                    ];
                    let idx = bs.u(2) as usize;
                    self.pic_data.std_picture_info.interpolation_filter = literal_to_filter[idx];
                }
            }
        }

        // refresh_frame_context / frame_parallel_decoding_mode
        if !self.pic_data.std_picture_info.flags.error_resilient_mode {
            self.pic_data.std_picture_info.flags.refresh_frame_context = bs.u(1) != 0;
            self.pic_data.std_picture_info.flags.frame_parallel_decoding_mode = bs.u(1) != 0;
        } else {
            self.pic_data.std_picture_info.flags.refresh_frame_context = false;
            self.pic_data.std_picture_info.flags.frame_parallel_decoding_mode = true;
        }

        self.pic_data.std_picture_info.frame_context_idx = bs.u(2) as u8;

        if self.pic_data.frame_is_intra || self.pic_data.std_picture_info.flags.error_resilient_mode {
            // setup_past_independence() — clear previous segment data
            self.pic_data.std_segmentation.feature_enabled = [0u8; VP9_MAX_SEGMENTS];
            self.pic_data.std_segmentation.feature_data = [[0i16; VP9_SEG_LVL_MAX]; VP9_MAX_SEGMENTS];
            self.pic_data.std_picture_info.frame_context_idx = 0;
        }

        self.parse_loop_filter_params(bs);
        Self::parse_quantization_params(&mut self.pic_data, bs);
        self.parse_segmentation_params(bs);
        Self::parse_tile_info_static(&mut self.pic_data, bs);

        self.pic_data.compressed_header_size = bs.u(16);

        self.pic_data.uncompressed_header_offset = 0;
        self.pic_data.compressed_header_offset = (bs.consumed_bits() + 7) >> 3;
        self.pic_data.tiles_offset =
            self.pic_data.compressed_header_offset + self.pic_data.compressed_header_size;

        self.pic_data.chroma_format =
            if self.pic_data.std_color_config.subsampling_x == 1
                && self.pic_data.std_color_config.subsampling_y == 1
            {
                1
            } else {
                0
            };

        true
    }

    // -----------------------------------------------------------------------
    // Color config
    // -----------------------------------------------------------------------

    /// Parse color configuration from the bitstream.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseColorConfig()`.
    fn parse_color_config_static(
        pic_info: &mut Vp9StdPictureInfo,
        color_config: &mut Vp9ColorConfig,
        bs: &mut BitstreamReader,
    ) -> bool {
        if pic_info.profile >= Vp9Profile::Profile2 {
            color_config.bit_depth = if bs.u(1) != 0 { 12 } else { 10 };
        } else {
            color_config.bit_depth = 8;
        }

        color_config.color_space = Vp9ColorSpace::from_raw(bs.u(3));

        if color_config.color_space != Vp9ColorSpace::Rgb {
            color_config.color_range = bs.u(1) != 0;
            if pic_info.profile == Vp9Profile::Profile1 || pic_info.profile == Vp9Profile::Profile3
            {
                color_config.subsampling_x = bs.u(1) as u8;
                color_config.subsampling_y = bs.u(1) as u8;
                // VP9_CHECK_ZERO_BIT
                if bs.u(1) != 0 {
                    tracing::error!("Invalid syntax: reserved zero bit");
                    return false;
                }
            } else {
                color_config.subsampling_x = 1;
                color_config.subsampling_y = 1;
            }
        } else {
            color_config.color_range = true;
            if pic_info.profile == Vp9Profile::Profile1 || pic_info.profile == Vp9Profile::Profile3
            {
                color_config.subsampling_x = 0;
                color_config.subsampling_y = 0;
                // VP9_CHECK_ZERO_BIT
                if bs.u(1) != 0 {
                    tracing::error!("Invalid syntax: reserved zero bit");
                    return false;
                }
            }
        }
        true
    }

    // -----------------------------------------------------------------------
    // Frame / render size
    // -----------------------------------------------------------------------

    /// Parse frame width/height and optional render size.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseFrameAndRenderSize()`.
    /// Note: Does NOT call compute_image_size — caller must do that.
    fn parse_frame_and_render_size_static(
        pic_data: &mut Vp9PictureData,
        bs: &mut BitstreamReader,
    ) {
        pic_data.frame_width = bs.u(16) + 1;
        pic_data.frame_height = bs.u(16) + 1;

        if bs.u(1) == 1 {
            // render_and_frame_size_different
            pic_data.render_width = bs.u(16) + 1;
            pic_data.render_height = bs.u(16) + 1;
        } else {
            pic_data.render_width = pic_data.frame_width;
            pic_data.render_height = pic_data.frame_height;
        }
    }

    /// Parse frame size from reference frames, falling back to explicit size.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseFrameAndRenderSizeWithRefs()`.
    fn parse_frame_and_render_size_with_refs(&mut self, bs: &mut BitstreamReader) {
        let mut found_ref = false;

        for i in 0..VP9_REFS_PER_FRAME {
            if bs.u(1) != 0 {
                found_ref = true;
                let ref_idx = self.pic_data.ref_frame_idx[i] as usize;
                if self.buffers[ref_idx].buffer_idx.is_some() {
                    self.pic_data.frame_width = self.buffers[ref_idx].decode_width;
                    self.pic_data.frame_height = self.buffers[ref_idx].decode_height;
                }
                self.compute_image_size();

                if bs.u(1) == 1 {
                    self.pic_data.render_width = bs.u(16) + 1;
                    self.pic_data.render_height = bs.u(16) + 1;
                } else {
                    self.pic_data.render_width = self.pic_data.frame_width;
                    self.pic_data.render_height = self.pic_data.frame_height;
                }

                break;
            }
        }
        if !found_ref {
            Self::parse_frame_and_render_size_static(&mut self.pic_data, bs);
            self.compute_image_size();
        }
    }

    /// Compute derived image size fields from FrameWidth / FrameHeight.
    ///
    /// Also implements the "compute_image_size() side effects" from spec §7.2.6.
    ///
    /// Corresponds to `VulkanVP9Decoder::ComputeImageSize()`.
    fn compute_image_size(&mut self) {
        let pic_data = &mut self.pic_data;

        pic_data.mi_cols = (pic_data.frame_width + 7) >> 3;
        pic_data.mi_rows = (pic_data.frame_height + 7) >> 3;
        pic_data.sb64_cols = (pic_data.mi_cols + 7) >> 3;
        pic_data.sb64_rows = (pic_data.mi_rows + 7) >> 3;

        // Side effects (spec §7.2.6)
        if self.last_frame_height as u32 != pic_data.frame_height
            || self.last_frame_width as u32 != pic_data.frame_width
        {
            self.frame_size_changed = true;
            pic_data.std_picture_info.flags.use_prev_frame_mvs = false;
        } else {
            let intra_only = pic_data.std_picture_info.frame_type == Vp9FrameType::Key
                || pic_data.std_picture_info.flags.intra_only;
            pic_data.std_picture_info.flags.use_prev_frame_mvs = self.last_show_frame
                && !pic_data.std_picture_info.flags.error_resilient_mode
                && !intra_only;
        }
        self.last_frame_height = pic_data.frame_height as i32;
        self.last_frame_width = pic_data.frame_width as i32;
        self.last_show_frame = pic_data.std_picture_info.flags.show_frame;
    }

    // -----------------------------------------------------------------------
    // Loop filter
    // -----------------------------------------------------------------------

    /// Parse loop filter parameters from the bitstream.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseLoopFilterParams()`.
    fn parse_loop_filter_params(&mut self, bs: &mut BitstreamReader) {
        if self.pic_data.frame_is_intra
            || self.pic_data.std_picture_info.flags.error_resilient_mode
        {
            // setup_past_independence() for loop filter params
            self.loop_filter_ref_deltas = [0i8; VP9_MAX_REF_FRAMES];
            self.loop_filter_mode_deltas = [0i8; VP9_LOOP_FILTER_ADJUSTMENTS];
            self.loop_filter_ref_deltas[0] = 1;
            self.loop_filter_ref_deltas[1] = 0;
            self.loop_filter_ref_deltas[2] = -1;
            self.loop_filter_ref_deltas[3] = -1;
        }

        self.pic_data.std_loop_filter.loop_filter_level = bs.u(6) as u8;
        self.pic_data.std_loop_filter.loop_filter_sharpness = bs.u(3) as u8;

        self.pic_data.std_loop_filter.loop_filter_delta_enabled = bs.u(1) != 0;
        if self.pic_data.std_loop_filter.loop_filter_delta_enabled {
            self.pic_data.std_loop_filter.loop_filter_delta_update = bs.u(1) != 0;

            if self.pic_data.std_loop_filter.loop_filter_delta_update {
                self.pic_data.std_loop_filter.update_ref_delta = 0;
                for i in 0..VP9_MAX_REF_FRAMES {
                    let update_ref_delta = bs.u(1) as u8;
                    self.pic_data.std_loop_filter.update_ref_delta |= update_ref_delta << i;
                    if update_ref_delta == 1 {
                        let mut val = bs.u(6) as i8;
                        if bs.u(1) != 0 {
                            // sign
                            val = -val;
                        }
                        self.loop_filter_ref_deltas[i] = val;
                    }
                }

                self.pic_data.std_loop_filter.update_mode_delta = 0;
                for i in 0..VP9_LOOP_FILTER_ADJUSTMENTS {
                    let update_mode_delta = bs.u(1) as u8;
                    self.pic_data.std_loop_filter.update_mode_delta |= update_mode_delta << i;
                    if update_mode_delta != 0 {
                        let val = bs.u(6) as i8;
                        if bs.u(1) != 0 {
                            // sign — note: C++ has a bug here where it negates
                            // m_loopFilterRefDeltas[i] instead of the mode delta.
                            // We faithfully reproduce this behavior.
                            self.loop_filter_mode_deltas[i] = -self.loop_filter_ref_deltas[i];
                        } else {
                            self.loop_filter_mode_deltas[i] = val;
                        }
                    }
                }
            }
        }

        self.pic_data.std_loop_filter.loop_filter_ref_deltas = self.loop_filter_ref_deltas;
        self.pic_data.std_loop_filter.loop_filter_mode_deltas = self.loop_filter_mode_deltas;
    }

    // -----------------------------------------------------------------------
    // Quantization
    // -----------------------------------------------------------------------

    /// Parse quantization parameters.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseQuantizationParams()`.
    fn parse_quantization_params(pic_data: &mut Vp9PictureData, bs: &mut BitstreamReader) {
        pic_data.std_picture_info.base_q_idx = bs.u(8) as u8;
        pic_data.std_picture_info.delta_q_y_dc = Self::read_delta_q(bs);
        pic_data.std_picture_info.delta_q_uv_dc = Self::read_delta_q(bs);
        pic_data.std_picture_info.delta_q_uv_ac = Self::read_delta_q(bs);
    }

    /// Read a delta-Q value from the bitstream.
    ///
    /// Corresponds to `VulkanVP9Decoder::ReadDeltaQ()`.
    fn read_delta_q(bs: &mut BitstreamReader) -> i32 {
        if bs.u(1) != 0 {
            let delta = bs.u(4) as i32;
            if bs.u(1) != 0 {
                -delta
            } else {
                delta
            }
        } else {
            0
        }
    }

    // -----------------------------------------------------------------------
    // Segmentation
    // -----------------------------------------------------------------------

    /// Parse segmentation parameters.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseSegmentationParams()`.
    fn parse_segmentation_params(&mut self, bs: &mut BitstreamReader) {
        let segmentation_feature_bits: [u32; VP9_SEG_LVL_MAX] = [8, 6, 2, 0];
        let segmentation_feature_signed: [bool; VP9_SEG_LVL_MAX] = [true, true, false, false];

        let segment = &mut self.pic_data.std_segmentation;

        segment.flags.segmentation_update_map = false;
        segment.flags.segmentation_temporal_update = false;

        self.pic_data.std_picture_info.flags.segmentation_enabled = bs.u(1) != 0;
        if !self.pic_data.std_picture_info.flags.segmentation_enabled {
            return;
        }

        let segment = &mut self.pic_data.std_segmentation;

        segment.flags.segmentation_update_map = bs.u(1) != 0;

        if segment.flags.segmentation_update_map {
            for i in 0..VP9_MAX_SEGMENTATION_TREE_PROBS {
                let prob_coded = bs.u(1);
                segment.segmentation_tree_probs[i] = if prob_coded == 1 {
                    bs.u(8) as u8
                } else {
                    VP9_MAX_PROBABILITY
                };
            }

            segment.flags.segmentation_temporal_update = bs.u(1) != 0;
            for i in 0..VP9_MAX_SEGMENTATION_PRED_PROB {
                if segment.flags.segmentation_temporal_update {
                    let prob_coded = bs.u(1);
                    segment.segmentation_pred_prob[i] = if prob_coded == 1 {
                        bs.u(8) as u8
                    } else {
                        VP9_MAX_PROBABILITY
                    };
                } else {
                    segment.segmentation_pred_prob[i] = VP9_MAX_PROBABILITY;
                }
            }
        }

        let segment = &mut self.pic_data.std_segmentation;

        segment.flags.segmentation_update_data = bs.u(1) != 0;
        if segment.flags.segmentation_update_data {
            segment.flags.segmentation_abs_or_delta_update = bs.u(1) != 0;

            // Clear all previous segment data
            segment.feature_enabled = [0u8; VP9_MAX_SEGMENTS];
            segment.feature_data = [[0i16; VP9_SEG_LVL_MAX]; VP9_MAX_SEGMENTS];

            for i in 0..VP9_MAX_SEGMENTS {
                for j in 0..VP9_SEG_LVL_MAX {
                    let feature_enabled = bs.u(1) as u8;
                    segment.feature_enabled[i] |= feature_enabled << j;

                    if feature_enabled == 1 {
                        let bits = segmentation_feature_bits[j];
                        let mut val = if bits > 0 { bs.u(bits) as i16 } else { 0 };

                        if segmentation_feature_signed[j] {
                            if bs.u(1) == 1 {
                                val = -val;
                            }
                        }
                        segment.feature_data[i][j] = val;
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Tile info
    // -----------------------------------------------------------------------

    /// Calculate minimum log2 of tile columns.
    ///
    /// Corresponds to `VulkanVP9Decoder::CalcMinLog2TileCols()`.
    pub fn calc_min_log2_tile_cols(sb64_cols: u32) -> u8 {
        let mut min_log2: u8 = 0;
        while (VP9_MAX_TILE_WIDTH_B64 << min_log2) < sb64_cols {
            min_log2 += 1;
        }
        min_log2
    }

    /// Calculate maximum log2 of tile columns.
    ///
    /// Corresponds to `VulkanVP9Decoder::CalcMaxLog2TileCols()`.
    pub fn calc_max_log2_tile_cols(sb64_cols: u32) -> u8 {
        let mut max_log2: u8 = 1;
        while (sb64_cols >> max_log2) >= VP9_MIN_TILE_WIDTH_B64 {
            max_log2 += 1;
        }
        max_log2 - 1
    }

    /// Parse tile info from the bitstream.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseTileInfo()`.
    fn parse_tile_info_static(pic_data: &mut Vp9PictureData, bs: &mut BitstreamReader) {
        let min_log2 = Self::calc_min_log2_tile_cols(pic_data.sb64_cols);
        let max_log2 = Self::calc_max_log2_tile_cols(pic_data.sb64_cols);

        pic_data.std_picture_info.tile_cols_log2 = min_log2;

        while pic_data.std_picture_info.tile_cols_log2 < max_log2 {
            if bs.u(1) == 1 {
                pic_data.std_picture_info.tile_cols_log2 += 1;
            } else {
                break;
            }
        }

        pic_data.std_picture_info.tile_rows_log2 = bs.u(1) as u8;
        if pic_data.std_picture_info.tile_rows_log2 == 1 {
            pic_data.std_picture_info.tile_rows_log2 += bs.u(1) as u8;
        }

        pic_data.num_tiles = (1u32 << pic_data.std_picture_info.tile_rows_log2)
            * (1u32 << pic_data.std_picture_info.tile_cols_log2);
    }

    // -----------------------------------------------------------------------
    // Super frame index parsing
    // -----------------------------------------------------------------------

    /// Parse a VP9 superframe index from the end of the data buffer.
    ///
    /// If the buffer ends with a valid superframe index marker, writes the
    /// per-frame sizes into `frame_sizes` and returns the frame count.
    /// Otherwise returns 0.
    ///
    /// Corresponds to `VulkanVP9Decoder::ParseSuperFrameIndex()`.
    pub fn parse_superframe_index(data: &[u8]) -> (Vec<u32>, u32) {
        let data_sz = data.len() as u32;
        if data_sz == 0 {
            return (Vec::new(), 0);
        }

        let final_byte = data[data_sz as usize - 1];

        if (final_byte & 0xe0) != 0xc0 {
            return (Vec::new(), 0);
        }

        let frames = ((final_byte & 0x7) + 1) as u32;
        let mag = (((final_byte >> 3) & 0x3) + 1) as u32;
        let index_sz = 2 + mag * frames;

        if data_sz < index_sz {
            return (Vec::new(), 0);
        }

        if data[(data_sz - index_sz) as usize] != final_byte {
            return (Vec::new(), 0);
        }

        // Found a valid superframe index
        let mut frame_sizes = Vec::with_capacity(frames as usize);
        let mut x = (data_sz - index_sz + 1) as usize;

        for _ in 0..frames {
            let mut this_sz: u32 = 0;
            for j in 0..mag {
                this_sz |= (data[x] as u32) << (j * 8);
                x += 1;
            }
            frame_sizes.push(this_sz);
        }

        (frame_sizes, frames)
    }
}

// ===========================================================================
// Unit Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // BitstreamReader tests
    // -----------------------------------------------------------------------

    #[test]
    fn bitstream_reader_single_bits() {
        // 0b10110100 = 0xB4
        let data = [0xB4u8];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(bs.u(1), 1);
        assert_eq!(bs.u(1), 0);
        assert_eq!(bs.u(1), 1);
        assert_eq!(bs.u(1), 1);
        assert_eq!(bs.u(1), 0);
        assert_eq!(bs.u(1), 1);
        assert_eq!(bs.u(1), 0);
        assert_eq!(bs.u(1), 0);
    }

    #[test]
    fn bitstream_reader_multi_bit() {
        // 0xAB = 0b10101011, 0xCD = 0b11001101
        let data = [0xAB, 0xCD];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(bs.u(4), 0b1010); // 0xA
        assert_eq!(bs.u(4), 0b1011); // 0xB
        assert_eq!(bs.u(8), 0xCD);
    }

    #[test]
    fn bitstream_reader_cross_byte_boundary() {
        let data = [0xFF, 0x00];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(bs.u(4), 0xF);
        // Next 8 bits span the byte boundary: 4 bits of 0xFF (low nibble = 0xF)
        // plus 4 bits of 0x00 (high nibble = 0x0).
        assert_eq!(bs.u(8), 0xF0);
    }

    #[test]
    fn bitstream_reader_consumed_bits() {
        let data = [0x00; 4];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(bs.consumed_bits(), 0);
        bs.u(3);
        assert_eq!(bs.consumed_bits(), 3);
        bs.u(16);
        assert_eq!(bs.consumed_bits(), 19);
    }

    #[test]
    fn bitstream_reader_past_end_returns_zero() {
        let data = [0xFF];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(bs.u(8), 0xFF);
        // Reading past the end should produce zeros
        assert_eq!(bs.u(8), 0x00);
    }

    #[test]
    fn bitstream_reader_24_bit() {
        // Read the VP9 sync code
        let data = [0x49, 0x83, 0x42]; // VP9_FRAME_SYNC_CODE = 0x498342
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(bs.u(24), VP9_FRAME_SYNC_CODE);
    }

    // -----------------------------------------------------------------------
    // Frame marker validation
    // -----------------------------------------------------------------------

    #[test]
    fn parse_uncompressed_header_rejects_bad_frame_marker() {
        let mut dec = VulkanVp9Decoder::new();
        // Frame marker should be 2 (0b10). Use 0b11 = 3 to trigger failure.
        // 0b11_000000 = 0xC0
        let data = [0xC0, 0x00, 0x00, 0x00];
        let mut bs = BitstreamReader::new(&data);
        assert!(!dec.parse_uncompressed_header(&mut bs));
    }

    #[test]
    fn parse_uncompressed_header_accepts_valid_frame_marker() {
        // Build a minimal keyframe header:
        // 2 bits frame_marker = 0b10
        // 2 bits profile = 0b00  (Profile 0)
        // 1 bit show_existing_frame = 1
        // 3 bits frame_to_show_map_idx = 0b000
        // = 0b10_00_1_000 = 0x88
        let data = [0x88, 0x00, 0x00, 0x00];
        let mut dec = VulkanVp9Decoder::new();
        let mut bs = BitstreamReader::new(&data);
        assert!(dec.parse_uncompressed_header(&mut bs));
        assert!(dec.pic_data.show_existing_frame);
        assert_eq!(dec.pic_data.frame_to_show_map_idx, 0);
    }

    // -----------------------------------------------------------------------
    // show_existing_frame
    // -----------------------------------------------------------------------

    #[test]
    fn parse_show_existing_frame() {
        // frame_marker=0b10, profile=0b00, show_existing_frame=1, frame_to_show_map_idx=0b101
        // = 0b10_00_1_101 = 0x8D
        let data = [0x8D, 0x00];
        let mut dec = VulkanVp9Decoder::new();
        let mut bs = BitstreamReader::new(&data);
        assert!(dec.parse_uncompressed_header(&mut bs));
        assert!(dec.pic_data.show_existing_frame);
        assert_eq!(dec.pic_data.frame_to_show_map_idx, 5);
        assert_eq!(dec.pic_data.std_picture_info.refresh_frame_flags, 0);
    }

    // -----------------------------------------------------------------------
    // ReadDeltaQ
    // -----------------------------------------------------------------------

    #[test]
    fn read_delta_q_zero() {
        // First bit = 0 means delta = 0
        let data = [0x00];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanVp9Decoder::read_delta_q(&mut bs), 0);
        assert_eq!(bs.consumed_bits(), 1);
    }

    #[test]
    fn read_delta_q_positive() {
        // 1 bit present=1, 4 bits value=0b1010 (10), 1 bit sign=0 (positive)
        // = 0b1_1010_0_xx = 0b11010000 = 0xD0
        let data = [0xD0];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanVp9Decoder::read_delta_q(&mut bs), 10);
    }

    #[test]
    fn read_delta_q_negative() {
        // 1 bit present=1, 4 bits value=0b0011 (3), 1 bit sign=1 (negative)
        // = 0b1_0011_1_xx = 0b10011100 = 0x9C
        let data = [0x9C];
        let mut bs = BitstreamReader::new(&data);
        assert_eq!(VulkanVp9Decoder::read_delta_q(&mut bs), -3);
    }

    // -----------------------------------------------------------------------
    // Tile column log2 calculations
    // -----------------------------------------------------------------------

    #[test]
    fn calc_min_log2_tile_cols_small() {
        // sb64_cols = 1 -> VP9_MAX_TILE_WIDTH_B64 (64) << 0 = 64 >= 1, so min = 0
        assert_eq!(VulkanVp9Decoder::calc_min_log2_tile_cols(1), 0);
    }

    #[test]
    fn calc_min_log2_tile_cols_large() {
        // sb64_cols = 128 -> 64 << 0 = 64 < 128, need 64 << 1 = 128 >= 128, so min = 1
        assert_eq!(VulkanVp9Decoder::calc_min_log2_tile_cols(128), 1);
    }

    #[test]
    fn calc_min_log2_tile_cols_exact() {
        // sb64_cols = 64 -> 64 << 0 = 64 >= 64, so min = 0
        assert_eq!(VulkanVp9Decoder::calc_min_log2_tile_cols(64), 0);
    }

    #[test]
    fn calc_max_log2_tile_cols_small() {
        // sb64_cols = 4 -> (4 >> 1) = 2 < VP9_MIN_TILE_WIDTH_B64(4), so max = 1-1 = 0
        assert_eq!(VulkanVp9Decoder::calc_max_log2_tile_cols(4), 0);
    }

    #[test]
    fn calc_max_log2_tile_cols_typical() {
        // sb64_cols = 16 -> (16 >> 1)=8>=4, (16 >> 2)=4>=4, (16 >> 3)=2<4, so max = 3-1 = 2
        assert_eq!(VulkanVp9Decoder::calc_max_log2_tile_cols(16), 2);
    }

    // -----------------------------------------------------------------------
    // Superframe index parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_superframe_index_no_index() {
        // Data that does NOT end with a superframe marker
        let data = [0x00, 0x01, 0x02, 0x03];
        let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
        assert_eq!(count, 0);
        assert!(sizes.is_empty());
    }

    #[test]
    fn parse_superframe_index_single_byte_mag() {
        // Build a superframe index with:
        //   frames = 2 (marker low 3 bits = 1, since frames = (marker & 0x7) + 1)
        //   mag = 1 (bits [4:3] = 0, since mag = ((marker >> 3) & 0x3) + 1)
        //   marker = 0b110_00_001 = 0xC1
        //   index_sz = 2 + 1 * 2 = 4
        //   frame_sizes = [0x0A, 0x14] (10, 20)
        // Layout: [... payload ... marker, size0, size1, marker]
        // Total data must be >= index_sz=4 and data[data_sz - index_sz] == marker
        let marker = 0xC1u8;
        let data = [marker, 0x0A, 0x14, marker];
        let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
        assert_eq!(count, 2);
        assert_eq!(sizes, vec![0x0A, 0x14]);
    }

    #[test]
    fn parse_superframe_index_two_byte_mag() {
        // frames = 2 (low 3 bits = 1)
        // mag = 2 (bits [4:3] = 01, since mag = 1 + 1 = 2)
        // marker = 0b110_01_001 = 0xC9
        // index_sz = 2 + 2 * 2 = 6
        // frame_sizes[0] = 0x0100 (little-endian: 0x00, 0x01)
        // frame_sizes[1] = 0x0200 (little-endian: 0x00, 0x02)
        let marker = 0xC9u8;
        let data = [marker, 0x00, 0x01, 0x00, 0x02, marker];
        let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
        assert_eq!(count, 2);
        assert_eq!(sizes, vec![0x0100, 0x0200]);
    }

    #[test]
    fn parse_superframe_index_empty_data() {
        let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&[]);
        assert_eq!(count, 0);
        assert!(sizes.is_empty());
    }

    #[test]
    fn parse_superframe_index_mismatched_marker() {
        // Final byte is a valid marker pattern but the mirror byte at
        // data[data_sz - index_sz] doesn't match.
        let marker = 0xC1u8;
        let data = [0x00, 0x0A, 0x14, marker]; // data[0] = 0x00 != marker
        let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
        assert_eq!(count, 0);
        assert!(sizes.is_empty());
    }

    // -----------------------------------------------------------------------
    // Reference frame management
    // -----------------------------------------------------------------------

    #[test]
    fn update_frame_pointers_single_slot() {
        let mut dec = VulkanVp9Decoder::new();
        // refresh_frame_flags = 0b00000001 (only slot 0)
        dec.pic_data.std_picture_info.refresh_frame_flags = 0x01;
        dec.update_frame_pointers(Some(42), 1920, 1080);

        assert_eq!(dec.buffers[0].buffer_idx, Some(42));
        assert_eq!(dec.buffers[0].decode_width, 1920);
        assert_eq!(dec.buffers[0].decode_height, 1080);
        // Other slots unchanged
        assert_eq!(dec.buffers[1].buffer_idx, None);
        assert_eq!(dec.buffers[7].buffer_idx, None);
    }

    #[test]
    fn update_frame_pointers_all_slots() {
        let mut dec = VulkanVp9Decoder::new();
        // refresh_frame_flags = 0xFF (all 8 slots)
        dec.pic_data.std_picture_info.refresh_frame_flags = 0xFF;
        dec.update_frame_pointers(Some(7), 640, 480);

        for i in 0..VP9_NUM_REF_FRAMES {
            assert_eq!(dec.buffers[i].buffer_idx, Some(7));
            assert_eq!(dec.buffers[i].decode_width, 640);
        }
    }

    #[test]
    fn update_frame_pointers_alternating() {
        let mut dec = VulkanVp9Decoder::new();
        // refresh_frame_flags = 0b10101010 (slots 1, 3, 5, 7)
        dec.pic_data.std_picture_info.refresh_frame_flags = 0xAA;
        dec.update_frame_pointers(Some(99), 320, 240);

        assert_eq!(dec.buffers[0].buffer_idx, None);
        assert_eq!(dec.buffers[1].buffer_idx, Some(99));
        assert_eq!(dec.buffers[2].buffer_idx, None);
        assert_eq!(dec.buffers[3].buffer_idx, Some(99));
        assert_eq!(dec.buffers[4].buffer_idx, None);
        assert_eq!(dec.buffers[5].buffer_idx, Some(99));
        assert_eq!(dec.buffers[6].buffer_idx, None);
        assert_eq!(dec.buffers[7].buffer_idx, Some(99));
    }

    // -----------------------------------------------------------------------
    // End of stream
    // -----------------------------------------------------------------------

    #[test]
    fn end_of_stream_clears_buffers() {
        let mut dec = VulkanVp9Decoder::new();
        dec.curr_pic = Some(10);
        dec.buffers[0].buffer_idx = Some(1);
        dec.buffers[3].buffer_idx = Some(4);
        dec.buffers[7].buffer_idx = Some(8);

        dec.end_of_stream();

        assert_eq!(dec.curr_pic, None);
        for i in 0..VP9_NUM_REF_FRAMES {
            assert_eq!(dec.buffers[i].buffer_idx, None);
        }
    }

    // -----------------------------------------------------------------------
    // Compute image size
    // -----------------------------------------------------------------------

    #[test]
    fn compute_image_size_basic() {
        let mut dec = VulkanVp9Decoder::new();
        dec.pic_data.frame_width = 1920;
        dec.pic_data.frame_height = 1080;
        dec.pic_data.std_picture_info.flags.show_frame = true;
        dec.compute_image_size();

        // mi_cols = (1920 + 7) / 8 = 240
        assert_eq!(dec.pic_data.mi_cols, 240);
        // mi_rows = (1080 + 7) / 8 = 135 (rounding: (1080+7)>>3 = 1087>>3 = 135)
        assert_eq!(dec.pic_data.mi_rows, 135);
        // sb64_cols = (240 + 7) / 8 = 30 (rounding: 247>>3 = 30)
        assert_eq!(dec.pic_data.sb64_cols, 30);
        // sb64_rows = (135 + 7) / 8 = 17 (rounding: 142>>3 = 17)
        assert_eq!(dec.pic_data.sb64_rows, 17);
    }

    #[test]
    fn compute_image_size_detects_size_change() {
        let mut dec = VulkanVp9Decoder::new();
        dec.last_frame_width = 640;
        dec.last_frame_height = 480;
        dec.pic_data.frame_width = 1920;
        dec.pic_data.frame_height = 1080;

        dec.compute_image_size();

        assert!(dec.frame_size_changed);
        assert!(!dec.pic_data.std_picture_info.flags.use_prev_frame_mvs);
    }

    #[test]
    fn compute_image_size_same_size_inter_frame() {
        let mut dec = VulkanVp9Decoder::new();
        dec.last_frame_width = 1920;
        dec.last_frame_height = 1080;
        dec.last_show_frame = true;
        dec.pic_data.frame_width = 1920;
        dec.pic_data.frame_height = 1080;
        dec.pic_data.std_picture_info.frame_type = Vp9FrameType::Inter;
        dec.pic_data.std_picture_info.flags.error_resilient_mode = false;
        dec.pic_data.std_picture_info.flags.intra_only = false;

        dec.compute_image_size();

        assert!(!dec.frame_size_changed);
        assert!(dec.pic_data.std_picture_info.flags.use_prev_frame_mvs);
    }

    // -----------------------------------------------------------------------
    // Keyframe header parsing (integration-style)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_keyframe_header_minimal() {
        // Build a minimal VP9 keyframe uncompressed header:
        //
        // Bits:
        //   [0..1]   frame_marker = 0b10
        //   [2]      profile_low_bit = 0
        //   [3]      profile_high_bit = 0  -> profile 0
        //   [4]      show_existing_frame = 0
        //   [5]      frame_type = 0 (KEY)
        //   [6]      show_frame = 1
        //   [7]      error_resilient_mode = 0
        //
        //   Byte 0: 0b10_00_0_0_1_0 = 0x82
        //
        //   [8..31]  sync_code = 0x498342 (24 bits)
        //   Bytes 1..3: 0x49, 0x83, 0x42
        //
        //   Color config (profile 0 skips bit_depth, color_space is 3 bits):
        //   [32..34] color_space = 0b001 (BT.601, not RGB)
        //   [35]     color_range = 0
        //   (profile 0: subsampling_x=1, subsampling_y=1 implicit)
        //
        //   [36..51] frame_width - 1 = 0x0077 (16 bits) -> width = 120
        //            Actually let's use width=16, height=16 for simplicity.
        //            width-1 = 15 = 0x000F, height-1 = 15 = 0x000F
        //
        //   [36..51] frame_width-1 = 0x000F  (16 bits)
        //   [52..67] frame_height-1 = 0x000F (16 bits)
        //   [68]     render_and_frame_size_different = 0
        //
        //   Now loop filter:
        //   [69..74] loop_filter_level = 0 (6 bits)
        //   [75..77] loop_filter_sharpness = 0 (3 bits)
        //   [78]     loop_filter_delta_enabled = 0
        //
        //   Quantization:
        //   [79..86] base_q_idx = 0 (8 bits)
        //   [87]     delta_q_y_dc present = 0
        //   [88]     delta_q_uv_dc present = 0
        //   [89]     delta_q_uv_ac present = 0
        //
        //   Segmentation:
        //   [90]     segmentation_enabled = 0
        //
        //   Tile info (sb64_cols for 16px frame):
        //   mi_cols = (16+7)/8 = 2, sb64_cols = (2+7)/8 = 1
        //   min_log2 = 0, max_log2 = 0, so no bits for tile_cols_log2
        //   [91]     tile_rows_log2 = 0
        //
        //   [92..107] compressed_header_size = 0 (16 bits)
        //
        // Let's construct this bitstream carefully.
        let mut bits: Vec<u8> = Vec::new();
        // Helper: we build a bit vector and then pack into bytes.
        let mut bitvec: Vec<u8> = Vec::new();

        fn push_bits(bitvec: &mut Vec<u8>, value: u32, nbits: u32) {
            for i in (0..nbits).rev() {
                bitvec.push(((value >> i) & 1) as u8);
            }
        }

        push_bits(&mut bitvec, 0b10, 2);      // frame_marker
        push_bits(&mut bitvec, 0, 1);          // profile low bit
        push_bits(&mut bitvec, 0, 1);          // profile high bit
        push_bits(&mut bitvec, 0, 1);          // show_existing_frame
        push_bits(&mut bitvec, 0, 1);          // frame_type (KEY=0)
        push_bits(&mut bitvec, 1, 1);          // show_frame
        push_bits(&mut bitvec, 0, 1);          // error_resilient_mode
        push_bits(&mut bitvec, 0x498342, 24);  // sync code
        push_bits(&mut bitvec, 0b001, 3);      // color_space (BT.601)
        push_bits(&mut bitvec, 0, 1);          // color_range
        // subsampling is implicit for profile 0
        push_bits(&mut bitvec, 15, 16);        // frame_width - 1 = 15
        push_bits(&mut bitvec, 15, 16);        // frame_height - 1 = 15
        push_bits(&mut bitvec, 0, 1);          // render_and_frame_size_different = 0
        // refresh_frame_context (since error_resilient=0)
        push_bits(&mut bitvec, 0, 1);          // refresh_frame_context
        push_bits(&mut bitvec, 0, 1);          // frame_parallel_decoding_mode
        push_bits(&mut bitvec, 0, 2);          // frame_context_idx
        // loop filter
        push_bits(&mut bitvec, 0, 6);          // loop_filter_level
        push_bits(&mut bitvec, 0, 3);          // loop_filter_sharpness
        push_bits(&mut bitvec, 0, 1);          // loop_filter_delta_enabled
        // quantization
        push_bits(&mut bitvec, 0, 8);          // base_q_idx
        push_bits(&mut bitvec, 0, 1);          // delta_q_y_dc present
        push_bits(&mut bitvec, 0, 1);          // delta_q_uv_dc present
        push_bits(&mut bitvec, 0, 1);          // delta_q_uv_ac present
        // segmentation
        push_bits(&mut bitvec, 0, 1);          // segmentation_enabled
        // tile info
        // sb64_cols = 1, min_log2=0, max_log2=0, so no increment bits
        push_bits(&mut bitvec, 0, 1);          // tile_rows_log2
        // compressed_header_size
        push_bits(&mut bitvec, 0, 16);         // compressed_header_size

        // Pack bits into bytes
        let byte_len = (bitvec.len() + 7) / 8;
        bits.resize(byte_len, 0);
        for (i, &bit) in bitvec.iter().enumerate() {
            if bit != 0 {
                bits[i >> 3] |= 1 << (7 - (i & 7));
            }
        }

        let mut dec = VulkanVp9Decoder::new();
        let mut bs = BitstreamReader::new(&bits);
        let result = dec.parse_uncompressed_header(&mut bs);
        assert!(result, "parse_uncompressed_header should succeed");

        assert_eq!(dec.pic_data.std_picture_info.profile, Vp9Profile::Profile0);
        assert_eq!(dec.pic_data.std_picture_info.frame_type, Vp9FrameType::Key);
        assert!(dec.pic_data.std_picture_info.flags.show_frame);
        assert!(!dec.pic_data.std_picture_info.flags.error_resilient_mode);
        assert_eq!(dec.pic_data.frame_width, 16);
        assert_eq!(dec.pic_data.frame_height, 16);
        assert_eq!(dec.pic_data.render_width, 16);
        assert_eq!(dec.pic_data.render_height, 16);
        assert_eq!(dec.pic_data.std_picture_info.refresh_frame_flags, 0xFF);
        assert!(dec.pic_data.frame_is_intra);
        assert_eq!(dec.pic_data.std_color_config.bit_depth, 8);
        assert_eq!(dec.pic_data.std_color_config.color_space, Vp9ColorSpace::Bt601);
        assert_eq!(dec.pic_data.chroma_format, 1);
    }

    // -----------------------------------------------------------------------
    // Segmentation parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_segmentation_disabled() {
        let data = [0x00]; // segmentation_enabled = 0
        let mut dec = VulkanVp9Decoder::new();
        let mut bs = BitstreamReader::new(&data);
        dec.parse_segmentation_params(&mut bs);
        assert!(!dec.pic_data.std_picture_info.flags.segmentation_enabled);
        assert_eq!(bs.consumed_bits(), 1);
    }

    // -----------------------------------------------------------------------
    // Init parser
    // -----------------------------------------------------------------------

    #[test]
    fn init_parser_resets_state() {
        let mut dec = VulkanVp9Decoder::new();
        dec.curr_pic = Some(5);
        dec.bitstream_complete = false;
        dec.picture_started = true;
        dec.buffers[2].buffer_idx = Some(10);

        dec.init_parser();

        assert_eq!(dec.curr_pic, None);
        assert!(dec.bitstream_complete);
        assert!(!dec.picture_started);
        assert_eq!(dec.buffers[2].buffer_idx, None);
    }
}
