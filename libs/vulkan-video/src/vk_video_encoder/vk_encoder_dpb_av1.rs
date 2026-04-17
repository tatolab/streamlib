// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderDpbAV1.h + VkEncoderDpbAV1.cpp
//!
//! AV1 DPB management for the encoder.
//! Implements reference buffer pool management, reference frame type assignment,
//! reference frame group construction, stale reference invalidation,
//! refresh_frame_flags derivation, and primary reference frame selection.


// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const BUFFER_POOL_MAX_SIZE: usize = 10;
pub const INVALID_IDX: i32 = -1;
pub const NUM_REF_FRAMES: usize = 8; // STD_VIDEO_AV1_NUM_REF_FRAMES
pub const REFS_PER_FRAME: usize = 7; // STD_VIDEO_AV1_REFS_PER_FRAME
pub const ORDER_HINT_BITS: u32 = 8;  // Default, may be configured

// Reference frame flags
#[allow(dead_code)] // AV1 codec stub
const REFRESH_LAST_FRAME_FLAG: u32 = 1 << 1;  // STD_VIDEO_AV1_REFERENCE_NAME_LAST_FRAME
const REFRESH_GOLDEN_FRAME_FLAG: u32 = 1 << 4;
const REFRESH_BWD_FRAME_FLAG: u32 = 1 << 5;
const REFRESH_ALT2_FRAME_FLAG: u32 = 1 << 6;
const REFRESH_ALT_FRAME_FLAG: u32 = 1 << 7;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// AV1 frame types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Av1FrameType {
    #[default]
    Key = 0,
    Inter = 1,
    IntraOnly = 2,
    Switch = 3,
}

/// AV1 reference name.
/// Note: In the C++ code, INTRA_FRAME and INVALID both map to 0.
/// In Rust we separate them to avoid discriminant conflicts.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Av1ReferenceName {
    #[default]
    IntraFrame = 0, // Also serves as INVALID
    LastFrame = 1,
    Last2Frame = 2,
    Last3Frame = 3,
    GoldenFrame = 4,
    BwdrefFrame = 5,
    Altref2Frame = 6,
    AltrefFrame = 7,
}

/// Sentinel value for invalid reference name (maps to IntraFrame/0 in the spec).
pub const AV1_REFERENCE_NAME_INVALID: u32 = 0;

/// AV1 primary reference type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryRefType {
    RegularFrame = 0,
    ArfFrame = 1,
    OverlayFrame = 2,
    GldFrame = 3,
    BrfFrame = 4,
    IntArfFrame = 5,
}

pub const MAX_PRI_REF_TYPES: usize = 6;

/// AV1 frame update type.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameUpdateType {
    KfUpdate = 0,
    LfUpdate = 1,
    GfUpdate = 2,
    ArfUpdate = 3,
    OverlayUpdate = 4,
    IntnlOverlayUpdate = 5,
    IntnlArfUpdate = 6,
    BwdUpdate = 7,
    NoUpdate = 8,
}

// ---------------------------------------------------------------------------
// DpbEntryAV1
// ---------------------------------------------------------------------------

/// A single entry in the AV1 encoder DPB.
#[derive(Debug, Clone, Default)]
pub struct DpbEntryAV1 {
    pub ref_count: u32,
    pub frame_id: u32,
    pub pic_order_cnt_val: u32,
    pub frame_type: Av1FrameType,
    pub ref_name: u32, // Av1ReferenceName as u32
    pub dirty_intra_refresh_regions: u32,
}

// ---------------------------------------------------------------------------
// VkEncDpbAV1
// ---------------------------------------------------------------------------

/// AV1 encoder DPB.
pub struct VkEncDpbAV1 {
    dpb: [DpbEntryAV1; BUFFER_POOL_MAX_SIZE + 1],
    max_dpb_size: u8,

    max_ref_frames_l0: i32,
    max_ref_frames_l1: i32,
    num_ref_frames_l0: i32,
    num_ref_frames_l1: i32,

    num_ref_frames_in_group1: i32,
    num_ref_frames_in_group2: i32,
    #[allow(dead_code)] // AV1 codec stub
    ref_names_in_group1: [i32; REFS_PER_FRAME],
    #[allow(dead_code)] // AV1 codec stub
    ref_names_in_group2: [i32; REFS_PER_FRAME],

    #[allow(dead_code)] // AV1 codec stub
    ref_name2_dpb_idx: [i32; REFS_PER_FRAME],
    ref_buf_id_map: [i32; NUM_REF_FRAMES],
    ref_frame_dpb_id_map: [i8; NUM_REF_FRAMES],
    primary_ref_buf_id_map: [i32; MAX_PRI_REF_TYPES],
    primary_ref_dpb_idx: i32,
    ref_buf_update_flag: u32,
    last_last_ref_name_in_use: u32, // Av1ReferenceName

