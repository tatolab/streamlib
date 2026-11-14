// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Matroska and WebM files (RFC 9559 EBML elements): duration from the segment's `Info`, and
//! each `TrackEntry`'s picture size, frame duration, sample rate, channel count and codec.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetAudioFactsOnTheWire, AssetMediaFactsOnTheWire, AssetVideoFactsOnTheWire,
};

use crate::{AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable};

const EBML_HEADER_ELEMENT_ID: u64 = 0x1A45_DFA3;
const SEGMENT_ELEMENT_ID: u64 = 0x1853_8067;
const SEGMENT_INFO_ELEMENT_ID: u64 = 0x1549_A966;
const TIMESTAMP_SCALE_ELEMENT_ID: u64 = 0x2A_D7B1;
const DURATION_ELEMENT_ID: u64 = 0x4489;
const TRACKS_ELEMENT_ID: u64 = 0x1654_AE6B;
const TRACK_ENTRY_ELEMENT_ID: u64 = 0xAE;
const TRACK_TYPE_ELEMENT_ID: u64 = 0x83;
const CODEC_ID_ELEMENT_ID: u64 = 0x86;
const DEFAULT_DURATION_ELEMENT_ID: u64 = 0x23_E383;
const VIDEO_ELEMENT_ID: u64 = 0xE0;
const PIXEL_WIDTH_ELEMENT_ID: u64 = 0xB0;
const PIXEL_HEIGHT_ELEMENT_ID: u64 = 0xBA;
const AUDIO_ELEMENT_ID: u64 = 0xE1;
const SAMPLING_FREQUENCY_ELEMENT_ID: u64 = 0xB5;
const CHANNELS_ELEMENT_ID: u64 = 0x9F;
const CLUSTER_ELEMENT_ID: u64 = 0x1F43_B675;

const VIDEO_TRACK_TYPE: u64 = 1;
const AUDIO_TRACK_TYPE: u64 = 2;

/// The default `TimestampScale`: one tick is a millisecond, in nanoseconds.
const DEFAULT_TIMESTAMP_SCALE_NANOSECONDS: u64 = 1_000_000;

/// The largest `Info` or `Tracks` element read into memory.
const LARGEST_HEADER_ELEMENT_READ_BYTES: u64 = 4 * 1024 * 1024;

/// A variable-length integer's length from its first byte's leading zeros, 1 to 8.
fn variable_length_integer_length(first_byte: u8) -> Option<usize> {
    let leading_zeros = first_byte.leading_zeros() as usize;
    (leading_zeros < 8).then_some(leading_zeros + 1)
}

/// An element ID, its length marker kept, as EBML spells IDs.
fn read_element_id(reader: &mut impl Read) -> Option<u64> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first).ok()?;
    let length = variable_length_integer_length(first[0]).filter(|length| *length <= 4)?;
    let mut rest = [0u8; 3];
    reader.read_exact(&mut rest[..length - 1]).ok()?;
    Some(
        rest[..length - 1]
            .iter()
            .fold(u64::from(first[0]), |id, byte| (id << 8) | u64::from(*byte)),
    )
}

/// An element's data size, its length marker stripped; `Some(None)` for an unknown size, which
/// EBML marks with every data bit set.
fn read_element_size(reader: &mut impl Read) -> Option<Option<u64>> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first).ok()?;
    let length = variable_length_integer_length(first[0])?;
    let mut rest = [0u8; 7];
    reader.read_exact(&mut rest[..length - 1]).ok()?;
    let marker_stripped = u64::from(first[0]) & (0xFF >> length);
    let size = rest[..length - 1]
        .iter()
        .fold(marker_stripped, |size, byte| (size << 8) | u64::from(*byte));
    let all_ones = (1u64 << (7 * length)) - 1;
    Some((size != all_ones).then_some(size))
}

/// Each child element in `bytes`, by ID and data.
fn elements_in(bytes: &[u8]) -> Vec<(u64, &[u8])> {
    let mut elements = Vec::new();
    let mut reader = bytes;
    while !reader.is_empty() {
        let Some(id) = read_element_id(&mut reader) else {
            break;
        };
        let Some(Some(size)) = read_element_size(&mut reader) else {
            break;
        };
        let Ok(size) = usize::try_from(size) else {
            break;
        };
        if size > reader.len() {
            break;
        }
        let (data, rest) = reader.split_at(size);
        elements.push((id, data));
        reader = rest;
    }
    elements
}

