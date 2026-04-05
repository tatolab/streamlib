// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use ash::vk;
use ash::vk::native::{
    StdVideoDecodeH264PictureInfo, StdVideoDecodeH264PictureInfoFlags,
    StdVideoDecodeH264ReferenceInfo, StdVideoDecodeH264ReferenceInfoFlags,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
};

use crate::_generated_::Videoframe;
use crate::core::codec::VideoDecoderConfig;
use crate::core::rhi::PixelFormat;
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use crate::vulkan::rhi::VulkanDevice;

use super::VulkanVideoDecodeSession;

/// Maximum bitstream input buffer size (256 KB — decode receives full access
/// unit NALs including SPS/PPS/SEI).
const BITSTREAM_INPUT_BUFFER_SIZE: vk::DeviceSize = 256 * 1024;

/// 5-second timeout for all GPU fence waits (detect hangs instead of deadlocking).
const FENCE_TIMEOUT_NS: u64 = 5_000_000_000;

/// GPU resources created lazily on first decode (when SPS/PPS are known).
struct DecodeResources {
    video_decode_session: VulkanVideoDecodeSession,
    // DPB images: decode output + reference frame storage + transfer source (coincide mode)
    dpb_images: Vec<vk::Image>,
    dpb_image_views: Vec<vk::ImageView>,
    dpb_image_memories: Vec<vk::DeviceMemory>,
    // Bitstream buffer: CPU-visible, holds incoming H.264 NAL data for GPU decode
    bitstream_input_buffer: vk::Buffer,
    bitstream_input_buffer_memory: vk::DeviceMemory,
    bitstream_input_buffer_mapped_ptr: *mut u8,
    // Decode queue command resources
    decode_command_pool: vk::CommandPool,
    decode_command_buffer: vk::CommandBuffer,
    decode_fence: vk::Fence,
    // Transfer command resources (image→buffer copy for readback, on graphics queue)
    transfer_command_pool: vk::CommandPool,
    transfer_command_buffer: vk::CommandBuffer,
    transfer_fence: vk::Fence,
    // Result status query pool
    decode_status_query_pool: vk::QueryPool,
    // Dimensions
    decode_width: u32,
    decode_height: u32,
    session_initialized: bool,
    // Which DPB slot was last decoded into (for transfer readback)
    last_decoded_dpb_slot: usize,
}

/// Vulkan Video H.264 decoder using GPU hardware decode.
pub struct VulkanVideoDecoder {
    config: VideoDecoderConfig,
    device: ash::Device,
    vulkan_device: Arc<VulkanDevice>,
    video_decode_queue: vk::Queue,
    vd_family: u32,
    video_queue_loader: ash::khr::video_queue::Device,
    video_decode_queue_loader: ash::khr::video_decode_queue::Device,
    frame_count: u64,
    /// Cached SPS NAL payload (after NAL header byte stripped).
    cached_sps: Option<Vec<u8>>,
    /// Cached PPS NAL payload (after NAL header byte stripped).
    cached_pps: Option<Vec<u8>>,
    /// DPB tracking: which slots have valid reference pictures.
    /// Maps slot_index → (frame_num, PicOrderCnt) for active references.
    dpb_slot_frame_numbers: Vec<Option<(u32, i32)>>,
    /// Cached NAL data when init_resources fails — replayed on next successful init.
    pending_nal_data: Option<(Vec<u8>, i64)>,
    /// Lazily initialized on first decode when SPS/PPS are known.
    resources: Option<DecodeResources>,
}

impl VulkanVideoDecoder {
    /// Create a new Vulkan Video decoder.
    pub fn new(
        config: VideoDecoderConfig,
        gpu_context: Option<GpuContext>,
        _ctx: &RuntimeContext,
    ) -> Result<Self> {
        let gpu = gpu_context.ok_or_else(|| {
            StreamError::Configuration("GPU context required for Vulkan Video decoder".into())
        })?;

        let vulkan_device: Arc<VulkanDevice> = Arc::clone(&gpu.device().inner);

        if !vulkan_device.supports_video_decode() {
            return Err(StreamError::Configuration(
                "Vulkan Video decode not supported on this device".into(),
            ));
        }

        let device = vulkan_device.device().clone();
        let instance = vulkan_device.instance();

        let video_decode_queue = vulkan_device.video_decode_queue().ok_or_else(|| {
            StreamError::Configuration("No video decode queue available".into())
        })?;
        let vd_family = vulkan_device
            .video_decode_queue_family_index()
            .ok_or_else(|| {
                StreamError::Configuration("No video decode queue family available".into())
            })?;

        let video_queue_loader = ash::khr::video_queue::Device::new(instance, &device);
        let video_decode_queue_loader =
            ash::khr::video_decode_queue::Device::new(instance, &device);

        tracing::info!(
            "VulkanVideoDecoder created (resources deferred until SPS/PPS received)"
        );

        Ok(Self {
            config,
            device,
            vulkan_device,
            video_decode_queue,
            vd_family,
            video_queue_loader,
            video_decode_queue_loader,
            frame_count: 0,
            cached_sps: None,
            cached_pps: None,
            dpb_slot_frame_numbers: Vec::new(),
            pending_nal_data: None,
            resources: None,
        })
    }

    /// Update decoder format with SPS/PPS parameter sets.
    pub fn update_format(&mut self, sps: &[u8], pps: &[u8]) -> Result<()> {
        self.cached_sps = Some(sps.to_vec());
        self.cached_pps = Some(pps.to_vec());
        // Drop existing resources — they will be re-created on next decode()
        self.resources = None;
        self.frame_count = 0;
        self.dpb_slot_frame_numbers.clear();
        tracing::info!(
            "VulkanVideoDecoder: format updated (sps={} bytes, pps={} bytes)",
            sps.len(),
            pps.len()
        );
        Ok(())
    }

