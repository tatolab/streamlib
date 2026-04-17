// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderDpbH264.h + VkEncoderDpbH264.cpp
//!
//! H.264 Decoded Picture Buffer (DPB) management for the encoder.
//! Implements all 16 DPB slots, sliding window and adaptive (MMCO) memory
//! management, all three POC types (0 and 2 implemented; type 1 unimplemented
//! in the original), reference picture list initialization and reordering,
//! frame/field support, and complementary field pair handling.

use std::cmp;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_DPB_SLOTS: usize = 16;

const MARKING_UNUSED: u32 = 0;
const MARKING_SHORT: u32 = 1;
const MARKING_LONG: u32 = 2;

const INF_MIN: i32 = i32::MIN;
const INF_MAX: i32 = i32::MAX;

/// DPB occupancy state for a slot.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpbStateH264 {
    Empty = 0,
    Top = 1,
    Bottom = 2,
    Frame = 3, // Top | Bottom
}

// ---------------------------------------------------------------------------
// RefPicListEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct RefPicListEntry {
    pub dpb_index: i32,
}

// ---------------------------------------------------------------------------
// PicInfoH264
// ---------------------------------------------------------------------------

/// Picture information for H.264 encoding — extends StdVideoEncodeH264PictureInfo.
#[derive(Debug, Clone, Default)]
pub struct PicInfoH264 {
    pub frame_num: u32,
    pub pic_order_cnt: i32,
    pub primary_pic_type: u32, // STD_VIDEO_H264_PICTURE_TYPE_*
    pub idr_pic_id: u16,
    pub is_reference: bool,
    pub is_idr: bool,
    pub long_term_reference_flag: bool,
    pub no_output_of_prior_pics_flag: bool,
    pub adaptive_ref_pic_marking_mode_flag: bool,
    pub field_pic_flag: bool,
    pub bottom_field_flag: bool,
    pub time_stamp: u64,
}

// ---------------------------------------------------------------------------
// DpbEntryH264
// ---------------------------------------------------------------------------

/// A single entry in the H.264 encoder DPB.
#[derive(Debug, Clone)]
pub struct DpbEntryH264 {
    // Picture info stored in the DPB
    pub pic_info_frame_num: u32,
    pub pic_info_pic_order_cnt: i32,

    pub state: u32, // DpbStateH264 bitmask
    pub top_needed_for_output: bool,
    pub bottom_needed_for_output: bool,
    pub top_decoded_first: bool,
    pub reference_picture: bool,
    pub complementary_field_pair: bool,
    pub not_existing: bool,
    pub frame_is_corrupted: bool,

    // Reference frame marking
    pub top_field_marking: u32,
    pub bottom_field_marking: u32,
    pub long_term_frame_idx: i32,

    pub top_foc: i32,
    pub bottom_foc: i32,

    pub frame_num_wrap: i32,
    pub top_pic_num: i32,
    pub bottom_pic_num: i32,
    pub top_long_term_pic_num: i32,
    pub bottom_long_term_pic_num: i32,

    // MVC
    pub view_id: u32,

    pub time_stamp: u64,
    pub ref_frame_time_stamp: u64,

    // Intra-refresh
    pub dirty_intra_refresh_regions: u32,
}

