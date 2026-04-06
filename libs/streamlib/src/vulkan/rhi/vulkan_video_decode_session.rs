// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::mem::MaybeUninit;
use std::ptr;
use std::sync::Arc;

use ash::vk;
use ash::vk::native::{
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
    StdVideoH264LevelIdc,
    StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1,
    StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0,
    StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_1,
    StdVideoH264PictureParameterSet, StdVideoH264PocType,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_1,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2,
    StdVideoH264PpsFlags, StdVideoH264ProfileIdc,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
    StdVideoH264SequenceParameterSet, StdVideoH264SpsFlags,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
};

use crate::core::{Result, StreamError};

use super::VulkanDevice;

/// Vulkan Video session for H.264 decoding.
pub struct VulkanVideoDecodeSession {
    device: ash::Device,
    vulkan_device: Arc<VulkanDevice>,
    video_queue_loader: ash::khr::video_queue::Device,
    video_session: vk::VideoSessionKHR,
    video_session_parameters: vk::VideoSessionParametersKHR,
    video_session_memory: Vec<vk::DeviceMemory>,
    video_decode_queue_family_index: u32,
    max_dpb_slots: u32,
    max_active_reference_pictures: u32,
    min_bitstream_buffer_size_alignment: vk::DeviceSize,
}

impl VulkanVideoDecodeSession {
    /// Create a new Vulkan Video session for H.264 decoding.
    ///
    /// `sps_bytes` and `pps_bytes` are raw NAL unit payloads (after start code
    /// and NAL header byte have been stripped).
    pub fn new(
        vulkan_device: &Arc<VulkanDevice>,
        width: u32,
        height: u32,
        sps_bytes: &[u8],
        pps_bytes: &[u8],
    ) -> Result<Self> {
        let vd_family = vulkan_device
            .video_decode_queue_family_index()
            .ok_or_else(|| {
                StreamError::GpuError("No video decode queue family available".into())
            })?;

        if !vulkan_device.supports_video_decode() {
            return Err(StreamError::GpuError(
                "Vulkan Video decode extensions not available".into(),
            ));
        }

        let device = vulkan_device.device().clone();

        let video_queue_instance_loader =
            ash::khr::video_queue::Instance::new(vulkan_device.entry(), vulkan_device.instance());
        let video_queue_loader =
            ash::khr::video_queue::Device::new(vulkan_device.instance(), &device);

        // 1. Parse profile_idc from SPS to build the correct video profile chain.
        let parsed_sps = parse_sps(sps_bytes)?;
        let std_profile_idc = match parsed_sps.profile_idc {
            66 => StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
            77 => StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
            100 => StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
            other => {
                tracing::warn!(
                    "Unknown H.264 profile_idc {} in SPS, defaulting to High",
                    other
                );
                StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH
            }
        };

        let mut h264_decode_profile_info = vk::VideoDecodeH264ProfileInfoKHR::default()
            .std_profile_idc(std_profile_idc)
            .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE);

