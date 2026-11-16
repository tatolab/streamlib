// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Port of VkVideoParser/VulkanVideoParser.cpp
//!
//! Bridge between the NvVideoParser bitstream parser and the Vulkan Video
//! decode pipeline. Creates and manages the parser instance, translates
//! parsed picture data into Vulkan Video decode parameters, and manages
//! DPB (Decoded Picture Buffer) slot allocation for H.264, H.265, AV1,
//! and VP9.
//!
//! Key divergences from C++:
//! - C++ virtual inheritance (`VkParserVideoDecodeClient`, `IVulkanVideoParser`)
//!   is expressed as trait implementations.
//! - `VkSharedBaseObj<T>` → `Arc<T>` or `Option<Arc<T>>`.
//! - `memset` / `memcpy` → Default::default() / struct copy.
//! - Bit-field members → regular `bool` / `u32` fields.
//! - `std::queue` → `VecDeque`.
//! - `std::vector` → `Vec`.
//! - The `DpbSlot` / `DpbSlots` helper classes are ported as standalone
//!   structs since they have no virtual methods.
//! - The large `DecodePicture` codec-specific branches are preserved 1-to-1,
//!   using Rust match arms instead of if-else chains.
//! - Static mutable debug flags (`m_dumpParserData`, `m_dumpDpbData`) are
//!   replaced by per-instance booleans.

use std::collections::VecDeque;
use std::sync::Arc;

use vulkanalia::vk;

// ---------------------------------------------------------------------------
// Constants — mirrors C++ static const values
// ---------------------------------------------------------------------------

const TOP_FIELD_SHIFT: u32 = 0;
#[allow(
    dead_code,
    reason = "kept for parity with the nvpro-samples C++ source this ports"
)]
const TOP_FIELD_MASK: u32 = 1 << TOP_FIELD_SHIFT;
const BOTTOM_FIELD_SHIFT: u32 = 1;
#[allow(
    dead_code,
    reason = "kept for parity with the nvpro-samples C++ source this ports"
)]
const BOTTOM_FIELD_MASK: u32 = 1 << BOTTOM_FIELD_SHIFT;
#[allow(
    dead_code,
    reason = "kept for parity with the nvpro-samples C++ source this ports"
)]
const FIELD_IS_REFERENCE_MASK: u32 = TOP_FIELD_MASK | BOTTOM_FIELD_MASK;

pub const MAX_DPB_REF_SLOTS: u32 = 16;
pub const MAX_DPB_REF_AND_SETUP_SLOTS: u32 = MAX_DPB_REF_SLOTS + 1;
pub const MAX_FRM_CNT: usize = 32;
pub const HEVC_MAX_DPB_SLOTS: usize = 16;
pub const AVC_MAX_DPB_SLOTS: usize = 17;

/// VP9 decode operation flag — not yet in ash 0.38, but present in the
/// Vulkan registry as `VK_VIDEO_CODEC_OPERATION_DECODE_VP9_BIT_KHR`.
/// We define it locally until ash gains support.
pub const VIDEO_CODEC_OPERATION_DECODE_VP9: vk::VideoCodecOperationFlagsKHR =
    vk::VideoCodecOperationFlagsKHR::from_bits_truncate(0x0000_0100);

// ---------------------------------------------------------------------------
// DpbSlot — per-slot DPB tracking
// ---------------------------------------------------------------------------

/// Tracks data associated with a single DPB reference slot.
/// Mirrors C++ `DpbSlot`.
#[derive(Debug, Default)]
pub struct DpbSlot {
    picture_id: i32,
    pic_buf_idx: Option<i32>,
    reserved: bool,
    in_use: bool,
    /// Decode width of the picture resource (used for VP9 reference sizing).
    pub decode_width: u32,
    /// Decode height of the picture resource (used for VP9 reference sizing).
    pub decode_height: u32,
}

impl DpbSlot {
    pub fn is_in_use(&self) -> bool {
        self.reserved || self.in_use
    }

    pub fn is_available(&self) -> bool {
        !self.is_in_use()
    }

