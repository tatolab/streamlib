// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unit tests — pure logic only (no GPU).

use vulkanalia::vk;

use super::*;

// ------------------------------------------------------------------
// align_up
// ------------------------------------------------------------------

#[test]
fn test_align_up_already_aligned() {
    assert_eq!(align_up(256, 256), 256);
    assert_eq!(align_up(512, 256), 512);
    assert_eq!(align_up(0, 256), 0);
}

#[test]
fn test_align_up_needs_rounding() {
    assert_eq!(align_up(1, 256), 256);
    assert_eq!(align_up(255, 256), 256);
    assert_eq!(align_up(257, 256), 512);
    assert_eq!(align_up(100, 256), 256);
}

#[test]
fn test_align_up_power_of_two() {
    assert_eq!(align_up(3, 4), 4);
    assert_eq!(align_up(5, 8), 8);
    assert_eq!(align_up(9, 16), 16);
    assert_eq!(align_up(1023, 1024), 1024);
    assert_eq!(align_up(1025, 1024), 2048);
}

#[test]
fn test_align_up_one() {
    // alignment=1 should return value unchanged
    assert_eq!(align_up(0, 1), 0);
    assert_eq!(align_up(1, 1), 1);
    assert_eq!(align_up(42, 1), 42);
}

// ------------------------------------------------------------------
// select_picture_format
// ------------------------------------------------------------------

#[test]
fn test_select_picture_format_420_8bit() {
    let fmt = select_picture_format(
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_8,
    );
    assert_eq!(fmt, vk::Format::G8_B8R8_2PLANE_420_UNORM);
}

#[test]
fn test_select_picture_format_420_10bit() {
    let fmt = select_picture_format(
        vk::VideoChromaSubsamplingFlagsKHR::_420,
        vk::VideoComponentBitDepthFlagsKHR::_10,
    );
    assert_eq!(fmt, vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16);
}

#[test]
fn test_select_picture_format_422_8bit() {
    let fmt = select_picture_format(
        vk::VideoChromaSubsamplingFlagsKHR::_422,
        vk::VideoComponentBitDepthFlagsKHR::_8,
    );
    assert_eq!(fmt, vk::Format::G8_B8R8_2PLANE_422_UNORM);
}

#[test]
fn test_select_picture_format_444_8bit() {
    let fmt = select_picture_format(
        vk::VideoChromaSubsamplingFlagsKHR::_444,
        vk::VideoComponentBitDepthFlagsKHR::_8,
    );
    assert_eq!(fmt, vk::Format::G8_B8_R8_3PLANE_444_UNORM);
}

#[test]
fn test_select_picture_format_fallback() {
    // Unknown combination falls back to NV12
    let fmt = select_picture_format(
        vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME,
        vk::VideoComponentBitDepthFlagsKHR::_8,
    );
    assert_eq!(fmt, vk::Format::G8_B8R8_2PLANE_420_UNORM);
}

// ------------------------------------------------------------------
// memory_type_matches
// ------------------------------------------------------------------

#[test]
fn test_memory_type_matches_empty_props() {
    let props = vk::PhysicalDeviceMemoryProperties::default();
    // memory_type_count is 0, so nothing matches.
    assert!(!memory_type_matches(
        &props,
        0,
        0xFFFF_FFFF,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ));
}

#[test]
fn test_memory_type_matches_type_bit_not_set() {
    let mut props = vk::PhysicalDeviceMemoryProperties::default();
    props.memory_type_count = 2;
    props.memory_types[0].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    props.memory_types[1].property_flags = vk::MemoryPropertyFlags::HOST_VISIBLE;

    // type_bits excludes index 0
    assert!(!memory_type_matches(
        &props,
        0,
        0b10, // only bit 1 set
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ));
}

#[test]
fn test_memory_type_matches_flags_missing() {
    let mut props = vk::PhysicalDeviceMemoryProperties::default();
    props.memory_type_count = 1;
    props.memory_types[0].property_flags = vk::MemoryPropertyFlags::HOST_VISIBLE;

    // We require DEVICE_LOCAL but index 0 only has HOST_VISIBLE
    assert!(!memory_type_matches(
        &props,
        0,
        0b1,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ));
}

