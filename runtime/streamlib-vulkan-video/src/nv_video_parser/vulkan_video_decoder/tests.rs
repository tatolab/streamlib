// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// -----------------------------------------------------------------------
// Frame-rate helpers
// -----------------------------------------------------------------------

#[test]
fn test_make_frame_rate_roundtrip() {
    let fr = make_frame_rate(30000, 1001);
    assert_eq!(frame_rate_num(fr), 30000);
    assert_eq!(frame_rate_den(fr), 1001);
}

#[test]
fn test_pack_frame_rate_small() {
    let fr = pack_frame_rate(24000, 1001);
    assert_eq!(frame_rate_num(fr), 24000);
    assert_eq!(frame_rate_den(fr), 1001);
}

#[test]
fn test_pack_frame_rate_large_needs_reduction() {
    // Numerator too large for 18 bits — should be reduced
    let fr = pack_frame_rate(500_000, 1000);
    let num = frame_rate_num(fr);
    let den = frame_rate_den(fr);
    // The ratio should be preserved (approximately)
    assert!(num < (1 << 18));
    assert!(den < (1 << 14));
    // 500_000 / 1000 = 500, so num/den should ~ 500
    let ratio = num as f64 / den as f64;
    assert!((ratio - 500.0).abs() < 1.0);
}

// -----------------------------------------------------------------------
// Exp-Golomb free functions
// -----------------------------------------------------------------------

#[test]
fn test_exp_golomb_ue_zero() {
    // ue(0) = 1 (single '1' bit)
    let data = [0b1000_0000];
    let (val, pos) = exp_golomb_ue(&data, 0);
    assert_eq!(val, 0);
    assert_eq!(pos, 1);
}

#[test]
fn test_exp_golomb_ue_one() {
    // ue(1) = 010 (one leading zero, then 1, then 0)
    let data = [0b0100_0000];
    let (val, pos) = exp_golomb_ue(&data, 0);
    assert_eq!(val, 1);
    assert_eq!(pos, 3);
}

#[test]
fn test_exp_golomb_ue_two() {
    // ue(2) = 011
    let data = [0b0110_0000];
    let (val, pos) = exp_golomb_ue(&data, 0);
    assert_eq!(val, 2);
    assert_eq!(pos, 3);
}

#[test]
fn test_exp_golomb_ue_three() {
    // ue(3) = 00100
    let data = [0b0010_0000];
    let (val, pos) = exp_golomb_ue(&data, 0);
    assert_eq!(val, 3);
    assert_eq!(pos, 5);
}

#[test]
fn test_exp_golomb_ue_four() {
    // ue(4) = 00101
    let data = [0b0010_1000];
    let (val, pos) = exp_golomb_ue(&data, 0);
    assert_eq!(val, 4);
    assert_eq!(pos, 5);
}

#[test]
fn test_exp_golomb_ue_seven() {
    // ue(7) = 0001000
    let data = [0b0001_0000];
    let (val, pos) = exp_golomb_ue(&data, 0);
    assert_eq!(val, 7);
    assert_eq!(pos, 7);
}

#[test]
fn test_exp_golomb_ue_with_offset() {
    // 3 padding bits, then ue(0) = 1
    // Bits: xxx1....
    let data = [0b0001_0000];
    let (val, pos) = exp_golomb_ue(&data, 3);
    assert_eq!(val, 0);
    assert_eq!(pos, 4);
}

#[test]
fn test_exp_golomb_se_zero() {
    // se(0) maps from ue(0) = 0
    let data = [0b1000_0000];
    let (val, pos) = exp_golomb_se(&data, 0);
    assert_eq!(val, 0);
    assert_eq!(pos, 1);
}

#[test]
fn test_exp_golomb_se_positive() {
    // se(1) maps from ue(1) = 010. eg=1, odd -> +1
    let data = [0b0100_0000];
    let (val, pos) = exp_golomb_se(&data, 0);
    assert_eq!(val, 1);
    assert_eq!(pos, 3);
}

#[test]
fn test_exp_golomb_se_negative() {
    // se(-1) maps from ue(2) = 011. eg=2, even -> -(2/2) = -1
    let data = [0b0110_0000];
    let (val, pos) = exp_golomb_se(&data, 0);
    assert_eq!(val, -1);
    assert_eq!(pos, 3);
}

#[test]
fn test_exp_golomb_se_positive_two() {
    // se(2) maps from ue(3) = 00100. eg=3, odd -> +2
    let data = [0b0010_0000];
    let (val, pos) = exp_golomb_se(&data, 0);
    assert_eq!(val, 2);
    assert_eq!(pos, 5);
}

#[test]
fn test_exp_golomb_se_negative_two() {
    // se(-2) maps from ue(4) = 00101. eg=4, even -> -2
    let data = [0b0010_1000];
    let (val, pos) = exp_golomb_se(&data, 0);
    assert_eq!(val, -2);
    assert_eq!(pos, 5);
}

