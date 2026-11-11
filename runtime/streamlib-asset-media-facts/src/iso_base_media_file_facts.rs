// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MP4 and QuickTime files (ISO/IEC 14496-12 boxes): duration from `mvhd`, and from each
//! track's `mdhd`, `hdlr`, `stsd` and `stts` its picture size, frame rate, sample rate, channel
//! count and codec.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetAudioFactsOnTheWire, AssetMediaFactsOnTheWire, AssetVideoFactsOnTheWire,
};

use crate::{AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable, milliseconds_of_ticks};

/// The largest `moov` box read into memory; a sample table past it is not a file Studio plays.
const LARGEST_MOVIE_BOX_READ_BYTES: u64 = 64 * 1024 * 1024;

/// One box's type and body, inside a box already in memory.
struct IsoBaseMediaBoxInMemory<'file> {
    box_type: [u8; 4],
    body: &'file [u8],
}

/// Each box in `bytes`, stopping at the first whose size runs past them.
fn boxes_in(bytes: &[u8]) -> Vec<IsoBaseMediaBoxInMemory<'_>> {
    let mut boxes = Vec::new();
    let mut offset = 0usize;
    while offset + 8 <= bytes.len() {
        let size32 = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        let box_type = [
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ];
        let (header_len, box_len) = match size32 {
            0 => (8, bytes.len() - offset),
            1 => {
                let Some(largesize) = read_u64(bytes, offset + 8) else {
                    break;
                };
                let Ok(largesize) = usize::try_from(largesize) else {
                    break;
                };
                (16, largesize)
            }
            size => (8, size as usize),
        };
        if box_len < header_len || offset + box_len > bytes.len() {
            break;
        }
        boxes.push(IsoBaseMediaBoxInMemory {
            box_type,
            body: &bytes[offset + header_len..offset + box_len],
        });
        offset += box_len;
    }
    boxes
}

fn child<'file>(bytes: &'file [u8], box_type: &[u8; 4]) -> Option<&'file [u8]> {
    boxes_in(bytes)
        .into_iter()
        .find(|found| &found.box_type == box_type)
        .map(|found| found.body)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

/// A full box's timescale and duration, as `mvhd` and `mdhd` lay them out by version.
fn timescale_and_duration_of_a_header_box(body: &[u8]) -> Option<(u32, u64)> {
    match *body.first()? {
        1 => Some((read_u32(body, 20)?, read_u64(body, 24)?)),
        _ => Some((read_u32(body, 12)?, u64::from(read_u32(body, 16)?))),
    }
}

/// The movie box's bytes, found among the file's top-level boxes without reading the media.
fn the_movie_box_of(file: &mut File) -> Result<Vec<u8>, AssetMediaFactsUnreadable> {
    let unreadable = |why: std::io::Error| {
        AssetMediaFactsUnreadable::because(format!("the file did not read: {why}"))
    };
    let file_len = file.metadata().map_err(unreadable)?.len();
    let mut offset = 0u64;
    while offset + 8 <= file_len {
        file.seek(SeekFrom::Start(offset)).map_err(unreadable)?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8]).map_err(unreadable)?;
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let box_type = [header[4], header[5], header[6], header[7]];
        let (header_len, box_len) = match size32 {
            0 => (8u64, file_len - offset),
            1 => {
                file.read_exact(&mut header[8..16]).map_err(unreadable)?;
                (
                    16,
                    u64::from_be_bytes(header[8..16].try_into().unwrap_or_default()),
                )
            }
            size => (8, u64::from(size)),
        };
        if box_len < header_len || offset + box_len > file_len {
            break;
        }
        if &box_type == b"moov" {
            let body_len = box_len - header_len;
            if body_len > LARGEST_MOVIE_BOX_READ_BYTES {
                return Err(AssetMediaFactsUnreadable::because(format!(
                    "its movie box is {body_len} bytes, past the {LARGEST_MOVIE_BOX_READ_BYTES} the runtime reads"
                )));
            }
            let mut body = vec![0u8; usize::try_from(body_len).unwrap_or_default()];
            file.read_exact(&mut body).map_err(unreadable)?;
            return Ok(body);
        }
        offset += box_len;
    }
    Err(AssetMediaFactsUnreadable::because(
        "it holds no movie box (`moov`), so it is no MP4 or QuickTime file the runtime can read",
    ))
}

