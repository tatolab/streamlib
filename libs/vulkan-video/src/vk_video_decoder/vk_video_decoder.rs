// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoDecoder.h + VkVideoDecoder.cpp
//!
//! Main Vulkan Video decode pipeline. Creates video sessions, manages DPB
//! images, submits decode commands.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk::{
    self,
    KhrVideoQueueExtensionDeviceCommands,
    KhrVideoDecodeQueueExtensionDeviceCommands,
};
use vulkanalia_vma as vma;
use vma::Alloc;
use std::sync::Arc;
use std::ptr;

use crate::video_context::{VideoContext, VideoError, VideoResult};
use crate::codec_utils::vulkan_video_session::VulkanVideoSession;
use crate::decode::{DecodeSubmitInfo, DecodedFrame};
use super::vk_parser_video_picture_parameters::VkParserVideoPictureParameters;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 100 ms fence timeout in nanoseconds.
const FENCE_TIMEOUT: u64 = 100 * 1000 * 1000;
/// 1000 ms long timeout in nanoseconds.
const LONG_TIMEOUT: u64 = 1000 * 1000 * 1000;

/// GPU alignment helper — round up to 256-byte boundary.
#[inline]
fn gpu_align(x: u64) -> u64 {
    (x + 0xFF) & !0xFF
}

/// Invalid image-type sentinel (matches C++ `InvalidImageTypeIdx`).
const INVALID_IMAGE_TYPE_IDX: u8 = u8::MAX;

// ---------------------------------------------------------------------------
// Small structs mirroring C++ header declarations
// ---------------------------------------------------------------------------

/// Simple rectangle (left, top, right, bottom).
#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

/// Simple dimension (width, height).
#[derive(Debug, Clone, Copy, Default)]
pub struct Dim {
    pub w: i32,
    pub h: i32,
}

/// Per-slot frame data for a single decode operation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NvVkDecodeFrameDataSlot {
    pub slot: u32,
    pub command_buffer: vk::CommandBuffer,
}

// ---------------------------------------------------------------------------
// NvVkDecodeFrameData
// ---------------------------------------------------------------------------

/// Owns the command pool and command buffers used for decode submissions.
///
/// Mirrors the C++ `NvVkDecodeFrameData` class.
///
/// # Divergence from C++
/// - `VulkanBitstreamBufferPool` is not yet ported; represented as a placeholder.
/// - Vulkan device context is an opaque pointer; will be replaced by `vulkanalia::Device`.
pub struct NvVkDecodeFrameData {
    _vk_dev_ctx: *const (),
    video_command_pool: vk::CommandPool,
    command_buffers: Vec<vk::CommandBuffer>,
    // TODO: bitstream_buffers_queue (VulkanVideoRefCountedPool) placeholder
}

// Safety: raw pointer is only an opaque handle.
unsafe impl Send for NvVkDecodeFrameData {}
unsafe impl Sync for NvVkDecodeFrameData {}

impl NvVkDecodeFrameData {
    pub fn new(vk_dev_ctx: *const ()) -> Self {
        Self {
            _vk_dev_ctx: vk_dev_ctx,
            video_command_pool: vk::CommandPool::null(),
            command_buffers: Vec::new(),
        }
    }

    /// Allocate command buffers for `max_decode_frames_count` slots.
    ///
    /// Mirrors `resize`.
    pub fn resize(&mut self, max_decode_frames_count: usize) -> usize {
        if self.video_command_pool == vk::CommandPool::null() {
            // TODO: Create command pool and allocate buffers via vulkanalia::Device.
            // Placeholder — just size the vector.
            self.command_buffers
                .resize(max_decode_frames_count, vk::CommandBuffer::null());
            max_decode_frames_count
        } else {
            let existing = self.command_buffers.len();
            debug_assert!(max_decode_frames_count <= existing);
            existing
        }
    }

    pub fn get_command_buffer(&self, slot: u32) -> vk::CommandBuffer {
        debug_assert!((slot as usize) < self.command_buffers.len());
        self.command_buffers[slot as usize]
    }

    pub fn size(&self) -> usize {
        self.command_buffers.len()
    }

    /// Release command pool resources.
    ///
    /// Mirrors `deinit`.
    pub fn deinit(&mut self) {
        if self.video_command_pool != vk::CommandPool::null() {
            // TODO: vkFreeCommandBuffers + vkDestroyCommandPool via vulkanalia::Device.
            self.command_buffers.clear();
            self.video_command_pool = vk::CommandPool::null();
        }
    }
}

impl Drop for NvVkDecodeFrameData {
    fn drop(&mut self) {
        self.deinit();
    }
}

// ---------------------------------------------------------------------------
// Decoder feature flags
// ---------------------------------------------------------------------------

/// Feature flags passed to `VkVideoDecoder::create`.
///
/// Mirrors the C++ `DecoderFeatures` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderFeatures(pub u32);

impl DecoderFeatures {
    pub const ENABLE_LINEAR_OUTPUT: Self              = Self(1 << 0);
    pub const ENABLE_HW_LOAD_BALANCING: Self          = Self(1 << 1);
    pub const ENABLE_POST_PROCESS_FILTER: Self        = Self(1 << 2);
    pub const ENABLE_GRAPHICS_TEXTURE_SAMPLING: Self  = Self(1 << 3);
    pub const ENABLE_EXTERNAL_CONSUMER_EXPORT: Self   = Self(1 << 4);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & 0x1F)
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for DecoderFeatures {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

// ---------------------------------------------------------------------------
// Filter type placeholder
// ---------------------------------------------------------------------------

/// Placeholder for VulkanFilterYuvCompute::FilterType.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FilterType {
    YcbcrCopy = 0,
}

impl Default for FilterType {
    fn default() -> Self {
        Self::YcbcrCopy
    }
}

// ---------------------------------------------------------------------------
// Image specs index (per-frame image type bookkeeping)
// ---------------------------------------------------------------------------

/// Tracks which image-spec indices are used for DPB, output, filter, linear,
/// and display images. Mirrors `DecodeFrameBufferIf::ImageSpecsIndex`.
#[derive(Debug, Clone, Copy)]
pub struct ImageSpecsIndex {
    pub decode_dpb: u8,
    pub decode_out: u8,
    pub filter_out: u8,
    pub filter_in: u8,
    pub linear_out: u8,
    pub display_out: u8,
    pub film_grain_out: u8,
}

impl Default for ImageSpecsIndex {
    fn default() -> Self {
        Self {
            decode_dpb: INVALID_IMAGE_TYPE_IDX,
            decode_out: INVALID_IMAGE_TYPE_IDX,
            filter_out: INVALID_IMAGE_TYPE_IDX,
            filter_in: INVALID_IMAGE_TYPE_IDX,
            linear_out: INVALID_IMAGE_TYPE_IDX,
            display_out: INVALID_IMAGE_TYPE_IDX,
            film_grain_out: INVALID_IMAGE_TYPE_IDX,
        }
    }
}

// ---------------------------------------------------------------------------
// Placeholder types for parser / frame-buffer interfaces
// ---------------------------------------------------------------------------

