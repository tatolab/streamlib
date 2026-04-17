// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderConfigH264.h + VkEncoderConfigH264.cpp
//!
//! H.264-specific encoder configuration: profile/level selection, level limits
//! table (Table A-1), SPS/PPS initialization, VUI parameters, rate control,
//! aspect ratio, and argument parsing.

use vulkanalia::vk;

use crate::vk_video_encoder::vk_encoder_config::EncoderConfig;
use crate::vk_video_encoder::vk_video_encoder_def::{div_up, gcd};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// H.264 entropy coding modes.
///
/// Equivalent to the C++ `EncoderConfigH264::EntropyCodingMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EntropyCodingMode {
    Cabac = 0x1,
    Cavlc = 0x2,
}

/// H.264 adaptive transform modes.
///
/// Equivalent to the C++ `EncoderConfigH264::AdaptiveTransformMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AdaptiveTransformMode {
    AutoSelect = 0x0,
    Disable = 0x1,
    Enable = 0x2,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const FRAME_RATE_NUM_DEFAULT: u32 = 30_000;
pub const FRAME_RATE_DEN_DEFAULT: u32 = 1_001;
pub const IDR_PERIOD_DEFAULT: u32 = 30;
pub const GOP_LENGTH_DEFAULT: u32 = 30;

// ---------------------------------------------------------------------------
// Level Limits (Table A-1)
// ---------------------------------------------------------------------------

/// H.264 level limits entry.
///
/// Equivalent to the C++ `EncoderConfigH264::LevelLimits`.
#[derive(Debug, Clone, Copy)]
pub struct LevelLimits {
    pub level_idc: u32,
    pub max_mbps: u32,
    pub max_fs: u32,
    pub max_dpb: f64,
    pub max_br: u32,
    pub max_cpb: u32,
    pub max_vmv_r: u32,
    /// Index into the StdVideoH264LevelIdc enum (stored as u32).
    pub level: u32,
}

