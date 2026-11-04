// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The two messages a page input reads: a `VideoFrame` record naming the
//! surface to read back, and an audio block whose samples reach the page as
//! interleaved little-endian `f32`.

use rmpv::Value as MsgpackValue;

/// The entries of a `VideoFrame` record a page input reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageInputVideoFrameRecord {
    pub surface_id: String,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: i64,
}

/// One audio block, its samples converted to little-endian `f32`.
#[derive(Debug, Clone, PartialEq)]
pub struct PageInputAudioBlock {
    pub sample_rate: u32,
    pub channel_count: u32,
    pub first_sample_timestamp_ns: i64,
    pub interleaved_samples_le_f32: Vec<u8>,
}

fn msgpack_map_of(
    payload: &[u8],
    subject: &str,
) -> Result<Vec<(MsgpackValue, MsgpackValue)>, String> {
    let mut remaining = payload;
    match rmpv::decode::read_value(&mut remaining) {
        Ok(MsgpackValue::Map(entries)) => Ok(entries),
        Ok(other) => Err(format!("{subject} is a msgpack map, not {other}")),
        Err(undecodable) => Err(format!("{subject} is not msgpack: {undecodable}")),
    }
}

fn entry<'map>(
    entries: &'map [(MsgpackValue, MsgpackValue)],
    key: &str,
    subject: &str,
) -> Result<&'map MsgpackValue, String> {
    entries
        .iter()
        .find(|(entry_key, _)| entry_key.as_str() == Some(key))
        .map(|(_, value)| value)
        .ok_or_else(|| format!("{subject} has no {key:?}"))
}

fn u32_entry(
    entries: &[(MsgpackValue, MsgpackValue)],
    key: &str,
    subject: &str,
) -> Result<u32, String> {
    entry(entries, key, subject)?
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("{subject}'s {key:?} is not an unsigned 32-bit integer"))
}

fn i64_entry(
    entries: &[(MsgpackValue, MsgpackValue)],
    key: &str,
    subject: &str,
) -> Result<i64, String> {
    entry(entries, key, subject)?
        .as_i64()
        .ok_or_else(|| format!("{subject}'s {key:?} is not a signed 64-bit integer"))
}

impl PageInputVideoFrameRecord {
    /// Read `surface_id`, `width`, `height` and `timestamp_ns` from a
    /// `VideoFrame` message; every other entry is ignored.
    pub fn from_msgpack(payload: &[u8]) -> Result<Self, String> {
        let subject = "a video input's message";
        let entries = msgpack_map_of(payload, subject)?;
        let surface_id = entry(&entries, "surface_id", subject)?
            .as_str()
            .ok_or_else(|| format!("{subject}'s \"surface_id\" is not text"))?
            .to_string();
        Ok(Self {
            surface_id,
            width: u32_entry(&entries, "width", subject)?,
            height: u32_entry(&entries, "height", subject)?,
            timestamp_ns: i64_entry(&entries, "timestamp_ns", subject)?,
        })
    }
}

impl PageInputAudioBlock {
    /// Read an audio-block message (`docs/plan/ARCHITECTURE.md` §Media I/O),
    /// converting `i16` samples to `f32` in -1 to 1.
    pub fn from_msgpack(payload: &[u8]) -> Result<Self, String> {
        let subject = "an audio input's message";
        let entries = msgpack_map_of(payload, subject)?;
        let samples = match entry(&entries, "samples", subject)? {
            MsgpackValue::Binary(samples) => samples,
            _ => return Err(format!("{subject}'s \"samples\" is not binary")),
        };
        let channel_count = u32_entry(&entries, "channels", subject)?;
        let sample_count = u32_entry(&entries, "sample_count", subject)?;
        let dtype = match entries
            .iter()
            .find(|(entry_key, _)| entry_key.as_str() == Some("dtype"))
        {
            Some((_, dtype)) => dtype
                .as_str()
                .ok_or_else(|| format!("{subject}'s \"dtype\" is not text"))?,
            None => "f32",
        };
        let bytes_per_sample = match dtype {
            "f32" => 4,
            "i16" => 2,
            other => {
                return Err(format!(
                    "{subject}'s dtype {other:?} is neither f32 nor i16"
                ));
            }
        };
        let expected_byte_count =
            u64::from(sample_count) * u64::from(channel_count) * bytes_per_sample;
        if samples.len() as u64 != expected_byte_count {
            return Err(format!(
                "{subject} holds {} sample bytes, not the {expected_byte_count} of \
                 {sample_count} {dtype} samples on {channel_count} channels",
                samples.len()
            ));
        }
        let interleaved_samples_le_f32 = if dtype == "f32" {
            samples.clone()
        } else {
            let (i16_samples, _) = samples.as_chunks::<2>();
            i16_samples
                .iter()
                .flat_map(|sample| {
                    (f32::from(i16::from_le_bytes(*sample)) / 32_768.0).to_le_bytes()
                })
                .collect()
        };
        Ok(Self {
            sample_rate: u32_entry(&entries, "sample_rate", subject)?,
            channel_count,
            first_sample_timestamp_ns: i64_entry(&entries, "first_sample_timestamp_ns", subject)?,
            interleaved_samples_le_f32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_audio_blocks::PageAudioStreamTimeline;
    use crate::video_frame_record::PaintedPageVideoFrame;

    fn msgpack_of(value: &MsgpackValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, value).unwrap();
        bytes
    }

    #[test]
    fn a_video_frame_record_reads_back_the_entries_the_readback_needs() {
        let record = PaintedPageVideoFrame {
            surface_id: "slot-2#9",
            width: 1920,
            height: 1080,
            timestamp_ns: 123,
            frames_per_second: 30,
        }
        .to_msgpack();

        assert_eq!(
            PageInputVideoFrameRecord::from_msgpack(&record).unwrap(),
            PageInputVideoFrameRecord {
                surface_id: "slot-2#9".to_string(),
                width: 1920,
                height: 1080,
                timestamp_ns: 123,
            }
        );
        assert!(PageInputVideoFrameRecord::from_msgpack(&[0xc0]).is_err());
    }

    #[test]
    fn an_f32_block_passes_its_samples_through() {
        let block = PageAudioStreamTimeline::new(48_000, 2).audio_block_message_payload(
            &[0.25, -0.5],
            1,
            77,
        );

        let read = PageInputAudioBlock::from_msgpack(&block).unwrap();

        assert_eq!(read.sample_rate, 48_000);
        assert_eq!(read.channel_count, 2);
        assert_eq!(read.first_sample_timestamp_ns, 77);
        let expected: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(read.interleaved_samples_le_f32, expected);
    }

    #[test]
    fn an_i16_block_converts_to_f32_and_a_short_block_is_refused() {
        let samples: Vec<u8> = [i16::MIN, 16_384]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let block = |samples: Vec<u8>| {
            msgpack_of(&MsgpackValue::Map(vec![
                ("samples".into(), MsgpackValue::Binary(samples)),
                ("sample_rate".into(), 16_000.into()),
                ("channels".into(), 1.into()),
                ("sample_count".into(), 2.into()),
                ("dtype".into(), "i16".into()),
                ("first_sample_timestamp_ns".into(), (-4).into()),
            ]))
        };

        let read = PageInputAudioBlock::from_msgpack(&block(samples.clone())).unwrap();

        let expected: Vec<u8> = [-1.0f32, 0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(read.interleaved_samples_le_f32, expected);
        assert_eq!(read.first_sample_timestamp_ns, -4);
        assert!(PageInputAudioBlock::from_msgpack(&block(samples[..3].to_vec())).is_err());
    }
}
