// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How a kernel's sampled-texture binding filters and addresses the texture
//! it reads.

/// How texels are blended when a sample falls between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerFilter {
    /// The nearest texel, unblended.
    Nearest,
    /// A bilinear blend of the four nearest texels.
    Linear,
}

/// Where a coordinate outside `[0, 1]` reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerAddressMode {
    /// The edge texel.
    ClampToEdge,
    /// The texture tiled.
    Repeat,
    /// The texture tiled, every other tile mirrored.
    MirroredRepeat,
    /// The [`SamplerBorderColor`].
    ClampToBorder,
}

/// What a [`SamplerAddressMode::ClampToBorder`] coordinate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerBorderColor {
    TransparentBlack,
    OpaqueBlack,
    OpaqueWhite,
}

/// Which mip levels a sample reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerMipmapMode {
    /// Level 0 only.
    BaseLevelOnly,
    /// The nearest level to the sample's footprint.
    Nearest,
    /// A blend of the two nearest levels.
    Linear,
}

/// The most texels an anisotropic sample takes along its footprint's long
/// axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerAnisotropy {
    Off,
    X2,
    X4,
    X8,
    X16,
}

impl SamplerAnisotropy {
    /// The sample count, 1 when anisotropic filtering is off.
    pub const fn max_samples(self) -> u32 {
        match self {
            Self::Off => 1,
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
            Self::X16 => 16,
        }
    }
}

/// How one sampled-texture binding filters and addresses its texture.
///
/// Attach it to a binding with `ComputeBindingSpec::with_sampler` or
/// `GraphicsBindingSpec::with_sampler`; a binding without one samples with
/// [`SamplerDescriptor::LINEAR_CLAMP`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerDescriptor {
    pub magnification_filter: SamplerFilter,
    pub minification_filter: SamplerFilter,
    pub mipmap_mode: SamplerMipmapMode,
    /// Addressing along u, v and w.
    pub address_modes: [SamplerAddressMode; 3],
    pub border_color: SamplerBorderColor,
    pub anisotropy: SamplerAnisotropy,
}

impl SamplerDescriptor {
    /// Bilinear, base level only, clamped to the edge.
    pub const LINEAR_CLAMP: Self = Self {
        magnification_filter: SamplerFilter::Linear,
        minification_filter: SamplerFilter::Linear,
        mipmap_mode: SamplerMipmapMode::BaseLevelOnly,
        address_modes: [SamplerAddressMode::ClampToEdge; 3],
        border_color: SamplerBorderColor::TransparentBlack,
        anisotropy: SamplerAnisotropy::Off,
    };

    /// Unfiltered, base level only, clamped to the edge: what a kernel that
    /// reads whole texels wants.
    pub const NEAREST_CLAMP: Self = Self {
        magnification_filter: SamplerFilter::Nearest,
        minification_filter: SamplerFilter::Nearest,
        ..Self::LINEAR_CLAMP
    };
}

impl Default for SamplerDescriptor {
    fn default() -> Self {
        Self::LINEAR_CLAMP
    }
}
