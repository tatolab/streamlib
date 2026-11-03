// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A V4L2 capture format's colour fields, named the way `tatolab.stream.ColorInfo` names them.
//!
//! V4L2 reports four orthogonal fields on `v4l2_pix_format`. A field reported as
//! `*_DEFAULT` (0) is derived from `colorspace` by the `V4L2_MAP_*_DEFAULT` rules
//! in `<linux/videodev2.h>`; `V4L2_COLORSPACE_DEFAULT` and any enumerant H.273
//! cannot name leave that axis unspecified.

use crate::captured_frame_colour_description::CapturedFrameColourDescription;

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

/// The four colour fields a V4L2 capture format carries, as the driver reported them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4l2CaptureFormatColourFields {
    pub colorspace: u32,
    pub xfer_func: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
}

impl V4l2CaptureFormatColourFields {
    /// The description every frame captured in this format carries.
    pub fn colour_description(&self) -> CapturedFrameColourDescription {
        CapturedFrameColourDescription {
            primaries: primaries_name(self.colorspace),
            transfer: transfer_name(self.xfer_func, self.colorspace),
            matrix: matrix_name(self.ycbcr_enc, self.colorspace),
            range: range_name(self.quantization, self.colorspace),
        }
    }
}

fn primaries_name(colorspace: u32) -> Option<&'static str> {
    match colorspace {
        V4L2_COLORSPACE_SMPTE170M | V4L2_COLORSPACE_BT878 => Some("smpte170m"),
        V4L2_COLORSPACE_SMPTE240M => Some("smpte240m"),
        V4L2_COLORSPACE_REC709 | V4L2_COLORSPACE_JPEG | V4L2_COLORSPACE_SRGB => Some("bt709"),
        V4L2_COLORSPACE_470_SYSTEM_M => Some("bt470_m"),
        V4L2_COLORSPACE_470_SYSTEM_BG => Some("bt470_bg"),
        V4L2_COLORSPACE_BT2020 => Some("bt2020"),
        V4L2_COLORSPACE_DCI_P3 => Some("smpte431"),
        _ => None,
    }
}

fn transfer_name(xfer_func: u32, colorspace: u32) -> Option<&'static str> {
    let resolved = if xfer_func == V4L2_XFER_FUNC_DEFAULT {
        match colorspace {
            V4L2_COLORSPACE_DEFAULT => return None,
            V4L2_COLORSPACE_OPRGB => V4L2_XFER_FUNC_OPRGB,
            V4L2_COLORSPACE_SMPTE240M => V4L2_XFER_FUNC_SMPTE240M,
            V4L2_COLORSPACE_DCI_P3 => V4L2_XFER_FUNC_DCI_P3,
            V4L2_COLORSPACE_RAW => V4L2_XFER_FUNC_NONE,
            V4L2_COLORSPACE_SRGB | V4L2_COLORSPACE_JPEG => V4L2_XFER_FUNC_SRGB,
            _ => V4L2_XFER_FUNC_709,
        }
    } else {
        xfer_func
    };
    match resolved {
        V4L2_XFER_FUNC_709 => Some("bt709"),
        V4L2_XFER_FUNC_SRGB => Some("srgb"),
        V4L2_XFER_FUNC_SMPTE240M => Some("smpte240m"),
        V4L2_XFER_FUNC_NONE => Some("linear"),
        V4L2_XFER_FUNC_SMPTE2084 => Some("smpte2084"),
        _ => None,
    }
}

