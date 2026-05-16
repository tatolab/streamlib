// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema↔engine-ID translation for the H.273 / ITU-T VUI 4-tuple.
//!
//! Single engine module that imports
//! [`crate::_generated_::tatolab__core::color_info::*`]; the rest of
//! the engine's color stack speaks engine IDs only. Consumers call
//! these helpers at their schema boundary (V4L2 capture, VUI parse,
//! EDID read, IPC ingress) to translate sparse wire-format inputs
//! into engine-ID shapes that flow into the core color math.

use crate::_generated_::tatolab__core::color_info::{Matrix, Primaries, Range, Transfer};
use crate::_generated_::{ColorInfo, ContentLight, MasteringDisplay};

use super::{
    ColorTraits, HdrStaticMetadata, MatrixId, PrimariesId, RangeId, TransferId,
};

/// Map a wire-format `Transfer` enum to the shader-side [`TransferId`].
/// Unmapped curves fall back to `Linear` — they're rare in practice
/// and the converter applies no transfer in that case. A future pass
/// can extend the shader's switch.
pub fn transfer_id_from_schema(t: &Transfer) -> TransferId {
    match t {
        Transfer::Srgb => TransferId::Srgb,
        Transfer::Bt709
        | Transfer::Smpte170m
        | Transfer::Bt2020TenBit
        | Transfer::Bt2020TwelveBit => TransferId::Bt709,
        Transfer::Smpte2084 => TransferId::Pq,
        Transfer::AribStdB67 => TransferId::Hlg,
        Transfer::Linear => TransferId::Linear,
        // Gamma22 / Gamma28 / Smpte240m / Log* / Xvycc / Bt1361 / Smpte428
        // are uncommon end-to-end; map to Linear (no transform) for now.
        _ => TransferId::Linear,
    }
}

/// Map a wire-format `Primaries` enum to [`PrimariesId`]. Total — every
/// schema variant is named; absence is the consumer's job to track via
/// `Option<&Primaries>` before calling.
pub fn primaries_id_from_schema(p: &Primaries) -> PrimariesId {
    match p {
        Primaries::Bt709 => PrimariesId::Bt709,
        Primaries::Bt470M => PrimariesId::Bt470M,
        Primaries::Bt470Bg => PrimariesId::Bt470Bg,
        Primaries::Smpte170m => PrimariesId::Smpte170m,
        Primaries::Smpte240m => PrimariesId::Smpte240m,
        Primaries::Film => PrimariesId::Film,
        Primaries::Bt2020 => PrimariesId::Bt2020,
        Primaries::Smpte428 => PrimariesId::Smpte428,
        Primaries::Smpte431 => PrimariesId::Smpte431,
        Primaries::Smpte432 => PrimariesId::Smpte432,
        Primaries::Ebu3213 => PrimariesId::Ebu3213,
    }
}

/// Map a wire-format `Matrix` enum to [`MatrixId`]. Total.
pub fn matrix_id_from_schema(m: &Matrix) -> MatrixId {
    match m {
        Matrix::Identity => MatrixId::Identity,
        Matrix::Bt709 => MatrixId::Bt709,
        Matrix::Fcc => MatrixId::Fcc,
        Matrix::Bt470Bg => MatrixId::Bt470Bg,
        Matrix::Smpte170m => MatrixId::Smpte170m,
        Matrix::Smpte240m => MatrixId::Smpte240m,
        Matrix::Ycgco => MatrixId::Ycgco,
        Matrix::Bt2020Ncl => MatrixId::Bt2020Ncl,
        Matrix::Bt2020Cl => MatrixId::Bt2020Cl,
        Matrix::Smpte2085 => MatrixId::Smpte2085,
        Matrix::ChromaNcl => MatrixId::ChromaNcl,
        Matrix::ChromaCl => MatrixId::ChromaCl,
        Matrix::Ictcp => MatrixId::Ictcp,
    }
}

/// Map a wire-format `Range` enum to [`RangeId`]. Total.
pub fn range_id_from_schema(r: &Range) -> RangeId {
    match r {
        Range::Limited => RangeId::Limited,
        Range::Full => RangeId::Full,
    }
}

