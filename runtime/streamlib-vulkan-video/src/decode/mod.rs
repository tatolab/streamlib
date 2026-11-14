// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan Video decode on any [`crate::VulkanVideoCodecDevice`] with a decode
//! queue. [`SimpleDecoder`] parses Annex-B NAL units, manages session
//! parameters and the DPB, and reads decoded pictures back, delegating each
//! picture to [`crate::vk_video_decoder::vk_video_decoder::VkVideoDecoder`]
//! for both H.264 and H.265.

pub(crate) mod decoded_picture_display_window;
pub mod exported_nv12_dma_buf_ring;
mod h264;
mod h265;
mod session;
mod types;

#[cfg(test)]
mod owned_device_decode_tests;
#[cfg(test)]
mod tests;

pub use types::*;

use std::ptr;
use std::sync::Arc;
use tracing::{debug, info};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use self::decoded_picture_display_window::DecodedPictureDisplayWindow;
use crate::VideoError;
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
    /// Mint a decoder on `codec_device`'s video decode queue, staging
    /// readbacks on its transfer queue and converting NV12 → RGBA on its
    /// compute queue.
    ///
    /// # Errors
    ///
    /// Refused when the device enabled no Vulkan Video decode queue, or any
    /// Vulkan resource creation fails.
    pub fn new(
        codec_device: Arc<dyn crate::VulkanVideoCodecDevice>,
        config: SimpleDecoderConfig,
    ) -> Result<Self, VideoError> {
        let crate::VulkanVideoCodecQueue {
            queue: decode_queue,
            queue_family_index: decode_queue_family,
        } = codec_device.video_decode_queue().ok_or_else(|| {
            VideoError::BitstreamError(
                "the device enabled no video decode queue — it does not support Vulkan \
                 Video decode"
                    .into(),
            )
        })?;
        let crate::VulkanVideoCodecQueue {
            queue: transfer_queue,
            queue_family_index: transfer_queue_family,
        } = codec_device.transfer_queue();
        let crate::VulkanVideoCodecQueue {
            queue: compute_queue,
            queue_family_index: compute_queue_family,
        } = codec_device.compute_queue();
        let device = codec_device.device().clone();
        let ctx = Arc::new(VulkanVideoDecodeContext::new(Arc::clone(&codec_device))?);

        let transfer_pool = unsafe {
            device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::builder()
                        .queue_family_index(transfer_queue_family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(VideoError::from)?
        };

        let transfer_cb = unsafe {
            device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::builder()
                        .command_pool(transfer_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(VideoError::from)?[0]
        };

        let transfer_fence = unsafe {
            device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(VideoError::from)?
        };

        info!(codec = ?config.codec, "SimpleDecoder created (external device)");
        let exported_nv12_dma_buf_ring =
            exported_nv12_dma_buf_ring::ExportedNv12DmaBufRing::new(device.clone());

        Ok(Self {
            device,
            vk_decoder: None,
            decode_queue,
            decode_queue_family,
            transfer_queue,
            transfer_queue_family,
            transfer_pool,
            transfer_cb,
            transfer_fence,
            nal_buffer: Vec::new(),
            cached_vps_nalu: None,
            cached_sps_nalu: None,
            cached_pps_nalu: None,
            coded_picture_width: 0,
            coded_picture_height: 0,
            decoded_picture_display_window: None,
            session_configured: false,
            frame_counter: 0,
            frame_num: 0,
            idr_pic_id: 0,
            dpb_slot_in_use: Vec::new(),
            dpb_slot_frame_num: Vec::new(),
            dpb_slot_poc: Vec::new(),
            config,
            ctx,
            h264_parser: None,
            h264_dpb_to_slot: [-1i32; H264_MAX_DPB_SIZE + 1],
            h265_parser: None,
            h265_dpb_to_slot: [-1i32; HEVC_DPB_SIZE],
            readback_staging: None,
            exported_nv12_dma_buf_ring,
            pending_frame: None,
            nv12_converter: None,
            rgba_staging: None,
            compute_queue,
            compute_queue_family,
            codec_device,
        })
    }

    /// Feed arbitrary bytes of H.264 Annex B bitstream data.
    ///
    /// The data can contain partial NALs, full NALs, or multiple NALs.
    /// Internally the decoder finds start codes, identifies NAL types,
    /// auto-configures on the first SPS, and decodes slice NALs.
    ///
    /// Returns decoded frames (with NV12 data read back from the GPU).
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<SimpleDecodedFrame>, VideoError> {
        // Accumulate data
        self.nal_buffer.extend_from_slice(data);

        // Split into NAL units
        let nals = crate::split_annex_b_nal_units(&self.nal_buffer);

        if nals.is_empty() {
            return Ok(Vec::new());
        }

        // Keep any trailing partial NAL (data after the last start code
        // that might be incomplete)
        let last_sc_pos = Self::find_last_start_code_pos(&self.nal_buffer);
        let trailing = if let Some(pos) = last_sc_pos {
            // Check if this might be an incomplete NAL (no following start code)
            let after_last = Self::find_start_code_after(&self.nal_buffer, pos);
            if after_last.is_none() {
                // The last NAL might be incomplete — but we still process it
                // since feed() is typically called with complete data.
                // We clear the buffer entirely.
                None
            } else {
                None
            }
        } else {
            None
        };

        // Clear the buffer (we'll process all NALs)
        let buffer_copy = std::mem::take(&mut self.nal_buffer);
        if let Some(trailing_data) = trailing {
            self.nal_buffer = trailing_data;
        }

        // Process each NAL
        let mut frames = Vec::new();
        let extracted_nals = crate::split_annex_b_nal_units(&buffer_copy);

        let is_h265 = self.config.codec == crate::encode::Codec::H265;

        for nal in &extracted_nals {
            if nal.is_empty() {
                continue;
            }

            if is_h265 {
                // H.265 NAL header: 2 bytes, type = (byte0 >> 1) & 0x3F
                if nal.len() < 2 {
                    continue;
                }
                let nal_type = (nal[0] >> 1) & 0x3F;

                match nal_type {
                    32 => {
                        // VPS
                        debug!("H265 NAL: VPS ({} bytes)", nal.len());
                        self.handle_h265_vps(nal)?;
                    }
                    33 => {
                        // SPS
                        debug!("H265 NAL: SPS ({} bytes)", nal.len());
                        self.handle_h265_sps(nal)?;
                    }
                    34 => {
                        // PPS
                        debug!("H265 NAL: PPS ({} bytes)", nal.len());
                        self.handle_h265_pps(nal)?;
                    }
                    19 | 20 => {
                        // IDR_W_RADL (19) or IDR_N_LP (20)
                        debug!(
                            "H265 NAL: IDR slice type {} ({} bytes)",
                            nal_type,
                            nal.len()
                        );
                        if let Some(frame) = self.handle_h265_slice(nal, true)? {
                            frames.push(frame);
                        }
                    }
                    0..=9 | 16..=18 | 21 => {
                        // TRAIL_N/R, TSA_N/R, STSA_N/R, RADL_N/R, RASL_N/R,
                        // BLA_W_LP, BLA_W_RADL, BLA_N_LP, CRA_NUT
                        debug!("H265 NAL: slice type {} ({} bytes)", nal_type, nal.len());
                        let is_irap = (16..=21).contains(&nal_type);
                        if let Some(frame) = self.handle_h265_slice(nal, is_irap)? {
                            frames.push(frame);
                        }
                    }
                    _ => {
                        debug!(
                            "H265 NAL: type {} ({} bytes) -- skipped",
                            nal_type,
                            nal.len()
                        );
                    }
                }
            } else {
                // H.264 NAL header: 1 byte, type = byte0 & 0x1F
                let nal_type = nal[0] & 0x1F;

                match nal_type {
                    7 => {
                        // SPS
                        debug!("NAL: SPS ({} bytes)", nal.len());
                        self.handle_sps(nal)?;
                    }
                    8 => {
                        // PPS
                        debug!("NAL: PPS ({} bytes)", nal.len());
                        self.handle_pps(nal)?;
                    }
                    5 => {
                        // IDR slice
                        debug!("NAL: IDR slice ({} bytes)", nal.len());
                        if let Some(frame) = self.handle_slice(nal, true)? {
                            frames.push(frame);
                        }
                    }
                    1 => {
                        // Non-IDR slice
                        debug!("NAL: non-IDR slice ({} bytes)", nal.len());
                        if let Some(frame) = self.handle_slice(nal, false)? {
                            frames.push(frame);
                        }
                    }
                    _ => {
                        debug!("NAL: type {} ({} bytes) -- skipped", nal_type, nal.len());
                    }
                }
            }
        }

        // Flush the last pending frame (GPU decode submitted but not yet read back)
        if let Some(frame) = self.drain_pending_frame()? {
            frames.push(frame);
        }

        Ok(frames)
    }

    /// Signal a discontinuity in the bitstream (e.g., seek).
    ///
    /// Resets parser state and waits for the next IDR before decoding.
    pub fn feed_discontinuity(&mut self) {
        self.nal_buffer.clear();
        self.frame_num = 0;
        self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
        // Mark all DPB slots as unused
        self.dpb_slot_in_use.fill(false);
        // Reset H.264 parser DPB and physical slot mappings
        if let Some(ref mut parser) = self.h264_parser {
            parser.flush_decoded_picture_buffer();
        }
        self.h264_dpb_to_slot.fill(-1);
        info!("Discontinuity: waiting for next IDR");
    }

    /// Full reset: re-initialize session on the next SPS.
    pub fn reset(&mut self) {
        self.feed_discontinuity();
        self.session_configured = false;
        self.cached_vps_nalu = None;
        self.cached_sps_nalu = None;
        self.cached_pps_nalu = None;
        self.coded_picture_width = 0;
        self.coded_picture_height = 0;
        self.decoded_picture_display_window = None;
        self.frame_counter = 0;
        info!("Full reset: will reconfigure on next SPS");
    }

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

    /// The active SPS's display window, or the whole coded picture when no
    /// SPS has been parsed yet.
    fn decoded_picture_display_window_or_whole_picture(&self) -> DecodedPictureDisplayWindow {
        self.decoded_picture_display_window.unwrap_or_else(|| {
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(
                self.coded_picture_width,
                self.coded_picture_height,
            )
        })
    }

    /// Record the coded extent and the display window a freshly parsed SPS
    /// states. A window the SPS's own offsets make unusable is refused by
    /// name and the whole coded picture published instead — a mis-cropped
    /// frame is worse than an uncropped one, because its buffer and its
    /// header agree on a picture that was never coded.
    fn adopt_parsed_sps_geometry(
        &mut self,
        coded_width: u32,
        coded_height: u32,
        parsed_window: Option<DecodedPictureDisplayWindow>,
    ) {
        self.coded_picture_width = coded_width;
        self.coded_picture_height = coded_height;
        let window = parsed_window.unwrap_or_else(|| {
            tracing::warn!(
                codec = ?self.config.codec,
                coded_width,
                coded_height,
                "SPS conformance window does not describe a region inside the coded picture; \
                 publishing the whole coded picture"
            );
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(coded_width, coded_height)
        });
        // Said once per SPS, because a run whose decoded extent does not
        // match its coded one is exactly the thing a reader of these logs is
        // trying to confirm.
        if !window.crops_nothing(coded_width, coded_height) {
            info!(
                codec = ?self.config.codec,
                coded_width,
                coded_height,
                published_width = window.width,
                published_height = window.height,
                "SPS conformance window applied — decoded frames publish the cropped extent"
            );
        }
        self.decoded_picture_display_window = Some(window);
    }

    /// The colour a picture in `display_window` decoded under the active SPS
    /// is in: the signalled axes, with matrix and range filled by the decoded
    /// picture defaults.
    fn decoded_picture_color_vui(
        &self,
        display_window: DecodedPictureDisplayWindow,
    ) -> streamlib_consumer_rhi::h273_color_vui::H273ColorVui {
        self.signalled_color_vui()
            .unwrap_or_default()
            .with_decoded_picture_matrix_and_range_defaults(
                display_window.width,
                display_window.height,
            )
    }

    /// The H.273 colour the active SPS's VUI signals, or `None` if no SPS has
    /// been parsed yet or it carried no VUI.
    ///
    /// Each axis is `Some(byte)` only if the bitstream actually carried it
    /// AND it isn't the H.273 "Unspecified" enumerant (value `2`).
    fn signalled_color_vui(&self) -> Option<streamlib_consumer_rhi::h273_color_vui::H273ColorVui> {
        match self.config.codec {
            crate::encode::Codec::H264 => {
                let parser = self.h264_parser.as_ref()?;
                let sps = parser.sps.as_ref()?;
                if !sps.flags.vui_parameters_present_flag {
                    return None;
                }
                let vui = &sps.vui;
                let primaries = decoded_byte_to_option(vui.colour_primaries);
                let transfer = decoded_byte_to_option(vui.transfer_characteristics);
                let matrix = decoded_byte_to_option(vui.matrix_coefficients);
                let full_range = if vui.video_signal_type_present_flag {
                    Some(vui.video_full_range_flag)
                } else {
                    None
                };
                build_color_vui(primaries, transfer, matrix, full_range)
            }
            crate::encode::Codec::H265 => {
                let parser = self.h265_parser.as_ref()?;
                let sps = parser.active_sps[0].as_ref()?;
                if !sps.flags.vui_parameters_present_flag {
                    return None;
                }
                let vui = &sps.vui;
                let primaries = decoded_byte_to_option(vui.colour_primaries as i32);
                let transfer = decoded_byte_to_option(vui.transfer_characteristics as i32);
                let matrix = decoded_byte_to_option(vui.matrix_coeffs as i32);
                let full_range = if vui.flags.video_signal_type_present_flag {
                    Some(vui.flags.video_full_range_flag)
                } else {
                    None
                };
                build_color_vui(primaries, transfer, matrix, full_range)
            }
        }
    }

    // ------------------------------------------------------------------
    // Private: NAL unit splitting
    // ------------------------------------------------------------------

    /// Find the byte position of the last start code in the buffer.
    fn find_last_start_code_pos(data: &[u8]) -> Option<usize> {
        let mut last = None;
        let mut i = 0;
        while i + 2 < data.len() {
            if i + 3 < data.len()
                && data[i] == 0
                && data[i + 1] == 0
                && data[i + 2] == 0
                && data[i + 3] == 1
            {
                last = Some(i);
                i += 4;
            } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                last = Some(i);
                i += 3;
            } else {
                i += 1;
            }
        }
        last
    }

    /// Find a start code that begins strictly after `pos`.
    fn find_start_code_after(data: &[u8], pos: usize) -> Option<usize> {
        let mut i = pos + 1;
        while i + 2 < data.len() {
            let is_four_byte_start_code = i + 3 < data.len()
                && data[i] == 0
                && data[i + 1] == 0
                && data[i + 2] == 0
                && data[i + 3] == 1;
            let is_three_byte_start_code = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;
            if is_four_byte_start_code || is_three_byte_start_code {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    // ------------------------------------------------------------------
    // Private: Emulation prevention byte removal
    // ------------------------------------------------------------------

    /// Remove Annex B emulation prevention bytes (00 00 03 -> 00 00).
    ///
    /// H.264/H.265 NAL units use byte-stuffing: any occurrence of
    /// `00 00 03 XX` in the raw stream means the `03` is an escape byte
    /// and should be removed to recover the original RBSP data.
    fn remove_emulation_prevention_bytes(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 3 {
                out.push(0);
                out.push(0);
                i += 3; // skip the 0x03
            } else {
                out.push(data[i]);
                i += 1;
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // Private: Staging buffer management
    // ------------------------------------------------------------------

    /// The buffer the next decode copies its NV12 picture into and, when it is
    /// an exported one, its ring slot — or no buffer when the RGBA converter
    /// samples the DPB slot itself.
    fn staging_buffer_for_the_next_picture(
        &mut self,
        coded_width: u32,
        coded_height: u32,
    ) -> Result<(Option<StagingBuffer>, Option<usize>), VideoError> {
        match self.config.decoded_picture_delivery {
            DecodedPictureDelivery::ReadBackRgba => Ok((None, None)),
            DecodedPictureDelivery::ReadBackNv12 => {
                self.ensure_readback_staging(coded_width, coded_height)?;
                let &(buffer, _, _, _) = self.readback_staging.as_ref().ok_or_else(|| {
                    VideoError::BitstreamError("No readback staging buffer available".into())
                })?;
                Ok((Some(StagingBuffer { buffer }), None))
            }
            DecodedPictureDelivery::Nv12InAnExportedDmaBuf => {
                // SAFETY: the decode before this one was waited for when it
                // was drained, so no decode writes the ring.
                let (buffer, slot_index) = unsafe {
                    self.exported_nv12_dma_buf_ring.next_buffer_for_a_picture(
                        self.codec_device.instance(),
                        self.codec_device.physical_device(),
                        coded_width,
                        coded_height,
                    )?
                };
                Ok((Some(StagingBuffer { buffer }), Some(slot_index)))
            }
        }
    }

    /// Ensure the persistent readback staging buffer is large enough for the
    /// given frame dimensions. Allocates or grows as needed.
    fn ensure_readback_staging(&mut self, width: u32, height: u32) -> Result<(), VideoError> {
        let total = (width * height) as u64 + (width * height / 2) as u64;

        if self.readback_staging.as_ref().is_none_or(|s| s.2 < total) {
            if let Some((buf, alloc, _, _)) = self.readback_staging.take() {
                unsafe {
                    self.ctx.allocator().destroy_buffer(buf, alloc);
                }
            }
            let buf_info = vk::BufferCreateInfo::builder()
                .size(total)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            // The CPU reads this buffer back every frame, so it must not
            // land in write-combined memory: WC reads run at a few hundred
            // MB/s and one 1080p RGBA frame is 8.3 MB, which turns the
            // readback memcpy into tens of milliseconds — measured at 37 ms
            // median (225 MB/s) on NVIDIA 595.84, 93% of the whole decode
            // path. HOST_ACCESS_RANDOM declares the read access so VMA picks
            // a HOST_CACHED type, the same trap
            // `codec_utils/vulkan_bitstream_buffer_impl.rs` documents for the
            // encoder's bitstream readback.
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_RANDOM,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
                preferred_flags: vk::MemoryPropertyFlags::HOST_CACHED,
                ..Default::default()
            };
            let (buf, alloc) = unsafe {
                self.ctx
                    .allocator()
                    .create_buffer(buf_info, &alloc_opts)
                    .map_err(VideoError::from)?
            };
            let info = self.ctx.allocator().get_allocation_info(alloc);
            self.readback_staging = Some((buf, alloc, total, info.pMappedData as *mut u8));
        }

        Ok(())
    }

    /// Ensure the RGBA staging buffer is large enough for W*H*4 readback.
    fn ensure_rgba_staging(&mut self, width: u32, height: u32) -> Result<(), VideoError> {
        let total = (width as u64) * (height as u64) * 4;

        if self.rgba_staging.as_ref().is_none_or(|s| s.2 < total) {
            if let Some((buf, alloc, _, _)) = self.rgba_staging.take() {
                unsafe {
                    self.ctx.allocator().destroy_buffer(buf, alloc);
                }
            }
            let buf_info = vk::BufferCreateInfo::builder()
                .size(total)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            // Same read-back constraint as `ensure_readback_staging` above:
            // HOST_ACCESS_RANDOM + preferred HOST_CACHED, or the per-frame
            // 8.3 MB memcpy runs at write-combined speed.
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_RANDOM,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
                preferred_flags: vk::MemoryPropertyFlags::HOST_CACHED,
                ..Default::default()
            };
            let (buf, alloc) = unsafe {
                self.ctx
                    .allocator()
                    .create_buffer(buf_info, &alloc_opts)
                    .map_err(VideoError::from)?
            };
            let info = self.ctx.allocator().get_allocation_info(alloc);
            self.rgba_staging = Some((buf, alloc, total, info.pMappedData as *mut u8));
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Private: Pending frame drain
    // ------------------------------------------------------------------

    /// Wait for the in-flight decode to finish and read back the decoded frame.
    fn drain_pending_frame(&mut self) -> Result<Option<SimpleDecodedFrame>, VideoError> {
        let pending = match self.pending_frame.take() {
            Some(p) => p,
            None => return Ok(None),
        };

        // Wait for the GPU to finish writing to the staging buffer
        if let Some(ref mut vk_dec) = self.vk_decoder {
            unsafe {
                vk_dec.wait_for_decode()?;
            }
        }

        // RGBA path: run NV12→RGBA GPU compute conversion, then readback RGBA
        if self.nv12_converter.is_some() {
            let dpb_image = self
                .vk_decoder
                .as_ref()
                .ok_or_else(|| VideoError::BitstreamError("VkVideoDecoder not initialized".into()))?
                .dpb_image();
            let converter = self.nv12_converter_for(&pending.decoded_picture_color_vui)?;
            let (rgba_img, _) = unsafe {
                converter.convert(
                    dpb_image,
                    pending.setup_slot as u32,
                    vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                )?
            };

            // Sized for the copy below, which is the display window alone —
            // the padding rows outside it never reach a consumer.
            let display_window = pending.display_window;
            self.ensure_rgba_staging(display_window.width, display_window.height)?;
            let &(stg_buf, _, stg_size, stg_ptr) = self.rgba_staging.as_ref().unwrap();

            // Copy RGBA image → staging buffer via transfer command buffer
            unsafe {
                self.device
                    .reset_command_buffer(self.transfer_cb, vk::CommandBufferResetFlags::empty())
                    .map_err(VideoError::from)?;
                self.device
                    .begin_command_buffer(
                        self.transfer_cb,
                        &vk::CommandBufferBeginInfo::builder()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(VideoError::from)?;

                let copy_region = vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    // The conformance window's origin, not the picture's:
                    // a stream that offsets its window would otherwise copy
                    // the padding and call it the picture.
                    image_offset: vk::Offset3D {
                        x: display_window.origin_x as i32,
                        y: display_window.origin_y as i32,
                        z: 0,
                    },
                    image_extent: vk::Extent3D {
                        width: display_window.width,
                        height: display_window.height,
                        depth: 1,
                    },
                };

                self.device.cmd_copy_image_to_buffer(
                    self.transfer_cb,
                    rgba_img,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    stg_buf,
                    &[copy_region],
                );

                self.device
                    .end_command_buffer(self.transfer_cb)
                    .map_err(VideoError::from)?;
                self.device
                    .reset_fences(&[self.transfer_fence])
                    .map_err(VideoError::from)?;
                let cb_submit = vk::CommandBufferSubmitInfo::builder()
                    .command_buffer(self.transfer_cb)
                    .build();
                let cb_submits = [cb_submit];
                let submit_info = vk::SubmitInfo2::builder()
                    .command_buffer_infos(&cb_submits)
                    .build();
                self.codec_device.submit_to_queue(
                    self.transfer_queue,
                    &[submit_info],
                    self.transfer_fence,
                )?;
                self.device
                    .wait_for_fences(&[self.transfer_fence], true, u64::MAX)
                    .map_err(VideoError::from)?;
            }

            // Read RGBA data from staging buffer
            let rgba_size = (display_window.width * display_window.height * 4) as usize;
            let read_size = rgba_size.min(stg_size as usize);
            let mut rgba_data = vec![0u8; read_size];
            unsafe {
                ptr::copy_nonoverlapping(stg_ptr, rgba_data.as_mut_ptr(), read_size);
            }

            // Update DPB slot layout (converter transitions to SHADER_READ_ONLY_OPTIMAL)
            let vk_dec = self.vk_decoder.as_mut().ok_or_else(|| {
                VideoError::BitstreamError("VkVideoDecoder not initialized".into())
            })?;
            vk_dec.set_dpb_slot_layout(
                pending.setup_slot,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );

            return Ok(Some(SimpleDecodedFrame {
                data: rgba_data,
                width: display_window.width,
                height: display_window.height,
                decode_order: pending.decode_order,
                picture_order_count: pending.poc,
                is_rgba: true,
                decoded_picture_color_vui: pending.decoded_picture_color_vui,
                exported_nv12: None,
            }));
        }

        if let Some(slot_index) = pending.exported_nv12_ring_slot_index {
            let display_window = pending.display_window;
            let exported_nv12 = self
                .exported_nv12_dma_buf_ring
                .picture_in_slot(slot_index, display_window.origin_x, display_window.origin_y)
                .ok_or_else(|| {
                    VideoError::BitstreamError(format!(
                        "exported NV12 ring slot {slot_index} was reallocated before its picture \
                         was handed out"
                    ))
                })?;
            return Ok(Some(SimpleDecodedFrame {
                data: Vec::new(),
                width: display_window.width,
                height: display_window.height,
                decode_order: pending.decode_order,
                picture_order_count: pending.poc,
                is_rgba: false,
                decoded_picture_color_vui: pending.decoded_picture_color_vui,
                exported_nv12: Some(exported_nv12),
            }));
        }

        // NV12 path: read decoded NV12 data from the persistent staging buffer
        let &(_, _, size, ptr) = self.readback_staging.as_ref().ok_or_else(|| {
            VideoError::BitstreamError("No readback staging buffer available".into())
        })?;
        let mut decoded_data = vec![0u8; size as usize];
        unsafe {
            ptr::copy_nonoverlapping(ptr, decoded_data.as_mut_ptr(), decoded_data.len());
        }

        Ok(Some(SimpleDecodedFrame {
            data: decoded_data,
            width: pending.coded_width,
            height: pending.coded_height,
            decode_order: pending.decode_order,
            picture_order_count: pending.poc,
            is_rgba: false,
            decoded_picture_color_vui: pending.decoded_picture_color_vui,
            exported_nv12: None,
        }))
    }

    // ------------------------------------------------------------------
    // Private: DPB slot management
    // ------------------------------------------------------------------

    /// Find a free DPB slot, or evict the oldest one.
    fn find_free_dpb_slot(&self) -> usize {
        // First, find an unused slot
        for (i, in_use) in self.dpb_slot_in_use.iter().enumerate() {
            if !in_use {
                return i;
            }
        }
        // All slots in use — evict slot 0 (simplistic sliding window)
        0
    }
}

// ---------------------------------------------------------------------------
// Helpers — H.273 byte → Option<u8>
// ---------------------------------------------------------------------------

/// Map an H.273 byte parsed from the bitstream to `Some(byte)` if it is a
/// real value, or `None` if it is the H.273 "Unspecified" enumerant
/// (value `2`) or out-of-range. Returning `None` for `Unspecified` means
/// `signalled_color_vui()` axes match the on-wire ColorInfo semantics ("absent
/// IS unknown").
fn decoded_byte_to_option(value: i32) -> Option<u8> {
    if value <= 0 || value > 255 {
        return None;
    }
    let byte = value as u8;
    if byte == streamlib_consumer_rhi::h273_color_vui::H273_UNSPECIFIED {
        return None;
    }
    Some(byte)
}

/// Build an [`H273ColorVui`] from per-axis options, returning `None` when
/// every axis is `None` (so callers can disambiguate "no VUI" from "VUI
/// present but every axis was Unspecified").
fn build_color_vui(
    primaries: Option<u8>,
    transfer: Option<u8>,
    matrix: Option<u8>,
    full_range: Option<bool>,
) -> Option<streamlib_consumer_rhi::h273_color_vui::H273ColorVui> {
    if primaries.is_none() && transfer.is_none() && matrix.is_none() && full_range.is_none() {
        return None;
    }
    Some(streamlib_consumer_rhi::h273_color_vui::H273ColorVui {
        primaries,
        transfer,
        matrix,
        full_range,
    })
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

#[cfg(test)]
mod color_vui_helper_tests {
    use super::*;
    use streamlib_consumer_rhi::h273_color_vui;

    #[test]
    fn unspecified_byte_becomes_none() {
        assert_eq!(decoded_byte_to_option(2), None);
    }

    #[test]
    fn zero_or_negative_byte_becomes_none() {
        assert_eq!(decoded_byte_to_option(0), None);
        assert_eq!(decoded_byte_to_option(-1), None);
    }

    #[test]
    fn real_byte_becomes_some() {
        assert_eq!(decoded_byte_to_option(1), Some(1)); // BT.709
        assert_eq!(decoded_byte_to_option(13), Some(13)); // sRGB
        assert_eq!(decoded_byte_to_option(16), Some(16)); // PQ
    }

    #[test]
    fn all_axes_none_returns_none_vui() {
        assert!(build_color_vui(None, None, None, None).is_none());
    }

    #[test]
    fn full_range_alone_returns_some_vui() {
        let vui =
            build_color_vui(None, None, None, Some(true)).expect("non-empty axis yields Some");
        assert_eq!(vui.full_range, Some(true));
        assert!(vui.primaries.is_none());
    }

    #[test]
    fn hdr10_axes_round_trip_to_h273_bytes() {
        let vui = build_color_vui(
            Some(h273_color_vui::primaries::BT2020),
            Some(h273_color_vui::transfer::SMPTE2084),
            Some(h273_color_vui::matrix::BT2020_NCL),
            Some(true),
        )
        .expect("non-empty axes yield Some");
        assert_eq!(vui.primaries, Some(9));
        assert_eq!(vui.transfer, Some(16));
        assert_eq!(vui.matrix, Some(9));
        assert_eq!(vui.full_range, Some(true));
    }
}
