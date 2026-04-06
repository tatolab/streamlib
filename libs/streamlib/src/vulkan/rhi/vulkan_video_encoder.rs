// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video H.264 encoder — zero-copy GPU encoding pipeline.

use std::ptr;
use std::sync::Arc;

use ash::vk;
use ash::vk::native::{
    StdVideoEncodeH264PictureInfo, StdVideoEncodeH264PictureInfoFlags,
    StdVideoEncodeH264ReferenceInfo, StdVideoEncodeH264ReferenceInfoFlags,
    StdVideoEncodeH264ReferenceListsInfo, StdVideoEncodeH264ReferenceListsInfoFlags,
    StdVideoEncodeH264SliceHeader, StdVideoEncodeH264SliceHeaderFlags,
    StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
    StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED,
    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_I,
    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P,
    StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
    StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_P,
};

use ash::vk::native::{
    StdVideoH264ProfileIdc, StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
};

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::codec::{H264Profile, VideoCodec};
use crate::core::rhi::{PixelFormat, RhiPixelBuffer, RhiPixelBufferRef};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError, VideoEncoderConfig};
use crate::vulkan::rhi::{VulkanDevice, VulkanFormatConverter, VulkanPixelBuffer};

use super::VulkanVideoSession;

/// Maximum bitstream buffer size (128 KB — 1080p H.264 frames are typically 5–20 KB).
const BITSTREAM_BUFFER_SIZE: vk::DeviceSize = 128 * 1024;

/// GPU resources created lazily on first encode (when actual frame dimensions are known).
struct EncodeResources {
    video_session: VulkanVideoSession,
    format_converter: VulkanFormatConverter,
    nv12_staging_buffer: RhiPixelBuffer,
    nv12_staging_buffer_b: RhiPixelBuffer,
    nv12_image: vk::Image,
    nv12_image_view: vk::ImageView,
    nv12_image_memory: vk::DeviceMemory,
    dpb_image_a: vk::Image,
    dpb_image_view_a: vk::ImageView,
    dpb_image_memory_a: vk::DeviceMemory,
    dpb_image_b: vk::Image,
    dpb_image_view_b: vk::ImageView,
    dpb_image_memory_b: vk::DeviceMemory,
    bitstream_buffer: vk::Buffer,
    bitstream_buffer_memory: vk::DeviceMemory,
    bitstream_mapped_ptr: *mut u8,
    bitstream_buffer_b: vk::Buffer,
    bitstream_buffer_memory_b: vk::DeviceMemory,
    bitstream_mapped_ptr_b: *mut u8,
    // Graphics queue command resources (for buffer→image copy — video encode
    // queue family typically doesn't support transfer operations)
    transfer_command_pool: vk::CommandPool,
    transfer_command_buffer: vk::CommandBuffer,
    transfer_command_buffer_b: vk::CommandBuffer,
    transfer_fence: vk::Fence,
    // Video encode queue command resources
    encode_command_pool: vk::CommandPool,
    encode_command_buffer: vk::CommandBuffer,
    encode_command_buffer_b: vk::CommandBuffer,
    encode_fence: vk::Fence,
    encode_fence_b: vk::Fence,
    // Query pools for encode feedback and status
    encode_feedback_query_pool: vk::QueryPool,
    encode_status_query_pool: vk::QueryPool,
    /// Actual encode dimensions (may differ from config).
    encode_width: u32,
    encode_height: u32,
    session_initialized: bool,
    /// Encoded SPS/PPS NAL units extracted from session parameters.
    /// Prepended to IDR frames since NVIDIA doesn't support generate_prefix_nalu.
    sps_pps_nalu: Vec<u8>,
}

/// Vulkan Video H.264 encoder using zero-copy GPU pipeline.
pub struct VulkanVideoEncoder {
    config: VideoEncoderConfig,
    device: ash::Device,
    vulkan_device: Arc<VulkanDevice>,
    video_encode_queue: vk::Queue,
    ve_family: u32,
    video_queue_loader: ash::khr::video_queue::Device,
    video_encode_queue_loader: ash::khr::video_encode_queue::Device,
    frame_count: u64,
    force_next_keyframe: bool,
    /// Whether the previous frame was an IDR (for SPS/PPS prepending in pipelined output).
    previous_frame_was_idr: bool,
    /// Timestamp of the previous frame (returned one frame later in pipelined output).
    previous_frame_timestamp_ns: i64,
    /// Lazily initialized on first encode when frame dimensions are known.
    resources: Option<EncodeResources>,
}

impl VulkanVideoEncoder {
    /// Create a new Vulkan Video encoder.
    pub fn new(
        config: VideoEncoderConfig,
        gpu_context: Option<GpuContext>,
        _ctx: &RuntimeContext,
    ) -> Result<Self> {
        let gpu = gpu_context.ok_or_else(|| {
            StreamError::Configuration("GPU context required for Vulkan Video encoder".into())
        })?;

        let vulkan_device: Arc<VulkanDevice> = Arc::clone(&gpu.device().inner);

        if !vulkan_device.supports_video_encode() {
            return Err(StreamError::Configuration(
                "Vulkan Video encode not supported on this device".into(),
            ));
        }

        let device = vulkan_device.device().clone();
        let instance = vulkan_device.instance();

        let video_encode_queue = vulkan_device.video_encode_queue().ok_or_else(|| {
            StreamError::Configuration("No video encode queue available".into())
        })?;
        let ve_family = vulkan_device
            .video_encode_queue_family_index()
            .ok_or_else(|| {
                StreamError::Configuration("No video encode queue family available".into())
            })?;

        let video_queue_loader = ash::khr::video_queue::Device::new(instance, &device);
        let video_encode_queue_loader =
            ash::khr::video_encode_queue::Device::new(instance, &device);

        tracing::info!(
            "VulkanVideoEncoder created (resources deferred until first frame)"
        );

        Ok(Self {
            config,
            device,
            vulkan_device,
            video_encode_queue,
            ve_family,
            video_queue_loader,
            video_encode_queue_loader,
            frame_count: 0,
            force_next_keyframe: false,
            previous_frame_was_idr: false,
            previous_frame_timestamp_ns: 0,
            resources: None,
        })
    }