        let video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut h264_decode_profile_info);

        // 2. Query video decode capabilities
        let mut h264_decode_capabilities = vk::VideoDecodeH264CapabilitiesKHR::default();
        let mut decode_capabilities = vk::VideoDecodeCapabilitiesKHR::default();
        let mut capabilities = vk::VideoCapabilitiesKHR::default()
            .push_next(&mut decode_capabilities)
            .push_next(&mut h264_decode_capabilities);

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
            StreamError::GpuError(format!("Failed to query video decode capabilities: {e}"))
        })?;

        tracing::info!(
            "Video decode capabilities: max_coded_extent={}x{}, max_dpb_slots={}, max_active_refs={}",
            capabilities.max_coded_extent.width,
            capabilities.max_coded_extent.height,
            capabilities.max_dpb_slots,
            capabilities.max_active_reference_pictures
        );

        tracing::info!(
            "SPS parsed: profile_idc={}, level_idc={}, max_num_ref_frames={}, \
             pic_width_in_mbs_minus1={}, pic_height_in_map_units_minus1={}, \
             log2_max_frame_num_minus4={}, pic_order_cnt_type={}, \
             log2_max_pic_order_cnt_lsb_minus4={}, frame_mbs_only={}, \
             direct_8x8_inference={}, bit_depth_luma_minus8={}, bit_depth_chroma_minus8={}, \
             crop=[{},{},{},{}], constraint_set=[{},{}]",
            parsed_sps.profile_idc,
            parsed_sps.level_idc,
            parsed_sps.max_num_ref_frames,
            parsed_sps.pic_width_in_mbs_minus1,
            parsed_sps.pic_height_in_map_units_minus1,
            parsed_sps.log2_max_frame_num_minus4,
            parsed_sps.pic_order_cnt_type,
            parsed_sps.log2_max_pic_order_cnt_lsb_minus4,
            parsed_sps.frame_mbs_only_flag,
            parsed_sps.direct_8x8_inference_flag,
            parsed_sps.bit_depth_luma_minus8,
            parsed_sps.bit_depth_chroma_minus8,
            parsed_sps.frame_crop_left_offset,
            parsed_sps.frame_crop_right_offset,
            parsed_sps.frame_crop_top_offset,
            parsed_sps.frame_crop_bottom_offset,
            parsed_sps.constraint_set0_flag,
            parsed_sps.constraint_set1_flag,
        );

        let sps_derived_width = (parsed_sps.pic_width_in_mbs_minus1 + 1) * 16;
        let sps_derived_height = (parsed_sps.pic_height_in_map_units_minus1 + 1) * 16;
        tracing::info!(
            "SPS derived resolution: {}x{} (before cropping)",
            sps_derived_width,
            sps_derived_height
        );

        if width > capabilities.max_coded_extent.width
            || height > capabilities.max_coded_extent.height
        {
            return Err(StreamError::GpuError(format!(
                "Requested decode size {}x{} exceeds hardware max {}x{}",
                width,
                height,
                capabilities.max_coded_extent.width,
                capabilities.max_coded_extent.height
            )));
        }

        // 3. Create video session
        // DPB slots: max_num_ref_frames from SPS + 1 (for the current reconstructed picture),
        // capped to hardware max.
        let desired_dpb_slots = (parsed_sps.max_num_ref_frames + 1).min(17);
        let max_dpb_slots = desired_dpb_slots.min(capabilities.max_dpb_slots);
        let max_active_reference_pictures = parsed_sps
            .max_num_ref_frames
            .min(capabilities.max_active_reference_pictures);

        let session_create_info = vk::VideoSessionCreateInfoKHR::default()
            .queue_family_index(vd_family)
            .video_profile(&video_profile)
            .picture_format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .max_coded_extent(vk::Extent2D { width, height })
            .reference_picture_format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .max_dpb_slots(max_dpb_slots)
            .max_active_reference_pictures(max_active_reference_pictures)
            .std_header_version(&capabilities.std_header_version);

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
                StreamError::GpuError(format!("Failed to create video decode session: {e}"))
            })?;
            session.assume_init()
        };

        tracing::info!(
            "Vulkan Video decode session created (H.264 profile={}, {}x{}, \
             dpb_slots={} (sps_max_ref={}+1={}, hw_max={}), max_active_refs={} (sps={}, hw={}))",
            parsed_sps.profile_idc,
            width,
            height,
            max_dpb_slots,
            parsed_sps.max_num_ref_frames,
            desired_dpb_slots,
            capabilities.max_dpb_slots,
            max_active_reference_pictures,
            parsed_sps.max_num_ref_frames,
            capabilities.max_active_reference_pictures
        );

        // 4. Bind video session memory
        let video_session_memory =
            Self::bind_session_memory(vulkan_device, &video_queue_loader, video_session)?;

        // 5. Create session parameters from SPS/PPS
        let video_session_parameters = Self::create_decode_session_parameters(
            &video_queue_loader,
            &device,
            video_session,
            &parsed_sps,
            pps_bytes,
            std_profile_idc,
        )?;

        Ok(Self {
            device,
            vulkan_device: Arc::clone(vulkan_device),
            video_queue_loader,
            video_session,
            video_session_parameters,
            video_session_memory,
            video_decode_queue_family_index: vd_family,
            max_dpb_slots,
            max_active_reference_pictures,
            min_bitstream_buffer_size_alignment: capabilities.min_bitstream_buffer_size_alignment,
        })
    }

    fn bind_session_memory(
        vulkan_device: &VulkanDevice,
        video_queue_loader: &ash::khr::video_queue::Device,
        video_session: vk::VideoSessionKHR,
    ) -> Result<Vec<vk::DeviceMemory>> {
        let device = vulkan_device.device();

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
            tracing::info!("Video decode session requires 0 memory bindings");
            return Ok(Vec::new());
        }

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

        let mut allocations = Vec::with_capacity(mem_req_count as usize);
        let mut bind_infos = Vec::with_capacity(mem_req_count as usize);

        for req in &mem_reqs {
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

            let memory = vulkan_device.allocate_session_memory(
                req.memory_requirements.size,
                memory_type_index,
            )?;

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
            "Video decode session memory bound: {} allocations",
            allocations.len()
        );

        Ok(allocations)
    }

    fn create_decode_session_parameters(
        video_queue_loader: &ash::khr::video_queue::Device,
        device: &ash::Device,
        video_session: vk::VideoSessionKHR,
        parsed_sps: &ParsedSps,
        pps_bytes: &[u8],
        std_profile_idc: StdVideoH264ProfileIdc,
    ) -> Result<vk::VideoSessionParametersKHR> {
        // Build StdVideoH264SequenceParameterSet from parsed SPS
        let mut sps_flags = StdVideoH264SpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: Default::default(),
            __bindgen_padding_0: 0,
        };
        sps_flags.set_frame_mbs_only_flag(parsed_sps.frame_mbs_only_flag as u32);
        sps_flags.set_direct_8x8_inference_flag(parsed_sps.direct_8x8_inference_flag as u32);
        if parsed_sps.frame_crop_bottom_offset > 0
            || parsed_sps.frame_crop_right_offset > 0
            || parsed_sps.frame_crop_top_offset > 0
            || parsed_sps.frame_crop_left_offset > 0
        {
            sps_flags.set_frame_cropping_flag(1);
        }
        sps_flags.set_constraint_set0_flag(parsed_sps.constraint_set0_flag as u32);
        sps_flags.set_constraint_set1_flag(parsed_sps.constraint_set1_flag as u32);

        let poc_type: StdVideoH264PocType = match parsed_sps.pic_order_cnt_type {
            0 => StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
            1 => StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_1,
            _ => StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2,
        };

        let level_idc = map_level_idc(parsed_sps.level_idc);

        let sps = StdVideoH264SequenceParameterSet {
            flags: sps_flags,
            profile_idc: std_profile_idc,
            level_idc,
            chroma_format_idc: StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
            seq_parameter_set_id: parsed_sps.seq_parameter_set_id as u8,
            bit_depth_luma_minus8: parsed_sps.bit_depth_luma_minus8 as u8,
            bit_depth_chroma_minus8: parsed_sps.bit_depth_chroma_minus8 as u8,
            log2_max_frame_num_minus4: parsed_sps.log2_max_frame_num_minus4 as u8,
            pic_order_cnt_type: poc_type,
            offset_for_non_ref_pic: parsed_sps.offset_for_non_ref_pic,
            offset_for_top_to_bottom_field: parsed_sps.offset_for_top_to_bottom_field,
            log2_max_pic_order_cnt_lsb_minus4: parsed_sps.log2_max_pic_order_cnt_lsb_minus4 as u8,
            num_ref_frames_in_pic_order_cnt_cycle: parsed_sps.num_ref_frames_in_pic_order_cnt_cycle
                as u8,
            max_num_ref_frames: parsed_sps.max_num_ref_frames as u8,
            reserved1: 0,
            pic_width_in_mbs_minus1: parsed_sps.pic_width_in_mbs_minus1,
            pic_height_in_map_units_minus1: parsed_sps.pic_height_in_map_units_minus1,
            frame_crop_left_offset: parsed_sps.frame_crop_left_offset,
            frame_crop_right_offset: parsed_sps.frame_crop_right_offset,
            frame_crop_top_offset: parsed_sps.frame_crop_top_offset,
            frame_crop_bottom_offset: parsed_sps.frame_crop_bottom_offset,
            reserved2: 0,
            pOffsetForRefFrame: ptr::null(),
            pScalingLists: ptr::null(),
            pSequenceParameterSetVui: ptr::null(),
        };

        // Build StdVideoH264PictureParameterSet from parsed PPS
        let parsed_pps = parse_pps(pps_bytes, parsed_sps.profile_idc)?;

        let mut pps_flags = StdVideoH264PpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: Default::default(),
            __bindgen_padding_0: [0; 3],
        };
        pps_flags.set_entropy_coding_mode_flag(parsed_pps.entropy_coding_mode_flag as u32);
        pps_flags
            .set_bottom_field_pic_order_in_frame_present_flag(
                parsed_pps.bottom_field_pic_order_in_frame_present_flag as u32,
            );
        pps_flags.set_weighted_pred_flag(parsed_pps.weighted_pred_flag as u32);
        pps_flags.set_deblocking_filter_control_present_flag(
            parsed_pps.deblocking_filter_control_present_flag as u32,
        );
        pps_flags
            .set_constrained_intra_pred_flag(parsed_pps.constrained_intra_pred_flag as u32);
        pps_flags
            .set_redundant_pic_cnt_present_flag(parsed_pps.redundant_pic_cnt_present_flag as u32);
        pps_flags.set_transform_8x8_mode_flag(parsed_pps.transform_8x8_mode_flag as u32);

        let weighted_bipred_idc = match parsed_pps.weighted_bipred_idc {
            1 => ash::vk::native::StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_EXPLICIT,
            2 => ash::vk::native::StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_IMPLICIT,
            _ => StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
        };

        let pps = StdVideoH264PictureParameterSet {
            flags: pps_flags,
            seq_parameter_set_id: parsed_pps.seq_parameter_set_id as u8,
            pic_parameter_set_id: parsed_pps.pic_parameter_set_id as u8,
            num_ref_idx_l0_default_active_minus1: parsed_pps.num_ref_idx_l0_default_active_minus1
                as u8,
            num_ref_idx_l1_default_active_minus1: parsed_pps.num_ref_idx_l1_default_active_minus1
                as u8,
            weighted_bipred_idc,
            pic_init_qp_minus26: parsed_pps.pic_init_qp_minus26 as i8,
            pic_init_qs_minus26: parsed_pps.pic_init_qs_minus26 as i8,
            chroma_qp_index_offset: parsed_pps.chroma_qp_index_offset as i8,
            second_chroma_qp_index_offset: parsed_pps.second_chroma_qp_index_offset as i8,
            pScalingLists: ptr::null(),
        };

        // Build decode session parameters
        let add_info = vk::VideoDecodeH264SessionParametersAddInfoKHR::default()
            .std_sp_ss(std::slice::from_ref(&sps))
            .std_pp_ss(std::slice::from_ref(&pps));

        let mut h264_params_create_info =
            vk::VideoDecodeH264SessionParametersCreateInfoKHR::default()
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
                    "Failed to create video decode session parameters: {e}"
                ))
            })?;
            params.assume_init()
        };

        tracing::info!(
            "PPS parsed: pps_id={}, sps_id={}, entropy_coding_mode={}, \
             num_ref_idx_l0_default_active_minus1={}, num_ref_idx_l1_default_active_minus1={}, \
             weighted_pred={}, pic_init_qp_minus26={}, chroma_qp_index_offset={}, \
             deblocking_filter_control={}, constrained_intra_pred={}, redundant_pic_cnt={}",
            parsed_pps.pic_parameter_set_id,
            parsed_pps.seq_parameter_set_id,
            parsed_pps.entropy_coding_mode_flag,
            parsed_pps.num_ref_idx_l0_default_active_minus1,
            parsed_pps.num_ref_idx_l1_default_active_minus1,
            parsed_pps.weighted_pred_flag,
            parsed_pps.pic_init_qp_minus26,
            parsed_pps.chroma_qp_index_offset,
            parsed_pps.deblocking_filter_control_present_flag,
            parsed_pps.constrained_intra_pred_flag,
            parsed_pps.redundant_pic_cnt_present_flag,
        );

        tracing::info!(
            "Video decode session parameters created (profile={}, level={}, sps_max_num_ref_frames={})",
            parsed_sps.profile_idc,
            parsed_sps.level_idc,
            parsed_sps.max_num_ref_frames
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

    /// Get the video decode queue family index.
    #[allow(dead_code)]
    pub fn video_decode_queue_family_index(&self) -> u32 {
        self.video_decode_queue_family_index
    }

    /// Get the video queue extension loader.
    #[allow(dead_code)]
    pub fn video_queue_loader(&self) -> &ash::khr::video_queue::Device {
        &self.video_queue_loader
    }

    /// Maximum DPB slots for this session.
    #[allow(dead_code)]
    pub fn max_dpb_slots(&self) -> u32 {
        self.max_dpb_slots
    }

    /// Maximum active reference pictures for this session.
    #[allow(dead_code)]
    pub fn max_active_reference_pictures(&self) -> u32 {
        self.max_active_reference_pictures
    }

    /// Minimum bitstream buffer size alignment from hardware capabilities.
    pub fn min_bitstream_buffer_size_alignment(&self) -> vk::DeviceSize {
        self.min_bitstream_buffer_size_alignment
    }
}

