// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Faithful port of:
//!   - nvVulkanh264ScalingList.h
//!   - nvVulkanh264ScalingList.cpp
//!
//! H.264 scaling list tables and derivation logic used in quantization.
//! Includes default scaling matrices (flat and spec-defined), zigzag scan
//! patterns (Tables 8-12 and 8-12a), and functions to derive weight scale
//! matrices from SPS and PPS scaling list parameters.

// ---------------------------------------------------------------------------
// Scaling list type enum (maps to NvScalingListTypeH264)
// ---------------------------------------------------------------------------

/// Indicates how a scaling list entry was signaled in the bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScalingListType {
    /// Scaling list was not present in the bitstream.
    NotPresent = 0,
    /// Scaling list was explicitly present in the bitstream.
    Present = 1,
    /// Scaling list signals fall-back to the default scaling list.
    UseDefault = 2,
}

impl From<u8> for ScalingListType {
    fn from(v: u8) -> Self {
        match v {
            1 => ScalingListType::Present,
            2 => ScalingListType::UseDefault,
            _ => ScalingListType::NotPresent,
        }
    }
}

// ---------------------------------------------------------------------------
// Scaling list struct (maps to NvScalingListH264)
// ---------------------------------------------------------------------------

/// H.264 scaling list data parsed from SPS or PPS.
///
/// Maps to `NvScalingListH264` in the C++ source.
#[derive(Debug, Clone)]
pub struct ScalingListH264 {
    /// Whether `seq_scaling_matrix_present_flag` or
    /// `pic_scaling_matrix_present_flag` was set.
    pub scaling_matrix_present_flag: bool,
    /// Per-list type for indices 0..7 (6 x 4x4 lists + 2 x 8x8 lists).
    pub scaling_list_type: [ScalingListType; 8],
    /// 4x4 scaling lists (6 lists of 16 coefficients each, in scan order).
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// 8x8 scaling lists (2 lists of 64 coefficients each, in scan order).
    pub scaling_list_8x8: [[u8; 64]; 2],
}

impl Default for ScalingListH264 {
    fn default() -> Self {
        Self {
            scaling_matrix_present_flag: false,
            scaling_list_type: [ScalingListType::NotPresent; 8],
            scaling_list_4x4: [[0u8; 16]; 6],
            scaling_list_8x8: [[0u8; 64]; 2],
        }
    }
}

// ---------------------------------------------------------------------------
// Default scaling list matrices
// ---------------------------------------------------------------------------

/// Flat 4x4 matrix — all values 16.
pub const FLAT_4X4_16: [[u8; 4]; 4] = [
    [16, 16, 16, 16],
    [16, 16, 16, 16],
    [16, 16, 16, 16],
    [16, 16, 16, 16],
];

/// Default 4x4 intra scaling matrix (Table 7-3).
pub const DEFAULT_4X4_INTRA: [[u8; 4]; 4] = [
    [ 6, 13, 20, 28],
    [13, 20, 28, 32],
    [20, 28, 32, 37],
    [28, 32, 37, 42],
];

/// Default 4x4 inter scaling matrix (Table 7-4).
pub const DEFAULT_4X4_INTER: [[u8; 4]; 4] = [
    [10, 14, 20, 24],
    [14, 20, 24, 27],
    [20, 24, 27, 30],
    [24, 27, 30, 34],
];

/// Flat 8x8 matrix — all values 16.
pub const FLAT_8X8_16: [[u8; 8]; 8] = [
    [16, 16, 16, 16, 16, 16, 16, 16],
    [16, 16, 16, 16, 16, 16, 16, 16],
    [16, 16, 16, 16, 16, 16, 16, 16],
    [16, 16, 16, 16, 16, 16, 16, 16],
    [16, 16, 16, 16, 16, 16, 16, 16],
    [16, 16, 16, 16, 16, 16, 16, 16],
    [16, 16, 16, 16, 16, 16, 16, 16],
    [16, 16, 16, 16, 16, 16, 16, 16],
];

/// Default 8x8 intra scaling matrix (Table 7-5).
pub const DEFAULT_8X8_INTRA: [[u8; 8]; 8] = [
    [ 6, 10, 13, 16, 18, 23, 25, 27],
    [10, 11, 16, 18, 23, 25, 27, 29],
    [13, 16, 18, 23, 25, 27, 29, 31],
    [16, 18, 23, 25, 27, 29, 31, 33],
    [18, 23, 25, 27, 29, 31, 33, 36],
    [23, 25, 27, 29, 31, 33, 36, 38],
    [25, 27, 29, 31, 33, 36, 38, 40],
    [27, 29, 31, 33, 36, 38, 40, 42],
];

