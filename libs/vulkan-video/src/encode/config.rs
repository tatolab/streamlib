// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Encoder configuration types, enums, and validation.
//!
//! Contains both the user-facing [`SimpleEncoderConfig`] and the internal
//! [`EncodeConfig`] used by the encoder pipeline.

use vulkanalia::vk;
use vulkanalia::vk::Handle;

use crate::video_context::{VideoError, VideoResult};
use crate::vk_video_encoder::vk_video_encoder_def::{align_size, H264_MB_SIZE_ALIGNMENT};
use crate::vk_video_encoder::vk_video_gop_structure::{
    FrameType as GopFrameType, VkVideoGopStructure,
};

// ---------------------------------------------------------------------------
// FrameType (public re-export with simpler naming)
// ---------------------------------------------------------------------------

/// Frame type for encode submission.
///
/// Maps to the internal `vk_video_gop_structure::FrameType` but is presented
/// as the public API surface so callers do not need to reach into internal
/// modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// Instantaneous Decoder Refresh -- forces a clean-point.
    Idr,
    /// Intra frame (non-IDR).
    I,
    /// Predictive frame -- references previous frames.
    P,
    /// Bi-predictive frame -- references previous and future frames.
    B,
}

impl FrameType {
    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Idr => "IDR",
            Self::I => "I",
            Self::P => "P",
            Self::B => "B",
        }
    }
}

impl Default for FrameType {
    fn default() -> Self {
        Self::P
    }
}

// ---------------------------------------------------------------------------
// RateControlMode
// ---------------------------------------------------------------------------

/// Rate control mode for the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControlMode {
    /// Driver default / disabled.
    Default,
    /// Constant Bitrate.
    Cbr,
    /// Variable Bitrate.
    Vbr,
    /// Constant QP (no rate control).
    Cqp,
}

impl Default for RateControlMode {
    fn default() -> Self {
        Self::Default
    }
}

impl RateControlMode {
    /// Convert to the Vulkan flag equivalent.
    pub fn to_vk_flags(self) -> vk::VideoEncodeRateControlModeFlagsKHR {
        match self {
            Self::Default => vk::VideoEncodeRateControlModeFlagsKHR::DEFAULT,
            Self::Cbr => vk::VideoEncodeRateControlModeFlagsKHR::CBR,
            Self::Vbr => vk::VideoEncodeRateControlModeFlagsKHR::VBR,
            Self::Cqp => vk::VideoEncodeRateControlModeFlagsKHR::DISABLED,
        }
    }
}

// ---------------------------------------------------------------------------
// EncodeConfig
// ---------------------------------------------------------------------------

/// Internal encoder configuration.
///
/// This is the simplified configuration derived from `SimpleEncoderConfig`.
/// Internally it maps to the full `vk_encoder_config::EncoderConfig` which
/// carries all the nvpro detail fields.
#[derive(Debug, Clone)]
pub(crate) struct EncodeConfig {
    /// Encode width in pixels.
    pub(crate) width: u32,
    /// Encode height in pixels.
    pub(crate) height: u32,
    /// Frames per second (numerator).
    pub(crate) framerate_numerator: u32,
    /// Frames per second (denominator, typically 1).
    pub(crate) framerate_denominator: u32,
    /// Codec operation flags (e.g. `ENCODE_H264` or `ENCODE_H265`).
    #[allow(dead_code)] // Stored for future use in multi-codec paths
    pub(crate) codec: vk::VideoCodecOperationFlagsKHR,
    /// Chroma subsampling (default: 4:2:0).
    pub(crate) chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
    /// Luma bit depth (default: 8).
    pub(crate) luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
    /// Chroma bit depth (default: 8).
    pub(crate) chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
    /// Rate control mode.
    pub(crate) rate_control_mode: RateControlMode,
    /// Average bitrate in bits/sec (used for CBR and VBR).
    pub(crate) average_bitrate: u32,
    /// Maximum bitrate in bits/sec (used for VBR).
    pub(crate) max_bitrate: u32,
    /// Quality level (0 = driver default, higher = better quality/slower).
    pub(crate) quality_level: u32,
    /// Constant QP for intra frames (used when rate_control_mode == Cqp).
    pub(crate) const_qp_intra: i32,
    /// Constant QP for P frames (used when rate_control_mode == Cqp).
    pub(crate) const_qp_inter_p: i32,
    /// Constant QP for B frames (used when rate_control_mode == Cqp).
    pub(crate) const_qp_inter_b: i32,
    /// GOP size (number of frames between I-frames, 0 = single IDR).
    pub(crate) gop_size: u32,
    /// IDR period (number of frames between IDR frames, 0 = only first frame).
    pub(crate) idr_period: u32,
    /// Number of consecutive B-frames in the GOP (0 = IP-only).
    pub(crate) num_b_frames: u8,
    /// Maximum number of DPB reference frames.
    pub(crate) max_dpb_slots: u32,
    /// Bitstream output buffer size in bytes (0 = use default 2 MiB).
    pub(crate) bitstream_buffer_size: usize,
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            framerate_numerator: 30,
            framerate_denominator: 1,
            codec: vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
            chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR::_420,
            luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::_8,
            chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::_8,
            rate_control_mode: RateControlMode::Default,
            average_bitrate: 5_000_000,
            max_bitrate: 10_000_000,
            quality_level: 0,
            const_qp_intra: 26,
            const_qp_inter_p: 28,
            const_qp_inter_b: 30,
            gop_size: 30,
            idr_period: 60,
            num_b_frames: 0,
            max_dpb_slots: 16,
            bitstream_buffer_size: 0,
        }
    }
}

