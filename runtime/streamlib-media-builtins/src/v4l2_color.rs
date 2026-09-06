// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! V4L2 colorspace ↔ [`ColorInfo`] translation.
//!
//! Mirrors FFmpeg's `libavcodec/v4l2_buffers.c` mapping plus the
//! V4L2 `*_DEFAULT` resolution rules from `<linux/videodev2.h>`. V4L2
//! reports four orthogonal fields on `v4l2_pix_format`: `colorspace`,
//! `xfer_func`, `ycbcr_enc`, `quantization`. When any sub-field is
//! `*_DEFAULT` (= 0), V4L2's `V4L2_MAP_*_DEFAULT` macros derive the value
//! from `colorspace`. We do the same here.
//!
//! Each axis returns `Option<T>` — `None` is the canonical "unknown"
//! representation. `V4L2_COLORSPACE_DEFAULT` and any unrecognized
//! enumerant propagate as `None`.
//!
//! The inverse, [`resolved_color_to_v4l2_color`], is what a V4L2 *output*
//! device is told at `S_FMT`: the fully resolved description the engine's
//! kernel encodes with, every axis explicit, so readers decode the pixels
//! that were written rather than a default derived from the colorspace.

use streamlib::sdk::color::{MatrixId, PrimariesId, RangeId, ResolvedColorInfo, TransferId};

use crate::video_frame::{ColorInfo, Matrix, Primaries, Range, Transfer};

// V4L2 `colorspace` enumerants (from `<linux/videodev2.h>`).
const V4L2_COLORSPACE_DEFAULT: u32 = 0;
const V4L2_COLORSPACE_SMPTE170M: u32 = 1;
const V4L2_COLORSPACE_SMPTE240M: u32 = 2;
const V4L2_COLORSPACE_REC709: u32 = 3;
const V4L2_COLORSPACE_BT878: u32 = 4;
const V4L2_COLORSPACE_470_SYSTEM_M: u32 = 5;
const V4L2_COLORSPACE_470_SYSTEM_BG: u32 = 6;
const V4L2_COLORSPACE_JPEG: u32 = 7;
const V4L2_COLORSPACE_SRGB: u32 = 8;
const V4L2_COLORSPACE_OPRGB: u32 = 9;
const V4L2_COLORSPACE_BT2020: u32 = 10;
const V4L2_COLORSPACE_RAW: u32 = 11;
const V4L2_COLORSPACE_DCI_P3: u32 = 12;

const V4L2_XFER_FUNC_DEFAULT: u32 = 0;
const V4L2_XFER_FUNC_709: u32 = 1;
const V4L2_XFER_FUNC_SRGB: u32 = 2;
const V4L2_XFER_FUNC_OPRGB: u32 = 3;
const V4L2_XFER_FUNC_SMPTE240M: u32 = 4;
const V4L2_XFER_FUNC_NONE: u32 = 5;
const V4L2_XFER_FUNC_DCI_P3: u32 = 6;
const V4L2_XFER_FUNC_SMPTE2084: u32 = 7;

const V4L2_YCBCR_ENC_DEFAULT: u32 = 0;
const V4L2_YCBCR_ENC_601: u32 = 1;
const V4L2_YCBCR_ENC_709: u32 = 2;
const V4L2_YCBCR_ENC_XV601: u32 = 3;
const V4L2_YCBCR_ENC_XV709: u32 = 4;
const V4L2_YCBCR_ENC_SYCC: u32 = 5;
const V4L2_YCBCR_ENC_BT2020: u32 = 6;
const V4L2_YCBCR_ENC_BT2020_CONST_LUM: u32 = 7;
const V4L2_YCBCR_ENC_SMPTE240M: u32 = 8;

const V4L2_QUANTIZATION_DEFAULT: u32 = 0;
const V4L2_QUANTIZATION_FULL_RANGE: u32 = 1;
const V4L2_QUANTIZATION_LIM_RANGE: u32 = 2;

