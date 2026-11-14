// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// -----------------------------------------------------------------------
// BitstreamReader tests
// -----------------------------------------------------------------------

#[test]
fn bitstream_reader_single_bits() {
    // 0b10110100 = 0xB4
    let data = [0xB4u8];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(bs.u(1), 1);
    assert_eq!(bs.u(1), 0);
    assert_eq!(bs.u(1), 1);
    assert_eq!(bs.u(1), 1);
    assert_eq!(bs.u(1), 0);
    assert_eq!(bs.u(1), 1);
    assert_eq!(bs.u(1), 0);
    assert_eq!(bs.u(1), 0);
}

#[test]
fn bitstream_reader_multi_bit() {
    // 0xAB = 0b10101011, 0xCD = 0b11001101
    let data = [0xAB, 0xCD];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(bs.u(4), 0b1010); // 0xA
    assert_eq!(bs.u(4), 0b1011); // 0xB
    assert_eq!(bs.u(8), 0xCD);
}

#[test]
fn bitstream_reader_cross_byte_boundary() {
    let data = [0xFF, 0x00];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(bs.u(4), 0xF);
    // Next 8 bits span the byte boundary: 4 bits of 0xFF (low nibble = 0xF)
    // plus 4 bits of 0x00 (high nibble = 0x0).
    assert_eq!(bs.u(8), 0xF0);
}

#[test]
fn bitstream_reader_consumed_bits() {
    let data = [0x00; 4];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(bs.consumed_bits(), 0);
    bs.u(3);
    assert_eq!(bs.consumed_bits(), 3);
    bs.u(16);
    assert_eq!(bs.consumed_bits(), 19);
}

#[test]
fn bitstream_reader_past_end_returns_zero() {
    let data = [0xFF];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(bs.u(8), 0xFF);
    // Reading past the end should produce zeros
    assert_eq!(bs.u(8), 0x00);
}

#[test]
fn bitstream_reader_24_bit() {
    // Read the VP9 sync code
    let data = [0x49, 0x83, 0x42]; // VP9_FRAME_SYNC_CODE = 0x498342
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(bs.u(24), VP9_FRAME_SYNC_CODE);
}

// -----------------------------------------------------------------------
// Frame marker validation
// -----------------------------------------------------------------------

#[test]
fn parse_uncompressed_header_rejects_bad_frame_marker() {
    let mut dec = VulkanVp9Decoder::new();
    // Frame marker should be 2 (0b10). Use 0b11 = 3 to trigger failure.
    // 0b11_000000 = 0xC0
    let data = [0xC0, 0x00, 0x00, 0x00];
    let mut bs = BitstreamReader::new(&data);
    assert!(!dec.parse_uncompressed_header(&mut bs));
}

#[test]
fn parse_uncompressed_header_accepts_valid_frame_marker() {
    // Build a minimal keyframe header:
    // 2 bits frame_marker = 0b10
    // 2 bits profile = 0b00  (Profile 0)
    // 1 bit show_existing_frame = 1
    // 3 bits frame_to_show_map_idx = 0b000
    // = 0b10_00_1_000 = 0x88
    let data = [0x88, 0x00, 0x00, 0x00];
    let mut dec = VulkanVp9Decoder::new();
    let mut bs = BitstreamReader::new(&data);
    assert!(dec.parse_uncompressed_header(&mut bs));
    assert!(dec.pic_data.show_existing_frame);
    assert_eq!(dec.pic_data.frame_to_show_map_idx, 0);
}

// -----------------------------------------------------------------------
// show_existing_frame
// -----------------------------------------------------------------------

#[test]
fn parse_show_existing_frame() {
    // frame_marker=0b10, profile=0b00, show_existing_frame=1, frame_to_show_map_idx=0b101
    // = 0b10_00_1_101 = 0x8D
    let data = [0x8D, 0x00];
    let mut dec = VulkanVp9Decoder::new();
    let mut bs = BitstreamReader::new(&data);
    assert!(dec.parse_uncompressed_header(&mut bs));
    assert!(dec.pic_data.show_existing_frame);
    assert_eq!(dec.pic_data.frame_to_show_map_idx, 5);
    assert_eq!(dec.pic_data.std_picture_info.refresh_frame_flags, 0);
}

// -----------------------------------------------------------------------
// ReadDeltaQ
// -----------------------------------------------------------------------

#[test]
fn read_delta_q_zero() {
    // First bit = 0 means delta = 0
    let data = [0x00];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanVp9Decoder::read_delta_q(&mut bs), 0);
    assert_eq!(bs.consumed_bits(), 1);
}

