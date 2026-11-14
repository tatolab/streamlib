// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// --- DpbSlot tests ---

#[test]
fn test_dpb_slot_lifecycle() {
    let mut slot = DpbSlot::default();
    assert!(slot.is_available());
    assert!(!slot.is_in_use());

    slot.reserve();
    assert!(slot.is_in_use());
    assert!(!slot.is_available());

    slot.mark_in_use(42);
    assert!(slot.is_in_use());
    assert_eq!(slot.get_age(), 42);

    let was_in_use = slot.invalidate();
    assert!(was_in_use);
    assert!(slot.is_available());
}

#[test]
fn test_dpb_slot_set_picture_resource() {
    let mut slot = DpbSlot::default();
    assert_eq!(slot.get_picture_resource(), None);

    let old = slot.set_picture_resource(Some(7), 10);
    assert_eq!(old, None);
    assert_eq!(slot.get_picture_resource(), Some(7));
    assert_eq!(slot.get_age(), 10);

    let old2 = slot.set_picture_resource(Some(3), 20);
    assert_eq!(old2, Some(7));
}

// --- DpbSlots tests ---

#[test]
fn test_dpb_slots_allocate_and_free() {
    let mut dpb = DpbSlots::new(4);
    assert_eq!(dpb.get_max_size(), 4);
    assert_eq!(dpb.get_slot_in_use_mask(), 0);

    let s0 = dpb.allocate_slot();
    assert!(s0 >= 0);
    assert!(dpb.get_slot_in_use_mask() & (1 << s0) != 0);

    let s1 = dpb.allocate_slot();
    assert!(s1 >= 0);
    assert_ne!(s0, s1);

    dpb.free_slot(s0);
    assert!(dpb.get_slot_in_use_mask() & (1 << s0) == 0);

    // Should be able to reallocate the freed slot.
    let s2 = dpb.allocate_slot();
    assert!(s2 >= 0);
}

#[test]
fn test_dpb_slots_init_reconfigure() {
    let mut dpb = DpbSlots::new(4);
    assert_eq!(dpb.get_max_size(), 4);

    // Reconfigure to larger size.
    let new_size = dpb.init(8, true);
    assert_eq!(new_size, 8);

    // Reconfigure to smaller should keep old size.
    let kept_size = dpb.init(3, true);
    assert_eq!(kept_size, 8);
}

#[test]
fn test_dpb_slots_get_slot_of_picture_resource() {
    let mut dpb = DpbSlots::new(4);
    let slot = dpb.allocate_slot();
    dpb.get_mut(slot as u32).mark_in_use(1);
    dpb.get_mut(slot as u32).set_picture_resource(Some(42), 1);

    assert_eq!(dpb.get_slot_of_picture_resource(42), slot);
    assert_eq!(dpb.get_slot_of_picture_resource(99), -1);
}

// --- DpbH264Entry tests ---

#[test]
fn test_dpb_h264_entry_set_reference() {
    let mut entry = DpbH264Entry::default();
    entry.set_reference(false, 100, Some(5));
    assert!(entry.is_ref());
    assert!(!entry.is_long_term);
    assert!(!entry.is_non_existing);
    assert_eq!(entry.pic_buf_idx, Some(5));
    assert_eq!(entry.pic_order_cnt(), 100);
}

#[test]
fn test_dpb_h264_entry_set_reference_non_existing() {
    let mut entry = DpbH264Entry::default();
    entry.set_reference(true, 200, None);
    assert!(!entry.is_ref());
    assert!(entry.is_non_existing);
    assert!(entry.is_long_term);
}

#[test]
fn test_dpb_h264_entry_set_reference_and_top_bottom_field() {
    let mut entry = DpbH264Entry::default();
    let foc = [10i16, 20i16];
    entry.set_reference_and_top_bottom_field(
        true,
        false,
        false,
        true,
        true,
        false,
        5,
        &foc,
        Some(3),
    );
    assert!(entry.is_ref());
    assert!(entry.is_field_ref);
    assert!(!entry.is_non_existing);
    assert_eq!(entry.frame_idx, 5);
    assert_eq!(entry.pic_buf_idx, Some(3));
    // used_for_reference should have top set, bottom clear.
    assert_eq!(entry.used_for_reference & TOP_FIELD_MASK, TOP_FIELD_MASK);
    assert_eq!(entry.used_for_reference & BOTTOM_FIELD_MASK, 0);
}

// --- VulkanVideoParser tests ---

#[test]
fn test_parser_new_default_state() {
    let parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    assert_eq!(parser.current_picture_id, 0);
    assert_eq!(parser.dpb_slots_mask, 0);
    assert_eq!(parser.field_pic_flag_mask, 0);
    assert_eq!(parser.max_num_decode_surfaces, 16);
    for &slot in &parser.picture_to_dpb_slot_map {
        assert_eq!(slot, -1);
    }
}

