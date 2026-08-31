// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Translation between the bag vocabulary's [`ColorInfo`] and the codec
//! layer's H.273 byte representation ([`H273ColorVui`], the enumerants that
//! appear verbatim in the H.264 / H.265 bitstream VUI).
//!
//! Encode direction: `ColorInfo` → `H273ColorVui` for the encoder session's
//! SPS VUI. Decode direction: per-axis byte → `ColorInfo` variant for
//! surfacing parsed VUI on decoded frames; an H.273 enumerant the bag
//! vocabulary does not model decodes to `None` rather than fabricating a
//! variant.

use streamlib::sdk::engine::video::H273ColorVui;
use streamlib::sdk::engine::video::color_vui::{matrix, primaries, transfer};

use crate::video_frame::{ColorInfo, Matrix, Primaries, Range, Transfer};

/// Translate a bag's [`ColorInfo`] into the H.273 byte tuple the encoder
/// session bakes into its SPS VUI. Absent axes stay absent; an all-absent
/// result is how "emit no colour_description block" is spelled.
pub fn color_info_to_h273_color_vui(color_info: &ColorInfo) -> H273ColorVui {
    H273ColorVui {
        primaries: color_info.primaries.as_ref().map(primaries_to_h273_byte),
        transfer: color_info.transfer.as_ref().map(transfer_to_h273_byte),
        matrix: color_info.matrix.as_ref().map(matrix_to_h273_byte),
        full_range: color_info
            .range
            .as_ref()
            .map(|range| matches!(range, Range::Full)),
    }
}

