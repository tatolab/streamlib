// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan texture implementation for RHI.

use crate::core::rhi::TextureFormat;

/// Vulkan texture wrapper.
///
/// This is a stub implementation. Full Vulkan support is pending.
pub struct VulkanTexture {
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl VulkanTexture {
    /// Create a placeholder texture for cases where a VulkanTexture is needed
    /// but the actual texture is stored elsewhere (e.g., Metal texture on macOS).
    pub fn placeholder() -> Self {
        Self {
            width: 0,
            height: 0,
            format: TextureFormat::Rgba8Unorm,
        }
    }

    /// Texture width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Texture height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Texture format.
    pub fn format(&self) -> TextureFormat {
        self.format
    }
}

impl Clone for VulkanTexture {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }
}
