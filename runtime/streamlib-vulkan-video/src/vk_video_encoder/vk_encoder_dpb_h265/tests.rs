// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn test_dpb_h265_new() {
    let dpb = VkEncDpbH265::new();
    assert_eq!(dpb.dpb_size, 0);
    assert_eq!(dpb.cur_dpb_index, 0);
}

#[test]
fn test_dpb_h265_sequence_start() {
    let mut dpb = VkEncDpbH265::new();
    assert!(dpb.dpb_sequence_start(4, true));
    assert_eq!(dpb.dpb_size, 4);
    assert!(dpb.use_multiple_refs);
}

#[test]
fn test_dpb_h265_picture_start_idr() {
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);
    let idx = dpb.dpb_picture_start(0, 0, true, true, true, 0, false, 0);
    assert!(idx >= 0);
    assert_eq!(dpb.dpb[idx as usize].pic_order_cnt_val, 0);
}

#[test]
fn test_dpb_h265_full_empty() {
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(2, false);
    assert!(dpb.is_dpb_empty());
    assert!(!dpb.is_dpb_full());

    dpb.dpb[0].state = 1;
    dpb.dpb[1].state = 1;
    assert!(dpb.is_dpb_full());
}

#[test]
fn test_dpb_h265_bumping() {
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(2, false);

    dpb.dpb[0].state = 1;
    dpb.dpb[0].output = true;
    dpb.dpb[0].pic_order_cnt_val = 10;

    dpb.dpb[1].state = 1;
    dpb.dpb[1].output = true;
    dpb.dpb[1].pic_order_cnt_val = 5;

    dpb.dpb_bumping();
    // Entry with POC=5 should be marked as not needed for output
    assert!(!dpb.dpb[1].output);
}

#[test]
fn test_ref_pic_set_default() {
    let rps = RefPicSetH265::default();
    for i in 0..MAX_NUM_LIST_REF_H265 {
        assert_eq!(rps.st_curr_before[i], -1);
        assert_eq!(rps.st_curr_after[i], -1);
    }
}

// -----------------------------------------------------------------------
// Tests for the newly ported DPB pipeline functions
// -----------------------------------------------------------------------

#[test]
fn test_initialize_rps_single_ref() {
    // Simulate: IDR at POC 0, then P at POC 1 referencing IDR.
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);

    // IDR frame: POC=0
    dpb.reference_picture_marking(0, PIC_TYPE_IDR, false);
    dpb.dpb_picture_start(0, 0, true, true, true, 0, false, 0);
    dpb.dpb_picture_end(1, true);

    // P frame: POC=1 — initialize_rps should find 1 negative ref
    let result = dpb.initialize_rps(
        &[], // no SPS STRPS
        PIC_TYPE_P,
        1,     // pic_order_cnt_val
        0,     // temporal_id
        false, // not IRAP
        1,     // num_ref_l0
        0,     // num_ref_l1
    );

    assert_eq!(result.short_term_ref_pic_set.num_negative_pics, 1);
    assert_eq!(result.short_term_ref_pic_set.num_positive_pics, 0);
    assert_eq!(result.short_term_ref_pic_set.delta_poc_s0_minus1[0], 0); // delta=-1, minus1=0
    assert_eq!(result.short_term_ref_pic_set.used_by_curr_pic_s0_flag, 1);
    assert!(!result.short_term_ref_pic_set_sps_flag);
}

#[test]
fn test_initialize_rps_multiple_refs() {
    // IDR at POC 0, P at POC 1, P at POC 2 referencing both
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);

    // IDR frame: POC=0
    dpb.reference_picture_marking(0, PIC_TYPE_IDR, false);
    dpb.dpb_picture_start(0, 0, true, true, true, 0, false, 0);
    dpb.dpb_picture_end(1, true);

    // P frame: POC=1
    dpb.reference_picture_marking(1, PIC_TYPE_P, false);
    dpb.dpb_picture_start(1, 1, false, false, true, 0, false, 1);
    dpb.dpb_picture_end(1, true);

    // P frame: POC=2 — should find 2 negative refs (POC=1, POC=0)
    // But with use_multiple_refs=false, only 1 is used by curr pic.
    // The STRPS still lists both refs (num_negative_pics=2), but only
    // the first is marked as "used by current picture".
    let result = dpb.initialize_rps(
        &[],
        PIC_TYPE_P,
        2,     // pic_order_cnt_val
        0,     // temporal_id
        false, // not IRAP
        1,     // num_ref_l0
        0,     // num_ref_l1
    );

    // With dpb_size=4 and 2 refs, both fit (2 <= 3), so num_negative_pics=2
    // but only the first is marked as used_by_curr_pic_s0
    assert!(result.short_term_ref_pic_set.num_negative_pics >= 1);
    assert_eq!(
        result.short_term_ref_pic_set.used_by_curr_pic_s0_flag & 1,
        1
    );
}

