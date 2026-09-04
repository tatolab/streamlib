// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `stsd` sample entry each CMAF track is described by, and the RFC 6381
//! string the catalog names that track's codec with.
//!
//! Both are minted by the same call from the same parameter sets, because a
//! subscriber configures its decoder from the sample entry and picks the track
//! by the catalog string: if the two disagreed, a player would either reject a
//! stream it can play or accept one it cannot.
//!
//! The entry is written once, into the init object, and can never be revised —
//! ISO/IEC 14496-12 §6.1.2 puts it in the one `moov`, and no box under a `moof`
//! carries one. Everything a decoder needs is therefore read out of the first
//! sync point or refused there.
//!
//! Spelled in this wheel rather than imported: an extension links no engine
//! crate. The H.265 constants and the fixed-offset technique are the engine's,
//! at `runtime/streamlib-media-builtins/src/mp4_track_sample_entry.rs`.

use mp4_atom::{Audio, Avc1, Avcc, Codec, Dops, FixedPoint, Hvc1, HvcCArray, Hvcc, Opus, Visual};

use crate::annex_b_access_unit::{
    AnnexBNalHeaderGrammar, NAL_UNIT_LENGTH_PREFIX_BYTES, ParameterSetsFromAnnexBAccessUnit,
    remove_emulation_prevention_bytes,
};
use crate::cmaf_track_timeline::OPUS_TRACK_TIMESCALE_HZ;
use crate::encoded_media_sample::TrackMedium;
use crate::error::{MoqExtensionError, Result};

/// The RFC 6381 string an Opus track is named by in the catalog. Opus takes no
/// profile or level suffix — the Opus-in-ISOBMFF registration defines the whole
/// name as these four characters.
pub(crate) const OPUS_RFC6381_CODEC_STRING: &str = "opus";

/// The most Opus channels this container path can describe.
///
/// `mp4-atom` writes `dOps` `ChannelMappingFamily` 0 unconditionally and
/// refuses any other value on read, so a track it can express is one Opus
/// stream of mono or stereo. Three to eight channels need mapping family 1,
/// which has no representation in the crate at all.
pub(crate) const HIGHEST_OPUS_CHANNEL_COUNT_THIS_CONTAINER_PATH_PLACES: u32 = 2;

/// The shortest H.264 SPS NAL unit that still states a profile and a level:
/// the one-byte NAL header plus `profile_idc`, the constraint-flag byte and
/// `level_idc`.
const H264_SHORTEST_SEQUENCE_PARAMETER_SET_STATING_PROFILE_AND_LEVEL: usize = 4;

/// `forbidden_zero_bit`, `nal_unit_type`, `nuh_layer_id` and
/// `nuh_temporal_id_plus1` — ITU-T H.265 §7.3.1.2.
const H265_NAL_UNIT_HEADER_BYTES: usize = 2;

/// H.265 profile-tier-level sits at a fixed position in the SPS RBSP:
/// `sps_video_parameter_set_id` (4), `sps_max_sub_layers_minus1` (3) and
/// `sps_temporal_id_nesting_flag` (1) fill the first byte exactly, so the PTL
/// syntax structure of ITU-T H.265 §7.3.3 starts on the second.
const H265_PROFILE_TIER_LEVEL_OFFSET_IN_SEQUENCE_PARAMETER_SET_RBSP: usize = 1;

/// `general_profile_space` (2) + `general_tier_flag` (1) +
/// `general_profile_idc` (5) + 32 compatibility bits + 48 constraint bits +
/// `general_level_idc` (8).
const H265_PROFILE_TIER_LEVEL_BYTES: usize = 12;

/// H.265 `nal_unit_type` for a video parameter set — ITU-T H.265 §7.4.2.2.
const H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET: u8 = 32;
/// H.265 `nal_unit_type` for a sequence parameter set.
const H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET: u8 = 33;
/// H.265 `nal_unit_type` for a picture parameter set.
const H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET: u8 = 34;

/// One track's `stsd` entry, tagged with the medium the track carries.
///
/// The tag is what the init-segment writer builds the rest of the `trak`
/// subtree from: a video track needs a `vmhd` and an audio track an `smhd`,
/// and the entry is the only thing that knows which.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CmafTrackSampleEntry {
    Video(Codec),
    Audio(Codec),
}

impl CmafTrackSampleEntry {
    /// The entry as `Stsd.codecs` holds it.
    pub(crate) fn into_stsd_sample_entry(self) -> Codec {
        match self {
            CmafTrackSampleEntry::Video(sample_entry) => sample_entry,
            CmafTrackSampleEntry::Audio(sample_entry) => sample_entry,
        }
    }

    /// Which medium the track this entry describes carries.
    pub(crate) fn track_medium(&self) -> TrackMedium {
        match self {
            CmafTrackSampleEntry::Video(_) => TrackMedium::Video,
            CmafTrackSampleEntry::Audio(_) => TrackMedium::Audio,
        }
    }
}

/// An `avc1` or `hvc1` entry for the init segment, beside the catalog string
/// naming the same codec.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VideoSampleEntryForTheInitSegment {
    pub(crate) cmaf_track_sample_entry: CmafTrackSampleEntry,
    pub(crate) rfc6381_codec_string: String,
}

/// An `Opus` entry for the init segment, beside the catalog string naming the
/// same codec.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpusSampleEntryForTheInitSegment {
    pub(crate) cmaf_track_sample_entry: CmafTrackSampleEntry,
    pub(crate) rfc6381_codec_string: String,
}

/// Build the sample entry describing a video track, and the catalog string
/// naming its codec, from the parameter sets its first sync point carried.
pub(crate) fn build_video_sample_entry(
    codec: &str,
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    coded_width: u32,
    coded_height: u32,
) -> Result<VideoSampleEntryForTheInitSegment> {
    let grammar = nal_header_grammar_of_wire_codec(codec)?;
    let compressor_name = match grammar {
        AnnexBNalHeaderGrammar::H264 => "StreamLib H.264",
        AnnexBNalHeaderGrammar::H265 => "StreamLib H.265",
    };
    let visual = visual_sample_entry(codec, coded_width, coded_height, compressor_name)?;
    let rfc6381_codec_string = rfc6381_codec_string_for_video(codec, parameter_sets)?;

    let sample_entry = match grammar {
        AnnexBNalHeaderGrammar::H264 => Codec::Avc1(Avc1 {
            visual,
            avcc: avc_decoder_configuration_record(parameter_sets)?,
            btrt: None,
            colr: None,
            pasp: None,
            taic: None,
            fiel: None,
        }),
        AnnexBNalHeaderGrammar::H265 => Codec::Hvc1(Hvc1 {
            visual,
            hvcc: hevc_decoder_configuration_record(parameter_sets)?,
            lhvc: None,
            btrt: None,
            colr: None,
            pasp: None,
            taic: None,
            fiel: None,
            ccst: None,
        }),
    };

    Ok(VideoSampleEntryForTheInitSegment {
        cmaf_track_sample_entry: CmafTrackSampleEntry::Video(sample_entry),
        rfc6381_codec_string,
    })
}

