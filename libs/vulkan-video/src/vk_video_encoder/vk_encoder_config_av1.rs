// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderConfigAV1.h + VkEncoderConfigAV1.cpp
//!
//! AV1-specific encoder configuration: profile/level/tier selection, level
//! limits table, tile/quantization/loop-filter/CDEF/LR configs, DPB sizing,
//! rate control, and argument parsing.

use vulkanalia::vk;

use crate::vk_video_encoder::vk_encoder_config::EncoderConfig;
use crate::vk_video_encoder::vk_video_encoder_def::div_up;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const FRAME_ID_BITS: u32 = 15;
pub const DELTA_FRAME_ID_BITS: u32 = 14;
pub const ORDER_HINT_BITS: u32 = 7;

pub const BASE_QIDX_INTRA: u32 = 114;
pub const BASE_QIDX_INTER_P: u32 = 131;
pub const BASE_QIDX_INTER_B: u32 = 147;

pub const SUPERBLOCK_SIZE: u32 = 64;

pub const FRAME_RATE_NUM_DEFAULT: u32 = 30_000;
pub const FRAME_RATE_DEN_DEFAULT: u32 = 1_001;
pub const IDR_PERIOD_DEFAULT: u32 = 60;
pub const GOP_LENGTH_DEFAULT: u32 = 60;

/// Maximum tile columns (from AV1 spec).
pub const MAX_TILE_COLS: usize = 64;
/// Maximum tile rows (from AV1 spec).
pub const MAX_TILE_ROWS: usize = 64;

// ---------------------------------------------------------------------------
// Level Limits
// ---------------------------------------------------------------------------

/// AV1 level limits entry.
///
/// Equivalent to the C++ `EncoderConfigAV1::LevelLimits`.
#[derive(Debug, Clone, Copy)]
pub struct LevelLimits {
    /// AV1 level enum value (stored as u32; u32::MAX = INVALID).
    pub level: u32,
    pub max_pic_size: u32,
    pub max_h_size: u32,
    pub max_v_size: u32,
    pub max_display_rate: u64,
    pub max_decode_rate: u64,
    pub max_header_rate: u32,
    pub main_bps: u32,
    pub high_bps: u32,
    pub main_cr: f64,
    pub high_cr: f64,
    pub max_tiles: u32,
    pub max_tile_cols: u32,
}

