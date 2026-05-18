// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-vendor [`JpegDecodeBackend`] implementation: host CPU parser +
//! Huffman entropy decode → fused Vulkan compute kernel (dequant + IDCT +
//! chroma upsample + YCbCr→RGB), writing directly into a non-exportable
//! DEVICE_LOCAL [`TextureRing`] of `Rgba8Unorm` STORAGE textures.
//!
//! Construction is privileged (allocates the kernel pipeline, the
//! coefficient + quant-table HOST_VISIBLE SSBOs sized for the worst-case
//! 4:2:0 frame at the declared maxima, and the ring). Per-frame
//! [`JpegDecodeBackend::decode`] rotates the ring and reuses the SSBOs —
//! zero `vkAllocateMemory`, zero texture creation, zero command-pool /
//! cb / fence creation in steady state.
//!
//! Default backend selected by [`crate::simple_decoder::SimpleJpegDecoder`]
//! when nvJPEG isn't available or isn't preferred.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use streamlib::sdk::context::{GpuContextFullAccess, TextureRing};
use streamlib::sdk::engine::host_rhi::{
    HostVulkanBuffer, RhiCommandRecorder, VulkanAccess, VulkanStage,
};
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{TextureFormat, TextureUsages, VulkanLayout};

use crate::backend::{JpegBackendKind, JpegDecodeBackend};
use crate::color::JpegColorInfo;
use crate::kernel::{
    worst_case_coefficient_buffer_bytes_420, JpegDecodeKernel, QUANT_TABLE_BUFFER_BYTES,
};
use crate::simple_decoder::{JpegDecodeOutput, MAX_FRAMES_IN_FLIGHT};
use crate::JpegColorSource;

/// Cross-vendor Vulkan-compute JPEG decode backend.
pub struct VulkanComputeBackend {
    kernel: JpegDecodeKernel,
    coef_buf: Arc<HostVulkanBuffer>,
    qt_buf: Arc<HostVulkanBuffer>,
    ring: Arc<TextureRing>,
    max_width: u32,
    max_height: u32,
    /// Latch for the "non-sRGB declaration → JFIF fallback" warning.
    /// Flips to `true` on the first frame that takes the fallback;
    /// subsequent fallback frames stay silent so a 30 Hz pipeline
    /// doesn't flood the log. Resetting to `false` re-arms the warn
    /// (e.g. across a teardown / rebuild cycle).
    unsupported_fallback_warned: AtomicBool,
}

impl std::fmt::Debug for VulkanComputeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanComputeBackend")
            .field("max_width", &self.max_width)
            .field("max_height", &self.max_height)
            .field("ring_slots", &self.ring.len())
            .finish()
    }
}