/// Default 8x8 inter scaling matrix (Table 7-6).
pub const DEFAULT_8X8_INTER: [[u8; 8]; 8] = [
    [ 9, 13, 15, 17, 19, 21, 22, 24],
    [13, 13, 17, 19, 21, 22, 24, 25],
    [15, 17, 19, 21, 22, 24, 25, 27],
    [17, 19, 21, 22, 24, 25, 27, 28],
    [19, 21, 22, 24, 25, 27, 28, 30],
    [21, 22, 24, 25, 27, 28, 30, 32],
    [22, 24, 25, 27, 28, 30, 32, 33],
    [24, 25, 27, 28, 30, 32, 33, 35],
];

// ---------------------------------------------------------------------------
// Zigzag scan maps (Tables 8-12, 8-12a)
// ---------------------------------------------------------------------------

/// 4x4 zigzag scan order (Table 8-12).
/// Each entry is (row, col) for scan position k = 0..15.
const ZIGZAG_MAP_4X4: [(usize, usize); 16] = [
    (0,0), (0,1), (1,0), (2,0), (1,1), (0,2), (0,3), (1,2),
    (2,1), (3,0), (3,1), (2,2), (1,3), (2,3), (3,2), (3,3),
];

/// 8x8 zigzag scan order (Table 8-12a).
/// Each entry is (row, col) for scan position k = 0..63.
const ZIGZAG_MAP_8X8: [(usize, usize); 64] = [
    (0,0), (0,1), (1,0), (2,0), (1,1), (0,2), (0,3), (1,2),
    (2,1), (3,0), (4,0), (3,1), (2,2), (1,3), (0,4), (0,5),
    (1,4), (2,3), (3,2), (4,1), (5,0), (6,0), (5,1), (4,2),
    (3,3), (2,4), (1,5), (0,6), (0,7), (1,6), (2,5), (3,4),
    (4,3), (5,2), (6,1), (7,0), (7,1), (6,2), (5,3), (4,4),
    (3,5), (2,6), (1,7), (2,7), (3,6), (4,5), (5,4), (6,3),
    (7,2), (7,3), (6,4), (5,5), (4,6), (3,7), (4,7), (5,6),
    (6,5), (7,4), (7,5), (6,6), (5,7), (6,7), (7,6), (7,7),
];

// ---------------------------------------------------------------------------
// Matrix helper functions
// ---------------------------------------------------------------------------

/// Copy a 4x4 matrix.
///
/// Maps to `matrix_from_matrix_4x4` in the C++ source.
#[inline]
fn matrix_copy_4x4(dst: &mut [[u8; 4]; 4], src: &[[u8; 4]; 4]) {
    *dst = *src;
}

/// Copy an 8x8 matrix.
///
/// Maps to `matrix_from_matrix_8x8` in the C++ source.
#[inline]
fn matrix_copy_8x8(dst: &mut [[u8; 8]; 8], src: &[[u8; 8]; 8]) {
    *dst = *src;
}

/// Convert a 16-element scan-order list to a 4x4 matrix using the zigzag
/// scan pattern from Table 8-12.
///
/// Maps to `matrix_from_list_4x4` in the C++ source.
pub fn matrix_from_list_4x4(list: &[u8; 16]) -> [[u8; 4]; 4] {
    let mut matrix = [[0u8; 4]; 4];
    for (k, &val) in list.iter().enumerate() {
        let (i, j) = ZIGZAG_MAP_4X4[k];
        matrix[i][j] = val;
    }
    matrix
}

/// Convert a 64-element scan-order list to an 8x8 matrix using the zigzag
/// scan pattern from Table 8-12a.
///
/// Maps to `matrix_from_list_8x8` in the C++ source.
pub fn matrix_from_list_8x8(list: &[u8; 64]) -> [[u8; 8]; 8] {
    let mut matrix = [[0u8; 8]; 8];
    for (k, &val) in list.iter().enumerate() {
        let (i, j) = ZIGZAG_MAP_8X8[k];
        matrix[i][j] = val;
    }
    matrix
}

// ---------------------------------------------------------------------------
// SPS scaling list derivation
// ---------------------------------------------------------------------------