/// AV1 level limits table.
pub static LEVEL_LIMITS_AV1: &[LevelLimits] = &[
    LevelLimits { level: 0, max_pic_size: 147456, max_h_size: 2048, max_v_size: 1152, max_display_rate: 4423680, max_decode_rate: 5529600, max_header_rate: 150, main_bps: 1500000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 8, max_tile_cols: 4 },      // 2.0
    LevelLimits { level: 1, max_pic_size: 278784, max_h_size: 2816, max_v_size: 1584, max_display_rate: 8363520, max_decode_rate: 10454400, max_header_rate: 150, main_bps: 3000000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 8, max_tile_cols: 4 },      // 2.1
    LevelLimits { level: u32::MAX, max_pic_size: 278784, max_h_size: 2816, max_v_size: 1584, max_display_rate: 8363520, max_decode_rate: 10454400, max_header_rate: 150, main_bps: 3000000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 8, max_tile_cols: 4 }, // 2.2 undefined
    LevelLimits { level: u32::MAX, max_pic_size: 278784, max_h_size: 2816, max_v_size: 1584, max_display_rate: 8363520, max_decode_rate: 10454400, max_header_rate: 150, main_bps: 3000000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 8, max_tile_cols: 4 }, // 2.3 undefined
    LevelLimits { level: 4, max_pic_size: 665856, max_h_size: 4352, max_v_size: 2448, max_display_rate: 19975680, max_decode_rate: 24969600, max_header_rate: 150, main_bps: 6000000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 16, max_tile_cols: 6 },    // 3.0
    LevelLimits { level: 5, max_pic_size: 1065024, max_h_size: 5504, max_v_size: 3096, max_display_rate: 31950720, max_decode_rate: 39938400, max_header_rate: 150, main_bps: 10000000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 16, max_tile_cols: 6 },  // 3.1
    LevelLimits { level: u32::MAX, max_pic_size: 1065024, max_h_size: 5504, max_v_size: 3096, max_display_rate: 31950720, max_decode_rate: 39938400, max_header_rate: 150, main_bps: 10000000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 16, max_tile_cols: 6 },
    LevelLimits { level: u32::MAX, max_pic_size: 1065024, max_h_size: 5504, max_v_size: 3096, max_display_rate: 31950720, max_decode_rate: 39938400, max_header_rate: 150, main_bps: 10000000, high_bps: 0, main_cr: 2.0, high_cr: -1.0, max_tiles: 16, max_tile_cols: 6 },
    LevelLimits { level: 8, max_pic_size: 2359296, max_h_size: 6144, max_v_size: 3456, max_display_rate: 70778880, max_decode_rate: 77856768, max_header_rate: 300, main_bps: 12000000, high_bps: 30000000, main_cr: 4.0, high_cr: 4.0, max_tiles: 32, max_tile_cols: 8 },  // 4.0
    LevelLimits { level: 9, max_pic_size: 2359296, max_h_size: 6144, max_v_size: 3456, max_display_rate: 141557760, max_decode_rate: 155713536, max_header_rate: 300, main_bps: 20000000, high_bps: 50000000, main_cr: 4.0, high_cr: 4.0, max_tiles: 32, max_tile_cols: 8 },  // 4.1
    LevelLimits { level: u32::MAX, max_pic_size: 2359296, max_h_size: 6144, max_v_size: 3456, max_display_rate: 141557760, max_decode_rate: 155713536, max_header_rate: 300, main_bps: 20000000, high_bps: 50000000, main_cr: 4.0, high_cr: 4.0, max_tiles: 32, max_tile_cols: 8 },
    LevelLimits { level: u32::MAX, max_pic_size: 2359296, max_h_size: 6144, max_v_size: 3456, max_display_rate: 141557760, max_decode_rate: 155713536, max_header_rate: 300, main_bps: 20000000, high_bps: 50000000, main_cr: 4.0, high_cr: 4.0, max_tiles: 32, max_tile_cols: 8 },
    LevelLimits { level: 12, max_pic_size: 8912896, max_h_size: 8192, max_v_size: 4352, max_display_rate: 267386880, max_decode_rate: 273715200, max_header_rate: 300, main_bps: 30000000, high_bps: 100000000, main_cr: 6.0, high_cr: 4.0, max_tiles: 64, max_tile_cols: 8 },  // 5.0
    LevelLimits { level: 13, max_pic_size: 8912896, max_h_size: 8192, max_v_size: 4352, max_display_rate: 534773760, max_decode_rate: 547430400, max_header_rate: 300, main_bps: 40000000, high_bps: 160000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 64, max_tile_cols: 8 },  // 5.1
    LevelLimits { level: 14, max_pic_size: 8912896, max_h_size: 8192, max_v_size: 4352, max_display_rate: 1069547520, max_decode_rate: 1094860800, max_header_rate: 300, main_bps: 60000000, high_bps: 240000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 64, max_tile_cols: 8 },  // 5.2
    LevelLimits { level: 15, max_pic_size: 8912896, max_h_size: 8192, max_v_size: 4352, max_display_rate: 1069547520, max_decode_rate: 1176502272, max_header_rate: 300, main_bps: 60000000, high_bps: 240000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 64, max_tile_cols: 8 },  // 5.3
    LevelLimits { level: 16, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 1069547520, max_decode_rate: 1176502272, max_header_rate: 300, main_bps: 60000000, high_bps: 240000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },  // 6.0
    LevelLimits { level: 17, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 2139095040, max_decode_rate: 2189721600, max_header_rate: 300, main_bps: 100000000, high_bps: 480000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },  // 6.1
    LevelLimits { level: 18, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 4278190080, max_decode_rate: 4379443200, max_header_rate: 300, main_bps: 160000000, high_bps: 800000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },  // 6.2
    LevelLimits { level: 19, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 4278190080, max_decode_rate: 4706009088, max_header_rate: 300, main_bps: 160000000, high_bps: 800000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },  // 6.3
    // 7.x undefined (copies of 6.3)
    LevelLimits { level: u32::MAX, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 4278190080, max_decode_rate: 4706009088, max_header_rate: 300, main_bps: 160000000, high_bps: 800000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },
    LevelLimits { level: u32::MAX, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 4278190080, max_decode_rate: 4706009088, max_header_rate: 300, main_bps: 160000000, high_bps: 800000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },
    LevelLimits { level: u32::MAX, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 4278190080, max_decode_rate: 4706009088, max_header_rate: 300, main_bps: 160000000, high_bps: 800000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },
    LevelLimits { level: u32::MAX, max_pic_size: 35651584, max_h_size: 16384, max_v_size: 8704, max_display_rate: 4278190080, max_decode_rate: 4706009088, max_header_rate: 300, main_bps: 160000000, high_bps: 800000000, main_cr: 8.0, high_cr: 4.0, max_tiles: 128, max_tile_cols: 16 },
];

