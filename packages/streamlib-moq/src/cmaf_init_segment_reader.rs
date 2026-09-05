// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The subscriber's half of the init object: everything a bag needs that the
//! fragments do not carry.
//!
//! A CMAF fragment states a decode time, a duration and a sync flag and
//! nothing else. The codec, the parameter sets, the coded extent and the Opus
//! configuration are read out of this one object or out of nothing —
//! ISO/IEC 14496-12 §6.1.2 puts the sample entries in the one `moov`, and
//! `avc1`/`hvc1` forbid in-band parameter sets, so a track this reader cannot
//! describe is a track no decoder can be configured for.

use mp4_atom::{Atom, Avcc, Codec, Decode, Encode, Ftyp, Header, Hvcc, Moov, Trak, Visual};

use crate::annex_b_access_unit::{
    AnnexBNalHeaderGrammar, H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET,
    H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET, H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET,
    ParameterSetsFromAnnexBAccessUnit,
};
use crate::encoded_media_sample::TrackMedium;
use crate::error::{MoqExtensionError, Result};

/// What a refusal on this path calls the container it was reading.
const CMAF_CONTAINER_NAME: &str = "cmaf";

/// The bag's `codec` spelling for a track an `avc1` sample entry describes.
const H264_WIRE_CODEC: &str = "h264";

/// The bag's `codec` spelling for a track an `hvc1` or `hev1` sample entry
/// describes.
const H265_WIRE_CODEC: &str = "h265";

/// The four-character code occupies bytes 4..8 of every ISOBMFF box, behind
/// the 32-bit size — ISO/IEC 14496-12 §4.2.
const BOX_HEADER_BYTES: usize = 8;
const BOX_KIND_BYTE_RANGE: std::ops::Range<usize> = 4..BOX_HEADER_BYTES;

/// One track, as the init object describes it to a subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CmafTrackDescriptionFromTheInitSegment {
    /// The `tkhd.track_id`, which is also what names this track's MoQ media
    /// track — a subscriber asks for `{track_id}.m4s`.
    pub(crate) track_id: u32,
    /// Which medium this track's samples carry.
    pub(crate) track_medium: TrackMedium,
    /// The clock every decode time and duration in this track's fragments is
    /// stated on.
    pub(crate) media_timescale_hz: u32,
    /// Video only: the wire codec spelling ("h264" / "h265") the sample entry implies.
    pub(crate) wire_codec: Option<String>,
    /// Video only: the parameter sets a bag's bitstream must carry inline at
    /// every sync point, ordered VPS then SPS then PPS — a decoder handed a
    /// PPS before the SPS it refers to discards it.
    pub(crate) parameter_set_nal_units: Vec<Vec<u8>>,
    /// Video only: the coded extent the visual sample entry states.
    pub(crate) coded_extent: Option<(u32, u32)>,
    /// Audio only.
    pub(crate) channels: Option<u32>,
    /// Audio only.
    pub(crate) sample_rate: Option<u32>,
    /// Audio only.
    pub(crate) pre_skip: Option<u32>,
}

/// Read every track the init object describes, in the order its `moov` lists
/// them.
pub(crate) fn read_cmaf_init_segment(
    init_segment_bytes: &[u8],
) -> Result<Vec<CmafTrackDescriptionFromTheInitSegment>> {
    let moov = decode_the_moov_of_the_init_segment(init_segment_bytes)?;
    if moov.trak.is_empty() {
        return Err(refuse_as_malformed_cmaf_init_segment(
            "the moov describes no tracks, so this broadcast configures no decoder".to_owned(),
        ));
    }
    let track_descriptions: Vec<CmafTrackDescriptionFromTheInitSegment> = moov
        .trak
        .iter()
        .map(describe_the_track_a_moov_entry_carries)
        .collect::<Result<_>>()?;
    refuse_if_two_traks_share_a_track_id(&track_descriptions)?;
    Ok(track_descriptions)
}

