// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Texture cache for creating texture views from pixel buffers.

use super::{PixelFormat, RhiPixelBuffer};
use crate::core::Result;

/// Creates texture views from pixel buffers.
///
/// Wraps the platform's texture cache (CVMetalTextureCache on macOS).
/// Create one per processor that needs to render pixel buffers.
/// The platform handles internal caching and GPU synchronization.
pub struct RhiTextureCache {
    #[cfg(target_os = "macos")]
    pub(crate) inner: crate::metal::rhi::texture_cache::TextureCacheMacOS,

    #[cfg(target_os = "linux")]
    pub(crate) inner: crate::vulkan::rhi::VulkanTextureCache,

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) _marker: std::marker::PhantomData<()>,
}

impl RhiTextureCache {
    /// Create a texture view from a pixel buffer.
    ///
    /// The view is ephemeral - create it each frame and let it drop after use.
    /// The platform manages GPU synchronization internally.
    pub fn create_view(&self, buffer: &RhiPixelBuffer) -> Result<RhiTextureView> {
        #[cfg(target_os = "macos")]
        {
            self.inner.create_view(buffer)
        }
        #[cfg(target_os = "linux")]
        {
            // VulkanTextureCache needs a VkImage to create a view from,
            // but RhiPixelBuffer on Linux wraps a VkBuffer (not VkImage).
            // For now, return NotSupported until pixel buffer -> texture path is wired.
            let _ = buffer;
            Err(crate::core::StreamError::NotSupported(
                "Vulkan texture cache create_view not yet implemented".into(),
            ))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = buffer;
            Err(crate::core::StreamError::Configuration(
                "RhiTextureCache not implemented for this platform".into(),
            ))
        }
    }

    /// Flush the cache to free unused textures.
    ///
    /// Call periodically (e.g., every few seconds) to free memory.
    pub fn flush(&self) {
        #[cfg(target_os = "macos")]
        {
            self.inner.flush();
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.flush();
        }
    }
}

impl std::fmt::Debug for RhiTextureCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiTextureCache").finish()
    }
}

/// Ephemeral texture view created from a pixel buffer.
///
/// Create per-frame via [`RhiTextureCache::create_view`] and let drop after use.
/// Holds a reference to the source buffer to keep it alive during rendering.
pub struct RhiTextureView {
    #[cfg(target_os = "macos")]
    pub(crate) inner: crate::metal::rhi::texture_cache::TextureViewMacOS,

    #[cfg(target_os = "linux")]
    pub(crate) vulkan_image_view_handle: u64,

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) _marker: std::marker::PhantomData<()>,

    /// Keep the source buffer alive while this view exists.
    #[allow(dead_code)]
    pub(crate) source_buffer: RhiPixelBuffer,
}

impl RhiTextureView {
    /// Width of the texture view.
    pub fn width(&self) -> u32 {
        self.source_buffer.width
    }

    /// Height of the texture view.
    pub fn height(&self) -> u32 {
        self.source_buffer.height
    }

    /// Pixel format of the texture view.
    pub fn format(&self) -> PixelFormat {
        self.source_buffer.format()
    }

    /// Get the underlying Metal texture (macOS only).
    #[cfg(target_os = "macos")]
    pub fn as_metal_texture(&self) -> &metal::TextureRef {
        self.inner.as_metal_texture()
    }
}

impl std::fmt::Debug for RhiTextureView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiTextureView")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("format", &self.format())
            .finish()
    }
}
