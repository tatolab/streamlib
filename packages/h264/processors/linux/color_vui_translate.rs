// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Translation between the engine-free `ColorInfo` domain enums (from
//! `@tatolab/core`'s JTD codegen) and the codec-native H.273 byte
//! representation the hardware encode/decode surface exchanges over the
//! plugin ABI:
//!
//! - **Encode:** `ColorInfo` → [`H273ColorVuiRepr`] (the `color_vui` field
//!   of the encoder-session descriptor). Each axis flattens to a `value` +
//!   `present` byte pair; an all-absent repr reads host-side as "no VUI".
//! - **Decode:** the SDK's [`DecodedColorVui`] (parsed SPS VUI, each axis
//!   `Option`) → `ColorInfo` for surfacing on `VideoFrame.color_info`.
//!
//! The raw H.273 enumerant byte values (ITU-T H.273 / ISO/IEC 23091-2 — the
//! representation that appears verbatim in the H.264 / H.265 bitstream) are
//! defined locally: the engine-free codec package no longer links the
//! engine's `color_vui` module, and these standardized enumerants are frozen
//! forever. Both H.264 and H.265 packages carry an identical translator
//! because each has its own codegen module (`crate::_generated_::*`).

use crate::_generated_::tatolab__core::color_info::{
    ColorInfo, Matrix, Primaries, Range, Transfer,
};
use streamlib_plugin_abi::H273ColorVuiRepr;
use streamlib_plugin_sdk::sdk::rhi::DecodedColorVui;

// ---------------------------------------------------------------------------
// H.273 enumerant constants (ITU-T H.273 / ISO/IEC 23091-2)
// ---------------------------------------------------------------------------
//
// The subset streamlib's `ColorInfo` schema declares — full H.273 tables are
// larger. Values are frozen by the spec.

/// ColourPrimaries values — H.273 §8.1.
mod primaries {
    pub const BT709: u8 = 1;
    pub const BT470_M: u8 = 4;
    pub const BT470_BG: u8 = 5;
    pub const SMPTE170M: u8 = 6;
    pub const SMPTE240M: u8 = 7;
    pub const FILM: u8 = 8;
    pub const BT2020: u8 = 9;
    pub const SMPTE428: u8 = 10;
    pub const SMPTE431: u8 = 11;
    pub const SMPTE432: u8 = 12;
    pub const EBU3213: u8 = 22;
}

/// TransferCharacteristics values — H.273 §8.2.
mod transfer {
    pub const BT709: u8 = 1;
    pub const GAMMA22: u8 = 4;
    pub const GAMMA28: u8 = 5;
    pub const SMPTE170M: u8 = 6;
    pub const SMPTE240M: u8 = 7;
    pub const LINEAR: u8 = 8;
    pub const LOG100: u8 = 9;
    pub const LOG100_SQRT10: u8 = 10;
    pub const XVYCC: u8 = 11;
    pub const BT1361: u8 = 12;
    pub const SRGB: u8 = 13;
    pub const BT2020_TEN_BIT: u8 = 14;
    pub const BT2020_TWELVE_BIT: u8 = 15;
    pub const SMPTE2084: u8 = 16;
    pub const SMPTE428: u8 = 17;
    pub const ARIB_STD_B67: u8 = 18;
}

/// MatrixCoefficients values — H.273 §8.3.
mod matrix {
    pub const IDENTITY: u8 = 0;
    pub const BT709: u8 = 1;
    pub const FCC: u8 = 4;
    pub const BT470_BG: u8 = 5;
    pub const SMPTE170M: u8 = 6;
    pub const SMPTE240M: u8 = 7;
    pub const YCGCO: u8 = 8;
    pub const BT2020_NCL: u8 = 9;
    pub const BT2020_CL: u8 = 10;
    pub const SMPTE2085: u8 = 11;
    pub const CHROMA_NCL: u8 = 12;
    pub const CHROMA_CL: u8 = 13;
    pub const ICTCP: u8 = 14;
}