/// Translate a V4L2 colorspace report to a [`ColorInfo`]. Sub-fields
/// reported as `*_DEFAULT` are resolved from the `colorspace` field per the
/// V4L2 mapping macros; `V4L2_COLORSPACE_DEFAULT` propagates as `None`
/// across the board.
pub fn v4l2_color_to_color_info(
    colorspace: u32,
    xfer_func: u32,
    ycbcr_enc: u32,
    quantization: u32,
) -> ColorInfo {
    ColorInfo {
        primaries: primaries_from_v4l2(colorspace),
        transfer: transfer_from_v4l2(xfer_func, colorspace),
        matrix: matrix_from_v4l2(ycbcr_enc, colorspace),
        range: range_from_v4l2(quantization, colorspace),
    }
}

/// The four `v4l2_pix_format` colour fields a writer sets at `S_FMT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4l2PixFormatColorFields {
    pub colorspace: u32,
    pub xfer_func: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
}

/// Translate the resolved description the engine's kernel encodes with to
/// the V4L2 fields a reader will see — every axis explicit. The colorspace
/// enumerant follows the primaries (and sRGB's transfer under BT.709
/// primaries, which V4L2 spells as its own colorspace); a primaries value
/// V4L2 cannot name leaves `V4L2_COLORSPACE_DEFAULT`, which the loopback
/// module reports as sRGB, and the three explicit axes still hold.
pub fn resolved_color_to_v4l2_color(info: &ResolvedColorInfo) -> V4l2PixFormatColorFields {
    V4l2PixFormatColorFields {
        colorspace: match info.primaries {
            PrimariesId::Smpte170m => V4L2_COLORSPACE_SMPTE170M,
            PrimariesId::Smpte240m => V4L2_COLORSPACE_SMPTE240M,
            PrimariesId::Bt709 => match info.transfer {
                TransferId::Srgb => V4L2_COLORSPACE_SRGB,
                _ => V4L2_COLORSPACE_REC709,
            },
            PrimariesId::Bt470M => V4L2_COLORSPACE_470_SYSTEM_M,
            PrimariesId::Bt470Bg => V4L2_COLORSPACE_470_SYSTEM_BG,
            PrimariesId::Bt2020 => V4L2_COLORSPACE_BT2020,
            PrimariesId::Smpte431 => V4L2_COLORSPACE_DCI_P3,
            _ => V4L2_COLORSPACE_DEFAULT,
        },
        xfer_func: match info.transfer {
            TransferId::Bt709 => V4L2_XFER_FUNC_709,
            TransferId::Srgb => V4L2_XFER_FUNC_SRGB,
            TransferId::Linear => V4L2_XFER_FUNC_NONE,
            TransferId::Pq => V4L2_XFER_FUNC_SMPTE2084,
            // V4L2 has no HLG enumerant; the reader derives from the colorspace.
            TransferId::Hlg => V4L2_XFER_FUNC_DEFAULT,
        },
        ycbcr_enc: match info.matrix {
            MatrixId::Smpte170m | MatrixId::Bt470Bg | MatrixId::Fcc => V4L2_YCBCR_ENC_601,
            MatrixId::Bt709 => V4L2_YCBCR_ENC_709,
            MatrixId::Bt2020Ncl => V4L2_YCBCR_ENC_BT2020,
            MatrixId::Bt2020Cl => V4L2_YCBCR_ENC_BT2020_CONST_LUM,
            MatrixId::Smpte240m => V4L2_YCBCR_ENC_SMPTE240M,
            _ => V4L2_YCBCR_ENC_DEFAULT,
        },
        quantization: match info.range {
            RangeId::Full => V4L2_QUANTIZATION_FULL_RANGE,
            RangeId::Limited => V4L2_QUANTIZATION_LIM_RANGE,
        },
    }
}