impl EncodeConfig {
    /// Validate the configuration, returning an error on invalid parameters.
    pub(crate) fn validate(&self) -> VideoResult<()> {
        if self.width == 0 || self.height == 0 {
            return Err(VideoError::BitstreamError(
                "Encode width and height must be non-zero".to_string(),
            ));
        }
        if self.framerate_numerator == 0 || self.framerate_denominator == 0 {
            return Err(VideoError::BitstreamError(
                "Framerate numerator and denominator must be non-zero".to_string(),
            ));
        }
        if self.max_dpb_slots == 0 || self.max_dpb_slots > 16 {
            return Err(VideoError::BitstreamError(format!(
                "max_dpb_slots must be 1..=16, got {}",
                self.max_dpb_slots
            )));
        }
        Ok(())
    }

    /// Compute the aligned width for the codec block alignment.
    pub(crate) fn aligned_width(&self) -> u32 {
        align_size(self.width, H264_MB_SIZE_ALIGNMENT)
    }

    /// Compute the aligned height for the codec block alignment.
    pub(crate) fn aligned_height(&self) -> u32 {
        align_size(self.height, H264_MB_SIZE_ALIGNMENT)
    }

    /// Effective bitstream buffer size (uses default if zero).
    pub(crate) fn effective_bitstream_buffer_size(&self) -> usize {
        if self.bitstream_buffer_size > 0 {
            self.bitstream_buffer_size
        } else {
            // Default: 2 MiB or width*height (whichever is larger) to handle
            // high-quality intra frames.
            let pixel_budget = (self.width as usize) * (self.height as usize);
            pixel_budget.max(2 * 1024 * 1024)
        }
    }
}

// ---------------------------------------------------------------------------
// EncodedOutput
// ---------------------------------------------------------------------------

/// The encoded bitstream chunk for a single frame.
#[derive(Debug, Clone)]
pub struct EncodedOutput {
    /// Raw encoded bitstream data (H.264/H.265 NAL units).
    pub data: Vec<u8>,
    /// The type of frame that was encoded.
    pub frame_type: FrameType,
    /// Presentation timestamp (frame index in input order).
    pub pts: u64,
    /// Encode order (may differ from pts when B-frames are used).
    pub encode_order: u64,
    /// Bitstream offset within the output buffer (from query feedback).
    pub bitstream_offset: u32,
    /// Bitstream size in bytes (from query feedback).
    pub bitstream_size: u32,
}

// ---------------------------------------------------------------------------
// DPB slot tracking
// ---------------------------------------------------------------------------

/// Internal DPB slot state for the encoder.
#[derive(Debug, Clone)]
pub(crate) struct DpbSlot {
    /// Per-slot image view into one array layer of the shared DPB image.
    pub(crate) view: vk::ImageView,
    /// Array layer index within the shared DPB image.
    #[allow(dead_code)] // Stored for multi-layer DPB configurations
    pub(crate) array_layer: u32,
    pub(crate) in_use: bool,
    pub(crate) frame_num: u64,
    pub(crate) poc: i32,
    /// H.264 picture type stored in this slot (STD_VIDEO_H264_PICTURE_TYPE_*).
    /// Used when building reference info so the driver knows whether a
    /// reference was an IDR, I, P, or B frame.
    pub(crate) pic_type: vk::video::StdVideoH264PictureType,
    /// H.265 picture type stored in this slot (STD_VIDEO_H265_PICTURE_TYPE_*).
    pub(crate) h265_pic_type: vk::video::StdVideoH265PictureType,
}

