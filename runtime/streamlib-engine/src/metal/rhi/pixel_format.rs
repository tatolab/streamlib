// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS PixelFormat Metal conversions.

use crate::core::rhi::PixelFormat;

// MTLPixelFormat constants
const MTL_PIXEL_FORMAT_BGRA8_UNORM: u64 = 80;
const MTL_PIXEL_FORMAT_RGBA8_UNORM: u64 = 70;
const MTL_PIXEL_FORMAT_RGBA16_UNORM: u64 = 90;
const MTL_PIXEL_FORMAT_R8_UNORM: u64 = 10;

impl PixelFormat {
    /// Convert to MTLPixelFormat for texture creation.
    pub fn to_mtl_pixel_format(&self) -> u64 {
        match self {
            Self::Bgra32 => MTL_PIXEL_FORMAT_BGRA8_UNORM,
            Self::Rgba32 => MTL_PIXEL_FORMAT_RGBA8_UNORM,
            Self::Argb32 => MTL_PIXEL_FORMAT_BGRA8_UNORM, // Metal doesn't have ARGB, use BGRA
            Self::Rgba64 => MTL_PIXEL_FORMAT_RGBA16_UNORM,
            Self::Gray8 => MTL_PIXEL_FORMAT_R8_UNORM,
            // For YUV formats, return BGRA as default for texture cache
            // Actual YUV→RGB conversion happens in shader
            Self::Nv12VideoRange | Self::Nv12FullRange => MTL_PIXEL_FORMAT_BGRA8_UNORM,
            Self::Uyvy422 | Self::Yuyv422 => MTL_PIXEL_FORMAT_BGRA8_UNORM,
            Self::Unknown => MTL_PIXEL_FORMAT_BGRA8_UNORM,
        }
    }
}