#[test]
fn read_delta_q_positive() {
    // 1 bit present=1, 4 bits value=0b1010 (10), 1 bit sign=0 (positive)
    // = 0b1_1010_0_xx = 0b11010000 = 0xD0
    let data = [0xD0];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanVp9Decoder::read_delta_q(&mut bs), 10);
}

#[test]
fn read_delta_q_negative() {
    // 1 bit present=1, 4 bits value=0b0011 (3), 1 bit sign=1 (negative)
    // = 0b1_0011_1_xx = 0b10011100 = 0x9C
    let data = [0x9C];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanVp9Decoder::read_delta_q(&mut bs), -3);
}

// -----------------------------------------------------------------------
// Tile column log2 calculations
// -----------------------------------------------------------------------

#[test]
fn calc_min_log2_tile_cols_small() {
    // sb64_cols = 1 -> VP9_MAX_TILE_WIDTH_B64 (64) << 0 = 64 >= 1, so min = 0
    assert_eq!(VulkanVp9Decoder::calc_min_log2_tile_cols(1), 0);
}

#[test]
fn calc_min_log2_tile_cols_large() {
    // sb64_cols = 128 -> 64 << 0 = 64 < 128, need 64 << 1 = 128 >= 128, so min = 1
    assert_eq!(VulkanVp9Decoder::calc_min_log2_tile_cols(128), 1);
}

#[test]
fn calc_min_log2_tile_cols_exact() {
    // sb64_cols = 64 -> 64 << 0 = 64 >= 64, so min = 0
    assert_eq!(VulkanVp9Decoder::calc_min_log2_tile_cols(64), 0);
}

#[test]
fn calc_max_log2_tile_cols_small() {
    // sb64_cols = 4 -> (4 >> 1) = 2 < VP9_MIN_TILE_WIDTH_B64(4), so max = 1-1 = 0
    assert_eq!(VulkanVp9Decoder::calc_max_log2_tile_cols(4), 0);
}

#[test]
fn calc_max_log2_tile_cols_typical() {
    // sb64_cols = 16 -> (16 >> 1)=8>=4, (16 >> 2)=4>=4, (16 >> 3)=2<4, so max = 3-1 = 2
    assert_eq!(VulkanVp9Decoder::calc_max_log2_tile_cols(16), 2);
}

// -----------------------------------------------------------------------
// Superframe index parsing
// -----------------------------------------------------------------------

#[test]
fn parse_superframe_index_no_index() {
    // Data that does NOT end with a superframe marker
    let data = [0x00, 0x01, 0x02, 0x03];
    let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
    assert_eq!(count, 0);
    assert!(sizes.is_empty());
}

#[test]
fn parse_superframe_index_single_byte_mag() {
    // Build a superframe index with:
    //   frames = 2 (marker low 3 bits = 1, since frames = (marker & 0x7) + 1)
    //   mag = 1 (bits [4:3] = 0, since mag = ((marker >> 3) & 0x3) + 1)
    //   marker = 0b110_00_001 = 0xC1
    //   index_sz = 2 + 1 * 2 = 4
    //   frame_sizes = [0x0A, 0x14] (10, 20)
    // Layout: [... payload ... marker, size0, size1, marker]
    // Total data must be >= index_sz=4 and data[data_sz - index_sz] == marker
    let marker = 0xC1u8;
    let data = [marker, 0x0A, 0x14, marker];
    let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
    assert_eq!(count, 2);
    assert_eq!(sizes, vec![0x0A, 0x14]);
}

#[test]
fn parse_superframe_index_two_byte_mag() {
    // frames = 2 (low 3 bits = 1)
    // mag = 2 (bits [4:3] = 01, since mag = 1 + 1 = 2)
    // marker = 0b110_01_001 = 0xC9
    // index_sz = 2 + 2 * 2 = 6
    // frame_sizes[0] = 0x0100 (little-endian: 0x00, 0x01)
    // frame_sizes[1] = 0x0200 (little-endian: 0x00, 0x02)
    let marker = 0xC9u8;
    let data = [marker, 0x00, 0x01, 0x00, 0x02, marker];
    let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
    assert_eq!(count, 2);
    assert_eq!(sizes, vec![0x0100, 0x0200]);
}

#[test]
fn parse_superframe_index_empty_data() {
    let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&[]);
    assert_eq!(count, 0);
    assert!(sizes.is_empty());
}

