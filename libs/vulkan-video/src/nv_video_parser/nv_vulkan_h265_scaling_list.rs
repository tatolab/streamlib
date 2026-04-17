// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `nvVulkanh265ScalingList.h`
//!
//! H.265 scaling list definitions: parsing structures, default scaling tables,
//! and derivation functions for 4x4, 8x8, 16x16, and 32x32 scaling matrices.

/// Specification of default values of `ScalingList[1..3][matrixId][i]` with `i = 0..63`.
///
/// Index 0: intra, Index 1: inter.
pub const DEFAULT_SCALING_LIST_8X8: [[u8; 64]; 2] = [
    [
        16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18,
        17, 18, 18, 17, 18, 21, 19, 20, 21, 20, 19, 21, 24, 22, 22, 24,
        24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31,
        29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
    ],
    [
        16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18,
        18, 18, 18, 18, 18, 20, 20, 20, 20, 20, 20, 20, 24, 24, 24, 24,
        24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28,
        28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
    ],
];

/// One entry of the scaling list syntax, parsed from the bitstream.
///
/// Corresponds to `scaling_list_entry_s` in the C++ source.
#[derive(Clone, Debug)]
pub struct ScalingListEntry {
    pub scaling_list_pred_mode_flag: i32,
    pub scaling_list_pred_matrix_id_delta: i32,
    pub scaling_list_dc_coef_minus8: i32,
    pub scaling_list_delta_coef: [i8; 64],
}

impl Default for ScalingListEntry {
    fn default() -> Self {
        Self {
            scaling_list_pred_mode_flag: 0,
            scaling_list_pred_matrix_id_delta: 0,
            scaling_list_dc_coef_minus8: 0,
            scaling_list_delta_coef: [0i8; 64],
        }
    }
}

/// Full scaling list data for all size IDs (0..3) and matrix IDs (0..5).
///
/// Corresponds to `scaling_list_s` in the C++ source.
/// Indexed as `entry[size_id][matrix_id]`.
#[derive(Clone, Debug)]
pub struct ScalingList {
    pub entry: [[ScalingListEntry; 6]; 4],
}

impl Default for ScalingList {
    fn default() -> Self {
        // Rust const arrays require Copy, so build manually.
        Self {
            entry: [
                [
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                ],
                [
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                ],
                [
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                ],
                [
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                    ScalingListEntry::default(), ScalingListEntry::default(),
                ],
            ],
        }
    }
}

/// Derive 4x4 scaling matrices from parsed scaling list data.
///
/// Corresponds to `Init4x4ScalingListsH265` in the C++ source.
///
/// `scaling_factors` must have length >= `4 * 4 * 6` (96 bytes).
pub fn init_4x4_scaling_lists_h265(scaling_factors: &mut [u8], scl: &ScalingList) {
    for matrix_id in 0..6_usize {
        let offset = 4 * 4 * matrix_id;
        let scle = &scl.entry[0][matrix_id];

        if scle.scaling_list_pred_mode_flag == 0 {
            if scle.scaling_list_pred_matrix_id_delta != 0 {
                // Duplicate from a reference matrix.
                let ref_matrix_id =
                    matrix_id as i32 - scle.scaling_list_pred_matrix_id_delta;
                debug_assert!(ref_matrix_id >= 0);
                let ref_offset = 4 * 4 * ref_matrix_id as usize;
                // Copy within the same slice — use split or temporary copy.
                let mut tmp = [0u8; 16];
                tmp.copy_from_slice(&scaling_factors[ref_offset..ref_offset + 16]);
                scaling_factors[offset..offset + 16].copy_from_slice(&tmp);
            } else {
                // Default values (4x4): all 16.
                for k in 0..16 {
                    scaling_factors[offset + k] = 16;
                }
            }
        } else {
            // Explicit delta coefficients.
            let mut next_coef: i32 = 8;
            for k in 0..16 {
                next_coef = (next_coef + scle.scaling_list_delta_coef[k] as i32) & 0xff;
                scaling_factors[offset + k] = next_coef as u8;
            }
        }
    }
}