fn video_codec_of_a_sample_entry(sample_entry_type: &[u8; 4]) -> Option<&'static str> {
    Some(match sample_entry_type {
        b"avc1" | b"avc3" => "h264",
        b"hvc1" | b"hev1" => "h265",
        b"av01" => "av1",
        b"vp09" => "vp9",
        b"vp08" => "vp8",
        b"mp4v" => "mpeg4",
        b"apcn" | b"apch" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => "prores",
        b"jpeg" | b"mjpa" | b"mjpb" => "mjpeg",
        _ => return None,
    })
}

fn audio_codec_of_a_sample_entry(sample_entry_type: &[u8; 4]) -> Option<&'static str> {
    Some(match sample_entry_type {
        b"mp4a" => "aac",
        b"Opus" => "opus",
        b"fLaC" => "flac",
        b"alac" => "alac",
        b"ac-3" => "ac3",
        b"ec-3" => "eac3",
        b".mp3" => "mp3",
        b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"ipcm" | b"fpcm" => {
            "pcm"
        }
        _ => return None,
    })
}

/// The first sample entry of a track's `stsd`: its type and its body after the 8-byte header.
fn first_sample_entry_of(sample_description: &[u8]) -> Option<([u8; 4], &[u8])> {
    let entries = sample_description.get(8..)?;
    let entry = boxes_in(entries).into_iter().next()?;
    Some((entry.box_type, entry.body))
}

/// How many samples a track's `stts` counts.
fn sample_count_of_a_time_to_sample_box(time_to_sample: &[u8]) -> Option<u64> {
    let entry_count = read_u32(time_to_sample, 4)? as usize;
    (0..entry_count)
        .map(|entry| read_u32(time_to_sample, 8 + entry * 8).map(u64::from))
        .sum()
}

struct TrackFactsInAMovie {
    duration_ms: Option<u64>,
    video: Option<AssetVideoFactsOnTheWire>,
    audio: Option<AssetAudioFactsOnTheWire>,
}

fn facts_of_a_track(track: &[u8]) -> Option<TrackFactsInAMovie> {
    let media = child(track, b"mdia")?;
    let (media_timescale, media_duration) =
        child(media, b"mdhd").and_then(timescale_and_duration_of_a_header_box)?;
    let handler_type: [u8; 4] = child(media, b"hdlr")?.get(8..12)?.try_into().ok()?;
    let sample_table = child(child(media, b"minf")?, b"stbl")?;
    let (sample_entry_type, sample_entry) =
        child(sample_table, b"stsd").and_then(first_sample_entry_of)?;
    let duration_ms = milliseconds_of_ticks(media_duration, u64::from(media_timescale));
    match &handler_type {
        b"vide" => {
            let frames_per_second = child(sample_table, b"stts")
                .and_then(sample_count_of_a_time_to_sample_box)
                .filter(|_| media_duration > 0)
                .map(|sample_count| {
                    sample_count as f64 * f64::from(media_timescale) / media_duration as f64
                });
            Some(TrackFactsInAMovie {
                duration_ms,
                video: Some(AssetVideoFactsOnTheWire {
                    pixel_width: read_u16(sample_entry, 24).map(u32::from),
                    pixel_height: read_u16(sample_entry, 26).map(u32::from),
                    frames_per_second: frames_per_second
                        .map(|fps| (fps * 1_000.0).round() / 1_000.0),
                    codec: video_codec_of_a_sample_entry(&sample_entry_type).map(str::to_owned),
                }),
                audio: None,
            })
        }
        b"soun" => {
            let sound_entry_version = read_u16(sample_entry, 8)?;
            let (channel_count, sample_rate_hz) = if sound_entry_version == 2 {
                let sample_rate = f64::from_bits(read_u64(sample_entry, 32)?);
                (read_u32(sample_entry, 40), Some(sample_rate.round() as u32))
            } else {
                (
                    read_u16(sample_entry, 16).map(u32::from),
                    read_u32(sample_entry, 24).map(|fixed_16_16| fixed_16_16 >> 16),
                )
            };
            let sample_rate_hz = sample_rate_hz
                .filter(|rate| *rate > 0)
                .or(Some(media_timescale).filter(|rate| *rate > 0));
            Some(TrackFactsInAMovie {
                duration_ms,
                video: None,
                audio: Some(AssetAudioFactsOnTheWire {
                    sample_rate_hz,
                    channel_count: channel_count.filter(|count| *count > 0),
                    codec: audio_codec_of_a_sample_entry(&sample_entry_type).map(str::to_owned),
                }),
            })
        }
        _ => None,
    }
}

