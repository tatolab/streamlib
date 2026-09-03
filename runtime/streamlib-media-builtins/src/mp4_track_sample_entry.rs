// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `stsd` sample entry each track's `moov` carries, built from the first
//! sync-point bag a link delivered.
//!
//! A sample entry is written once and cannot move: it lives only in the one
//! `moov` (ISO/IEC 14496-12 §6.1.2), and no box in a `moof` subtree carries
//! one. Everything a decoder needs to configure itself therefore has to be
//! known before the first fragment closes, which is why the writer holds bags
//! until every track has delivered a sync point.

use mp4_atom::{
    Audio, Avc1, Avcc, Dops, FixedPoint, Hvc1, HvcCArray, Hvcc, Opus, Visual,
};
use streamlib::sdk::engine::video::nv_video_parser::byte_stream_parser::remove_emulation_prevention_bytes;
use streamlib::sdk::engine::video::nv_video_parser::vulkan_h265_decoder::{
    BitstreamReader as H265BitstreamReader, VulkanH265Decoder,
};

use crate::mp4_annex_b_access_unit::{
    AnnexBNalHeaderGrammar, NAL_UNIT_LENGTH_PREFIX_BYTES, ParameterSetsFromAnnexBAccessUnit,
};
use crate::opus_stream_layout::OpusStreamLayoutForSourceChannelCount;

/// Opus's own clock, and the only rate an `OpusEncoder` bag carries.
pub const OPUS_TRACK_TIMESCALE_HZ: u32 = 48_000;

/// The most channels `Dops` can describe.
///
/// `mp4-atom` writes `ChannelMappingFamily` 0 unconditionally and refuses any
/// other value when reading back, so a track it can express is a family-0
/// track: one Opus stream carrying mono or stereo. Mapping family 1 — the
/// three-to-eight-channel case the encoder does mint — has no representation
/// in the crate, which is why the refusal below names the container rather
/// than the codec.
pub const HIGHEST_CHANNEL_COUNT_THIS_CONTAINER_WRITER_PLACES: u32 = 2;

/// H.265 profile-tier-level is 12 bytes at a fixed position in the SPS RBSP:
/// `sps_video_parameter_set_id` (4), `sps_max_sub_layers_minus1` (3) and
/// `sps_temporal_id_nesting_flag` (1) fill the first byte exactly, so the
/// PTL syntax structure of ITU-T H.265 §7.3.3 starts on the second.
const H265_PROFILE_TIER_LEVEL_OFFSET_IN_SPS_RBSP: usize = 1;
/// `general_profile_space` (2) + `general_tier_flag` (1) +
/// `general_profile_idc` (5) + 32 compatibility bits + 48 constraint bits +
/// `general_level_idc` (8).
const H265_PROFILE_TIER_LEVEL_BYTES: usize = 12;

/// Why a track's sample entry could not be built, in the terms the refusal
/// names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mp4SampleEntryRefusal {
    /// The first sync-point access unit did not carry the sets `avcC`/`hvcC`
    /// is built from, so nothing can describe the track to a decoder.
    ParameterSetsMissingFromSyncPoint {
        inbound_link_name: String,
        codec: &'static str,
    },
    /// An SPS too short to read the fields the configuration record needs.
    SequenceParameterSetTooShort {
        inbound_link_name: String,
        codec: &'static str,
        sequence_parameter_set_bytes: usize,
    },
    /// The engine's own H.265 parser could not read the SPS.
    SequenceParameterSetUnparsable { inbound_link_name: String },
    /// An Opus channel count this container writer cannot describe.
    ChannelCountThisContainerWriterCannotPlace {
        inbound_link_name: String,
        channels: u32,
    },
}

impl std::error::Error for Mp4SampleEntryRefusal {}

impl std::fmt::Display for Mp4SampleEntryRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParameterSetsMissingFromSyncPoint {
                inbound_link_name,
                codec,
            } => write!(
                formatter,
                "the first {codec} sync-point bag on `{inbound_link_name}` carried no parameter \
                 sets, and a sample entry cannot be written without them — under `avc1`/`hvc1` \
                 the sets live only in the sample entry, so this track can never be described"
            ),
            Self::SequenceParameterSetTooShort {
                inbound_link_name,
                codec,
                sequence_parameter_set_bytes,
            } => write!(
                formatter,
                "the {codec} sequence parameter set on `{inbound_link_name}` is \
                 {sequence_parameter_set_bytes} bytes, too short to read the profile and level \
                 the sample entry must state"
            ),
            Self::SequenceParameterSetUnparsable { inbound_link_name } => write!(
                formatter,
                "the h265 sequence parameter set on `{inbound_link_name}` could not be read by \
                 the engine's own parser, so the chroma format and bit depths `hvcC` must state \
                 are unknown"
            ),
            Self::ChannelCountThisContainerWriterCannotPlace {
                inbound_link_name,
                channels,
            } => write!(
                formatter,
                "the opus track on `{inbound_link_name}` carries {channels} channels, which this \
                 writer cannot describe: an `Opus` sample entry with more than \
                 {HIGHEST_CHANNEL_COUNT_THIS_CONTAINER_WRITER_PLACES} channels needs channel \
                 mapping family 1, and `mp4-atom` writes family 0 only. The encoder mints such a \
                 stream; recording it does not yet follow"
            ),
        }
    }
}

