// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn test_dpb_create_instance() {
    let dpb = VkEncDpbH264::create_instance();
    assert_eq!(dpb.max_dpb_size, 0);
    assert_eq!(dpb.curr_dpb_idx, -1);
}

#[test]
fn test_dpb_sequence_start() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);
    assert_eq!(dpb.max_dpb_size, 4);
}

#[test]
fn test_dpb_entry_default() {
    let entry = DpbEntryH264::default();
    assert_eq!(entry.state, DpbStateH264::Empty as u32);
    assert_eq!(entry.top_field_marking, MARKING_UNUSED);
    assert!(!entry.frame_is_corrupted);
}

#[test]
fn test_sort_check_functions() {
    let entry = DpbEntryH264 {
        top_field_marking: MARKING_SHORT,
        bottom_field_marking: MARKING_SHORT,
        top_pic_num: 5,
        ..Default::default()
    };
    let mut v = 0;
    assert!(sort_check_short_term_p_frame(&entry, 0, &mut v));
    assert_eq!(v, 5);
}

#[test]
fn test_dpb_full_empty() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(2);
    assert!(dpb.is_dpb_empty());
    assert!(!dpb.is_dpb_full());

    dpb.dpb[0].state = DpbStateH264::Frame as u32;
    dpb.dpb[1].state = DpbStateH264::Frame as u32;
    assert!(dpb.is_dpb_full());
    assert!(!dpb.is_dpb_empty());
}

#[test]
fn test_poc_type0_calculation() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);
    dpb.curr_dpb_idx = MAX_DPB_SLOTS as i8;

    let pic = PicInfoH264 {
        is_idr: true,
        pic_order_cnt: 0,
        is_reference: true,
        ..Default::default()
    };

    dpb.calculate_poc_type0(&pic, 0); // log2_max_pic_order_cnt_lsb_minus4 = 0
    assert_eq!(dpb.dpb[MAX_DPB_SLOTS].top_foc, 0);
    assert_eq!(dpb.dpb[MAX_DPB_SLOTS].bottom_foc, 0);
}

#[test]
fn test_pic_num_calculation() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);
    dpb.curr_dpb_idx = 0;

    dpb.dpb[0].pic_info_frame_num = 5;
    dpb.dpb[1].pic_info_frame_num = 3;

    let pic = PicInfoH264 {
        frame_num: 10,
        field_pic_flag: false,
        ..Default::default()
    };

    dpb.calculate_pic_num(&pic, 0); // log2_max_frame_num_minus4 = 0
    assert_eq!(dpb.dpb[0].top_pic_num, 5);
    assert_eq!(dpb.dpb[1].top_pic_num, 3);
}

#[test]
fn test_ref_frames_corrupted() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);
    assert!(!dpb.is_ref_frames_corrupted());

    dpb.dpb[0].top_field_marking = MARKING_SHORT;
    dpb.dpb[0].frame_is_corrupted = true;
    assert!(dpb.is_ref_frames_corrupted());
}

#[test]
fn test_need_to_reorder() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);
    assert!(!dpb.need_to_reorder());

    dpb.dpb[0].top_field_marking = MARKING_SHORT;
    dpb.dpb[0].frame_is_corrupted = true;
    assert!(dpb.need_to_reorder());
}

#[test]
fn test_num_ref_frames_in_dpb() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);

    dpb.dpb[0].top_field_marking = MARKING_SHORT;
    dpb.dpb[0].view_id = 0;
    dpb.dpb[1].top_field_marking = MARKING_LONG;
    dpb.dpb[1].view_id = 0;

    let (total, short, long) = dpb.get_num_ref_frames_in_dpb(0);
    assert_eq!(total, 2);
    assert_eq!(short, 1);
    assert_eq!(long, 1);
}

