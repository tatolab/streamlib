// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderAV1.h + VkVideoEncoderAV1.cpp
//!
//! AV1 codec-specific encoder.
//! Manages AV1 frame info (tile info, quantization, CDEF, loop filter,
//! loop restoration), DPB processing via VkEncDpbAV1, reference frame
//! group construction, show-existing-frame support, IVF container writing,
//! and the AV1 bitstream bit-writer for OBU headers.

use crate::vk_video_encoder::vk_encoder_dpb_av1::{
    VkEncDpbAV1, Av1FrameType, REFS_PER_FRAME, NUM_REF_FRAMES,
};
use crate::vk_video_encoder::vk_video_encoder::{
    VkVideoEncodeFrameInfo, VkVideoEncoder, CodecType,
    mem_put_le32, mem_put_le16, make_fourcc,
};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_TILE_ROWS: usize = 64; // STD_VIDEO_AV1_MAX_TILE_ROWS
pub const MAX_TILE_COLS: usize = 64; // STD_VIDEO_AV1_MAX_TILE_COLS

// ---------------------------------------------------------------------------
// AV1 Bit Writer
// ---------------------------------------------------------------------------

/// AV1 bitstream bit writer for OBU headers and show-existing-frame headers.
pub struct Av1BitWriter {
    buffer: Vec<u8>,
    byte_data: u8,
    bit_count: u8,
}

impl Av1BitWriter {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            byte_data: 0,
            bit_count: 0,
        }
    }

    /// Write `len` bits from `code`.
    pub fn put_bits(&mut self, code: i32, len: i32) {
        for i in (0..len).rev() {
            let mask = 1u32 << i;
            let bit = if (code as u32 & mask) != 0 { 1u8 } else { 0u8 };
            self.byte_data = (self.byte_data << 1) | bit;
            self.bit_count += 1;

            if self.bit_count >= 8 {
                self.buffer.push(self.byte_data);
                self.byte_data = 0;
                self.bit_count = 0;
            }
        }
    }

    /// Write trailing bits (1 followed by zeros to byte-align).
    pub fn put_trailing_bits(&mut self) {
        self.put_bits(1, 1);
        if self.bit_count > 0 {
            self.byte_data <<= 8 - self.bit_count;
            self.buffer.push(self.byte_data);
            self.byte_data = 0;
            self.bit_count = 0;
        }
    }

    /// Write a LEB128-encoded size.
    pub fn put_leb128(&mut self, mut size: u32) {
        debug_assert_eq!(self.bit_count, 0);
        while size >> 7 != 0 {
            self.buffer.push(0x80 | (size as u8 & 0x7f));
            size >>= 7;
        }
        self.buffer.push(size as u8);
    }

    /// Get the written data.
    pub fn data(&self) -> &[u8] {
        &self.buffer
    }

    /// Get the size of written data.
    pub fn len(&self) -> usize {
        self.buffer.len()
    }
}

// ---------------------------------------------------------------------------
// VkVideoEncodeFrameInfoAV1
// ---------------------------------------------------------------------------

/// AV1-specific per-frame encode info.
#[derive(Debug)]
pub struct VkVideoEncodeFrameInfoAV1 {
    pub base: VkVideoEncodeFrameInfo,

    // AV1 picture info
    pub frame_type: Av1FrameType,
    pub current_frame_id: u32,
    pub order_hint: u8,
    pub primary_ref_frame: u8,
    pub refresh_frame_flags: u8,

    // Reference frame slot indices (referenceNameSlotIndices in C++)
    pub reference_name_slot_indices: [i32; REFS_PER_FRAME],

    // Show existing frame
    pub show_existing_frame: bool,
    pub frame_to_show_buf_id: i32,
    pub shown_key_frame_or_switch: bool,
    pub overlay_frame: bool,

    // Flags
    pub show_frame: bool,
    pub showable_frame: bool,
    pub error_resilient_mode: bool,