#[test]
fn parse_superframe_index_mismatched_marker() {
    // Final byte is a valid marker pattern but the mirror byte at
    // data[data_sz - index_sz] doesn't match.
    let marker = 0xC1u8;
    let data = [0x00, 0x0A, 0x14, marker]; // data[0] = 0x00 != marker
    let (sizes, count) = VulkanVp9Decoder::parse_superframe_index(&data);
    assert_eq!(count, 0);
    assert!(sizes.is_empty());
}

// -----------------------------------------------------------------------
// Reference frame management
// -----------------------------------------------------------------------

#[test]
fn update_frame_pointers_single_slot() {
    let mut dec = VulkanVp9Decoder::new();
    // refresh_frame_flags = 0b00000001 (only slot 0)
    dec.pic_data.std_picture_info.refresh_frame_flags = 0x01;
    dec.update_frame_pointers(Some(42), 1920, 1080);

    assert_eq!(dec.buffers[0].buffer_idx, Some(42));
    assert_eq!(dec.buffers[0].decode_width, 1920);
    assert_eq!(dec.buffers[0].decode_height, 1080);
    // Other slots unchanged
    assert_eq!(dec.buffers[1].buffer_idx, None);
    assert_eq!(dec.buffers[7].buffer_idx, None);
}

#[test]
fn update_frame_pointers_all_slots() {
    let mut dec = VulkanVp9Decoder::new();
    // refresh_frame_flags = 0xFF (all 8 slots)
    dec.pic_data.std_picture_info.refresh_frame_flags = 0xFF;
    dec.update_frame_pointers(Some(7), 640, 480);

    for i in 0..VP9_NUM_REF_FRAMES {
        assert_eq!(dec.buffers[i].buffer_idx, Some(7));
        assert_eq!(dec.buffers[i].decode_width, 640);
    }
}

#[test]
fn update_frame_pointers_alternating() {
    let mut dec = VulkanVp9Decoder::new();
    // refresh_frame_flags = 0b10101010 (slots 1, 3, 5, 7)
    dec.pic_data.std_picture_info.refresh_frame_flags = 0xAA;
    dec.update_frame_pointers(Some(99), 320, 240);

    assert_eq!(dec.buffers[0].buffer_idx, None);
    assert_eq!(dec.buffers[1].buffer_idx, Some(99));
    assert_eq!(dec.buffers[2].buffer_idx, None);
    assert_eq!(dec.buffers[3].buffer_idx, Some(99));
    assert_eq!(dec.buffers[4].buffer_idx, None);
    assert_eq!(dec.buffers[5].buffer_idx, Some(99));
    assert_eq!(dec.buffers[6].buffer_idx, None);
    assert_eq!(dec.buffers[7].buffer_idx, Some(99));
}

// -----------------------------------------------------------------------
// End of stream
// -----------------------------------------------------------------------

#[test]
fn end_of_stream_clears_buffers() {
    let mut dec = VulkanVp9Decoder::new();
    dec.curr_pic = Some(10);
    dec.buffers[0].buffer_idx = Some(1);
    dec.buffers[3].buffer_idx = Some(4);
    dec.buffers[7].buffer_idx = Some(8);

    dec.end_of_stream();

    assert_eq!(dec.curr_pic, None);
    for i in 0..VP9_NUM_REF_FRAMES {
        assert_eq!(dec.buffers[i].buffer_idx, None);
    }
}

// -----------------------------------------------------------------------
// Compute image size
// -----------------------------------------------------------------------

#[test]
fn compute_image_size_basic() {
    let mut dec = VulkanVp9Decoder::new();
    dec.pic_data.frame_width = 1920;
    dec.pic_data.frame_height = 1080;
    dec.pic_data.std_picture_info.flags.show_frame = true;
    dec.compute_image_size();

    // mi_cols = (1920 + 7) / 8 = 240
    assert_eq!(dec.pic_data.mi_cols, 240);
    // mi_rows = (1080 + 7) / 8 = 135 (rounding: (1080+7)>>3 = 1087>>3 = 135)
    assert_eq!(dec.pic_data.mi_rows, 135);
    // sb64_cols = (240 + 7) / 8 = 30 (rounding: 247>>3 = 30)
    assert_eq!(dec.pic_data.sb64_cols, 30);
    // sb64_rows = (135 + 7) / 8 = 17 (rounding: 142>>3 = 17)
    assert_eq!(dec.pic_data.sb64_rows, 17);
}