/// AV1 profile constants.
pub mod av1_profile {
    pub const INVALID: u32 = u32::MAX;
    pub const MAIN: u32 = 0;
    pub const HIGH: u32 = 1;
    pub const PROFESSIONAL: u32 = 2;
}

// ---------------------------------------------------------------------------
// EncoderConfigAV1
// ---------------------------------------------------------------------------

/// AV1-specific encoder configuration.
///
/// Equivalent to the C++ `EncoderConfigAV1` struct.
#[derive(Debug, Clone)]
pub struct EncoderConfigAV1 {
    /// Base encoder configuration.
    pub base: EncoderConfig,

    pub profile: u32,
    pub level: u32,
    pub tier: u8,
    pub hrd_bitrate: u32,
    pub pic_width_in_sbs: u32,
    pub pic_height_in_sbs: u32,

    // Tile configuration
    pub enable_tiles: bool,
    pub custom_tile_config: bool,
    pub tile_cols: u32,
    pub tile_rows: u32,
    pub tile_width_in_sbs_minus1: [u16; MAX_TILE_COLS],
    pub tile_height_in_sbs_minus1: [u16; MAX_TILE_ROWS],

    // Quantization configuration
    pub enable_quant: bool,
    pub custom_quant_config: bool,
    pub base_q_idx: u32,

    // Loop filter configuration
    pub enable_lf: bool,
    pub custom_lf_config: bool,

    // CDEF configuration
    pub enable_cdef: bool,
    pub custom_cdef_config: bool,

    // Loop restoration configuration
    pub enable_lr: bool,
    pub custom_lr_config: bool,
}

impl Default for EncoderConfigAV1 {
    fn default() -> Self {
        let mut base = EncoderConfig::default();
        base.frame_rate_numerator = FRAME_RATE_NUM_DEFAULT;
        base.frame_rate_denominator = FRAME_RATE_DEN_DEFAULT;

        Self {
            base,
            profile: av1_profile::INVALID,
            level: u32::MAX,
            tier: 0,
            hrd_bitrate: 0,
            pic_width_in_sbs: 0,
            pic_height_in_sbs: 0,
            enable_tiles: false,
            custom_tile_config: false,
            tile_cols: 0,
            tile_rows: 0,
            tile_width_in_sbs_minus1: [0; MAX_TILE_COLS],
            tile_height_in_sbs_minus1: [0; MAX_TILE_ROWS],
            enable_quant: false,
            custom_quant_config: false,
            base_q_idx: 0,
            enable_lf: false,
            custom_lf_config: false,
            enable_cdef: false,
            custom_cdef_config: false,
            enable_lr: false,
            custom_lr_config: false,
        }
    }
}