impl Default for DpbSlot {
    fn default() -> Self {
        Self {
            view: vk::ImageView::null(),
            array_layer: 0,
            in_use: false,
            frame_num: 0,
            poc: 0,
            pic_type: vk::video::STD_VIDEO_H264_PICTURE_TYPE_P,
            h265_pic_type: vk::video::STD_VIDEO_H265_PICTURE_TYPE_P,
        }
    }
}

// ---------------------------------------------------------------------------
// Encode feedback query result layout
// ---------------------------------------------------------------------------

/// Layout of the query pool result for `VK_QUERY_TYPE_VIDEO_ENCODE_FEEDBACK_KHR`.
///
/// When `VK_VIDEO_ENCODE_FEEDBACK_BITSTREAM_BUFFER_OFFSET_BIT_KHR` and
/// `VK_VIDEO_ENCODE_FEEDBACK_BITSTREAM_BYTES_WRITTEN_BIT_KHR` are both
/// requested, the result contains these two u32 fields in order.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EncodeFeedback {
    pub(crate) bitstream_offset: u32,
    pub(crate) bitstream_bytes_written: u32,
}

// ---------------------------------------------------------------------------
// Codec / Preset / EncodePacket
// ---------------------------------------------------------------------------

/// Codec selection for [`SimpleEncoderConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
}

/// Quality preset for [`SimpleEncoderConfig`].
///
/// Controls default GOP size, B-frame count, and rate-control parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Low latency streaming: CQP qp=20, GOP=30, IP-only.
    Fast,
    /// Balanced streaming: CQP qp=18, GOP=30, IP-only.
    Medium,
    /// High quality streaming: CQP qp=15, GOP=60, IP-only.
    Quality,
}

/// Encoded output packet returned by [`SimpleEncoder::submit_frame`].
#[derive(Debug, Clone)]
pub struct EncodePacket {
    /// Raw encoded bitstream data (H.264 / H.265 NAL units).
    pub data: Vec<u8>,
    /// The frame type that was encoded.
    pub frame_type: FrameType,
    /// Presentation timestamp (frame index in input order).
    pub pts: u64,
    /// `true` if this frame is an IDR keyframe.
    pub is_keyframe: bool,
    /// Monotonic timestamp in nanoseconds, passed through from the caller.
    /// `None` if the caller did not provide a timestamp.
    pub timestamp_ns: Option<i64>,
}

// ---------------------------------------------------------------------------
// SimpleEncoderConfig
// ---------------------------------------------------------------------------

/// User-facing configuration for [`SimpleEncoder`].
///
/// Only the essentials: resolution, fps, codec, and either a preset or
/// explicit QP / bitrate.  Everything else is derived automatically.
#[derive(Debug, Clone)]
pub struct SimpleEncoderConfig {
    /// Encode width in pixels (must be > 0, even).
    pub width: u32,
    /// Encode height in pixels (must be > 0, even).
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
    /// Codec to use.
    pub codec: Codec,
    /// Quality preset.  Ignored when `qp` or `bitrate_bps` is set.
    pub preset: Preset,
    /// Explicit constant QP (overrides preset).  `None` = use preset default.
    pub qp: Option<i32>,
    /// Explicit target bitrate in bits / second (overrides preset to VBR).
    pub bitrate_bps: Option<u32>,
    /// Streaming / live mode.  When `true`: 0 B-frames, periodic IDR at
    /// `idr_interval_secs`, SPS/PPS prepended to every IDR packet.
    pub streaming: bool,
    /// Seconds between IDR frames in streaming mode (default 2).
    pub idr_interval_secs: u32,
    /// When `true`, SPS/PPS bytes are prepended to every IDR packet.
    /// Defaults to `true` in streaming mode, `false` otherwise.
    pub prepend_header_to_idr: Option<bool>,
}

impl Default for SimpleEncoderConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fps: 30,
            codec: Codec::H264,
            preset: Preset::Medium,
            qp: None,
            bitrate_bps: None,
            streaming: false,
            idr_interval_secs: 2,
            prepend_header_to_idr: None,
        }
    }
}