/// Detected video format from the parser. Mirrors `VkParserDetectedVideoFormat`.
#[derive(Debug, Clone, Default)]
pub struct VkParserDetectedVideoFormat {
    pub codec: vk::VideoCodecOperationFlagsKHR,
    pub coded_width: u32,
    pub coded_height: u32,
    pub display_area: Rect,
    pub progressive_sequence: bool,
    pub chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
    pub luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
    pub chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
    pub bit_depth_luma_minus8: u32,
    pub codec_profile: u32,
    pub can_use_fields: bool,
    pub min_num_decode_surfaces: u32,
    pub max_num_dpb_slots: u32,
    pub frame_rate_numerator: u32,
    pub frame_rate_denominator: u32,
    pub film_grain_used: bool,
}

/// Per-frame decode parameters from the parser. Placeholder for
/// `VkParserPerFrameDecodeParameters`.
pub struct VkParserPerFrameDecodeParameters {
    pub curr_pic_idx: i32,
    pub bitstream_data_len: u64,
    pub bitstream_data_offset: u64,
    pub first_slice_index: u32,
    pub num_goreference_slots: i32,
    pub use_inlined_picture_parameters: bool,
    // Many more fields in the real struct — stubbed for port structure.
}

/// Decode picture info from the parser. Placeholder for `VkParserDecodePictureInfo`.
pub struct VkParserDecodePictureInfo {
    pub flags: DecodePictureFlags,
    pub image_layer_index: u32,
}

/// Bitfield flags within `VkParserDecodePictureInfo`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodePictureFlags {
    pub apply_film_grain: bool,
    pub unpaired_field: bool,
    pub field_pic: bool,
    pub sync_first_ready: bool,
    pub sync_to_first_field: bool,
}

// ---------------------------------------------------------------------------
// VkVideoDecoder
// ---------------------------------------------------------------------------

/// Maximum render targets. Must be <= 32 (used as u32 bitmask).
pub const MAX_RENDER_TARGETS: usize = 32;

/// The main Vulkan Video decoder. Creates video sessions, manages DPB images,
/// and submits decode commands.
///
/// Mirrors the C++ `VkVideoDecoder` class.
///
/// # Divergence from C++
/// - Inherits no base class; the `IVulkanVideoDecoderHandler` callback
///   interface is represented as method signatures directly on this struct.
/// - Shared ownership via `Arc<VkVideoDecoder>` instead of `VkSharedBaseObj`.
/// - Vulkan device context is opaque `*const ()`; to be replaced by
///   `vulkanalia::Device` + extension function tables.
pub struct VkVideoDecoder {
    // Real Vulkan device context — replaces the C++ opaque pointer.
    ctx: Arc<crate::video_context::VideoContext>,
    queue: vk::Queue,
    queue_family_index: u32,

    // Host-side queue submission gateway (per-queue mutex synchronization).
    submitter: Arc<dyn crate::rhi::RhiQueueSubmitter>,

    current_video_queue_indx: i32,
    coded_extent: vk::Extent2D,
    video_format: VkParserDetectedVideoFormat,
    num_decode_images_in_flight: i32,
    num_decode_images_to_preallocate: i32,

    capability_flags: vk::VideoDecodeCapabilityFlagsKHR,
    video_session: Option<vk::VideoSessionKHR>,
    video_session_arc: Option<Arc<crate::codec_utils::vulkan_video_session::VulkanVideoSession>>,
    decode_frames_data: NvVkDecodeFrameData,

    decode_pic_count: u64,
    current_picture_parameters: Option<Arc<VkParserVideoPictureParameters>>,
    session_parameters: vk::VideoSessionParametersKHR,

    // DPB — array-layered image (VUID-07244 for decode)
    dpb_image: vk::Image,
    dpb_allocation: vma::Allocation,
    dpb_image_views: Vec<vk::ImageView>,
    dpb_slot_layouts: Vec<vk::ImageLayout>,
    dpb_slot_active: Vec<bool>,

    // Command recording
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// True when a decode submission is in flight (fence not yet waited on).
    decode_in_flight: bool,

    // Bitstream buffer
    bitstream_buffer: vk::Buffer,
    bitstream_allocation: vma::Allocation,
    bitstream_size: u64,
    bitstream_mapped_ptr: *mut u8,

    // Video profile (kept alive for DPB image creation)
    video_profile: vk::VideoProfileInfoKHR,

    // Config
    dpb_and_output_coincide: bool,
    reset_decoder: bool,
    use_image_array: bool,
    max_stream_buffer_size: u64,

    // Cross-queue sharing (for post-process compute + readback)
    sharing_queue_families: Vec<u32>,

    // Codec-specific profiles kept alive for video_profile pNext chain
    _h264_decode_profile: Option<Box<vk::VideoDecodeH264ProfileInfoKHR>>,
    _h265_decode_profile: Option<Box<vk::VideoDecodeH265ProfileInfoKHR>>,
}

// SAFETY: Vulkan handles are not tied to a thread. Queue submissions are
// serialized by the caller.
unsafe impl Send for VkVideoDecoder {}
unsafe impl Sync for VkVideoDecoder {}

impl VkVideoDecoder {
    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    /// Create a new VkVideoDecoder with real Vulkan device context.
    pub fn new(
        ctx: Arc<VideoContext>,
        queue_family_index: u32,
        queue: vk::Queue,
        codec_operation: vk::VideoCodecOperationFlagsKHR,
        submitter: Arc<dyn crate::rhi::RhiQueueSubmitter>,
    ) -> VideoResult<Self> {
        let device = ctx.device();

        // Create command pool for the decode queue family
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        let command_pool = unsafe {
            device.create_command_pool(&pool_info, None)
                .map_err(VideoError::from)?
        };

        // Allocate one command buffer
        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);

        let command_buffers = unsafe {
            device.allocate_command_buffers(&alloc_info)
                .map_err(VideoError::from)?
        };
        let command_buffer = command_buffers[0];

        // Create fence
        let fence = unsafe {
            device.create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(VideoError::from)?
        };