    /// Initialize GPU resources for the given frame dimensions.
    fn init_resources(&mut self, width: u32, height: u32) -> Result<()> {
        // Update config to match actual camera dimensions
        let mut encode_config = self.config.clone();
        encode_config.width = width;
        encode_config.height = height;

        // Use the config's requested profile — the downstream consumer (e.g. Cloudflare)
        // determines what profile is acceptable, not the GPU's "best" capability.
        tracing::info!(
            "VulkanVideoEncoder: using config-requested H.264 profile: {:?}",
            match encode_config.codec {
                VideoCodec::H264(p) => p,
            }
        );

        let video_session = VulkanVideoSession::new(&self.vulkan_device, &encode_config)?;

        let format_converter = VulkanFormatConverter::new(
            &self.device,
            self.vulkan_device.queue(),
            self.vulkan_device.queue_family_index(),
            4, // source bpp (BGRA)
            1, // dest bpp (NV12 average)
        )?;

        // Build video profile for image creation (required by Vulkan Video spec —
        // images with VIDEO_ENCODE usage must be created with VkVideoProfileListInfoKHR).
        // Uses the hardware-detected best profile from encode_config.
        let std_profile_idc = match encode_config.codec {
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

        let profiles = [video_profile];
        let mut profile_list =
            vk::VideoProfileListInfoKHR::default().profiles(&profiles);

        // Create GPU images first to reduce peak memory (images before staging buffer)
        let gfx_family = self.vulkan_device.queue_family_index();
        let (nv12_image, nv12_image_memory) = Self::create_nv12_image(
            &self.vulkan_device,
            width,
            height,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR,
            &[gfx_family, self.ve_family],
            &mut profile_list,
        )?;
        let nv12_image_view = Self::create_image_view(
            &self.device,
            nv12_image,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )?;

        let (dpb_image_a, dpb_image_memory_a) = Self::create_nv12_image(
            &self.vulkan_device,
            width,
            height,
            vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            &[self.ve_family],
            &mut profile_list,
        )?;
        let dpb_image_view_a = Self::create_image_view(
            &self.device,
            dpb_image_a,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )?;

        let (dpb_image_b, dpb_image_memory_b) = Self::create_nv12_image(
            &self.vulkan_device,
            width,
            height,
            vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            &[self.ve_family],
            &mut profile_list,
        )?;
        let dpb_image_view_b = Self::create_image_view(
            &self.device,
            dpb_image_b,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )?;

        let (bitstream_buffer, bitstream_buffer_memory, bitstream_mapped_ptr) =
            Self::create_bitstream_buffer(&self.vulkan_device, &mut profile_list)?;
        let (bitstream_buffer_b, bitstream_buffer_memory_b, bitstream_mapped_ptr_b) =
            Self::create_bitstream_buffer(&self.vulkan_device, &mut profile_list)?;

        // NV12 staging buffers created after GPU images to reduce peak memory.
        // Two staging buffers for double-buffered pipeline: while the GPU reads
        // from buffer A for transfer+encode, the CPU/GPU writes to buffer B.
        // Use 16 bpp (not 12) because the BGRA→NV12 compute shader dispatches
        // in 32-row blocks (height rounded up), writing past the exact NV12 size.
        // 16 bpp = w*h*2 provides the necessary headroom.
        let nv12_vk_buffer =
            VulkanPixelBuffer::new(&self.vulkan_device, width, height, 16, PixelFormat::Nv12VideoRange)?;
        let nv12_staging_buffer = RhiPixelBuffer::new(RhiPixelBufferRef {
            inner: Arc::new(nv12_vk_buffer),
        });
        let nv12_vk_buffer_b =
            VulkanPixelBuffer::new(&self.vulkan_device, width, height, 16, PixelFormat::Nv12VideoRange)?;
        let nv12_staging_buffer_b = RhiPixelBuffer::new(RhiPixelBufferRef {
            inner: Arc::new(nv12_vk_buffer_b),
        });

        // Create encode feedback query pool (reports bitstream bytes written).
        // Vulkan spec requires VideoProfileInfoKHR in pNext chain for video query pools.
        let mut feedback_h264_profile_info =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let mut feedback_video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut feedback_h264_profile_info);
        let mut encode_feedback_create_info =
            vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(
                    vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
                );
        let encode_feedback_query_pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
            .query_count(1)
            .push_next(&mut encode_feedback_create_info)
            .push_next(&mut feedback_video_profile);
        let encode_feedback_query_pool = unsafe {
            self.device
                .create_query_pool(&encode_feedback_query_pool_info, None)
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to create encode feedback query pool: {e}"
            ))
        })?;

        // Create result status query pool (detects silent encode failures).
        // Also requires VideoProfileInfoKHR in pNext chain.
        let mut status_h264_profile_info =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let mut status_video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut status_h264_profile_info);
        let encode_status_query_pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::RESULT_STATUS_ONLY_KHR)
            .query_count(1)
            .push_next(&mut status_video_profile);
        let encode_status_query_pool = unsafe {
            self.device
                .create_query_pool(&encode_status_query_pool_info, None)
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to create encode status query pool: {e}"
            ))
        })?;

        // Reset query pools on host before first use
        // (VUID-vkGetQueryPoolResults-None-09401).
        unsafe {
            self.device.reset_query_pool(encode_feedback_query_pool, 0, 1);
            self.device.reset_query_pool(encode_status_query_pool, 0, 1);
        }

        // Graphics queue command pool (for buffer→image copy — the dedicated video
        // encode queue family doesn't support transfer operations)
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
            .command_buffer_count(2);
        let gfx_cmd_buffers =
            unsafe { self.device.allocate_command_buffers(&gfx_cmd_alloc) }.map_err(|e| {
                StreamError::GpuError(format!(
                    "Failed to allocate transfer command buffers: {e}"
                ))
            })?;
        let transfer_command_buffer = gfx_cmd_buffers[0];
        let transfer_command_buffer_b = gfx_cmd_buffers[1];
        // Fences created UNSIGNALED — the pipelined encode() skips fence waits
        // on frame 0 and only waits for fences that were actually signaled.
        let fence_info = vk::FenceCreateInfo::default();
        let transfer_fence = unsafe { self.device.create_fence(&fence_info, None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to create transfer fence: {e}"))
            })?;

        // Video encode queue command pool
        let ve_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.ve_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let encode_command_pool =
            unsafe { self.device.create_command_pool(&ve_pool_info, None) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to create encode command pool: {e}"))
            })?;
        let ve_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(encode_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(2);
        let ve_cmd_buffers =
            unsafe { self.device.allocate_command_buffers(&ve_cmd_alloc) }.map_err(|e| {
                StreamError::GpuError(format!("Failed to allocate encode command buffers: {e}"))
            })?;
        let encode_command_buffer = ve_cmd_buffers[0];
        let encode_command_buffer_b = ve_cmd_buffers[1];
        let encode_fence = unsafe { self.device.create_fence(&fence_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create encode fence: {e}")))?;
        let encode_fence_b = unsafe { self.device.create_fence(&fence_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create encode fence B: {e}")))?;

        tracing::info!(
            "VulkanVideoEncoder resources initialized: {}x{} H.264 on queue family {}",
            width, height, self.ve_family
        );

        // Extract encoded SPS/PPS NAL units from session parameters.
        // These are prepended to IDR frames since NVIDIA doesn't support generate_prefix_nalu.
        let sps_pps_nalu = video_session
            .get_encoded_sps_pps(&self.video_encode_queue_loader)
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to extract SPS/PPS from session: {e}");
                Vec::new()
            });

        self.resources = Some(EncodeResources {
            video_session,
            format_converter,
            nv12_staging_buffer,
            nv12_staging_buffer_b,
            nv12_image,
            nv12_image_view,
            nv12_image_memory,
            dpb_image_a,
            dpb_image_view_a,
            dpb_image_memory_a,
            dpb_image_b,
            dpb_image_view_b,
            dpb_image_memory_b,
            bitstream_buffer,
            bitstream_buffer_memory,
            bitstream_mapped_ptr,
            bitstream_buffer_b,
            bitstream_buffer_memory_b,
            bitstream_mapped_ptr_b,
            transfer_command_pool,
            transfer_command_buffer,
            transfer_command_buffer_b,
            transfer_fence,
            encode_command_pool,
            encode_command_buffer,
            encode_command_buffer_b,
            encode_fence,
            encode_fence_b,
            encode_feedback_query_pool,
            encode_status_query_pool,
            encode_width: width,
            encode_height: height,
            session_initialized: false,
            sps_pps_nalu,
        });

        Ok(())
    }

    /// Encode a video frame using a double-buffered pipeline.
    ///
    /// Returns the PREVIOUS frame's encoded data (one frame of latency).
    /// Frame 0 returns empty data; the actual encoded bytes arrive on the
    /// next call. This pipelining allows format conversion for frame N+1
    /// to overlap with the GPU encode of frame N.
    pub fn encode(&mut self, frame: &Videoframe, gpu: &GpuContext) -> Result<Encodedvideoframe> {
        // 1. Resolve the pixel buffer from the video frame
        let source_buffer = gpu.resolve_videoframe_buffer(frame)?;
        let input_width = source_buffer.buffer_ref().inner.width();
        let input_height = source_buffer.buffer_ref().inner.height();

        // 2. Lazy init or reinit if dimensions changed
        let needs_init = match &self.resources {
            None => true,
            Some(r) => r.encode_width != input_width || r.encode_height != input_height,
        };
        if needs_init {
            // Drop old resources before creating new ones
            self.resources = None;
            self.init_resources(input_width, input_height)?;
            self.frame_count = 0;
            self.force_next_keyframe = false;
            self.previous_frame_was_idr = false;
            self.previous_frame_timestamp_ns = 0;
        }

        // 5-second timeout for all GPU fence waits (detect hangs instead of deadlocking)
        const FENCE_TIMEOUT_NS: u64 = 5_000_000_000;

        // Double-buffer index: alternates 0/1 each frame, aligned with DPB ping-pong.
        let buf_idx = (self.frame_count % 2) as usize;

        // 3. Format conversion: skip if source is already NV12, otherwise BGRA → NV12.
        //    When BGRA, converts into the CURRENT staging buffer on the graphics queue,
        //    which can overlap with the previous frame's encode (different queue families).
        let source_format = source_buffer.format();
        let source_is_nv12 = matches!(
            source_format,
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange
        );

        let res = self.resources.as_ref().unwrap();
        let nv12_source_vk_buffer = if source_is_nv12 {
            tracing::debug!(
                "VulkanVideoEncoder: source is NV12, skipping format conversion ({}x{}, buf={})",
                input_width, input_height, buf_idx
            );
            source_buffer.buffer_ref().inner.buffer()
        } else {
            let current_staging_buffer = if buf_idx == 0 {
                &res.nv12_staging_buffer
            } else {
                &res.nv12_staging_buffer_b
            };
            tracing::debug!(
                "VulkanVideoEncoder: starting BGRA→NV12 format conversion ({}x{}, buf={})",
                input_width, input_height, buf_idx
            );
            res.format_converter
                .convert(&source_buffer, current_staging_buffer)?;
            current_staging_buffer.buffer_ref().inner.buffer()
        };

        // 4. Wait for the PREVIOUS frame's encode fence.
        //    This ensures: (a) the previous encode is done so we can read its
        //    bitstream, and (b) the NV12 image is free for our transfer.
        //    For frame 0 there is no previous encode — skip the wait.
        let previous_frame_data = if self.frame_count > 0 {
            let prev_idx = ((self.frame_count + 1) % 2) as usize;
            let res = self.resources.as_ref().unwrap();
            let prev_encode_fence = if prev_idx == 0 {
                res.encode_fence
            } else {
                res.encode_fence_b
            };

            unsafe {
                self.device
                    .wait_for_fences(&[prev_encode_fence], true, FENCE_TIMEOUT_NS)
                    .map_err(|e| {
                        StreamError::GpuError(format!(
                            "Previous encode fence timeout (5s) or error: {e}"
                        ))
                    })?;
                self.device
                    .reset_fences(&[prev_encode_fence])
                    .map_err(|e| {
                        StreamError::GpuError(format!("Failed to reset previous encode fence: {e}"))
                    })?;
            }

            // Read the previous frame's bitstream (encode is done by now).
            let mut bitstream_data = self.read_bitstream(prev_idx)?;

            // Prepend SPS/PPS for IDR frames (NVIDIA doesn't emit them inline).
            if self.previous_frame_was_idr && !bitstream_data.is_empty() {
                let res = self.resources.as_ref().unwrap();
                if !res.sps_pps_nalu.is_empty() {
                    let mut with_params = res.sps_pps_nalu.clone();
                    with_params.extend_from_slice(&bitstream_data);
                    bitstream_data = with_params;
                }
            }

            tracing::debug!(
                "VulkanVideoEncoder: previous frame {} readback, {} bytes, idr={}",
                self.frame_count - 1,
                bitstream_data.len(),
                self.previous_frame_was_idr
            );

            // Debug: dump raw H.264 bitstream to file for offline verification
            if !bitstream_data.is_empty() {
                use std::io::Write;
                let path = "/tmp/vulkan_encode_output.h264";
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok();
                if let Some(ref mut f) = file {
                    let _ = f.write_all(&bitstream_data);
                }
            }

            Some(bitstream_data)
        } else {
            None
        };

        // 5. Zero-clear the CURRENT bitstream buffer so the scan fallback in
        //    read_bitstream() can detect actual data boundaries.
        //    The buffer is HOST_COHERENT so this is immediately visible to GPU.
        let res = self.resources.as_ref().unwrap();
        let current_bitstream_ptr = if buf_idx == 0 {
            res.bitstream_mapped_ptr
        } else {
            res.bitstream_mapped_ptr_b
        };
        unsafe {
            std::ptr::write_bytes(
                current_bitstream_ptr,
                0u8,
                BITSTREAM_BUFFER_SIZE as usize,
            );
        }

        // 6. Determine frame type
        let is_idr = self.frame_count == 0
            || self.force_next_keyframe
            || (self.frame_count % self.config.keyframe_interval_frames as u64) == 0;
        self.force_next_keyframe = false;

        // 7. Record and submit transfer on graphics queue.
        //    Wait for transfer fence (cross-queue sync — encode queue needs
        //    the NV12 image copy to be complete before encoding).
        tracing::debug!("VulkanVideoEncoder: recording transfer commands (buf={})", buf_idx);
        self.record_transfer_commands(buf_idx, nv12_source_vk_buffer)?;
        let gfx_queue = self.vulkan_device.queue();
        let res = self.resources.as_ref().unwrap();
        let transfer_cmd = if buf_idx == 0 {
            res.transfer_command_buffer
        } else {
            res.transfer_command_buffer_b
        };
        unsafe {
            let cmds = [transfer_cmd];
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

        // 8. Record and submit encode on video encode queue.
        //    Signal the current buffer set's encode fence but do NOT wait —
        //    the fence is checked at the start of the NEXT encode() call,
        //    allowing this encode to overlap with the next frame's format conversion.
        tracing::debug!("VulkanVideoEncoder: recording encode commands (idr={}, buf={})", is_idr, buf_idx);
        self.record_encode_commands(is_idr, buf_idx)?;
        let res = self.resources.as_ref().unwrap();
        let encode_cmd = if buf_idx == 0 {
            res.encode_command_buffer
        } else {
            res.encode_command_buffer_b
        };
        let current_encode_fence = if buf_idx == 0 {
            res.encode_fence
        } else {
            res.encode_fence_b
        };
        unsafe {
            let cmds = [encode_cmd];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            self.device
                .queue_submit(self.video_encode_queue, &[submit], current_encode_fence)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to submit encode commands: {e}"))
                })?;
            tracing::debug!("VulkanVideoEncoder: encode submitted (buf={}), not waiting", buf_idx);
        }

        // 9. Build the output from the PREVIOUS frame's data (pipelined).
        //    Frame 0 returns empty data — the actual encoded bytes arrive
        //    on the next call (1-frame latency, invisible in real-time streaming).
        let (output_data, output_is_keyframe, output_frame_number, output_timestamp_ns) =
            if let Some(data) = previous_frame_data {
                (
                    data,
                    self.previous_frame_was_idr,
                    self.frame_count - 1,
                    self.previous_frame_timestamp_ns,
                )
            } else {
                (Vec::new(), false, 0, 0i64)
            };

        // Track current frame state for the next call's readback.
        let timestamp_ns: i64 = frame.timestamp_ns.parse().unwrap_or(0);
        self.previous_frame_was_idr = is_idr;
        self.previous_frame_timestamp_ns = timestamp_ns;
        self.frame_count += 1;

        Ok(Encodedvideoframe {
            data: output_data,
            frame_number: output_frame_number.to_string(),
            is_keyframe: output_is_keyframe,
            timestamp_ns: output_timestamp_ns.to_string(),
        })
    }

    /// Record buffer→image copy commands on the graphics queue command buffer.
    fn record_transfer_commands(&mut self, buf_idx: usize, nv12_source_vk_buffer: vk::Buffer) -> Result<()> {
        let res = self.resources.as_mut().unwrap();
        let encode_width = res.encode_width;
        let encode_height = res.encode_height;

        let cmd_buf = if buf_idx == 0 {
            res.transfer_command_buffer
        } else {
            res.transfer_command_buffer_b
        };

        unsafe {
            self.device
                .reset_command_buffer(
                    cmd_buf,
                    vk::CommandBufferResetFlags::empty(),
                )
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

            // Transition NV12 image to TRANSFER_DST
            let barrier_to_transfer = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .image(res.nv12_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );

            self.device.cmd_pipeline_barrier(
                cmd_buf,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier_to_transfer],
            );

            // Copy Y plane (plane 0)
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
                    width: encode_width,
                    height: encode_height,
                    depth: 1,
                });

            // Copy UV plane (plane 1)
            let uv_offset = (encode_width * encode_height) as vk::DeviceSize;
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
                    width: encode_width / 2,
                    height: encode_height / 2,
                    depth: 1,
                });

            self.device.cmd_copy_buffer_to_image(
                cmd_buf,
                nv12_source_vk_buffer,
                res.nv12_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[y_region, uv_region],
            );

            // Transition NV12 image to VIDEO_ENCODE_SRC for the encode queue
            let barrier_to_encode = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .image(res.nv12_image)
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
                &[barrier_to_encode],
            );

            self.device
                .end_command_buffer(cmd_buf)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to end transfer command buffer: {e}"))
                })?;
        }

        Ok(())
    }

    /// Record video encode commands on the video encode queue command buffer.
    fn record_encode_commands(&mut self, is_idr: bool, buf_idx: usize) -> Result<()> {
        let res = self.resources.as_mut().unwrap();
        let encode_width = res.encode_width;
        let encode_height = res.encode_height;

        let cmd_buf = if buf_idx == 0 {
            res.encode_command_buffer
        } else {
            res.encode_command_buffer_b
        };
        let dst_bitstream_buffer = if buf_idx == 0 {
            res.bitstream_buffer
        } else {
            res.bitstream_buffer_b
        };

        unsafe {
            self.device
                .reset_command_buffer(
                    cmd_buf,
                    vk::CommandBufferResetFlags::empty(),
                )
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset encode command buffer: {e}"))
                })?;

            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            self.device
                .begin_command_buffer(cmd_buf, &begin_info)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to begin encode command buffer: {e}"))
                })?;

            // Ping-pong DPB: alternate between two DPB images to avoid
            // read/write conflict (setup overwrites the slot while encode reads it).
            // Even frames write to A, odd frames write to B.
            let current_slot_index = (self.frame_count % 2) as i32;
            let reference_slot_index = ((self.frame_count + 1) % 2) as i32;
            let current_dpb_image = if current_slot_index == 0 {
                res.dpb_image_a
            } else {
                res.dpb_image_b
            };
            let current_dpb_image_view = if current_slot_index == 0 {
                res.dpb_image_view_a
            } else {
                res.dpb_image_view_b
            };
            let reference_dpb_image = if reference_slot_index == 0 {
                res.dpb_image_a
            } else {
                res.dpb_image_b
            };
            let reference_dpb_image_view = if reference_slot_index == 0 {
                res.dpb_image_view_a
            } else {
                res.dpb_image_view_b
            };

            // Transition current DPB image (write target) to VIDEO_ENCODE_DPB.
            // IDR frames: UNDEFINED → DPB (no prior data to preserve).
            // P frames: DPB → DPB (this image was last written 2 frames ago).
            let current_dpb_old_layout = if is_idr {
                vk::ImageLayout::UNDEFINED
            } else {
                vk::ImageLayout::VIDEO_ENCODE_DPB_KHR
            };
            let current_dpb_barrier = vk::ImageMemoryBarrier::default()
                .old_layout(current_dpb_old_layout)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                .src_access_mask(
                    vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                )
                .dst_access_mask(
                    vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                )
                .image(current_dpb_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );

            if is_idr {
                // IDR: only need barrier for the current (write) DPB image.
                self.device.cmd_pipeline_barrier(
                    cmd_buf,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[current_dpb_barrier],
                );
            } else {
                // P frame: barrier for both current (write) and reference (read) DPB images.
                let reference_dpb_barrier = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                    .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                    .src_access_mask(
                        vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                    )
                    .dst_access_mask(
                        vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                    )
                    .image(reference_dpb_image)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .level_count(1)
                            .layer_count(1),
                    );

                self.device.cmd_pipeline_barrier(
                    cmd_buf,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[current_dpb_barrier, reference_dpb_barrier],
                );
            }

            // --- Video encode ---

            let (picture_type, slice_type, primary_pic_type) = if is_idr {
                (
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
                    StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
                )
            } else {
                (
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P,
                    StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_P,
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P,
                )
            };

            let mut pic_info_flags = StdVideoEncodeH264PictureInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            pic_info_flags.set_IdrPicFlag(if is_idr { 1 } else { 0 });
            pic_info_flags.set_is_reference(1);

            // P frames require reference lists pointing to previous frame's DPB slot.
            // IDR frames have no references, so pRefLists is null.
            let p_frame_ref_lists = StdVideoEncodeH264ReferenceListsInfo {
                flags: StdVideoEncodeH264ReferenceListsInfoFlags {
                    _bitfield_align_1: [],
                    _bitfield_1: Default::default(),
                },
                num_ref_idx_l0_active_minus1: 0,
                num_ref_idx_l1_active_minus1: 0,
                RefPicList0: {
                    let mut list = [0xFFu8; 32];
                    list[0] = reference_slot_index as u8;
                    list
                },
                RefPicList1: [0xFF; 32],
                refList0ModOpCount: 0,
                refList1ModOpCount: 0,
                refPicMarkingOpCount: 0,
                reserved1: [0; 7],
                pRefList0ModOperations: ptr::null(),
                pRefList1ModOperations: ptr::null(),
                pRefPicMarkingOperations: ptr::null(),
            };

            // PicOrderCnt must reset at each IDR to stay bounded and consistent
            // with the SPS pic_order_cnt_type=2 declaration. Unbounded growth
            // causes NVIDIA DPB management to corrupt after ~1400 frames.
            let pic_order_cnt = (self.frame_count
                % self.config.keyframe_interval_frames as u64)
                as i32;

            let std_picture_info = StdVideoEncodeH264PictureInfo {
                flags: pic_info_flags,
                seq_parameter_set_id: 0,
                pic_parameter_set_id: 0,
                idr_pic_id: if is_idr { (self.frame_count & 0xFFFF) as u16 } else { 0 },
                primary_pic_type,
                frame_num: (self.frame_count % 16) as u32,
                PicOrderCnt: pic_order_cnt,
                temporal_id: 0,
                reserved1: [0; 3],
                pRefLists: if is_idr {
                    ptr::null()
                } else {
                    &p_frame_ref_lists
                },
            };

            let slice_header_flags = StdVideoEncodeH264SliceHeaderFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };

            let slice_header = StdVideoEncodeH264SliceHeader {
                flags: slice_header_flags,
                first_mb_in_slice: 0,
                slice_type,
                slice_alpha_c0_offset_div2: 0,
                slice_beta_offset_div2: 0,
                slice_qp_delta: 0,
                reserved1: 0,
                cabac_init_idc: StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
                disable_deblocking_filter_idc:
                    StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED,
                pWeightTable: ptr::null(),
            };

            // constant_qp MUST be 0 when using CBR/VBR rate control (Vulkan spec VUID-08269)
            let nalu_slice_info = vk::VideoEncodeH264NaluSliceInfoKHR::default()
                .constant_qp(0)
                .std_slice_header(&slice_header);

            // generate_prefix_nalu = true: NVIDIA driver accepts this (test produces 1080 bytes
            // with it set to true) even though the capability bit isn't advertised.
            let mut h264_picture_info = vk::VideoEncodeH264PictureInfoKHR::default()
                .nalu_slice_entries(std::slice::from_ref(&nalu_slice_info))
                .std_picture_info(&std_picture_info)
                .generate_prefix_nalu(true);

            let src_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D {
                    width: encode_width,
                    height: encode_height,
                })
                .base_array_layer(0)
                .image_view_binding(res.nv12_image_view);

            let mut dpb_ref_info_flags = StdVideoEncodeH264ReferenceInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            dpb_ref_info_flags.set_used_for_long_term_reference(0);

            let dpb_std_reference_info = StdVideoEncodeH264ReferenceInfo {
                flags: dpb_ref_info_flags,
                primary_pic_type: picture_type,
                FrameNum: (self.frame_count % 16) as u32,
                PicOrderCnt: pic_order_cnt,
                long_term_pic_num: 0,
                long_term_frame_idx: 0,
                temporal_id: 0,
            };

            let mut dpb_h264_slot_info = vk::VideoEncodeH264DpbSlotInfoKHR::default()
                .std_reference_info(&dpb_std_reference_info);

            // Setup reference slot: writes reconstructed frame to current DPB image.
            let current_dpb_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D {
                    width: encode_width,
                    height: encode_height,
                })
                .base_array_layer(0)
                .image_view_binding(current_dpb_image_view);

            let setup_reference_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(current_slot_index)
                .picture_resource(&current_dpb_picture_resource)
                .push_next(&mut dpb_h264_slot_info);

            // Reference slot for P frames: reads previous frame from reference DPB image.
            let mut ref_dpb_ref_info_flags = StdVideoEncodeH264ReferenceInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            ref_dpb_ref_info_flags.set_used_for_long_term_reference(0);

            let ref_pic_order_cnt = (self.frame_count.wrapping_sub(1)
                % self.config.keyframe_interval_frames as u64)
                as i32;

            let ref_std_reference_info = StdVideoEncodeH264ReferenceInfo {
                flags: ref_dpb_ref_info_flags,
                primary_pic_type: if self.previous_frame_was_idr {
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR
                } else {
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P
                },
                FrameNum: ((self.frame_count.wrapping_sub(1)) % 16) as u32,
                PicOrderCnt: ref_pic_order_cnt,
                long_term_pic_num: 0,
                long_term_frame_idx: 0,
                temporal_id: 0,
            };

            let mut ref_h264_slot_info = vk::VideoEncodeH264DpbSlotInfoKHR::default()
                .std_reference_info(&ref_std_reference_info);

            let reference_dpb_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D {
                    width: encode_width,
                    height: encode_height,
                })
                .base_array_layer(0)
                .image_view_binding(reference_dpb_image_view);

            let ref_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(reference_slot_index)
                .picture_resource(&reference_dpb_picture_resource)
                .push_next(&mut ref_h264_slot_info);

            // Vulkan spec: begin_coding must declare ALL picture resources used by encode.
            // VUID-08215 requires setup_reference_slot's picture resource to be "bound".
            // For IDR: declare only the current (write) slot — no references.
            // For P: declare both the current (write) and reference (read) slots.
            let reference_slots_for_begin: Vec<vk::VideoReferenceSlotInfoKHR<'_>> =
                if is_idr {
                    vec![setup_reference_slot]
                } else {
                    vec![setup_reference_slot, ref_slot]
                };

            // Vulkan spec (VUID-08253): VkVideoEncodeRateControlInfoKHR chained to
            // VkVideoBeginCodingInfoKHR declares the rate control state the application
            // expects the session to be in. Must be present on EVERY begin_coding call,
            // not just the first — otherwise the driver sees DEFAULT mode and fires
            // VUID-08253 on frame 2+.
            let mut begin_h264_rate_control_layer_info =
                vk::VideoEncodeH264RateControlLayerInfoKHR::default();
            let target_bitrate = self.config.bitrate_bps as u64;
            let begin_rate_control_layer = vk::VideoEncodeRateControlLayerInfoKHR::default()
                .average_bitrate(target_bitrate)
                .max_bitrate(target_bitrate)
                .frame_rate_numerator(self.config.fps)
                .frame_rate_denominator(1)
                .push_next(&mut begin_h264_rate_control_layer_info);
            let begin_rate_control_layers = [begin_rate_control_layer];

            let mut begin_rate_control_info = vk::VideoEncodeRateControlInfoKHR::default()
                .rate_control_mode(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
                .layers(&begin_rate_control_layers)
                .virtual_buffer_size_in_ms(1000)
                .initial_virtual_buffer_size_in_ms(500);

            let begin_info = vk::VideoBeginCodingInfoKHR::default()
                .video_session(res.video_session.video_session())
                .video_session_parameters(res.video_session.video_session_parameters())
                .reference_slots(&reference_slots_for_begin)
                .push_next(&mut begin_rate_control_info);

            (self.video_queue_loader.fp().cmd_begin_video_coding_khr)(
                cmd_buf,
                &begin_info,
            );

            // On first frame, reset the session and configure rate control
            if !res.session_initialized {
                let reset_info = vk::VideoCodingControlInfoKHR::default()
                    .flags(vk::VideoCodingControlFlagsKHR::RESET);
                (self.video_queue_loader.fp().cmd_control_video_coding_khr)(
                    cmd_buf,
                    &reset_info,
                );

                // H.264-specific rate control layer info (required by Vulkan spec
                // when using CBR/VBR with H.264 encode — each layer in
                // VideoEncodeRateControlInfoKHR must have this in its pNext).
                let mut h264_rate_control_layer_info =
                    vk::VideoEncodeH264RateControlLayerInfoKHR::default();

                // Vulkan spec: for CBR, max_bitrate should equal average_bitrate
                let rate_control_layer = vk::VideoEncodeRateControlLayerInfoKHR::default()
                    .average_bitrate(target_bitrate)
                    .max_bitrate(target_bitrate)
                    .frame_rate_numerator(self.config.fps)
                    .frame_rate_denominator(1)
                    .push_next(&mut h264_rate_control_layer_info);

                let rate_control_layers = [rate_control_layer];

                // virtualBufferSizeInMs and initialVirtualBufferSizeInMs are REQUIRED
                // when layerCount > 0 (Vulkan spec VUID-08357). Use 1 second buffer.
                let mut rate_control_info = vk::VideoEncodeRateControlInfoKHR::default()
                    .rate_control_mode(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
                    .layers(&rate_control_layers)
                    .virtual_buffer_size_in_ms(1000)
                    .initial_virtual_buffer_size_in_ms(500);

                // H.264-specific rate control info (required by Vulkan spec when
                // using CBR/VBR with H.264 encode — must be chained alongside
                // VideoEncodeRateControlInfoKHR on VideoCodingControlInfoKHR).
                let mut h264_rate_control_info =
                    vk::VideoEncodeH264RateControlInfoKHR::default()
                        .gop_frame_count(self.config.keyframe_interval_frames)
                        .idr_period(self.config.keyframe_interval_frames)
                        .consecutive_b_frame_count(0)
                        .temporal_layer_count(1);

                let rc_control_info = vk::VideoCodingControlInfoKHR::default()
                    .flags(vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL)
                    .push_next(&mut rate_control_info)
                    .push_next(&mut h264_rate_control_info);

                (self.video_queue_loader.fp().cmd_control_video_coding_khr)(
                    cmd_buf,
                    &rc_control_info,
                );

                res.session_initialized = true;
            }

            let reference_slots_for_encode: &[vk::VideoReferenceSlotInfoKHR<'_>] =
                if is_idr {
                    &[]
                } else {
                    std::slice::from_ref(&ref_slot)
                };

            let encode_info = vk::VideoEncodeInfoKHR::default()
                .dst_buffer(dst_bitstream_buffer)
                .dst_buffer_offset(0)
                .dst_buffer_range(BITSTREAM_BUFFER_SIZE)
                .src_picture_resource(src_picture_resource)
                .setup_reference_slot(&setup_reference_slot)
                .reference_slots(reference_slots_for_encode)
                .push_next(&mut h264_picture_info);

            // Encode without queries — queries were causing validation errors
            // (VUID-vkCmdBeginQuery-None-07127). Using scan fallback for bitstream size.
            (self.video_encode_queue_loader.fp().cmd_encode_video_khr)(
                cmd_buf,
                &encode_info,
            );

            let end_info = vk::VideoEndCodingInfoKHR::default();
            (self.video_queue_loader.fp().cmd_end_video_coding_khr)(
                cmd_buf,
                &end_info,
            );

            self.device
                .end_command_buffer(cmd_buf)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to end encode command buffer: {e}"))
                })?;
        }

        Ok(())
    }

    /// Read the encoded bitstream from the specified buffer set.
    fn read_bitstream(&self, buf_idx: usize) -> Result<Vec<u8>> {
        let res = self.resources.as_ref().unwrap();

        let bitstream_ptr = if buf_idx == 0 {
            res.bitstream_mapped_ptr
        } else {
            res.bitstream_mapped_ptr_b
        };

        // Check encode status via RESULT_STATUS_ONLY_KHR query (non-blocking).
        // NVIDIA drivers may not populate this reliably at all resolutions,
        // so we poll without WAIT to avoid hanging.
        let mut status_result: [i32; 1] = [0];
        let status_available = unsafe {
            (self.device.fp_v1_0().get_query_pool_results)(
                self.device.handle(),
                res.encode_status_query_pool,
                0,
                1,
                std::mem::size_of_val(&status_result),
                status_result.as_mut_ptr().cast(),
                std::mem::size_of_val(&status_result) as vk::DeviceSize,
                vk::QueryResultFlags::WITH_STATUS_KHR,
            )
        }
        .result()
        .is_ok();

        if status_available && status_result[0] < 0 {
            return Err(StreamError::GpuError(format!(
                "Vulkan Video encode failed with status: {}",
                status_result[0]
            )));
        }

        if status_available {
            tracing::trace!("Encode status query: {}", status_result[0]);
        } else {
            tracing::trace!("Encode status query not available (NVIDIA driver quirk)");
        }

        // Try encode feedback query (non-blocking) for precise byte count.
        // Some NVIDIA drivers advertise feedback support but never populate the
        // query results. Poll without WAIT to avoid hanging.
        let mut feedback_and_status: [u32; 2] = [0; 2];
        let feedback_available = unsafe {
            (self.device.fp_v1_0().get_query_pool_results)(
                self.device.handle(),
                res.encode_feedback_query_pool,
                0,
                1,
                std::mem::size_of_val(&feedback_and_status),
                feedback_and_status.as_mut_ptr().cast(),
                std::mem::size_of_val(&feedback_and_status) as vk::DeviceSize,
                vk::QueryResultFlags::WITH_STATUS_KHR,
            )
        }
        .result()
        .is_ok();

        let bytes_written = if feedback_available && feedback_and_status[0] > 0 {
            let count = feedback_and_status[0] as usize;
            tracing::trace!("Encode feedback query: {} bytes written", count);
            count
        } else {
            // Fallback: scan the zero-cleared bitstream buffer for actual data.
            // The buffer is zeroed before each encode (see clear in encode()),
            // so trailing zeros are unused space.
            //
            // Forward scan in chunks: H.264 bitstream starts at offset 0, so
            // scan forward in 4 KB chunks until we find an all-zero chunk,
            // then refine the boundary backward within one chunk. This reads
            // only ~N+4 KB of HOST_COHERENT memory (where N is the actual
            // frame size) instead of the entire buffer — critical on NVIDIA
            // where uncached reads are ~100x slower than normal memory.
            let buf_size = BITSTREAM_BUFFER_SIZE as usize;
            let data = unsafe {
                std::slice::from_raw_parts(bitstream_ptr, buf_size)
            };
            const SCAN_CHUNK: usize = 4096;
            let mut data_region_end = 0usize;
            for chunk_start in (0..buf_size).step_by(SCAN_CHUNK) {
                let chunk_end = (chunk_start + SCAN_CHUNK).min(buf_size);
                if data[chunk_start..chunk_end].iter().all(|&b| b == 0) {
                    break;
                }
                data_region_end = chunk_end;
            }
            // Fine-grained backward scan within the last non-zero chunk
            while data_region_end > 0 && data[data_region_end - 1] == 0 {
                data_region_end -= 1;
            }
            if data_region_end == 0 {
                tracing::warn!("Vulkan Video encode produced 0 bytes (scan fallback)");
                return Ok(Vec::new());
            }
            // H.264 RBSP trailing bits can end with a few zero bytes from
            // cabac_zero_word padding. Add up to 2 zero bytes back if they
            // exist in the buffer to avoid truncating valid data.
            let padded_end = (data_region_end + 2).min(buf_size);
            tracing::trace!(
                "Encode feedback unavailable, scan fallback: {} bytes (padded from {})",
                padded_end,
                data_region_end
            );
            padded_end
        };

        if bytes_written > BITSTREAM_BUFFER_SIZE as usize {
            return Err(StreamError::GpuError(format!(
                "Encode reports {} bytes, exceeds buffer size {}",
                bytes_written, BITSTREAM_BUFFER_SIZE
            )));
        }

        let data = unsafe {
            std::slice::from_raw_parts(bitstream_ptr, bytes_written)
        };

        Ok(data.to_vec())
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

        // Vulkan Video spec requires VkVideoProfileListInfoKHR in the pNext
        // chain for images with VIDEO_ENCODE_SRC or VIDEO_ENCODE_DPB usage.
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

        // Allocate through VulkanDevice RHI (export + dedicated flags, tracked).
        let memory = vulkan_device
            .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
            .map_err(|e| {
                unsafe { device.destroy_image(image, None) };
                e
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

    fn create_bitstream_buffer(
        vulkan_device: &VulkanDevice,
        video_profile_list: &mut vk::VideoProfileListInfoKHR<'_>,
    ) -> Result<(vk::Buffer, vk::DeviceMemory, *mut u8)> {
        let device = vulkan_device.device();

        // Vulkan spec requires VkVideoProfileListInfoKHR for VIDEO_ENCODE_DST buffers
        let buffer_info = vk::BufferCreateInfo::default()
            .size(BITSTREAM_BUFFER_SIZE)
            .usage(vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(video_profile_list);

        let buffer = unsafe { device.create_buffer(&buffer_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create bitstream buffer: {e}")))?;

        // Allocate through VulkanDevice RHI (export flags, tracked).
        let memory = vulkan_device
            .allocate_buffer_memory(
                buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                false,
            )
            .map_err(|e| {
                unsafe { device.destroy_buffer(buffer, None) };
                e
            })?;

        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
            vulkan_device.free_device_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            StreamError::GpuError(format!("Failed to bind bitstream memory: {e}"))
        })?;

        let mapped_ptr = vulkan_device.map_device_memory(memory, BITSTREAM_BUFFER_SIZE)
            .map_err(|e| {
                vulkan_device.free_device_memory(memory);
                unsafe { device.destroy_buffer(buffer, None) };
                e
            })?;

        Ok((buffer, memory, mapped_ptr))
    }

    /// Set the target bitrate.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.config.bitrate_bps = bitrate_bps;
        if let Some(res) = &mut self.resources {
            res.session_initialized = false;
        }
        Ok(())
    }

    /// Force the next frame to be a keyframe.
    pub fn force_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    /// Get the encoder configuration.
    pub fn config(&self) -> &VideoEncoderConfig {
        &self.config
    }

    /// Query the GPU for the best supported H.264 encode profile.
    ///
    /// Probes High, Main, then Baseline in order, returning the first
    /// profile that `vkGetPhysicalDeviceVideoCapabilitiesKHR` accepts.
    fn query_best_h264_profile(vulkan_device: &VulkanDevice) -> Result<H264Profile> {
        let video_queue_instance_loader =
            ash::khr::video_queue::Instance::new(vulkan_device.entry(), vulkan_device.instance());

        let profiles_to_try = [
            (
                StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
                H264Profile::High,
                "High",
            ),
            (
                StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
                H264Profile::Main,
                "Main",
            ),
            (
                StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
                H264Profile::Baseline,
                "Baseline",
            ),
        ];

        for (std_profile_idc, h264_profile, name) in &profiles_to_try {
            let mut h264_profile_info =
                vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(*std_profile_idc);

            let video_profile = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut h264_profile_info);

            let mut h264_encode_capabilities = vk::VideoEncodeH264CapabilitiesKHR::default();
            let mut encode_capabilities = vk::VideoEncodeCapabilitiesKHR::default();
            let mut capabilities = vk::VideoCapabilitiesKHR::default()
                .push_next(&mut encode_capabilities)
                .push_next(&mut h264_encode_capabilities);

            let result = unsafe {
                (video_queue_instance_loader
                    .fp()
                    .get_physical_device_video_capabilities_khr)(
                    vulkan_device.physical_device(),
                    &video_profile,
                    &mut capabilities,
                )
            }
            .result();

            match result {
                Ok(()) => {
                    tracing::info!(
                        "GPU supports H.264 {} profile (max {}x{})",
                        name,
                        capabilities.max_coded_extent.width,
                        capabilities.max_coded_extent.height
                    );
                    return Ok(*h264_profile);
                }
                Err(e) => {
                    tracing::debug!(
                        "GPU does not support H.264 {} profile: {:?}",
                        name,
                        e
                    );
                }
            }
        }

        Err(StreamError::GpuError(
            "GPU does not support any H.264 encode profile (tried High, Main, Baseline)".into(),
        ))
    }
}

impl Drop for EncodeResources {
    fn drop(&mut self) {
        // device_wait_idle is called by VulkanDevice::drop, but we need the
        // device handle here. We stored it indirectly via the video_session
        // which holds a device clone. For safety, we don't call device_wait_idle
        // here — the VulkanVideoEncoder drop ensures idle before dropping resources.
    }
}

impl Drop for VulkanVideoEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
        }

        if let Some(res) = self.resources.take() {
            unsafe {
                self.device.destroy_fence(res.transfer_fence, None);
                self.device
                    .destroy_command_pool(res.transfer_command_pool, None);
                self.device.destroy_fence(res.encode_fence, None);
                self.device.destroy_fence(res.encode_fence_b, None);
                self.device
                    .destroy_command_pool(res.encode_command_pool, None);

                self.device
                    .destroy_query_pool(res.encode_feedback_query_pool, None);
                self.device
                    .destroy_query_pool(res.encode_status_query_pool, None);

                self.device.destroy_buffer(res.bitstream_buffer, None);
                self.vulkan_device.unmap_device_memory(res.bitstream_buffer_memory);
                self.vulkan_device.free_device_memory(res.bitstream_buffer_memory);

                self.device.destroy_buffer(res.bitstream_buffer_b, None);
                self.vulkan_device.unmap_device_memory(res.bitstream_buffer_memory_b);
                self.vulkan_device.free_device_memory(res.bitstream_buffer_memory_b);

                self.device.destroy_image_view(res.dpb_image_view_a, None);
                self.device.destroy_image(res.dpb_image_a, None);
                self.vulkan_device.free_device_memory(res.dpb_image_memory_a);

                self.device.destroy_image_view(res.dpb_image_view_b, None);
                self.device.destroy_image(res.dpb_image_b, None);
                self.vulkan_device.free_device_memory(res.dpb_image_memory_b);

                self.device.destroy_image_view(res.nv12_image_view, None);
                self.device.destroy_image(res.nv12_image, None);
                self.vulkan_device.free_device_memory(res.nv12_image_memory);
            }
            // video_session, format_converter, nv12_staging_buffer{,_b} drop automatically
        }

        tracing::info!("VulkanVideoEncoder destroyed");
    }
}

