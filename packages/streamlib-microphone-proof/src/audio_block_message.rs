// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio-block message, encoded exactly as the built-in `MicrophoneSource` encodes it.

use serde::Serialize;

/// The audio-block wire map (`docs/plan/ARCHITECTURE.md` §Media I/O): msgpack named map,
/// `samples` as `bin`, little-endian interleaved scalars read by `dtype`.
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

/// What one interleaved `f32` capture block is described by on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapturedF32BlockDescription {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    pub(crate) sample_count: u32,
    pub(crate) first_sample_timestamp_ns: i64,
}

/// The message payload a captured `f32` block is published as.
pub(crate) fn audio_block_message_payload_for_f32_block(
    interleaved_f32_sample_bytes: &[u8],
    description: CapturedF32BlockDescription,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(&AudioBlockWireMap {
        samples: interleaved_f32_sample_bytes,
        sample_rate: description.sample_rate,
        channels: description.channels,
        sample_count: description.sample_count,
        dtype: "f32",
        first_sample_timestamp_ns: description.first_sample_timestamp_ns,
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
    fn the_payload_carries_the_built_ins_keys_with_samples_as_binary() {
        let samples: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let payload = audio_block_message_payload_for_f32_block(
            &samples,
            CapturedF32BlockDescription {
                sample_rate: 48_000,
                channels: 1,
                sample_count: 2,
                first_sample_timestamp_ns: 123_456_789,
            },
        )
        .expect("encode");

        let decoded = rmpv::decode::read_value(&mut payload.as_slice()).expect("decode");
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
}