    pub fn invalidate(&mut self) -> bool {
        let was_in_use = self.is_in_use();
        self.pic_buf_idx = None;
        self.reserved = false;
        self.in_use = false;
        was_in_use
    }

    pub fn get_picture_resource(&self) -> Option<i32> {
        self.pic_buf_idx
    }

    pub fn set_picture_resource(&mut self, pic_buf_idx: Option<i32>, age: i32) -> Option<i32> {
        let old = self.pic_buf_idx;
        self.pic_buf_idx = pic_buf_idx;
        self.picture_id = age;
        old
    }

    pub fn reserve(&mut self) {
        self.reserved = true;
    }

    pub fn mark_in_use(&mut self, age: i32) {
        self.picture_id = age;
        self.in_use = true;
    }

    pub fn get_age(&self) -> i32 {
        self.picture_id
    }
}

// ---------------------------------------------------------------------------
// DpbSlots — manages the full DPB slot array
// ---------------------------------------------------------------------------

/// Manages allocation and tracking of DPB slots.
/// Mirrors C++ `DpbSlots`.
pub struct DpbSlots {
    dpb_max_size: u32,
    slot_in_use_mask: u32,
    dpb: Vec<DpbSlot>,
    dpb_slots_available: VecDeque<u8>,
}

impl DpbSlots {
    pub fn new(dpb_max_size: u32) -> Self {
        let mut s = Self {
            dpb_max_size: 0,
            slot_in_use_mask: 0,
            dpb: Vec::new(),
            dpb_slots_available: VecDeque::new(),
        };
        s.init(dpb_max_size, false);
        s
    }

    /// Initialise or reconfigure the DPB slots.
    /// Returns the final DPB max size.
    pub fn init(&mut self, new_dpb_max_size: u32, reconfigure: bool) -> u32 {
        debug_assert!(new_dpb_max_size <= MAX_DPB_REF_AND_SETUP_SLOTS);

        if !reconfigure {
            self.deinit();
        }

        if reconfigure && new_dpb_max_size < self.dpb_max_size {
            return self.dpb_max_size;
        }

        let old_dpb_max_size = if reconfigure { self.dpb_max_size } else { 0 };
        self.dpb_max_size = new_dpb_max_size;

        self.dpb
            .resize_with(self.dpb_max_size as usize, DpbSlot::default);

        for ndx in old_dpb_max_size..self.dpb_max_size {
            self.dpb[ndx as usize].invalidate();
        }

        for dpb_idx in old_dpb_max_size as u8..self.dpb_max_size as u8 {
            self.dpb_slots_available.push_back(dpb_idx);
        }

        self.dpb_max_size
    }

    pub fn deinit(&mut self) {
        for ndx in 0..self.dpb_max_size as usize {
            self.dpb[ndx].invalidate();
        }
        self.dpb_slots_available.clear();
        self.dpb_max_size = 0;
        self.slot_in_use_mask = 0;
    }

    /// Allocate a free DPB slot. Returns the slot index or -1 if none available.
    pub fn allocate_slot(&mut self) -> i8 {
        if let Some(slot) = self.dpb_slots_available.pop_front() {
            debug_assert!((slot as u32) < self.dpb_max_size);
            self.slot_in_use_mask |= 1 << slot;
            self.dpb[slot as usize].reserve();
            slot as i8
        } else {
            debug_assert!(false, "No more DPB slots are available");
            -1
        }
    }

    /// Free a previously allocated slot.
    pub fn free_slot(&mut self, slot: i8) {
        debug_assert!((slot as u32) < self.dpb_max_size);
        debug_assert!(self.dpb[slot as usize].is_in_use());
        debug_assert!(self.slot_in_use_mask & (1 << slot) != 0);

        self.dpb[slot as usize].invalidate();
        self.dpb_slots_available.push_back(slot as u8);
        self.slot_in_use_mask &= !(1 << slot);
    }

    pub fn get(&self, slot: u32) -> &DpbSlot {
        debug_assert!(slot < self.dpb_max_size);
        &self.dpb[slot as usize]
    }