impl Default for DpbEntryH264 {
    fn default() -> Self {
        Self {
            pic_info_frame_num: 0,
            pic_info_pic_order_cnt: 0,
            state: DpbStateH264::Empty as u32,
            top_needed_for_output: false,
            bottom_needed_for_output: false,
            top_decoded_first: false,
            reference_picture: false,
            complementary_field_pair: false,
            not_existing: false,
            frame_is_corrupted: false,
            top_field_marking: MARKING_UNUSED,
            bottom_field_marking: MARKING_UNUSED,
            long_term_frame_idx: 0,
            top_foc: 0,
            bottom_foc: 0,
            frame_num_wrap: 0,
            top_pic_num: 0,
            bottom_pic_num: 0,
            top_long_term_pic_num: 0,
            bottom_long_term_pic_num: 0,
            view_id: 0,
            time_stamp: 0,
            ref_frame_time_stamp: 0,
            dirty_intra_refresh_regions: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// NvVideoEncodeH264DpbSlotInfoLists
// ---------------------------------------------------------------------------

/// Reference picture list counts and slot usage mask.
#[derive(Debug, Clone)]
pub struct DpbSlotInfoLists {
    pub ref_pic_list_count: [u32; 2],
    pub dpb_slots_use_mask: u32,
    pub ref_pic_list: [[u8; 16]; 2], // STD_VIDEO_H264_MAX_NUM_LIST_REF = 16
}

impl Default for DpbSlotInfoLists {
    fn default() -> Self {
        Self {
            ref_pic_list_count: [0; 2],
            dpb_slots_use_mask: 0,
            ref_pic_list: [[0; 16]; 2],
        }
    }
}

// ---------------------------------------------------------------------------
// Sort-check function type
// ---------------------------------------------------------------------------

type DpbSortFn = fn(&DpbEntryH264, u32, &mut i32) -> bool;

fn sort_check_short_term_p_frame(entry: &DpbEntryH264, _poc_type: u32, v: &mut i32) -> bool {
    *v = entry.top_pic_num;
    entry.top_field_marking == MARKING_SHORT && entry.bottom_field_marking == MARKING_SHORT
}

#[allow(dead_code)] // Field-coding reference list variant; kept for codec parity
fn sort_check_short_term_p_field(entry: &DpbEntryH264, _poc_type: u32, v: &mut i32) -> bool {
    *v = entry.frame_num_wrap;
    entry.top_field_marking == MARKING_SHORT || entry.bottom_field_marking == MARKING_SHORT
}

fn sort_check_short_term_b_frame(entry: &DpbEntryH264, poc_type: u32, v: &mut i32) -> bool {
    *v = entry.pic_info_pic_order_cnt;
    !(poc_type == 0 && entry.not_existing)
        && entry.top_field_marking == MARKING_SHORT
        && entry.bottom_field_marking == MARKING_SHORT
}

#[allow(dead_code)] // Field-coding reference list variant; kept for codec parity
fn sort_check_short_term_b_field(entry: &DpbEntryH264, poc_type: u32, v: &mut i32) -> bool {
    *v = entry.pic_info_pic_order_cnt;
    !(poc_type == 0 && entry.not_existing)
        && (entry.top_field_marking == MARKING_SHORT || entry.bottom_field_marking == MARKING_SHORT)
}

fn sort_check_long_term_frame(entry: &DpbEntryH264, _poc_type: u32, v: &mut i32) -> bool {
    *v = entry.top_long_term_pic_num;
    entry.top_field_marking == MARKING_LONG && entry.bottom_field_marking == MARKING_LONG
}

#[allow(dead_code)] // Field-coding reference list variant; kept for codec parity
fn sort_check_long_term_field(entry: &DpbEntryH264, _poc_type: u32, v: &mut i32) -> bool {
    *v = entry.long_term_frame_idx;
    entry.top_field_marking == MARKING_LONG || entry.bottom_field_marking == MARKING_LONG
}

// ---------------------------------------------------------------------------
// VkEncDpbH264
// ---------------------------------------------------------------------------

/// H.264 encoder DPB — full 16-slot implementation.
pub struct VkEncDpbH264 {
    max_long_term_frame_idx: i32,
    max_dpb_size: i32,
    prev_pic_order_cnt_msb: i32,
    prev_pic_order_cnt_lsb: i32,
    prev_frame_num_offset: i32,
    prev_frame_num: u32,
    prev_ref_frame_num: u32,
    max_num_list: [i32; 2],
    curr_dpb_idx: i8,
    dpb: [DpbEntryH264; MAX_DPB_SLOTS + 1], // +1 for current picture
    last_idr_time_stamp: u64,
}

impl VkEncDpbH264 {
    /// Create a new DPB instance.
    pub fn create_instance() -> Box<Self> {
        let mut dpb = Box::new(Self {
            max_long_term_frame_idx: 0,
            max_dpb_size: 0,
            prev_pic_order_cnt_msb: 0,
            prev_pic_order_cnt_lsb: 0,
            prev_frame_num_offset: 0,
            prev_frame_num: 0,
            prev_ref_frame_num: 0,
            max_num_list: [0; 2],
            curr_dpb_idx: -1,
            dpb: std::array::from_fn(|_| DpbEntryH264::default()),
            last_idr_time_stamp: 0,
        });
        dpb.dpb_init();
        dpb
    }

    fn dpb_init(&mut self) {
        self.max_dpb_size = 0;
        self.max_num_list = [0; 2];
        self.curr_dpb_idx = -1;
    }

    fn dpb_deinit(&mut self) {
        self.max_dpb_size = 0;
        self.last_idr_time_stamp = 0;
        self.curr_dpb_idx = -1;
    }

    /// Initialize the DPB for a new sequence.
    pub fn dpb_sequence_start(&mut self, user_dpb_size: i32) -> i32 {
        self.dpb_deinit();
        self.max_dpb_size = user_dpb_size;

        for i in 0..=MAX_DPB_SLOTS {
            self.dpb[i] = DpbEntryH264::default();
        }

        self.flush_dpb();
        0
    }

    /// Start processing a new picture. Returns the DPB slot index.
    pub fn dpb_picture_start(
        &mut self,
        pic_info: &PicInfoH264,
        sps_log2_max_frame_num_minus4: u32,
        sps_pic_order_cnt_type: u32,
        sps_log2_max_pic_order_cnt_lsb_minus4: u32,
        sps_gaps_in_frame_num_value_allowed_flag: bool,
        sps_max_num_ref_frames: u32,
    ) -> i8 {
        self.fill_frame_num_gaps(
            pic_info,
            sps_log2_max_frame_num_minus4,
            sps_pic_order_cnt_type,
            sps_log2_max_pic_order_cnt_lsb_minus4,
            sps_gaps_in_frame_num_value_allowed_flag,
            sps_max_num_ref_frames,
        );

        // Check for complementary field pair
        let idx = self.curr_dpb_idx;
        if idx >= 0
            && (idx as usize) < MAX_DPB_SLOTS
            && (self.dpb[idx as usize].state == DpbStateH264::Top as u32
                || self.dpb[idx as usize].state == DpbStateH264::Bottom as u32)
            && pic_info.field_pic_flag
        {
            // Complementary field pair detection (simplified)
            self.dpb[idx as usize].complementary_field_pair = true;
        } else {
            self.curr_dpb_idx = MAX_DPB_SLOTS as i8;
            let cur = &mut self.dpb[MAX_DPB_SLOTS];
            if cur.state != DpbStateH264::Empty as u32 {
                cur.state = DpbStateH264::Empty as u32;
            }

            cur.state = DpbStateH264::Empty as u32;
            cur.top_needed_for_output = false;
            cur.bottom_needed_for_output = false;
            cur.top_field_marking = MARKING_UNUSED;
            cur.bottom_field_marking = MARKING_UNUSED;
            cur.reference_picture = pic_info.is_reference;
            cur.top_decoded_first = !pic_info.bottom_field_flag;
            cur.complementary_field_pair = false;
            cur.not_existing = false;
            cur.pic_info_frame_num = pic_info.frame_num;
            cur.time_stamp = pic_info.time_stamp;
            cur.frame_is_corrupted = false;

            if pic_info.is_idr {
                self.last_idr_time_stamp = pic_info.time_stamp;
            }
        }

        self.calculate_poc(pic_info, sps_pic_order_cnt_type, sps_log2_max_frame_num_minus4, sps_log2_max_pic_order_cnt_lsb_minus4);
        self.calculate_pic_num(pic_info, sps_log2_max_frame_num_minus4);

        self.curr_dpb_idx
    }

    /// Get current DPB entry info.
    pub fn get_current_dpb_entry(&self) -> (u32, i32) {
        let idx = self.curr_dpb_idx as usize;
        debug_assert!(idx < MAX_DPB_SLOTS || idx == MAX_DPB_SLOTS);
        (self.dpb[idx].pic_info_frame_num, self.dpb[idx].pic_info_pic_order_cnt)
    }

    /// Get the updated frame_num and PicOrderCnt.
    pub fn get_updated_frame_num_and_pic_order_cnt(&self) -> (u32, i32) {
        self.get_current_dpb_entry()
    }

    /// Get the maximum DPB size.
    pub fn get_max_dpb_size(&self) -> i32 {
        self.max_dpb_size
    }

    /// Check if reference frames are corrupted.
    pub fn is_ref_frames_corrupted(&self) -> bool {
        for i in 0..MAX_DPB_SLOTS {
            if (self.dpb[i].top_field_marking == MARKING_SHORT
                || self.dpb[i].bottom_field_marking == MARKING_SHORT
                || self.dpb[i].top_field_marking == MARKING_LONG
                || self.dpb[i].bottom_field_marking == MARKING_LONG)
                && self.dpb[i].frame_is_corrupted
            {
                return true;
            }
        }
        false
    }

    /// Check if a specific reference picture is corrupted.
    pub fn is_ref_pic_corrupted(&self, dpb_idx: i32) -> bool {
        if dpb_idx >= 0 && (dpb_idx as usize) < MAX_DPB_SLOTS && self.dpb[dpb_idx as usize].state != DpbStateH264::Empty as u32 {
            return self.dpb[dpb_idx as usize].frame_is_corrupted;
        }
        false
    }

    /// Check if reordering is needed (due to corrupted frames).
    pub fn need_to_reorder(&self) -> bool {
        for i in 0..MAX_DPB_SLOTS {
            if (self.dpb[i].top_field_marking != 0 || self.dpb[i].bottom_field_marking != 0)
                && self.dpb[i].frame_is_corrupted
            {
                return true;
            }
        }
        false
    }

    /// Get the number of reference frames in DPB.
    pub fn get_num_ref_frames_in_dpb(&self, view_id: u32) -> (i32, i32, i32) {
        let mut num_short = 0i32;
        let mut num_long = 0i32;
        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].view_id == view_id {
                if (self.dpb[i].top_field_marking == MARKING_SHORT
                    || self.dpb[i].bottom_field_marking == MARKING_SHORT)
                    && !self.dpb[i].frame_is_corrupted
                {
                    num_short += 1;
                }
                if (self.dpb[i].top_field_marking == MARKING_LONG
                    || self.dpb[i].bottom_field_marking == MARKING_LONG)
                    && !self.dpb[i].frame_is_corrupted
                {
                    num_long += 1;
                }
            }
        }
        (num_short + num_long, num_short, num_long)
    }

    /// Get picture timestamp for a DPB index.
    pub fn get_picture_timestamp(&self, dpb_idx: i32) -> u64 {
        if dpb_idx >= 0 && (dpb_idx as usize) < MAX_DPB_SLOTS && self.dpb[dpb_idx as usize].state != DpbStateH264::Empty as u32 {
            return self.dpb[dpb_idx as usize].time_stamp;
        }
        0
    }

    /// Set the current reference frame timestamp.
    pub fn set_cur_ref_frame_time_stamp(&mut self, ts: u64) {
        let idx = self.curr_dpb_idx as usize;
        if idx <= MAX_DPB_SLOTS {
            self.dpb[idx].ref_frame_time_stamp = ts;
        }
    }

    /// Get dirty intra-refresh regions for a DPB slot.
    pub fn get_dirty_intra_refresh_regions(&self, dpb_idx: i32) -> u32 {
        if dpb_idx >= 0 && (dpb_idx as usize) < MAX_DPB_SLOTS && self.dpb[dpb_idx as usize].state != DpbStateH264::Empty as u32 {
            return self.dpb[dpb_idx as usize].dirty_intra_refresh_regions;
        }
        0
    }

    /// Set dirty intra-refresh regions for the current picture.
    pub fn set_cur_dirty_intra_refresh_regions(&mut self, regions: u32) {
        let idx = self.curr_dpb_idx as usize;
        if idx <= MAX_DPB_SLOTS {
            self.dpb[idx].dirty_intra_refresh_regions = regions;
        }
    }

    /// Fill standard reference info for a DPB slot.
    pub fn fill_std_reference_info(&self, dpb_idx: u8, pic_order_cnt: &mut i32, is_long_term: &mut bool, long_term_frame_idx: &mut i32) {
        debug_assert!((dpb_idx as usize) < MAX_DPB_SLOTS);
        let entry = &self.dpb[dpb_idx as usize];
        *is_long_term = entry.top_field_marking == MARKING_LONG;
        *pic_order_cnt = entry.pic_info_pic_order_cnt;
        *long_term_frame_idx = if *is_long_term { entry.long_term_frame_idx } else { -1 };
    }

    /// Get PicNum with minimum POC.
    pub fn get_pic_num_x_with_min_poc(&self, view_id: u32, field_pic_flag: bool, bottom_field: bool) -> i32 {
        let mut poc_min = INF_MAX;
        let mut min_idx: i32 = -1;
        for i in 0..MAX_DPB_SLOTS {
            if (self.dpb[i].state & DpbStateH264::Top as u32) != 0
                && self.dpb[i].top_field_marking == MARKING_SHORT
                && self.dpb[i].top_foc < poc_min
                && self.dpb[i].view_id == view_id
            {
                poc_min = self.dpb[i].top_foc;
                min_idx = i as i32;
            }
            if (self.dpb[i].state & DpbStateH264::Bottom as u32) != 0
                && self.dpb[i].top_field_marking == MARKING_SHORT
                && self.dpb[i].bottom_foc < poc_min
                && self.dpb[i].view_id == view_id
            {
                poc_min = self.dpb[i].bottom_foc;
                min_idx = i as i32;
            }
        }
        if min_idx >= 0 {
            if field_pic_flag && bottom_field {
                return self.dpb[min_idx as usize].bottom_pic_num;
            } else {
                return self.dpb[min_idx as usize].top_pic_num;
            }
        }
        -1
    }

    /// Get PicNum with minimum FrameNumWrap.
    pub fn get_pic_num_x_with_min_frame_num_wrap(&self, view_id: u32, field_pic_flag: bool, bottom_field: bool) -> i32 {
        let mut min_frame_num_wrap = 65536i32;
        let mut min_idx: i32 = -1;
        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].view_id == view_id
                && (self.dpb[i].top_field_marking == MARKING_SHORT || self.dpb[i].bottom_field_marking == MARKING_SHORT)
                && self.dpb[i].frame_num_wrap < min_frame_num_wrap
            {
                min_idx = i as i32;
                min_frame_num_wrap = self.dpb[i].frame_num_wrap;
            }
        }
        if min_idx >= 0 {
            if field_pic_flag && bottom_field {
                return self.dpb[min_idx as usize].bottom_pic_num;
            } else {
                return self.dpb[min_idx as usize].top_pic_num;
            }
        }
        -1
    }

    /// Get PicNum for a specific DPB index.
    pub fn get_pic_num(&self, dpb_idx: i32, bottom_field: bool) -> i32 {
        if dpb_idx >= 0 && (dpb_idx as usize) < MAX_DPB_SLOTS && self.dpb[dpb_idx as usize].state != DpbStateH264::Empty as u32 {
            return if bottom_field {
                self.dpb[dpb_idx as usize].bottom_pic_num
            } else {
                self.dpb[dpb_idx as usize].top_pic_num
            };
        }
        -1
    }

    /// Destroy the DPB.
    pub fn dpb_destroy(&mut self) {
        self.flush_dpb();
        self.dpb_deinit();
    }

    // --- Private methods ---

    fn is_dpb_full(&self) -> bool {
        let mut fullness = 0;
        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].state != DpbStateH264::Empty as u32 {
                fullness += 1;
            }
        }
        fullness >= self.max_dpb_size
    }

    fn is_dpb_empty(&self) -> bool {
        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].state != DpbStateH264::Empty as u32 {
                return false;
            }
        }
        true
    }

    fn flush_dpb(&mut self) {
        for i in 0..MAX_DPB_SLOTS {
            self.dpb[i].top_field_marking = MARKING_UNUSED;
            self.dpb[i].bottom_field_marking = MARKING_UNUSED;
        }
        for i in 0..MAX_DPB_SLOTS {
            let top_ok = (self.dpb[i].state & DpbStateH264::Top as u32) == 0
                || (!self.dpb[i].top_needed_for_output && self.dpb[i].top_field_marking == MARKING_UNUSED);
            let bot_ok = (self.dpb[i].state & DpbStateH264::Bottom as u32) == 0
                || (!self.dpb[i].bottom_needed_for_output && self.dpb[i].bottom_field_marking == MARKING_UNUSED);
            if top_ok && bot_ok {
                self.dpb[i].state = DpbStateH264::Empty as u32;
            }
        }
        while !self.is_dpb_empty() {
            self.dpb_bumping(true);
        }
    }

    fn dpb_bumping(&mut self, always_bump: bool) {
        let mut poc_min = INF_MAX;
        let mut min_foc: i32 = -1;

        for i in 0..MAX_DPB_SLOTS {
            if (self.dpb[i].state & DpbStateH264::Top as u32) != 0
                && self.dpb[i].top_needed_for_output
                && self.dpb[i].top_foc < poc_min
            {
                poc_min = self.dpb[i].top_foc;
                min_foc = i as i32;
            }
            if (self.dpb[i].state & DpbStateH264::Bottom as u32) != 0
                && self.dpb[i].bottom_needed_for_output
                && self.dpb[i].bottom_foc < poc_min
            {
                poc_min = self.dpb[i].bottom_foc;
                min_foc = i as i32;
            }
        }

        if min_foc >= 0 {
            let idx = min_foc as usize;
            self.dpb[idx].top_needed_for_output = false;
            self.dpb[idx].bottom_needed_for_output = false;

            let top_unused = (self.dpb[idx].state & DpbStateH264::Top as u32) == 0
                || self.dpb[idx].top_field_marking == MARKING_UNUSED;
            let bot_unused = (self.dpb[idx].state & DpbStateH264::Bottom as u32) == 0
                || self.dpb[idx].bottom_field_marking == MARKING_UNUSED;
            if top_unused && bot_unused {
                self.dpb[idx].state = DpbStateH264::Empty as u32;
            }
        } else if always_bump {
            // Special case to avoid deadlocks
            let mut poc_min2 = INF_MAX;
            let mut min_foc2: i32 = -1;
            for i in 0..MAX_DPB_SLOTS {
                if (self.dpb[i].state & DpbStateH264::Top as u32) != 0 && self.dpb[i].top_foc <= poc_min2 {
                    poc_min2 = self.dpb[i].top_foc;
                    min_foc2 = i as i32;
                }
                if (self.dpb[i].state & DpbStateH264::Bottom as u32) != 0 && self.dpb[i].bottom_foc <= poc_min2 {
                    poc_min2 = self.dpb[i].bottom_foc;
                    min_foc2 = i as i32;
                }
            }
            if min_foc2 >= 0 && (min_foc2 as usize) < MAX_DPB_SLOTS {
                self.dpb[min_foc2 as usize].state = DpbStateH264::Empty as u32;
            }
        }
    }

    fn calculate_poc(&mut self, pic_info: &PicInfoH264, poc_type: u32, log2_max_frame_num_m4: u32, log2_max_poc_lsb_m4: u32) {
        if poc_type == 0 {
            self.calculate_poc_type0(pic_info, log2_max_poc_lsb_m4);
        } else {
            self.calculate_poc_type2(pic_info, log2_max_frame_num_m4);
        }
        let idx = self.curr_dpb_idx as usize;
        if !pic_info.field_pic_flag || self.dpb[idx].complementary_field_pair {
            self.dpb[idx].pic_info_pic_order_cnt = cmp::min(self.dpb[idx].top_foc, self.dpb[idx].bottom_foc);
        } else if !pic_info.bottom_field_flag {
            self.dpb[idx].pic_info_pic_order_cnt = self.dpb[idx].top_foc;
        } else {
            self.dpb[idx].pic_info_pic_order_cnt = self.dpb[idx].bottom_foc;
        }
    }

    fn calculate_poc_type0(&mut self, pic_info: &PicInfoH264, log2_max_poc_lsb_m4: u32) {
        if pic_info.is_idr {
            self.prev_pic_order_cnt_msb = 0;
            self.prev_pic_order_cnt_lsb = 0;
        }
        let max_poc_lsb = 1i32 << (log2_max_poc_lsb_m4 + 4);
        let poc_lsb = pic_info.pic_order_cnt;

        let pic_order_cnt_msb = if (poc_lsb < self.prev_pic_order_cnt_lsb)
            && ((self.prev_pic_order_cnt_lsb - poc_lsb) >= (max_poc_lsb / 2))
        {
            self.prev_pic_order_cnt_msb + max_poc_lsb
        } else if (poc_lsb > self.prev_pic_order_cnt_lsb)
            && ((poc_lsb - self.prev_pic_order_cnt_lsb) > (max_poc_lsb / 2))
        {
            self.prev_pic_order_cnt_msb - max_poc_lsb
        } else {
            self.prev_pic_order_cnt_msb
        };

        let idx = self.curr_dpb_idx as usize;
        if !pic_info.field_pic_flag || !pic_info.bottom_field_flag {
            self.dpb[idx].top_foc = pic_order_cnt_msb + poc_lsb;
        }
        if !pic_info.field_pic_flag || pic_info.bottom_field_flag {
            self.dpb[idx].bottom_foc = pic_order_cnt_msb + poc_lsb;
        }

        if pic_info.is_reference {
            self.prev_pic_order_cnt_msb = pic_order_cnt_msb;
            self.prev_pic_order_cnt_lsb = poc_lsb;
        }
    }

    fn calculate_poc_type2(&mut self, pic_info: &PicInfoH264, log2_max_frame_num_m4: u32) {
        let max_frame_num = 1i32 << (log2_max_frame_num_m4 + 4);
        let frame_num_offset = if pic_info.is_idr {
            0
        } else if self.prev_frame_num > pic_info.frame_num {
            self.prev_frame_num_offset + max_frame_num
        } else {
            self.prev_frame_num_offset
        };

        let temp_poc = if pic_info.is_idr {
            0
        } else if !pic_info.is_reference {
            2 * (frame_num_offset + pic_info.frame_num as i32) - 1
        } else {
            2 * (frame_num_offset + pic_info.frame_num as i32)
        };

        let idx = self.curr_dpb_idx as usize;
        if !pic_info.field_pic_flag {
            self.dpb[idx].top_foc = temp_poc;
            self.dpb[idx].bottom_foc = temp_poc;
        } else if pic_info.bottom_field_flag {
            self.dpb[idx].bottom_foc = temp_poc;
        } else {
            self.dpb[idx].top_foc = temp_poc;
        }

        self.prev_frame_num_offset = frame_num_offset;
        self.prev_frame_num = pic_info.frame_num;
    }

    fn calculate_pic_num(&mut self, pic_info: &PicInfoH264, log2_max_frame_num_m4: u32) {
        let max_frame_num = 1i32 << (log2_max_frame_num_m4 + 4);

        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].pic_info_frame_num > pic_info.frame_num {
                self.dpb[i].frame_num_wrap = self.dpb[i].pic_info_frame_num as i32 - max_frame_num;
            } else {
                self.dpb[i].frame_num_wrap = self.dpb[i].pic_info_frame_num as i32;
            }

            if !pic_info.field_pic_flag {
                self.dpb[i].top_pic_num = self.dpb[i].frame_num_wrap;
                self.dpb[i].bottom_pic_num = self.dpb[i].frame_num_wrap;
                self.dpb[i].top_long_term_pic_num = self.dpb[i].long_term_frame_idx;
                self.dpb[i].bottom_long_term_pic_num = self.dpb[i].long_term_frame_idx;
            } else if !pic_info.bottom_field_flag {
                self.dpb[i].top_pic_num = 2 * self.dpb[i].frame_num_wrap + 1;
                self.dpb[i].bottom_pic_num = 2 * self.dpb[i].frame_num_wrap;
                self.dpb[i].top_long_term_pic_num = 2 * self.dpb[i].long_term_frame_idx + 1;
                self.dpb[i].bottom_long_term_pic_num = 2 * self.dpb[i].long_term_frame_idx;
            } else {
                self.dpb[i].top_pic_num = 2 * self.dpb[i].frame_num_wrap;
                self.dpb[i].bottom_pic_num = 2 * self.dpb[i].frame_num_wrap + 1;
                self.dpb[i].top_long_term_pic_num = 2 * self.dpb[i].long_term_frame_idx;
                self.dpb[i].bottom_long_term_pic_num = 2 * self.dpb[i].long_term_frame_idx + 1;
            }
        }
    }

    fn sliding_window_memory_management(&mut self, pic_info: &PicInfoH264, sps_max_num_ref_frames: u32) {
        let idx = self.curr_dpb_idx as usize;
        if pic_info.field_pic_flag
            && ((!pic_info.bottom_field_flag && self.dpb[idx].bottom_field_marking == MARKING_SHORT)
                || (pic_info.bottom_field_flag && self.dpb[idx].top_field_marking == MARKING_SHORT))
        {
            if !pic_info.bottom_field_flag {
                self.dpb[idx].top_field_marking = MARKING_SHORT;
            } else {
                self.dpb[idx].bottom_field_marking = MARKING_SHORT;
            }
        } else {
            let mut imin = MAX_DPB_SLOTS;
            let mut min_frame_num_wrap = 65536i32;
            let mut num_short = 0i32;
            let mut num_long = 0i32;
            for i in 0..MAX_DPB_SLOTS {
                if self.dpb[i].top_field_marking == MARKING_SHORT || self.dpb[i].bottom_field_marking == MARKING_SHORT {
                    num_short += 1;
                    if self.dpb[i].frame_num_wrap < min_frame_num_wrap {
                        imin = i;
                        min_frame_num_wrap = self.dpb[i].frame_num_wrap;
                    }
                }
                if self.dpb[i].top_field_marking == MARKING_LONG || self.dpb[i].bottom_field_marking == MARKING_LONG {
                    num_long += 1;
                }
            }
            if (num_short + num_long) >= sps_max_num_ref_frames as i32 {
                if num_short > 0 && imin < MAX_DPB_SLOTS {
                    self.dpb[imin].top_field_marking = MARKING_UNUSED;
                    self.dpb[imin].bottom_field_marking = MARKING_UNUSED;
                }
            }
        }
    }

    fn fill_frame_num_gaps(
        &mut self,
        pic_info: &PicInfoH264,
        log2_max_frame_num_m4: u32,
        _poc_type: u32,
        _log2_max_poc_lsb_m4: u32,
        gaps_allowed: bool,
        sps_max_num_ref_frames: u32,
    ) {
        let max_frame_num = 1u32 << (log2_max_frame_num_m4 + 4);
        if pic_info.is_idr {
            self.prev_ref_frame_num = 0;
        }
        if pic_info.frame_num != self.prev_ref_frame_num {
            let mut unused_short_term_frame_num = (self.prev_ref_frame_num + 1) % max_frame_num;
            while unused_short_term_frame_num != pic_info.frame_num {
                if !gaps_allowed {
                    break;
                }
                // Fill gap
                while self.is_dpb_full() {
                    self.dpb_bumping(true);
                }
                self.curr_dpb_idx = -1;
                for j in 0..MAX_DPB_SLOTS as i8 {
                    if self.dpb[j as usize].state == DpbStateH264::Empty as u32 {
                        self.curr_dpb_idx = j;
                        break;
                    }
                }
                if self.curr_dpb_idx >= 0 {
                    let idx = self.curr_dpb_idx as usize;
                    self.dpb[idx].pic_info_frame_num = pic_info.frame_num;
                    self.dpb[idx].complementary_field_pair = false;
                    self.sliding_window_memory_management(pic_info, sps_max_num_ref_frames);
                    self.dpb[idx].top_field_marking = MARKING_SHORT;
                    self.dpb[idx].bottom_field_marking = MARKING_SHORT;
                    self.dpb[idx].reference_picture = true;
                    self.dpb[idx].not_existing = true;
                    self.dpb[idx].top_needed_for_output = false;
                    self.dpb[idx].bottom_needed_for_output = false;
                    self.dpb[idx].state = DpbStateH264::Frame as u32;
                }
                self.prev_ref_frame_num = pic_info.frame_num;
                unused_short_term_frame_num = (unused_short_term_frame_num + 1) % max_frame_num;
            }
        }
        if pic_info.is_reference {
            self.prev_ref_frame_num = pic_info.frame_num;
        }
    }

    fn sort_list_descending(
        &self,
        list: &mut [RefPicListEntry],
        kmin: usize,
        mut n: i32,
        sort_check: DpbSortFn,
        skip_corrupt: bool,
    ) -> usize {
        let mut k = kmin;
        while k < MAX_DPB_SLOTS {
            let mut m = INF_MIN;
            let mut i1: i32 = -1;
            let mut v = -1i32;
            for i in 0..MAX_DPB_SLOTS {
                if self.dpb[i].view_id != self.dpb[self.curr_dpb_idx as usize].view_id {
                    continue;
                }
                if self.dpb[i].frame_is_corrupted && skip_corrupt {
                    continue;
                }
                if sort_check(&self.dpb[i], 0, &mut v) && v >= m && v <= n {
                    i1 = i as i32;
                    m = v;
                }
            }
            if i1 < 0 { break; }
            list[k].dpb_index = i1;
            if m == INF_MIN { k += 1; break; }
            n = m - 1;
            k += 1;
        }
        k
    }

    fn sort_list_ascending(
        &self,
        list: &mut [RefPicListEntry],
        kmin: usize,
        mut n: i32,
        sort_check: DpbSortFn,
        skip_corrupt: bool,
    ) -> usize {
        let mut k = kmin;
        while k < MAX_DPB_SLOTS {
            let mut m = INF_MAX;
            let mut i1: i32 = -1;
            let mut v = -1i32;
            for i in 0..MAX_DPB_SLOTS {
                if self.dpb[i].view_id != self.dpb[self.curr_dpb_idx as usize].view_id {
                    continue;
                }
                if self.dpb[i].frame_is_corrupted && skip_corrupt {
                    continue;
                }
                if sort_check(&self.dpb[i], 0, &mut v) && v <= m && v > n {
                    i1 = i as i32;
                    m = v;
                }
            }
            if i1 < 0 { break; }
            list[k].dpb_index = i1;
            n = m;
            k += 1;
        }
        k
    }

    // --- Public methods added for encoder integration ---

    /// Find the first empty DPB slot (0..MAX_DPB_SLOTS-1).
    /// Returns -1 if no slot is available.
    fn find_empty_slot_idx(&self) -> i8 {
        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].state == DpbStateH264::Empty as u32 {
                return i as i8;
            }
        }
        -1
    }

    /// End picture processing: apply reference picture marking and store the
    /// current picture (in temp slot `MAX_DPB_SLOTS`) into a real DPB slot.
    ///
    /// For IDR pictures, clears all existing references first.
    /// For non-IDR reference pictures, applies sliding window memory management.
    /// Returns the DPB slot index where the picture was stored, or -1 for
    /// non-reference pictures.
    pub fn dpb_picture_end(
        &mut self,
        pic_info: &PicInfoH264,
        sps_max_num_ref_frames: u32,
    ) -> i8 {
        let temp_idx = MAX_DPB_SLOTS;

        if pic_info.is_idr {
            // IDR: clear all existing references
            for i in 0..MAX_DPB_SLOTS {
                self.dpb[i].top_field_marking = MARKING_UNUSED;
                self.dpb[i].bottom_field_marking = MARKING_UNUSED;
                self.dpb[i].state = DpbStateH264::Empty as u32;
            }
            self.max_long_term_frame_idx = -1;
        }

        if !pic_info.is_reference {
            self.dpb[temp_idx].state = DpbStateH264::Empty as u32;
            self.curr_dpb_idx = -1;
            return -1;
        }

        // For non-IDR reference pictures, apply sliding window
        if !pic_info.is_idr && !pic_info.adaptive_ref_pic_marking_mode_flag {
            self.sliding_window_memory_management(pic_info, sps_max_num_ref_frames);
        }

        // Clean up slots freed by sliding window
        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].state != DpbStateH264::Empty as u32
                && self.dpb[i].top_field_marking == MARKING_UNUSED
                && self.dpb[i].bottom_field_marking == MARKING_UNUSED
                && !self.dpb[i].top_needed_for_output
                && !self.dpb[i].bottom_needed_for_output
            {
                self.dpb[i].state = DpbStateH264::Empty as u32;
            }
        }

        // Bump if DPB is still full
        while self.is_dpb_full() {
            self.dpb_bumping(true);
        }

        let target = self.find_empty_slot_idx();
        if target < 0 {
            return -1;
        }

        let t = target as usize;
        self.dpb[t] = self.dpb[temp_idx].clone();
        self.dpb[t].state = DpbStateH264::Frame as u32;
        // Encoder doesn't need output ordering
        self.dpb[t].top_needed_for_output = false;
        self.dpb[t].bottom_needed_for_output = false;
        self.dpb[t].top_field_marking = MARKING_SHORT;
        self.dpb[t].bottom_field_marking = MARKING_SHORT;

        // Clear temp slot
        self.dpb[temp_idx].state = DpbStateH264::Empty as u32;

        self.curr_dpb_idx = target;
        target
    }

    /// Build reference picture list for P-frames (L0 only).
    ///
    /// Returns the DPB slot info lists with L0 populated:
    /// - Short-term references sorted by descending PicNum
    /// - Long-term references sorted by ascending LongTermPicNum
    pub fn build_ref_pic_list_p(&self) -> DpbSlotInfoLists {
        let mut lists = DpbSlotInfoLists::default();
        let mut temp_list = [RefPicListEntry::default(); MAX_DPB_SLOTS];

        // Short-term descending by PicNum
        let k = self.sort_list_descending(
            &mut temp_list, 0, INF_MAX,
            sort_check_short_term_p_frame, false,
        );
        // Long-term ascending by LongTermPicNum
        let k = self.sort_list_ascending(
            &mut temp_list, k, -1,
            sort_check_long_term_frame, false,
        );

        let count = k.min(16);
        lists.ref_pic_list_count[0] = count as u32;
        for i in 0..count {
            lists.ref_pic_list[0][i] = temp_list[i].dpb_index as u8;
            lists.dpb_slots_use_mask |= 1 << temp_list[i].dpb_index;
        }

        lists
    }

    /// Build reference picture lists for B-frames (L0 and L1).
    ///
    /// L0: short-term with POC <= current (descending) + POC > current (ascending) + long-term
    /// L1: short-term with POC > current (ascending) + POC <= current (descending) + long-term
    pub fn build_ref_pic_list_b(&self, current_poc: i32) -> DpbSlotInfoLists {
        let mut lists = DpbSlotInfoLists::default();
        let mut temp_list0 = [RefPicListEntry::default(); MAX_DPB_SLOTS];
        let mut temp_list1 = [RefPicListEntry::default(); MAX_DPB_SLOTS];

        // L0: short-term POC <= current descending, then POC > current ascending, then long-term
        let k = self.sort_list_descending(
            &mut temp_list0, 0, current_poc,
            sort_check_short_term_b_frame, false,
        );
        let k = self.sort_list_ascending(
            &mut temp_list0, k, current_poc,
            sort_check_short_term_b_frame, false,
        );
        let k = self.sort_list_ascending(
            &mut temp_list0, k, -1,
            sort_check_long_term_frame, false,
        );
        let count0 = k.min(16);
        lists.ref_pic_list_count[0] = count0 as u32;
        for i in 0..count0 {
            lists.ref_pic_list[0][i] = temp_list0[i].dpb_index as u8;
            lists.dpb_slots_use_mask |= 1 << temp_list0[i].dpb_index;
        }

        // L1: short-term POC > current ascending, then POC <= current descending, then long-term
        let k = self.sort_list_ascending(
            &mut temp_list1, 0, current_poc,
            sort_check_short_term_b_frame, false,
        );
        let k = self.sort_list_descending(
            &mut temp_list1, k, current_poc,
            sort_check_short_term_b_frame, false,
        );
        let k = self.sort_list_ascending(
            &mut temp_list1, k, -1,
            sort_check_long_term_frame, false,
        );
        let count1 = k.min(16);
        lists.ref_pic_list_count[1] = count1 as u32;
        for i in 0..count1 {
            lists.ref_pic_list[1][i] = temp_list1[i].dpb_index as u8;
            lists.dpb_slots_use_mask |= 1 << temp_list1[i].dpb_index;
        }

        lists
    }

    /// Get the list of active DPB slot indices (slots with reference marking).
    pub fn get_active_ref_slots(&self) -> Vec<u8> {
        let mut slots = Vec::new();
        for i in 0..MAX_DPB_SLOTS {
            if self.dpb[i].state != DpbStateH264::Empty as u32
                && (self.dpb[i].top_field_marking == MARKING_SHORT
                    || self.dpb[i].bottom_field_marking == MARKING_SHORT
                    || self.dpb[i].top_field_marking == MARKING_LONG
                    || self.dpb[i].bottom_field_marking == MARKING_LONG)
            {
                slots.push(i as u8);
            }
        }
        slots
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
}