/// Build the sample entry describing an Opus track.
///
/// `pre_skip` is the encoder's own reported lookahead. Opus-in-ISOBMFF §4.3.2
/// states an 80 ms floor, but that is RFC 7845 §4.2's advice for *cropping an
/// existing stream* rendered as a `shall`; FFmpeg, Chromium and ExoPlayer all
/// trim playback by this field, so writing the floor instead of the lookahead
/// would destroy real audio.
pub(crate) fn build_opus_sample_entry(
    channels: u32,
    sample_rate: u32,
    pre_skip: u32,
) -> Result<OpusSampleEntryForTheInitSegment> {
    if channels == 0 {
        return Err(MoqExtensionError::Refused {
            what: "an opus track declaring zero channels cannot be described: `dOps` \
                   OutputChannelCount states how many channels a decoder emits, and no decoder \
                   emits none"
                .to_string(),
        });
    }
    if channels > HIGHEST_OPUS_CHANNEL_COUNT_THIS_CONTAINER_PATH_PLACES {
        return Err(MoqExtensionError::Refused {
            what: format!(
                "an opus track of {channels} channels cannot be published as CMAF: more than \
                 {HIGHEST_OPUS_CHANNEL_COUNT_THIS_CONTAINER_PATH_PLACES} channels needs `dOps` \
                 channel mapping family 1, and this container path writes family 0 only. The \
                 `streamlib_bag` packaging carries the same multichannel opus losslessly — \
                 publish that track over it instead"
            ),
        });
    }
    let pre_skip_the_container_states: u16 =
        pre_skip
            .try_into()
            .map_err(|_| MoqExtensionError::Refused {
                what: format!(
                    "an opus track declaring a `pre_skip` of {pre_skip} samples cannot be \
                     described: a `dOps` PreSkip states at most {}, and truncating it would \
                     silently change how much a decoder trims",
                    u16::MAX
                ),
            })?;

    let sample_entry = Codec::Opus(Opus {
        audio: Audio {
            data_reference_index: 1,
            channel_count: channels as u16,
            sample_size: 16,
            // Opus-in-ISOBMFF §4.3.1: the entry's own rate field is the 16.16
            // fixed-point one every audio entry carries and is always 48 kHz;
            // the stream's real input rate is `dOps` InputSampleRate, where 0
            // is the legal spelling of "unspecified" (RFC 7845 §5.1).
            sample_rate: FixedPoint::new(OPUS_TRACK_TIMESCALE_HZ as u16, 0),
        },
        dops: Dops {
            output_channel_count: channels as u8,
            pre_skip: pre_skip_the_container_states,
            input_sample_rate: sample_rate,
            output_gain: 0,
        },
        btrt: None,
    });

    Ok(OpusSampleEntryForTheInitSegment {
        cmaf_track_sample_entry: CmafTrackSampleEntry::Audio(sample_entry),
        rfc6381_codec_string: OPUS_RFC6381_CODEC_STRING.to_string(),
    })
}

/// The RFC 6381 string a video track is named by in the catalog.
///
/// A subscriber decides whether it can play a track from this string alone, so
/// every field in it is read out of the parameter sets and a set that cannot
/// be read is refused rather than guessed at.
pub(crate) fn rfc6381_codec_string_for_video(
    codec: &str,
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
) -> Result<String> {
    match nal_header_grammar_of_wire_codec(codec)? {
        AnnexBNalHeaderGrammar::H264 => {
            let sequence_parameter_set = first_complete_sequence_parameter_set(
                parameter_sets,
                AnnexBNalHeaderGrammar::H264,
            )?;
            let [profile_idc, constraint_flags, level_idc] =
                h264_profile_constraint_and_level_bytes(sequence_parameter_set)?;
            // ISO/IEC 14496-15 Annex E.3: `avc1.` then those three bytes as six
            // hex digits, and every player that string-matches expects them
            // lowercase.
            Ok(format!(
                "avc1.{profile_idc:02x}{constraint_flags:02x}{level_idc:02x}"
            ))
        }
        AnnexBNalHeaderGrammar::H265 => {
            let sequence_parameter_set = first_complete_sequence_parameter_set(
                parameter_sets,
                AnnexBNalHeaderGrammar::H265,
            )?;
            let rbsp = h265_sequence_parameter_set_rbsp(sequence_parameter_set)?;
            let profile_tier_level = h265_profile_tier_level_bytes(&rbsp)?;
            Ok(rfc6381_codec_string_from_h265_profile_tier_level(
                &profile_tier_level,
            ))
        }
    }
}

fn nal_header_grammar_of_wire_codec(codec: &str) -> Result<AnnexBNalHeaderGrammar> {
    AnnexBNalHeaderGrammar::of_wire_codec(codec).ok_or_else(|| MoqExtensionError::Refused {
        what: format!(
            "`{codec}` is not a codec a CMAF video track can be written for: this container path \
             writes `avc1` for h264 and `hvc1` for h265"
        ),
    })
}

fn visual_sample_entry(
    codec: &str,
    coded_width: u32,
    coded_height: u32,
    compressor_name: &str,
) -> Result<Visual> {
    let dimensions_a_visual_entry_can_state = 1..=u32::from(u16::MAX);
    if !dimensions_a_visual_entry_can_state.contains(&coded_width)
        || !dimensions_a_visual_entry_can_state.contains(&coded_height)
    {
        return Err(MoqExtensionError::Refused {
            what: format!(
                "a {codec} track coded at {coded_width}x{coded_height} cannot be described: a \
                 visual sample entry states each dimension in sixteen bits, so only 1 to {} is \
                 expressible and anything else would be written as a different size",
                u16::MAX
            ),
        });
    }
    Ok(Visual {
        data_reference_index: 1,
        width: coded_width as u16,
        height: coded_height as u16,
        // 72 dpi in 16.16 fixed point, the value every muxer writes.
        horizresolution: FixedPoint::new(72, 0),
        vertresolution: FixedPoint::new(72, 0),
        frame_count: 1,
        compressor: compressor_name.into(),
        depth: 24,
    })
}