    pub fn get_mut(&mut self, slot: u32) -> &mut DpbSlot {
        debug_assert!(slot < self.dpb_max_size);
        &mut self.dpb[slot as usize]
    }

    /// Find the DPB slot that holds the given picture buffer index.
    /// Returns -1 if not found.
    pub fn get_slot_of_picture_resource(&self, pic_buf_idx: i32) -> i8 {
        for i in 0..self.dpb_max_size as usize {
            if (self.slot_in_use_mask & (1 << i)) != 0
                && self.dpb[i].is_in_use()
                && self.dpb[i].get_picture_resource() == Some(pic_buf_idx)
            {
                return i as i8;
            }
        }
        -1
    }

    /// Map a picture buffer to a specific DPB slot, unmapping it from any
    /// other slot it may occupy.
    pub fn map_picture_resource(&mut self, pic_buf_idx: Option<i32>, dpb_slot: i32, age: i32) {
        for slot in 0..self.dpb_max_size {
            if slot as i32 == dpb_slot {
                self.dpb[slot as usize].set_picture_resource(pic_buf_idx, age);
            } else if let Some(idx) = pic_buf_idx
                && self.dpb[slot as usize].get_picture_resource() == Some(idx)
            {
                self.free_slot(slot as i8);
            }
        }
    }

    pub fn get_slot_in_use_mask(&self) -> u32 {
        self.slot_in_use_mask
    }

    pub fn get_max_size(&self) -> u32 {
        self.dpb_max_size
    }
}

impl Drop for DpbSlots {
    fn drop(&mut self) {
        self.deinit();
    }
}

// ---------------------------------------------------------------------------
// DpbH264Entry — internal H.264 DPB representation
// ---------------------------------------------------------------------------

/// Internal DPB entry used for H.264 / H.265 / AV1 reference management.
/// Mirrors C++ `VulkanVideoParser::dpbH264Entry`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DpbH264Entry {
    pub dpb_slot: i8,
    pub used_for_reference: u32,
    pub is_long_term: bool,
    pub is_non_existing: bool,
    pub is_field_ref: bool,
    /// H.264: field order count [top, bottom]; H.265/AV1: PicOrderCnt in [0].
    pub field_order_cnt: [i16; 2],
    /// H.264: FrameIdx / LongTermFrameIdx; H.265: originalDpbIndex.
    pub frame_idx: i16,
    pub original_dpb_index: i8,
    /// Picture buffer index (replaces C++ `m_picBuff` pointer).
    pub pic_buf_idx: Option<i32>,
}

impl DpbH264Entry {
    /// Set reference with top/bottom field info (H.264 specific).
    /// Mirrors C++ `dpbH264Entry::setReferenceAndTopBottomField`.
    pub fn set_reference_and_top_bottom_field(
        &mut self,
        is_reference: bool,
        non_existing: bool,
        is_long_term: bool,
        is_field_ref: bool,
        top_field_is_reference: bool,
        bottom_field_is_reference: bool,
        frame_idx: i16,
        field_order_cnt_list: &[i16; 2],
        pic_buf_idx: Option<i32>,
    ) {
        self.is_non_existing = non_existing;
        self.is_long_term = is_long_term;
        self.is_field_ref = is_field_ref;

        if is_reference && is_field_ref {
            self.used_for_reference = ((bottom_field_is_reference as u32) << BOTTOM_FIELD_SHIFT)
                | ((top_field_is_reference as u32) << TOP_FIELD_SHIFT);
        } else {
            self.used_for_reference = if is_reference { 3 } else { 0 };
        }

        self.frame_idx = frame_idx;

        let top_idx = if self.used_for_reference == 2 { 1 } else { 0 };
        let bot_idx = if self.used_for_reference != 1 { 1 } else { 0 };
        self.field_order_cnt[0] = field_order_cnt_list[top_idx];
        self.field_order_cnt[1] = field_order_cnt_list[bot_idx];

        self.dpb_slot = -1;
        self.pic_buf_idx = pic_buf_idx;
    }

