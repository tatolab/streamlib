// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan Video decode on any [`crate::VulkanVideoCodecDevice`] with a decode
//! queue. [`SimpleDecoder`] parses Annex-B NAL units, manages session
//! parameters and the DPB, and reads decoded pictures back, delegating each
//! picture to [`crate::vk_video_decoder::vk_video_decoder::VkVideoDecoder`]
//! for both H.264 and H.265.

mod annex_b_bitstream_feed;
pub(crate) mod decoded_picture_display_window;
mod decoded_picture_geometry_and_color;
mod decoded_picture_readback;
pub mod exported_nv12_dma_buf_ring;
mod h264;
mod h265;
mod session;
mod simple_decoder_construction;
mod types;

#[cfg(test)]
mod owned_device_decode_tests;
#[cfg(test)]
mod tests;

pub use types::{
    DecodeSubmitInfo, DecodedFrame, DecodedPictureDelivery, DpbOutputMode, H264DecodeInfo,
    H264RefPicInfo, H265DecodeInfo, H265RefPicInfo, ReferenceSlot, SimpleDecodedFrame,
    SimpleDecoderConfig, StagingBuffer, align_up, memory_type_matches, select_picture_format,
};

use std::sync::Arc;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use self::decoded_picture_display_window::DecodedPictureDisplayWindow;
use crate::VulkanVideoDecodeContext;
use crate::nv_video_parser::vulkan_h264_decoder::{
    MAX_DPB_SIZE as H264_MAX_DPB_SIZE, VulkanH264Decoder,
};
use crate::nv_video_parser::vulkan_h265_decoder::{HEVC_DPB_SIZE, VulkanH265Decoder};

// ======================================================================
// SimpleDecoder — high-level decoder with auto NAL parsing
// ======================================================================

/// H.264 / H.265 decoder on a codec device: Annex-B in, NV12 or RGBA
/// pictures read back out.
pub struct SimpleDecoder {
    device: vulkanalia::Device,

    // The ported VkVideoDecoder (used by both H.264 and H.265)
    vk_decoder: Option<crate::vk_video_decoder::vk_video_decoder::VkVideoDecoder>,

    // Queues
    decode_queue: vk::Queue,
    decode_queue_family: u32,
    transfer_queue: vk::Queue,
    transfer_queue_family: u32,

    // Transfer command pool/buffer/fence (for readback)
    transfer_pool: vk::CommandPool,
    transfer_cb: vk::CommandBuffer,
    transfer_fence: vk::Fence,

    // NAL parser state
    nal_buffer: Vec<u8>,

    // Cached VPS/SPS/PPS
    cached_vps_nalu: Option<Vec<u8>>,
    cached_sps_nalu: Option<Vec<u8>>,
    cached_pps_nalu: Option<Vec<u8>>,

    // The coded extent the active SPS states — what the video session, its
    // parameter sets and the DPB images are sized from. Both codecs pad this
    // up to a block size, so it is not what a decoded frame is published at.
    coded_picture_width: u32,
    coded_picture_height: u32,

    // The sub-region of the coded picture a decoded frame is published as,
    // read off the active SPS's conformance window. `None` until an SPS has
    // been parsed.
    decoded_picture_display_window: Option<DecodedPictureDisplayWindow>,

    // Session state
    session_configured: bool,
    frame_counter: u64,
    frame_num: u16,
    idr_pic_id: u16,

    // DPB tracking
    dpb_slot_in_use: Vec<bool>,
    dpb_slot_frame_num: Vec<u16>,
    dpb_slot_poc: Vec<[i32; 2]>,

    // Config
    config: SimpleDecoderConfig,

    ctx: Arc<VulkanVideoDecodeContext>,

    // H.264 parser state (SPS/PPS parsing, POC, DPB, ref list construction)
    h264_parser: Option<VulkanH264Decoder>,
    /// Maps parser DPB index → physical Vulkan DPB slot. -1 = no mapping.
    h264_dpb_to_slot: [i32; H264_MAX_DPB_SIZE + 1],

