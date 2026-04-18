// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public Encoder API for the nvpro-vulkan-video Rust library.
//!
//! This module provides the high-level `SimpleEncoder` type that wraps the
//! internal encoder pipeline (video session, DPB, command buffers, query pool)
//! and exposes simple `submit_frame` / `encode_image` methods for callers who
//! want encoded bitstream output from either raw NV12 pixels or GPU-resident
//! RGBA images.
//!
//! `SimpleEncoder` handles all Vulkan boilerplate automatically: instance and
//! device creation, staging buffers, image layout transitions, GOP-driven
//! frame type selection, and SPS/PPS header extraction.
//!
//! Port note: There is no single C++ file this maps to. It is the public
//! interface that stitches together VkVideoEncoder, VkEncoderConfig, the DPB
//! modules, and Vulkan Video encode commands into a usable library API.

pub mod config;
mod session;
mod submit;
mod staging;
mod gop;
mod vui_patch;

#[cfg(test)]
mod tests;

pub use config::*;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionDeviceCommands;
use vulkanalia_vma as vma;
use std::ptr;
use std::sync::Arc;

use crate::video_context::{VideoContext, VideoError};
use crate::vk_video_encoder::vk_video_encoder_h264::VkVideoEncoderH264;
use crate::vk_video_encoder::vk_video_encoder_h265::VkVideoEncoderH265;
use crate::vk_video_encoder::vk_encoder_config_h264::EncoderConfigH264;
use crate::vk_video_encoder::vk_encoder_config_h265::EncoderConfigH265;
use crate::vk_video_encoder::vk_video_gop_structure::{
    GopState, VkVideoGopStructure,
};

use config::{DpbSlot, EncodeConfig};
use gop::ReorderEntry;

// ===========================================================================
// SimpleEncoder
// ===========================================================================
//
// High-level encoder that owns the complete Vulkan encode pipeline: instance,
// device, video session, DPB, bitstream buffer, staging, source image, and
// GOP-driven frame type selection.  Callers can encode raw NV12 frames or
// GPU-resident RGBA images in under 20 lines.

/// High-level encoder that handles all Vulkan boilerplate automatically.
///
/// Owns the complete Vulkan Video encode pipeline including video session,
/// DPB, bitstream buffer, staging buffer, source image, and GOP-driven
/// frame type selection.
///
/// # Example
///
/// ```ignore
/// let config = SimpleEncoderConfig {
///     width: 1920,
///     height: 1080,
///     fps: 30,
///     codec: Codec::H264,
///     preset: Preset::Medium,
///     ..Default::default()
/// };
///
/// let mut enc = SimpleEncoder::new(config)?;
/// let header = enc.header();
///
/// for nv12_frame in frames {
///     let packet = enc.submit_frame(&nv12_frame)?;
///     output.write_all(&packet.data)?;
/// }
///
/// let trailing = enc.finish()?;
/// for pkt in trailing {
///     output.write_all(&pkt.data)?;
/// }
/// ```
pub struct SimpleEncoder {
    // Vulkan objects we own
    pub(crate) _entry: vulkanalia::Entry,
    pub(crate) _instance: vulkanalia::Instance,
    pub(crate) device: vulkanalia::Device,

    // --- Encoder fields (merged from former Encoder struct) ---
    pub(crate) ctx: Arc<VideoContext>,
    pub(crate) codec_flag: vk::VideoCodecOperationFlagsKHR,

    // Configuration (set during configure())
    pub(crate) encode_config: Option<EncodeConfig>,

    // Vulkan Video session
    pub(crate) video_session: vk::VideoSessionKHR,
    pub(crate) session_memory: Vec<vma::Allocation>,
    pub(crate) session_params: vk::VideoSessionParametersKHR,

    // DPB images (library-managed, device-local)
    pub(crate) dpb_image: vk::Image,
    pub(crate) dpb_allocation: vma::Allocation,
    pub(crate) dpb_separate_images: Vec<vk::Image>,
    pub(crate) dpb_separate_allocations: Vec<vma::Allocation>,
    pub(crate) dpb_slots: Vec<DpbSlot>,

    // Bitstream output buffer (host-visible for CPU readback)
    pub(crate) bitstream_buffer: vk::Buffer,
    pub(crate) bitstream_allocation: vma::Allocation,
    pub(crate) bitstream_buffer_size: usize,
    pub(crate) bitstream_mapped_ptr: *mut u8,

