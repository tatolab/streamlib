// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderConfigH265.h + VkEncoderConfigH265.cpp
//!
//! H.265-specific encoder configuration: profile/level/tier selection, level
//! limits table, CTB alignment, DPB sizing, VUI parameters, SPS/PPS/VPS
//! initialization, rate control, and argument parsing.

use vulkanalia::vk;

use crate::vk_video_encoder::vk_encoder_config::EncoderConfig;
use crate::vk_video_encoder::vk_video_encoder_def::{align_size, gcd};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const FRAME_RATE_NUM_DEFAULT: u32 = 30_000;
pub const FRAME_RATE_DEN_DEFAULT: u32 = 1_001;
pub const MAX_LEVELS: usize = 14;
pub const MAX_NUM_REF_PICS: u8 = 15;
pub const LOG2_MB_SIZE: u32 = 4;

// ---------------------------------------------------------------------------
// CU / TU size enums
// ---------------------------------------------------------------------------

/// H.265 coding unit size.
///
/// Equivalent to the C++ `EncoderConfigH265::CuSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CuSize {
    Size8x8 = 0,
    Size16x16 = 1,
    Size32x32 = 2,
    Size64x64 = 4,
}

/// H.265 transform unit size.
///
/// Equivalent to the C++ `EncoderConfigH265::TransformUnitSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TransformUnitSize {
    Size4x4 = 0,
    Size8x8 = 1,
    Size16x16 = 2,
    Size32x32 = 3,
}

// ---------------------------------------------------------------------------
// Level Limits (Table A-1 from H.265 spec)
// ---------------------------------------------------------------------------

/// H.265 level limits entry.
///
/// Equivalent to the C++ `EncoderConfigH265::LevelLimits`.
#[derive(Debug, Clone, Copy)]
pub struct LevelLimits {
    pub level_idc: i32,
    pub max_luma_ps: u32,
    pub max_cpb_size_main_tier: i32,
    pub max_cpb_size_high_tier: i32,
    pub max_slice_segment_per_picture: i32,
    pub max_tile_rows: i32,
    pub max_tile_cols: i32,
    pub max_luma_sr: u32,
    pub max_bit_rate_main_tier: i32,
    pub max_bit_rate_high_tier: i32,
    pub min_cr: i32,
    /// Index into StdVideoH265LevelIdc (stored as u32).
    pub std_level: u32,
}