impl EncoderConfigAV1 {
    /// Initialize parameters (base + AV1 specific).
    ///
    /// Equivalent to the C++ `EncoderConfigAV1::InitializeParameters`.
    pub fn initialize_parameters(&mut self) -> Result<(), vk::Result> {
        self.base.initialize_parameters()?;

        self.hrd_bitrate = self.base.max_bitrate;
        self.pic_width_in_sbs = div_up(self.base.encode_width, SUPERBLOCK_SIZE);
        self.pic_height_in_sbs = div_up(self.base.encode_height, SUPERBLOCK_SIZE);

        if self.pic_width_in_sbs > 0 && self.pic_height_in_sbs > 0 {
            self.init_profile_level();
            return Ok(());
        }

        Err(vk::Result::ERROR_UNKNOWN)
    }

    /// Initialize AV1 profile and level.
    ///
    /// Equivalent to the C++ `EncoderConfigAV1::InitProfileLevel`.
    pub fn init_profile_level(&mut self) {
        if self.profile == av1_profile::INVALID {
            self.profile = av1_profile::MAIN;
        }
        self.determine_level_tier();
    }

    /// Get the maximum bitrate for a given level and tier.
    ///
    /// Equivalent to the C++ `GetLevelMaxBitrate`.
    pub fn get_level_max_bitrate(&self, l_level: usize, l_tier: u32) -> u32 {
        let tier = if (l_level as u32) < 8 { 0 } else { l_tier }; // < level 4.0
        let max_bitrate = if tier != 0 {
            LEVEL_LIMITS_AV1[l_level].high_bps
        } else {
            LEVEL_LIMITS_AV1[l_level].main_bps
        };
        let profile_factor = match self.profile {
            av1_profile::MAIN => 1,
            av1_profile::HIGH => 1,
            _ => 3,
        };
        max_bitrate * profile_factor
    }

    /// Get the bitrate for a level and tier (with profile factor).
    ///
    /// Equivalent to the C++ `GetLevelBitrate`.
    pub fn get_level_bitrate(&self, l_level: usize, l_tier: u32) -> u32 {
        let tier = if (l_level as u32) < 8 { 0 } else { l_tier };
        let profile_factor = match self.profile {
            av1_profile::MAIN => 1,
            av1_profile::HIGH => 2,
            _ => 3,
        };
        let max_bps = if tier == 0 {
            LEVEL_LIMITS_AV1[l_level].main_bps
        } else {
            LEVEL_LIMITS_AV1[l_level].high_bps
        };
        max_bps * profile_factor
    }

    /// Get uncompressed frame size.
    ///
    /// Equivalent to the C++ `GetUncompressedSize`.
    pub fn get_uncompressed_size(&self) -> u32 {
        let profile_factor = match self.profile {
            av1_profile::MAIN => 15,
            av1_profile::HIGH => 30,
            _ => 36,
        };
        (self.base.encode_width * self.base.encode_height * profile_factor) >> 3
    }

    /// Validate a level for the current configuration.
    ///
    /// Equivalent to the C++ `ValidateLevel`.
    pub fn validate_level(&self, l_level: usize, l_tier: u32) -> bool {
        if l_level >= LEVEL_LIMITS_AV1.len() {
            return false;
        }
        let limits = &LEVEL_LIMITS_AV1[l_level];

        let pic_size = self.base.encode_width * self.base.encode_height;
        if pic_size > limits.max_pic_size {
            return false;
        }
        if self.base.encode_width > limits.max_h_size {
            return false;
        }
        if self.base.encode_height > limits.max_v_size {
            return false;
        }

        let max_bitrate = self.get_level_max_bitrate(l_level, l_tier);
        if self.base.average_bitrate > 0 && self.base.average_bitrate > max_bitrate {
            return false;
        }
        if self.hrd_bitrate > 0 && self.hrd_bitrate > max_bitrate {
            return false;
        }

        true
    }