fn unsigned_integer_of(data: &[u8]) -> Option<u64> {
    (data.len() <= 8).then(|| {
        data.iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
    })
}

fn float_of(data: &[u8]) -> Option<f64> {
    match data.len() {
        4 => Some(f64::from(f32::from_be_bytes(data.try_into().ok()?))),
        8 => Some(f64::from_be_bytes(data.try_into().ok()?)),
        _ => None,
    }
}

fn child_of(bytes: &[u8], id: u64) -> Option<&[u8]> {
    elements_in(bytes)
        .into_iter()
        .find(|(found, _)| *found == id)
        .map(|(_, data)| data)
}

fn video_codec_of_a_codec_id(codec_id: &str) -> Option<&'static str> {
    Some(match codec_id {
        "V_MPEG4/ISO/AVC" => "h264",
        "V_MPEGH/ISO/HEVC" => "h265",
        "V_AV1" => "av1",
        "V_VP9" => "vp9",
        "V_VP8" => "vp8",
        "V_PRORES" => "prores",
        "V_MJPEG" => "mjpeg",
        _ => return None,
    })
}

fn audio_codec_of_a_codec_id(codec_id: &str) -> Option<&'static str> {
    Some(match codec_id {
        "A_OPUS" => "opus",
        "A_VORBIS" => "vorbis",
        "A_FLAC" => "flac",
        "A_AC3" => "ac3",
        "A_EAC3" => "eac3",
        "A_MPEG/L3" => "mp3",
        id if id.starts_with("A_AAC") => "aac",
        id if id.starts_with("A_PCM") => "pcm",
        _ => return None,
    })
}

fn facts_of_the_tracks(tracks: &[u8], media_facts: &mut AssetMediaFactsOnTheWire) {
    for (_, track_entry) in elements_in(tracks)
        .into_iter()
        .filter(|(id, _)| *id == TRACK_ENTRY_ELEMENT_ID)
    {
        let track_type = child_of(track_entry, TRACK_TYPE_ELEMENT_ID).and_then(unsigned_integer_of);
        let codec_id = child_of(track_entry, CODEC_ID_ELEMENT_ID).map(|data| {
            String::from_utf8_lossy(data)
                .trim_end_matches('\0')
                .to_owned()
        });
        match track_type {
            Some(VIDEO_TRACK_TYPE) if media_facts.video.is_none() => {
                let video = child_of(track_entry, VIDEO_ELEMENT_ID).unwrap_or_default();
                let frame_duration_nanoseconds = child_of(track_entry, DEFAULT_DURATION_ELEMENT_ID)
                    .and_then(unsigned_integer_of)
                    .filter(|nanoseconds| *nanoseconds > 0);
                media_facts.video = Some(AssetVideoFactsOnTheWire {
                    pixel_width: child_of(video, PIXEL_WIDTH_ELEMENT_ID)
                        .and_then(unsigned_integer_of)
                        .and_then(|width| u32::try_from(width).ok()),
                    pixel_height: child_of(video, PIXEL_HEIGHT_ELEMENT_ID)
                        .and_then(unsigned_integer_of)
                        .and_then(|height| u32::try_from(height).ok()),
                    frames_per_second: frame_duration_nanoseconds
                        .map(|nanoseconds| (1e12 / nanoseconds as f64).round() / 1_000.0),
                    codec: codec_id
                        .as_deref()
                        .and_then(video_codec_of_a_codec_id)
                        .map(str::to_owned),
                });
            }
            Some(AUDIO_TRACK_TYPE) if media_facts.audio.is_none() => {
                let audio = child_of(track_entry, AUDIO_ELEMENT_ID).unwrap_or_default();
                // RFC 9559 defaults: `SamplingFrequency` 8000 and `Channels` 1 when absent.
                media_facts.audio = Some(AssetAudioFactsOnTheWire {
                    sample_rate_hz: child_of(audio, SAMPLING_FREQUENCY_ELEMENT_ID)
                        .and_then(float_of)
                        .map(|hz| hz.round() as u32)
                        .or(Some(8_000)),
                    channel_count: child_of(audio, CHANNELS_ELEMENT_ID)
                        .and_then(unsigned_integer_of)
                        .and_then(|count| u32::try_from(count).ok())
                        .or(Some(1)),
                    codec: codec_id
                        .as_deref()
                        .and_then(audio_codec_of_a_codec_id)
                        .map(str::to_owned),
                });
            }
            _ => {}
        }
    }
}