    /// Initialize GPU resources for the given frame dimensions.
    fn init_resources(&mut self, width: u32, height: u32) -> Result<()> {
        let sps = self.cached_sps.as_ref().ok_or_else(|| {
            StreamError::Configuration("Cannot init decode resources: SPS not set".into())
        })?;
        let pps = self.cached_pps.as_ref().ok_or_else(|| {
            StreamError::Configuration("Cannot init decode resources: PPS not set".into())
        })?;

        let video_decode_session = VulkanVideoDecodeSession::new(
            &self.vulkan_device,
            width,
            height,
            sps,
            pps,
        )?;

        // Build video profile for image/buffer creation (required by Vulkan Video spec)
        let profile_idc = sps.first().copied().unwrap_or(100);
        let std_profile_idc = match profile_idc {
            66 => StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
            77 => StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
            _ => StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
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

        let profiles = [video_profile];
        let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);

        let gfx_family = self.vulkan_device.queue_family_index();

        // Create DPB images (coincide mode: DPB + decode output + transfer source)
        let max_dpb_slots = video_decode_session.max_dpb_slots();
        let mut dpb_images = Vec::with_capacity(max_dpb_slots as usize);
        let mut dpb_image_views = Vec::with_capacity(max_dpb_slots as usize);
        let mut dpb_image_memories = Vec::with_capacity(max_dpb_slots as usize);

        for slot_idx in 0..max_dpb_slots {
            let dpb_result = Self::create_nv12_image(
                &self.vulkan_device,
                width,
                height,
                vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR
                    | vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                    | vk::ImageUsageFlags::TRANSFER_SRC,
                &[self.vd_family, gfx_family],
                &mut profile_list,
            );
            let (dpb_image, dpb_image_memory) = match dpb_result {
                Ok(r) => r,
                Err(e) => {
                    // Clean up all resources allocated so far before propagating
                    tracing::warn!(
                        "VulkanVideoDecoder: DPB image {} allocation failed, cleaning up {} prior resources: {}",
                        slot_idx, dpb_images.len(), e
                    );
                    unsafe {
                        for view in &dpb_image_views {
                            self.device.destroy_image_view(*view, None);
                        }
                        for img in &dpb_images {
                            self.device.destroy_image(*img, None);
                        }
                        for mem in &dpb_image_memories {
                            self.vulkan_device.free_device_memory(*mem);
                        }
                    }
                    return Err(e);
                }
            };
            let dpb_image_view = match Self::create_image_view(
                &self.device,
                dpb_image,
                vk::Format::G8_B8R8_2PLANE_420_UNORM,
            ) {
                Ok(v) => v,
                Err(e) => {
                    unsafe {
                        self.device.destroy_image(dpb_image, None);
                        self.vulkan_device.free_device_memory(dpb_image_memory);
                        for view in &dpb_image_views {
                            self.device.destroy_image_view(*view, None);
                        }
                        for img in &dpb_images {
                            self.device.destroy_image(*img, None);
                        }
                        for mem in &dpb_image_memories {
                            self.vulkan_device.free_device_memory(*mem);
                        }
                    }
                    return Err(e);
                }
            };
            dpb_images.push(dpb_image);
            dpb_image_views.push(dpb_image_view);
            dpb_image_memories.push(dpb_image_memory);
        }

        // Create bitstream input buffer (CPU-visible, GPU reads for decode)
        let (bitstream_input_buffer, bitstream_input_buffer_memory, bitstream_input_buffer_mapped_ptr) =
            Self::create_bitstream_input_buffer(&self.vulkan_device, &mut profile_list)?;

        // Decode queue command pool
        let vd_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.vd_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let decode_command_pool =
            unsafe { self.device.create_command_pool(&vd_pool_info, None) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to create decode command pool: {e}"))
            })?;
        let vd_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(decode_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let vd_cmd_buffers =
            unsafe { self.device.allocate_command_buffers(&vd_cmd_alloc) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to allocate decode command buffer: {e}"))
            })?;
        let decode_command_buffer = vd_cmd_buffers[0];

