// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoder.h + VkVideoEncoder.cpp
//!
//! Main encoder pipeline: creates video sessions, manages DPB image pools,
//! submits encode commands, and orchestrates the frame encoding lifecycle.

use std::sync::atomic::{AtomicU32, AtomicU64};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum reference pictures: 16 DPB slots + 1 for current picture.
pub const MAX_IMAGE_REF_RESOURCES: usize = 17;

/// Maximum bitstream header buffer size (SPS/PPS/VPS non-VCL data).
pub const MAX_BITSTREAM_HEADER_BUFFER_SIZE: usize = 256;

// ---------------------------------------------------------------------------
// Timeline Semaphore Synchronization
// ---------------------------------------------------------------------------

/// Synchronization state indices for timeline semaphores.
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    InputPreprocessingComplete = 1,
    AqProcessingComplete = 2,
    EncodeProcessingComplete = 3,
    AssemblyProcessingComplete = 4,
}

/// Total number of sync states (used for static assertions).
pub const SYNC_PROCESSING_STATE_COUNT: u64 = 5;

/// Shift amount for encoding stage in timeline semaphore value.
/// We have 5 sync states requiring 3 bits (2^3 = 8).
pub const SEM_SYNC_TYPE_IDX_SHIFT: u64 = 3;

// Static assert equivalent
const _: () = assert!(SYNC_PROCESSING_STATE_COUNT <= (1u64 << SEM_SYNC_TYPE_IDX_SHIFT));

/// Calculate timeline semaphore value from frame number and stage.
#[inline]
pub fn get_semaphore_value(stage: SyncState, frame_number: u64) -> u64 {
    (frame_number << SEM_SYNC_TYPE_IDX_SHIFT) | (stage as u64)
}

/// Extract frame number from timeline semaphore value.
#[inline]
pub fn get_frame_number_from_semaphore(semaphore_value: u64) -> u64 {
    semaphore_value >> SEM_SYNC_TYPE_IDX_SHIFT
}

/// Extract stage from timeline semaphore value.
#[inline]
pub fn get_stage_from_semaphore(semaphore_value: u64) -> SyncState {
    let mask = (1u64 << SEM_SYNC_TYPE_IDX_SHIFT) - 1;
    match semaphore_value & mask {
        1 => SyncState::InputPreprocessingComplete,
        2 => SyncState::AqProcessingComplete,
        3 => SyncState::EncodeProcessingComplete,
        4 => SyncState::AssemblyProcessingComplete,
        _ => SyncState::InputPreprocessingComplete, // fallback
    }
}

// ---------------------------------------------------------------------------
// Bitstream Readback
// ---------------------------------------------------------------------------

/// Result of reading back encoded bitstream data from the GPU.
#[derive(Debug, Default)]
pub struct BitstreamReadback {
    pub bitstream_start_offset: u32,
    pub bitstream_size: u32,
    pub status: i32, // VkQueryResultStatusKHR
    pub bitstream_copy: Vec<u8>,
    pub readback_done: bool,
}

// ---------------------------------------------------------------------------
// Constant QP Settings
// ---------------------------------------------------------------------------

/// Per-frame-type constant QP values used when rate control is disabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConstQpSettings {
    pub qp_intra: i32,
    pub qp_inter_p: i32,
    pub qp_inter_b: i32,
}

// ---------------------------------------------------------------------------
// GOP Position
// ---------------------------------------------------------------------------

/// Frame type within a GOP structure.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameType {
    #[default]
    Invalid = 0,
    Idr = 1,
    I = 2,
    P = 3,
    B = 4,
    IntraRefresh = 5,
}

/// Position of a frame within the GOP structure.
#[derive(Debug, Clone, Copy, Default)]
pub struct GopPosition {
    pub input_order: u32,
    pub encode_order: u32,
    pub picture_type: FrameType,
    pub intra_refresh_index: u32,
}

// ---------------------------------------------------------------------------
// VkVideoEncodeFrameInfo
// ---------------------------------------------------------------------------