pub(crate) fn read_an_iso_base_media_file(
    movie_file_path: &Path,
) -> Result<AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable> {
    let mut file = File::open(movie_file_path).map_err(|why| {
        AssetMediaFactsUnreadable::because(format!("the file could not be opened: {why}"))
    })?;
    let movie = the_movie_box_of(&mut file)?;
    let mut media_facts = AssetMediaFactsOnTheWire {
        duration_ms: child(&movie, b"mvhd")
            .and_then(timescale_and_duration_of_a_header_box)
            .filter(|(_, duration)| *duration > 0)
            .and_then(|(timescale, duration)| {
                milliseconds_of_ticks(duration, u64::from(timescale))
            }),
        ..AssetMediaFactsOnTheWire::default()
    };
    let mut longest_track_ms = None;
    for track in boxes_in(&movie)
        .into_iter()
        .filter(|found| &found.box_type == b"trak")
    {
        let Some(track_facts) = facts_of_a_track(track.body) else {
            continue;
        };
        longest_track_ms = longest_track_ms.max(track_facts.duration_ms);
        if media_facts.video.is_none() {
            media_facts.video = track_facts.video;
        }
        if media_facts.audio.is_none() {
            media_facts.audio = track_facts.audio;
        }
    }
    if media_facts.duration_ms.is_none() {
        media_facts.duration_ms = longest_track_ms.filter(|ms| *ms > 0);
    }
    Ok(AssetMediaFactsReadFromTheFile {
        media_facts,
        preview_picture: None,
    })
}

#[cfg(test)]
pub(crate) mod iso_base_media_test_files {
    //! Box writers for the smallest MP4 the reader reads: a movie header, a video track and an
    //! audio track, with no media data.

    pub(crate) fn a_box(box_type: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut written = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        written.extend_from_slice(box_type);
        written.extend_from_slice(body);
        written
    }

    fn a_header_box(box_type: &[u8; 4], timescale: u32, duration: u32) -> Vec<u8> {
        let mut body = vec![0u8; 12];
        body.extend_from_slice(&timescale.to_be_bytes());
        body.extend_from_slice(&duration.to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);
        a_box(box_type, &body)
    }

    fn a_handler_box(handler_type: &[u8; 4]) -> Vec<u8> {
        let mut body = vec![0u8; 8];
        body.extend_from_slice(handler_type);
        body.extend_from_slice(&[0u8; 13]);
        a_box(b"hdlr", &body)
    }

    fn a_track(
        handler_type: &[u8; 4],
        timescale: u32,
        duration: u32,
        sample_entry: Vec<u8>,
        sample_count: u32,
    ) -> Vec<u8> {
        let mut sample_description = vec![0u8; 4];
        sample_description.extend_from_slice(&1u32.to_be_bytes());
        sample_description.extend(sample_entry);
        let mut time_to_sample = vec![0u8; 4];
        time_to_sample.extend_from_slice(&1u32.to_be_bytes());
        time_to_sample.extend_from_slice(&sample_count.to_be_bytes());
        time_to_sample.extend_from_slice(&(duration / sample_count.max(1)).to_be_bytes());
        let sample_table = [
            a_box(b"stsd", &sample_description),
            a_box(b"stts", &time_to_sample),
        ]
        .concat();
        let media_information = a_box(b"stbl", &sample_table);
        let media = [
            a_header_box(b"mdhd", timescale, duration),
            a_handler_box(handler_type),
            a_box(b"minf", &media_information),
        ]
        .concat();
        a_box(b"trak", &a_box(b"mdia", &media))
    }