/// ColourPrimaries variant → H.273 §8.1 enumerant byte.
pub fn primaries_to_h273_byte(value: &Primaries) -> u8 {
    match value {
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

/// H.273 §8.1 enumerant byte → ColourPrimaries variant, `None` for a value
/// the bag vocabulary does not model (Unspecified included).
pub fn primaries_from_h273_byte(byte: u8) -> Option<Primaries> {
    Some(match byte {
        primaries::BT709 => Primaries::Bt709,
        primaries::BT470_M => Primaries::Bt470M,
        primaries::BT470_BG => Primaries::Bt470Bg,
        primaries::SMPTE170M => Primaries::Smpte170m,
        primaries::SMPTE240M => Primaries::Smpte240m,
        primaries::FILM => Primaries::Film,
        primaries::BT2020 => Primaries::Bt2020,
        primaries::SMPTE428 => Primaries::Smpte428,
        primaries::SMPTE431 => Primaries::Smpte431,
        primaries::SMPTE432 => Primaries::Smpte432,
        primaries::EBU3213 => Primaries::Ebu3213,
        _ => return None,
    })
}

/// TransferCharacteristics variant → H.273 §8.2 enumerant byte.
pub fn transfer_to_h273_byte(value: &Transfer) -> u8 {
    match value {
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

/// H.273 §8.2 enumerant byte → TransferCharacteristics variant, `None` for
/// a value the bag vocabulary does not model.
pub fn transfer_from_h273_byte(byte: u8) -> Option<Transfer> {
    Some(match byte {
        transfer::BT709 => Transfer::Bt709,
        transfer::GAMMA22 => Transfer::Gamma22,
        transfer::GAMMA28 => Transfer::Gamma28,
        transfer::SMPTE170M => Transfer::Smpte170m,
        transfer::SMPTE240M => Transfer::Smpte240m,
        transfer::LINEAR => Transfer::Linear,
        transfer::LOG100 => Transfer::Log100,
        transfer::LOG100_SQRT10 => Transfer::Log100Sqrt10,
        transfer::XVYCC => Transfer::Xvycc,
        transfer::BT1361 => Transfer::Bt1361,
        transfer::SRGB => Transfer::Srgb,
        transfer::BT2020_TEN_BIT => Transfer::Bt2020TenBit,
        transfer::BT2020_TWELVE_BIT => Transfer::Bt2020TwelveBit,
        transfer::SMPTE2084 => Transfer::Smpte2084,
        transfer::SMPTE428 => Transfer::Smpte428,
        transfer::ARIB_STD_B67 => Transfer::AribStdB67,
        _ => return None,
    })
}

/// MatrixCoefficients variant → H.273 §8.3 enumerant byte.
pub fn matrix_to_h273_byte(value: &Matrix) -> u8 {
    match value {
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

/// H.273 §8.3 enumerant byte → MatrixCoefficients variant, `None` for a
/// value the bag vocabulary does not model.
pub fn matrix_from_h273_byte(byte: u8) -> Option<Matrix> {
    Some(match byte {
        matrix::IDENTITY => Matrix::Identity,
        matrix::BT709 => Matrix::Bt709,
        matrix::FCC => Matrix::Fcc,
        matrix::BT470_BG => Matrix::Bt470Bg,
        matrix::SMPTE170M => Matrix::Smpte170m,
        matrix::SMPTE240M => Matrix::Smpte240m,
        matrix::YCGCO => Matrix::Ycgco,
        matrix::BT2020_NCL => Matrix::Bt2020Ncl,
        matrix::BT2020_CL => Matrix::Bt2020Cl,
        matrix::SMPTE2085 => Matrix::Smpte2085,
        matrix::CHROMA_NCL => Matrix::ChromaNcl,
        matrix::CHROMA_CL => Matrix::ChromaCl,
        matrix::ICTCP => Matrix::Ictcp,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bt709_limited_color_info_lands_on_the_spec_enumerants() {
        let vui = color_info_to_h273_color_vui(&ColorInfo {
            primaries: Some(Primaries::Bt709),
            transfer: Some(Transfer::Srgb),
            matrix: Some(Matrix::Bt709),
            range: Some(Range::Limited),
        });
        assert_eq!(vui.primaries, Some(1));
        assert_eq!(vui.transfer, Some(13));
        assert_eq!(vui.matrix, Some(1));
        assert_eq!(vui.full_range, Some(false));
    }

    #[test]
    fn absent_axes_stay_absent_so_no_vui_block_is_forced() {
        let vui = color_info_to_h273_color_vui(&ColorInfo {
            primaries: None,
            transfer: None,
            matrix: None,
            range: None,
        });
        assert_eq!(vui, H273ColorVui::default());
        assert!(!vui.is_video_signal_type_block_needed());
    }

    #[test]
    fn full_range_maps_to_the_video_full_range_flag() {
        let vui = color_info_to_h273_color_vui(&ColorInfo {
            primaries: None,
            transfer: None,
            matrix: None,
            range: Some(Range::Full),
        });
        assert_eq!(vui.full_range, Some(true));
        assert!(vui.is_video_signal_type_block_needed());
        assert!(!vui.is_colour_description_block_needed());
    }

    /// Every modeled variant survives the byte round trip — the property
    /// that keeps encode-side and decode-side translation one table.
    #[test]
    fn every_modeled_variant_round_trips_through_its_h273_byte() {
        let all_primaries = [
            Primaries::Bt709,
            Primaries::Bt470M,
            Primaries::Bt470Bg,
            Primaries::Smpte170m,
            Primaries::Smpte240m,
            Primaries::Film,
            Primaries::Bt2020,
            Primaries::Smpte428,
            Primaries::Smpte431,
            Primaries::Smpte432,
            Primaries::Ebu3213,
        ];
        for value in all_primaries {
            assert_eq!(
                primaries_from_h273_byte(primaries_to_h273_byte(&value)),
                Some(value.clone()),
            );
        }
        let all_transfers = [
            Transfer::Bt709,
            Transfer::Gamma22,
            Transfer::Gamma28,
            Transfer::Smpte170m,
            Transfer::Smpte240m,
            Transfer::Linear,
            Transfer::Log100,
            Transfer::Log100Sqrt10,
            Transfer::Xvycc,
            Transfer::Bt1361,
            Transfer::Srgb,
            Transfer::Bt2020TenBit,
            Transfer::Bt2020TwelveBit,
            Transfer::Smpte2084,
            Transfer::Smpte428,
            Transfer::AribStdB67,
        ];
        for value in all_transfers {
            assert_eq!(
                transfer_from_h273_byte(transfer_to_h273_byte(&value)),
                Some(value.clone()),
            );
        }
        let all_matrices = [
            Matrix::Identity,
            Matrix::Bt709,
            Matrix::Fcc,
            Matrix::Bt470Bg,
            Matrix::Smpte170m,
            Matrix::Smpte240m,
            Matrix::Ycgco,
            Matrix::Bt2020Ncl,
            Matrix::Bt2020Cl,
            Matrix::Smpte2085,
            Matrix::ChromaNcl,
            Matrix::ChromaCl,
            Matrix::Ictcp,
        ];
        for value in all_matrices {
            assert_eq!(
                matrix_from_h273_byte(matrix_to_h273_byte(&value)),
                Some(value.clone()),
            );
        }
    }

    #[test]
    fn an_unmodeled_h273_byte_decodes_to_none_rather_than_a_fabricated_variant() {
        // 2 is H.273 "Unspecified" on every axis; 255 is out of every table.
        for byte in [2u8, 255] {
            assert_eq!(primaries_from_h273_byte(byte), None);
            assert_eq!(transfer_from_h273_byte(byte), None);
            assert_eq!(matrix_from_h273_byte(byte), None);
        }
    }
}
