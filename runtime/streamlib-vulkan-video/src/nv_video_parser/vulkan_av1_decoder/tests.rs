// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// -----------------------------------------------------------------------
// Global motion parameter calculations
// -----------------------------------------------------------------------

#[test]
fn test_default_warp_params() {
    let p = default_warp_params();
    assert_eq!(p.wmtype, Av1TransformationType::Identity);
    assert_eq!(p.wmmat[0], 0);
    assert_eq!(p.wmmat[1], 0);
    assert_eq!(p.wmmat[2], 1 << WARPEDMODEL_PREC_BITS);
    assert_eq!(p.wmmat[3], 0);
    assert_eq!(p.wmmat[4], 0);
    assert_eq!(p.wmmat[5], 1 << WARPEDMODEL_PREC_BITS);
    assert!(!p.invalid);
}

#[test]
fn test_identity_global_motion() {
    // A bitstream with a single 0 bit => IDENTITY transformation
    let data = [0x00u8]; // All zeros
    let mut bs = BitstreamReader::new(&data);
    let ref_params = default_warp_params();
    let result = VulkanAv1Decoder::read_gm_params(&mut bs, &ref_params, true);
    assert_eq!(result.wmtype, Av1TransformationType::Identity);
    assert_eq!(result.wmmat, default_warp_params().wmmat);
    assert!(!result.invalid);
}

#[test]
fn test_inv_recenter_nonneg() {
    assert_eq!(inv_recenter_nonneg(5, 0), 5);
    assert_eq!(inv_recenter_nonneg(5, 1), 4);
    assert_eq!(inv_recenter_nonneg(5, 2), 6);
    assert_eq!(inv_recenter_nonneg(5, 11), 11);
    assert_eq!(inv_recenter_nonneg(0, 0), 0);
}

#[test]
fn test_inv_recenter_finite_nonneg() {
    // When r*2 <= n
    assert_eq!(inv_recenter_finite_nonneg(10, 3, 0), 3);
    assert_eq!(inv_recenter_finite_nonneg(10, 3, 1), 2);
    assert_eq!(inv_recenter_finite_nonneg(10, 3, 2), 4);

    // When r*2 > n
    assert_eq!(inv_recenter_finite_nonneg(10, 8, 0), 8);
}

#[test]
fn test_get_msb() {
    assert_eq!(get_msb(1), 0);
    assert_eq!(get_msb(2), 1);
    assert_eq!(get_msb(3), 1);
    assert_eq!(get_msb(4), 2);
    assert_eq!(get_msb(255), 7);
    assert_eq!(get_msb(256), 8);
}

// -----------------------------------------------------------------------
// OBU parsing basics
// -----------------------------------------------------------------------

#[test]
fn test_obu_type_from_u8() {
    assert_eq!(Av1ObuType::from_u8(1), Some(Av1ObuType::SequenceHeader));
    assert_eq!(Av1ObuType::from_u8(2), Some(Av1ObuType::TemporalDelimiter));
    assert_eq!(Av1ObuType::from_u8(3), Some(Av1ObuType::FrameHeader));
    assert_eq!(Av1ObuType::from_u8(4), Some(Av1ObuType::TileGroup));
    assert_eq!(Av1ObuType::from_u8(6), Some(Av1ObuType::Frame));
    assert_eq!(Av1ObuType::from_u8(15), Some(Av1ObuType::Padding));
    assert_eq!(Av1ObuType::from_u8(0), None);
    assert_eq!(Av1ObuType::from_u8(9), None);
}

#[test]
fn test_read_obu_size_single_byte() {
    // 0x05 => value 5, no continuation bit
    let data = [0x05u8];
    let result = VulkanAv1Decoder::read_obu_size(&data);
    assert_eq!(result, Some((5, 1)));
}

#[test]
fn test_read_obu_size_multi_byte() {
    // LEB128: 0x80, 0x01 => (0 | (1 << 7)) = 128
    let data = [0x80u8, 0x01];
    let result = VulkanAv1Decoder::read_obu_size(&data);
    assert_eq!(result, Some((128, 2)));
}

