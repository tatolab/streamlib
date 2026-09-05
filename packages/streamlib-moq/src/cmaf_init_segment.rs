// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The init object: `ftyp` immediately followed by `moov`, and nothing else.
//!
//! What a subscriber fetches first and configures every decoder from. The
//! reference subscriber asserts this shape literally — it reads two atoms and
//! checks bytes 4..8 of each — so a `styp`, a `free` or a `sidx` between them
//! breaks playback rather than being skipped.
//!
//! Written once and never revised: ISO/IEC 14496-12 §6.1.2 puts the sample
//! entries in the one `moov`, and no box under a `moof` carries one. A
//! publisher therefore cannot describe a track until its first sync point has
//! arrived, and cannot re-describe it afterwards at all.

use mp4_atom::{
    Dinf, Dref, Encode, FixedPoint, Ftyp, Hdlr, Mdhd, Mdia, Minf, Moov, Mvex, Mvhd, Smhd, Stbl,
    Stco, Stsd, Tkhd, Trak, Trex, Url, Vmhd,
};

use crate::cmaf_fragment::SAMPLE_FLAGS_OF_A_NON_SYNC_POINT;
use crate::cmaf_sample_entry::CmafTrackSampleEntry;
use crate::cmaf_track_timeline::VIDEO_TRACK_TIMESCALE_HZ;
use crate::encoded_media_sample::TrackMedium;
use crate::error::{MoqExtensionError, Result};
use crate::moq_broadcast_catalog::CMAF_PACKAGING;

/// The brands a CMAF init segment declares — the same set, in the same order,
/// as the engine's own recorder.
///
/// `iso6` is the base ISOBMFF version fragmented boxes need, and it is the
/// major brand every MoQ-ecosystem player has actually been exercised against
/// on a muxed `moov`: ffmpeg's CMAF muxer writes `iso6` major with `cmfc`
/// compatible whatever the track count, and `moq-pub` forwards ffmpeg's init
/// verbatim. `cmfc` stays in the compatible list, where it is the media-profile
/// assertion this file makes.
const MAJOR_BRAND: &[u8; 4] = b"iso6";
const COMPATIBLE_BRANDS: [&[u8; 4]; 3] = [b"iso6", b"mp41", b"cmfc"];

/// One track, as the init segment describes it.
pub(crate) struct CmafTrackDescriptionForTheInitSegment {
    /// The `tkhd.track_id`, which is also what names the track's MoQ media
    /// track — a subscriber reaches `{track_id}.m4s` from the moov alone.
    pub(crate) track_id: u32,
    /// The inbound link this track carries, written into `hdlr.name` so a
    /// recording of the broadcast says where each track came from.
    pub(crate) inbound_link_name: String,
    pub(crate) cmaf_track_sample_entry: CmafTrackSampleEntry,
    pub(crate) media_timescale_hz: u32,
    /// The coded extent, for a video track. An audio track states none.
    pub(crate) coded_extent: Option<(u32, u32)>,
}