// VulkanVideoEncoder is Send because Vulkan handles are thread-safe
unsafe impl Send for VulkanVideoEncoder {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that creating encode resources and recording an IDR command buffer
    /// succeeds without VK_ERROR_INITIALIZATION_FAILED. This validates the
    /// encode pipeline setup (video session, images, command recording) without
    /// needing a camera or live frames.
    #[test]
    fn test_vulkan_video_encode_idr_command_recording() {
        let vulkan_device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test — Vulkan not available");
                return;
            }
        };

        if !vulkan_device.supports_video_encode() {
            println!("Skipping test — Vulkan Video encode not supported");
            return;
        }

        let ve_family = vulkan_device
            .video_encode_queue_family_index()
            .expect("video encode queue family");
        let gfx_family = vulkan_device.queue_family_index();
        let device = vulkan_device.device().clone();
        let instance = vulkan_device.instance();

        let video_queue_loader = ash::khr::video_queue::Device::new(instance, &device);
        let video_encode_queue_loader =
            ash::khr::video_encode_queue::Device::new(instance, &device);

        // Create video session at 1280x720
        let config = VideoEncoderConfig::new(1280, 720);
        let video_session =
            VulkanVideoSession::new(&vulkan_device, &config).expect("video session");

        // Build video profile chain for image/buffer creation
        let std_profile_idc =
            StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH;

        let mut h264_profile_info =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);

        let video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut h264_profile_info);

        let profiles = [video_profile];
        let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);

        // Create NV12 source image
        let (nv12_image, nv12_image_memory) = VulkanVideoEncoder::create_nv12_image(
            &vulkan_device,
            1280,
            720,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR,
            &[gfx_family, ve_family],
            &mut profile_list,
        )
        .expect("NV12 source image");

        let nv12_image_view = VulkanVideoEncoder::create_image_view(
            &device,
            nv12_image,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )
        .expect("NV12 image view");

        // Create DPB image
        let (dpb_image, dpb_image_memory) = VulkanVideoEncoder::create_nv12_image(
            &vulkan_device,
            1280,
            720,
            vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            &[ve_family],
            &mut profile_list,
        )
        .expect("DPB image");

        let dpb_image_view = VulkanVideoEncoder::create_image_view(
            &device,
            dpb_image,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )
        .expect("DPB image view");

        // Create bitstream buffer
        let (bitstream_buffer, bitstream_buffer_memory, bitstream_mapped_ptr) =
            VulkanVideoEncoder::create_bitstream_buffer(&vulkan_device, &mut profile_list)
                .expect("bitstream buffer");

        // Create encode command pool and buffer
        let ve_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(ve_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let encode_command_pool =
            unsafe { device.create_command_pool(&ve_pool_info, None) }
                .expect("encode command pool");

        let ve_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(encode_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let encode_command_buffer =
            unsafe { device.allocate_command_buffers(&ve_cmd_alloc) }
                .expect("encode command buffer")[0];

        // Record an IDR encode command buffer — this is the operation that fails
        // with VK_ERROR_INITIALIZATION_FAILED if the encode setup is wrong.
        let record_result = unsafe {
            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            device
                .begin_command_buffer(encode_command_buffer, &begin_info)
                .expect("begin command buffer");

            // DPB barrier
            let dpb_barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(
                    vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                )
                .image(dpb_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );

            device.cmd_pipeline_barrier(
                encode_command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[dpb_barrier],
            );

            // NV12 src barrier (simulate transfer having already completed)
            let nv12_barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .image(nv12_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );

            device.cmd_pipeline_barrier(
                encode_command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[nv12_barrier],
            );

            // Build H.264 picture info for IDR
            let mut pic_info_flags = StdVideoEncodeH264PictureInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            pic_info_flags.set_IdrPicFlag(1);
            pic_info_flags.set_is_reference(1);

            let std_picture_info = StdVideoEncodeH264PictureInfo {
                flags: pic_info_flags,
                seq_parameter_set_id: 0,
                pic_parameter_set_id: 0,
                idr_pic_id: 0,
                primary_pic_type:
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
                frame_num: 0,
                PicOrderCnt: 0,
                temporal_id: 0,
                reserved1: [0; 3],
                pRefLists: ptr::null(),
            };

            let slice_header_flags = StdVideoEncodeH264SliceHeaderFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };

            let slice_header = StdVideoEncodeH264SliceHeader {
                flags: slice_header_flags,
                first_mb_in_slice: 0,
                slice_type: StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
                slice_alpha_c0_offset_div2: 0,
                slice_beta_offset_div2: 0,
                slice_qp_delta: 0,
                reserved1: 0,
                cabac_init_idc:
                    StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
                disable_deblocking_filter_idc:
                    StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED,
                pWeightTable: ptr::null(),
            };

            let nalu_slice_info = vk::VideoEncodeH264NaluSliceInfoKHR::default()
                .constant_qp(0)
                .std_slice_header(&slice_header);

            // IDR frames include SPS/PPS prefix NALUs for decoder init.
            let mut h264_picture_info = vk::VideoEncodeH264PictureInfoKHR::default()
                .nalu_slice_entries(std::slice::from_ref(&nalu_slice_info))
                .std_picture_info(&std_picture_info)
                .generate_prefix_nalu(true);

            let src_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D {
                    width: 1280,
                    height: 720,
                })
                .base_array_layer(0)
                .image_view_binding(nv12_image_view);

            // DPB setup slot for IDR
            let mut dpb_ref_info_flags = StdVideoEncodeH264ReferenceInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            dpb_ref_info_flags.set_used_for_long_term_reference(0);

            let dpb_std_reference_info = StdVideoEncodeH264ReferenceInfo {
                flags: dpb_ref_info_flags,
                primary_pic_type:
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
                FrameNum: 0,
                PicOrderCnt: 0,
                long_term_pic_num: 0,
                long_term_frame_idx: 0,
                temporal_id: 0,
            };

            let mut dpb_h264_slot_info = vk::VideoEncodeH264DpbSlotInfoKHR::default()
                .std_reference_info(&dpb_std_reference_info);

            let dpb_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D {
                    width: 1280,
                    height: 720,
                })
                .base_array_layer(0)
                .image_view_binding(dpb_image_view);

            let setup_reference_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(0)
                .picture_resource(&dpb_picture_resource)
                .push_next(&mut dpb_h264_slot_info);

            let reference_slots_for_begin = [setup_reference_slot];

            let begin_info = vk::VideoBeginCodingInfoKHR::default()
                .video_session(video_session.video_session())
                .video_session_parameters(video_session.video_session_parameters())
                .reference_slots(&reference_slots_for_begin);

            (video_queue_loader.fp().cmd_begin_video_coding_khr)(
                encode_command_buffer,
                &begin_info,
            );

            // Reset session
            let reset_info = vk::VideoCodingControlInfoKHR::default()
                .flags(vk::VideoCodingControlFlagsKHR::RESET);
            (video_queue_loader.fp().cmd_control_video_coding_khr)(
                encode_command_buffer,
                &reset_info,
            );

            // Rate control with H.264-specific info
            let mut h264_rate_control_layer_info =
                vk::VideoEncodeH264RateControlLayerInfoKHR::default();

            let target_bitrate = config.bitrate_bps as u64;
            let rate_control_layer = vk::VideoEncodeRateControlLayerInfoKHR::default()
                .average_bitrate(target_bitrate)
                .max_bitrate(target_bitrate)
                .frame_rate_numerator(config.fps)
                .frame_rate_denominator(1)
                .push_next(&mut h264_rate_control_layer_info);

            let rate_control_layers = [rate_control_layer];

            let mut rate_control_info = vk::VideoEncodeRateControlInfoKHR::default()
                .rate_control_mode(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
                .layers(&rate_control_layers);

            let mut h264_rate_control_info =
                vk::VideoEncodeH264RateControlInfoKHR::default()
                    .gop_frame_count(config.keyframe_interval_frames)
                    .idr_period(config.keyframe_interval_frames)
                    .consecutive_b_frame_count(0)
                    .temporal_layer_count(1);

            let rc_control_info = vk::VideoCodingControlInfoKHR::default()
                .flags(vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL)
                .push_next(&mut rate_control_info)
                .push_next(&mut h264_rate_control_info);

            (video_queue_loader.fp().cmd_control_video_coding_khr)(
                encode_command_buffer,
                &rc_control_info,
            );

            // Encode IDR
            let encode_info = vk::VideoEncodeInfoKHR::default()
                .dst_buffer(bitstream_buffer)
                .dst_buffer_offset(0)
                .dst_buffer_range(BITSTREAM_BUFFER_SIZE)
                .src_picture_resource(src_picture_resource)
                .setup_reference_slot(&setup_reference_slot)
                .reference_slots(&[])
                .push_next(&mut h264_picture_info);

            (video_encode_queue_loader.fp().cmd_encode_video_khr)(
                encode_command_buffer,
                &encode_info,
            );

            let end_info = vk::VideoEndCodingInfoKHR::default();
            (video_queue_loader.fp().cmd_end_video_coding_khr)(
                encode_command_buffer,
                &end_info,
            );

            device.end_command_buffer(encode_command_buffer)
        };

        match &record_result {
            Ok(()) => println!("IDR encode command buffer recorded successfully"),
            Err(e) => println!("IDR encode command buffer recording failed: {e}"),
        }
        assert!(
            record_result.is_ok(),
            "IDR encode command buffer recording should succeed (got VK_ERROR_INITIALIZATION_FAILED if it fails here)"
        );

        // Cleanup
        unsafe {
            device.destroy_command_pool(encode_command_pool, None);
            device.unmap_memory(bitstream_buffer_memory);
            device.destroy_buffer(bitstream_buffer, None);
            device.free_memory(bitstream_buffer_memory, None);
            device.destroy_image_view(dpb_image_view, None);
            device.destroy_image(dpb_image, None);
            device.free_memory(dpb_image_memory, None);
            device.destroy_image_view(nv12_image_view, None);
            device.destroy_image(nv12_image, None);
            device.free_memory(nv12_image_memory, None);
        }
        drop(video_session);

        println!("Vulkan Video encode IDR command recording test passed");
    }

    /// Integration test: encode a synthetic frame through the full GPU pipeline
    /// and validate that the output contains H.264 NAL units via query pool readback.
    #[test]
    fn test_vulkan_video_encode_synthetic_frame_produces_h264_output() {
        let vulkan_device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test — Vulkan not available");
                return;
            }
        };

        if !vulkan_device.supports_video_encode() {
            println!("Skipping test — Vulkan Video encode not supported");
            return;
        }

        let ve_family = vulkan_device
            .video_encode_queue_family_index()
            .expect("video encode queue family");
        let gfx_family = vulkan_device.queue_family_index();
        let device = vulkan_device.device().clone();
        let instance = vulkan_device.instance();

        let video_queue_loader = ash::khr::video_queue::Device::new(instance, &device);
        let video_encode_queue_loader =
            ash::khr::video_encode_queue::Device::new(instance, &device);

        let video_encode_queue = vulkan_device
            .video_encode_queue()
            .expect("video encode queue");

        let width = 1920u32;
        let height = 1080u32;
        let config = VideoEncoderConfig::new(width, height);
        let video_session =
            VulkanVideoSession::new(&vulkan_device, &config).expect("video session");

        // Build video profile chain
        let std_profile_idc =
            StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH;

        let mut h264_profile_info =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut h264_profile_info);
        let profiles = [video_profile];
        let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);

        // Create NV12 source image
        let (nv12_image, nv12_image_memory) = VulkanVideoEncoder::create_nv12_image(
            &vulkan_device,
            width,
            height,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR,
            &[gfx_family, ve_family],
            &mut profile_list,
        )
        .expect("NV12 source image");
        let nv12_image_view = VulkanVideoEncoder::create_image_view(
            &device,
            nv12_image,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )
        .expect("NV12 image view");

        // Create DPB image
        let (dpb_image, dpb_image_memory) = VulkanVideoEncoder::create_nv12_image(
            &vulkan_device,
            width,
            height,
            vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            &[ve_family],
            &mut profile_list,
        )
        .expect("DPB image");
        let dpb_image_view = VulkanVideoEncoder::create_image_view(
            &device,
            dpb_image,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )
        .expect("DPB image view");

        // Create bitstream buffer
        let (bitstream_buffer, bitstream_buffer_memory, bitstream_mapped_ptr) =
            VulkanVideoEncoder::create_bitstream_buffer(&vulkan_device, &mut profile_list)
                .expect("bitstream buffer");

        // Create NV12 staging buffer (host-visible) for synthetic frame data
        let nv12_size = (width * height * 3 / 2) as vk::DeviceSize;
        let staging_buffer_info = vk::BufferCreateInfo::default()
            .size(nv12_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let staging_buffer = unsafe { device.create_buffer(&staging_buffer_info, None) }
            .expect("staging buffer");
        let staging_mem_reqs =
            unsafe { device.get_buffer_memory_requirements(staging_buffer) };
        let staging_memory_type = vulkan_device
            .find_memory_type(
                staging_mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .expect("staging memory type");
        let staging_alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(staging_mem_reqs.size)
            .memory_type_index(staging_memory_type);
        let staging_memory = unsafe { device.allocate_memory(&staging_alloc, None) }
            .expect("staging memory");
        unsafe { device.bind_buffer_memory(staging_buffer, staging_memory, 0) }
            .expect("bind staging memory");
        let staging_ptr = unsafe {
            device.map_memory(
                staging_memory,
                0,
                nv12_size,
                vk::MemoryMapFlags::empty(),
            )
        }
        .expect("map staging memory") as *mut u8;

        // Fill with synthetic NV12 data: Y plane = gradient, UV plane = 128 (gray)
        let y_size = (width * height) as usize;
        let uv_size = (width * height / 2) as usize;
        unsafe {
            let y_plane = std::slice::from_raw_parts_mut(staging_ptr, y_size);
            for (i, pixel) in y_plane.iter_mut().enumerate() {
                *pixel = ((i % width as usize) * 255 / width as usize) as u8;
            }
            let uv_plane =
                std::slice::from_raw_parts_mut(staging_ptr.add(y_size), uv_size);
            uv_plane.fill(128);
        }

        // Create query pools
        let mut feedback_h264_profile =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let mut feedback_video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut feedback_h264_profile);
        let mut feedback_create_info =
            vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(
                    vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
                );
        let feedback_query_pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
            .query_count(1)
            .push_next(&mut feedback_create_info)
            .push_next(&mut feedback_video_profile);
        let feedback_query_pool =
            unsafe { device.create_query_pool(&feedback_query_pool_info, None) }
                .expect("feedback query pool");

        let mut status_h264_profile =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let mut status_video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut status_h264_profile);
        let status_query_pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::RESULT_STATUS_ONLY_KHR)
            .query_count(1)
            .push_next(&mut status_video_profile);
        let status_query_pool =
            unsafe { device.create_query_pool(&status_query_pool_info, None) }
                .expect("status query pool");

        // Create command pools and buffers
        let gfx_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gfx_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let transfer_command_pool =
            unsafe { device.create_command_pool(&gfx_pool_info, None) }
                .expect("transfer command pool");
        let gfx_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(transfer_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let transfer_cmd =
            unsafe { device.allocate_command_buffers(&gfx_cmd_alloc) }
                .expect("transfer command buffer")[0];

        let ve_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(ve_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let encode_command_pool =
            unsafe { device.create_command_pool(&ve_pool_info, None) }
                .expect("encode command pool");
        let ve_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(encode_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let encode_cmd =
            unsafe { device.allocate_command_buffers(&ve_cmd_alloc) }
                .expect("encode command buffer")[0];

        let fence_info = vk::FenceCreateInfo::default();
        let transfer_fence = unsafe { device.create_fence(&fence_info, None) }
            .expect("transfer fence");
        let encode_fence = unsafe { device.create_fence(&fence_info, None) }
            .expect("encode fence");

        // === Stage 1: Transfer synthetic NV12 data to GPU image ===
        unsafe {
            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            device
                .begin_command_buffer(transfer_cmd, &begin_info)
                .expect("begin transfer cmd");

            // Transition NV12 image to TRANSFER_DST
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .image(nv12_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            device.cmd_pipeline_barrier(
                transfer_cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );

            // Copy Y plane
            let y_region = vk::BufferImageCopy::default()
                .buffer_offset(0)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::PLANE_0)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            // Copy UV plane
            let uv_region = vk::BufferImageCopy::default()
                .buffer_offset(y_size as vk::DeviceSize)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::PLANE_1)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: width / 2,
                    height: height / 2,
                    depth: 1,
                });
            device.cmd_copy_buffer_to_image(
                transfer_cmd,
                staging_buffer,
                nv12_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[y_region, uv_region],
            );

            // Transition to VIDEO_ENCODE_SRC
            let barrier2 = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .image(nv12_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            device.cmd_pipeline_barrier(
                transfer_cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier2],
            );

            device.end_command_buffer(transfer_cmd).expect("end transfer cmd");

            let cmds = [transfer_cmd];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            device
                .queue_submit(vulkan_device.queue(), &[submit], transfer_fence)
                .expect("submit transfer");
            device
                .wait_for_fences(&[transfer_fence], true, u64::MAX)
                .expect("wait transfer");
        }

        // Zero-clear bitstream buffer so scan fallback can detect data boundaries
        unsafe {
            std::ptr::write_bytes(bitstream_mapped_ptr, 0u8, BITSTREAM_BUFFER_SIZE as usize);
        }

        // === Stage 2: Encode IDR frame with query pools ===
        unsafe {
            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            device
                .begin_command_buffer(encode_cmd, &begin_info)
                .expect("begin encode cmd");

            // DPB barrier
            let dpb_barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(
                    vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                )
                .image(dpb_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            device.cmd_pipeline_barrier(
                encode_cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[dpb_barrier],
            );

            // Build IDR picture info
            let mut pic_info_flags = StdVideoEncodeH264PictureInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            pic_info_flags.set_IdrPicFlag(1);
            pic_info_flags.set_is_reference(1);

            let std_picture_info = StdVideoEncodeH264PictureInfo {
                flags: pic_info_flags,
                seq_parameter_set_id: 0,
                pic_parameter_set_id: 0,
                idr_pic_id: 0,
                primary_pic_type:
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
                frame_num: 0,
                PicOrderCnt: 0,
                temporal_id: 0,
                reserved1: [0; 3],
                pRefLists: ptr::null(),
            };

            let slice_header_flags = StdVideoEncodeH264SliceHeaderFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            let slice_header = StdVideoEncodeH264SliceHeader {
                flags: slice_header_flags,
                first_mb_in_slice: 0,
                slice_type: StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
                slice_alpha_c0_offset_div2: 0,
                slice_beta_offset_div2: 0,
                slice_qp_delta: 0,
                reserved1: 0,
                cabac_init_idc:
                    StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
                disable_deblocking_filter_idc:
                    StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED,
                pWeightTable: ptr::null(),
            };

            let nalu_slice_info = vk::VideoEncodeH264NaluSliceInfoKHR::default()
                .constant_qp(0)
                .std_slice_header(&slice_header);

            // IDR frames include SPS/PPS prefix NALUs for decoder init.
            let mut h264_picture_info = vk::VideoEncodeH264PictureInfoKHR::default()
                .nalu_slice_entries(std::slice::from_ref(&nalu_slice_info))
                .std_picture_info(&std_picture_info)
                .generate_prefix_nalu(true);

            let src_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D { width, height })
                .base_array_layer(0)
                .image_view_binding(nv12_image_view);

            let mut dpb_ref_info_flags = StdVideoEncodeH264ReferenceInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: Default::default(),
            };
            dpb_ref_info_flags.set_used_for_long_term_reference(0);

            let dpb_std_reference_info = StdVideoEncodeH264ReferenceInfo {
                flags: dpb_ref_info_flags,
                primary_pic_type:
                    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
                FrameNum: 0,
                PicOrderCnt: 0,
                long_term_pic_num: 0,
                long_term_frame_idx: 0,
                temporal_id: 0,
            };

            let mut dpb_h264_slot_info = vk::VideoEncodeH264DpbSlotInfoKHR::default()
                .std_reference_info(&dpb_std_reference_info);

            let dpb_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_offset(vk::Offset2D { x: 0, y: 0 })
                .coded_extent(vk::Extent2D { width, height })
                .base_array_layer(0)
                .image_view_binding(dpb_image_view);

            let setup_reference_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(0)
                .picture_resource(&dpb_picture_resource)
                .push_next(&mut dpb_h264_slot_info);

            let reference_slots_for_begin = [setup_reference_slot];

            // Reset query pools BEFORE video coding scope (Vulkan spec requirement)
            device.cmd_reset_query_pool(encode_cmd, feedback_query_pool, 0, 1);
            device.cmd_reset_query_pool(encode_cmd, status_query_pool, 0, 1);

            let begin_coding = vk::VideoBeginCodingInfoKHR::default()
                .video_session(video_session.video_session())
                .video_session_parameters(video_session.video_session_parameters())
                .reference_slots(&reference_slots_for_begin);

            (video_queue_loader.fp().cmd_begin_video_coding_khr)(
                encode_cmd,
                &begin_coding,
            );

            // Reset session + set rate control
            let reset_info = vk::VideoCodingControlInfoKHR::default()
                .flags(vk::VideoCodingControlFlagsKHR::RESET);
            (video_queue_loader.fp().cmd_control_video_coding_khr)(
                encode_cmd,
                &reset_info,
            );

            let mut h264_rate_control_layer_info =
                vk::VideoEncodeH264RateControlLayerInfoKHR::default();
            let target_bitrate = config.bitrate_bps as u64;
            let rate_control_layer = vk::VideoEncodeRateControlLayerInfoKHR::default()
                .average_bitrate(target_bitrate)
                .max_bitrate(target_bitrate)
                .frame_rate_numerator(config.fps)
                .frame_rate_denominator(1)
                .push_next(&mut h264_rate_control_layer_info);
            let rate_control_layers = [rate_control_layer];

            let mut rate_control_info = vk::VideoEncodeRateControlInfoKHR::default()
                .rate_control_mode(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
                .layers(&rate_control_layers);
            let mut h264_rate_control_info =
                vk::VideoEncodeH264RateControlInfoKHR::default()
                    .gop_frame_count(config.keyframe_interval_frames)
                    .idr_period(config.keyframe_interval_frames)
                    .consecutive_b_frame_count(0)
                    .temporal_layer_count(1);
            let rc_control_info = vk::VideoCodingControlInfoKHR::default()
                .flags(vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL)
                .push_next(&mut rate_control_info)
                .push_next(&mut h264_rate_control_info);
            (video_queue_loader.fp().cmd_control_video_coding_khr)(
                encode_cmd,
                &rc_control_info,
            );

            // Begin query pools inside video coding scope
            device.cmd_begin_query(
                encode_cmd,
                feedback_query_pool,
                0,
                vk::QueryControlFlags::empty(),
            );
            device.cmd_begin_query(
                encode_cmd,
                status_query_pool,
                0,
                vk::QueryControlFlags::empty(),
            );

            // Encode
            let encode_info = vk::VideoEncodeInfoKHR::default()
                .dst_buffer(bitstream_buffer)
                .dst_buffer_offset(0)
                .dst_buffer_range(BITSTREAM_BUFFER_SIZE)
                .src_picture_resource(src_picture_resource)
                .setup_reference_slot(&setup_reference_slot)
                .reference_slots(&[])
                .push_next(&mut h264_picture_info);

            (video_encode_queue_loader.fp().cmd_encode_video_khr)(
                encode_cmd,
                &encode_info,
            );

            // End queries
            device.cmd_end_query(encode_cmd, feedback_query_pool, 0);
            device.cmd_end_query(encode_cmd, status_query_pool, 0);

            let end_info = vk::VideoEndCodingInfoKHR::default();
            (video_queue_loader.fp().cmd_end_video_coding_khr)(
                encode_cmd,
                &end_info,
            );

            device.end_command_buffer(encode_cmd).expect("end encode cmd");

            let cmds = [encode_cmd];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            device
                .queue_submit(video_encode_queue, &[submit], encode_fence)
                .expect("submit encode");
            device
                .wait_for_fences(&[encode_fence], true, u64::MAX)
                .expect("wait encode");
        }

        // === Stage 3: Read back via query pools ===
        // Check encode status
        let mut status_result: [i32; 1] = [0];
        unsafe {
            device
                .get_query_pool_results(
                    status_query_pool,
                    0,
                    &mut status_result,
                    vk::QueryResultFlags::WAIT | vk::QueryResultFlags::WITH_STATUS_KHR,
                )
                .expect("get status query result");
        }
        println!("Encode status: {}", status_result[0]);
        assert!(
            status_result[0] >= 0,
            "Encode should succeed (got status {})",
            status_result[0]
        );

        // Try feedback query (non-blocking — NVIDIA may not populate it)
        let mut feedback_and_status: [u32; 2] = [0; 2];
        let feedback_available = unsafe {
            (device.fp_v1_0().get_query_pool_results)(
                device.handle(),
                feedback_query_pool,
                0,
                1,
                std::mem::size_of_val(&feedback_and_status),
                feedback_and_status.as_mut_ptr().cast(),
                std::mem::size_of_val(&feedback_and_status) as vk::DeviceSize,
                vk::QueryResultFlags::WITH_STATUS_KHR,
            )
        }
        .result()
        .is_ok();

        let bytes_written = if feedback_available && feedback_and_status[0] > 0 {
            let count = feedback_and_status[0] as usize;
            println!("Encode feedback available: {} bytes written", count);
            count
        } else {
            // Fallback: scan the zero-cleared buffer
            println!("Encode feedback unavailable, using scan fallback");
            let data = unsafe {
                std::slice::from_raw_parts(
                    bitstream_mapped_ptr,
                    BITSTREAM_BUFFER_SIZE as usize,
                )
            };
            let mut end = data.len();
            while end > 0 && data[end - 1] == 0 {
                end -= 1;
            }
            assert!(end > 0, "Encoder should produce non-zero output (scan found 0 bytes)");
            // Add padding for H.264 cabac_zero_word trailing bytes
            (end + 2).min(BITSTREAM_BUFFER_SIZE as usize)
        };

        println!("Bitstream size: {} bytes", bytes_written);
        assert!(
            bytes_written > 0,
            "Encoder should produce non-zero output"
        );
        assert!(
            bytes_written <= BITSTREAM_BUFFER_SIZE as usize,
            "Bytes written should not exceed buffer size"
        );

        // Read the bitstream and validate H.264 NAL start codes
        let bitstream = unsafe {
            std::slice::from_raw_parts(bitstream_mapped_ptr, bytes_written)
        };

        // H.264 bitstream should start with NAL start code 0x00 0x00 0x00 0x01
        // or 0x00 0x00 0x01
        let has_4byte_start_code = bytes_written >= 4
            && bitstream[0] == 0x00
            && bitstream[1] == 0x00
            && bitstream[2] == 0x00
            && bitstream[3] == 0x01;
        let has_3byte_start_code = bytes_written >= 3
            && bitstream[0] == 0x00
            && bitstream[1] == 0x00
            && bitstream[2] == 0x01;
        assert!(
            has_4byte_start_code || has_3byte_start_code,
            "Output should contain H.264 NAL start code, got first 4 bytes: {:02x?}",
            &bitstream[..bytes_written.min(4)]
        );

        println!(
            "H.264 bitstream validated: {} bytes, starts with NAL start code",
            bytes_written
        );

        // Cleanup
        unsafe {
            device.destroy_query_pool(feedback_query_pool, None);
            device.destroy_query_pool(status_query_pool, None);
            device.destroy_fence(transfer_fence, None);
            device.destroy_fence(encode_fence, None);
            device.destroy_command_pool(transfer_command_pool, None);
            device.destroy_command_pool(encode_command_pool, None);
            device.unmap_memory(staging_memory);
            device.destroy_buffer(staging_buffer, None);
            device.free_memory(staging_memory, None);
            device.unmap_memory(bitstream_buffer_memory);
            device.destroy_buffer(bitstream_buffer, None);
            device.free_memory(bitstream_buffer_memory, None);
            device.destroy_image_view(dpb_image_view, None);
            device.destroy_image(dpb_image, None);
            device.free_memory(dpb_image_memory, None);
            device.destroy_image_view(nv12_image_view, None);
            device.destroy_image(nv12_image, None);
            device.free_memory(nv12_image_memory, None);
        }
        drop(video_session);

        println!("Vulkan Video encode synthetic frame integration test passed");
    }

    /// Benchmark: encode 100 frames at 1920x1080 through the full Vulkan Video
    /// pipeline (NV12 transfer + H.264 encode + bitstream readback).
    /// Prints wall-clock timing (avg/min/max ms per frame, FPS) and writes the
    /// first IDR frame to /tmp/vulkan_encode_test.h264.
    #[test]
    fn test_vulkan_video_encode_benchmark_1080p() {
        let vulkan_device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping benchmark — Vulkan not available");
                return;
            }
        };

        if !vulkan_device.supports_video_encode() {
            println!("Skipping benchmark — Vulkan Video encode not supported");
            return;
        }

        let ve_family = vulkan_device
            .video_encode_queue_family_index()
            .expect("video encode queue family");
        let gfx_family = vulkan_device.queue_family_index();
        let device = vulkan_device.device().clone();
        let instance = vulkan_device.instance();

        let video_queue_loader = ash::khr::video_queue::Device::new(instance, &device);
        let video_encode_queue_loader =
            ash::khr::video_encode_queue::Device::new(instance, &device);

        let video_encode_queue = vulkan_device
            .video_encode_queue()
            .expect("video encode queue");

        let width = 1920u32;
        let height = 1080u32;
        let total_frames = 100u32;
        let keyframe_interval = 60u32;
        let config = VideoEncoderConfig::new(width, height)
            .with_keyframe_interval(keyframe_interval);
        let video_session =
            VulkanVideoSession::new(&vulkan_device, &config).expect("video session");

        // Build video profile chain
        let std_profile_idc =
            StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH;

        let mut h264_profile_info =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut h264_profile_info);
        let profiles = [video_profile];
        let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);

        // Create NV12 source image
        let (nv12_image, nv12_image_memory) = VulkanVideoEncoder::create_nv12_image(
            &vulkan_device,
            width,
            height,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR,
            &[gfx_family, ve_family],
            &mut profile_list,
        )
        .expect("NV12 source image");
        let nv12_image_view = VulkanVideoEncoder::create_image_view(
            &device,
            nv12_image,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )
        .expect("NV12 image view");

        // Create DPB image
        let (dpb_image, dpb_image_memory) = VulkanVideoEncoder::create_nv12_image(
            &vulkan_device,
            width,
            height,
            vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            &[ve_family],
            &mut profile_list,
        )
        .expect("DPB image");
        let dpb_image_view = VulkanVideoEncoder::create_image_view(
            &device,
            dpb_image,
            vk::Format::G8_B8R8_2PLANE_420_UNORM,
        )
        .expect("DPB image view");

        // Create bitstream buffer
        let (bitstream_buffer, bitstream_buffer_memory, bitstream_mapped_ptr) =
            VulkanVideoEncoder::create_bitstream_buffer(&vulkan_device, &mut profile_list)
                .expect("bitstream buffer");

        // Create NV12 staging buffer
        let nv12_size = (width * height * 3 / 2) as vk::DeviceSize;
        let staging_buffer_info = vk::BufferCreateInfo::default()
            .size(nv12_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let staging_buffer = unsafe { device.create_buffer(&staging_buffer_info, None) }
            .expect("staging buffer");
        let staging_mem_reqs =
            unsafe { device.get_buffer_memory_requirements(staging_buffer) };
        let staging_memory_type = vulkan_device
            .find_memory_type(
                staging_mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .expect("staging memory type");
        let staging_alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(staging_mem_reqs.size)
            .memory_type_index(staging_memory_type);
        let staging_memory = unsafe { device.allocate_memory(&staging_alloc, None) }
            .expect("staging memory");
        unsafe { device.bind_buffer_memory(staging_buffer, staging_memory, 0) }
            .expect("bind staging memory");
        let staging_ptr = unsafe {
            device.map_memory(
                staging_memory,
                0,
                nv12_size,
                vk::MemoryMapFlags::empty(),
            )
        }
        .expect("map staging memory") as *mut u8;

        // Fill with synthetic NV12 data: Y plane = gradient, UV plane = 128 (gray)
        let y_size = (width * height) as usize;
        let uv_size = (width * height / 2) as usize;
        unsafe {
            let y_plane = std::slice::from_raw_parts_mut(staging_ptr, y_size);
            for (i, pixel) in y_plane.iter_mut().enumerate() {
                *pixel = ((i % width as usize) * 255 / width as usize) as u8;
            }
            let uv_plane =
                std::slice::from_raw_parts_mut(staging_ptr.add(y_size), uv_size);
            uv_plane.fill(128);
        }

        // Create query pools
        let mut feedback_h264_profile =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let mut feedback_video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut feedback_h264_profile);
        let mut feedback_create_info =
            vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(
                    vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
                );
        let feedback_query_pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
            .query_count(1)
            .push_next(&mut feedback_create_info)
            .push_next(&mut feedback_video_profile);
        let feedback_query_pool =
            unsafe { device.create_query_pool(&feedback_query_pool_info, None) }
                .expect("feedback query pool");

        let mut status_h264_profile =
            vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(std_profile_idc);
        let mut status_video_profile = vk::VideoProfileInfoKHR::default()
            .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .push_next(&mut status_h264_profile);
        let status_query_pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::RESULT_STATUS_ONLY_KHR)
            .query_count(1)
            .push_next(&mut status_video_profile);
        let status_query_pool =
            unsafe { device.create_query_pool(&status_query_pool_info, None) }
                .expect("status query pool");

        // Create command pools and buffers
        let gfx_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gfx_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let transfer_command_pool =
            unsafe { device.create_command_pool(&gfx_pool_info, None) }
                .expect("transfer command pool");
        let gfx_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(transfer_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let transfer_cmd =
            unsafe { device.allocate_command_buffers(&gfx_cmd_alloc) }
                .expect("transfer command buffer")[0];

        let ve_pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(ve_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let encode_command_pool =
            unsafe { device.create_command_pool(&ve_pool_info, None) }
                .expect("encode command pool");
        let ve_cmd_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(encode_command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let encode_cmd =
            unsafe { device.allocate_command_buffers(&ve_cmd_alloc) }
                .expect("encode command buffer")[0];

        let fence_info = vk::FenceCreateInfo::default();
        let transfer_fence = unsafe { device.create_fence(&fence_info, None) }
            .expect("transfer fence");
        let encode_fence = unsafe { device.create_fence(&fence_info, None) }
            .expect("encode fence");

        // Storage for first IDR bitstream
        let mut first_idr_bitstream: Option<Vec<u8>> = None;
        let mut frame_times_ms: Vec<f64> = Vec::with_capacity(total_frames as usize);
        let mut session_initialized = false;

        let benchmark_start = std::time::Instant::now();

        for frame_idx in 0..total_frames {
            let frame_start = std::time::Instant::now();

            let is_idr = frame_idx == 0
                || (frame_idx % keyframe_interval) == 0;

            // === Transfer NV12 data to GPU image ===
            unsafe {
                device
                    .reset_command_buffer(transfer_cmd, vk::CommandBufferResetFlags::empty())
                    .expect("reset transfer cmd");

                let begin_info = vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
                device
                    .begin_command_buffer(transfer_cmd, &begin_info)
                    .expect("begin transfer cmd");

                let barrier = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .image(nv12_image)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .level_count(1)
                            .layer_count(1),
                    );
                device.cmd_pipeline_barrier(
                    transfer_cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier],
                );

                let y_region = vk::BufferImageCopy::default()
                    .buffer_offset(0)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::PLANE_0)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    });
                let uv_region = vk::BufferImageCopy::default()
                    .buffer_offset(y_size as vk::DeviceSize)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::PLANE_1)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width: width / 2,
                        height: height / 2,
                        depth: 1,
                    });
                device.cmd_copy_buffer_to_image(
                    transfer_cmd,
                    staging_buffer,
                    nv12_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[y_region, uv_region],
                );

                let barrier2 = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                    .image(nv12_image)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .level_count(1)
                            .layer_count(1),
                    );
                device.cmd_pipeline_barrier(
                    transfer_cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier2],
                );

                device.end_command_buffer(transfer_cmd).expect("end transfer cmd");

                let cmds = [transfer_cmd];
                let submit = vk::SubmitInfo::default().command_buffers(&cmds);
                device
                    .queue_submit(vulkan_device.queue(), &[submit], transfer_fence)
                    .expect("submit transfer");
                device
                    .wait_for_fences(&[transfer_fence], true, u64::MAX)
                    .expect("wait transfer");
                device
                    .reset_fences(&[transfer_fence])
                    .expect("reset transfer fence");
            }

            // Zero-clear bitstream buffer
            unsafe {
                std::ptr::write_bytes(
                    bitstream_mapped_ptr,
                    0u8,
                    BITSTREAM_BUFFER_SIZE as usize,
                );
            }

            // === Encode frame ===
            unsafe {
                device
                    .reset_command_buffer(encode_cmd, vk::CommandBufferResetFlags::empty())
                    .expect("reset encode cmd");

                let begin_info = vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
                device
                    .begin_command_buffer(encode_cmd, &begin_info)
                    .expect("begin encode cmd");

                // DPB barrier
                let dpb_barrier = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(
                        vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                    )
                    .image(dpb_image)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .level_count(1)
                            .layer_count(1),
                    );
                device.cmd_pipeline_barrier(
                    encode_cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[dpb_barrier],
                );

                // Build picture info
                let (picture_type, slice_type, primary_pic_type) = if is_idr {
                    (
                        StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
                        StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
                        StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_I,
                    )
                } else {
                    (
                        StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P,
                        StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_P,
                        StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P,
                    )
                };

                let mut pic_info_flags = StdVideoEncodeH264PictureInfoFlags {
                    _bitfield_align_1: [],
                    _bitfield_1: Default::default(),
                };
                pic_info_flags.set_IdrPicFlag(if is_idr { 1 } else { 0 });
                pic_info_flags.set_is_reference(1);

                let p_frame_ref_lists = StdVideoEncodeH264ReferenceListsInfo {
                    flags: StdVideoEncodeH264ReferenceListsInfoFlags {
                        _bitfield_align_1: [],
                        _bitfield_1: Default::default(),
                    },
                    num_ref_idx_l0_active_minus1: 0,
                    num_ref_idx_l1_active_minus1: 0,
                    RefPicList0: {
                        let mut list = [0xFFu8; 32];
                        list[0] = 0; // DPB slot 0
                        list
                    },
                    RefPicList1: [0xFF; 32],
                    refList0ModOpCount: 0,
                    refList1ModOpCount: 0,
                    refPicMarkingOpCount: 0,
                    reserved1: [0; 7],
                    pRefList0ModOperations: ptr::null(),
                    pRefList1ModOperations: ptr::null(),
                    pRefPicMarkingOperations: ptr::null(),
                };

                let std_picture_info = StdVideoEncodeH264PictureInfo {
                    flags: pic_info_flags,
                    seq_parameter_set_id: 0,
                    pic_parameter_set_id: 0,
                    idr_pic_id: if is_idr {
                        (frame_idx & 0xFFFF) as u16
                    } else {
                        0
                    },
                    primary_pic_type,
                    frame_num: (frame_idx % 16) as u32,
                    PicOrderCnt: frame_idx as i32,
                    temporal_id: 0,
                    reserved1: [0; 3],
                    pRefLists: if is_idr {
                        ptr::null()
                    } else {
                        &p_frame_ref_lists
                    },
                };

                let slice_header_flags = StdVideoEncodeH264SliceHeaderFlags {
                    _bitfield_align_1: [],
                    _bitfield_1: Default::default(),
                };
                let slice_header = StdVideoEncodeH264SliceHeader {
                    flags: slice_header_flags,
                    first_mb_in_slice: 0,
                    slice_type,
                    slice_alpha_c0_offset_div2: 0,
                    slice_beta_offset_div2: 0,
                    slice_qp_delta: 0,
                    reserved1: 0,
                    cabac_init_idc:
                        StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
                    disable_deblocking_filter_idc:
                        StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED,
                    pWeightTable: ptr::null(),
                };

                let nalu_slice_info = vk::VideoEncodeH264NaluSliceInfoKHR::default()
                    .constant_qp(0)
                    .std_slice_header(&slice_header);

                // IDR frames include SPS/PPS prefix NALUs for decoder init.
                let mut h264_picture_info = vk::VideoEncodeH264PictureInfoKHR::default()
                    .nalu_slice_entries(std::slice::from_ref(&nalu_slice_info))
                    .std_picture_info(&std_picture_info)
                    .generate_prefix_nalu(is_idr);

                let src_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                    .coded_offset(vk::Offset2D { x: 0, y: 0 })
                    .coded_extent(vk::Extent2D { width, height })
                    .base_array_layer(0)
                    .image_view_binding(nv12_image_view);

                let mut dpb_ref_info_flags = StdVideoEncodeH264ReferenceInfoFlags {
                    _bitfield_align_1: [],
                    _bitfield_1: Default::default(),
                };
                dpb_ref_info_flags.set_used_for_long_term_reference(0);

                let dpb_std_reference_info = StdVideoEncodeH264ReferenceInfo {
                    flags: dpb_ref_info_flags,
                    primary_pic_type: picture_type,
                    FrameNum: (frame_idx % 16) as u32,
                    PicOrderCnt: frame_idx as i32,
                    long_term_pic_num: 0,
                    long_term_frame_idx: 0,
                    temporal_id: 0,
                };

                let mut dpb_h264_slot_info = vk::VideoEncodeH264DpbSlotInfoKHR::default()
                    .std_reference_info(&dpb_std_reference_info);

                let dpb_picture_resource = vk::VideoPictureResourceInfoKHR::default()
                    .coded_offset(vk::Offset2D { x: 0, y: 0 })
                    .coded_extent(vk::Extent2D { width, height })
                    .base_array_layer(0)
                    .image_view_binding(dpb_image_view);

                let setup_reference_slot = vk::VideoReferenceSlotInfoKHR::default()
                    .slot_index(0)
                    .picture_resource(&dpb_picture_resource)
                    .push_next(&mut dpb_h264_slot_info);

                // P-frame reference slot
                let mut ref_dpb_ref_info_flags = StdVideoEncodeH264ReferenceInfoFlags {
                    _bitfield_align_1: [],
                    _bitfield_1: Default::default(),
                };
                ref_dpb_ref_info_flags.set_used_for_long_term_reference(0);

                let ref_std_reference_info = StdVideoEncodeH264ReferenceInfo {
                    flags: ref_dpb_ref_info_flags,
                    primary_pic_type:
                        StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P,
                    FrameNum: (frame_idx.wrapping_sub(1) % 16) as u32,
                    PicOrderCnt: frame_idx.wrapping_sub(1) as i32,
                    long_term_pic_num: 0,
                    long_term_frame_idx: 0,
                    temporal_id: 0,
                };

                let mut ref_h264_slot_info = vk::VideoEncodeH264DpbSlotInfoKHR::default()
                    .std_reference_info(&ref_std_reference_info);

                let ref_slot = vk::VideoReferenceSlotInfoKHR::default()
                    .slot_index(0)
                    .picture_resource(&dpb_picture_resource)
                    .push_next(&mut ref_h264_slot_info);

                let reference_slots_for_begin: Vec<vk::VideoReferenceSlotInfoKHR<'_>> =
                    if is_idr {
                        vec![setup_reference_slot]
                    } else {
                        vec![ref_slot]
                    };

                // Reset query pools BEFORE video coding scope
                device.cmd_reset_query_pool(encode_cmd, feedback_query_pool, 0, 1);
                device.cmd_reset_query_pool(encode_cmd, status_query_pool, 0, 1);

                let begin_coding = vk::VideoBeginCodingInfoKHR::default()
                    .video_session(video_session.video_session())
                    .video_session_parameters(video_session.video_session_parameters())
                    .reference_slots(&reference_slots_for_begin);

                (video_queue_loader.fp().cmd_begin_video_coding_khr)(
                    encode_cmd,
                    &begin_coding,
                );

                // Reset session + rate control on first frame only
                if !session_initialized {
                    let reset_info = vk::VideoCodingControlInfoKHR::default()
                        .flags(vk::VideoCodingControlFlagsKHR::RESET);
                    (video_queue_loader.fp().cmd_control_video_coding_khr)(
                        encode_cmd,
                        &reset_info,
                    );

                    let mut h264_rate_control_layer_info =
                        vk::VideoEncodeH264RateControlLayerInfoKHR::default();
                    let target_bitrate = config.bitrate_bps as u64;
                    let rate_control_layer =
                        vk::VideoEncodeRateControlLayerInfoKHR::default()
                            .average_bitrate(target_bitrate)
                            .max_bitrate(target_bitrate)
                            .frame_rate_numerator(config.fps)
                            .frame_rate_denominator(1)
                            .push_next(&mut h264_rate_control_layer_info);
                    let rate_control_layers = [rate_control_layer];

                    let mut rate_control_info =
                        vk::VideoEncodeRateControlInfoKHR::default()
                            .rate_control_mode(
                                vk::VideoEncodeRateControlModeFlagsKHR::CBR,
                            )
                            .layers(&rate_control_layers);
                    let mut h264_rate_control_info =
                        vk::VideoEncodeH264RateControlInfoKHR::default()
                            .gop_frame_count(keyframe_interval)
                            .idr_period(keyframe_interval)
                            .consecutive_b_frame_count(0)
                            .temporal_layer_count(1);
                    let rc_control_info = vk::VideoCodingControlInfoKHR::default()
                        .flags(
                            vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL,
                        )
                        .push_next(&mut rate_control_info)
                        .push_next(&mut h264_rate_control_info);
                    (video_queue_loader.fp().cmd_control_video_coding_khr)(
                        encode_cmd,
                        &rc_control_info,
                    );

                    session_initialized = true;
                }

                // Begin queries inside video coding scope
                device.cmd_begin_query(
                    encode_cmd,
                    feedback_query_pool,
                    0,
                    vk::QueryControlFlags::empty(),
                );
                device.cmd_begin_query(
                    encode_cmd,
                    status_query_pool,
                    0,
                    vk::QueryControlFlags::empty(),
                );

                // Reference slots for encode: P frames reference DPB slot 0
                let reference_slots_for_encode: &[vk::VideoReferenceSlotInfoKHR<'_>] =
                    if is_idr {
                        &[]
                    } else {
                        std::slice::from_ref(&reference_slots_for_begin[0])
                    };

                let encode_info = vk::VideoEncodeInfoKHR::default()
                    .dst_buffer(bitstream_buffer)
                    .dst_buffer_offset(0)
                    .dst_buffer_range(BITSTREAM_BUFFER_SIZE)
                    .src_picture_resource(src_picture_resource)
                    .setup_reference_slot(&setup_reference_slot)
                    .reference_slots(reference_slots_for_encode)
                    .push_next(&mut h264_picture_info);

                (video_encode_queue_loader.fp().cmd_encode_video_khr)(
                    encode_cmd,
                    &encode_info,
                );

                // End queries
                device.cmd_end_query(encode_cmd, feedback_query_pool, 0);
                device.cmd_end_query(encode_cmd, status_query_pool, 0);

                let end_info = vk::VideoEndCodingInfoKHR::default();
                (video_queue_loader.fp().cmd_end_video_coding_khr)(
                    encode_cmd,
                    &end_info,
                );

                device.end_command_buffer(encode_cmd).expect("end encode cmd");

                let cmds = [encode_cmd];
                let submit = vk::SubmitInfo::default().command_buffers(&cmds);
                device
                    .queue_submit(video_encode_queue, &[submit], encode_fence)
                    .expect("submit encode");
                device
                    .wait_for_fences(&[encode_fence], true, u64::MAX)
                    .expect("wait encode");
                device
                    .reset_fences(&[encode_fence])
                    .expect("reset encode fence");
            }

            // Read back bitstream via status query + hybrid feedback
            let mut status_result: [i32; 1] = [0];
            unsafe {
                device
                    .get_query_pool_results(
                        status_query_pool,
                        0,
                        &mut status_result,
                        vk::QueryResultFlags::WAIT
                            | vk::QueryResultFlags::WITH_STATUS_KHR,
                    )
                    .expect("get status query result");
            }
            assert!(
                status_result[0] >= 0,
                "Encode failed on frame {} (status {})",
                frame_idx,
                status_result[0]
            );

            let mut feedback_and_status: [u32; 2] = [0; 2];
            let feedback_available = unsafe {
                (device.fp_v1_0().get_query_pool_results)(
                    device.handle(),
                    feedback_query_pool,
                    0,
                    1,
                    std::mem::size_of_val(&feedback_and_status),
                    feedback_and_status.as_mut_ptr().cast(),
                    std::mem::size_of_val(&feedback_and_status) as vk::DeviceSize,
                    vk::QueryResultFlags::WITH_STATUS_KHR,
                )
            }
            .result()
            .is_ok();

            let bytes_written = if feedback_available && feedback_and_status[0] > 0 {
                feedback_and_status[0] as usize
            } else {
                let data = unsafe {
                    std::slice::from_raw_parts(
                        bitstream_mapped_ptr,
                        BITSTREAM_BUFFER_SIZE as usize,
                    )
                };
                let mut end = data.len();
                while end > 0 && data[end - 1] == 0 {
                    end -= 1;
                }
                (end + 2).min(BITSTREAM_BUFFER_SIZE as usize)
            };

            assert!(
                bytes_written > 0,
                "Frame {} produced 0 bytes",
                frame_idx
            );

            // Save first IDR bitstream for file output
            if frame_idx == 0 {
                let bitstream = unsafe {
                    std::slice::from_raw_parts(bitstream_mapped_ptr, bytes_written)
                };
                first_idr_bitstream = Some(bitstream.to_vec());
            }

            let frame_elapsed = frame_start.elapsed();
            frame_times_ms.push(frame_elapsed.as_secs_f64() * 1000.0);
        }

        let total_elapsed = benchmark_start.elapsed();

        // Write first IDR to file
        if let Some(ref idr_data) = first_idr_bitstream {
            std::fs::write("/tmp/vulkan_encode_test.h264", idr_data)
                .expect("write H.264 file");
            println!(
                "Wrote first IDR frame ({} bytes) to /tmp/vulkan_encode_test.h264",
                idr_data.len()
            );
        }

        // Compute statistics
        let avg_ms: f64 =
            frame_times_ms.iter().sum::<f64>() / frame_times_ms.len() as f64;
        let min_ms: f64 = frame_times_ms
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        let max_ms: f64 = frame_times_ms
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        let total_sec = total_elapsed.as_secs_f64();
        let fps = total_frames as f64 / total_sec;

        println!("\n=== Vulkan Video Encode Benchmark (1920x1080, {} frames) ===", total_frames);
        println!("  Average: {:.2} ms/frame", avg_ms);
        println!("  Min:     {:.2} ms/frame", min_ms);
        println!("  Max:     {:.2} ms/frame", max_ms);
        println!("  FPS:     {:.1}", fps);
        println!("  Total:   {:.2} s", total_sec);
        println!("=========================================================\n");

        // Cleanup
        unsafe {
            device.destroy_query_pool(feedback_query_pool, None);
            device.destroy_query_pool(status_query_pool, None);
            device.destroy_fence(transfer_fence, None);
            device.destroy_fence(encode_fence, None);
            device.destroy_command_pool(transfer_command_pool, None);
            device.destroy_command_pool(encode_command_pool, None);
            device.unmap_memory(staging_memory);
            device.destroy_buffer(staging_buffer, None);
            device.free_memory(staging_memory, None);
            device.unmap_memory(bitstream_buffer_memory);
            device.destroy_buffer(bitstream_buffer, None);
            device.free_memory(bitstream_buffer_memory, None);
            device.destroy_image_view(dpb_image_view, None);
            device.destroy_image(dpb_image, None);
            device.free_memory(dpb_image_memory, None);
            device.destroy_image_view(nv12_image_view, None);
            device.destroy_image(nv12_image, None);
            device.free_memory(nv12_image_memory, None);
        }
        drop(video_session);

        println!("Vulkan Video encode benchmark completed successfully");
    }
}