/// H.264 level limits table (Table A-1 from the H.264 specification).
pub static LEVEL_LIMITS_H264: &[LevelLimits] = &[
    LevelLimits { level_idc: 10, max_mbps: 1485, max_fs: 99, max_dpb: 148.5, max_br: 64, max_cpb: 175, max_vmv_r: 64, level: 0 },     // 1.0
    LevelLimits { level_idc: 11, max_mbps: 3000, max_fs: 396, max_dpb: 337.5, max_br: 192, max_cpb: 500, max_vmv_r: 128, level: 1 },   // 1.1
    LevelLimits { level_idc: 12, max_mbps: 6000, max_fs: 396, max_dpb: 891.0, max_br: 384, max_cpb: 1000, max_vmv_r: 128, level: 2 },  // 1.2
    LevelLimits { level_idc: 13, max_mbps: 11880, max_fs: 396, max_dpb: 891.0, max_br: 768, max_cpb: 2000, max_vmv_r: 128, level: 3 }, // 1.3
    LevelLimits { level_idc: 20, max_mbps: 11880, max_fs: 396, max_dpb: 891.0, max_br: 2000, max_cpb: 2000, max_vmv_r: 128, level: 4 },  // 2.0
    LevelLimits { level_idc: 21, max_mbps: 19800, max_fs: 792, max_dpb: 1782.0, max_br: 4000, max_cpb: 4000, max_vmv_r: 256, level: 5 }, // 2.1
    LevelLimits { level_idc: 22, max_mbps: 20250, max_fs: 1620, max_dpb: 3037.5, max_br: 4000, max_cpb: 4000, max_vmv_r: 256, level: 6 }, // 2.2
    LevelLimits { level_idc: 30, max_mbps: 40500, max_fs: 1620, max_dpb: 3037.5, max_br: 10000, max_cpb: 10000, max_vmv_r: 256, level: 7 }, // 3.0
    LevelLimits { level_idc: 31, max_mbps: 108000, max_fs: 3600, max_dpb: 6750.0, max_br: 14000, max_cpb: 14000, max_vmv_r: 512, level: 8 }, // 3.1
    LevelLimits { level_idc: 32, max_mbps: 216000, max_fs: 5120, max_dpb: 7680.0, max_br: 20000, max_cpb: 20000, max_vmv_r: 512, level: 9 }, // 3.2
    LevelLimits { level_idc: 40, max_mbps: 245760, max_fs: 8192, max_dpb: 12288.0, max_br: 20000, max_cpb: 25000, max_vmv_r: 512, level: 10 }, // 4.0
    LevelLimits { level_idc: 41, max_mbps: 245760, max_fs: 8192, max_dpb: 12288.0, max_br: 50000, max_cpb: 62500, max_vmv_r: 512, level: 11 }, // 4.1
    LevelLimits { level_idc: 42, max_mbps: 522240, max_fs: 8704, max_dpb: 13056.0, max_br: 50000, max_cpb: 62500, max_vmv_r: 512, level: 12 }, // 4.2
    LevelLimits { level_idc: 50, max_mbps: 589824, max_fs: 22080, max_dpb: 41400.0, max_br: 135000, max_cpb: 135000, max_vmv_r: 512, level: 13 }, // 5.0
    LevelLimits { level_idc: 51, max_mbps: 983040, max_fs: 36864, max_dpb: 69120.0, max_br: 240000, max_cpb: 240000, max_vmv_r: 512, level: 14 }, // 5.1
    LevelLimits { level_idc: 52, max_mbps: 2073600, max_fs: 36864, max_dpb: 69120.0, max_br: 240000, max_cpb: 240000, max_vmv_r: 512, level: 15 }, // 5.2
    LevelLimits { level_idc: 60, max_mbps: 4177920, max_fs: 139264, max_dpb: 261120.0, max_br: 240000, max_cpb: 240000, max_vmv_r: 8192, level: 16 }, // 6.0
    LevelLimits { level_idc: 61, max_mbps: 8355840, max_fs: 139264, max_dpb: 261120.0, max_br: 480000, max_cpb: 480000, max_vmv_r: 8192, level: 17 }, // 6.1
    LevelLimits { level_idc: 62, max_mbps: 16711680, max_fs: 139264, max_dpb: 261120.0, max_br: 800000, max_cpb: 800000, max_vmv_r: 8192, level: 18 }, // 6.2
];

// ---------------------------------------------------------------------------
// EncoderConfigH264
// ---------------------------------------------------------------------------

/// H.264-specific encoder configuration.
///
/// Equivalent to the C++ `EncoderConfigH264` struct. Extends `EncoderConfig`
/// via composition.
#[derive(Debug, Clone)]
pub struct EncoderConfigH264 {
    /// Base encoder configuration.
    pub base: EncoderConfig,

    /// H.264 profile indicator (STD_VIDEO_H264_PROFILE_IDC_*).
    pub profile_idc: u32,
    /// H.264 level indicator (STD_VIDEO_H264_LEVEL_IDC_*).
    pub level_idc: u32,
    /// HRD bitrate (hypothetical reference decoder).
    pub hrd_bitrate: u32,
    /// Picture width in macroblocks.
    pub pic_width_in_mbs: u32,
    /// Picture height in macroblock map units.
    pub pic_height_in_map_units: u32,
    /// Number of L0 reference frames.
    pub num_ref_l0: u8,
    /// Number of L1 reference frames.
    pub num_ref_l1: u8,
    /// Total number of reference frames.
    pub num_ref_frames: u8,
    pub entropy_coding_mode: EntropyCodingMode,
    pub adaptive_transform_mode: AdaptiveTransformMode,
    pub sps_id: u8,
    pub pps_id: u8,
    pub num_slices_per_picture: u32,
    pub slice_count: u32,
    pub qpprime_y_zero_transform_bypass_flag: bool,
    pub constrained_intra_pred_flag: bool,
}

