// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared types for the decode module.

use vulkanalia::vk;
use vulkanalia::vk::Handle;
use vulkanalia_vma as vma;

// ---------------------------------------------------------------------------
// DPB output mode
// ---------------------------------------------------------------------------

/// Whether the DPB images also serve as the decode output (coincide mode)
/// or whether output goes to a separate image (distinct mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpbOutputMode {
    /// DPB image is also the decode output (`VK_VIDEO_DECODE_CAPABILITY_DPB_AND_OUTPUT_COINCIDE_BIT_KHR`).
    Coincide,
    /// Decode output is a separate image (`VK_VIDEO_DECODE_CAPABILITY_DPB_AND_OUTPUT_DISTINCT_BIT_KHR`).
    Distinct,
}

impl Default for DpbOutputMode {
    fn default() -> Self {
        DpbOutputMode::Coincide
    }
}

// ---------------------------------------------------------------------------
// DecodedFrame — output of a single decode operation
// ---------------------------------------------------------------------------

/// A decoded video frame, holding the Vulkan image and associated metadata.
///
/// When the library allocates the DPB, this borrows from the internal pool.
/// When the caller provides output images, the library fills in the metadata.
#[derive(Debug)]
/// Optional staging buffer for inline readback within the decode command buffer.
pub struct StagingBuffer {
    pub buffer: vk::Buffer,
    pub allocation: vma::Allocation,
    pub size: u64,
    pub mapped_ptr: *mut u8,
}

pub struct DecodedFrame {
    /// The image containing decoded picture data.
    pub image: vk::Image,
    /// Image view for sampling or display.
    pub image_view: vk::ImageView,
    /// Format of the decoded image (e.g., `G8_B8R8_2PLANE_420_UNORM` for NV12).
    pub format: vk::Format,
    /// Coded extent of the decoded picture.
    pub extent: vk::Extent2D,
    /// Picture Order Count (display ordering).
    pub picture_order_count: i32,
    /// Decode order index (submission order).
    pub decode_order: u64,
    /// DPB slot index this frame occupies (-1 if not in DPB).
    pub dpb_slot: i32,
    /// Optional staging buffer for inline readback (decode + copy in same submit).
    pub staging_buffer: Option<StagingBuffer>,
}