/// Codec type for a frame info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecType {
    None,
    H264,
    H265,
    Av1,
}

/// Base encode frame info — shared across all codecs.
///
/// Ports `VkVideoEncodeFrameInfo` from the C++ base class.
/// Codec-specific subclasses (H264, H265, AV1) embed this and add their own fields.
#[derive(Debug)]
pub struct VkVideoEncodeFrameInfo {
    pub codec: CodecType,

    // Frame ordering
    pub frame_input_order_num: u64,
    pub frame_encode_input_order_num: u64,
    pub frame_encode_encode_order_num: u64,
    pub gop_position: GopPosition,
    pub pic_order_cnt_val: i32,
    pub input_time_stamp: u64,

    // Bitstream header
    pub bitstream_header_buffer_size: usize,
    pub bitstream_header_offset: u32,
    pub bitstream_header_buffer: [u8; MAX_BITSTREAM_HEADER_BUFFER_SIZE],

    // QP / quality
    pub const_qp: ConstQpSettings,
    pub quality_level: u32,

    // Flags (bitfields in C++)
    pub is_long_term_reference: bool,
    pub send_control_cmd: bool,
    pub send_reset_control_cmd: bool,
    pub send_quality_level_cmd: bool,
    pub send_rate_control_cmd: bool,
    pub last_frame: bool,

    pub num_dpb_image_resources: u32,

    // Reference slots (indices into DPB)
    pub reference_slots: [i8; MAX_IMAGE_REF_RESOURCES],
    pub setup_reference_slot_index: i8,

    // External input support
    pub is_external_input: bool,
}

impl Default for VkVideoEncodeFrameInfo {
    fn default() -> Self {
        Self {
            codec: CodecType::None,
            frame_input_order_num: u64::MAX,
            frame_encode_input_order_num: u64::MAX,
            frame_encode_encode_order_num: u64::MAX,
            gop_position: GopPosition {
                input_order: u32::MAX,
                encode_order: u32::MAX,
                picture_type: FrameType::Invalid,
                intra_refresh_index: 0,
            },
            pic_order_cnt_val: -1,
            input_time_stamp: 0,
            bitstream_header_buffer_size: 0,
            bitstream_header_offset: 0,
            bitstream_header_buffer: [0u8; MAX_BITSTREAM_HEADER_BUFFER_SIZE],
            const_qp: ConstQpSettings::default(),
            quality_level: 0,
            is_long_term_reference: false,
            send_control_cmd: false,
            send_reset_control_cmd: false,
            send_quality_level_cmd: false,
            send_rate_control_cmd: false,
            last_frame: false,
            num_dpb_image_resources: 0,
            reference_slots: [-1i8; MAX_IMAGE_REF_RESOURCES],
            setup_reference_slot_index: -1,
            is_external_input: false,
        }
    }
}

impl VkVideoEncodeFrameInfo {
    /// Reset frame info to default state, optionally releasing resources.
    pub fn reset(&mut self, _release_resources: bool) {
        self.frame_input_order_num = u64::MAX;
        self.frame_encode_input_order_num = u64::MAX;
        self.frame_encode_encode_order_num = u64::MAX;
        self.gop_position.input_order = u32::MAX;
        self.gop_position.encode_order = u32::MAX;
        self.gop_position.picture_type = FrameType::Invalid;
        self.pic_order_cnt_val = -1;
        self.input_time_stamp = u64::MAX;
        self.bitstream_header_buffer_size = 0;
        self.bitstream_header_offset = 0;
        self.quality_level = 0;
        self.is_long_term_reference = false;
        self.send_control_cmd = false;
        self.send_reset_control_cmd = false;
        self.send_quality_level_cmd = false;
        self.send_rate_control_cmd = false;
        self.last_frame = false;
        self.num_dpb_image_resources = 0;
        self.reference_slots = [-1i8; MAX_IMAGE_REF_RESOURCES];
        self.setup_reference_slot_index = -1;
    }
}

// ---------------------------------------------------------------------------
// VkVideoEncoder (base)
// ---------------------------------------------------------------------------