/// Derive weight-scale matrices from SPS scaling list parameters.
///
/// Returns `true` if `scaling_matrix_present_flag` was set in the SPS,
/// `false` otherwise (in which case flat matrices are produced).
///
/// Maps to `SetSpsScalingListsH264` in the C++ source.
pub fn set_sps_scaling_lists_h264(
    seq_scaling_list: Option<&ScalingListH264>,
    weight_scale_4x4: &mut [[[u8; 4]; 4]; 6],
    weight_scale_8x8: &mut [[[u8; 8]; 8]; 2],
) -> bool {
    let seq = match seq_scaling_list {
        Some(s) if s.scaling_matrix_present_flag => s,
        _ => {
            // No scaling list present — use flat matrices.
            for m in weight_scale_4x4.iter_mut() {
                matrix_copy_4x4(m, &FLAT_4X4_16);
            }
            for m in weight_scale_8x8.iter_mut() {
                matrix_copy_8x8(m, &FLAT_8X8_16);
            }
            return false;
        }
    };

    // 4x4 lists (indices 0..5)
    for i in 0..6usize {
        let list_type = seq.scaling_list_type[i];
        if list_type != ScalingListType::NotPresent {
            if list_type == ScalingListType::UseDefault {
                let default = if i < 3 { &DEFAULT_4X4_INTRA } else { &DEFAULT_4X4_INTER };
                matrix_copy_4x4(&mut weight_scale_4x4[i], default);
            } else {
                weight_scale_4x4[i] = matrix_from_list_4x4(&seq.scaling_list_4x4[i]);
            }
        } else {
            // Fall-back rule set A
            if i == 0 || i == 3 {
                let default = if i < 3 { &DEFAULT_4X4_INTRA } else { &DEFAULT_4X4_INTER };
                matrix_copy_4x4(&mut weight_scale_4x4[i], default);
            } else {
                // Copy from previous list — must clone to avoid borrow conflict.
                let prev = weight_scale_4x4[i - 1];
                weight_scale_4x4[i] = prev;
            }
        }
    }

    // 8x8 lists (indices 6..7 in scaling_list_type, mapped to 0..1)
    for i in 6..8usize {
        let idx = i - 6;
        let list_type = seq.scaling_list_type[i];
        if list_type != ScalingListType::NotPresent {
            if list_type == ScalingListType::UseDefault {
                let default = if i < 7 { &DEFAULT_8X8_INTRA } else { &DEFAULT_8X8_INTER };
                matrix_copy_8x8(&mut weight_scale_8x8[idx], default);
            } else {
                weight_scale_8x8[idx] = matrix_from_list_8x8(&seq.scaling_list_8x8[idx]);
            }
        } else {
            // Fall-back rule set A
            let default = if i < 7 { &DEFAULT_8X8_INTRA } else { &DEFAULT_8X8_INTER };
            matrix_copy_8x8(&mut weight_scale_8x8[idx], default);
        }
    }

    true
}

// ---------------------------------------------------------------------------
// PPS scaling list derivation
// ---------------------------------------------------------------------------

