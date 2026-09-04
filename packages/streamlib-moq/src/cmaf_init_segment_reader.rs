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

use mp4_atom::{Atom, Codec, Decode, Encode, Ftyp, Header, HvcCArray, Hvcc, Moov, Trak, Visual};

use crate::encoded_media_sample::TrackMedium;
use crate::error::{MoqExtensionError, Result};

/// What a refusal on this path calls the container it was reading.
const CMAF_CONTAINER_NAME: &str = "cmaf";

/// The bag's `codec` spelling for a track an `avc1` sample entry describes.
const H264_WIRE_CODEC: &str = "h264";

/// The bag's `codec` spelling for a track an `hvc1` or `hev1` sample entry
/// describes.
const H265_WIRE_CODEC: &str = "h265";

/// ITU-T H.265 §7.4.2.2 `nal_unit_type` for a video parameter set.
const H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET: u8 = 32;
/// ITU-T H.265 §7.4.2.2 `nal_unit_type` for a sequence parameter set.
const H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET: u8 = 33;
/// ITU-T H.265 §7.4.2.2 `nal_unit_type` for a picture parameter set.
const H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET: u8 = 34;

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
    /// Video only: the parameter sets a bag's bitstream must carry inline at every sync point.
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
    moov.trak
        .iter()
        .map(describe_the_track_a_moov_entry_carries)
        .collect()
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
    // A zero size field means "to the end of the enclosing container", which
    // for a self-delimiting MoQ object is the end of the object.
    let moov_body_byte_count = moov_header.size.unwrap_or(unread_init_segment_bytes.len());
    if moov_body_byte_count > unread_init_segment_bytes.len() {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the object declares a {moov_body_byte_count} byte moov body but carries only {} bytes after the moov header",
            unread_init_segment_bytes.len()
        )));
    }
    let mut moov_body_bytes = &unread_init_segment_bytes[..moov_body_byte_count];
    let moov = Moov::decode_body(&mut moov_body_bytes).map_err(|failure| {
        refuse_as_malformed_cmaf_init_segment(format!("the moov atom does not parse: {failure}"))
    })?;

    let byte_count_after_the_moov = unread_init_segment_bytes.len() - moov_body_byte_count;
    if byte_count_after_the_moov != 0 {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "the object carries {byte_count_after_the_moov} bytes after the moov, but an init segment is an ftyp and a moov and nothing else"
        )));
    }
    Ok(moov)
}