/// Main encoder state — base structure shared across codec-specific encoders.
///
/// Ports `VkVideoEncoder` from the C++ class hierarchy.
/// The C++ version uses virtual dispatch; the Rust port uses an enum or trait
/// for codec-specific behavior (implemented by the H264/H265/AV1 modules).
pub struct VkVideoEncoder {
    // Frame counters
    pub input_frame_num: u64,
    pub encode_input_frame_num: u64,
    pub encode_encode_frame_num: u64,

    // Session configuration
    pub max_dpb_pictures_count: u32,
    pub min_stream_buffer_size: usize,
    pub stream_buffer_size: usize,

    // DPB tracking
    pub pic_idx_to_dpb: [i8; 17], // MAX_DPB_SLOTS + 1
    pub dpb_slots_mask: u32,
    pub frame_num_syntax: u32,
    pub frame_num_in_gop: u32,
    pub idr_pic_id: u32,

    // Feature flags (bitfields in C++)
    pub video_maintenance1_features_supported: bool,
    pub send_control_cmd: bool,
    pub send_reset_control_cmd: bool,
    pub send_quality_level_cmd: bool,
    pub send_rate_control_cmd: bool,
    pub use_image_array: bool,
    pub use_image_view_array: bool,
    pub use_separate_output_images: bool,
    pub use_linear_input: bool,
    pub reset_encoder: bool,
    pub enable_encoder_thread_queue: bool,
    pub verbose: bool,

    // Deferred frame management
    pub num_deferred_frames: u32,
    pub num_deferred_ref_frames: u32,
    pub hold_ref_frames_in_queue: u32,

    // Assembly
    pub async_assembly_enabled: bool,
    pub assembly_sequence_counter: AtomicU64,
    pub next_write_sequence: AtomicU64,
    pub assembly_error_count: AtomicU32,
}

impl Default for VkVideoEncoder {
    fn default() -> Self {
        Self {
            input_frame_num: 0,
            encode_input_frame_num: 0,
            encode_encode_frame_num: 0,
            max_dpb_pictures_count: 16,
            min_stream_buffer_size: 2 * 1024 * 1024,
            stream_buffer_size: 2 * 1024 * 1024,
            pic_idx_to_dpb: [-1i8; 17],
            dpb_slots_mask: 0,
            frame_num_syntax: 0,
            frame_num_in_gop: 0,
            idr_pic_id: 0,
            video_maintenance1_features_supported: false,
            send_control_cmd: true,
            send_reset_control_cmd: true,
            send_quality_level_cmd: true,
            send_rate_control_cmd: true,
            use_image_array: false,
            use_image_view_array: false,
            use_separate_output_images: false,
            use_linear_input: false,
            reset_encoder: false,
            enable_encoder_thread_queue: false,
            verbose: false,
            num_deferred_frames: 0,
            num_deferred_ref_frames: 0,
            hold_ref_frames_in_queue: 1,
            async_assembly_enabled: false,
            assembly_sequence_counter: AtomicU64::new(0),
            next_write_sequence: AtomicU64::new(0),
            assembly_error_count: AtomicU32::new(0),
        }
    }
}