        // Transfer command pool (for image→buffer copy on graphics queue)
        let gfx_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gfx_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let transfer_command_pool =
            unsafe { self.device.create_command_pool(&gfx_pool_info, None) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to create transfer command pool: {e}"))
            })?;
        let gfx_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(transfer_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let gfx_cmd_buffers =
            unsafe { self.device.allocate_command_buffers(&gfx_cmd_alloc) }.map_err(|e| {
                StreamError::GpuError(format!(
                    "Failed to allocate transfer command buffer: {e}"
                ))
            })?;
        let transfer_command_buffer = gfx_cmd_buffers[0];

        // Fences (unsignaled)
        let fence_info = vk::FenceCreateInfo::default();
        let decode_fence = unsafe { self.device.create_fence(&fence_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create decode fence: {e}")))?;
        let transfer_fence = unsafe { self.device.create_fence(&fence_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create transfer fence: {e}"))
            })?;

        // Result status query pool
        let mut status_h264_decode_profile_info = vk::VideoDecodeH264ProfileInfoKHR::default()
            .std_profile_idc(std_profile_idc)
            .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE);
        let mut status_video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut status_h264_decode_profile_info);
        let decode_status_query_pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::RESULT_STATUS_ONLY_KHR)
            .query_count(1)
            .push_next(&mut status_video_profile);
        let decode_status_query_pool = unsafe {
            self.device
                .create_query_pool(&decode_status_query_pool_info, None)
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to create decode status query pool: {e}"
            ))
        })?;

        // Initialize DPB tracking
        self.dpb_slot_frame_numbers = vec![None; max_dpb_slots as usize];

        tracing::info!(
            "VulkanVideoDecoder resources initialized: {}x{} H.264 on queue family {}, dpb_slots={}",
            width,
            height,
            self.vd_family,
            max_dpb_slots
        );

        self.resources = Some(DecodeResources {
            video_decode_session,
            dpb_images,
            dpb_image_views,
            dpb_image_memories,
            bitstream_input_buffer,
            bitstream_input_buffer_memory,
            bitstream_input_buffer_mapped_ptr,
            decode_command_pool,
            decode_command_buffer,
            decode_fence,
            transfer_command_pool,
            transfer_command_buffer,
            transfer_fence,
            decode_status_query_pool,
            decode_width: width,
            decode_height: height,
            session_initialized: false,
            last_decoded_dpb_slot: 0,
        });

        Ok(())
    }

    /// Decode H.264 NAL units to a video frame.
    pub fn decode(
        &mut self,
        nal_units_annex_b: &[u8],
        timestamp_ns: i64,
        gpu: &GpuContext,
    ) -> Result<Option<Videoframe>> {
        // 1. Lazy init resources if SPS/PPS are cached but resources not yet created
        if self.resources.is_none() {
            let sps = match self.cached_sps.as_ref() {
                Some(s) => s,
                None => return Ok(None),
            };
            if self.cached_pps.is_none() {
                return Ok(None);
            }
            let (coded_width, coded_height) = parse_sps_dimensions(sps)?;
            tracing::info!(
                "VulkanVideoDecoder: SPS-derived coded dimensions: {}x{}",
                coded_width,
                coded_height
            );
            if let Err(e) = self.init_resources(coded_width, coded_height) {
                tracing::warn!(
                    "VulkanVideoDecoder: init failed ({}), caching frame for retry",
                    e
                );
                self.pending_nal_data = Some((nal_units_annex_b.to_vec(), timestamp_ns));
                return Ok(None);
            }
        }

        // If there is cached NAL data from a prior init failure, decode it instead
        // of the current frame (the cached data is the IDR that triggered init).
        let pending_owned = self.pending_nal_data.take();
        let (active_nal_data, active_timestamp): (&[u8], i64) =
            if let Some((ref pending_data, pending_ts)) = pending_owned {
                tracing::info!(
                    "VulkanVideoDecoder: replaying cached frame ({} bytes) after successful init",
                    pending_data.len()
                );
                (pending_data.as_slice(), pending_ts)
            } else {
                (nal_units_annex_b, timestamp_ns)
            };

        let nal_data_size = active_nal_data.len();
        if nal_data_size == 0 {
            return Ok(None);
        }
        if nal_data_size as vk::DeviceSize > BITSTREAM_INPUT_BUFFER_SIZE {
            return Err(StreamError::GpuError(format!(
                "NAL unit data ({} bytes) exceeds bitstream buffer size ({})",
                nal_data_size, BITSTREAM_INPUT_BUFFER_SIZE
            )));
        }

        // Parse the NAL units to determine frame type for DPB management
        let mut nal_info = parse_nal_unit_info(active_nal_data);

        // For non-IDR frames, derive frame_num and POC from frame_count
        // (proper slice header parsing requires RBSP emulation prevention removal)
        if !nal_info.is_idr {
            let log2_max_frame_num = parse_log2_max_frame_num(self.cached_sps.as_deref())
                .unwrap_or(4);
            let max_frame_num = 1u32 << log2_max_frame_num;
            nal_info.frame_num = (self.frame_count as u32) % max_frame_num;
            nal_info.pic_order_cnt = (self.frame_count as i32) * 2;
        }

        tracing::info!(
            "VulkanVideoDecoder: received {} bytes (idr={}, ref={}, frame_num={}, poc={})",
            nal_data_size,
            nal_info.is_idr,
            nal_info.is_reference,
            nal_info.frame_num,
            nal_info.pic_order_cnt
        );

        // Guard: never decode a P-frame when DPB is empty (no reference frames)
        if !nal_info.is_idr && self.dpb_slot_frame_numbers.iter().all(|s| s.is_none()) {
            tracing::warn!(
                "VulkanVideoDecoder: dropping non-IDR frame — DPB is empty (no reference frames available)"
            );
            return Ok(None);
        }

        // 2. Copy only slice NAL data to GPU buffer (strip SPS/PPS — those are
        //    already in session parameters; NVIDIA rejects bitstream buffers
        //    containing non-slice NALs alongside slice data).
        let slice_start = nal_info.slice_data_offset;
        let slice_data = &active_nal_data[slice_start..];
        let slice_data_size = slice_data.len();
        if slice_data_size == 0 {
            tracing::warn!("VulkanVideoDecoder: no slice data found in packet (slice_data_offset={})", slice_start);
            return Ok(None);
        }
        if slice_data_size as vk::DeviceSize > BITSTREAM_INPUT_BUFFER_SIZE {
            return Err(StreamError::GpuError(format!(
                "Slice data ({} bytes) exceeds bitstream buffer size ({})",
                slice_data_size, BITSTREAM_INPUT_BUFFER_SIZE
            )));
        }
        tracing::info!(
            "VulkanVideoDecoder: copying {} bytes of slice data to GPU (stripped {} bytes of SPS/PPS/start codes)",
            slice_data_size,
            slice_start
        );
        let res = self.resources.as_ref().unwrap();
        unsafe {
            std::ptr::copy_nonoverlapping(
                slice_data.as_ptr(),
                res.bitstream_input_buffer_mapped_ptr,
                slice_data_size,
            );
        }

        // 3. Record and submit decode commands (using slice-only buffer size)
        tracing::info!("VulkanVideoDecoder: submitting decode commands (frame={}, slice_bytes={})", self.frame_count, slice_data_size);
        self.record_decode_commands(&nal_info, slice_data_size)?;
        let res = self.resources.as_ref().unwrap();
        unsafe {
            let cmds = [res.decode_command_buffer];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            self.device
                .queue_submit(self.video_decode_queue, &[submit], res.decode_fence)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to submit decode commands: {e}"))
                })?;
        }

        // 5. Wait for decode to complete
        let res = self.resources.as_ref().unwrap();
        unsafe {
            self.device
                .wait_for_fences(&[res.decode_fence], true, FENCE_TIMEOUT_NS)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Decode fence wait timeout (5s) or error: {e}"
                    ))
                })?;
            self.device
                .reset_fences(&[res.decode_fence])
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset decode fence: {e}"))
                })?;
        }

        // Check decode status query result
        {
            let res = self.resources.as_ref().unwrap();
            let mut status_data: [u32; 1] = [0];
            let query_result = unsafe {
                self.device.get_query_pool_results(
                    res.decode_status_query_pool,
                    0,
                    &mut status_data,
                    vk::QueryResultFlags::WITH_STATUS_KHR,
                )
            };
            // Status: VK_QUERY_RESULT_STATUS_COMPLETE_KHR = 1, NEGATIVE = error
            let status = status_data[0] as i32;
            if query_result.is_err() || status < 0 {
                tracing::error!(
                    "VulkanVideoDecoder: decode status query failed (status={}, result={:?})",
                    status,
                    query_result
                );
                return Ok(None);
            }
            tracing::info!(
                "VulkanVideoDecoder: decode status OK (status={}), transferring NV12 output (frame={})",
                status,
                self.frame_count
            );
        }

        // 6. Copy decoded NV12 image to pixel buffer
        let res = self.resources.as_ref().unwrap();
        let decode_width = res.decode_width;
        let decode_height = res.decode_height;

        let (pool_id, dest_buffer) =
            gpu.acquire_pixel_buffer(decode_width, decode_height, PixelFormat::Nv12VideoRange)?;

        let dest_vk_buffer = dest_buffer.buffer_ref().inner.buffer();

        self.record_transfer_commands(dest_vk_buffer)?;
        let res = self.resources.as_ref().unwrap();
        let gfx_queue = self.vulkan_device.graphics_queue_secondary().unwrap_or(self.vulkan_device.queue());
        unsafe {
            let cmds = [res.transfer_command_buffer];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            self.device
                .queue_submit(gfx_queue, &[submit], res.transfer_fence)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to submit transfer commands: {e}"))
                })?;
            self.device
                .wait_for_fences(&[res.transfer_fence], true, FENCE_TIMEOUT_NS)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "Transfer fence timeout (5s) or error: {e}"
                    ))
                })?;
            self.device
                .reset_fences(&[res.transfer_fence])
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset transfer fence: {e}"))
                })?;
        }

        // 7. Update DPB
        if nal_info.is_idr {
            tracing::info!("VulkanVideoDecoder: IDR frame — clearing all DPB slots");
            // IDR: clear all DPB slots
            for slot in self.dpb_slot_frame_numbers.iter_mut() {
                *slot = None;
            }
        }
        if nal_info.is_reference {
            // Find empty slot or evict oldest
            let setup_slot = self
                .dpb_slot_frame_numbers
                .iter()
                .position(|s| s.is_none())
                .unwrap_or(0);
            if setup_slot < self.dpb_slot_frame_numbers.len() {
                let evicted = self.dpb_slot_frame_numbers[setup_slot];
                self.dpb_slot_frame_numbers[setup_slot] =
                    Some((nal_info.frame_num, nal_info.pic_order_cnt));
                tracing::info!(
                    "VulkanVideoDecoder: DPB update — stored ref in slot {} (frame_num={}, poc={}), evicted={:?}",
                    setup_slot,
                    nal_info.frame_num,
                    nal_info.pic_order_cnt,
                    evicted
                );
            }
        }

        self.frame_count += 1;

        tracing::info!(
            "VulkanVideoDecoder: decoded frame {} ({}x{}, idr={}, ref={})",
            self.frame_count - 1,
            decode_width,
            decode_height,
            nal_info.is_idr,
            nal_info.is_reference
        );

        Ok(Some(Videoframe {
            width: decode_width,
            height: decode_height,
            surface_id: pool_id.to_string(),
            timestamp_ns: active_timestamp.to_string(),
            frame_index: (self.frame_count - 1).to_string(),
        }))
    }

    /// Record decode commands on the video decode queue command buffer.
    fn record_decode_commands(
        &mut self,
        nal_info: &NalUnitInfo,
        nal_data_size: usize,
    ) -> Result<()> {
        // Compute setup slot index and snapshot DPB state before borrowing resources mutably.
        let setup_slot_index = self.find_setup_slot_index(nal_info);
        let frame_count = self.frame_count;

        // Snapshot which DPB slots are active and their slot indices (for reference_slots).
        let active_dpb_slots: Vec<(usize, u32, i32)> = self
            .dpb_slot_frame_numbers
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.map(|(fnum, poc)| (i, fnum, poc)))
            .collect();

        // Log DPB state and reference frame list for this decode operation
        tracing::info!(
            "VulkanVideoDecoder: DPB state before decode (frame={}): setup_slot={}, active_refs={}, slots={:?}",
            frame_count,
            setup_slot_index,
            active_dpb_slots.len(),
            active_dpb_slots.iter().map(|(idx, fnum, poc)| format!("[slot{}:fnum={},poc={}]", idx, fnum, poc)).collect::<Vec<_>>().join(", ")
        );

        let res = self.resources.as_mut().unwrap();
        let decode_width = res.decode_width;
        let decode_height = res.decode_height;

        let cmd_buf = res.decode_command_buffer;

        unsafe {
            self.device
                .reset_command_buffer(cmd_buf, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset decode command buffer: {e}"))
                })?;

            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            self.device
                .begin_command_buffer(cmd_buf, &begin_info)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to begin decode command buffer: {e}"))
                })?;

            // Transition DPB images (coincide mode: setup slot → VIDEO_DECODE_DST,
            // reference slots → VIDEO_DECODE_DPB)
            let mut dpb_barriers = Vec::new();
            for (i, slot) in self.dpb_slot_frame_numbers.iter().enumerate() {
                if i == setup_slot_index {
                    // Setup slot is the decode output in coincide mode
                    let old_layout = if slot.is_some() {
                        vk::ImageLayout::VIDEO_DECODE_DPB_KHR
                    } else {
                        vk::ImageLayout::UNDEFINED
                    };
                    dpb_barriers.push(
                        vk::ImageMemoryBarrier::default()
                            .old_layout(old_layout)
                            .new_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
                            .src_access_mask(vk::AccessFlags::empty())
                            .dst_access_mask(vk::AccessFlags::MEMORY_WRITE)
                            .image(res.dpb_images[i])
                            .subresource_range(
                                vk::ImageSubresourceRange::default()
                                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                                    .level_count(1)
                                    .layer_count(1),
                            ),
                    );
                } else if slot.is_some() {
                    // Reference slots stay at DPB layout
                    dpb_barriers.push(
                        vk::ImageMemoryBarrier::default()
                            .old_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
                            .new_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
                            .src_access_mask(
                                vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                            )
                            .dst_access_mask(
                                vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                            )
                            .image(res.dpb_images[i])
                            .subresource_range(
                                vk::ImageSubresourceRange::default()
                                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                                    .level_count(1)
                                    .layer_count(1),
                            ),
                    );
                }
            }

            if !dpb_barriers.is_empty() {
                self.device.cmd_pipeline_barrier(
                    cmd_buf,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &dpb_barriers,
                );
            }

            // Build decode picture info
            let mut pic_info_flags = StdVideoDecodeH264PictureInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
                __bindgen_padding_0: [0; 3],
            };
            pic_info_flags.set_IdrPicFlag(if nal_info.is_idr { 1 } else { 0 });
            pic_info_flags.set_is_intra(if nal_info.is_idr { 1 } else { 0 });
            pic_info_flags.set_is_reference(if nal_info.is_reference { 1 } else { 0 });

            // Slice offset is 0 because the bitstream buffer contains only
            // the slice NAL data (SPS/PPS already stripped before GPU copy).
            let slice_offset: u32 = 0;

            let std_picture_info = StdVideoDecodeH264PictureInfo {
                flags: pic_info_flags,
                seq_parameter_set_id: 0,
                pic_parameter_set_id: 0,
                reserved1: 0,
                reserved2: 0,
                frame_num: nal_info.frame_num as u16,
                idr_pic_id: if nal_info.is_idr {
                    (frame_count & 0xFFFF) as u16
                } else {
                    0
                },
                PicOrderCnt: [nal_info.pic_order_cnt, nal_info.pic_order_cnt],
            };

            let mut h264_decode_picture_info = vk::VideoDecodeH264PictureInfoKHR::default()
                .std_picture_info(&std_picture_info)
                .slice_offsets(std::slice::from_ref(&slice_offset));

            // Output picture resource (coincide mode: same DPB image as setup slot)
            let output_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D {
                    width: decode_width,
                    height: decode_height,
                })
                .base_array_layer(0)
                .image_view_binding(res.dpb_image_views[setup_slot_index]);

            // Setup reference slot (where reconstructed picture goes for future reference)
            let mut setup_dpb_ref_flags = StdVideoDecodeH264ReferenceInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
                __bindgen_padding_0: [0; 3],
            };
            setup_dpb_ref_flags.set_top_field_flag(0);
            setup_dpb_ref_flags.set_bottom_field_flag(0);
            setup_dpb_ref_flags.set_used_for_long_term_reference(0);

            let setup_std_reference_info = StdVideoDecodeH264ReferenceInfo {
                flags: setup_dpb_ref_flags,
                FrameNum: nal_info.frame_num as u16,
                reserved: 0,
                PicOrderCnt: [nal_info.pic_order_cnt, nal_info.pic_order_cnt],
            };

            let mut setup_h264_dpb_slot_info = vk::VideoDecodeH264DpbSlotInfoKHR::default()
                .std_reference_info(&setup_std_reference_info);

            let setup_dpb_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D {
                    width: decode_width,
                    height: decode_height,
                })
                .base_array_layer(0)
                .image_view_binding(res.dpb_image_views[setup_slot_index]);

            let setup_reference_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(setup_slot_index as i32)
                .picture_resource(&setup_dpb_picture_resource)
                .push_next(&mut setup_h264_dpb_slot_info);

            // Build reference slots for active DPB entries.
            // All data structures must be built in separate passes to avoid
            // overlapping mutable borrows from the Vulkan builder pattern.
            let ref_count = active_dpb_slots.len();

            let mut ref_std_infos: Vec<StdVideoDecodeH264ReferenceInfo> =
                Vec::with_capacity(ref_count);
            let mut ref_picture_resources: Vec<vk::VideoPictureResourceInfoKHR<'_>> =
                Vec::with_capacity(ref_count);

            for &(slot_idx, ref_frame_num, ref_poc) in &active_dpb_slots {
                let mut ref_flags = StdVideoDecodeH264ReferenceInfoFlags {
                    _bitfield_align_1: [],
                    _bitfield_1: Default::default(),
                    __bindgen_padding_0: [0; 3],
                };
                ref_flags.set_top_field_flag(0);
                ref_flags.set_bottom_field_flag(0);
                ref_flags.set_used_for_long_term_reference(0);

                ref_std_infos.push(StdVideoDecodeH264ReferenceInfo {
                    flags: ref_flags,
                    FrameNum: ref_frame_num as u16,
                    reserved: 0,
                    PicOrderCnt: [ref_poc, ref_poc],
                });

                ref_picture_resources.push(
                    vk::VideoPictureResourceInfoKHR::default()
                        .coded_offset(vk::Offset2D { x: 0, y: 0 })
                        .coded_extent(vk::Extent2D {
                            width: decode_width,
                            height: decode_height,
                        })
                        .base_array_layer(0)
                        .image_view_binding(res.dpb_image_views[slot_idx]),
                );
            }

            // Build h264 slot infos in a separate pass (references ref_std_infos)
            let mut ref_h264_slot_infos: Vec<vk::VideoDecodeH264DpbSlotInfoKHR<'_>> =
                Vec::with_capacity(ref_count);
            for std_info in &ref_std_infos {
                ref_h264_slot_infos.push(
                    vk::VideoDecodeH264DpbSlotInfoKHR::default()
                        .std_reference_info(std_info),
                );
            }

            // Build reference slot infos in a final pass.
            // Cannot use push_next in a loop (overlapping mutable borrows),
            // so set p_next via raw pointer.
            let mut reference_slots: Vec<vk::VideoReferenceSlotInfoKHR<'_>> =
                Vec::with_capacity(ref_count);
            for i in 0..ref_count {
                let mut slot_info = vk::VideoReferenceSlotInfoKHR::default()
                    .slot_index(active_dpb_slots[i].0 as i32)
                    .picture_resource(&ref_picture_resources[i]);
                // Chain h264 DPB slot info via raw pointer (safe: all structures
                // live on the stack until cmd_decode_video_khr consumes them)
                slot_info.p_next = &ref_h264_slot_infos[i]
                    as *const vk::VideoDecodeH264DpbSlotInfoKHR<'_>
                    as *const std::ffi::c_void;
                reference_slots.push(slot_info);
            }

            // All slots that appear in the decode (setup + references)
            let mut all_slots_for_begin: Vec<vk::VideoReferenceSlotInfoKHR<'_>> =
                Vec::with_capacity(reference_slots.len() + 1);
            all_slots_for_begin.push(setup_reference_slot);
            all_slots_for_begin.extend_from_slice(&reference_slots);

            // Begin video coding session
            let begin_info = vk::VideoBeginCodingInfoKHR::default()
                .video_session(res.video_decode_session.video_session())
                .video_session_parameters(res.video_decode_session.video_session_parameters())
                .reference_slots(&all_slots_for_begin);

            // Reset query pool BEFORE entering video coding scope (VUID-vkCmdResetQueryPool-videocoding)
            self.device.cmd_reset_query_pool(cmd_buf, res.decode_status_query_pool, 0, 1);

            (self.video_queue_loader.fp().cmd_begin_video_coding_khr)(
                cmd_buf,
                &begin_info,
            );

            // Reset session on first frame
            if !res.session_initialized {
                tracing::info!("VulkanVideoDecoder: resetting video coding session (first frame)");
                let reset_info = vk::VideoCodingControlInfoKHR::default()
                    .flags(vk::VideoCodingControlFlagsKHR::RESET);
                (self.video_queue_loader.fp().cmd_control_video_coding_khr)(
                    cmd_buf,
                    &reset_info,
                );
                res.session_initialized = true;
            }

            // Begin decode status query
            self.device.cmd_begin_query(
                cmd_buf,
                res.decode_status_query_pool,
                0,
                vk::QueryControlFlags::empty(),
            );

            // Issue decode command
            let alignment = res.video_decode_session.min_bitstream_buffer_size_alignment();
            let aligned_range = if alignment > 0 {
                ((nal_data_size as vk::DeviceSize) + alignment - 1) & !(alignment - 1)
            } else {
                nal_data_size as vk::DeviceSize
            };
            let decode_info = vk::VideoDecodeInfoKHR::default()
                .src_buffer(res.bitstream_input_buffer)
                .src_buffer_offset(0)
                .src_buffer_range(aligned_range)
                .dst_picture_resource(output_picture_resource)
                .setup_reference_slot(&setup_reference_slot)
                .reference_slots(&reference_slots)
                .push_next(&mut h264_decode_picture_info);

            (self
                .video_decode_queue_loader
                .fp()
                .cmd_decode_video_khr)(cmd_buf, &decode_info);

            // End decode status query
            self.device.cmd_end_query(cmd_buf, res.decode_status_query_pool, 0);

            // End video coding session
            let end_info = vk::VideoEndCodingInfoKHR::default();
            (self.video_queue_loader.fp().cmd_end_video_coding_khr)(
                cmd_buf,
                &end_info,
            );

            self.device.end_command_buffer(cmd_buf).map_err(|e| {
                StreamError::GpuError(format!("Failed to end decode command buffer: {e}"))
            })?;
        }

        // Track which DPB slot was decoded into for transfer readback
        let res = self.resources.as_mut().unwrap();
        res.last_decoded_dpb_slot = setup_slot_index;

        Ok(())
    }

    /// Record image→buffer copy commands for NV12 readback on the graphics queue.
    fn record_transfer_commands(&self, dest_vk_buffer: vk::Buffer) -> Result<()> {
        let res = self.resources.as_ref().unwrap();
        let decode_width = res.decode_width;
        let decode_height = res.decode_height;
        let cmd_buf = res.transfer_command_buffer;

        unsafe {
            self.device
                .reset_command_buffer(cmd_buf, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset transfer command buffer: {e}"))
                })?;

            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            self.device
                .begin_command_buffer(cmd_buf, &begin_info)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to begin transfer command buffer: {e}"))
                })?;

            // Transition decoded DPB image to TRANSFER_SRC (coincide mode)
            let decoded_image = res.dpb_images[res.last_decoded_dpb_slot];
            let barrier_to_transfer = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .image(decoded_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );

            self.device.cmd_pipeline_barrier(
                cmd_buf,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_to_transfer],
            );

            // Copy Y plane (plane 0) from image to buffer
            let y_region = vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::PLANE_0)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: decode_width,
                    height: decode_height,
                    depth: 1,
                });

            // Copy UV plane (plane 1) from image to buffer
            let uv_offset = (decode_width * decode_height) as vk::DeviceSize;
            let uv_region = vk::BufferImageCopy::default()
                .buffer_offset(uv_offset)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::PLANE_1)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: decode_width / 2,
                    height: decode_height / 2,
                    depth: 1,
                });

            self.device.cmd_copy_image_to_buffer(
                cmd_buf,
                decoded_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dest_vk_buffer,
                &[y_region, uv_region],
            );

            // Transition DPB image back to VIDEO_DECODE_DPB_KHR for future reference use
            let barrier_back_to_dpb = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .image(decoded_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );

            self.device.cmd_pipeline_barrier(
                cmd_buf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_back_to_dpb],
            );

            self.device.end_command_buffer(cmd_buf).map_err(|e| {
                StreamError::GpuError(format!("Failed to end transfer command buffer: {e}"))
            })?;
        }

        Ok(())
    }

    /// Find the DPB slot index to use for the setup (reconstructed) picture.
    fn find_setup_slot_index(&self, nal_info: &NalUnitInfo) -> usize {
        if nal_info.is_idr {
            return 0;
        }
        // Find first empty slot
        self.dpb_slot_frame_numbers
            .iter()
            .position(|s| s.is_none())
            .unwrap_or(0)
    }

    fn create_nv12_image(
        vulkan_device: &VulkanDevice,
        width: u32,
        height: u32,
        usage: vk::ImageUsageFlags,
        queue_families: &[u32],
        video_profile_list: &mut vk::VideoProfileListInfoKHR<'_>,
    ) -> Result<(vk::Image, vk::DeviceMemory)> {
        let device = vulkan_device.device();

        let mut unique_families = queue_families.to_vec();
        unique_families.sort();
        unique_families.dedup();

        let sharing_mode = if unique_families.len() > 1 {
            vk::SharingMode::CONCURRENT
        } else {
            vk::SharingMode::EXCLUSIVE
        };

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(sharing_mode)
            .queue_family_indices(&unique_families)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(video_profile_list);

        let image = unsafe { device.create_image(&image_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create NV12 image: {e}")))?;

        let memory = vulkan_device
            .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
            .inspect_err(|_| {
                unsafe { device.destroy_image(image, None) };
            })?;

        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_image(image, None) };
            StreamError::GpuError(format!("Failed to bind NV12 image memory: {e}"))
        })?;

        Ok((image, memory))
    }

    fn create_image_view(
        device: &ash::Device,
        image: vk::Image,
        format: vk::Format,
    ) -> Result<vk::ImageView> {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );

        unsafe { device.create_image_view(&view_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create image view: {e}")))
    }

    fn create_bitstream_input_buffer(
        vulkan_device: &VulkanDevice,
        video_profile_list: &mut vk::VideoProfileListInfoKHR<'_>,
    ) -> Result<(vk::Buffer, vk::DeviceMemory, *mut u8)> {
        let device = vulkan_device.device();

        let buffer_info = vk::BufferCreateInfo::default()
            .size(BITSTREAM_INPUT_BUFFER_SIZE)
            .usage(vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(video_profile_list);

        let buffer = unsafe { device.create_buffer(&buffer_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create bitstream input buffer: {e}"))
            })?;

        let memory = vulkan_device
            .allocate_buffer_memory(
                buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                false,
            )
            .inspect_err(|_| {
                unsafe { device.destroy_buffer(buffer, None) };
            })?;

        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            StreamError::GpuError(format!("Failed to bind bitstream input buffer memory: {e}"))
        })?;

        let mapped_ptr =
            vulkan_device
                .map_device_memory(memory, BITSTREAM_INPUT_BUFFER_SIZE)
                .inspect_err(|_| {
                    vulkan_device.free_device_memory(memory);
                    unsafe { device.destroy_buffer(buffer, None) };
                })?;

        Ok((buffer, memory, mapped_ptr))
    }

    /// Get the decoder configuration.
    pub fn config(&self) -> &VideoDecoderConfig {
        &self.config
    }
}