#[test]
fn test_read_obu_header_sequence() {
    // OBU type = 1 (SequenceHeader), has_size_field = 1
    // Byte: 0_0001_0_1_0 = 0b00001010 = 0x0A
    let data = [0x0Au8, 0x00];
    let hdr = VulkanAv1Decoder::read_obu_header(&data).unwrap();
    assert_eq!(hdr.obu_type, Some(Av1ObuType::SequenceHeader));
    assert!(hdr.has_size_field);
    assert!(!hdr.has_extension);
    assert_eq!(hdr.header_size, 1);
}

#[test]
fn test_read_obu_header_with_extension() {
    // OBU type = 3 (FrameHeader), has_extension = 1, has_size_field = 1
    // Byte 0: 0_0011_1_1_0 = 0b00011110 = 0x1E
    // Byte 1: temporal_id=2, spatial_id=1 => 010_01_000 = 0b01001000 = 0x48
    let data = [0x1Eu8, 0x48];
    let hdr = VulkanAv1Decoder::read_obu_header(&data).unwrap();
    assert_eq!(hdr.obu_type, Some(Av1ObuType::FrameHeader));
    assert!(hdr.has_size_field);
    assert!(hdr.has_extension);
    assert_eq!(hdr.temporal_id, 2);
    assert_eq!(hdr.spatial_id, 1);
    assert_eq!(hdr.header_size, 2);
}

#[test]
fn test_read_obu_header_forbidden_bit() {
    // Forbidden bit set (MSB = 1)
    let data = [0x80u8];
    assert!(VulkanAv1Decoder::read_obu_header(&data).is_none());
}

#[test]
fn test_read_obu_header_reserved_bit() {
    // Reserved bit set (LSB = 1)
    // OBU type = 1, has_size_field = 1, reserved = 1
    // 0_0001_0_1_1 = 0b00001011 = 0x0B
    let data = [0x0Bu8];
    assert!(VulkanAv1Decoder::read_obu_header(&data).is_none());
}

#[test]
fn test_parse_obu_header_and_size() {
    let decoder = VulkanAv1Decoder::new(false);
    // OBU type = 1 (SequenceHeader), has_size_field = 1
    // Header byte: 0x0A
    // Size byte: 0x03 (payload = 3 bytes)
    // Payload: 3 zero bytes
    let data = [0x0Au8, 0x03, 0x00, 0x00, 0x00];
    let hdr = decoder.parse_obu_header_and_size(&data).unwrap();
    assert_eq!(hdr.obu_type, Some(Av1ObuType::SequenceHeader));
    assert_eq!(hdr.header_size, 2); // 1 byte header + 1 byte size
    assert_eq!(hdr.payload_size, 3);
}

// -----------------------------------------------------------------------
// Reference frame selection logic
// -----------------------------------------------------------------------

#[test]
fn test_relative_dist_no_order_hint() {
    let mut decoder = VulkanAv1Decoder::new(false);
    let mut sps = Av1SequenceHeader::default();
    sps.enable_order_hint = false;
    decoder.sps = Some(sps);
    // Without order hint, relative dist is always 0
    assert_eq!(decoder.get_relative_dist(10, 5), 0);
}

#[test]
fn test_relative_dist_with_order_hint() {
    let mut decoder = VulkanAv1Decoder::new(false);
    let mut sps = Av1SequenceHeader::default();
    sps.enable_order_hint = true;
    sps.order_hint_bits_minus_1 = 6; // 7 bits
    decoder.sps = Some(sps);

    // Simple forward distance
    assert_eq!(decoder.get_relative_dist(10, 5), 5);
    // Simple backward distance
    assert_eq!(decoder.get_relative_dist(5, 10), -5);
    // Wrap-around (7 bits => max 128)
    assert_eq!(decoder.get_relative_dist(2, 126), 4); // wraps
}