impl VkVideoEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the frame type name for debug logging.
    pub fn get_frame_type_name(frame_type: FrameType) -> &'static str {
        match frame_type {
            FrameType::Invalid => "INVALID",
            FrameType::Idr => "IDR",
            FrameType::I => "I",
            FrameType::P => "P",
            FrameType::B => "B",
            FrameType::IntraRefresh => "INTRA_REFRESH",
        }
    }

    /// Check if a frame is a reference frame based on GOP position.
    pub fn is_frame_reference(gop_position: &GopPosition) -> bool {
        matches!(
            gop_position.picture_type,
            FrameType::Idr | FrameType::I | FrameType::P | FrameType::IntraRefresh
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a little-endian u32 to a byte buffer.
#[inline]
pub fn mem_put_le32(mem: &mut [u8], val: u32) {
    mem[0] = (val & 0xff) as u8;
    mem[1] = ((val >> 8) & 0xff) as u8;
    mem[2] = ((val >> 16) & 0xff) as u8;
    mem[3] = ((val >> 24) & 0xff) as u8;
}

/// Write a little-endian u16 to a byte buffer.
#[inline]
pub fn mem_put_le16(mem: &mut [u8], val: u16) {
    mem[0] = (val & 0xff) as u8;
    mem[1] = ((val >> 8) & 0xff) as u8;
}

/// Make a FOURCC value from 4 ASCII characters.
#[inline]
pub const fn make_fourcc(ch0: u8, ch1: u8, ch2: u8, ch3: u8) -> u32 {
    (ch0 as u32) | ((ch1 as u32) << 8) | ((ch2 as u32) << 16) | ((ch3 as u32) << 24)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semaphore_value_roundtrip() {
        for frame in [0u64, 1, 100, 1000, u64::MAX >> SEM_SYNC_TYPE_IDX_SHIFT] {
            for stage in [
                SyncState::InputPreprocessingComplete,
                SyncState::AqProcessingComplete,
                SyncState::EncodeProcessingComplete,
                SyncState::AssemblyProcessingComplete,
            ] {
                let val = get_semaphore_value(stage, frame);
                assert_eq!(get_frame_number_from_semaphore(val), frame);
                assert_eq!(get_stage_from_semaphore(val), stage);
            }
        }
    }

    #[test]
    fn test_frame_info_default() {
        let info = VkVideoEncodeFrameInfo::default();
        assert_eq!(info.codec, CodecType::None);
        assert_eq!(info.frame_input_order_num, u64::MAX);
        assert_eq!(info.pic_order_cnt_val, -1);
        assert!(!info.last_frame);
        assert_eq!(info.num_dpb_image_resources, 0);
    }

    #[test]
    fn test_frame_info_reset() {
        let mut info = VkVideoEncodeFrameInfo::default();
        info.frame_input_order_num = 42;
        info.pic_order_cnt_val = 10;
        info.last_frame = true;
        info.num_dpb_image_resources = 5;
        info.reset(true);
        assert_eq!(info.frame_input_order_num, u64::MAX);
        assert_eq!(info.pic_order_cnt_val, -1);
        assert!(!info.last_frame);
        assert_eq!(info.num_dpb_image_resources, 0);
    }

    #[test]
    fn test_mem_put_le32() {
        let mut buf = [0u8; 4];
        mem_put_le32(&mut buf, 0x04030201);
        assert_eq!(buf, [0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn test_mem_put_le16() {
        let mut buf = [0u8; 2];
        mem_put_le16(&mut buf, 0x0201);
        assert_eq!(buf, [0x01, 0x02]);
    }

    #[test]
    fn test_make_fourcc() {
        assert_eq!(make_fourcc(b'D', b'K', b'I', b'F'), 0x46494b44);
    }

    #[test]
    fn test_frame_type_name() {
        assert_eq!(VkVideoEncoder::get_frame_type_name(FrameType::Idr), "IDR");
        assert_eq!(VkVideoEncoder::get_frame_type_name(FrameType::B), "B");
    }

    #[test]
    fn test_is_frame_reference() {
        let mut pos = GopPosition::default();
        pos.picture_type = FrameType::Idr;
        assert!(VkVideoEncoder::is_frame_reference(&pos));

        pos.picture_type = FrameType::B;
        assert!(!VkVideoEncoder::is_frame_reference(&pos));

        pos.picture_type = FrameType::P;
        assert!(VkVideoEncoder::is_frame_reference(&pos));
    }

    #[test]
    fn test_encoder_default() {
        let enc = VkVideoEncoder::new();
        assert_eq!(enc.max_dpb_pictures_count, 16);
        assert_eq!(enc.min_stream_buffer_size, 2 * 1024 * 1024);
        assert!(enc.send_control_cmd);
        assert_eq!(enc.hold_ref_frames_in_queue, 1);
    }
}