fn primaries_from_v4l2(colorspace: u32) -> Option<Primaries> {
    match colorspace {
        V4L2_COLORSPACE_DEFAULT => None,
        V4L2_COLORSPACE_SMPTE170M | V4L2_COLORSPACE_BT878 => Some(Primaries::Smpte170m),
        V4L2_COLORSPACE_SMPTE240M => Some(Primaries::Smpte240m),
        V4L2_COLORSPACE_REC709 => Some(Primaries::Bt709),
        V4L2_COLORSPACE_470_SYSTEM_M => Some(Primaries::Bt470M),
        V4L2_COLORSPACE_470_SYSTEM_BG => Some(Primaries::Bt470Bg),
        // V4L2_COLORSPACE_JPEG is "shorthand for SRGB primaries + BT.601
        // matrix + full range" per kernel comment.
        V4L2_COLORSPACE_JPEG | V4L2_COLORSPACE_SRGB => Some(Primaries::Bt709),
        // OPRGB (Adobe RGB) primaries have no H.273 code point; don't guess.
        V4L2_COLORSPACE_OPRGB => None,
        V4L2_COLORSPACE_BT2020 => Some(Primaries::Bt2020),
        V4L2_COLORSPACE_DCI_P3 => Some(Primaries::Smpte431),
        // RAW, anything unrecognized: don't guess.
        _ => None,
    }
}

fn transfer_from_v4l2(xfer_func: u32, colorspace: u32) -> Option<Transfer> {
    let resolved = if xfer_func == V4L2_XFER_FUNC_DEFAULT {
        // V4L2_MAP_XFER_FUNC_DEFAULT: derive from colorspace.
        match colorspace {
            V4L2_COLORSPACE_OPRGB => V4L2_XFER_FUNC_OPRGB,
            V4L2_COLORSPACE_SMPTE240M => V4L2_XFER_FUNC_SMPTE240M,
            V4L2_COLORSPACE_DCI_P3 => V4L2_XFER_FUNC_DCI_P3,
            V4L2_COLORSPACE_RAW => V4L2_XFER_FUNC_NONE,
            V4L2_COLORSPACE_SRGB | V4L2_COLORSPACE_JPEG => V4L2_XFER_FUNC_SRGB,
            V4L2_COLORSPACE_DEFAULT => return None,
            _ => V4L2_XFER_FUNC_709,
        }
    } else {
        xfer_func
    };
    match resolved {
        V4L2_XFER_FUNC_709 => Some(Transfer::Bt709),
        V4L2_XFER_FUNC_SRGB => Some(Transfer::Srgb),
        // OPRGB / DCI_P3 have no direct H.273 mapping; report None rather
        // than misrepresent.
        V4L2_XFER_FUNC_OPRGB | V4L2_XFER_FUNC_DCI_P3 => None,
        V4L2_XFER_FUNC_SMPTE240M => Some(Transfer::Smpte240m),
        V4L2_XFER_FUNC_NONE => Some(Transfer::Linear),
        V4L2_XFER_FUNC_SMPTE2084 => Some(Transfer::Smpte2084),
        _ => None,
    }
}

fn matrix_from_v4l2(ycbcr_enc: u32, colorspace: u32) -> Option<Matrix> {
    let resolved = if ycbcr_enc == V4L2_YCBCR_ENC_DEFAULT {
        // V4L2_MAP_YCBCR_ENC_DEFAULT: derive from colorspace.
        match colorspace {
            V4L2_COLORSPACE_REC709 | V4L2_COLORSPACE_DCI_P3 => V4L2_YCBCR_ENC_709,
            V4L2_COLORSPACE_BT2020 => V4L2_YCBCR_ENC_BT2020,
            V4L2_COLORSPACE_SMPTE240M => V4L2_YCBCR_ENC_SMPTE240M,
            V4L2_COLORSPACE_DEFAULT => return None,
            _ => V4L2_YCBCR_ENC_601,
        }
    } else {
        ycbcr_enc
    };
    match resolved {
        V4L2_YCBCR_ENC_601 | V4L2_YCBCR_ENC_XV601 | V4L2_YCBCR_ENC_SYCC => Some(Matrix::Smpte170m),
        V4L2_YCBCR_ENC_709 | V4L2_YCBCR_ENC_XV709 => Some(Matrix::Bt709),
        V4L2_YCBCR_ENC_BT2020 => Some(Matrix::Bt2020Ncl),
        V4L2_YCBCR_ENC_BT2020_CONST_LUM => Some(Matrix::Bt2020Cl),
        V4L2_YCBCR_ENC_SMPTE240M => Some(Matrix::Smpte240m),
        _ => None,
    }
}

