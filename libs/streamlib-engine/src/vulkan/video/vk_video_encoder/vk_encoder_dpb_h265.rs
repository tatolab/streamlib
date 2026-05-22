// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderDpbH265.h + VkEncoderDpbH265.cpp
//!
//! H.265/HEVC DPB management for the encoder.
//! Implements reference picture set (RPS) derivation, short-term and long-term
//! reference picture marking, DPB bumping, and reference picture list construction.

use std::cmp;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_DPB_SIZE_H265: usize = 16; // STD_VIDEO_H265_MAX_DPB_SIZE
pub const MAX_NUM_LIST_REF_H265: usize = 15; // STD_VIDEO_H265_MAX_NUM_LIST_REF

/// Sentinel value indicating "no reference picture" (STD_VIDEO_H265_NO_REFERENCE_PICTURE).
pub const NO_REFERENCE_PICTURE_H265: u8 = 0xFF;

// H.265 picture types matching StdVideoH265PictureType enum values.
const PIC_TYPE_IDR: u32 = 0; // STD_VIDEO_H265_PICTURE_TYPE_IDR
const PIC_TYPE_I: u32 = 1;   // STD_VIDEO_H265_PICTURE_TYPE_I
const PIC_TYPE_P: u32 = 2;   // STD_VIDEO_H265_PICTURE_TYPE_P
const PIC_TYPE_B: u32 = 3;   // STD_VIDEO_H265_PICTURE_TYPE_B

// ---------------------------------------------------------------------------
// DpbEntryH265
// ---------------------------------------------------------------------------

/// A single entry in the H.265 encoder DPB.
#[derive(Debug, Clone, Default)]
pub struct DpbEntryH265 {
    pub state: u32,    // 0: empty, 1: in use
    pub marking: u32,  // 0: unused, 1: short-term, 2: long-term
    pub output: bool,  // needed for output
    pub corrupted: bool,
    pub pic_order_cnt_val: u32,
    pub ref_pic_order_cnt: [i32; MAX_DPB_SIZE_H265],
    pub long_term_ref_pic: u32, // bitfield array
    pub frame_id: u64,
    pub temporal_id: i32,
    pub dirty_intra_refresh_regions: u32,
}

// ---------------------------------------------------------------------------
// RefPicSetH265
// ---------------------------------------------------------------------------

/// Reference picture set for H.265.
#[derive(Debug, Clone)]
pub struct RefPicSetH265 {
    pub st_curr_before: [i8; MAX_NUM_LIST_REF_H265],
    pub st_curr_after: [i8; MAX_NUM_LIST_REF_H265],
    pub lt_curr: [i8; MAX_NUM_LIST_REF_H265],
    pub st_foll: [i8; MAX_NUM_LIST_REF_H265],
    pub lt_foll: [i8; MAX_NUM_LIST_REF_H265],
}

impl Default for RefPicSetH265 {
    fn default() -> Self {
        Self {
            st_curr_before: [-1; MAX_NUM_LIST_REF_H265],
            st_curr_after: [-1; MAX_NUM_LIST_REF_H265],
            lt_curr: [-1; MAX_NUM_LIST_REF_H265],
            st_foll: [-1; MAX_NUM_LIST_REF_H265],
            lt_foll: [-1; MAX_NUM_LIST_REF_H265],
        }
    }
}

// ---------------------------------------------------------------------------
// InitializeRpsResult
// ---------------------------------------------------------------------------

/// Result of `initialize_rps`, containing the STRPS and SPS-flag information
/// needed to populate StdVideoEncodeH265PictureInfo.
#[derive(Debug, Clone)]
pub struct InitializeRpsResult {
    /// The constructed short-term reference picture set.
    pub short_term_ref_pic_set: ShortTermRefPicSet,
    /// If true, the STRPS matches one in the SPS and short_term_ref_pic_set_idx
    /// should be used instead of signalling the STRPS inline.
    pub short_term_ref_pic_set_sps_flag: bool,
    /// Index into the SPS STRPS array (valid only when sps_flag is true).
    pub short_term_ref_pic_set_idx: u8,
}

// ---------------------------------------------------------------------------
// ShortTermRefPicSet — Rust-side mirror of StdVideoH265ShortTermRefPicSet
// ---------------------------------------------------------------------------

/// Rust-side mirror of StdVideoH265ShortTermRefPicSet.
/// We build this in the DPB module, and the caller copies it into the
/// vulkanalia struct for the Vulkan command buffer.
#[derive(Debug, Clone, Default)]
pub struct ShortTermRefPicSet {
    pub inter_ref_pic_set_prediction_flag: bool,
    pub num_negative_pics: u8,
    pub num_positive_pics: u8,
    pub delta_poc_s0_minus1: [u16; MAX_DPB_SIZE_H265],
    pub delta_poc_s1_minus1: [u16; MAX_DPB_SIZE_H265],
    pub used_by_curr_pic_s0_flag: u16,
    pub used_by_curr_pic_s1_flag: u16,
}

// ---------------------------------------------------------------------------
// SetupRefListResult
// ---------------------------------------------------------------------------

/// Result of `setup_reference_picture_list_lx`.
#[derive(Debug, Clone)]
pub struct SetupRefListResult {
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub ref_pic_list0: [u8; MAX_NUM_LIST_REF_H265],
    pub ref_pic_list1: [u8; MAX_NUM_LIST_REF_H265],
}

impl Default for SetupRefListResult {
    fn default() -> Self {
        Self {
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list0: [NO_REFERENCE_PICTURE_H265; MAX_NUM_LIST_REF_H265],
            ref_pic_list1: [NO_REFERENCE_PICTURE_H265; MAX_NUM_LIST_REF_H265],
        }
    }
}

// ---------------------------------------------------------------------------
// VkEncDpbH265
// ---------------------------------------------------------------------------

/// H.265 encoder DPB.
pub struct VkEncDpbH265 {
    dpb: [DpbEntryH265; MAX_DPB_SIZE_H265],
    cur_dpb_index: i8,
    dpb_size: i8,

    num_poc_st_curr_before: i8,
    num_poc_st_curr_after: i8,
    num_poc_st_foll: i8,
    num_poc_lt_curr: i8,
    num_poc_lt_foll: i8,

    last_idr_time_stamp: u64,
    pic_order_cnt_cra: i32,
    refresh_pending: bool,
    long_term_flags: u32,
    use_multiple_refs: bool,
}

impl VkEncDpbH265 {
    pub fn new() -> Self {
        Self {
            dpb: std::array::from_fn(|_| DpbEntryH265::default()),
            cur_dpb_index: 0,
            dpb_size: 0,
            num_poc_st_curr_before: 0,
            num_poc_st_curr_after: 0,
            num_poc_st_foll: 0,
            num_poc_lt_curr: 0,
            num_poc_lt_foll: 0,
            last_idr_time_stamp: 0,
            pic_order_cnt_cra: 0,
            refresh_pending: false,
            long_term_flags: 0,
            use_multiple_refs: false,
        }
    }

    /// Initialize DPB for a new sequence.
    pub fn dpb_sequence_start(&mut self, dpb_size: i32, use_multiple_references: bool) -> bool {
        debug_assert!(dpb_size >= 0);
        self.dpb_size = cmp::min(dpb_size as i8, MAX_DPB_SIZE_H265 as i8);

        for i in 0..MAX_DPB_SIZE_H265 {
            self.dpb[i] = DpbEntryH265::default();
        }

        self.use_multiple_refs = use_multiple_references;
        tracing::debug!(
            dpb_size = self.dpb_size,
            use_multiple_references = use_multiple_references,
            "H265 DPB sequence start"
        );
        true
    }

    // -----------------------------------------------------------------------
    // Reference Picture Marking (port of C++ ReferencePictureMarking)
    // -----------------------------------------------------------------------

