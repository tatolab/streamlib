// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! High-level JPEG decoder API: parser + Huffman + fused compute kernel +
//! pre-allocated HOST_VISIBLE SSBOs + pre-allocated [`TextureRing`] of
//! `MAX_FRAMES_IN_FLIGHT = 2` output textures.
//!
//! Construction is privileged (allocates the kernel pipeline, the
//! coefficient + quant-table HOST_VISIBLE SSBOs sized for the worst-case
//! 4:2:0 frame at `(max_width, max_height)`, and a `TextureRing` of
//! `Rgba8Unorm` STORAGE textures). Per-frame [`Self::decode`] rotates
//! the ring and reuses the SSBOs — zero `vkAllocateMemory`, zero
//! texture creation, zero command-pool / cb / fence creation in steady
//! state.

use std::sync::Arc;

use streamlib::sdk::context::{GpuContextFullAccess, TextureRing};
use streamlib::sdk::engine::host_rhi::{
    HostVulkanBuffer, RhiCommandRecorder, VulkanAccess, VulkanStage,
};
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{Texture, TextureFormat, TextureUsages, VulkanLayout};

use crate::kernel::{
    worst_case_coefficient_buffer_bytes_420, JpegDecodeKernel, QUANT_TABLE_BUFFER_BYTES,
};

/// Ring depth — one frame in flight on the GPU, one being recorded on
/// the CPU. Matches every decoder in the engine.
pub const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// Output produced by [`SimpleJpegDecoder::decode`]: the GPU texture the
/// decode wrote into, its `surface_id` (registered in
/// [`streamlib::sdk::context::GpuContext`]'s texture cache so downstream
/// in-process consumers can resolve it), and the decoded frame's actual
/// pixel dimensions.
#[derive(Debug, Clone)]
pub struct JpegDecodeOutput {
    /// Output texture handle (`Arc<HostVulkanTexture>` under the hood —
    /// cheap to clone, safe to ship across threads).
    pub texture: Texture,
    /// Same-process texture-cache id for this slot. Stable across
    /// decodes against the same slot; rotates with the ring.
    pub surface_id: String,
    /// Decoded frame width, in pixels. Always ≤ `max_width`.
    pub width: u32,
    /// Decoded frame height, in pixels. Always ≤ `max_height`.
    pub height: u32,
}

/// High-level fused-compute JPEG decoder with pre-allocated GPU
/// resources. Drops in steady-state allocation on the per-frame hot
/// path.
pub struct SimpleJpegDecoder {
    kernel: JpegDecodeKernel,
    coef_buf: Arc<HostVulkanBuffer>,
    qt_buf: Arc<HostVulkanBuffer>,
    ring: Arc<TextureRing>,
    max_width: u32,
    max_height: u32,
}

impl std::fmt::Debug for SimpleJpegDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleJpegDecoder")
            .field("max_width", &self.max_width)
            .field("max_height", &self.max_height)
            .field("ring_slots", &self.ring.len())
            .finish()
    }
}

impl SimpleJpegDecoder {
    /// Construct a decoder ready to handle JPEGs up to
    /// `(max_width, max_height)` pixels.
    ///
    /// Pre-allocates the fused compute kernel pipeline, a coefficient
    /// SSBO sized for the worst-case 4:2:0 frame at the declared maxima,
    /// a 128-entry quant-table SSBO, and a [`MAX_FRAMES_IN_FLIGHT`]-slot
    /// [`TextureRing`] of `Rgba8Unorm` STORAGE textures. Each ring slot
    /// is eagerly transitioned `UNDEFINED → GENERAL` and the slot's
    /// registration layout updated to match — subsequent kernel
    /// dispatches write directly without further transitions, and
    /// downstream consumers that resolve the slot's `surface_id` see a
    /// registration claim that matches reality.
    ///
    /// Must be called inside a [`streamlib::sdk::context::GpuContext::escalate`]
    /// closure (the only way crate-external code obtains a
    /// `&GpuContextFullAccess`). The processor-setup mutex is held for
    /// the duration of `new`, serializing this work against any other
    /// concurrent processor-setup-class allocation in the runtime.
    pub fn new(
        full_access: &GpuContextFullAccess,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self> {
        if max_width == 0 || max_height == 0 {
            return Err(Error::GpuError(format!(
                "SimpleJpegDecoder::new: max dimensions must be non-zero, got {}x{}",
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
                RhiCommandRecorder::new(device, "simple_jpeg_decoder_init_layout")?;
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
        })
    }

    /// Decode `jpeg_bytes` into the next ring slot's texture and return
    /// a handle to it. Steady-state Limited-safe — no escalation, no
    /// allocation. Single-threaded by `&mut self`; multi-decoder
    /// parallelism is the caller's pattern.
    ///
    /// Rejects baseline-but-non-4:2:0 input ([`Error::NotSupported`]
    /// from the kernel) and frames whose dimensions exceed the decoder's
    /// declared maxima ([`Error::GpuError`] with "exceeds max"). Parser
    /// / Huffman / kernel errors surface as their typed variants — no
    /// panics, no leaked GPU resources.
    pub fn decode(&mut self, jpeg_bytes: &[u8]) -> Result<JpegDecodeOutput> {
        let decoded = crate::decode(jpeg_bytes)
            .map_err(|e| Error::GpuError(format!("jpeg parse/huffman: {e}")))?;

        let width = u32::from(decoded.frame.width);
        let height = u32::from(decoded.frame.height);
        if width > self.max_width || height > self.max_height {
            return Err(Error::GpuError(format!(
                "SimpleJpegDecoder::decode: frame {}x{} exceeds decoder maxima {}x{} \
                 (rebuild SimpleJpegDecoder with larger max_width/max_height)",
                width, height, self.max_width, self.max_height,
            )));
        }

        // Honor declared colorimetry from APP segments. APP14 transform=2
        // (YCCK) bubbles up as `Error::NotSupported` here — the kernel only
        // handles 3-component YCbCr / RGB-direct decode. EXIF / ICC
        // declarations the engine can't yet represent fall back to JFIF
        // default with a tracing::warn from inside `resolve()`.
        let resolved = decoded
            .color_info
            .resolve()
            .map_err(|e| Error::GpuError(format!("jpeg colorimetry: {e}")))?;

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
        })
    }

    /// Borrow the underlying [`TextureRing`] — for tests and
    /// introspection. Production callers consume slots via
    /// [`Self::decode`].
    #[doc(hidden)]
    pub fn ring(&self) -> &Arc<TextureRing> {
        &self.ring
    }

    /// Declared maximum width this decoder can handle, in pixels.
    pub fn max_width(&self) -> u32 {
        self.max_width
    }

    /// Declared maximum height this decoder can handle, in pixels.
    pub fn max_height(&self) -> u32 {
        self.max_height
    }
}