#[test]
fn test_memory_type_matches_success() {
    let mut props = vk::PhysicalDeviceMemoryProperties::default();
    props.memory_type_count = 2;
    props.memory_types[0].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    props.memory_types[1].property_flags =
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;

    assert!(memory_type_matches(
        &props,
        0,
        0b11,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ));
    assert!(memory_type_matches(
        &props,
        1,
        0b11,
        vk::MemoryPropertyFlags::HOST_VISIBLE,
    ));
    // Both flags present at index 1
    assert!(memory_type_matches(
        &props,
        1,
        0b11,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    ));
}

#[test]
fn test_memory_type_matches_out_of_range() {
    let props = vk::PhysicalDeviceMemoryProperties::default();
    assert!(!memory_type_matches(
        &props,
        99,
        0xFFFF_FFFF,
        vk::MemoryPropertyFlags::empty(),
    ));
}

#[test]
fn test_decoded_frame_defaults() {
    let frame = DecodedFrame::default();
    assert_eq!(frame.image, vk::Image::null());
    assert_eq!(frame.image_view, vk::ImageView::null());
    assert_eq!(frame.format, vk::Format::UNDEFINED);
    assert_eq!(frame.extent.width, 0);
    assert_eq!(frame.extent.height, 0);
    assert_eq!(frame.dpb_slot, -1);
    assert_eq!(frame.decode_order, 0);
}

// ------------------------------------------------------------------
// DpbOutputMode
// ------------------------------------------------------------------

#[test]
fn test_dpb_output_mode_default() {
    assert_eq!(DpbOutputMode::default(), DpbOutputMode::Coincide);
}

#[test]
fn test_dpb_output_mode_equality() {
    assert_ne!(DpbOutputMode::Coincide, DpbOutputMode::Distinct);
    assert_eq!(DpbOutputMode::Coincide, DpbOutputMode::Coincide);
    assert_eq!(DpbOutputMode::Distinct, DpbOutputMode::Distinct);
}

// ------------------------------------------------------------------
// SimpleDecoderConfig
// ------------------------------------------------------------------

#[test]
fn test_simple_decoder_config_defaults() {
    let cfg = SimpleDecoderConfig::default();
    assert_eq!(cfg.max_width, 0);
    assert_eq!(cfg.max_height, 0);
    assert_eq!(cfg.output_mode, DpbOutputMode::Coincide);
}

// ------------------------------------------------------------------
// SimpleDecoder NAL splitting (pure logic, no GPU)
// ------------------------------------------------------------------

#[test]
fn test_split_nal_units_empty() {
    let nals = SimpleDecoder::split_nal_units_owned(&[]);
    assert!(nals.is_empty());
}

#[test]
fn test_split_nal_units_single_3byte_sc() {
    // 00 00 01 <NAL data>
    let data = [0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E];
    let nals = SimpleDecoder::split_nal_units_owned(&data);
    assert_eq!(nals.len(), 1);
    assert_eq!(nals[0], &[0x67, 0x42, 0x00, 0x1E]);
}

#[test]
fn test_split_nal_units_single_4byte_sc() {
    let data = [0x00, 0x00, 0x00, 0x01, 0x67, 0x42];
    let nals = SimpleDecoder::split_nal_units_owned(&data);
    assert_eq!(nals.len(), 1);
    assert_eq!(nals[0], &[0x67, 0x42]);
}

#[test]
fn test_split_nal_units_multiple() {
    // SPS + PPS + IDR
    let mut data = Vec::new();
    data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // SC
    data.extend_from_slice(&[0x67, 0x42, 0x00, 0x1E]);  // SPS
    data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // SC
    data.extend_from_slice(&[0x68, 0xCE, 0x38, 0x80]);  // PPS
    data.extend_from_slice(&[0x00, 0x00, 0x01]);         // 3-byte SC
    data.extend_from_slice(&[0x65, 0x88, 0x84]);          // IDR

    let nals = SimpleDecoder::split_nal_units_owned(&data);
    assert_eq!(nals.len(), 3);
    assert_eq!(nals[0][0] & 0x1F, 7);  // SPS
    assert_eq!(nals[1][0] & 0x1F, 8);  // PPS
    assert_eq!(nals[2][0] & 0x1F, 5);  // IDR
}