impl Drop for VulkanVideoDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
        }

        if let Some(res) = self.resources.take() {
            unsafe {
                self.device.destroy_fence(res.decode_fence, None);
                self.device
                    .destroy_command_pool(res.decode_command_pool, None);
                self.device.destroy_fence(res.transfer_fence, None);
                self.device
                    .destroy_command_pool(res.transfer_command_pool, None);

                self.device
                    .destroy_query_pool(res.decode_status_query_pool, None);

                self.device
                    .destroy_buffer(res.bitstream_input_buffer, None);
                self.vulkan_device
                    .unmap_device_memory(res.bitstream_input_buffer_memory);
                self.vulkan_device
                    .free_device_memory(res.bitstream_input_buffer_memory);

                for i in (0..res.dpb_images.len()).rev() {
                    self.device
                        .destroy_image_view(res.dpb_image_views[i], None);
                    self.device.destroy_image(res.dpb_images[i], None);
                    self.vulkan_device
                        .free_device_memory(res.dpb_image_memories[i]);
                }
            }
            // video_decode_session drops automatically
        }

        tracing::info!("VulkanVideoDecoder destroyed");
    }
}

// VulkanVideoDecoder is Send because Vulkan handles are thread-safe
unsafe impl Send for VulkanVideoDecoder {}

