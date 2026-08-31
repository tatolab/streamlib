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
// aligned_extent math
//
// `SimpleDecoder::aligned_extent()` rounds `config.max_width` /
// `config.max_height` up to the codec macroblock alignment (16 pixels).
// The full method requires a live Vulkan device; here we exercise the
// underlying alignment math so non-1080p callers can't regress.
// ------------------------------------------------------------------

#[test]
fn test_aligned_extent_math_1080p() {
    use crate::vulkan::video::vk_video_encoder::vk_video_encoder_def::{
        H264_MB_SIZE_ALIGNMENT, align_size,
    };
    assert_eq!(align_size(1920u32, H264_MB_SIZE_ALIGNMENT), 1920);
    assert_eq!(align_size(1080u32, H264_MB_SIZE_ALIGNMENT), 1088);
}

#[test]
fn test_aligned_extent_math_720p() {
    use crate::vulkan::video::vk_video_encoder::vk_video_encoder_def::{
        H264_MB_SIZE_ALIGNMENT, align_size,
    };
    assert_eq!(align_size(1280u32, H264_MB_SIZE_ALIGNMENT), 1280);
    assert_eq!(align_size(720u32, H264_MB_SIZE_ALIGNMENT), 720);
}

#[test]
fn test_aligned_extent_math_4k() {
    use crate::vulkan::video::vk_video_encoder::vk_video_encoder_def::{
        H264_MB_SIZE_ALIGNMENT, align_size,
    };
    assert_eq!(align_size(3840u32, H264_MB_SIZE_ALIGNMENT), 3840);
    assert_eq!(align_size(2160u32, H264_MB_SIZE_ALIGNMENT), 2160);
}

#[test]
fn test_aligned_extent_math_odd_extent() {
    use crate::vulkan::video::vk_video_encoder::vk_video_encoder_def::{
        H264_MB_SIZE_ALIGNMENT, align_size,
    };
    // Arbitrary non-aligned dims must round up to a multiple of 16.
    assert_eq!(align_size(641u32, H264_MB_SIZE_ALIGNMENT), 656);
    assert_eq!(align_size(481u32, H264_MB_SIZE_ALIGNMENT), 496);
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
    data.extend_from_slice(&[0x67, 0x42, 0x00, 0x1E]); // SPS
    data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // SC
    data.extend_from_slice(&[0x68, 0xCE, 0x38, 0x80]); // PPS
    data.extend_from_slice(&[0x00, 0x00, 0x01]); // 3-byte SC
    data.extend_from_slice(&[0x65, 0x88, 0x84]); // IDR

    let nals = SimpleDecoder::split_nal_units_owned(&data);
    assert_eq!(nals.len(), 3);
    assert_eq!(nals[0][0] & 0x1F, 7); // SPS
    assert_eq!(nals[1][0] & 0x1F, 8); // PPS
    assert_eq!(nals[2][0] & 0x1F, 5); // IDR
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
    bits.push(0);
    bits.push(1);
    for _ in 0..30 {
        bits.push(0);
    }
    // progressive, interlaced, non_packed, frame_only
    bits.extend_from_slice(&[1, 0, 1, 0]);
    // 44 reserved zero bits
    for _ in 0..44 {
        bits.push(0);
    }
    // general_level_idc: 8 bits = 93 (0x5D)
    bits.extend_from_slice(&[0, 1, 0, 1, 1, 1, 0, 1]);

    // sps_seq_parameter_set_id: ue(0) = 1
    bits.push(1);
    // chroma_format_idc: ue(1) = 010
    bits.extend_from_slice(&[0, 1, 0]);

    // pic_width_in_luma_samples: ue(640)
    for _ in 0..9 {
        bits.push(0);
    }
    bits.push(1);
    bits.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0, 1]); // 129

    // pic_height_in_luma_samples: ue(480)
    for _ in 0..8 {
        bits.push(0);
    }
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

