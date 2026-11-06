// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A captured frame's colour, each axis named the way `tatolab.stream.ColorInfo` names it.

use tatolab_module_sdk::{MODULE_HOST_H273_UNSPECIFIED, ModuleHostImageFrameH273Colour};

/// The H.273 four-tuple, each axis spelled as `tatolab.stream.ColorInfo` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapturedFrameColourDescription {
    pub primaries: Option<&'static str>,
    pub transfer: Option<&'static str>,
    pub matrix: Option<&'static str>,
    pub range: Option<&'static str>,
}

/// The matrix a frame is decoded with: the one the device named, else the one a
/// player falls back to — BT.709 at HD and above, BT.601 below it.
pub fn matrix_name_or_the_players_fallback(
    matrix: Option<&'static str>,
    frame_height: u32,
) -> &'static str {
    match matrix {
        Some(matrix) => matrix,
        None if frame_height >= 720 => "bt709",
        None => "smpte170m",
    }
}

/// H.273 `ColourPrimaries` code points and their `ColorInfo` names.
const PRIMARIES_CODE_POINTS: &[(u8, &str)] = &[
    (1, "bt709"),
    (4, "bt470_m"),
    (5, "bt470_bg"),
    (6, "smpte170m"),
    (7, "smpte240m"),
    (8, "film"),
    (9, "bt2020"),
    (10, "smpte428"),
    (11, "smpte431"),
    (12, "smpte432"),
    (22, "ebu3213"),
];

/// H.273 `TransferCharacteristics` code points and their `ColorInfo` names.
const TRANSFER_CODE_POINTS: &[(u8, &str)] = &[
    (1, "bt709"),
    (4, "gamma22"),
    (5, "gamma28"),
    (6, "smpte170m"),
    (7, "smpte240m"),
    (8, "linear"),
    (9, "log100"),
    (10, "log100_sqrt10"),
    (11, "xvycc"),
    (12, "bt1361"),
    (13, "srgb"),
    (14, "bt2020_ten_bit"),
    (15, "bt2020_twelve_bit"),
    (16, "smpte2084"),
    (17, "smpte428"),
    (18, "arib_std_b67"),
];

/// H.273 `MatrixCoefficients` code points and their `ColorInfo` names.
/// Identity (0) names no YCbCr matrix, so a YCbCr frame never carries it.
const MATRIX_CODE_POINTS: &[(u8, &str)] = &[
    (1, "bt709"),
    (4, "fcc"),
    (5, "bt470_bg"),
    (6, "smpte170m"),
    (7, "smpte240m"),
    (8, "ycgco"),
    (9, "bt2020_ncl"),
    (10, "bt2020_cl"),
    (11, "smpte2085"),
    (12, "chroma_ncl"),
    (13, "chroma_cl"),
    (14, "ictcp"),
];

fn name_of_code_point(code_points: &[(u8, &'static str)], code_point: i32) -> Option<&'static str> {
    code_points
        .iter()
        .find(|&&(named_code_point, _)| i32::from(named_code_point) == code_point)
        .map(|&(_, name)| name)
}

fn code_point_of_name(code_points: &[(u8, &'static str)], name: Option<&str>) -> u8 {
    name.and_then(|name| {
        code_points
            .iter()
            .find(|&&(_, named)| named == name)
            .map(|&(code_point, _)| code_point)
    })
    .unwrap_or(MODULE_HOST_H273_UNSPECIFIED)
}

/// The `ColorInfo` name of H.273 `ColourPrimaries` code point `code_point`.
pub fn primaries_name_of_h273_code_point(code_point: i32) -> Option<&'static str> {
    name_of_code_point(PRIMARIES_CODE_POINTS, code_point)
}

/// The `ColorInfo` name of H.273 `TransferCharacteristics` code point `code_point`.
pub fn transfer_name_of_h273_code_point(code_point: i32) -> Option<&'static str> {
    name_of_code_point(TRANSFER_CODE_POINTS, code_point)
}

/// The `ColorInfo` name of H.273 `MatrixCoefficients` code point `code_point`.
pub fn matrix_name_of_h273_code_point(code_point: i32) -> Option<&'static str> {
    name_of_code_point(MATRIX_CODE_POINTS, code_point)
}

impl CapturedFrameColourDescription {
    /// The four H.273 code points an Image frame states this colour with, an
    /// absent axis unspecified.
    pub fn image_frame_h273_colour(&self) -> ModuleHostImageFrameH273Colour {
        ModuleHostImageFrameH273Colour {
            primaries: code_point_of_name(PRIMARIES_CODE_POINTS, self.primaries),
            transfer: code_point_of_name(TRANSFER_CODE_POINTS, self.transfer),
            matrix: code_point_of_name(MATRIX_CODE_POINTS, self.matrix),
            range: match self.range {
                Some("limited") => 0,
                Some("full") => 1,
                _ => MODULE_HOST_H273_UNSPECIFIED,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unspecified_matrix_decodes_by_the_frame_height() {
        assert_eq!(matrix_name_or_the_players_fallback(None, 1080), "bt709");
        assert_eq!(matrix_name_or_the_players_fallback(None, 480), "smpte170m");
        assert_eq!(
            matrix_name_or_the_players_fallback(Some("smpte170m"), 1080),
            "smpte170m"
        );
    }

    #[test]
    fn unspecified_and_reserved_code_points_name_nothing() {
        for unnamed in [0, 2, 3] {
            assert_eq!(primaries_name_of_h273_code_point(unnamed), None);
            assert_eq!(transfer_name_of_h273_code_point(unnamed), None);
            assert_eq!(matrix_name_of_h273_code_point(unnamed), None);
        }
    }

    #[test]
    fn the_hd_and_sd_code_points_name_their_axes() {
        assert_eq!(primaries_name_of_h273_code_point(1), Some("bt709"));
        assert_eq!(primaries_name_of_h273_code_point(6), Some("smpte170m"));
        assert_eq!(transfer_name_of_h273_code_point(1), Some("bt709"));
        assert_eq!(transfer_name_of_h273_code_point(16), Some("smpte2084"));
        assert_eq!(matrix_name_of_h273_code_point(6), Some("smpte170m"));
        assert_eq!(matrix_name_of_h273_code_point(9), Some("bt2020_ncl"));
    }

    #[test]
    fn a_description_states_its_axes_as_code_points_and_an_absent_one_unspecified() {
        let colour = CapturedFrameColourDescription {
            primaries: Some("bt709"),
            transfer: Some("srgb"),
            matrix: None,
            range: Some("limited"),
        }
        .image_frame_h273_colour();
        assert_eq!(
            colour,
            ModuleHostImageFrameH273Colour {
                primaries: 1,
                transfer: 13,
                matrix: MODULE_HOST_H273_UNSPECIFIED,
                range: 0,
            }
        );
    }

    #[test]
    fn every_name_reads_back_as_its_code_point() {
        for code_points in [
            PRIMARIES_CODE_POINTS,
            TRANSFER_CODE_POINTS,
            MATRIX_CODE_POINTS,
        ] {
            for &(code_point, name) in code_points {
                assert_eq!(code_point_of_name(code_points, Some(name)), code_point);
                assert_eq!(
                    name_of_code_point(code_points, i32::from(code_point)),
                    Some(name)
                );
            }
        }
    }
}
