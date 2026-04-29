// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Skia adapter error taxonomy.

use thiserror::Error;

/// Errors specific to constructing or operating the Skia surface
/// adapter.
///
/// Per-acquire failures travel through the standard
/// [`streamlib_adapter_abi::AdapterError`]; this enum covers the
/// adapter's setup-time and Skia-binding failure modes.
#[derive(Debug, Error)]
pub enum SkiaAdapterError {
    /// Skia's `gpu::DirectContext` could not be created from the
    /// underlying Vulkan handles. Carries Skia's reason.
    #[error("skia DirectContext build failed: {reason}")]
    DirectContextBuildFailed { reason: String },

    /// Wrapping a host-allocated `VkImage` as a Skia
    /// `GrBackendRenderTarget` returned `None`. Most common cause is
    /// `VK_FORMAT_UNDEFINED` reaching Skia (means the underlying
    /// `VulkanImageInfoExt::vk_image_info()` is incomplete) or a
    /// usage flag set Skia rejects.
    #[error("skia BackendRenderTarget wrap failed: {reason}")]
    BackendRenderTargetWrapFailed { reason: String },

    /// Wrapping the host's `VkImage` as a Skia `Image` (read path)
    /// returned `None`. Same root causes as
    /// [`Self::BackendRenderTargetWrapFailed`].
    #[error("skia Image wrap failed: {reason}")]
    ImageWrapFailed { reason: String },

    /// `VulkanImageInfoExt::vk_image_info()` reported
    /// `VK_FORMAT_UNDEFINED`. Skia rejects this — the upstream
    /// `VulkanTextureLike` impl is missing format metadata.
    #[error("vk_image_info reported VK_FORMAT_UNDEFINED — upstream VulkanTextureLike impl is missing format metadata")]
    UndefinedFormat,
}