    fn a_visual_sample_entry(sample_entry_type: &[u8; 4], width: u16, height: u16) -> Vec<u8> {
        let mut body = vec![0u8; 24];
        body.extend_from_slice(&width.to_be_bytes());
        body.extend_from_slice(&height.to_be_bytes());
        body.extend_from_slice(&[0u8; 50]);
        a_box(sample_entry_type, &body)
    }

    fn an_audio_sample_entry(channel_count: u16, sample_rate: u32) -> Vec<u8> {
        let mut body = vec![0u8; 16];
        body.extend_from_slice(&channel_count.to_be_bytes());
        body.extend_from_slice(&16u16.to_be_bytes());
        body.extend_from_slice(&[0u8; 4]);
        body.extend_from_slice(&(sample_rate << 16).to_be_bytes());
        a_box(b"mp4a", &body)
    }

    /// An MP4 of 42 seconds: H.264 1920×1080 at 30 fps, and AAC stereo at 48 kHz.
    pub(crate) fn an_h264_and_aac_mp4_of_42_seconds() -> Vec<u8> {
        let movie = [
            a_header_box(b"mvhd", 1_000, 42_000),
            a_track(
                b"vide",
                90_000,
                90_000 * 42,
                a_visual_sample_entry(b"avc1", 1920, 1080),
                30 * 42,
            ),
            a_track(
                b"soun",
                48_000,
                48_000 * 42,
                an_audio_sample_entry(2, 48_000),
                (48_000 * 42) / 1024,
            ),
        ]
        .concat();
        [
            a_box(b"ftyp", b"isom\0\0\x02\0isomavc1"),
            a_box(b"mdat", &[0u8; 32]),
            a_box(b"moov", &movie),
        ]
        .concat()
    }
}

#[cfg(test)]
mod tests {
    use super::iso_base_media_test_files::*;
    use super::*;
    use crate::read_the_media_facts_of_an_asset_file;

    #[test]
    fn an_mp4_reads_its_duration_and_each_tracks_facts_from_a_movie_box_after_its_media() {
        let directory = tempfile::tempdir().unwrap();
        let mp4 = directory.path().join("intro.mp4");
        std::fs::write(&mp4, an_h264_and_aac_mp4_of_42_seconds()).unwrap();
        let read = read_the_media_facts_of_an_asset_file(&mp4).unwrap();
        assert_eq!(read.media_facts.duration_ms, Some(42_000));
        assert_eq!(
            read.media_facts.video,
            Some(AssetVideoFactsOnTheWire {
                pixel_width: Some(1920),
                pixel_height: Some(1080),
                frames_per_second: Some(30.0),
                codec: Some("h264".to_owned()),
            })
        );
        assert_eq!(
            read.media_facts.audio,
            Some(AssetAudioFactsOnTheWire {
                sample_rate_hz: Some(48_000),
                channel_count: Some(2),
                codec: Some("aac".to_owned()),
            })
        );
        assert_eq!(read.preview_picture, None);
    }

    #[test]
    fn a_file_with_no_movie_box_is_unreadable() {
        let directory = tempfile::tempdir().unwrap();
        let mp4 = directory.path().join("broken.mp4");
        std::fs::write(&mp4, a_box(b"ftyp", b"isom")).unwrap();
        let refused = read_the_media_facts_of_an_asset_file(&mp4).unwrap_err();
        assert!(refused.0.contains("no movie box"), "{refused}");
    }
}
