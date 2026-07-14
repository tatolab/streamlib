// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fully-resolved color description — every axis has a concrete value.
//!
//! Engine-internal ID enums mirror the H.273 / ITU-T VUI 4-tuple
//! variants of the on-wire `ColorInfo` schema. Schema↔engine-ID
//! translation lives in [`super::translate`] — engine core math and
//! kernel inputs consume only the IDs here.

use super::TransferId;

/// Engine-internal color-primaries id. Mirrors H.273
/// `ColourPrimaries` variants — the schema's primaries enum is
/// translated into this in [`super::translate`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimariesId {
    Bt709,
    Bt470M,
    Bt470Bg,
    Smpte170m,
    Smpte240m,
    Film,
    Bt2020,
    Smpte428,
    Smpte431,
    Smpte432,
    Ebu3213,
}

/// Engine-internal YCbCr-matrix id. Mirrors H.273
/// `MatrixCoefficients` variants — the schema's matrix enum is
/// translated into this in [`super::translate`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatrixId {
    Identity,
    Bt709,
    Fcc,
    Bt470Bg,
    Smpte170m,
    Smpte240m,
    Ycgco,
    Bt2020Ncl,
    Bt2020Cl,
    Smpte2085,
    ChromaNcl,
    ChromaCl,
    Ictcp,
}

/// Engine-internal quantization range id. Maps to H.264/H.265 VUI
/// `video_full_range_flag` (`Limited` = 0, `Full` = 1).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeId {
    Limited,
    Full,
}

/// Color description with every axis resolved to a concrete value. The
/// converter consumes this; the on-wire [`crate::_generated_::ColorInfo`]
/// is a sparse `Option<T>`-per-axis projection of the same shape.
///
/// Construction goes through [`super::resolve::resolve_color_defaults`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedColorInfo {
    pub primaries: PrimariesId,
    pub transfer: TransferId,
    pub matrix: MatrixId,
    pub range: RangeId,
}

/// Engine-internal trait pair consumed by swapchain colorspace
/// negotiation. Holds only the axes [`super::pick_swapchain_format`]
/// actually inspects — primaries (Bt2020 vs other) and transfer
/// (`Pq` / `Hlg` vs other). Schema → traits translation in
/// [`super::translate::color_traits_from_color_info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ColorTraits {
    pub primaries: Option<PrimariesId>,
    pub transfer: Option<TransferId>,
}

/// Engine-internal HDR static metadata, pre-translated to the f32
/// fields `vkSetHdrMetadataEXT` expects. Constructed via
/// [`super::translate::hdr_metadata_from_schema`] from the wire-format
/// `MasteringDisplay` + `ContentLight` integers; consumed by
/// [`super::build_hdr_metadata`].
///
/// Chromaticities are CIE 1931 xy in the `[0, 1]` float domain.
/// Luminances are cd/m² (nits). Content-light fields are integer
/// cd/m² promoted to f32.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrStaticMetadata {
    pub display_primary_red: [f32; 2],
    pub display_primary_green: [f32; 2],
    pub display_primary_blue: [f32; 2],
    pub white_point: [f32; 2],
    pub min_luminance_cd_m2: f32,
    pub max_luminance_cd_m2: f32,
    pub max_content_light_level: f32,
    pub max_frame_average_light_level: f32,
}

/// Disambiguator for the resolver's per-axis defaults. RGB-encoded
/// sources default transfer→`Srgb` and range→`Full`; YCbCr-encoded
/// sources default transfer→`Bt709` and range→`Limited`. The dataset
/// matches V4L2's `V4L2_MAP_*_DEFAULT` macros and libplacebo's
/// `pl_color_space_infer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpaceKind {
    /// RGB / BGRA / packed-RGB source (matrix axis collapses to
    /// `Identity` regardless of the on-wire matrix enum).
    Rgb,
    /// YCbCr / NV12 / YUYV source (matrix axis honors the on-wire
    /// value, with BT.601 525-line as the fallback).
    Yuv,
}