#[test]
fn test_dpb_picture_end_idr() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);

    let pic = PicInfoH264 {
        frame_num: 0,
        pic_order_cnt: 0,
        is_idr: true,
        is_reference: true,
        ..Default::default()
    };

    dpb.dpb_picture_start(&pic, 0, 0, 4, false, 4);
    let slot = dpb.dpb_picture_end(&pic, 4);
    assert!(slot >= 0);
    assert_eq!(slot, 0); // first slot after IDR clear

    // Verify the slot is marked as short-term reference
    let s = slot as usize;
    assert_eq!(dpb.dpb[s].state, DpbStateH264::Frame as u32);
    assert_eq!(dpb.dpb[s].top_field_marking, MARKING_SHORT);
}

#[test]
fn test_dpb_picture_end_p_frame() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);

    // IDR first
    let idr = PicInfoH264 {
        frame_num: 0,
        pic_order_cnt: 0,
        is_idr: true,
        is_reference: true,
        ..Default::default()
    };
    dpb.dpb_picture_start(&idr, 0, 0, 4, false, 4);
    let idr_slot = dpb.dpb_picture_end(&idr, 4);
    assert_eq!(idr_slot, 0);

    // P frame
    let p = PicInfoH264 {
        frame_num: 1,
        pic_order_cnt: 2,
        is_idr: false,
        is_reference: true,
        ..Default::default()
    };
    dpb.dpb_picture_start(&p, 0, 0, 4, false, 4);
    let p_slot = dpb.dpb_picture_end(&p, 4);
    assert!(p_slot >= 0);
    assert_ne!(p_slot, idr_slot); // different slot from IDR
}

#[test]
fn test_build_ref_pic_list_p() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);

    // IDR
    let idr = PicInfoH264 {
        frame_num: 0,
        pic_order_cnt: 0,
        is_idr: true,
        is_reference: true,
        ..Default::default()
    };
    dpb.dpb_picture_start(&idr, 0, 0, 4, false, 4);
    dpb.dpb_picture_end(&idr, 4);

    // P frame - build reference list (should reference the IDR)
    let p = PicInfoH264 {
        frame_num: 1,
        pic_order_cnt: 2,
        is_idr: false,
        is_reference: true,
        ..Default::default()
    };
    dpb.dpb_picture_start(&p, 0, 0, 4, false, 4);

    let lists = dpb.build_ref_pic_list_p();
    assert_eq!(lists.ref_pic_list_count[0], 1); // one reference (the IDR)
    assert_eq!(lists.ref_pic_list[0][0], 0); // IDR is in slot 0
}

#[test]
fn test_get_active_ref_slots() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);

    // IDR
    let idr = PicInfoH264 {
        frame_num: 0,
        pic_order_cnt: 0,
        is_idr: true,
        is_reference: true,
        ..Default::default()
    };
    dpb.dpb_picture_start(&idr, 0, 0, 4, false, 4);
    dpb.dpb_picture_end(&idr, 4);

    let active = dpb.get_active_ref_slots();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0], 0);
}

#[test]
fn test_sliding_window_eviction() {
    let mut dpb = VkEncDpbH264::create_instance();
    dpb.dpb_sequence_start(4);

    // IDR
    let idr = PicInfoH264 {
        frame_num: 0,
        pic_order_cnt: 0,
        is_idr: true,
        is_reference: true,
        ..Default::default()
    };
    dpb.dpb_picture_start(&idr, 0, 0, 4, false, 1); // max_num_ref = 1
    dpb.dpb_picture_end(&idr, 1);
    assert_eq!(dpb.get_active_ref_slots().len(), 1);

    // P frame with max_ref=1 should evict IDR via sliding window
    let p = PicInfoH264 {
        frame_num: 1,
        pic_order_cnt: 2,
        is_idr: false,
        is_reference: true,
        ..Default::default()
    };
    dpb.dpb_picture_start(&p, 0, 0, 4, false, 1);

    // Reference list should still see the IDR (not yet evicted)
    let lists = dpb.build_ref_pic_list_p();
    assert_eq!(lists.ref_pic_list_count[0], 1);

    // After dpb_picture_end, sliding window evicts IDR, stores P
    dpb.dpb_picture_end(&p, 1);
    assert_eq!(dpb.get_active_ref_slots().len(), 1);
}