/// H.265 level limits table (Table A-1).
pub static LEVEL_LIMITS_H265: &[LevelLimits] = &[
    LevelLimits { level_idc: 30, max_luma_ps: 36864, max_cpb_size_main_tier: 350, max_cpb_size_high_tier: -1, max_slice_segment_per_picture: 16, max_tile_rows: 1, max_tile_cols: 1, max_luma_sr: 552960, max_bit_rate_main_tier: 128, max_bit_rate_high_tier: -1, min_cr: 2, std_level: 0 },
    LevelLimits { level_idc: 60, max_luma_ps: 122880, max_cpb_size_main_tier: 1500, max_cpb_size_high_tier: -1, max_slice_segment_per_picture: 16, max_tile_rows: 1, max_tile_cols: 1, max_luma_sr: 3686400, max_bit_rate_main_tier: 1500, max_bit_rate_high_tier: -1, min_cr: 2, std_level: 1 },
    LevelLimits { level_idc: 63, max_luma_ps: 245760, max_cpb_size_main_tier: 3000, max_cpb_size_high_tier: -1, max_slice_segment_per_picture: 20, max_tile_rows: 1, max_tile_cols: 1, max_luma_sr: 7372800, max_bit_rate_main_tier: 3000, max_bit_rate_high_tier: -1, min_cr: 2, std_level: 2 },
    LevelLimits { level_idc: 90, max_luma_ps: 552960, max_cpb_size_main_tier: 6000, max_cpb_size_high_tier: -1, max_slice_segment_per_picture: 30, max_tile_rows: 2, max_tile_cols: 2, max_luma_sr: 16588800, max_bit_rate_main_tier: 6000, max_bit_rate_high_tier: -1, min_cr: 2, std_level: 3 },
    LevelLimits { level_idc: 93, max_luma_ps: 983040, max_cpb_size_main_tier: 10000, max_cpb_size_high_tier: -1, max_slice_segment_per_picture: 40, max_tile_rows: 3, max_tile_cols: 3, max_luma_sr: 33177600, max_bit_rate_main_tier: 10000, max_bit_rate_high_tier: -1, min_cr: 2, std_level: 4 },
    LevelLimits { level_idc: 120, max_luma_ps: 2228224, max_cpb_size_main_tier: 12000, max_cpb_size_high_tier: 30000, max_slice_segment_per_picture: 75, max_tile_rows: 5, max_tile_cols: 5, max_luma_sr: 66846720, max_bit_rate_main_tier: 12000, max_bit_rate_high_tier: 30000, min_cr: 4, std_level: 5 },
    LevelLimits { level_idc: 123, max_luma_ps: 2228224, max_cpb_size_main_tier: 20000, max_cpb_size_high_tier: 50000, max_slice_segment_per_picture: 75, max_tile_rows: 5, max_tile_cols: 5, max_luma_sr: 133693440, max_bit_rate_main_tier: 20000, max_bit_rate_high_tier: 50000, min_cr: 4, std_level: 6 },
    LevelLimits { level_idc: 150, max_luma_ps: 8912896, max_cpb_size_main_tier: 25000, max_cpb_size_high_tier: 100000, max_slice_segment_per_picture: 200, max_tile_rows: 11, max_tile_cols: 10, max_luma_sr: 267386880, max_bit_rate_main_tier: 25000, max_bit_rate_high_tier: 100000, min_cr: 6, std_level: 7 },
    LevelLimits { level_idc: 153, max_luma_ps: 8912896, max_cpb_size_main_tier: 40000, max_cpb_size_high_tier: 160000, max_slice_segment_per_picture: 200, max_tile_rows: 11, max_tile_cols: 10, max_luma_sr: 534773760, max_bit_rate_main_tier: 40000, max_bit_rate_high_tier: 160000, min_cr: 8, std_level: 8 },
    LevelLimits { level_idc: 156, max_luma_ps: 8912896, max_cpb_size_main_tier: 60000, max_cpb_size_high_tier: 240000, max_slice_segment_per_picture: 200, max_tile_rows: 11, max_tile_cols: 10, max_luma_sr: 1069547520, max_bit_rate_main_tier: 60000, max_bit_rate_high_tier: 240000, min_cr: 8, std_level: 9 },
    LevelLimits { level_idc: 180, max_luma_ps: 35651584, max_cpb_size_main_tier: 60000, max_cpb_size_high_tier: 240000, max_slice_segment_per_picture: 600, max_tile_rows: 22, max_tile_cols: 20, max_luma_sr: 1069547520, max_bit_rate_main_tier: 60000, max_bit_rate_high_tier: 240000, min_cr: 8, std_level: 10 },
    LevelLimits { level_idc: 183, max_luma_ps: 35651584, max_cpb_size_main_tier: 120000, max_cpb_size_high_tier: 480000, max_slice_segment_per_picture: 600, max_tile_rows: 22, max_tile_cols: 20, max_luma_sr: 2139095040, max_bit_rate_main_tier: 120000, max_bit_rate_high_tier: 480000, min_cr: 8, std_level: 11 },
    LevelLimits { level_idc: 186, max_luma_ps: 35651584, max_cpb_size_main_tier: 240000, max_cpb_size_high_tier: 800000, max_slice_segment_per_picture: 600, max_tile_rows: 22, max_tile_cols: 20, max_luma_sr: 4278190080, max_bit_rate_main_tier: 240000, max_bit_rate_high_tier: 800000, min_cr: 6, std_level: 12 },
    LevelLimits { level_idc: 187, max_luma_ps: 67108864, max_cpb_size_main_tier: 240000, max_cpb_size_high_tier: 800000, max_slice_segment_per_picture: 600, max_tile_rows: 22, max_tile_cols: 20, max_luma_sr: 4278190080, max_bit_rate_main_tier: 240000, max_bit_rate_high_tier: 800000, min_cr: 6, std_level: 12 },
];

