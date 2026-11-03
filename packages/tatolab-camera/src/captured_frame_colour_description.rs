// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A captured frame's colour, each axis named the way `tatolab.stream.ColorInfo` names it.

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

/// The `ColorInfo` name of H.273 `ColourPrimaries` code point `code_point`.
pub fn primaries_name_of_h273_code_point(code_point: i32) -> Option<&'static str> {
    match code_point {
        1 => Some("bt709"),
        4 => Some("bt470_m"),
        5 => Some("bt470_bg"),
        6 => Some("smpte170m"),
        7 => Some("smpte240m"),
        8 => Some("film"),
        9 => Some("bt2020"),
        10 => Some("smpte428"),
        11 => Some("smpte431"),
        12 => Some("smpte432"),
        22 => Some("ebu3213"),
        _ => None,
    }
}

/// The `ColorInfo` name of H.273 `TransferCharacteristics` code point `code_point`.
pub fn transfer_name_of_h273_code_point(code_point: i32) -> Option<&'static str> {
    match code_point {
        1 => Some("bt709"),
        4 => Some("gamma22"),
        5 => Some("gamma28"),
        6 => Some("smpte170m"),
        7 => Some("smpte240m"),
        8 => Some("linear"),
        9 => Some("log100"),
        10 => Some("log100_sqrt10"),
        11 => Some("xvycc"),
        12 => Some("bt1361"),
        13 => Some("srgb"),
        14 => Some("bt2020_ten_bit"),
        15 => Some("bt2020_twelve_bit"),
        16 => Some("smpte2084"),
        17 => Some("smpte428"),
        18 => Some("arib_std_b67"),
        _ => None,
    }
}

/// The `ColorInfo` name of H.273 `MatrixCoefficients` code point `code_point`.
/// Identity (0) names no YCbCr matrix, so a YCbCr frame never carries it.
pub fn matrix_name_of_h273_code_point(code_point: i32) -> Option<&'static str> {
    match code_point {
        1 => Some("bt709"),
        4 => Some("fcc"),
        5 => Some("bt470_bg"),
        6 => Some("smpte170m"),
        7 => Some("smpte240m"),
        8 => Some("ycgco"),
        9 => Some("bt2020_ncl"),
        10 => Some("bt2020_cl"),
        11 => Some("smpte2085"),
        12 => Some("chroma_ncl"),
        13 => Some("chroma_cl"),
        14 => Some("ictcp"),
        _ => None,
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
}
