// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Packs `u(n)` and `ue(v)` syntax elements MSB-first, zero-padded to the byte.
#[derive(Default)]
struct RbspSyntaxBitWriter {
    bits: Vec<bool>,
}

impl RbspSyntaxBitWriter {
    fn unsigned(&mut self, value: u64, bit_count: u32) -> &mut Self {
        for shift in (0..bit_count).rev() {
            self.bits.push((value >> shift) & 1 == 1);
        }
        self
    }

    fn exp_golomb(&mut self, value: u64) -> &mut Self {
        let code = value + 1;
        let significant_bits = 64 - code.leading_zeros();
        self.unsigned(0, significant_bits - 1);
        self.unsigned(code, significant_bits)
    }

    fn bytes(&self) -> Vec<u8> {
        self.bits
            .chunks(8)
            .map(|chunk| {
                (0..8).fold(0u8, |packed, index| {
                    (packed << 1) | u8::from(chunk.get(index).copied().unwrap_or(false))
                })
            })
            .collect()
    }
}

#[test]
fn an_sps_whose_dpb_size_exceeds_the_spec_is_refused_rather_than_overflowing() {
    let mut writer = RbspSyntaxBitWriter::default();
    writer
        .unsigned(0, 4) // sps_video_parameter_set_id
        .unsigned(0, 3) // sps_max_sub_layers_minus1
        .unsigned(1, 1) // sps_temporal_id_nesting_flag
        .unsigned(0, 8) // profile space, tier, profile_idc
        .unsigned(0, 32) // compatibility flags
        .unsigned(0, 48) // source flags and reserved bits
        .unsigned(93, 8) // general_level_idc
        .exp_golomb(0) // sps_seq_parameter_set_id
        .exp_golomb(1) // chroma_format_idc
        .exp_golomb(320) // pic_width_in_luma_samples
        .exp_golomb(240) // pic_height_in_luma_samples
        .unsigned(0, 1) // conformance_window_flag
        .exp_golomb(0) // bit_depth_luma_minus8
        .exp_golomb(0) // bit_depth_chroma_minus8
        .exp_golomb(4) // log2_max_pic_order_cnt_lsb_minus4
        .unsigned(1, 1) // sps_sub_layer_ordering_info_present_flag
        .exp_golomb(255) // sps_max_dec_pic_buffering_minus1[0]
        .exp_golomb(0) // sps_max_num_reorder_pics[0]
        .exp_golomb(0); // sps_max_latency_increase_plus1[0]
    let rbsp = writer.bytes();

    let mut reader = RbspBitstreamReader::new(&rbsp);
    assert!(parse_h265_sequence_parameter_set(&mut reader).is_none());
}

#[test]
fn a_predicted_ref_pic_set_whose_delta_exceeds_the_spec_is_refused_rather_than_overflowing() {
    let mut writer = RbspSyntaxBitWriter::default();
    writer
        .unsigned(1, 1) // inter_ref_pic_set_prediction_flag
        .unsigned(0, 1) // delta_rps_sign
        .exp_golomb(u64::from(i32::MAX as u32)) // abs_delta_rps_minus1
        .unsigned(1, 1); // used_by_curr_pic_flag[0]
    let rbsp = writer.bytes();
    let earlier_sets = [ShortTermRefPicSet::default()];

    let mut reader = RbspBitstreamReader::new(&rbsp);
    let parsed = parse_h265_short_term_ref_pic_set(
        &mut reader,
        &mut StdShortTermRefPicSet::default(),
        &mut ShortTermRefPicSet::default(),
        &earlier_sets,
        1,
        2,
    );
    assert!(parsed.is_none());
}

/// The SPS each hardware arm actually emits. Vulkan Video sets only the
/// Main compatibility flag and writes `general_level_idc` as the spec's
/// byte, not the Vulkan enum index the decode side tolerates.
#[test]
fn each_arms_checked_in_sps_spells_its_hvc1_codec_string() {
    for (file_name, expected_codec_string) in [
        ("vulkan_video_h265_main_1280x720.sps", "hvc1.1.2.L93.90"),
        ("videotoolbox_h265_main_320x180.sps", "hvc1.1.6.L60.B0"),
    ] {
        let sequence_parameter_set = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/sequence_parameter_sets")
                .join(file_name),
        )
        .expect("a checked-in SPS fixture");
        let profile_tier_level =
            h265_profile_tier_level_from_sequence_parameter_set_nal_unit(&sequence_parameter_set)
                .unwrap_or_else(|| panic!("{file_name} parses"));
        assert_eq!(
            profile_tier_level.hvc1_codec_string(),
            expected_codec_string,
            "{file_name}"
        );
    }
}

/// x265's 1080p Main level 3.1 SPS opening, emulation-prevention bytes
/// and all: `42 01 | 01 | 01 60 00 00 03 00 90 00 00 03 00 00 03 00 5d`.
#[test]
fn a_main_profile_sps_spells_its_hvc1_codec_string() {
    let profile_tier_level = h265_profile_tier_level_from_sequence_parameter_set_nal_unit(&[
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x5D, 0xA0,
    ])
    .expect("a complete profile_tier_level");
    assert_eq!(profile_tier_level.general_profile_idc, 1);
    assert_eq!(profile_tier_level.general_level_idc_in_the_bitstream, 93);
    assert_eq!(profile_tier_level.hvc1_codec_string(), "hvc1.1.6.L93.90");
}