/// `ftyp` immediately followed by `moov`, and nothing else — the shape the
/// reference subscriber asserts literally, reading two atoms and checking
/// bytes 4..8 of each.
fn decode_the_moov_of_the_init_segment(init_segment_bytes: &[u8]) -> Result<Moov> {
    let mut unread_init_segment_bytes: &[u8] = init_segment_bytes;

    let ftyp_header = Header::decode(&mut unread_init_segment_bytes).map_err(|failure| {
        refuse_as_malformed_cmaf_init_segment(format!(
            "the object is too short to open with an ftyp atom header: {failure}"
        ))
    })?;
    if ftyp_header.kind != Ftyp::KIND {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the object opens with a `{}` atom, but an init segment opens with an ftyp",
            ftyp_header.kind
        )));
    }
    let ftyp_body_byte_count = ftyp_header.size.ok_or_else(|| {
        refuse_as_malformed_cmaf_init_segment(
            "the ftyp atom declares no size, so the moov that must follow it cannot be found"
                .to_owned(),
        )
    })?;
    if ftyp_body_byte_count > unread_init_segment_bytes.len() {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the object declares a {ftyp_body_byte_count} byte ftyp body but carries only {} bytes after the ftyp header",
            unread_init_segment_bytes.len()
        )));
    }
    unread_init_segment_bytes = &unread_init_segment_bytes[ftyp_body_byte_count..];

    let moov_header = Header::decode(&mut unread_init_segment_bytes).map_err(|failure| {
        refuse_as_malformed_cmaf_init_segment(format!(
            "the ftyp atom is not followed by a moov atom header: {failure}"
        ))
    })?;
    if moov_header.kind != Moov::KIND {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the ftyp atom is followed by a `{}` atom, but an init segment's ftyp is followed by its moov",
            moov_header.kind
        )));
    }
    // A zero size field is ISO/IEC 14496-12 §4.2's "to the end of the
    // enclosing container", which is a length this reader would have to take
    // on faith rather than check anything against.
    let moov_body_byte_count = moov_header.size.ok_or_else(|| {
        refuse_as_malformed_cmaf_init_segment(
            "the moov atom declares a size of 0, meaning it runs to the end of the object, so \
             nothing states where its last child box ends — an init object whose ftyp must \
             declare its own size declares its moov's too"
                .to_owned(),
        )
    })?;
    if moov_body_byte_count > unread_init_segment_bytes.len() {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the object declares a {moov_body_byte_count} byte moov body but carries only {} bytes after the moov header",
            unread_init_segment_bytes.len()
        )));
    }
    let mut unread_moov_body_bytes = &unread_init_segment_bytes[..moov_body_byte_count];
    let moov = Moov::decode_body(&mut unread_moov_body_bytes).map_err(|failure| {
        refuse_as_malformed_cmaf_init_segment(format!("the moov atom does not parse: {failure}"))
    })?;
    // `Moov::decode_body` stops at the first child it cannot read whole and
    // reports success on what it got, so a trak truncated inside a moov of the
    // declared length would otherwise vanish along with its whole track.
    if !unread_moov_body_bytes.is_empty() {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the moov's last {} bytes are not a child box that could be read whole, so a trak \
             they describe would be dropped without account",
            unread_moov_body_bytes.len()
        )));
    }

    let byte_count_after_the_moov = unread_init_segment_bytes.len() - moov_body_byte_count;
    if byte_count_after_the_moov != 0 {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the object carries {byte_count_after_the_moov} bytes after the moov, but an init segment is an ftyp and a moov and nothing else"
        )));
    }
    Ok(moov)
}

/// A subscriber reaches a track's media as `{track_id}.m4s`, so two traks
/// sharing an id name one MoQ track between them.
fn refuse_if_two_traks_share_a_track_id(
    track_descriptions: &[CmafTrackDescriptionFromTheInitSegment],
) -> Result<()> {
    let mut track_ids_already_described: Vec<u32> = Vec::with_capacity(track_descriptions.len());
    for track_description in track_descriptions {
        if track_ids_already_described.contains(&track_description.track_id) {
            return Err(refuse_as_malformed_cmaf_init_segment(format!(
                "two traks both state track_id {track_id}, so the media track named \
                 `{track_id}.m4s` describes neither of them",
                track_id = track_description.track_id
            )));
        }
        track_ids_already_described.push(track_description.track_id);
    }
    Ok(())
}

fn describe_the_track_a_moov_entry_carries(
    trak: &Trak,
) -> Result<CmafTrackDescriptionFromTheInitSegment> {
    let track_id = trak.tkhd.track_id;
    if track_id == 0 {
        return Err(refuse_as_malformed_cmaf_init_segment(
            "a trak states track_id 0, which ISO/IEC 14496-12 §8.3.2.3 reserves, so no media \
             track this subscriber could subscribe to is named by it"
                .to_owned(),
        ));
    }
    let media_timescale_hz = trak.mdia.mdhd.timescale;
    if media_timescale_hz == 0 {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "track {track_id} states an mdhd timescale of 0, which is not a clock: no decode \
             time or duration its fragments carry could be read as a time"
        )));
    }
    let sample_entry = the_one_sample_entry_of_the_track(trak, track_id)?;

    match sample_entry {
        Codec::Avc1(avc1) => describe_a_video_track(
            track_id,
            media_timescale_hz,
            AnnexBNalHeaderGrammar::H264,
            parameter_sets_of_an_avc_configuration_record(&avc1.avcc),
            &avc1.visual,
        ),
        Codec::Hvc1(hvc1) => describe_a_video_track(
            track_id,
            media_timescale_hz,
            AnnexBNalHeaderGrammar::H265,
            parameter_sets_of_an_hevc_configuration_record(&hvc1.hvcc),
            &hvc1.visual,
        ),
        Codec::Hev1(hev1) => describe_a_video_track(
            track_id,
            media_timescale_hz,
            AnnexBNalHeaderGrammar::H265,
            parameter_sets_of_an_hevc_configuration_record(&hev1.hvcc),
            &hev1.visual,
        ),
        Codec::Opus(opus) => Ok(CmafTrackDescriptionFromTheInitSegment {
            track_id,
            track_medium: TrackMedium::Audio,
            media_timescale_hz,
            wire_codec: None,
            parameter_set_nal_units: Vec::new(),
            coded_extent: None,
            channels: Some(u32::from(opus.dops.output_channel_count)),
            // RFC 7845 §5.1 makes 0 the legal spelling of "unspecified", so
            // this is carried through rather than defaulted: only a caller
            // knows what to do with a stream that states no input rate.
            sample_rate: Some(opus.dops.input_sample_rate),
            pre_skip: Some(u32::from(opus.dops.pre_skip)),
        }),
        unreadable_sample_entry => {
            let four_character_code = four_character_code_of_sample_entry(unreadable_sample_entry)?;
            Err(refuse_as_malformed_cmaf_init_segment(format!(
                "track {track_id} is described by a `{four_character_code}` sample entry, which \
                 this subscriber does not read: it reads `avc1` for H.264, `hvc1` and `hev1` for \
                 H.265, and `Opus`"
            )))
        }
    }
}