/// STD_VIDEO_H264_PROFILE_IDC constants (subset used by the encoder).
pub mod h264_profile {
    pub const INVALID: u32 = u32::MAX;
    pub const BASELINE: u32 = 66;
    pub const MAIN: u32 = 77;
    pub const HIGH: u32 = 100;
    pub const HIGH_444_PREDICTIVE: u32 = 244;
}

impl Default for EncoderConfigH264 {
    fn default() -> Self {
        let mut base = EncoderConfig::default();
        base.frame_rate_numerator = FRAME_RATE_NUM_DEFAULT;
        base.frame_rate_denominator = FRAME_RATE_DEN_DEFAULT;

        Self {
            base,
            profile_idc: h264_profile::INVALID,
            level_idc: 13, // STD_VIDEO_H264_LEVEL_IDC_5_0 index
            hrd_bitrate: 0,
            pic_width_in_mbs: 0,
            pic_height_in_map_units: 0,
            num_ref_l0: 0,
            num_ref_l1: 0,
            num_ref_frames: 0,
            entropy_coding_mode: EntropyCodingMode::Cabac,
            adaptive_transform_mode: AdaptiveTransformMode::Enable,
            sps_id: 0,
            pps_id: 0,
            num_slices_per_picture: crate::vk_video_encoder::vk_encoder_config::DEFAULT_NUM_SLICES_PER_PICTURE,
            slice_count: 1,
            qpprime_y_zero_transform_bypass_flag: true,
            constrained_intra_pred_flag: false,
        }
    }
}

impl EncoderConfigH264 {
    /// Initialize parameters (base + H.264 specific).
    ///
    /// Equivalent to the C++ `EncoderConfigH264::InitializeParameters`.
    pub fn initialize_parameters(&mut self) -> Result<(), vk::Result> {
        self.base.initialize_parameters()?;

        self.hrd_bitrate = self.base.max_bitrate;
        self.pic_width_in_mbs = div_up(self.base.encode_width, 16);
        self.pic_height_in_map_units = div_up(self.base.encode_height, 16);

        if self.pic_width_in_mbs > 0 && self.pic_height_in_map_units > 0 {
            self.init_profile_level();
            return Ok(());
        }

        Err(vk::Result::ERROR_UNKNOWN)
    }

    /// Determine the H.264 level given the DPB size, bitrate, VBV buffer, and frame rate.
    ///
    /// Equivalent to the C++ `EncoderConfigH264::DetermineLevel`.
    pub fn determine_level(
        &self,
        _dpb_size: u8,
        bitrate: u32,
        vbv_buffer_size: u32,
        frame_rate: f64,
    ) -> u32 {
        let mut cpb_br_nal_factor: u32 = 1200;
        if self.profile_idc >= h264_profile::HIGH {
            cpb_br_nal_factor = if self.profile_idc >= h264_profile::HIGH_444_PREDICTIVE {
                4800
            } else {
                1500
            };
        }

        let frame_size_in_mbs = self.pic_width_in_mbs * self.pic_height_in_map_units;
        for limits in LEVEL_LIMITS_H264.iter() {
            if (frame_size_in_mbs as f64 * frame_rate) > limits.max_mbps as f64 {
                continue;
            }
            if frame_size_in_mbs > limits.max_fs {
                continue;
            }
            if (frame_size_in_mbs as u64 * self.num_ref_frames as u64 * 384)
                > (limits.max_dpb * 1024.0) as u64
            {
                continue;
            }
            if bitrate != 0 && bitrate > limits.max_br * cpb_br_nal_factor {
                continue;
            }
            if vbv_buffer_size != 0 && vbv_buffer_size > limits.max_cpb * cpb_br_nal_factor {
                continue;
            }
            return limits.level;
        }

        tracing::error!("Invalid h264_level");
        u32::MAX // INVALID
    }

