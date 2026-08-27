// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio-block bag convention the built-ins produce and consume.
//!
//! A link carries a self-describing msgpack named map; these types are the
//! optional Rust cast for it — never declared on a port, never registered
//! anywhere. The field names ARE the wire contract: a consumer in any
//! language reads the same keys from the bag dict. The map is open — a
//! producer may carry extra keys and this cast ignores them, matching the
//! Python cast's behavior.
//!
//! Unlike a video frame, an audio block carries its payload inline: the
//! samples ride the bag as msgpack `bin`, and `dtype` says how to read those
//! bytes.

use serde::{Deserialize, Serialize};

/// Audio block bag: interleaved CPU samples ride the link inline, described
/// by the rate, channel count, and dtype beside them.
///
/// `first_sample_timestamp_ns` is the ordering primitive and the whole of
/// A/V sync: any sample's instant derives from it, `sample_count` and
/// `sample_rate`, so joining a block to a camera frame is subtracting two
/// timestamps in the same monotonic epoch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioBlock {
    /// The block's scalars, interleaved by channel, little-endian, read
    /// according to `dtype`.
    ///
    /// The wire key is `samples` and the payload is msgpack `bin`: one field
    /// spelling serves every dtype, and little-endian is a wire statement
    /// rather than an assumption — it is what a tap, a CLI, or a consumer in
    /// another language depends on.
    #[serde(rename = "samples", with = "serde_bytes")]
    pub interleaved_sample_bytes: Vec<u8>,
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Channel count the payload is interleaved by.
    pub channels: u32,
    /// Per-channel sample count: the payload carries
    /// `sample_count × channels` scalars, and the block's duration is
    /// `sample_count / sample_rate`.
    pub sample_count: u32,
    /// How to read `samples`. Absent on the wire means `f32`.
    #[serde(default)]
    pub dtype: AudioSampleDtype,
    /// Monotonic timestamp in nanoseconds of the block's first sample,
    /// stamped by the capturing device — the machine's monotonic epoch, the
    /// one a `VideoFrame.timestamp_ns` is stamped in.
    pub first_sample_timestamp_ns: i64,
}

/// How the scalars in an [`AudioBlock`]'s payload are encoded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioSampleDtype {
    #[default]
    #[serde(rename = "f32")]
    F32,
    #[serde(rename = "i16")]
    I16,
}

impl AudioSampleDtype {
    /// Bytes one scalar occupies in the payload.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            AudioSampleDtype::F32 => 4,
            AudioSampleDtype::I16 => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interleaved_f32_bytes(scalars: &[f32]) -> Vec<u8> {
        scalars.iter().flat_map(|scalar| scalar.to_le_bytes()).collect()
    }

    fn interleaved_i16_bytes(scalars: &[i16]) -> Vec<u8> {
        scalars.iter().flat_map(|scalar| scalar.to_le_bytes()).collect()
    }

    fn wire_map_entry(wire_bytes: &[u8], key_name: &str) -> rmpv::Value {
        let value: rmpv::Value =
            rmpv::decode::read_value(&mut &wire_bytes[..]).expect("msgpack decode");
        let rmpv::Value::Map(entries) = value else {
            panic!("wire value must be a named map, got {value:?}");
        };
        entries
            .iter()
            .find(|(key, _)| key.as_str() == Some(key_name))
            .unwrap_or_else(|| panic!("wire map missing key {key_name:?}"))
            .1
            .clone()
    }

    /// The actual wire is msgpack via `rmp_serde::to_vec_named` (what
    /// `OutputWriter::write` does), and `samples` has to land on it as `bin`.
    ///
    /// Asserted as the msgpack type rather than through a serde round trip,
    /// which passes either way: a `Vec<f32>` field would encode as a msgpack
    /// *array* — five bytes per sample, and a shape another language reads as
    /// a list of numbers rather than a buffer.
    #[test]
    fn audio_block_msgpack_wire_carries_the_samples_as_a_binary_payload() {
        let block = AudioBlock {
            interleaved_sample_bytes: interleaved_f32_bytes(&[
                -1.0, -0.5, 0.0, 0.25, 0.5, 1.0,
            ]),
            sample_rate: 48_000,
            channels: 2,
            sample_count: 3,
            dtype: AudioSampleDtype::F32,
            first_sample_timestamp_ns: 123_456_789,
        };

        let wire_bytes = rmp_serde::to_vec_named(&block).expect("msgpack serialize");
        assert_eq!(
            wire_map_entry(&wire_bytes, "samples"),
            rmpv::Value::Binary(block.interleaved_sample_bytes.clone()),
            "the samples field is a byte buffer, not a sequence of scalars"
        );
        assert_eq!(
            wire_map_entry(&wire_bytes, "sample_rate").as_u64(),
            Some(48_000)
        );
        assert_eq!(wire_map_entry(&wire_bytes, "channels").as_u64(), Some(2));
        assert_eq!(wire_map_entry(&wire_bytes, "sample_count").as_u64(), Some(3));
        assert_eq!(wire_map_entry(&wire_bytes, "dtype").as_str(), Some("f32"));
        assert_eq!(
            wire_map_entry(&wire_bytes, "first_sample_timestamp_ns").as_i64(),
            Some(123_456_789)
        );

        let round_tripped: AudioBlock =
            rmp_serde::from_slice(&wire_bytes).expect("msgpack deserialize");
        assert_eq!(round_tripped, block);
    }