        // Build video profile with codec-specific pNext.
        let h264_profile = Box::new(
            vk::VideoDecodeH264ProfileInfoKHR::builder()
                .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH)
                .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE)
                .build(),
        );
        let h265_profile = Box::new(
            vk::VideoDecodeH265ProfileInfoKHR::builder()
                .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN)
                .build(),
        );

        let mut video_profile = vk::VideoProfileInfoKHR::builder()
            .video_codec_operation(codec_operation)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
            .build();

        // Attach codec-specific pNext (must remain alive for session lifetime)
        if codec_operation == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
            video_profile.next = &*h264_profile
                as *const vk::VideoDecodeH264ProfileInfoKHR
                as *const std::ffi::c_void;
        } else if codec_operation == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
            video_profile.next = &*h265_profile
                as *const vk::VideoDecodeH265ProfileInfoKHR
                as *const std::ffi::c_void;
        }

        Ok(Self {
            ctx,
            queue,
            queue_family_index,
            submitter,
            current_video_queue_indx: 0,
            coded_extent: vk::Extent2D::default(),
            video_format: VkParserDetectedVideoFormat::default(),
            num_decode_images_in_flight: 4,
            num_decode_images_to_preallocate: 8,
            capability_flags: vk::VideoDecodeCapabilityFlagsKHR::empty(),
            video_session: None,
            video_session_arc: None,
            decode_frames_data: NvVkDecodeFrameData::new(ptr::null()),
            decode_pic_count: 0,
            current_picture_parameters: None,
            session_parameters: vk::VideoSessionParametersKHR::null(),
            dpb_image: vk::Image::null(),
            dpb_allocation: unsafe { std::mem::zeroed() },
            dpb_image_views: Vec::new(),
            dpb_slot_layouts: Vec::new(),
            dpb_slot_active: Vec::new(),
            command_pool,
            command_buffer,
            fence,
            decode_in_flight: false,
            bitstream_buffer: vk::Buffer::null(),
            bitstream_allocation: unsafe { std::mem::zeroed() },
            bitstream_size: 0,
            bitstream_mapped_ptr: ptr::null_mut(),
            video_profile,
            dpb_and_output_coincide: true,
            reset_decoder: true,
            use_image_array: true,
            max_stream_buffer_size: 2 * 1024 * 1024,
            sharing_queue_families: Vec::new(),
            _h264_decode_profile: Some(h264_profile),
            _h265_decode_profile: Some(h265_profile),
        })
    }

    /// Set queue families for CONCURRENT sharing on the DPB image.
    ///
    /// Must be called before `initialize_video_decoder`. When multiple
    /// queue families are listed, DPB images use `CONCURRENT` sharing so
    /// they can be sampled from a compute queue (e.g. for NV12→RGB
    /// post-processing) without queue family ownership transfers.
    pub fn set_sharing_queue_families(&mut self, families: Vec<u32>) {
        self.sharing_queue_families = families;
    }

    // ------------------------------------------------------------------
    // Static helpers
    // ------------------------------------------------------------------

    /// Return a human-readable name for a video codec operation.
    ///
    /// Mirrors `GetVideoCodecString`.
    pub fn get_video_codec_string(codec: vk::VideoCodecOperationFlagsKHR) -> &'static str {
        if codec == vk::VideoCodecOperationFlagsKHR::NONE {
            "None"
        } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
            "AVC/H.264"
        } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
            "H.265/HEVC"
        } else if codec.bits() & 0x00000100 != 0 {
            // VP9 — VK_VIDEO_CODEC_OPERATION_DECODE_VP9_BIT_KHR (provisional, not yet in ash)
            "VP9"
        } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
            "AV1"
        } else {
            "Unknown"
        }
    }

    /// Return a human-readable name for a chroma subsampling format.
    ///
    /// Mirrors `GetVideoChromaFormatString`.
    pub fn get_video_chroma_format_string(
        chroma: vk::VideoChromaSubsamplingFlagsKHR,
    ) -> &'static str {
        if chroma == vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME {
            "YCbCr 400 (Monochrome)"
        } else if chroma == vk::VideoChromaSubsamplingFlagsKHR::_420 {
            "YCbCr 420"
        } else if chroma == vk::VideoChromaSubsamplingFlagsKHR::_422 {
            "YCbCr 422"
        } else if chroma == vk::VideoChromaSubsamplingFlagsKHR::_444 {
            "YCbCr 444"
        } else {
            "Unknown"
        }
    }

    // ------------------------------------------------------------------
    // Video format info
    // ------------------------------------------------------------------

    /// Return the current detected video format. Only valid after
    /// `start_video_sequence` has been called.
    ///
    /// Mirrors `GetVideoFormatInfo`.
    pub fn get_video_format_info(&self) -> &VkParserDetectedVideoFormat {
        debug_assert!(self.video_format.coded_width != 0);
        &self.video_format
    }

    // ------------------------------------------------------------------
    // IVulkanVideoDecoderHandler callbacks
    // ------------------------------------------------------------------

    /// Called when a new video sequence is detected (codec, resolution change).
    /// Returns the number of decode surfaces allocated, or -1 on error.
    ///
    /// Mirrors `StartVideoSequence`.
    ///
    /// # Placeholder
    /// The full implementation requires Vulkan capability queries, image pool
    /// creation, and session creation — all of which depend on the device
    /// context and frame-buffer subsystems. This port preserves the complete
    /// logic flow with TODO markers for each Vulkan call.
    pub fn start_video_sequence(&mut self, video_format: &VkParserDetectedVideoFormat) -> i32 {
        self.coded_extent.width = video_format.coded_width;
        self.coded_extent.height = video_format.coded_height;

        let display_width = (video_format.display_area.r - video_format.display_area.l) as u32;
        let display_height = (video_format.display_area.b - video_format.display_area.t) as u32;

        let _image_width = display_width.max(video_format.coded_width);
        let _image_height = display_height.max(video_format.coded_height);

        tracing::debug!(
            codec = %Self::get_video_codec_string(video_format.codec),
            frame_rate_num = video_format.frame_rate_numerator,
            frame_rate_den = video_format.frame_rate_denominator,
            progressive = video_format.progressive_sequence,
            coded_size = %format!("[{}, {}]", self.coded_extent.width, self.coded_extent.height),
            display_area = %format!("[{}, {}, {}, {}]",
                video_format.display_area.l, video_format.display_area.t,
                video_format.display_area.r, video_format.display_area.b),
            chroma = %Self::get_video_chroma_format_string(video_format.chroma_subsampling),
            bit_depth = video_format.bit_depth_luma_minus8 + 8,
            "Video Input Information"
        );

        let num_decode_surfaces = video_format
            .min_num_decode_surfaces
            .saturating_add(self.num_decode_images_in_flight as u32);

        // Save the original config.
        self.video_format = video_format.clone();

        // --- Create video session ---
        let max_dpb_slots = video_format.max_num_dpb_slots.max(4);
        let session_params = crate::codec_utils::vulkan_video_session::VideoSessionCreateParams {
            session_create_flags: vk::VideoSessionCreateFlagsKHR::empty(),
            video_queue_family: self.queue_family_index,
            video_profile: self.video_profile,
            codec_operation: self.video_profile.video_codec_operation,
            picture_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_coded_extent: self.coded_extent,
            reference_pictures_format: vk::Format::G8_B8R8_2PLANE_420_UNORM,
            max_dpb_slots,
            max_active_reference_pictures: max_dpb_slots.saturating_sub(1).min(16),
        };

        let video_session = match unsafe { VulkanVideoSession::create(
            self.ctx.device(),
            self.ctx.instance(),
            self.ctx.physical_device(),
            self.ctx.allocator(),
            &self.submitter,
            &session_params,
        ) } {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to create video session: {:?}", e);
                return -1;
            }
        };

        self.video_session = Some(video_session.video_session());
        self.video_session_arc = Some(video_session);

        // --- Allocate DPB images (array-layered, VUID-07244) ---
        let dpb_count = max_dpb_slots as usize;
        let image_usage = vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR
            | vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
            | vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::SAMPLED;

        let mut profile_list = vk::VideoProfileListInfoKHR::builder()
            .profiles(std::slice::from_ref(&self.video_profile));

        let use_concurrent = self.sharing_queue_families.len() > 1;

        let mut image_create = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(vk::Extent3D {
                width: self.coded_extent.width,
                height: self.coded_extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(dpb_count as u32)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(image_usage)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut profile_list);

        if use_concurrent {
            image_create = image_create
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&self.sharing_queue_families);
        } else {
            image_create = image_create
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
        }

        let alloc_options = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };

        // DPB image allocation runs under the host's device-level resource
        // lock (fixes #278).
        let mut dpb_result: vulkanalia::VkResult<(vk::Image, vma::Allocation)> =
            Err(vk::ErrorCode::INITIALIZATION_FAILED);
        let dpb_result_ref = &mut dpb_result;
        let dpb_allocator = self.ctx.allocator();
        self.submitter.with_device_resource_lock(&mut || {
            *dpb_result_ref = unsafe {
                dpb_allocator.create_image(image_create, &alloc_options)
            };
        });
        match dpb_result {
            Ok((image, allocation)) => {
                self.dpb_image = image;
                self.dpb_allocation = allocation;
            }
            Err(e) => {
                tracing::error!("Failed to create DPB image: {:?}", e);
                return -1;
            }
        }

        // Create per-layer views
        let device = self.ctx.device();
        self.dpb_image_views = Vec::with_capacity(dpb_count);
        self.dpb_slot_layouts = vec![vk::ImageLayout::UNDEFINED; dpb_count];
        self.dpb_slot_active = vec![false; dpb_count];

        let mut ycbcr_info = vk::SamplerYcbcrConversionInfo::builder()
            .conversion(self.ctx.nv12_ycbcr_conversion());

        for i in 0..dpb_count {
            let view_info = vk::ImageViewCreateInfo::builder()
                .image(self.dpb_image)
                .view_type(vk::ImageViewType::_2D)
                .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: i as u32,
                    layer_count: 1,
                })
                .push_next(&mut ycbcr_info);

            match unsafe { device.create_image_view(&view_info, None) } {
                Ok(view) => self.dpb_image_views.push(view),
                Err(e) => {
                    tracing::error!("Failed to create DPB image view {}: {:?}", i, e);
                    return -1;
                }
            }
        }

        // --- Allocate bitstream buffer ---
        let bs_size = self.max_stream_buffer_size;
        let mut bs_profile_list = vk::VideoProfileListInfoKHR::builder()
            .profiles(std::slice::from_ref(&self.video_profile));

        let bs_create = vk::BufferCreateInfo::builder()
            .size(bs_size)
            .usage(vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut bs_profile_list);

        let bs_alloc_options = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };

        // Bitstream buffer allocation runs under the host's device-level
        // resource lock (fixes #278).
        let mut bs_result: vulkanalia::VkResult<(vk::Buffer, vma::Allocation)> =
            Err(vk::ErrorCode::INITIALIZATION_FAILED);
        let bs_result_ref = &mut bs_result;
        let bs_allocator = self.ctx.allocator();
        self.submitter.with_device_resource_lock(&mut || {
            *bs_result_ref = unsafe {
                bs_allocator.create_buffer(bs_create, &bs_alloc_options)
            };
        });
        match bs_result {
            Ok((buffer, allocation)) => {
                let info = self.ctx.allocator().get_allocation_info(allocation);
                self.bitstream_buffer = buffer;
                self.bitstream_allocation = allocation;
                self.bitstream_size = bs_size;
                self.bitstream_mapped_ptr = info.pMappedData as *mut u8;
            }
            Err(e) => {
                tracing::error!("Failed to create bitstream buffer: {:?}", e);
                return -1;
            }
        }

        self.reset_decoder = true;

        tracing::info!(
            num_surfaces = num_decode_surfaces,
            dpb_slots = dpb_count,
            resize = %format!("{} x {}", self.coded_extent.width, self.coded_extent.height),
            "Video session created"
        );

        num_decode_surfaces as i32
    }

    /// Called when the parser has a new picture parameter set (SPS/PPS).
    ///
    /// Mirrors `UpdatePictureParameters`.
    ///
    /// # Placeholder
    /// Requires `StdVideoPictureParametersSet` integration from codec_utils.
    pub fn update_picture_parameters(&mut self) -> bool {
        // TODO: Delegate to VkParserVideoPictureParameters::AddPictureParameters.
        // self.current_picture_parameters = ...;
        // client = self.current_picture_parameters.clone();
        true
    }

    /// Called when a picture is ready to be decoded. Returns the picture index
    /// on success, or -1 on error.
    ///
    /// Mirrors `DecodePictureWithParameters`.
    ///
    /// # Placeholder
    /// The full 200+ line implementation is preserved as structured TODO
    /// comments. The actual Vulkan command recording (begin/end video coding,
    /// barriers, semaphore submission) requires the full device context and
    /// frame-buffer integration.
    pub fn decode_picture_with_parameters(
        &mut self,
        curr_pic_idx: i32,
        decode_info: &VkParserDecodePictureInfo,
    ) -> i32 {
        if self.video_session.is_none() {
            debug_assert!(false, "Decoder not initialized!");
            return -1;
        }

        let _pic_num_in_decode_order = self.decode_pic_count as i32;

        tracing::debug!(
            curr_pic_idx,
            current_video_queue_indx = self.current_video_queue_indx,
            decode_pic_count = self.decode_pic_count,
            "Decode frame"
        );

        // TODO: Full decode pipeline:
        // 1. Get current frame data slot (command buffer).
        // 2. Set up bitstream buffer reference + alignment.
        // 3. Build VkVideoBeginCodingInfoKHR.
        // 4. Build bitstream buffer memory barrier.
        // 5. Get DPB setup picture resource + image barriers.
        // 6. Get separate output picture resource if needed.
        // 7. Get reference picture resources + barriers.
        // 8. Record command buffer:
        //    a. CmdBeginVideoCodingKHR
        //    b. CmdControlVideoCodingKHR (reset if needed)
        //    c. CmdPipelineBarrier2KHR
        //    d. CmdDecodeVideoKHR
        //    e. CmdEndVideoCodingKHR
        //    f. Optional CopyOptimalToLinearImage
        // 9. Submit with semaphore/fence synchronization.
        // 10. Optional compute filter pass.
        // 11. HW load balancing queue rotation.

        // Interlaced sync.
        if decode_info.flags.field_pic {
            // TODO: Wait on frame-complete fence for interlaced content.
        }

        self.decode_pic_count += 1;
        curr_pic_idx
    }

    /// Decode a single picture using the full Vulkan Video pipeline.
    ///
    /// This is the real implementation that records and submits the decode
    /// command buffer. It replaces the stubbed decode_picture_with_parameters
    /// for actual decode operations.
    /// Wait for any in-flight decode to complete. Must be called before
    /// reading the staging buffer from the previous submission.
    pub unsafe fn wait_for_decode(&mut self) -> VideoResult<()> {
        if self.decode_in_flight {
            let device = self.ctx.device();
            device.wait_for_fences(&[self.fence], true, LONG_TIMEOUT)
                .map_err(VideoError::from)?;
            self.decode_in_flight = false;
        }
        Ok(())
    }

    #[allow(unused_assignments, unused_mut)]
    pub unsafe fn decode_frame(
        &mut self,
        submit: &DecodeSubmitInfo,
        output: &mut DecodedFrame,
    ) -> VideoResult<()> {
        // Wait for any previous in-flight decode before reusing the command buffer
        self.wait_for_decode()?;

        let device = self.ctx.device();
        let session = self.video_session
            .ok_or(VideoError::BitstreamError("Not initialized".into()))?;

        // --- Upload bitstream ---
        let aligned_size = (submit.bitstream.len() as u64 + 0xFF) & !0xFF;
        if aligned_size > self.bitstream_size {
            // Reallocate bitstream buffer if needed
            self.ctx.allocator().destroy_buffer(self.bitstream_buffer, self.bitstream_allocation);
            let mut profile_list = vk::VideoProfileListInfoKHR::builder()
                .profiles(std::slice::from_ref(&self.video_profile));
            let bs_create = vk::BufferCreateInfo::builder()
                .size(aligned_size)
                .usage(vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut profile_list);
            let bs_alloc = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
                ..Default::default()
            };
            // Bitstream resize runs under the host's device-level resource
            // lock (fixes #278).
            let mut resize_result: vulkanalia::VkResult<(vk::Buffer, vma::Allocation)> =
                Err(vk::ErrorCode::INITIALIZATION_FAILED);
            let resize_result_ref = &mut resize_result;
            let resize_allocator = self.ctx.allocator();
            self.submitter.with_device_resource_lock(&mut || {
                *resize_result_ref = unsafe {
                    resize_allocator.create_buffer(bs_create, &bs_alloc)
                };
            });
            let (buf, alloc) = resize_result.map_err(VideoError::from)?;
            let info = self.ctx.allocator().get_allocation_info(alloc);
            self.bitstream_buffer = buf;
            self.bitstream_allocation = alloc;
            self.bitstream_size = aligned_size;
            self.bitstream_mapped_ptr = info.pMappedData as *mut u8;
        }
        ptr::copy_nonoverlapping(
            submit.bitstream.as_ptr(),
            self.bitstream_mapped_ptr,
            submit.bitstream.len(),
        );
        // Zero-pad alignment
        let padding = aligned_size as usize - submit.bitstream.len();
        if padding > 0 {
            ptr::write_bytes(
                self.bitstream_mapped_ptr.add(submit.bitstream.len()),
                0,
                padding,
            );
        }

        // --- Begin command buffer ---
        device.reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
            .map_err(VideoError::from)?;
        device.begin_command_buffer(
            self.command_buffer,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        ).map_err(VideoError::from)?;

        // --- Build setup resource ---
        let setup_resource = vk::VideoPictureResourceInfoKHR::builder()
            .coded_offset(vk::Offset2D { x: 0, y: 0 })
            .coded_extent(self.coded_extent)
            .base_array_layer(0)
            .image_view_binding(submit.setup_image_view)
            .build();

        // --- Build reference resources ---
        let nrefs = submit.reference_slots.len();
        let mut ref_resources = Vec::with_capacity(nrefs);
        let mut vk_ref_slots = Vec::with_capacity(nrefs);

        for slot in submit.reference_slots {
            ref_resources.push(
                vk::VideoPictureResourceInfoKHR::builder()
                    .coded_offset(vk::Offset2D { x: 0, y: 0 })
                    .coded_extent(self.coded_extent)
                    .base_array_layer(0)
                    .image_view_binding(slot.image_view)
                    .build(),
            );
        }
        for (i, slot) in submit.reference_slots.iter().enumerate() {
            vk_ref_slots.push(
                vk::VideoReferenceSlotInfoKHR::builder()
                    .slot_index(slot.slot_index)
                    .picture_resource(&ref_resources[i])
                    .build(),
            );
        }

        // --- Build H.264-specific structures ---
        let mut h264_std_pic_info: vk::video::StdVideoDecodeH264PictureInfo =
            std::mem::zeroed();
        let mut h264_pic_info = vk::VideoDecodeH264PictureInfoKHR::default();
        let mut h264_ref_infos = Vec::with_capacity(nrefs);
        let mut h264_ref_dpb_slot_infos = Vec::with_capacity(nrefs);
        let mut h264_setup_ref_info: vk::video::StdVideoDecodeH264ReferenceInfo =
            std::mem::zeroed();
        let mut h264_setup_dpb_slot = vk::VideoDecodeH264DpbSlotInfoKHR::default();
        let h264_slice_offsets_storage: Vec<u32>;

        let is_h264 = submit.h264_info.is_some();

        if let Some(ref h264) = submit.h264_info {
            let mut flags: vk::video::StdVideoDecodeH264PictureInfoFlags =
                std::mem::zeroed();
            if h264.is_idr { flags.set_IdrPicFlag(1); }
            if h264.is_intra { flags.set_is_intra(1); }
            if h264.is_reference { flags.set_is_reference(1); }
            if h264.field_pic_flag { flags.set_field_pic_flag(1); }
            if h264.bottom_field_flag { flags.set_bottom_field_flag(1); }

            h264_std_pic_info = vk::video::StdVideoDecodeH264PictureInfo {
                flags,
                seq_parameter_set_id: h264.sps_id,
                pic_parameter_set_id: h264.pps_id,
                reserved1: 0,
                reserved2: 0,
                frame_num: h264.frame_num,
                idr_pic_id: h264.idr_pic_id,
                PicOrderCnt: h264.pic_order_cnt,
            };

            h264_slice_offsets_storage = if h264.slice_offsets.is_empty() {
                vec![0]
            } else {
                h264.slice_offsets.clone()
            };

            h264_pic_info = vk::VideoDecodeH264PictureInfoKHR::builder()
                .std_picture_info(&h264_std_pic_info)
                .slice_offsets(&h264_slice_offsets_storage)
                .build();

            for ref_info in &h264.ref_pic_infos {
                let mut rf: vk::video::StdVideoDecodeH264ReferenceInfoFlags =
                    std::mem::zeroed();
                if ref_info.long_term_ref { rf.set_used_for_long_term_reference(1); }
                if ref_info.non_existing { rf.set_is_non_existing(1); }
                h264_ref_infos.push(vk::video::StdVideoDecodeH264ReferenceInfo {
                    flags: rf,
                    FrameNum: ref_info.frame_num,
                    reserved: 0,
                    PicOrderCnt: ref_info.pic_order_cnt,
                });
            }
            for info in &h264_ref_infos {
                h264_ref_dpb_slot_infos.push(
                    vk::VideoDecodeH264DpbSlotInfoKHR::builder()
                        .std_reference_info(info)
                        .build(),
                );
            }

            h264_setup_ref_info = vk::video::StdVideoDecodeH264ReferenceInfo {
                flags: std::mem::zeroed(),
                FrameNum: h264.setup_ref_info.frame_num,
                reserved: 0,
                PicOrderCnt: h264.setup_ref_info.pic_order_cnt,
            };
            h264_setup_dpb_slot = vk::VideoDecodeH264DpbSlotInfoKHR::builder()
                .std_reference_info(&h264_setup_ref_info)
                .build();
        } else {
            h264_slice_offsets_storage = vec![0];
        }

        // --- Build H.265-specific structures ---
        let mut h265_std_pic_info: vk::video::StdVideoDecodeH265PictureInfo =
            std::mem::zeroed();
        let mut h265_pic_info = vk::VideoDecodeH265PictureInfoKHR::default();
        let mut h265_ref_infos = Vec::with_capacity(nrefs);
        let mut h265_ref_dpb_slot_infos = Vec::with_capacity(nrefs);
        let mut h265_setup_ref_info: vk::video::StdVideoDecodeH265ReferenceInfo =
            std::mem::zeroed();
        let mut h265_setup_dpb_slot = vk::VideoDecodeH265DpbSlotInfoKHR::default();
        let slice_segment_offsets_storage: Vec<u32>;

        let is_h265 = submit.h265_info.is_some();

        if let Some(ref h265) = submit.h265_info {
            let mut flags: vk::video::StdVideoDecodeH265PictureInfoFlags = std::mem::zeroed();
            if h265.is_irap { flags.set_IrapPicFlag(1); }
            if h265.is_idr { flags.set_IdrPicFlag(1); }
            if h265.is_reference { flags.set_IsReference(1); }
            if h265.short_term_ref_pic_set_sps_flag {
                flags.set_short_term_ref_pic_set_sps_flag(1);
            }

            let mut ref_pic_set_st_curr_before = [0xFF_u8; 8];
            let mut ref_pic_set_st_curr_after = [0xFF_u8; 8];
            let ref_pic_set_lt_curr = [0xFF_u8; 8];

            if !h265.is_idr && !h265.ref_pic_infos.is_empty() {
                let nb = h265.num_st_curr_before as usize;
                let na = h265.num_st_curr_after as usize;
                // RefPicSetStCurrBefore/After values are DPB slot indices
                // (matching slotIndex), NOT positions into pReferenceSlots.
                // Both ffmpeg (vulkan_hevc.c) and NVIDIA reference
                // (VulkanVideoParser.cpp frmListToDpb) use slot indices.
                for i in 0..nb.min(8) {
                    ref_pic_set_st_curr_before[i] =
                        submit.reference_slots[i].slot_index as u8;
                }
                for i in 0..na.min(8) {
                    ref_pic_set_st_curr_after[i] =
                        submit.reference_slots[nb + i].slot_index as u8;
                }
            }

            h265_std_pic_info = vk::video::StdVideoDecodeH265PictureInfo {
                flags,
                sps_video_parameter_set_id: h265.vps_id,
                pps_seq_parameter_set_id: h265.sps_id,
                pps_pic_parameter_set_id: h265.pps_id,
                NumDeltaPocsOfRefRpsIdx: h265.num_delta_pocs_of_ref_rps_idx,
                PicOrderCntVal: h265.pic_order_cnt_val,
                NumBitsForSTRefPicSetInSlice: h265.num_bits_for_st_ref_pic_set_in_slice,
                reserved: 0,
                RefPicSetStCurrBefore: ref_pic_set_st_curr_before,
                RefPicSetStCurrAfter: ref_pic_set_st_curr_after,
                RefPicSetLtCurr: ref_pic_set_lt_curr,
            };

            slice_segment_offsets_storage = if h265.slice_segment_offsets.is_empty() {
                vec![0]
            } else {
                h265.slice_segment_offsets.clone()
            };

            h265_pic_info = vk::VideoDecodeH265PictureInfoKHR::builder()
                .std_picture_info(&h265_std_pic_info)
                .slice_segment_offsets(&slice_segment_offsets_storage)
                .build();

            for ref_info in &h265.ref_pic_infos {
                let mut rf: vk::video::StdVideoDecodeH265ReferenceInfoFlags = std::mem::zeroed();
                if ref_info.long_term_ref { rf.set_used_for_long_term_reference(1); }
                h265_ref_infos.push(vk::video::StdVideoDecodeH265ReferenceInfo {
                    flags: rf,
                    PicOrderCntVal: ref_info.pic_order_cnt_val,
                });
            }
            for info in &h265_ref_infos {
                h265_ref_dpb_slot_infos.push(
                    vk::VideoDecodeH265DpbSlotInfoKHR::builder()
                        .std_reference_info(info)
                        .build(),
                );
            }

            h265_setup_ref_info = vk::video::StdVideoDecodeH265ReferenceInfo {
                flags: std::mem::zeroed(),
                PicOrderCntVal: h265.setup_ref_info.pic_order_cnt_val,
            };
            h265_setup_dpb_slot = vk::VideoDecodeH265DpbSlotInfoKHR::builder()
                .std_reference_info(&h265_setup_ref_info)
                .build();
        } else {
            slice_segment_offsets_storage = vec![0];
        }

        // Attach codec pNext to reference slots
        if is_h264 {
            for (i, dpb_slot_info) in h264_ref_dpb_slot_infos.iter_mut().enumerate() {
                if i < vk_ref_slots.len() {
                    vk_ref_slots[i].next = dpb_slot_info
                        as *mut vk::VideoDecodeH264DpbSlotInfoKHR
                        as *const std::ffi::c_void;
                }
            }
        } else if is_h265 {
            for (i, dpb_slot_info) in h265_ref_dpb_slot_infos.iter_mut().enumerate() {
                if i < vk_ref_slots.len() {
                    vk_ref_slots[i].next = dpb_slot_info
                        as *mut vk::VideoDecodeH265DpbSlotInfoKHR
                        as *const std::ffi::c_void;
                }
            }
        }

        // Build setup slot with codec pNext
        let mut setup_slot = vk::VideoReferenceSlotInfoKHR::builder()
            .slot_index(submit.setup_slot_index)
            .picture_resource(&setup_resource);
        if is_h264 {
            setup_slot = setup_slot.push_next(&mut h264_setup_dpb_slot);
        } else if is_h265 {
            setup_slot = setup_slot.push_next(&mut h265_setup_dpb_slot);
        }
        let setup_slot = setup_slot.build();

        // --- Build begin_coding with ALL active DPB slots + setup ---
        // The Vulkan spec requires all active DPB slots to be listed in
        // begin_coding; unlisted slots are DEACTIVATED when the scope ends.
        // For H.264 sliding window DPB, this means ALL active reference
        // slots must be included, not just the ones used by the current
        // picture's reference list.
        let mut begin_active_resources = Vec::new();
        let mut begin_slots = Vec::new();

        if !self.reset_decoder {
            // Include all active DPB slots from active_slots list
            for active in submit.active_slots {
                begin_active_resources.push(
                    vk::VideoPictureResourceInfoKHR::builder()
                        .coded_offset(vk::Offset2D { x: 0, y: 0 })
                        .coded_extent(self.coded_extent)
                        .base_array_layer(0)
                        .image_view_binding(active.image_view)
                        .build(),
                );
            }
            for (i, active) in submit.active_slots.iter().enumerate() {
                begin_slots.push(
                    vk::VideoReferenceSlotInfoKHR::builder()
                        .slot_index(active.slot_index)
                        .picture_resource(&begin_active_resources[i])
                        .build(),
                );
            }
        }
        // Add setup slot with -1 (activates after decode)
        begin_slots.push(
            vk::VideoReferenceSlotInfoKHR::builder()
                .slot_index(-1)
                .picture_resource(&setup_resource)
                .build(),
        );

        let begin_coding = vk::VideoBeginCodingInfoKHR::builder()
            .video_session(session)
            .video_session_parameters(submit.session_parameters)
            .reference_slots(&begin_slots);

        device.cmd_begin_video_coding_khr(self.command_buffer, &begin_coding);

        // Session reset on first decode
        if self.reset_decoder {
            let control = vk::VideoCodingControlInfoKHR::builder()
                .flags(vk::VideoCodingControlFlagsKHR::RESET);
            device.cmd_control_video_coding_khr(self.command_buffer, &control);
            self.reset_decoder = false;
        }

        // --- Pipeline barriers ---
        let bitstream_barrier = vk::BufferMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::HOST)
            .src_access_mask(vk::AccessFlags2::HOST_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::VIDEO_DECODE_KHR)
            .dst_access_mask(vk::AccessFlags2::VIDEO_DECODE_READ_KHR)
            .buffer(self.bitstream_buffer)
            .offset(0)
            .size(aligned_size);

        let setup_idx = submit.setup_slot_index as usize;
        let setup_old_layout = if setup_idx < self.dpb_slot_layouts.len() {
            self.dpb_slot_layouts[setup_idx]
        } else {
            vk::ImageLayout::UNDEFINED
        };

        let dpb_setup_barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::VIDEO_DECODE_KHR)
            .dst_access_mask(vk::AccessFlags2::VIDEO_DECODE_WRITE_KHR)
            .old_layout(setup_old_layout)
            .new_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
            .image(self.dpb_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: setup_idx as u32,
                layer_count: 1,
            });

        let mut image_barriers = vec![dpb_setup_barrier];
        for slot in submit.reference_slots {
            let ref_idx = slot.slot_index as usize;
            let ref_old_layout = if ref_idx < self.dpb_slot_layouts.len() {
                self.dpb_slot_layouts[ref_idx]
            } else {
                vk::ImageLayout::UNDEFINED
            };
            image_barriers.push(
                vk::ImageMemoryBarrier2::builder()
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .src_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::VIDEO_DECODE_KHR)
                    .dst_access_mask(vk::AccessFlags2::VIDEO_DECODE_READ_KHR)
                    .old_layout(ref_old_layout)
                    .new_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
                    .image(self.dpb_image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: ref_idx as u32,
                        layer_count: 1,
                    })
            );
        }

        let dep_info = vk::DependencyInfo::builder()
            .buffer_memory_barriers(std::slice::from_ref(&bitstream_barrier))
            .image_memory_barriers(&image_barriers);
        device.cmd_pipeline_barrier2(self.command_buffer, &dep_info);

        // --- Decode ---
        let mut decode_info = vk::VideoDecodeInfoKHR::builder()
            .src_buffer(self.bitstream_buffer)
            .src_buffer_offset(submit.bitstream_offset)
            .src_buffer_range(aligned_size)
            .dst_picture_resource(setup_resource)
            .setup_reference_slot(&setup_slot)
            .reference_slots(&vk_ref_slots);

        if is_h264 {
            decode_info = decode_info.push_next(&mut h264_pic_info);
        } else if is_h265 {
            decode_info = decode_info.push_next(&mut h265_pic_info);
        }

        device.cmd_decode_video_khr(self.command_buffer, &decode_info);

        // --- End video coding ---
        device.cmd_end_video_coding_khr(
            self.command_buffer,
            &vk::VideoEndCodingInfoKHR::default(),
        );

        // --- Inline readback (DPB → staging) ---
        if let Some(ref staging) = output.staging_buffer {
            let rb_barrier = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::VIDEO_DECODE_KHR)
                .src_access_mask(vk::AccessFlags2::VIDEO_DECODE_WRITE_KHR)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .old_layout(vk::ImageLayout::VIDEO_DECODE_DPB_KHR)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(self.dpb_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: setup_idx as u32,
                    layer_count: 1,
                });
            let rb_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(std::slice::from_ref(&rb_barrier));
            device.cmd_pipeline_barrier2(self.command_buffer, &rb_dep);

            let w = self.coded_extent.width;
            let h = self.coded_extent.height;
            let y_region = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_0,
                    mip_level: 0,
                    base_array_layer: setup_idx as u32,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                image_extent: vk::Extent3D { width: w, height: h, depth: 1 },
            };
            let uv_region = vk::BufferImageCopy {
                buffer_offset: (w * h) as u64,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_1,
                    mip_level: 0,
                    base_array_layer: setup_idx as u32,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                image_extent: vk::Extent3D { width: w / 2, height: h / 2, depth: 1 },
            };
            device.cmd_copy_image_to_buffer(
                self.command_buffer,
                self.dpb_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                staging.buffer,
                &[y_region, uv_region],
            );
        }

        // --- Submit ---
        device.end_command_buffer(self.command_buffer).map_err(VideoError::from)?;
        device.reset_fences(&[self.fence]).map_err(VideoError::from)?;
        let cbs = [self.command_buffer];
        let submit_info = vk::SubmitInfo::builder().command_buffers(&cbs).build();
        self.submitter.submit_to_queue_legacy(self.queue, &[submit_info], self.fence)
            .map_err(VideoError::from)?;
        self.decode_in_flight = true;

        // --- Update layout tracking ---
        if setup_idx < self.dpb_slot_layouts.len() {
            self.dpb_slot_layouts[setup_idx] = if output.staging_buffer.is_some() {
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL
            } else {
                vk::ImageLayout::VIDEO_DECODE_DPB_KHR
            };
            self.dpb_slot_active[setup_idx] = true;
        }
        for slot in submit.reference_slots {
            let ref_idx = slot.slot_index as usize;
            if ref_idx < self.dpb_slot_layouts.len() {
                self.dpb_slot_layouts[ref_idx] = vk::ImageLayout::VIDEO_DECODE_DPB_KHR;
            }
        }

        // --- Fill output ---
        output.image = self.dpb_image;
        if setup_idx < self.dpb_image_views.len() {
            output.image_view = self.dpb_image_views[setup_idx];
        }
        output.format = vk::Format::G8_B8R8_2PLANE_420_UNORM;
        output.extent = self.coded_extent;
        output.dpb_slot = submit.setup_slot_index;
        output.decode_order = self.decode_pic_count;

        self.decode_pic_count += 1;
        Ok(())
    }

    /// Get the session parameters handle.
    pub fn session_parameters(&self) -> vk::VideoSessionParametersKHR {
        self.session_parameters
    }

    /// Set the session parameters handle.
    pub fn set_session_parameters(&mut self, params: vk::VideoSessionParametersKHR) {
        self.session_parameters = params;
    }

    /// Get image view for a DPB slot.
    pub fn dpb_slot_image_view(&self, index: usize) -> Option<vk::ImageView> {
        self.dpb_image_views.get(index).copied()
    }

    /// Get the DPB image.
    pub fn dpb_image(&self) -> vk::Image {
        self.dpb_image
    }

    /// Get the DPB slot count.
    pub fn dpb_slot_count(&self) -> usize {
        self.dpb_image_views.len()
    }

    /// Override the tracked layout for a specific DPB slot.
    ///
    /// Used after external operations (e.g. NV12→RGB compute conversion)
    /// that transition a DPB layer to a different layout. The decoder's
    /// next barrier will use this as the old layout.
    pub fn set_dpb_slot_layout(&mut self, slot: usize, layout: vk::ImageLayout) {
        if slot < self.dpb_slot_layouts.len() {
            self.dpb_slot_layouts[slot] = layout;
        }
    }

    /// Get the video session handle.
    pub fn video_session_handle(&self) -> Option<vk::VideoSessionKHR> {
        self.video_session
    }

    /// Get the video session Arc.
    pub fn video_session_arc(&self) -> Option<&Arc<VulkanVideoSession>> {
        self.video_session_arc.as_ref()
    }

    /// Copy from an optimal-tiled image to a linear image (luma + chroma planes).
    ///
    /// Mirrors `CopyOptimalToLinearImage`.
    ///
    /// # Placeholder
    /// Requires `VkCmdCopyImage` via the device context.
    pub fn copy_optimal_to_linear_image(&self) -> i32 {
        // TODO: Record vkCmdCopyImage for luma (plane 0) and chroma (plane 1),
        //       then a memory barrier HOST_READ.
        0
    }

    /// Obtain (or create) a bitstream buffer of at least `size` bytes.
    /// Returns the actual buffer capacity.
    ///
    /// Mirrors `GetBitstreamBuffer`.
    pub fn get_bitstream_buffer(&mut self, size: u64) -> u64 {
        let new_size = size;

        // TODO: Try to get a buffer from the pool; if unavailable, allocate a new one.
        // TODO: Copy initial data if provided.

        if new_size > self.max_stream_buffer_size {
            tracing::debug!(
                new_size,
                old_max = self.max_stream_buffer_size,
                "Re-allocated bitstream buffer"
            );
            self.max_stream_buffer_size = new_size;
        }

        new_size
    }

    /// Get the current frame data slot for a given slot ID.
    ///
    /// Mirrors `GetCurrentFrameData`.
    pub fn get_current_frame_data(&self, slot_id: u32) -> Option<NvVkDecodeFrameDataSlot> {
        if (slot_id as usize) < self.decode_frames_data.size() {
            Some(NvVkDecodeFrameDataSlot {
                slot: slot_id,
                command_buffer: self.decode_frames_data.get_command_buffer(slot_id),
            })
        } else {
            None
        }
    }

    // ------------------------------------------------------------------
    // Teardown
    // ------------------------------------------------------------------

    /// Release all Vulkan resources.
    ///
    /// Mirrors `Deinitialize`.
    pub fn deinitialize(&mut self) {
        let device = self.ctx.device();

        // Wait for all operations to complete before cleanup.
        unsafe { let _ = device.device_wait_idle(); }

        // Destroy DPB image views.
        for &view in &self.dpb_image_views {
            if view != vk::ImageView::null() {
                unsafe { device.destroy_image_view(view, None); }
            }
        }
        self.dpb_image_views.clear();

        // Destroy DPB image + free allocation.
        if self.dpb_image != vk::Image::null() {
            let allocator = self.ctx.allocator();
            unsafe {
                allocator.destroy_image(self.dpb_image, self.dpb_allocation);
            }
            self.dpb_image = vk::Image::null();
        }

        // Destroy bitstream buffer + free allocation.
        if self.bitstream_buffer != vk::Buffer::null() {
            let allocator = self.ctx.allocator();
            unsafe {
                allocator.destroy_buffer(self.bitstream_buffer, self.bitstream_allocation);
            }
            self.bitstream_buffer = vk::Buffer::null();
            self.bitstream_mapped_ptr = ptr::null_mut();
            self.bitstream_size = 0;
        }

        // Destroy command pool (implicitly frees command buffers).
        if self.command_pool != vk::CommandPool::null() {
            unsafe { device.destroy_command_pool(self.command_pool, None); }
            self.command_pool = vk::CommandPool::null();
            self.command_buffer = vk::CommandBuffer::null();
        }

        // Destroy fence.
        if self.fence != vk::Fence::null() {
            unsafe { device.destroy_fence(self.fence, None); }
            self.fence = vk::Fence::null();
        }

        self.decode_frames_data.deinit();
        self.current_picture_parameters = None;
        self.video_session = None;
    }
}