fn duration_ms_of_the_segment_info(segment_info: &[u8]) -> Option<u64> {
    let timestamp_scale_nanoseconds = child_of(segment_info, TIMESTAMP_SCALE_ELEMENT_ID)
        .and_then(unsigned_integer_of)
        .unwrap_or(DEFAULT_TIMESTAMP_SCALE_NANOSECONDS);
    let duration_ticks = child_of(segment_info, DURATION_ELEMENT_ID).and_then(float_of)?;
    let milliseconds = duration_ticks * timestamp_scale_nanoseconds as f64 / 1e6;
    (milliseconds.is_finite() && milliseconds >= 0.0).then(|| milliseconds.round() as u64)
}

pub(crate) fn read_a_matroska_file(
    matroska_file_path: &Path,
) -> Result<AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable> {
    let not_matroska = || {
        AssetMediaFactsUnreadable::because(
            "it does not open with an EBML header, so it is no Matroska or WebM file",
        )
    };
    let file = File::open(matroska_file_path).map_err(|why| {
        AssetMediaFactsUnreadable::because(format!("the file could not be opened: {why}"))
    })?;
    let mut reader = BufReader::new(file);
    if read_element_id(&mut reader) != Some(EBML_HEADER_ELEMENT_ID) {
        return Err(not_matroska());
    }
    let Some(Some(ebml_header_size)) = read_element_size(&mut reader) else {
        return Err(not_matroska());
    };
    let skip = |reader: &mut BufReader<File>, size: u64| {
        i64::try_from(size)
            .ok()
            .and_then(|size| reader.seek_relative(size).ok())
    };
    skip(&mut reader, ebml_header_size).ok_or_else(not_matroska)?;
    if read_element_id(&mut reader) != Some(SEGMENT_ELEMENT_ID)
        || read_element_size(&mut reader).is_none()
    {
        return Err(AssetMediaFactsUnreadable::because(
            "its EBML header is followed by no Matroska segment",
        ));
    }
    let mut media_facts = AssetMediaFactsOnTheWire::default();
    let (mut info_read, mut tracks_read) = (false, false);
    while !(info_read && tracks_read) {
        let Some(id) = read_element_id(&mut reader) else {
            break;
        };
        let Some(size) = read_element_size(&mut reader) else {
            break;
        };
        if id == CLUSTER_ELEMENT_ID || size.is_none() {
            break;
        }
        let size = size.unwrap_or_default();
        if (id == SEGMENT_INFO_ELEMENT_ID || id == TRACKS_ELEMENT_ID)
            && size <= LARGEST_HEADER_ELEMENT_READ_BYTES
        {
            let mut data = vec![0u8; usize::try_from(size).unwrap_or_default()];
            if reader.read_exact(&mut data).is_err() {
                break;
            }
            if id == SEGMENT_INFO_ELEMENT_ID {
                media_facts.duration_ms = duration_ms_of_the_segment_info(&data);
                info_read = true;
            } else {
                facts_of_the_tracks(&data, &mut media_facts);
                tracks_read = true;
            }
        } else if skip(&mut reader, size).is_none() {
            break;
        }
    }
    Ok(AssetMediaFactsReadFromTheFile {
        media_facts,
        preview_picture: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetMediaFactsReadersOfThisRuntime, read_the_media_facts_of_an_asset_file};

    fn an_element(id: u64, data: &[u8]) -> Vec<u8> {
        let id_bytes = id.to_be_bytes();
        let first_non_zero = id_bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
        let mut written = id_bytes[first_non_zero..].to_vec();
        written.push(0x01);
        written.extend_from_slice(&(data.len() as u64).to_be_bytes()[1..]);
        written.extend_from_slice(data);
        written
    }

    fn an_unsigned_integer(id: u64, value: u64) -> Vec<u8> {
        an_element(id, &value.to_be_bytes())
    }

    /// A WebM of 3.5 seconds: VP9 1280×720 at 25 fps, and Opus stereo at 48 kHz.
    fn a_vp9_and_opus_webm() -> Vec<u8> {
        let segment_info = [
            an_unsigned_integer(TIMESTAMP_SCALE_ELEMENT_ID, 1_000_000),
            an_element(DURATION_ELEMENT_ID, &3_500.0f64.to_be_bytes()),
        ]
        .concat();
        let video_track = [
            an_unsigned_integer(TRACK_TYPE_ELEMENT_ID, VIDEO_TRACK_TYPE),
            an_element(CODEC_ID_ELEMENT_ID, b"V_VP9"),
            an_unsigned_integer(DEFAULT_DURATION_ELEMENT_ID, 40_000_000),
            an_element(
                VIDEO_ELEMENT_ID,
                &[
                    an_unsigned_integer(PIXEL_WIDTH_ELEMENT_ID, 1280),
                    an_unsigned_integer(PIXEL_HEIGHT_ELEMENT_ID, 720),
                ]
                .concat(),
            ),
        ]
        .concat();
        let audio_track = [
            an_unsigned_integer(TRACK_TYPE_ELEMENT_ID, AUDIO_TRACK_TYPE),
            an_element(CODEC_ID_ELEMENT_ID, b"A_OPUS"),
            an_element(
                AUDIO_ELEMENT_ID,
                &[
                    an_element(SAMPLING_FREQUENCY_ELEMENT_ID, &48_000.0f64.to_be_bytes()),
                    an_unsigned_integer(CHANNELS_ELEMENT_ID, 2),
                ]
                .concat(),
            ),
        ]
        .concat();
        let tracks = [
            an_element(TRACK_ENTRY_ELEMENT_ID, &video_track),
            an_element(TRACK_ENTRY_ELEMENT_ID, &audio_track),
        ]
        .concat();
        let segment = [
            an_element(SEGMENT_INFO_ELEMENT_ID, &segment_info),
            an_element(TRACKS_ELEMENT_ID, &tracks),
            an_element(CLUSTER_ELEMENT_ID, &[0u8; 16]),
        ]
        .concat();
        [
            an_element(EBML_HEADER_ELEMENT_ID, &an_element(0x4282, b"webm")),
            an_element(SEGMENT_ELEMENT_ID, &segment),
        ]
        .concat()
    }

    #[test]
    fn a_webm_reads_its_duration_and_each_tracks_facts() {
        let directory = tempfile::tempdir().unwrap();
        let webm = directory.path().join("loop.webm");
        std::fs::write(&webm, a_vp9_and_opus_webm()).unwrap();
        let read = read_the_media_facts_of_an_asset_file(
            &webm,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        )
        .unwrap();
        assert_eq!(read.media_facts.duration_ms, Some(3_500));
        assert_eq!(
            read.media_facts.video,
            Some(AssetVideoFactsOnTheWire {
                pixel_width: Some(1280),
                pixel_height: Some(720),
                frames_per_second: Some(25.0),
                codec: Some("vp9".to_owned()),
            })
        );
        assert_eq!(
            read.media_facts.audio,
            Some(AssetAudioFactsOnTheWire {
                sample_rate_hz: Some(48_000),
                channel_count: Some(2),
                codec: Some("opus".to_owned()),
            })
        );
    }

    #[test]
    fn a_file_that_is_no_matroska_is_unreadable() {
        let directory = tempfile::tempdir().unwrap();
        let mkv = directory.path().join("broken.mkv");
        std::fs::write(&mkv, b"RIFF....").unwrap();
        assert!(
            read_the_media_facts_of_an_asset_file(
                &mkv,
                &AssetMediaFactsReadersOfThisRuntime::default()
            )
            .is_err()
        );
    }
}