#[test]
fn compute_image_size_detects_size_change() {
    let mut dec = VulkanVp9Decoder::new();
    dec.last_frame_width = 640;
    dec.last_frame_height = 480;
    dec.pic_data.frame_width = 1920;
    dec.pic_data.frame_height = 1080;

    dec.compute_image_size();

    assert!(dec.frame_size_changed);
    assert!(!dec.pic_data.std_picture_info.flags.use_prev_frame_mvs);
}

#[test]
fn compute_image_size_same_size_inter_frame() {
    let mut dec = VulkanVp9Decoder::new();
    dec.last_frame_width = 1920;
    dec.last_frame_height = 1080;
    dec.last_show_frame = true;
    dec.pic_data.frame_width = 1920;
    dec.pic_data.frame_height = 1080;
    dec.pic_data.std_picture_info.frame_type = Vp9FrameType::Inter;
    dec.pic_data.std_picture_info.flags.error_resilient_mode = false;
    dec.pic_data.std_picture_info.flags.intra_only = false;

    dec.compute_image_size();

    assert!(!dec.frame_size_changed);
    assert!(dec.pic_data.std_picture_info.flags.use_prev_frame_mvs);
}

// -----------------------------------------------------------------------
// Keyframe header parsing (integration-style)
// -----------------------------------------------------------------------

