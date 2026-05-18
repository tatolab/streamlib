// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Backend dispatch for [`SimpleJpegDecoder`].
//!
//! Two implementations ship today:
//!
//! - [`VulkanComputeBackend`] — cross-vendor Vulkan compute kernel, host CPU
//!   parser + Huffman entropy decode → fused dequant + IDCT + chroma upsample
//!   + YCbCr→RGB compute kernel. Always available on any Vulkan device.
//! - [`NvJpegBackend`] — NVIDIA `libnvjpeg` (CUDA-based JPEG decode). Used
//!   automatically when [`HostVulkanDevice::third_party_gpu_capabilities`]
//!   reports `nvjpeg = true` and the backend's own initialization succeeds.
//!
//! **If you're integrating a non-JPEG GPU library that follows the same
//! engine-allocates / vendor-imports shape (NVDEC for H.264/H.265, AMD AMF,
//! Intel MFX, OptiX denoiser): do not copy this trait shape per-library.**
//! See `docs/architecture/third-party-gpu-backends.md` — the second
//! backend-using library is the trigger to lift this JPEG-specific trait
//! to an engine-tier `ThirdPartyGpuBackend` primitive.
//!
//! [`HostVulkanDevice::third_party_gpu_capabilities`]:
//!   streamlib::sdk::engine::host_rhi::HostVulkanDevice::third_party_gpu_capabilities
//! [`VulkanComputeBackend`]: crate::vulkan_compute_backend::VulkanComputeBackend
//! [`NvJpegBackend`]: crate::nvjpeg_backend::NvJpegBackend

use streamlib::sdk::error::Result;

use crate::simple_decoder::JpegDecodeOutput;

/// Stable identifier for a JPEG-decode backend implementation. Used by
/// callers that want to force a specific backend via
/// [`crate::simple_decoder::JpegBackendPreference::Force`], by tracing
/// instrumentation that wants to label which backend produced a frame,
/// and by callers preflighting available backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JpegBackendKind {
    /// Cross-vendor Vulkan compute kernel — host CPU parser + Huffman
    /// entropy decode → fused dequant + IDCT + chroma upsample +
    /// YCbCr→RGB compute kernel. Always available on any Vulkan device.
    VulkanCompute,
    /// NVIDIA `libnvjpeg` (CUDA-based JPEG decode). Available on NVIDIA
    /// hardware when `libnvjpeg.so.12` is loadable and the backend's
    /// own initialization succeeds.
    NvJpeg,
}

impl JpegBackendKind {
    /// Human-readable label, used in tracing and error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            JpegBackendKind::VulkanCompute => "vulkan-compute",
            JpegBackendKind::NvJpeg => "nvjpeg",
        }
    }
}

/// JPEG-decode backend contract — `&[u8] → JpegDecodeOutput`. The trait
/// sits above the host CPU parser + Huffman stage because nvJPEG does its
/// own entropy decode on the GPU and bypasses the
/// host-parser-then-dispatch-compute shape of the Vulkan-compute backend.
///
/// Implementations own all backend-specific pre-allocated state
/// (compute pipeline + SSBOs for Vulkan-compute; CUDA context + OPAQUE_FD
/// staging buffer + nvJPEG handle for nvJPEG) and a [`TextureRing`] of
/// output texture slots that `decode` rotates through. Per-call work is
/// Limited-safe — no `vkAllocateMemory`, no `cudaMalloc`, no pipeline /
/// fence creation in steady state.
///
/// [`TextureRing`]: streamlib::sdk::context::TextureRing
pub trait JpegDecodeBackend: Send + std::fmt::Debug {
    /// Decode `jpeg_bytes` into the backend's next output slot. Returns
    /// the slot's [`JpegDecodeOutput`] (texture + surface_id + dimensions
    /// + resolved colorimetry) so downstream consumers can pick it up
    /// through the normal `surface_id` contract.
    ///
    /// Errors:
    /// - [`streamlib::sdk::error::Error::GpuError`] for parser / Huffman
    ///   failures, dimension overflows past the backend's declared maxima,
    ///   colorimetry-resolution failures, and backend-specific GPU /
    ///   vendor-SDK failures.
    /// - [`streamlib::sdk::error::Error::NotSupported`] for inputs the
    ///   backend cannot handle (e.g. non-4:2:0 sampling on the
    ///   Vulkan-compute backend; progressive JPEGs on nvJPEG when the
    ///   subsystem isn't enabled).
    fn decode(&mut self, jpeg_bytes: &[u8]) -> Result<JpegDecodeOutput>;

    /// Backend identity. Stable across the backend's lifetime; used by
    /// tracing and callers that want to confirm which backend
    /// [`crate::simple_decoder::SimpleJpegDecoder`] selected.
    fn kind(&self) -> JpegBackendKind;
}
