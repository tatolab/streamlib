// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderH265.h + VkVideoEncoderH265.cpp
//!
//! H.265/HEVC codec-specific encoder.
//! Manages H.265 frame info (slice segment headers, VPS/SPS/PPS references,
//! rate control), DPB processing via VkEncDpbH265, and reference picture
//! list construction.

use crate::vk_video_encoder::vk_encoder_dpb_h265::VkEncDpbH265;
use crate::vk_video_encoder::vk_video_encoder::{
    VkVideoEncodeFrameInfo, VkVideoEncoder, CodecType,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_REFERENCES_H265: usize = 16;
pub const MAX_NUM_SLICES_H265: usize = 64;

// ---------------------------------------------------------------------------
// VkVideoEncodeFrameInfoH265
// ---------------------------------------------------------------------------

/// H.265-specific per-frame encode info.
#[derive(Debug)]
pub struct VkVideoEncodeFrameInfoH265 {
    pub base: VkVideoEncodeFrameInfo,

    // Picture info
    pub nalu_slice_segment_entry_count: u32,

    // VPS/SPS/PPS IDs
    pub sps_video_parameter_set_id: u8,
    pub pps_seq_parameter_set_id: u8,
    pub pps_pic_parameter_set_id: u8,

    // Picture type
    pub pic_type: u32, // StdVideoH265PictureType
    pub pic_order_cnt_val: i32,
    pub temporal_id: u8,

    // Flags
    pub is_reference: bool,
    pub is_irap: bool,
    pub pic_output_flag: bool,
    pub no_output_of_prior_pics_flag: bool,
    pub short_term_ref_pic_set_sps_flag: bool,

    // Slice segment header
    pub slice_type: [u32; MAX_NUM_SLICES_H265],
    pub max_num_merge_cand: [u8; MAX_NUM_SLICES_H265],

    // Reference lists
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub ref_pic_list0: [u8; 15],
    pub ref_pic_list1: [u8; 15],

    // Rate control
    pub constant_qp: [i32; MAX_NUM_SLICES_H265],
}

impl Default for VkVideoEncodeFrameInfoH265 {
    fn default() -> Self {
        Self {
            base: VkVideoEncodeFrameInfo { codec: CodecType::H265, ..Default::default() },
            nalu_slice_segment_entry_count: 1,
            sps_video_parameter_set_id: 0,
            pps_seq_parameter_set_id: 0,
            pps_pic_parameter_set_id: 0,
            pic_type: 0,
            pic_order_cnt_val: 0,
            temporal_id: 0,
            is_reference: false,
            is_irap: false,
            pic_output_flag: true,
            no_output_of_prior_pics_flag: false,
            short_term_ref_pic_set_sps_flag: true,
            slice_type: [0; MAX_NUM_SLICES_H265],
            max_num_merge_cand: [5; MAX_NUM_SLICES_H265],
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list0: [0xff; 15],
            ref_pic_list1: [0xff; 15],
            constant_qp: [0; MAX_NUM_SLICES_H265],
        }
    }
}

impl VkVideoEncodeFrameInfoH265 {
    pub fn reset(&mut self, release_resources: bool) {
        self.base.reset(release_resources);
        self.ref_pic_list0 = [0xff; 15];
        self.ref_pic_list1 = [0xff; 15];
    }
}

// ---------------------------------------------------------------------------
// VkVideoEncoderH265
// ---------------------------------------------------------------------------

/// H.265 encoder — codec-specific implementation.
pub struct VkVideoEncoderH265 {
    pub base: VkVideoEncoder,
    pub dpb: VkEncDpbH265,

    // SPS parameters
    pub num_ref_l0: u8,
    pub num_ref_l1: u8,
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub num_short_term_ref_pic_sets: u8,
    pub long_term_ref_pics_present_flag: bool,

    // PPS parameters
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,

    // Config
    pub slice_count: u32,
    pub encode_width: u32,
    pub encode_height: u32,
}

impl VkVideoEncoderH265 {
    pub fn new() -> Self {
        Self {
            base: VkVideoEncoder::new(),
            dpb: VkEncDpbH265::new(),
            num_ref_l0: 0,
            num_ref_l1: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            num_short_term_ref_pic_sets: 0,
            long_term_ref_pics_present_flag: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            slice_count: 1,
            encode_width: 0,
            encode_height: 0,
        }
    }

    /// Initialize the H.265 DPB.
    pub fn init_dpb(&mut self, max_dpb_size: i32, use_multiple_refs: bool) {
        self.dpb.dpb_sequence_start(max_dpb_size, use_multiple_refs);
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_info_h265_default() {
        let info = VkVideoEncodeFrameInfoH265::default();
        assert_eq!(info.base.codec, CodecType::H265);
        assert_eq!(info.nalu_slice_segment_entry_count, 1);
        assert!(info.pic_output_flag);
        assert_eq!(info.ref_pic_list0[0], 0xff);
    }

    #[test]
    fn test_encoder_h265_new() {
        let enc = VkVideoEncoderH265::new();
        assert_eq!(enc.slice_count, 1);
        assert_eq!(enc.num_ref_l0, 0);
    }

    #[test]
    fn test_encoder_h265_init_dpb() {
        let mut enc = VkVideoEncoderH265::new();
        enc.init_dpb(4, true);
    }

    #[test]
    fn test_frame_info_h265_reset() {
        let mut info = VkVideoEncodeFrameInfoH265::default();
        info.base.frame_input_order_num = 42;
        info.reset(true);
        assert_eq!(info.base.frame_input_order_num, u64::MAX);
        assert_eq!(info.ref_pic_list0[0], 0xff);
    }
}
