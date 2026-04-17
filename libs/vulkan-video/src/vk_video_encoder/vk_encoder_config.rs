// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderConfig.h + VkEncoderConfig.cpp
//!
//! Base encoder configuration: input image parameters, file handlers,
//! rate control, GOP structure, and the `EncoderConfig` base struct.
//!
//! Divergence from C++: File I/O handlers (EncoderInputFileHandler,
//! EncoderOutputFileHandler, EncoderQpMapFileHandler) are represented as
//! placeholder structs since they depend on memory-mapped I/O (`mio`),
//! which is not available (no external deps beyond ash). The full
//! implementation will use std::fs when needed.

use vulkanalia::vk;

use crate::vk_video_encoder::vk_video_encoder_def::ConstQpSettings;
use crate::vk_video_encoder::vk_video_gop_structure::{FrameType, VkVideoGopStructure};

/// Map bits-per-pixel to `VkVideoComponentBitDepthFlagBitsKHR`.
///
/// Equivalent to the C++ `GetComponentBitDepthFlagBits` free function.
pub fn get_component_bit_depth_flag_bits(bpp: u32) -> vk::VideoComponentBitDepthFlagsKHR {
    match bpp {
        8 => vk::VideoComponentBitDepthFlagsKHR::_8,
        10 => vk::VideoComponentBitDepthFlagsKHR::_10,
        12 => vk::VideoComponentBitDepthFlagsKHR::_12,
        _ => vk::VideoComponentBitDepthFlagsKHR::empty(),
    }
}

// ---------------------------------------------------------------------------
// EncoderInputImageParameters
// ---------------------------------------------------------------------------

/// Parameters describing the raw input image format.
///
/// Equivalent to the C++ `EncoderInputImageParameters` struct.
#[derive(Debug, Clone)]
pub struct EncoderInputImageParameters {
    pub width: u32,
    pub height: u32,
    pub bpp: u8,
    pub msb_shift: i8,
    pub chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
    pub num_planes: u32,
    pub plane_layouts: [vk::SubresourceLayout; 3],
    pub full_image_size: u64,
    pub vk_format: vk::Format,
}

impl Default for EncoderInputImageParameters {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            bpp: 8,
            msb_shift: -1,
            chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR::_420,
            num_planes: 3,
            plane_layouts: [vk::SubresourceLayout::default(); 3],
            full_image_size: 0,
            vk_format: vk::Format::G8_B8_R8_3PLANE_420_UNORM,
        }
    }
}

impl EncoderInputImageParameters {
    /// Validate and compute derived layout values (plane sizes, offsets, format).
    ///
    /// Returns `true` on success.
    ///
    /// Equivalent to the C++ `VerifyInputs` method. The `vkFormat` resolution
    /// is deferred (set to UNDEFINED) since `VkVideoCoreProfile::CodecGetVkFormat`
    /// is not yet ported; callers must resolve the format externally.
    pub fn verify_inputs(&mut self) -> bool {
        if self.width == 0 || self.height == 0 {
            tracing::error!(
                "Invalid input width ({}) and/or height({}) parameters!",
                self.width,
                self.height
            );
            return false;
        }

        let bytes_per_pixel = (self.bpp as u32 + 7) / 8;
        if !(1..=2).contains(&bytes_per_pixel) {
            tracing::error!("Invalid input bpp ({}) parameter!", self.bpp);
            return false;
        }

        let mut offset: u64 = 0;
        for plane in 0..self.num_planes as usize {
            let mut plane_stride = bytes_per_pixel * self.width;
            let mut plane_height = self.height;

            if plane > 0 {
                if self.chroma_subsampling == vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME {
                    plane_stride = 0;
                    plane_height = 0;
                } else if self.chroma_subsampling == vk::VideoChromaSubsamplingFlagsKHR::_420 {
                    plane_stride = (plane_stride + 1) / 2;
                    plane_height = (plane_height + 1) / 2;
                } else if self.chroma_subsampling == vk::VideoChromaSubsamplingFlagsKHR::_422 {
                    plane_stride = (plane_stride + 1) / 2;
                }
                // 444: no change
            }

            if (self.plane_layouts[plane].row_pitch as u32) < plane_stride {
                self.plane_layouts[plane].row_pitch = plane_stride as u64;
            }

            let min_size = self.plane_layouts[plane].row_pitch * plane_height as u64;
            if self.plane_layouts[plane].size < min_size {
                self.plane_layouts[plane].size = min_size;
            }

            if self.plane_layouts[plane].offset < offset {
                self.plane_layouts[plane].offset = offset;
            }

            offset += self.plane_layouts[plane].size;
        }

        self.full_image_size = offset;

        // Format resolution deferred -- caller must set vk_format via CodecGetVkFormat
        // equivalent once the video core profile module is ported.
        // For now, leave at default.

        true
    }
}