/// H.265 profile IDC constants.
pub mod h265_profile {
    pub const INVALID: u32 = u32::MAX;
    pub const MAIN: u32 = 1;
    pub const MAIN_10: u32 = 2;
    pub const MAIN_STILL_PICTURE: u32 = 3;
    pub const FORMAT_RANGE_EXTENSIONS: u32 = 4;
    pub const SCC_EXTENSIONS: u32 = 9;
}

// ---------------------------------------------------------------------------
// EncoderConfigH265
// ---------------------------------------------------------------------------

/// H.265-specific encoder configuration.
///
/// Equivalent to the C++ `EncoderConfigH265` struct.
#[derive(Debug, Clone)]
pub struct EncoderConfigH265 {
    /// Base encoder configuration.
    pub base: EncoderConfig,

    pub profile: u32,
    pub level_idc: u32,
    pub general_tier_flag: bool,
    pub num_ref_l0: u8,
    pub num_ref_l1: u8,
    pub vps_id: u8,
    pub sps_id: u8,
    pub pps_id: u8,
    pub cu_min_size: CuSize,
    pub cu_size: CuSize,
    pub min_transform_unit_size: TransformUnitSize,
    pub max_transform_unit_size: TransformUnitSize,
    pub slice_count: u32,
    pub hrd_bitrate: u32,
}

impl Default for EncoderConfigH265 {
    fn default() -> Self {
        let mut base = EncoderConfig::default();
        base.frame_rate_numerator = FRAME_RATE_NUM_DEFAULT;
        base.frame_rate_denominator = FRAME_RATE_DEN_DEFAULT;

        Self {
            base,
            profile: h265_profile::INVALID,
            level_idc: u32::MAX,
            general_tier_flag: false,
            num_ref_l0: 1,
            num_ref_l1: 1,
            vps_id: 0,
            sps_id: 0,
            pps_id: 0,
            cu_min_size: CuSize::Size16x16,
            cu_size: CuSize::Size32x32,
            min_transform_unit_size: TransformUnitSize::Size4x4,
            max_transform_unit_size: TransformUnitSize::Size32x32,
            slice_count: 1,
            hrd_bitrate: 0,
        }
    }
}

impl EncoderConfigH265 {
    /// Initialize parameters (base + H.265 specific).
    ///
    /// Equivalent to the C++ `EncoderConfigH265::InitializeParameters`.
    pub fn initialize_parameters(&mut self) -> Result<(), vk::Result> {
        self.base.initialize_parameters()?;
        self.hrd_bitrate = self.base.max_bitrate;
        self.init_profile_level();
        Ok(())
    }

    /// Get the CPB/VCL factor (Table A.8).
    ///
    /// Equivalent to the C++ `EncoderConfigH265::GetCpbVclFactor`.
    pub fn get_cpb_vcl_factor(&self) -> u32 {
        let chroma_format_idc = self.base.encode_chroma_subsampling;
        let bit_depth = self
            .base
            .encode_bit_depth_luma
            .max(self.base.encode_bit_depth_chroma) as u32;
        let is_444 = chroma_format_idc == vk::VideoChromaSubsamplingFlagsKHR::_444;
        let base_factor: u32 = if is_444 {
            if bit_depth >= 10 { 2500 } else { 2000 }
        } else {
            1000
        };
        let depth_factor: u32 = if bit_depth >= 10 {
            ((bit_depth - 10) >> 1) * 500
        } else {
            0
        };
        base_factor + depth_factor
    }

    /// Compute the CTB-aligned picture size in samples.
    ///
    /// Returns `(pic_width_aligned, pic_height_aligned, total_size_in_samples)`.
    ///
    /// Equivalent to the C++ `GetCtbAlignedPicSizeInSamples`.
    pub fn get_ctb_aligned_pic_size_in_samples(
        &self,
        min_ctbs_y: bool,
    ) -> (u32, u32, u32) {
        let (width, height) = if min_ctbs_y {
            let min_cb_log2_size_y = self.cu_min_size as u32 + 3;
            let min_cb_size_y = 1u32 << min_cb_log2_size_y;
            (
                align_size(self.base.encode_width, min_cb_size_y),
                align_size(self.base.encode_height, min_cb_size_y),
            )
        } else {
            let ctb_log2_size_y = self.cu_size as u32 + 3;
            let ctb_size_y = 1u32 << ctb_log2_size_y;
            (
                align_size(self.base.encode_width, ctb_size_y),
                align_size(self.base.encode_height, ctb_size_y),
            )
        };
        (width, height, width * height)
    }

