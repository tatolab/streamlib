// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Translation between the engine-side `ColorInfo` domain enums (from
//! `@tatolab/core`'s JTD codegen) and `vulkan_video::H273ColorVui`, the
//! codec-native byte representation that crosses into the leaf
//! `libs/vulkan-video` crate.
//!
//! The vulkan-video crate does not depend on `@tatolab/core` or the
//! streamlib SDK; this seam is what keeps that boundary clean. Both H.264
//! and H.265 packages carry an identical translator because each has its
//! own codegen module (`crate::_generated_::*`); the duplication is ~70
//! lines per codec and disappears if vulkan-video later adopts the SDK.

use crate::_generated_::tatolab__core::color_info::{
    ColorInfo, Matrix, Primaries, Range, Transfer,
};
use vulkan_video::{color_vui, H273ColorVui};

/// Translate `ColorInfo` → `H273ColorVui` for the encoder's SPS chain.
///
/// Returns `None` when every axis is `None` — the caller treats that as
/// "no codec-side color VUI requested" and leaves `SimpleEncoderConfig::
/// color_vui = None`, which skips the colour_description block in the
/// SPS entirely (timing is still emitted because the H.264 SPS always
/// chains a VUI for it).
pub fn color_info_to_h273(info: &ColorInfo) -> Option<H273ColorVui> {
    let v = H273ColorVui {
        primaries: info.primaries.as_ref().map(primaries_to_byte),
        transfer: info.transfer.as_ref().map(transfer_to_byte),
        matrix: info.matrix.as_ref().map(matrix_to_byte),
        full_range: info.range.as_ref().map(|r| matches!(r, Range::Full)),
    };
    if !v.is_video_signal_type_block_needed() {
        return None;
    }
    Some(v)
}

/// Translate `H273ColorVui` → `ColorInfo` for surfacing decoded VUI
/// metadata on `VideoFrame.color_info`. Per-axis `None` (Unspecified on
/// the wire) stays `None` in the result.
pub fn h273_to_color_info(vui: &H273ColorVui) -> ColorInfo {
    ColorInfo {
        primaries: vui.primaries.and_then(primaries_from_byte),
        transfer: vui.transfer.and_then(transfer_from_byte),
        matrix: vui.matrix.and_then(matrix_from_byte),
        range: vui.full_range.map(|f| if f { Range::Full } else { Range::Limited }),
    }
}

fn primaries_to_byte(p: &Primaries) -> u8 {
    match p {
        Primaries::Bt709 => color_vui::primaries::BT709,
        Primaries::Bt470M => color_vui::primaries::BT470_M,
        Primaries::Bt470Bg => color_vui::primaries::BT470_BG,
        Primaries::Smpte170m => color_vui::primaries::SMPTE170M,
        Primaries::Smpte240m => color_vui::primaries::SMPTE240M,
        Primaries::Film => color_vui::primaries::FILM,
        Primaries::Bt2020 => color_vui::primaries::BT2020,
        Primaries::Smpte428 => color_vui::primaries::SMPTE428,
        Primaries::Smpte431 => color_vui::primaries::SMPTE431,
        Primaries::Smpte432 => color_vui::primaries::SMPTE432,
        Primaries::Ebu3213 => color_vui::primaries::EBU3213,
    }
}

fn primaries_from_byte(b: u8) -> Option<Primaries> {
    Some(match b {
        x if x == color_vui::primaries::BT709 => Primaries::Bt709,
        x if x == color_vui::primaries::BT470_M => Primaries::Bt470M,
        x if x == color_vui::primaries::BT470_BG => Primaries::Bt470Bg,
        x if x == color_vui::primaries::SMPTE170M => Primaries::Smpte170m,
        x if x == color_vui::primaries::SMPTE240M => Primaries::Smpte240m,
        x if x == color_vui::primaries::FILM => Primaries::Film,
        x if x == color_vui::primaries::BT2020 => Primaries::Bt2020,
        x if x == color_vui::primaries::SMPTE428 => Primaries::Smpte428,
        x if x == color_vui::primaries::SMPTE431 => Primaries::Smpte431,
        x if x == color_vui::primaries::SMPTE432 => Primaries::Smpte432,
        x if x == color_vui::primaries::EBU3213 => Primaries::Ebu3213,
        _ => return None,
    })
}

fn transfer_to_byte(t: &Transfer) -> u8 {
    match t {
        Transfer::Bt709 => color_vui::transfer::BT709,
        Transfer::Gamma22 => color_vui::transfer::GAMMA22,
        Transfer::Gamma28 => color_vui::transfer::GAMMA28,
        Transfer::Smpte170m => color_vui::transfer::SMPTE170M,
        Transfer::Smpte240m => color_vui::transfer::SMPTE240M,
        Transfer::Linear => color_vui::transfer::LINEAR,
        Transfer::Log100 => color_vui::transfer::LOG100,
        Transfer::Log100Sqrt10 => color_vui::transfer::LOG100_SQRT10,
        Transfer::Xvycc => color_vui::transfer::XVYCC,
        Transfer::Bt1361 => color_vui::transfer::BT1361,
        Transfer::Srgb => color_vui::transfer::SRGB,
        Transfer::Bt2020TenBit => color_vui::transfer::BT2020_TEN_BIT,
        Transfer::Bt2020TwelveBit => color_vui::transfer::BT2020_TWELVE_BIT,
        Transfer::Smpte2084 => color_vui::transfer::SMPTE2084,
        Transfer::Smpte428 => color_vui::transfer::SMPTE428,
        Transfer::AribStdB67 => color_vui::transfer::ARIB_STD_B67,
    }
}