/// The `avc1` sample entry an H.264 track is described by.
///
/// Profile, compatibility and level are the SPS payload's first three bytes
/// after its one-byte NAL header — ITU-T H.264 §7.3.2.1 puts
/// `profile_idc`, the constraint-flag byte and `level_idc` there with no
/// preceding variable-length field, so no bit reader is needed.
pub fn build_avc1_sample_entry(
    inbound_link_name: &str,
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    coded_width: u32,
    coded_height: u32,
) -> Result<Avc1, Mp4SampleEntryRefusal> {
    if !parameter_sets.is_complete_for(AnnexBNalHeaderGrammar::H264) {
        return Err(Mp4SampleEntryRefusal::ParameterSetsMissingFromSyncPoint {
            inbound_link_name: inbound_link_name.to_string(),
            codec: "h264",
        });
    }
    let sequence_parameter_set = &parameter_sets.sequence_parameter_set_nal_units[0];
    if sequence_parameter_set.len() < 4 {
        return Err(Mp4SampleEntryRefusal::SequenceParameterSetTooShort {
            inbound_link_name: inbound_link_name.to_string(),
            codec: "h264",
            sequence_parameter_set_bytes: sequence_parameter_set.len(),
        });
    }

    Ok(Avc1 {
        visual: visual_sample_entry(coded_width, coded_height, "StreamLib H.264"),
        avcc: Avcc {
            configuration_version: 1,
            avc_profile_indication: sequence_parameter_set[1],
            profile_compatibility: sequence_parameter_set[2],
            avc_level_indication: sequence_parameter_set[3],
            length_size: NAL_UNIT_LENGTH_PREFIX_BYTES,
            sequence_parameter_sets: parameter_sets.sequence_parameter_set_nal_units.clone(),
            picture_parameter_sets: parameter_sets.picture_parameter_set_nal_units.clone(),
            ext: None,
        },
        btrt: None,
        colr: None,
        pasp: None,
        taic: None,
        fiel: None,
    })
}

/// The `hvc1` sample entry an H.265 track is described by.
///
/// The chroma format and bit depths come from the engine's own H.265 parser;
/// the profile-tier-level bytes are read at their fixed position rather than
/// re-derived, because the engine's `ProfileTierLevel` keeps only the profile
/// and level and `hvcC` also states the tier, the profile space and the two
/// flag arrays.
pub fn build_hvc1_sample_entry(
    inbound_link_name: &str,
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    coded_width: u32,
    coded_height: u32,
) -> Result<Hvc1, Mp4SampleEntryRefusal> {
    if !parameter_sets.is_complete_for(AnnexBNalHeaderGrammar::H265) {
        return Err(Mp4SampleEntryRefusal::ParameterSetsMissingFromSyncPoint {
            inbound_link_name: inbound_link_name.to_string(),
            codec: "h265",
        });
    }
    let sequence_parameter_set = &parameter_sets.sequence_parameter_set_nal_units[0];
    // Two-byte NAL header, then the RBSP the bit reader and the fixed-offset
    // read both work over.
    let sequence_parameter_set_rbsp = remove_emulation_prevention_bytes(&sequence_parameter_set[2..]);
    let profile_tier_level_end =
        H265_PROFILE_TIER_LEVEL_OFFSET_IN_SPS_RBSP + H265_PROFILE_TIER_LEVEL_BYTES;
    if sequence_parameter_set_rbsp.len() < profile_tier_level_end {
        return Err(Mp4SampleEntryRefusal::SequenceParameterSetTooShort {
            inbound_link_name: inbound_link_name.to_string(),
            codec: "h265",
            sequence_parameter_set_bytes: sequence_parameter_set.len(),
        });
    }
    let profile_tier_level = &sequence_parameter_set_rbsp
        [H265_PROFILE_TIER_LEVEL_OFFSET_IN_SPS_RBSP..profile_tier_level_end];

    let mut reader = H265BitstreamReader::new(&sequence_parameter_set_rbsp);
    let parsed_sequence_parameter_set = VulkanH265Decoder::parse_sps(&mut reader).ok_or_else(|| {
        Mp4SampleEntryRefusal::SequenceParameterSetUnparsable {
            inbound_link_name: inbound_link_name.to_string(),
        }
    })?;

    let mut hvcc = Hvcc::new();
    hvcc.general_profile_space = (profile_tier_level[0] >> 6) & 0b11;
    hvcc.general_tier_flag = (profile_tier_level[0] >> 5) & 0b1 == 1;
    hvcc.general_profile_idc = profile_tier_level[0] & 0b0001_1111;
    hvcc.general_profile_compatibility_flags = profile_tier_level[1..5]
        .try_into()
        .expect("four compatibility bytes");
    hvcc.general_constraint_indicator_flags = profile_tier_level[5..11]
        .try_into()
        .expect("six constraint bytes");
    hvcc.general_level_idc = profile_tier_level[11];
    hvcc.chroma_format_idc = parsed_sequence_parameter_set.chroma_format_idc;
    hvcc.bit_depth_luma_minus8 = parsed_sequence_parameter_set.bit_depth_luma_minus8;
    hvcc.bit_depth_chroma_minus8 = parsed_sequence_parameter_set.bit_depth_chroma_minus8;
    hvcc.num_temporal_layers = parsed_sequence_parameter_set.sps_max_sub_layers_minus1 + 1;
    hvcc.length_size_minus_one = NAL_UNIT_LENGTH_PREFIX_BYTES - 1;
    hvcc.arrays = vec![
        hvcc_array(32, &parameter_sets.video_parameter_set_nal_units),
        hvcc_array(33, &parameter_sets.sequence_parameter_set_nal_units),
        hvcc_array(34, &parameter_sets.picture_parameter_set_nal_units),
    ];

    Ok(Hvc1 {
        visual: visual_sample_entry(coded_width, coded_height, "StreamLib H.265"),
        hvcc,
        lhvc: None,
        btrt: None,
        colr: None,
        pasp: None,
        taic: None,
        fiel: None,
        ccst: None,
    })
}

