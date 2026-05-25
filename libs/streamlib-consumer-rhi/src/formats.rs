// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI format primitives shared between host and consumer sides.
//!
//! `streamlib::sdk::rhi::{TextureFormat, TextureUsages}` re-export
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
///
/// `#[repr(transparent)]` so the newtype is byte-equivalent to its
/// inner `u32` across the plugin FFI boundary — adapter vtables
/// frequently carry `usage_bits: u32` arguments that get reconstituted
/// via [`Self::from_bits_truncate`] on the receiving side. Pinning the
/// repr means a cdylib compiled with a different rustc/dep-graph than
/// the host still reads / writes the bit pattern at the same byte
/// offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
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

    /// Raw bits for ABI crossings (vtable wire payloads). Pair with
    /// [`Self::from_bits_truncate`] to round-trip across a cdylib
    /// boundary.
    pub fn bits(&self) -> u32 {
        self.0
    }

    /// Reconstruct from a raw bit pattern. Bits outside the defined
    /// constants are silently dropped so unknown future flags don't
    /// trip the receiver.
    pub fn from_bits_truncate(bits: u32) -> Self {
        const ALL: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4);
        Self(bits & ALL)
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

#[cfg(test)]
mod layout_tests {
    //! Layout regression tests for the FFI-crossing format primitives.
    //!
    //! Mirrors the pattern in `streamlib-plugin-abi`'s `layout_tests`
    //! module: every byte offset, size, and alignment is asserted so a
    //! silent rustc / dep-graph drift fails CI loudly. The discriminant
    //! values for [`TextureFormat`] are part of the wire contract —
    //! adapter vtables in `streamlib-adapter-*` pass them as bare
    //! `u32` ([`crate::TextureFormat`] then reconstitutes via an `as`
    //! cast). Changing a discriminant silently re-maps existing
    //! cdylibs' format payloads onto the wrong host-side variant; the
    //! discriminant pins below are the regression lock for that.
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn texture_format_layout() {
        assert_eq!(size_of::<TextureFormat>(), 4);
        assert_eq!(align_of::<TextureFormat>(), 4);
    }

    #[test]
    fn texture_format_discriminants_are_pinned() {
        // Every variant's discriminant value IS the wire contract.
        assert_eq!(TextureFormat::Rgba8Unorm as u32, 0);
        assert_eq!(TextureFormat::Rgba8UnormSrgb as u32, 1);
        assert_eq!(TextureFormat::Bgra8Unorm as u32, 2);
        assert_eq!(TextureFormat::Bgra8UnormSrgb as u32, 3);
        assert_eq!(TextureFormat::Rgba16Float as u32, 4);
        assert_eq!(TextureFormat::Rgba32Float as u32, 5);
        assert_eq!(TextureFormat::Nv12 as u32, 6);
    }

    #[test]
    fn texture_usages_layout() {
        // `#[repr(transparent)]` over `u32` — byte-equivalent to its
        // single inner field so adapter vtables can pass a bare
        // `u32` and the receiver can `TextureUsages::from_bits_truncate`
        // it back without copying.
        assert_eq!(size_of::<TextureUsages>(), size_of::<u32>());
        assert_eq!(align_of::<TextureUsages>(), align_of::<u32>());
    }

    #[test]
    fn texture_usages_bit_pattern_is_pinned() {
        // The bit pattern IS the wire contract.
        assert_eq!(TextureUsages::NONE.bits(), 0);
        assert_eq!(TextureUsages::COPY_SRC.bits(), 1 << 0);
        assert_eq!(TextureUsages::COPY_DST.bits(), 1 << 1);
        assert_eq!(TextureUsages::TEXTURE_BINDING.bits(), 1 << 2);
        assert_eq!(TextureUsages::STORAGE_BINDING.bits(), 1 << 3);
        assert_eq!(TextureUsages::RENDER_ATTACHMENT.bits(), 1 << 4);
    }

    #[test]
    fn texture_usages_from_bits_truncate_drops_unknown_bits() {
        let known = TextureUsages::COPY_SRC | TextureUsages::COPY_DST;
        let with_unknown = TextureUsages::from_bits_truncate(known.bits() | 0xFFFF_0000);
        assert_eq!(with_unknown, known);
    }
}
