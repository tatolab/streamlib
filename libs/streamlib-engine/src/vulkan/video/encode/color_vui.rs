// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.273 / VUI color metadata in its native bitstream byte representation.
//!
//! The codec layer (`vulkan/video/`) is leaf-level relative to the
//! schema graph — it does not depend on `@tatolab/core` schemas or the
//! engine's `ColorInfo` type. The encoder and decoder work in raw H.273
//! enumerant bytes (the representation that appears verbatim in the
//! H.264 / H.265 / AV1 bitstream). Callers in `packages/h26{4,5}/`
//! translate `ColorInfo` ↔ [`H273ColorVui`] at the codec-processor seam.

/// H.273 color VUI carried by the H.264 / H.265 SPS.
///
/// Each axis is optional: `None` means "the codec processor did not specify
/// this axis." When the SPS VUI is emitted, an axis that is `None` while a
/// peer axis is `Some` is written as H.273 value `2` (Unspecified) per
/// ISO/IEC 23091-2.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct H273ColorVui {
    /// ColourPrimaries — ITU-T H.273 §8.1. `bt709 == 1`, `smpte170m == 6`,
    /// `bt2020 == 9`, etc.
    pub primaries: Option<u8>,
    /// TransferCharacteristics — ITU-T H.273 §8.2. `bt709 == 1`,
    /// `srgb == 13`, `smpte2084 == 16`, etc.
    pub transfer: Option<u8>,
    /// MatrixCoefficients — ITU-T H.273 §8.3. `identity == 0`, `bt709 == 1`,
    /// `smpte170m == 6`, `bt2020_ncl == 9`, etc.
    pub matrix: Option<u8>,
    /// `true` = full range (PC), `false` = limited range (TV). Maps to the
    /// H.264 / H.265 `video_full_range_flag`.
    pub full_range: Option<bool>,
}

/// H.273 value 2 — "Unspecified" placeholder for axes that are `None` but
/// must be written because a peer axis in the same description block is
/// `Some`.
pub const H273_UNSPECIFIED: u8 = 2;

impl H273ColorVui {
    /// `true` when at least one axis is set; signals that the SPS should
    /// emit `video_signal_type_present_flag = 1`.
    pub fn is_video_signal_type_block_needed(&self) -> bool {
        self.primaries.is_some()
            || self.transfer.is_some()
            || self.matrix.is_some()
            || self.full_range.is_some()
    }

    /// `true` when at least one of primaries / transfer / matrix is set;
    /// signals that the SPS should emit `colour_description_present_flag = 1`
    /// inside the video signal type block.
    pub fn is_colour_description_block_needed(&self) -> bool {
        self.primaries.is_some() || self.transfer.is_some() || self.matrix.is_some()
    }

    /// Returns the byte that should be written to the SPS for `colour_primaries`.
    /// Substitutes [`H273_UNSPECIFIED`] for `None`.
    pub fn primaries_byte(&self) -> u8 {
        self.primaries.unwrap_or(H273_UNSPECIFIED)
    }

    /// Returns the byte that should be written for `transfer_characteristics`.
    pub fn transfer_byte(&self) -> u8 {
        self.transfer.unwrap_or(H273_UNSPECIFIED)
    }

    /// Returns the byte that should be written for `matrix_coefficients`
    /// (H.264) / `matrix_coeffs` (H.265).
    pub fn matrix_byte(&self) -> u8 {
        self.matrix.unwrap_or(H273_UNSPECIFIED)
    }

    /// Returns the bit (`0` or `1`) that should be written for
    /// `video_full_range_flag`. Defaults to `0` (limited range) when unset
    /// — H.264 / H.265 spec requires writing the flag whenever
    /// `video_signal_type_present_flag = 1`.
    pub fn full_range_bit(&self) -> u32 {
        u32::from(self.full_range.unwrap_or(false))
    }
}

// ---------------------------------------------------------------------------
// H.273 enumerant constants (ITU-T H.273 / ISO/IEC 23091-2)
// ---------------------------------------------------------------------------
//
// These let translator callers reference values by name rather than by
// magic byte. The set is the subset streamlib's `ColorInfo` schema
// declares — full H.273 tables are larger.

/// ColourPrimaries values — H.273 §8.1.
pub mod primaries {
    pub const BT709: u8 = 1;
    pub const UNSPECIFIED: u8 = 2;
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
pub mod transfer {
    pub const BT709: u8 = 1;
    pub const UNSPECIFIED: u8 = 2;
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
pub mod matrix {
    pub const IDENTITY: u8 = 0;
    pub const BT709: u8 = 1;
    pub const UNSPECIFIED: u8 = 2;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_none() {
        let v = H273ColorVui::default();
        assert!(!v.is_video_signal_type_block_needed());
        assert!(!v.is_colour_description_block_needed());
    }

    #[test]
    fn full_range_alone_triggers_video_signal_type_block_but_not_colour_description() {
        let v = H273ColorVui {
            full_range: Some(true),
            ..Default::default()
        };
        assert!(v.is_video_signal_type_block_needed());
        assert!(!v.is_colour_description_block_needed());
        assert_eq!(v.full_range_bit(), 1);
    }

    #[test]
    fn any_color_axis_triggers_both_blocks() {
        let v = H273ColorVui {
            primaries: Some(primaries::BT709),
            ..Default::default()
        };
        assert!(v.is_video_signal_type_block_needed());
        assert!(v.is_colour_description_block_needed());
    }

    #[test]
    fn unspecified_substitutes_for_none() {
        let v = H273ColorVui {
            primaries: Some(primaries::BT709),
            transfer: None,
            matrix: Some(matrix::SMPTE170M),
            full_range: Some(false),
        };
        assert_eq!(v.primaries_byte(), 1);
        assert_eq!(v.transfer_byte(), H273_UNSPECIFIED);
        assert_eq!(v.matrix_byte(), 6);
        assert_eq!(v.full_range_bit(), 0);
    }

    #[test]
    fn h273_enumerant_constants_match_spec() {
        // Spot-check the values streamlib's color_info.yaml claims map to
        // H.273. If any of these change the YAML is wrong.
        assert_eq!(primaries::BT709, 1);
        assert_eq!(primaries::SMPTE170M, 6);
        assert_eq!(primaries::BT2020, 9);
        assert_eq!(transfer::BT709, 1);
        assert_eq!(transfer::SRGB, 13);
        assert_eq!(transfer::SMPTE2084, 16);
        assert_eq!(transfer::ARIB_STD_B67, 18);
        assert_eq!(matrix::IDENTITY, 0);
        assert_eq!(matrix::BT709, 1);
        assert_eq!(matrix::SMPTE170M, 6);
        assert_eq!(matrix::BT2020_NCL, 9);
    }
}
