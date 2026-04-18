// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Video session creation, DPB images, bitstream buffer, query pool, and
//! session parameter management for the encoder.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionDeviceCommands;
use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;
use vulkanalia::vk::KhrVideoEncodeQueueExtensionDeviceCommands;
use vulkanalia_vma::{self as vma, Alloc};
use std::ptr;

use crate::video_context::{VideoError, VideoResult};
use crate::vk_video_encoder::vk_video_encoder_def::{align_size, H264_MB_SIZE_ALIGNMENT};
use crate::vk_video_encoder::vk_encoder_config_h264::{
    EncoderConfigH264, profile_idc_to_std_video, level_index_to_std_video,
};
use crate::vk_video_encoder::vk_encoder_config_h265::EncoderConfigH265;
use crate::vk_video_encoder::vk_video_encoder_h264::VkVideoEncoderH264;
use crate::vk_video_encoder::vk_video_encoder_h265::VkVideoEncoderH265;

use super::config::{DpbSlot, EncodeConfig, RateControlMode};
use super::SimpleEncoder;

impl SimpleEncoder {
    /// Configure the encoder: create video session, DPB images, bitstream
    /// buffer, command pool, query pool, and fence.
    ///
    /// Must be called exactly once before `encode_frame`.
    ///
    /// # Safety
    ///
    /// The caller must ensure the `VideoContext` device has the required video
    /// encode extensions enabled and that the queue family supports encode.
    pub(crate) unsafe fn configure(&mut self, config: &EncodeConfig) -> VideoResult<()> {
        config.validate()?;

        tracing::info!(
            width = config.width,
            height = config.height,
            codec = ?self.codec_flag,
            "Configuring encoder"
        );

        let device = self.ctx.device();
        let instance = self.ctx.instance();

        // --- Initialize H.264 config (profile/level auto-selection) ---
        let h264_cfg = if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            let mut cfg = EncoderConfigH264::default();
            cfg.base.input.width = config.width;
            cfg.base.input.height = config.height;
            cfg.base.input.bpp = 8;
            cfg.base.encode_width = config.width;
            cfg.base.encode_height = config.height;
            cfg.base.frame_rate_numerator = config.framerate_numerator;
            cfg.base.frame_rate_denominator = config.framerate_denominator;
            cfg.base.average_bitrate = config.average_bitrate;
            cfg.base.max_bitrate = config.max_bitrate;
            cfg.base.rate_control_mode = config.rate_control_mode.to_vk_flags();
            cfg.base.gop_structure.set_consecutive_b_frame_count(config.num_b_frames);
            let _ = cfg.initialize_parameters();
            Some(cfg)
        } else {
            None
        };