    /// Compute the sample aspect ratio from the display aspect ratio and set it in VUI.
    ///
    /// Equivalent to the C++ `EncoderConfigH264::SetAspectRatio` static method.
    /// Returns `(aspect_ratio_idc, sar_width, sar_height)`.
    pub fn compute_aspect_ratio(
        width: i32,
        height: i32,
        dar_width: i32,
        dar_height: i32,
    ) -> (u32, u16, u16) {
        // SAR table (subset of H.264 Table E-1)
        static SAR_TABLE: &[(u32, u32, u32)] = &[
            (1, 1, 1),   // SQUARE
            (12, 11, 2),
            (10, 11, 3),
            (16, 11, 4),
            (40, 33, 5),
            (24, 11, 6),
            (20, 11, 7),
            (32, 11, 8),
            (80, 33, 9),
            (18, 11, 10),
            (15, 11, 11),
            (64, 33, 12),
            (160, 99, 13),
        ];

        if dar_width <= 0 && dar_height <= 0 {
            return (0, 0, 0);
        }

        let w = (height as u32) * (dar_width as u32);
        let h = (width as u32) * (dar_height as u32);
        let d = gcd(w, h);
        let w = w / d;
        let h = h / d;

        for &(sw, sh, idc) in SAR_TABLE {
            if sw == w && sh == h {
                return (idc, 0, 0);
            }
        }

        // Extended SAR (idc = 255)
        (255, (w & 0xFFFF) as u16, (h & 0xFFFF) as u16)
    }

    /// Initialize the H.264 profile and level.
    ///
    /// Equivalent to the C++ `EncoderConfigH264::InitProfileLevel`.
    pub fn init_profile_level(&mut self) {
        let mut use_8x8_transform = false;

        if self.adaptive_transform_mode == AdaptiveTransformMode::Enable {
            use_8x8_transform = true;
        } else if self.adaptive_transform_mode == AdaptiveTransformMode::Disable {
            use_8x8_transform = false;
        } else {
            // Autoselect
            if self.profile_idc == h264_profile::INVALID
                || self.profile_idc >= h264_profile::HIGH
            {
                use_8x8_transform = true;
            }
        }

        if self.profile_idc == h264_profile::INVALID {
            self.profile_idc = h264_profile::BASELINE;

            if self.base.gop_structure.consecutive_b_frame_count() > 0
                || self.entropy_coding_mode == EntropyCodingMode::Cabac
            {
                self.profile_idc = h264_profile::MAIN;
            }

            if use_8x8_transform {
                self.profile_idc = h264_profile::HIGH;
            }

            if self.base.input.bpp > 8 {
                self.profile_idc = 110; // High 10
            }

            // Upgrade to HIGH_444_PREDICTIVE for lossless or 4:4:4
            if self.base.input.chroma_subsampling
                == vk::VideoChromaSubsamplingFlagsKHR::_444
            {
                self.profile_idc = h264_profile::HIGH_444_PREDICTIVE;
            }
        }

        let level_bit_rate = if self.base.rate_control_mode
            != vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
            && self.hrd_bitrate == 0
        {
            self.base.average_bitrate
        } else {
            self.hrd_bitrate
        };

        let frame_rate = if self.base.frame_rate_numerator > 0
            && self.base.frame_rate_denominator > 0
        {
            self.base.frame_rate_numerator as f64 / self.base.frame_rate_denominator as f64
        } else {
            FRAME_RATE_NUM_DEFAULT as f64 / FRAME_RATE_DEN_DEFAULT as f64
        };

        self.level_idc = self.determine_level(
            self.base.dpb_count as u8,
            level_bit_rate,
            self.base.vbv_buffer_size,
            frame_rate,
        );
    }

