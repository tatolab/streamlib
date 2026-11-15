// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Video session creation, DPB images, bitstream buffer and query pool for the encoder. Its
//! codec session parameters are created in `session_parameters`.

use std::ptr;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoEncodeQueueExtensionDeviceCommands;
use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;

use crate::vk_video_encoder::vk_encoder_config_h264::{
    EncoderConfigH264, profile_idc_to_std_video,
};
use crate::vk_video_encoder::vk_encoder_config_h265::EncoderConfigH265;
use crate::vk_video_encoder::vk_video_encoder_def::{H264_MB_SIZE_ALIGNMENT, align_size};
use crate::vk_video_encoder::vk_video_encoder_h264::VkVideoEncoderH264;
use crate::vk_video_encoder::vk_video_encoder_h265::VkVideoEncoderH265;
use crate::{VideoError, VideoResult};

use super::SimpleEncoder;

use super::config::{DpbSlot, EncodeConfig};

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
        unsafe {
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
                cfg.base
                    .gop_structure
                    .set_consecutive_b_frame_count(config.num_b_frames);
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
                cfg.base
                    .gop_structure
                    .set_consecutive_b_frame_count(config.num_b_frames);
                let _ = cfg.initialize_parameters();
                Some(cfg)
            } else {
                None
            };

            // --- Video profile ---
            let h264_profile_idc = h264_cfg
                .as_ref()
                .map(|c| profile_idc_to_std_video(c.profile_idc))
                .unwrap_or(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
            let mut h264_profile =
                vk::VideoEncodeH264ProfileInfoKHR::builder().std_profile_idc(h264_profile_idc);
            // Use config-derived profile for H.265 (h265_profile::MAIN = 1 = STD_VIDEO_H265_PROFILE_IDC_MAIN)
            let h265_profile_idc = h265_cfg
                .as_ref()
                .map(|c| vk::video::StdVideoH265ProfileIdc(c.profile as i32))
                .unwrap_or(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);
            let mut h265_profile =
                vk::VideoEncodeH265ProfileInfoKHR::builder().std_profile_idc(h265_profile_idc);

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

            let _profile_list = vk::VideoProfileListInfoKHR::builder()
                .profiles(std::slice::from_ref(&profile_info));

            // --- Query video capabilities ---
            // The pNext chain must include codec-specific capability structs.
            // Chain: VideoCapabilitiesKHR → VideoEncodeCapabilitiesKHR → codec caps
            let mut h264_encode_caps = vk::VideoEncodeH264CapabilitiesKHR::default();
            let mut h265_encode_caps = vk::VideoEncodeH265CapabilitiesKHR::default();
            // Save pointer before push_next takes a mutable borrow.
            let h265_encode_caps_ptr: *const vk::VideoEncodeH265CapabilitiesKHR = &h265_encode_caps;
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

            tracing::info!(
                codec = ?self.codec_flag,
                max_quality_levels = encode_caps.max_quality_levels,
                requested_quality = config.quality_level,
                effective_quality = effective_quality_level,
                "Encoder quality level"
            );
            tracing::debug!(
                max_dpb = caps.max_dpb_slots,
                max_active_refs = caps.max_active_reference_pictures,
                picture_access_w = caps.picture_access_granularity.width,
                picture_access_h = caps.picture_access_granularity.height,
                "Video encode capabilities"
            );

            // Use the driver-reported pictureAccessGranularity for alignment.
            // H.265 may require larger alignment (e.g. 32x32) than H.264's 16x16.
            let granularity_w = caps
                .picture_access_granularity
                .width
                .max(H264_MB_SIZE_ALIGNMENT);
            let granularity_h = caps
                .picture_access_granularity
                .height
                .max(H264_MB_SIZE_ALIGNMENT);
            let aligned_w = align_size(config.width, granularity_w);
            let aligned_h = align_size(config.height, granularity_h);

            // Add 1 extra DPB slot to accommodate the setup (reconstructed)
            // picture without evicting a reference. This follows the C++ reference
            // pattern where DPB count = max_ref_frames + 1.
            let max_dpb = caps.max_dpb_slots.min(config.max_dpb_slots + 1);

            // --- Initialize H.264 encoder early (needed by create_session_parameters) ---
            if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
                let mut enc = Box::new(VkVideoEncoderH264::new());
                enc.log2_max_frame_num_minus4 = 0; // max_frame_num = 16
                enc.log2_max_pic_order_cnt_lsb_minus4 = 4; // max_poc_lsb = 256
                // Reserve 1 slot for setup (reconstructed picture), rest for references
                enc.max_num_ref_frames = max_dpb.saturating_sub(1).max(1);
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
            // Through the engine RHI primitive: allocator + multi-binding
            // dance + `lock_device` is owned by `VulkanVideoSession::new`.
            let session_descriptor = crate::VideoSessionDescriptor {
                label: "encode/session",
                session_create_flags: vk::VideoSessionCreateFlagsKHR::empty(),
                video_queue_family: self.encode_queue_family,
                video_profile: *profile_info,
                codec_operation: self.codec_flag,
                picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
                max_coded_extent: vk::Extent2D {
                    width: aligned_w,
                    height: aligned_h,
                },
                reference_pictures_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
                max_dpb_slots: max_dpb,
                max_active_reference_pictures: caps
                    .max_active_reference_pictures
                    .min(max_dpb.saturating_sub(1)),
            };
            let video_session_arc =
                crate::VulkanVideoSession::new(self.ctx.codec_device(), &session_descriptor)
                    .map_err(|e| VideoError::BitstreamError(format!("encoder session: {e}")))?;
            let video_session = video_session_arc.handle();

            // --- Determine H.265 CTB size from capabilities ---
            // The driver reports supported CTB sizes; we pick the largest supported
            // one to derive log2_diff_max_min_luma_coding_block_size for the SPS.
            // With log2_min_luma_coding_block_size_minus3 = 0 (MinCbSizeY = 8),
            // CtbSizeY = 2^(3 + log2_diff), so log2_diff = log2(CtbSizeY) - 3.
            let h265_ctb_log2_size: u32 = {
                let h265c = &*h265_encode_caps_ptr;
                if h265c
                    .ctb_sizes
                    .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::_64)
                {
                    6
                } else if h265c
                    .ctb_sizes
                    .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::_32)
                {
                    5
                } else {
                    4 // TYPE_16
                }
            };

            // --- Create session parameters (SPS/PPS) ---
            let session_params_arc = self.create_session_parameters(
                &video_session_arc,
                config,
                &profile_info,
                h265_ctb_log2_size,
                aligned_w,
                aligned_h,
                effective_quality_level,
            )?;
            let session_params = session_params_arc.handle();

            // --- Allocate DPB images ---
            // Use separate images when the driver supports it. Shared array-layer
            // DPB images cause progressive quality degradation on NVIDIA drivers
            // (P→P reconstruction corrupts neighbouring layers).
            let use_separate = caps
                .flags
                .contains(vk::VideoCapabilityFlagsKHR::SEPARATE_REFERENCE_IMAGES);
            let (dpb_texture, dpb_separate_textures, dpb_slots) =
                self.create_dpb_images(max_dpb, aligned_w, aligned_h, &profile_info, use_separate)?;

            // --- Create bitstream output buffer ---
            let bs_size = config.effective_bitstream_buffer_size();
            let bitstream_buffer_owner = self.create_bitstream_buffer(bs_size, &profile_info)?;

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
            // Through the engine RHI primitive: it chains
            // `VkQueryPoolVideoEncodeFeedbackCreateInfoKHR` + the profile
            // onto `pNext` and runs `vkCreateQueryPool` under the device
            // resource lock.
            let query_pool = crate::VulkanQueryPool::new(
                self.ctx.codec_device(),
                &crate::QueryPoolDescriptor {
                    label: "simple_encoder/feedback",
                    query_type: vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR,
                    query_count: 1,
                    pipeline_statistics: vk::QueryPipelineStatisticFlags::empty(),
                    video_encode_feedback_flags:
                        vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BUFFER_OFFSET
                            | vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
                    video_profile: Some(&profile_info),
                },
            )
            .map_err(|e| VideoError::BitstreamError(format!("encoder query pool: {e}")))?;

            // --- Fence ---
            let fence_info = vk::FenceCreateInfo::default();
            let fence = device.create_fence(&fence_info, None)?;

            // --- Store state ---
            self.aligned_width = aligned_w;
            self.aligned_height = aligned_h;
            self.video_session = video_session;
            self.video_session_arc = Some(video_session_arc);
            self.session_params = session_params;
            self.session_params_arc = Some(session_params_arc);
            self.dpb_texture = dpb_texture;
            self.dpb_separate_textures = dpb_separate_textures;
            self.dpb_slots = dpb_slots;
            self.bitstream_buffer_owner = Some(bitstream_buffer_owner);
            self.bitstream_buffer_size = bs_size;
            self.command_pool = command_pool;
            self.command_buffer = command_buffers[0];
            self.query_pool = Some(query_pool);
            self.fence = fence;
            self.encode_config = Some(config.clone());
            self.configured = true;
            self.effective_quality_level = effective_quality_level;
            tracing::info!(
                width = aligned_w,
                height = aligned_h,
                codec = ?self.codec_flag,
                quality_level = config.quality_level,
                effective_quality_level,
                max_dpb,
                "encoder configured",
            );
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
        self.h264_config
            .as_ref()
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
        unsafe {
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

            let data =
                device.get_encoded_video_session_parameters_khr(&get_info, Some(&mut feedback))?;

            tracing::debug!(
                size = data.len(),
                has_overrides = feedback.has_overrides != 0,
                "Extracted session parameter header"
            );

            Ok(data)
        }
    }

    /// Create DPB images for the video session.
    ///
    /// Returns `(shared_layered_texture, separate_textures, slots)`. Exactly
    /// one of the two texture containers is populated:
    /// - `shared_layered_texture = Some(...)`, `separate_textures = []` when
    ///   `use_separate_images = false` (driver does NOT advertise
    ///   `SEPARATE_REFERENCE_IMAGES`): one `VkImage` with `count` array
    ///   layers, one image view per layer.
    /// - `shared_layered_texture = None`, `separate_textures = [N]` when
    ///   `use_separate_images = true`: `count` independent `VkImage`s, each
    ///   with one layer + one image view.
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
    ) -> VideoResult<(
        Option<crate::VulkanVideoDpbImage>,
        Vec<crate::VulkanVideoDpbImage>,
        Vec<DpbSlot>,
    )> {
        unsafe {
            use crate::{VideoDpbDirection, VideoDpbImageDescriptor, VulkanVideoDpbImage};

            let device = self.ctx.device();
            let mut slots = Vec::with_capacity(count as usize);

            if use_separate_images {
                // Create SEPARATE VkImage per DPB slot (matches ffmpeg vulkan encoder
                // pattern). Each image has 1 array layer.
                let mut textures = Vec::with_capacity(count as usize);

                for i in 0..count {
                    let descriptor = VideoDpbImageDescriptor {
                        label: "simple_encoder/dpb-separate",
                        width,
                        height,
                        format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
                        array_layers: 1,
                        direction: VideoDpbDirection::Encode,
                        additional_usage: vk::ImageUsageFlags::empty(),
                        sharing_queue_families: &[],
                        video_profile: profile_info,
                    };
                    let texture = VulkanVideoDpbImage::new(self.ctx.codec_device(), &descriptor)
                        .map_err(|e| {
                            VideoError::BitstreamError(format!("encoder dpb separate: {e}"))
                        })?;
                    let image = texture.image();

                    let mut ycbcr_info = vk::SamplerYcbcrConversionInfo::builder()
                        .conversion(self.ctx.nv12_ycbcr_conversion());
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
                        })
                        .push_next(&mut ycbcr_info);

                    let view = device.create_image_view(&view_info, None)?;
                    textures.push(texture);

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

                Ok((None, textures, slots))
            } else {
                // Create a SINGLE VkImage with `count` array layers.
                let descriptor = VideoDpbImageDescriptor {
                    label: "simple_encoder/dpb-shared-layered",
                    width,
                    height,
                    format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
                    array_layers: count,
                    direction: VideoDpbDirection::Encode,
                    additional_usage: vk::ImageUsageFlags::empty(),
                    sharing_queue_families: &[],
                    video_profile: profile_info,
                };
                let texture = VulkanVideoDpbImage::new(self.ctx.codec_device(), &descriptor)
                    .map_err(|e| VideoError::BitstreamError(format!("encoder dpb shared: {e}")))?;
                let image = texture.image();

                let mut ycbcr_info = vk::SamplerYcbcrConversionInfo::builder()
                    .conversion(self.ctx.nv12_ycbcr_conversion());

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
                        })
                        .push_next(&mut ycbcr_info);

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

                Ok((Some(texture), Vec::new(), slots))
            }
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
    ) -> VideoResult<crate::VulkanVideoBitstreamBuffer> {
        use crate::{
            VideoBitstreamBufferDescriptor, VideoBitstreamDirection, VulkanVideoBitstreamBuffer,
        };

        let descriptor = VideoBitstreamBufferDescriptor {
            label: "simple_encoder/bitstream",
            size: size as u64,
            direction: VideoBitstreamDirection::Encode,
            video_profile: profile_info,
        };
        VulkanVideoBitstreamBuffer::new(self.ctx.codec_device(), &descriptor)
            .map_err(|e| VideoError::BitstreamError(format!("encoder bitstream: {e}")))
    }
}
