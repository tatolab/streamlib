// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// -----------------------------------------------------------------------
// Helper / utility tests
// -----------------------------------------------------------------------

#[test]
fn test_ceil_log2() {
    // CeilLog2(n) = Log2U31(n-1) where Log2U31 counts bit-width.
    // This matches ceil(log2(n)) for n >= 1.
    assert_eq!(ceil_log2(0), 0);
    assert_eq!(ceil_log2(-1), 0);
    assert_eq!(ceil_log2(1), 0); // ceil(log2(1)) = 0
    assert_eq!(ceil_log2(2), 1); // ceil(log2(2)) = 1
    assert_eq!(ceil_log2(3), 2); // ceil(log2(3)) = 2
    assert_eq!(ceil_log2(4), 2); // ceil(log2(4)) = 2
    assert_eq!(ceil_log2(5), 3); // ceil(log2(5)) = 3
    assert_eq!(ceil_log2(8), 3); // ceil(log2(8)) = 3
    assert_eq!(ceil_log2(9), 4); // ceil(log2(9)) = 4
    assert_eq!(ceil_log2(16), 4); // ceil(log2(16)) = 4
    assert_eq!(ceil_log2(17), 5); // ceil(log2(17)) = 5
}

// -----------------------------------------------------------------------
// NAL unit type tests
// -----------------------------------------------------------------------

#[test]
fn test_nal_unit_type_from_raw() {
    assert_eq!(NalUnitType::from_raw(0), Some(NalUnitType::TrailN));
    assert_eq!(NalUnitType::from_raw(19), Some(NalUnitType::IdrWRadl));
    assert_eq!(NalUnitType::from_raw(21), Some(NalUnitType::CraNut));
    assert_eq!(NalUnitType::from_raw(33), Some(NalUnitType::SpsNut));
    assert_eq!(NalUnitType::from_raw(10), None); // reserved
    assert_eq!(NalUnitType::from_raw(42), None);
}

#[test]
fn test_nal_unit_type_is_irap() {
    assert!(NalUnitType::BlaWLp.is_irap());
    assert!(NalUnitType::IdrWRadl.is_irap());
    assert!(NalUnitType::CraNut.is_irap());
    assert!(!NalUnitType::TrailN.is_irap());
    assert!(!NalUnitType::SpsNut.is_irap());
}

#[test]
fn test_nal_unit_type_is_idr() {
    assert!(NalUnitType::IdrWRadl.is_idr());
    assert!(NalUnitType::IdrNLp.is_idr());
    assert!(!NalUnitType::CraNut.is_idr());
    assert!(!NalUnitType::BlaWLp.is_idr());
}

#[test]
fn test_nal_unit_type_is_slice() {
    assert!(NalUnitType::TrailN.is_slice());
    assert!(NalUnitType::RaslR.is_slice());
    assert!(NalUnitType::BlaWLp.is_slice());
    assert!(NalUnitType::CraNut.is_slice());
    assert!(!NalUnitType::VpsNut.is_slice());
    assert!(!NalUnitType::SpsNut.is_slice());
}

// -----------------------------------------------------------------------
// POC calculation tests
// -----------------------------------------------------------------------

#[test]
fn test_poc_idr_picture() {
    let mut decoder = VulkanH265Decoder::new();
    // Set up a minimal active SPS
    let mut sps = HevcSeqParam::default();
    sps.log2_max_pic_order_cnt_lsb_minus4 = 4; // MaxPicOrderCntLsb = 256
    decoder.active_sps[0] = Some(Box::new(sps));

    // IDR picture: POC should be just pic_order_cnt_lsb
    let slh = HevcSliceHeader {
        nal_unit_type: NalUnitType::IdrWRadl as u8,
        nuh_temporal_id_plus1: 1,
        pic_order_cnt_lsb: 0,
        ..Default::default()
    };
    decoder.no_rasl_output_flag = true;
    let poc = decoder.picture_order_count(&slh);
    assert_eq!(poc, 0);
}

#[test]
fn test_poc_increment_sequence() {
    let mut decoder = VulkanH265Decoder::new();
    let mut sps = HevcSeqParam::default();
    sps.log2_max_pic_order_cnt_lsb_minus4 = 4; // MaxPicOrderCntLsb = 256
    decoder.active_sps[0] = Some(Box::new(sps));

    // First IDR
    decoder.no_rasl_output_flag = true;
    let slh_idr = HevcSliceHeader {
        nal_unit_type: NalUnitType::IdrWRadl as u8,
        nuh_temporal_id_plus1: 1,
        pic_order_cnt_lsb: 0,
        ..Default::default()
    };
    let poc = decoder.picture_order_count(&slh_idr);
    assert_eq!(poc, 0);

    // Subsequent non-IDR pictures
    decoder.no_rasl_output_flag = false;
    for expected_poc in 1..10i32 {
        let slh = HevcSliceHeader {
            nal_unit_type: NalUnitType::TrailR as u8,
            nuh_temporal_id_plus1: 1,
            pic_order_cnt_lsb: expected_poc as u16,
            ..Default::default()
        };
        let poc = decoder.picture_order_count(&slh);
        assert_eq!(poc, expected_poc);
    }
}

