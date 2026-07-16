// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public decoder API for the codec layer.
//!
//! [`SimpleDecoder`] is the high-level entry point: it handles Vulkan
//! instance/device creation, NAL parsing, session management, and frame
//! readback internally. Under the hood it delegates to [`VkVideoDecoder`]
//! for both H.264 and H.265.
//!
//! Supporting types ([`DecodeSubmitInfo`], [`DecodedFrame`], [`ReferenceSlot`],
//! etc.) are shared with [`VkVideoDecoder`].

mod h264;
mod h265;
mod session;
mod types;

#[cfg(test)]
mod tests;

pub use types::*;

use std::ptr;
use std::sync::Arc;
use tracing::{debug, info};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use crate::vulkan::video::nv_video_parser::vulkan_h264_decoder::{
    MAX_DPB_SIZE as H264_MAX_DPB_SIZE, VulkanH264Decoder,
};
use crate::vulkan::video::nv_video_parser::vulkan_h265_decoder::{
    HEVC_DPB_SIZE, VulkanH265Decoder,
};
use crate::vulkan::video::video_context::{VideoContext, VideoError};

// ======================================================================
// SimpleDecoder — high-level decoder with auto NAL parsing
// ======================================================================

/// High-level decoder that handles all Vulkan boilerplate automatically.
///
/// Creates a Vulkan instance and device internally, parses NAL units,
/// manages SPS/PPS extraction, session parameters, DPB, and frame readback.
/// Delegates actual decode to [`VkVideoDecoder`].
///
/// # Example
///
/// ```ignore
/// let config = SimpleDecoderConfig {
///     codec: Codec::H264,
///     ..Default::default()
/// };
///
/// let mut dec = SimpleDecoder::new(config)?;
/// let frames = dec.feed(&h264_bitstream)?;
/// for frame in frames {
///     // frame.data contains NV12 pixels
///     assert!(frame.width > 0 && frame.height > 0);
/// }
/// ```
pub struct SimpleDecoder {
    // Vulkan objects we own
    _entry: vulkanalia::Entry,
    _instance: vulkanalia::Instance,
    device: vulkanalia::Device,

    // The ported VkVideoDecoder (used by both H.264 and H.265)
    vk_decoder: Option<crate::vulkan::video::vk_video_decoder::vk_video_decoder::VkVideoDecoder>,

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

    // Parsed dimensions from SPS
    sps_width: u32,
    sps_height: u32,

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

    // VideoContext (needed for readback)
    ctx: Arc<VideoContext>,

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

    // Deferred readback: metadata for the frame whose GPU decode is in flight.
    // Drained at the start of the next handle_h265_slice call (or flush).
    pending_frame: Option<PendingFrame>,

    // NV12→RGBA GPU compute converter (created when config.rgba_output is true)
    nv12_converter: Option<crate::vulkan::video::nv12_to_rgb::Nv12ToRgbConverter>,
    // RGBA staging buffer for readback after GPU conversion
    rgba_staging: Option<(vk::Buffer, vma::Allocation, u64, *mut u8)>,
    // Compute/transfer queue info for converter (graphics queue supports compute)
    compute_queue: vk::Queue,
    compute_queue_family: u32,

    // Host-side queue submission gateway (per-queue mutex synchronization).
    host_device: Arc<crate::vulkan::rhi::HostVulkanDevice>,
}

/// Metadata for a frame whose GPU decode has been submitted but not yet read back.
struct PendingFrame {
    width: u32,
    height: u32,
    decode_order: u64,
    poc: i32,
    setup_slot: usize,
    _setup_image: vk::Image,
}

// SAFETY: Vulkan handles are only accessed through &mut self methods.
unsafe impl Send for SimpleDecoder {}