#[test]
fn test_dpb_picture_start_with_rps_idr() {
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);

    let empty_strps = ShortTermRefPicSet::default();
    let (slot, ref_pic_set) = dpb.dpb_picture_start_with_rps(
        0, // frame_id
        0, // pic_order_cnt_val
        PIC_TYPE_IDR,
        true,  // is_irap
        true,  // is_idr
        true,  // pic_output_flag
        0,     // temporal_id
        false, // no_output_of_prior_pics_flag
        0,     // time_stamp
        &empty_strps,
        256, // max_pic_order_cnt_lsb
    );

    assert!(slot >= 0);
    // All RefPicSet entries should be -1 (no references for IDR)
    for i in 0..MAX_NUM_LIST_REF_H265 {
        assert_eq!(ref_pic_set.st_curr_before[i], -1);
    }
}

#[test]
fn test_full_ipp_pipeline() {
    // Full pipeline test: IDR -> P -> P, matching C++ call order
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);

    // --- IDR frame (POC=0) ---
    dpb.reference_picture_marking(0, PIC_TYPE_IDR, false);
    let _rps_result = dpb.initialize_rps(&[], PIC_TYPE_IDR, 0, 0, true, 0, 0);
    let empty_strps = ShortTermRefPicSet::default();
    let (slot0, _rps0) = dpb.dpb_picture_start_with_rps(
        0,
        0,
        PIC_TYPE_IDR,
        true,
        true,
        true,
        0,
        false,
        0,
        &empty_strps,
        256,
    );
    assert!(slot0 >= 0);
    dpb.dpb_picture_end(1, true);

    // --- P frame (POC=1) ---
    dpb.reference_picture_marking(1, PIC_TYPE_P, false);
    let rps_result1 = dpb.initialize_rps(&[], PIC_TYPE_P, 1, 0, false, 1, 0);
    assert_eq!(rps_result1.short_term_ref_pic_set.num_negative_pics, 1);

    let (slot1, rps1) = dpb.dpb_picture_start_with_rps(
        1,
        1,
        PIC_TYPE_P,
        false,
        false,
        true,
        0,
        false,
        1,
        &rps_result1.short_term_ref_pic_set,
        256,
    );
    assert!(slot1 >= 0);

    // stCurrBefore should contain the DPB index of POC=0
    assert!(rps1.st_curr_before[0] >= 0);
    assert_eq!(rps1.st_curr_before[0], slot0);

    let ref_lists = dpb.setup_reference_picture_list_lx(PIC_TYPE_P, &rps1, 1, 0);
    assert_eq!(ref_lists.num_ref_idx_l0_active_minus1, 0);
    assert_eq!(ref_lists.ref_pic_list0[0], slot0 as u8);

    dpb.dpb_picture_end(1, true);

    // --- P frame (POC=2) ---
    dpb.reference_picture_marking(2, PIC_TYPE_P, false);
    let rps_result2 = dpb.initialize_rps(&[], PIC_TYPE_P, 2, 0, false, 1, 0);
    // Both POC=0 and POC=1 are valid negative refs; with dpb_size=4,
    // both fit. Only 1 is used_by_curr_pic though.
    assert!(rps_result2.short_term_ref_pic_set.num_negative_pics >= 1);

    let (slot2, rps2) = dpb.dpb_picture_start_with_rps(
        2,
        2,
        PIC_TYPE_P,
        false,
        false,
        true,
        0,
        false,
        2,
        &rps_result2.short_term_ref_pic_set,
        256,
    );
    assert!(slot2 >= 0);

    // stCurrBefore should reference POC=1 (most recent)
    assert!(rps2.st_curr_before[0] >= 0);
    assert_eq!(rps2.st_curr_before[0], slot1);

    let ref_lists2 = dpb.setup_reference_picture_list_lx(PIC_TYPE_P, &rps2, 1, 0);
    assert_eq!(ref_lists2.ref_pic_list0[0], slot1 as u8);

    dpb.dpb_picture_end(1, true);
}