/// Project a sparse `ColorInfo` into the [`ColorTraits`] pair the
/// swapchain colorspace picker actually inspects — primaries and
/// transfer. Matrix and range are dropped: neither informs swapchain
/// format selection.
pub fn color_traits_from_color_info(info: &ColorInfo) -> ColorTraits {
    ColorTraits {
        primaries: info.primaries.as_ref().map(primaries_id_from_schema),
        transfer: info.transfer.as_ref().map(transfer_id_from_schema),
    }
}

/// Translate the wire-format integer fields of `MasteringDisplay` and
/// `ContentLight` into the f32 fields `vkSetHdrMetadataEXT` expects.
///
/// Schema units:
/// - chromaticity in 1/50000 increments (CIE 1931) → divided by 50000
///   to land in `[0.0, 1.0]`.
/// - luminance in 0.0001 cd/m² increments → divided by 10000 to land
///   in cd/m².
/// - `max_cll` / `max_fall` are integer cd/m² in the schema → cast to
///   f32 directly (no scaling).
pub fn hdr_metadata_from_schema(
    mastering: &MasteringDisplay,
    content_light: &ContentLight,
) -> HdrStaticMetadata {
    const CHROMA_SCALE: f32 = 1.0 / 50_000.0;
    const LUM_SCALE: f32 = 1.0 / 10_000.0;

    HdrStaticMetadata {
        display_primary_red: [
            mastering.display_primaries_r_x as f32 * CHROMA_SCALE,
            mastering.display_primaries_r_y as f32 * CHROMA_SCALE,
        ],
        display_primary_green: [
            mastering.display_primaries_g_x as f32 * CHROMA_SCALE,
            mastering.display_primaries_g_y as f32 * CHROMA_SCALE,
        ],
        display_primary_blue: [
            mastering.display_primaries_b_x as f32 * CHROMA_SCALE,
            mastering.display_primaries_b_y as f32 * CHROMA_SCALE,
        ],
        white_point: [
            mastering.white_point_x as f32 * CHROMA_SCALE,
            mastering.white_point_y as f32 * CHROMA_SCALE,
        ],
        min_luminance_cd_m2: mastering.min_luminance as f32 * LUM_SCALE,
        max_luminance_cd_m2: mastering.max_luminance as f32 * LUM_SCALE,
        max_content_light_level: content_light.max_cll as f32,
        max_frame_average_light_level: content_light.max_fall as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each PQ/HLG variant routes to its dedicated shader id; the
    /// SDR triple maps to `Bt709`; sRGB stays its own id; uncommon
    /// curves fall through to `Linear`. Mentally revert any specific
    /// arm to `_ => TransferId::Linear` and the targeted assertion
    /// catches it.
    #[test]
    fn transfer_id_from_schema_covers_hdr_and_sdr_arms() {
        assert_eq!(transfer_id_from_schema(&Transfer::Srgb), TransferId::Srgb);
        assert_eq!(transfer_id_from_schema(&Transfer::Bt709), TransferId::Bt709);
        assert_eq!(
            transfer_id_from_schema(&Transfer::Smpte170m),
            TransferId::Bt709
        );
        assert_eq!(
            transfer_id_from_schema(&Transfer::Bt2020TenBit),
            TransferId::Bt709
        );
        assert_eq!(
            transfer_id_from_schema(&Transfer::Bt2020TwelveBit),
            TransferId::Bt709
        );
        assert_eq!(
            transfer_id_from_schema(&Transfer::Smpte2084),
            TransferId::Pq
        );
        assert_eq!(
            transfer_id_from_schema(&Transfer::AribStdB67),
            TransferId::Hlg
        );
        assert_eq!(
            transfer_id_from_schema(&Transfer::Linear),
            TransferId::Linear
        );
        // Fallthrough arm — uncommon curves map to Linear.
        assert_eq!(
            transfer_id_from_schema(&Transfer::Gamma22),
            TransferId::Linear
        );
        assert_eq!(
            transfer_id_from_schema(&Transfer::Xvycc),
            TransferId::Linear
        );
    }

    /// Every schema variant of Primaries / Matrix / Range maps 1:1 to
    /// its engine ID — these are exhaustive translations with no
    /// fallthrough. A new schema variant added without extending the
    /// helper is a non-exhaustive-match compile error.
    #[test]
    fn primaries_matrix_range_are_total_translations() {
        assert_eq!(
            primaries_id_from_schema(&Primaries::Bt2020),
            PrimariesId::Bt2020
        );
        assert_eq!(
            primaries_id_from_schema(&Primaries::Smpte432),
            PrimariesId::Smpte432
        );
        assert_eq!(matrix_id_from_schema(&Matrix::Bt709), MatrixId::Bt709);
        assert_eq!(
            matrix_id_from_schema(&Matrix::Bt2020Ncl),
            MatrixId::Bt2020Ncl
        );
        assert_eq!(matrix_id_from_schema(&Matrix::Ictcp), MatrixId::Ictcp);
        assert_eq!(range_id_from_schema(&Range::Limited), RangeId::Limited);
        assert_eq!(range_id_from_schema(&Range::Full), RangeId::Full);
    }

    /// `ColorTraits` projects only the axes the swapchain picker
    /// uses. Matrix and range fields on the input are dropped on the
    /// floor. Mentally swap the matrix → primaries assignment in the
    /// projection and the picker would mis-trigger HDR on YUV-only
    /// signals.
    #[test]
    fn color_traits_projects_primaries_and_transfer() {
        let info = ColorInfo {
            primaries: Some(Primaries::Bt2020),
            transfer: Some(Transfer::Smpte2084),
            matrix: Some(Matrix::Bt2020Ncl),
            range: Some(Range::Limited),
        };
        let traits = color_traits_from_color_info(&info);
        assert_eq!(traits.primaries, Some(PrimariesId::Bt2020));
        assert_eq!(traits.transfer, Some(TransferId::Pq));
    }

    /// All-None `ColorInfo` projects to all-None `ColorTraits` — the
    /// swapchain picker stays on the SDR fallback path.
    #[test]
    fn color_traits_from_all_none_info_is_all_none() {
        let info = ColorInfo::default();
        let traits = color_traits_from_color_info(&info);
        assert_eq!(traits.primaries, None);
        assert_eq!(traits.transfer, None);
    }

    /// Canonical HDR10 mastering display + content light values
    /// translate to the corresponding cd/m² and CIE-xy floats with
    /// the right per-axis scale: 1/50000 for chromaticity, 1/10000
    /// for luminance, 1.0 for content light. A silent unit-scale bug
    /// here would propagate wrong HDR metadata to the driver.
    #[test]
    fn hdr_metadata_unit_scaling_matches_canonical_values() {
        let mastering = MasteringDisplay {
            // BT.2020 primaries scaled to 1/50000 increments.
            display_primaries_r_x: 35_400,
            display_primaries_r_y: 14_600,
            display_primaries_g_x: 8_500,
            display_primaries_g_y: 39_850,
            display_primaries_b_x: 6_550,
            display_primaries_b_y: 2_300,
            // D65 white point.
            white_point_x: 15_635,
            white_point_y: 16_450,
            // 0.005 cd/m² floor → 50.
            min_luminance: 50,
            // 1000 cd/m² peak → 10_000_000.
            max_luminance: 10_000_000,
        };
        let content_light = ContentLight {
            max_cll: 1000,
            max_fall: 400,
        };
        let md = hdr_metadata_from_schema(&mastering, &content_light);

        let eps = 1e-4;
        assert!((md.display_primary_red[0] - 0.708).abs() < eps);
        assert!((md.display_primary_red[1] - 0.292).abs() < eps);
        assert!((md.display_primary_green[0] - 0.170).abs() < eps);
        assert!((md.display_primary_green[1] - 0.797).abs() < eps);
        assert!((md.display_primary_blue[0] - 0.131).abs() < eps);
        assert!((md.display_primary_blue[1] - 0.046).abs() < eps);
        assert!((md.white_point[0] - 0.3127).abs() < eps);
        assert!((md.white_point[1] - 0.3290).abs() < eps);
        assert!((md.max_luminance_cd_m2 - 1000.0).abs() < 1e-3);
        assert!((md.min_luminance_cd_m2 - 0.005).abs() < 1e-6);
        assert!((md.max_content_light_level - 1000.0).abs() < 1e-3);
        assert!((md.max_frame_average_light_level - 400.0).abs() < 1e-3);
    }
}