// ---------------------------------------------------------------------------
// File handler stubs
// ---------------------------------------------------------------------------

/// Placeholder for the C++ `EncoderInputFileHandler`.
///
/// Full file I/O and memory-mapped input will be implemented when the
/// encoder pipeline is wired up. Currently stores filename and basic state.
#[derive(Debug, Clone, Default)]
pub struct EncoderInputFileHandler {
    pub file_name: String,
    pub frame_size: u32,
    pub curr_frame_offset: usize,
    pub y4m_header_offset: u64,
    pub verbose: bool,
}

impl EncoderInputFileHandler {
    pub fn has_file_name(&self) -> bool {
        !self.file_name.is_empty()
    }

    /// Compute frame size from geometry and set internal state.
    ///
    /// Equivalent to the C++ `SetFrameGeometry` method.
    pub fn set_frame_geometry(
        &mut self,
        width: u32,
        height: u32,
        bpp: u8,
        chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
    ) -> u32 {
        let num_bytes = ((bpp as u32) + 7) / 8;
        let sampling_factor: f64 =
            if chroma_subsampling == vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME {
                1.0
            } else if chroma_subsampling == vk::VideoChromaSubsamplingFlagsKHR::_420 {
                1.5
            } else if chroma_subsampling == vk::VideoChromaSubsamplingFlagsKHR::_422 {
                2.0
            } else {
                // 444
                3.0
            };

        self.frame_size = (width as f64 * height as f64 * num_bytes as f64 * sampling_factor) as u32;
        self.curr_frame_offset = 0;
        self.frame_size
    }

    pub fn get_max_frame_count(&self) -> u32 {
        // Placeholder: would need actual file size
        0
    }
}

/// Placeholder for the C++ `EncoderOutputFileHandler`.
#[derive(Debug, Clone, Default)]
pub struct EncoderOutputFileHandler {
    pub file_name: String,
}

impl EncoderOutputFileHandler {
    pub fn has_file_name(&self) -> bool {
        !self.file_name.is_empty()
    }
}

/// Placeholder for the C++ `EncoderQpMapFileHandler`.
#[derive(Debug, Clone, Default)]
pub struct EncoderQpMapFileHandler {
    pub file_name: String,
    pub verbose: bool,
}

impl EncoderQpMapFileHandler {
    pub fn has_file_name(&self) -> bool {
        !self.file_name.is_empty()
    }
}

// ---------------------------------------------------------------------------
// QP Map mode / Intra Refresh mode
// ---------------------------------------------------------------------------

/// QP map mode selection.
///
/// Equivalent to the C++ `EncoderConfig::QpMapMode` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QpMapMode {
    DeltaQpMap,
    EmphasisMap,
}

impl Default for QpMapMode {
    fn default() -> Self {
        Self::DeltaQpMap
    }
}

/// Intra-refresh mode selection.
///
/// Equivalent to the C++ `EncoderConfig::IntraRefreshMode` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntraRefreshMode {
    None,
    PerPartition,
    BlockRows,
    BlockColumns,
    Blocks,
}

impl Default for IntraRefreshMode {
    fn default() -> Self {
        Self::None
    }
}

// ---------------------------------------------------------------------------
// Filter type (placeholder for VulkanFilterYuvCompute)
// ---------------------------------------------------------------------------