// ---------------------------------------------------------------------------
// NAL unit parsing for DPB management
// ---------------------------------------------------------------------------

/// Parsed information from NAL units needed for decode command setup.
struct NalUnitInfo {
    is_idr: bool,
    is_reference: bool,
    frame_num: u32,
    pic_order_cnt: i32,
    /// Byte offset of the first slice data within the Annex B input.
    slice_data_offset: usize,
}

/// Parse NAL unit info from Annex B encoded data for decode DPB management.
fn parse_nal_unit_info(annex_b_data: &[u8]) -> NalUnitInfo {
    let mut is_idr = false;
    let mut is_reference = false;
    let mut frame_num = 0u32;
    let mut pic_order_cnt = 0i32;
    let mut slice_data_offset = 0usize;

    // Collect NAL types found for logging
    let mut nal_types_found: Vec<(u8, u8)> = Vec::new(); // (nal_unit_type, nal_ref_idc)

    // Scan for NAL unit start codes and identify types
    let mut i = 0;
    while i + 3 < annex_b_data.len() {
        // Look for 3-byte (00 00 01) or 4-byte (00 00 00 01) start codes
        let (start_code_len, found) = if i + 3 < annex_b_data.len()
            && annex_b_data[i] == 0
            && annex_b_data[i + 1] == 0
            && annex_b_data[i + 2] == 1
        {
            (3, true)
        } else if i + 4 <= annex_b_data.len()
            && annex_b_data[i] == 0
            && annex_b_data[i + 1] == 0
            && annex_b_data[i + 2] == 0
            && annex_b_data[i + 3] == 1
        {
            (4, true)
        } else {
            (0, false)
        };

        if !found {
            i += 1;
            continue;
        }

        let nal_header_pos = i + start_code_len;
        if nal_header_pos >= annex_b_data.len() {
            break;
        }

        let nal_header = annex_b_data[nal_header_pos];
        let nal_ref_idc = (nal_header >> 5) & 0x03;
        let nal_unit_type = nal_header & 0x1F;
        nal_types_found.push((nal_unit_type, nal_ref_idc));

        match nal_unit_type {
            5 => {
                // IDR slice — offset points to start code (Annex B required by NVIDIA)
                is_idr = true;
                is_reference = true;
                slice_data_offset = i;
                frame_num = 0;
                pic_order_cnt = 0;
            }
            1 => {
                // Non-IDR slice
                is_reference = nal_ref_idc > 0;
                if slice_data_offset == 0 {
                    slice_data_offset = i;
                }
                // frame_num and pic_order_cnt are set by the caller from frame_count
                // (proper slice header parsing requires RBSP emulation prevention removal)
            }
            _ => {}
        }

        i = nal_header_pos + 1;
    }

    let nal_type_names: Vec<String> = nal_types_found.iter().map(|(ntype, ref_idc)| {
        let name = match ntype {
            1 => "non-IDR slice",
            2 => "slice part A",
            3 => "slice part B",
            4 => "slice part C",
            5 => "IDR slice",
            6 => "SEI",
            7 => "SPS",
            8 => "PPS",
            9 => "AUD",
            _ => "other",
        };
        format!("{}(type={},ref_idc={})", name, ntype, ref_idc)
    }).collect();
    tracing::info!(
        "VulkanVideoDecoder: NAL units in packet: [{}], slice_data_offset={}",
        nal_type_names.join(", "),
        slice_data_offset
    );

    NalUnitInfo {
        is_idr,
        is_reference,
        frame_num,
        pic_order_cnt,
        slice_data_offset,
    }
}