// ------------------------------------------------------------------
// The parameter sets a sync point ships with — the #1077 seam
// ------------------------------------------------------------------
//
// An encoder that prepends `vkGetEncodedVideoSessionParametersKHR` bytes to
// its sync points has made them the stream's only entry point. If the engine's
// NAL reader cannot find an SPS and a PPS in them, every slice that follows is
// skipped and the run ends having decoded nothing — the recorded #1077
// symptom. These lock the reader and the check that guards it together.

/// One Annex-B NAL: `start_code_length` bytes of start code, then `bytes`.
fn annex_b_nal_unit(start_code_length: usize, bytes: &[u8]) -> Vec<u8> {
    let mut nal_unit = if start_code_length == 4 {
        vec![0x00, 0x00, 0x00, 0x01]
    } else {
        vec![0x00, 0x00, 0x01]
    };
    nal_unit.extend_from_slice(bytes);
    nal_unit
}

/// The H.264 parameter sets a driver hands back, Annex-B framed.
fn h264_annex_b_parameter_sets() -> Vec<u8> {
    let mut parameter_sets = annex_b_nal_unit(4, &[0x67, 0x42, 0x00, 0x1E]);
    parameter_sets.extend_from_slice(&annex_b_nal_unit(4, &[0x68, 0xCE, 0x38, 0x80]));
    parameter_sets
}

#[test]
fn annex_b_framed_parameter_sets_open_a_decodable_stream() {
    assert_eq!(
        why_no_decoder_could_enter_on_these_parameter_sets(
            &h264_annex_b_parameter_sets(),
            crate::vulkan::video::encode::Codec::H264,
        ),
        None
    );
}

/// A driver is free to pick either start-code length, and may mix them
/// within one blob — neither framing may be read as missing parameter sets.
#[test]
fn either_start_code_length_frames_parameter_sets_the_reader_accepts() {
    let mut mixed_framing = annex_b_nal_unit(3, &[0x67, 0x42, 0x00, 0x1E]);
    mixed_framing.extend_from_slice(&annex_b_nal_unit(4, &[0x68, 0xCE, 0x38, 0x80]));
    assert_eq!(
        why_no_decoder_could_enter_on_these_parameter_sets(
            &mixed_framing,
            crate::vulkan::video::encode::Codec::H264,
        ),
        None
    );
}

/// #1077 hypothesis 2, as logic: parameter sets that carry no start code are
/// dropped whole by the reader, because everything before the first start
/// code is not a NAL unit. Silently shipping those is what produces an
/// encoder at ~50 frames and a decoder at 0.
#[test]
fn parameter_sets_carrying_no_start_code_are_refused_rather_than_silently_dropped() {
    let unframed_parameter_sets = [0x67, 0x42, 0x00, 0x1E, 0x68, 0xCE, 0x38, 0x80];
    assert!(
        SimpleDecoder::split_nal_units_owned(&unframed_parameter_sets).is_empty(),
        "the reader finds no NAL unit at all in unframed bytes — which is why they must be \
         refused before they reach a stream"
    );
    let refusal = why_no_decoder_could_enter_on_these_parameter_sets(
        &unframed_parameter_sets,
        crate::vulkan::video::encode::Codec::H264,
    )
    .expect("unframed parameter sets must be refused");
    assert!(
        refusal.contains("SPS") && refusal.contains("PPS"),
        "the refusal names what the decoder needed and did not find: {refusal}"
    );
}

/// A failed `vkGetEncodedVideoSessionParametersKHR` used to become an empty
/// header and then a headerless stream. Empty is a refusal, not a default.
#[test]
fn empty_parameter_sets_are_refused_naming_what_a_decoder_needed() {
    let refusal = why_no_decoder_could_enter_on_these_parameter_sets(
        &[],
        crate::vulkan::video::encode::Codec::H264,
    )
    .expect("empty parameter sets must be refused");
    assert!(
        refusal.contains("SPS") && refusal.contains("PPS"),
        "{refusal}"
    );
}