impl VulkanComputeBackend {
    /// Build a Vulkan-compute backend ready to handle JPEGs up to
    /// `(max_width, max_height)` pixels.
    ///
    /// Pre-allocates the fused compute kernel pipeline, a coefficient
    /// SSBO sized for the worst-case 4:2:0 frame at the declared maxima,
    /// a 128-entry quant-table SSBO, and a [`MAX_FRAMES_IN_FLIGHT`]-slot
    /// [`TextureRing`] of `Rgba8Unorm` STORAGE textures. Each ring slot
    /// is eagerly transitioned `UNDEFINED → GENERAL` and the slot's
    /// registration layout updated to match — subsequent kernel dispatches
    /// write directly without further transitions, and downstream
    /// consumers that resolve the slot's `surface_id` see a registration
    /// claim that matches reality.
    pub fn new(
        full_access: &GpuContextFullAccess,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self> {
        if max_width == 0 || max_height == 0 {
            return Err(Error::GpuError(format!(
                "VulkanComputeBackend::new: max dimensions must be non-zero, got {}x{}",
                max_width, max_height,
            )));
        }

        let device = full_access.device().vulkan_device();
        let kernel = JpegDecodeKernel::new(device)?;

        let coef_bytes = worst_case_coefficient_buffer_bytes_420(max_width, max_height);
        let coef_buf = Arc::new(HostVulkanBuffer::new_storage_buffer_host_visible(
            device, coef_bytes,
        )?);
        let qt_buf = Arc::new(HostVulkanBuffer::new_storage_buffer_host_visible(
            device,
            QUANT_TABLE_BUFFER_BYTES,
        )?);

        let ring = full_access.create_texture_ring(
            max_width,
            max_height,
            TextureFormat::Rgba8Unorm,
            TextureUsages::STORAGE_BINDING
                | TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::TEXTURE_BINDING,
            MAX_FRAMES_IN_FLIGHT,
        )?;

        // Eagerly transition every slot UNDEFINED → GENERAL and refresh
        // its TextureRegistration to match. The kernel writes via
        // STORAGE_BINDING in GENERAL and never transitions back; this
        // pre-warm means per-frame `decode()` records no transitions.
        for slot_index in 0..ring.len() {
            let slot = ring
                .slot(slot_index)
                .ok_or_else(|| Error::GpuError("ring slot index out of range".into()))?;
            let mut recorder =
                RhiCommandRecorder::new(device, "vulkan_compute_backend_init_layout")?;
            recorder.begin()?;
            recorder.record_image_barrier(
                &slot.texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::GENERAL,
                VulkanStage::TOP_OF_PIPE,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::NONE,
                VulkanAccess::SHADER_WRITE,
            )?;
            recorder.submit_and_wait()?;
            full_access
                .update_texture_registration_layout(&slot.surface_id, VulkanLayout::GENERAL);
        }

        Ok(Self {
            kernel,
            coef_buf,
            qt_buf,
            ring,
            max_width,
            max_height,
            unsupported_fallback_warned: AtomicBool::new(false),
        })
    }

    /// Borrow the underlying [`TextureRing`] — for tests and
    /// introspection.
    #[doc(hidden)]
    pub fn ring(&self) -> &Arc<TextureRing> {
        &self.ring
    }

    /// Emit a structured `tracing::warn!` describing the first frame
    /// where this backend's input declared a non-sRGB colorimetry that
    /// the engine can't honor yet. Subsequent fallback frames stay
    /// silent — the latch fires once per backend instance.
    ///
    /// The warn uses the stable target `vulkan_jpeg::colorimetry_fallback`
    /// so an AGP-style runtime can `grep`/filter for it cleanly. Carries
    /// the parsed APP-segment fields so the operator can tell which
    /// declaration tripped the fallback.
    fn warn_unsupported_fallback_once(&self, info: &JpegColorInfo) {
        if self
            .unsupported_fallback_warned
            .swap(true, Ordering::Relaxed)
        {
            return;
        }
        tracing::warn!(
            target: "vulkan_jpeg::colorimetry_fallback",
            adobe_transform = ?info.adobe.map(|a| a.transform),
            exif_color_space = ?info.exif_color_space,
            icc_profile_bytes = info.icc_profile.as_ref().map(|p| p.len()),
            "JPEG stream declares non-sRGB colorimetry the engine `PrimariesId` enum can't represent yet (Adobe RGB / Display P3 / Rec.2020); decoded under the JFIF default. Extend `PrimariesId` to honor it.",
        );
    }
}

impl JpegDecodeBackend for VulkanComputeBackend {
    fn decode(&mut self, jpeg_bytes: &[u8]) -> Result<JpegDecodeOutput> {
        let decoded = crate::decode(jpeg_bytes)
            .map_err(|e| Error::GpuError(format!("jpeg parse/huffman: {e}")))?;

        let width = u32::from(decoded.frame.width);
        let height = u32::from(decoded.frame.height);
        if width > self.max_width || height > self.max_height {
            return Err(Error::GpuError(format!(
                "VulkanComputeBackend::decode: frame {}x{} exceeds decoder maxima {}x{} \
                 (rebuild SimpleJpegDecoder with larger max_width/max_height)",
                width, height, self.max_width, self.max_height,
            )));
        }

        // Honor declared colorimetry from APP segments. APP14 transform=2
        // (YCCK) bubbles up as `Error::NotSupported` here — the kernel only
        // handles 3-component YCbCr / RGB-direct decode. EXIF / ICC
        // declarations the engine can't yet represent fall back to JFIF
        // default and surface as `JpegColorSource::UnsupportedDeclarationFallback`
        // on the output, plus a one-shot `tracing::warn!` per backend
        // instance.
        let resolved = decoded
            .color_info
            .resolve()
            .map_err(|e| Error::GpuError(format!("jpeg colorimetry: {e}")))?;

        if resolved.source == JpegColorSource::UnsupportedDeclarationFallback {
            self.warn_unsupported_fallback_once(&decoded.color_info);
        }

        let slot = self.ring.acquire_next();
        self.kernel.dispatch_pooled(
            &decoded,
            &slot.texture,
            &self.coef_buf,
            &self.qt_buf,
            &resolved.info,
        )?;

        Ok(JpegDecodeOutput {
            texture: slot.texture,
            surface_id: slot.surface_id,
            width,
            height,
            color_source: resolved.source,
            color_info: resolved.info,
        })
    }

    fn kind(&self) -> JpegBackendKind {
        JpegBackendKind::VulkanCompute
    }
}