/// The `Opus` sample entry an Opus track is described by.
///
/// `channelcount` is the sum of the Opus bitstreams and the bitstreams
/// producing two channels (Opus-in-ISOBMFF §4.3.1), which for every standard
/// layout equals the source's own count — so the layout table is consulted
/// rather than assumed, and the two agreeing is asserted below.
///
/// `PreSkip` is the encoder's reported lookahead, deliberately below the
/// 80 ms floor §4.3.2 states: that floor is RFC 7845 §4.2's recommendation
/// for *cropping an existing stream* rendered as a `shall`, the spec's own
/// §4.7 example writes the lookahead, and no shipping muxer writes anything
/// else. FFmpeg, Chromium, ExoPlayer and Android all trim by this field, so
/// writing 3 840 would destroy 73.5 ms of real audio.
pub fn build_opus_sample_entry(
    inbound_link_name: &str,
    channels: u32,
    pre_skip: u32,
) -> Result<Opus, Mp4SampleEntryRefusal> {
    if channels > HIGHEST_CHANNEL_COUNT_THIS_CONTAINER_WRITER_PLACES {
        return Err(
            Mp4SampleEntryRefusal::ChannelCountThisContainerWriterCannotPlace {
                inbound_link_name: inbound_link_name.to_string(),
                channels,
            },
        );
    }
    let layout = OpusStreamLayoutForSourceChannelCount::resolve(channels).map_err(|_| {
        Mp4SampleEntryRefusal::ChannelCountThisContainerWriterCannotPlace {
            inbound_link_name: inbound_link_name.to_string(),
            channels,
        }
    })?;
    debug_assert_eq!(
        layout.channel_mapping_family(),
        0,
        "a track this writer places is a family-0 track"
    );

    Ok(Opus {
        audio: Audio {
            data_reference_index: 1,
            channel_count: channels as u16,
            sample_size: 16,
            // Opus-in-ISOBMFF §4.3.1: the sample entry's rate field is the
            // 16.16 fixed-point one every audio entry carries, and a decoder
            // takes the real rate from `dOps`.
            sample_rate: FixedPoint::new(OPUS_TRACK_TIMESCALE_HZ as u16, 0),
        },
        dops: Dops {
            output_channel_count: channels as u8,
            pre_skip: pre_skip as u16,
            input_sample_rate: OPUS_TRACK_TIMESCALE_HZ,
            output_gain: 0,
        },
        btrt: None,
    })
}

fn hvcc_array(nal_unit_type: u8, nal_units: &[Vec<u8>]) -> HvcCArray {
    HvcCArray {
        // Every set the track will ever use is here: `hvc1` forbids in-band
        // sets, so the arrays are complete by construction.
        completeness: true,
        nal_unit_type,
        nalus: nal_units.to_vec(),
    }
}