    /// Get the maximum DPB size for a given picture size and level.
    ///
    /// Equivalent to the C++ `GetMaxDpbSize`.
    pub fn get_max_dpb_size(&self, picture_size_in_samples_y: u32, level_index: usize) -> u32 {
        let max_dpb_pic_buf: u32 = 9;
        let max_luma_ps = LEVEL_LIMITS_H265[level_index].max_luma_ps;

        let max_dpb_size = if picture_size_in_samples_y <= (max_luma_ps >> 2) {
            max_dpb_pic_buf * 4
        } else if picture_size_in_samples_y <= (max_luma_ps >> 1) {
            max_dpb_pic_buf * 2
        } else if picture_size_in_samples_y <= ((3 * max_luma_ps) >> 2) {
            (max_dpb_pic_buf * 4) / 3
        } else {
            max_dpb_pic_buf
        };

        max_dpb_size.min(16) // STD_VIDEO_H265_MAX_DPB_SIZE
    }

    /// Check if a given level index is suitable.
    ///
    /// Equivalent to the C++ `IsSuitableLevel`.
    pub fn is_suitable_level(&self, level_idx: usize, high_tier: bool) -> bool {
        if level_idx >= LEVEL_LIMITS_H265.len() {
            return false;
        }

        let (width_aligned, height_aligned, pic_size) =
            self.get_ctb_aligned_pic_size_in_samples(false);

        let max_cpb = if high_tier {
            LEVEL_LIMITS_H265[level_idx].max_cpb_size_high_tier
        } else {
            LEVEL_LIMITS_H265[level_idx].max_cpb_size_main_tier
        };
        let max_br = if high_tier {
            LEVEL_LIMITS_H265[level_idx].max_bit_rate_high_tier
        } else {
            LEVEL_LIMITS_H265[level_idx].max_bit_rate_main_tier
        };
        let cpb_factor = self.get_cpb_vcl_factor();

        if pic_size > LEVEL_LIMITS_H265[level_idx].max_luma_ps {
            return false;
        }

        let max_dim = ((LEVEL_LIMITS_H265[level_idx].max_luma_ps as f64) * 8.0).sqrt() as u32;
        if width_aligned > max_dim || height_aligned > max_dim {
            return false;
        }

        if self.base.vbv_buffer_size != 0
            && max_cpb > 0
            && self.base.vbv_buffer_size > (max_cpb as u32 * cpb_factor)
        {
            return false;
        }

        if self.base.max_bitrate != 0
            && max_br > 0
            && self.base.max_bitrate > (max_br as u32 * cpb_factor)
        {
            return false;
        }

        if self.base.average_bitrate != 0
            && max_br > 0
            && self.base.average_bitrate > (max_br as u32 * cpb_factor)
        {
            return false;
        }

        true
    }

    /// Determine level and tier.
    ///
    /// Equivalent to the C++ `DetermineLevelTier`.
    pub fn determine_level_tier(&mut self) {
        self.level_idc = u32::MAX; // INVALID
        self.general_tier_flag = false;

        for level_idx in 0..LEVEL_LIMITS_H265.len() {
            if self.is_suitable_level(level_idx, false) {
                self.level_idc = LEVEL_LIMITS_H265[level_idx].std_level;
                self.general_tier_flag = false;
                return;
            }

            if LEVEL_LIMITS_H265[level_idx].level_idc >= 120
                && self.is_suitable_level(level_idx, true)
            {
                self.level_idc = LEVEL_LIMITS_H265[level_idx].std_level;
                self.general_tier_flag = true;
                return;
            }
        }

        tracing::error!("No suitable H.265 level selected");
    }