#[test]
fn test_poc_wraparound() {
    let mut decoder = VulkanH265Decoder::new();
    let mut sps = HevcSeqParam::default();
    sps.log2_max_pic_order_cnt_lsb_minus4 = 0; // MaxPicOrderCntLsb = 16
    decoder.active_sps[0] = Some(Box::new(sps));

    // IDR at POC 0
    decoder.no_rasl_output_flag = true;
    let slh_idr = HevcSliceHeader {
        nal_unit_type: NalUnitType::IdrWRadl as u8,
        nuh_temporal_id_plus1: 1,
        pic_order_cnt_lsb: 0,
        ..Default::default()
    };
    let poc = decoder.picture_order_count(&slh_idr);
    assert_eq!(poc, 0);

    // Advance to LSB = 15
    decoder.no_rasl_output_flag = false;
    for lsb in 1..=15u16 {
        let slh = HevcSliceHeader {
            nal_unit_type: NalUnitType::TrailR as u8,
            nuh_temporal_id_plus1: 1,
            pic_order_cnt_lsb: lsb,
            ..Default::default()
        };
        let poc = decoder.picture_order_count(&slh);
        assert_eq!(poc, lsb as i32);
    }

    // Wraparound: LSB goes back to 0, POC should be 16
    let slh_wrap = HevcSliceHeader {
        nal_unit_type: NalUnitType::TrailR as u8,
        nuh_temporal_id_plus1: 1,
        pic_order_cnt_lsb: 0,
        ..Default::default()
    };
    let poc = decoder.picture_order_count(&slh_wrap);
    assert_eq!(poc, 16);
}

// -----------------------------------------------------------------------
// DPB management tests
// -----------------------------------------------------------------------

#[test]
fn test_dpb_initially_empty() {
    let decoder = VulkanH265Decoder::new();
    assert!(decoder.dpb_empty());
    assert_eq!(decoder.dpb_fullness(), 0);
}

#[test]
fn test_dpb_fullness_tracking() {
    let mut decoder = VulkanH265Decoder::new();
    decoder.dpb[0].state = DPB_STATE_IN_USE;
    decoder.dpb[0].marking = DPB_MARKING_SHORT_TERM;
    decoder.dpb[3].state = DPB_STATE_IN_USE;
    decoder.dpb[3].marking = DPB_MARKING_LONG_TERM;
    assert_eq!(decoder.dpb_fullness(), 2);
    assert!(!decoder.dpb_empty());
}

#[test]
fn test_dpb_bumping_outputs_smallest_poc() {
    let mut decoder = VulkanH265Decoder::new();
    decoder.max_dpb_size = 4;

    // Add entries with different POCs
    for (i, poc) in [10, 5, 15, 3].iter().enumerate() {
        decoder.dpb[i].state = DPB_STATE_IN_USE;
        decoder.dpb[i].marking = DPB_MARKING_SHORT_TERM;
        decoder.dpb[i].output = 1;
        decoder.dpb[i].pic_order_cnt_val = *poc;
        decoder.dpb[i].layer_id = 0;
    }

    // Bump should output POC=3 (index 3)
    assert!(decoder.dpb_bumping(3));
    assert_eq!(decoder.dpb[3].output, 0); // output flag cleared
}

#[test]
fn test_dpb_flush() {
    let mut decoder = VulkanH265Decoder::new();

    for i in 0..4 {
        decoder.dpb[i].state = DPB_STATE_IN_USE;
        decoder.dpb[i].marking = DPB_MARKING_SHORT_TERM;
        decoder.dpb[i].output = 1;
        decoder.dpb[i].pic_order_cnt_val = i as i32;
        decoder.dpb[i].layer_id = 0;
    }
    assert_eq!(decoder.dpb_fullness(), 4);

    decoder.flush_decoded_picture_buffer(false);
    assert!(decoder.dpb_empty());
}