/// Placeholder for `VulkanFilterYuvCompute::FilterType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    YcbcrCopy,
}

impl Default for FilterType {
    fn default() -> Self {
        Self::YcbcrCopy
    }
}

// ---------------------------------------------------------------------------
// EncoderConfig — base configuration
// ---------------------------------------------------------------------------

/// Default constants from the C++ `EncoderConfig` enum.
pub const DEFAULT_NUM_INPUT_IMAGES: u32 = 16;
pub const DEFAULT_GOP_FRAME_COUNT: u32 = 16;
pub const DEFAULT_GOP_IDR_PERIOD: u32 = 60;
pub const DEFAULT_CONSECUTIVE_B_FRAME_COUNT: u8 = 3;
pub const DEFAULT_TEMPORAL_LAYER_COUNT: u8 = 1;
pub const DEFAULT_NUM_SLICES_PER_PICTURE: u32 = 4;
pub const DEFAULT_MAX_NUM_REF_FRAMES: u32 = 16;
pub const ZERO_GOP_FRAME_COUNT: u32 = 0;
pub const ZERO_GOP_IDR_PERIOD: u32 = 0;
pub const CONSECUTIVE_B_FRAME_COUNT_MAX_VALUE: u8 = u8::MAX;

/// Base encoder configuration.
///
/// Equivalent to the C++ `EncoderConfig` struct. Codec-specific configuration
/// (H264, H265, AV1) extends this via composition (see the codec-specific
/// config modules).
///
/// Fields that reference Vulkan capability structs or external types
/// (VulkanDeviceContext, VkVideoCoreProfile, etc.) are represented as
/// placeholder types or omitted until those modules are ported.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub app_name: String,
    pub device_id: i32,
    pub queue_id: i32,
    pub codec: vk::VideoCodecOperationFlagsKHR,
    pub use_dpb_array: bool,
    pub num_input_images: u32,
    pub input: EncoderInputImageParameters,
    pub encode_bit_depth_luma: u8,
    pub encode_bit_depth_chroma: u8,
    pub encode_num_planes: u8,
    pub num_bitstream_buffers_to_preallocate: u8,
    pub encode_chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
    pub encode_offset_x: u32,
    pub encode_offset_y: u32,
    pub encode_width: u32,
    pub encode_height: u32,
    pub encode_aligned_width: u32,
    pub encode_aligned_height: u32,
    pub encode_max_width: u32,
    pub encode_max_height: u32,
    pub start_frame: u32,
    pub num_frames: u32,
    pub codec_block_alignment: u32,
    pub quality_level: u32,
    pub rate_control_mode: vk::VideoEncodeRateControlModeFlagsKHR,
    pub average_bitrate: u32,
    pub max_bitrate: u32,
    pub hrd_bitrate: u32,
    pub vbv_buffer_size: u32,
    pub vbv_initial_delay: u32,
    pub frame_rate_numerator: u32,
    pub frame_rate_denominator: u32,
    pub min_qp: i32,
    pub max_qp: i32,
    pub const_qp: ConstQpSettings,
    pub enable_qp_map: bool,
    pub qp_map_mode: QpMapMode,
    pub gop_structure: VkVideoGopStructure,
    pub dpb_count: i8,

    // Intra-refresh parameters
    pub enable_intra_refresh: bool,
    pub intra_refresh_cycle_duration: u32,
    pub intra_refresh_mode: IntraRefreshMode,
    pub intra_refresh_cycle_restart_index: u32,
    pub intra_refresh_skipped_start_index: u32,

    // VUI parameters
    pub dar_width: u32,
    pub dar_height: u32,
    pub aspect_ratio_info_present_flag: bool,
    pub overscan_info_present_flag: bool,
    pub overscan_appropriate_flag: bool,
    pub video_signal_type_present_flag: bool,
    pub video_full_range_flag: bool,
    pub color_description_present_flag: bool,
    pub chroma_loc_info_present_flag: bool,
    pub bitstream_restriction_flag: bool,
    pub video_format: u8,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub max_num_reorder_frames: u8,
    pub max_dec_frame_buffering: u8,
    pub chroma_sample_loc_type: u8,

    pub input_file_handler: EncoderInputFileHandler,
    pub output_file_handler: EncoderOutputFileHandler,
    pub qp_map_file_handler: EncoderQpMapFileHandler,

    pub filter_type: FilterType,

    // Adaptive Quantization
    pub enable_aq: bool,
    pub spatial_aq_strength: f32,
    pub temporal_aq_strength: f32,
    pub aq_dump_dir: String,

    // Flags
    pub validate: bool,
    pub validate_verbose: bool,
    pub verbose: bool,
    pub verbose_frame_struct: bool,
    pub verbose_msg: bool,
    pub enable_frame_present: bool,
    pub enable_frame_direct_mode_present: bool,
    pub enable_hw_load_balancing: bool,
    pub no_device_fallback: bool,
    pub select_video_with_compute_queue: bool,
    pub enable_preprocess_compute_filter: bool,
    pub repeat_input_frames: bool,
    pub enable_picture_row_col_replication: u32,
    pub enable_out_of_order_recording: bool,
    pub enable_psnr_metrics: bool,
    pub crc_init_value: Vec<u32>,
    pub disable_encode_parameter_optimizations: bool,
    pub async_assembly: bool,
    pub assembly_thread_count: u32,
    pub output_crc_per_frame: bool,
    pub crc_output_file_name: String,
    pub drm_format_modifier_index: i32,
    pub selected_drm_format_modifier: u64,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            app_name: String::new(),
            device_id: -1,
            queue_id: 0,
            codec: vk::VideoCodecOperationFlagsKHR::NONE,
            use_dpb_array: false,
            num_input_images: DEFAULT_NUM_INPUT_IMAGES,
            input: EncoderInputImageParameters::default(),
            encode_bit_depth_luma: 0,
            encode_bit_depth_chroma: 0,
            encode_num_planes: 2,
            num_bitstream_buffers_to_preallocate: 8,
            encode_chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR::_420,
            encode_offset_x: 0,
            encode_offset_y: 0,
            encode_width: 0,
            encode_height: 0,
            encode_aligned_width: 0,
            encode_aligned_height: 0,
            encode_max_width: 0,
            encode_max_height: 0,
            start_frame: 0,
            num_frames: 0,
            codec_block_alignment: 16,
            quality_level: 0,
            rate_control_mode: vk::VideoEncodeRateControlModeFlagsKHR::DEFAULT,
            average_bitrate: 0,
            max_bitrate: 0,
            hrd_bitrate: 0,
            vbv_buffer_size: 0,
            vbv_initial_delay: 0,
            frame_rate_numerator: 0,
            frame_rate_denominator: 0,
            min_qp: -1,
            max_qp: -1,
            const_qp: ConstQpSettings::default(),
            enable_qp_map: false,
            qp_map_mode: QpMapMode::default(),
            gop_structure: VkVideoGopStructure::new(
                ZERO_GOP_FRAME_COUNT as u8,
                ZERO_GOP_IDR_PERIOD as i32,
                CONSECUTIVE_B_FRAME_COUNT_MAX_VALUE,
                DEFAULT_TEMPORAL_LAYER_COUNT,
                FrameType::P,
                FrameType::P,
                false,
                0,
            ),
            dpb_count: 8,
            enable_intra_refresh: false,
            intra_refresh_cycle_duration: 0,
            intra_refresh_mode: IntraRefreshMode::default(),
            intra_refresh_cycle_restart_index: 0,
            intra_refresh_skipped_start_index: 0,
            dar_width: 0,
            dar_height: 0,
            aspect_ratio_info_present_flag: false,
            overscan_info_present_flag: false,
            overscan_appropriate_flag: false,
            video_signal_type_present_flag: false,
            video_full_range_flag: false,
            color_description_present_flag: false,
            chroma_loc_info_present_flag: false,
            bitstream_restriction_flag: false,
            video_format: 0,
            colour_primaries: 0,
            transfer_characteristics: 0,
            matrix_coefficients: 0,
            max_num_reorder_frames: 0,
            max_dec_frame_buffering: 0,
            chroma_sample_loc_type: 0,
            input_file_handler: EncoderInputFileHandler::default(),
            output_file_handler: EncoderOutputFileHandler::default(),
            qp_map_file_handler: EncoderQpMapFileHandler::default(),
            filter_type: FilterType::default(),
            enable_aq: false,
            spatial_aq_strength: -2.0,
            temporal_aq_strength: -2.0,
            aq_dump_dir: "./aqDump".to_string(),
            validate: false,
            validate_verbose: false,
            verbose: false,
            verbose_frame_struct: false,
            verbose_msg: false,
            enable_frame_present: false,
            enable_frame_direct_mode_present: false,
            enable_hw_load_balancing: false,
            no_device_fallback: false,
            select_video_with_compute_queue: false,
            enable_preprocess_compute_filter: true,
            repeat_input_frames: false,
            enable_picture_row_col_replication: 1,
            enable_out_of_order_recording: false,
            enable_psnr_metrics: false,
            crc_init_value: Vec::new(),
            disable_encode_parameter_optimizations: false,
            async_assembly: true,
            assembly_thread_count: 2,
            output_crc_per_frame: false,
            crc_output_file_name: String::new(),
            drm_format_modifier_index: -1,
            selected_drm_format_modifier: 0,
        }
    }
}