impl SimpleDecoder {
    /// Create a `SimpleDecoder` bound to the engine's host RHI.
    ///
    /// Borrows the FullAccess context to pull the host's Vulkan instance,
    /// device, allocator, queue mutex, and the video decode / transfer
    /// queues — the codec submits through the host's per-queue
    /// serialization via [`crate::vulkan::rhi::HostVulkanDevice::submit_to_queue`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The host device wasn't created with video decode support
    ///   ([`vk::QueueFlags::VIDEO_DECODE_KHR`] queue family missing).
    /// - Any Vulkan resource creation fails.
    pub fn from_full_access(
        full: &crate::core::context::GpuContextFullAccess,
        config: SimpleDecoderConfig,
    ) -> Result<Self, VideoError> {
        // Cdylib-safe: `GpuContextFullAccess::device()` returns
        // `&Arc<GpuDevice>` which borrows engine-private state and
        // panics in cdylib mode. Route through the
        // `host_vulkan_device_arc` FullAccess vtable slot so workspace
        // plugin cdylibs (the decoder packages) can construct a
        // decoder without tripping the panic guard.
        //
        // This ABI-transit path is retiring (#1265): the modern host-side
        // construction is
        // [`crate::core::context::GpuContext::create_decoder_session`],
        // which shares [`Self::from_host_device`] below without the
        // `host_vulkan_device_arc` transit. `from_full_access` stays only
        // until the decoder packages flip to the cdylib-safe primitive.
        let host_device = full.host_vulkan_device_arc().map_err(|e| {
            VideoError::Engine(format!(
                "Failed to acquire host Vulkan device for decoder: {e}"
            ))
        })?;
        Self::from_host_device(host_device, config)
    }

    /// Create a `SimpleDecoder` directly from a host-owned
    /// `Arc<HostVulkanDevice>` — the modern, cdylib-safe construction path
    /// (no `host_vulkan_device_arc` ABI transit). Pulls the host's Vulkan
    /// instance, device, allocator, and the video decode / transfer queues
    /// off the Arc; the codec submits through the host's per-queue
    /// serialization via
    /// [`crate::vulkan::rhi::HostVulkanDevice::submit_to_queue`]. Backs
    /// [`crate::core::context::GpuContext::create_decoder_session`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The host device wasn't created with video decode support
    ///   ([`vk::QueueFlags::VIDEO_DECODE_KHR`] queue family missing).
    /// - Any Vulkan resource creation fails.
    pub(crate) fn from_host_device(
        host_device: Arc<crate::vulkan::rhi::HostVulkanDevice>,
        config: SimpleDecoderConfig,
    ) -> Result<Self, VideoError> {
        let decode_queue = host_device.video_decode_queue().ok_or_else(|| {
            VideoError::BitstreamError(
                "host device has no video decode queue family — \
                 GPU does not support Vulkan Video decode"
                    .into(),
            )
        })?;
        let decode_queue_family =
            host_device
                .video_decode_queue_family_index()
                .ok_or_else(|| {
                    VideoError::BitstreamError(
                        "host device exposes decode queue but no queue family index".into(),
                    )
                })?;
        let host_arc: Arc<crate::vulkan::rhi::HostVulkanDevice> = Arc::clone(&host_device);

        Self::from_external_parts(
            config,
            host_device.instance().clone(),
            host_device.device().clone(),
            host_device.physical_device(),
            host_device.allocator().clone(),
            host_arc,
            decode_queue,
            decode_queue_family,
            host_device.queue(),
            host_device.queue_family_index(),
        )
    }

    /// Internal helper — assemble a `SimpleDecoder` from the host RHI's
    /// already-validated Vulkan handles. Only callable from
    /// [`Self::from_host_device`]; not exposed to consumers.
    #[allow(clippy::too_many_arguments)]
    fn from_external_parts(
        config: SimpleDecoderConfig,
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        physical_device: vk::PhysicalDevice,
        allocator: Arc<vma::Allocator>,
        host_device: Arc<crate::vulkan::rhi::HostVulkanDevice>,
        decode_queue: vk::Queue,
        decode_queue_family: u32,
        transfer_queue: vk::Queue,
        transfer_queue_family: u32,
    ) -> Result<Self, VideoError> {
        let ctx = Arc::new(VideoContext::from_external(
            instance.clone(),
            device.clone(),
            physical_device,
            allocator,
            host_device.clone(),
        )?);

        // Load Vulkan for the Entry field (required by struct, not used for external path).
        let entry = unsafe {
            vulkanalia::Entry::new(
                vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY).map_err(
                    |e| VideoError::BitstreamError(format!("Failed to load Vulkan loader: {}", e)),
                )?,
            )
            .map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan: {}", e)))?
        };

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