// -----------------------------------------------------------------------
// Start code search
// -----------------------------------------------------------------------

#[test]
fn test_next_start_code_found() {
    let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
    dec.bit_bfr = !0u32;
    let data = [0x00, 0x00, 0x00, 0x01, 0x65];
    let (consumed, found) = dec.next_start_code(&data);
    assert!(found);
    assert_eq!(consumed, 4); // consumed through the 0x01
}

#[test]
fn test_next_start_code_not_found() {
    let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
    dec.bit_bfr = !0u32;
    let data = [0xAA, 0xBB, 0xCC, 0xDD];
    let (consumed, found) = dec.next_start_code(&data);
    assert!(!found);
    assert_eq!(consumed, 4);
}

#[test]
fn test_next_start_code_immediate() {
    let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
    // Pre-load bit_bfr with 0x0000__XX so that after one byte we get 00 00 01
    dec.bit_bfr = 0x00000000;
    let data = [0x01, 0x65, 0x88];
    let (consumed, found) = dec.next_start_code(&data);
    assert!(found);
    assert_eq!(consumed, 1);
}

// -----------------------------------------------------------------------
// NvVkNalUnit default
// -----------------------------------------------------------------------

#[test]
fn test_nal_unit_default() {
    let nalu = NvVkNalUnit::default();
    assert_eq!(nalu.start_offset, 0);
    assert_eq!(nalu.end_offset, 0);
    assert_eq!(nalu.get_offset, 0);
    assert_eq!(nalu.get_zerocnt, 0);
    assert_eq!(nalu.get_bfr, 0);
    assert_eq!(nalu.get_bfroffs, 0);
    assert_eq!(nalu.get_emulcnt, 0);
}

// -----------------------------------------------------------------------
// Bitstream reading (u, ue, se via the decoder)
// -----------------------------------------------------------------------

/// Helper: create a decoder with bitstream data loaded and init_dbits called.
fn decoder_with_bitstream(data: &[u8]) -> VulkanVideoDecoder {
    let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
    // Prepend start code so init_dbits skips 3 bytes
    let mut buf = vec![0x00, 0x00, 0x01];
    buf.extend_from_slice(data);
    let len = buf.len();
    dec.bitstream_data = BitstreamData::new(len);
    dec.bitstream_data.copy_from_slice(&buf, 0, len);
    dec.bitstream_data_len = len;
    dec.nalu.start_offset = 0;
    dec.nalu.end_offset = len as i64;
    dec.emul_bytes_present = false;
    dec.no_start_codes = false;
    dec.init_dbits();
    dec
}

#[test]
fn test_u_read_8_bits() {
    let mut dec = decoder_with_bitstream(&[0xAB]);
    let val = dec.u(8);
    assert_eq!(val, 0xAB);
}

#[test]
fn test_u_read_1_bit() {
    let mut dec = decoder_with_bitstream(&[0x80]); // bit 7 is set
    assert_eq!(dec.u(1), 1);
    assert_eq!(dec.u(1), 0);
}

#[test]
fn test_flag() {
    let mut dec = decoder_with_bitstream(&[0xC0]); // bits: 1 1 0 0 ...
    assert!(dec.flag());
    assert!(dec.flag());
    assert!(!dec.flag());
}

#[test]
fn test_ue_via_decoder() {
    // ue(0) = single '1' bit = 0x80 after start code
    let mut dec = decoder_with_bitstream(&[0x80]);
    assert_eq!(dec.ue(), 0);
}

#[test]
fn test_se_via_decoder() {
    // se(1) from ue(1) = 010 = 0x40 after start code
    let mut dec = decoder_with_bitstream(&[0x40]);
    assert_eq!(dec.se(), 1);
}

#[test]
fn test_available_bits() {
    let mut dec = decoder_with_bitstream(&[0xAB, 0xCD]);
    // 2 data bytes = 16 bits
    assert_eq!(dec.available_bits(), 16);
    dec.u(4);
    assert_eq!(dec.available_bits(), 12);
}

#[test]
fn test_byte_aligned() {
    let mut dec = decoder_with_bitstream(&[0xFF]);
    assert!(dec.byte_aligned());
    dec.u(1);
    assert!(!dec.byte_aligned());
    dec.byte_alignment();
    assert!(dec.byte_aligned());
}

#[test]
fn test_more_rbsp_data_with_data() {
    let dec = decoder_with_bitstream(&[0xFF, 0x80]);
    assert!(dec.more_rbsp_data());
}

#[test]
fn test_end_at_nalu_boundary() {
    let mut dec = decoder_with_bitstream(&[0xAB]);
    // Read all 8 bits
    dec.u(8);
    assert!(dec.end());
}

// -----------------------------------------------------------------------
// Emulation prevention byte handling
// -----------------------------------------------------------------------

