// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The opening of an H.264 sequence parameter set (ITU-T H.264 §7.3.2.1.1):
//! the profile, constraint flags and level an `avc1` codec string spells.

use crate::core::nal_unit_raw_byte_sequence_payload::{
    RbspBitstreamReader, remove_emulation_prevention_bytes,
};

/// The three bytes that open every H.264 SPS after its NAL header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264ProfileAndLevelFromSequenceParameterSet {
    /// `profile_idc` — `0x42` Baseline, `0x4D` Main, `0x64` High.
    pub profile_idc: u8,
    /// `constraint_set0_flag` through `constraint_set5_flag` and the two
    /// reserved zero bits, as the bitstream carries the byte.
    pub constraint_set_flags: u8,
    /// `level_idc` — ten times the level number.
    pub level_idc: u8,
}

impl H264ProfileAndLevelFromSequenceParameterSet {
    /// Read the profile and level from one SPS NAL unit, NAL header included.
    /// `None` when the unit ends before `level_idc`.
    pub fn parse_from_sequence_parameter_set_nal_unit(
        sequence_parameter_set_nal_unit: &[u8],
    ) -> Option<Self> {
        let raw_byte_sequence_payload =
            remove_emulation_prevention_bytes(sequence_parameter_set_nal_unit.get(1..)?);
        let mut reader = RbspBitstreamReader::new(&raw_byte_sequence_payload);
        Some(Self {
            profile_idc: reader.u(8)? as u8,
            constraint_set_flags: reader.u(8)? as u8,
            level_idc: reader.u(8)? as u8,
        })
    }

    /// The WebCodecs / RFC 6381 codec string for a stream with this profile
    /// and level — `avc1.640028` for High at level 4.0.
    pub fn avc1_codec_string(&self) -> String {
        hang::catalog::H264 {
            inline: false,
            profile: self.profile_idc,
            constraints: self.constraint_set_flags,
            level: self.level_idc,
        }
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_in_sequence_parameter_set(file_name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/sequence_parameter_sets")
                .join(file_name),
        )
        .expect("a checked-in SPS fixture")
    }

    /// The SPS each hardware arm actually emits: both encode High at the
    /// level their extent needs.
    #[test]
    fn each_arms_checked_in_sps_spells_high_at_its_own_level() {
        for (file_name, expected_codec_string) in [
            ("vulkan_video_h264_high_1280x720.sps", "avc1.64001f"),
            ("videotoolbox_h264_high_320x180.sps", "avc1.64000b"),
        ] {
            let profile_and_level =
                H264ProfileAndLevelFromSequenceParameterSet::parse_from_sequence_parameter_set_nal_unit(
                    &checked_in_sequence_parameter_set(file_name),
                )
                .unwrap_or_else(|| panic!("{file_name} parses"));
            assert_eq!(
                profile_and_level.avc1_codec_string(),
                expected_codec_string,
                "{file_name}"
            );
        }
    }

    /// x264's 1080p High 4.0 SPS opening: `67 64 00 28 ac d9 …`.
    #[test]
    fn a_high_profile_sps_spells_its_profile_constraints_and_level() {
        let profile_and_level =
            H264ProfileAndLevelFromSequenceParameterSet::parse_from_sequence_parameter_set_nal_unit(
                &[0x67, 0x64, 0x00, 0x28, 0xAC, 0xD9, 0x40],
            )
            .expect("a complete SPS opening");
        assert_eq!(
            profile_and_level,
            H264ProfileAndLevelFromSequenceParameterSet {
                profile_idc: 0x64,
                constraint_set_flags: 0x00,
                level_idc: 0x28,
            }
        );
        assert_eq!(profile_and_level.avc1_codec_string(), "avc1.640028");
    }

    /// Constrained Baseline sets `constraint_set0_flag` and
    /// `constraint_set1_flag`, which a per-arm constant would misstate.
    #[test]
    fn a_constrained_baseline_sps_keeps_its_constraint_flags_in_the_string() {
        let profile_and_level =
            H264ProfileAndLevelFromSequenceParameterSet::parse_from_sequence_parameter_set_nal_unit(
                &[0x67, 0x42, 0xC0, 0x1F, 0x95],
            )
            .unwrap();
        assert_eq!(profile_and_level.avc1_codec_string(), "avc1.42c01f");
    }

    #[test]
    fn hang_reads_the_string_back_to_the_same_profile_and_level() {
        let profile_and_level = H264ProfileAndLevelFromSequenceParameterSet {
            profile_idc: 0x4D,
            constraint_set_flags: 0x40,
            level_idc: 0x33,
        };
        let read_back: hang::catalog::H264 = profile_and_level
            .avc1_codec_string()
            .parse()
            .expect("hang parses the string");
        assert_eq!(
            (read_back.profile, read_back.constraints, read_back.level),
            (0x4D, 0x40, 0x33)
        );
    }

    #[test]
    fn an_sps_ending_before_level_idc_is_not_read() {
        assert_eq!(
            H264ProfileAndLevelFromSequenceParameterSet::parse_from_sequence_parameter_set_nal_unit(
                &[0x67, 0x64, 0x00],
            ),
            None
        );
        assert_eq!(
            H264ProfileAndLevelFromSequenceParameterSet::parse_from_sequence_parameter_set_nal_unit(
                &[],
            ),
            None
        );
    }
}
