// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! High-level JPEG decoder API. Wraps a [`JpegDecodeBackend`] and
//! selects between [`VulkanComputeBackend`] (cross-vendor, default) and
//! [`NvJpegBackend`] (NVIDIA fast path) at construction time based on
//! [`HostVulkanDevice::third_party_gpu_capabilities`] and an optional
//! caller-supplied [`JpegBackendPreference`].
//!
//! Runtime selection rule: nvJPEG is preferred when (a) the device's
//! `ThirdPartyGpuCapabilities::nvjpeg` is `true` and (b)
//! [`NvJpegBackend::new`] succeeds. Construction falls back to
//! [`VulkanComputeBackend`] otherwise. Callers can force a specific
//! backend via [`JpegBackendPreference::Force`] — useful for testing,
//! benchmarking, and consumers who want determinism across hosts.
//!
//! [`VulkanComputeBackend`]: crate::vulkan_compute_backend::VulkanComputeBackend
//! [`NvJpegBackend`]: crate::nvjpeg_backend::NvJpegBackend
//! [`HostVulkanDevice::third_party_gpu_capabilities`]:
//!   streamlib::sdk::engine::host_rhi::HostVulkanDevice::third_party_gpu_capabilities

use streamlib::sdk::color::ResolvedColorInfo;
use streamlib::sdk::context::GpuContextFullAccess;
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::Texture;

use crate::backend::{JpegBackendKind, JpegDecodeBackend};
use crate::color::JpegColorSource;
use crate::vulkan_compute_backend::VulkanComputeBackend;

#[cfg(target_os = "linux")]
use crate::nvjpeg_backend::NvJpegBackend;

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

/// Backend-selection preference passed to [`SimpleJpegDecoder::new`]
/// alongside the device.
///
/// [`Auto`](Self::Auto) is the recommended default: nvJPEG when
/// available, Vulkan-compute otherwise. The escape hatches are for
/// testing, benchmarking, and consumers that need determinism across
/// hosts.
#[derive(Debug, Clone, Copy)]
pub enum JpegBackendPreference {
    /// Use nvJPEG when the device's
    /// [`ThirdPartyGpuCapabilities::nvjpeg`] is `true` and
    /// [`NvJpegBackend::new`] succeeds. Fall back to the
    /// cross-vendor Vulkan-compute backend otherwise.
    ///
    /// [`ThirdPartyGpuCapabilities::nvjpeg`]:
    ///   streamlib::sdk::engine::host_rhi::ThirdPartyGpuCapabilities::nvjpeg
    /// [`NvJpegBackend::new`]: crate::nvjpeg_backend::NvJpegBackend::new
    Auto,
    /// Force a specific backend. `new` returns
    /// [`Error::NotSupported`] when the forced backend isn't
    /// available (e.g. `Force(NvJpeg)` on a non-NVIDIA host or one
    /// without `libnvjpeg.so.12`).
    Force(JpegBackendKind),
}

impl Default for JpegBackendPreference {
    fn default() -> Self {
        Self::Auto
    }
}

/// High-level fused-compute JPEG decoder.
///
/// Wraps a single [`JpegDecodeBackend`] chosen at construction time per
/// [`JpegBackendPreference`]. Per-frame [`Self::decode`] forwards to the
/// backend — no decision overhead, no hot-path allocation.
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
    /// `(max_width, max_height)` pixels, with auto-selected backend.
    /// Equivalent to [`Self::new_with_preference`] with
    /// [`JpegBackendPreference::Auto`].
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
        Self::new_with_preference(
            full_access,
            max_width,
            max_height,
            JpegBackendPreference::Auto,
        )
    }

    /// Construct a decoder with explicit backend preference.
    ///
    /// - [`JpegBackendPreference::Auto`] picks nvJPEG when the device's
    ///   [`ThirdPartyGpuCapabilities::nvjpeg`] reports `true` and
    ///   nvJPEG construction succeeds; falls back to Vulkan-compute
    ///   otherwise. A failed nvJPEG construction logs at
    ///   `tracing::warn!` level and the decoder transparently uses
    ///   Vulkan-compute.
    /// - [`JpegBackendPreference::Force`] requires the specified backend
    ///   to construct successfully. Returns
    ///   [`Error::NotSupported`] otherwise. Useful for testing,
    ///   benchmarking, and consumers that need determinism across
    ///   hosts.
    ///
    /// [`ThirdPartyGpuCapabilities::nvjpeg`]:
    ///   streamlib::sdk::engine::host_rhi::ThirdPartyGpuCapabilities::nvjpeg
    pub fn new_with_preference(
        full_access: &GpuContextFullAccess,
        max_width: u32,
        max_height: u32,
        preference: JpegBackendPreference,
    ) -> Result<Self> {
        let backend = build_backend(full_access, max_width, max_height, preference)?;
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
    /// a handle to it. Forwards to the selected backend — see the
    /// backend's documentation for backend-specific behavior.
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

#[cfg(target_os = "linux")]
fn build_backend(
    full_access: &GpuContextFullAccess,
    max_width: u32,
    max_height: u32,
    preference: JpegBackendPreference,
) -> Result<Box<dyn JpegDecodeBackend>> {
    let caps = full_access
        .device()
        .vulkan_device()
        .third_party_gpu_capabilities();

    match preference {
        JpegBackendPreference::Force(JpegBackendKind::VulkanCompute) => {
            VulkanComputeBackend::new(full_access, max_width, max_height)
                .map(|b| Box::new(b) as Box<dyn JpegDecodeBackend>)
        }
        JpegBackendPreference::Force(JpegBackendKind::NvJpeg) => {
            if !caps.nvjpeg {
                return Err(Error::NotSupported(
                    "JpegBackendPreference::Force(NvJpeg): nvJPEG not available on this device \
                     (ThirdPartyGpuCapabilities::nvjpeg = false)"
                        .into(),
                ));
            }
            NvJpegBackend::new(full_access, max_width, max_height)
                .map(|b| Box::new(b) as Box<dyn JpegDecodeBackend>)
        }
        JpegBackendPreference::Auto => {
            if caps.nvjpeg {
                match NvJpegBackend::new(full_access, max_width, max_height) {
                    Ok(b) => return Ok(Box::new(b)),
                    Err(e) => {
                        tracing::warn!(
                            target: "vulkan_jpeg::backend_selection",
                            error = %e,
                            "nvJPEG construction failed; falling back to Vulkan-compute backend",
                        );
                    }
                }
            }
            VulkanComputeBackend::new(full_access, max_width, max_height)
                .map(|b| Box::new(b) as Box<dyn JpegDecodeBackend>)
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn build_backend(
    full_access: &GpuContextFullAccess,
    max_width: u32,
    max_height: u32,
    preference: JpegBackendPreference,
) -> Result<Box<dyn JpegDecodeBackend>> {
    // nvJPEG ships only for Linux; on other platforms the only available
    // backend is Vulkan-compute.
    match preference {
        JpegBackendPreference::Force(JpegBackendKind::NvJpeg) => Err(Error::NotSupported(
            "JpegBackendPreference::Force(NvJpeg): nvJPEG backend is Linux-only".into(),
        )),
        _ => VulkanComputeBackend::new(full_access, max_width, max_height)
            .map(|b| Box::new(b) as Box<dyn JpegDecodeBackend>),
    }
}