    last_key_frame_time_stamp: u64,
}

impl VkEncDpbAV1 {
    /// Create a new AV1 DPB instance.
    pub fn create_instance() -> Box<Self> {
        let mut dpb = Box::new(Self {
            dpb: std::array::from_fn(|_| DpbEntryAV1::default()),
            max_dpb_size: 0,
            max_ref_frames_l0: 0,
            max_ref_frames_l1: 0,
            num_ref_frames_l0: 0,
            num_ref_frames_l1: 0,
            num_ref_frames_in_group1: 0,
            num_ref_frames_in_group2: 0,
            ref_names_in_group1: [0; REFS_PER_FRAME],
            ref_names_in_group2: [0; REFS_PER_FRAME],
            ref_name2_dpb_idx: [-1; REFS_PER_FRAME],
            ref_buf_id_map: [-1; NUM_REF_FRAMES],
            ref_frame_dpb_id_map: [-1; NUM_REF_FRAMES],
            primary_ref_buf_id_map: [-1; MAX_PRI_REF_TYPES],
            primary_ref_dpb_idx: -1,
            ref_buf_update_flag: 0,
            last_last_ref_name_in_use: 0,
            last_key_frame_time_stamp: 0,
        });
        dpb.dpb_init();
        dpb
    }

    fn dpb_init(&mut self) {
        self.dpb_deinit();
    }

    fn dpb_deinit(&mut self) {
        for entry in self.dpb.iter_mut() {
            *entry = DpbEntryAV1::default();
        }
        self.max_dpb_size = 0;
        self.max_ref_frames_l0 = 0;
        self.max_ref_frames_l1 = 0;
        self.num_ref_frames_in_group1 = 0;
        self.num_ref_frames_in_group2 = 0;
        self.ref_buf_id_map = [-1; NUM_REF_FRAMES];
        self.ref_frame_dpb_id_map = [-1; NUM_REF_FRAMES];
        self.primary_ref_buf_id_map = [-1; MAX_PRI_REF_TYPES];
        self.primary_ref_dpb_idx = -1;
        self.ref_buf_update_flag = 0;
        self.last_last_ref_name_in_use = 0;
        self.last_key_frame_time_stamp = 0;
    }

    /// Initialize DPB for a new sequence.
    pub fn dpb_sequence_start(&mut self, user_dpb_size: u32, num_b_frames: i32) -> i32 {
        self.dpb_deinit();

        debug_assert!(user_dpb_size <= BUFFER_POOL_MAX_SIZE as u32);
        debug_assert!(user_dpb_size >= NUM_REF_FRAMES as u32);

        self.max_dpb_size = user_dpb_size as u8;
        self.max_ref_frames_l0 = 4;
        self.max_ref_frames_l1 = if num_b_frames > 0 { 3 } else { 3 };

        for i in 0..NUM_REF_FRAMES {
            self.ref_buf_id_map[i] = i as i32;
        }

        self.last_last_ref_name_in_use = if num_b_frames == 0 { 4 } else { 3 }; // GOLDEN or LAST3

        0
    }

    /// Start processing a picture. Returns the DPB slot index.
    pub fn dpb_picture_start(
        &mut self,
        frame_type: Av1FrameType,
        ref_name: u32,
        pic_order_cnt_val: u32,
        frame_id: u32,
        show_existing_frame: bool,
        frame_to_show_map_id: i32,
    ) -> i8 {
        if !show_existing_frame {
            let mut dpb_idx: i8 = -1;
            for i in 0..self.max_dpb_size as i8 {
                if self.dpb[i as usize].ref_count == 0 {
                    dpb_idx = i;
                    break;
                }
            }
            if dpb_idx < 0 || dpb_idx >= self.max_dpb_size as i8 {
                return INVALID_IDX as i8;
            }

            let idx = dpb_idx as usize;
            self.dpb[idx].frame_id = frame_id;
            self.dpb[idx].pic_order_cnt_val = pic_order_cnt_val;
            self.dpb[idx].frame_type = frame_type;
            self.dpb[idx].ref_name = ref_name;
            self.dpb[idx].ref_count = 1;
            dpb_idx
        } else {
            let dpb_idx = self.get_ref_buf_dpb_id(frame_to_show_map_id);
            if dpb_idx == INVALID_IDX as i8 {
                return INVALID_IDX as i8;
            }
            self.dpb[dpb_idx as usize].ref_count += 1;
            dpb_idx
        }
    }

    /// End picture processing.
    pub fn dpb_picture_end(&mut self, dpb_idx: i8, _show_existing_frame: bool) {
        self.update_ref_frame_dpb_id_map(dpb_idx);
        self.release_frame(dpb_idx as usize);
    }