    /// Determine level and tier.
    ///
    /// Equivalent to the C++ `DetermineLevelTier`.
    pub fn determine_level_tier(&mut self) -> bool {
        self.level = u32::MAX;
        self.tier = 0;

        for i in 0..LEVEL_LIMITS_AV1.len() {
            if LEVEL_LIMITS_AV1[i].level == u32::MAX {
                continue; // skip undefined levels
            }

            // Try main tier first
            if self.validate_level(i, 0) {
                self.level = LEVEL_LIMITS_AV1[i].level;
                self.tier = 0;
                return true;
            }

            // Try high tier for level >= 4.0
            if i >= 8 && self.validate_level(i, 1) {
                self.level = LEVEL_LIMITS_AV1[i].level;
                self.tier = 1;
                return true;
            }
        }

        tracing::error!("No suitable AV1 level found");
        false
    }

    /// Initialize DPB count for AV1.
    pub fn init_dpb_count(&mut self) -> i8 {
        // AV1 uses up to 8 reference frames (7 forward + 1 alt)
        self.base.dpb_count = 8;
        8
    }

    /// Initialize rate control for AV1 (placeholder).
    ///
    /// Follows the same pattern as H264/H265.
    pub fn init_rate_control(&mut self) -> bool {
        let level_idx = self.level as usize;
        if level_idx >= LEVEL_LIMITS_AV1.len() {
            return false;
        }

        let level_bit_rate = self.get_level_bitrate(level_idx, self.tier as u32);

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

        true
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
        let cfg = EncoderConfigAV1::default();
        assert_eq!(cfg.profile, av1_profile::INVALID);
        assert_eq!(cfg.level, u32::MAX);
        assert_eq!(cfg.tier, 0);
        assert!(!cfg.enable_tiles);
        assert!(!cfg.enable_cdef);
    }

    #[test]
    fn test_level_limits_table_size() {
        assert_eq!(LEVEL_LIMITS_AV1.len(), 24);
    }

    #[test]
    fn test_init_profile_level_1080p() {
        let mut cfg = EncoderConfigAV1::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_sbs = div_up(cfg.base.encode_width, SUPERBLOCK_SIZE);
        cfg.pic_height_in_sbs = div_up(cfg.base.encode_height, SUPERBLOCK_SIZE);
        cfg.init_profile_level();
        assert_eq!(cfg.profile, av1_profile::MAIN);
        assert_ne!(cfg.level, u32::MAX);
    }

    #[test]
    fn test_get_uncompressed_size() {
        let mut cfg = EncoderConfigAV1::default();
        cfg.base.encode_width = 1920;
        cfg.base.encode_height = 1080;
        cfg.profile = av1_profile::MAIN;
        let size = cfg.get_uncompressed_size();
        // 1920 * 1080 * 15 / 8 = 3888000
        assert_eq!(size, 3888000);
    }

    #[test]
    fn test_validate_level_small_res() {
        let mut cfg = EncoderConfigAV1::default();
        cfg.base.encode_width = 320;
        cfg.base.encode_height = 240;
        cfg.profile = av1_profile::MAIN;
        assert!(cfg.validate_level(0, 0)); // Level 2.0 should work for 320x240
    }

    #[test]
    fn test_superblock_constants() {
        assert_eq!(SUPERBLOCK_SIZE, 64);
        assert_eq!(FRAME_ID_BITS, 15);
        assert_eq!(DELTA_FRAME_ID_BITS, 14);
        assert_eq!(ORDER_HINT_BITS, 7);
    }
}