#[test]
fn hang_reads_the_hvc1_string_back_to_the_same_profile_tier_and_level() {
    let profile_tier_level = ProfileTierLevel {
        general_profile_space: 0,
        general_tier_flag: true,
        general_profile_idc: 2,
        general_profile_compatibility_flags: [0x20, 0, 0, 0],
        general_constraint_indicator_flags: [0xB0, 0, 0, 0, 0, 0],
        general_level_idc_in_the_bitstream: 153,
        general_level_idc: H265LevelIdc::Level5_1,
    };
    let read_back: hang::catalog::H265 = profile_tier_level
        .hvc1_codec_string()
        .parse()
        .expect("hang parses the string");
    assert_eq!(
        (
            read_back.profile_idc,
            read_back.tier_flag,
            read_back.level_idc,
            read_back.profile_compatibility_flags,
            read_back.constraint_flags
        ),
        (2, true, 153, [0x20, 0, 0, 0], [0xB0, 0, 0, 0, 0, 0])
    );
}

#[test]
fn an_sps_ending_inside_profile_tier_level_is_not_read() {
    assert!(
        h265_profile_tier_level_from_sequence_parameter_set_nal_unit(&[
            0x42, 0x01, 0x01, 0x01, 0x60
        ])
        .is_none()
    );
}

#[test]
fn test_general_level_idc_to_vulkan_raw_spec_bytes() {
    // Raw H.265 spec bytes (general_level_idc = 30 * level)
    assert_eq!(general_level_idc_to_vulkan(30), H265LevelIdc::Level1_0);
    assert_eq!(general_level_idc_to_vulkan(60), H265LevelIdc::Level2_0);
    assert_eq!(general_level_idc_to_vulkan(63), H265LevelIdc::Level2_1);
    assert_eq!(general_level_idc_to_vulkan(90), H265LevelIdc::Level3_0);
    assert_eq!(general_level_idc_to_vulkan(120), H265LevelIdc::Level4_0);
    assert_eq!(general_level_idc_to_vulkan(150), H265LevelIdc::Level5_0);
    assert_eq!(general_level_idc_to_vulkan(186), H265LevelIdc::Level6_2);
    // Invalid should map to Level6_2
    assert_eq!(general_level_idc_to_vulkan(255), H265LevelIdc::Level6_2);
}

#[test]
fn test_general_level_idc_to_vulkan_enum_indices() {
    // Vulkan StdVideoH265LevelIdc enum indices (0–12), as written
    // by the NVIDIA encoder driver into the bitstream SPS.
    assert_eq!(general_level_idc_to_vulkan(0), H265LevelIdc::Level1_0);
    assert_eq!(general_level_idc_to_vulkan(3), H265LevelIdc::Level3_0);
    assert_eq!(general_level_idc_to_vulkan(5), H265LevelIdc::Level4_0);
    assert_eq!(general_level_idc_to_vulkan(7), H265LevelIdc::Level5_0);
    assert_eq!(general_level_idc_to_vulkan(12), H265LevelIdc::Level6_2);
}

// -----------------------------------------------------------------------
// Short-term ref pic set parsing test
// -----------------------------------------------------------------------

#[test]
fn test_parse_short_term_ref_pic_set_direct() {
    // Encode: inter_ref_pic_set_prediction_flag = 0 (implicit for idx=0)
    // num_negative_pics = ue(2) = '011'
    // num_positive_pics = ue(0) = '1'
    // For each negative pic:
    //   delta_poc_s0_minus1[0] = ue(0) = '1', used_by_curr_pic_s0_flag[0] = 1 = '1'
    //   delta_poc_s0_minus1[1] = ue(1) = '010', used_by_curr_pic_s0_flag[1] = 1 = '1'
    //
    // MSB-first bit sequence:
    //   0 1 1 | 1 | 1 | 1 | 0 1 0 | 1
    //   byte 0: 0111_1101 = 0x7D
    //   byte 1: 01xx_xxxx = 0x40
    let data = [0x7D, 0x40];
    let mut reader = RbspBitstreamReader::new(&data);
    let mut std_strps = StdShortTermRefPicSet::default();
    let mut strps = ShortTermRefPicSet::default();

    let result =
        parse_h265_short_term_ref_pic_set(&mut reader, &mut std_strps, &mut strps, &[], 0, 1);

    assert!(result.is_some());
    assert_eq!(strps.num_negative_pics, 2);
    assert_eq!(strps.num_positive_pics, 0);
    // DeltaPocS0[0] = 0 - (0 + 1) = -1
    assert_eq!(strps.delta_poc_s0[0], -1);
    // DeltaPocS0[1] = -1 - (1 + 1) = -3
    assert_eq!(strps.delta_poc_s0[1], -3);
    assert_eq!(strps.used_by_curr_pic_s0[0], 1);
    assert_eq!(strps.used_by_curr_pic_s0[1], 1);
}