/// Flatten an `Option<u8>` H.273 axis into the repr's `(value, present)`
/// byte pair. Absent → `(0, 0)`; the host reads `present == 0` as "axis
/// Unspecified".
fn axis(value: Option<u8>) -> (u8, u8) {
    match value {
        Some(v) => (v, 1),
        None => (0, 0),
    }
}

/// Translate `ColorInfo` → [`H273ColorVuiRepr`] for the encoder-session
/// descriptor's SPS VUI chain.
///
/// An all-absent repr (every `*_present` byte 0) is what the host reads as
/// "no colour_description block" — the encoder emits no VUI rather than an
/// empty one, so no separate `Option` wrapper is needed.
pub fn color_info_to_h273_repr(info: &ColorInfo) -> H273ColorVuiRepr {
    let (primaries, primaries_present) = axis(info.primaries.as_ref().map(primaries_to_byte));
    let (transfer, transfer_present) = axis(info.transfer.as_ref().map(transfer_to_byte));
    let (matrix, matrix_present) = axis(info.matrix.as_ref().map(matrix_to_byte));
    let (full_range, full_range_present) =
        axis(info.range.as_ref().map(|r| u8::from(matches!(r, Range::Full))));
    H273ColorVuiRepr {
        primaries,
        primaries_present,
        transfer,
        transfer_present,
        matrix,
        matrix_present,
        full_range,
        full_range_present,
    }
}

/// Translate the SDK's parsed [`DecodedColorVui`] → `ColorInfo` for surfacing
/// decoded VUI metadata on `VideoFrame.color_info`. Per-axis `None`
/// (Unspecified on the wire) stays `None`; an H.273 enumerant streamlib
/// doesn't model decodes to `None` rather than fabricating a variant.
pub fn decoded_vui_to_color_info(vui: &DecodedColorVui) -> ColorInfo {
    ColorInfo {
        primaries: vui.primaries.and_then(primaries_from_byte),
        transfer: vui.transfer.and_then(transfer_from_byte),
        matrix: vui.matrix.and_then(matrix_from_byte),
        range: vui.full_range.map(|f| if f { Range::Full } else { Range::Limited }),
    }
}

fn primaries_to_byte(p: &Primaries) -> u8 {
    match p {
        Primaries::Bt709 => primaries::BT709,
        Primaries::Bt470M => primaries::BT470_M,
        Primaries::Bt470Bg => primaries::BT470_BG,
        Primaries::Smpte170m => primaries::SMPTE170M,
        Primaries::Smpte240m => primaries::SMPTE240M,
        Primaries::Film => primaries::FILM,
        Primaries::Bt2020 => primaries::BT2020,
        Primaries::Smpte428 => primaries::SMPTE428,
        Primaries::Smpte431 => primaries::SMPTE431,
        Primaries::Smpte432 => primaries::SMPTE432,
        Primaries::Ebu3213 => primaries::EBU3213,
    }
}

fn primaries_from_byte(b: u8) -> Option<Primaries> {
    Some(match b {
        x if x == primaries::BT709 => Primaries::Bt709,
        x if x == primaries::BT470_M => Primaries::Bt470M,
        x if x == primaries::BT470_BG => Primaries::Bt470Bg,
        x if x == primaries::SMPTE170M => Primaries::Smpte170m,
        x if x == primaries::SMPTE240M => Primaries::Smpte240m,
        x if x == primaries::FILM => Primaries::Film,
        x if x == primaries::BT2020 => Primaries::Bt2020,
        x if x == primaries::SMPTE428 => Primaries::Smpte428,
        x if x == primaries::SMPTE431 => Primaries::Smpte431,
        x if x == primaries::SMPTE432 => Primaries::Smpte432,
        x if x == primaries::EBU3213 => Primaries::Ebu3213,
        _ => return None,
    })
}