    /// Initialize the DPB count for H.264.
    ///
    /// Equivalent to the C++ `EncoderConfigH264::InitDpbCount`.
    pub fn init_dpb_count(&mut self) -> i8 {
        self.base.dpb_count = 0;

        let level_idx = self.level_idc as usize;
        if level_idx >= LEVEL_LIMITS_H264.len() {
            return 16;
        }

        let level_dpb_size = ((1024.0 * LEVEL_LIMITS_H264[level_idx].max_dpb)
            / (self.pic_width_in_mbs as f64
                * self.pic_height_in_map_units as f64
                * 384.0)) as u8;

        let level_dpb_size = level_dpb_size.min(
            crate::vk_video_encoder::vk_encoder_config::DEFAULT_MAX_NUM_REF_FRAMES as u8,
        );

        let dpb_size = if self.base.dpb_count < 1 {
            level_dpb_size + 1
        } else {
            (self.base.dpb_count as u8).min(level_dpb_size) + 1
        };

        self.base.dpb_count = dpb_size as i8;
        dpb_size as i8
    }

    /// Initialize rate control for H.264.
    ///
    /// Equivalent to the C++ `EncoderConfigH264::InitRateControl`.
    pub fn init_rate_control(&mut self) -> bool {
        let level_idx = self.level_idc as usize;
        if level_idx >= LEVEL_LIMITS_H264.len() {
            return false;
        }

        let mut level_bit_rate = if self.base.rate_control_mode
            != vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
            && self.hrd_bitrate == 0
        {
            self.base.average_bitrate
        } else {
            self.hrd_bitrate
        };

        // 800 instead of 1000 for BD compliance at level 4.1
        level_bit_rate =
            level_bit_rate.max(LEVEL_LIMITS_H264[level_idx].max_br * 800).min(120_000_000);

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
                (LEVEL_LIMITS_H264[level_idx].max_cpb * 1000).min(120_000_000);
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
        }

