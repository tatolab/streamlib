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

/// `ColorInfo.primaries` names and the H.273 byte each spells.
const PRIMARIES_NAMES: [(&str, u8); 11] = [
    ("bt709", primaries::BT709),
    ("bt470_m", primaries::BT470_M),
    ("bt470_bg", primaries::BT470_BG),
    ("smpte170m", primaries::SMPTE170M),
    ("smpte240m", primaries::SMPTE240M),
    ("film", primaries::FILM),
    ("bt2020", primaries::BT2020),
    ("smpte428", primaries::SMPTE428),
    ("smpte431", primaries::SMPTE431),
    ("smpte432", primaries::SMPTE432),
    ("ebu3213", primaries::EBU3213),
];

/// `ColorInfo.transfer` names and the H.273 byte each spells.
const TRANSFER_NAMES: [(&str, u8); 16] = [
    ("bt709", transfer::BT709),
    ("gamma22", transfer::GAMMA22),
    ("gamma28", transfer::GAMMA28),
    ("smpte170m", transfer::SMPTE170M),
    ("smpte240m", transfer::SMPTE240M),
    ("linear", transfer::LINEAR),
    ("log100", transfer::LOG100),
    ("log100_sqrt10", transfer::LOG100_SQRT10),
    ("xvycc", transfer::XVYCC),
    ("bt1361", transfer::BT1361),
    ("srgb", transfer::SRGB),
    ("bt2020_ten_bit", transfer::BT2020_TEN_BIT),
    ("bt2020_twelve_bit", transfer::BT2020_TWELVE_BIT),
    ("smpte2084", transfer::SMPTE2084),
    ("smpte428", transfer::SMPTE428),
    ("arib_std_b67", transfer::ARIB_STD_B67),
];

/// `ColorInfo.matrix` names and the H.273 byte each spells.
const MATRIX_NAMES: [(&str, u8); 13] = [
    ("identity", matrix::IDENTITY),
    ("bt709", matrix::BT709),
    ("fcc", matrix::FCC),
    ("bt470_bg", matrix::BT470_BG),
    ("smpte170m", matrix::SMPTE170M),
    ("smpte240m", matrix::SMPTE240M),
    ("ycgco", matrix::YCGCO),
    ("bt2020_ncl", matrix::BT2020_NCL),
    ("bt2020_cl", matrix::BT2020_CL),
    ("smpte2085", matrix::SMPTE2085),
    ("chroma_ncl", matrix::CHROMA_NCL),
    ("chroma_cl", matrix::CHROMA_CL),
    ("ictcp", matrix::ICTCP),
];

/// The H.273 byte `name` spells on `axis`, refusing a name the axis lacks.
fn h273_byte_of_axis_name(
    axis: &str,
    names: &[(&str, u8)],
    name: Option<&str>,
) -> crate::core::Result<Option<u8>> {
    let Some(name) = name else {
        return Ok(None);
    };
    names
        .iter()
        .find(|(known_name, _)| *known_name == name)
        .map(|(_, byte)| Some(*byte))
        .ok_or_else(|| {
            let known_names: Vec<&str> = names.iter().map(|(known_name, _)| *known_name).collect();
            crate::core::Error::Configuration(format!(
                "{name:?} is not a {axis} name ColorInfo knows; the {axis} names are {}",
                known_names.join(", ")
            ))
        })
}

/// The description `ColorInfo`'s axis names spell — the names
/// `tatolab.stream.video_frame` declares — with an absent axis left
/// unspecified and an unknown name refused.
pub fn h273_color_vui_from_color_info_axis_names(
    primaries_name: Option<&str>,
    transfer_name: Option<&str>,
    matrix_name: Option<&str>,
    range_name: Option<&str>,
) -> crate::core::Result<H273ColorVui> {
    let full_range = match range_name {
        None => None,
        Some("full") => Some(true),
        Some("limited") => Some(false),
        Some(other) => {
            return Err(crate::core::Error::Configuration(format!(
                "{other:?} is not a range name ColorInfo knows; the range names are full and limited"
            )));
        }
    };
    Ok(H273ColorVui {
        primaries: h273_byte_of_axis_name("primaries", &PRIMARIES_NAMES, primaries_name)?,
        transfer: h273_byte_of_axis_name("transfer", &TRANSFER_NAMES, transfer_name)?,
        matrix: h273_byte_of_axis_name("matrix", &MATRIX_NAMES, matrix_name)?,
        full_range,
    })
}

/// The `ColorInfo` axis names `vui` spells — primaries, transfer, matrix
/// and range — with an absent axis, or a byte no name spells, as `None`.
pub fn color_info_axis_names_of_h273_color_vui(vui: &H273ColorVui) -> [Option<&'static str>; 4] {
    let axis_name = |names: &[(&'static str, u8)], byte: Option<u8>| {
        let byte = byte?;
        names
            .iter()
            .find(|(_, known_byte)| *known_byte == byte)
            .map(|(name, _)| *name)
    };
    [
        axis_name(&PRIMARIES_NAMES, vui.primaries),
        axis_name(&TRANSFER_NAMES, vui.transfer),
        axis_name(&MATRIX_NAMES, vui.matrix),
        vui.full_range
            .map(|full_range| if full_range { "full" } else { "limited" }),
    ]
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

    #[test]
    fn color_info_axis_names_spell_their_h273_bytes() {
        let described = h273_color_vui_from_color_info_axis_names(
            Some("bt709"),
            Some("srgb"),
            Some("smpte170m"),
            Some("limited"),
        )
        .expect("every name is one ColorInfo declares");
        assert_eq!(
            described,
            H273ColorVui {
                primaries: Some(primaries::BT709),
                transfer: Some(transfer::SRGB),
                matrix: Some(matrix::SMPTE170M),
                full_range: Some(false),
            }
        );
    }

    #[test]
    fn every_color_info_axis_name_round_trips_through_its_h273_byte() {
        for primaries_name in PRIMARIES_NAMES.map(|(name, _)| name) {
            for (transfer_name, matrix_name) in TRANSFER_NAMES
                .map(|(name, _)| name)
                .into_iter()
                .zip(MATRIX_NAMES.map(|(name, _)| name).into_iter().cycle())
            {
                let names = [
                    Some(primaries_name),
                    Some(transfer_name),
                    Some(matrix_name),
                    Some("full"),
                ];
                let described = h273_color_vui_from_color_info_axis_names(
                    names[0], names[1], names[2], names[3],
                )
                .expect("every name is one ColorInfo declares");
                assert_eq!(color_info_axis_names_of_h273_color_vui(&described), names);
            }
        }
        assert_eq!(
            color_info_axis_names_of_h273_color_vui(&H273ColorVui {
                primaries: Some(primaries::UNSPECIFIED),
                ..H273ColorVui::default()
            }),
            [None; 4]
        );
    }

    #[test]
    fn absent_color_info_axes_stay_unspecified() {
        assert_eq!(
            h273_color_vui_from_color_info_axis_names(None, None, None, None)
                .expect("an absent axis is not a refusal"),
            H273ColorVui::default()
        );
    }

    #[test]
    fn an_unknown_color_info_name_is_refused_naming_its_axis() {
        let refusal = h273_color_vui_from_color_info_axis_names(None, None, Some("rec709"), None)
            .expect_err("rec709 is not a matrix name");
        assert!(refusal.to_string().contains("matrix"), "{refusal}");
        assert!(refusal.to_string().contains("bt709"), "{refusal}");
    }

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
