// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI format primitives shared between host and consumer sides.
//!
//! `streamlib::core::rhi::{TextureFormat, TextureUsages}` re-export
//! these so existing host-side call sites compile unchanged.

/// Texture pixel formats supported by the RHI.
///
/// Platform backends map these to native format constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureFormat {
    /// 8-bit RGBA, unsigned normalized.
    Rgba8Unorm = 0,
    /// 8-bit RGBA, sRGB.
    Rgba8UnormSrgb = 1,
    /// 8-bit BGRA, unsigned normalized.
    Bgra8Unorm = 2,
    /// 8-bit BGRA, sRGB.
    Bgra8UnormSrgb = 3,
    /// 16-bit float RGBA.
    Rgba16Float = 4,
    /// 32-bit float RGBA.
    Rgba32Float = 5,
    /// NV12 YUV (for video decode).
    Nv12 = 6,
}

impl TextureFormat {
    /// Bytes per pixel for this format.
    pub fn bytes_per_pixel(&self) -> u32 {
        match self {
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm | Self::Bgra8UnormSrgb => 4,
            Self::Rgba16Float => 8,
            Self::Rgba32Float => 16,
            Self::Nv12 => 1, // Planar format, varies per plane
        }
    }

    /// Whether this format has an sRGB transfer function.
    pub fn is_srgb(&self) -> bool {
        matches!(self, Self::Rgba8UnormSrgb | Self::Bgra8UnormSrgb)
    }
}

/// Texture usage flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureUsages(u32);

impl TextureUsages {
    pub const NONE: Self = Self(0);
    /// Can be copied from.
    pub const COPY_SRC: Self = Self(1 << 0);
    /// Can be copied to.
    pub const COPY_DST: Self = Self(1 << 1);
    /// Can be bound as a texture (sampled).
    pub const TEXTURE_BINDING: Self = Self(1 << 2);
    /// Can be bound as a storage texture (compute read/write).
    pub const STORAGE_BINDING: Self = Self(1 << 3);
    /// Can be used as a render target.
    pub const RENDER_ATTACHMENT: Self = Self(1 << 4);

    pub fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for TextureUsages {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for TextureUsages {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