fn range_from_v4l2(quantization: u32, colorspace: u32) -> Option<Range> {
    let resolved = if quantization == V4L2_QUANTIZATION_DEFAULT {
        // V4L2_MAP_QUANTIZATION_DEFAULT with is_rgb_or_hsv = false (this
        // path is YUV-only): full range for JPEG only; SRGB and OPRGB YUV
        // data defaults to limited like everything else. Mapping SRGB to
        // full (a former bug carried from the incumbent) skipped the range
        // expansion and rendered limited-range webcam data with crushed
        // contrast.
        match colorspace {
            V4L2_COLORSPACE_JPEG => V4L2_QUANTIZATION_FULL_RANGE,
            V4L2_COLORSPACE_DEFAULT => return None,
            _ => V4L2_QUANTIZATION_LIM_RANGE,
        }
    } else {
        quantization
    };
    match resolved {
        V4L2_QUANTIZATION_FULL_RANGE => Some(Range::Full),
        V4L2_QUANTIZATION_LIM_RANGE => Some(Range::Limited),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec709_explicit_maps_to_bt709() {
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_REC709,
            V4L2_XFER_FUNC_709,
            V4L2_YCBCR_ENC_709,
            V4L2_QUANTIZATION_LIM_RANGE,
        );
        assert_eq!(info.primaries, Some(Primaries::Bt709));
        assert_eq!(info.transfer, Some(Transfer::Bt709));
        assert_eq!(info.matrix, Some(Matrix::Bt709));
        assert_eq!(info.range, Some(Range::Limited));
    }

    #[test]
    fn vivid_smpte170m_with_defaults_resolves_to_bt601_525() {
        // Vivid reports V4L2_COLORSPACE_SMPTE170M with everything else
        // default. SMPTE 170M is BT.601 525-line.
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_SMPTE170M,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, Some(Primaries::Smpte170m));
        assert_eq!(info.transfer, Some(Transfer::Bt709));
        assert_eq!(info.matrix, Some(Matrix::Smpte170m));
        assert_eq!(info.range, Some(Range::Limited));
    }

    #[test]
    fn webcam_srgb_with_defaults_resolves_to_bt601_matrix_limited_range() {
        // The standard UVC webcam combo: SRGB colorspace means sRGB
        // primaries/transfer + BT.601 matrix, and the YUV quantization
        // default is LIMITED — V4L2_MAP_QUANTIZATION_DEFAULT returns full
        // only for JPEG when the data is YCbCr.
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_SRGB,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, Some(Primaries::Bt709));
        assert_eq!(info.transfer, Some(Transfer::Srgb));
        assert_eq!(info.matrix, Some(Matrix::Smpte170m));
        assert_eq!(info.range, Some(Range::Limited));
    }

    #[test]
    fn jpeg_with_defaults_is_the_only_full_range_yuv_default() {
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_JPEG,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.range, Some(Range::Full));
    }

    #[test]
    fn oprgb_primaries_are_not_misrepresented() {
        // OPRGB has no H.273 primaries code point; both axes stay unknown
        // rather than guessing BT.709.
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_OPRGB,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, None);
        assert_eq!(info.transfer, None);
        assert_eq!(info.range, Some(Range::Limited));
    }

    #[test]
    fn bt2020_with_defaults_resolves_to_bt2020_ncl() {
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_BT2020,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, Some(Primaries::Bt2020));
        assert_eq!(info.transfer, Some(Transfer::Bt709));
        assert_eq!(info.matrix, Some(Matrix::Bt2020Ncl));
        assert_eq!(info.range, Some(Range::Limited));
    }

    #[test]
    fn colorspace_default_propagates_none_on_every_axis() {
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_DEFAULT,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, None);
        assert_eq!(info.transfer, None);
        assert_eq!(info.matrix, None);
        assert_eq!(info.range, None);
    }

    #[test]
    fn bt2020_with_pq_transfer_resolves_to_smpte2084() {
        // HDR10 source: BT.2020 primaries + PQ transfer + BT.2020 NCL
        // matrix + limited range.
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_BT2020,
            V4L2_XFER_FUNC_SMPTE2084,
            V4L2_YCBCR_ENC_BT2020,
            V4L2_QUANTIZATION_LIM_RANGE,
        );
        assert_eq!(info.primaries, Some(Primaries::Bt2020));
        assert_eq!(info.transfer, Some(Transfer::Smpte2084));
        assert_eq!(info.matrix, Some(Matrix::Bt2020Ncl));
        assert_eq!(info.range, Some(Range::Limited));
    }

    #[test]
    fn default_color_info_is_all_none() {
        // ColorInfo::default() must be the semantic "unknown" state.
        let info = ColorInfo::default();
        assert_eq!(info.primaries, None);
        assert_eq!(info.transfer, None);
        assert_eq!(info.matrix, None);
        assert_eq!(info.range, None);
    }

    /// The inverse map round-trips through the forward one for every
    /// description both sides can name: what the sink signals at `S_FMT` is
    /// what a StreamLib camera reading that device resolves back to.
    #[test]
    fn resolved_color_to_v4l2_round_trips_through_the_forward_map() {
        use streamlib::sdk::color::ColorSpaceKind;
        let cases = [
            ResolvedColorInfo {
                primaries: PrimariesId::Bt709,
                transfer: TransferId::Srgb,
                matrix: MatrixId::Smpte170m,
                range: RangeId::Full,
            },
            ResolvedColorInfo {
                primaries: PrimariesId::Bt709,
                transfer: TransferId::Bt709,
                matrix: MatrixId::Bt709,
                range: RangeId::Limited,
            },
            ResolvedColorInfo {
                primaries: PrimariesId::Smpte170m,
                transfer: TransferId::Bt709,
                matrix: MatrixId::Smpte170m,
                range: RangeId::Full,
            },
            ResolvedColorInfo {
                primaries: PrimariesId::Bt2020,
                transfer: TransferId::Pq,
                matrix: MatrixId::Bt2020Ncl,
                range: RangeId::Limited,
            },
        ];
        for info in cases {
            let fields = resolved_color_to_v4l2_color(&info);
            let back = v4l2_color_to_color_info(
                fields.colorspace,
                fields.xfer_func,
                fields.ycbcr_enc,
                fields.quantization,
            )
            .resolve_defaults(ColorSpaceKind::Yuv);
            assert_eq!(back, info, "round trip through {fields:?}");
        }
    }

    /// The test pattern's own description — sRGB primaries and transfer,
    /// full range, the YUV default matrix — reaches the device with every
    /// axis explicit, so a reader decodes full range rather than deriving
    /// limited from the colorspace.
    #[test]
    fn a_full_range_srgb_source_signals_every_axis_explicitly() {
        let fields = resolved_color_to_v4l2_color(&ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Srgb,
            matrix: MatrixId::Smpte170m,
            range: RangeId::Full,
        });
        assert_eq!(
            fields,
            V4l2PixFormatColorFields {
                colorspace: V4L2_COLORSPACE_SRGB,
                xfer_func: V4L2_XFER_FUNC_SRGB,
                ycbcr_enc: V4L2_YCBCR_ENC_601,
                quantization: V4L2_QUANTIZATION_FULL_RANGE,
            }
        );
        assert_ne!(fields.quantization, V4L2_QUANTIZATION_DEFAULT);
    }
}
