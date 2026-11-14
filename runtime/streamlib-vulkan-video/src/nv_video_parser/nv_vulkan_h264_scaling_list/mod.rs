// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
    [6, 13, 20, 28],
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
    [6, 10, 13, 16, 18, 23, 25, 27],
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
    [9, 13, 15, 17, 19, 21, 22, 24],
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
    (0, 0),
    (0, 1),
    (1, 0),
    (2, 0),
    (1, 1),
    (0, 2),
    (0, 3),
    (1, 2),
    (2, 1),
    (3, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (2, 3),
    (3, 2),
    (3, 3),
];

/// 8x8 zigzag scan order (Table 8-12a).
/// Each entry is (row, col) for scan position k = 0..63.
const ZIGZAG_MAP_8X8: [(usize, usize); 64] = [
    (0, 0),
    (0, 1),
    (1, 0),
    (2, 0),
    (1, 1),
    (0, 2),
    (0, 3),
    (1, 2),
    (2, 1),
    (3, 0),
    (4, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (0, 4),
    (0, 5),
    (1, 4),
    (2, 3),
    (3, 2),
    (4, 1),
    (5, 0),
    (6, 0),
    (5, 1),
    (4, 2),
    (3, 3),
    (2, 4),
    (1, 5),
    (0, 6),
    (0, 7),
    (1, 6),
    (2, 5),
    (3, 4),
    (4, 3),
    (5, 2),
    (6, 1),
    (7, 0),
    (7, 1),
    (6, 2),
    (5, 3),
    (4, 4),
    (3, 5),
    (2, 6),
    (1, 7),
    (2, 7),
    (3, 6),
    (4, 5),
    (5, 4),
    (6, 3),
    (7, 2),
    (7, 3),
    (6, 4),
    (5, 5),
    (4, 6),
    (3, 7),
    (4, 7),
    (5, 6),
    (6, 5),
    (7, 4),
    (7, 5),
    (6, 6),
    (5, 7),
    (6, 7),
    (7, 6),
    (7, 7),
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
                let default = if i < 3 {
                    &DEFAULT_4X4_INTRA
                } else {
                    &DEFAULT_4X4_INTER
                };
                matrix_copy_4x4(&mut weight_scale_4x4[i], default);
            } else {
                weight_scale_4x4[i] = matrix_from_list_4x4(&seq.scaling_list_4x4[i]);
            }
        } else {
            // Fall-back rule set A
            if i == 0 || i == 3 {
                let default = if i < 3 {
                    &DEFAULT_4X4_INTRA
                } else {
                    &DEFAULT_4X4_INTER
                };
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
                let default = if i < 7 {
                    &DEFAULT_8X8_INTRA
                } else {
                    &DEFAULT_8X8_INTER
                };
                matrix_copy_8x8(&mut weight_scale_8x8[idx], default);
            } else {
                weight_scale_8x8[idx] = matrix_from_list_8x8(&seq.scaling_list_8x8[idx]);
            }
        } else {
            // Fall-back rule set A
            let default = if i < 7 {
                &DEFAULT_8X8_INTRA
            } else {
                &DEFAULT_8X8_INTER
            };
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
                let default = if i < 3 {
                    &DEFAULT_4X4_INTRA
                } else {
                    &DEFAULT_4X4_INTER
                };
                matrix_copy_4x4(&mut weight_scale_4x4[i], default);
            } else {
                weight_scale_4x4[i] = matrix_from_list_4x4(&pic.scaling_list_4x4[i]);
            }
        } else if !seq_scaling_matrix_present_flag {
            // Fall-back rule set A
            if i == 0 || i == 3 {
                let default = if i < 3 {
                    &DEFAULT_4X4_INTRA
                } else {
                    &DEFAULT_4X4_INTER
                };
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
                let default = if i < 7 {
                    &DEFAULT_8X8_INTRA
                } else {
                    &DEFAULT_8X8_INTER
                };
                matrix_copy_8x8(&mut weight_scale_8x8[idx], default);
            } else {
                weight_scale_8x8[idx] = matrix_from_list_8x8(&pic.scaling_list_8x8[idx]);
            }
        } else if !seq_scaling_matrix_present_flag {
            // Fall-back rule set A
            let default = if i < 7 {
                &DEFAULT_8X8_INTRA
            } else {
                &DEFAULT_8X8_INTER
            };
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
mod tests;
