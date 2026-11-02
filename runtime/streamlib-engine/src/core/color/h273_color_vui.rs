// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.273 / VUI color metadata, and the one table that maps its enumerant bytes
//! onto the engine's color ids.
//!
//! The vocabulary itself is [`streamlib_consumer_rhi::h273_color_vui`], shared
//! with every codec and capture path outside the engine.

pub use streamlib_consumer_rhi::h273_color_vui::{
    H273_UNSPECIFIED, H273ColorVui, color_vui_every_engine_encoder_writes, matrix, primaries,
    transfer,
};

use super::ResolvedColorInfo;
use super::resolve::resolve_color_defaults;
use super::resolved::{ColorSpaceKind, MatrixId, PrimariesId, RangeId};
use super::transfer::TransferId;

/// An [`H273ColorVui`] resolved onto the engine's color ids.
pub trait ResolveH273ColorVuiDefaults {
    /// The fully resolved description the engine's color kernels take: every
    /// axis this description leaves absent, or names with a byte the engine
    /// has no id for, filled with `resolve_color_defaults`' answer for a
    /// source of `kind`.
    fn resolve_defaults(&self, kind: ColorSpaceKind) -> ResolvedColorInfo;
}

impl ResolveH273ColorVuiDefaults for H273ColorVui {
    fn resolve_defaults(&self, kind: ColorSpaceKind) -> ResolvedColorInfo {
        resolve_color_defaults(
            self.primaries.and_then(primaries_id_from_h273_byte),
            self.transfer.and_then(transfer_id_from_h273_byte),
            self.matrix.and_then(matrix_id_from_h273_byte),
            self.full_range.map(range_id_from_h273_full_range_flag),
            kind,
        )
    }
}

/// The engine primaries id an H.273 `ColourPrimaries` byte names, `None` for
/// Unspecified and for any value the engine has no id for.
fn primaries_id_from_h273_byte(byte: u8) -> Option<PrimariesId> {
    Some(match byte {
        primaries::BT709 => PrimariesId::Bt709,
        primaries::BT470_M => PrimariesId::Bt470M,
        primaries::BT470_BG => PrimariesId::Bt470Bg,
        primaries::SMPTE170M => PrimariesId::Smpte170m,
        primaries::SMPTE240M => PrimariesId::Smpte240m,
        primaries::FILM => PrimariesId::Film,
        primaries::BT2020 => PrimariesId::Bt2020,
        primaries::SMPTE428 => PrimariesId::Smpte428,
        primaries::SMPTE431 => PrimariesId::Smpte431,
        primaries::SMPTE432 => PrimariesId::Smpte432,
        primaries::EBU3213 => PrimariesId::Ebu3213,
        _ => return None,
    })
}

/// The engine transfer id an H.273 `TransferCharacteristics` byte decodes
/// with, `None` for Unspecified and for any value outside the H.273 set.
///
/// The engine's kernels implement five curves, so most encoded transfers
/// share one: every SDR camera curve decodes as BT.709, and the encoded
/// transfers with no exact engine curve take the BT.709 shape too, because
/// `Linear` would skip decoding entirely.
fn transfer_id_from_h273_byte(byte: u8) -> Option<TransferId> {
    Some(match byte {
        transfer::SRGB => TransferId::Srgb,
        transfer::BT709
        | transfer::SMPTE170M
        | transfer::SMPTE240M
        | transfer::BT2020_TEN_BIT
        | transfer::BT2020_TWELVE_BIT => TransferId::Bt709,
        transfer::SMPTE2084 => TransferId::Pq,
        transfer::ARIB_STD_B67 => TransferId::Hlg,
        transfer::LINEAR => TransferId::Linear,
        transfer::GAMMA22
        | transfer::GAMMA28
        | transfer::BT1361
        | transfer::LOG100
        | transfer::LOG100_SQRT10
        | transfer::SMPTE428
        | transfer::XVYCC => TransferId::Bt709,
        _ => return None,
    })
}