    /// Destroy the DPB.
    pub fn dpb_destroy(&mut self) {
        self.dpb_deinit();
    }

    /// Get the maximum DPB size.
    pub fn get_max_dpb_size(&self) -> u8 {
        self.max_dpb_size
    }

    /// Get refresh frame flags.
    pub fn get_refresh_frame_flags(&self, shown_key_or_switch: bool, show_existing: bool) -> i32 {
        if shown_key_or_switch {
            return 0xff;
        }
        if show_existing {
            return 0;
        }
        let mut flags = 0i32;
        for i in 0..NUM_REF_FRAMES {
            if (self.ref_buf_update_flag & (1 << i)) != 0 {
                let ref_buf_id = self.get_ref_buf_id(i as u32);
                if ref_buf_id != INVALID_IDX {
                    flags |= 1 << ref_buf_id;
                }
            }
        }
        flags
    }

    /// Get reference frame DPB ID for a reference name.
    pub fn get_ref_frame_dpb_id(&self, ref_name: u32) -> i32 {
        if ref_name >= 1 && ref_name <= 7 {
            let buf_id = self.ref_buf_id_map[ref_name as usize];
            if buf_id >= 0 && (buf_id as usize) < NUM_REF_FRAMES {
                return self.ref_frame_dpb_id_map[buf_id as usize] as i32;
            }
        }
        INVALID_IDX
    }

    /// Get reference buffer ID for a reference name.
    pub fn get_ref_buf_id(&self, ref_name: u32) -> i32 {
        if ref_name >= 1 && ref_name <= 7 {
            return self.ref_buf_id_map[ref_name as usize];
        }
        INVALID_IDX
    }

    /// Get the DPB ID for a reference buffer slot.
    pub fn get_ref_buf_dpb_id(&self, ref_buf_id: i32) -> i8 {
        if ref_buf_id >= 0 && (ref_buf_id as usize) < NUM_REF_FRAMES {
            return self.ref_frame_dpb_id_map[ref_buf_id as usize];
        }
        INVALID_IDX as i8
    }

    /// Get number of references in a group.
    pub fn get_num_refs_in_group(&self, group_id: i32) -> i32 {
        if group_id == 0 { self.num_ref_frames_in_group1 } else { self.num_ref_frames_in_group2 }
    }

    pub fn get_num_refs_l0(&self) -> i32 { self.num_ref_frames_l0 }
    pub fn get_num_refs_l1(&self) -> i32 { self.num_ref_frames_l1 }

    /// Get frame type for a DPB index.
    pub fn get_frame_type(&self, dpb_idx: i32) -> Av1FrameType {
        debug_assert!(dpb_idx >= 0 && (dpb_idx as usize) < self.max_dpb_size as usize);
        self.dpb[dpb_idx as usize].frame_type
    }

    /// Get frame ID for a DPB index.
    pub fn get_frame_id(&self, dpb_idx: i32) -> u32 {
        debug_assert!(dpb_idx >= 0 && (dpb_idx as usize) < self.max_dpb_size as usize);
        self.dpb[dpb_idx as usize].frame_id
    }

    /// Get POC for a DPB index.
    pub fn get_pic_order_cnt_val(&self, dpb_idx: i32) -> u32 {
        debug_assert!(dpb_idx >= 0 && (dpb_idx as usize) < self.max_dpb_size as usize);
        self.dpb[dpb_idx as usize].pic_order_cnt_val
    }

    /// Get dirty intra-refresh regions.
    pub fn get_dirty_intra_refresh_regions(&self, dpb_idx: i8) -> u32 {
        debug_assert!((dpb_idx as usize) < self.max_dpb_size as usize);
        self.dpb[dpb_idx as usize].dirty_intra_refresh_regions
    }

    /// Set dirty intra-refresh regions.
    pub fn set_dirty_intra_refresh_regions(&mut self, dpb_idx: i8, regions: u32) {
        debug_assert!((dpb_idx as usize) < self.max_dpb_size as usize);
        self.dpb[dpb_idx as usize].dirty_intra_refresh_regions = regions;
    }

    /// Fill standard reference info for a DPB slot.
    pub fn fill_std_reference_info(&self, dpb_idx: u8, frame_type: &mut Av1FrameType, order_hint: &mut u32) {
        debug_assert!((dpb_idx as usize) < self.max_dpb_size as usize);
        let entry = &self.dpb[dpb_idx as usize];
        *frame_type = entry.frame_type;
        *order_hint = entry.pic_order_cnt_val % (1 << ORDER_HINT_BITS);
    }