#[test]
fn test_emulation_prevention_bytes() {
    // In H.264/H.265, 00 00 03 XX sequences have the 03 removed.
    // Input after start code: 00 00 03 01 -> should read as 00 00 01
    let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
    let buf = vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x03, 0x01];
    let len = buf.len();
    dec.bitstream_data = BitstreamData::new(len);
    dec.bitstream_data.copy_from_slice(&buf, 0, len);
    dec.bitstream_data_len = len;
    dec.nalu.start_offset = 0;
    dec.nalu.end_offset = len as i64;
    dec.emul_bytes_present = true;
    dec.no_start_codes = false;
    dec.init_dbits();

    assert_eq!(dec.u(8), 0x00);
    assert_eq!(dec.u(8), 0x00);
    // The 0x03 should be consumed as an emulation prevention byte
    assert_eq!(dec.u(8), 0x01);
    assert_eq!(dec.nalu.get_emulcnt, 1);
}

// -----------------------------------------------------------------------
// BitstreamData
// -----------------------------------------------------------------------

#[test]
fn test_bitstream_data_start_code() {
    let mut bs = BitstreamData::new(16);
    bs.set_slice_start_code_at_offset(4);
    assert!(bs.has_slice_start_code_at_offset(4));
    assert!(!bs.has_slice_start_code_at_offset(0));
}

#[test]
fn test_bitstream_data_stream_markers() {
    let mut bs = BitstreamData::new(16);
    assert_eq!(bs.get_stream_markers_count(), 0);
    bs.add_stream_marker(10);
    bs.add_stream_marker(20);
    assert_eq!(bs.get_stream_markers_count(), 2);
    assert_eq!(bs.stream_markers(), &[10, 20]);
    bs.reset_stream_markers();
    assert_eq!(bs.get_stream_markers_count(), 0);
}

#[test]
fn test_bitstream_data_swap_buffer() {
    let mut bs = BitstreamData::new(32);
    bs.copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD], 4, 4);
    let new_len = bs.swap_buffer(4, 4);
    assert_eq!(new_len, 32);
    // First 4 bytes should be what was at offset 4
    assert_eq!(bs.get(0), 0xAA);
    assert_eq!(bs.get(1), 0xBB);
    assert_eq!(bs.get(2), 0xCC);
    assert_eq!(bs.get(3), 0xDD);
}

// -----------------------------------------------------------------------
// u16_le, u24_le, u32_le
// -----------------------------------------------------------------------

#[test]
fn test_u16_le() {
    // Little-endian: first byte is low, second is high
    let mut dec = decoder_with_bitstream(&[0x34, 0x12]);
    assert_eq!(dec.u16_le(), 0x1234);
}

#[test]
fn test_u32_le() {
    let mut dec = decoder_with_bitstream(&[0x78, 0x56, 0x34, 0x12]);
    assert_eq!(dec.u32_le(), 0x12345678);
}

// -----------------------------------------------------------------------
// VkParserSequenceInfo equality (used for sequence change detection)
// -----------------------------------------------------------------------

#[test]
fn test_sequence_info_equality() {
    let a = VkParserSequenceInfo::default();
    let b = VkParserSequenceInfo::default();
    assert_eq!(a, b);

    let mut c = VkParserSequenceInfo::default();
    c.coded_width = 1920;
    assert_ne!(a, c);
}

#[test]
fn test_is_sequence_change() {
    let dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
    let info = VkParserSequenceInfo::default();
    // prev_seq_info is default, so no change
    assert!(!dec.is_sequence_change(&info));

    let mut changed = VkParserSequenceInfo::default();
    changed.coded_width = 1920;
    assert!(dec.is_sequence_change(&changed));
}

// -----------------------------------------------------------------------
// API version constant
// -----------------------------------------------------------------------

#[test]
fn test_api_version() {
    // (0 << 22) | (9 << 12) | 9 = 36873
    assert_eq!(NV_VULKAN_VIDEO_PARSER_API_VERSION, (9 << 12) | 9);
}

// -----------------------------------------------------------------------
// Large Exp-Golomb values
// -----------------------------------------------------------------------

#[test]
fn test_exp_golomb_ue_large() {
    // ue(14) = 0001111 (3 leading zeros, 1, then 110)
    // Binary: 000 1 111 -> 0b0001_1110 at bit 0
    let data = [0b0001_1110];
    let (val, pos) = exp_golomb_ue(&data, 0);
    assert_eq!(val, 14);
    assert_eq!(pos, 7);
}

#[test]
fn test_exp_golomb_se_negative_large() {
    // se(-3) from ue(6) = 00111.  eg=6, even -> -(6/2) = -3
    // 00111 = 0b0011_1000
    let data = [0b0011_1000];
    let (val, pos) = exp_golomb_se(&data, 0);
    assert_eq!(val, -3);
    assert_eq!(pos, 5);
}
