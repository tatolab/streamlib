// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fully-resolved color description — every axis has a concrete value.
//!
//! Engine-internal ID enums mirror the H.273 / ITU-T VUI 4-tuple
//! variants of the on-wire `ColorInfo` schema. H.273-byte → engine-ID
//! translation lives in [`super::h273_color_vui`] — engine core math and
//! kernel inputs consume only the IDs here.

use super::TransferId;

/// Engine-internal color-primaries id. Mirrors H.273
/// `ColourPrimaries` variants — an H.273 primaries byte is
/// translated into this by [`super::H273ColorVui::resolve_defaults`].
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
/// `MatrixCoefficients` variants — an H.273 matrix byte is
/// translated into this by [`super::H273ColorVui::resolve_defaults`].
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
/// converter consumes this; a consumer's on-wire color description is a
/// sparse `Option<T>`-per-axis projection of the same shape.
///
/// Construction goes through [`super::resolve::resolve_color_defaults`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedColorInfo {
    pub primaries: PrimariesId,
    pub transfer: TransferId,
    pub matrix: MatrixId,
    pub range: RangeId,
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