fn first_complete_sequence_parameter_set(
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    grammar: AnnexBNalHeaderGrammar,
) -> Result<&[u8]> {
    if !parameter_sets.is_complete_for(grammar) {
        return Err(MoqExtensionError::MalformedBitstream {
            what: format!(
                "the first {} sync point carried no {}, and under `avc1`/`hvc1` the parameter \
                 sets live only in the sample entry — so this track can never be described to a \
                 decoder",
                wire_codec_of_nal_header_grammar(grammar),
                name_the_parameter_sets_that_are_missing(parameter_sets, grammar),
            ),
        });
    }
    Ok(parameter_sets.sequence_parameter_set_nal_units[0].as_slice())
}

fn wire_codec_of_nal_header_grammar(grammar: AnnexBNalHeaderGrammar) -> &'static str {
    match grammar {
        AnnexBNalHeaderGrammar::H264 => "h264",
        AnnexBNalHeaderGrammar::H265 => "h265",
    }
}

fn name_the_parameter_sets_that_are_missing(
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    grammar: AnnexBNalHeaderGrammar,
) -> String {
    let mut missing = Vec::new();
    if grammar == AnnexBNalHeaderGrammar::H265
        && parameter_sets.video_parameter_set_nal_units.is_empty()
    {
        missing.push("video parameter set");
    }
    if parameter_sets.sequence_parameter_set_nal_units.is_empty() {
        missing.push("sequence parameter set");
    }
    if parameter_sets.picture_parameter_set_nal_units.is_empty() {
        missing.push("picture parameter set");
    }
    missing.join(" and no ")
}

/// `profile_idc`, the constraint-flag byte and `level_idc`, which ITU-T H.264
/// §7.3.2.1 puts at SPS payload bytes 1, 2 and 3 — they precede every
/// variable-length field, so no bit reader is needed to reach them.
fn h264_profile_constraint_and_level_bytes(sequence_parameter_set: &[u8]) -> Result<[u8; 3]> {
    if sequence_parameter_set.len() < H264_SHORTEST_SEQUENCE_PARAMETER_SET_STATING_PROFILE_AND_LEVEL
    {
        return Err(MoqExtensionError::MalformedBitstream {
            what: format!(
                "the h264 sequence parameter set is {} bytes, too short to read the profile, \
                 constraint flags and level that the sample entry and the catalog string must \
                 both state",
                sequence_parameter_set.len()
            ),
        });
    }
    Ok([
        sequence_parameter_set[1],
        sequence_parameter_set[2],
        sequence_parameter_set[3],
    ])
}

fn avc_decoder_configuration_record(
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
) -> Result<Avcc> {
    let sequence_parameter_set =
        first_complete_sequence_parameter_set(parameter_sets, AnnexBNalHeaderGrammar::H264)?;
    let [profile_idc, constraint_flags, level_idc] =
        h264_profile_constraint_and_level_bytes(sequence_parameter_set)?;

    Ok(Avcc {
        configuration_version: 1,
        avc_profile_indication: profile_idc,
        profile_compatibility: constraint_flags,
        avc_level_indication: level_idc,
        length_size: NAL_UNIT_LENGTH_PREFIX_BYTES,
        // Raw NAL units: no start code, the NAL header kept, and the
        // emulation-prevention bytes left in place (ISO/IEC 14496-15 §5.3.3.1).
        sequence_parameter_sets: parameter_sets.sequence_parameter_set_nal_units.clone(),
        picture_parameter_sets: parameter_sets.picture_parameter_set_nal_units.clone(),
        ext: None,
    })
}

fn hevc_decoder_configuration_record(
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
) -> Result<Hvcc> {
    let sequence_parameter_set =
        first_complete_sequence_parameter_set(parameter_sets, AnnexBNalHeaderGrammar::H265)?;
    let rbsp = h265_sequence_parameter_set_rbsp(sequence_parameter_set)?;
    let profile_tier_level = h265_profile_tier_level_bytes(&rbsp)?;
    let sequence_parameter_set_fields = read_h265_sequence_parameter_set_fields(&rbsp)?;

    let parameter_set_arrays = [
        (
            H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET,
            &parameter_sets.video_parameter_set_nal_units,
        ),
        (
            H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET,
            &parameter_sets.sequence_parameter_set_nal_units,
        ),
        (
            H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET,
            &parameter_sets.picture_parameter_set_nal_units,
        ),
    ];
    for (nal_unit_type, nal_units) in parameter_set_arrays {
        // `hvc1` forbids in-band parameter sets, so a set that is only a NAL
        // header leaves a decoder with no second source for it.
        if let Some(carries_no_payload) = nal_units
            .iter()
            .find(|nal_unit| nal_unit.len() <= H265_NAL_UNIT_HEADER_BYTES)
        {
            return Err(MoqExtensionError::MalformedBitstream {
                what: format!(
                    "the h265 parameter set of type {nal_unit_type} is {} bytes, its NAL header \
                     and nothing else — writing it into `hvcC` would describe the track with a \
                     record no decoder can read",
                    carries_no_payload.len()
                ),
            });
        }
    }

    let mut hevc_configuration = Hvcc::new();
    hevc_configuration.general_profile_space = (profile_tier_level[0] >> 6) & 0b11;
    hevc_configuration.general_tier_flag = (profile_tier_level[0] >> 5) & 0b1 == 1;
    hevc_configuration.general_profile_idc = profile_tier_level[0] & 0b0001_1111;
    hevc_configuration.general_profile_compatibility_flags = profile_tier_level[1..5]
        .try_into()
        .expect("four compatibility bytes, sliced from the twelve-byte block one line above");
    hevc_configuration.general_constraint_indicator_flags = profile_tier_level[5..11]
        .try_into()
        .expect("six constraint bytes, sliced from the twelve-byte block one line above");
    hevc_configuration.general_level_idc = profile_tier_level[11];
    hevc_configuration.chroma_format_idc = sequence_parameter_set_fields.chroma_format_idc;
    hevc_configuration.bit_depth_luma_minus8 = sequence_parameter_set_fields.bit_depth_luma_minus8;
    hevc_configuration.bit_depth_chroma_minus8 =
        sequence_parameter_set_fields.bit_depth_chroma_minus8;
    hevc_configuration.num_temporal_layers =
        sequence_parameter_set_fields.sps_max_sub_layers_minus1 + 1;
    hevc_configuration.length_size_minus_one = NAL_UNIT_LENGTH_PREFIX_BYTES - 1;
    hevc_configuration.arrays = parameter_set_arrays
        .into_iter()
        .map(|(nal_unit_type, nal_units)| HvcCArray {
            // Every set the track will ever use is here: `hvc1` forbids in-band
            // sets, so the arrays are complete by construction.
            completeness: true,
            nal_unit_type,
            nalus: nal_units.clone(),
        })
        .collect();
    Ok(hevc_configuration)
}