        Ok(Self {
            _entry: entry,
            _instance: instance,
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
            sps_width: 0,
            sps_height: 0,
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
            pending_frame: None,
            nv12_converter: None,
            rgba_staging: None,
            compute_queue: transfer_queue,
            compute_queue_family: transfer_queue_family,
            host_device,
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
        let nals = Self::split_nal_units_owned(&self.nal_buffer);

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
        let extracted_nals = Self::split_nal_units_owned(&buffer_copy);

        let is_h265 = self.config.codec == crate::vulkan::video::encode::Codec::H265;

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
                        let is_irap = nal_type >= 16 && nal_type <= 21;
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
        for slot in &mut self.dpb_slot_in_use {
            *slot = false;
        }
        // Reset H.264 parser DPB and physical slot mappings
        if let Some(ref mut parser) = self.h264_parser {
            parser.flush_decoded_picture_buffer();
        }
        for s in &mut self.h264_dpb_to_slot {
            *s = -1;
        }
        info!("Discontinuity: waiting for next IDR");
    }

    /// Full reset: re-initialize session on the next SPS.
    pub fn reset(&mut self) {
        self.feed_discontinuity();
        self.session_configured = false;
        self.cached_vps_nalu = None;
        self.cached_sps_nalu = None;
        self.cached_pps_nalu = None;
        self.sps_width = 0;
        self.sps_height = 0;
        self.frame_counter = 0;
        info!("Full reset: will reconfigure on next SPS");
    }

    /// Return the number of frames decoded so far.
    pub fn decode_count(&self) -> u64 {
        self.frame_counter
    }

    /// Return the detected stream dimensions (from SPS).
    pub fn dimensions(&self) -> (u32, u32) {
        (self.sps_width, self.sps_height)
    }

    /// Return the H.273 color VUI parsed from the active SPS, or `None` if
    /// no SPS has been parsed yet or the SPS didn't carry a VUI.
    ///
    /// Each axis is `Some(byte)` only if the bitstream actually carried it
    /// AND it isn't the H.273 "Unspecified" enumerant (value `2`); axes
    /// that the bitstream omitted or marked Unspecified come back as
    /// `None`. Callers translate these byte values to their domain
    /// `ColorInfo` at the codec-processor seam.
    pub fn current_color_vui(&self) -> Option<crate::vulkan::video::H273ColorVui> {
        match self.config.codec {
            crate::vulkan::video::encode::Codec::H264 => {
                let parser = self.h264_parser.as_ref()?;
                let sps = parser.sps.as_ref()?;
                if !sps.flags.vui_parameters_present_flag {
                    return None;
                }
                let vui = &sps.vui;
                let primaries = decoded_byte_to_option(vui.colour_primaries as i32);
                let transfer = decoded_byte_to_option(vui.transfer_characteristics as i32);
                let matrix = decoded_byte_to_option(vui.matrix_coefficients as i32);
                let full_range = if vui.video_signal_type_present_flag {
                    Some(vui.video_full_range_flag)
                } else {
                    None
                };
                build_color_vui(primaries, transfer, matrix, full_range)
            }
            crate::vulkan::video::encode::Codec::H265 => {
                let parser = self.h265_parser.as_ref()?;
                // Find the first populated SPS slot — streamlib's encoder
                // only emits sps_id=0, and decoders typically only see one.
                let sps = parser.spss.iter().filter_map(|s| s.as_ref()).next()?;
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

    /// Find Annex B start codes and split into NAL units (owned version).
    fn split_nal_units_owned(data: &[u8]) -> Vec<Vec<u8>> {
        let mut nals = Vec::new();
        let mut i = 0;
        let mut start: Option<usize> = None;

        while i + 2 < data.len() {
            let is_start_code = if i + 3 < data.len()
                && data[i] == 0
                && data[i + 1] == 0
                && data[i + 2] == 0
                && data[i + 3] == 1
            {
                Some(4)
            } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                Some(3)
            } else {
                None
            };

            if let Some(sc_len) = is_start_code {
                if let Some(s) = start {
                    nals.push(data[s..i].to_vec());
                }
                start = Some(i + sc_len);
                i += sc_len;
            } else {
                i += 1;
            }
        }

        if let Some(s) = start {
            if s < data.len() {
                nals.push(data[s..].to_vec());
            }
        }

        nals
    }

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
            if i + 3 < data.len()
                && data[i] == 0
                && data[i + 1] == 0
                && data[i + 2] == 0
                && data[i + 3] == 1
            {
                return Some(i);
            } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                return Some(i);
            } else {
                i += 1;
            }
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

    /// Ensure the persistent readback staging buffer is large enough for the
    /// given frame dimensions. Allocates or grows as needed.
    fn ensure_readback_staging(&mut self, width: u32, height: u32) -> Result<(), VideoError> {
        let total = (width * height) as u64 + (width * height / 2) as u64;

        if self.readback_staging.as_ref().map_or(true, |s| s.2 < total) {
            if let Some((buf, alloc, _, _)) = self.readback_staging.take() {
                unsafe {
                    self.ctx.allocator().destroy_buffer(buf, alloc);
                }
            }
            let buf_info = vk::BufferCreateInfo::builder()
                .size(total)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
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

        if self.rgba_staging.as_ref().map_or(true, |s| s.2 < total) {
            if let Some((buf, alloc, _, _)) = self.rgba_staging.take() {
                unsafe {
                    self.ctx.allocator().destroy_buffer(buf, alloc);
                }
            }
            let buf_info = vk::BufferCreateInfo::builder()
                .size(total)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
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
            let vk_dec = self.vk_decoder.as_ref().ok_or_else(|| {
                VideoError::BitstreamError("VkVideoDecoder not initialized".into())
            })?;
            let dpb_image = vk_dec.dpb_image();

            // Run GPU NV12→RGBA conversion
            let converter = self.nv12_converter.as_mut().unwrap();
            let (rgba_img, _) = unsafe {
                converter.convert(
                    dpb_image,
                    pending.setup_slot as u32,
                    vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                )?
            };

            // Ensure RGBA staging buffer exists
            self.ensure_rgba_staging(pending.width, pending.height)?;
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
                    image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                    image_extent: vk::Extent3D {
                        width: pending.width,
                        height: pending.height,
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
                self.host_device
                    .submit_to_queue(self.transfer_queue, &[submit_info], self.transfer_fence)
                    .map_err(VideoError::from)?;
                self.device
                    .wait_for_fences(&[self.transfer_fence], true, u64::MAX)
                    .map_err(VideoError::from)?;
            }

            // Read RGBA data from staging buffer
            let rgba_size = (pending.width * pending.height * 4) as usize;
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
                width: pending.width,
                height: pending.height,
                decode_order: pending.decode_order,
                picture_order_count: pending.poc,
                is_rgba: true,
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
            width: pending.width,
            height: pending.height,
            decode_order: pending.decode_order,
            picture_order_count: pending.poc,
            is_rgba: false,
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
/// `current_color_vui()` axes match the on-wire ColorInfo semantics ("absent
/// IS unknown").
fn decoded_byte_to_option(value: i32) -> Option<u8> {
    if value <= 0 || value > 255 {
        return None;
    }
    let byte = value as u8;
    if byte == crate::vulkan::video::encode::color_vui::H273_UNSPECIFIED {
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
) -> Option<crate::vulkan::video::H273ColorVui> {
    if primaries.is_none() && transfer.is_none() && matrix.is_none() && full_range.is_none() {
        return None;
    }
    Some(crate::vulkan::video::H273ColorVui {
        primaries,
        transfer,
        matrix,
        full_range,
    })
}

#[cfg(test)]
mod color_vui_helper_tests {
    use super::*;
    use crate::vulkan::video::encode::color_vui;

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
            Some(color_vui::primaries::BT2020),
            Some(color_vui::transfer::SMPTE2084),
            Some(color_vui::matrix::BT2020_NCL),
            Some(true),
        )
        .expect("non-empty axes yield Some");
        assert_eq!(vui.primaries, Some(9));
        assert_eq!(vui.transfer, Some(16));
        assert_eq!(vui.matrix, Some(9));
        assert_eq!(vui.full_range, Some(true));
    }
}

impl Drop for SimpleDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.host_device.wait_idle();

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
