// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Backend dispatch for [`SimpleJpegDecoder`].
//!
//! One implementation ships here:
//!
//! - [`VulkanComputeBackend`] — cross-vendor Vulkan compute kernel, host CPU
//!   parser + Huffman entropy decode → fused dequant + IDCT + chroma upsample
//!   + YCbCr→RGB compute kernel. Always available on any Vulkan device, and
//!   built entirely through the engine-free FullAccess primitives so this
//!   crate stays safe to compile into a `.slpkg` cdylib.
//!
//! The NVIDIA `libnvjpeg` (CUDA) backend that previously shipped behind this
//! trait reaches the raw `HostVulkanDevice` / OPAQUE_FD export path, which is
//! engine-internal and has no cdylib-safe FullAccess primitive yet. It was
//! split out into the engine (parked, disabled) during the plugin-SDK
//! extraction; re-integration + cdylib-safe plugin exposure is tracked
//! separately. The trait is retained as a single-implementation seam so
//! tracing / introspection callers keep a stable `kind()` label and a future
//! backend can slot back in without reshaping the decoder.
//!
//! [`SimpleJpegDecoder`]: crate::simple_decoder::SimpleJpegDecoder
//! [`VulkanComputeBackend`]: crate::vulkan_compute_backend::VulkanComputeBackend

use streamlib_plugin_sdk::sdk::error::Result;

use crate::simple_decoder::JpegDecodeOutput;

/// Stable identifier for a JPEG-decode backend implementation. Used by
/// tracing instrumentation that wants to label which backend produced a
/// frame and by callers that want to confirm which backend
/// [`crate::simple_decoder::SimpleJpegDecoder`] selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JpegBackendKind {
    /// Cross-vendor Vulkan compute kernel — host CPU parser + Huffman
    /// entropy decode → fused dequant + IDCT + chroma upsample +
    /// YCbCr→RGB compute kernel. Always available on any Vulkan device.
    VulkanCompute,
}

impl JpegBackendKind {
    /// Human-readable label, used in tracing and error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            JpegBackendKind::VulkanCompute => "vulkan-compute",
        }
    }
}

/// JPEG-decode backend contract — `&[u8] → JpegDecodeOutput`. The trait
/// sits above the host CPU parser + Huffman stage so a future GPU-native
/// entropy-decode backend can slot in without reshaping the decoder.
///
/// Implementations own all backend-specific pre-allocated state (compute
/// pipeline + SSBOs for Vulkan-compute) and a [`TextureRing`] of output
/// texture slots that `decode` rotates through. Per-call work is
/// Limited-safe — no `vkAllocateMemory`, no pipeline / fence creation in
/// steady state.
///
/// [`TextureRing`]: streamlib_plugin_sdk::sdk::rhi::TextureRing
pub trait JpegDecodeBackend: Send + std::fmt::Debug {
    /// Decode `jpeg_bytes` into the backend's next output slot. Returns
    /// the slot's [`JpegDecodeOutput`] (texture + surface_id + dimensions
    /// + resolved colorimetry) so downstream consumers can pick it up
    /// through the normal `surface_id` contract.
    ///
    /// Errors:
    /// - [`streamlib_plugin_sdk::sdk::error::Error::GpuError`] for parser /
    ///   Huffman failures, dimension overflows past the backend's declared
    ///   maxima, colorimetry-resolution failures, and backend-specific GPU
    ///   failures.
    /// - [`streamlib_plugin_sdk::sdk::error::Error::NotSupported`] for inputs
    ///   the backend cannot handle (e.g. non-4:2:0 sampling on the
    ///   Vulkan-compute backend).
    fn decode(&mut self, jpeg_bytes: &[u8]) -> Result<JpegDecodeOutput>;

    /// Backend identity. Stable across the backend's lifetime; used by
    /// tracing and callers that want to confirm which backend
    /// [`crate::simple_decoder::SimpleJpegDecoder`] selected.
    fn kind(&self) -> JpegBackendKind;
}