/// Derive 8x8 (and 16x16, 32x32) scaling matrices from parsed scaling list data.
///
/// Corresponds to `Init8x8ScalingListsH265` in the C++ source.
///
/// * `size_id` — 1 for 8x8, 2 for 16x16, 3 for 32x32.
/// * `scaling_factors` — output buffer, length >= `8 * 8 * matrix_count`.
/// * `scaling_factors_dc` — DC coefficients for size_id >= 2, length >= matrix_count.
///   Ignored (may be empty) when `size_id < 2`.
pub fn init_8x8_scaling_lists_h265(
    scaling_factors: &mut [u8],
    scaling_factors_dc: &mut [u8],
    scl: &ScalingList,
    size_id: i32,
) {
    let matrix_count: usize = if size_id == 3 { 2 } else { 6 };

    for matrix_id in 0..matrix_count {
        let offset = 8 * 8 * matrix_id;
        let scle = &scl.entry[size_id as usize][matrix_id];

        if scle.scaling_list_pred_mode_flag == 0 {
            if scle.scaling_list_pred_matrix_id_delta != 0 {
                // Duplicate from a reference matrix.
                let ref_matrix_id =
                    matrix_id as i32 - scle.scaling_list_pred_matrix_id_delta;
                debug_assert!(ref_matrix_id >= 0);
                let ref_offset = 8 * 8 * ref_matrix_id as usize;
                let mut tmp = [0u8; 64];
                tmp.copy_from_slice(&scaling_factors[ref_offset..ref_offset + 64]);
                scaling_factors[offset..offset + 64].copy_from_slice(&tmp);
                if size_id >= 2 {
                    scaling_factors_dc[matrix_id] =
                        scaling_factors_dc[ref_matrix_id as usize];
                }
            } else {
                // Default values (>= 8x8).
                let list_idx = if size_id != 3 {
                    if matrix_id >= 3 { 1 } else { 0 }
                } else {
                    if matrix_id >= 1 { 1 } else { 0 }
                };
                for k in 0..64 {
                    scaling_factors[offset + k] = DEFAULT_SCALING_LIST_8X8[list_idx][k];
                }
                if size_id >= 2 {
                    scaling_factors_dc[matrix_id] = DEFAULT_SCALING_LIST_8X8[list_idx][0];
                }
            }
        } else {
            // Explicit delta coefficients.
            let mut next_coef: i32 = if size_id < 2 {
                8
            } else {
                scle.scaling_list_dc_coef_minus8 + 8
            };
            for k in 0..64 {
                next_coef = (next_coef + scle.scaling_list_delta_coef[k] as i32) & 0xff;
                scaling_factors[offset + k] = next_coef as u8;
            }
            if size_id >= 2 {
                scaling_factors_dc[matrix_id] =
                    (scle.scaling_list_dc_coef_minus8 + 8) as u8;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Default 4x4 scaling: all entries should be 16 when no explicit data.
    #[test]
    fn test_default_4x4() {
        let scl = ScalingList::default();
        let mut factors = [0u8; 4 * 4 * 6];
        init_4x4_scaling_lists_h265(&mut factors, &scl);
        for &v in &factors {
            assert_eq!(v, 16);
        }
    }

    /// Default 8x8 scaling (size_id=1): entries should match the default tables.
    #[test]
    fn test_default_8x8_size_id_1() {
        let scl = ScalingList::default();
        let mut factors = [0u8; 8 * 8 * 6];
        let mut dc = [0u8; 6];
        init_8x8_scaling_lists_h265(&mut factors, &mut dc, &scl, 1);
        // Matrices 0..2 use list_idx 0 (intra), 3..5 use list_idx 1 (inter).
        for matrix_id in 0..6 {
            let list_idx = if matrix_id >= 3 { 1 } else { 0 };
            let offset = 8 * 8 * matrix_id;
            for k in 0..64 {
                assert_eq!(
                    factors[offset + k],
                    DEFAULT_SCALING_LIST_8X8[list_idx][k],
                    "mismatch at matrix_id={matrix_id}, k={k}"
                );
            }
        }
    }

    /// Default 16x16 scaling (size_id=2): same 8x8 defaults plus DC values.
    #[test]
    fn test_default_16x16_size_id_2() {
        let scl = ScalingList::default();
        let mut factors = [0u8; 8 * 8 * 6];
        let mut dc = [0u8; 6];
        init_8x8_scaling_lists_h265(&mut factors, &mut dc, &scl, 2);
        for matrix_id in 0..6 {
            let list_idx = if matrix_id >= 3 { 1 } else { 0 };
            assert_eq!(dc[matrix_id], DEFAULT_SCALING_LIST_8X8[list_idx][0]);
        }
    }

    /// Default 32x32 scaling (size_id=3): only 2 matrices.
    #[test]
    fn test_default_32x32_size_id_3() {
        let scl = ScalingList::default();
        let mut factors = [0u8; 8 * 8 * 2];
        let mut dc = [0u8; 2];
        init_8x8_scaling_lists_h265(&mut factors, &mut dc, &scl, 3);
        // matrix 0 -> list_idx 0, matrix 1 -> list_idx 1
        for matrix_id in 0..2 {
            let list_idx = if matrix_id >= 1 { 1 } else { 0 };
            let offset = 8 * 8 * matrix_id;
            for k in 0..64 {
                assert_eq!(factors[offset + k], DEFAULT_SCALING_LIST_8X8[list_idx][k]);
            }
            assert_eq!(dc[matrix_id], DEFAULT_SCALING_LIST_8X8[list_idx][0]);
        }
    }

    /// 4x4 explicit delta coefficients: nextCoef starts at 8, accumulates.
    #[test]
    fn test_explicit_4x4() {
        let mut scl = ScalingList::default();
        // Set matrix 0 to use explicit mode.
        scl.entry[0][0].scaling_list_pred_mode_flag = 1;
        // All deltas = 0 => every factor = 8.
        let mut factors = [0u8; 4 * 4 * 6];
        init_4x4_scaling_lists_h265(&mut factors, &scl);
        for k in 0..16 {
            assert_eq!(factors[k], 8);
        }
        // Matrices 1..5 should still be default (16).
        for k in 16..96 {
            assert_eq!(factors[k], 16);
        }
    }

    /// 4x4 explicit with non-zero deltas.
    #[test]
    fn test_explicit_4x4_with_deltas() {
        let mut scl = ScalingList::default();
        scl.entry[0][0].scaling_list_pred_mode_flag = 1;
        // delta[0] = 8 => nextCoef = (8+8) & 0xff = 16
        // delta[1] = -1 => nextCoef = (16-1) & 0xff = 15
        scl.entry[0][0].scaling_list_delta_coef[0] = 8;
        scl.entry[0][0].scaling_list_delta_coef[1] = -1;
        let mut factors = [0u8; 4 * 4 * 6];
        init_4x4_scaling_lists_h265(&mut factors, &scl);
        assert_eq!(factors[0], 16);
        assert_eq!(factors[1], 15);
        // Remaining deltas are 0, so the rest should stay at 15.
        for k in 2..16 {
            assert_eq!(factors[k], 15);
        }
    }

    /// 4x4 duplicate: matrix 1 duplicates matrix 0.
    #[test]
    fn test_duplicate_4x4() {
        let mut scl = ScalingList::default();
        // matrix 0: explicit, all factors = 8
        scl.entry[0][0].scaling_list_pred_mode_flag = 1;
        // matrix 1: pred_mode_flag=0, delta=1 => duplicate from matrix 0
        scl.entry[0][1].scaling_list_pred_mode_flag = 0;
        scl.entry[0][1].scaling_list_pred_matrix_id_delta = 1;

        let mut factors = [0u8; 4 * 4 * 6];
        init_4x4_scaling_lists_h265(&mut factors, &scl);
        // matrix 0 should be all 8
        for k in 0..16 {
            assert_eq!(factors[k], 8);
        }
        // matrix 1 should duplicate matrix 0 => all 8
        for k in 16..32 {
            assert_eq!(factors[k], 8);
        }
    }

    /// 8x8 explicit with DC coef (size_id=2).
    #[test]
    fn test_explicit_8x8_with_dc() {
        let mut scl = ScalingList::default();
        scl.entry[2][0].scaling_list_pred_mode_flag = 1;
        scl.entry[2][0].scaling_list_dc_coef_minus8 = 16; // DC = 24
        // All deltas 0 => nextCoef starts at 24, stays at 24.
        let mut factors = [0u8; 8 * 8 * 6];
        let mut dc = [0u8; 6];
        init_8x8_scaling_lists_h265(&mut factors, &mut dc, &scl, 2);
        assert_eq!(dc[0], 24);
        for k in 0..64 {
            assert_eq!(factors[k], 24);
        }
    }

    /// 8x8 duplicate with DC propagation (size_id=2).
    #[test]
    fn test_duplicate_8x8_dc_propagation() {
        let mut scl = ScalingList::default();
        // matrix 0: explicit with dc=24
        scl.entry[2][0].scaling_list_pred_mode_flag = 1;
        scl.entry[2][0].scaling_list_dc_coef_minus8 = 16;
        // matrix 1: duplicate from matrix 0
        scl.entry[2][1].scaling_list_pred_mode_flag = 0;
        scl.entry[2][1].scaling_list_pred_matrix_id_delta = 1;

        let mut factors = [0u8; 8 * 8 * 6];
        let mut dc = [0u8; 6];
        init_8x8_scaling_lists_h265(&mut factors, &mut dc, &scl, 2);
        assert_eq!(dc[0], 24);
        assert_eq!(dc[1], 24);
        // Factors for matrix 1 should equal matrix 0.
        for k in 0..64 {
            assert_eq!(factors[64 + k], factors[k]);
        }
    }

    /// Wrapping: delta that causes underflow wraps around via `& 0xff`.
    #[test]
    fn test_wrapping_behavior() {
        let mut scl = ScalingList::default();
        scl.entry[0][0].scaling_list_pred_mode_flag = 1;
        // nextCoef starts at 8, delta = -9 => (8 + (-9)) & 0xff = (-1) & 0xff = 255
        scl.entry[0][0].scaling_list_delta_coef[0] = -9;
        let mut factors = [0u8; 4 * 4 * 6];
        init_4x4_scaling_lists_h265(&mut factors, &scl);
        assert_eq!(factors[0], 255);
        // Next: (255 + 0) & 0xff = 255
        assert_eq!(factors[1], 255);
    }

    /// ScalingListEntry default has all fields zeroed.
    #[test]
    fn test_scaling_list_entry_default() {
        let e = ScalingListEntry::default();
        assert_eq!(e.scaling_list_pred_mode_flag, 0);
        assert_eq!(e.scaling_list_pred_matrix_id_delta, 0);
        assert_eq!(e.scaling_list_dc_coef_minus8, 0);
        assert!(e.scaling_list_delta_coef.iter().all(|&v| v == 0));
    }
}