impl Default for DecodedFrame {
    fn default() -> Self {
        Self {
            image: vk::Image::null(),
            image_view: vk::ImageView::null(),
            format: vk::Format::UNDEFINED,
            extent: vk::Extent2D { width: 0, height: 0 },
            picture_order_count: 0,
            decode_order: 0,
            dpb_slot: -1,
            staging_buffer: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ReferenceSlot — used to build vkCmdDecodeVideoKHR reference lists
// ---------------------------------------------------------------------------

/// A reference slot description for a single decode command.
#[derive(Debug, Clone)]
pub struct ReferenceSlot {
    /// DPB slot index (0-based).
    pub slot_index: i32,
    /// Image view of the reference picture.
    pub image_view: vk::ImageView,
    /// Image layout of the reference picture.
    pub image_layout: vk::ImageLayout,
}

// ---------------------------------------------------------------------------
// DecodeSubmitInfo — per-frame submission parameters
// ---------------------------------------------------------------------------

/// H.264-specific decode picture parameters.
///
/// Provides the codec-specific info required by `VkVideoDecodeH264PictureInfoKHR`
/// and the per-slot `VkVideoDecodeH264DpbSlotInfoKHR`. Without these, the
/// driver has no picture parameters and produces zeroed/green output.
#[derive(Debug, Clone)]
pub struct H264DecodeInfo {
    /// frame_num from the slice header.
    pub frame_num: u16,
    /// idr_pic_id from the slice header (only meaningful for IDR pictures).
    pub idr_pic_id: u16,
    /// Picture Order Count: `[TopFieldOrderCnt, BottomFieldOrderCnt]`.
    pub pic_order_cnt: [i32; 2],
    /// SPS ID referenced by this picture.
    pub sps_id: u8,
    /// PPS ID referenced by this picture.
    pub pps_id: u8,
    /// True if this is an IDR picture.
    pub is_idr: bool,
    /// True if this is an intra-only picture.
    pub is_intra: bool,
    /// True if this picture is used as a reference.
    pub is_reference: bool,
    /// True if this is a field picture (rather than frame).
    pub field_pic_flag: bool,
    /// True if this is the bottom field (only meaningful when field_pic_flag is set).
    pub bottom_field_flag: bool,
    /// Byte offsets of each slice within the bitstream data.
    /// For a single-slice picture this is `[0]`.
    pub slice_offsets: Vec<u32>,
    /// Per-reference-slot H.264 info: `(FrameNum, PicOrderCnt[2])`.
    /// Must be in the same order as `DecodeSubmitInfo::reference_slots`.
    pub ref_pic_infos: Vec<H264RefPicInfo>,
    /// H.264 reference info for the setup (destination) slot.
    pub setup_ref_info: H264RefPicInfo,
}

/// Per-reference-picture H.264 info for DPB slots.
#[derive(Debug, Clone, Copy, Default)]
pub struct H264RefPicInfo {
    /// frame_num of the reference picture.
    pub frame_num: u16,
    /// Picture Order Count: `[TopFieldOrderCnt, BottomFieldOrderCnt]`.
    pub pic_order_cnt: [i32; 2],
    /// True if this reference is a long-term reference.
    pub long_term_ref: bool,
    /// True if this slot is non-existing (placeholder).
    pub non_existing: bool,
}

/// H.265-specific decode picture parameters.
///
/// Provides the codec-specific info required by `VkVideoDecodeH265PictureInfoKHR`
/// and the per-slot `VkVideoDecodeH265DpbSlotInfoKHR`.
#[derive(Debug, Clone)]
pub struct H265DecodeInfo {
    /// Picture Order Count value.
    pub pic_order_cnt_val: i32,
    /// VPS ID referenced by this picture.
    pub vps_id: u8,
    /// SPS ID referenced by this picture.
    pub sps_id: u8,
    /// PPS ID referenced by this picture.
    pub pps_id: u8,
    /// True if this is an IRAP (IDR/BLA/CRA) picture.
    pub is_irap: bool,
    /// True if this is an IDR picture.
    pub is_idr: bool,
    /// True if this picture is used as a reference.
    pub is_reference: bool,
    /// Byte offsets of each slice segment within the bitstream data.
    pub slice_segment_offsets: Vec<u32>,
    /// Per-reference-slot H.265 info.
    /// Must be in the same order as `DecodeSubmitInfo::reference_slots`.
    pub ref_pic_infos: Vec<H265RefPicInfo>,
    /// H.265 reference info for the setup (destination) slot.
    pub setup_ref_info: H265RefPicInfo,
    /// Number of delta POCs of the referenced short-term RPS (0 for simple IPP).
    pub num_delta_pocs_of_ref_rps_idx: u8,
    /// Number of bits used for the short-term RPS in the slice header (0 for simple IPP).
    pub num_bits_for_st_ref_pic_set_in_slice: u16,
    /// Number of StCurrBefore references in `ref_pic_infos` (first N entries).
    pub num_st_curr_before: u8,
    /// Number of StCurrAfter references in `ref_pic_infos` (next N entries).
    pub num_st_curr_after: u8,
    /// The short_term_ref_pic_set_sps_flag from the slice header.
    /// Tells the driver if the STRPS is from the SPS (true) or inline (false).
    pub short_term_ref_pic_set_sps_flag: bool,
}

/// Per-reference-picture H.265 info for DPB slots.
#[derive(Debug, Clone, Copy, Default)]
pub struct H265RefPicInfo {
    /// Picture Order Count value.
    pub pic_order_cnt_val: i32,
    /// True if this reference is a long-term reference.
    pub long_term_ref: bool,
}

/// Parameters for a single [`Decoder::decode_frame`] call.
///
/// The caller fills in the bitstream data and reference slot info obtained
/// from the parser layer. This mirrors the information that the C++ code
/// assembles inside `VkVideoDecoder::DecodePictureWithParameters`.
pub struct DecodeSubmitInfo<'a> {
    /// Raw compressed bitstream for this picture.
    pub bitstream: &'a [u8],
    /// Offset into the bitstream buffer where data starts.
    pub bitstream_offset: vk::DeviceSize,
    /// The DPB slot to set up (write the decoded picture into).
    pub setup_slot_index: i32,
    /// Image view for the setup (destination) slot.
    pub setup_image_view: vk::ImageView,
    /// Reference slots used by this decode operation.
    pub reference_slots: &'a [ReferenceSlot],
    /// ALL currently active DPB slots (must be included in begin_coding
    /// to prevent the Vulkan driver from deactivating them).
    pub active_slots: &'a [ReferenceSlot],
    /// Session parameters handle (SPS/PPS).
    pub session_parameters: vk::VideoSessionParametersKHR,
    /// H.264-specific decode picture info (required for H.264 decode).
    pub h264_info: Option<H264DecodeInfo>,
    /// H.265-specific decode picture info (required for H.265 decode).
    pub h265_info: Option<H265DecodeInfo>,
}

// ---------------------------------------------------------------------------
// SimpleDecoder config and output types
// ---------------------------------------------------------------------------

/// User-facing configuration for [`SimpleDecoder`].
///
/// Only the essentials: codec, optional max resolution (auto-detected from
/// SPS if zero), and DPB output mode.
#[derive(Debug, Clone)]
pub struct SimpleDecoderConfig {
    /// Codec to decode.
    pub codec: crate::encode::Codec,
    /// Maximum width for DPB allocation.  0 = auto-detect from first SPS.
    pub max_width: u32,
    /// Maximum height for DPB allocation.  0 = auto-detect from first SPS.
    pub max_height: u32,
    /// DPB/output mode.
    pub output_mode: DpbOutputMode,
}

impl Default for SimpleDecoderConfig {
    fn default() -> Self {
        Self {
            codec: crate::encode::Codec::H264,
            max_width: 0,
            max_height: 0,
            output_mode: DpbOutputMode::Coincide,
        }
    }
}

/// A fully decoded video frame with raw NV12 pixel data read back from the GPU.
#[derive(Debug, Clone)]
pub struct SimpleDecodedFrame {
    /// Raw NV12 data (Y plane followed by interleaved UV plane).
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Decode order index.
    pub decode_order: u64,
    /// Picture Order Count.
    pub picture_order_count: i32,
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Align `value` up to the next multiple of `alignment`.
///
/// `alignment` must be a power of two.
#[inline]
pub const fn align_up(value: vk::DeviceSize, alignment: vk::DeviceSize) -> vk::DeviceSize {
    (value + alignment - 1) & !(alignment - 1)
}

/// Select the best picture format for the given chroma subsampling and bit
/// depth. Returns NV12 for 4:2:0 8-bit, P010 for 4:2:0 10-bit, etc.
pub fn select_picture_format(
    chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
    luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
) -> vk::Format {
    match (chroma_subsampling, luma_bit_depth) {
        (vk::VideoChromaSubsamplingFlagsKHR::_420, vk::VideoComponentBitDepthFlagsKHR::_8) => {
            vk::Format::G8_B8R8_2PLANE_420_UNORM
        }
        (vk::VideoChromaSubsamplingFlagsKHR::_420, vk::VideoComponentBitDepthFlagsKHR::_10) => {
            vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16
        }
        (vk::VideoChromaSubsamplingFlagsKHR::_420, vk::VideoComponentBitDepthFlagsKHR::_12) => {
            vk::Format::G12X4_B12X4R12X4_2PLANE_420_UNORM_3PACK16
        }
        (vk::VideoChromaSubsamplingFlagsKHR::_422, vk::VideoComponentBitDepthFlagsKHR::_8) => {
            vk::Format::G8_B8R8_2PLANE_422_UNORM
        }
        (vk::VideoChromaSubsamplingFlagsKHR::_422, vk::VideoComponentBitDepthFlagsKHR::_10) => {
            vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
        }
        (vk::VideoChromaSubsamplingFlagsKHR::_444, vk::VideoComponentBitDepthFlagsKHR::_8) => {
            vk::Format::G8_B8_R8_3PLANE_444_UNORM
        }
        _ => vk::Format::G8_B8R8_2PLANE_420_UNORM, // fallback
    }
}

/// Check whether a memory type index matches the given requirements.
///
/// Pure logic helper for unit testing.
pub fn memory_type_matches(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    type_index: u32,
    type_bits: u32,
    required_flags: vk::MemoryPropertyFlags,
) -> bool {
    if type_index >= memory_properties.memory_type_count {
        return false;
    }
    if (type_bits & (1 << type_index)) == 0 {
        return false;
    }
    memory_properties.memory_types[type_index as usize]
        .property_flags
        .contains(required_flags)
}
