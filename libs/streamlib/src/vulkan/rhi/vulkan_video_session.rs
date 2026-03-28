// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video session for H.264 hardware encoding.

use std::mem::MaybeUninit;
use std::ptr;

use ash::vk;
use ash::vk::native::{
    StdVideoH264ChromaFormatIdc, StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
    StdVideoH264LevelIdc, StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1,
    StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0,
    StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_1, StdVideoH264PictureParameterSet,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2, StdVideoH264PpsFlags,
    StdVideoH264ProfileIdc, StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN, StdVideoH264SequenceParameterSet,
    StdVideoH264SpsFlags,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
};

use crate::core::codec::{H264Profile, VideoCodec};
use crate::core::{Result, StreamError, VideoEncoderConfig};

use super::VulkanDevice;

/// Vulkan Video session for H.264 encoding.
pub struct VulkanVideoSession {
    device: ash::Device,
    video_queue_loader: ash::khr::video_queue::Device,
    video_session: vk::VideoSessionKHR,
    video_session_parameters: vk::VideoSessionParametersKHR,
    video_session_memory: Vec<vk::DeviceMemory>,
    video_encode_queue_family_index: u32,
}

impl VulkanVideoSession {
    /// Create a new Vulkan Video session for H.264 encoding.
    pub fn new(vulkan_device: &VulkanDevice, config: &VideoEncoderConfig) -> Result<Self> {
        let ve_family = vulkan_device
            .video_encode_queue_family_index()
            .ok_or_else(|| {
                StreamError::GpuError("No video encode queue family available".into())
            })?;

        if !vulkan_device.supports_video_encode() {
            return Err(StreamError::GpuError(
                "Vulkan Video encode extensions not available".into(),
            ));
        }

        let device = vulkan_device.device().clone();
        let instance = vulkan_device.instance().clone();

        let video_queue_instance_loader =
            ash::khr::video_queue::Instance::new(vulkan_device.entry(), vulkan_device.instance());
        let video_queue_loader =
            ash::khr::video_queue::Device::new(vulkan_device.instance(), &device);

        // 1. Build H.264 video profile chain
        let std_profile_idc = match config.codec {
            VideoCodec::H264(H264Profile::Baseline) => {
                StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE
            }
            VideoCodec::H264(H264Profile::Main) => {
                StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN
            }
            VideoCodec::H264(H264Profile::High) => {
                StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH
            }
        };

        let mut h264_profile_info =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);