fn describe_a_video_track(
    track_id: u32,
    media_timescale_hz: u32,
    nal_header_grammar: AnnexBNalHeaderGrammar,
    parameter_sets: ParameterSetsFromAnnexBAccessUnit,
    visual: &Visual,
) -> Result<CmafTrackDescriptionFromTheInitSegment> {
    let wire_codec = wire_codec_spelling_of_nal_header_grammar(nal_header_grammar);
    if !parameter_sets.is_complete_for(nal_header_grammar) {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "track {track_id} is a {wire_codec} track whose sample entry carries no {}, and \
             `avc1`/`hvc1` forbid in-band parameter sets — so nothing on this track can ever be \
             decoded",
            missing_parameter_set_kinds_of(&parameter_sets, nal_header_grammar)
        )));
    }
    let mut parameter_set_nal_units = parameter_sets.video_parameter_set_nal_units;
    parameter_set_nal_units.extend(parameter_sets.sequence_parameter_set_nal_units);
    parameter_set_nal_units.extend(parameter_sets.picture_parameter_set_nal_units);
    Ok(CmafTrackDescriptionFromTheInitSegment {
        track_id,
        track_medium: TrackMedium::Video,
        media_timescale_hz,
        wire_codec: Some(wire_codec.to_owned()),
        parameter_set_nal_units,
        coded_extent: Some((u32::from(visual.width), u32::from(visual.height))),
        channels: None,
        sample_rate: None,
        pre_skip: None,
    })
}

/// The bag's `codec` spelling for the elementary stream a NAL header grammar
/// reads.
fn wire_codec_spelling_of_nal_header_grammar(
    nal_header_grammar: AnnexBNalHeaderGrammar,
) -> &'static str {
    match nal_header_grammar {
        AnnexBNalHeaderGrammar::H264 => H264_WIRE_CODEC,
        AnnexBNalHeaderGrammar::H265 => H265_WIRE_CODEC,
    }
}

/// Which of the sets a decoder needs are absent, spelled for a refusal.
fn missing_parameter_set_kinds_of(
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
    nal_header_grammar: AnnexBNalHeaderGrammar,
) -> String {
    let mut missing_parameter_set_kinds: Vec<&str> = Vec::new();
    if nal_header_grammar == AnnexBNalHeaderGrammar::H265
        && parameter_sets.video_parameter_set_nal_units.is_empty()
    {
        missing_parameter_set_kinds.push("video parameter set");
    }
    if parameter_sets.sequence_parameter_set_nal_units.is_empty() {
        missing_parameter_set_kinds.push("sequence parameter set");
    }
    if parameter_sets.picture_parameter_set_nal_units.is_empty() {
        missing_parameter_set_kinds.push("picture parameter set");
    }
    missing_parameter_set_kinds.join(" and no ")
}

fn parameter_sets_of_an_avc_configuration_record(avcc: &Avcc) -> ParameterSetsFromAnnexBAccessUnit {
    ParameterSetsFromAnnexBAccessUnit {
        video_parameter_set_nal_units: Vec::new(),
        sequence_parameter_set_nal_units: avcc.sequence_parameter_sets.clone(),
        picture_parameter_set_nal_units: avcc.picture_parameter_sets.clone(),
    }
}

/// `hvcC` also carries prefix-SEI and other arrays, which configure no decoder
/// and are not what the publisher lifted out of the bitstream, so only the
/// three parameter-set kinds come back.
fn parameter_sets_of_an_hevc_configuration_record(
    hvcc: &Hvcc,
) -> ParameterSetsFromAnnexBAccessUnit {
    let mut parameter_sets = ParameterSetsFromAnnexBAccessUnit::default();
    for array in &hvcc.arrays {
        let pile_for_this_nal_unit_type = match array.nal_unit_type {
            H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET => {
                &mut parameter_sets.video_parameter_set_nal_units
            }
            H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET => {
                &mut parameter_sets.sequence_parameter_set_nal_units
            }
            H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET => {
                &mut parameter_sets.picture_parameter_set_nal_units
            }
            _ => continue,
        };
        pile_for_this_nal_unit_type.extend(array.nalus.iter().cloned());
    }
    parameter_sets
}

