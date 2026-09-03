// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask mp4-inspect <file>` — what a written recording actually
//! contains, as JSON.
//!
//! Pure Rust over `mp4-atom`, so nothing downstream needs ffprobe: the rig
//! tests, the Python surface's tests and `/verify-video` all read a real file
//! through this.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use mp4_atom::{Any, Codec, DecodeMaybe, Moof, Moov};
use serde_json::{Value, json};

#[derive(Args, Debug)]
pub struct Mp4InspectCommand {
    /// The recording to read.
    pub file: PathBuf,
}

pub fn run(command: Mp4InspectCommand) -> Result<()> {
    let report = inspect_file(&command.file)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Read one file into the report `mp4-inspect` prints.
pub fn inspect_file(path: &Path) -> Result<Value> {
    let file_bytes = std::fs::read(path)
        .with_context(|| format!("{} could not be read", path.display()))?;
    inspect_bytes(&file_bytes)
}

/// The same read over bytes, which is what the tests below drive.
pub fn inspect_bytes(file_bytes: &[u8]) -> Result<Value> {
    let mut cursor = std::io::Cursor::new(file_bytes);
    let mut brands = Value::Null;
    let mut moov: Option<Moov> = None;
    let mut fragments: Vec<Moof> = Vec::new();

    loop {
        match Any::decode_maybe(&mut cursor) {
            Ok(Some(Any::Ftyp(ftyp))) => {
                brands = json!({
                    "major_brand": fourcc_string(&ftyp.major_brand),
                    "compatible_brands": ftyp
                        .compatible_brands
                        .iter()
                        .map(fourcc_string)
                        .collect::<Vec<_>>(),
                });
            }
            Ok(Some(Any::Moov(parsed))) => moov = Some(parsed),
            Ok(Some(Any::Moof(parsed))) => fragments.push(parsed),
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(failure) => bail!("the file did not parse as ISOBMFF: {failure}"),
        }
    }

    let Some(moov) = moov else {
        bail!("the file carries no `moov`, so it describes no track — a recording whose header never landed");
    };

    let tracks: Vec<Value> = moov
        .trak
        .iter()
        .map(|trak| {
            let track_id = trak.tkhd.track_id;
            let samples: u64 = fragments
                .iter()
                .flat_map(|moof| moof.traf.iter())
                .filter(|traf| traf.tfhd.track_id == track_id)
                .flat_map(|traf| traf.trun.iter())
                .map(|trun| trun.entries.len() as u64)
                .sum();
            let duration_in_timescale: u64 = fragments
                .iter()
                .flat_map(|moof| moof.traf.iter())
                .filter(|traf| traf.tfhd.track_id == track_id)
                .flat_map(|traf| traf.trun.iter())
                .flat_map(|trun| trun.entries.iter())
                .filter_map(|entry| entry.duration)
                .map(u64::from)
                .sum();
            let timescale = trak.mdia.mdhd.timescale;
            json!({
                "track_id": track_id,
                // The track's name is the inbound link it recorded, which is
                // what makes a recording self-describing.
                "name": trak.mdia.hdlr.name,
                "handler": fourcc_string(&trak.mdia.hdlr.handler),
                "timescale": timescale,
                "sample_entry": trak
                    .mdia
                    .minf
                    .stbl
                    .stsd
                    .codecs
                    .first()
                    .map(describe_sample_entry)
                    .unwrap_or(Value::Null),
                "samples": samples,
                "duration_in_timescale": duration_in_timescale,
                "duration_seconds": if timescale == 0 {
                    Value::Null
                } else {
                    json!(duration_in_timescale as f64 / timescale as f64)
                },
            })
        })
        .collect();

    let inspected_fragments: Vec<Value> = fragments
        .iter()
        .map(|moof| {
            json!({
                "sequence_number": moof.mfhd.sequence_number,
                "tracks": moof
                    .traf
                    .iter()
                    .map(|traf| json!({
                        "track_id": traf.tfhd.track_id,
                        "base_media_decode_time": traf
                            .tfdt
                            .as_ref()
                            .map(|tfdt| tfdt.base_media_decode_time),
                        "samples": traf.trun.iter().map(|trun| trun.entries.len()).sum::<usize>(),
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(json!({
        "brands": brands,
        "tracks": tracks,
        "fragments": inspected_fragments,
        "fragment_count": fragments.len(),
    }))
}

fn describe_sample_entry(codec: &Codec) -> Value {
    match codec {
        Codec::Avc1(avc1) => json!({
            "kind": "avc1",
            "width": avc1.visual.width,
            "height": avc1.visual.height,
            "profile_indication": avc1.avcc.avc_profile_indication,
            "profile_compatibility": avc1.avcc.profile_compatibility,
            "level_indication": avc1.avcc.avc_level_indication,
            "length_size": avc1.avcc.length_size,
            "sequence_parameter_sets": avc1.avcc.sequence_parameter_sets.len(),
            "picture_parameter_sets": avc1.avcc.picture_parameter_sets.len(),
        }),
        Codec::Hvc1(hvc1) => json!({
            "kind": "hvc1",
            "width": hvc1.visual.width,
            "height": hvc1.visual.height,
            "general_profile_idc": hvc1.hvcc.general_profile_idc,
            "general_level_idc": hvc1.hvcc.general_level_idc,
            "general_tier_flag": hvc1.hvcc.general_tier_flag,
            "chroma_format_idc": hvc1.hvcc.chroma_format_idc,
            "bit_depth_luma_minus8": hvc1.hvcc.bit_depth_luma_minus8,
            "bit_depth_chroma_minus8": hvc1.hvcc.bit_depth_chroma_minus8,
            "length_size_minus_one": hvc1.hvcc.length_size_minus_one,
            "parameter_set_arrays": hvc1.hvcc.arrays.len(),
        }),
        Codec::Opus(opus) => json!({
            "kind": "Opus",
            "channel_count": opus.audio.channel_count,
            "output_channel_count": opus.dops.output_channel_count,
            "pre_skip": opus.dops.pre_skip,
            "input_sample_rate": opus.dops.input_sample_rate,
            "output_gain": opus.dops.output_gain,
        }),
        // A sample entry this sink never writes; name the variant and move on
        // rather than pretending to describe fields that are not there.
        other => {
            let debug_rendering = format!("{other:?}");
            json!({
                "kind": debug_rendering
                    .split('(')
                    .next()
                    .unwrap_or("unknown")
                    .to_string(),
            })
        }
    }
}

fn fourcc_string(fourcc: &mp4_atom::FourCC) -> String {
    String::from_utf8_lossy(&<[u8; 4]>::from(*fourcc)).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest legal fragmented file: `ftyp`, a `moov` with one Opus
    /// track, and one `moof` + `mdat`. Built with `mp4-atom` so the fixture
    /// and the reader cannot drift apart.
    fn one_opus_track_file() -> Vec<u8> {
        use mp4_atom::{
            Audio, Dinf, Dops, Dref, Encode, FixedPoint, Ftyp, Hdlr, Mdat, Mdhd, Mdia, Mfhd,
            Minf, Moof, Moov, Mvex, Mvhd, Opus, Smhd, Stbl, Stsd, Tfdt, Tfhd, Tkhd, Traf, Trak,
            Trex, Trun, TrunEntry, Url,
        };

        let mut bytes = Vec::new();
        Ftyp {
            major_brand: b"iso6".into(),
            minor_version: 512,
            compatible_brands: vec![b"iso6".into(), b"cmfc".into()],
        }
        .encode(&mut bytes)
        .unwrap();

        Moov {
            mvhd: Mvhd {
                timescale: 1_000_000_000,
                next_track_id: 2,
                ..Default::default()
            },
            mvex: Some(Mvex {
                mehd: None,
                trex: vec![Trex {
                    track_id: 1,
                    default_sample_description_index: 1,
                    default_sample_duration: 0,
                    default_sample_size: 0,
                    default_sample_flags: 0x0101_0000,
                }],
            }),
            trak: vec![Trak {
                tkhd: Tkhd {
                    track_id: 1,
                    enabled: true,
                    in_movie: true,
                    ..Default::default()
                },
                mdia: Mdia {
                    mdhd: Mdhd {
                        timescale: 48_000,
                        language: "und".into(),
                        ..Default::default()
                    },
                    hdlr: Hdlr {
                        handler: b"soun".into(),
                        name: "microphone/audio".into(),
                    },
                    minf: Minf {
                        smhd: Some(Smhd::default()),
                        dinf: Dinf {
                            dref: Dref {
                                urls: vec![Url {
                                    location: String::new(),
                                }],
                            },
                        },
                        stbl: Stbl {
                            stsd: Stsd {
                                codecs: vec![Codec::Opus(Opus {
                                    audio: Audio {
                                        data_reference_index: 1,
                                        channel_count: 2,
                                        sample_size: 16,
                                        sample_rate: FixedPoint::new(48_000, 0),
                                    },
                                    dops: Dops {
                                        output_channel_count: 2,
                                        pre_skip: 312,
                                        input_sample_rate: 48_000,
                                        output_gain: 0,
                                    },
                                    btrt: None,
                                })],
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                },
                ..Default::default()
            }],
            ..Default::default()
        }
        .encode(&mut bytes)
        .unwrap();

        Moof {
            mfhd: Mfhd { sequence_number: 1 },
            traf: vec![Traf {
                tfhd: Tfhd {
                    track_id: 1,
                    default_base_is_moof: true,
                    ..Default::default()
                },
                tfdt: Some(Tfdt {
                    base_media_decode_time: 0,
                }),
                trun: vec![Trun {
                    data_offset: Some(0),
                    entries: vec![
                        TrunEntry {
                            duration: Some(960),
                            size: Some(4),
                            flags: Some(0x0200_0000),
                            cts: None,
                        },
                        TrunEntry {
                            duration: Some(960),
                            size: Some(4),
                            flags: Some(0x0200_0000),
                            cts: None,
                        },
                    ],
                }],
                ..Default::default()
            }],
        }
        .encode(&mut bytes)
        .unwrap();
        Mdat {
            data: vec![0xFC; 8],
        }
        .encode(&mut bytes)
        .unwrap();
        bytes
    }

    #[test]
    fn a_track_is_reported_under_the_link_name_it_recorded() {
        let report = inspect_bytes(&one_opus_track_file()).expect("the fixture parses");
        let track = &report["tracks"][0];
        assert_eq!(track["name"], "microphone/audio");
        assert_eq!(track["handler"], "soun");
        assert_eq!(track["track_id"], 1);
        assert_eq!(track["timescale"], 48_000);
    }

    #[test]
    fn an_opus_sample_entry_reports_its_dops_fields() {
        let report = inspect_bytes(&one_opus_track_file()).expect("parses");
        let entry = &report["tracks"][0]["sample_entry"];
        assert_eq!(entry["kind"], "Opus");
        assert_eq!(entry["output_channel_count"], 2);
        assert_eq!(entry["pre_skip"], 312);
        assert_eq!(entry["input_sample_rate"], 48_000);
    }

    #[test]
    fn durations_are_summed_per_track_and_converted_to_seconds() {
        let report = inspect_bytes(&one_opus_track_file()).expect("parses");
        let track = &report["tracks"][0];
        assert_eq!(track["samples"], 2);
        assert_eq!(track["duration_in_timescale"], 1920);
        assert_eq!(
            track["duration_seconds"].as_f64().expect("a number"),
            1920.0 / 48_000.0,
            "two 20 ms packets are 40 ms"
        );
    }

    #[test]
    fn fragments_are_reported_with_their_sequence_and_decode_time() {
        let report = inspect_bytes(&one_opus_track_file()).expect("parses");
        assert_eq!(report["fragment_count"], 1);
        let fragment = &report["fragments"][0];
        assert_eq!(fragment["sequence_number"], 1);
        assert_eq!(fragment["tracks"][0]["base_media_decode_time"], 0);
        assert_eq!(fragment["tracks"][0]["samples"], 2);
    }

    #[test]
    fn the_brands_the_file_opens_with_are_reported() {
        let report = inspect_bytes(&one_opus_track_file()).expect("parses");
        assert_eq!(report["brands"]["major_brand"], "iso6");
        assert_eq!(
            report["brands"]["compatible_brands"]
                .as_array()
                .expect("a list")
                .len(),
            2
        );
    }

    #[test]
    fn a_file_whose_header_never_landed_is_refused_by_name() {
        use mp4_atom::{Encode, Ftyp};
        let mut header_only = Vec::new();
        Ftyp {
            major_brand: b"iso6".into(),
            minor_version: 512,
            compatible_brands: vec![],
        }
        .encode(&mut header_only)
        .unwrap();

        let failure = inspect_bytes(&header_only).expect_err("no moov means no tracks");
        assert!(
            failure.to_string().contains("no `moov`"),
            "the refusal says what is missing: {failure}"
        );
    }

    #[test]
    fn a_recording_cut_off_mid_box_still_reports_what_landed() {
        let whole = one_opus_track_file();
        // Teardown is not a promise: a run killed mid-`mdat` must still
        // inspect, which is the whole reason the layout is fragmented. The
        // partial trailing box is ignored rather than failing the read.
        let report = inspect_bytes(&whole[..whole.len() - 3])
            .expect("the boxes that landed are still a readable recording");

        assert_eq!(report["tracks"][0]["name"], "microphone/audio");
        assert_eq!(
            report["fragment_count"], 1,
            "the closed fragment is reported even though the file stops mid-box"
        );
    }

    #[test]
    fn a_file_whose_first_box_is_not_isobmff_is_refused_by_name() {
        let failure = inspect_bytes(b"this is not an mp4 file at all, not even close")
            .expect_err("garbage is not a recording");
        assert!(
            failure.to_string().contains("did not parse")
                || failure.to_string().contains("no `moov`"),
            "the refusal says what went wrong: {failure}"
        );
    }
}
