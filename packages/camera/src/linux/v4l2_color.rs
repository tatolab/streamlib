// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! V4L2 colorspace → `ColorInfo` translation.
//!
//! Mirrors FFmpeg's `libavcodec/v4l2_buffers.c` mapping plus the
//! V4L2 `*_DEFAULT` resolution rules from
//! `<linux/videodev2.h>`. V4L2 reports four orthogonal fields on
//! `v4l2_pix_format`: `colorspace`, `xfer_func`, `ycbcr_enc`,
//! `quantization`. When any sub-field is `*_DEFAULT` (= 0), V4L2's
//! `V4L2_MAP_*_DEFAULT` macros derive the value from `colorspace`.
//! We do the same here.

use crate::_generated_::tatolab__core::color_info::{Matrix, Primaries, Range, Transfer};
use crate::_generated_::ColorInfo;

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

/// Translate a V4L2 colorspace report to a `ColorInfo`. Sub-fields
/// reported as `*_DEFAULT` are resolved from the `colorspace` field
/// per the V4L2 mapping macros. `V4L2_COLORSPACE_DEFAULT` propagates
/// as `unspecified` across the board.
pub fn v4l2_color_to_color_info(
    colorspace: u32,
    xfer_func: u32,
    ycbcr_enc: u32,
    quantization: u32,
) -> ColorInfo {
    let primaries = primaries_from_v4l2(colorspace);
    let transfer = transfer_from_v4l2(xfer_func, colorspace);
    let matrix = matrix_from_v4l2(ycbcr_enc, colorspace);
    let range = range_from_v4l2(quantization, colorspace);
    ColorInfo { primaries, transfer, matrix, range }
}

fn primaries_from_v4l2(colorspace: u32) -> Primaries {
    match colorspace {
        V4L2_COLORSPACE_DEFAULT => Primaries::Unspecified,
        V4L2_COLORSPACE_SMPTE170M | V4L2_COLORSPACE_BT878 => Primaries::Smpte170m,
        V4L2_COLORSPACE_SMPTE240M => Primaries::Smpte240m,
        V4L2_COLORSPACE_REC709 => Primaries::Bt709,
        V4L2_COLORSPACE_470_SYSTEM_M => Primaries::Bt470M,
        V4L2_COLORSPACE_470_SYSTEM_BG => Primaries::Bt470Bg,
        // V4L2_COLORSPACE_JPEG is "shorthand for SRGB primaries +
        // BT.601 matrix + full range" per kernel comment.
        V4L2_COLORSPACE_JPEG | V4L2_COLORSPACE_SRGB | V4L2_COLORSPACE_OPRGB => Primaries::Bt709,
        V4L2_COLORSPACE_BT2020 => Primaries::Bt2020,
        V4L2_COLORSPACE_DCI_P3 => Primaries::Smpte431,
        // RAW, anything unrecognized: don't guess.
        _ => Primaries::Unspecified,
    }
}

fn transfer_from_v4l2(xfer_func: u32, colorspace: u32) -> Transfer {
    let resolved = if xfer_func == V4L2_XFER_FUNC_DEFAULT {
        // V4L2_MAP_XFER_FUNC_DEFAULT: derive from colorspace.
        match colorspace {
            V4L2_COLORSPACE_OPRGB => V4L2_XFER_FUNC_OPRGB,
            V4L2_COLORSPACE_SMPTE240M => V4L2_XFER_FUNC_SMPTE240M,
            V4L2_COLORSPACE_DCI_P3 => V4L2_XFER_FUNC_DCI_P3,
            V4L2_COLORSPACE_RAW => V4L2_XFER_FUNC_NONE,
            V4L2_COLORSPACE_SRGB | V4L2_COLORSPACE_JPEG => V4L2_XFER_FUNC_SRGB,
            V4L2_COLORSPACE_DEFAULT => return Transfer::Unspecified,
            _ => V4L2_XFER_FUNC_709,
        }
    } else {
        xfer_func
    };
    match resolved {
        V4L2_XFER_FUNC_709 => Transfer::Bt709,
        V4L2_XFER_FUNC_SRGB => Transfer::Srgb,
        // OPRGB / DCI_P3 have no direct H.273 mapping; report
        // unspecified rather than misrepresent.
        V4L2_XFER_FUNC_OPRGB | V4L2_XFER_FUNC_DCI_P3 => Transfer::Unspecified,
        V4L2_XFER_FUNC_SMPTE240M => Transfer::Smpte240m,
        V4L2_XFER_FUNC_NONE => Transfer::Linear,
        V4L2_XFER_FUNC_SMPTE2084 => Transfer::Smpte2084,
        _ => Transfer::Unspecified,
    }
}