    // Command recording
    pub(crate) command_pool: vk::CommandPool,
    pub(crate) command_buffer: vk::CommandBuffer,

    // Query pool for encode feedback (offset + size)
    pub(crate) query_pool: vk::QueryPool,

    // Synchronization
    pub(crate) fence: vk::Fence,

    // Frame counters
    pub(crate) frame_count: u64,
    pub(crate) encode_order_count: u64,
    /// POC counter that resets on IDR frames (H.265 requirement).
    pub(crate) poc_counter: u64,

    // Track whether rate control has been sent
    pub(crate) rate_control_sent: bool,

    // Aligned width/height from driver capabilities
    pub(crate) aligned_width: u32,
    pub(crate) aligned_height: u32,

    // Track whether the session has been configured
    pub(crate) configured: bool,

    // Effective quality level (clamped to driver's max_quality_levels - 1)
    pub(crate) effective_quality_level: u32,

    // --- H.265-specific encoder ---
    pub(crate) h265_encoder: Option<Box<VkVideoEncoderH265>>,
    pub(crate) h265_config: Option<EncoderConfigH265>,

    // --- H.264-specific encoder ---
    pub(crate) h264_encoder: Option<Box<VkVideoEncoderH264>>,
    pub(crate) h264_config: Option<EncoderConfigH264>,

    // --- SimpleEncoder's own fields ---

    // Source image + staging buffer (reused across frames)
    pub(crate) source_image: vk::Image,
    pub(crate) source_view: vk::ImageView,
    pub(crate) source_allocation: vma::Allocation,
    pub(crate) staging_buffer: vk::Buffer,
    pub(crate) staging_allocation: vma::Allocation,
    pub(crate) staging_mapped_ptr: *mut u8,
    pub(crate) staging_size: usize,

    // Transfer command pool/buffer (for staging uploads + layout transitions)
    pub(crate) transfer_pool: vk::CommandPool,
    pub(crate) transfer_cb: vk::CommandBuffer,
    pub(crate) transfer_fence: vk::Fence,
    pub(crate) transfer_queue: vk::Queue,
    pub(crate) transfer_queue_family: u32,

    // Encode queue (may differ from transfer queue)
    pub(crate) encode_queue: vk::Queue,
    pub(crate) encode_queue_family: u32,

    // Compute queue for RGB→NV12 conversion
    pub(crate) compute_queue: vk::Queue,
    pub(crate) compute_queue_family: u32,

    // Lazy-initialized RGB→NV12 converter
    pub(crate) rgb_to_nv12: Option<crate::rgb_to_nv12::RgbToNv12Converter>,

    // GOP structure + state
    pub(crate) gop: VkVideoGopStructure,
    pub(crate) gop_state: GopState,
    pub(crate) frame_counter: u64,
    pub(crate) force_idr_flag: bool,

    // B-frame reorder buffer
    pub(crate) reorder_buffer: Vec<ReorderEntry>,

    // Cached header bytes (SPS/PPS or VPS/SPS/PPS)
    pub(crate) cached_header: Vec<u8>,

    // Config
    pub(crate) config: SimpleEncoderConfig,
    pub(crate) prepend_header: bool,

    // Host-side queue submission gateway (per-queue mutex synchronization).
    pub(crate) submitter: Arc<dyn crate::rhi::RhiQueueSubmitter>,
}

// SAFETY: Vulkan handles are only accessed through &mut self methods.
// The raw pointers (staging_mapped_ptr, bitstream_mapped_ptr) are only
unsafe impl Send for SimpleEncoder {}

impl SimpleEncoder {
    /// Create a new `SimpleEncoder` from the given configuration.
    ///
    /// This creates a Vulkan instance, selects a GPU with video encode
    /// support, creates a device with the required extensions, configures the
    /// video session and DPB, and allocates a staging buffer + source image.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The config is invalid
    /// - Vulkan cannot be loaded
    /// - No GPU supports video encode for the requested codec
    /// - Any Vulkan resource creation fails
    pub fn new(config: SimpleEncoderConfig) -> Result<Self, VideoError> {
        config.validate().map_err(|e| VideoError::BitstreamError(e))?;

        unsafe { Self::create_internal(config) }
    }