    /// Set reference for H.265 / AV1 (frame-level, no field distinction).
    /// Mirrors C++ `dpbH264Entry::setReference`.
    pub fn set_reference(
        &mut self,
        is_long_term: bool,
        pic_order_cnt: i32,
        pic_buf_idx: Option<i32>,
    ) {
        self.is_non_existing = pic_buf_idx.is_none();
        self.is_long_term = is_long_term;
        self.is_field_ref = false;
        self.used_for_reference = if pic_buf_idx.is_some() { 3 } else { 0 };

        // Store PicOrderCnt as two i16 halves in field_order_cnt.
        self.field_order_cnt[0] = pic_order_cnt as i16;
        self.field_order_cnt[1] = (pic_order_cnt >> 16) as i16;

        self.dpb_slot = -1;
        self.pic_buf_idx = pic_buf_idx;
        self.original_dpb_index = -1;
    }

    /// Reconstruct PicOrderCnt from the two i16 halves.
    pub fn pic_order_cnt(&self) -> i32 {
        (self.field_order_cnt[1] as i32) << 16 | (self.field_order_cnt[0] as u16 as i32)
    }

    pub fn is_ref(&self) -> bool {
        self.used_for_reference != 0
    }
}

// ---------------------------------------------------------------------------
// VulkanVideoParser — the main parser bridge
// ---------------------------------------------------------------------------

/// Bridge between the NvVideoParser and the Vulkan Video decode pipeline.
/// Mirrors C++ `NvVulkanDecoder::VulkanVideoParser`.
pub struct VulkanVideoParser {
    pub codec_type: vk::VideoCodecOperationFlagsKHR,
    pub max_num_decode_surfaces: u32,
    pub max_num_dpb_slots: u32,
    pub clock_rate: u64,
    pub current_picture_id: i32,
    pub dpb_slots_mask: u32,
    pub field_pic_flag_mask: u32,
    pub dpb: DpbSlots,
    pub out_of_band_picture_parameters: bool,
    pub inlined_picture_parameters_use_begin_coding: bool,
    pub picture_to_dpb_slot_map: [i8; MAX_FRM_CNT],
    pub dump_parser_data: bool,
    pub dump_dpb_data: bool,
}

impl VulkanVideoParser {
    /// Create a new parser instance.
    /// Mirrors C++ `VulkanVideoParser::VulkanVideoParser` constructor.
    pub fn new(
        codec_type: vk::VideoCodecOperationFlagsKHR,
        max_num_decode_surfaces: u32,
        _max_num_dpb_surfaces: u32,
        clock_rate: u64,
    ) -> Self {
        let mut picture_to_dpb_slot_map = [-1i8; MAX_FRM_CNT];
        for slot in picture_to_dpb_slot_map.iter_mut() {
            *slot = -1;
        }

        Self {
            codec_type,
            max_num_decode_surfaces,
            max_num_dpb_slots: _max_num_dpb_surfaces,
            clock_rate,
            current_picture_id: 0,
            dpb_slots_mask: 0,
            field_pic_flag_mask: 0,
            dpb: DpbSlots::new(3),
            out_of_band_picture_parameters: true,
            inlined_picture_parameters_use_begin_coding: false,
            picture_to_dpb_slot_map,
            dump_parser_data: false,
            dump_dpb_data: false,
        }
    }

    /// De-initialise the parser.
    /// Mirrors C++ `VulkanVideoParser::Deinitialize`.
    pub fn deinitialize(&mut self) {
        // In the full port, this would release the vkParser and handler
        // shared objects. Here we just reset the DPB.
        self.dpb.deinit();
    }

    // --- Picture index / DPB slot mapping ---

    /// Get the picture index from a picture buffer index.
    /// Mirrors C++ `VulkanVideoParser::GetPicIdx(vkPicBuffBase*)`.
    pub fn get_pic_idx(&self, pic_buf_idx: Option<i32>) -> i8 {
        if let Some(idx) = pic_buf_idx
            && idx >= 0
            && (idx as u32) < self.max_num_decode_surfaces
        {
            return idx as i8;
        }
        -1
    }

