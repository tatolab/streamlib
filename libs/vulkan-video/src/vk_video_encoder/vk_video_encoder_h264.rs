// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderH264.h + VkVideoEncoderH264.cpp
//!
//! H.264 codec-specific encoder.
//! Manages H.264 frame info (slice headers, NALU info, rate control),
//! DPB processing, reference picture list construction, MMCO commands,
//! and reference picture reordering.

use crate::vk_video_encoder::vk_encoder_dpb_h264::VkEncDpbH264;
use crate::vk_video_encoder::vk_video_encoder::{
    VkVideoEncodeFrameInfo, VkVideoEncoder, CodecType,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const NON_VCL_BITSTREAM_OFFSET: usize = 4096;
pub const MAX_NUM_SLICES_H264: usize = 64;
pub const MAX_MEM_MGMNT_CTRL_OPS_COMMANDS: usize = 16;
pub const MAX_REFERENCES: usize = 16;

// ---------------------------------------------------------------------------
// VkVideoEncodeFrameInfoH264
// ---------------------------------------------------------------------------

/// H.264-specific per-frame encode info.
///
/// Ports `VkVideoEncodeFrameInfoH264` from the C++ struct.
#[derive(Debug)]
pub struct VkVideoEncodeFrameInfoH264 {
    pub base: VkVideoEncodeFrameInfo,

    // H.264-specific picture info
    pub nalu_slice_entry_count: u32,
    pub slice_type: [u32; MAX_NUM_SLICES_H264], // STD_VIDEO_H264_SLICE_TYPE_*

    // Picture parameters
    pub seq_parameter_set_id: u8,
    pub pic_parameter_set_id: u8,
    pub frame_num: u32,
    pub pic_order_cnt: i32,
    pub idr_pic_id: u16,
    pub primary_pic_type: u32,

    // Flags
    pub is_idr: bool,
    pub is_reference: bool,
    pub long_term_reference_flag: bool,
    pub no_output_of_prior_pics_flag: bool,
    pub adaptive_ref_pic_marking_mode_flag: bool,

    // Slice header fields
    pub disable_deblocking_filter_idc: u32,
    pub cabac_init_idc: u32,
    pub num_ref_idx_active_override_flag: [bool; MAX_NUM_SLICES_H264],

    // Reference lists
    pub ref_pic_list0: [u8; 16],
    pub ref_pic_list1: [u8; 16],
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,

    // MMCO
    pub ref_pic_marking_op_count: u8,
    pub ref_list0_mod_op_count: u8,
    pub ref_list1_mod_op_count: u8,

    // Rate control
    pub constant_qp: [i32; MAX_NUM_SLICES_H264],
}

impl Default for VkVideoEncodeFrameInfoH264 {
    fn default() -> Self {
        Self {
            base: VkVideoEncodeFrameInfo { codec: CodecType::H264, ..Default::default() },
            nalu_slice_entry_count: 1,
            slice_type: [0; MAX_NUM_SLICES_H264],
            seq_parameter_set_id: 0,
            pic_parameter_set_id: 0,
            frame_num: 0,
            pic_order_cnt: 0,
            idr_pic_id: 0,
            primary_pic_type: 0,
            is_idr: false,
            is_reference: false,
            long_term_reference_flag: false,
            no_output_of_prior_pics_flag: false,
            adaptive_ref_pic_marking_mode_flag: false,
            disable_deblocking_filter_idc: 0,
            cabac_init_idc: 0,
            num_ref_idx_active_override_flag: [false; MAX_NUM_SLICES_H264],
            ref_pic_list0: [0xff; 16],
            ref_pic_list1: [0xff; 16],
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_marking_op_count: 0,
            ref_list0_mod_op_count: 0,
            ref_list1_mod_op_count: 0,
            constant_qp: [0; MAX_NUM_SLICES_H264],
        }
    }
}

impl VkVideoEncodeFrameInfoH264 {
    pub fn reset(&mut self, release_resources: bool) {
        self.base.reset(release_resources);
        self.ref_pic_list0 = [0xff; 16];
        self.ref_pic_list1 = [0xff; 16];
        self.ref_pic_marking_op_count = 0;
        self.ref_list0_mod_op_count = 0;
        self.ref_list1_mod_op_count = 0;
    }
}

// ---------------------------------------------------------------------------
// VkVideoEncoderH264
// ---------------------------------------------------------------------------

/// H.264 encoder — codec-specific implementation.
pub struct VkVideoEncoderH264 {
    pub base: VkVideoEncoder,
    pub dpb264: Option<Box<VkEncDpbH264>>,

    // SPS/PPS parameters (from EncoderH264State)
    pub log2_max_frame_num_minus4: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub pic_order_cnt_type: u32,
    pub max_num_ref_frames: u32,
    pub gaps_in_frame_num_value_allowed_flag: bool,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,

    // Rate control
    pub slice_count: u32,

    // Per-sequence mutable counters (previously on Encoder)
    /// H.264 frame_num syntax element: increments per reference frame, wraps at max_frame_num.
    pub frame_num: u32,
    /// H.264 POC LSB: increments by 2 per frame (for POC type 0), wraps at max_poc_lsb.
    pub poc_lsb: i32,
    /// H.264 IDR picture ID: increments per IDR frame.
    pub idr_pic_id: u16,
}

impl VkVideoEncoderH264 {
    pub fn new() -> Self {
        Self {
            base: VkVideoEncoder::new(),
            dpb264: None,
            log2_max_frame_num_minus4: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            pic_order_cnt_type: 0,
            max_num_ref_frames: 1,
            gaps_in_frame_num_value_allowed_flag: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            slice_count: 1,
            frame_num: 0,
            poc_lsb: 0,
            idr_pic_id: 0,
        }
    }

    /// Initialize the H.264 DPB.
    pub fn init_dpb(&mut self, max_dpb_size: i32) {
        let mut dpb = VkEncDpbH264::create_instance();
        dpb.dpb_sequence_start(max_dpb_size);
        self.dpb264 = Some(dpb);
    }

    /// Destroy the H.264 encoder.
    pub fn destroy(&mut self) {
        if let Some(ref mut dpb) = self.dpb264 {
            dpb.dpb_destroy();
        }
        self.dpb264 = None;
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_info_h264_default() {
        let info = VkVideoEncodeFrameInfoH264::default();
        assert_eq!(info.base.codec, CodecType::H264);
        assert_eq!(info.nalu_slice_entry_count, 1);
        assert!(!info.is_idr);
        assert_eq!(info.ref_pic_list0[0], 0xff);
    }

    #[test]
    fn test_encoder_h264_new() {
        let enc = VkVideoEncoderH264::new();
        assert!(enc.dpb264.is_none());
        assert_eq!(enc.slice_count, 1);
    }

    #[test]
    fn test_encoder_h264_init_dpb() {
        let mut enc = VkVideoEncoderH264::new();
        enc.init_dpb(4);
        assert!(enc.dpb264.is_some());
    }

    #[test]
    fn test_frame_info_h264_reset() {
        let mut info = VkVideoEncodeFrameInfoH264::default();
        info.base.frame_input_order_num = 42;
        info.ref_pic_marking_op_count = 5;
        info.reset(true);
        assert_eq!(info.base.frame_input_order_num, u64::MAX);
        assert_eq!(info.ref_pic_marking_op_count, 0);
    }

    #[test]
    fn test_constants() {
        assert_eq!(MAX_NUM_SLICES_H264, 64);
        assert_eq!(MAX_MEM_MGMNT_CTRL_OPS_COMMANDS, 16);
        assert_eq!(MAX_REFERENCES, 16);
    }
}