#[test]
fn test_set_frame_refs_basic() {
    let mut decoder = VulkanAv1Decoder::new(false);
    let mut sps = Av1SequenceHeader::default();
    sps.enable_order_hint = true;
    sps.order_hint_bits_minus_1 = 6;
    decoder.sps = Some(sps);

    // Set up some reference order hints
    for i in 0..BUFFER_POOL_MAX_SIZE {
        decoder.ref_order_hint[i] = i as i32;
    }
    decoder.pic_data.std_info.order_hint = 5;

    decoder.set_frame_refs(0, 3);

    // LAST_FRAME should be index 0
    assert_eq!(
        decoder.ref_frame_idx[REFERENCE_NAME_LAST_FRAME - REFERENCE_NAME_LAST_FRAME],
        0
    );
    // GOLDEN_FRAME should be index 3
    assert_eq!(
        decoder.ref_frame_idx[REFERENCE_NAME_GOLDEN_FRAME - REFERENCE_NAME_LAST_FRAME],
        3
    );
    // All slots should be filled (>= 0)
    for i in 0..REFS_PER_FRAME {
        assert!(
            decoder.ref_frame_idx[i] >= 0,
            "ref_frame_idx[{}] = {}",
            i,
            decoder.ref_frame_idx[i]
        );
    }
}

#[test]
fn test_is_frame_intra() {
    let mut decoder = VulkanAv1Decoder::new(false);
    decoder.pic_data.std_info.frame_type = Av1FrameType::Key;
    assert!(decoder.is_frame_intra());

    decoder.pic_data.std_info.frame_type = Av1FrameType::IntraOnly;
    assert!(decoder.is_frame_intra());

    decoder.pic_data.std_info.frame_type = Av1FrameType::Inter;
    assert!(!decoder.is_frame_intra());

    decoder.pic_data.std_info.frame_type = Av1FrameType::Switch;
    assert!(!decoder.is_frame_intra());
}

#[test]
fn test_skip_mode_not_allowed_for_intra() {
    let mut decoder = VulkanAv1Decoder::new(false);
    let mut sps = Av1SequenceHeader::default();
    sps.enable_order_hint = true;
    decoder.sps = Some(sps);
    decoder.pic_data.std_info.frame_type = Av1FrameType::Key;
    decoder.pic_data.std_info.flags.reference_select = true;
    assert!(!decoder.is_skip_mode_allowed());
}

// -----------------------------------------------------------------------
// Bitstream reader
// -----------------------------------------------------------------------

#[test]
fn test_bitstream_reader_u() {
    let data = [0b10110100u8, 0b11000000];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(bs.u(1), 1);
    assert_eq!(bs.u(1), 0);
    assert_eq!(bs.u(1), 1);
    assert_eq!(bs.u(1), 1);
    assert_eq!(bs.u(4), 0b0100);
    assert_eq!(bs.u(2), 0b11);
}

#[test]
fn test_bitstream_reader_byte_alignment() {
    let data = [0xFF, 0xAA];
    let mut bs = BitstreamReader::new(&data);
    bs.u(3); // consume 3 bits
    bs.byte_alignment(); // skip to bit 8
    assert_eq!(bs.consumed_bits(), 8);
    assert_eq!(bs.u(8), 0xAA);
}

#[test]
fn test_read_uvlc() {
    // UVLC for value 0: leading zeros = 0, then 1 bit "1" => value = 0
    // That is: bit "1" => lz=0, val = u(0) + (1<<0) - 1 = 0
    let data = [0b10000000u8];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanAv1Decoder::read_uvlc(&mut bs), 0);

    // UVLC for value 1: "010" => lz=1, val = u(1)=0 + 2 - 1 = 1
    let data = [0b01000000u8];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanAv1Decoder::read_uvlc(&mut bs), 1);

    // UVLC for value 2: "011" => lz=1, val = u(1)=1 + 2 - 1 = 2
    let data = [0b01100000u8];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanAv1Decoder::read_uvlc(&mut bs), 2);
}

#[test]
fn test_read_signed_bits() {
    // 3 bits signed: value bits=2, sign bit
    // "011" => (0b011 << (32-3)) >> (32-3) = 3
    let data = [0b01100000u8];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanAv1Decoder::read_signed_bits(&mut bs, 2), 3);

    // "111" => negative
    let data = [0b11100000u8];
    let mut bs = BitstreamReader::new(&data);
    assert_eq!(VulkanAv1Decoder::read_signed_bits(&mut bs, 2), -1);
}