fn matrix_name(ycbcr_enc: u32, colorspace: u32) -> Option<&'static str> {
    let resolved = if ycbcr_enc == V4L2_YCBCR_ENC_DEFAULT {
        match colorspace {
            V4L2_COLORSPACE_DEFAULT => return None,
            V4L2_COLORSPACE_REC709 | V4L2_COLORSPACE_DCI_P3 => V4L2_YCBCR_ENC_709,
            V4L2_COLORSPACE_BT2020 => V4L2_YCBCR_ENC_BT2020,
            V4L2_COLORSPACE_SMPTE240M => V4L2_YCBCR_ENC_SMPTE240M,
            _ => V4L2_YCBCR_ENC_601,
        }
    } else {
        ycbcr_enc
    };
    match resolved {
        V4L2_YCBCR_ENC_601 | V4L2_YCBCR_ENC_XV601 | V4L2_YCBCR_ENC_SYCC => Some("smpte170m"),
        V4L2_YCBCR_ENC_709 | V4L2_YCBCR_ENC_XV709 => Some("bt709"),
        V4L2_YCBCR_ENC_BT2020 => Some("bt2020_ncl"),
        V4L2_YCBCR_ENC_BT2020_CONST_LUM => Some("bt2020_cl"),
        V4L2_YCBCR_ENC_SMPTE240M => Some("smpte240m"),
        _ => None,
    }
}

fn range_name(quantization: u32, colorspace: u32) -> Option<&'static str> {
    // `V4L2_MAP_QUANTIZATION_DEFAULT` for a YCbCr format: full range for JPEG
    // only, limited for everything else, sRGB included.
    let resolved = if quantization == V4L2_QUANTIZATION_DEFAULT {
        match colorspace {
            V4L2_COLORSPACE_DEFAULT => return None,
            V4L2_COLORSPACE_JPEG => V4L2_QUANTIZATION_FULL_RANGE,
            _ => V4L2_QUANTIZATION_LIM_RANGE,
        }
    } else {
        quantization
    };
    match resolved {
        V4L2_QUANTIZATION_FULL_RANGE => Some("full"),
        V4L2_QUANTIZATION_LIM_RANGE => Some("limited"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(
        colorspace: u32,
        xfer_func: u32,
        ycbcr_enc: u32,
        quantization: u32,
    ) -> V4l2CaptureFormatColourFields {
        V4l2CaptureFormatColourFields {
            colorspace,
            xfer_func,
            ycbcr_enc,
            quantization,
        }
    }

    #[test]
    fn an_explicit_rec709_format_is_bt709_limited() {
        let description = fields(
            V4L2_COLORSPACE_REC709,
            V4L2_XFER_FUNC_709,
            V4L2_YCBCR_ENC_709,
            V4L2_QUANTIZATION_LIM_RANGE,
        )
        .colour_description();
        assert_eq!(
            description,
            CapturedFrameColourDescription {
                primaries: Some("bt709"),
                transfer: Some("bt709"),
                matrix: Some("bt709"),
                range: Some("limited"),
            }
        );
    }

    #[test]
    fn an_srgb_webcam_with_defaults_decodes_as_bt601_limited() {
        let description = fields(V4L2_COLORSPACE_SRGB, 0, 0, 0).colour_description();
        assert_eq!(description.primaries, Some("bt709"));
        assert_eq!(description.transfer, Some("srgb"));
        assert_eq!(description.matrix, Some("smpte170m"));
        assert_eq!(description.range, Some("limited"));
    }

    #[test]
    fn a_jpeg_colorspace_is_full_range() {
        let description = fields(V4L2_COLORSPACE_JPEG, 0, 0, 0).colour_description();
        assert_eq!(description.range, Some("full"));
        assert_eq!(description.matrix, Some("smpte170m"));
    }

    #[test]
    fn a_default_colorspace_leaves_every_derived_axis_unspecified() {
        assert_eq!(
            fields(V4L2_COLORSPACE_DEFAULT, 0, 0, 0).colour_description(),
            CapturedFrameColourDescription::default()
        );
    }

    #[test]
    fn an_explicit_axis_wins_over_the_colorspace_default() {
        let description = fields(
            V4L2_COLORSPACE_SRGB,
            V4L2_XFER_FUNC_DEFAULT,
            V4L2_YCBCR_ENC_709,
            V4L2_QUANTIZATION_FULL_RANGE,
        )
        .colour_description();
        assert_eq!(description.matrix, Some("bt709"));
        assert_eq!(description.range, Some("full"));
    }
}