#[test]
fn test_parser_pic_idx() {
    let parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    assert_eq!(parser.get_pic_idx(Some(0)), 0);
    assert_eq!(parser.get_pic_idx(Some(15)), 15);
    assert_eq!(parser.get_pic_idx(Some(16)), -1); // out of range
    assert_eq!(parser.get_pic_idx(None), -1);
    assert_eq!(parser.get_pic_idx(Some(-1)), -1);
}

#[test]
fn test_parser_set_pic_dpb_slot() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    // Initially no slot.
    assert_eq!(parser.get_pic_dpb_slot(0), -1);

    // Allocate a DPB slot for pic 0.
    let dpb_slot = parser.dpb.allocate_slot();
    let old = parser.set_pic_dpb_slot(0, dpb_slot);
    assert_eq!(old, -1);
    assert_eq!(parser.get_pic_dpb_slot(0), dpb_slot);
    assert!(parser.dpb_slots_mask & 1 != 0);

    // Clear it.
    let old2 = parser.set_pic_dpb_slot(0, -1);
    assert_eq!(old2, dpb_slot);
    assert_eq!(parser.get_pic_dpb_slot(0), -1);
    assert!(parser.dpb_slots_mask & 1 == 0);
}

#[test]
fn test_parser_field_pic_flag() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    assert!(!parser.get_field_pic_flag(0));
    let old = parser.set_field_pic_flag(0, true);
    assert!(!old);
    assert!(parser.get_field_pic_flag(0));
    let old2 = parser.set_field_pic_flag(0, false);
    assert!(old2);
    assert!(!parser.get_field_pic_flag(0));
}

#[test]
fn test_parser_reset_pic_dpb_slots() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H265, 16, 16, 90000);
    // Grow DPB to 16 slots.
    parser.dpb.init(16, false);

    // Assign slots to pic 0 and pic 1.
    let slot_a = parser.dpb.allocate_slot();
    parser.set_pic_dpb_slot(0, slot_a);
    let slot_b = parser.dpb.allocate_slot();
    parser.set_pic_dpb_slot(1, slot_b);

    assert!(parser.dpb_slots_mask & 0b11 == 0b11);

    // Reset all except pic 0.
    let mask = parser.reset_pic_dpb_slots(1 << 0);
    assert!(mask & (1 << 0) != 0); // pic 0 still mapped
    assert!(mask & (1 << 1) == 0); // pic 1 reset
}

#[test]
fn test_parser_allocate_dpb_slot_h264() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    parser.dpb.init(17, false);

    let slot = parser.allocate_dpb_slot_for_current_h264(Some(0), false, -1);
    assert!(slot >= 0);
    assert_eq!(parser.get_pic_dpb_slot(0), slot);

    // Allocating again for the same pic should return the same slot.
    let slot2 = parser.allocate_dpb_slot_for_current_h264(Some(0), false, -1);
    assert_eq!(slot, slot2);
}

#[test]
fn test_parser_allocate_dpb_slot_h265() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H265, 16, 16, 90000);
    parser.dpb.init(16, false);

    let slot = parser.allocate_dpb_slot_for_current_h265(Some(5), true, -1);
    assert!(slot >= 0);
    assert_eq!(parser.get_pic_dpb_slot(5), slot);
}

#[test]
fn test_parser_allocate_dpb_slot_av1() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_AV1, 16, 9, 90000);
    parser.dpb.init(9, false);

    let slot = parser.allocate_dpb_slot_for_current_av1(Some(2), true, -1);
    assert!(slot >= 0);
    assert_eq!(parser.get_pic_dpb_slot(2), slot);
}

#[test]
fn test_parser_begin_sequence_h264() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    let result = parser.begin_sequence(
        vk::VideoCodecOperationFlagsKHR::DECODE_H264,
        1920,
        1080,
        1920,
        1080,
        16,
        0,
        0,
        0,
        0,
        0,
    );
    assert!(result > 0);
    assert_eq!(parser.max_num_dpb_slots, MAX_DPB_REF_AND_SETUP_SLOTS);
}

#[test]
fn test_parser_begin_sequence_av1() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_AV1, 16, 9, 90000);
    let result = parser.begin_sequence(
        vk::VideoCodecOperationFlagsKHR::DECODE_AV1,
        1920,
        1080,
        1920,
        1080,
        16,
        0,
        0,
        0,
        0,
        0,
    );
    assert!(result > 0);
    assert_eq!(parser.max_num_dpb_slots, 9);
}

#[test]
fn test_parser_advance_picture_id() {
    let mut parser =
        VulkanVideoParser::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    assert_eq!(parser.current_picture_id, 0);
    parser.advance_picture_id();
    assert_eq!(parser.current_picture_id, 1);
}

#[test]
fn test_create_vulkan_video_parser_factory() {
    let result =
        create_vulkan_video_parser(vk::VideoCodecOperationFlagsKHR::DECODE_H264, 16, 17, 90000);
    assert!(result.is_ok());
}

#[test]
fn test_picture_parameters_type_name() {
    assert_eq!(PictureParametersType::H264Sps.name(), "H264_SPS");
    assert_eq!(PictureParametersType::H265Vps.name(), "H265_VPS");
    assert_eq!(PictureParametersType::Av1Sps.name(), "AV1_SPS");
}