    /// Initialize profile and level.
    ///
    /// Equivalent to the C++ `EncoderConfigH265::InitProfileLevel`.
    pub fn init_profile_level(&mut self) {
        if self.profile == h265_profile::INVALID {
            if self.base.encode_chroma_subsampling
                == vk::VideoChromaSubsamplingFlagsKHR::_420
            {
                if self.base.input.bpp == 8 {
                    self.profile = h265_profile::MAIN;
                } else if self.base.input.bpp <= 10 {
                    self.profile = h265_profile::MAIN_10;
                } else {
                    self.profile = h265_profile::FORMAT_RANGE_EXTENSIONS;
                }
            } else {
                self.profile = h265_profile::FORMAT_RANGE_EXTENSIONS;
            }
        }

        self.determine_level_tier();
    }

    /// Initialize the DPB count for H.265.
    ///
    /// Equivalent to the C++ `EncoderConfigH265::InitDpbCount`.
    pub fn init_dpb_count(&mut self) -> i8 {
        self.base.dpb_count = 5;
        self.verify_dpb_size()
    }

    /// Verify and clamp the DPB size against level limits.
    ///
    /// Equivalent to the C++ `VerifyDpbSize`.
    pub fn verify_dpb_size(&mut self) -> i8 {
        let (_, _, pic_size) = self.get_ctb_aligned_pic_size_in_samples(false);

        let level_idx_found = LEVEL_LIMITS_H265
            .iter()
            .position(|l| l.std_level == self.level_idc);

        if let Some(idx) = level_idx_found {
            let max_dpb = self.get_max_dpb_size(pic_size, idx);
            if (self.base.dpb_count as u32) > max_dpb {
                return max_dpb as i8;
            }
        } else {
            tracing::error!("Invalid level idc for DPB verification");
            return -1;
        }

        self.base.dpb_count
    }

    /// Initialize rate control for H.265.
    ///
    /// Equivalent to the C++ `EncoderConfigH265::InitRateControl`.
    pub fn init_rate_control(&mut self) -> bool {
        if self.level_idc as usize >= LEVEL_LIMITS_H265.len() {
            tracing::error!("Invalid H.265 level index for rate control");
            return false;
        }

        let cpb_vcl_factor = self.get_cpb_vcl_factor();
        let level_idx = self.level_idc as usize;

        let mut level_bit_rate =
            self.base.average_bitrate.max(self.hrd_bitrate);
        level_bit_rate = level_bit_rate.max(
            (LEVEL_LIMITS_H265[level_idx].max_bit_rate_main_tier as u32 * 800).min(120_000_000),
        );

        if self.base.average_bitrate == 0 {
            self.base.average_bitrate = if self.hrd_bitrate != 0 {
                self.hrd_bitrate
            } else {
                level_bit_rate
            };
        }

        if self.hrd_bitrate == 0 {
            if self.base.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::VBR
                && self.base.average_bitrate < level_bit_rate
            {
                self.hrd_bitrate = (self.base.average_bitrate * 3).min(level_bit_rate);
                if self.base.vbv_buffer_size != 0 {
                    self.hrd_bitrate = self.hrd_bitrate.min(
                        (self.base.vbv_buffer_size * 2).max(self.base.average_bitrate),
                    );
                }
            } else {
                self.hrd_bitrate = self.base.average_bitrate;
            }
        }

        if self.base.average_bitrate > self.hrd_bitrate {
            self.base.average_bitrate = self.hrd_bitrate;
        }

        if self.base.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::CBR {
            self.hrd_bitrate = self.base.average_bitrate;
        }

        if self.base.vbv_buffer_size == 0 {
            self.base.vbv_buffer_size =
                (LEVEL_LIMITS_H265[level_idx].max_cpb_size_main_tier as u32 * cpb_vcl_factor)
                    .min(100_000_000);
            if self.base.rate_control_mode
                != vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
            {
                if (self.base.vbv_buffer_size >> 3) > self.hrd_bitrate {
                    self.base.vbv_buffer_size = self.hrd_bitrate << 3;
                }
            }
        }

        if self.base.vbv_initial_delay == 0 {
            self.base.vbv_initial_delay = (self.base.vbv_buffer_size
                - self.base.vbv_buffer_size / 10)
                .max(self.base.vbv_buffer_size.min(self.hrd_bitrate));
        } else if self.base.vbv_initial_delay > self.base.vbv_buffer_size {
            self.base.vbv_initial_delay = self.base.vbv_buffer_size;
        }

        true
    }