    /// Reference picture marking. Full port of C++ ReferencePictureMarking,
    /// including CRA handling and sliding window for multi-ref.
    ///
    /// `pic_type` uses the STD_VIDEO_H265_PICTURE_TYPE_* constants (0=IDR, 1=I, 2=P, 3=B).
    pub fn reference_picture_marking(
        &mut self,
        cur_poc: i32,
        pic_type: u32,
        long_term_ref_pics_present_flag: bool,
    ) {
        let pic_type_name = match pic_type {
            PIC_TYPE_IDR => "IDR",
            PIC_TYPE_I => "I",
            PIC_TYPE_P => "P",
            PIC_TYPE_B => "B",
            _ => "UNKNOWN",
        };
        tracing::debug!(
            cur_poc = cur_poc,
            pic_type = pic_type,
            pic_type_name = pic_type_name,
            long_term_ref_pics_present = long_term_ref_pics_present_flag,
            "H265 DPB reference_picture_marking BEGIN"
        );

        // Log DPB state before marking
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 {
                tracing::debug!(
                    slot = i,
                    state = self.dpb[i].state,
                    marking = self.dpb[i].marking,
                    poc = self.dpb[i].pic_order_cnt_val,
                    corrupted = self.dpb[i].corrupted,
                    "  DPB entry BEFORE marking"
                );
            }
        }

        if pic_type == PIC_TYPE_IDR {
            tracing::debug!("  IDR: clearing all DPB markings to 0");
            for i in 0..self.dpb_size as usize {
                self.dpb[i].marking = 0;
            }
        } else {
            // CRA reference marking pending
            if self.refresh_pending && cur_poc > self.pic_order_cnt_cra {
                for i in 0..self.dpb_size as usize {
                    if self.dpb[i].pic_order_cnt_val != self.pic_order_cnt_cra as u32 {
                        self.dpb[i].marking = 0;
                    }
                }
                self.refresh_pending = false;
            }

            // CRA picture found
            if pic_type == PIC_TYPE_I {
                self.refresh_pending = true;
                self.pic_order_cnt_cra = cur_poc;
            }

            if self.use_multiple_refs {
                let mut num_long_term_ref_pics: i32 = 0;
                let mut num_short_term_ref_pics: i32 = 0;
                let mut num_corrupted_ref_pics: i32 = 0;
                let mut min_poc_st_idx: i32 = -1;
                let mut min_poc_st_val: u32 = u32::MAX;
                let mut min_poc_lt_idx: i32 = -1;
                let mut min_poc_lt_val: u32 = u32::MAX;
                let mut min_poc_corrupted_idx: i32 = -1;
                let mut min_poc_corrupted_val: u32 = u32::MAX;

                for i in 0..self.dpb_size as usize {
                    if self.dpb[i].state == 1 && self.dpb[i].marking == 1 && !self.dpb[i].corrupted {
                        num_short_term_ref_pics += 1;
                        if self.dpb[i].pic_order_cnt_val < min_poc_st_val {
                            min_poc_st_val = self.dpb[i].pic_order_cnt_val;
                            min_poc_st_idx = i as i32;
                        }
                    }
                    if self.dpb[i].state == 1 && self.dpb[i].marking == 2 && !self.dpb[i].corrupted {
                        num_long_term_ref_pics += 1;
                        if self.dpb[i].pic_order_cnt_val < min_poc_lt_val {
                            min_poc_lt_val = self.dpb[i].pic_order_cnt_val;
                            min_poc_lt_idx = i as i32;
                        }
                    }
                    if self.dpb[i].state == 1 && self.dpb[i].corrupted {
                        num_corrupted_ref_pics += 1;
                        if self.dpb[i].pic_order_cnt_val < min_poc_corrupted_val {
                            min_poc_corrupted_val = self.dpb[i].pic_order_cnt_val;
                            min_poc_corrupted_idx = i as i32;
                        }
                    }
                }

                if !long_term_ref_pics_present_flag {
                    if (num_short_term_ref_pics + num_long_term_ref_pics + num_corrupted_ref_pics)
                        > (self.dpb_size as i32 - 1)
                    {
                        if num_corrupted_ref_pics > 0
                            && min_poc_corrupted_val < min_poc_st_val
                            && min_poc_corrupted_idx >= 0
                            && min_poc_corrupted_idx < self.dpb_size as i32
                        {
                            self.dpb[min_poc_corrupted_idx as usize].marking = 0;
                        } else if num_short_term_ref_pics > 0
                            && min_poc_st_idx >= 0
                            && min_poc_st_idx < self.dpb_size as i32
                        {
                            self.dpb[min_poc_st_idx as usize].marking = 0;
                        } else if num_long_term_ref_pics > 0
                            && min_poc_lt_idx >= 0
                            && min_poc_lt_idx < self.dpb_size as i32
                        {
                            self.dpb[min_poc_lt_idx as usize].marking = 0;
                        }
                    }
                } else {
                    let num_active_ref_frames =
                        num_short_term_ref_pics + num_long_term_ref_pics + num_corrupted_ref_pics;
                    let max_allowed_ltr_frames = 0i32;
                    if num_active_ref_frames > (self.dpb_size as i32 - 1) {
                        if num_corrupted_ref_pics > 0
                            && min_poc_corrupted_val < min_poc_st_val
                            && min_poc_corrupted_idx >= 0
                            && min_poc_corrupted_idx < self.dpb_size as i32
                        {
                            self.dpb[min_poc_corrupted_idx as usize].marking = 0;
                        } else if num_long_term_ref_pics > max_allowed_ltr_frames
                            && min_poc_lt_idx < self.dpb_size as i32
                            && min_poc_lt_idx >= 0
                        {
                            self.dpb[min_poc_lt_idx as usize].marking = 0;
                        } else if num_short_term_ref_pics > 0
                            && min_poc_st_idx < self.dpb_size as i32
                            && min_poc_st_idx >= 0
                        {
                            self.dpb[min_poc_st_idx as usize].marking = 0;
                        }
                    }
                }
            }
        }