/// Extract coded dimensions from SPS RBSP bytes.
///
/// Returns (width, height) in pixels from pic_width_in_mbs_minus1 and
/// pic_height_in_map_units_minus1 fields. These are the coded dimensions
/// (before crop is applied) and represent the actual image size the decoder
/// must allocate.
fn parse_sps_dimensions(sps_rbsp: &[u8]) -> Result<(u32, u32)> {
    if sps_rbsp.len() < 4 {
        return Err(StreamError::Configuration(
            "SPS too short to extract dimensions".into(),
        ));
    }

    let profile_idc = sps_rbsp[0];
    // bytes 1-2: constraint_flags, level_idc
    // byte 3+: exp-golomb fields

    let mut bit_pos: usize = 24; // skip profile_idc(8) + constraint_flags(8) + level_idc(8)

    // seq_parameter_set_id (ue)
    read_exp_golomb(sps_rbsp, &mut bit_pos)?;

    // High profile (100, 110, 122, 244, 44, 83, 86, 118, 128) has extra fields
    if matches!(profile_idc, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128) {
        let chroma_format_idc = read_exp_golomb(sps_rbsp, &mut bit_pos)?;
        if chroma_format_idc == 3 {
            bit_pos += 1; // separate_colour_plane_flag
        }
        read_exp_golomb(sps_rbsp, &mut bit_pos)?; // bit_depth_luma_minus8
        read_exp_golomb(sps_rbsp, &mut bit_pos)?; // bit_depth_chroma_minus8
        bit_pos += 1; // qpprime_y_zero_transform_bypass_flag
        let seq_scaling_matrix_present = read_bit(sps_rbsp, &mut bit_pos)?;
        if seq_scaling_matrix_present == 1 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for _ in 0..count {
                let present = read_bit(sps_rbsp, &mut bit_pos)?;
                if present == 1 {
                    skip_scaling_list(sps_rbsp, &mut bit_pos, if count <= 6 { 16 } else { 64 })?;
                }
            }
        }
    }

    // log2_max_frame_num_minus4 (ue)
    read_exp_golomb(sps_rbsp, &mut bit_pos)?;
    // pic_order_cnt_type (ue)
    let poc_type = read_exp_golomb(sps_rbsp, &mut bit_pos)?;
    if poc_type == 0 {
        read_exp_golomb(sps_rbsp, &mut bit_pos)?; // log2_max_pic_order_cnt_lsb_minus4
    } else if poc_type == 1 {
        bit_pos += 1; // delta_pic_order_always_zero_flag
        read_signed_exp_golomb(sps_rbsp, &mut bit_pos)?; // offset_for_non_ref_pic
        read_signed_exp_golomb(sps_rbsp, &mut bit_pos)?; // offset_for_top_to_bottom_field
        let num_ref_frames_in_poc_cycle = read_exp_golomb(sps_rbsp, &mut bit_pos)?;
        for _ in 0..num_ref_frames_in_poc_cycle {
            read_signed_exp_golomb(sps_rbsp, &mut bit_pos)?;
        }
    }

    // max_num_ref_frames (ue)
    read_exp_golomb(sps_rbsp, &mut bit_pos)?;
    // gaps_in_frame_num_value_allowed_flag
    bit_pos += 1;
    // pic_width_in_mbs_minus1 (ue)
    let pic_width_in_mbs_minus1 = read_exp_golomb(sps_rbsp, &mut bit_pos)?;
    // pic_height_in_map_units_minus1 (ue)
    let pic_height_in_map_units_minus1 = read_exp_golomb(sps_rbsp, &mut bit_pos)?;

    let width = (pic_width_in_mbs_minus1 + 1) * 16;
    let height = (pic_height_in_map_units_minus1 + 1) * 16;

    Ok((width, height))
}