#[test]
fn parse_keyframe_header_minimal() {
    // Build a minimal VP9 keyframe uncompressed header:
    //
    // Bits:
    //   [0..1]   frame_marker = 0b10
    //   [2]      profile_low_bit = 0
    //   [3]      profile_high_bit = 0  -> profile 0
    //   [4]      show_existing_frame = 0
    //   [5]      frame_type = 0 (KEY)
    //   [6]      show_frame = 1
    //   [7]      error_resilient_mode = 0
    //
    //   Byte 0: 0b10_00_0_0_1_0 = 0x82
    //
    //   [8..31]  sync_code = 0x498342 (24 bits)
    //   Bytes 1..3: 0x49, 0x83, 0x42
    //
    //   Color config (profile 0 skips bit_depth, color_space is 3 bits):
    //   [32..34] color_space = 0b001 (BT.601, not RGB)
    //   [35]     color_range = 0
    //   (profile 0: subsampling_x=1, subsampling_y=1 implicit)
    //
    //   [36..51] frame_width - 1 = 0x0077 (16 bits) -> width = 120
    //            Actually let's use width=16, height=16 for simplicity.
    //            width-1 = 15 = 0x000F, height-1 = 15 = 0x000F
    //
    //   [36..51] frame_width-1 = 0x000F  (16 bits)
    //   [52..67] frame_height-1 = 0x000F (16 bits)
    //   [68]     render_and_frame_size_different = 0
    //
    //   Now loop filter:
    //   [69..74] loop_filter_level = 0 (6 bits)
    //   [75..77] loop_filter_sharpness = 0 (3 bits)
    //   [78]     loop_filter_delta_enabled = 0
    //
    //   Quantization:
    //   [79..86] base_q_idx = 0 (8 bits)
    //   [87]     delta_q_y_dc present = 0
    //   [88]     delta_q_uv_dc present = 0
    //   [89]     delta_q_uv_ac present = 0
    //
    //   Segmentation:
    //   [90]     segmentation_enabled = 0
    //
    //   Tile info (sb64_cols for 16px frame):
    //   mi_cols = (16+7)/8 = 2, sb64_cols = (2+7)/8 = 1
    //   min_log2 = 0, max_log2 = 0, so no bits for tile_cols_log2
    //   [91]     tile_rows_log2 = 0
    //
    //   [92..107] compressed_header_size = 0 (16 bits)
    //
    // Let's construct this bitstream carefully.
    let mut bits: Vec<u8> = Vec::new();
    // Helper: we build a bit vector and then pack into bytes.
    let mut bitvec: Vec<u8> = Vec::new();

    fn push_bits(bitvec: &mut Vec<u8>, value: u32, nbits: u32) {
        for i in (0..nbits).rev() {
            bitvec.push(((value >> i) & 1) as u8);
        }
    }

    push_bits(&mut bitvec, 0b10, 2); // frame_marker
    push_bits(&mut bitvec, 0, 1); // profile low bit
    push_bits(&mut bitvec, 0, 1); // profile high bit
    push_bits(&mut bitvec, 0, 1); // show_existing_frame
    push_bits(&mut bitvec, 0, 1); // frame_type (KEY=0)
    push_bits(&mut bitvec, 1, 1); // show_frame
    push_bits(&mut bitvec, 0, 1); // error_resilient_mode
    push_bits(&mut bitvec, 0x498342, 24); // sync code
    push_bits(&mut bitvec, 0b001, 3); // color_space (BT.601)
    push_bits(&mut bitvec, 0, 1); // color_range
    // subsampling is implicit for profile 0
    push_bits(&mut bitvec, 15, 16); // frame_width - 1 = 15
    push_bits(&mut bitvec, 15, 16); // frame_height - 1 = 15
    push_bits(&mut bitvec, 0, 1); // render_and_frame_size_different = 0
    // refresh_frame_context (since error_resilient=0)
    push_bits(&mut bitvec, 0, 1); // refresh_frame_context
    push_bits(&mut bitvec, 0, 1); // frame_parallel_decoding_mode
    push_bits(&mut bitvec, 0, 2); // frame_context_idx
    // loop filter
    push_bits(&mut bitvec, 0, 6); // loop_filter_level
    push_bits(&mut bitvec, 0, 3); // loop_filter_sharpness
    push_bits(&mut bitvec, 0, 1); // loop_filter_delta_enabled
    // quantization
    push_bits(&mut bitvec, 0, 8); // base_q_idx
    push_bits(&mut bitvec, 0, 1); // delta_q_y_dc present
    push_bits(&mut bitvec, 0, 1); // delta_q_uv_dc present
    push_bits(&mut bitvec, 0, 1); // delta_q_uv_ac present
    // segmentation
    push_bits(&mut bitvec, 0, 1); // segmentation_enabled
    // tile info
    // sb64_cols = 1, min_log2=0, max_log2=0, so no increment bits
    push_bits(&mut bitvec, 0, 1); // tile_rows_log2
    // compressed_header_size
    push_bits(&mut bitvec, 0, 16); // compressed_header_size

    // Pack bits into bytes
    let byte_len = bitvec.len().div_ceil(8);
    bits.resize(byte_len, 0);
    for (i, &bit) in bitvec.iter().enumerate() {
        if bit != 0 {
            bits[i >> 3] |= 1 << (7 - (i & 7));
        }
    }

    let mut dec = VulkanVp9Decoder::new();
    let mut bs = BitstreamReader::new(&bits);
    let result = dec.parse_uncompressed_header(&mut bs);
    assert!(result, "parse_uncompressed_header should succeed");

    assert_eq!(dec.pic_data.std_picture_info.profile, Vp9Profile::Profile0);
    assert_eq!(dec.pic_data.std_picture_info.frame_type, Vp9FrameType::Key);
    assert!(dec.pic_data.std_picture_info.flags.show_frame);
    assert!(!dec.pic_data.std_picture_info.flags.error_resilient_mode);
    assert_eq!(dec.pic_data.frame_width, 16);
    assert_eq!(dec.pic_data.frame_height, 16);
    assert_eq!(dec.pic_data.render_width, 16);
    assert_eq!(dec.pic_data.render_height, 16);
    assert_eq!(dec.pic_data.std_picture_info.refresh_frame_flags, 0xFF);
    assert!(dec.pic_data.frame_is_intra);
    assert_eq!(dec.pic_data.std_color_config.bit_depth, 8);
    assert_eq!(
        dec.pic_data.std_color_config.color_space,
        Vp9ColorSpace::Bt601
    );
    assert_eq!(dec.pic_data.chroma_format, 1);
}

// -----------------------------------------------------------------------
// Segmentation parsing
// -----------------------------------------------------------------------

#[test]
fn parse_segmentation_disabled() {
    let data = [0x00]; // segmentation_enabled = 0
    let mut dec = VulkanVp9Decoder::new();
    let mut bs = BitstreamReader::new(&data);
    dec.parse_segmentation_params(&mut bs);
    assert!(!dec.pic_data.std_picture_info.flags.segmentation_enabled);
    assert_eq!(bs.consumed_bits(), 1);
}

// -----------------------------------------------------------------------
// Init parser
// -----------------------------------------------------------------------

#[test]
fn init_parser_resets_state() {
    let mut dec = VulkanVp9Decoder::new();
    dec.curr_pic = Some(5);
    dec.bitstream_complete = false;
    dec.picture_started = true;
    dec.buffers[2].buffer_idx = Some(10);

    dec.init_parser();

    assert_eq!(dec.curr_pic, None);
    assert!(dec.bitstream_complete);
    assert!(!dec.picture_started);
    assert_eq!(dec.buffers[2].buffer_idx, None);
}