// -----------------------------------------------------------------------
// Helper functions
// -----------------------------------------------------------------------

#[test]
fn test_tile_log2() {
    assert_eq!(tile_log2(64, 64), 0);
    assert_eq!(tile_log2(64, 65), 1);
    assert_eq!(tile_log2(64, 128), 1);
    assert_eq!(tile_log2(64, 129), 2);
    assert_eq!(tile_log2(1, 1), 0);
    assert_eq!(tile_log2(1, 4), 2);
}

#[test]
fn test_floor_log2() {
    assert_eq!(floor_log2(1), 0);
    assert_eq!(floor_log2(2), 1);
    assert_eq!(floor_log2(3), 1);
    assert_eq!(floor_log2(4), 2);
    assert_eq!(floor_log2(7), 2);
    assert_eq!(floor_log2(8), 3);
}

#[test]
fn test_clamp() {
    assert_eq!(clamp(5, 0, 10), 5);
    assert_eq!(clamp(-1, 0, 10), 0);
    assert_eq!(clamp(11, 0, 10), 10);
    assert_eq!(clamp(0, 0, 0), 0);
}

#[test]
fn test_decoder_construction() {
    let decoder = VulkanAv1Decoder::new(false);
    assert!(!decoder.obu_annex_b);
    assert!(!decoder.sps_received);
    assert_eq!(
        decoder.pic_data.std_info.primary_ref_frame,
        PRIMARY_REF_NONE
    );
    assert_eq!(decoder.pic_data.std_info.refresh_frame_flags, 0xFF);
    assert_eq!(decoder.ref_frame_id, [-1; NUM_REF_FRAMES]);
    assert_eq!(decoder.tile_size_bytes_minus_1, 3);
}

#[test]
fn test_decoder_annex_b() {
    let decoder = VulkanAv1Decoder::new(true);
    assert!(decoder.obu_annex_b);
}

#[test]
fn test_frame_type_conversions() {
    assert_eq!(Av1FrameType::from_u32(0), Some(Av1FrameType::Key));
    assert_eq!(Av1FrameType::from_u32(1), Some(Av1FrameType::Inter));
    assert_eq!(Av1FrameType::from_u32(2), Some(Av1FrameType::IntraOnly));
    assert_eq!(Av1FrameType::from_u32(3), Some(Av1FrameType::Switch));
    assert_eq!(Av1FrameType::from_u32(4), None);
}

#[test]
fn test_transformation_type_conversions() {
    assert_eq!(
        Av1TransformationType::from_u32(0),
        Some(Av1TransformationType::Identity)
    );
    assert_eq!(
        Av1TransformationType::from_u32(1),
        Some(Av1TransformationType::Translation)
    );
    assert_eq!(
        Av1TransformationType::from_u32(2),
        Some(Av1TransformationType::Rotzoom)
    );
    assert_eq!(
        Av1TransformationType::from_u32(3),
        Some(Av1TransformationType::Affine)
    );
    assert_eq!(Av1TransformationType::from_u32(4), None);
}

#[test]
fn test_le_bytes_reader() {
    let data = [0x01, 0x02, 0x03, 0x04];
    let mut bs = BitstreamReader::new(&data);
    // le(2) reads 2 bytes in little-endian: 0x01 + (0x02 << 8) = 0x0201
    assert_eq!(bs.le(2), 0x0201);
}

#[test]
fn test_read_tile_group_size() {
    assert_eq!(read_tile_group_size(&[42], 1), Some(42));
    assert_eq!(read_tile_group_size(&[0x01, 0x02], 2), Some(0x0201));
    assert_eq!(read_tile_group_size(&[0x01, 0x02, 0x03], 3), Some(0x030201));
    assert_eq!(
        read_tile_group_size(&[0x01, 0x02, 0x03, 0x04], 4),
        Some(0x04030201)
    );
    assert_eq!(read_tile_group_size(&[0x00], 5), None);
}