fn read_bit(data: &[u8], bit_pos: &mut usize) -> Result<u32> {
    let byte_idx = *bit_pos / 8;
    if byte_idx >= data.len() {
        return Err(StreamError::Configuration("SPS bitstream truncated".into()));
    }
    let bit_idx = 7 - (*bit_pos % 8);
    let val = ((data[byte_idx] >> bit_idx) & 1) as u32;
    *bit_pos += 1;
    Ok(val)
}

fn read_exp_golomb(data: &[u8], bit_pos: &mut usize) -> Result<u32> {
    let mut leading_zeros = 0u32;
    loop {
        let bit = read_bit(data, bit_pos)?;
        if bit == 1 {
            break;
        }
        leading_zeros += 1;
        if leading_zeros > 31 {
            return Err(StreamError::Configuration("Invalid exp-golomb in SPS".into()));
        }
    }
    if leading_zeros == 0 {
        return Ok(0);
    }
    let mut suffix = 0u32;
    for _ in 0..leading_zeros {
        suffix = (suffix << 1) | read_bit(data, bit_pos)?;
    }
    Ok((1 << leading_zeros) - 1 + suffix)
}

fn read_signed_exp_golomb(data: &[u8], bit_pos: &mut usize) -> Result<i32> {
    let code_num = read_exp_golomb(data, bit_pos)?;
    let value = (code_num + 1).div_ceil(2) as i32;
    if code_num % 2 == 0 {
        Ok(-value)
    } else {
        Ok(value)
    }
}