impl Drop for VulkanVideoDecodeSession {
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

            for &memory in &self.video_session_memory {
                self.vulkan_device.free_device_memory(memory);
            }
        }

        tracing::info!("Vulkan Video decode session destroyed");
    }
}

// VulkanVideoDecodeSession is Send because Vulkan handles are thread-safe
unsafe impl Send for VulkanVideoDecodeSession {}

// ---------------------------------------------------------------------------
// H.264 SPS/PPS bitstream parsing (inline, not a separate utility)
// ---------------------------------------------------------------------------

/// Parsed fields from an H.264 SPS NAL unit.
struct ParsedSps {
    profile_idc: u8,
    constraint_set0_flag: bool,
    constraint_set1_flag: bool,
    level_idc: u8,
    seq_parameter_set_id: u32,
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
    log2_max_frame_num_minus4: u32,
    pic_order_cnt_type: u32,
    log2_max_pic_order_cnt_lsb_minus4: u32,
    offset_for_non_ref_pic: i32,
    offset_for_top_to_bottom_field: i32,
    num_ref_frames_in_pic_order_cnt_cycle: u32,
    max_num_ref_frames: u32,
    pic_width_in_mbs_minus1: u32,
    pic_height_in_map_units_minus1: u32,
    frame_mbs_only_flag: bool,
    direct_8x8_inference_flag: bool,
    frame_crop_left_offset: u32,
    frame_crop_right_offset: u32,
    frame_crop_top_offset: u32,
    frame_crop_bottom_offset: u32,
}