        // Log DPB state after marking
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 {
                tracing::debug!(
                    slot = i,
                    state = self.dpb[i].state,
                    marking = self.dpb[i].marking,
                    poc = self.dpb[i].pic_order_cnt_val,
                    "  DPB entry AFTER marking"
                );
            }
        }
        tracing::debug!("H265 DPB reference_picture_marking END");
    }

    // -----------------------------------------------------------------------
    // InitializeRPS (port of C++ InitializeRPS + InitializeShortTermRPSPFrame)
    // -----------------------------------------------------------------------

    /// Initialize the Reference Picture Set for the current picture.
    /// Port of C++ InitializeRPS which delegates to InitializeShortTermRPSPFrame.
    ///
    /// Parameters:
    /// - `sps_short_term_rps`: STRPS array from the SPS (for matching against SPS-signalled sets).
    ///   Pass `&[]` if none are signalled in the SPS.
    /// - `pic_type`: STD_VIDEO_H265_PICTURE_TYPE_* value
    /// - `pic_order_cnt_val`: current picture's POC
    /// - `temporal_id`: current picture's temporal ID
    /// - `is_irap`: whether the current picture is an IRAP
    /// - `num_ref_l0`, `num_ref_l1`: desired number of L0/L1 references
    pub fn initialize_rps(
        &mut self,
        sps_short_term_rps: &[ShortTermRefPicSet],
        pic_type: u32,
        pic_order_cnt_val: u32,
        temporal_id: i32,
        is_irap: bool,
        num_ref_l0: u32,
        num_ref_l1: u32,
    ) -> InitializeRpsResult {
        tracing::debug!(
            pic_type = pic_type,
            pic_order_cnt_val = pic_order_cnt_val,
            temporal_id = temporal_id,
            is_irap = is_irap,
            num_ref_l0 = num_ref_l0,
            num_ref_l1 = num_ref_l1,
            sps_short_term_rps_count = sps_short_term_rps.len(),
            "H265 DPB initialize_rps BEGIN"
        );

        // Log all DPB entries for reference analysis
        for i in 0..self.dpb_size as usize {
            tracing::debug!(
                slot = i,
                state = self.dpb[i].state,
                marking = self.dpb[i].marking,
                poc = self.dpb[i].pic_order_cnt_val,
                corrupted = self.dpb[i].corrupted,
                temporal_id = self.dpb[i].temporal_id,
                "  DPB entry scanned by initialize_rps"
            );
        }

        let num_poc_lt_curr: i32 = 0; // No long-term refs for IPP-only
        let result = self.initialize_short_term_rps_p_frame(
            num_poc_lt_curr,
            sps_short_term_rps,
            pic_type,
            pic_order_cnt_val,
            temporal_id,
            is_irap,
            num_ref_l0,
            num_ref_l1,
        );

        // Log the produced STRPS
        tracing::debug!(
            num_negative = result.short_term_ref_pic_set.num_negative_pics,
            num_positive = result.short_term_ref_pic_set.num_positive_pics,
            sps_flag = result.short_term_ref_pic_set_sps_flag,
            sps_idx = result.short_term_ref_pic_set_idx,
            used_by_curr_pic_s0_flag = result.short_term_ref_pic_set.used_by_curr_pic_s0_flag,
            "H265 DPB initialize_rps STRPS result"
        );
        for i in 0..result.short_term_ref_pic_set.num_negative_pics as usize {
            tracing::debug!(
                idx = i,
                delta_poc_s0_minus1 = result.short_term_ref_pic_set.delta_poc_s0_minus1[i],
                used = (result.short_term_ref_pic_set.used_by_curr_pic_s0_flag >> i) & 1,
                "  STRPS negative pic"
            );
        }
        for i in 0..result.short_term_ref_pic_set.num_positive_pics as usize {
            tracing::debug!(
                idx = i,
                delta_poc_s1_minus1 = result.short_term_ref_pic_set.delta_poc_s1_minus1[i],
                used = (result.short_term_ref_pic_set.used_by_curr_pic_s1_flag >> i) & 1,
                "  STRPS positive pic"
            );
        }

        result
    }

    /// Port of C++ InitializeShortTermRPSPFrame.
    /// Scans the DPB for valid short-term references, builds delta_poc arrays,
    /// constructs the ShortTermRefPicSet, and checks for SPS match.
    fn initialize_short_term_rps_p_frame(
        &self,
        num_poc_lt_curr: i32,
        sps_short_term_rps: &[ShortTermRefPicSet],
        pic_type: u32,
        cur_poc: u32,
        cur_temporal_id: i32,
        is_irap: bool,
        num_ref_l0: u32,
        num_ref_l1: u32,
    ) -> InitializeRpsResult {
        let mut short_term_ref_pic_poc_l0 = [0u32; MAX_DPB_SIZE_H265];
        let mut delta_poc_s0 = [0u32; MAX_DPB_SIZE_H265]; // stored as unsigned (wrapping subtraction)
        let mut used_by_curr_pic_s0 = [0i32; MAX_DPB_SIZE_H265];
        let mut short_term_ref_pic_poc_l1 = [0u32; MAX_DPB_SIZE_H265];
        let mut delta_poc_s1 = [0u32; MAX_DPB_SIZE_H265];
        let mut used_by_curr_pic_s1 = [0i32; MAX_DPB_SIZE_H265];

        let mut num_negative_ref_pics: i32 = 0;
        let mut num_positive_ref_pics: i32 = 0;
        let mut num_long_term_ref_pic: i32 = 0;
        let mut _num_poc_st_curr_before: i32 = 0;
        let mut _num_poc_st_curr_after: i32 = 0;
        let mut short_term_ref_pic_temporal_id_l0 = [0i32; MAX_DPB_SIZE_H265];
        let tsa_picture = false;

        let _ = num_long_term_ref_pic; // no long-term for IPP-only
        num_long_term_ref_pic = 0;

        // Scan DPB for negative references (POC < curPOC)
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].marking == 1
                && self.dpb[i].pic_order_cnt_val < cur_poc
                && !self.dpb[i].corrupted
                && (self.dpb[i].temporal_id < cur_temporal_id
                    || (!tsa_picture && self.dpb[i].temporal_id == cur_temporal_id))
            {
                let idx = num_negative_ref_pics as usize;
                short_term_ref_pic_poc_l0[idx] = self.dpb[i].pic_order_cnt_val;
                short_term_ref_pic_temporal_id_l0[idx] = self.dpb[i].temporal_id;
                // delta_poc_s0 stores the delta as wrapping unsigned (negative delta wraps)
                delta_poc_s0[idx] = self.dpb[i].pic_order_cnt_val.wrapping_sub(cur_poc);
                num_negative_ref_pics += 1;
            }
            if self.use_multiple_refs {
                if self.dpb[i].marking == 1
                    && self.dpb[i].pic_order_cnt_val > cur_poc
                    && !self.dpb[i].corrupted
                    && (self.dpb[i].temporal_id < cur_temporal_id
                        || (!tsa_picture && self.dpb[i].temporal_id == cur_temporal_id))
                {
                    let idx = num_positive_ref_pics as usize;
                    short_term_ref_pic_poc_l1[idx] = self.dpb[i].pic_order_cnt_val;
                    delta_poc_s1[idx] = self.dpb[i].pic_order_cnt_val.wrapping_sub(cur_poc);
                    num_positive_ref_pics += 1;
                }
            }
        }

        // Sort negative pictures in decreasing order of POC value
        for _i in 0..num_negative_ref_pics {
            for j in 0..num_negative_ref_pics as usize - 1 {
                if short_term_ref_pic_poc_l0[j] < short_term_ref_pic_poc_l0[j + 1] {
                    short_term_ref_pic_poc_l0.swap(j, j + 1);
                    delta_poc_s0.swap(j, j + 1);
                    short_term_ref_pic_temporal_id_l0.swap(j, j + 1);
                }
            }
        }

        // Sort positive pictures in increasing order of POC value
        if self.use_multiple_refs {
            for _i in 0..num_positive_ref_pics {
                for j in 0..num_positive_ref_pics as usize - 1 {
                    if short_term_ref_pic_poc_l1[j] > short_term_ref_pic_poc_l1[j + 1] {
                        short_term_ref_pic_poc_l1.swap(j, j + 1);
                        delta_poc_s1.swap(j, j + 1);
                    }
                }
            }

            // Trim if exceeding max ref frames
            while (num_poc_lt_curr + num_negative_ref_pics + num_positive_ref_pics)
                > (self.dpb_size as i32 - 1)
            {
                if num_negative_ref_pics > 0 {
                    num_negative_ref_pics -= 1;
                } else if num_positive_ref_pics > 0 {
                    num_positive_ref_pics -= 1;
                }
            }
        } else {
            while (num_long_term_ref_pic + num_negative_ref_pics + num_positive_ref_pics)
                > (self.dpb_size as i32 - 1)
            {
                num_negative_ref_pics -= 1;
            }
        }

        // HEVC spec: max 8 total reference frames usable by current picture
        const MAX_ALLOWED_NUM_REF_FRAMES: i32 = 8;
        let num_negative_ref_pics_used;
        let num_positive_ref_pics_used;

        if self.use_multiple_refs {
            if pic_type == PIC_TYPE_B {
                let max_st_ref_pics_curr =
                    cmp::max(MAX_ALLOWED_NUM_REF_FRAMES - num_poc_lt_curr, 2);
                num_positive_ref_pics_used =
                    cmp::max(max_st_ref_pics_curr - num_ref_l0 as i32, 1);
                num_negative_ref_pics_used = max_st_ref_pics_curr - num_positive_ref_pics_used;
            } else {
                let max_st_ref_pics_curr =
                    cmp::max(MAX_ALLOWED_NUM_REF_FRAMES - num_poc_lt_curr, 1);
                num_negative_ref_pics_used = max_st_ref_pics_curr;
                num_positive_ref_pics_used = 0;
            }
        } else {
            let max_st_ref_pics_curr =
                cmp::min(1, MAX_ALLOWED_NUM_REF_FRAMES - num_poc_lt_curr);
            num_negative_ref_pics_used = max_st_ref_pics_curr;
            num_positive_ref_pics_used = 0;
        }

        if !is_irap {
            for i in 0..num_negative_ref_pics as usize {
                if (i as i32) < num_negative_ref_pics_used && (i as u32) < num_ref_l0 {
                    used_by_curr_pic_s0[i] = 1;
                    _num_poc_st_curr_before += 1;
                } else {
                    used_by_curr_pic_s0[i] = 0;
                }
            }

            if self.use_multiple_refs {
                for i in 0..num_positive_ref_pics as usize {
                    if (i as i32) < num_positive_ref_pics_used && (i as u32) < num_ref_l1 {
                        used_by_curr_pic_s1[i] = 1;
                        _num_poc_st_curr_after += 1;
                    } else {
                        used_by_curr_pic_s1[i] = 0;
                    }
                }
            }
        }

        // Build the STRPS struct
        let mut tmp_strps = ShortTermRefPicSet::default();

        if num_negative_ref_pics > 0 || num_positive_ref_pics > 0 {
            tmp_strps.inter_ref_pic_set_prediction_flag = false;
            tmp_strps.num_negative_pics = num_negative_ref_pics as u8;
            tmp_strps.num_positive_pics = num_positive_ref_pics as u8;

            let mut prev_delta: u32 = 0;
            for i in 0..tmp_strps.num_negative_pics as usize {
                // delta_poc_s0[i] is wrapping unsigned (e.g., -1 = 0xFFFF_FFFF).
                // delta_poc_s0_minus1 = prevDelta - delta_poc_s0[i] - 1
                // In C++: tmpSTRPS.delta_poc_s0_minus1[numStRefL0] = (uint8_t)(prevDelta - deltaPocS0[numStRefL0] - 1);
                tmp_strps.delta_poc_s0_minus1[i] =
                    prev_delta.wrapping_sub(delta_poc_s0[i]).wrapping_sub(1) as u16;
                tmp_strps.used_by_curr_pic_s0_flag |=
                    ((used_by_curr_pic_s0[i] & 1) as u16) << i;
                prev_delta = delta_poc_s0[i];
            }

            if self.use_multiple_refs {
                let mut prev_delta: u32 = 0;
                for i in 0..tmp_strps.num_positive_pics as usize {
                    tmp_strps.delta_poc_s1_minus1[i] =
                        delta_poc_s1[i].wrapping_sub(prev_delta).wrapping_sub(1) as u16;
                    tmp_strps.used_by_curr_pic_s1_flag |=
                        ((used_by_curr_pic_s1[i] & 1) as u16) << i;
                    prev_delta = delta_poc_s1[i];
                }
            }
        }

        // Check if the STRPS matches one signalled in the SPS
        let mut sps_strps_idx: i32 = -1;
        for (i, sps_rps) in sps_short_term_rps.iter().enumerate() {
            if sps_rps.num_negative_pics == tmp_strps.num_negative_pics
                && sps_rps.num_positive_pics == tmp_strps.num_positive_pics
            {
                let mut found = true;

                let used_s0_xored =
                    sps_rps.used_by_curr_pic_s0_flag ^ tmp_strps.used_by_curr_pic_s0_flag;
                for j in 0..sps_rps.num_negative_pics as usize {
                    if sps_rps.delta_poc_s0_minus1[j] != tmp_strps.delta_poc_s0_minus1[j]
                        || ((used_s0_xored >> j) & 0x1) != 0
                    {
                        found = false;
                        break;
                    }
                }

                if found && self.use_multiple_refs {
                    let used_s1_xored =
                        sps_rps.used_by_curr_pic_s1_flag ^ tmp_strps.used_by_curr_pic_s1_flag;
                    for j in 0..sps_rps.num_positive_pics as usize {
                        if sps_rps.delta_poc_s1_minus1[j] != tmp_strps.delta_poc_s1_minus1[j]
                            || ((used_s1_xored >> j) & 0x1) != 0
                        {
                            found = false;
                            break;
                        }
                    }
                }

                if found {
                    sps_strps_idx = i as i32;
                    break;
                }
            }
        }

        if sps_strps_idx >= 0 {
            InitializeRpsResult {
                short_term_ref_pic_set: tmp_strps,
                short_term_ref_pic_set_sps_flag: true,
                short_term_ref_pic_set_idx: sps_strps_idx as u8,
            }
        } else {
            InitializeRpsResult {
                short_term_ref_pic_set: tmp_strps,
                short_term_ref_pic_set_sps_flag: false,
                short_term_ref_pic_set_idx: 0,
            }
        }
    }

    // -----------------------------------------------------------------------
    // DpbPictureStart (updated port: now calls ApplyReferencePictureSet)
    // -----------------------------------------------------------------------

    /// Start processing a picture. This version matches the C++ DpbPictureStart
    /// signature: it takes the STRPS and calls ApplyReferencePictureSet internally
    /// to derive the RefPicSet.
    ///
    /// Returns `(dpb_slot_index, ref_pic_set)`. The dpb_slot_index is -1 on error.
    pub fn dpb_picture_start_with_rps(
        &mut self,
        frame_id: u64,
        pic_order_cnt_val: u32,
        pic_type: u32,
        is_irap: bool,
        is_idr: bool,
        pic_output_flag: bool,
        temporal_id: i32,
        no_output_of_prior_pics_flag: bool,
        time_stamp: u64,
        short_term_ref_pic_set: &ShortTermRefPicSet,
        max_pic_order_cnt_lsb: i32,
    ) -> (i8, RefPicSetH265) {
        tracing::debug!(
            frame_id = frame_id,
            poc = pic_order_cnt_val,
            pic_type = pic_type,
            is_irap = is_irap,
            is_idr = is_idr,
            temporal_id = temporal_id,
            no_output_of_prior_pics = no_output_of_prior_pics_flag,
            strps_num_negative = short_term_ref_pic_set.num_negative_pics,
            strps_num_positive = short_term_ref_pic_set.num_positive_pics,
            max_pic_order_cnt_lsb = max_pic_order_cnt_lsb,
            "H265 DPB dpb_picture_start_with_rps BEGIN"
        );

        // Apply the reference picture set first (before DPB slot allocation)
        let ref_pic_set = self.apply_reference_picture_set(
            pic_order_cnt_val,
            pic_type,
            is_irap,
            short_term_ref_pic_set,
            max_pic_order_cnt_lsb,
        );

        let no_rasl_output_flag = is_idr;

        if is_irap && no_rasl_output_flag {
            let no_output = if is_idr {
                // C++ uses I-type check for NoOutputOfPriorPicsFlag on IDR;
                // in practice IDR always gets no_output_of_prior_pics from the flags.
                no_output_of_prior_pics_flag
            } else {
                no_output_of_prior_pics_flag
            };
            if no_output {
                for i in 0..self.dpb_size as usize {
                    self.dpb[i] = DpbEntryH265::default();
                }
            } else {
                self.flush_dpb();
            }
        } else {
            for i in 0..self.dpb_size as usize {
                if self.dpb[i].marking == 0 && !self.dpb[i].output {
                    self.dpb[i].state = 0;
                }
            }
            while self.is_dpb_full() {
                self.dpb_bumping();
            }
        }

        // Find free slot
        self.cur_dpb_index = -1;
        for i in 0..self.dpb_size as i8 {
            if self.dpb[i as usize].state == 0 {
                self.cur_dpb_index = i;
                break;
            }
        }
        if self.cur_dpb_index < 0 {
            return (-1, ref_pic_set);
        }

        let idx = self.cur_dpb_index as usize;
        self.dpb[idx].frame_id = frame_id;
        self.dpb[idx].pic_order_cnt_val = pic_order_cnt_val;
        self.dpb[idx].output = pic_output_flag;
        self.dpb[idx].corrupted = false;
        self.dpb[idx].temporal_id = temporal_id;

        if is_irap && no_rasl_output_flag {
            self.last_idr_time_stamp = time_stamp;
        }

        // Record reference POC values from current DPB
        for i in 0..self.dpb_size as usize {
            self.dpb[idx].ref_pic_order_cnt[i] = self.dpb[i].pic_order_cnt_val as i32;
            if self.dpb[i].marking == 2 {
                self.dpb[idx].long_term_ref_pic |= 1 << i;
            } else {
                self.dpb[idx].long_term_ref_pic &= !(1 << i);
            }
        }

        // Log the resulting RefPicSet
        tracing::debug!(
            allocated_slot = self.cur_dpb_index,
            "H265 DPB dpb_picture_start_with_rps: RefPicSet result"
        );
        for i in 0..MAX_NUM_LIST_REF_H265 {
            if ref_pic_set.st_curr_before[i] >= 0 {
                tracing::debug!(
                    idx = i,
                    dpb_slot = ref_pic_set.st_curr_before[i],
                    "  stCurrBefore entry"
                );
            }
        }
        for i in 0..MAX_NUM_LIST_REF_H265 {
            if ref_pic_set.st_curr_after[i] >= 0 {
                tracing::debug!(
                    idx = i,
                    dpb_slot = ref_pic_set.st_curr_after[i],
                    "  stCurrAfter entry"
                );
            }
        }

        (self.cur_dpb_index, ref_pic_set)
    }

    /// Legacy start processing (without RPS). Kept for backward compatibility.
    pub fn dpb_picture_start(
        &mut self,
        frame_id: u64,
        pic_order_cnt_val: u32,
        is_irap: bool,
        is_idr: bool,
        pic_output_flag: bool,
        temporal_id: i32,
        no_output_of_prior_pics_flag: bool,
        time_stamp: u64,
    ) -> i8 {
        tracing::debug!(
            frame_id = frame_id,
            poc = pic_order_cnt_val,
            is_irap = is_irap,
            is_idr = is_idr,
            "H265 DPB dpb_picture_start (legacy) BEGIN"
        );
        let no_rasl_output_flag = is_idr;

        if is_irap && no_rasl_output_flag {
            let no_output = if is_idr { false } else { no_output_of_prior_pics_flag };
            if no_output {
                for i in 0..self.dpb_size as usize {
                    self.dpb[i] = DpbEntryH265::default();
                }
            } else {
                self.flush_dpb();
            }
        } else {
            for i in 0..self.dpb_size as usize {
                if self.dpb[i].marking == 0 && !self.dpb[i].output {
                    self.dpb[i].state = 0;
                }
            }
            while self.is_dpb_full() {
                self.dpb_bumping();
            }
        }

        // Find free slot
        self.cur_dpb_index = -1;
        for i in 0..self.dpb_size as i8 {
            if self.dpb[i as usize].state == 0 {
                self.cur_dpb_index = i;
                break;
            }
        }
        if self.cur_dpb_index < 0 {
            return -1;
        }

        let idx = self.cur_dpb_index as usize;
        self.dpb[idx].frame_id = frame_id;
        self.dpb[idx].pic_order_cnt_val = pic_order_cnt_val;
        self.dpb[idx].output = pic_output_flag;
        self.dpb[idx].corrupted = false;
        self.dpb[idx].temporal_id = temporal_id;

        if is_irap && no_rasl_output_flag {
            self.last_idr_time_stamp = time_stamp;
        }

        // Record reference POC values from current DPB
        for i in 0..self.dpb_size as usize {
            self.dpb[idx].ref_pic_order_cnt[i] = self.dpb[i].pic_order_cnt_val as i32;
            if self.dpb[i].marking == 2 {
                self.dpb[idx].long_term_ref_pic |= 1 << i;
            } else {
                self.dpb[idx].long_term_ref_pic &= !(1 << i);
            }
        }

        self.cur_dpb_index
    }

    // -----------------------------------------------------------------------
    // ApplyReferencePictureSet (port of C++ ApplyReferencePictureSet)
    // -----------------------------------------------------------------------

    /// Derives RefPicSetStCurrBefore / StCurrAfter / LtCurr by finding DPB entries
    /// whose POC matches the STRPS delta POC values. Also marks DPB entries NOT
    /// in the reference set as marking=0.
    ///
    /// Port of C++ ApplyReferencePictureSet (line 260-519).
    fn apply_reference_picture_set(
        &mut self,
        pic_order_cnt_val: u32,
        pic_type: u32,
        is_irap: bool,
        short_term_ref_pic_set: &ShortTermRefPicSet,
        _max_pic_order_cnt_lsb: i32,
    ) -> RefPicSetH265 {
        tracing::debug!(
            poc = pic_order_cnt_val,
            pic_type = pic_type,
            is_irap = is_irap,
            num_negative = short_term_ref_pic_set.num_negative_pics,
            num_positive = short_term_ref_pic_set.num_positive_pics,
            used_by_curr_pic_s0_flag = short_term_ref_pic_set.used_by_curr_pic_s0_flag,
            "H265 DPB apply_reference_picture_set BEGIN"
        );

        let mut poc_st_curr_before = [0u32; MAX_NUM_LIST_REF_H265];
        let mut poc_st_curr_after = [0u32; MAX_NUM_LIST_REF_H265];
        let mut poc_st_foll = [0u32; MAX_NUM_LIST_REF_H265];

        let no_rasl_output_flag = if is_irap {
            pic_type == PIC_TYPE_IDR
        } else {
            false
        };

        if is_irap && no_rasl_output_flag {
            for i in 0..self.dpb_size as usize {
                self.dpb[i].marking = 0;
            }
        }

        if pic_type == PIC_TYPE_IDR {
            self.num_poc_st_curr_before = 0;
            self.num_poc_st_curr_after = 0;
            self.num_poc_st_foll = 0;
            self.num_poc_lt_curr = 0;
            self.num_poc_lt_foll = 0;
        } else {
            // Derive DeltaPocS0 and DeltaPocS1 from the STRPS
            let mut delta_poc_s0 = [-1i32; MAX_DPB_SIZE_H265];
            let mut delta_poc_s1 = [-1i32; MAX_DPB_SIZE_H265];

            for i in 0..short_term_ref_pic_set.num_negative_pics as usize {
                delta_poc_s0[i] = if i == 0 {
                    -(short_term_ref_pic_set.delta_poc_s0_minus1[i] as i32 + 1)
                } else {
                    delta_poc_s0[i - 1] - (short_term_ref_pic_set.delta_poc_s0_minus1[i] as i32 + 1)
                };
            }

            for i in 0..short_term_ref_pic_set.num_positive_pics as usize {
                delta_poc_s1[i] = if i == 0 {
                    short_term_ref_pic_set.delta_poc_s1_minus1[i] as i32 + 1
                } else {
                    delta_poc_s1[i - 1] + short_term_ref_pic_set.delta_poc_s1_minus1[i] as i32 + 1
                };
            }

            // Log computed DeltaPocS0/S1
            for i in 0..short_term_ref_pic_set.num_negative_pics as usize {
                tracing::debug!(
                    idx = i,
                    delta_poc_s0 = delta_poc_s0[i],
                    target_poc = (pic_order_cnt_val as i32 + delta_poc_s0[i]),
                    "  apply_rps: DeltaPocS0 computed"
                );
            }
            for i in 0..short_term_ref_pic_set.num_positive_pics as usize {
                tracing::debug!(
                    idx = i,
                    delta_poc_s1 = delta_poc_s1[i],
                    target_poc = (pic_order_cnt_val as i32 + delta_poc_s1[i]),
                    "  apply_rps: DeltaPocS1 computed"
                );
            }

            // Derive pocStCurrBefore, pocStCurrAfter, pocStFoll
            let mut j: usize = 0;
            let mut k: usize = 0;

            for i in 0..short_term_ref_pic_set.num_negative_pics as usize {
                if (short_term_ref_pic_set.used_by_curr_pic_s0_flag >> i) & 0x1 != 0 {
                    poc_st_curr_before[j] = (pic_order_cnt_val as i32 + delta_poc_s0[i]) as u32;
                    j += 1;
                } else {
                    poc_st_foll[k] = (pic_order_cnt_val as i32 + delta_poc_s0[i]) as u32;
                    k += 1;
                }
            }
            self.num_poc_st_curr_before = j as i8;

            j = 0;
            for i in 0..short_term_ref_pic_set.num_positive_pics as usize {
                if (short_term_ref_pic_set.used_by_curr_pic_s1_flag >> i) & 0x1 != 0 {
                    poc_st_curr_after[j] = (pic_order_cnt_val as i32 + delta_poc_s1[i]) as u32;
                    j += 1;
                } else {
                    poc_st_foll[k] = (pic_order_cnt_val as i32 + delta_poc_s1[i]) as u32;
                    k += 1;
                }
            }
            self.num_poc_st_curr_after = j as i8;
            self.num_poc_st_foll = k as i8;

            tracing::debug!(
                num_poc_st_curr_before = self.num_poc_st_curr_before,
                num_poc_st_curr_after = self.num_poc_st_curr_after,
                num_poc_st_foll = self.num_poc_st_foll,
                "  apply_rps: poc list counts"
            );
            for i in 0..self.num_poc_st_curr_before as usize {
                tracing::debug!(
                    idx = i,
                    poc = poc_st_curr_before[i],
                    "  apply_rps: pocStCurrBefore"
                );
            }
            for i in 0..self.num_poc_st_curr_after as usize {
                tracing::debug!(
                    idx = i,
                    poc = poc_st_curr_after[i],
                    "  apply_rps: pocStCurrAfter"
                );
            }

            // No long-term references for IPP-only
            self.num_poc_lt_curr = 0;
            self.num_poc_lt_foll = 0;
        }

        // Initialize RefPicSet to "no reference picture"
        let mut ref_pic_set = RefPicSetH265::default();

        // Find DPB entries matching pocStCurrBefore
        for i in 0..self.num_poc_st_curr_before as usize {
            tracing::debug!(
                idx = i,
                searching_poc = poc_st_curr_before[i],
                "  apply_rps: searching DPB for pocStCurrBefore match"
            );
            for j in 0..self.dpb_size as usize {
                if self.dpb[j].state == 1 {
                    tracing::debug!(
                        dpb_slot = j,
                        dpb_state = self.dpb[j].state,
                        dpb_marking = self.dpb[j].marking,
                        dpb_poc = self.dpb[j].pic_order_cnt_val,
                        target_poc = poc_st_curr_before[i],
                        matches = (self.dpb[j].marking == 1 && self.dpb[j].pic_order_cnt_val == poc_st_curr_before[i]),
                        "    DPB entry checked"
                    );
                }
                if self.dpb[j].state == 1
                    && self.dpb[j].marking == 1
                    && self.dpb[j].pic_order_cnt_val == poc_st_curr_before[i]
                {
                    ref_pic_set.st_curr_before[i] = j as i8;
                    tracing::debug!(
                        idx = i,
                        matched_slot = j,
                        poc = poc_st_curr_before[i],
                        "  apply_rps: pocStCurrBefore MATCHED"
                    );
                    break;
                }
            }
            if ref_pic_set.st_curr_before[i] < 0 {
                tracing::warn!(
                    idx = i,
                    poc = poc_st_curr_before[i],
                    "short-term reference picture NOT FOUND in DPB (pocStCurrBefore)"
                );
            }
        }

        // Find DPB entries matching pocStCurrAfter
        for i in 0..self.num_poc_st_curr_after as usize {
            for j in 0..self.dpb_size as usize {
                if self.dpb[j].state == 1
                    && self.dpb[j].marking == 1
                    && self.dpb[j].pic_order_cnt_val == poc_st_curr_after[i]
                {
                    ref_pic_set.st_curr_after[i] = j as i8;
                    break;
                }
            }
            if ref_pic_set.st_curr_after[i] < 0 {
                tracing::warn!(
                    "short-term reference picture not available (POC={})",
                    poc_st_curr_after[i]
                );
            }
        }

        // Find DPB entries matching pocStFoll
        for i in 0..self.num_poc_st_foll as usize {
            for j in 0..self.dpb_size as usize {
                if self.dpb[j].state == 1
                    && self.dpb[j].marking == 1
                    && self.dpb[j].pic_order_cnt_val == poc_st_foll[i]
                {
                    ref_pic_set.st_foll[i] = j as i8;
                    break;
                }
            }
        }

        // Mark all DPB entries NOT in any RefPicSet list as "unused for reference"
        let mut in_use = [false; MAX_DPB_SIZE_H265];

        for i in 0..self.num_poc_lt_curr as usize {
            if ref_pic_set.lt_curr[i] >= 0 {
                in_use[ref_pic_set.lt_curr[i] as usize] = true;
            }
        }
        for i in 0..self.num_poc_lt_foll as usize {
            if ref_pic_set.lt_foll[i] >= 0 {
                in_use[ref_pic_set.lt_foll[i] as usize] = true;
            }
        }
        for i in 0..self.num_poc_st_curr_before as usize {
            if ref_pic_set.st_curr_before[i] >= 0 {
                in_use[ref_pic_set.st_curr_before[i] as usize] = true;
            }
        }
        for i in 0..self.num_poc_st_curr_after as usize {
            if ref_pic_set.st_curr_after[i] >= 0 {
                in_use[ref_pic_set.st_curr_after[i] as usize] = true;
            }
        }
        for i in 0..self.num_poc_st_foll as usize {
            if ref_pic_set.st_foll[i] >= 0 {
                in_use[ref_pic_set.st_foll[i] as usize] = true;
            }
        }

        for i in 0..self.dpb_size as usize {
            if !in_use[i] && self.dpb[i].marking != 0 {
                tracing::debug!(
                    slot = i,
                    poc = self.dpb[i].pic_order_cnt_val,
                    old_marking = self.dpb[i].marking,
                    "  apply_rps: marking DPB entry as UNUSED (not in any RefPicSet)"
                );
                self.dpb[i].marking = 0;
            }
        }

        tracing::debug!("H265 DPB apply_reference_picture_set END");
        ref_pic_set
    }

    // -----------------------------------------------------------------------
    // SetupReferencePictureListLx (port of C++ SetupReferencePictureListLx)
    // -----------------------------------------------------------------------

    /// Builds RefPicList0/1 from the derived RefPicSet.
    /// Port of C++ SetupReferencePictureListLx (line 521-611).
    ///
    /// For IPP-only (no B-frames, no long-term), this populates RefPicList0
    /// from stCurrBefore entries.
    pub fn setup_reference_picture_list_lx(
        &mut self,
        pic_type: u32,
        ref_pic_set: &RefPicSetH265,
        num_ref_l0: u32,
        num_ref_l1: u32,
    ) -> SetupRefListResult {
        tracing::debug!(
            pic_type = pic_type,
            num_ref_l0 = num_ref_l0,
            num_ref_l1 = num_ref_l1,
            num_poc_st_curr_before = self.num_poc_st_curr_before,
            num_poc_st_curr_after = self.num_poc_st_curr_after,
            num_poc_lt_curr = self.num_poc_lt_curr,
            "H265 DPB setup_reference_picture_list_lx BEGIN"
        );

        // Log input RefPicSet
        for i in 0..MAX_NUM_LIST_REF_H265 {
            if ref_pic_set.st_curr_before[i] >= 0 {
                tracing::debug!(
                    idx = i,
                    dpb_slot = ref_pic_set.st_curr_before[i],
                    "  input stCurrBefore"
                );
            }
        }
        for i in 0..MAX_NUM_LIST_REF_H265 {
            if ref_pic_set.st_curr_after[i] >= 0 {
                tracing::debug!(
                    idx = i,
                    dpb_slot = ref_pic_set.st_curr_after[i],
                    "  input stCurrAfter"
                );
            }
        }

        let mut result = SetupRefListResult::default();

        let num_poc_total_curr: u8 = (self.num_poc_st_curr_before
            + self.num_poc_st_curr_after
            + self.num_poc_lt_curr) as u8;

        debug_assert!(num_poc_total_curr <= 8);

        result.num_ref_idx_l0_active_minus1 =
            if num_ref_l0 > 0 { (num_ref_l0 - 1) as u8 } else { 0 };
        result.num_ref_idx_l1_active_minus1 =
            if num_ref_l1 > 0 { (num_ref_l1 - 1) as u8 } else { 0 };

        self.long_term_flags = 0;

        if self.use_multiple_refs {
            if (result.num_ref_idx_l0_active_minus1 as i8 + 1) > self.num_poc_st_curr_before {
                result.num_ref_idx_l0_active_minus1 = if (self.num_poc_st_curr_before - 1) >= 0 {
                    (self.num_poc_st_curr_before - 1) as u8
                } else {
                    0
                };
            }
            if pic_type == PIC_TYPE_B
                && (result.num_ref_idx_l1_active_minus1 as i8 + 1) > self.num_poc_st_curr_after
            {
                result.num_ref_idx_l1_active_minus1 = if (self.num_poc_st_curr_after - 1) >= 0 {
                    (self.num_poc_st_curr_after - 1) as u8
                } else {
                    0
                };
            }
        }

        // Build RefPicList0 for P and B frames
        if pic_type == PIC_TYPE_P || pic_type == PIC_TYPE_B {
            let n_num_rps_curr_temp_list0 = cmp::max(
                result.num_ref_idx_l0_active_minus1 + 1,
                num_poc_total_curr,
            );
            debug_assert!(n_num_rps_curr_temp_list0 as usize <= MAX_NUM_LIST_REF_H265);

            let mut ref_pic_list_temp0 = [0i8; MAX_NUM_LIST_REF_H265];
            let mut is_long_term = [0i32; 32];
            let mut r_idx: u8 = 0;

            while r_idx < n_num_rps_curr_temp_list0 {
                for i in 0..self.num_poc_st_curr_before as usize {
                    if r_idx >= n_num_rps_curr_temp_list0 {
                        break;
                    }
                    ref_pic_list_temp0[r_idx as usize] = ref_pic_set.st_curr_before[i];
                    is_long_term[r_idx as usize] = 0;
                    r_idx += 1;
                }
                for i in 0..self.num_poc_st_curr_after as usize {
                    if r_idx >= n_num_rps_curr_temp_list0 {
                        break;
                    }
                    ref_pic_list_temp0[r_idx as usize] = ref_pic_set.st_curr_after[i];
                    is_long_term[r_idx as usize] = 0;
                    r_idx += 1;
                }
                for i in 0..self.num_poc_lt_curr as usize {
                    if r_idx >= n_num_rps_curr_temp_list0 {
                        break;
                    }
                    ref_pic_list_temp0[r_idx as usize] = ref_pic_set.lt_curr[i];
                    is_long_term[r_idx as usize] = 1;
                    r_idx += 1;
                }
                // If all lists are empty, break to avoid infinite loop
                if self.num_poc_st_curr_before == 0
                    && self.num_poc_st_curr_after == 0
                    && self.num_poc_lt_curr == 0
                {
                    break;
                }
            }

            for r_idx in 0..=result.num_ref_idx_l0_active_minus1 as usize {
                // No ref_pic_list_modification for now (flag is 0)
                result.ref_pic_list0[r_idx] = ref_pic_list_temp0[r_idx] as u8;
                self.long_term_flags |= (is_long_term[r_idx] as u32) << r_idx;
            }
        }

        // Build RefPicList1 for B frames
        if pic_type == PIC_TYPE_B {
            let num_poc_total_curr: u8 = (self.num_poc_st_curr_before
                + self.num_poc_st_curr_after
                + self.num_poc_lt_curr) as u8;
            let n_num_rps_curr_temp_list1 = cmp::max(
                result.num_ref_idx_l1_active_minus1 + 1,
                num_poc_total_curr,
            );
            debug_assert!(n_num_rps_curr_temp_list1 as usize <= MAX_NUM_LIST_REF_H265);

            let mut ref_pic_list_temp1 = [0i8; MAX_NUM_LIST_REF_H265];
            let mut is_long_term = [0i32; 32];
            let mut r_idx: u8 = 0;

            while r_idx < n_num_rps_curr_temp_list1 {
                for i in 0..self.num_poc_st_curr_after as usize {
                    if r_idx >= n_num_rps_curr_temp_list1 {
                        break;
                    }
                    ref_pic_list_temp1[r_idx as usize] = ref_pic_set.st_curr_after[i];
                    is_long_term[16 + r_idx as usize] = 0;
                    r_idx += 1;
                }
                for i in 0..self.num_poc_st_curr_before as usize {
                    if r_idx >= n_num_rps_curr_temp_list1 {
                        break;
                    }
                    ref_pic_list_temp1[r_idx as usize] = ref_pic_set.st_curr_before[i];
                    is_long_term[16 + r_idx as usize] = 0;
                    r_idx += 1;
                }
                for i in 0..self.num_poc_lt_curr as usize {
                    if r_idx >= n_num_rps_curr_temp_list1 {
                        break;
                    }
                    ref_pic_list_temp1[r_idx as usize] = ref_pic_set.lt_curr[i];
                    is_long_term[16 + r_idx as usize] = 1;
                    r_idx += 1;
                }
                if self.num_poc_st_curr_before == 0
                    && self.num_poc_st_curr_after == 0
                    && self.num_poc_lt_curr == 0
                {
                    break;
                }
            }

            for r_idx in 0..=result.num_ref_idx_l1_active_minus1 as usize {
                result.ref_pic_list1[r_idx] = ref_pic_list_temp1[r_idx] as u8;
                self.long_term_flags |=
                    (is_long_term[16 + r_idx] as u32) << (16 + r_idx);
            }
        }

        // Log output RefPicList0/1
        tracing::debug!(
            num_ref_idx_l0_active_minus1 = result.num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1 = result.num_ref_idx_l1_active_minus1,
            "H265 DPB setup_reference_picture_list_lx RESULT"
        );
        for i in 0..=result.num_ref_idx_l0_active_minus1 as usize {
            tracing::debug!(
                idx = i,
                ref_pic_list0_slot = result.ref_pic_list0[i],
                "  RefPicList0 entry"
            );
        }
        if result.num_ref_idx_l0_active_minus1 == 0 && result.ref_pic_list0[0] == NO_REFERENCE_PICTURE_H265 {
            tracing::warn!("RefPicList0 is EMPTY for a P/B frame -- this will cause encode errors");
        }
        for i in 0..=result.num_ref_idx_l1_active_minus1 as usize {
            if result.ref_pic_list1[i] != NO_REFERENCE_PICTURE_H265 {
                tracing::debug!(
                    idx = i,
                    ref_pic_list1_slot = result.ref_pic_list1[i],
                    "  RefPicList1 entry"
                );
            }
        }

        result
    }

    // -----------------------------------------------------------------------
    // DpbPictureEnd
    // -----------------------------------------------------------------------

    /// End picture processing.
    pub fn dpb_picture_end(&mut self, num_temporal_layers: u32, is_reference: bool) {
        tracing::debug!(
            cur_dpb_index = self.cur_dpb_index,
            num_temporal_layers = num_temporal_layers,
            is_reference = is_reference,
            "H265 DPB dpb_picture_end BEGIN"
        );

        if num_temporal_layers > 1 {
            for i in 0..self.dpb_size as usize {
                if self.dpb[i].state == 1
                    && self.dpb[i].marking != 0
                    && self.dpb[i].temporal_id == self.dpb[self.cur_dpb_index as usize].temporal_id
                {
                    self.dpb[i].marking = 0;
                }
            }
        }

        let idx = self.cur_dpb_index as usize;
        self.dpb[idx].state = 1;
        let marking_value = if is_reference { 1 } else { 0 };
        self.dpb[idx].marking = marking_value;

        tracing::debug!(
            slot = idx,
            state = self.dpb[idx].state,
            marking = marking_value,
            poc = self.dpb[idx].pic_order_cnt_val,
            "H265 DPB dpb_picture_end: slot committed"
        );

        // Log full DPB state after picture end
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 {
                tracing::debug!(
                    slot = i,
                    state = self.dpb[i].state,
                    marking = self.dpb[i].marking,
                    poc = self.dpb[i].pic_order_cnt_val,
                    "  DPB state after dpb_picture_end"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Returns the DPB size (number of slots).
    pub fn get_dpb_size(&self) -> i8 {
        self.dpb_size
    }

    /// Get a reference picture from the DPB.
    pub fn get_ref_picture(&self, dpb_index: i8) -> bool {
        if dpb_index >= 0 && (dpb_index as usize) < MAX_DPB_SIZE_H265 {
            return self.dpb[dpb_index as usize].state == 1;
        }
        false
    }

    /// Fill standard reference info for a DPB slot.
    pub fn fill_std_reference_info(
        &self,
        dpb_index: u8,
        poc: &mut u32,
        temporal_id: &mut i32,
        unused_for_ref: &mut bool,
    ) {
        debug_assert!((dpb_index as usize) < MAX_DPB_SIZE_H265);
        let entry = &self.dpb[dpb_index as usize];
        *unused_for_ref = entry.marking == 0;
        *poc = entry.pic_order_cnt_val;
        *temporal_id = entry.temporal_id;
    }

    /// Get dirty intra-refresh regions.
    pub fn get_dirty_intra_refresh_regions(&self, dpb_idx: i32) -> u32 {
        if dpb_idx >= 0 && (dpb_idx as usize) < MAX_DPB_SIZE_H265 {
            return self.dpb[dpb_idx as usize].dirty_intra_refresh_regions;
        }
        0
    }

    /// Set dirty intra-refresh regions for the current picture.
    pub fn set_cur_dirty_intra_refresh_regions(&mut self, regions: u32) {
        let idx = self.cur_dpb_index as usize;
        if idx < MAX_DPB_SIZE_H265 {
            self.dpb[idx].dirty_intra_refresh_regions = regions;
        }
    }

    /// Returns the current DPB slot index.
    pub fn get_cur_dpb_index(&self) -> i8 {
        self.cur_dpb_index
    }

    // --- Private methods ---

    fn is_dpb_full(&self) -> bool {
        let mut count = 0;
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 {
                count += 1;
            }
        }
        count >= self.dpb_size as i32
    }

    fn is_dpb_empty(&self) -> bool {
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 {
                return false;
            }
        }
        true
    }

    fn flush_dpb(&mut self) {
        for i in 0..self.dpb_size as usize {
            self.dpb[i].marking = 0;
        }
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 && !self.dpb[i].output && self.dpb[i].marking == 0 {
                self.dpb[i].state = 0;
            }
        }
        while !self.is_dpb_empty() {
            self.dpb_bumping();
        }
    }

    fn dpb_bumping(&mut self) {
        let mut min_poc = u32::MAX;
        let mut min_idx: i32 = -1;

        // First try: bump an entry that needs output (decoder path)
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 && self.dpb[i].output {
                if min_idx < 0 || self.dpb[i].pic_order_cnt_val < min_poc {
                    min_poc = self.dpb[i].pic_order_cnt_val;
                    min_idx = i as i32;
                }
            }
        }

        if min_idx >= 0 {
            let idx = min_idx as usize;
            self.dpb[idx].output = false;
            if self.dpb[idx].marking == 0 {
                self.dpb[idx].state = 0;
            }
            return;
        }

        // Encoder sliding window: if no output-pending entries, evict the
        // oldest short-term reference to make room for new pictures.
        min_poc = u32::MAX;
        min_idx = -1;
        for i in 0..self.dpb_size as usize {
            if self.dpb[i].state == 1 && self.dpb[i].marking == 1 {
                if min_idx < 0 || self.dpb[i].pic_order_cnt_val < min_poc {
                    min_poc = self.dpb[i].pic_order_cnt_val;
                    min_idx = i as i32;
                }
            }
        }

        if min_idx >= 0 {
            let idx = min_idx as usize;
            self.dpb[idx].marking = 0;
            self.dpb[idx].state = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
            1,    // pic_order_cnt_val
            0,    // temporal_id
            false, // not IRAP
            1,    // num_ref_l0
            0,    // num_ref_l1
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
        assert_eq!(result.short_term_ref_pic_set.used_by_curr_pic_s0_flag & 1, 1);
    }

    #[test]
    fn test_dpb_picture_start_with_rps_idr() {
        let mut dpb = VkEncDpbH265::new();
        dpb.dpb_sequence_start(4, false);

        let empty_strps = ShortTermRefPicSet::default();
        let (slot, ref_pic_set) = dpb.dpb_picture_start_with_rps(
            0,       // frame_id
            0,       // pic_order_cnt_val
            PIC_TYPE_IDR,
            true,    // is_irap
            true,    // is_idr
            true,    // pic_output_flag
            0,       // temporal_id
            false,   // no_output_of_prior_pics_flag
            0,       // time_stamp
            &empty_strps,
            256,     // max_pic_order_cnt_lsb
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
        let _rps_result = dpb.initialize_rps(
            &[], PIC_TYPE_IDR, 0, 0, true, 0, 0,
        );
        let empty_strps = ShortTermRefPicSet::default();
        let (slot0, _rps0) = dpb.dpb_picture_start_with_rps(
            0, 0, PIC_TYPE_IDR, true, true, true, 0, false, 0,
            &empty_strps, 256,
        );
        assert!(slot0 >= 0);
        dpb.dpb_picture_end(1, true);

        // --- P frame (POC=1) ---
        dpb.reference_picture_marking(1, PIC_TYPE_P, false);
        let rps_result1 = dpb.initialize_rps(
            &[], PIC_TYPE_P, 1, 0, false, 1, 0,
        );
        assert_eq!(rps_result1.short_term_ref_pic_set.num_negative_pics, 1);

        let (slot1, rps1) = dpb.dpb_picture_start_with_rps(
            1, 1, PIC_TYPE_P, false, false, true, 0, false, 1,
            &rps_result1.short_term_ref_pic_set, 256,
        );
        assert!(slot1 >= 0);

        // stCurrBefore should contain the DPB index of POC=0
        assert!(rps1.st_curr_before[0] >= 0);
        assert_eq!(rps1.st_curr_before[0], slot0);

        let ref_lists = dpb.setup_reference_picture_list_lx(
            PIC_TYPE_P, &rps1, 1, 0,
        );
        assert_eq!(ref_lists.num_ref_idx_l0_active_minus1, 0);
        assert_eq!(ref_lists.ref_pic_list0[0], slot0 as u8);

        dpb.dpb_picture_end(1, true);

        // --- P frame (POC=2) ---
        dpb.reference_picture_marking(2, PIC_TYPE_P, false);
        let rps_result2 = dpb.initialize_rps(
            &[], PIC_TYPE_P, 2, 0, false, 1, 0,
        );
        // Both POC=0 and POC=1 are valid negative refs; with dpb_size=4,
        // both fit. Only 1 is used_by_curr_pic though.
        assert!(rps_result2.short_term_ref_pic_set.num_negative_pics >= 1);

        let (slot2, rps2) = dpb.dpb_picture_start_with_rps(
            2, 2, PIC_TYPE_P, false, false, true, 0, false, 2,
            &rps_result2.short_term_ref_pic_set, 256,
        );
        assert!(slot2 >= 0);

        // stCurrBefore should reference POC=1 (most recent)
        assert!(rps2.st_curr_before[0] >= 0);
        assert_eq!(rps2.st_curr_before[0], slot1);

        let ref_lists2 = dpb.setup_reference_picture_list_lx(
            PIC_TYPE_P, &rps2, 1, 0,
        );
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
            1, 1, PIC_TYPE_P, false, false, true, 0, false, 1,
            &rps_result.short_term_ref_pic_set, 256,
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

        let ref_pic_set = dpb.apply_reference_picture_set(
            2, PIC_TYPE_P, false, &strps, 256,
        );

        // POC=1 should be in stCurrBefore, POC=0 should be marked unused
        assert!(ref_pic_set.st_curr_before[0] >= 0);
        // Entry at POC=0 should have marking=0 (not in RPS)
        assert_eq!(dpb.dpb[0].marking, 0);
        // Entry at POC=1 should still have marking=1
        assert_eq!(dpb.dpb[1].marking, 1);
    }
}