/// The SPS payload with its two-byte NAL header cut and its
/// emulation-prevention bytes removed, which is what both the fixed-offset
/// read and the bit reader below work over.
fn h265_sequence_parameter_set_rbsp(sequence_parameter_set: &[u8]) -> Result<Vec<u8>> {
    if sequence_parameter_set.len() <= H265_NAL_UNIT_HEADER_BYTES {
        return Err(MoqExtensionError::MalformedBitstream {
            what: format!(
                "the h265 sequence parameter set is {} bytes, its NAL header and nothing else, so \
                 there is no payload to read the profile, tier and level from",
                sequence_parameter_set.len()
            ),
        });
    }
    Ok(remove_emulation_prevention_bytes(
        &sequence_parameter_set[H265_NAL_UNIT_HEADER_BYTES..],
    ))
}

/// The 12-byte profile-tier-level block, read at its fixed position rather
/// than re-derived: `hvcC` and the catalog string both state the tier, the
/// profile space and the two flag arrays, which a decoded profile/level pair
/// does not keep.
fn h265_profile_tier_level_bytes(
    sequence_parameter_set_rbsp: &[u8],
) -> Result<[u8; H265_PROFILE_TIER_LEVEL_BYTES]> {
    let profile_tier_level_end = H265_PROFILE_TIER_LEVEL_OFFSET_IN_SEQUENCE_PARAMETER_SET_RBSP
        + H265_PROFILE_TIER_LEVEL_BYTES;
    if sequence_parameter_set_rbsp.len() < profile_tier_level_end {
        return Err(MoqExtensionError::MalformedBitstream {
            what: format!(
                "the h265 sequence parameter set payload is {} bytes, too short to hold the \
                 {H265_PROFILE_TIER_LEVEL_BYTES}-byte profile-tier-level block that the sample \
                 entry and the catalog string are both built from",
                sequence_parameter_set_rbsp.len()
            ),
        });
    }
    let profile_tier_level = sequence_parameter_set_rbsp
        [H265_PROFILE_TIER_LEVEL_OFFSET_IN_SEQUENCE_PARAMETER_SET_RBSP..profile_tier_level_end]
        .try_into()
        .expect("a slice of exactly the block's length, whose end was bounds-checked above");
    Ok(profile_tier_level)
}

/// ISO/IEC 14496-15 Annex E.3's `hvc1.` form.
///
/// The compatibility element is the 32 flag bits in *reverse* bit order, which
/// is why a Main-profile stream whose flags are `0x60000000` is named `.6` and
/// not `.60000000`. Trailing zero constraint bytes are omitted, and the whole
/// element disappears when all six are zero.
fn rfc6381_codec_string_from_h265_profile_tier_level(
    profile_tier_level: &[u8; H265_PROFILE_TIER_LEVEL_BYTES],
) -> String {
    let general_profile_space = (profile_tier_level[0] >> 6) & 0b11;
    let general_tier_flag = (profile_tier_level[0] >> 5) & 0b1;
    let general_profile_idc = profile_tier_level[0] & 0b0001_1111;
    let general_profile_compatibility_flags = u32::from_be_bytes([
        profile_tier_level[1],
        profile_tier_level[2],
        profile_tier_level[3],
        profile_tier_level[4],
    ])
    .reverse_bits();
    let general_constraint_indicator_flags = &profile_tier_level[5..11];
    let general_level_idc = profile_tier_level[11];

    let profile_space_prefix = match general_profile_space {
        0 => String::new(),
        space => char::from(b'A' + space - 1).to_string(),
    };
    let tier = if general_tier_flag == 1 { 'H' } else { 'L' };
    let mut codec_string = format!(
        "hvc1.{profile_space_prefix}{general_profile_idc}.\
         {general_profile_compatibility_flags:X}.{tier}{general_level_idc}"
    );
    if let Some(last_flag_byte_that_is_set) = general_constraint_indicator_flags
        .iter()
        .rposition(|&flags| flags != 0)
    {
        for flags in &general_constraint_indicator_flags[..=last_flag_byte_that_is_set] {
            codec_string.push_str(&format!(".{flags:02X}"));
        }
    }
    codec_string
}

/// The SPS fields `hvcC` states that the profile-tier-level block does not
/// carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SequenceParameterSetFieldsTheHevcConfigurationStates {
    sps_max_sub_layers_minus1: u8,
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

/// Walk the SPS RBSP as far as the bit depths — ITU-T H.265 §7.3.2.2.1, in the
/// element order the standard fixes.
fn read_h265_sequence_parameter_set_fields(
    sequence_parameter_set_rbsp: &[u8],
) -> Result<SequenceParameterSetFieldsTheHevcConfigurationStates> {
    let mut reader = RbspBitReaderThatRefusesToReadPastTheEnd::over(sequence_parameter_set_rbsp);
    let unreadable = || MoqExtensionError::MalformedBitstream {
        what: "the h265 sequence parameter set ends before the chroma format and bit depths that \
               `hvcC` must state, so the track cannot be described"
            .to_string(),
    };

    reader.read_next_bits(4).ok_or_else(unreadable)?; // sps_video_parameter_set_id
    let sps_max_sub_layers_minus1 = reader.read_next_bits(3).ok_or_else(unreadable)? as u8;
    reader.read_next_bits(1).ok_or_else(unreadable)?; // sps_temporal_id_nesting_flag
    skip_h265_profile_tier_level(&mut reader, sps_max_sub_layers_minus1).ok_or_else(unreadable)?;

    reader
        .read_next_unsigned_exponential_golomb()
        .ok_or_else(unreadable)?; // sps_seq_parameter_set_id
    let chroma_format_idc = reader
        .read_next_unsigned_exponential_golomb()
        .ok_or_else(unreadable)? as u8;
    if chroma_format_idc > 3 {
        return Err(MoqExtensionError::MalformedBitstream {
            what: format!(
                "the h265 sequence parameter set states `chroma_format_idc` {chroma_format_idc}, \
                 which ITU-T H.265 §7.4.3.2.1 does not define — the elements after it cannot be \
                 read from here"
            ),
        });
    }
    if chroma_format_idc == 3 {
        reader.read_next_bits(1).ok_or_else(unreadable)?; // separate_colour_plane_flag
    }
    reader
        .read_next_unsigned_exponential_golomb()
        .ok_or_else(unreadable)?; // pic_width_in_luma_samples
    reader
        .read_next_unsigned_exponential_golomb()
        .ok_or_else(unreadable)?; // pic_height_in_luma_samples
    if reader.read_next_bits(1).ok_or_else(unreadable)? == 1 {
        for _ in 0..4 {
            // conf_win_{left,right,top,bottom}_offset
            reader
                .read_next_unsigned_exponential_golomb()
                .ok_or_else(unreadable)?;
        }
    }
    let bit_depth_luma_minus8 = reader
        .read_next_unsigned_exponential_golomb()
        .ok_or_else(unreadable)? as u8;
    let bit_depth_chroma_minus8 = reader
        .read_next_unsigned_exponential_golomb()
        .ok_or_else(unreadable)? as u8;

    Ok(SequenceParameterSetFieldsTheHevcConfigurationStates {
        sps_max_sub_layers_minus1,
        chroma_format_idc,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
    })
}