/// Parsed fields from an H.264 PPS NAL unit.
struct ParsedPps {
    pic_parameter_set_id: u32,
    seq_parameter_set_id: u32,
    entropy_coding_mode_flag: bool,
    bottom_field_pic_order_in_frame_present_flag: bool,
    num_ref_idx_l0_default_active_minus1: u32,
    num_ref_idx_l1_default_active_minus1: u32,
    weighted_pred_flag: bool,
    weighted_bipred_idc: u32,
    pic_init_qp_minus26: i32,
    pic_init_qs_minus26: i32,
    chroma_qp_index_offset: i32,
    deblocking_filter_control_present_flag: bool,
    constrained_intra_pred_flag: bool,
    redundant_pic_cnt_present_flag: bool,
    transform_8x8_mode_flag: bool,
    second_chroma_qp_index_offset: i32,
}

/// Bitstream reader for exp-golomb and fixed-width field extraction.
struct BitstreamReader<'a> {
    data: &'a [u8],
    byte_offset: usize,
    bit_offset: u8,
}

impl<'a> BitstreamReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_offset: 0,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u32> {
        if self.byte_offset >= self.data.len() {
            return Err(StreamError::GpuError(
                "SPS/PPS bitstream truncated".into(),
            ));
        }
        let bit = ((self.data[self.byte_offset] >> (7 - self.bit_offset)) & 1) as u32;
        self.bit_offset += 1;
        if self.bit_offset == 8 {
            self.bit_offset = 0;
            self.byte_offset += 1;
        }
        Ok(bit)
    }

    fn read_bits(&mut self, n: u8) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }

    /// Read unsigned exp-golomb coded value.
    fn read_ue(&mut self) -> Result<u32> {
        let mut leading_zeros = 0u32;
        loop {
            let bit = self.read_bit()?;
            if bit == 1 {
                break;
            }
            leading_zeros += 1;
            if leading_zeros > 31 {
                return Err(StreamError::GpuError(
                    "Invalid exp-golomb code in SPS/PPS".into(),
                ));
            }
        }
        if leading_zeros == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(leading_zeros as u8)?;
        Ok((1 << leading_zeros) - 1 + suffix)
    }

    /// Read signed exp-golomb coded value.
    fn read_se(&mut self) -> Result<i32> {
        let code_num = self.read_ue()?;
        let value = (code_num + 1).div_ceil(2) as i32;
        if code_num % 2 == 0 {
            Ok(-value)
        } else {
            Ok(value)
        }
    }
}