fn transfer_from_byte(b: u8) -> Option<Transfer> {
    Some(match b {
        x if x == color_vui::transfer::BT709 => Transfer::Bt709,
        x if x == color_vui::transfer::GAMMA22 => Transfer::Gamma22,
        x if x == color_vui::transfer::GAMMA28 => Transfer::Gamma28,
        x if x == color_vui::transfer::SMPTE170M => Transfer::Smpte170m,
        x if x == color_vui::transfer::SMPTE240M => Transfer::Smpte240m,
        x if x == color_vui::transfer::LINEAR => Transfer::Linear,
        x if x == color_vui::transfer::LOG100 => Transfer::Log100,
        x if x == color_vui::transfer::LOG100_SQRT10 => Transfer::Log100Sqrt10,
        x if x == color_vui::transfer::XVYCC => Transfer::Xvycc,
        x if x == color_vui::transfer::BT1361 => Transfer::Bt1361,
        x if x == color_vui::transfer::SRGB => Transfer::Srgb,
        x if x == color_vui::transfer::BT2020_TEN_BIT => Transfer::Bt2020TenBit,
        x if x == color_vui::transfer::BT2020_TWELVE_BIT => Transfer::Bt2020TwelveBit,
        x if x == color_vui::transfer::SMPTE2084 => Transfer::Smpte2084,
        x if x == color_vui::transfer::SMPTE428 => Transfer::Smpte428,
        x if x == color_vui::transfer::ARIB_STD_B67 => Transfer::AribStdB67,
        _ => return None,
    })
}

fn matrix_to_byte(m: &Matrix) -> u8 {
    match m {
        Matrix::Identity => color_vui::matrix::IDENTITY,
        Matrix::Bt709 => color_vui::matrix::BT709,
        Matrix::Fcc => color_vui::matrix::FCC,
        Matrix::Bt470Bg => color_vui::matrix::BT470_BG,
        Matrix::Smpte170m => color_vui::matrix::SMPTE170M,
        Matrix::Smpte240m => color_vui::matrix::SMPTE240M,
        Matrix::Ycgco => color_vui::matrix::YCGCO,
        Matrix::Bt2020Ncl => color_vui::matrix::BT2020_NCL,
        Matrix::Bt2020Cl => color_vui::matrix::BT2020_CL,
        Matrix::Smpte2085 => color_vui::matrix::SMPTE2085,
        Matrix::ChromaNcl => color_vui::matrix::CHROMA_NCL,
        Matrix::ChromaCl => color_vui::matrix::CHROMA_CL,
        Matrix::Ictcp => color_vui::matrix::ICTCP,
    }
}

fn matrix_from_byte(b: u8) -> Option<Matrix> {
    Some(match b {
        x if x == color_vui::matrix::IDENTITY => Matrix::Identity,
        x if x == color_vui::matrix::BT709 => Matrix::Bt709,
        x if x == color_vui::matrix::FCC => Matrix::Fcc,
        x if x == color_vui::matrix::BT470_BG => Matrix::Bt470Bg,
        x if x == color_vui::matrix::SMPTE170M => Matrix::Smpte170m,
        x if x == color_vui::matrix::SMPTE240M => Matrix::Smpte240m,
        x if x == color_vui::matrix::YCGCO => Matrix::Ycgco,
        x if x == color_vui::matrix::BT2020_NCL => Matrix::Bt2020Ncl,
        x if x == color_vui::matrix::BT2020_CL => Matrix::Bt2020Cl,
        x if x == color_vui::matrix::SMPTE2085 => Matrix::Smpte2085,
        x if x == color_vui::matrix::CHROMA_NCL => Matrix::ChromaNcl,
        x if x == color_vui::matrix::CHROMA_CL => Matrix::ChromaCl,
        x if x == color_vui::matrix::ICTCP => Matrix::Ictcp,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_color_info_round_trip() {
        let info = ColorInfo {
            primaries: Some(Primaries::Bt709),
            transfer: Some(Transfer::Srgb),
            matrix: Some(Matrix::Smpte170m),
            range: Some(Range::Full),
        };
        let vui = color_info_to_h273(&info).expect("non-empty input yields Some");
        assert_eq!(vui.primaries, Some(1));
        assert_eq!(vui.transfer, Some(13));
        assert_eq!(vui.matrix, Some(6));
        assert_eq!(vui.full_range, Some(true));
        let back = h273_to_color_info(&vui);
        assert_eq!(back, info);
    }

    #[test]
    fn all_none_yields_no_vui() {
        let info = ColorInfo::default();
        assert!(color_info_to_h273(&info).is_none());
    }

    #[test]
    fn range_alone_still_yields_vui() {
        let info = ColorInfo {
            range: Some(Range::Limited),
            ..Default::default()
        };
        let vui = color_info_to_h273(&info).expect("range alone is enough");
        assert_eq!(vui.full_range, Some(false));
        assert!(vui.primaries.is_none());
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
        let vui = color_info_to_h273(&info).unwrap();
        assert_eq!(vui.primaries, Some(9));
        assert_eq!(vui.transfer, Some(16));
        assert_eq!(vui.matrix, Some(9));
        assert_eq!(vui.full_range, Some(true));
        assert_eq!(h273_to_color_info(&vui), info);
    }

    #[test]
    fn unknown_byte_decodes_to_none() {
        // A decoder seeing H.273 enumerants streamlib doesn't model (e.g.
        // 99) should drop them silently rather than fabricate variants.
        let vui = H273ColorVui {
            primaries: Some(99),
            transfer: Some(13),
            matrix: Some(99),
            full_range: Some(false),
        };
        let info = h273_to_color_info(&vui);
        assert!(info.primaries.is_none());
        assert_eq!(info.transfer, Some(Transfer::Srgb));
        assert!(info.matrix.is_none());
        assert_eq!(info.range, Some(Range::Limited));
    }
}