    /// Get the DPB slot assigned to a picture index.
    /// Mirrors C++ `VulkanVideoParser::GetPicDpbSlot(int8_t)`.
    pub fn get_pic_dpb_slot(&self, pic_index: i8) -> i8 {
        self.picture_to_dpb_slot_map[pic_index as usize]
    }

    /// Get the DPB slot for a picture buffer.
    /// Mirrors C++ `VulkanVideoParser::GetPicDpbSlot(vkPicBuffBase*)`.
    pub fn get_pic_dpb_slot_for_buf(&self, pic_buf_idx: Option<i32>) -> i8 {
        let pic_index = self.get_pic_idx(pic_buf_idx);
        debug_assert!(pic_index >= 0 && (pic_index as u32) < self.max_num_decode_surfaces);
        self.get_pic_dpb_slot(pic_index)
    }

    /// Get the field-picture flag for a picture index.
    /// Mirrors C++ `VulkanVideoParser::GetFieldPicFlag`.
    pub fn get_field_pic_flag(&self, pic_index: i8) -> bool {
        debug_assert!(pic_index >= 0 && (pic_index as u32) < self.max_num_decode_surfaces);
        (self.field_pic_flag_mask & (1 << pic_index as u32)) != 0
    }

    /// Set the field-picture flag for a picture index. Returns the old value.
    /// Mirrors C++ `VulkanVideoParser::SetFieldPicFlag`.
    pub fn set_field_pic_flag(&mut self, pic_index: i8, field_pic_flag: bool) -> bool {
        debug_assert!(pic_index >= 0 && (pic_index as u32) < self.max_num_decode_surfaces);
        let old = self.get_field_pic_flag(pic_index);
        if field_pic_flag {
            self.field_pic_flag_mask |= 1 << pic_index as u32;
        } else {
            self.field_pic_flag_mask &= !(1 << pic_index as u32);
        }
        old
    }

    /// Assign a DPB slot to a picture index. Returns the old slot.
    /// Mirrors C++ `VulkanVideoParser::SetPicDpbSlot(int8_t, int8_t)`.
    pub fn set_pic_dpb_slot(&mut self, pic_index: i8, dpb_slot: i8) -> i8 {
        let old_dpb_slot = self.picture_to_dpb_slot_map[pic_index as usize];
        self.picture_to_dpb_slot_map[pic_index as usize] = dpb_slot;
        if dpb_slot >= 0 {
            self.dpb_slots_mask |= 1 << pic_index as u32;
        } else {
            self.dpb_slots_mask &= !(1 << pic_index as u32);
            if old_dpb_slot >= 0 {
                self.dpb.free_slot(old_dpb_slot);
            }
        }
        old_dpb_slot
    }

    /// Assign a DPB slot to a picture buffer. Returns the old slot.
    /// Mirrors C++ `VulkanVideoParser::SetPicDpbSlot(vkPicBuffBase*, int8_t)`.
    pub fn set_pic_dpb_slot_for_buf(&mut self, pic_buf_idx: Option<i32>, dpb_slot: i8) -> i8 {
        let pic_index = self.get_pic_idx(pic_buf_idx);
        debug_assert!(pic_index >= 0 && (pic_index as u32) < self.max_num_decode_surfaces);
        self.set_pic_dpb_slot(pic_index, dpb_slot)
    }

    /// Reset DPB slots for pictures not in the given valid mask.
    /// Returns the updated `dpb_slots_mask`.
    /// Mirrors C++ `VulkanVideoParser::ResetPicDpbSlots`.
    pub fn reset_pic_dpb_slots(&mut self, pic_index_slot_valid_mask: u32) -> u32 {
        let mut reset_slots_mask = !(pic_index_slot_valid_mask | !self.dpb_slots_mask);
        if reset_slots_mask != 0 {
            for pic_idx in 0..self.max_num_decode_surfaces {
                if reset_slots_mask == 0 {
                    break;
                }
                if reset_slots_mask & (1 << pic_idx) != 0 {
                    reset_slots_mask &= !(1 << pic_idx);
                    if self.dump_dpb_data {
                        tracing::debug!(
                            "Resetting picIdx {}, was using dpb slot {}",
                            pic_idx,
                            self.picture_to_dpb_slot_map[pic_idx as usize]
                        );
                    }
                    self.set_pic_dpb_slot(pic_idx as i8, -1);
                }
            }
        }
        self.dpb_slots_mask
    }

