// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Video session configuration and session parameter creation for decode.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionDeviceCommands;
use std::ptr;
use tracing::{debug, info};

use crate::nv_video_parser::vulkan_h264_decoder::{
    SeqParameterSet as H264Sps, PicParameterSet as H264Pps,
    H264PocType, H264LevelIdc,
};
use crate::nv_video_parser::vulkan_h265_decoder as h265dec;
use crate::video_context::VideoError;
use crate::vk_video_decoder::vk_video_decoder::{VkVideoDecoder, VkParserDetectedVideoFormat};

use super::SimpleDecoder;

impl SimpleDecoder {
    // ------------------------------------------------------------------
    // Session configuration
    // ------------------------------------------------------------------

    pub(crate) fn configure_session(&mut self) -> Result<(), VideoError> {
        let width = if self.config.max_width > 0 {
            self.config.max_width
        } else {
            self.sps_width
        };
        let height = if self.config.max_height > 0 {
            self.config.max_height
        } else {
            self.sps_height
        };

        if width == 0 || height == 0 {
            return Err(VideoError::BitstreamError(
                "Cannot configure: dimensions unknown (no SPS parsed yet)".to_string(),
            ));
        }

        let dpb_size = 16u32;

        // Initialize DPB tracking
        self.dpb_slot_in_use = vec![false; dpb_size as usize];
        self.dpb_slot_frame_num = vec![0u16; dpb_size as usize];
        self.dpb_slot_poc = vec![[0i32; 2]; dpb_size as usize];

        // Create VkVideoDecoder (ported C++ decode pipeline) for both codecs
        {
            let codec_flag = if self.config.codec == crate::encode::Codec::H265 {
                vk::VideoCodecOperationFlagsKHR::DECODE_H265
            } else {
                vk::VideoCodecOperationFlagsKHR::DECODE_H264
            };

            let mut vk_dec = VkVideoDecoder::new(
                self.ctx.clone(),
                self.decode_queue_family,
                self.decode_queue,
                codec_flag,
            )?;

            // Propagate sharing queue families for CONCURRENT DPB access
            if self.decode_queue_family != self.transfer_queue_family {
                vk_dec.set_sharing_queue_families(
                    vec![self.decode_queue_family, self.transfer_queue_family],
                );
            }

            let video_fmt = VkParserDetectedVideoFormat {
                codec: codec_flag,
                coded_width: width,
                coded_height: height,
                max_num_dpb_slots: dpb_size,
                ..Default::default()
            };

            let result = vk_dec.start_video_sequence(&video_fmt);
            if result < 0 {
                return Err(VideoError::BitstreamError(
                    "VkVideoDecoder::start_video_sequence failed".into(),
                ));
            }

            self.vk_decoder = Some(vk_dec);
        }

        if self.config.codec == crate::encode::Codec::H265 {
            self.create_session_params_h265()?;
        } else {
            self.create_session_params_h264_vk()?;
        }

        self.session_configured = true;
        info!(width, height, dpb_size, "Session configured");

        Ok(())
    }

    // ------------------------------------------------------------------
    // H.264 session parameters
    // ------------------------------------------------------------------