    /// Configure reference buffer update flags.
    pub fn configure_ref_buf_update(&mut self, shown_key_or_switch: bool, show_existing: bool, update_type: FrameUpdateType) {
        if shown_key_or_switch {
            self.ref_buf_update_flag = 0xff;
            return;
        }
        if show_existing || update_type == FrameUpdateType::NoUpdate {
            self.ref_buf_update_flag = 0;
            return;
        }

        let refresh_last = 1u32 << self.last_last_ref_name_in_use;
        self.ref_buf_update_flag = match update_type {
            FrameUpdateType::KfUpdate => refresh_last | REFRESH_GOLDEN_FRAME_FLAG | REFRESH_ALT2_FRAME_FLAG | REFRESH_ALT_FRAME_FLAG,
            FrameUpdateType::LfUpdate => refresh_last,
            FrameUpdateType::GfUpdate => refresh_last | REFRESH_GOLDEN_FRAME_FLAG,
            FrameUpdateType::OverlayUpdate => refresh_last,
            FrameUpdateType::ArfUpdate => REFRESH_ALT_FRAME_FLAG,
            FrameUpdateType::IntnlOverlayUpdate => refresh_last,
            FrameUpdateType::IntnlArfUpdate => REFRESH_ALT2_FRAME_FLAG,
            FrameUpdateType::BwdUpdate => REFRESH_BWD_FRAME_FLAG,
            _ => 0,
        };
    }

    // --- Private ---

    fn release_frame(&mut self, dpb_id: usize) {
        debug_assert!(dpb_id < self.max_dpb_size as usize || dpb_id < BUFFER_POOL_MAX_SIZE + 1);
        if self.dpb[dpb_id].ref_count > 0 {
            self.dpb[dpb_id].ref_count -= 1;
        }
    }

    fn update_ref_frame_dpb_id_map(&mut self, dpb_idx: i8) {
        for i in 0..NUM_REF_FRAMES {
            if (self.ref_buf_update_flag >> i) & 1 == 1 {
                let buf_id = self.ref_buf_id_map[i];
                if buf_id >= 0 && (buf_id as usize) < NUM_REF_FRAMES {
                    let old_dpb_id = self.ref_frame_dpb_id_map[buf_id as usize];
                    if old_dpb_id >= 0 && (old_dpb_id as usize) < BUFFER_POOL_MAX_SIZE + 1 {
                        self.release_frame(old_dpb_id as usize);
                    }
                    self.ref_frame_dpb_id_map[buf_id as usize] = dpb_idx;
                    if dpb_idx >= 0 {
                        self.dpb[dpb_idx as usize].ref_count += 1;
                    }
                }
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

    #[test]
    fn test_dpb_av1_create() {
        let dpb = VkEncDpbAV1::create_instance();
        assert_eq!(dpb.max_dpb_size, 0);
    }

    #[test]
    fn test_dpb_av1_sequence_start() {
        let mut dpb = VkEncDpbAV1::create_instance();
        dpb.dpb_sequence_start(8, 0);
        assert_eq!(dpb.max_dpb_size, 8);
        assert_eq!(dpb.max_ref_frames_l0, 4);
    }

    #[test]
    fn test_dpb_av1_picture_start() {
        let mut dpb = VkEncDpbAV1::create_instance();
        dpb.dpb_sequence_start(8, 0);
        let idx = dpb.dpb_picture_start(Av1FrameType::Key, 0, 0, 0, false, -1);
        assert!(idx >= 0);
        assert_eq!(dpb.dpb[idx as usize].ref_count, 1);
    }

    #[test]
    fn test_refresh_frame_flags_key() {
        let mut dpb = VkEncDpbAV1::create_instance();
        dpb.dpb_sequence_start(8, 0);
        assert_eq!(dpb.get_refresh_frame_flags(true, false), 0xff);
    }

    #[test]
    fn test_refresh_frame_flags_show_existing() {
        let mut dpb = VkEncDpbAV1::create_instance();
        dpb.dpb_sequence_start(8, 0);
        assert_eq!(dpb.get_refresh_frame_flags(false, true), 0);
    }

    #[test]
    fn test_configure_ref_buf_update() {
        let mut dpb = VkEncDpbAV1::create_instance();
        dpb.dpb_sequence_start(8, 0);
        dpb.configure_ref_buf_update(true, false, FrameUpdateType::KfUpdate);
        assert_eq!(dpb.ref_buf_update_flag, 0xff);

        dpb.configure_ref_buf_update(false, true, FrameUpdateType::LfUpdate);
        assert_eq!(dpb.ref_buf_update_flag, 0);
    }

    #[test]
    fn test_dpb_entry_default() {
        let entry = DpbEntryAV1::default();
        assert_eq!(entry.ref_count, 0);
        assert_eq!(entry.frame_type, Av1FrameType::Key);
    }
}