        true
    }

    /// Build an `EncoderH264State` populated from this config's profile, level,
    /// and coding tool settings.
    ///
    /// This replaces the hard-coded SPS/PPS construction in `Encoder::create_session_parameters()`.
    /// The returned state contains `H264Sps` and `H264Pps` with values derived from the
    /// config rather than inline literals.
    pub fn build_sps_pps_state(
        &self,
        aligned_w: u32,
        aligned_h: u32,
        config_width: u32,
        config_height: u32,
        max_num_ref_frames: u32,
        log2_max_frame_num_minus4: u32,
        log2_max_pic_order_cnt_lsb_minus4: u32,
    ) -> crate::vk_video_encoder::vk_video_encoder_state_h264::EncoderH264State {
        use crate::vk_video_encoder::vk_video_encoder_state_h264::*;

        let needs_crop = aligned_w != config_width || aligned_h != config_height;
        let use_8x8 = match self.adaptive_transform_mode {
            AdaptiveTransformMode::Enable => true,
            AdaptiveTransformMode::Disable => false,
            AdaptiveTransformMode::AutoSelect => self.profile_idc >= h264_profile::HIGH,
        };

        let sps = H264Sps {
            profile_idc: self.profile_idc,
            level_idc: self.level_idc,
            seq_parameter_set_id: self.sps_id,
            chroma_format_idc: 1, // 4:2:0
            pic_width_in_mbs_minus1: (aligned_w / 16).saturating_sub(1),
            pic_height_in_map_units_minus1: (aligned_h / 16).saturating_sub(1),
            max_num_ref_frames: max_num_ref_frames as u8,
            pic_order_cnt_type: 0, // POC type 0
            log2_max_frame_num_minus4: log2_max_frame_num_minus4 as u8,
            log2_max_pic_order_cnt_lsb_minus4: log2_max_pic_order_cnt_lsb_minus4 as u8,
            frame_crop_right_offset: (aligned_w - config_width) / 2,
            frame_crop_bottom_offset: (aligned_h - config_height) / 2,
            frame_mbs_only_flag: true,
            frame_cropping_flag: needs_crop,
            direct_8x8_inference_flag: true,
            qpprime_y_zero_transform_bypass_flag: self.qpprime_y_zero_transform_bypass_flag,
            ..Default::default()
        };

        let pps = H264Pps {
            seq_parameter_set_id: self.sps_id,
            pic_parameter_set_id: self.pps_id,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            transform_8x8_mode_flag: use_8x8,
            entropy_coding_mode_flag: self.entropy_coding_mode == EntropyCodingMode::Cabac,
            deblocking_filter_control_present_flag: true,
            constrained_intra_pred_flag: self.constrained_intra_pred_flag,
            ..Default::default()
        };

        EncoderH264State {
            sps_info: sps,
            pps_info: pps,
            vui_info: H264Vui::default(),
            hrd_parameters: H264HrdParameters::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Profile / level mapping helpers
// ---------------------------------------------------------------------------

/// Map a raw H.264 profile_idc integer to the vulkanalia StdVideoH264ProfileIdc.
pub fn profile_idc_to_std_video(profile_idc: u32) -> vk::video::StdVideoH264ProfileIdc {
    match profile_idc {
        66  => vk::video::STD_VIDEO_H264_PROFILE_IDC_BASELINE,
        77  => vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN,
        100 => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH,
        244 => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH_444_PREDICTIVE,
        _   => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH,
    }
}

/// Map a LEVEL_LIMITS_H264 table index (0–18) to the vulkanalia StdVideoH264LevelIdc.
///
/// The `level` field in LEVEL_LIMITS_H264 is the ordinal of the
/// StdVideoH264LevelIdc enum (0 = 1.0, 1 = 1.1, ..., 11 = 4.1, ..., 18 = 6.2).
pub fn level_index_to_std_video(level_index: u32) -> vk::video::StdVideoH264LevelIdc {
    vk::video::StdVideoH264LevelIdc(level_index as std::ffi::c_int)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = EncoderConfigH264::default();
        assert_eq!(cfg.profile_idc, h264_profile::INVALID);
        assert_eq!(cfg.entropy_coding_mode, EntropyCodingMode::Cabac);
        assert_eq!(cfg.adaptive_transform_mode, AdaptiveTransformMode::Enable);
        assert_eq!(cfg.base.frame_rate_numerator, FRAME_RATE_NUM_DEFAULT);
        assert_eq!(cfg.base.frame_rate_denominator, FRAME_RATE_DEN_DEFAULT);
    }

    #[test]
    fn test_level_limits_table_size() {
        assert_eq!(LEVEL_LIMITS_H264.len(), 19);
    }

    #[test]
    fn test_determine_level_1080p() {
        let mut cfg = EncoderConfigH264::default();
        cfg.profile_idc = h264_profile::HIGH;
        cfg.pic_width_in_mbs = div_up(1920, 16);
        cfg.pic_height_in_map_units = div_up(1080, 16);
        cfg.num_ref_frames = 4;

        let level = cfg.determine_level(4, 0, 0, 30.0);
        // 1080p @ 30fps with 4 ref frames should be at least level 4.0
        assert!(level >= 10, "level={}", level);
    }

    #[test]
    fn test_compute_aspect_ratio_square() {
        let (idc, sw, sh) = EncoderConfigH264::compute_aspect_ratio(1920, 1080, 16, 9);
        // 16:9 at 1920x1080 => SAR 1:1
        assert_eq!(idc, 1); // SQUARE
    }

    #[test]
    fn test_compute_aspect_ratio_no_dar() {
        let (idc, _, _) = EncoderConfigH264::compute_aspect_ratio(1920, 1080, 0, 0);
        assert_eq!(idc, 0);
    }

    #[test]
    fn test_init_profile_level_baseline() {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 320;
        cfg.base.input.height = 240;
        cfg.base.input.bpp = 8;
        cfg.base.gop_structure.set_consecutive_b_frame_count(0);
        cfg.entropy_coding_mode = EntropyCodingMode::Cavlc;
        cfg.adaptive_transform_mode = AdaptiveTransformMode::Disable;
        cfg.profile_idc = h264_profile::INVALID;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_mbs = div_up(cfg.base.encode_width, 16);
        cfg.pic_height_in_map_units = div_up(cfg.base.encode_height, 16);
        cfg.init_profile_level();
        assert_eq!(cfg.profile_idc, h264_profile::BASELINE);
    }

    #[test]
    fn test_init_profile_level_main_with_b_frames() {
        let mut cfg = EncoderConfigH264::default();
        cfg.base.input.width = 1920;
        cfg.base.input.height = 1080;
        cfg.base.input.bpp = 8;
        cfg.base.gop_structure.set_consecutive_b_frame_count(2);
        cfg.entropy_coding_mode = EntropyCodingMode::Cavlc;
        cfg.adaptive_transform_mode = AdaptiveTransformMode::Disable;
        cfg.profile_idc = h264_profile::INVALID;
        cfg.base.initialize_parameters().unwrap();
        cfg.pic_width_in_mbs = div_up(cfg.base.encode_width, 16);
        cfg.pic_height_in_map_units = div_up(cfg.base.encode_height, 16);
        cfg.init_profile_level();
        assert_eq!(cfg.profile_idc, h264_profile::MAIN);
    }

    #[test]
    fn test_profile_idc_to_std_video() {
        assert_eq!(profile_idc_to_std_video(66), vk::video::STD_VIDEO_H264_PROFILE_IDC_BASELINE);
        assert_eq!(profile_idc_to_std_video(77), vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN);
        assert_eq!(profile_idc_to_std_video(100), vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
        assert_eq!(profile_idc_to_std_video(244), vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH_444_PREDICTIVE);
        // Unknown falls back to HIGH
        assert_eq!(profile_idc_to_std_video(999), vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
    }

    #[test]
    fn test_level_index_to_std_video() {
        // Table index 0 = Level 1.0, index 11 = Level 4.1
        assert_eq!(level_index_to_std_video(0), vk::video::STD_VIDEO_H264_LEVEL_IDC_1_0);
        assert_eq!(level_index_to_std_video(11), vk::video::STD_VIDEO_H264_LEVEL_IDC_4_1);
    }

    #[test]
    fn test_build_sps_pps_state_480p() {
        let mut cfg = EncoderConfigH264::default();
        cfg.profile_idc = h264_profile::HIGH;
        cfg.level_idc = 11; // Level 4.1

        let state = cfg.build_sps_pps_state(
            640, 480,  // aligned
            640, 480,  // config (no crop needed)
            3,         // max_num_ref_frames
            0,         // log2_max_frame_num_minus4
            4,         // log2_max_pic_order_cnt_lsb_minus4
        );

        assert_eq!(state.sps_info.profile_idc, h264_profile::HIGH);
        assert_eq!(state.sps_info.level_idc, 11);
        assert_eq!(state.sps_info.pic_width_in_mbs_minus1, 39);  // 640/16 - 1
        assert_eq!(state.sps_info.pic_height_in_map_units_minus1, 29); // 480/16 - 1
        assert_eq!(state.sps_info.max_num_ref_frames, 3);
        assert!(!state.sps_info.frame_cropping_flag);
        assert!(state.sps_info.frame_mbs_only_flag);
        assert!(state.sps_info.direct_8x8_inference_flag);
        // Default config: CABAC + 8x8 transform
        assert!(state.pps_info.entropy_coding_mode_flag);
        assert!(state.pps_info.transform_8x8_mode_flag);
        assert!(state.pps_info.deblocking_filter_control_present_flag);
    }

    #[test]
    fn test_build_sps_pps_state_with_crop() {
        let mut cfg = EncoderConfigH264::default();
        cfg.profile_idc = h264_profile::HIGH;
        cfg.level_idc = 11;

        let state = cfg.build_sps_pps_state(
            640, 496,  // aligned (padded from 480)
            640, 480,  // config
            3, 0, 4,
        );

        assert!(state.sps_info.frame_cropping_flag);
        assert_eq!(state.sps_info.frame_crop_bottom_offset, (496 - 480) / 2);
        assert_eq!(state.sps_info.frame_crop_right_offset, 0);
    }
}
