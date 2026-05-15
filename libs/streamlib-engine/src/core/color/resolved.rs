// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fully-resolved color description — every axis has a concrete value.

use crate::_generated_::tatolab__core::color_info::{Matrix, Primaries, Range, Transfer};

/// Color description with every axis resolved to a concrete value. The
/// converter consumes this; the on-wire [`ColorInfo`] is a sparse
/// `Option<T>`-per-axis projection of the same shape.
///
/// Construction goes through [`super::resolve::resolve_color_defaults`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedColorInfo {
    pub primaries: Primaries,
    pub transfer: Transfer,
    pub matrix: Matrix,
    pub range: Range,
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