    /// Build StdVideoH264SequenceParameterSet from the ported parser's SPS.
    unsafe fn build_std_sps_from_parsed(
        sps: &H264Sps,
    ) -> vk::video::StdVideoH264SequenceParameterSet {
        let mut flags: vk::video::StdVideoH264SpsFlags = std::mem::zeroed();
        flags.set_direct_8x8_inference_flag(sps.flags.direct_8x8_inference_flag as u32);
        flags.set_frame_mbs_only_flag(sps.flags.frame_mbs_only_flag as u32);
        flags.set_frame_cropping_flag(sps.flags.frame_cropping_flag as u32);
        flags.set_mb_adaptive_frame_field_flag(sps.flags.mb_adaptive_frame_field_flag as u32);
        flags.set_vui_parameters_present_flag(sps.flags.vui_parameters_present_flag as u32);
        flags.set_delta_pic_order_always_zero_flag(sps.flags.delta_pic_order_always_zero_flag as u32);
        flags.set_separate_colour_plane_flag(sps.flags.separate_colour_plane_flag as u32);
        flags.set_qpprime_y_zero_transform_bypass_flag(sps.flags.qpprime_y_zero_transform_bypass_flag as u32);
        flags.set_gaps_in_frame_num_value_allowed_flag(sps.flags.gaps_in_frame_num_value_allowed_flag as u32);
        flags.set_seq_scaling_matrix_present_flag(sps.flags.seq_scaling_matrix_present_flag as u32);

        let profile_idc = match sps.profile_idc {
            66 => vk::video::STD_VIDEO_H264_PROFILE_IDC_BASELINE,
            77 => vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN,
            88 => vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN, // Extended → Main
            100 => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH,
            110 => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH,
            122 => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH,
            244 => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH_444_PREDICTIVE,
            _ => vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH,
        };

        let level_idc = match sps.level_idc {
            H264LevelIdc::Level1_0 => vk::video::STD_VIDEO_H264_LEVEL_IDC_1_0,
            H264LevelIdc::Level1_1 => vk::video::STD_VIDEO_H264_LEVEL_IDC_1_1,
            H264LevelIdc::Level1_2 => vk::video::STD_VIDEO_H264_LEVEL_IDC_1_2,
            H264LevelIdc::Level1_3 => vk::video::STD_VIDEO_H264_LEVEL_IDC_1_3,
            H264LevelIdc::Level2_0 => vk::video::STD_VIDEO_H264_LEVEL_IDC_2_0,
            H264LevelIdc::Level2_1 => vk::video::STD_VIDEO_H264_LEVEL_IDC_2_1,
            H264LevelIdc::Level2_2 => vk::video::STD_VIDEO_H264_LEVEL_IDC_2_2,
            H264LevelIdc::Level3_0 => vk::video::STD_VIDEO_H264_LEVEL_IDC_3_0,
            H264LevelIdc::Level3_1 => vk::video::STD_VIDEO_H264_LEVEL_IDC_3_1,
            H264LevelIdc::Level3_2 => vk::video::STD_VIDEO_H264_LEVEL_IDC_3_2,
            H264LevelIdc::Level4_0 => vk::video::STD_VIDEO_H264_LEVEL_IDC_4_0,
            H264LevelIdc::Level4_1 => vk::video::STD_VIDEO_H264_LEVEL_IDC_4_1,
            H264LevelIdc::Level4_2 => vk::video::STD_VIDEO_H264_LEVEL_IDC_4_2,
            H264LevelIdc::Level5_0 => vk::video::STD_VIDEO_H264_LEVEL_IDC_5_0,
            H264LevelIdc::Level5_1 => vk::video::STD_VIDEO_H264_LEVEL_IDC_5_1,
            H264LevelIdc::Level5_2 => vk::video::STD_VIDEO_H264_LEVEL_IDC_5_2,
            H264LevelIdc::Level6_0 => vk::video::STD_VIDEO_H264_LEVEL_IDC_6_0,
            H264LevelIdc::Level6_1 => vk::video::STD_VIDEO_H264_LEVEL_IDC_6_1,
            H264LevelIdc::Level6_2 => vk::video::STD_VIDEO_H264_LEVEL_IDC_6_2,
            _ => vk::video::STD_VIDEO_H264_LEVEL_IDC_4_1,
        };

        let chroma_format_idc = match sps.chroma_format_idc {
            0 => vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_MONOCHROME,
            1 => vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
            2 => vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_422,
            3 => vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_444,
            _ => vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
        };

        let poc_type = match sps.pic_order_cnt_type {
            H264PocType::Type0 => vk::video::STD_VIDEO_H264_POC_TYPE_0,
            H264PocType::Type1 => vk::video::STD_VIDEO_H264_POC_TYPE_1,
            H264PocType::Type2 => vk::video::STD_VIDEO_H264_POC_TYPE_2,
        };

        vk::video::StdVideoH264SequenceParameterSet {
            flags,
            profile_idc,
            level_idc,
            chroma_format_idc,
            seq_parameter_set_id: sps.seq_parameter_set_id as u8,
            bit_depth_luma_minus8: sps.bit_depth_luma_minus8 as u8,
            bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8 as u8,
            log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4 as u8,
            pic_order_cnt_type: poc_type,
            offset_for_non_ref_pic: sps.offset_for_non_ref_pic,
            offset_for_top_to_bottom_field: sps.offset_for_top_to_bottom_field,
            log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4 as u8,
            num_ref_frames_in_pic_order_cnt_cycle: sps.num_ref_frames_in_pic_order_cnt_cycle,
            max_num_ref_frames: sps.max_num_ref_frames as u8,
            reserved1: 0,
            pic_width_in_mbs_minus1: sps.pic_width_in_mbs_minus1 as u32,
            pic_height_in_map_units_minus1: sps.pic_height_in_map_units_minus1 as u32,
            frame_crop_left_offset: sps.frame_crop_left_offset as u32,
            frame_crop_right_offset: sps.frame_crop_right_offset as u32,
            frame_crop_top_offset: sps.frame_crop_top_offset as u32,
            frame_crop_bottom_offset: sps.frame_crop_bottom_offset as u32,
            reserved2: 0,
            pOffsetForRefFrame: if sps.num_ref_frames_in_pic_order_cnt_cycle > 0 {
                sps.offset_for_ref_frame.as_ptr()
            } else {
                ptr::null()
            },
            pScalingLists: ptr::null(),
            pSequenceParameterSetVui: ptr::null(),
        }
    }