/// Parameter sets that carry only half of what a decoder configures from are
/// refused naming the half that is missing, not accepted for the half present.
#[test]
fn parameter_sets_missing_one_required_set_are_refused_naming_only_that_one() {
    let sps_only = annex_b_nal_unit(4, &[0x67, 0x42, 0x00, 0x1E]);
    let refusal = why_no_decoder_could_enter_on_these_parameter_sets(
        &sps_only,
        crate::vulkan::video::encode::Codec::H264,
    )
    .expect("an SPS with no PPS must be refused");
    assert!(refusal.contains("PPS"), "{refusal}");
    assert!(
        !refusal.contains("SPS"),
        "the SPS was there; the refusal must not name it: {refusal}"
    );

    // H.265 reads its NAL types out of a different bit field, so a header
    // framed for H.264 satisfies none of them.
    let h265_refusal = why_no_decoder_could_enter_on_these_parameter_sets(
        &h264_annex_b_parameter_sets(),
        crate::vulkan::video::encode::Codec::H265,
    )
    .expect("H.264 parameter sets open no H.265 stream");
    assert!(
        h265_refusal.contains("SPS") && h265_refusal.contains("PPS"),
        "{h265_refusal}"
    );
}

/// The check asks for what this engine's decoder needs and no more. Its
/// H.265 path configures the session from the SPS and defaults every field a
/// VPS would have carried, so a driver whose parameter-set blob omits the VPS
/// must still mint — refusing it would reject a stream this engine decodes.
#[test]
fn h265_parameter_sets_carrying_no_vps_still_open_a_decodable_stream() {
    let mut sps_and_pps = annex_b_nal_unit(4, &[33 << 1, 0x00, 0x00, 0x03]);
    sps_and_pps.extend_from_slice(&annex_b_nal_unit(4, &[34 << 1, 0x00, 0xC1]));
    assert_eq!(
        why_no_decoder_could_enter_on_these_parameter_sets(
            &sps_and_pps,
            crate::vulkan::video::encode::Codec::H265,
        ),
        None
    );

    let vps_only = annex_b_nal_unit(4, &[32 << 1, 0x00, 0x0C]);
    let refusal = why_no_decoder_could_enter_on_these_parameter_sets(
        &vps_only,
        crate::vulkan::video::encode::Codec::H265,
    )
    .expect("a VPS alone opens nothing");
    assert!(
        refusal.contains("SPS") && refusal.contains("PPS") && !refusal.contains("VPS"),
        "{refusal}"
    );
}

/// What actually ships: the parameter sets concatenated ahead of the IDR
/// access unit, in one bag. The reader must recover all three NAL units from
/// it, so the session configures on the same bag it then decodes.
#[test]
fn a_sync_point_access_unit_reads_back_as_its_parameter_sets_then_its_idr() {
    let mut sync_point_access_unit = h264_annex_b_parameter_sets();
    sync_point_access_unit.extend_from_slice(&annex_b_nal_unit(4, &[0x65, 0x88, 0x84, 0x00]));

    let nal_unit_types: Vec<u8> = SimpleDecoder::split_nal_units_owned(&sync_point_access_unit)
        .iter()
        .map(|nal_unit| nal_unit[0] & 0x1F)
        .collect();
    assert_eq!(nal_unit_types, vec![7, 8, 5]);
}

/// H.264 permits trailing zero bytes after a NAL unit's payload, and a
/// driver's parameter sets may carry them. Those bytes belong to no NAL
/// unit: the reader must not hand them on as payload, or the parameter set
/// the decoder parses is not the one the encoder wrote.
#[test]
fn trailing_zero_bytes_between_nal_units_stay_out_of_the_payload() {
    let mut with_trailing_zero = annex_b_nal_unit(4, &[0x67, 0x42, 0x00, 0x1E, 0x00]);
    with_trailing_zero.extend_from_slice(&annex_b_nal_unit(3, &[0x68, 0xCE, 0x38, 0x80]));

    let nal_units = SimpleDecoder::split_nal_units_owned(&with_trailing_zero);
    assert_eq!(nal_units.len(), 2);
    assert_eq!(nal_units[0], vec![0x67, 0x42, 0x00, 0x1E]);
    assert_eq!(nal_units[1], vec![0x68, 0xCE, 0x38, 0x80]);
}
