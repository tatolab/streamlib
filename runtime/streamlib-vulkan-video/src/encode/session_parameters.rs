// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The encoder's codec session parameters: the H.264 SPS and PPS, or the H.265 VPS, SPS and PPS,
//! with the VUI the SPS points at, created against the video session.

use std::ptr;
use std::sync::Arc;
use vulkanalia::vk;

use crate::vk_video_encoder::vk_encoder_config_h264::{
    level_index_to_std_video, profile_idc_to_std_video,
};
use crate::{VideoError, VideoResult};

use super::SimpleEncoder;

use super::config::{EncodeConfig, RateControlMode};

/// The `chroma_sample_loc_type_*_field` value both codecs write into the SPS VUI.
///
/// Bitstream contract: the VUI never sets `chroma_loc_info_present_flag`, so the type
/// never reaches the wire as a signalled value — but H.264 §E.2.1 / H.265 §E.3.1 have
/// decoders infer exactly this type from the absent flag, so written and inferred
/// agree, and both describe what `rgb_to_nv12.comp` produces. The decode side
/// reconstructs at the siting this names; moving it without moving `nv12_to_rgb`'s
/// offsets sites chroma against our own stream.
pub const SPS_VUI_CHROMA_SAMPLE_LOC_TYPE: u8 = 0;

