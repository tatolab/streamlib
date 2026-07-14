// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! High-level JPEG decoder API. Wraps a [`JpegDecodeBackend`] — today the
//! single cross-vendor [`VulkanComputeBackend`] — built at construction
//! time through the engine-free FullAccess primitives.
//!
//! [`VulkanComputeBackend`]: crate::vulkan_compute_backend::VulkanComputeBackend

use streamlib_plugin_sdk::sdk::color::ResolvedColorInfo;
use streamlib_plugin_sdk::sdk::context::GpuContextFullAccess;
use streamlib_plugin_sdk::sdk::error::Result;
use streamlib_plugin_sdk::sdk::rhi::Texture;

use crate::backend::{JpegBackendKind, JpegDecodeBackend};
use crate::color::JpegColorSource;
use crate::vulkan_compute_backend::VulkanComputeBackend;

/// Ring depth — one frame in flight on the GPU, one being recorded on
/// the CPU. Matches every decoder in the engine.
pub const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// Output produced by [`SimpleJpegDecoder::decode`]: the GPU texture the
/// decode wrote into, its `surface_id` (registered in
/// [`streamlib_plugin_sdk::sdk::context::GpuContextFullAccess`]'s texture
/// cache so downstream in-process consumers can resolve it), and the
/// decoded frame's actual pixel dimensions.
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
    /// Which APP-segment declaration drove the colorimetry decision
    /// for this frame. `UnsupportedDeclarationFallback` means the
    /// bitstream declared a colorimetry the engine can't represent
    /// yet (Adobe RGB / Display P3 / Rec.2020 via EXIF or ICC) and
    /// the backend decoded under the JFIF default instead.
    pub color_source: JpegColorSource,
    /// Resolved 4-tuple the backend actually used for this frame.
    /// Cheap to compare; surfaces the dispatched matrix / range /
    /// transfer / primaries for downstream consumers that want to
    /// reason about the color decision per frame.
    pub color_info: ResolvedColorInfo,
}

/// High-level fused-compute JPEG decoder.
///
/// Wraps a single [`JpegDecodeBackend`] (the cross-vendor
/// [`VulkanComputeBackend`]) built at construction time. Per-frame
/// [`Self::decode`] forwards to the backend — no decision overhead, no
/// hot-path allocation.
pub struct SimpleJpegDecoder {
    backend: Box<dyn JpegDecodeBackend>,
    max_width: u32,
    max_height: u32,
}

impl std::fmt::Debug for SimpleJpegDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleJpegDecoder")
            .field("backend", &self.backend.kind().as_str())
            .field("max_width", &self.max_width)
            .field("max_height", &self.max_height)
            .finish()
    }
}

impl SimpleJpegDecoder {
    /// Construct a decoder ready to handle JPEGs up to
    /// `(max_width, max_height)` pixels.
    ///
    /// Must be called inside a
    /// [`streamlib_plugin_sdk::sdk::context::GpuContextFullAccess`] scope
    /// (the only way crate-external code obtains a `&GpuContextFullAccess`).
    /// The processor-setup mutex is held for the duration of `new`,
    /// serializing this work against any other concurrent
    /// processor-setup-class allocation in the runtime.
    ///
    /// The backend is the cross-vendor [`VulkanComputeBackend`], built
    /// entirely through the FullAccess primitives — this decoder never
    /// touches the raw `HostVulkanDevice`, so it stays sound when compiled
    /// into a separately-built `.slpkg` plugin.
    pub fn new(
        full_access: &GpuContextFullAccess,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self> {
        let backend: Box<dyn JpegDecodeBackend> = Box::new(VulkanComputeBackend::new(
            full_access,
            max_width,
            max_height,
        )?);
        tracing::debug!(
            target: "vulkan_jpeg::backend_selection",
            backend = backend.kind().as_str(),
            max_width,
            max_height,
            "SimpleJpegDecoder constructed",
        );
        Ok(Self {
            backend,
            max_width,
            max_height,
        })
    }

    /// Decode `jpeg_bytes` into the next ring slot's texture and return
    /// a handle to it. Forwards to the backend — see the backend's
    /// documentation for backend-specific behavior.
    pub fn decode(&mut self, jpeg_bytes: &[u8]) -> Result<JpegDecodeOutput> {
        self.backend.decode(jpeg_bytes)
    }

    /// The backend this decoder selected at construction.
    pub fn backend_kind(&self) -> JpegBackendKind {
        self.backend.kind()
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