#[test]
fn test_split_nal_units_no_start_code() {
    let data = [0x67, 0x42, 0x00, 0x1E];
    let nals = SimpleDecoder::split_nal_units_owned(&data);
    assert!(nals.is_empty());
}

// ------------------------------------------------------------------
// SPS dimension parsing (pure logic, no GPU)
// ------------------------------------------------------------------

#[test]
fn test_parse_sps_dimensions_baseline_320x240() {
    // Too-short NALU returns (0,0)
    let (w, h) = SimpleDecoder::parse_sps_dimensions(&[0x67, 0x42]);
    assert_eq!(w, 0);
    assert_eq!(h, 0);
}

#[test]
fn test_parse_h265_sps_dimensions_too_short() {
    // Too-short NALU returns (0,0)
    let (w, h) = SimpleDecoder::parse_h265_sps_dimensions(&[0x42, 0x01]);
    assert_eq!(w, 0);
    assert_eq!(h, 0);
}

#[test]
fn test_parse_h265_sps_dimensions_640x480() {
    // Construct a minimal H.265 SPS NALU (type 33).
    // NAL header: (33 << 1) | 0 = 0x42, layer_id=0 tid=1 => 0x01
    let mut data: Vec<u8> = vec![0x42, 0x01];
    let mut bits: Vec<u8> = Vec::new();

    // sps_video_parameter_set_id: 4 bits = 0
    bits.extend_from_slice(&[0, 0, 0, 0]);
    // sps_max_sub_layers_minus1: 3 bits = 0
    bits.extend_from_slice(&[0, 0, 0]);
    // sps_temporal_id_nesting_flag: 1 bit = 1
    bits.push(1);

    // profile_tier_level(true, 0):
    bits.extend_from_slice(&[0, 0]); // general_profile_space
    bits.push(0); // general_tier_flag
    bits.extend_from_slice(&[0, 0, 0, 0, 1]); // general_profile_idc = 1 (Main)
    // general_profile_compatibility_flag[32]:
    bits.push(0); bits.push(1);
    for _ in 0..30 { bits.push(0); }
    // progressive, interlaced, non_packed, frame_only
    bits.extend_from_slice(&[1, 0, 1, 0]);
    // 44 reserved zero bits
    for _ in 0..44 { bits.push(0); }
    // general_level_idc: 8 bits = 93 (0x5D)
    bits.extend_from_slice(&[0, 1, 0, 1, 1, 1, 0, 1]);

    // sps_seq_parameter_set_id: ue(0) = 1
    bits.push(1);
    // chroma_format_idc: ue(1) = 010
    bits.extend_from_slice(&[0, 1, 0]);

    // pic_width_in_luma_samples: ue(640)
    for _ in 0..9 { bits.push(0); }
    bits.push(1);
    bits.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0, 1]); // 129

    // pic_height_in_luma_samples: ue(480)
    for _ in 0..8 { bits.push(0); }
    bits.push(1);
    bits.extend_from_slice(&[1, 1, 1, 0, 0, 0, 0, 1]); // 225

    // Pad to byte boundary
    while bits.len() % 8 != 0 {
        bits.push(0);
    }

    // Convert bits to bytes
    for chunk in bits.chunks(8) {
        let mut byte = 0u8;
        for (j, &bit) in chunk.iter().enumerate() {
            byte |= bit << (7 - j);
        }
        data.push(byte);
    }

    let (w, h) = SimpleDecoder::parse_h265_sps_dimensions(&data);
    assert_eq!(w, 640, "Expected width 640, got {w}");
    assert_eq!(h, 480, "Expected height 480, got {h}");
}

#[test]
fn test_find_last_start_code_pos() {
    let data = [0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x00, 0x01, 0x68];
    let pos = SimpleDecoder::find_last_start_code_pos(&data);
    assert_eq!(pos, Some(4)); // 4-byte SC at offset 4
}

#[test]
fn test_find_start_code_after() {
    let data = [0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x01, 0x68];
    let pos = SimpleDecoder::find_start_code_after(&data, 0);
    assert_eq!(pos, Some(4)); // next SC at offset 4
}