impl SimpleEncoder {
    /// Create codec-specific session parameters (SPS/PPS for H.264,
    /// VPS/SPS/PPS for H.265). Returns an `Arc` parented to the
    /// given session — destruction is automatic when the Arc drops.
    ///
    /// # Safety
    ///
    /// Caller must ensure `profile_info` is live.
    pub(super) unsafe fn create_session_parameters(
        &self,
        session: &Arc<crate::VulkanVideoSession>,
        config: &EncodeConfig,
        _profile_info: &vk::VideoProfileInfoKHR,
        h265_ctb_log2_size: u32,
        aligned_w: u32,
        aligned_h: u32,
        quality_level: u32,
    ) -> VideoResult<Arc<crate::VulkanVideoSessionParameters>> {
        unsafe {
            // H.264: SPS + PPS parameter sets, plus the VUI the SPS points
            // at via raw pointer. All declared in outer scope so the
            // single-element arrays we wrap below stay valid through
            // `VulkanVideoSessionParameters::new`.
            let h264_sps;
            let h264_pps;
            let h264_sps_vui;
            let h264_sps_array: [vk::video::StdVideoH264SequenceParameterSet; 1];
            let h264_pps_array: [vk::video::StdVideoH264PictureParameterSet; 1];

            // H.265: VPS + SPS + PPS plus the auxiliary structs the VPS / SPS
            // point at via raw pointer (DPB management, profile-tier-level,
            // optional VUI).
            let h265_vps;
            let h265_sps;
            let h265_pps;
            let h265_sps_vui: Option<vk::video::StdVideoH265SequenceParameterSetVui>;
            let h265_dec_pic_buf_mgr;
            let h265_profile_tier_level;
            let h265_vps_array: [vk::video::StdVideoH265VideoParameterSet; 1];
            let h265_sps_array: [vk::video::StdVideoH265SequenceParameterSet; 1];
            let h265_pps_array: [vk::video::StdVideoH265PictureParameterSet; 1];

            // Active typed add-info selected below.

            let add_info: crate::VideoSessionParametersAddInfo = if self.codec_flag
                == vk::VideoCodecOperationFlagsKHR::ENCODE_H264
            {
                // Build SPS/PPS from the ported EncoderConfigH264 state instead of
                // hard-coding values inline.
                let enc = self
                    .h264_encoder
                    .as_ref()
                    .expect("h264_encoder must be initialized before create_session_parameters");
                let cfg = self
                    .h264_config
                    .as_ref()
                    .expect("h264_config must be initialized before create_session_parameters");
                let state = cfg.build_sps_pps_state(
                    aligned_w,
                    aligned_h,
                    config.width,
                    config.height,
                    enc.max_num_ref_frames,
                    enc.log2_max_frame_num_minus4,
                    enc.log2_max_pic_order_cnt_lsb_minus4,
                );

                // H.264 timing is field-based, two ticks per frame:
                // num_units_in_tick = framerate_denominator,
                // time_scale = framerate_numerator * 2.
                let mut vui_flags: vk::video::StdVideoH264SpsVuiFlags = std::mem::zeroed();
                vui_flags.set_timing_info_present_flag(1);
                vui_flags.set_fixed_frame_rate_flag(1);
                let color_vui = config.color_vui;
                if let Some(cv) = color_vui
                    && cv.is_video_signal_type_block_needed()
                {
                    vui_flags.set_video_signal_type_present_flag(1);
                    vui_flags.set_video_full_range_flag(cv.full_range_bit());
                    if cv.is_colour_description_block_needed() {
                        vui_flags.set_color_description_present_flag(1);
                    }
                }
                // Decoders skip these fields entirely when
                // `colour_description_present_flag = 0`, but emit H.273
                // "Unspecified" (2) rather than "Reserved" (0) for
                // spec-symmetry — the fields are part of the same byte
                // budget the per-axis `_byte()` accessors already honor.
                let unspec = streamlib_consumer_rhi::H273_UNSPECIFIED;
                let (vui_primaries, vui_transfer, vui_matrix) = color_vui
                    .map(|cv| (cv.primaries_byte(), cv.transfer_byte(), cv.matrix_byte()))
                    .unwrap_or((unspec, unspec, unspec));

                h264_sps_vui = vk::video::StdVideoH264SequenceParameterSetVui {
                    flags: vui_flags,
                    aspect_ratio_idc: vk::video::StdVideoH264AspectRatioIdc(0),
                    sar_width: 0,
                    sar_height: 0,
                    video_format: 5, // "Unspecified" per H.264 Annex E
                    colour_primaries: vui_primaries,
                    transfer_characteristics: vui_transfer,
                    matrix_coefficients: vui_matrix,
                    num_units_in_tick: config.framerate_denominator,
                    time_scale: config.framerate_numerator.saturating_mul(2),
                    max_num_reorder_frames: 0,
                    max_dec_frame_buffering: 0,
                    chroma_sample_loc_type_top_field: SPS_VUI_CHROMA_SAMPLE_LOC_TYPE,
                    chroma_sample_loc_type_bottom_field: SPS_VUI_CHROMA_SAMPLE_LOC_TYPE,
                    reserved1: 0,
                    pHrdParameters: ptr::null(),
                };

                let mut sps_flags: vk::video::StdVideoH264SpsFlags = std::mem::zeroed();
                sps_flags.set_direct_8x8_inference_flag(
                    if state.sps_info.direct_8x8_inference_flag {
                        1
                    } else {
                        0
                    },
                );
                sps_flags.set_frame_mbs_only_flag(if state.sps_info.frame_mbs_only_flag {
                    1
                } else {
                    0
                });
                sps_flags.set_frame_cropping_flag(if state.sps_info.frame_cropping_flag {
                    1
                } else {
                    0
                });
                sps_flags.set_vui_parameters_present_flag(1);

                h264_sps = vk::video::StdVideoH264SequenceParameterSet {
                    flags: sps_flags,
                    profile_idc: profile_idc_to_std_video(state.sps_info.profile_idc),
                    level_idc: level_index_to_std_video(state.sps_info.level_idc),
                    chroma_format_idc: vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
                    seq_parameter_set_id: state.sps_info.seq_parameter_set_id,
                    bit_depth_luma_minus8: 0,
                    bit_depth_chroma_minus8: 0,
                    log2_max_frame_num_minus4: state.sps_info.log2_max_frame_num_minus4,
                    pic_order_cnt_type: vk::video::STD_VIDEO_H264_POC_TYPE_0,
                    offset_for_non_ref_pic: 0,
                    offset_for_top_to_bottom_field: 0,
                    log2_max_pic_order_cnt_lsb_minus4: state
                        .sps_info
                        .log2_max_pic_order_cnt_lsb_minus4,
                    num_ref_frames_in_pic_order_cnt_cycle: 0,
                    max_num_ref_frames: state.sps_info.max_num_ref_frames,
                    reserved1: 0,
                    pic_width_in_mbs_minus1: state.sps_info.pic_width_in_mbs_minus1,
                    pic_height_in_map_units_minus1: state.sps_info.pic_height_in_map_units_minus1,
                    frame_crop_left_offset: 0,
                    frame_crop_right_offset: state.sps_info.frame_crop_right_offset,
                    frame_crop_top_offset: 0,
                    frame_crop_bottom_offset: state.sps_info.frame_crop_bottom_offset,
                    reserved2: 0,
                    pOffsetForRefFrame: ptr::null(),
                    pScalingLists: ptr::null(),
                    pSequenceParameterSetVui: &h264_sps_vui,
                };

                let mut pps_flags: vk::video::StdVideoH264PpsFlags = std::mem::zeroed();
                pps_flags.set_transform_8x8_mode_flag(if state.pps_info.transform_8x8_mode_flag {
                    1
                } else {
                    0
                });
                pps_flags.set_deblocking_filter_control_present_flag(
                    if state.pps_info.deblocking_filter_control_present_flag {
                        1
                    } else {
                        0
                    },
                );
                pps_flags.set_entropy_coding_mode_flag(
                    if state.pps_info.entropy_coding_mode_flag {
                        1
                    } else {
                        0
                    },
                );

                h264_pps = vk::video::StdVideoH264PictureParameterSet {
                    flags: pps_flags,
                    seq_parameter_set_id: state.pps_info.seq_parameter_set_id,
                    pic_parameter_set_id: state.pps_info.pic_parameter_set_id,
                    num_ref_idx_l0_default_active_minus1: state
                        .pps_info
                        .num_ref_idx_l0_default_active_minus1,
                    num_ref_idx_l1_default_active_minus1: state
                        .pps_info
                        .num_ref_idx_l1_default_active_minus1,
                    weighted_bipred_idc: vk::video::STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
                    pic_init_qp_minus26: config.const_qp_intra as i8 - 26,
                    pic_init_qs_minus26: 0,
                    chroma_qp_index_offset: state.pps_info.chroma_qp_index_offset,
                    second_chroma_qp_index_offset: state.pps_info.second_chroma_qp_index_offset,
                    pScalingLists: ptr::null(),
                };

                h264_sps_array = [h264_sps];
                h264_pps_array = [h264_pps];
                crate::VideoSessionParametersAddInfo::EncodeH264 {
                    sps: &h264_sps_array,
                    pps: &h264_pps_array,
                    max_std_sps_count: 1,
                    max_std_pps_count: 1,
                }
            } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
                // --- VPS flags (matching C++ VkEncoderConfigH265.cpp lines 761-764) ---
                let mut vps_flags: vk::video::StdVideoH265VpsFlags = std::mem::zeroed();
                vps_flags.set_vps_temporal_id_nesting_flag(1);
                vps_flags.set_vps_sub_layer_ordering_info_present_flag(1); // C++ line 762

                h265_dec_pic_buf_mgr = vk::video::StdVideoH265DecPicBufMgr {
                    max_latency_increase_plus1: [0; 7],
                    max_dec_pic_buffering_minus1: [(config.max_dpb_slots + 1).min(16) as u8 - 1; 7],
                    max_num_reorder_pics: [0; 7],
                };

                // --- Profile Tier Level (C++ lines 642-651) ---
                // Use EncoderConfigH265-derived profile/level instead of hardcoded values.
                let mut ptl_flags: vk::video::StdVideoH265ProfileTierLevelFlags =
                    std::mem::zeroed();
                ptl_flags.set_general_progressive_source_flag(1); // C++ line 648
                ptl_flags.set_general_frame_only_constraint_flag(1); // C++ line 651
                if let Some(ref cfg) = self.h265_config
                    && cfg.general_tier_flag
                {
                    ptl_flags.set_general_tier_flag(1);
                }
                let (ptl_profile, ptl_level) = if let Some(ref cfg) = self.h265_config {
                    (
                        vk::video::StdVideoH265ProfileIdc(cfg.profile as i32),
                        vk::video::StdVideoH265LevelIdc(cfg.level_idc as i32),
                    )
                } else {
                    (
                        vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN,
                        vk::video::STD_VIDEO_H265_LEVEL_IDC_4_1,
                    )
                };
                h265_profile_tier_level = vk::video::StdVideoH265ProfileTierLevel {
                    flags: ptl_flags,
                    general_profile_idc: ptl_profile,
                    general_level_idc: ptl_level,
                };

                // --- VPS (C++ lines 760-773) ---
                h265_vps = vk::video::StdVideoH265VideoParameterSet {
                    flags: vps_flags,
                    vps_video_parameter_set_id: 0,
                    vps_max_sub_layers_minus1: 0,
                    reserved1: 0,
                    reserved2: 0,
                    // C++ uses 0 with vps_timing_info_present_flag=0 (lines 767-768)
                    vps_num_units_in_tick: 0,
                    vps_time_scale: 0,
                    vps_num_ticks_poc_diff_one_minus1: 0,
                    reserved3: 0,
                    pDecPicBufMgr: &h265_dec_pic_buf_mgr,
                    pHrdParameters: ptr::null(),
                    pProfileTierLevel: &h265_profile_tier_level,
                };

                // --- SPS VUI (color-only) ---
                // H.265 timing rides in the VPS (patched separately in
                // vui_patch.rs because NVIDIA's driver emits broken VPS
                // timing). The SPS VUI therefore carries color metadata
                // only.
                h265_sps_vui = match config.color_vui {
                    Some(cv) if cv.is_video_signal_type_block_needed() => {
                        let mut flags: vk::video::StdVideoH265SpsVuiFlags = std::mem::zeroed();
                        flags.set_video_signal_type_present_flag(1);
                        flags.set_video_full_range_flag(cv.full_range_bit());
                        if cv.is_colour_description_block_needed() {
                            flags.set_colour_description_present_flag(1);
                        }
                        Some(vk::video::StdVideoH265SequenceParameterSetVui {
                            flags,
                            aspect_ratio_idc: vk::video::StdVideoH265AspectRatioIdc(0),
                            sar_width: 0,
                            sar_height: 0,
                            video_format: 5, // "Unspecified" per H.265 Annex E
                            colour_primaries: cv.primaries_byte(),
                            transfer_characteristics: cv.transfer_byte(),
                            matrix_coeffs: cv.matrix_byte(),
                            chroma_sample_loc_type_top_field: SPS_VUI_CHROMA_SAMPLE_LOC_TYPE,
                            chroma_sample_loc_type_bottom_field: SPS_VUI_CHROMA_SAMPLE_LOC_TYPE,
                            reserved1: 0,
                            reserved2: 0,
                            def_disp_win_left_offset: 0,
                            def_disp_win_right_offset: 0,
                            def_disp_win_top_offset: 0,
                            def_disp_win_bottom_offset: 0,
                            vui_num_units_in_tick: 0,
                            vui_time_scale: 0,
                            vui_num_ticks_poc_diff_one_minus1: 0,
                            min_spatial_segmentation_idc: 0,
                            reserved3: 0,
                            max_bytes_per_pic_denom: 0,
                            max_bits_per_min_cu_denom: 0,
                            log2_max_mv_length_horizontal: 0,
                            log2_max_mv_length_vertical: 0,
                            pHrdParameters: ptr::null(),
                        })
                    }
                    _ => None,
                };

                // --- SPS flags (C++ lines 661-689) ---
                let mut sps_flags: vk::video::StdVideoH265SpsFlags = std::mem::zeroed();
                sps_flags.set_sps_temporal_id_nesting_flag(1); // C++ line 661
                sps_flags.set_conformance_window_flag(
                    if aligned_w != config.width || aligned_h != config.height {
                        1
                    } else {
                        0
                    },
                );
                sps_flags.set_sps_sub_layer_ordering_info_present_flag(1); // C++ line 663
                sps_flags.set_amp_enabled_flag(1); // C++ line 666
                sps_flags.set_sample_adaptive_offset_enabled_flag(1); // C++ line 667
                // strong_intra_smoothing_enabled_flag = 0 (C++ line 672) — already 0
                // sps_temporal_mvp_enabled_flag = 0 (C++ line 671) — already 0
                if h265_sps_vui.is_some() {
                    sps_flags.set_vui_parameters_present_flag(1);
                }

                // --- SPS (C++ lines 691-757) ---
                h265_sps = vk::video::StdVideoH265SequenceParameterSet {
                    flags: sps_flags,
                    chroma_format_idc: vk::video::STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
                    pic_width_in_luma_samples: aligned_w,
                    pic_height_in_luma_samples: aligned_h,
                    sps_video_parameter_set_id: 0,
                    sps_max_sub_layers_minus1: 0,
                    sps_seq_parameter_set_id: 0,
                    bit_depth_luma_minus8: 0,
                    bit_depth_chroma_minus8: 0,
                    log2_max_pic_order_cnt_lsb_minus4: 4,
                    log2_min_luma_coding_block_size_minus3: 0,
                    log2_diff_max_min_luma_coding_block_size: (h265_ctb_log2_size - 3) as u8,
                    log2_min_luma_transform_block_size_minus2: 0,
                    log2_diff_max_min_luma_transform_block_size: 3,
                    max_transform_hierarchy_depth_inter: 3,
                    max_transform_hierarchy_depth_intra: 3,
                    num_short_term_ref_pic_sets: 0,
                    num_long_term_ref_pics_sps: 0,
                    pcm_sample_bit_depth_luma_minus1: 0,
                    pcm_sample_bit_depth_chroma_minus1: 0,
                    log2_min_pcm_luma_coding_block_size_minus3: 0,
                    log2_diff_max_min_pcm_luma_coding_block_size: 0,
                    reserved1: 0,
                    reserved2: 0,
                    palette_max_size: 0,
                    delta_palette_max_predictor_size: 0,
                    motion_vector_resolution_control_idc: 0,
                    sps_num_palette_predictor_initializers_minus1: 0,
                    conf_win_left_offset: 0,
                    conf_win_right_offset: (aligned_w - config.width) / 2,
                    conf_win_top_offset: 0,
                    conf_win_bottom_offset: (aligned_h - config.height) / 2,
                    pProfileTierLevel: &h265_profile_tier_level,
                    pDecPicBufMgr: &h265_dec_pic_buf_mgr,
                    pScalingLists: ptr::null(),
                    pShortTermRefPicSet: ptr::null(),
                    pLongTermRefPicsSps: ptr::null(),
                    pSequenceParameterSetVui: h265_sps_vui
                        .as_ref()
                        .map(|v| v as *const _)
                        .unwrap_or(ptr::null()),
                    pPredictorPaletteEntries: ptr::null(),
                };

                let mut pps_flags: vk::video::StdVideoH265PpsFlags = std::mem::zeroed();
                pps_flags.set_cabac_init_present_flag(1);
                pps_flags.set_transform_skip_enabled_flag(1); // C++ reference line 780
                // cu_qp_delta_enabled_flag: disable for CQP mode so the driver uses
                // a uniform QP across all CUs.
                if config.rate_control_mode != RateControlMode::Cqp {
                    pps_flags.set_cu_qp_delta_enabled_flag(1);
                }
                pps_flags.set_pps_loop_filter_across_slices_enabled_flag(1);
                pps_flags.set_deblocking_filter_control_present_flag(1);

                h265_pps = vk::video::StdVideoH265PictureParameterSet {
                    flags: pps_flags,
                    pps_pic_parameter_set_id: 0,
                    pps_seq_parameter_set_id: 0,
                    sps_video_parameter_set_id: 0,
                    num_extra_slice_header_bits: 0,
                    num_ref_idx_l0_default_active_minus1: 0,
                    num_ref_idx_l1_default_active_minus1: 0,
                    // Driver overrides init_qp_minus26 to 0 (init_qp=26).
                    // We MUST match this so constant_qp - init_qp gives the correct
                    // slice_qp_delta. If we set -8 but driver uses 0, the slice_qp_delta
                    // computation is wrong and QP ends up at 26 instead of 18.
                    init_qp_minus26: 0,
                    diff_cu_qp_delta_depth: 0,
                    pps_cb_qp_offset: 0,
                    pps_cr_qp_offset: 0,
                    pps_beta_offset_div2: 0,
                    pps_tc_offset_div2: 0,
                    diff_cu_chroma_qp_offset_depth: 0,
                    chroma_qp_offset_list_len_minus1: 0,
                    cb_qp_offset_list: [0; 6],
                    cr_qp_offset_list: [0; 6],
                    log2_parallel_merge_level_minus2: 0,
                    log2_max_transform_skip_block_size_minus2: 0,
                    log2_sao_offset_scale_luma: 0,
                    log2_sao_offset_scale_chroma: 0,
                    pps_act_y_qp_offset_plus5: 0,
                    pps_act_cb_qp_offset_plus5: 0,
                    pps_act_cr_qp_offset_plus3: 0,
                    pps_num_palette_predictor_initializers: 0,
                    luma_bit_depth_entry_minus8: 0,
                    chroma_bit_depth_entry_minus8: 0,
                    num_tile_columns_minus1: 0,
                    num_tile_rows_minus1: 0,
                    reserved1: 0,
                    reserved2: 0,
                    column_width_minus1: [0; 19],
                    row_height_minus1: [0; 21],
                    reserved3: 0,
                    pScalingLists: ptr::null(),
                    pPredictorPaletteEntries: ptr::null(),
                };

                h265_vps_array = [h265_vps];
                h265_sps_array = [h265_sps];
                h265_pps_array = [h265_pps];
                crate::VideoSessionParametersAddInfo::EncodeH265 {
                    vps: &h265_vps_array,
                    sps: &h265_sps_array,
                    pps: &h265_pps_array,
                    max_std_vps_count: 1,
                    max_std_sps_count: 1,
                    max_std_pps_count: 1,
                }
            } else {
                return Err(VideoError::CodecNotSupported(self.codec_flag));
            };

            let descriptor = crate::VideoSessionParametersDescriptor {
                label: "encode/session-params",
                add_info,
                // VUID-vkCmdEncodeVideoKHR-None-08318: session params must be
                // created with the currently set quality level.
                quality_level: if quality_level > 0 {
                    Some(quality_level)
                } else {
                    None
                },
                template: None,
            };
            crate::VulkanVideoSessionParameters::new(session, &descriptor)
                .map_err(|e| VideoError::BitstreamError(format!("encoder session params: {e}")))
        }
    }
}