        let video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut h264_profile_info);

        // 2. Query video capabilities
        let mut h264_encode_capabilities = vk::VideoEncodeH264CapabilitiesKHR::default();
        let mut encode_capabilities = vk::VideoEncodeCapabilitiesKHR::default();
        let mut capabilities = vk::VideoCapabilitiesKHR::default()
            .push_next(&mut encode_capabilities)
            .push_next(&mut h264_encode_capabilities);

        unsafe {
            (video_queue_instance_loader
                .fp()
                .get_physical_device_video_capabilities_khr)(
                vulkan_device.physical_device(),
                &video_profile,
                &mut capabilities,
            )
        }
        .result()
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to query video encode capabilities: {e}"))
        })?;

        tracing::info!(
            "Video encode capabilities: max_coded_extent={}x{}, max_dpb_slots={}, max_active_refs={}",
            capabilities.max_coded_extent.width,
            capabilities.max_coded_extent.height,
            capabilities.max_dpb_slots,
            capabilities.max_active_reference_pictures
        );

        // Validate requested dimensions fit within hardware limits
        if config.width > capabilities.max_coded_extent.width
            || config.height > capabilities.max_coded_extent.height
        {
            return Err(StreamError::GpuError(format!(
                "Requested encode size {}x{} exceeds hardware max {}x{}",
                config.width,
                config.height,
                capabilities.max_coded_extent.width,
                capabilities.max_coded_extent.height
            )));
        }

        // 3. Create video session
        // Use 1 DPB slot for IPP encoding (no B-frames), capped to hardware max
        let dpb_slots = 1u32.min(capabilities.max_dpb_slots);
        let active_refs = 1u32.min(capabilities.max_active_reference_pictures);

        let mut h264_session_create_info =
            vk::VideoEncodeH264SessionCreateInfoKHR::default()
                .use_max_level_idc(false);

        let session_create_info = vk::VideoSessionCreateInfoKHR::default()
            .queue_family_index(ve_family)
            .video_profile(&video_profile)
            .picture_format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .max_coded_extent(vk::Extent2D {
                width: config.width,
                height: config.height,
            })
            .reference_picture_format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .max_dpb_slots(dpb_slots)
            .max_active_reference_pictures(active_refs)
            .std_header_version(&capabilities.std_header_version)
            .push_next(&mut h264_session_create_info);

        let video_session = unsafe {
            let mut session = MaybeUninit::uninit();
            (video_queue_loader.fp().create_video_session_khr)(
                device.handle(),
                &session_create_info,
                ptr::null(),
                session.as_mut_ptr(),
            )
            .result()
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create video session: {e}"))
            })?;
            session.assume_init()
        };

        tracing::info!("Vulkan Video session created (H.264, {}x{})", config.width, config.height);

        // 4. Bind video session memory
        let video_session_memory =
            Self::bind_session_memory(vulkan_device, &video_queue_loader, video_session)?;

        // 5. Create session parameters (SPS/PPS)
        let video_session_parameters = Self::create_session_parameters(
            &video_queue_loader,
            &device,
            video_session,
            config,
            std_profile_idc,
        )?;

        Ok(Self {
            device,
            video_queue_loader,
            video_session,
            video_session_parameters,
            video_session_memory,
            video_encode_queue_family_index: ve_family,
        })
    }

    fn bind_session_memory(
        vulkan_device: &VulkanDevice,
        video_queue_loader: &ash::khr::video_queue::Device,
        video_session: vk::VideoSessionKHR,
    ) -> Result<Vec<vk::DeviceMemory>> {
        let device = vulkan_device.device();

        // Query how many memory bindings the session needs
        let mut mem_req_count = 0u32;
        unsafe {
            (video_queue_loader
                .fp()
                .get_video_session_memory_requirements_khr)(
                device.handle(),
                video_session,
                &mut mem_req_count,
                ptr::null_mut(),
            )
        }
        .result()
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to query video session memory requirements count: {e}"
            ))
        })?;

        if mem_req_count == 0 {
            tracing::info!("Video session requires 0 memory bindings");
            return Ok(Vec::new());
        }

        // Fill the requirements
        let mut mem_reqs: Vec<vk::VideoSessionMemoryRequirementsKHR<'_>> =
            vec![vk::VideoSessionMemoryRequirementsKHR::default(); mem_req_count as usize];

        unsafe {
            (video_queue_loader
                .fp()
                .get_video_session_memory_requirements_khr)(
                device.handle(),
                video_session,
                &mut mem_req_count,
                mem_reqs.as_mut_ptr(),
            )
        }
        .result()
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to query video session memory requirements: {e}"
            ))
        })?;

        // Allocate and bind each memory requirement
        let mut allocations = Vec::with_capacity(mem_req_count as usize);
        let mut bind_infos = Vec::with_capacity(mem_req_count as usize);

        for req in &mem_reqs {
            // Video session memory may require specific memory types that aren't
            // necessarily DEVICE_LOCAL. Try DEVICE_LOCAL first, fall back to any
            // matching type.
            let memory_type_index = vulkan_device
                .find_memory_type(
                    req.memory_requirements.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                )
                .or_else(|_| {
                    vulkan_device.find_memory_type(
                        req.memory_requirements.memory_type_bits,
                        vk::MemoryPropertyFlags::empty(),
                    )
                })?;

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(req.memory_requirements.size)
                .memory_type_index(memory_type_index);

            let memory = unsafe { device.allocate_memory(&alloc_info, None) }.map_err(|e| {
                StreamError::GpuError(format!(
                    "Failed to allocate video session memory (bind_index={}): {e}",
                    req.memory_bind_index
                ))
            })?;

            bind_infos.push(
                vk::BindVideoSessionMemoryInfoKHR::default()
                    .memory_bind_index(req.memory_bind_index)
                    .memory(memory)
                    .memory_offset(0)
                    .memory_size(req.memory_requirements.size),
            );

            allocations.push(memory);
        }

        unsafe {
            (video_queue_loader.fp().bind_video_session_memory_khr)(
                device.handle(),
                video_session,
                bind_infos.len() as u32,
                bind_infos.as_ptr(),
            )
        }
        .result()
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to bind video session memory: {e}"))
        })?;

        tracing::info!(
            "Video session memory bound: {} allocations",
            allocations.len()
        );

        Ok(allocations)
    }

    fn create_session_parameters(
        video_queue_loader: &ash::khr::video_queue::Device,
        device: &ash::Device,
        video_session: vk::VideoSessionKHR,
        config: &VideoEncoderConfig,
        std_profile_idc: StdVideoH264ProfileIdc,
    ) -> Result<vk::VideoSessionParametersKHR> {
        // Select H.264 level based on resolution
        let level_idc = select_h264_level(config.width, config.height, config.fps);

        // Determine if CABAC is available (Main/High only, not Baseline)
        let is_baseline =
            std_profile_idc == StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE;

        // Build SPS
        let mut sps_flags = StdVideoH264SpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: Default::default(),
            __bindgen_padding_0: 0,
        };
        sps_flags.set_frame_mbs_only_flag(1);
        sps_flags.set_direct_8x8_inference_flag(1);
        // Enable frame cropping if height isn't a multiple of 16
        // (e.g. 1080 → 1088 needs 8 pixels cropped from bottom)
        if config.height % 16 != 0 {
            sps_flags.set_frame_cropping_flag(1);
        }
        if is_baseline {
            // Baseline: constraint_set0_flag signals Baseline conformance (H.264 A.2.1),
            // constraint_set1_flag signals Constrained Baseline (WebRTC profile-level-id 42e01f).
            sps_flags.set_constraint_set0_flag(1);
            sps_flags.set_constraint_set1_flag(1);
        } else if std_profile_idc == StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN {
            // Main: constraint_set1_flag signals Main profile conformance.
            sps_flags.set_constraint_set1_flag(1);
        }
        // High: no constraint flags — setting constraint_set1_flag on High
        // is an H.264 standard violation that NVIDIA hardware may reject.

        let sps = StdVideoH264SequenceParameterSet {
            flags: sps_flags,
            profile_idc: std_profile_idc,
            level_idc,
            chroma_format_idc:
                StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
            seq_parameter_set_id: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            max_num_ref_frames: 1,
            reserved1: 0,
            pic_width_in_mbs_minus1: (config.width + 15) / 16 - 1,
            pic_height_in_map_units_minus1: (config.height + 15) / 16 - 1,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            // Crop bottom to remove macroblock padding (e.g. 1080 → 1088 = 8 pixel pad).
            // In 4:2:0, crop units are 2 luma pixels, so divide by 2.
            frame_crop_bottom_offset: (((config.height + 15) / 16 * 16) - config.height) / 2,
            reserved2: 0,
            pOffsetForRefFrame: ptr::null(),
            pScalingLists: ptr::null(),
            pSequenceParameterSetVui: ptr::null(),
        };

        // Build PPS
        let mut pps_flags = StdVideoH264PpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: Default::default(),
            __bindgen_padding_0: [0; 3],
        };
        // CABAC for Main/High, CAVLC for Baseline
        if !is_baseline {
            pps_flags.set_entropy_coding_mode_flag(1);
        }

        let pps = StdVideoH264PictureParameterSet {
            flags: pps_flags,
            seq_parameter_set_id: 0,
            pic_parameter_set_id: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_bipred_idc:
                StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
            pic_init_qp_minus26: 0,
            pic_init_qs_minus26: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pScalingLists: ptr::null(),
        };

        // Build session parameters create info chain
        let add_info = vk::VideoEncodeH264SessionParametersAddInfoKHR::default()
            .std_sp_ss(std::slice::from_ref(&sps))
            .std_pp_ss(std::slice::from_ref(&pps));

        let mut h264_params_create_info =
            vk::VideoEncodeH264SessionParametersCreateInfoKHR::default()
                .max_std_sps_count(1)
                .max_std_pps_count(1)
                .parameters_add_info(&add_info);

        let params_create_info = vk::VideoSessionParametersCreateInfoKHR::default()
            .video_session(video_session)
            .push_next(&mut h264_params_create_info);

        let video_session_parameters = unsafe {
            let mut params = MaybeUninit::uninit();
            (video_queue_loader
                .fp()
                .create_video_session_parameters_khr)(
                device.handle(),
                &params_create_info,
                ptr::null(),
                params.as_mut_ptr(),
            )
            .result()
            .map_err(|e| {
                StreamError::GpuError(format!(
                    "Failed to create video session parameters: {e}"
                ))
            })?;
            params.assume_init()
        };

        tracing::info!(
            "Video session parameters created (profile={}, level={}, cabac={})",
            std_profile_idc,
            level_idc,
            !is_baseline
        );

        Ok(video_session_parameters)
    }

    /// Get the video session handle.
    #[allow(dead_code)]
    pub fn video_session(&self) -> vk::VideoSessionKHR {
        self.video_session
    }

    /// Get the video session parameters handle.
    #[allow(dead_code)]
    pub fn video_session_parameters(&self) -> vk::VideoSessionParametersKHR {
        self.video_session_parameters
    }

    /// Get the video encode queue family index.
    #[allow(dead_code)]
    pub fn video_encode_queue_family_index(&self) -> u32 {
        self.video_encode_queue_family_index
    }

    /// Get the video queue extension loader.
    #[allow(dead_code)]
    pub fn video_queue_loader(&self) -> &ash::khr::video_queue::Device {
        &self.video_queue_loader
    }

    /// Extract encoded SPS/PPS NAL units from the session parameters.
    ///
    /// Uses `vkGetEncodedVideoSessionParametersKHR` to get the driver-generated
    /// SPS and PPS as raw H.264 Annex B NAL units. These must be prepended to
    /// IDR frames since NVIDIA does not support `generate_prefix_nalu`.
    pub fn get_encoded_sps_pps(
        &self,
        video_encode_queue_loader: &ash::khr::video_encode_queue::Device,
    ) -> Result<Vec<u8>> {
        let mut h264_get_info = vk::VideoEncodeH264SessionParametersGetInfoKHR::default()
            .write_std_sps(true)
            .write_std_pps(true)
            .std_sps_id(0)
            .std_pps_id(0);

        let get_info = vk::VideoEncodeSessionParametersGetInfoKHR::default()
            .video_session_parameters(self.video_session_parameters)
            .push_next(&mut h264_get_info);

        let mut feedback_info = vk::VideoEncodeSessionParametersFeedbackInfoKHR::default();

        // First call: get the size
        let mut data_size: usize = 0;
        unsafe {
            (video_encode_queue_loader
                .fp()
                .get_encoded_video_session_parameters_khr)(
                self.device.handle(),
                &get_info,
                &mut feedback_info,
                &mut data_size,
                ptr::null_mut(),
            )
        }
        .result()
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to query encoded session parameters size: {e}"
            ))
        })?;

        if data_size == 0 {
            return Err(StreamError::GpuError(
                "Encoded session parameters returned 0 bytes".into(),
            ));
        }

        // Second call: get the data
        let mut data = vec![0u8; data_size];
        unsafe {
            (video_encode_queue_loader
                .fp()
                .get_encoded_video_session_parameters_khr)(
                self.device.handle(),
                &get_info,
                &mut feedback_info,
                &mut data_size,
                data.as_mut_ptr().cast(),
            )
        }
        .result()
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to get encoded session parameters: {e}"
            ))
        })?;

        data.truncate(data_size);
        tracing::info!(
            "Extracted SPS/PPS from session parameters: {} bytes",
            data.len()
        );

        Ok(data)
    }
}

