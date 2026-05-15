// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Resolve a sparse [`ColorInfo`] into a [`ResolvedColorInfo`] by
//! filling missing axes from the V4L2 / libplacebo default table.

use crate::_generated_::tatolab__core::color_info::{Matrix, Primaries, Range, Transfer};
use crate::_generated_::ColorInfo;

use super::resolved::{ColorSpaceKind, ResolvedColorInfo};

/// Resolve every missing axis to its per-kind default. The defaults
/// mirror V4L2's `V4L2_MAP_*_DEFAULT` macros and libplacebo's
/// `pl_color_space_infer`:
///
/// | axis | RGB default | YCbCr default |
/// |---|---|---|
/// | `primaries` | `Bt709` | `Bt709` |
/// | `transfer` | `Srgb` | `Bt709` |
/// | `matrix` | `Identity` | `Smpte170m` (BT.601, the UVC convention) |
/// | `range` | `Full` | `Limited` |
///
/// RGB-encoded sources also override the matrix axis to `Identity`
/// regardless of the on-wire value, since matrix is meaningless when
/// data is already RGB.
pub fn resolve_color_defaults(info: &ColorInfo, kind: ColorSpaceKind) -> ResolvedColorInfo {
    let primaries = info.primaries.clone().unwrap_or(Primaries::Bt709);
    let transfer = info.transfer.clone().unwrap_or(match kind {
        ColorSpaceKind::Rgb => Transfer::Srgb,
        ColorSpaceKind::Yuv => Transfer::Bt709,
    });
    let matrix = match kind {
        ColorSpaceKind::Rgb => Matrix::Identity,
        ColorSpaceKind::Yuv => info.matrix.clone().unwrap_or(Matrix::Smpte170m),
    };
    let range = info.range.clone().unwrap_or(match kind {
        ColorSpaceKind::Rgb => Range::Full,
        ColorSpaceKind::Yuv => Range::Limited,
    });
    ResolvedColorInfo {
        primaries,
        transfer,
        matrix,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All-None `ColorInfo` for an RGB source resolves to
    /// `Bt709 / Srgb / Identity / Full`.
    #[test]
    fn rgb_all_none_defaults_match_table() {
        let info = ColorInfo::default();
        let r = resolve_color_defaults(&info, ColorSpaceKind::Rgb);
        assert_eq!(r.primaries, Primaries::Bt709);
        assert_eq!(r.transfer, Transfer::Srgb);
        assert_eq!(r.matrix, Matrix::Identity);
        assert_eq!(r.range, Range::Full);
    }

    /// All-None `ColorInfo` for a YCbCr source resolves to the UVC
    /// convention: `Bt709 / Bt709 / Smpte170m / Limited`.
    #[test]
    fn yuv_all_none_defaults_match_table() {
        let info = ColorInfo::default();
        let r = resolve_color_defaults(&info, ColorSpaceKind::Yuv);
        assert_eq!(r.primaries, Primaries::Bt709);
        assert_eq!(r.transfer, Transfer::Bt709);
        assert_eq!(r.matrix, Matrix::Smpte170m);
        assert_eq!(r.range, Range::Limited);
    }

    /// Explicit values on every axis pass through unchanged for YUV.
    #[test]
    fn explicit_values_pass_through_yuv() {
        let info = ColorInfo {
            primaries: Some(Primaries::Bt2020),
            transfer: Some(Transfer::Smpte2084),
            matrix: Some(Matrix::Bt2020Ncl),
            range: Some(Range::Limited),
        };
        let r = resolve_color_defaults(&info, ColorSpaceKind::Yuv);
        assert_eq!(r.primaries, Primaries::Bt2020);
        assert_eq!(r.transfer, Transfer::Smpte2084);
        assert_eq!(r.matrix, Matrix::Bt2020Ncl);
        assert_eq!(r.range, Range::Limited);
    }

    /// RGB sources override the matrix axis to `Identity` even when
    /// the on-wire field claims otherwise. Matrix coefficients are
    /// meaningless for already-RGB data.
    #[test]
    fn rgb_overrides_matrix_to_identity() {
        let info = ColorInfo {
            primaries: Some(Primaries::Bt709),
            transfer: Some(Transfer::Srgb),
            matrix: Some(Matrix::Bt709),
            range: Some(Range::Full),
        };
        let r = resolve_color_defaults(&info, ColorSpaceKind::Rgb);
        assert_eq!(r.matrix, Matrix::Identity);
    }

    /// Per-axis fallback matches what `v4l2_color.rs::tests` lock
    /// for the vivid + UVC default cases.
    ///
    /// vivid reports `colorspace = SMPTE170M` with everything else
    /// default. After `v4l2_color_to_color_info`, that's:
    ///   primaries=Smpte170m, transfer=Bt709, matrix=Smpte170m, range=Limited.
    /// All axes set → resolver passes through unchanged.
    #[test]
    fn vivid_v4l2_baseline_passes_through() {
        let info = ColorInfo {
            primaries: Some(Primaries::Smpte170m),
            transfer: Some(Transfer::Bt709),
            matrix: Some(Matrix::Smpte170m),
            range: Some(Range::Limited),
        };
        let r = resolve_color_defaults(&info, ColorSpaceKind::Yuv);
        assert_eq!(r.primaries, Primaries::Smpte170m);
        assert_eq!(r.transfer, Transfer::Bt709);
        assert_eq!(r.matrix, Matrix::Smpte170m);
        assert_eq!(r.range, Range::Limited);
    }

    /// Partial info: only matrix specified. Other axes pull defaults.
    #[test]
    fn partial_info_fills_only_missing_axes() {
        let info = ColorInfo {
            primaries: None,
            transfer: None,
            matrix: Some(Matrix::Bt709),
            range: None,
        };
        let r = resolve_color_defaults(&info, ColorSpaceKind::Yuv);
        assert_eq!(r.matrix, Matrix::Bt709); // explicit
        assert_eq!(r.primaries, Primaries::Bt709); // default
        assert_eq!(r.transfer, Transfer::Bt709); // YUV default
        assert_eq!(r.range, Range::Limited); // YUV default
    }
}