    /// Build StdVideoH264PictureParameterSet from the ported parser's PPS.
    unsafe fn build_std_pps_from_parsed(
        pps: &H264Pps,
    ) -> vk::video::StdVideoH264PictureParameterSet {
        let mut flags: vk::video::StdVideoH264PpsFlags = std::mem::zeroed();
        flags.set_entropy_coding_mode_flag(pps.flags.entropy_coding_mode_flag as u32);
        flags.set_deblocking_filter_control_present_flag(pps.flags.deblocking_filter_control_present_flag as u32);
        flags.set_weighted_pred_flag(pps.flags.weighted_pred_flag as u32);
        flags.set_constrained_intra_pred_flag(pps.flags.constrained_intra_pred_flag as u32);
        flags.set_redundant_pic_cnt_present_flag(pps.flags.redundant_pic_cnt_present_flag as u32);
        flags.set_transform_8x8_mode_flag(pps.flags.transform_8x8_mode_flag as u32);
        flags.set_pic_scaling_matrix_present_flag(pps.flags.pic_scaling_matrix_present_flag as u32);
        flags.set_bottom_field_pic_order_in_frame_present_flag(
            pps.flags.bottom_field_pic_order_in_frame_present_flag as u32,
        );

        vk::video::StdVideoH264PictureParameterSet {
            flags,
            seq_parameter_set_id: pps.seq_parameter_set_id,
            pic_parameter_set_id: pps.pic_parameter_set_id,
            num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
            weighted_bipred_idc: match pps.weighted_bipred_idc {
                0 => vk::video::STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
                1 => vk::video::STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_EXPLICIT,
                2 => vk::video::STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_IMPLICIT,
                _ => vk::video::STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
            },
            pic_init_qp_minus26: pps.pic_init_qp_minus26,
            pic_init_qs_minus26: pps.pic_init_qs_minus26,
            chroma_qp_index_offset: pps.chroma_qp_index_offset,
            second_chroma_qp_index_offset: pps.second_chroma_qp_index_offset,
            pScalingLists: ptr::null(),
        }
    }

    /// Build a default PPS when no PPS NAL has been received yet.
    unsafe fn build_std_pps_default(
        sps_id: u8,
    ) -> vk::video::StdVideoH264PictureParameterSet {
        let mut flags: vk::video::StdVideoH264PpsFlags = std::mem::zeroed();
        flags.set_deblocking_filter_control_present_flag(1);
        flags.set_entropy_coding_mode_flag(1);

        vk::video::StdVideoH264PictureParameterSet {
            flags,
            seq_parameter_set_id: sps_id,
            pic_parameter_set_id: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_bipred_idc: vk::video::STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
            pic_init_qp_minus26: 0,
            pic_init_qs_minus26: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pScalingLists: ptr::null(),
        }
    }