    /// One field spelling serves every dtype: an `i16` block's payload is the
    /// same `bin`, half the width per scalar.
    #[test]
    fn an_i16_block_carries_its_samples_as_a_binary_payload_too() {
        let block = AudioBlock {
            interleaved_sample_bytes: interleaved_i16_bytes(&[i16::MIN, -1, 0, i16::MAX]),
            sample_rate: 16_000,
            channels: 1,
            sample_count: 4,
            dtype: AudioSampleDtype::I16,
            first_sample_timestamp_ns: 7,
        };

        let wire_bytes = rmp_serde::to_vec_named(&block).expect("msgpack serialize");
        assert_eq!(
            wire_map_entry(&wire_bytes, "samples"),
            rmpv::Value::Binary(block.interleaved_sample_bytes.clone())
        );
        assert_eq!(wire_map_entry(&wire_bytes, "dtype").as_str(), Some("i16"));
        assert_eq!(
            block.interleaved_sample_bytes.len(),
            block.sample_count as usize
                * block.channels as usize
                * block.dtype.bytes_per_sample(),
            "an interleaved block carries sample_count × channels scalars"
        );

        let round_tripped: AudioBlock =
            rmp_serde::from_slice(&wire_bytes).expect("msgpack deserialize");
        assert_eq!(round_tripped, block);
    }

    /// The bag map is open: a producer carrying extra keys must not break
    /// this cast (mirrors the Python cast's behavior).
    #[test]
    fn audio_block_cast_ignores_unknown_keys() {
        let wire_bytes = wire_bytes_for(vec![
            ("samples", rmpv::Value::Binary(interleaved_f32_bytes(&[0.5]))),
            ("sample_rate", rmpv::Value::from(44_100)),
            ("channels", rmpv::Value::from(1)),
            ("sample_count", rmpv::Value::from(1)),
            ("dtype", rmpv::Value::from("f32")),
            ("first_sample_timestamp_ns", rmpv::Value::from(11)),
            ("a_future_key", rmpv::Value::from("ignored")),
        ]);

        let block: AudioBlock = rmp_serde::from_slice(&wire_bytes).expect("open map");
        assert_eq!(block.sample_rate, 44_100);
        assert_eq!(block.interleaved_sample_bytes, interleaved_f32_bytes(&[0.5]));
    }

    /// `dtype` is metadata with a default, so a producer that omits it is
    /// describing an `f32` block rather than an undecodable one.
    #[test]
    fn a_block_with_no_dtype_on_the_wire_reads_as_f32() {
        let wire_bytes = wire_bytes_for(vec![
            ("samples", rmpv::Value::Binary(interleaved_f32_bytes(&[1.0]))),
            ("sample_rate", rmpv::Value::from(8_000)),
            ("channels", rmpv::Value::from(1)),
            ("sample_count", rmpv::Value::from(1)),
            ("first_sample_timestamp_ns", rmpv::Value::from(0)),
        ]);

        let block: AudioBlock = rmp_serde::from_slice(&wire_bytes).expect("dtype defaults");
        assert_eq!(block.dtype, AudioSampleDtype::F32);
    }

    fn wire_bytes_for(entries: Vec<(&str, rmpv::Value)>) -> Vec<u8> {
        let map = rmpv::Value::Map(
            entries
                .into_iter()
                .map(|(key, value)| (rmpv::Value::from(key), value))
                .collect(),
        );
        let mut wire_bytes = Vec::new();
        rmpv::encode::write_value(&mut wire_bytes, &map).expect("msgpack encode");
        wire_bytes
    }
}