fn transfer_to_byte(t: &Transfer) -> u8 {
    match t {
        Transfer::Bt709 => transfer::BT709,
        Transfer::Gamma22 => transfer::GAMMA22,
        Transfer::Gamma28 => transfer::GAMMA28,
        Transfer::Smpte170m => transfer::SMPTE170M,
        Transfer::Smpte240m => transfer::SMPTE240M,
        Transfer::Linear => transfer::LINEAR,
        Transfer::Log100 => transfer::LOG100,
        Transfer::Log100Sqrt10 => transfer::LOG100_SQRT10,
        Transfer::Xvycc => transfer::XVYCC,
        Transfer::Bt1361 => transfer::BT1361,
        Transfer::Srgb => transfer::SRGB,
        Transfer::Bt2020TenBit => transfer::BT2020_TEN_BIT,
        Transfer::Bt2020TwelveBit => transfer::BT2020_TWELVE_BIT,
        Transfer::Smpte2084 => transfer::SMPTE2084,
        Transfer::Smpte428 => transfer::SMPTE428,
        Transfer::AribStdB67 => transfer::ARIB_STD_B67,
    }
}

fn transfer_from_byte(b: u8) -> Option<Transfer> {
    Some(match b {
        x if x == transfer::BT709 => Transfer::Bt709,
        x if x == transfer::GAMMA22 => Transfer::Gamma22,
        x if x == transfer::GAMMA28 => Transfer::Gamma28,
        x if x == transfer::SMPTE170M => Transfer::Smpte170m,
        x if x == transfer::SMPTE240M => Transfer::Smpte240m,
        x if x == transfer::LINEAR => Transfer::Linear,
        x if x == transfer::LOG100 => Transfer::Log100,
        x if x == transfer::LOG100_SQRT10 => Transfer::Log100Sqrt10,
        x if x == transfer::XVYCC => Transfer::Xvycc,
        x if x == transfer::BT1361 => Transfer::Bt1361,
        x if x == transfer::SRGB => Transfer::Srgb,
        x if x == transfer::BT2020_TEN_BIT => Transfer::Bt2020TenBit,
        x if x == transfer::BT2020_TWELVE_BIT => Transfer::Bt2020TwelveBit,
        x if x == transfer::SMPTE2084 => Transfer::Smpte2084,
        x if x == transfer::SMPTE428 => Transfer::Smpte428,
        x if x == transfer::ARIB_STD_B67 => Transfer::AribStdB67,
        _ => return None,
    })
}

fn matrix_to_byte(m: &Matrix) -> u8 {
    match m {
        Matrix::Identity => matrix::IDENTITY,
        Matrix::Bt709 => matrix::BT709,
        Matrix::Fcc => matrix::FCC,
        Matrix::Bt470Bg => matrix::BT470_BG,
        Matrix::Smpte170m => matrix::SMPTE170M,
        Matrix::Smpte240m => matrix::SMPTE240M,
        Matrix::Ycgco => matrix::YCGCO,
        Matrix::Bt2020Ncl => matrix::BT2020_NCL,
        Matrix::Bt2020Cl => matrix::BT2020_CL,
        Matrix::Smpte2085 => matrix::SMPTE2085,
        Matrix::ChromaNcl => matrix::CHROMA_NCL,
        Matrix::ChromaCl => matrix::CHROMA_CL,
        Matrix::Ictcp => matrix::ICTCP,
    }
}