    // Ref frame indices
    pub ref_frame_idx: [i8; REFS_PER_FRAME],
    pub ref_order_hint: [u8; NUM_REF_FRAMES],
    pub delta_frame_id_minus_1: [i32; REFS_PER_FRAME],

    // Prediction mode
    pub prediction_mode: u32,
    pub rate_control_group: u32,
    pub primary_reference_cdf_only: bool,
    pub constant_q_index: u8,

    // Tile info
    pub enable_tiles: bool,
    pub tile_rows: u32,
    pub tile_cols: u32,
    pub uniform_tile_spacing: bool,

    // Quantization
    pub base_q_idx: u8,
}

impl Default for VkVideoEncodeFrameInfoAV1 {
    fn default() -> Self {
        Self {
            base: VkVideoEncodeFrameInfo { codec: CodecType::Av1, ..Default::default() },
            frame_type: Av1FrameType::Key,
            current_frame_id: 0,
            order_hint: 0,
            primary_ref_frame: 7, // STD_VIDEO_AV1_PRIMARY_REF_NONE
            refresh_frame_flags: 0,
            reference_name_slot_indices: [-1; REFS_PER_FRAME],
            show_existing_frame: false,
            frame_to_show_buf_id: -1,
            shown_key_frame_or_switch: false,
            overlay_frame: false,
            show_frame: true,
            showable_frame: false,
            error_resilient_mode: false,
            ref_frame_idx: [-1; REFS_PER_FRAME],
            ref_order_hint: [0; NUM_REF_FRAMES],
            delta_frame_id_minus_1: [0; REFS_PER_FRAME],
            prediction_mode: 0,
            rate_control_group: 0,
            primary_reference_cdf_only: false,
            constant_q_index: 0,
            enable_tiles: false,
            tile_rows: 0,
            tile_cols: 0,
            uniform_tile_spacing: true,
            base_q_idx: 0,
        }
    }
}

impl VkVideoEncodeFrameInfoAV1 {
    pub fn reset(&mut self, release_resources: bool) {
        self.base.reset(release_resources);
        self.reference_name_slot_indices = [-1; REFS_PER_FRAME];
        self.show_existing_frame = false;
        self.frame_to_show_buf_id = -1;
    }
}

// ---------------------------------------------------------------------------
// VkVideoEncoderAV1
// ---------------------------------------------------------------------------

/// AV1 encoder — codec-specific implementation.
pub struct VkVideoEncoderAV1 {
    pub base: VkVideoEncoder,
    pub dpb_av1: Option<Box<VkEncDpbAV1>>,

    pub last_key_frame_order_hint: i32,
    pub num_b_frames_to_encode: u32,
    pub batch_frames_idx_set_to_assemble: BTreeSet<u32>,
    pub bitstream: Vec<Vec<u8>>,

    // Config
    pub encode_width: u32,
    pub encode_height: u32,
    pub frame_rate_numerator: u32,
    pub frame_rate_denominator: u32,
    pub num_frames: u32,
}

impl VkVideoEncoderAV1 {
    pub fn new() -> Self {
        Self {
            base: VkVideoEncoder::new(),
            dpb_av1: None,
            last_key_frame_order_hint: 0,
            num_b_frames_to_encode: 0,
            batch_frames_idx_set_to_assemble: BTreeSet::new(),
            bitstream: Vec::new(),
            encode_width: 0,
            encode_height: 0,
            frame_rate_numerator: 30,
            frame_rate_denominator: 1,
            num_frames: 0,
        }
    }

    /// Initialize the AV1 DPB.
    pub fn init_dpb(&mut self, max_dpb_size: u32, num_b_frames: i32) {
        let mut dpb = VkEncDpbAV1::create_instance();
        dpb.dpb_sequence_start(max_dpb_size, num_b_frames);
        self.dpb_av1 = Some(dpb);
    }