    /// Create H.264 session parameters on VkVideoDecoder's video session.
    pub(crate) fn create_session_params_h264_vk(&mut self) -> Result<(), VideoError> {
        let vk_dec = self.vk_decoder.as_mut().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder not created for H.264".into())
        })?;
        let session = vk_dec.video_session_handle().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder has no video session".into())
        })?;

        let parser = self.h264_parser.as_ref().ok_or_else(|| {
            VideoError::BitstreamError("H.264 parser not initialized".into())
        })?;
        let sps = parser.sps.as_ref().ok_or_else(|| {
            VideoError::BitstreamError("No active H.264 SPS".into())
        })?;
        let pps = parser.pps.as_ref().or_else(|| {
            parser.ppss.iter().flatten().next()
        });

        let h264_sps = unsafe { Self::build_std_sps_from_parsed(sps) };
        let h264_pps = if let Some(pps) = pps {
            unsafe { Self::build_std_pps_from_parsed(pps) }
        } else {
            unsafe { Self::build_std_pps_default(sps.seq_parameter_set_id as u8) }
        };

        let h264_add_info = vk::VideoDecodeH264SessionParametersAddInfoKHR::builder()
            .std_sp_ss(std::slice::from_ref(&h264_sps))
            .std_pp_ss(std::slice::from_ref(&h264_pps));

        let mut h264_params = vk::VideoDecodeH264SessionParametersCreateInfoKHR::builder()
            .max_std_sps_count(32)
            .max_std_pps_count(256)
            .parameters_add_info(&h264_add_info);

        let params_create = vk::VideoSessionParametersCreateInfoKHR::builder()
            .video_session(session)
            .push_next(&mut h264_params);

        // Wait for in-flight command buffers before destroying old params
        // (VUID-vkDestroyVideoSessionParametersKHR-videoSessionParameters-07212)
        let old = vk_dec.session_parameters();
        if old != vk::VideoSessionParametersKHR::null() {
            unsafe {
                self.ctx.device().device_wait_idle().map_err(VideoError::from)?;
                self.ctx.device().destroy_video_session_parameters_khr(old, None);
            }
        }

        let new_params = unsafe {
            self.ctx.device()
                .create_video_session_parameters_khr(&params_create, None)
                .map_err(VideoError::from)?
        };
        vk_dec.set_session_parameters(new_params);

        debug!("H.264 session parameters created on VkVideoDecoder");
        Ok(())
    }

    // ------------------------------------------------------------------
    // H.265 session parameters
    // ------------------------------------------------------------------

    pub(crate) fn create_session_params_h265(&mut self) -> Result<(), VideoError> {
        let width = self.sps_width;
        let height = self.sps_height;

        // Get parsed VPS/SPS/PPS from the parser if available
        let parsed_vps = self
            .h265_parser
            .as_ref()
            .and_then(|p| p.active_vps.as_ref());
        let parsed_sps = self
            .h265_parser
            .as_ref()
            .and_then(|p| p.active_sps[0].as_ref());
        let parsed_pps = self
            .h265_parser
            .as_ref()
            .and_then(|p| p.active_pps[0].as_ref());

        // Build VPS from parsed data
        let mut vps_flags: vk::video::StdVideoH265VpsFlags = unsafe { std::mem::zeroed() };
        if let Some(v) = parsed_vps {
            if v.base_flags.vps_temporal_id_nesting_flag {
                vps_flags.set_vps_temporal_id_nesting_flag(1);
            }
            if v.base_flags.vps_sub_layer_ordering_info_present_flag {
                vps_flags.set_vps_sub_layer_ordering_info_present_flag(1);
            }
            if v.base_flags.vps_timing_info_present_flag {
                vps_flags.set_vps_timing_info_present_flag(1);
            }
            if v.base_flags.vps_poc_proportional_to_timing_flag {
                vps_flags.set_vps_poc_proportional_to_timing_flag(1);
            }
        }
        // VPS DecPicBufMgr (from parsed VPS, falls back to SPS values)
        let max_dpb_minus1 = parsed_sps.map_or(3u8, |s| {
            s.max_dec_pic_buffering.saturating_sub(1).max(1)
        });
        let max_reorder = parsed_sps.map_or(0u8, |s| s.max_num_reorder_pics);
        let vps_dec_pic_buf_mgr = if let Some(v) = parsed_vps {
            vk::video::StdVideoH265DecPicBufMgr {
                max_latency_increase_plus1: {
                    let mut arr = [0u32; 7];
                    for (i, val) in v.dec_pic_buf_mgr.max_latency_increase_plus1.iter().enumerate() {
                        arr[i] = *val as u32;
                    }
                    arr
                },
                max_dec_pic_buffering_minus1: v.dec_pic_buf_mgr.max_dec_pic_buffering_minus1,
                max_num_reorder_pics: v.dec_pic_buf_mgr.max_num_reorder_pics,
            }
        } else {
            vk::video::StdVideoH265DecPicBufMgr {
                max_latency_increase_plus1: [0; 7],
                max_dec_pic_buffering_minus1: [max_dpb_minus1; 7],
                max_num_reorder_pics: [max_reorder; 7],
            }
        };
        let dec_pic_buf_mgr = vk::video::StdVideoH265DecPicBufMgr {
            max_latency_increase_plus1: parsed_sps.map_or([0u32; 7], |s| {
                let mut arr = [0u32; 7];
                for (i, v) in s.dec_pic_buf_mgr.max_latency_increase_plus1.iter().enumerate() {
                    arr[i] = *v as u32;
                }
                arr
            }),
            max_dec_pic_buffering_minus1: parsed_sps.map_or([max_dpb_minus1; 7], |s| {
                s.dec_pic_buf_mgr.max_dec_pic_buffering_minus1
            }),
            max_num_reorder_pics: parsed_sps.map_or([max_reorder; 7], |s| {
                s.dec_pic_buf_mgr.max_num_reorder_pics
            }),
        };

        let general_profile_idc = parsed_sps.map_or(
            vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN,
            |s| match s.profile_tier_level.general_profile_idc {
                2 => vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN_10,
                _ => vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN,
            },
        );
        let general_level_idc = parsed_sps.map_or(
            vk::video::STD_VIDEO_H265_LEVEL_IDC_4_1,
            |s| match s.profile_tier_level.general_level_idc {
                h265dec::H265LevelIdc::Level1_0 => vk::video::STD_VIDEO_H265_LEVEL_IDC_1_0,
                h265dec::H265LevelIdc::Level2_0 => vk::video::STD_VIDEO_H265_LEVEL_IDC_2_0,
                h265dec::H265LevelIdc::Level2_1 => vk::video::STD_VIDEO_H265_LEVEL_IDC_2_1,
                h265dec::H265LevelIdc::Level3_0 => vk::video::STD_VIDEO_H265_LEVEL_IDC_3_0,
                h265dec::H265LevelIdc::Level3_1 => vk::video::STD_VIDEO_H265_LEVEL_IDC_3_1,
                h265dec::H265LevelIdc::Level4_0 => vk::video::STD_VIDEO_H265_LEVEL_IDC_4_0,
                h265dec::H265LevelIdc::Level4_1 => vk::video::STD_VIDEO_H265_LEVEL_IDC_4_1,
                h265dec::H265LevelIdc::Level5_0 => vk::video::STD_VIDEO_H265_LEVEL_IDC_5_0,
                h265dec::H265LevelIdc::Level5_1 => vk::video::STD_VIDEO_H265_LEVEL_IDC_5_1,
                h265dec::H265LevelIdc::Level5_2 => vk::video::STD_VIDEO_H265_LEVEL_IDC_5_2,
                h265dec::H265LevelIdc::Level6_0 => vk::video::STD_VIDEO_H265_LEVEL_IDC_6_0,
                h265dec::H265LevelIdc::Level6_1 => vk::video::STD_VIDEO_H265_LEVEL_IDC_6_1,
                h265dec::H265LevelIdc::Level6_2 => vk::video::STD_VIDEO_H265_LEVEL_IDC_6_2,
                _ => vk::video::STD_VIDEO_H265_LEVEL_IDC_4_1,
            },
        );
        let profile_tier_level = vk::video::StdVideoH265ProfileTierLevel {
            flags: unsafe { std::mem::zeroed() },
            general_profile_idc,
            general_level_idc,
        };
        let h265_vps = vk::video::StdVideoH265VideoParameterSet {
            flags: vps_flags,
            vps_video_parameter_set_id: parsed_vps
                .map_or(0, |v| v.vps_video_parameter_set_id as u8),
            vps_max_sub_layers_minus1: parsed_vps
                .map_or(0, |v| v.vps_max_sub_layers_minus1 as u8),
            reserved1: 0,
            reserved2: 0,
            vps_num_units_in_tick: parsed_vps
                .map_or(1, |v| v.vps_num_units_in_tick),
            vps_time_scale: parsed_vps
                .map_or(30, |v| v.vps_time_scale),
            vps_num_ticks_poc_diff_one_minus1: parsed_vps
                .map_or(0, |v| v.vps_num_ticks_poc_diff_one_minus1),
            reserved3: 0,
            pDecPicBufMgr: &vps_dec_pic_buf_mgr,
            pHrdParameters: ptr::null(),
            pProfileTierLevel: &profile_tier_level,
        };

        // Build SPS from parsed data
        let log2_min_cb = parsed_sps.map_or(0u8, |s| s.log2_min_luma_coding_block_size_minus3);
        let log2_diff_cb =
            parsed_sps.map_or(2u8, |s| s.log2_diff_max_min_luma_coding_block_size);
        let log2_ctb = log2_min_cb as u32 + 3 + log2_diff_cb as u32;
        let ctb_size = 1u32 << log2_ctb;
        let aligned_w = (width + ctb_size - 1) & !(ctb_size - 1);
        let aligned_h = (height + ctb_size - 1) & !(ctb_size - 1);

        let mut sps_flags: vk::video::StdVideoH265SpsFlags = unsafe { std::mem::zeroed() };
        if let Some(s) = parsed_sps {
            if s.flags.sps_temporal_id_nesting_flag {
                sps_flags.set_sps_temporal_id_nesting_flag(1);
            }
            if s.flags.conformance_window_flag {
                sps_flags.set_conformance_window_flag(1);
            }
            if s.flags.amp_enabled_flag {
                sps_flags.set_amp_enabled_flag(1);
            }
            if s.flags.sample_adaptive_offset_enabled_flag {
                sps_flags.set_sample_adaptive_offset_enabled_flag(1);
            }
            if s.flags.strong_intra_smoothing_enabled_flag {
                sps_flags.set_strong_intra_smoothing_enabled_flag(1);
            }
            if s.flags.sps_temporal_mvp_enabled_flag {
                sps_flags.set_sps_temporal_mvp_enabled_flag(1);
            }
            if s.flags.long_term_ref_pics_present_flag {
                sps_flags.set_long_term_ref_pics_present_flag(1);
            }
            if s.flags.scaling_list_enabled_flag {
                sps_flags.set_scaling_list_enabled_flag(1);
            }
        } else {
            sps_flags.set_sps_temporal_id_nesting_flag(1);
            if aligned_w != width || aligned_h != height {
                sps_flags.set_conformance_window_flag(1);
            }
            sps_flags.set_amp_enabled_flag(1);
            sps_flags.set_sample_adaptive_offset_enabled_flag(1);
            sps_flags.set_strong_intra_smoothing_enabled_flag(1);
        }

        // Build StdVideoH265ShortTermRefPicSet array from parsed SPS
        let std_strps_vec: Vec<vk::video::StdVideoH265ShortTermRefPicSet> = parsed_sps
            .map_or_else(Vec::new, |s| {
                s.std_short_term_ref_pic_sets
                    .iter()
                    .map(|st| {
                        let mut flags: vk::video::StdVideoH265ShortTermRefPicSetFlags =
                            unsafe { std::mem::zeroed() };
                        if st.flags.inter_ref_pic_set_prediction_flag {
                            flags.set_inter_ref_pic_set_prediction_flag(1);
                        }
                        if st.flags.delta_rps_sign {
                            flags.set_delta_rps_sign(1);
                        }
                        vk::video::StdVideoH265ShortTermRefPicSet {
                            flags,
                            delta_idx_minus1: st.delta_idx_minus1,
                            use_delta_flag: st.use_delta_flag as u16,
                            abs_delta_rps_minus1: st.abs_delta_rps_minus1 as u16,
                            used_by_curr_pic_flag: st.used_by_curr_pic_flag as u16,
                            used_by_curr_pic_s0_flag: st.used_by_curr_pic_s0_flag as u16,
                            used_by_curr_pic_s1_flag: st.used_by_curr_pic_s1_flag as u16,
                            reserved1: 0,
                            reserved2: 0,
                            reserved3: 0,
                            num_negative_pics: st.num_negative_pics as u8,
                            num_positive_pics: st.num_positive_pics as u8,
                            delta_poc_s0_minus1: st.delta_poc_s0_minus1,
                            delta_poc_s1_minus1: {
                                let mut arr = [0u16; 16];
                                for (i, v) in st.delta_poc_s1_minus1.iter().enumerate() {
                                    arr[i] = *v as u16;
                                }
                                arr
                            },
                        }
                    })
                    .collect()
            });

        let num_strps = parsed_sps.map_or(0u8, |s| s.num_short_term_ref_pic_sets);
        let p_short_term_rps: *const vk::video::StdVideoH265ShortTermRefPicSet =
            if std_strps_vec.is_empty() {
                ptr::null()
            } else {
                std_strps_vec.as_ptr()
            };

        let h265_sps = vk::video::StdVideoH265SequenceParameterSet {
            flags: sps_flags,
            chroma_format_idc: parsed_sps.map_or(
                vk::video::STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
                |s| match s.chroma_format_idc {
                    0 => vk::video::STD_VIDEO_H265_CHROMA_FORMAT_IDC_MONOCHROME,
                    2 => vk::video::STD_VIDEO_H265_CHROMA_FORMAT_IDC_422,
                    3 => vk::video::STD_VIDEO_H265_CHROMA_FORMAT_IDC_444,
                    _ => vk::video::STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
                },
            ),
            pic_width_in_luma_samples: width,
            pic_height_in_luma_samples: height,
            sps_video_parameter_set_id: parsed_sps.map_or(0, |s| s.sps_video_parameter_set_id),
            sps_max_sub_layers_minus1: parsed_sps.map_or(0, |s| s.sps_max_sub_layers_minus1),
            sps_seq_parameter_set_id: parsed_sps.map_or(0, |s| s.sps_seq_parameter_set_id),
            bit_depth_luma_minus8: parsed_sps.map_or(0, |s| s.bit_depth_luma_minus8),
            bit_depth_chroma_minus8: parsed_sps.map_or(0, |s| s.bit_depth_chroma_minus8),
            log2_max_pic_order_cnt_lsb_minus4: parsed_sps
                .map_or(4, |s| s.log2_max_pic_order_cnt_lsb_minus4),
            log2_min_luma_coding_block_size_minus3: log2_min_cb,
            log2_diff_max_min_luma_coding_block_size: log2_diff_cb,
            log2_min_luma_transform_block_size_minus2: parsed_sps
                .map_or(0, |s| s.log2_min_luma_transform_block_size_minus2),
            log2_diff_max_min_luma_transform_block_size: parsed_sps
                .map_or(3, |s| s.log2_diff_max_min_luma_transform_block_size),
            max_transform_hierarchy_depth_inter: parsed_sps
                .map_or(2, |s| s.max_transform_hierarchy_depth_inter),
            max_transform_hierarchy_depth_intra: parsed_sps
                .map_or(2, |s| s.max_transform_hierarchy_depth_intra),
            num_short_term_ref_pic_sets: num_strps,
            num_long_term_ref_pics_sps: parsed_sps.map_or(0, |s| s.num_long_term_ref_pics_sps),
            pcm_sample_bit_depth_luma_minus1: parsed_sps
                .map_or(0, |s| s.pcm_sample_bit_depth_luma_minus1),
            pcm_sample_bit_depth_chroma_minus1: parsed_sps
                .map_or(0, |s| s.pcm_sample_bit_depth_chroma_minus1),
            log2_min_pcm_luma_coding_block_size_minus3: parsed_sps
                .map_or(0, |s| s.log2_min_pcm_luma_coding_block_size_minus3),
            log2_diff_max_min_pcm_luma_coding_block_size: parsed_sps
                .map_or(0, |s| s.log2_diff_max_min_pcm_luma_coding_block_size),
            reserved1: 0,
            reserved2: 0,
            palette_max_size: 0,
            delta_palette_max_predictor_size: 0,
            motion_vector_resolution_control_idc: 0,
            sps_num_palette_predictor_initializers_minus1: 0,
            conf_win_left_offset: parsed_sps.map_or(0, |s| s.conf_win_left_offset as u32),
            conf_win_right_offset: parsed_sps
                .map_or((aligned_w - width) / 2, |s| s.conf_win_right_offset as u32),
            conf_win_top_offset: parsed_sps.map_or(0, |s| s.conf_win_top_offset as u32),
            conf_win_bottom_offset: parsed_sps
                .map_or((aligned_h - height) / 2, |s| s.conf_win_bottom_offset as u32),
            pProfileTierLevel: &profile_tier_level,
            pDecPicBufMgr: &dec_pic_buf_mgr,
            pScalingLists: ptr::null(),
            pShortTermRefPicSet: p_short_term_rps,
            pLongTermRefPicsSps: ptr::null(),
            pSequenceParameterSetVui: ptr::null(),
            pPredictorPaletteEntries: ptr::null(),
        };

        // Build PPS from parsed data
        let mut pps_flags: vk::video::StdVideoH265PpsFlags = unsafe { std::mem::zeroed() };
        if let Some(p) = parsed_pps {
            if p.flags.cabac_init_present_flag {
                pps_flags.set_cabac_init_present_flag(1);
            }
            if p.flags.uniform_spacing_flag {
                pps_flags.set_uniform_spacing_flag(1);
            }
            if p.flags.pps_loop_filter_across_slices_enabled_flag {
                pps_flags.set_pps_loop_filter_across_slices_enabled_flag(1);
            }
            if p.flags.deblocking_filter_control_present_flag {
                pps_flags.set_deblocking_filter_control_present_flag(1);
            }
            if p.flags.transform_skip_enabled_flag {
                pps_flags.set_transform_skip_enabled_flag(1);
            }
            if p.flags.cu_qp_delta_enabled_flag {
                pps_flags.set_cu_qp_delta_enabled_flag(1);
            }
            if p.flags.weighted_pred_flag {
                pps_flags.set_weighted_pred_flag(1);
            }
            if p.flags.weighted_bipred_flag {
                pps_flags.set_weighted_bipred_flag(1);
            }
            if p.flags.tiles_enabled_flag {
                pps_flags.set_tiles_enabled_flag(1);
            }
            if p.flags.entropy_coding_sync_enabled_flag {
                pps_flags.set_entropy_coding_sync_enabled_flag(1);
            }
            if p.flags.loop_filter_across_tiles_enabled_flag {
                pps_flags.set_loop_filter_across_tiles_enabled_flag(1);
            }
            if p.flags.sign_data_hiding_enabled_flag {
                pps_flags.set_sign_data_hiding_enabled_flag(1);
            }
        } else {
            pps_flags.set_cabac_init_present_flag(1);
            pps_flags.set_uniform_spacing_flag(1);
            pps_flags.set_pps_loop_filter_across_slices_enabled_flag(1);
            pps_flags.set_deblocking_filter_control_present_flag(1);
        }

        let h265_pps = vk::video::StdVideoH265PictureParameterSet {
            flags: pps_flags,
            pps_pic_parameter_set_id: parsed_pps.map_or(0, |p| p.pps_pic_parameter_set_id),
            pps_seq_parameter_set_id: parsed_pps.map_or(0, |p| p.pps_seq_parameter_set_id),
            sps_video_parameter_set_id: parsed_pps.map_or(0, |p| p.sps_video_parameter_set_id),
            num_extra_slice_header_bits: parsed_pps.map_or(0, |p| p.num_extra_slice_header_bits),
            num_ref_idx_l0_default_active_minus1: parsed_pps
                .map_or(0, |p| p.num_ref_idx_l0_default_active_minus1),
            num_ref_idx_l1_default_active_minus1: parsed_pps
                .map_or(0, |p| p.num_ref_idx_l1_default_active_minus1),
            init_qp_minus26: parsed_pps.map_or(0, |p| p.init_qp_minus26),
            diff_cu_qp_delta_depth: parsed_pps.map_or(0, |p| p.diff_cu_qp_delta_depth),
            pps_cb_qp_offset: parsed_pps.map_or(0, |p| p.pps_cb_qp_offset),
            pps_cr_qp_offset: parsed_pps.map_or(0, |p| p.pps_cr_qp_offset),
            pps_beta_offset_div2: parsed_pps.map_or(0, |p| p.pps_beta_offset_div2),
            pps_tc_offset_div2: parsed_pps.map_or(0, |p| p.pps_tc_offset_div2),
            diff_cu_chroma_qp_offset_depth: parsed_pps
                .map_or(0, |p| p.diff_cu_chroma_qp_offset_depth),
            chroma_qp_offset_list_len_minus1: parsed_pps
                .map_or(0, |p| p.chroma_qp_offset_list_len_minus1),
            cb_qp_offset_list: parsed_pps.map_or([0; 6], |p| p.cb_qp_offset_list),
            cr_qp_offset_list: parsed_pps.map_or([0; 6], |p| p.cr_qp_offset_list),
            log2_parallel_merge_level_minus2: parsed_pps
                .map_or(0, |p| p.log2_parallel_merge_level_minus2),
            log2_max_transform_skip_block_size_minus2: parsed_pps
                .map_or(0, |p| p.log2_max_transform_skip_block_size_minus2),
            log2_sao_offset_scale_luma: parsed_pps.map_or(0, |p| p.log2_sao_offset_scale_luma),
            log2_sao_offset_scale_chroma: parsed_pps
                .map_or(0, |p| p.log2_sao_offset_scale_chroma),
            pps_act_y_qp_offset_plus5: 0,
            pps_act_cb_qp_offset_plus5: 0,
            pps_act_cr_qp_offset_plus3: 0,
            pps_num_palette_predictor_initializers: 0,
            luma_bit_depth_entry_minus8: 0,
            chroma_bit_depth_entry_minus8: 0,
            num_tile_columns_minus1: parsed_pps.map_or(0, |p| p.num_tile_columns_minus1),
            num_tile_rows_minus1: parsed_pps.map_or(0, |p| p.num_tile_rows_minus1),
            reserved1: 0,
            reserved2: 0,
            column_width_minus1: parsed_pps.map_or([0; 19], |p| {
                let mut arr = [0u16; 19];
                for (i, v) in p.column_width_minus1.iter().take(19).enumerate() {
                    arr[i] = *v;
                }
                arr
            }),
            row_height_minus1: parsed_pps.map_or([0; 21], |p| {
                let mut arr = [0u16; 21];
                for (i, v) in p.row_height_minus1.iter().take(21).enumerate() {
                    arr[i] = *v;
                }
                arr
            }),
            reserved3: 0,
            pScalingLists: ptr::null(),
            pPredictorPaletteEntries: ptr::null(),
        };

        let h265_add_info = vk::VideoDecodeH265SessionParametersAddInfoKHR::builder()
            .std_vp_ss(std::slice::from_ref(&h265_vps))
            .std_sp_ss(std::slice::from_ref(&h265_sps))
            .std_pp_ss(std::slice::from_ref(&h265_pps));

        // Create session params directly on VkVideoDecoder's session
        let vk_dec = self.vk_decoder.as_mut().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder not created for H.265".into())
        })?;
        let session = vk_dec.video_session_handle().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder has no video session".into())
        })?;

        // Wait for in-flight command buffers before destroying old params
        // (VUID-vkDestroyVideoSessionParametersKHR-videoSessionParameters-07212)
        let old = vk_dec.session_parameters();
        if old != vk::VideoSessionParametersKHR::null() {
            unsafe {
                self.ctx.device().device_wait_idle().map_err(VideoError::from)?;
                self.ctx.device().destroy_video_session_parameters_khr(old, None);
            }
        }

        let mut h265_params = vk::VideoDecodeH265SessionParametersCreateInfoKHR::builder()
            .max_std_vps_count(16)
            .max_std_sps_count(32)
            .max_std_pps_count(256)
            .parameters_add_info(&h265_add_info);

        let params_create = vk::VideoSessionParametersCreateInfoKHR::builder()
            .video_session(session)
            .push_next(&mut h265_params);

        let vk_params = unsafe {
            self.ctx.device()
                .create_video_session_parameters_khr(&params_create, None)
                .map_err(VideoError::from)?
        };
        vk_dec.set_session_parameters(vk_params);
        debug!("H265 session parameters created on VkVideoDecoder (from parsed SPS/PPS)");

        Ok(())
    }
}
