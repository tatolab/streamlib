// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

    let result = set_pps_scaling_lists_h264(None, true, &sps_w4, &sps_w8, &mut w4, &mut w8);
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

    let result = set_pps_scaling_lists_h264(None, false, &sps_w4, &sps_w8, &mut w4, &mut w8);
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
    let result = set_pps_scaling_lists_h264(Some(&pps), true, &sps_w4, &sps_w8, &mut w4, &mut w8);
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
    set_pps_scaling_lists_h264(Some(&pps), false, &sps_w4, &sps_w8, &mut w4, &mut w8);

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
    let result = set_seq_pic_scaling_lists_h264(Some(&sps), Some(&pps), &mut w4, &mut w8);
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