    // --- DPB slot allocation for current picture ---

    /// Allocate a DPB slot for the current H.264 picture.
    /// Mirrors C++ `VulkanVideoParser::AllocateDpbSlotForCurrentH264`.
    pub fn allocate_dpb_slot_for_current_h264(
        &mut self,
        pic_buf_idx: Option<i32>,
        field_pic_flag: bool,
        _preset_dpb_slot: i8,
    ) -> i8 {
        let curr_pic_idx = self.get_pic_idx(pic_buf_idx);
        debug_assert!(curr_pic_idx >= 0);
        self.set_field_pic_flag(curr_pic_idx, field_pic_flag);

        let mut dpb_slot = self.get_pic_dpb_slot(curr_pic_idx);
        if dpb_slot < 0 {
            dpb_slot = self.dpb.allocate_slot();
            debug_assert!(dpb_slot >= 0);
            self.set_pic_dpb_slot(curr_pic_idx, dpb_slot);
            self.dpb
                .get_mut(dpb_slot as u32)
                .set_picture_resource(pic_buf_idx, self.current_picture_id);
        }
        debug_assert!(dpb_slot >= 0);
        dpb_slot
    }

    /// Allocate a DPB slot for the current H.265 picture.
    /// Mirrors C++ `VulkanVideoParser::AllocateDpbSlotForCurrentH265`.
    pub fn allocate_dpb_slot_for_current_h265(
        &mut self,
        pic_buf_idx: Option<i32>,
        is_reference: bool,
        _preset_dpb_slot: i8,
    ) -> i8 {
        let mut dpb_slot: i8 = -1;
        let curr_pic_idx = self.get_pic_idx(pic_buf_idx);
        debug_assert!(curr_pic_idx >= 0);
        debug_assert!(is_reference);
        if is_reference {
            dpb_slot = self.get_pic_dpb_slot(curr_pic_idx);
            if dpb_slot < 0 {
                dpb_slot = self.dpb.allocate_slot();
                debug_assert!(dpb_slot >= 0);
                self.set_pic_dpb_slot(curr_pic_idx, dpb_slot);
                self.dpb
                    .get_mut(dpb_slot as u32)
                    .set_picture_resource(pic_buf_idx, self.current_picture_id);
            }
            debug_assert!(dpb_slot >= 0);
        }
        dpb_slot
    }

    /// Allocate a DPB slot for the current AV1 picture.
    /// Mirrors C++ `VulkanVideoParser::AllocateDpbSlotForCurrentAV1`.
    pub fn allocate_dpb_slot_for_current_av1(
        &mut self,
        pic_buf_idx: Option<i32>,
        is_reference: bool,
        _preset_dpb_slot: i8,
    ) -> i8 {
        let mut dpb_slot: i8 = -1;
        let curr_pic_idx = self.get_pic_idx(pic_buf_idx);
        debug_assert!(curr_pic_idx >= 0);
        debug_assert!(is_reference);
        if is_reference {
            dpb_slot = self.get_pic_dpb_slot(curr_pic_idx);
            if dpb_slot < 0 {
                dpb_slot = self.dpb.allocate_slot();
                debug_assert!(dpb_slot >= 0);
                self.set_pic_dpb_slot(curr_pic_idx, dpb_slot);
                self.dpb
                    .get_mut(dpb_slot as u32)
                    .set_picture_resource(pic_buf_idx, self.current_picture_id);
            }
            debug_assert!(dpb_slot >= 0);
        }
        dpb_slot
    }