/// Step the reader over `profile_tier_level(1, sps_max_sub_layers_minus1)` —
/// ITU-T H.265 §7.3.3. The structure is variable-length: a fixed 96-bit general
/// part, then two presence flags per sub-layer, then a padding run out to eight
/// sub-layers, then 88 or 8 further bits per sub-layer that declared them.
fn skip_h265_profile_tier_level(
    reader: &mut RbspBitReaderThatRefusesToReadPastTheEnd<'_>,
    sps_max_sub_layers_minus1: u8,
) -> Option<()> {
    reader.skip_next_bits(H265_PROFILE_TIER_LEVEL_BYTES as u32 * 8)?;

    let mut sub_layer_profile_present = [false; 8];
    let mut sub_layer_level_present = [false; 8];
    for sub_layer in 0..sps_max_sub_layers_minus1 as usize {
        sub_layer_profile_present[sub_layer] = reader.read_next_bits(1)? == 1;
        sub_layer_level_present[sub_layer] = reader.read_next_bits(1)? == 1;
    }
    if sps_max_sub_layers_minus1 > 0 {
        for _ in sps_max_sub_layers_minus1..8 {
            reader.skip_next_bits(2)?; // reserved_zero_2bits
        }
    }
    for sub_layer in 0..sps_max_sub_layers_minus1 as usize {
        if sub_layer_profile_present[sub_layer] {
            // The same 2+1+5+32+4+44 bits the general part spends, less its level.
            reader.skip_next_bits(88)?;
        }
        if sub_layer_level_present[sub_layer] {
            reader.skip_next_bits(8)?; // sub_layer_level_idc
        }
    }
    Some(())
}

/// A most-significant-bit-first reader over an RBSP.
///
/// Every read that would pass the last bit yields `None` rather than a value
/// padded with zeros: a truncated SPS that silently read as `chroma_format_idc`
/// 0 would be written into `hvcC` as a monochrome track.
struct RbspBitReaderThatRefusesToReadPastTheEnd<'rbsp> {
    rbsp_bytes: &'rbsp [u8],
    next_bit_offset: usize,
}

impl<'rbsp> RbspBitReaderThatRefusesToReadPastTheEnd<'rbsp> {
    fn over(rbsp_bytes: &'rbsp [u8]) -> Self {
        Self {
            rbsp_bytes,
            next_bit_offset: 0,
        }
    }