/// The engine matrix id an H.273 `MatrixCoefficients` byte names, `None` for
/// Unspecified and for any value the engine has no id for.
fn matrix_id_from_h273_byte(byte: u8) -> Option<MatrixId> {
    Some(match byte {
        matrix::IDENTITY => MatrixId::Identity,
        matrix::BT709 => MatrixId::Bt709,
        matrix::FCC => MatrixId::Fcc,
        matrix::BT470_BG => MatrixId::Bt470Bg,
        matrix::SMPTE170M => MatrixId::Smpte170m,
        matrix::SMPTE240M => MatrixId::Smpte240m,
        matrix::YCGCO => MatrixId::Ycgco,
        matrix::BT2020_NCL => MatrixId::Bt2020Ncl,
        matrix::BT2020_CL => MatrixId::Bt2020Cl,
        matrix::SMPTE2085 => MatrixId::Smpte2085,
        matrix::CHROMA_NCL => MatrixId::ChromaNcl,
        matrix::CHROMA_CL => MatrixId::ChromaCl,
        matrix::ICTCP => MatrixId::Ictcp,
        _ => return None,
    })
}

/// The engine range id a `video_full_range_flag` names.
fn range_id_from_h273_full_range_flag(full_range: bool) -> RangeId {
    if full_range {
        RangeId::Full
    } else {
        RangeId::Limited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unspecified is how H.273 spells an absent axis, so it must resolve
    /// exactly as an absent one does rather than landing on an id.
    #[test]
    fn unspecified_names_no_engine_id_on_any_axis() {
        assert_eq!(primaries_id_from_h273_byte(primaries::UNSPECIFIED), None);
        assert_eq!(transfer_id_from_h273_byte(transfer::UNSPECIFIED), None);
        assert_eq!(matrix_id_from_h273_byte(matrix::UNSPECIFIED), None);
    }

    #[test]
    fn a_byte_outside_the_h273_set_names_no_engine_id() {
        assert_eq!(primaries_id_from_h273_byte(200), None);
        assert_eq!(transfer_id_from_h273_byte(200), None);
        assert_eq!(matrix_id_from_h273_byte(200), None);
    }

    #[test]
    fn an_absent_description_resolves_to_the_per_kind_defaults() {
        for kind in [ColorSpaceKind::Rgb, ColorSpaceKind::Yuv] {
            assert_eq!(
                H273ColorVui::default().resolve_defaults(kind),
                resolve_color_defaults(None, None, None, None, kind)
            );
        }
    }

    #[test]
    fn an_hdr10_description_resolves_to_pq_over_bt2020() {
        let resolved = H273ColorVui {
            primaries: Some(primaries::BT2020),
            transfer: Some(transfer::SMPTE2084),
            matrix: Some(matrix::BT2020_NCL),
            full_range: Some(false),
        }
        .resolve_defaults(ColorSpaceKind::Yuv);
        assert_eq!(
            resolved,
            ResolvedColorInfo {
                primaries: PrimariesId::Bt2020,
                transfer: TransferId::Pq,
                matrix: MatrixId::Bt2020Ncl,
                range: RangeId::Limited,
            }
        );
    }

    #[test]
    fn an_encoder_writes_bt709_limited_for_a_test_pattern_frame_that_names_no_matrix() {
        let test_pattern_source = H273ColorVui {
            primaries: Some(primaries::BT709),
            transfer: Some(transfer::SRGB),
            matrix: None,
            full_range: Some(true),
        };
        let written = color_vui_every_engine_encoder_writes(Some(test_pattern_source));
        assert_eq!(
            written,
            H273ColorVui {
                primaries: Some(primaries::BT709),
                transfer: Some(transfer::SRGB),
                matrix: Some(matrix::BT709),
                full_range: Some(false),
            }
        );
        let resolved = written.resolve_defaults(ColorSpaceKind::Yuv);
        assert_eq!(
            (resolved.matrix, resolved.range),
            (MatrixId::Bt709, RangeId::Limited)
        );
    }
}