    // H.265 parser state (POC calculation, RPS derivation, logical DPB)
    h265_parser: Option<VulkanH265Decoder>,
    /// Maps logical DPB index (h265_parser.dpb[i]) → physical Vulkan DPB slot.
    /// -1 means no mapping.
    h265_dpb_to_slot: [i32; HEVC_DPB_SIZE],

    // Persistent staging buffer for decoded frame readback (avoids per-frame alloc)
    readback_staging: Option<(vk::Buffer, vma::Allocation, u64, *mut u8)>,

    // The exported buffers pictures are copied into under
    // `DecodedPictureDelivery::Nv12InAnExportedDmaBuf`.
    exported_nv12_dma_buf_ring: exported_nv12_dma_buf_ring::ExportedNv12DmaBufRing,

    // Deferred readback: metadata for the frame whose GPU decode is in flight.
    // Drained at the start of the next handle_h265_slice call (or flush).
    pending_frame: Option<PendingFrame>,

    // NV12→RGBA GPU compute converter (created under `DecodedPictureDelivery::ReadBackRgba`)
    nv12_converter: Option<crate::nv12_to_rgb::Nv12ToRgbConverter>,
    // RGBA staging buffer for readback after GPU conversion
    rgba_staging: Option<(vk::Buffer, vma::Allocation, u64, *mut u8)>,
    // Compute/transfer queue info for converter (graphics queue supports compute)
    compute_queue: vk::Queue,
    compute_queue_family: u32,

    // Host-side queue submission gateway (per-queue mutex synchronization).
    codec_device: Arc<dyn crate::VulkanVideoCodecDevice>,
}

/// Metadata for a frame whose GPU decode has been submitted but not yet read back.
struct PendingFrame {
    /// The coded extent the DPB slot holds — what the raw NV12 readback
    /// copies, since that path hands back the decoded picture as coded.
    coded_width: u32,
    coded_height: u32,
    /// The sub-region the RGBA path converts and publishes.
    display_window: DecodedPictureDisplayWindow,
    /// The colour the SPS this picture was decoded under signals, matrix and
    /// range filled for its display window.
    decoded_picture_color_vui: streamlib_consumer_rhi::h273_color_vui::H273ColorVui,
    decode_order: u64,
    poc: i32,
    setup_slot: usize,
    _setup_image: vk::Image,
    /// The exported buffer the picture was copied into, when it was.
    exported_nv12_ring_slot_index: Option<usize>,
}

// SAFETY: Vulkan handles are only accessed through &mut self methods.
unsafe impl Send for SimpleDecoder {}

impl SimpleDecoder {
    /// Return the number of frames decoded so far.
    pub fn decode_count(&self) -> u64 {
        self.frame_counter
    }

    /// The extent decoded frames are published at — the active SPS's coded
    /// extent with its conformance window applied, which is the picture the
    /// stream meant to carry rather than the block-aligned one it codes.
    pub fn dimensions(&self) -> (u32, u32) {
        let window = self.decoded_picture_display_window_or_whole_picture();
        (window.width, window.height)
    }
}

impl Drop for SimpleDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.codec_device.wait_idle();

            // Destroy NV12→RGBA converter (owns its own Vulkan resources)
            self.nv12_converter.take();

            // Destroy RGBA staging buffer
            if let Some((buf, alloc, _, _)) = self.rgba_staging.take() {
                self.ctx.allocator().destroy_buffer(buf, alloc);
            }

            // Destroy persistent NV12 staging buffer
            if let Some((buf, alloc, _, _)) = self.readback_staging.take() {
                self.ctx.allocator().destroy_buffer(buf, alloc);
            }

            // Destroy transfer resources
            if self.transfer_fence != vk::Fence::null() {
                self.device.destroy_fence(self.transfer_fence, None);
            }
            if self.transfer_pool != vk::CommandPool::null() {
                self.device.destroy_command_pool(self.transfer_pool, None);
            }

            // VkVideoDecoder, device, instance are dropped via their own Drop impls.
        }
    }
}