fn matrix_from_v4l2(ycbcr_enc: u32, colorspace: u32) -> Matrix {
    let resolved = if ycbcr_enc == V4L2_YCBCR_ENC_DEFAULT {
        // V4L2_MAP_YCBCR_ENC_DEFAULT: derive from colorspace.
        match colorspace {
            V4L2_COLORSPACE_REC709 | V4L2_COLORSPACE_DCI_P3 => V4L2_YCBCR_ENC_709,
            V4L2_COLORSPACE_BT2020 => V4L2_YCBCR_ENC_BT2020,
            V4L2_COLORSPACE_SMPTE240M => V4L2_YCBCR_ENC_SMPTE240M,
            V4L2_COLORSPACE_DEFAULT => return Matrix::Unspecified,
            _ => V4L2_YCBCR_ENC_601,
        }
    } else {
        ycbcr_enc
    };
    match resolved {
        V4L2_YCBCR_ENC_601 | V4L2_YCBCR_ENC_XV601 | V4L2_YCBCR_ENC_SYCC => Matrix::Smpte170m,
        V4L2_YCBCR_ENC_709 | V4L2_YCBCR_ENC_XV709 => Matrix::Bt709,
        V4L2_YCBCR_ENC_BT2020 => Matrix::Bt2020Ncl,
        V4L2_YCBCR_ENC_BT2020_CONST_LUM => Matrix::Bt2020Cl,
        V4L2_YCBCR_ENC_SMPTE240M => Matrix::Smpte240m,
        _ => Matrix::Unspecified,
    }
}

fn range_from_v4l2(quantization: u32, colorspace: u32) -> Range {
    let resolved = if quantization == V4L2_QUANTIZATION_DEFAULT {
        // V4L2_MAP_QUANTIZATION_DEFAULT: full for JPEG / SRGB / OPRGB,
        // limited for everything else.
        match colorspace {
            V4L2_COLORSPACE_JPEG | V4L2_COLORSPACE_SRGB | V4L2_COLORSPACE_OPRGB => {
                V4L2_QUANTIZATION_FULL_RANGE
            }
            V4L2_COLORSPACE_DEFAULT => return Range::Unspecified,
            _ => V4L2_QUANTIZATION_LIM_RANGE,
        }
    } else {
        quantization
    };
    match resolved {
        V4L2_QUANTIZATION_FULL_RANGE => Range::Full,
        V4L2_QUANTIZATION_LIM_RANGE => Range::Limited,
        _ => Range::Unspecified,
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
        assert_eq!(info.primaries, Primaries::Bt709);
        assert_eq!(info.transfer, Transfer::Bt709);
        assert_eq!(info.matrix, Matrix::Bt709);
        assert_eq!(info.range, Range::Limited);
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
        assert_eq!(info.primaries, Primaries::Smpte170m);
        assert_eq!(info.transfer, Transfer::Bt709);
        assert_eq!(info.matrix, Matrix::Smpte170m);
        assert_eq!(info.range, Range::Limited);
    }

    #[test]
    fn webcam_srgb_with_defaults_resolves_to_bt601_matrix_full_range() {
        // The standard UVC webcam combo per V4L2 convention: SRGB
        // colorspace means SRGB primaries + sRGB transfer + BT.601
        // matrix + FULL range. Surprising but documented.
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_SRGB,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, Primaries::Bt709);
        assert_eq!(info.transfer, Transfer::Srgb);
        assert_eq!(info.matrix, Matrix::Smpte170m);
        assert_eq!(info.range, Range::Full);
    }

    #[test]
    fn bt2020_with_defaults_resolves_to_bt2020_ncl() {
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_BT2020,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, Primaries::Bt2020);
        assert_eq!(info.transfer, Transfer::Bt709);
        assert_eq!(info.matrix, Matrix::Bt2020Ncl);
        assert_eq!(info.range, Range::Limited);
    }

    #[test]
    fn colorspace_default_propagates_unspecified() {
        let info = v4l2_color_to_color_info(
            V4L2_COLORSPACE_DEFAULT,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_DEFAULT,
            V4L2_QUANTIZATION_DEFAULT,
        );
        assert_eq!(info.primaries, Primaries::Unspecified);
        assert_eq!(info.transfer, Transfer::Unspecified);
        assert_eq!(info.matrix, Matrix::Unspecified);
        assert_eq!(info.range, Range::Unspecified);
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
        assert_eq!(info.primaries, Primaries::Bt2020);
        assert_eq!(info.transfer, Transfer::Smpte2084);
        assert_eq!(info.matrix, Matrix::Bt2020Ncl);
        assert_eq!(info.range, Range::Limited);
    }
}