#[test]
fn test_dpb_flush_no_output() {
    let mut decoder = VulkanH265Decoder::new();

    for i in 0..4 {
        decoder.dpb[i].state = DPB_STATE_IN_USE;
        decoder.dpb[i].marking = DPB_MARKING_SHORT_TERM;
        decoder.dpb[i].output = 1;
        decoder.dpb[i].pic_order_cnt_val = i as i32;
        decoder.dpb[i].layer_id = 0;
    }

    decoder.flush_decoded_picture_buffer(true);
    assert!(decoder.dpb_empty());
}

#[test]
fn test_dpb_reordering_delay() {
    let mut decoder = VulkanH265Decoder::new();
    decoder.nuh_layer_id = 0;

    decoder.dpb[0].state = DPB_STATE_IN_USE;
    decoder.dpb[0].output = 1;
    decoder.dpb[0].layer_id = 0;

    decoder.dpb[1].state = DPB_STATE_IN_USE;
    decoder.dpb[1].output = 0;
    decoder.dpb[1].layer_id = 0;

    decoder.dpb[2].state = DPB_STATE_IN_USE;
    decoder.dpb[2].output = 1;
    decoder.dpb[2].layer_id = 0;

    // Layer 1 entry should not be counted
    decoder.dpb[3].state = DPB_STATE_IN_USE;
    decoder.dpb[3].output = 1;
    decoder.dpb[3].layer_id = 1;

    assert_eq!(decoder.dpb_reordering_delay(), 2);
}

#[test]
fn test_get_max_dpb_size() {
    let mut sps = HevcSeqParam::default();
    sps.profile_tier_level.general_level_idc = H265LevelIdc::Level4_0;
    sps.pic_width_in_luma_samples = 1920;
    sps.pic_height_in_luma_samples = 1080;
    let max_dpb = VulkanH265Decoder::get_max_dpb_size(&sps);
    // 1920*1080 = 2073600, MaxLumaPS for 4.0 = 2228224
    // PicSize <= MaxLumaPS, so MaxDpbSize = MaxDpbPicBuf = 6
    assert_eq!(max_dpb, 6);
}

#[test]
fn test_get_max_dpb_size_small_picture() {
    let mut sps = HevcSeqParam::default();
    sps.profile_tier_level.general_level_idc = H265LevelIdc::Level4_0;
    sps.pic_width_in_luma_samples = 320;
    sps.pic_height_in_luma_samples = 240;
    let max_dpb = VulkanH265Decoder::get_max_dpb_size(&sps);
    // 320*240 = 76800. MaxLumaPS/4 = 557056. PicSize <= MaxLumaPS/4 => MaxDpbSize = 24
    // But capped at HEVC_DPB_SIZE = 16
    assert_eq!(max_dpb, 16);
}

// -----------------------------------------------------------------------
// RPS management tests
// -----------------------------------------------------------------------

#[test]
fn test_reference_picture_set_idr() {
    let mut decoder = VulkanH265Decoder::new();
    let sps = HevcSeqParam::default();
    decoder.active_sps[0] = Some(Box::new(sps));

    let slh = HevcSliceHeader {
        nal_unit_type: NalUnitType::IdrWRadl as u8,
        ..Default::default()
    };

    decoder.no_rasl_output_flag = true;
    decoder.reference_picture_set(&slh, 0);

    // After IDR, all ref pic sets should be empty (-1)
    assert_eq!(decoder.num_poc_st_curr_before, 0);
    assert_eq!(decoder.num_poc_st_curr_after, 0);
    assert_eq!(decoder.num_poc_lt_curr, 0);
}

#[test]
fn test_create_lost_ref_pic_found() {
    let mut decoder = VulkanH265Decoder::new();
    decoder.dpb[0].state = DPB_STATE_IN_USE;
    decoder.dpb[0].marking = DPB_MARKING_SHORT_TERM;
    decoder.dpb[0].pic_order_cnt_val = 5;
    decoder.dpb[0].layer_id = 0;

    decoder.dpb[3].state = DPB_STATE_IN_USE;
    decoder.dpb[3].marking = DPB_MARKING_SHORT_TERM;
    decoder.dpb[3].pic_order_cnt_val = 8;
    decoder.dpb[3].layer_id = 0;

    // Looking for POC 7, closest is POC 8 (DPB[3])
    let result = decoder.create_lost_ref_pic(7, 0, DPB_MARKING_SHORT_TERM);
    assert_eq!(result, 3);
}

#[test]
fn test_create_lost_ref_pic_not_found() {
    let decoder = VulkanH265Decoder::new();
    let result = decoder.create_lost_ref_pic(42, 0, DPB_MARKING_SHORT_TERM);
    assert_eq!(result, -1);
}