fn skip_scaling_list(data: &[u8], bit_pos: &mut usize, size: usize) -> Result<()> {
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for _ in 0..size {
        if next_scale != 0 {
            let delta = read_signed_exp_golomb(data, bit_pos)?;
            next_scale = (last_scale + delta + 256) % 256;
        }
        last_scale = if next_scale == 0 { last_scale } else { next_scale };
    }
    Ok(())
}

/// Extract log2_max_frame_num_minus4 + 4 from SPS RBSP bytes.
fn parse_log2_max_frame_num(sps_rbsp: Option<&[u8]>) -> Result<u32> {
    let sps_rbsp = sps_rbsp.ok_or_else(|| {
        StreamError::Configuration("SPS not available for log2_max_frame_num".into())
    })?;
    if sps_rbsp.len() < 4 {
        return Err(StreamError::Configuration("SPS too short".into()));
    }

    let profile_idc = sps_rbsp[0];
    let mut bit_pos: usize = 24; // skip profile_idc(8) + constraint_flags(8) + level_idc(8)

    // seq_parameter_set_id (ue)
    read_exp_golomb(sps_rbsp, &mut bit_pos)?;

    if matches!(profile_idc, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128) {
        let chroma_format_idc = read_exp_golomb(sps_rbsp, &mut bit_pos)?;
        if chroma_format_idc == 3 {
            bit_pos += 1;
        }
        read_exp_golomb(sps_rbsp, &mut bit_pos)?; // bit_depth_luma_minus8
        read_exp_golomb(sps_rbsp, &mut bit_pos)?; // bit_depth_chroma_minus8
        bit_pos += 1; // qpprime_y_zero_transform_bypass_flag
        let seq_scaling_matrix_present = read_bit(sps_rbsp, &mut bit_pos)?;
        if seq_scaling_matrix_present == 1 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for _ in 0..count {
                let present = read_bit(sps_rbsp, &mut bit_pos)?;
                if present == 1 {
                    skip_scaling_list(sps_rbsp, &mut bit_pos, if count <= 6 { 16 } else { 64 })?;
                }
            }
        }
    }

    let log2_max_frame_num_minus4 = read_exp_golomb(sps_rbsp, &mut bit_pos)?;
    Ok(log2_max_frame_num_minus4 + 4)
}