/// Build the init object's bytes: `ftyp` then `moov`, concatenated.
pub(crate) fn build_cmaf_init_segment(
    tracks: &[CmafTrackDescriptionForTheInitSegment],
) -> Result<bytes::Bytes> {
    if tracks.is_empty() {
        return Err(MoqExtensionError::Refused {
            what: "an init segment describing no tracks configures no decoder; a broadcast \
                   needs at least one track"
                .to_owned(),
        });
    }

    let mut moov = Moov {
        mvhd: Mvhd {
            // ISO/IEC 14496-12 §8.2.2 fixes 1.0 as normal playback and full
            // volume. The derived `Default` is 0 for both, which misdescribes
            // the movie and makes some players render it silently or not at
            // all.
            rate: FixedPoint::new(1, 0),
            volume: FixedPoint::new(1, 0),
            timescale: VIDEO_TRACK_TIMESCALE_HZ,
            // A live broadcast has no duration, and `mehd` is optional
            // precisely so one need not be stated.
            duration: 0,
            // §8.2.2.3 wants a value larger than every track id in use, which
            // is the largest id plus one and not the track count — a caller
            // names its tracks and need not name them `1..n`.
            next_track_id: tracks
                .iter()
                .map(|track| track.track_id.saturating_add(1))
                .max()
                .expect("a segment describing no tracks is refused above"),
            ..Default::default()
        },
        mvex: Some(Mvex {
            mehd: None,
            trex: Vec::with_capacity(tracks.len()),
        }),
        ..Default::default()
    };

    for track in tracks {
        let medium = track.cmaf_track_sample_entry.track_medium();
        let (coded_width, coded_height) = track.coded_extent.unwrap_or((0, 0));
        moov.trak.push(Trak {
            tkhd: Tkhd {
                track_id: track.track_id,
                // Both are `false` in the derived `Default`, which describes a
                // track no player will select.
                enabled: true,
                in_movie: true,
                duration: 0,
                // §8.3.2.3: a visual track states its presentation size and
                // carries no volume; an audio track is the other way round.
                width: FixedPoint::new(coded_width as u16, 0),
                height: FixedPoint::new(coded_height as u16, 0),
                volume: FixedPoint::new(u8::from(medium == TrackMedium::Audio), 0),
                ..Default::default()
            },
            mdia: Mdia {
                mdhd: Mdhd {
                    timescale: track.media_timescale_hz,
                    duration: 0,
                    // An empty language packs into garbage; ISO 639-2/T's
                    // undetermined code is what a stream that states no
                    // language says.
                    language: "und".to_owned(),
                    ..Default::default()
                },
                hdlr: Hdlr {
                    handler: handler_of(medium).into(),
                    name: track.inbound_link_name.clone(),
                },
                minf: Minf {
                    vmhd: (medium == TrackMedium::Video).then(Vmhd::default),
                    smhd: (medium == TrackMedium::Audio).then(Smhd::default),
                    dinf: Dinf {
                        // The derived `Default` is a `dref` with no entries at
                        // all, which is invalid. One `Url` whose location is
                        // empty is what sets the self-contained flag — the
                        // emptiness is the declaration.
                        dref: Dref {
                            urls: vec![Url {
                                location: String::new(),
                            }],
                        },
                    },
                    stbl: Stbl {
                        stsd: Stsd {
                            codecs: vec![
                                track
                                    .cmaf_track_sample_entry
                                    .clone()
                                    .into_stsd_sample_entry(),
                            ],
                        },
                        // A fragmented movie has no chunks in its `moov`, so
                        // this table is empty — but it must be present.
                        // ISO/IEC 14496-12 §8.7.5 makes a chunk offset box
                        // mandatory in every `stbl`, and the reference MoQ
                        // subscriber enforces it literally: without one it
                        // refuses the whole init segment with "stco and co64
                        // not found" and plays nothing.
                        stco: Some(Stco {
                            entries: Vec::new(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
            ..Default::default()
        });
        if let Some(mvex) = moov.mvex.as_mut() {
            mvex.trex.push(Trex {
                track_id: track.track_id,
                default_sample_description_index: 1,
                default_sample_duration: 0,
                default_sample_size: 0,
                default_sample_flags: SAMPLE_FLAGS_OF_A_NON_SYNC_POINT,
            });
        }
    }

    let mut init_segment_bytes: Vec<u8> = Vec::with_capacity(1024);
    Ftyp {
        major_brand: (*MAJOR_BRAND).into(),
        minor_version: 0,
        compatible_brands: COMPATIBLE_BRANDS
            .into_iter()
            .map(|brand| (*brand).into())
            .collect(),
    }
    .encode(&mut init_segment_bytes)
    .map_err(|failure| MoqExtensionError::MalformedObject {
        container: CMAF_PACKAGING,
        what: format!("the init segment's ftyp could not be written: {failure}"),
    })?;
    moov.encode(&mut init_segment_bytes)
        .map_err(|failure| MoqExtensionError::MalformedObject {
            container: CMAF_PACKAGING,
            what: format!("the init segment's moov could not be written: {failure}"),
        })?;

    Ok(bytes::Bytes::from(init_segment_bytes))
}

/// The ISO/IEC 14496-12 §8.4.3 handler a track's media type is declared by.
fn handler_of(medium: TrackMedium) -> [u8; 4] {
    match medium {
        TrackMedium::Video => *b"vide",
        TrackMedium::Audio => *b"soun",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annex_b_access_unit::ParameterSetsFromAnnexBAccessUnit;
    use crate::cmaf_sample_entry::{build_opus_sample_entry, build_video_sample_entry};
    use crate::cmaf_track_timeline::OPUS_TRACK_TIMESCALE_HZ;
    use mp4_atom::{Any, Decode};

    /// A real H.264 SPS and PPS: profile 0x42 (baseline), level 0x1f.
    fn h264_parameter_sets() -> ParameterSetsFromAnnexBAccessUnit {
        ParameterSetsFromAnnexBAccessUnit {
            video_parameter_set_nal_units: Vec::new(),
            sequence_parameter_set_nal_units: vec![vec![
                0x67, 0x42, 0xC0, 0x1F, 0xDA, 0x02, 0xD0, 0x49,
            ]],
            picture_parameter_set_nal_units: vec![vec![0x68, 0xCE, 0x3C, 0x80]],
        }
    }

    fn a_video_track(track_id: u32) -> CmafTrackDescriptionForTheInitSegment {
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

    fn an_audio_track(track_id: u32) -> CmafTrackDescriptionForTheInitSegment {
        let entry =
            build_opus_sample_entry(2, OPUS_TRACK_TIMESCALE_HZ, 312).expect("stereo Opus fits");
        CmafTrackDescriptionForTheInitSegment {
            track_id,
            inbound_link_name: "encoder/encoded_audio".to_owned(),
            cmaf_track_sample_entry: entry.cmaf_track_sample_entry,
            media_timescale_hz: OPUS_TRACK_TIMESCALE_HZ,
            coded_extent: None,
        }
    }

    fn decoded_atoms(init_segment: &[u8]) -> Vec<Any> {
        let mut unread = init_segment;
        let mut atoms = Vec::new();
        while !unread.is_empty() {
            atoms.push(Any::decode(&mut unread).expect("the init segment is a run of atoms"));
        }
        atoms
    }

    #[test]
    fn the_init_segment_is_an_ftyp_immediately_followed_by_a_moov() {
        // The reference subscriber reads exactly two atoms and checks bytes
        // 4..8 of each, so anything between them is not skipped — it breaks.
        let bytes = build_cmaf_init_segment(&[a_video_track(1)]).unwrap();

        assert_eq!(&bytes[4..8], b"ftyp");
        let atoms = decoded_atoms(&bytes);
        assert_eq!(atoms.len(), 2);
        assert!(matches!(atoms[0], Any::Ftyp(_)));
        assert!(matches!(atoms[1], Any::Moov(_)));
    }

    /// The brands are what a third-party reader gates on before it looks at a
    /// single box, so they are pinned as bytes rather than as a membership
    /// check — the same shape the engine recorder's `ftyp` test has.
    #[test]
    fn the_init_segment_declares_the_brands_a_multi_track_cmaf_movie_is_read_by() {
        let bytes =
            build_cmaf_init_segment(&[a_video_track(1), an_audio_track(2)]).expect("two tracks");

        let Any::Ftyp(ftyp) = &decoded_atoms(&bytes)[0] else {
            panic!("the init segment opens with ftyp");
        };
        assert_eq!(ftyp.major_brand, b"iso6".into());
        assert_eq!(
            ftyp.compatible_brands,
            vec![b"iso6".into(), b"mp41".into(), b"cmfc".into()],
            "the engine recorder's list, byte for byte, so one bitstream is described one way"
        );
    }

    #[test]
    fn the_movie_header_states_normal_rate_and_full_volume() {
        // mp4-atom's derived Default is 0 for both, which makes some players
        // render the movie silently or hide it.
        let bytes = build_cmaf_init_segment(&[a_video_track(1)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert_eq!(moov.mvhd.rate, FixedPoint::new(1, 0));
        assert_eq!(moov.mvhd.volume, FixedPoint::new(1, 0));
    }

    #[test]
    fn every_track_is_enabled_and_in_the_movie() {
        let bytes = build_cmaf_init_segment(&[a_video_track(1), an_audio_track(2)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert_eq!(moov.trak.len(), 2);
        for trak in &moov.trak {
            assert!(trak.tkhd.enabled, "a disabled track is selected by nobody");
            assert!(trak.tkhd.in_movie);
        }
    }

    #[test]
    fn the_data_reference_declares_the_media_is_in_this_very_file() {
        // An empty `dref` is invalid, and the empty URL location is precisely
        // what sets the self-contained flag.
        let bytes = build_cmaf_init_segment(&[a_video_track(1)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        let urls = &moov.trak[0].mdia.minf.dinf.dref.urls;
        assert_eq!(urls.len(), 1);
        assert!(urls[0].location.is_empty());
    }

    #[test]
    fn a_track_states_a_language_rather_than_an_empty_string() {
        // An empty language packs into garbage bits.
        let bytes = build_cmaf_init_segment(&[a_video_track(1)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert_eq!(moov.trak[0].mdia.mdhd.language, "und");
    }

    #[test]
    fn a_video_track_carries_a_video_media_header_and_an_audio_track_a_sound_one() {
        let bytes = build_cmaf_init_segment(&[a_video_track(1), an_audio_track(2)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert!(moov.trak[0].mdia.minf.vmhd.is_some());
        assert!(moov.trak[0].mdia.minf.smhd.is_none());
        assert!(moov.trak[1].mdia.minf.smhd.is_some());
        assert!(moov.trak[1].mdia.minf.vmhd.is_none());
    }

    #[test]
    fn each_track_keeps_its_own_timescale() {
        // Video is placed on nanoseconds so a monotonic stamp needs no
        // rescale; Opus is placed on its own 48 kHz clock so a sample count
        // is not a division.
        let bytes = build_cmaf_init_segment(&[a_video_track(1), an_audio_track(2)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert_eq!(moov.trak[0].mdia.mdhd.timescale, VIDEO_TRACK_TIMESCALE_HZ);
        assert_eq!(moov.trak[1].mdia.mdhd.timescale, OPUS_TRACK_TIMESCALE_HZ);
    }

    #[test]
    fn every_track_gets_a_trex_declaring_its_samples_are_not_sync_points() {
        // A fragment that omits the flag then describes a delta frame, which
        // is what makes a sync point mean something when one does state it.
        let bytes = build_cmaf_init_segment(&[a_video_track(1), an_audio_track(2)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        let mvex = moov
            .mvex
            .as_ref()
            .expect("a fragmented movie declares mvex");
        assert_eq!(mvex.trex.len(), 2);
        for trex in &mvex.trex {
            assert_eq!(trex.default_sample_flags, SAMPLE_FLAGS_OF_A_NON_SYNC_POINT);
            assert_eq!(trex.default_sample_description_index, 1);
        }
    }

    #[test]
    fn the_track_ids_a_caller_gave_are_the_ids_a_subscriber_reads_back() {
        // A subscriber with no catalog names media tracks `{track_id}.m4s`
        // straight off the moov, so these ids are the track names.
        let bytes = build_cmaf_init_segment(&[a_video_track(7), an_audio_track(9)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert_eq!(moov.trak[0].tkhd.track_id, 7);
        assert_eq!(moov.trak[1].tkhd.track_id, 9);
        assert_eq!(
            moov.mvhd.next_track_id, 10,
            "§8.2.2.3 wants a value larger than every id in use; the track count would name 3, \
             which is an id this movie already carries"
        );
    }

    #[test]
    fn a_video_track_states_its_coded_extent_and_an_audio_track_states_volume() {
        let bytes = build_cmaf_init_segment(&[a_video_track(1), an_audio_track(2)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert_eq!(moov.trak[0].tkhd.width, FixedPoint::new(320, 0));
        assert_eq!(moov.trak[0].tkhd.height, FixedPoint::new(180, 0));
        assert_eq!(moov.trak[0].tkhd.volume, FixedPoint::new(0, 0));
        assert_eq!(moov.trak[1].tkhd.volume, FixedPoint::new(1, 0));
    }

    #[test]
    fn a_tracks_handler_name_is_the_link_it_carries() {
        let bytes = build_cmaf_init_segment(&[a_video_track(1)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        assert_eq!(moov.trak[0].mdia.hdlr.name, "encoder/encoded_video");
    }

    #[test]
    fn every_track_carries_a_chunk_offset_table_even_though_it_is_empty() {
        // ISO/IEC 14496-12 §8.7.5 makes a chunk offset box mandatory in every
        // `stbl`, and the reference MoQ subscriber enforces it: an init
        // segment without one is refused outright with "stco and co64 not
        // found", so the whole broadcast plays nowhere.
        let bytes = build_cmaf_init_segment(&[a_video_track(1), an_audio_track(2)]).unwrap();

        let Any::Moov(moov) = &decoded_atoms(&bytes)[1] else {
            panic!("the second atom is the moov");
        };
        for trak in &moov.trak {
            let stco = trak
                .mdia
                .minf
                .stbl
                .stco
                .as_ref()
                .expect("a stbl states a chunk offset table");
            assert!(
                stco.entries.is_empty(),
                "a fragmented movie has no chunks in its moov"
            );
        }
    }

    #[test]
    fn an_init_segment_describing_no_tracks_is_refused_by_name() {
        let refusal = build_cmaf_init_segment(&[]).expect_err("no tracks configures no decoder");

        assert!(refusal.to_string().contains("no tracks"), "{refusal}");
    }
}