fn the_one_sample_entry_of_the_track(trak: &Trak, track_id: u32) -> Result<&Codec> {
    match trak.mdia.minf.stbl.stsd.codecs.as_slice() {
        [only_sample_entry] => Ok(only_sample_entry),
        other_sample_entries => Err(refuse_as_malformed_cmaf_init_segment(format!(
            "track {track_id} carries {} sample entries, but a CMAF track is described by exactly \
             one — a fragment names no sample description index, so a second entry could never be \
             selected",
            other_sample_entries.len()
        ))),
    }
}

/// The code a sample entry writes itself as. `mp4_atom::Codec` exposes its
/// variant's `FourCC` no other way, so an entry is re-encoded to be named.
fn four_character_code_of_sample_entry(sample_entry: &Codec) -> Result<String> {
    if let Codec::Unknown(four_character_code) = sample_entry {
        return Ok(four_character_code.to_string());
    }
    let mut encoded_sample_entry: Vec<u8> = Vec::new();
    sample_entry
        .encode(&mut encoded_sample_entry)
        .map_err(|failure| {
            refuse_as_malformed_cmaf_init_segment(format!(
                "a sample entry this subscriber does not read could not be re-encoded to be \
                 named: {failure}"
            ))
        })?;
    // Every box writes its 32-bit size then its four-character code before any
    // body, so a sample entry that encoded at all carries both.
    Ok(String::from_utf8_lossy(&encoded_sample_entry[BOX_KIND_BYTE_RANGE]).into_owned())
}