impl Drop for VkVideoDecoder {
    fn drop(&mut self) {
        self.deinitialize();
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpu_align() {
        assert_eq!(gpu_align(0), 0);
        assert_eq!(gpu_align(1), 256);
        assert_eq!(gpu_align(256), 256);
        assert_eq!(gpu_align(257), 512);
        assert_eq!(gpu_align(511), 512);
        assert_eq!(gpu_align(512), 512);
    }

    #[test]
    #[ignore = "requires GPU — VkVideoDecoder::new() needs a real VideoContext"]
    fn test_create_decoder() {
        // VkVideoDecoder::new() requires Arc<VideoContext> with a live Vulkan
        // device. This test is kept as a placeholder for GPU integration tests.
    }

    #[test]
    fn test_get_video_codec_string() {
        assert_eq!(
            VkVideoDecoder::get_video_codec_string(vk::VideoCodecOperationFlagsKHR::NONE),
            "None"
        );
        assert_eq!(
            VkVideoDecoder::get_video_codec_string(vk::VideoCodecOperationFlagsKHR::DECODE_H264),
            "AVC/H.264"
        );
        assert_eq!(
            VkVideoDecoder::get_video_codec_string(vk::VideoCodecOperationFlagsKHR::DECODE_H265),
            "H.265/HEVC"
        );
        assert_eq!(
            VkVideoDecoder::get_video_codec_string(vk::VideoCodecOperationFlagsKHR::DECODE_AV1),
            "AV1"
        );
    }

    #[test]
    fn test_get_video_chroma_format_string() {
        assert_eq!(
            VkVideoDecoder::get_video_chroma_format_string(
                vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME
            ),
            "YCbCr 400 (Monochrome)"
        );
        assert_eq!(
            VkVideoDecoder::get_video_chroma_format_string(
                vk::VideoChromaSubsamplingFlagsKHR::_420
            ),
            "YCbCr 420"
        );
    }

    #[test]
    fn test_image_specs_index_default() {
        let idx = ImageSpecsIndex::default();
        assert_eq!(idx.decode_dpb, INVALID_IMAGE_TYPE_IDX);
        assert_eq!(idx.decode_out, INVALID_IMAGE_TYPE_IDX);
        assert_eq!(idx.filter_out, INVALID_IMAGE_TYPE_IDX);
    }

    #[test]
    fn test_decoder_features_flags() {
        let features = DecoderFeatures::ENABLE_LINEAR_OUTPUT
            | DecoderFeatures::ENABLE_HW_LOAD_BALANCING;
        assert!(features.contains(DecoderFeatures::ENABLE_LINEAR_OUTPUT));
        assert!(features.contains(DecoderFeatures::ENABLE_HW_LOAD_BALANCING));
        assert!(!features.contains(DecoderFeatures::ENABLE_POST_PROCESS_FILTER));
    }

    // test_set_export_preferences removed — set_export_preferences method was removed.

    #[test]
    #[ignore = "requires GPU — VkVideoDecoder::new() needs a real VideoContext"]
    fn test_start_video_sequence() {
        // Requires Arc<VideoContext> with a live Vulkan device.
    }

    #[test]
    #[ignore = "requires GPU — VkVideoDecoder::new() needs a real VideoContext"]
    fn test_get_current_frame_data() {
        // Requires Arc<VideoContext> with a live Vulkan device.
    }

    #[test]
    #[ignore = "requires GPU — VkVideoDecoder::new() needs a real VideoContext"]
    fn test_get_bitstream_buffer_updates_max() {
        // Requires Arc<VideoContext> with a live Vulkan device.
    }

    #[test]
    #[ignore = "requires GPU — VkVideoDecoder::new() needs a real VideoContext"]
    fn test_deinitialize_is_safe() {
        // Requires Arc<VideoContext> with a live Vulkan device.
    }

    #[test]
    fn test_nv_vk_decode_frame_data_resize() {
        let mut data = NvVkDecodeFrameData::new(std::ptr::null());
        let allocated = data.resize(16);
        assert_eq!(allocated, 16);
        assert_eq!(data.size(), 16);
    }

    #[test]
    fn test_rect_default() {
        let r = Rect::default();
        assert_eq!(r.l, 0);
        assert_eq!(r.t, 0);
        assert_eq!(r.r, 0);
        assert_eq!(r.b, 0);
    }
}