impl EncoderConfig {
    /// Whether PSNR metrics computation is enabled.
    pub fn is_psnr_metrics_enabled(&self) -> bool {
        self.enable_psnr_metrics
    }

    /// Initialize base parameters from the input image configuration.
    ///
    /// Equivalent to the C++ `EncoderConfig::InitializeParameters` virtual method.
    pub fn initialize_parameters(&mut self) -> Result<(), vk::Result> {
        if !self.input.verify_inputs() {
            return Err(vk::Result::ERROR_UNKNOWN);
        }

        // Deal with input shift values
        if self.input.msb_shift == -1 {
            if self.input.bpp > 8 {
                self.input.msb_shift = (16 - self.input.bpp as i16) as i8;
            } else {
                self.input.msb_shift = 0;
            }
        }

        self.encode_chroma_subsampling = self.input.chroma_subsampling;

        if self.encode_width == 0 || self.encode_width > self.input.width {
            self.encode_width = self.input.width;
        }

        if self.encode_height == 0 || self.encode_height > self.input.height {
            self.encode_height = self.input.height;
        }

        Ok(())
    }

    /// Initialize rate control parameters.
    ///
    /// Equivalent to the C++ `EncoderConfig::InitRateControl` virtual method.
    pub fn init_rate_control(&mut self) -> bool {
        let level_bit_rate = if self.rate_control_mode
            != vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
            && self.hrd_bitrate == 0
        {
            self.average_bitrate
        } else {
            self.hrd_bitrate
        };

        if self.average_bitrate == 0 {
            self.average_bitrate = if self.hrd_bitrate != 0 {
                self.hrd_bitrate
            } else {
                level_bit_rate
            };
        }

        if self.hrd_bitrate == 0 {
            if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::VBR
                && self.average_bitrate < level_bit_rate
            {
                self.hrd_bitrate = (self.average_bitrate * 3).min(level_bit_rate);
            } else {
                self.hrd_bitrate = self.average_bitrate;
            }
        }

        if self.average_bitrate > self.hrd_bitrate {
            self.average_bitrate = self.hrd_bitrate;
        }

        if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::CBR {
            self.hrd_bitrate = self.average_bitrate;
        }

        true
    }