        // --- Initialize H.265 config (profile/level auto-selection) ---
        let h265_cfg = if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            let mut cfg = EncoderConfigH265::default();
            cfg.base.input.width = config.width;
            cfg.base.input.height = config.height;
            cfg.base.input.bpp = 8;
            cfg.base.encode_width = config.width;
            cfg.base.encode_height = config.height;
            cfg.base.frame_rate_numerator = config.framerate_numerator;
            cfg.base.frame_rate_denominator = config.framerate_denominator;
            cfg.base.average_bitrate = config.average_bitrate;
            cfg.base.max_bitrate = config.max_bitrate;
            cfg.base.rate_control_mode = config.rate_control_mode.to_vk_flags();
            cfg.base.gop_structure.set_consecutive_b_frame_count(config.num_b_frames);
            let _ = cfg.initialize_parameters();
            Some(cfg)
        } else {
            None
        };

        // --- Video profile ---
        let h264_profile_idc = h264_cfg.as_ref()
            .map(|c| profile_idc_to_std_video(c.profile_idc))
            .unwrap_or(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
        let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder()
            .std_profile_idc(h264_profile_idc);
        // Use config-derived profile for H.265 (h265_profile::MAIN = 1 = STD_VIDEO_H265_PROFILE_IDC_MAIN)
        let h265_profile_idc = h265_cfg.as_ref()
            .map(|c| vk::video::StdVideoH265ProfileIdc(c.profile as i32))
            .unwrap_or(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);
        let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder()
            .std_profile_idc(h265_profile_idc);

        // Hint to the driver that this is a low-latency streaming encoder.
        // This enables NVIDIA-specific optimizations for real-time encoding
        // (e.g. psycho-visual tuning, rate-distortion optimization).
        let mut encode_usage = vk::VideoEncodeUsageInfoKHR::builder()
            .tuning_mode(vk::VideoEncodeTuningModeKHR::LOW_LATENCY);

        let mut profile_info = vk::VideoProfileInfoKHR::builder()
            .video_codec_operation(self.codec_flag)
            .chroma_subsampling(config.chroma_subsampling)
            .luma_bit_depth(config.luma_bit_depth)
            .chroma_bit_depth(config.chroma_bit_depth)
            .push_next(&mut encode_usage);

        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            profile_info = profile_info.push_next(&mut h264_profile);
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            profile_info = profile_info.push_next(&mut h265_profile);
        }

        let _profile_list =
            vk::VideoProfileListInfoKHR::builder().profiles(std::slice::from_ref(&profile_info));

        // --- Query video capabilities ---
        // The pNext chain must include codec-specific capability structs.
        // Chain: VideoCapabilitiesKHR → VideoEncodeCapabilitiesKHR → codec caps
        let mut h264_encode_caps = vk::VideoEncodeH264CapabilitiesKHR::default();
        let mut h265_encode_caps = vk::VideoEncodeH265CapabilitiesKHR::default();
        // Save pointer before push_next takes a mutable borrow.
        let h265_encode_caps_ptr: *const vk::VideoEncodeH265CapabilitiesKHR =
            &h265_encode_caps;
        let mut encode_caps = vk::VideoEncodeCapabilitiesKHR::default();
        let mut caps = vk::VideoCapabilitiesKHR::default();
        // Chain codec-specific caps into the pNext chain via raw pointers.
        // push_next is only available on builders, but get_physical_device_video_capabilities_khr
        // takes &mut VideoCapabilitiesKHR, so we build the chain manually.
        encode_caps.next = ptr::null_mut();
        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            encode_caps.next = &mut h264_encode_caps as *mut _ as *mut std::ffi::c_void;
            caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            encode_caps.next = &mut h265_encode_caps as *mut _ as *mut std::ffi::c_void;
            caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;
        } else {
            caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;
        }

        instance.get_physical_device_video_capabilities_khr(
            self.ctx.physical_device(),
            &profile_info,
            &mut caps,
        )?;

        // Clamp quality_level to driver's maximum (max_quality_levels is a COUNT,
        // so valid levels are 0..max_quality_levels-1). H.265 on RTX 3090 only
        // supports level 0, while H.264 supports levels 0..3.
        let effective_quality_level = if encode_caps.max_quality_levels > 0 {
            config.quality_level.min(encode_caps.max_quality_levels - 1)
        } else {
            0
        };

        tracing::debug!(
            max_dpb = caps.max_dpb_slots,
            max_active_refs = caps.max_active_reference_pictures,
            max_quality_levels = encode_caps.max_quality_levels,
            requested_quality = config.quality_level,
            effective_quality = effective_quality_level,
            picture_access_w = caps.picture_access_granularity.width,
            picture_access_h = caps.picture_access_granularity.height,
            "Video encode capabilities"
        );

        // Use the driver-reported pictureAccessGranularity for alignment.
        // H.265 may require larger alignment (e.g. 32x32) than H.264's 16x16.
        let granularity_w = caps.picture_access_granularity.width.max(H264_MB_SIZE_ALIGNMENT);
        let granularity_h = caps.picture_access_granularity.height.max(H264_MB_SIZE_ALIGNMENT);
        let aligned_w = align_size(config.width, granularity_w);
        let aligned_h = align_size(config.height, granularity_h);

        // Add 1 extra DPB slot to accommodate the setup (reconstructed)
        // picture without evicting a reference. This follows the C++ reference
        // pattern where DPB count = max_ref_frames + 1.
        let max_dpb = caps.max_dpb_slots.min(config.max_dpb_slots + 1);

        // --- Initialize H.264 encoder early (needed by create_session_parameters) ---
        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            let mut enc = Box::new(VkVideoEncoderH264::new());
            enc.log2_max_frame_num_minus4 = 0;         // max_frame_num = 16
            enc.log2_max_pic_order_cnt_lsb_minus4 = 4; // max_poc_lsb = 256
            // Reserve 1 slot for setup (reconstructed picture), rest for references
            enc.max_num_ref_frames = (max_dpb as u32).saturating_sub(1).max(1);
            enc.init_dpb(max_dpb as i32);
            enc.frame_num = 0;
            enc.poc_lsb = 0;
            enc.idr_pic_id = 0;
            self.h264_encoder = Some(enc);
            self.h264_config = h264_cfg;
        }

        // --- Initialize H.265 encoder early (needed by create_session_parameters) ---
        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            let mut enc = Box::new(VkVideoEncoderH265::new());
            enc.log2_max_pic_order_cnt_lsb_minus4 = 4; // max_poc_lsb = 256
            enc.num_ref_l0 = 1;
            enc.num_ref_l1 = 0; // no B-frames
            // Initialize DPB: use_multiple_refs = (numRefL0 > 0) || (numRefL1 > 0) = true
            enc.init_dpb(max_dpb as i32, true);
            self.h265_encoder = Some(enc);
            self.h265_config = h265_cfg;
        }

        // --- Create Video Session ---
        let session_create_info = vk::VideoSessionCreateInfoKHR::builder()
            .queue_family_index(self.encode_queue_family)
            .video_profile(&profile_info)
            .picture_format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .max_coded_extent(vk::Extent2D {
                width: aligned_w,
                height: aligned_h,
            })
            .reference_picture_format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .max_dpb_slots(max_dpb)
            .max_active_reference_pictures(
                caps.max_active_reference_pictures
                    .min(max_dpb.saturating_sub(1)),
            )
            .std_header_version(&caps.std_header_version);

        // Video session creation, memory allocation, and binding run under the
        // host's device-level resource lock (fixes #278 — prevents NVIDIA
        // driver crashes when concurrent processors submit during setup).
        let allocator = self.ctx.allocator();
        let mut session_result: VideoResult<(
            vk::VideoSessionKHR,
            Vec<vma::Allocation>,
        )> = Err(VideoError::Vulkan(vk::Result::ERROR_INITIALIZATION_FAILED));
        let session_result_ref = &mut session_result;
        self.submitter.with_device_resource_lock(&mut || {
            *session_result_ref = (|| {
                let video_session = device
                    .create_video_session_khr(&session_create_info, None)
                    .map_err(VideoError::from)?;

                let mem_requirements = device
                    .get_video_session_memory_requirements_khr(video_session)
                    .map_err(VideoError::from)?;

                let mut session_memory = Vec::with_capacity(mem_requirements.len());
                let mut bind_infos = Vec::with_capacity(mem_requirements.len());

                for req in &mem_requirements {
                    let alloc_options = vma::AllocationOptions {
                        usage: vma::MemoryUsage::Unknown,
                        memory_type_bits: req.memory_requirements.memory_type_bits,
                        ..Default::default()
                    };

                    let allocation = allocator
                        .allocate_memory(req.memory_requirements, &alloc_options)
                        .map_err(VideoError::from)?;

                    let alloc_info = allocator.get_allocation_info(allocation);
                    session_memory.push(allocation);

                    bind_infos.push(
                        vk::BindVideoSessionMemoryInfoKHR::builder()
                            .memory_bind_index(req.memory_bind_index)
                            .memory(alloc_info.deviceMemory)
                            .memory_offset(alloc_info.offset)
                            .memory_size(req.memory_requirements.size),
                    );
                }

                device
                    .bind_video_session_memory_khr(video_session, &bind_infos)
                    .map_err(VideoError::from)?;

                Ok((video_session, session_memory))
            })();
        });
        let (video_session, session_memory) = session_result?;

        // --- Determine H.265 CTB size from capabilities ---
        // The driver reports supported CTB sizes; we pick the largest supported
        // one to derive log2_diff_max_min_luma_coding_block_size for the SPS.
        // With log2_min_luma_coding_block_size_minus3 = 0 (MinCbSizeY = 8),
        // CtbSizeY = 2^(3 + log2_diff), so log2_diff = log2(CtbSizeY) - 3.
        let h265_ctb_log2_size: u32 = {
            let h265c = &*h265_encode_caps_ptr;
            if h265c.ctb_sizes.contains(vk::VideoEncodeH265CtbSizeFlagsKHR::_64) {
                6
            } else if h265c.ctb_sizes.contains(vk::VideoEncodeH265CtbSizeFlagsKHR::_32) {
                5
            } else {
                4 // TYPE_16
            }
        };

        // --- Create session parameters (SPS/PPS) ---
        let session_params =
            self.create_session_parameters(video_session, config, &profile_info, h265_ctb_log2_size, aligned_w, aligned_h, effective_quality_level)?;

        // --- Allocate DPB images ---
        // Use separate images when the driver supports it. Shared array-layer
        // DPB images cause progressive quality degradation on NVIDIA drivers
        // (P→P reconstruction corrupts neighbouring layers).
        let use_separate = caps.flags.contains(vk::VideoCapabilityFlagsKHR::SEPARATE_REFERENCE_IMAGES);
        let (dpb_image, dpb_allocation, dpb_sep_images, dpb_sep_allocs, dpb_slots) =
            self.create_dpb_images(max_dpb, aligned_w, aligned_h, &profile_info, use_separate)?;

        // --- Create bitstream output buffer ---
        let bs_size = config.effective_bitstream_buffer_size();
        let (bs_buffer, bs_allocation, bs_ptr) =
            self.create_bitstream_buffer(bs_size, &profile_info)?;

        // --- Command pool and buffer ---
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(self.encode_queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        let command_pool = device.create_command_pool(&pool_info, None)?;

        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);

        let command_buffers = device.allocate_command_buffers(&alloc_info)?;

        // --- Query pool for encode feedback ---
        // Build the create info with the video encode feedback flags and profile.
        let mut query_pool_video = vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::builder()
            .encode_feedback_flags(
                vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BUFFER_OFFSET
                    | vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
            );

        let query_pool_info = vk::QueryPoolCreateInfo::builder()
            .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
            .query_count(1)
            .push_next(&mut query_pool_video)
            .push_next(&mut profile_info);

        let query_pool = device.create_query_pool(&query_pool_info, None)?;

        // --- Fence ---
        let fence_info = vk::FenceCreateInfo::default();
        let fence = device.create_fence(&fence_info, None)?;

        // --- Store state ---
        self.aligned_width = aligned_w;
        self.aligned_height = aligned_h;
        self.video_session = video_session;
        self.session_memory = session_memory;
        self.session_params = session_params;
        self.dpb_image = dpb_image;
        self.dpb_allocation = dpb_allocation;
        self.dpb_separate_images = dpb_sep_images;
        self.dpb_separate_allocations = dpb_sep_allocs;
        self.dpb_slots = dpb_slots;
        self.bitstream_buffer = bs_buffer;
        self.bitstream_allocation = bs_allocation;
        self.bitstream_buffer_size = bs_size;
        self.bitstream_mapped_ptr = bs_ptr;
        self.command_pool = command_pool;
        self.command_buffer = command_buffers[0];
        self.query_pool = query_pool;
        self.fence = fence;
        self.encode_config = Some(config.clone());
        self.configured = true;
        self.effective_quality_level = effective_quality_level;
        eprintln!("[ENCODER] configured: {}x{} codec={:?} quality={}->{} dpb={}",
            aligned_w, aligned_h, self.codec_flag, config.quality_level, effective_quality_level, max_dpb);
        self.frame_count = 0;
        self.poc_counter = 0;
        self.encode_order_count = 0;
        self.rate_control_sent = false;

        // H.264 and H.265 encoders already initialized above (before create_session_parameters)

        tracing::info!(
            dpb_count = self.dpb_slots.len(),
            bitstream_buffer_size = bs_size,
            "Encoder configured"
        );

        Ok(())
    }

    /// Returns whether the encoder has been configured.
    #[allow(dead_code)] // Public API for callers that check state
    pub(crate) fn is_configured(&self) -> bool {
        self.configured
    }

    /// Returns the number of frames encoded so far.
    #[allow(dead_code)] // Public API for callers that need frame count
    pub(crate) fn frame_count_value(&self) -> u64 {
        self.frame_count
    }

    /// Returns a reference to the current configuration, if set.
    pub(crate) fn encode_config(&self) -> Option<&EncodeConfig> {
        self.encode_config.as_ref()
    }

    /// Returns the H.264 profile IDC as a StdVideoH264ProfileIdc.
    /// Falls back to HIGH if no H.264 config is set.
    pub(crate) fn h264_profile_idc(&self) -> vk::video::StdVideoH264ProfileIdc {
        self.h264_config.as_ref()
            .map(|c| profile_idc_to_std_video(c.profile_idc))
            .unwrap_or(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH)
    }

    /// Extract the encoded session parameters (SPS/PPS for H.264,
    /// VPS/SPS/PPS for H.265) as raw Annex B bitstream data.
    ///
    /// The returned bytes contain the parameter set NAL units with start code
    /// prefixes. Callers should prepend this to the first IDR frame's
    /// bitstream data to produce a valid standalone H.264/H.265 stream.
    ///
    /// # Safety
    ///
    /// The encoder must be configured and the video session parameters
    /// must be valid.
    pub(crate) unsafe fn extract_header(&self) -> VideoResult<Vec<u8>> {
        if !self.configured {
            return Err(VideoError::BitstreamError(
                "Encoder not configured".to_string(),
            ));
        }

        let device = self.ctx.device();

        let mut h264_get_info;
        let mut h265_get_info;

        let mut get_info = vk::VideoEncodeSessionParametersGetInfoKHR::builder()
            .video_session_parameters(self.session_params);

        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            h264_get_info = vk::VideoEncodeH264SessionParametersGetInfoKHR::builder()
                .write_std_sps(true)
                .write_std_pps(true)
                .std_sps_id(0)
                .std_pps_id(0);
            get_info = get_info.push_next(&mut h264_get_info);
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            h265_get_info = vk::VideoEncodeH265SessionParametersGetInfoKHR::builder()
                .write_std_vps(true)
                .write_std_sps(true)
                .write_std_pps(true)
                .std_vps_id(0)
                .std_sps_id(0)
                .std_pps_id(0);
            get_info = get_info.push_next(&mut h265_get_info);
        }

        let mut feedback = vk::VideoEncodeSessionParametersFeedbackInfoKHR::default();

        let data = device.get_encoded_video_session_parameters_khr(
            &get_info,
            Some(&mut feedback),
        )?;

        tracing::debug!(
            size = data.len(),
            has_overrides = feedback.has_overrides != 0,
            "Extracted session parameter header"
        );

        Ok(data)
    }

    /// Create codec-specific session parameters (SPS/PPS for H.264,
    /// VPS/SPS/PPS for H.265).
    ///
    /// # Safety
    ///
    /// Caller must ensure `video_session` is valid and `profile_info` is live.
    unsafe fn create_session_parameters(
        &self,
        video_session: vk::VideoSessionKHR,
        config: &EncodeConfig,
        _profile_info: &vk::VideoProfileInfoKHR,
        h265_ctb_log2_size: u32,
        aligned_w: u32,
        aligned_h: u32,
        quality_level: u32,
    ) -> VideoResult<vk::VideoSessionParametersKHR> {
        let device = self.ctx.device();

        let mut params_create = vk::VideoSessionParametersCreateInfoKHR::builder()
            .video_session(video_session);

        // H.264: add SPS + PPS parameter sets
        let h264_sps;
        let h264_pps;
        let h264_add_info;
        let mut h264_params;

        // H.265: add VPS + SPS + PPS parameter sets
        // These must be in the outer scope so raw-pointer references from
        // h265_vps and h265_sps remain valid until create_video_session_parameters_khr.
        let h265_vps;
        let h265_sps;
        let h265_pps;
        let h265_add_info;
        let mut h265_params;
        let h265_dec_pic_buf_mgr;
        let h265_profile_tier_level;

        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            // Build SPS/PPS from the ported EncoderConfigH264 state instead of
            // hard-coding values inline.
            let enc = self.h264_encoder.as_ref()
                .expect("h264_encoder must be initialized before create_session_parameters");
            let cfg = self.h264_config.as_ref()
                .expect("h264_config must be initialized before create_session_parameters");
            let state = cfg.build_sps_pps_state(
                aligned_w, aligned_h,
                config.width, config.height,
                enc.max_num_ref_frames,
                enc.log2_max_frame_num_minus4,
                enc.log2_max_pic_order_cnt_lsb_minus4,
            );

            let mut sps_flags: vk::video::StdVideoH264SpsFlags = std::mem::zeroed();
            sps_flags.set_direct_8x8_inference_flag(if state.sps_info.direct_8x8_inference_flag { 1 } else { 0 });
            sps_flags.set_frame_mbs_only_flag(if state.sps_info.frame_mbs_only_flag { 1 } else { 0 });
            sps_flags.set_frame_cropping_flag(if state.sps_info.frame_cropping_flag { 1 } else { 0 });

            h264_sps = vk::video::StdVideoH264SequenceParameterSet {
                flags: sps_flags,
                profile_idc: profile_idc_to_std_video(state.sps_info.profile_idc),
                level_idc: level_index_to_std_video(state.sps_info.level_idc),
                chroma_format_idc:
                    vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
                seq_parameter_set_id: state.sps_info.seq_parameter_set_id,
                bit_depth_luma_minus8: 0,
                bit_depth_chroma_minus8: 0,
                log2_max_frame_num_minus4: state.sps_info.log2_max_frame_num_minus4,
                pic_order_cnt_type:
                    vk::video::STD_VIDEO_H264_POC_TYPE_0,
                offset_for_non_ref_pic: 0,
                offset_for_top_to_bottom_field: 0,
                log2_max_pic_order_cnt_lsb_minus4: state.sps_info.log2_max_pic_order_cnt_lsb_minus4,
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
                pSequenceParameterSetVui: ptr::null(),
            };

            let mut pps_flags: vk::video::StdVideoH264PpsFlags = std::mem::zeroed();
            pps_flags.set_transform_8x8_mode_flag(if state.pps_info.transform_8x8_mode_flag { 1 } else { 0 });
            pps_flags.set_deblocking_filter_control_present_flag(if state.pps_info.deblocking_filter_control_present_flag { 1 } else { 0 });
            pps_flags.set_entropy_coding_mode_flag(if state.pps_info.entropy_coding_mode_flag { 1 } else { 0 });

            h264_pps = vk::video::StdVideoH264PictureParameterSet {
                flags: pps_flags,
                seq_parameter_set_id: state.pps_info.seq_parameter_set_id,
                pic_parameter_set_id: state.pps_info.pic_parameter_set_id,
                num_ref_idx_l0_default_active_minus1: state.pps_info.num_ref_idx_l0_default_active_minus1,
                num_ref_idx_l1_default_active_minus1: state.pps_info.num_ref_idx_l1_default_active_minus1,
                weighted_bipred_idc:
                    vk::video::STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
                pic_init_qp_minus26: config.const_qp_intra as i8 - 26,
                pic_init_qs_minus26: 0,
                chroma_qp_index_offset: state.pps_info.chroma_qp_index_offset,
                second_chroma_qp_index_offset: state.pps_info.second_chroma_qp_index_offset,
                pScalingLists: ptr::null(),
            };

            h264_add_info = vk::VideoEncodeH264SessionParametersAddInfoKHR::builder()
                .std_sp_ss(std::slice::from_ref(&h264_sps))
                .std_pp_ss(std::slice::from_ref(&h264_pps));

            h264_params = vk::VideoEncodeH264SessionParametersCreateInfoKHR::builder()
                .max_std_sps_count(1)
                .max_std_pps_count(1)
                .parameters_add_info(&h264_add_info);

            params_create = params_create.push_next(&mut h264_params);
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
            ptl_flags.set_general_progressive_source_flag(1);   // C++ line 648
            ptl_flags.set_general_frame_only_constraint_flag(1); // C++ line 651
            if let Some(ref cfg) = self.h265_config {
                if cfg.general_tier_flag {
                    ptl_flags.set_general_tier_flag(1);
                }
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

            // --- SPS flags (C++ lines 661-689) ---
            let mut sps_flags: vk::video::StdVideoH265SpsFlags = std::mem::zeroed();
            sps_flags.set_sps_temporal_id_nesting_flag(1);            // C++ line 661
            sps_flags.set_conformance_window_flag(
                if aligned_w != config.width || aligned_h != config.height {
                    1
                } else {
                    0
                },
            );
            sps_flags.set_sps_sub_layer_ordering_info_present_flag(1); // C++ line 663
            sps_flags.set_amp_enabled_flag(1);                         // C++ line 666
            sps_flags.set_sample_adaptive_offset_enabled_flag(1);      // C++ line 667
            // strong_intra_smoothing_enabled_flag = 0 (C++ line 672) — already 0
            // sps_temporal_mvp_enabled_flag = 0 (C++ line 671) — already 0

            // --- SPS (C++ lines 691-757) ---
            h265_sps = vk::video::StdVideoH265SequenceParameterSet {
                flags: sps_flags,
                chroma_format_idc:
                    vk::video::STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
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
                pSequenceParameterSetVui: ptr::null(),
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

            h265_add_info = vk::VideoEncodeH265SessionParametersAddInfoKHR::builder()
                .std_vp_ss(std::slice::from_ref(&h265_vps))
                .std_sp_ss(std::slice::from_ref(&h265_sps))
                .std_pp_ss(std::slice::from_ref(&h265_pps));

            h265_params = vk::VideoEncodeH265SessionParametersCreateInfoKHR::builder()
                .max_std_vps_count(1)
                .max_std_sps_count(1)
                .max_std_pps_count(1)
                .parameters_add_info(&h265_add_info);

            params_create = params_create.push_next(&mut h265_params);
        }

        // Chain quality level into session parameters creation.
        // Required by VUID-vkCmdEncodeVideoKHR-None-08318: session params must
        // be created with the currently set quality level.
        let mut quality_level_sp;
        if quality_level > 0 {
            quality_level_sp = vk::VideoEncodeQualityLevelInfoKHR::builder()
                .quality_level(quality_level);
            params_create = params_create.push_next(&mut quality_level_sp);
        }

        let params = device.create_video_session_parameters_khr(&params_create, None)?;

        Ok(params)
    }

    /// Create DPB images for the video session.
    ///
    /// Returns `(shared_image, allocation, separate_images, separate_allocs, slots)`.
    ///
    /// # Safety
    ///
    /// Caller must ensure `profile_info` is valid for image creation.
    unsafe fn create_dpb_images(
        &self,
        count: u32,
        width: u32,
        height: u32,
        profile_info: &vk::VideoProfileInfoKHR,
        use_separate_images: bool,
    ) -> VideoResult<(vk::Image, vma::Allocation, Vec<vk::Image>, Vec<vma::Allocation>, Vec<DpbSlot>)> {
        let device = self.ctx.device();
        let allocator = self.ctx.allocator();
        let mut slots = Vec::with_capacity(count as usize);

        let profile_list =
            vk::VideoProfileListInfoKHR::builder().profiles(std::slice::from_ref(profile_info));

        let alloc_options = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        if use_separate_images {
            // Create SEPARATE VkImage per DPB slot (matches ffmpeg vulkan encoder
            // pattern). Each image has 1 array layer.
            let mut images = Vec::with_capacity(count as usize);
            let mut allocations = Vec::with_capacity(count as usize);

            for i in 0..count {
                let mut image_create_info = vk::ImageCreateInfo::builder()
                    .image_type(vk::ImageType::_2D)
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED);

                image_create_info.next =
                    &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

                // DPB image allocation runs under the host's device-level
                // resource lock (fixes #278).
                let mut alloc_result: VideoResult<(vk::Image, vma::Allocation)> =
                    Err(VideoError::Vulkan(vk::Result::ERROR_INITIALIZATION_FAILED));
                let alloc_result_ref = &mut alloc_result;
                self.submitter.with_device_resource_lock(&mut || {
                    *alloc_result_ref = allocator
                        .create_image(image_create_info, &alloc_options)
                        .map_err(VideoError::from);
                });
                let (image, allocation) = alloc_result?;

                let view_info = vk::ImageViewCreateInfo::builder()
                    .image(image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    });

                let view = device.create_image_view(&view_info, None)?;

                images.push(image);
                allocations.push(allocation);

                slots.push(DpbSlot {
                    view,
                    array_layer: i,
                    in_use: false,
                    frame_num: 0,
                    poc: 0,
                    pic_type: vk::video::STD_VIDEO_H264_PICTURE_TYPE_P,
                    h265_pic_type: vk::video::STD_VIDEO_H265_PICTURE_TYPE_P,
                });
            }

            Ok((vk::Image::null(), unsafe { std::mem::zeroed() }, images, allocations, slots))
        } else {
            // Create a SINGLE VkImage with `count` array layers.
            let mut image_create_info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::_2D)
                .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1)
                .array_layers(count)
                .samples(vk::SampleCountFlags::_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);

            image_create_info.next =
                &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

            // DPB image allocation runs under the host's device-level
            // resource lock (fixes #278).
            let mut alloc_result: VideoResult<(vk::Image, vma::Allocation)> =
                Err(VideoError::Vulkan(vk::Result::ERROR_INITIALIZATION_FAILED));
            let alloc_result_ref = &mut alloc_result;
            self.submitter.with_device_resource_lock(&mut || {
                *alloc_result_ref = allocator
                    .create_image(image_create_info, &alloc_options)
                    .map_err(VideoError::from);
            });
            let (image, allocation) = alloc_result?;

            for i in 0..count {
                let view_info = vk::ImageViewCreateInfo::builder()
                    .image(image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: i,
                        layer_count: 1,
                    });

                let view = device.create_image_view(&view_info, None)?;

                slots.push(DpbSlot {
                    view,
                    array_layer: i,
                    in_use: false,
                    frame_num: 0,
                    poc: 0,
                    pic_type: vk::video::STD_VIDEO_H264_PICTURE_TYPE_P,
                    h265_pic_type: vk::video::STD_VIDEO_H265_PICTURE_TYPE_P,
                });
            }

            Ok((image, allocation, Vec::new(), Vec::new(), slots))
        }
    }

    /// Create the host-visible bitstream output buffer.
    ///
    /// # Safety
    ///
    /// Caller must ensure `profile_info` is valid for buffer creation.
    unsafe fn create_bitstream_buffer(
        &self,
        size: usize,
        profile_info: &vk::VideoProfileInfoKHR,
    ) -> VideoResult<(vk::Buffer, vma::Allocation, *mut u8)> {
        let allocator = self.ctx.allocator();

        let profile_list =
            vk::VideoProfileListInfoKHR::builder().profiles(std::slice::from_ref(profile_info));

        let mut buffer_create_info = vk::BufferCreateInfo::builder()
            .size(size as u64)
            .usage(vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        buffer_create_info.next =
            &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

        let alloc_options = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        // Bitstream buffer allocation runs under the host's device-level
        // resource lock (fixes #278).
        let mut alloc_result: VideoResult<(vk::Buffer, vma::Allocation)> =
            Err(VideoError::Vulkan(vk::Result::ERROR_INITIALIZATION_FAILED));
        let alloc_result_ref = &mut alloc_result;
        self.submitter.with_device_resource_lock(&mut || {
            *alloc_result_ref = allocator
                .create_buffer(buffer_create_info, &alloc_options)
                .map_err(VideoError::from);
        });
        let (buffer, allocation) = alloc_result?;

        let info = allocator.get_allocation_info(allocation);
        let mapped = info.pMappedData as *mut u8;
        if mapped.is_null() {
            allocator.destroy_buffer(buffer, allocation);
            return Err(VideoError::Vulkan(vk::Result::ERROR_MEMORY_MAP_FAILED));
        }

        Ok((buffer, allocation, mapped))
    }
}