/// Parse an H.264 SPS NAL unit payload (after NAL header byte is stripped).
fn parse_sps(sps_bytes: &[u8]) -> Result<ParsedSps> {
    if sps_bytes.len() < 4 {
        return Err(StreamError::GpuError(
            "SPS NAL unit too short (< 4 bytes)".into(),
        ));
    }

    // Fixed header: profile_idc(8), constraint_set_flags(8), level_idc(8)
    let profile_idc = sps_bytes[0];
    let constraint_flags = sps_bytes[1];
    let constraint_set0_flag = (constraint_flags & 0x80) != 0;
    let constraint_set1_flag = (constraint_flags & 0x40) != 0;
    let level_idc = sps_bytes[2];

    // Remaining fields are exp-golomb coded, starting after the 3 fixed bytes
    let mut reader = BitstreamReader::new(&sps_bytes[3..]);

    let seq_parameter_set_id = reader.read_ue()?;

    let mut bit_depth_luma_minus8 = 0u32;
    let mut bit_depth_chroma_minus8 = 0u32;

    // High profile and above have additional fields
    if profile_idc == 100 || profile_idc == 110 || profile_idc == 122 || profile_idc == 244
        || profile_idc == 44 || profile_idc == 83 || profile_idc == 86
        || profile_idc == 118 || profile_idc == 128
    {
        let chroma_format_idc = reader.read_ue()?;
        if chroma_format_idc == 3 {
            // separate_colour_plane_flag
            let _ = reader.read_bit()?;
        }
        bit_depth_luma_minus8 = reader.read_ue()?;
        bit_depth_chroma_minus8 = reader.read_ue()?;
        // qpprime_y_zero_transform_bypass_flag
        let _ = reader.read_bit()?;
        // seq_scaling_matrix_present_flag
        let scaling_matrix_present = reader.read_bit()?;
        if scaling_matrix_present == 1 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for _ in 0..count {
                let scaling_list_present = reader.read_bit()?;
                if scaling_list_present == 1 {
                    // Skip the scaling list (delta_scale exp-golomb values)
                    let size = if count <= 6 { 16 } else { 64 };
                    let mut last_scale = 8i32;
                    let mut next_scale = 8i32;
                    for _ in 0..size {
                        if next_scale != 0 {
                            let delta_scale = reader.read_se()?;
                            next_scale = (last_scale + delta_scale + 256) % 256;
                        }
                        if next_scale != 0 {
                            last_scale = next_scale;
                        }
                    }
                }
            }
        }
    }

    let log2_max_frame_num_minus4 = reader.read_ue()?;
    let pic_order_cnt_type = reader.read_ue()?;

    let mut log2_max_pic_order_cnt_lsb_minus4 = 0u32;
    let mut offset_for_non_ref_pic = 0i32;
    let mut offset_for_top_to_bottom_field = 0i32;
    let mut num_ref_frames_in_pic_order_cnt_cycle = 0u32;

    if pic_order_cnt_type == 0 {
        log2_max_pic_order_cnt_lsb_minus4 = reader.read_ue()?;
    } else if pic_order_cnt_type == 1 {
        // delta_pic_order_always_zero_flag
        let _ = reader.read_bit()?;
        offset_for_non_ref_pic = reader.read_se()?;
        offset_for_top_to_bottom_field = reader.read_se()?;
        num_ref_frames_in_pic_order_cnt_cycle = reader.read_ue()?;
        for _ in 0..num_ref_frames_in_pic_order_cnt_cycle {
            // offset_for_ref_frame[i]
            let _ = reader.read_se()?;
        }
    }

    let max_num_ref_frames = reader.read_ue()?;
    // gaps_in_frame_num_value_allowed_flag
    let _ = reader.read_bit()?;
    let pic_width_in_mbs_minus1 = reader.read_ue()?;
    let pic_height_in_map_units_minus1 = reader.read_ue()?;
    let frame_mbs_only_flag = reader.read_bit()? == 1;

    if !frame_mbs_only_flag {
        // mb_adaptive_frame_field_flag
        let _ = reader.read_bit()?;
    }
    // direct_8x8_inference_flag — present for all profiles
    let direct_8x8_inference_flag = reader.read_bit()? == 1;

    // Frame cropping
    let mut frame_crop_left_offset = 0u32;
    let mut frame_crop_right_offset = 0u32;
    let mut frame_crop_top_offset = 0u32;
    let mut frame_crop_bottom_offset = 0u32;

    let frame_cropping_flag = reader.read_bit()?;
    if frame_cropping_flag == 1 {
        frame_crop_left_offset = reader.read_ue()?;
        frame_crop_right_offset = reader.read_ue()?;
        frame_crop_top_offset = reader.read_ue()?;
        frame_crop_bottom_offset = reader.read_ue()?;
    }

    Ok(ParsedSps {
        profile_idc,
        constraint_set0_flag,
        constraint_set1_flag,
        level_idc,
        seq_parameter_set_id,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        log2_max_frame_num_minus4,
        pic_order_cnt_type,
        log2_max_pic_order_cnt_lsb_minus4,
        offset_for_non_ref_pic,
        offset_for_top_to_bottom_field,
        num_ref_frames_in_pic_order_cnt_cycle,
        max_num_ref_frames,
        pic_width_in_mbs_minus1,
        pic_height_in_map_units_minus1,
        frame_mbs_only_flag,
        direct_8x8_inference_flag,
        frame_crop_left_offset,
        frame_crop_right_offset,
        frame_crop_top_offset,
        frame_crop_bottom_offset,
    })
}