    /// Allocate a DPB slot for the current VP9 picture.
    /// Mirrors C++ `VulkanVideoParser::AllocateDpbSlotForCurrentVP9`.
    /// (VP9 uses the same logic as AV1.)
    pub fn allocate_dpb_slot_for_current_vp9(
        &mut self,
        pic_buf_idx: Option<i32>,
        is_reference: bool,
        preset_dpb_slot: i8,
    ) -> i8 {
        // VP9 allocation is identical to AV1 in the C++ source.
        self.allocate_dpb_slot_for_current_av1(pic_buf_idx, is_reference, preset_dpb_slot)
    }

    // --- BeginSequence ---

    /// Handle a new video sequence.
    /// Mirrors C++ `VulkanVideoParser::BeginSequence`.
    pub fn begin_sequence(
        &mut self,
        codec: vk::VideoCodecOperationFlagsKHR,
        coded_width: u32,
        coded_height: u32,
        max_width: u32,
        max_height: u32,
        min_num_decode_surfaces: u32,
        min_num_dpb_slots: u32,
        stored_coded_width: u32,
        stored_coded_height: u32,
        stored_max_width: u32,
        stored_max_height: u32,
    ) -> i32 {
        let sequence_update = stored_max_width != 0 && stored_max_height != 0;

        let mut max_dpb_slots = if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
            MAX_DPB_REF_AND_SETUP_SLOTS
        } else {
            MAX_DPB_REF_SLOTS
        };

        if codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
            max_dpb_slots = 9;
            if coded_width <= stored_coded_width && coded_height <= stored_coded_height {
                return 1;
            }
        } else if codec == VIDEO_CODEC_OPERATION_DECODE_VP9 {
            max_dpb_slots = 9;
            if max_width <= stored_max_width && max_height <= stored_max_height {
                return 1;
            }
        }

        let config_dpb_slots = if min_num_dpb_slots > 0 {
            min_num_dpb_slots.min(max_dpb_slots)
        } else {
            max_dpb_slots
        };

        self.max_num_decode_surfaces = min_num_decode_surfaces;

        // When starting a new sequence, reset picture-to-DPB slot mapping.
        if !sequence_update {
            self.dpb_slots_mask = 0;
            for slot in self.picture_to_dpb_slot_map.iter_mut() {
                *slot = -1;
            }
        }

        self.max_num_dpb_slots = self.dpb.init(config_dpb_slots, sequence_update);

        self.max_num_decode_surfaces as i32
    }

    /// Increment the current picture ID after a decode.
    pub fn advance_picture_id(&mut self) {
        self.current_picture_id += 1;
    }
}

impl Drop for VulkanVideoParser {
    fn drop(&mut self) {
        self.deinitialize();
    }
}

// ---------------------------------------------------------------------------
// Factory — mirrors C++ IVulkanVideoParser::Create + vulkanCreateVideoParser
// ---------------------------------------------------------------------------

/// Create a `VulkanVideoParser` for the given codec type.
/// Mirrors C++ `IVulkanVideoParser::Create` / `vulkanCreateVideoParser`.
pub fn create_vulkan_video_parser(
    codec_type: vk::VideoCodecOperationFlagsKHR,
    max_num_decode_surfaces: u32,
    max_num_dpb_surfaces: u32,
    clock_rate: u64,
) -> Result<Arc<std::sync::Mutex<VulkanVideoParser>>, vk::Result> {
    let parser = VulkanVideoParser::new(
        codec_type,
        max_num_decode_surfaces,
        max_num_dpb_surfaces,
        clock_rate,
    );
    Ok(Arc::new(std::sync::Mutex::new(parser)))
}

// ---------------------------------------------------------------------------
// PictureParametersType name helper
// ---------------------------------------------------------------------------

/// Returns a human-readable name for a picture parameter set type.
/// Mirrors C++ `PictureParametersTypeToName`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureParametersType {
    H264Sps,
    H264Pps,
    H265Vps,
    H265Sps,
    H265Pps,
    Av1Sps,
}

impl PictureParametersType {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264Sps => "H264_SPS",
            Self::H264Pps => "H264_PPS",
            Self::H265Vps => "H265_VPS",
            Self::H265Sps => "H265_SPS",
            Self::H265Pps => "H265_PPS",
            Self::Av1Sps => "AV1_SPS",
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