    fn read_next_bits(&mut self, bit_count: u32) -> Option<u32> {
        if bit_count > u32::BITS {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..bit_count {
            let byte = *self.rbsp_bytes.get(self.next_bit_offset / 8)?;
            let bit = (byte >> (7 - self.next_bit_offset % 8)) & 1;
            self.next_bit_offset += 1;
            value = (value << 1) | u32::from(bit);
        }
        Some(value)
    }

    fn skip_next_bits(&mut self, bit_count: u32) -> Option<()> {
        let past_the_last_bit_read = self
            .next_bit_offset
            .checked_add(bit_count as usize)
            .filter(|end| *end <= self.rbsp_bytes.len() * 8)?;
        self.next_bit_offset = past_the_last_bit_read;
        Some(())
    }

    /// `ue(v)` — ITU-T H.265 §9.2: a run of zero bits, a one bit, then that
    /// many suffix bits.
    fn read_next_unsigned_exponential_golomb(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0u32;
        while self.read_next_bits(1)? == 0 {
            leading_zero_bits += 1;
            if leading_zero_bits >= u32::BITS {
                // A codeword this wide describes no syntax element an SPS has;
                // it is a run of padding being read as if it were a value.
                return None;
            }
        }
        if leading_zero_bits == 0 {
            return Some(0);
        }
        let suffix = u64::from(self.read_next_bits(leading_zero_bits)?);
        let value = (1u64 << leading_zero_bits) - 1 + suffix;
        u32::try_from(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp4_atom::{Encode, Stsd};

    /// Writes the syntax elements a parameter set is made of, so a fixture
    /// states a stream's shape rather than a hex blob nobody can check.
    #[derive(Default)]
    struct ParameterSetBitWriter {
        bytes: Vec<u8>,
        bits_free_in_last_byte: u32,
    }

    impl ParameterSetBitWriter {
        fn bit(&mut self, value: u8) -> &mut Self {
            if self.bits_free_in_last_byte == 0 {
                self.bytes.push(0);
                self.bits_free_in_last_byte = 8;
            }
            self.bits_free_in_last_byte -= 1;
            let last = self.bytes.len() - 1;
            self.bytes[last] |= (value & 1) << self.bits_free_in_last_byte;
            self
        }

        fn bits(&mut self, value: u64, count: u32) -> &mut Self {
            for index in (0..count).rev() {
                self.bit(((value >> index) & 1) as u8);
            }
            self
        }

        fn unsigned_exp_golomb(&mut self, value: u32) -> &mut Self {
            let code_number = value + 1;
            let significant_bits = u32::BITS - code_number.leading_zeros();
            self.bits(0, significant_bits - 1);
            self.bits(u64::from(code_number), significant_bits)
        }

        /// The `rbsp_trailing_bits()` every NAL unit ends on, behind the NAL
        /// header the container keeps.
        fn finish(&mut self, nal_unit_header: &[u8]) -> Vec<u8> {
            self.bit(1);
            while self.bits_free_in_last_byte != 0 {
                self.bit(0);
            }
            let mut nal_unit = nal_unit_header.to_vec();
            nal_unit.extend_from_slice(&self.bytes);
            nal_unit
        }
    }

    /// A constrained-baseline SPS for 320x180 4:2:0 progressive: 20 macroblocks
    /// across, 12 map units down (192 coded), the bottom 12 rows cropped away.
    /// `profile_idc` 66, constraint flags `0xC0`, `level_idc` 30 — ITU-T H.264
    /// §7.3.2.1.
    fn h264_baseline_320x180_sequence_parameter_set() -> Vec<u8> {
        let mut writer = ParameterSetBitWriter::default();
        writer
            .bits(66, 8) // profile_idc — constrained baseline
            .bits(0xC0, 8) // constraint_set0_flag, constraint_set1_flag
            .bits(30, 8) // level_idc — 3.0
            .unsigned_exp_golomb(0) // seq_parameter_set_id
            .unsigned_exp_golomb(0) // log2_max_frame_num_minus4
            .unsigned_exp_golomb(2) // pic_order_cnt_type
            .unsigned_exp_golomb(1) // max_num_ref_frames
            .bit(0) // gaps_in_frame_num_value_allowed_flag
            .unsigned_exp_golomb(19) // pic_width_in_mbs_minus1
            .unsigned_exp_golomb(11) // pic_height_in_map_units_minus1
            .bit(1) // frame_mbs_only_flag
            .bit(1) // direct_8x8_inference_flag
            .bit(1) // frame_cropping_flag
            .unsigned_exp_golomb(0) // frame_crop_left_offset
            .unsigned_exp_golomb(0) // frame_crop_right_offset
            .unsigned_exp_golomb(0) // frame_crop_top_offset
            .unsigned_exp_golomb(6) // frame_crop_bottom_offset — 12 luma rows
            .bit(0); // vui_parameters_present_flag
        writer.finish(&[0x67])
    }

    fn h264_picture_parameter_set() -> Vec<u8> {
        let mut writer = ParameterSetBitWriter::default();
        writer
            .unsigned_exp_golomb(0) // pic_parameter_set_id
            .unsigned_exp_golomb(0) // seq_parameter_set_id
            .bit(0) // entropy_coding_mode_flag
            .bit(0) // bottom_field_pic_order_in_frame_present_flag
            .unsigned_exp_golomb(0) // num_slice_groups_minus1
            .unsigned_exp_golomb(0) // num_ref_idx_l0_default_active_minus1
            .unsigned_exp_golomb(0) // num_ref_idx_l1_default_active_minus1
            .bit(0) // weighted_pred_flag
            .bits(0, 2) // weighted_bipred_idc
            .unsigned_exp_golomb(0) // pic_init_qp_minus26
            .unsigned_exp_golomb(0) // pic_init_qs_minus26
            .unsigned_exp_golomb(0) // chroma_qp_index_offset
            .bit(1) // deblocking_filter_control_present_flag
            .bit(0) // constrained_intra_pred_flag
            .bit(0); // redundant_pic_cnt_present_flag
        writer.finish(&[0x68])
    }

    fn h264_parameter_sets() -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: vec![],
            sequence_parameter_set_nal_units: vec![h264_baseline_320x180_sequence_parameter_set()],
            picture_parameter_set_nal_units: vec![h264_picture_parameter_set()],
        }
    }

    /// A Main-profile, main-tier, level 3.1 SPS for 320x180 4:2:0 8-bit — ITU-T
    /// H.265 §7.3.2.2.1 down to the bit depths, which is as far as `hvcC` reads.
    fn h265_main_profile_320x180_sequence_parameter_set() -> Vec<u8> {
        let mut writer = ParameterSetBitWriter::default();
        writer
            .bits(0, 4) // sps_video_parameter_set_id
            .bits(0, 3) // sps_max_sub_layers_minus1
            .bit(1) // sps_temporal_id_nesting_flag
            // profile_tier_level(1, 0)
            .bits(0, 2) // general_profile_space
            .bit(0) // general_tier_flag — main tier
            .bits(1, 5) // general_profile_idc — Main
            .bits(0x6000_0000, 32) // general_profile_compatibility_flags
            .bit(1) // general_progressive_source_flag
            .bit(0) // general_interlaced_source_flag
            .bit(1) // general_non_packed_constraint_flag
            .bit(1) // general_frame_only_constraint_flag
            .bits(0, 32) // general_reserved_zero_43bits, first half
            .bits(0, 12) // general_reserved_zero_43bits and general_inbld_flag
            .bits(93, 8) // general_level_idc — 3.1
            .unsigned_exp_golomb(0) // sps_seq_parameter_set_id
            .unsigned_exp_golomb(1) // chroma_format_idc — 4:2:0
            .unsigned_exp_golomb(320) // pic_width_in_luma_samples
            .unsigned_exp_golomb(180) // pic_height_in_luma_samples
            .bit(0) // conformance_window_flag
            .unsigned_exp_golomb(0) // bit_depth_luma_minus8
            .unsigned_exp_golomb(0) // bit_depth_chroma_minus8
            .unsigned_exp_golomb(4); // log2_max_pic_order_cnt_lsb_minus4
        writer.finish(&[0x42, 0x01])
    }

    fn h265_parameter_sets() -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: vec![vec![0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF]],
            sequence_parameter_set_nal_units: vec![
                h265_main_profile_320x180_sequence_parameter_set(),
            ],
            picture_parameter_set_nal_units: vec![vec![0x44, 0x01, 0xC1, 0x72, 0xB4, 0x62, 0x40]],
        }
    }

    #[test]
    fn an_avc_configuration_record_states_the_profile_flags_and_level_the_sps_payload_carries() {
        let entry = build_video_sample_entry("h264", &h264_parameter_sets(), 320, 180)
            .expect("the parameter sets are complete");

        let Codec::Avc1(avc1) = entry.cmaf_track_sample_entry.into_stsd_sample_entry() else {
            panic!("an h264 track is described by an `avc1` entry");
        };
        assert_eq!(avc1.avcc.avc_profile_indication, 66);
        assert_eq!(avc1.avcc.profile_compatibility, 0xC0);
        assert_eq!(avc1.avcc.avc_level_indication, 30);
        assert_eq!(
            avc1.avcc.length_size, NAL_UNIT_LENGTH_PREFIX_BYTES,
            "the width `avcC` declares and the width the samples are written with are one constant"
        );
        assert_eq!(avc1.visual.width, 320);
        assert_eq!(avc1.visual.height, 180);
        assert_eq!(avc1.visual.data_reference_index, 1);
    }

    #[test]
    fn an_avc_configuration_record_carries_every_parameter_set_the_sync_point_delivered() {
        let parameter_sets = h264_parameter_sets();
        let entry = build_video_sample_entry("h264", &parameter_sets, 320, 180)
            .expect("the parameter sets are complete");

        let Codec::Avc1(avc1) = entry.cmaf_track_sample_entry.into_stsd_sample_entry() else {
            panic!("an h264 track is described by an `avc1` entry");
        };
        assert_eq!(
            avc1.avcc.sequence_parameter_sets, parameter_sets.sequence_parameter_set_nal_units,
            "the sets go in as raw NAL units, header kept and no start code"
        );
        assert_eq!(
            avc1.avcc.picture_parameter_sets,
            parameter_sets.picture_parameter_set_nal_units
        );
    }

    #[test]
    fn the_h264_catalog_string_is_the_three_sps_bytes_as_lowercase_hex() {
        let codec_string = rfc6381_codec_string_for_video("h264", &h264_parameter_sets())
            .expect("the parameter sets are complete");

        assert_eq!(codec_string, "avc1.42c01e");
    }

    #[test]
    fn a_video_track_is_told_the_same_codec_by_its_sample_entry_and_by_the_catalog() {
        let entry = build_video_sample_entry("h264", &h264_parameter_sets(), 320, 180)
            .expect("the parameter sets are complete");

        assert_eq!(
            entry.rfc6381_codec_string,
            rfc6381_codec_string_for_video("h264", &h264_parameter_sets())
                .expect("the parameter sets are complete")
        );
    }

    #[test]
    fn a_sample_entry_goes_into_an_stsd_as_it_stands() {
        let video = build_video_sample_entry("h264", &h264_parameter_sets(), 320, 180)
            .expect("the parameter sets are complete");
        let audio = build_opus_sample_entry(2, 48_000, 312).expect("stereo opus is expressible");

        let sample_description = Stsd {
            codecs: vec![
                video.cmaf_track_sample_entry.into_stsd_sample_entry(),
                audio.cmaf_track_sample_entry.into_stsd_sample_entry(),
            ],
        };
        let mut encoded = Vec::new();
        sample_description
            .encode(&mut encoded)
            .expect("both entries encode");

        assert_eq!(&encoded[4..8], b"stsd");
        assert!(
            encoded.windows(4).any(|kind| kind == b"avc1"),
            "the video entry is in the sample description"
        );
        assert!(
            encoded.windows(4).any(|kind| kind == b"Opus"),
            "the audio entry is in the sample description"
        );
    }

    #[test]
    fn a_video_track_whose_sync_point_carried_no_picture_parameter_set_is_refused_by_name() {
        let mut parameter_sets = h264_parameter_sets();
        parameter_sets.picture_parameter_set_nal_units.clear();

        let refusal = build_video_sample_entry("h264", &parameter_sets, 320, 180)
            .expect_err("a track with no PPS cannot be described");

        let message = refusal.to_string();
        assert!(message.contains("h264"), "{message}");
        assert!(message.contains("picture parameter set"), "{message}");
        assert!(
            !message.contains("sequence parameter set"),
            "the SPS was delivered, so it is not named as missing: {message}"
        );
    }

    #[test]
    fn an_h265_track_missing_its_video_parameter_set_names_that_set_and_not_the_others() {
        let mut parameter_sets = h265_parameter_sets();
        parameter_sets.video_parameter_set_nal_units.clear();

        let refusal = rfc6381_codec_string_for_video("h265", &parameter_sets)
            .expect_err("a track with no VPS cannot be described");

        let message = refusal.to_string();
        assert!(message.contains("h265"), "{message}");
        assert!(message.contains("video parameter set"), "{message}");
    }

    #[test]
    fn a_sequence_parameter_set_too_short_to_state_profile_and_level_is_refused_by_name() {
        let parameter_sets = ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: vec![],
            sequence_parameter_set_nal_units: vec![vec![0x67, 0x42, 0xC0]],
            picture_parameter_set_nal_units: vec![h264_picture_parameter_set()],
        };

        let refusal = build_video_sample_entry("h264", &parameter_sets, 320, 180)
            .expect_err("three bytes cannot state a profile, flags and a level");

        let message = refusal.to_string();
        assert!(message.contains("3 bytes"), "{message}");
        assert!(message.contains("too short"), "{message}");
    }

    #[test]
    fn a_codec_this_container_path_does_not_write_is_refused_by_name() {
        let refusal = build_video_sample_entry("av1", &h264_parameter_sets(), 320, 180)
            .expect_err("this path writes avc1 and hvc1 only");

        assert!(refusal.to_string().contains("av1"), "{refusal}");
    }

    #[test]
    fn an_hevc_configuration_record_states_the_chroma_format_and_bit_depths_the_sps_carries() {
        let entry = build_video_sample_entry("h265", &h265_parameter_sets(), 320, 180)
            .expect("the parameter sets are complete");

        let Codec::Hvc1(hvc1) = entry.cmaf_track_sample_entry.into_stsd_sample_entry() else {
            panic!("an h265 track is described by an `hvc1` entry");
        };
        assert_eq!(hvc1.hvcc.chroma_format_idc, 1, "4:2:0");
        assert_eq!(hvc1.hvcc.bit_depth_luma_minus8, 0);
        assert_eq!(hvc1.hvcc.bit_depth_chroma_minus8, 0);
        assert_eq!(hvc1.hvcc.num_temporal_layers, 1);
        assert_eq!(hvc1.hvcc.general_profile_idc, 1, "Main");
        assert_eq!(hvc1.hvcc.general_level_idc, 93, "level 3.1");
        assert!(!hvc1.hvcc.general_tier_flag, "main tier");
        assert_eq!(
            hvc1.hvcc.general_profile_compatibility_flags,
            [0x60, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            hvc1.hvcc.length_size_minus_one,
            NAL_UNIT_LENGTH_PREFIX_BYTES - 1,
            "`hvcC` states the raw wire value, one less than the width the samples use"
        );
        assert_eq!(hvc1.hvcc.configuration_version, 1);
    }

    #[test]
    fn an_hevc_configuration_record_keeps_one_complete_array_per_parameter_set_kind() {
        let parameter_sets = h265_parameter_sets();
        let entry = build_video_sample_entry("h265", &parameter_sets, 320, 180)
            .expect("the parameter sets are complete");

        let Codec::Hvc1(hvc1) = entry.cmaf_track_sample_entry.into_stsd_sample_entry() else {
            panic!("an h265 track is described by an `hvc1` entry");
        };
        let kinds: Vec<u8> = hvc1
            .hvcc
            .arrays
            .iter()
            .map(|array| array.nal_unit_type)
            .collect();
        assert_eq!(kinds, vec![32, 33, 34]);
        assert!(hvc1.hvcc.arrays.iter().all(|array| array.completeness));
        assert_eq!(
            hvc1.hvcc.arrays[0].nalus,
            parameter_sets.video_parameter_set_nal_units
        );
    }

    #[test]
    fn the_h265_catalog_string_reverses_the_compatibility_flag_bits_and_drops_trailing_zeros() {
        let codec_string = rfc6381_codec_string_for_video("h265", &h265_parameter_sets())
            .expect("the parameter sets are complete");

        assert_eq!(codec_string, "hvc1.1.6.L93.B0");
    }

    #[test]
    fn an_h265_parameter_set_that_is_only_a_nal_header_is_refused_by_name() {
        let mut parameter_sets = h265_parameter_sets();
        parameter_sets.picture_parameter_set_nal_units = vec![vec![0x44, 0x01]];

        let refusal = build_video_sample_entry("h265", &parameter_sets, 320, 180)
            .expect_err("a set with no payload configures nothing");

        let message = refusal.to_string();
        assert!(message.contains("type 34"), "{message}");
        assert!(message.contains("2 bytes"), "{message}");
    }

    #[test]
    fn an_h265_sequence_parameter_set_cut_short_of_the_bit_depths_is_refused_rather_than_guessed() {
        let full = h265_main_profile_320x180_sequence_parameter_set();
        let mut parameter_sets = h265_parameter_sets();
        parameter_sets.sequence_parameter_set_nal_units = vec![full[..15].to_vec()];

        let refusal = build_video_sample_entry("h265", &parameter_sets, 320, 180)
            .expect_err("a truncated SPS states no chroma format");

        assert!(refusal.to_string().contains("h265"), "{refusal}");
    }

    #[test]
    fn a_frame_size_a_visual_sample_entry_cannot_state_is_refused_by_name() {
        let refusal = build_video_sample_entry("h264", &h264_parameter_sets(), 70_000, 180)
            .expect_err("a visual entry states each dimension in sixteen bits");

        assert!(refusal.to_string().contains("70000x180"), "{refusal}");
    }

    #[test]
    fn the_opus_sample_entry_states_the_pre_skip_the_encoder_reported() {
        let entry = build_opus_sample_entry(2, 48_000, 312).expect("stereo opus is expressible");

        let Codec::Opus(opus) = entry.cmaf_track_sample_entry.into_stsd_sample_entry() else {
            panic!("an opus track is described by an `Opus` entry");
        };
        assert_eq!(opus.dops.pre_skip, 312);
        assert_eq!(opus.dops.output_channel_count, 2);
        assert_eq!(opus.dops.input_sample_rate, 48_000);
        assert_eq!(opus.audio.channel_count, 2);
        assert_eq!(opus.audio.data_reference_index, 1);
        assert_eq!(
            opus.audio.sample_rate.integer(),
            OPUS_TRACK_TIMESCALE_HZ as u16,
            "Opus-in-ISOBMFF fixes the entry's own rate field at 48 kHz"
        );
        assert_eq!(entry.rfc6381_codec_string, OPUS_RFC6381_CODEC_STRING);
    }

    #[test]
    fn an_opus_track_of_more_than_two_channels_is_refused_by_name_with_the_packaging_that_holds_it()
    {
        let refusal =
            build_opus_sample_entry(6, 48_000, 312).expect_err("family 1 has no representation");

        let message = refusal.to_string();
        assert!(message.contains("6 channels"), "{message}");
        assert!(message.contains("family 1"), "{message}");
        assert!(
            message.contains("streamlib_bag"),
            "the refusal names the packaging that does carry it: {message}"
        );
    }

    #[test]
    fn an_opus_track_of_no_channels_is_refused_by_name() {
        let refusal =
            build_opus_sample_entry(0, 48_000, 312).expect_err("no decoder emits zero channels");

        assert!(refusal.to_string().contains("zero channels"), "{refusal}");
    }

    #[test]
    fn a_pre_skip_past_what_dops_can_state_is_refused_rather_than_truncated() {
        let refusal =
            build_opus_sample_entry(2, 48_000, 70_000).expect_err("a `dOps` PreSkip is 16 bits");

        assert!(refusal.to_string().contains("70000"), "{refusal}");
    }

    #[test]
    fn the_exponential_golomb_reader_refuses_a_read_past_the_end_rather_than_returning_a_number() {
        // Seven zeros then the one bit end the byte, so the seven suffix bits
        // the codeword promises are past the last byte.
        let mut whole_byte = RbspBitReaderThatRefusesToReadPastTheEnd::over(&[0b0000_0001]);
        assert_eq!(whole_byte.read_next_bits(8), Some(0b0000_0001));

        let mut truncated = RbspBitReaderThatRefusesToReadPastTheEnd::over(&[0b0000_0001]);
        assert_eq!(
            truncated.read_next_unsigned_exponential_golomb(),
            None,
            "the suffix runs past the last byte, so there is no value to report"
        );
    }

    #[test]
    fn the_exponential_golomb_reader_refuses_an_all_zero_run_instead_of_reading_forever() {
        let mut reader = RbspBitReaderThatRefusesToReadPastTheEnd::over(&[0u8; 16]);

        assert_eq!(reader.read_next_unsigned_exponential_golomb(), None);
    }

    #[test]
    fn the_exponential_golomb_reader_reads_the_codewords_the_standard_defines() {
        let codewords = [0u32, 1, 2, 3, 8, 319, 65_535];
        let mut writer = ParameterSetBitWriter::default();
        for value in codewords {
            writer.unsigned_exp_golomb(value);
        }
        let written = writer.finish(&[]);

        let mut reader = RbspBitReaderThatRefusesToReadPastTheEnd::over(&written);
        for value in codewords {
            assert_eq!(
                reader.read_next_unsigned_exponential_golomb(),
                Some(value),
                "ue({value})"
            );
        }
    }

    #[test]
    fn a_sample_entry_says_which_medium_its_track_carries() {
        let video = build_video_sample_entry("h264", &h264_parameter_sets(), 320, 180)
            .expect("the parameter sets are complete");
        let audio = build_opus_sample_entry(1, 48_000, 312).expect("mono opus is expressible");

        assert_eq!(
            video.cmaf_track_sample_entry.track_medium(),
            TrackMedium::Video
        );
        assert_eq!(
            audio.cmaf_track_sample_entry.track_medium(),
            TrackMedium::Audio
        );
    }
}