/// Parse an H.264 PPS NAL unit payload (after NAL header byte is stripped).
fn parse_pps(pps_bytes: &[u8], profile_idc: u8) -> Result<ParsedPps> {
    if pps_bytes.is_empty() {
        return Err(StreamError::GpuError(
            "PPS NAL unit is empty".into(),
        ));
    }

    let mut reader = BitstreamReader::new(pps_bytes);

    let pic_parameter_set_id = reader.read_ue()?;
    let seq_parameter_set_id = reader.read_ue()?;
    let entropy_coding_mode_flag = reader.read_bit()? == 1;
    let bottom_field_pic_order_in_frame_present_flag = reader.read_bit()? == 1;
    let num_slice_groups_minus1 = reader.read_ue()?;

    // Skip slice group map if present (rare in WebRTC streams)
    if num_slice_groups_minus1 > 0 {
        let slice_group_map_type = reader.read_ue()?;
        match slice_group_map_type {
            0 => {
                for _ in 0..=num_slice_groups_minus1 {
                    let _ = reader.read_ue()?;
                }
            }
            2 => {
                for _ in 0..num_slice_groups_minus1 {
                    let _ = reader.read_ue()?;
                    let _ = reader.read_ue()?;
                }
            }
            3..=5 => {
                let _ = reader.read_bit()?;
                let _ = reader.read_ue()?;
            }
            6 => {
                let pic_size_in_map_units = reader.read_ue()?;
                let bits_needed = ((num_slice_groups_minus1 + 1) as f64).log2().ceil() as u8;
                for _ in 0..=pic_size_in_map_units {
                    let _ = reader.read_bits(bits_needed)?;
                }
            }
            _ => {}
        }
    }

    let num_ref_idx_l0_default_active_minus1 = reader.read_ue()?;
    let num_ref_idx_l1_default_active_minus1 = reader.read_ue()?;
    let weighted_pred_flag = reader.read_bit()? == 1;
    let weighted_bipred_idc = reader.read_bits(2)?;
    let pic_init_qp_minus26 = reader.read_se()?;
    let pic_init_qs_minus26 = reader.read_se()?;
    let chroma_qp_index_offset = reader.read_se()?;
    let deblocking_filter_control_present_flag = reader.read_bit()? == 1;
    let constrained_intra_pred_flag = reader.read_bit()? == 1;
    let redundant_pic_cnt_present_flag = reader.read_bit()? == 1;

    // High profile additional fields (after redundant_pic_cnt_present_flag)
    let mut transform_8x8_mode_flag = false;
    let mut second_chroma_qp_index_offset = chroma_qp_index_offset;
    if profile_idc == 100 || profile_idc == 110 || profile_idc == 122 || profile_idc == 244
        || profile_idc == 44 || profile_idc == 83 || profile_idc == 86
        || profile_idc == 118 || profile_idc == 128
    {
        // more_rbsp_data() check — try to read, ignore errors at end of PPS
        if let Ok(flag) = reader.read_bit() {
            transform_8x8_mode_flag = flag == 1;
            // pic_scaling_matrix_present_flag
            if let Ok(scaling_present) = reader.read_bit() {
                if scaling_present == 1 {
                    let count = if transform_8x8_mode_flag { 6 + 2 } else { 6 };
                    for j in 0..count {
                        if let Ok(list_present) = reader.read_bit() {
                            if list_present == 1 {
                                let size = if j < 6 { 16 } else { 64 };
                                let mut last_scale = 8i32;
                                let mut next_scale = 8i32;
                                for _ in 0..size {
                                    if next_scale != 0 {
                                        if let Ok(delta) = reader.read_se() {
                                            next_scale = (last_scale + delta + 256) % 256;
                                        }
                                    }
                                    if next_scale != 0 {
                                        last_scale = next_scale;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Ok(val) = reader.read_se() {
                second_chroma_qp_index_offset = val;
            }
        }
    }

    Ok(ParsedPps {
        pic_parameter_set_id,
        seq_parameter_set_id,
        entropy_coding_mode_flag,
        bottom_field_pic_order_in_frame_present_flag,
        num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1,
        weighted_pred_flag,
        weighted_bipred_idc,
        pic_init_qp_minus26,
        pic_init_qs_minus26,
        chroma_qp_index_offset,
        deblocking_filter_control_present_flag,
        constrained_intra_pred_flag,
        redundant_pic_cnt_present_flag,
        transform_8x8_mode_flag,
        second_chroma_qp_index_offset,
    })
}

/// Map raw H.264 level_idc byte value to the ash native enum.
fn map_level_idc(level_idc: u8) -> StdVideoH264LevelIdc {
    match level_idc {
        31 => StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1,
        40 => StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0,
        51 => StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_1,
        // Default to 3.1 for WebRTC compatibility
        _ => StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1,
    }
}