fn matrix_from_byte(b: u8) -> Option<Matrix> {
    Some(match b {
        x if x == matrix::IDENTITY => Matrix::Identity,
        x if x == matrix::BT709 => Matrix::Bt709,
        x if x == matrix::FCC => Matrix::Fcc,
        x if x == matrix::BT470_BG => Matrix::Bt470Bg,
        x if x == matrix::SMPTE170M => Matrix::Smpte170m,
        x if x == matrix::SMPTE240M => Matrix::Smpte240m,
        x if x == matrix::YCGCO => Matrix::Ycgco,
        x if x == matrix::BT2020_NCL => Matrix::Bt2020Ncl,
        x if x == matrix::BT2020_CL => Matrix::Bt2020Cl,
        x if x == matrix::SMPTE2085 => Matrix::Smpte2085,
        x if x == matrix::CHROMA_NCL => Matrix::ChromaNcl,
        x if x == matrix::CHROMA_CL => Matrix::ChromaCl,
        x if x == matrix::ICTCP => Matrix::Ictcp,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `DecodedColorVui` the way the decoder session would from a
    /// repr — used to exercise the decode-side translation without a GPU.
    fn decoded(
        primaries: Option<u8>,
        transfer: Option<u8>,
        matrix: Option<u8>,
        full_range: Option<bool>,
    ) -> DecodedColorVui {
        DecodedColorVui {
            primaries,
            transfer,
            matrix,
            full_range,
        }
    }

    #[test]
    fn full_color_info_round_trip() {
        let info = ColorInfo {
            primaries: Some(Primaries::Bt709),
            transfer: Some(Transfer::Srgb),
            matrix: Some(Matrix::Smpte170m),
            range: Some(Range::Full),
        };
        let repr = color_info_to_h273_repr(&info);
        assert_eq!((repr.primaries, repr.primaries_present), (1, 1));
        assert_eq!((repr.transfer, repr.transfer_present), (13, 1));
        assert_eq!((repr.matrix, repr.matrix_present), (6, 1));
        assert_eq!((repr.full_range, repr.full_range_present), (1, 1));
        // Repr axes → DecodedColorVui (the shape the session emits) → back.
        let back = decoded_vui_to_color_info(&decoded(Some(1), Some(13), Some(6), Some(true)));
        assert_eq!(back, info);
    }

    #[test]
    fn all_none_yields_absent_repr() {
        let repr = color_info_to_h273_repr(&ColorInfo::default());
        assert_eq!(repr.primaries_present, 0);
        assert_eq!(repr.transfer_present, 0);
        assert_eq!(repr.matrix_present, 0);
        assert_eq!(repr.full_range_present, 0);
    }

    #[test]
    fn range_alone_still_present_in_repr() {
        let info = ColorInfo {
            range: Some(Range::Limited),
            ..Default::default()
        };
        let repr = color_info_to_h273_repr(&info);
        assert_eq!((repr.full_range, repr.full_range_present), (0, 1));
        assert_eq!(repr.primaries_present, 0);
    }

    #[test]
    fn hdr10_round_trip() {
        // The canonical HDR10 four-tuple: BT.2020 / PQ / BT.2020-NCL / Full.
        let info = ColorInfo {
            primaries: Some(Primaries::Bt2020),
            transfer: Some(Transfer::Smpte2084),
            matrix: Some(Matrix::Bt2020Ncl),
            range: Some(Range::Full),
        };
        let repr = color_info_to_h273_repr(&info);
        assert_eq!((repr.primaries, repr.primaries_present), (9, 1));
        assert_eq!((repr.transfer, repr.transfer_present), (16, 1));
        assert_eq!((repr.matrix, repr.matrix_present), (9, 1));
        assert_eq!((repr.full_range, repr.full_range_present), (1, 1));
        let back = decoded_vui_to_color_info(&decoded(Some(9), Some(16), Some(9), Some(true)));
        assert_eq!(back, info);
    }

    #[test]
    fn unknown_byte_decodes_to_none() {
        // A decoder seeing H.273 enumerants streamlib doesn't model (e.g. 99)
        // drops them silently rather than fabricating variants.
        let info = decoded_vui_to_color_info(&decoded(Some(99), Some(13), Some(99), Some(false)));
        assert!(info.primaries.is_none());
        assert_eq!(info.transfer, Some(Transfer::Srgb));
        assert!(info.matrix.is_none());
        assert_eq!(info.range, Some(Range::Limited));
    }
}