    /// Initialize the video profile (placeholder).
    ///
    /// The full implementation requires `VkVideoCoreProfile` which is not yet ported.
    pub fn init_video_profile(&mut self) {
        if self.encode_bit_depth_luma == 0 {
            self.encode_bit_depth_luma = self.input.bpp;
        }
        if self.encode_bit_depth_chroma == 0 {
            self.encode_bit_depth_chroma = self.encode_bit_depth_luma;
        }
        // Full profile creation deferred until VkVideoCoreProfile is ported.
    }

    /// Default DPB count initialization.
    pub fn init_dpb_count(&mut self) -> i8 {
        16
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_config_defaults() {
        let cfg = EncoderConfig::default();
        assert_eq!(cfg.device_id, -1);
        assert_eq!(cfg.dpb_count, 8);
        assert_eq!(cfg.min_qp, -1);
        assert_eq!(cfg.max_qp, -1);
        assert!(!cfg.enable_psnr_metrics);
        assert_eq!(cfg.codec_block_alignment, 16);
        assert_eq!(cfg.num_input_images, DEFAULT_NUM_INPUT_IMAGES);
        assert!(cfg.async_assembly);
        assert_eq!(cfg.assembly_thread_count, 2);
    }

    #[test]
    fn test_input_image_params_default() {
        let params = EncoderInputImageParameters::default();
        assert_eq!(params.width, 0);
        assert_eq!(params.height, 0);
        assert_eq!(params.bpp, 8);
        assert_eq!(params.msb_shift, -1);
        assert_eq!(params.num_planes, 3);
    }

    #[test]
    fn test_input_image_verify_valid() {
        let mut params = EncoderInputImageParameters::default();
        params.width = 1920;
        params.height = 1080;
        assert!(params.verify_inputs());
        assert!(params.full_image_size > 0);
    }

    #[test]
    fn test_input_image_verify_invalid_dimensions() {
        let mut params = EncoderInputImageParameters::default();
        params.width = 0;
        params.height = 1080;
        assert!(!params.verify_inputs());
    }

    #[test]
    fn test_initialize_parameters() {
        let mut cfg = EncoderConfig::default();
        cfg.input.width = 1920;
        cfg.input.height = 1080;
        assert!(cfg.initialize_parameters().is_ok());
        assert_eq!(cfg.encode_width, 1920);
        assert_eq!(cfg.encode_height, 1080);
        assert_eq!(cfg.input.msb_shift, 0);
    }

    #[test]
    fn test_initialize_parameters_10bit() {
        let mut cfg = EncoderConfig::default();
        cfg.input.width = 1920;
        cfg.input.height = 1080;
        cfg.input.bpp = 10;
        assert!(cfg.initialize_parameters().is_ok());
        assert_eq!(cfg.input.msb_shift, 6); // 16 - 10
    }

    #[test]
    fn test_init_rate_control_cbr() {
        let mut cfg = EncoderConfig::default();
        cfg.rate_control_mode = vk::VideoEncodeRateControlModeFlagsKHR::CBR;
        cfg.average_bitrate = 5_000_000;
        cfg.hrd_bitrate = 5_000_000;
        assert!(cfg.init_rate_control());
        assert_eq!(cfg.hrd_bitrate, cfg.average_bitrate);
    }

    #[test]
    fn test_get_component_bit_depth_flag_bits() {
        assert_eq!(
            get_component_bit_depth_flag_bits(8),
            vk::VideoComponentBitDepthFlagsKHR::_8
        );
        assert_eq!(
            get_component_bit_depth_flag_bits(10),
            vk::VideoComponentBitDepthFlagsKHR::_10
        );
        assert_eq!(
            get_component_bit_depth_flag_bits(12),
            vk::VideoComponentBitDepthFlagsKHR::_12
        );
        assert!(get_component_bit_depth_flag_bits(7).is_empty());
    }

    #[test]
    fn test_frame_geometry() {
        let mut handler = EncoderInputFileHandler::default();
        let size = handler.set_frame_geometry(
            1920,
            1080,
            8,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
        );
        // 1920*1080 * 1 * 1.5 = 3110400
        assert_eq!(size, 3110400);
    }

    #[test]
    fn test_qp_map_mode_default() {
        assert_eq!(QpMapMode::default(), QpMapMode::DeltaQpMap);
    }

    #[test]
    fn test_intra_refresh_mode_default() {
        assert_eq!(IntraRefreshMode::default(), IntraRefreshMode::None);
    }
}