fn visual_sample_entry(coded_width: u32, coded_height: u32, compressor: &str) -> Visual {
    Visual {
        data_reference_index: 1,
        width: coded_width as u16,
        height: coded_height as u16,
        // 72 dpi in 16.16 fixed point, the value every muxer writes.
        horizresolution: FixedPoint::new(72, 0),
        vertresolution: FixedPoint::new(72, 0),
        frame_count: 1,
        compressor: compressor.into(),
        depth: 24,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 320x240 baseline SPS: NAL header `0x67`, then `profile_idc = 0x42`,
    /// constraint flags `0xC0`, `level_idc = 0x1E`.
    const H264_SEQUENCE_PARAMETER_SET: &[u8] = &[
        0x67, 0x42, 0xC0, 0x1E, 0xD9, 0x00, 0xA0, 0x3D, 0xA1, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00,
        0x00, 0x03, 0x00, 0x32, 0x0F, 0x16, 0x2E, 0x48,
    ];
    const H264_PICTURE_PARAMETER_SET: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

    fn h264_parameter_sets() -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: vec![],
            sequence_parameter_set_nal_units: vec![H264_SEQUENCE_PARAMETER_SET.to_vec()],
            picture_parameter_set_nal_units: vec![H264_PICTURE_PARAMETER_SET.to_vec()],
        }
    }

    #[test]
    fn avcc_takes_profile_compatibility_and_level_from_the_sps_payloads_first_three_bytes() {
        let entry = build_avc1_sample_entry("camera/video", &h264_parameter_sets(), 320, 240)
            .expect("the sets are complete");

        assert_eq!(entry.avcc.avc_profile_indication, 0x42, "profile_idc");
        assert_eq!(entry.avcc.profile_compatibility, 0xC0, "constraint flags");
        assert_eq!(entry.avcc.avc_level_indication, 0x1E, "level_idc");
        assert_eq!(
            entry.avcc.length_size, NAL_UNIT_LENGTH_PREFIX_BYTES,
            "the declared prefix width and the one the walk writes are the same constant"
        );
        assert_eq!(entry.visual.width, 320);
        assert_eq!(entry.visual.height, 240);
    }

    #[test]
    fn avcc_carries_every_parameter_set_the_sync_point_delivered() {
        let entry = build_avc1_sample_entry("camera/video", &h264_parameter_sets(), 320, 240)
            .expect("the sets are complete");
        assert_eq!(
            entry.avcc.sequence_parameter_sets,
            vec![H264_SEQUENCE_PARAMETER_SET.to_vec()]
        );
        assert_eq!(
            entry.avcc.picture_parameter_sets,
            vec![H264_PICTURE_PARAMETER_SET.to_vec()]
        );
    }

    #[test]
    fn a_sync_point_with_no_parameter_sets_is_refused_by_name() {
        let refusal = build_avc1_sample_entry(
            "camera/video",
            &ParameterSetsFromAnnexBAccessUnit::default(),
            320,
            240,
        )
        .expect_err("nothing can describe the track");

        assert!(
            matches!(
                refusal,
                Mp4SampleEntryRefusal::ParameterSetsMissingFromSyncPoint { ref codec, .. }
                    if *codec == "h264"
            ),
            "got {refusal:?}"
        );
        assert!(refusal.to_string().contains("camera/video"));
    }

    #[test]
    fn an_opus_entry_states_the_bags_channels_and_the_encoders_lookahead() {
        let entry = build_opus_sample_entry("microphone/audio", 2, 312).expect("stereo is placed");

        assert_eq!(entry.dops.output_channel_count, 2);
        assert_eq!(
            entry.dops.pre_skip, 312,
            "the encoder's reported lookahead, not the spec's 80 ms cropping floor"
        );
        assert_eq!(entry.dops.input_sample_rate, OPUS_TRACK_TIMESCALE_HZ);
        assert_eq!(entry.dops.output_gain, 0);
        assert_eq!(
            entry.audio.channel_count, 2,
            "streams plus coupled streams, which for a standard layout is the source count"
        );
    }

    #[test]
    fn mono_is_placed_the_same_way_as_stereo() {
        let entry = build_opus_sample_entry("microphone/audio", 1, 312).expect("mono is placed");
        assert_eq!(entry.dops.output_channel_count, 1);
        assert_eq!(entry.audio.channel_count, 1);
    }

    #[test]
    fn a_channel_count_needing_mapping_family_one_is_refused_naming_the_container() {
        for channels in 3..=8 {
            let refusal = build_opus_sample_entry("ambisonic/audio", channels, 312)
                .expect_err("family 1 has no representation in the crate");
            let said = refusal.to_string();
            assert!(
                said.contains("mapping family 1") && said.contains("mp4-atom"),
                "the refusal names the container's limit, not the codec's: {said}"
            );
            assert!(said.contains("ambisonic/audio"), "and names the link");
        }
    }
}
