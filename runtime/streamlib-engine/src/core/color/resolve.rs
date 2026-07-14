// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Resolve sparse per-axis engine-ID inputs into a [`ResolvedColorInfo`]
//! by filling missing axes from the V4L2 / libplacebo default table.

use super::resolved::{ColorSpaceKind, MatrixId, PrimariesId, RangeId, ResolvedColorInfo};
use super::transfer::TransferId;

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
///
/// Each axis takes an `Option<EngineId>` — `None` means the on-wire
/// value was absent (H.273 "Unspecified"). Schema → engine-ID
/// translation happens at the consumer boundary via
/// [`super::translate`].
pub fn resolve_color_defaults(
    primaries: Option<PrimariesId>,
    transfer: Option<TransferId>,
    matrix: Option<MatrixId>,
    range: Option<RangeId>,
    kind: ColorSpaceKind,
) -> ResolvedColorInfo {
    let primaries = primaries.unwrap_or(PrimariesId::Bt709);
    let transfer = transfer.unwrap_or(match kind {
        ColorSpaceKind::Rgb => TransferId::Srgb,
        ColorSpaceKind::Yuv => TransferId::Bt709,
    });
    let matrix = match kind {
        ColorSpaceKind::Rgb => MatrixId::Identity,
        ColorSpaceKind::Yuv => matrix.unwrap_or(MatrixId::Smpte170m),
    };
    let range = range.unwrap_or(match kind {
        ColorSpaceKind::Rgb => RangeId::Full,
        ColorSpaceKind::Yuv => RangeId::Limited,
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

    /// All-None inputs for an RGB source resolve to
    /// `Bt709 / Srgb / Identity / Full`.
    #[test]
    fn rgb_all_none_defaults_match_table() {
        let r = resolve_color_defaults(None, None, None, None, ColorSpaceKind::Rgb);
        assert_eq!(r.primaries, PrimariesId::Bt709);
        assert_eq!(r.transfer, TransferId::Srgb);
        assert_eq!(r.matrix, MatrixId::Identity);
        assert_eq!(r.range, RangeId::Full);
    }

    /// All-None inputs for a YCbCr source resolve to the UVC
    /// convention: `Bt709 / Bt709 / Smpte170m / Limited`.
    #[test]
    fn yuv_all_none_defaults_match_table() {
        let r = resolve_color_defaults(None, None, None, None, ColorSpaceKind::Yuv);
        assert_eq!(r.primaries, PrimariesId::Bt709);
        assert_eq!(r.transfer, TransferId::Bt709);
        assert_eq!(r.matrix, MatrixId::Smpte170m);
        assert_eq!(r.range, RangeId::Limited);
    }

    /// Explicit values on every axis pass through unchanged for YUV.
    #[test]
    fn explicit_values_pass_through_yuv() {
        let r = resolve_color_defaults(
            Some(PrimariesId::Bt2020),
            Some(TransferId::Pq),
            Some(MatrixId::Bt2020Ncl),
            Some(RangeId::Limited),
            ColorSpaceKind::Yuv,
        );
        assert_eq!(r.primaries, PrimariesId::Bt2020);
        assert_eq!(r.transfer, TransferId::Pq);
        assert_eq!(r.matrix, MatrixId::Bt2020Ncl);
        assert_eq!(r.range, RangeId::Limited);
    }

    /// RGB sources override the matrix axis to `Identity` even when
    /// the input claims otherwise. Matrix coefficients are meaningless
    /// for already-RGB data.
    #[test]
    fn rgb_overrides_matrix_to_identity() {
        let r = resolve_color_defaults(
            Some(PrimariesId::Bt709),
            Some(TransferId::Srgb),
            Some(MatrixId::Bt709),
            Some(RangeId::Full),
            ColorSpaceKind::Rgb,
        );
        assert_eq!(r.matrix, MatrixId::Identity);
    }

    /// Per-axis fallback matches what `v4l2_color.rs::tests` lock for
    /// the vivid + UVC default cases. vivid reports
    /// `colorspace = SMPTE170M` with everything else default — after
    /// `v4l2_color_to_color_info` + schema→engine-ID translation
    /// that's primaries=Smpte170m, transfer=Bt709, matrix=Smpte170m,
    /// range=Limited. All axes set → resolver passes through.
    #[test]
    fn vivid_v4l2_baseline_passes_through() {
        let r = resolve_color_defaults(
            Some(PrimariesId::Smpte170m),
            Some(TransferId::Bt709),
            Some(MatrixId::Smpte170m),
            Some(RangeId::Limited),
            ColorSpaceKind::Yuv,
        );
        assert_eq!(r.primaries, PrimariesId::Smpte170m);
        assert_eq!(r.transfer, TransferId::Bt709);
        assert_eq!(r.matrix, MatrixId::Smpte170m);
        assert_eq!(r.range, RangeId::Limited);
    }

    /// Partial info: only matrix specified. Other axes pull defaults.
    #[test]
    fn partial_info_fills_only_missing_axes() {
        let r =
            resolve_color_defaults(None, None, Some(MatrixId::Bt709), None, ColorSpaceKind::Yuv);
        assert_eq!(r.matrix, MatrixId::Bt709); // explicit
        assert_eq!(r.primaries, PrimariesId::Bt709); // default
        assert_eq!(r.transfer, TransferId::Bt709); // YUV default
        assert_eq!(r.range, RangeId::Limited); // YUV default
    }
}