fn describe_the_track_a_moov_entry_carries(
    trak: &Trak,
) -> Result<CmafTrackDescriptionFromTheInitSegment> {
    let track_id = trak.tkhd.track_id;
    let media_timescale_hz = trak.mdia.mdhd.timescale;
    let sample_entry = the_one_sample_entry_of_the_track(trak, track_id)?;

    match sample_entry {
        Codec::Avc1(avc1) => {
            let mut parameter_set_nal_units = avc1.avcc.sequence_parameter_sets.clone();
            parameter_set_nal_units.extend(avc1.avcc.picture_parameter_sets.iter().cloned());
            describe_a_video_track(
                track_id,
                media_timescale_hz,
                H264_WIRE_CODEC,
                parameter_set_nal_units,
                &avc1.visual,
            )
        }
        Codec::Hvc1(hvc1) => describe_a_video_track(
            track_id,
            media_timescale_hz,
            H265_WIRE_CODEC,
            h265_parameter_set_nal_units_in_decoder_order(&hvc1.hvcc),
            &hvc1.visual,
        ),
        Codec::Hev1(hev1) => describe_a_video_track(
            track_id,
            media_timescale_hz,
            H265_WIRE_CODEC,
            h265_parameter_set_nal_units_in_decoder_order(&hev1.hvcc),
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
        unreadable_sample_entry => Err(refuse_as_malformed_cmaf_init_segment(format!(
            "track {track_id} is described by a `{}` sample entry, which this subscriber does not \
             read: it reads `avc1` for H.264, `hvc1` and `hev1` for H.265, and `Opus`",
            four_character_code_of_sample_entry(unreadable_sample_entry)
        ))),
    }
}

fn describe_a_video_track(
    track_id: u32,
    media_timescale_hz: u32,
    wire_codec: &str,
    parameter_set_nal_units: Vec<Vec<u8>>,
    visual: &Visual,
) -> Result<CmafTrackDescriptionFromTheInitSegment> {
    if parameter_set_nal_units.is_empty() {
        return Err(refuse_as_malformed_cmaf_init_segment(format!(
            "track {track_id} is a {wire_codec} track whose sample entry carries no parameter \
             sets, and `avc1`/`hvc1` forbid in-band sets — so nothing on this track can ever be \
             decoded"
        )));
    }
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

/// Every NAL unit `hvcC` carries, ordered VPS then SPS then PPS: a decoder
/// handed a PPS before the SPS it refers to discards it, and these are
/// prepended to a bag's bitstream in the order they come back.
fn h265_parameter_set_nal_units_in_decoder_order(hvcc: &Hvcc) -> Vec<Vec<u8>> {
    let mut arrays_in_decoder_order: Vec<&HvcCArray> = hvcc.arrays.iter().collect();
    arrays_in_decoder_order
        .sort_by_key(|array| decoder_order_rank_of_h265_nal_unit_type(array.nal_unit_type));
    arrays_in_decoder_order
        .into_iter()
        .flat_map(|array| array.nalus.iter().cloned())
        .collect()
}

fn decoder_order_rank_of_h265_nal_unit_type(nal_unit_type: u8) -> u8 {
    match nal_unit_type {
        H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET => 0,
        H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET => 1,
        H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET => 2,
        _ => 3,
    }
}

/// The code a sample entry writes itself as. `mp4_atom::Codec` exposes its
/// variant's `FourCC` no other way, so an entry is re-encoded to be named.
fn four_character_code_of_sample_entry(sample_entry: &Codec) -> String {
    if let Codec::Unknown(four_character_code) = sample_entry {
        return four_character_code.to_string();
    }
    let mut encoded_sample_entry: Vec<u8> = Vec::new();
    match sample_entry.encode(&mut encoded_sample_entry) {
        Ok(()) if encoded_sample_entry.len() >= BOX_HEADER_BYTES => {
            String::from_utf8_lossy(&encoded_sample_entry[BOX_KIND_BYTE_RANGE]).into_owned()
        }
        _ => "unnamed".to_owned(),
    }
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
    use crate::annex_b_access_unit::ParameterSetsFromAnnexBAccessUnit;
    use crate::cmaf_init_segment::{
        CmafTrackDescriptionForTheInitSegment, build_cmaf_init_segment,
    };
    use crate::cmaf_sample_entry::{
        CmafTrackSampleEntry, build_opus_sample_entry, build_video_sample_entry,
    };
    use crate::cmaf_track_timeline::{OPUS_TRACK_TIMESCALE_HZ, VIDEO_TRACK_TIMESCALE_HZ};
    use mp4_atom::{Avc1, Avcc, Hvc1, Vp08};

    /// What this tree's Opus encoder reports as its own lookahead.
    const THE_ENCODERS_PRE_SKIP: u32 = 312;

    const A_SEQUENCE_PARAMETER_SET: [u8; 8] = [0x67, 0x42, 0xC0, 0x1F, 0xDA, 0x02, 0xD0, 0x49];
    const A_PICTURE_PARAMETER_SET: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];

    fn h264_parameter_sets() -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: Vec::new(),
            sequence_parameter_set_nal_units: vec![A_SEQUENCE_PARAMETER_SET.to_vec()],
            picture_parameter_set_nal_units: vec![A_PICTURE_PARAMETER_SET.to_vec()],
        }
    }

    fn an_h264_track(track_id: u32) -> CmafTrackDescriptionForTheInitSegment {
        let entry = build_video_sample_entry("h264", &h264_parameter_sets(), 320, 180)
            .expect("the fixture parameter sets describe an H.264 track");
        CmafTrackDescriptionForTheInitSegment {
            track_id,
            inbound_link_name: "encoder/encoded_video".to_owned(),
            cmaf_track_sample_entry: entry.cmaf_track_sample_entry,
            media_timescale_hz: VIDEO_TRACK_TIMESCALE_HZ,
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
        let video_parameter_set = vec![0x40, 0x01, 0x0C, 0x01];
        let sequence_parameter_set = vec![0x42, 0x01, 0x01, 0x60];
        let picture_parameter_set = vec![0x44, 0x01, 0xC1, 0x72];
        let mut hvcc = Hvcc::new();
        // Listed picture-set-first, which is a legal `hvcC`: the arrays are a
        // set, and the order a decoder needs is not the order a writer used.
        hvcc.arrays = vec![
            HvcCArray {
                completeness: true,
                nal_unit_type: H265_NAL_UNIT_TYPE_PICTURE_PARAMETER_SET,
                nalus: vec![picture_parameter_set.clone()],
            },
            HvcCArray {
                completeness: true,
                nal_unit_type: H265_NAL_UNIT_TYPE_VIDEO_PARAMETER_SET,
                nalus: vec![video_parameter_set.clone()],
            },
            HvcCArray {
                completeness: true,
                nal_unit_type: H265_NAL_UNIT_TYPE_SEQUENCE_PARAMETER_SET,
                nalus: vec![sequence_parameter_set.clone()],
            },
        ];
        let scrambled_h265_track = a_track_described_by(
            1,
            CmafTrackSampleEntry::Video(Codec::Hvc1(Hvc1 {
                hvcc,
                ..Default::default()
            })),
        );

        let tracks = read_cmaf_init_segment(&init_segment_bytes_of(&[scrambled_h265_track]))
            .expect("an H.265 init segment reads back");

        assert_eq!(
            tracks[0].parameter_set_nal_units,
            vec![
                video_parameter_set,
                sequence_parameter_set,
                picture_parameter_set
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
    fn a_video_track_carrying_no_parameter_sets_at_all_is_refused_by_name() {
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
                && refusal.to_string().contains("no parameter sets"),
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