    /// Compute the aspect ratio for H.265 VUI (Table E-1).
    ///
    /// Returns `(aspect_ratio_idc, sar_width, sar_height)`.
    pub fn compute_aspect_ratio(
        width: u32,
        height: u32,
        dar_width: u32,
        dar_height: u32,
    ) -> (i32, u16, u16) {
        static SAR_TABLE: &[(u32, u32)] = &[
            (1, 1),
            (12, 11),
            (10, 11),
            (16, 11),
            (40, 33),
            (24, 11),
            (20, 11),
            (32, 11),
            (80, 33),
            (18, 11),
            (15, 11),
            (64, 33),
            (160, 99),
            (4, 3),
            (3, 2),
            (2, 1),
        ];

        if dar_width == 0 || dar_height == 0 {
            return (-1, 0, 0); // not present
        }

        let w = height * dar_width;
        let h = width * dar_height;
        let d = gcd(w, h);
        let w = w / d;
        let h = h / d;

        for (i, &(sw, sh)) in SAR_TABLE.iter().enumerate() {
            if sw == w && sh == h {
                return ((i + 1) as i32, 0, 0);
            }
        }

        // Extended SAR (idc = 255)
        (255, w as u16, h as u16)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = EncoderConfigH265::default();
        assert_eq!(cfg.profile, h265_profile::INVALID);
        assert_eq!(cfg.cu_min_size, CuSize::Size16x16);
        assert_eq!(cfg.cu_size, CuSize::Size32x32);
        assert_eq!(cfg.num_ref_l0, 1);
        assert_eq!(cfg.num_ref_l1, 1);
    }

    #[test]
    fn test_level_limits_table_size() {
        assert_eq!(LEVEL_LIMITS_H265.len(), 14);
    }

    #[test]
    fn test_cpb_vcl_factor_8bit_420() {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.encode_bit_depth_luma = 8;
        cfg.base.encode_bit_depth_chroma = 8;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        assert_eq!(cfg.get_cpb_vcl_factor(), 1000);
    }

    #[test]
    fn test_cpb_vcl_factor_10bit_420() {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.encode_bit_depth_luma = 10;
        cfg.base.encode_bit_depth_chroma = 10;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        assert_eq!(cfg.get_cpb_vcl_factor(), 1000);
    }

    #[test]
    fn test_init_profile_level_main_8bit() {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        cfg.base.initialize_parameters().unwrap();
        cfg.init_profile_level();
        assert_eq!(cfg.profile, h265_profile::MAIN);
        assert_ne!(cfg.level_idc, u32::MAX); // should find a valid level
    }

    #[test]
    fn test_init_profile_level_main10() {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 10;
        cfg.base.encode_chroma_subsampling = vk::VideoChromaSubsamplingFlagsKHR::_420;
        cfg.base.initialize_parameters().unwrap();
        cfg.init_profile_level();
        assert_eq!(cfg.profile, h265_profile::MAIN_10);
    }

    #[test]
    fn test_aspect_ratio_square() {
        let (idc, _, _) = EncoderConfigH265::compute_aspect_ratio(1920, 1080, 16, 9);
        assert_eq!(idc, 1); // 1:1 SAR
    }

    #[test]
    fn test_aspect_ratio_not_present() {
        let (idc, _, _) = EncoderConfigH265::compute_aspect_ratio(1920, 1080, 0, 0);
        assert_eq!(idc, -1);
    }

    #[test]
    fn test_ctb_aligned_pic_size() {
        let mut cfg = EncoderConfigH265::default();
        cfg.base.encode_width = 1920;
        cfg.base.encode_height = 1080;
        let (w, h, size) = cfg.get_ctb_aligned_pic_size_in_samples(false);
        // CuSize::Size32x32 => ctb_log2_size_y = 5, ctb_size_y = 32
        assert_eq!(w, 1920); // 1920 is aligned to 32
        assert_eq!(h, 1088); // 1080 aligned up to 32 = 1088
        assert_eq!(size, 1920 * 1088);
    }
}