    /// Build an IVF file header.
    pub fn build_ivf_header(&self) -> [u8; 32] {
        let mut header = [0u8; 32];
        mem_put_le32(&mut header[0..4], make_fourcc(b'D', b'K', b'I', b'F'));
        mem_put_le16(&mut header[4..6], 0);
        mem_put_le16(&mut header[6..8], 32);
        mem_put_le32(&mut header[8..12], make_fourcc(b'A', b'V', b'0', b'1'));
        mem_put_le16(&mut header[12..14], self.encode_width as u16);
        mem_put_le16(&mut header[14..16], self.encode_height as u16);
        mem_put_le32(&mut header[16..20], self.frame_rate_numerator);
        mem_put_le32(&mut header[20..24], self.frame_rate_denominator);
        mem_put_le32(&mut header[24..28], self.num_frames);
        mem_put_le32(&mut header[28..32], 0);
        header
    }

    /// Build an IVF frame header.
    pub fn build_ivf_frame_header(frame_size: u32, pts: u64) -> [u8; 12] {
        let mut header = [0u8; 12];
        mem_put_le32(&mut header[0..4], frame_size);
        mem_put_le32(&mut header[4..8], (pts & 0xffffffff) as u32);
        mem_put_le32(&mut header[8..12], (pts >> 32) as u32);
        header
    }

    /// Destroy the encoder.
    pub fn destroy(&mut self) {
        if let Some(ref mut dpb) = self.dpb_av1 {
            dpb.dpb_destroy();
        }
        self.dpb_av1 = None;
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bit_writer_put_bits() {
        let mut w = Av1BitWriter::new();
        w.put_bits(0b11001010, 8);
        assert_eq!(w.data(), &[0b11001010]);
    }

    #[test]
    fn test_bit_writer_partial_byte() {
        let mut w = Av1BitWriter::new();
        w.put_bits(0b101, 3);
        w.put_trailing_bits();
        // 101 + 1 + 0000 = 10110000
        assert_eq!(w.data(), &[0b10110000]);
    }

    #[test]
    fn test_bit_writer_leb128() {
        let mut w = Av1BitWriter::new();
        w.put_leb128(127);
        assert_eq!(w.data(), &[127]);

        let mut w2 = Av1BitWriter::new();
        w2.put_leb128(128);
        assert_eq!(w2.data(), &[0x80, 0x01]);
    }

    #[test]
    fn test_frame_info_av1_default() {
        let info = VkVideoEncodeFrameInfoAV1::default();
        assert_eq!(info.base.codec, CodecType::Av1);
        assert_eq!(info.primary_ref_frame, 7);
        assert!(info.show_frame);
        assert_eq!(info.reference_name_slot_indices, [-1; REFS_PER_FRAME]);
    }

    #[test]
    fn test_encoder_av1_new() {
        let enc = VkVideoEncoderAV1::new();
        assert!(enc.dpb_av1.is_none());
        assert_eq!(enc.frame_rate_numerator, 30);
    }

    #[test]
    fn test_encoder_av1_init_dpb() {
        let mut enc = VkVideoEncoderAV1::new();
        enc.init_dpb(8, 0);
        assert!(enc.dpb_av1.is_some());
    }

    #[test]
    fn test_ivf_header() {
        let mut enc = VkVideoEncoderAV1::new();
        enc.encode_width = 1920;
        enc.encode_height = 1080;
        enc.frame_rate_numerator = 30;
        enc.frame_rate_denominator = 1;
        enc.num_frames = 100;
        let header = enc.build_ivf_header();
        // Check DKIF signature
        assert_eq!(header[0], b'D');
        assert_eq!(header[1], b'K');
        assert_eq!(header[2], b'I');
        assert_eq!(header[3], b'F');
    }

    #[test]
    fn test_ivf_frame_header() {
        let header = VkVideoEncoderAV1::build_ivf_frame_header(1024, 42);
        assert_eq!(header[0], 0x00); // 1024 LE
        assert_eq!(header[1], 0x04);
    }
}
