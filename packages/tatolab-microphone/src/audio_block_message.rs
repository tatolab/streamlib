// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio-block message a captured block is published as.

use serde::Serialize;
use tatolab_module_sdk::{ModuleHostAudioSampleFormat, ModuleHostAudioStreamFormat};

/// The audio-block wire map (`docs/plan/ARCHITECTURE.md` §Media I/O): a msgpack
/// named map, `samples` as `bin`, little-endian interleaved scalars read by
/// `dtype`.
#[derive(Serialize)]
struct AudioBlockWireMap<'samples> {
    #[serde(with = "serde_bytes")]
    samples: &'samples [u8],
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    dtype: &'static str,
    first_sample_timestamp_ns: i64,
}

/// How a stream's scalar encoding is spelled on the wire.
fn wire_dtype_of(sample_format: ModuleHostAudioSampleFormat) -> &'static str {
    match sample_format {
        ModuleHostAudioSampleFormat::F32 => "f32",
        ModuleHostAudioSampleFormat::I16 => "i16",
    }
}

/// The payload one captured block is published as: the stream describes the
/// samples, and the device's own timestamp rides through untouched.
pub(crate) fn audio_block_message_payload(
    interleaved_sample_bytes: &[u8],
    sample_count: u32,
    first_sample_timestamp_ns: i64,
    stream_format: ModuleHostAudioStreamFormat,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(&AudioBlockWireMap {
        samples: interleaved_sample_bytes,
        sample_rate: stream_format.sample_rate,
        channels: stream_format.channels,
        sample_count,
        dtype: wire_dtype_of(stream_format.sample_format),
        first_sample_timestamp_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_entry<'map>(
        entries: &'map [(rmpv::Value, rmpv::Value)],
        key: &str,
    ) -> &'map rmpv::Value {
        &entries
            .iter()
            .find(|(entry_key, _)| entry_key.as_str() == Some(key))
            .unwrap_or_else(|| panic!("the wire map carries no `{key}`"))
            .1
    }

    #[test]
    fn the_payload_carries_the_wire_maps_keys_with_samples_as_binary() {
        let samples: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let payload = audio_block_message_payload(
            &samples,
            2,
            123_456_789,
            ModuleHostAudioStreamFormat {
                sample_rate: 48_000,
                channels: 1,
                sample_format: ModuleHostAudioSampleFormat::F32,
            },
        )
        .unwrap();

        let decoded = rmpv::decode::read_value(&mut payload.as_slice()).unwrap();
        let entries = decoded.as_map().expect("a named map");
        assert_eq!(entries.len(), 6);
        assert_eq!(
            wire_entry(entries, "samples"),
            &rmpv::Value::Binary(samples)
        );
        assert_eq!(wire_entry(entries, "sample_rate").as_u64(), Some(48_000));
        assert_eq!(wire_entry(entries, "channels").as_u64(), Some(1));
        assert_eq!(wire_entry(entries, "sample_count").as_u64(), Some(2));
        assert_eq!(wire_entry(entries, "dtype").as_str(), Some("f32"));
        assert_eq!(
            wire_entry(entries, "first_sample_timestamp_ns").as_i64(),
            Some(123_456_789)
        );
    }

    #[test]
    fn an_i16_stream_is_published_as_i16() {
        let payload = audio_block_message_payload(
            &[0, 0, 0, 0],
            1,
            5,
            ModuleHostAudioStreamFormat {
                sample_rate: 44_100,
                channels: 2,
                sample_format: ModuleHostAudioSampleFormat::I16,
            },
        )
        .unwrap();

        let decoded = rmpv::decode::read_value(&mut payload.as_slice()).unwrap();
        assert_eq!(
            wire_entry(decoded.as_map().unwrap(), "dtype").as_str(),
            Some("i16")
        );
    }
}