    /// Create an encoder using an externally-owned Vulkan device and allocator.
    ///
    /// Use this when integrating with a host application (e.g., streamlib RHI)
    /// that already owns the Vulkan device. No new instance/device is created —
    /// all GPU resources are allocated through the caller's VMA allocator.
    ///
    /// The caller must provide queue handles for encode, transfer, and compute
    /// operations. The device must have been created with the required video
    /// encode extensions enabled.
    pub fn from_device(
        config: SimpleEncoderConfig,
        instance: vulkanalia::Instance,
        device: vulkanalia::Device,
        physical_device: vk::PhysicalDevice,
        allocator: Arc<vma::Allocator>,
        submitter: Arc<dyn crate::rhi::RhiQueueSubmitter>,
        encode_queue: vk::Queue,
        encode_queue_family: u32,
        transfer_queue: vk::Queue,
        transfer_queue_family: u32,
        compute_queue: vk::Queue,
        compute_queue_family: u32,
    ) -> Result<Self, VideoError> {
        config.validate().map_err(|e| VideoError::BitstreamError(e))?;

        unsafe {
            Self::create_from_external(
                config, instance, device, physical_device, allocator, submitter,
                encode_queue, encode_queue_family,
                transfer_queue, transfer_queue_family,
                compute_queue, compute_queue_family,
            )
        }
    }

    /// Returns the cached SPS/PPS (H.264) or VPS/SPS/PPS (H.265) header
    /// bytes.  These should be written at the start of a file or sent before
    /// the first IDR in a live stream.
    pub fn header(&self) -> &[u8] {
        &self.cached_header
    }

    /// Force the next frame to be encoded as an IDR keyframe.
    ///
    /// Useful for live streaming when a new viewer connects and needs a
    /// clean decode entry point.
    pub fn force_idr(&mut self) {
        self.force_idr_flag = true;
    }

    // -- Device / queue / allocator accessors ---------------------------------
    //
    // These let callers (e.g. streamlib) create VkImages on the **same** device
    // so the image views can be passed to `encode_image()`.

    /// Returns a reference to the Vulkan device used by this encoder.
    pub fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    /// Returns the physical device selected during encoder construction.
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.ctx.physical_device()
    }

    /// Returns a shared reference to the VMA allocator used by this encoder.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        self.ctx.allocator()
    }

    /// Returns the transfer queue family index and queue handle.
    ///
    /// Callers can use these to upload RGBA pixels into a VkImage before
    /// passing its view to [`encode_image`](Self::encode_image).
    pub fn transfer_queue(&self) -> (u32, vk::Queue) {
        (self.transfer_queue_family, self.transfer_queue)
    }

    /// Returns the compute queue family index and queue handle.
    pub fn compute_queue(&self) -> (u32, vk::Queue) {
        (self.compute_queue_family, self.compute_queue)
    }

    /// Returns the aligned width and height used by the encode session.
    ///
    /// RGBA input images passed to `encode_image()` must have at least these
    /// dimensions.  The driver may round up from the config width/height to
    /// satisfy codec alignment requirements.
    pub fn aligned_extent(&self) -> (u32, u32) {
        (self.aligned_width, self.aligned_height)
    }

    /// Submit a raw NV12 frame for encoding.
    ///
    /// The `nv12_data` slice must contain exactly `width * height * 3 / 2`
    /// bytes in NV12 format (Y plane followed by interleaved UV plane).
    ///
    /// The encoder automatically selects the frame type (IDR, I, P) via the
    /// GOP structure.  Use [`force_idr`](Self::force_idr) to override.
    ///
    /// Returns an [`EncodePacket`] with the encoded bitstream data.  On the
    /// first IDR (or every IDR if `prepend_header_to_idr` is set), the
    /// SPS/PPS header is prepended to the data.
    pub fn submit_frame(
        &mut self,
        nv12_data: &[u8],
        timestamp_ns: Option<i64>,
    ) -> Result<Vec<EncodePacket>, VideoError> {
        let expected_size = (self.config.width * self.config.height * 3 / 2) as usize;
        if nv12_data.len() < expected_size {
            return Err(VideoError::BitstreamError(format!(
                "NV12 data too small: expected {} bytes, got {}",
                expected_size,
                nv12_data.len()
            )));
        }

        unsafe { self.submit_frame_reordered(nv12_data, timestamp_ns) }
    }

    /// Encode a GPU-resident RGBA image directly, without CPU staging.
    ///
    /// Runs the RGB→NV12 compute shader on the GPU, then feeds the NV12
    /// result to the video encoder. The input image must be in
    /// `SHADER_READ_ONLY_OPTIMAL` layout.
    ///
    /// The `RgbToNv12Converter` is lazily created on first call and reused
    /// for subsequent frames.
    ///
    /// # Arguments
    ///
    /// * `rgba_image_view` - An image view of the RGBA source image.
    ///   The underlying image must be in `SHADER_READ_ONLY_OPTIMAL` layout.
    pub fn encode_image(
        &mut self,
        rgba_image_view: vk::ImageView,
        timestamp_ns: Option<i64>,
    ) -> Result<Vec<EncodePacket>, VideoError> {
        unsafe { self.encode_image_internal(rgba_image_view, timestamp_ns) }
    }
}

