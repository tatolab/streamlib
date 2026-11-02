// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `VulkanH26xDecoder.h` — shared definitions for H.264 and H.265 decoders.
//!
//! This header is included by both `VulkanH264Decoder.h` and `VulkanH265Decoder.h`
//! and provides common slice-level types shared across the H.26x family.

/// Slice type for H.264 (H.26x family).
///
/// Corresponds to `slice_type_e` in the C++ source. These values match the
/// `slice_type` syntax element defined in the H.264 specification (Table 7-6).
///
/// Values 0..4 map to P, B, I, SP, SI respectively. The spec also defines
/// values 5..9 as redundant aliases (5=P, 6=B, 7=I, 8=SP, 9=SI) indicating
/// that all slices in the picture have the same type; the parser normalizes
/// these by subtracting 5 before storing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum SliceType {
    /// Predictive slice — uses one reference list (list 0).
    P = 0,
    /// Bi-predictive slice — uses two reference lists (list 0 and list 1).
    B = 1,
    /// Intra slice — no inter prediction, all macroblocks are intra-coded.
    I = 2,
    /// Switching Predictive slice — used for bitstream switching (Annex A).
    Sp = 3,
    /// Switching Intra slice — used for bitstream switching (Annex A).
    Si = 4,
}

impl SliceType {
    /// Try to convert from a raw `u32` value.
    ///
    /// The H.264 spec defines `slice_type` values 0..9 where 5..9 are
    /// redundant duplicates of 0..4. This function accepts both ranges
    /// and normalizes 5..9 down to 0..4.
    pub fn from_raw(value: u32) -> Option<Self> {
        // Normalize the 5..9 range down to 0..4 per H.264 Table 7-6.
        let normalized = if value >= 5 && value <= 9 {
            value - 5
        } else {
            value
        };
        match normalized {
            0 => Some(SliceType::P),
            1 => Some(SliceType::B),
            2 => Some(SliceType::I),
            3 => Some(SliceType::Sp),
            4 => Some(SliceType::Si),
            _ => None,
        }
    }

    /// Returns `true` if this is an intra-only slice type (`I` or `SI`).
    pub fn is_intra(self) -> bool {
        matches!(self, SliceType::I | SliceType::Si)
    }

    /// Returns `true` if this is a reference (inter-predicted) slice type
    /// (`P`, `B`, `SP`).
    pub fn is_reference(self) -> bool {
        matches!(self, SliceType::P | SliceType::B | SliceType::Sp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_type_repr_values() {
        assert_eq!(SliceType::P as u32, 0);
        assert_eq!(SliceType::B as u32, 1);
        assert_eq!(SliceType::I as u32, 2);
        assert_eq!(SliceType::Sp as u32, 3);
        assert_eq!(SliceType::Si as u32, 4);
    }

    #[test]
    fn slice_type_from_raw_direct() {
        assert_eq!(SliceType::from_raw(0), Some(SliceType::P));
        assert_eq!(SliceType::from_raw(1), Some(SliceType::B));
        assert_eq!(SliceType::from_raw(2), Some(SliceType::I));
        assert_eq!(SliceType::from_raw(3), Some(SliceType::Sp));
        assert_eq!(SliceType::from_raw(4), Some(SliceType::Si));
    }

    #[test]
    fn slice_type_from_raw_normalized() {
        // Values 5..9 are redundant aliases per H.264 Table 7-6.
        assert_eq!(SliceType::from_raw(5), Some(SliceType::P));
        assert_eq!(SliceType::from_raw(6), Some(SliceType::B));
        assert_eq!(SliceType::from_raw(7), Some(SliceType::I));
        assert_eq!(SliceType::from_raw(8), Some(SliceType::Sp));
        assert_eq!(SliceType::from_raw(9), Some(SliceType::Si));
    }

    #[test]
    fn slice_type_from_raw_invalid() {
        assert_eq!(SliceType::from_raw(10), None);
        assert_eq!(SliceType::from_raw(255), None);
    }

    #[test]
    fn slice_type_is_intra() {
        assert!(!SliceType::P.is_intra());
        assert!(!SliceType::B.is_intra());
        assert!(SliceType::I.is_intra());
        assert!(!SliceType::Sp.is_intra());
        assert!(SliceType::Si.is_intra());
    }

    #[test]
    fn slice_type_is_reference() {
        assert!(SliceType::P.is_reference());
        assert!(SliceType::B.is_reference());
        assert!(!SliceType::I.is_reference());
        assert!(SliceType::Sp.is_reference());
        assert!(!SliceType::Si.is_reference());
    }

    #[test]
    fn slice_type_equality() {
        assert_eq!(SliceType::P, SliceType::P);
        assert_ne!(SliceType::P, SliceType::B);
        assert_ne!(SliceType::I, SliceType::Si);
    }
}
