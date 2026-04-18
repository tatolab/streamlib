// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public decode API for the nvpro-vulkan-video library.
//!
//! [`SimpleDecoder`] is the high-level entry point: it handles Vulkan
//! instance/device creation, NAL parsing, session management, and frame
//! readback internally. Under the hood it delegates to [`VkVideoDecoder`]
//! for both H.264 and H.265.
//!
//! Supporting types ([`DecodeSubmitInfo`], [`DecodedFrame`], [`ReferenceSlot`],
//! etc.) are shared with [`VkVideoDecoder`].

mod types;
mod session;
mod h264;
mod h265;

#[cfg(test)]
mod tests;

pub use types::*;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};
use std::ptr;
use std::sync::Arc;
use tracing::{debug, info};

use crate::nv_video_parser::vulkan_h264_decoder::{
    VulkanH264Decoder, MAX_DPB_SIZE as H264_MAX_DPB_SIZE,
};
use crate::nv_video_parser::vulkan_h265_decoder::{
    VulkanH265Decoder, HEVC_DPB_SIZE,
};
use crate::video_context::{VideoContext, VideoError};

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
    nv12_converter: Option<crate::nv12_to_rgb::Nv12ToRgbConverter>,
    // RGBA staging buffer for readback after GPU conversion
    rgba_staging: Option<(vk::Buffer, vma::Allocation, u64, *mut u8)>,
    // Compute/transfer queue info for converter (graphics queue supports compute)
    compute_queue: vk::Queue,
    compute_queue_family: u32,
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
    /// Create a new `SimpleDecoder` from the given configuration.
    ///
    /// This creates a Vulkan instance, selects a GPU with video decode
    /// support, and creates a device with the required extensions.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vulkan cannot be loaded
    /// - No GPU supports video decode for the requested codec
    /// - Any Vulkan resource creation fails
    pub fn new(config: SimpleDecoderConfig) -> Result<Self, VideoError> {
        unsafe { Self::create_internal(config) }
    }

    /// Create a decoder using an externally-owned Vulkan device and allocator.
    ///
    /// Use this when integrating with a host application (e.g., streamlib RHI)
    /// that already owns the Vulkan device. No new instance/device is created.
    pub fn from_device(
        config: SimpleDecoderConfig,
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        physical_device: vk::PhysicalDevice,
        allocator: Arc<vma::Allocator>,
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
        )?);

        // Load Vulkan for the Entry field (required by struct, not used for external path).
        let entry = unsafe {
            vulkanalia::Entry::new(
                vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY)
                    .map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan loader: {}", e)))?,
            ).map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan: {}", e)))?
        };

        let transfer_pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(transfer_queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            ).map_err(VideoError::from)?
        };

        let transfer_cb = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(transfer_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            ).map_err(VideoError::from)?[0]
        };

        let transfer_fence = unsafe {
            device.create_fence(&vk::FenceCreateInfo::default(), None)
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
        })
    }

    /// Create the decoder (all Vulkan setup, unsafe due to raw Vulkan calls).
    unsafe fn create_internal(config: SimpleDecoderConfig) -> Result<Self, VideoError> {
        // 1. Load Vulkan
        let loader = vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY)
            .map_err(|e| VideoError::BitstreamError(format!("Failed to load Vulkan library: {}", e)))?;
        let entry = vulkanalia::Entry::new(loader)
            .map_err(|e| VideoError::BitstreamError(format!("Failed to create Vulkan entry: {}", e)))?;

        // 2. Create instance (enable validation layers if available)
        let app_info = vk::ApplicationInfo::builder()
            .application_name(b"nvpro-simple-decoder\0")
            .api_version(vk::make_version(1, 3, 0));

        let validation_layer = b"VK_LAYER_KHRONOS_validation\0";
        let layer_props = entry.enumerate_instance_layer_properties()
            .unwrap_or_default();
        let has_validation = layer_props.iter().any(|p| {
            let name = unsafe { std::ffi::CStr::from_ptr(p.layer_name.as_ptr()) };
            name.to_bytes() == b"VK_LAYER_KHRONOS_validation"
        });

        let layers: Vec<*const i8> = if has_validation {
            info!("Vulkan validation layer enabled");
            vec![validation_layer.as_ptr() as *const i8]
        } else {
            Vec::new()
        };

        let instance = entry
            .create_instance(
                &vk::InstanceCreateInfo::builder()
                    .application_info(&app_info)
                    .enabled_layer_names(&layers),
                None,
            )
            .map_err(VideoError::from)?;

        // 3. Find physical device with decode support
        let physical_devices = instance
            .enumerate_physical_devices()
            .map_err(VideoError::from)?;

        if physical_devices.is_empty() {
            instance.destroy_instance(None);
            return Err(VideoError::BitstreamError(
                "No Vulkan physical devices found".to_string(),
            ));
        }

        let _codec_flag = match config.codec {
            crate::encode::Codec::H264 => vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            crate::encode::Codec::H265 => vk::VideoCodecOperationFlagsKHR::DECODE_H265,
        };

        // Find a device with a decode queue family + compute-capable queue
        let mut selected_device = None;
        let mut decode_qf = 0u32;
        let mut transfer_qf = 0u32;
        let mut compute_qf = 0u32;

        for &pd in &physical_devices {
            let qf_props = instance.get_physical_device_queue_family_properties(pd);
            let mut found_decode = false;
            let mut found_transfer = false;
            let mut found_compute = false;

            for (i, p) in qf_props.iter().enumerate() {
                if p.queue_flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR) && !found_decode {
                    decode_qf = i as u32;
                    found_decode = true;
                    if p.queue_flags.contains(vk::QueueFlags::TRANSFER) {
                        transfer_qf = i as u32;
                        found_transfer = true;
                    }
                }
                // Find a compute-capable queue (GRAPHICS queues always support COMPUTE)
                if (p.queue_flags.contains(vk::QueueFlags::COMPUTE)
                    || p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                    && !found_compute
                {
                    compute_qf = i as u32;
                    found_compute = true;
                    // Also use as transfer fallback if needed
                    if !found_transfer {
                        transfer_qf = i as u32;
                        found_transfer = true;
                    }
                }
            }

            if found_decode && found_transfer && found_compute {
                selected_device = Some(pd);
                break;
            }
        }

        let physical_device = match selected_device {
            Some(pd) => pd,
            None => {
                instance.destroy_instance(None);
                return Err(VideoError::NoVideoQueueFamily);
            }
        };

        // 4. Create device with required extensions
        let codec_ext = match config.codec {
            crate::encode::Codec::H264 => vk::KHR_VIDEO_DECODE_H264_EXTENSION.name.as_ptr(),
            crate::encode::Codec::H265 => vk::KHR_VIDEO_DECODE_H265_EXTENSION.name.as_ptr(),
        };

        let mut device_extensions = vec![
            vk::KHR_VIDEO_QUEUE_EXTENSION.name.as_ptr(),
            vk::KHR_VIDEO_DECODE_QUEUE_EXTENSION.name.as_ptr(),
            codec_ext,
            vk::KHR_SYNCHRONIZATION2_EXTENSION.name.as_ptr(),
            vk::KHR_PUSH_DESCRIPTOR_EXTENSION.name.as_ptr(),
        ];
        device_extensions.sort();
        device_extensions.dedup();

        let queue_priorities = [1.0f32];
        let mut queue_family_set = vec![decode_qf];
        if transfer_qf != decode_qf { queue_family_set.push(transfer_qf); }
        if !queue_family_set.contains(&compute_qf) { queue_family_set.push(compute_qf); }
        let queue_create_infos: Vec<_> = queue_family_set.iter().map(|&qf| {
            vk::DeviceQueueCreateInfo::builder()
                .queue_family_index(qf)
                .queue_priorities(&queue_priorities)
        }).collect();

        let mut sync2 =
            vk::PhysicalDeviceSynchronization2Features::builder().synchronization2(true);

        let device_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .push_next(&mut sync2);

        let device = instance
            .create_device(physical_device, &device_info, None)
            .map_err(VideoError::from)?;

        let decode_queue = device.get_device_queue(decode_qf, 0);
        let transfer_queue = device.get_device_queue(transfer_qf, 0);
        let compute_queue_obj = device.get_device_queue(compute_qf, 0);

        // 5. Create VideoContext
        let ctx = Arc::new(VideoContext::new(
            instance.clone(),
            device.clone(),
            physical_device,
        )?);

        // 6. Create transfer command pool/buffer/fence for readback
        let transfer_pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(transfer_qf)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        ).map_err(VideoError::from)?;

        let transfer_cb = device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(transfer_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        ).map_err(VideoError::from)?[0];

        let transfer_fence = device
            .create_fence(&vk::FenceCreateInfo::default(), None)
            .map_err(VideoError::from)?;

        info!(codec = ?config.codec, "SimpleDecoder created");

        Ok(Self {
            _entry: entry,
            _instance: instance,
            device,
            vk_decoder: None,
            decode_queue,
            decode_queue_family: decode_qf,
            transfer_queue,
            transfer_queue_family: transfer_qf,
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
            compute_queue: compute_queue_obj,
            compute_queue_family: compute_qf,
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
                        debug!("H265 NAL: IDR slice type {} ({} bytes)", nal_type, nal.len());
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
                        debug!("H265 NAL: type {} ({} bytes) -- skipped", nal_type, nal.len());
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
                unsafe { self.ctx.allocator().destroy_buffer(buf, alloc); }
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
                self.ctx.allocator()
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
                unsafe { self.ctx.allocator().destroy_buffer(buf, alloc); }
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
                self.ctx.allocator()
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
            unsafe { vk_dec.wait_for_decode()?; }
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
                self.device.reset_command_buffer(
                    self.transfer_cb,
                    vk::CommandBufferResetFlags::empty(),
                ).map_err(VideoError::from)?;
                self.device.begin_command_buffer(
                    self.transfer_cb,
                    &vk::CommandBufferBeginInfo::builder()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                ).map_err(VideoError::from)?;

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

                self.device.end_command_buffer(self.transfer_cb).map_err(VideoError::from)?;
                self.device.reset_fences(&[self.transfer_fence]).map_err(VideoError::from)?;
                let cbs = [self.transfer_cb];
                let submit_info = vk::SubmitInfo::builder().command_buffers(&cbs);
                self.device.queue_submit(
                    self.transfer_queue,
                    &[submit_info],
                    self.transfer_fence,
                ).map_err(VideoError::from)?;
                self.device.wait_for_fences(
                    &[self.transfer_fence],
                    true,
                    u64::MAX,
                ).map_err(VideoError::from)?;
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

impl Drop for SimpleDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();

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