#[test]
fn test_setup_ref_list_no_reference_picture_sentinel() {
    // Verify unused RefPicList entries are filled with NO_REFERENCE_PICTURE
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);

    dpb.reference_picture_marking(0, PIC_TYPE_IDR, false);
    dpb.dpb_picture_start(0, 0, true, true, true, 0, false, 0);
    dpb.dpb_picture_end(1, true);

    dpb.reference_picture_marking(1, PIC_TYPE_P, false);
    let rps_result = dpb.initialize_rps(&[], PIC_TYPE_P, 1, 0, false, 1, 0);
    let (_, rps) = dpb.dpb_picture_start_with_rps(
        1,
        1,
        PIC_TYPE_P,
        false,
        false,
        true,
        0,
        false,
        1,
        &rps_result.short_term_ref_pic_set,
        256,
    );
    let ref_lists = dpb.setup_reference_picture_list_lx(PIC_TYPE_P, &rps, 1, 0);

    // First entry should be a valid DPB index
    assert_ne!(ref_lists.ref_pic_list0[0], NO_REFERENCE_PICTURE_H265);
    // Remaining entries should be NO_REFERENCE_PICTURE
    for i in 1..MAX_NUM_LIST_REF_H265 {
        assert_eq!(ref_lists.ref_pic_list0[i], NO_REFERENCE_PICTURE_H265);
    }
}

#[test]
fn test_reference_picture_marking_cra() {
    // Test CRA picture marking: after CRA, old refs should be marked unused
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);

    // IDR at POC=0
    dpb.reference_picture_marking(0, PIC_TYPE_IDR, false);
    dpb.dpb_picture_start(0, 0, true, true, true, 0, false, 0);
    dpb.dpb_picture_end(1, true);

    // P at POC=1
    dpb.reference_picture_marking(1, PIC_TYPE_P, false);
    dpb.dpb_picture_start(1, 1, false, false, true, 0, false, 1);
    dpb.dpb_picture_end(1, true);

    // I (CRA) at POC=2 — sets refresh_pending
    dpb.reference_picture_marking(2, PIC_TYPE_I, false);
    assert!(dpb.refresh_pending);
    assert_eq!(dpb.pic_order_cnt_cra, 2);

    // P at POC=3 — should clear refs with POC != CRA POC (2)
    dpb.reference_picture_marking(3, PIC_TYPE_P, false);
    assert!(!dpb.refresh_pending);
}

#[test]
fn test_apply_rps_marks_unused() {
    // ApplyReferencePictureSet should mark DPB entries NOT in RPS as unused
    let mut dpb = VkEncDpbH265::new();
    dpb.dpb_sequence_start(4, false);

    // Add two reference entries manually
    dpb.dpb[0].state = 1;
    dpb.dpb[0].marking = 1;
    dpb.dpb[0].pic_order_cnt_val = 0;

    dpb.dpb[1].state = 1;
    dpb.dpb[1].marking = 1;
    dpb.dpb[1].pic_order_cnt_val = 1;

    // Build STRPS that only references POC=1 (delta=-1 from POC=2)
    let strps = ShortTermRefPicSet {
        num_negative_pics: 1,
        num_positive_pics: 0,
        delta_poc_s0_minus1: {
            let mut d = [0u16; MAX_DPB_SIZE_H265];
            d[0] = 0; // delta = -1
            d
        },
        used_by_curr_pic_s0_flag: 1,
        ..ShortTermRefPicSet::default()
    };

    let ref_pic_set = dpb.apply_reference_picture_set(2, PIC_TYPE_P, false, &strps, 256);

    // POC=1 should be in stCurrBefore, POC=0 should be marked unused
    assert!(ref_pic_set.st_curr_before[0] >= 0);
    // Entry at POC=0 should have marking=0 (not in RPS)
    assert_eq!(dpb.dpb[0].marking, 0);
    // Entry at POC=1 should still have marking=1
    assert_eq!(dpb.dpb[1].marking, 1);
}