impl Drop for SimpleEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();

            // Drop RGB→NV12 converter first (owns Vulkan objects).
            drop(self.rgb_to_nv12.take());

            // Destroy transfer resources
            if self.transfer_fence != vk::Fence::null() {
                self.device.destroy_fence(self.transfer_fence, None);
            }
            if self.transfer_pool != vk::CommandPool::null() {
                self.device.destroy_command_pool(self.transfer_pool, None);
            }

            let allocator = self.ctx.allocator();

            // Destroy staging buffer + allocation (VMA handles unmap)
            if self.staging_buffer != vk::Buffer::null() {
                allocator.destroy_buffer(self.staging_buffer, self.staging_allocation);
                self.staging_mapped_ptr = ptr::null_mut();
            }

            // Destroy source image view, then image + allocation
            if self.source_view != vk::ImageView::null() {
                self.device
                    .destroy_image_view(self.source_view, None);
            }
            if self.source_image != vk::Image::null() {
                allocator.destroy_image(self.source_image, self.source_allocation);
            }

            // --- Encoder resource cleanup (merged from former Encoder Drop) ---
            if self.configured {
                // Destroy fence
                if self.fence != vk::Fence::null() {
                    self.device.destroy_fence(self.fence, None);
                }

                // Destroy query pool
                if self.query_pool != vk::QueryPool::null() {
                    self.device.destroy_query_pool(self.query_pool, None);
                }

                // Destroy command pool (frees command buffers)
                if self.command_pool != vk::CommandPool::null() {
                    self.device.destroy_command_pool(self.command_pool, None);
                }

                // Destroy bitstream buffer + allocation (VMA handles unmap)
                if self.bitstream_buffer != vk::Buffer::null() {
                    allocator.destroy_buffer(self.bitstream_buffer, self.bitstream_allocation);
                    self.bitstream_mapped_ptr = ptr::null_mut();
                }

                // Destroy DPB image views (per-slot) and images
                for slot in &self.dpb_slots {
                    if slot.view != vk::ImageView::null() {
                        self.device.destroy_image_view(slot.view, None);
                    }
                }
                // Destroy separate per-slot DPB images (if used)
                for (img, alloc) in self.dpb_separate_images.iter()
                    .zip(self.dpb_separate_allocations.iter())
                {
                    if *img != vk::Image::null() {
                        allocator.destroy_image(*img, *alloc);
                    }
                }
                // Destroy shared DPB image (if used, non-null when array layers mode)
                if self.dpb_image != vk::Image::null() {
                    allocator.destroy_image(self.dpb_image, self.dpb_allocation);
                }

                // Destroy session parameters
                if self.session_params != vk::VideoSessionParametersKHR::null() {
                    self.device.destroy_video_session_parameters_khr(self.session_params, None);
                }

                // Destroy video session
                if self.video_session != vk::VideoSessionKHR::null() {
                    self.device.destroy_video_session_khr(self.video_session, None);
                }

                // Free session memory allocations
                for alloc in &self.session_memory {
                    allocator.free_memory(*alloc);
                }
            }

            // NOTE: device and instance are dropped via Drop impls on the
            // cloned types inside VideoContext (which is Arc'd).
            // We must NOT double-destroy them here.
        }
    }
}