/// Derive weight-scale matrices from PPS scaling list parameters, using
/// the SPS weight-scale matrices as fall-back when needed.
///
/// Returns `true` if `scaling_matrix_present_flag` was set in the PPS,
/// `false` otherwise.
///
/// Maps to `SetPpsScalingListsH264` in the C++ source.
pub fn set_pps_scaling_lists_h264(
    pic_scaling_list: Option<&ScalingListH264>,
    seq_scaling_matrix_present_flag: bool,
    sps_weight_scale_4x4: &[[[u8; 4]; 4]; 6],
    sps_weight_scale_8x8: &[[[u8; 8]; 8]; 2],
    weight_scale_4x4: &mut [[[u8; 4]; 4]; 6],
    weight_scale_8x8: &mut [[[u8; 8]; 8]; 2],
) -> bool {
    let pic = match pic_scaling_list {
        Some(p) if p.scaling_matrix_present_flag => p,
        _ => {
            // No PPS scaling list — fall back to SPS or flat.
            if seq_scaling_matrix_present_flag {
                *weight_scale_4x4 = *sps_weight_scale_4x4;
            } else {
                for m in weight_scale_4x4.iter_mut() {
                    matrix_copy_4x4(m, &FLAT_4X4_16);
                }
            }
            if seq_scaling_matrix_present_flag {
                *weight_scale_8x8 = *sps_weight_scale_8x8;
            } else {
                for m in weight_scale_8x8.iter_mut() {
                    matrix_copy_8x8(m, &FLAT_8X8_16);
                }
            }
            return false;
        }
    };

    // 4x4 lists (indices 0..5)
    for i in 0..6usize {
        let list_type = pic.scaling_list_type[i];
        if list_type != ScalingListType::NotPresent {
            if list_type == ScalingListType::UseDefault {
                let default = if i < 3 { &DEFAULT_4X4_INTRA } else { &DEFAULT_4X4_INTER };
                matrix_copy_4x4(&mut weight_scale_4x4[i], default);
            } else {
                weight_scale_4x4[i] = matrix_from_list_4x4(&pic.scaling_list_4x4[i]);
            }
        } else if !seq_scaling_matrix_present_flag {
            // Fall-back rule set A
            if i == 0 || i == 3 {
                let default = if i < 3 { &DEFAULT_4X4_INTRA } else { &DEFAULT_4X4_INTER };
                matrix_copy_4x4(&mut weight_scale_4x4[i], default);
            } else {
                let prev = weight_scale_4x4[i - 1];
                weight_scale_4x4[i] = prev;
            }
        } else {
            // Fall-back rule set B
            if i == 0 || i == 3 {
                matrix_copy_4x4(&mut weight_scale_4x4[i], &sps_weight_scale_4x4[i]);
            } else {
                let prev = weight_scale_4x4[i - 1];
                weight_scale_4x4[i] = prev;
            }
        }
    }

    // 8x8 lists (indices 6..7)
    for i in 6..8usize {
        let idx = i - 6;
        let list_type = pic.scaling_list_type[i];
        if list_type != ScalingListType::NotPresent {
            if list_type == ScalingListType::UseDefault {
                let default = if i < 7 { &DEFAULT_8X8_INTRA } else { &DEFAULT_8X8_INTER };
                matrix_copy_8x8(&mut weight_scale_8x8[idx], default);
            } else {
                weight_scale_8x8[idx] = matrix_from_list_8x8(&pic.scaling_list_8x8[idx]);
            }
        } else if !seq_scaling_matrix_present_flag {
            // Fall-back rule set A
            let default = if i < 7 { &DEFAULT_8X8_INTRA } else { &DEFAULT_8X8_INTER };
            matrix_copy_8x8(&mut weight_scale_8x8[idx], default);
        } else {
            // Fall-back rule set B
            matrix_copy_8x8(&mut weight_scale_8x8[idx], &sps_weight_scale_8x8[idx]);
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Combined SPS + PPS derivation (top-level entry point)
// ---------------------------------------------------------------------------

/// Derive final weight-scale matrices from combined SPS and PPS scaling lists.
///
/// This is the main entry point: it first derives SPS matrices, then uses
/// those as fall-back for PPS derivation.
///
/// Maps to `SetSeqPicScalingListsH264` in the C++ source.
pub fn set_seq_pic_scaling_lists_h264(
    seq_scaling_list: Option<&ScalingListH264>,
    pic_scaling_list: Option<&ScalingListH264>,
    weight_scale_4x4: &mut [[[u8; 4]; 4]; 6],
    weight_scale_8x8: &mut [[[u8; 8]; 8]; 2],
) -> bool {
    let mut sps_weight_scale_4x4 = [[[0u8; 4]; 4]; 6];
    let mut sps_weight_scale_8x8 = [[[0u8; 8]; 8]; 2];

    let seq_scaling_matrix_present_flag = set_sps_scaling_lists_h264(
        seq_scaling_list,
        &mut sps_weight_scale_4x4,
        &mut sps_weight_scale_8x8,
    );

    set_pps_scaling_lists_h264(
        pic_scaling_list,
        seq_scaling_matrix_present_flag,
        &sps_weight_scale_4x4,
        &sps_weight_scale_8x8,
        weight_scale_4x4,
        weight_scale_8x8,
    )
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Default matrix sanity checks
    // -----------------------------------------------------------------------

    #[test]
    fn flat_4x4_all_16() {
        for row in &FLAT_4X4_16 {
            for &val in row {
                assert_eq!(val, 16);
            }
        }
    }

    #[test]
    fn flat_8x8_all_16() {
        for row in &FLAT_8X8_16 {
            for &val in row {
                assert_eq!(val, 16);
            }
        }
    }

    #[test]
    fn default_4x4_intra_corners() {
        assert_eq!(DEFAULT_4X4_INTRA[0][0], 6);
        assert_eq!(DEFAULT_4X4_INTRA[0][3], 28);
        assert_eq!(DEFAULT_4X4_INTRA[3][0], 28);
        assert_eq!(DEFAULT_4X4_INTRA[3][3], 42);
    }

    #[test]
    fn default_4x4_inter_corners() {
        assert_eq!(DEFAULT_4X4_INTER[0][0], 10);
        assert_eq!(DEFAULT_4X4_INTER[0][3], 24);
        assert_eq!(DEFAULT_4X4_INTER[3][0], 24);
        assert_eq!(DEFAULT_4X4_INTER[3][3], 34);
    }

    #[test]
    fn default_8x8_intra_corners() {
        assert_eq!(DEFAULT_8X8_INTRA[0][0], 6);
        assert_eq!(DEFAULT_8X8_INTRA[0][7], 27);
        assert_eq!(DEFAULT_8X8_INTRA[7][0], 27);
        assert_eq!(DEFAULT_8X8_INTRA[7][7], 42);
    }

    #[test]
    fn default_8x8_inter_corners() {
        assert_eq!(DEFAULT_8X8_INTER[0][0], 9);
        assert_eq!(DEFAULT_8X8_INTER[0][7], 24);
        assert_eq!(DEFAULT_8X8_INTER[7][0], 24);
        assert_eq!(DEFAULT_8X8_INTER[7][7], 35);
    }

    // -----------------------------------------------------------------------
    // Zigzag scan ordering tests
    // -----------------------------------------------------------------------

    #[test]
    fn zigzag_4x4_starts_at_origin() {
        assert_eq!(ZIGZAG_MAP_4X4[0], (0, 0));
    }

    #[test]
    fn zigzag_4x4_ends_at_corner() {
        assert_eq!(ZIGZAG_MAP_4X4[15], (3, 3));
    }

    #[test]
    fn zigzag_4x4_covers_all_positions() {
        let mut seen = [[false; 4]; 4];
        for &(r, c) in &ZIGZAG_MAP_4X4 {
            assert!(!seen[r][c], "duplicate position ({}, {})", r, c);
            seen[r][c] = true;
        }
        for r in 0..4 {
            for c in 0..4 {
                assert!(seen[r][c], "missing position ({}, {})", r, c);
            }
        }
    }

    #[test]
    fn zigzag_8x8_starts_at_origin() {
        assert_eq!(ZIGZAG_MAP_8X8[0], (0, 0));
    }

    #[test]
    fn zigzag_8x8_ends_at_corner() {
        assert_eq!(ZIGZAG_MAP_8X8[63], (7, 7));
    }

    #[test]
    fn zigzag_8x8_covers_all_positions() {
        let mut seen = [[false; 8]; 8];
        for &(r, c) in &ZIGZAG_MAP_8X8 {
            assert!(!seen[r][c], "duplicate position ({}, {})", r, c);
            seen[r][c] = true;
        }
        for r in 0..8 {
            for c in 0..8 {
                assert!(seen[r][c], "missing position ({}, {})", r, c);
            }
        }
    }

    // -----------------------------------------------------------------------
    // matrix_from_list round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn matrix_from_list_4x4_identity() {
        // A list of 0..15 placed through the zigzag should produce a matrix
        // where each zigzag position k has value k.
        let list: [u8; 16] = core::array::from_fn(|i| i as u8);
        let matrix = matrix_from_list_4x4(&list);
        for (k, &(r, c)) in ZIGZAG_MAP_4X4.iter().enumerate() {
            assert_eq!(
                matrix[r][c], k as u8,
                "mismatch at zigzag position {k} -> ({r},{c})"
            );
        }
    }

    #[test]
    fn matrix_from_list_8x8_identity() {
        let list: [u8; 64] = core::array::from_fn(|i| i as u8);
        let matrix = matrix_from_list_8x8(&list);
        for (k, &(r, c)) in ZIGZAG_MAP_8X8.iter().enumerate() {
            assert_eq!(
                matrix[r][c], k as u8,
                "mismatch at zigzag position {k} -> ({r},{c})"
            );
        }
    }

    #[test]
    fn matrix_from_list_4x4_flat() {
        let list = [16u8; 16];
        let matrix = matrix_from_list_4x4(&list);
        assert_eq!(matrix, FLAT_4X4_16);
    }

    #[test]
    fn matrix_from_list_8x8_flat() {
        let list = [16u8; 64];
        let matrix = matrix_from_list_8x8(&list);
        assert_eq!(matrix, FLAT_8X8_16);
    }

    // -----------------------------------------------------------------------
    // SPS scaling list derivation
    // -----------------------------------------------------------------------

    #[test]
    fn sps_none_produces_flat() {
        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let result = set_sps_scaling_lists_h264(None, &mut w4, &mut w8);
        assert!(!result);
        for m in &w4 {
            assert_eq!(*m, FLAT_4X4_16);
        }
        for m in &w8 {
            assert_eq!(*m, FLAT_8X8_16);
        }
    }

    #[test]
    fn sps_flag_false_produces_flat() {
        let sps = ScalingListH264::default(); // scaling_matrix_present_flag = false
        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let result = set_sps_scaling_lists_h264(Some(&sps), &mut w4, &mut w8);
        assert!(!result);
        for m in &w4 {
            assert_eq!(*m, FLAT_4X4_16);
        }
        for m in &w8 {
            assert_eq!(*m, FLAT_8X8_16);
        }
    }

    #[test]
    fn sps_use_default_gives_spec_defaults() {
        let mut sps = ScalingListH264::default();
        sps.scaling_matrix_present_flag = true;
        for t in sps.scaling_list_type.iter_mut() {
            *t = ScalingListType::UseDefault;
        }

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let result = set_sps_scaling_lists_h264(Some(&sps), &mut w4, &mut w8);
        assert!(result);

        // Indices 0,1,2 -> intra default; 3,4,5 -> inter default
        for i in 0..3 {
            assert_eq!(w4[i], DEFAULT_4X4_INTRA, "4x4 index {i}");
        }
        for i in 3..6 {
            assert_eq!(w4[i], DEFAULT_4X4_INTER, "4x4 index {i}");
        }
        assert_eq!(w8[0], DEFAULT_8X8_INTRA);
        assert_eq!(w8[1], DEFAULT_8X8_INTER);
    }

    #[test]
    fn sps_not_present_fallback_rule_a() {
        // When all lists are NOT_PRESENT, fall-back rule set A applies:
        // i==0 -> default intra, i==1,2 -> copy from previous
        // i==3 -> default inter, i==4,5 -> copy from previous
        // 8x8: i==6 -> default intra, i==7 -> default inter
        let mut sps = ScalingListH264::default();
        sps.scaling_matrix_present_flag = true;
        // scaling_list_type is already all NotPresent

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        set_sps_scaling_lists_h264(Some(&sps), &mut w4, &mut w8);

        assert_eq!(w4[0], DEFAULT_4X4_INTRA);
        assert_eq!(w4[1], DEFAULT_4X4_INTRA); // copied from [0]
        assert_eq!(w4[2], DEFAULT_4X4_INTRA); // copied from [1]
        assert_eq!(w4[3], DEFAULT_4X4_INTER);
        assert_eq!(w4[4], DEFAULT_4X4_INTER); // copied from [3]
        assert_eq!(w4[5], DEFAULT_4X4_INTER); // copied from [4]
        assert_eq!(w8[0], DEFAULT_8X8_INTRA);
        assert_eq!(w8[1], DEFAULT_8X8_INTER);
    }

    #[test]
    fn sps_present_list_uses_zigzag() {
        let mut sps = ScalingListH264::default();
        sps.scaling_matrix_present_flag = true;
        sps.scaling_list_type[0] = ScalingListType::Present;
        // Fill list 0 with sequential values
        for j in 0..16 {
            sps.scaling_list_4x4[0][j] = (j + 1) as u8;
        }
        // Others: NotPresent -> will use fall-back

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        set_sps_scaling_lists_h264(Some(&sps), &mut w4, &mut w8);

        // Verify the zigzag-scanned matrix for list 0
        let expected = matrix_from_list_4x4(&sps.scaling_list_4x4[0]);
        assert_eq!(w4[0], expected);
        // List 1 should copy from list 0 (fall-back rule A)
        assert_eq!(w4[1], expected);
    }

    // -----------------------------------------------------------------------
    // PPS scaling list derivation
    // -----------------------------------------------------------------------

    #[test]
    fn pps_none_with_sps_flag_copies_sps() {
        // When PPS has no scaling list but SPS had one, copy SPS matrices.
        let sps_w4 = [DEFAULT_4X4_INTRA; 6];
        let sps_w8 = [DEFAULT_8X8_INTRA; 2];
        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];

        let result = set_pps_scaling_lists_h264(
            None, true, &sps_w4, &sps_w8, &mut w4, &mut w8,
        );
        assert!(!result);
        assert_eq!(w4, sps_w4);
        assert_eq!(w8, sps_w8);
    }

    #[test]
    fn pps_none_without_sps_flag_gives_flat() {
        let sps_w4 = [[[99u8; 4]; 4]; 6]; // should NOT be used
        let sps_w8 = [[[99u8; 8]; 8]; 2];
        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];

        let result = set_pps_scaling_lists_h264(
            None, false, &sps_w4, &sps_w8, &mut w4, &mut w8,
        );
        assert!(!result);
        for m in &w4 {
            assert_eq!(*m, FLAT_4X4_16);
        }
        for m in &w8 {
            assert_eq!(*m, FLAT_8X8_16);
        }
    }

    #[test]
    fn pps_not_present_fallback_rule_b_uses_sps() {
        // PPS scaling_matrix_present_flag = true, all lists NOT_PRESENT,
        // but seq_scaling_matrix_present_flag = true -> rule set B.
        // Rule B: i==0,3 copy from SPS; i==1,2,4,5 copy from previous.
        let mut sps_w4 = [[[0u8; 4]; 4]; 6];
        sps_w4[0] = DEFAULT_4X4_INTRA;
        sps_w4[3] = DEFAULT_4X4_INTER;
        let mut sps_w8 = [[[0u8; 8]; 8]; 2];
        sps_w8[0] = DEFAULT_8X8_INTRA;
        sps_w8[1] = DEFAULT_8X8_INTER;

        let mut pps = ScalingListH264::default();
        pps.scaling_matrix_present_flag = true;
        // all types = NotPresent

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let result = set_pps_scaling_lists_h264(
            Some(&pps), true, &sps_w4, &sps_w8, &mut w4, &mut w8,
        );
        assert!(result);

        // i=0: from SPS[0]
        assert_eq!(w4[0], DEFAULT_4X4_INTRA);
        // i=1: from w4[0] (previous)
        assert_eq!(w4[1], DEFAULT_4X4_INTRA);
        // i=2: from w4[1]
        assert_eq!(w4[2], DEFAULT_4X4_INTRA);
        // i=3: from SPS[3]
        assert_eq!(w4[3], DEFAULT_4X4_INTER);
        // i=4: from w4[3]
        assert_eq!(w4[4], DEFAULT_4X4_INTER);
        // i=5: from w4[4]
        assert_eq!(w4[5], DEFAULT_4X4_INTER);

        // 8x8: from SPS
        assert_eq!(w8[0], DEFAULT_8X8_INTRA);
        assert_eq!(w8[1], DEFAULT_8X8_INTER);
    }

    #[test]
    fn pps_not_present_fallback_rule_a_no_sps() {
        // PPS scaling_matrix_present_flag = true, all lists NOT_PRESENT,
        // seq_scaling_matrix_present_flag = false -> rule set A.
        let sps_w4 = [[[0u8; 4]; 4]; 6];
        let sps_w8 = [[[0u8; 8]; 8]; 2];

        let mut pps = ScalingListH264::default();
        pps.scaling_matrix_present_flag = true;

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        set_pps_scaling_lists_h264(
            Some(&pps), false, &sps_w4, &sps_w8, &mut w4, &mut w8,
        );

        assert_eq!(w4[0], DEFAULT_4X4_INTRA);
        assert_eq!(w4[1], DEFAULT_4X4_INTRA);
        assert_eq!(w4[2], DEFAULT_4X4_INTRA);
        assert_eq!(w4[3], DEFAULT_4X4_INTER);
        assert_eq!(w4[4], DEFAULT_4X4_INTER);
        assert_eq!(w4[5], DEFAULT_4X4_INTER);
        assert_eq!(w8[0], DEFAULT_8X8_INTRA);
        assert_eq!(w8[1], DEFAULT_8X8_INTER);
    }

    // -----------------------------------------------------------------------
    // Combined SPS + PPS (top-level entry point)
    // -----------------------------------------------------------------------

    #[test]
    fn combined_no_sps_no_pps_gives_flat() {
        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let result = set_seq_pic_scaling_lists_h264(None, None, &mut w4, &mut w8);
        assert!(!result);
        for m in &w4 {
            assert_eq!(*m, FLAT_4X4_16);
        }
        for m in &w8 {
            assert_eq!(*m, FLAT_8X8_16);
        }
    }

    #[test]
    fn combined_sps_defaults_no_pps() {
        let mut sps = ScalingListH264::default();
        sps.scaling_matrix_present_flag = true;
        for t in sps.scaling_list_type.iter_mut() {
            *t = ScalingListType::UseDefault;
        }

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let result = set_seq_pic_scaling_lists_h264(Some(&sps), None, &mut w4, &mut w8);
        // PPS not present -> returns false, but copies SPS matrices
        assert!(!result);

        for i in 0..3 {
            assert_eq!(w4[i], DEFAULT_4X4_INTRA);
        }
        for i in 3..6 {
            assert_eq!(w4[i], DEFAULT_4X4_INTER);
        }
        assert_eq!(w8[0], DEFAULT_8X8_INTRA);
        assert_eq!(w8[1], DEFAULT_8X8_INTER);
    }

    #[test]
    fn combined_sps_and_pps_both_use_default() {
        let mut sps = ScalingListH264::default();
        sps.scaling_matrix_present_flag = true;
        for t in sps.scaling_list_type.iter_mut() {
            *t = ScalingListType::UseDefault;
        }

        let mut pps = ScalingListH264::default();
        pps.scaling_matrix_present_flag = true;
        for t in pps.scaling_list_type.iter_mut() {
            *t = ScalingListType::UseDefault;
        }

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        let result = set_seq_pic_scaling_lists_h264(
            Some(&sps), Some(&pps), &mut w4, &mut w8,
        );
        assert!(result);

        for i in 0..3 {
            assert_eq!(w4[i], DEFAULT_4X4_INTRA);
        }
        for i in 3..6 {
            assert_eq!(w4[i], DEFAULT_4X4_INTER);
        }
        assert_eq!(w8[0], DEFAULT_8X8_INTRA);
        assert_eq!(w8[1], DEFAULT_8X8_INTER);
    }

    // -----------------------------------------------------------------------
    // ScalingListType conversion
    // -----------------------------------------------------------------------

    #[test]
    fn scaling_list_type_from_u8() {
        assert_eq!(ScalingListType::from(0), ScalingListType::NotPresent);
        assert_eq!(ScalingListType::from(1), ScalingListType::Present);
        assert_eq!(ScalingListType::from(2), ScalingListType::UseDefault);
        assert_eq!(ScalingListType::from(255), ScalingListType::NotPresent);
    }

    // -----------------------------------------------------------------------
    // Default matrix value ranges (spec sanity)
    // -----------------------------------------------------------------------

    #[test]
    fn default_matrices_values_in_range() {
        // H.264 spec: scaling list values must be 1..=255 (nonzero u8).
        for row in &DEFAULT_4X4_INTRA {
            for &v in row {
                assert!(v >= 1);
            }
        }
        for row in &DEFAULT_4X4_INTER {
            for &v in row {
                assert!(v >= 1);
            }
        }
        for row in &DEFAULT_8X8_INTRA {
            for &v in row {
                assert!(v >= 1);
            }
        }
        for row in &DEFAULT_8X8_INTER {
            for &v in row {
                assert!(v >= 1);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Present 8x8 scaling list through SPS
    // -----------------------------------------------------------------------

    #[test]
    fn sps_present_8x8_list_uses_zigzag() {
        let mut sps = ScalingListH264::default();
        sps.scaling_matrix_present_flag = true;
        // Set 4x4 to UseDefault so they don't interfere
        for i in 0..6 {
            sps.scaling_list_type[i] = ScalingListType::UseDefault;
        }
        sps.scaling_list_type[6] = ScalingListType::Present;
        sps.scaling_list_type[7] = ScalingListType::Present;
        for j in 0..64 {
            sps.scaling_list_8x8[0][j] = (j + 1) as u8;
            sps.scaling_list_8x8[1][j] = (64 - j) as u8;
        }

        let mut w4 = [[[0u8; 4]; 4]; 6];
        let mut w8 = [[[0u8; 8]; 8]; 2];
        set_sps_scaling_lists_h264(Some(&sps), &mut w4, &mut w8);

        let expected_0 = matrix_from_list_8x8(&sps.scaling_list_8x8[0]);
        let expected_1 = matrix_from_list_8x8(&sps.scaling_list_8x8[1]);
        assert_eq!(w8[0], expected_0);
        assert_eq!(w8[1], expected_1);
    }
}
