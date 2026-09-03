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

use mp4_atom::{Audio, Avc1, Avcc, Dops, FixedPoint, Hvc1, HvcCArray, Hvcc, Opus, Visual};
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

/// `forbidden_zero_bit`, `nal_unit_type`, `nuh_layer_id` and
/// `nuh_temporal_id_plus1` — ITU-T H.265 §7.3.1.2.
const H265_NAL_UNIT_HEADER_BYTES: usize = 2;

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
    /// A parameter set with nothing after its NAL header.
    ParameterSetCarriesNoPayload {
        inbound_link_name: String,
        nal_unit_type: u8,
        nal_unit_bytes: usize,
    },
    /// A `pre_skip` past what `dOps` can state.
    PreSkipThisContainerCannotState {
        inbound_link_name: String,
        pre_skip: u32,
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
            Self::ParameterSetCarriesNoPayload {
                inbound_link_name,
                nal_unit_type,
                nal_unit_bytes,
            } => write!(
                formatter,
                "the h265 parameter set of type {nal_unit_type} on `{inbound_link_name}` is \
                 {nal_unit_bytes} bytes, which is its NAL header and nothing else — writing it \
                 into `hvcC` would describe the track with a record no decoder can read"
            ),
            Self::PreSkipThisContainerCannotState {
                inbound_link_name,
                pre_skip,
            } => write!(
                formatter,
                "the opus track on `{inbound_link_name}` declares a `pre_skip` of {pre_skip} \
                 samples, past the {} a `dOps` PreSkip can state — truncating it would silently \
                 change how much a decoder trims",
                u16::MAX
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
    // The walk admits a one-byte NAL, and `0x42` reads as `nal_unit_type` 33
    // under the H.265 grammar — so the header has to be there before it is cut.
    if sequence_parameter_set.len() <= H265_NAL_UNIT_HEADER_BYTES {
        return Err(Mp4SampleEntryRefusal::SequenceParameterSetTooShort {
            inbound_link_name: inbound_link_name.to_string(),
            codec: "h265",
            sequence_parameter_set_bytes: sequence_parameter_set.len(),
        });
    }
    // Two-byte NAL header, then the RBSP the bit reader and the fixed-offset
    // read both work over.
    let sequence_parameter_set_rbsp =
        remove_emulation_prevention_bytes(&sequence_parameter_set[H265_NAL_UNIT_HEADER_BYTES..]);
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
    let parsed_sequence_parameter_set =
        VulkanH265Decoder::parse_sps(&mut reader).ok_or_else(|| {
            Mp4SampleEntryRefusal::SequenceParameterSetUnparsable {
                inbound_link_name: inbound_link_name.to_string(),
            }
        })?;

    // A parameter set that is only a NAL header configures nothing, and `hvc1`
    // gives a decoder no second source for it.
    for (nal_unit_type, nal_units) in [
        (32u8, &parameter_sets.video_parameter_set_nal_units),
        (33, &parameter_sets.sequence_parameter_set_nal_units),
        (34, &parameter_sets.picture_parameter_set_nal_units),
    ] {
        if let Some(too_short) = nal_units
            .iter()
            .find(|nal_unit| nal_unit.len() <= H265_NAL_UNIT_HEADER_BYTES)
        {
            return Err(Mp4SampleEntryRefusal::ParameterSetCarriesNoPayload {
                inbound_link_name: inbound_link_name.to_string(),
                nal_unit_type,
                nal_unit_bytes: too_short.len(),
            });
        }
    }

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
    let pre_skip: u16 = pre_skip.try_into().map_err(|_| {
        Mp4SampleEntryRefusal::PreSkipThisContainerCannotState {
            inbound_link_name: inbound_link_name.to_string(),
            pre_skip,
        }
    })?;
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
            pre_skip,
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
                Mp4SampleEntryRefusal::ParameterSetsMissingFromSyncPoint { codec, .. }
                    if codec == "h264"
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

    /// Writes the bit-oriented syntax ITU-T H.265 §7.3 is spelled in, so the
    /// SPS below can be read against the spec rather than trusted as a blob.
    struct H265SyntaxBitWriter {
        bits: Vec<bool>,
    }

    impl H265SyntaxBitWriter {
        fn new() -> Self {
            Self { bits: Vec::new() }
        }

        /// `u(n)` — `n` bits, most significant first.
        fn unsigned(&mut self, value: u64, bit_count: u32) -> &mut Self {
            for shift in (0..bit_count).rev() {
                self.bits.push((value >> shift) & 1 == 1);
            }
            self
        }

        /// `ue(v)` — Exp-Golomb: `n` leading zeros, then `value + 1`.
        fn exp_golomb(&mut self, value: u64) -> &mut Self {
            let code = value + 1;
            let significant_bits = 64 - code.leading_zeros();
            self.unsigned(0, significant_bits - 1);
            self.unsigned(code, significant_bits)
        }

        /// `rbsp_trailing_bits()` — a one, then zeros to the byte.
        fn finish_rbsp(&mut self) -> Vec<u8> {
            self.unsigned(1, 1);
            while !self.bits.len().is_multiple_of(8) {
                self.bits.push(false);
            }
            self.bits
                .chunks(8)
                .map(|byte| {
                    byte.iter()
                        .fold(0u8, |packed, &bit| (packed << 1) | u8::from(bit))
                })
                .collect()
        }
    }

    /// A 320x240 Main-profile level-3.1 SPS, built to ITU-T H.265 §7.3.2.2.1.
    ///
    /// Hand-authored because the tree carries no HEVC bitstream and the build
    /// host has no HEVC encoder. It is not trusted blindly: the engine's own
    /// parser has to accept it for the assertions below to run at all.
    fn h265_sequence_parameter_set() -> Vec<u8> {
        let mut writer = H265SyntaxBitWriter::new();
        writer
            .unsigned(0, 4) // sps_video_parameter_set_id
            .unsigned(0, 3) // sps_max_sub_layers_minus1
            .unsigned(1, 1) // sps_temporal_id_nesting_flag
            // profile_tier_level(1, 0) — the 12 bytes hvcC reads back
            .unsigned(0, 2) // general_profile_space
            .unsigned(0, 1) // general_tier_flag
            .unsigned(1, 5) // general_profile_idc — Main
            .unsigned(0b0100_0000_0000_0000_0000_0000_0000_0000, 32) // compatibility, Main
            .unsigned(1, 1) // general_progressive_source_flag
            .unsigned(0, 1) // general_interlaced_source_flag
            .unsigned(0, 1) // general_non_packed_constraint_flag
            .unsigned(1, 1) // general_frame_only_constraint_flag
            .unsigned(0, 43) // general_reserved_zero_43bits
            .unsigned(0, 1) // general_inbld_flag
            .unsigned(93, 8) // general_level_idc — level 3.1
            .exp_golomb(0) // sps_seq_parameter_set_id
            .exp_golomb(1) // chroma_format_idc — 4:2:0
            .exp_golomb(320) // pic_width_in_luma_samples
            .exp_golomb(240) // pic_height_in_luma_samples
            .unsigned(0, 1) // conformance_window_flag
            .exp_golomb(0) // bit_depth_luma_minus8
            .exp_golomb(0) // bit_depth_chroma_minus8
            .exp_golomb(4) // log2_max_pic_order_cnt_lsb_minus4
            .unsigned(1, 1) // sps_sub_layer_ordering_info_present_flag
            .exp_golomb(3) // sps_max_dec_pic_buffering_minus1[0]
            .exp_golomb(0) // sps_max_num_reorder_pics[0]
            .exp_golomb(0) // sps_max_latency_increase_plus1[0]
            .exp_golomb(0) // log2_min_luma_coding_block_size_minus3
            .exp_golomb(2) // log2_diff_max_min_luma_coding_block_size
            .exp_golomb(0) // log2_min_luma_transform_block_size_minus2
            .exp_golomb(3) // log2_diff_max_min_luma_transform_block_size
            .exp_golomb(0) // max_transform_hierarchy_depth_inter
            .exp_golomb(0) // max_transform_hierarchy_depth_intra
            .unsigned(0, 1) // scaling_list_enabled_flag
            .unsigned(0, 1) // amp_enabled_flag
            .unsigned(0, 1) // sample_adaptive_offset_enabled_flag
            .unsigned(0, 1) // pcm_enabled_flag
            .exp_golomb(0) // num_short_term_ref_pic_sets
            .unsigned(0, 1) // long_term_ref_pics_present_flag
            .unsigned(0, 1) // sps_temporal_mvp_enabled_flag
            .unsigned(0, 1) // strong_intra_smoothing_enabled_flag
            .unsigned(0, 1) // vui_parameters_present_flag
            .unsigned(0, 1); // sps_extension_present_flag

        // NAL header: forbidden_zero, type 33 (SPS), layer 0, temporal id 1.
        let mut nal_unit = vec![0x42, 0x01];
        nal_unit.extend_from_slice(&writer.finish_rbsp());
        nal_unit
    }

    fn h265_parameter_sets() -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: vec![vec![0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF]],
            sequence_parameter_set_nal_units: vec![h265_sequence_parameter_set()],
            picture_parameter_set_nal_units: vec![vec![0x44, 0x01, 0xC0, 0x73]],
        }
    }

    #[test]
    fn hvcc_takes_its_profile_tier_level_from_the_sps_at_the_position_the_spec_fixes() {
        let entry = build_hvc1_sample_entry("camera/video", &h265_parameter_sets(), 320, 240)
            .expect("the engine's own parser accepts this SPS");

        assert_eq!(entry.hvcc.general_profile_space, 0);
        assert!(!entry.hvcc.general_tier_flag, "main tier");
        assert_eq!(entry.hvcc.general_profile_idc, 1, "Main profile");
        assert_eq!(entry.hvcc.general_level_idc, 93, "level 3.1");
        assert_eq!(
            entry.hvcc.general_profile_compatibility_flags,
            [0x40, 0x00, 0x00, 0x00],
            "Main sets compatibility flag 1"
        );
        assert_eq!(entry.hvcc.configuration_version, 1);
    }

    #[test]
    fn hvcc_takes_chroma_and_bit_depths_from_the_engines_own_parser() {
        let entry = build_hvc1_sample_entry("camera/video", &h265_parameter_sets(), 320, 240)
            .expect("parses");

        assert_eq!(entry.hvcc.chroma_format_idc, 1, "4:2:0");
        assert_eq!(entry.hvcc.bit_depth_luma_minus8, 0, "8-bit");
        assert_eq!(entry.hvcc.bit_depth_chroma_minus8, 0, "8-bit");
        assert_eq!(
            entry.hvcc.length_size_minus_one,
            NAL_UNIT_LENGTH_PREFIX_BYTES - 1,
            "the declared prefix width and the one the walk writes agree"
        );
    }

    #[test]
    fn an_hvc1_entry_carries_all_three_parameter_set_arrays_marked_complete() {
        let entry = build_hvc1_sample_entry("camera/video", &h265_parameter_sets(), 320, 240)
            .expect("parses");

        let kinds: Vec<u8> = entry.hvcc.arrays.iter().map(|a| a.nal_unit_type).collect();
        assert_eq!(kinds, vec![32, 33, 34], "VPS, SPS then PPS");
        assert!(
            entry.hvcc.arrays.iter().all(|array| array.completeness),
            "hvc1 forbids in-band sets, so the arrays are complete by construction"
        );
    }

    #[test]
    fn an_h265_sync_point_missing_its_vps_is_refused_by_name() {
        let mut sets = h265_parameter_sets();
        sets.video_parameter_set_nal_units.clear();
        let refusal = build_hvc1_sample_entry("camera/video", &sets, 320, 240)
            .expect_err("hvcC wants a VPS as well");
        assert!(matches!(
            refusal,
            Mp4SampleEntryRefusal::ParameterSetsMissingFromSyncPoint { codec, .. }
                if codec == "h265"
        ));
    }

    #[test]
    fn an_h265_sps_too_short_for_its_profile_tier_level_is_refused_by_name() {
        let mut sets = h265_parameter_sets();
        sets.sequence_parameter_set_nal_units = vec![vec![0x42, 0x01, 0x01, 0x02]];
        let refusal = build_hvc1_sample_entry("camera/video", &sets, 320, 240)
            .expect_err("twelve profile-tier-level bytes are not there");
        assert!(matches!(
            refusal,
            Mp4SampleEntryRefusal::SequenceParameterSetTooShort { .. }
        ));
    }

    #[test]
    fn an_h265_sps_shorter_than_its_nal_header_is_refused_rather_than_panicking() {
        let mut sets = h265_parameter_sets();
        // `0x42` alone reads as `nal_unit_type` 33 under the H.265 grammar, so
        // the walk files it as an SPS and this is reachable from a producer.
        sets.sequence_parameter_set_nal_units = vec![vec![0x42]];
        let refusal = build_hvc1_sample_entry("camera/video", &sets, 320, 240)
            .expect_err("a one-byte SPS carries no RBSP at all");
        assert!(matches!(
            refusal,
            Mp4SampleEntryRefusal::SequenceParameterSetTooShort {
                sequence_parameter_set_bytes: 1,
                ..
            }
        ));
    }

    #[test]
    fn a_parameter_set_that_is_only_a_nal_header_never_reaches_hvcc() {
        // `0x40` reads as `nal_unit_type` 32 from its first byte, so a truncated
        // VPS would be filed as one and written into `hvcC` verbatim.
        let split = crate::mp4_annex_b_access_unit::length_prefix_annex_b_access_unit(
            &[
                &[0x00, 0x00, 0x00, 0x01][..],
                &[0x40][..],
                &[0x00, 0x00, 0x00, 0x01][..],
                &[0x44][..],
            ]
            .concat(),
            AnnexBNalHeaderGrammar::H265,
        );
        assert!(
            split
                .parameter_sets
                .video_parameter_set_nal_units
                .is_empty()
                && split
                    .parameter_sets
                    .picture_parameter_set_nal_units
                    .is_empty(),
            "a parameter set that is only a header configures nothing and must not be filed"
        );

        // The builder defends itself too: it is public, so it cannot assume
        // its caller ran the walk.
        let mut sets = h265_parameter_sets();
        sets.video_parameter_set_nal_units = vec![vec![0x40]];
        let refusal = build_hvc1_sample_entry("camera/video", &sets, 320, 240)
            .expect_err("a header-only VPS describes the track with an unreadable record");
        assert!(matches!(
            refusal,
            Mp4SampleEntryRefusal::ParameterSetCarriesNoPayload {
                nal_unit_type: 32,
                nal_unit_bytes: 1,
                ..
            }
        ));
    }

    #[test]
    fn a_pre_skip_past_what_dops_can_state_is_refused_rather_than_truncated() {
        // `dOps.PreSkip` is a `u16` and the wire field a `u32`; 65_536 casts to
        // 0, which silently tells a decoder to trim nothing.
        let refusal = build_opus_sample_entry("microphone/audio", 2, 65_536)
            .expect_err("truncating would change the trim");
        assert!(matches!(
            refusal,
            Mp4SampleEntryRefusal::PreSkipThisContainerCannotState {
                pre_skip: 65_536,
                ..
            }
        ));
        assert!(refusal.to_string().contains("microphone/audio"));

        let highest_statable = build_opus_sample_entry("microphone/audio", 2, u16::MAX as u32)
            .expect("the largest value dOps can state is legal");
        assert_eq!(highest_statable.dops.pre_skip, u16::MAX);
    }
}