fn refuse_as_malformed_cmaf_init_segment(what: String) -> MoqExtensionError {
    MoqExtensionError::MalformedObject {
        container: CMAF_CONTAINER_NAME,
        what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmaf_init_segment::{
        CmafTrackDescriptionForTheInitSegment, build_cmaf_init_segment,
    };
    use crate::cmaf_sample_entry::{
        CmafTrackSampleEntry, build_opus_sample_entry, build_video_sample_entry,
    };
    use crate::cmaf_track_timeline::{OPUS_TRACK_TIMESCALE_HZ, VIDEO_TRACK_TIMESCALE_HZ};
    use mp4_atom::{Avc1, Hvc1, HvcCArray, Vp08};

    /// What this tree's Opus encoder reports as its own lookahead.
    const THE_ENCODERS_PRE_SKIP: u32 = 312;

    const A_SEQUENCE_PARAMETER_SET: [u8; 8] = [0x67, 0x42, 0xC0, 0x1F, 0xDA, 0x02, 0xD0, 0x49];
    const A_PICTURE_PARAMETER_SET: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];

    const AN_H265_VIDEO_PARAMETER_SET: [u8; 4] = [0x40, 0x01, 0x0C, 0x01];
    const AN_H265_SEQUENCE_PARAMETER_SET: [u8; 4] = [0x42, 0x01, 0x01, 0x60];
    const AN_H265_PICTURE_PARAMETER_SET: [u8; 4] = [0x44, 0x01, 0xC1, 0x72];

    fn h264_parameter_sets() -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: Vec::new(),
            sequence_parameter_set_nal_units: vec![A_SEQUENCE_PARAMETER_SET.to_vec()],
            picture_parameter_set_nal_units: vec![A_PICTURE_PARAMETER_SET.to_vec()],
        }
    }

    fn an_h264_track(track_id: u32) -> CmafTrackDescriptionForTheInitSegment {
        an_h264_track_on_timescale(track_id, VIDEO_TRACK_TIMESCALE_HZ)
    }

    fn an_h264_track_on_timescale(
        track_id: u32,
        media_timescale_hz: u32,
    ) -> CmafTrackDescriptionForTheInitSegment {
        let entry = build_video_sample_entry("h264", &h264_parameter_sets(), 320, 180)
            .expect("the fixture parameter sets describe an H.264 track");
        CmafTrackDescriptionForTheInitSegment {
            track_id,
            inbound_link_name: "encoder/encoded_video".to_owned(),
            cmaf_track_sample_entry: entry.cmaf_track_sample_entry,
            media_timescale_hz,
            coded_extent: Some((320, 180)),
        }
    }

    fn an_opus_track(track_id: u32) -> CmafTrackDescriptionForTheInitSegment {
        let entry = build_opus_sample_entry(2, OPUS_TRACK_TIMESCALE_HZ, THE_ENCODERS_PRE_SKIP)
            .expect("stereo Opus fits this container path");
        CmafTrackDescriptionForTheInitSegment {
            track_id,
            inbound_link_name: "encoder/encoded_audio".to_owned(),
            cmaf_track_sample_entry: entry.cmaf_track_sample_entry,
            media_timescale_hz: OPUS_TRACK_TIMESCALE_HZ,
            coded_extent: None,
        }
    }

    /// `mp4-atom` refuses to write a record whose `length_size` is zero or
    /// whose version is not 1, which the derived `Default` is both of.
    fn an_avcc_a_writer_can_encode() -> Avcc {
        Avcc {
            configuration_version: 1,
            length_size: 4,
            ..Default::default()
        }
    }

    fn an_hvcc_carrying(arrays: Vec<HvcCArray>) -> Hvcc {
        let mut hvcc = Hvcc::new();
        hvcc.arrays = arrays;
        hvcc
    }

    fn an_hvcc_array(nal_unit_type: u8, nal_unit: &[u8]) -> HvcCArray {
        HvcCArray {
            completeness: true,
            nal_unit_type,
            nalus: vec![nal_unit.to_vec()],
        }
    }

    fn a_track_described_by(
        track_id: u32,
        sample_entry: CmafTrackSampleEntry,
    ) -> CmafTrackDescriptionForTheInitSegment {
        CmafTrackDescriptionForTheInitSegment {
            track_id,
            inbound_link_name: "encoder/encoded_video".to_owned(),
            cmaf_track_sample_entry: sample_entry,
            media_timescale_hz: VIDEO_TRACK_TIMESCALE_HZ,
            coded_extent: Some((320, 180)),
        }
    }

    fn init_segment_bytes_of(tracks: &[CmafTrackDescriptionForTheInitSegment]) -> Vec<u8> {
        build_cmaf_init_segment(tracks)
            .expect("the fixture tracks describe a broadcast")
            .to_vec()
    }

    /// Where the moov box starts: right past the ftyp, whose 32-bit size is
    /// the object's first four bytes.
    fn moov_box_offset_of(init_segment_bytes: &[u8]) -> usize {
        let mut ftyp_box_size_field = [0u8; 4];
        ftyp_box_size_field.copy_from_slice(&init_segment_bytes[..4]);
        u32::from_be_bytes(ftyp_box_size_field) as usize
    }

    /// Cut bytes off the tail of the object and restate the moov's size to
    /// match, so what is short is the last trak and not the moov.
    fn with_the_last_trak_truncated_by(
        init_segment_bytes: &[u8],
        truncated_byte_count: usize,
    ) -> Vec<u8> {
        let moov_box_offset = moov_box_offset_of(init_segment_bytes);
        let mut truncated_init_segment_bytes =
            init_segment_bytes[..init_segment_bytes.len() - truncated_byte_count].to_vec();
        let moov_box_byte_count = (truncated_init_segment_bytes.len() - moov_box_offset) as u32;
        truncated_init_segment_bytes[moov_box_offset..moov_box_offset + 4]
            .copy_from_slice(&moov_box_byte_count.to_be_bytes());
        truncated_init_segment_bytes
    }

    fn with_the_moov_size_field_zeroed(init_segment_bytes: &[u8]) -> Vec<u8> {
        let moov_box_offset = moov_box_offset_of(init_segment_bytes);
        let mut zeroed_init_segment_bytes = init_segment_bytes.to_vec();
        zeroed_init_segment_bytes[moov_box_offset..moov_box_offset + 4]
            .copy_from_slice(&0u32.to_be_bytes());
        zeroed_init_segment_bytes
    }

    /// Overwrite the sample entry's four-character code in place, which is the
    /// only way to state a code no `mp4-atom` variant covers — encoding a
    /// `Codec::Unknown` writes the bare code and no box around it.
    fn with_the_avc1_sample_entry_kind_replaced(
        init_segment_bytes: &[u8],
        replacement_four_character_code: &[u8; 4],
    ) -> Vec<u8> {
        let mut replaced_init_segment_bytes = init_segment_bytes.to_vec();
        let sample_entry_kind_offset = replaced_init_segment_bytes
            .windows(4)
            .position(|window| window == b"avc1")
            .expect("the fixture track is described by an avc1 sample entry");
        replaced_init_segment_bytes
            [sample_entry_kind_offset..sample_entry_kind_offset + BOX_KIND_BYTE_RANGE.len()]
            .copy_from_slice(replacement_four_character_code);
        replaced_init_segment_bytes
    }

    /// Re-encodes an init segment with the first track's `stsd` replaced,
    /// which is the only way to state a sample-entry count the writer refuses
    /// to write.
    fn with_the_first_tracks_sample_entries_replaced(
        init_segment_bytes: &[u8],
        sample_entries: Vec<Codec>,
    ) -> Vec<u8> {
        let mut unread = init_segment_bytes;
        let ftyp = Ftyp::decode(&mut unread).expect("the object opens with an ftyp");
        let mut moov = Moov::decode(&mut unread).expect("the ftyp is followed by a moov");
        moov.trak[0].mdia.minf.stbl.stsd.codecs = sample_entries;

        let mut rebuilt_init_segment_bytes: Vec<u8> = Vec::new();
        ftyp.encode(&mut rebuilt_init_segment_bytes)
            .expect("an ftyp that decoded encodes");
        moov.encode(&mut rebuilt_init_segment_bytes)
            .expect("a moov that decoded encodes");
        rebuilt_init_segment_bytes
    }

    #[test]
    fn an_h264_tracks_parameter_sets_come_back_byte_identical_with_the_sps_before_the_pps() {
        let tracks = read_cmaf_init_segment(&init_segment_bytes_of(&[an_h264_track(1)]))
            .expect("an H.264 init segment reads back");

        assert_eq!(
            tracks[0].parameter_set_nal_units,
            vec![
                A_SEQUENCE_PARAMETER_SET.to_vec(),
                A_PICTURE_PARAMETER_SET.to_vec()
            ],
            "a decoder handed the PPS before its SPS discards it"
        );
        assert_eq!(tracks[0].wire_codec.as_deref(), Some("h264"));
        assert_eq!(tracks[0].track_medium, TrackMedium::Video);
    }

    #[test]
    fn an_h265_tracks_parameter_sets_come_back_as_vps_then_sps_then_pps() {
        // Listed picture-set-first, which is a legal `hvcC`: the arrays are a
        // set, and the order a decoder needs is not the order a writer used.
        let scrambled_h265_track = a_track_described_by(
            1,
            CmafTrackSampleEntry::Video(Codec::Hvc1(Hvc1 {
                hvcc: an_hvcc_carrying(vec![
                    an_hvcc_array(
                        H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET,
                        &AN_H265_PICTURE_PARAMETER_SET,
                    ),
                    an_hvcc_array(
                        H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET,
                        &AN_H265_VIDEO_PARAMETER_SET,
                    ),
                    an_hvcc_array(
                        H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET,
                        &AN_H265_SEQUENCE_PARAMETER_SET,
                    ),
                ]),
                ..Default::default()
            })),
        );

        let tracks = read_cmaf_init_segment(&init_segment_bytes_of(&[scrambled_h265_track]))
            .expect("an H.265 init segment reads back");

        assert_eq!(
            tracks[0].parameter_set_nal_units,
            vec![
                AN_H265_VIDEO_PARAMETER_SET.to_vec(),
                AN_H265_SEQUENCE_PARAMETER_SET.to_vec(),
                AN_H265_PICTURE_PARAMETER_SET.to_vec()
            ]
        );
        assert_eq!(tracks[0].wire_codec.as_deref(), Some("h265"));
    }

    #[test]
    fn a_video_tracks_coded_extent_survives_the_round_trip() {
        let tracks = read_cmaf_init_segment(&init_segment_bytes_of(&[an_h264_track(1)]))
            .expect("an H.264 init segment reads back");

        assert_eq!(tracks[0].coded_extent, Some((320, 180)));
    }

    #[test]
    fn an_opus_tracks_channels_rate_and_pre_skip_survive_the_round_trip_unrewritten() {
        let tracks = read_cmaf_init_segment(&init_segment_bytes_of(&[an_opus_track(1)]))
            .expect("an Opus init segment reads back");

        assert_eq!(tracks[0].track_medium, TrackMedium::Audio);
        assert_eq!(tracks[0].channels, Some(2));
        assert_eq!(tracks[0].sample_rate, Some(OPUS_TRACK_TIMESCALE_HZ));
        assert_eq!(
            tracks[0].pre_skip,
            Some(THE_ENCODERS_PRE_SKIP),
            "a player trims playback by this many samples, so a rewritten pre-skip destroys audio"
        );
        assert_eq!(tracks[0].wire_codec, None);
        assert_eq!(tracks[0].coded_extent, None);
    }

    #[test]
    fn every_tracks_id_and_timescale_survive_the_round_trip() {
        let tracks = read_cmaf_init_segment(&init_segment_bytes_of(&[
            an_h264_track(7),
            an_opus_track(9),
        ]))
        .expect("a two-track init segment reads back");

        assert_eq!(tracks[0].track_id, 7);
        assert_eq!(tracks[0].media_timescale_hz, VIDEO_TRACK_TIMESCALE_HZ);
        assert_eq!(tracks[1].track_id, 9);
        assert_eq!(tracks[1].media_timescale_hz, OPUS_TRACK_TIMESCALE_HZ);
    }

    #[test]
    fn a_two_track_segment_reads_back_in_the_order_the_moov_lists_its_traks() {
        let tracks = read_cmaf_init_segment(&init_segment_bytes_of(&[
            an_opus_track(4),
            an_h264_track(5),
        ]))
        .expect("a two-track init segment reads back");

        assert_eq!(tracks.len(), 2);
        assert_eq!(
            [tracks[0].track_medium, tracks[1].track_medium],
            [TrackMedium::Audio, TrackMedium::Video]
        );
    }

    #[test]
    fn a_truncated_object_is_refused_by_name() {
        let init_segment_bytes = init_segment_bytes_of(&[an_h264_track(1)]);

        let refusal = read_cmaf_init_segment(&init_segment_bytes[..init_segment_bytes.len() - 16])
            .expect_err("half a moov describes no decoder");

        assert!(
            refusal.to_string().contains("moov body"),
            "the refusal says what was short: {refusal}"
        );
    }

    #[test]
    fn a_moov_whose_last_trak_is_cut_short_is_refused_rather_than_read_back_one_track_fewer() {
        let two_tracks = init_segment_bytes_of(&[an_h264_track(1), an_opus_track(2)]);

        let refusal = read_cmaf_init_segment(&with_the_last_trak_truncated_by(&two_tracks, 16))
            .expect_err("a track the reader cannot see is a track the subscriber never gets");

        assert!(
            refusal.to_string().contains("not a child box"),
            "the refusal accounts for the bytes no trak could be read from: {refusal}"
        );
    }

    #[test]
    fn a_moov_declaring_a_size_of_zero_is_refused_by_name() {
        let init_segment_bytes =
            with_the_moov_size_field_zeroed(&init_segment_bytes_of(&[an_h264_track(1)]));

        let refusal = read_cmaf_init_segment(&init_segment_bytes)
            .expect_err("a moov running to the end of the object states no end for its children");

        assert!(refusal.to_string().contains("size of 0"), "{refusal}");
    }

    #[test]
    fn an_object_carrying_bytes_after_a_size_zero_moov_is_still_refused() {
        let mut init_segment_bytes =
            with_the_moov_size_field_zeroed(&init_segment_bytes_of(&[an_h264_track(1)]));
        init_segment_bytes.extend_from_slice(b"\0\0\0\x08free");

        let refusal = read_cmaf_init_segment(&init_segment_bytes)
            .expect_err("an init segment is an ftyp and a moov and nothing else");

        assert!(
            refusal.to_string().contains("size of 0"),
            "a size-0 moov must not swallow the free box that follows it: {refusal}"
        );
    }

    #[test]
    fn an_object_that_does_not_open_with_an_ftyp_is_refused_by_name() {
        let mut init_segment_bytes = init_segment_bytes_of(&[an_h264_track(1)]);
        init_segment_bytes[BOX_KIND_BYTE_RANGE].copy_from_slice(b"styp");

        let refusal =
            read_cmaf_init_segment(&init_segment_bytes).expect_err("a styp is not an init segment");

        assert!(
            refusal.to_string().contains("styp"),
            "the refusal names the atom it found: {refusal}"
        );
    }

    #[test]
    fn an_object_carrying_bytes_after_the_moov_is_refused_by_name() {
        let mut init_segment_bytes = init_segment_bytes_of(&[an_h264_track(1)]);
        init_segment_bytes.extend_from_slice(b"\0\0\0\x08free");

        let refusal = read_cmaf_init_segment(&init_segment_bytes)
            .expect_err("an init segment is an ftyp and a moov and nothing else");

        assert!(refusal.to_string().contains("after the moov"), "{refusal}");
    }

    #[test]
    fn a_track_described_by_no_sample_entry_is_refused_by_name() {
        let init_segment_bytes = with_the_first_tracks_sample_entries_replaced(
            &init_segment_bytes_of(&[an_h264_track(3)]),
            Vec::new(),
        );

        let refusal = read_cmaf_init_segment(&init_segment_bytes)
            .expect_err("a track with no sample entry configures no decoder");

        assert!(
            refusal.to_string().contains("track 3")
                && refusal.to_string().contains("0 sample entries"),
            "{refusal}"
        );
    }

    #[test]
    fn a_track_described_by_two_sample_entries_is_refused_by_name() {
        let an_entry = Codec::Avc1(Avc1 {
            avcc: Avcc {
                sequence_parameter_sets: vec![A_SEQUENCE_PARAMETER_SET.to_vec()],
                picture_parameter_sets: vec![A_PICTURE_PARAMETER_SET.to_vec()],
                ..an_avcc_a_writer_can_encode()
            },
            ..Default::default()
        });
        let init_segment_bytes = with_the_first_tracks_sample_entries_replaced(
            &init_segment_bytes_of(&[an_h264_track(3)]),
            vec![an_entry.clone(), an_entry],
        );

        let refusal = read_cmaf_init_segment(&init_segment_bytes)
            .expect_err("a fragment names no sample description index");

        assert!(
            refusal.to_string().contains("2 sample entries"),
            "{refusal}"
        );
    }

    #[test]
    fn a_video_track_carrying_no_parameter_sets_at_all_is_refused_naming_both_kinds() {
        let track_with_an_empty_configuration_record = a_track_described_by(
            2,
            CmafTrackSampleEntry::Video(Codec::Avc1(Avc1 {
                avcc: an_avcc_a_writer_can_encode(),
                ..Default::default()
            })),
        );

        let refusal = read_cmaf_init_segment(&init_segment_bytes_of(&[
            track_with_an_empty_configuration_record,
        ]))
        .expect_err("nothing on that track could ever be decoded");

        assert!(
            refusal.to_string().contains("track 2")
                && refusal.to_string().contains("sequence parameter set")
                && refusal.to_string().contains("picture parameter set"),
            "{refusal}"
        );
    }

    #[test]
    fn an_h264_track_carrying_a_picture_parameter_set_but_no_sequence_one_is_refused_by_name() {
        let track_with_only_a_picture_parameter_set = a_track_described_by(
            2,
            CmafTrackSampleEntry::Video(Codec::Avc1(Avc1 {
                avcc: Avcc {
                    picture_parameter_sets: vec![A_PICTURE_PARAMETER_SET.to_vec()],
                    ..an_avcc_a_writer_can_encode()
                },
                ..Default::default()
            })),
        );

        let refusal = read_cmaf_init_segment(&init_segment_bytes_of(&[
            track_with_only_a_picture_parameter_set,
        ]))
        .expect_err("a PPS without the SPS it refers to configures no decoder");

        assert!(
            refusal.to_string().contains("no sequence parameter set"),
            "the refusal names what is missing: {refusal}"
        );
        assert!(
            !refusal.to_string().contains("no picture parameter set"),
            "the PPS it did carry is not missing: {refusal}"
        );
    }

    #[test]
    fn an_h265_track_carrying_an_sps_and_a_pps_but_no_video_parameter_set_is_refused_by_name() {
        let track_without_a_video_parameter_set = a_track_described_by(
            2,
            CmafTrackSampleEntry::Video(Codec::Hvc1(Hvc1 {
                hvcc: an_hvcc_carrying(vec![
                    an_hvcc_array(
                        H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET,
                        &AN_H265_SEQUENCE_PARAMETER_SET,
                    ),
                    an_hvcc_array(
                        H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET,
                        &AN_H265_PICTURE_PARAMETER_SET,
                    ),
                ]),
                ..Default::default()
            })),
        );

        let refusal = read_cmaf_init_segment(&init_segment_bytes_of(&[
            track_without_a_video_parameter_set,
        ]))
        .expect_err("H.265 needs a VPS beside the SPS and PPS");

        assert!(
            refusal.to_string().contains("no video parameter set"),
            "the refusal names what is missing: {refusal}"
        );
    }

    #[test]
    fn two_traks_stating_the_same_track_id_are_refused_naming_the_id_they_share() {
        let both_called_track_one = init_segment_bytes_of(&[an_h264_track(1), an_opus_track(1)]);

        let refusal = read_cmaf_init_segment(&both_called_track_one)
            .expect_err("a subscriber asks for `{track_id}.m4s` and would get one of the two");

        assert!(
            refusal.to_string().contains("track_id 1"),
            "the refusal names the shared id: {refusal}"
        );
    }

    #[test]
    fn a_trak_stating_track_id_zero_is_refused_by_name() {
        let refusal = read_cmaf_init_segment(&init_segment_bytes_of(&[an_h264_track(0)]))
            .expect_err("ISO/IEC 14496-12 §8.3.2.3 reserves track id 0");

        assert!(refusal.to_string().contains("track_id 0"), "{refusal}");
    }

    #[test]
    fn a_track_whose_mdhd_states_a_timescale_of_zero_is_refused_by_name() {
        let track_on_no_clock_at_all = an_h264_track_on_timescale(1, 0);

        let refusal = read_cmaf_init_segment(&init_segment_bytes_of(&[track_on_no_clock_at_all]))
            .expect_err("no decode time can be read off a zero timescale");

        assert!(
            refusal.to_string().contains("track 1")
                && refusal.to_string().contains("timescale of 0"),
            "{refusal}"
        );
    }

    #[test]
    fn a_track_described_by_a_codec_this_subscriber_does_not_read_is_refused_by_name() {
        let a_vp8_track =
            a_track_described_by(6, CmafTrackSampleEntry::Video(Codec::Vp08(Vp08::default())));

        let refusal = read_cmaf_init_segment(&init_segment_bytes_of(&[a_vp8_track]))
            .expect_err("this subscriber reads H.264, H.265 and Opus");

        assert!(
            refusal.to_string().contains("track 6") && refusal.to_string().contains("vp08"),
            "the refusal names the track and the entry: {refusal}"
        );
        assert!(refusal.to_string().contains("avc1"), "{refusal}");
    }

    #[test]
    fn a_track_described_by_a_four_character_code_no_reader_recognises_is_refused_naming_it() {
        let init_segment_bytes = with_the_avc1_sample_entry_kind_replaced(
            &init_segment_bytes_of(&[an_h264_track(6)]),
            b"zzzz",
        );

        let refusal = read_cmaf_init_segment(&init_segment_bytes)
            .expect_err("an unrecognised sample entry configures no decoder");

        assert!(
            refusal.to_string().contains("track 6") && refusal.to_string().contains("zzzz"),
            "the refusal names the code it could not read: {refusal}"
        );
    }

    #[test]
    fn an_object_whose_moov_describes_no_tracks_is_refused_by_name() {
        let described_one_track = init_segment_bytes_of(&[an_h264_track(1)]);
        let mut unread: &[u8] = &described_one_track;
        let ftyp = Ftyp::decode(&mut unread).expect("the object opens with an ftyp");
        let mut moov = Moov::decode(&mut unread).expect("the ftyp is followed by a moov");
        moov.trak.clear();
        let mut init_segment_bytes: Vec<u8> = Vec::new();
        ftyp.encode(&mut init_segment_bytes)
            .expect("an ftyp that decoded encodes");
        moov.encode(&mut init_segment_bytes)
            .expect("a moov that decoded encodes");

        let refusal =
            read_cmaf_init_segment(&init_segment_bytes).expect_err("no tracks configures nothing");

        assert!(refusal.to_string().contains("no tracks"), "{refusal}");
    }
}