impl SimpleEncoderConfig {
    /// Validate the configuration, returning an error string on invalid
    /// parameters.
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0 {
            return Err("width must be > 0".to_string());
        }
        if self.height == 0 {
            return Err("height must be > 0".to_string());
        }
        if self.width % 2 != 0 {
            return Err(format!("width must be even, got {}", self.width));
        }
        if self.height % 2 != 0 {
            return Err(format!("height must be even, got {}", self.height));
        }
        if self.fps == 0 {
            return Err("fps must be > 0".to_string());
        }
        if let Some(qp) = self.qp {
            if qp < 0 || qp > 51 {
                return Err(format!("qp must be 0..=51, got {}", qp));
            }
        }
        if let Some(br) = self.bitrate_bps {
            if br == 0 {
                return Err("bitrate_bps must be > 0 when set".to_string());
            }
        }
        if self.streaming && self.idr_interval_secs == 0 {
            return Err("idr_interval_secs must be > 0 in streaming mode".to_string());
        }
        Ok(())
    }

    /// Whether SPS/PPS should be prepended to each IDR packet.
    pub(crate) fn effective_prepend_header(&self) -> bool {
        self.prepend_header_to_idr.unwrap_or(self.streaming)
    }

    /// Derive the low-level [`EncodeConfig`] from this simple config.
    pub(crate) fn to_encode_config(&self) -> EncodeConfig {
        let codec_flag = match self.codec {
            Codec::H264 => vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
            Codec::H265 => vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
        };

        // Derive rate control and QP from preset / overrides.
        let (rc_mode, avg_br, max_br, qp_i, qp_p, qp_b) = if let Some(br) = self.bitrate_bps {
            (RateControlMode::Vbr, br, br * 2, 0, 0, 0)
        } else if let Some(qp) = self.qp {
            (RateControlMode::Cqp, 0, 0, qp, qp + 2, qp + 4)
        } else {
            match self.preset {
                Preset::Fast   => (RateControlMode::Cqp, 0, 0, 20, 22, 24),
                Preset::Medium => (RateControlMode::Cqp, 0, 0, 18, 18, 20),
                Preset::Quality => (RateControlMode::Cqp, 0, 0, 15, 15, 17),
            }
        };

        // GOP size and IDR period.
        let (gop_size, idr_period, num_b) = if self.streaming {
            let idr_p = self.idr_interval_secs * self.fps;
            (idr_p, idr_p, 0u8)  // No B-frames in streaming (latency)
        } else {
            match self.preset {
                Preset::Fast   => (30, 60, 0),
                Preset::Medium => (30, 60, 0),
                Preset::Quality => (60, 120, 0),
            }
        };

        // Encode quality level (maps to NVIDIA NVENC P1-P7 presets).
        // Higher = better quality, slightly more GPU compute per frame.
        // Zero-latency: does NOT add frame delay, only uses more GPU
        // time within the same frame's encode pass.
        let quality = match self.preset {
            Preset::Fast   => 3,  // ~P4: fast, moderate quality
            Preset::Medium => 5,  // ~P6: OBS recommended for streaming
            Preset::Quality => 7, // ~P7: max quality
        };

        EncodeConfig {
            width: self.width,
            height: self.height,
            framerate_numerator: self.fps,
            framerate_denominator: 1,
            codec: codec_flag,
            rate_control_mode: rc_mode,
            average_bitrate: avg_br,
            max_bitrate: max_br,
            const_qp_intra: qp_i,
            const_qp_inter_p: qp_p,
            const_qp_inter_b: qp_b,
            quality_level: quality,
            gop_size,
            idr_period,
            num_b_frames: num_b,
            max_dpb_slots: if num_b > 0 { 6 } else { 4 },
            ..Default::default()
        }
    }

    /// Build a [`VkVideoGopStructure`] matching this config.
    pub(crate) fn to_gop_structure(&self) -> VkVideoGopStructure {
        let enc_cfg = self.to_encode_config();
        VkVideoGopStructure::new(
            enc_cfg.gop_size.min(255) as u8,
            enc_cfg.idr_period as i32,
            enc_cfg.num_b_frames,
            1,  // temporal_layer_count
            GopFrameType::P,
            GopFrameType::P,
            false,
            0,
        )
    }
}
