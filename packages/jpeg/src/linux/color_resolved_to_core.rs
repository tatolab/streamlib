// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Translate engine-side [`ResolvedColorInfo`] into the on-wire
//! [`ColorInfo`] schema produced by `@tatolab/core` JTD codegen.
//!
//! The engine carries colorimetry as `*Id` enums (PrimariesId,
//! TransferId, MatrixId, RangeId) — every variant maps 1:1 to a
//! wire-schema enumerant. `JpegDecodeOutput.color_info` is always a
//! fully-resolved 4-tuple (no Unspecified), so the wire `ColorInfo`
//! emitted here always has every axis populated.

use crate::_generated_::tatolab__core::color_info::{
    ColorInfo, Matrix, Primaries, Range, Transfer,
};
use streamlib_plugin_sdk::sdk::color::{MatrixId, PrimariesId, RangeId, ResolvedColorInfo, TransferId};

/// Convert a fully-resolved engine color tuple into the on-wire
/// `ColorInfo` schema. Every axis is `Some(_)` since
/// [`ResolvedColorInfo`] carries no Unspecified state.
pub fn resolved_color_info_to_core(resolved: &ResolvedColorInfo) -> ColorInfo {
    ColorInfo {
        primaries: Some(primaries_from_id(resolved.primaries)),
        transfer: Some(transfer_from_id(resolved.transfer)),
        matrix: Some(matrix_from_id(resolved.matrix)),
        range: Some(range_from_id(resolved.range)),
    }
}

fn primaries_from_id(id: PrimariesId) -> Primaries {
    match id {
        PrimariesId::Bt709 => Primaries::Bt709,
        PrimariesId::Bt470M => Primaries::Bt470M,
        PrimariesId::Bt470Bg => Primaries::Bt470Bg,
        PrimariesId::Smpte170m => Primaries::Smpte170m,
        PrimariesId::Smpte240m => Primaries::Smpte240m,
        PrimariesId::Film => Primaries::Film,
        PrimariesId::Bt2020 => Primaries::Bt2020,
        PrimariesId::Smpte428 => Primaries::Smpte428,
        PrimariesId::Smpte431 => Primaries::Smpte431,
        PrimariesId::Smpte432 => Primaries::Smpte432,
        PrimariesId::Ebu3213 => Primaries::Ebu3213,
    }
}

fn transfer_from_id(id: TransferId) -> Transfer {
    match id {
        TransferId::Linear => Transfer::Linear,
        TransferId::Srgb => Transfer::Srgb,
        TransferId::Bt709 => Transfer::Bt709,
        TransferId::Pq => Transfer::Smpte2084,
        TransferId::Hlg => Transfer::AribStdB67,
    }
}

fn matrix_from_id(id: MatrixId) -> Matrix {
    match id {
        MatrixId::Identity => Matrix::Identity,
        MatrixId::Bt709 => Matrix::Bt709,
        MatrixId::Fcc => Matrix::Fcc,
        MatrixId::Bt470Bg => Matrix::Bt470Bg,
        MatrixId::Smpte170m => Matrix::Smpte170m,
        MatrixId::Smpte240m => Matrix::Smpte240m,
        MatrixId::Ycgco => Matrix::Ycgco,
        MatrixId::Bt2020Ncl => Matrix::Bt2020Ncl,
        MatrixId::Bt2020Cl => Matrix::Bt2020Cl,
        MatrixId::Smpte2085 => Matrix::Smpte2085,
        MatrixId::ChromaNcl => Matrix::ChromaNcl,
        MatrixId::ChromaCl => Matrix::ChromaCl,
        MatrixId::Ictcp => Matrix::Ictcp,
    }
}

fn range_from_id(id: RangeId) -> Range {
    match id {
        RangeId::Limited => Range::Limited,
        RangeId::Full => Range::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jfif_default_maps_to_bt709_srgb_smpte170m_full() {
        // JFIF default per plugin/vulkan-jpeg/src/color.rs::JFIF_DEFAULT.
        let resolved = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Srgb,
            matrix: MatrixId::Smpte170m,
            range: RangeId::Full,
        };
        assert_eq!(
            resolved_color_info_to_core(&resolved),
            ColorInfo {
                primaries: Some(Primaries::Bt709),
                transfer: Some(Transfer::Srgb),
                matrix: Some(Matrix::Smpte170m),
                range: Some(Range::Full),
            }
        );
    }

    #[test]
    fn adobe_rgb_direct_maps_to_identity_full() {
        // APP14 transform=0 — Adobe RGB direct (no YCbCr matrix).
        let resolved = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Srgb,
            matrix: MatrixId::Identity,
            range: RangeId::Full,
        };
        let info = resolved_color_info_to_core(&resolved);
        assert_eq!(info.matrix, Some(Matrix::Identity));
        assert_eq!(info.range, Some(Range::Full));
    }

    #[test]
    fn hdr_pq_round_trip_through_smpte2084() {
        // Confirms the PQ ↔ Smpte2084 alias since the engine TransferId
        // is a smaller enum than the wire Transfer.
        let resolved = ResolvedColorInfo {
            primaries: PrimariesId::Bt2020,
            transfer: TransferId::Pq,
            matrix: MatrixId::Bt2020Ncl,
            range: RangeId::Limited,
        };
        let info = resolved_color_info_to_core(&resolved);
        assert_eq!(info.primaries, Some(Primaries::Bt2020));
        assert_eq!(info.transfer, Some(Transfer::Smpte2084));
        assert_eq!(info.matrix, Some(Matrix::Bt2020Ncl));
        assert_eq!(info.range, Some(Range::Limited));
    }

    #[test]
    fn hlg_maps_to_arib_std_b67() {
        let resolved = ResolvedColorInfo {
            primaries: PrimariesId::Bt2020,
            transfer: TransferId::Hlg,
            matrix: MatrixId::Bt2020Ncl,
            range: RangeId::Full,
        };
        assert_eq!(
            resolved_color_info_to_core(&resolved).transfer,
            Some(Transfer::AribStdB67),
        );
    }
}