/// Select H.264 level based on resolution and framerate.
/// For WebRTC/WHIP, Level 3.1 is the standard negotiated level (42e01f).
/// Most decoders accept streams at any level regardless of SDP, so we
/// default to 3.1 for maximum compatibility with WHIP providers like Cloudflare.
fn select_h264_level(_width: u32, _height: u32, _fps: u32) -> StdVideoH264LevelIdc {
    // Force Level 3.1 for WebRTC compatibility (Cloudflare profile-level-id: 42e01f).
    // The NVIDIA hardware encoder handles any resolution regardless of the SPS level.
    StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1
}

#[allow(dead_code)]
fn select_h264_level_by_resolution(width: u32, height: u32, fps: u32) -> StdVideoH264LevelIdc {
    let macroblocks_per_sec = ((width + 15) / 16) as u64 * ((height + 15) / 16) as u64 * fps as u64;

    if macroblocks_per_sec <= 108_000 {
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1
    } else if macroblocks_per_sec <= 245_760 {
        // Level 4.0: up to 1920x1080@30
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0
    } else {
        // Level 5.1: up to 4096x2160@30
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_1
    }
}

impl Drop for VulkanVideoSession {
    fn drop(&mut self) {
        unsafe {
            (self
                .video_queue_loader
                .fp()
                .destroy_video_session_parameters_khr)(
                self.device.handle(),
                self.video_session_parameters,
                ptr::null(),
            );

            (self.video_queue_loader.fp().destroy_video_session_khr)(
                self.device.handle(),
                self.video_session,
                ptr::null(),
            );

            for memory in &self.video_session_memory {
                self.device.free_memory(*memory, None);
            }
        }

        tracing::info!("Vulkan Video session destroyed");
    }
}

// VulkanVideoSession is Send because Vulkan handles are thread-safe
unsafe impl Send for VulkanVideoSession {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vulkan_video_session_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test — Vulkan not available");
                return;
            }
        };

        if !device.supports_video_encode() {
            println!("Skipping test — Vulkan Video encode not supported on this device");
            return;
        }

        let config = VideoEncoderConfig::new(1280, 720);
        let session = VulkanVideoSession::new(&device, &config);
        match &session {
            Ok(s) => {
                println!(
                    "Video session created: queue_family={}",
                    s.video_encode_queue_family_index()
                );
            }
            Err(e) => {
                println!("Video session creation failed: {e}");
            }
        }
        assert!(session.is_ok(), "Video session creation should succeed");

        // Drop tests cleanup
        drop(session);
        println!("Video session cleanup passed");
    }

    #[test]
    fn test_select_h264_level() {
        // 720p30 → Level 3.1
        assert_eq!(
            select_h264_level(1280, 720, 30),
            StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1
        );
        // 1080p30 → Level 4.0
        assert_eq!(
            select_h264_level(1920, 1080, 30),
            StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0
        );
        // 4K30 → Level 5.1
        assert_eq!(
            select_h264_level(3840, 2160, 30),
            StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_1
        );
    }
}
