// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A page's audio as the house audio block: CEF's planar float packets,
//! interleaved and stamped on one unbroken timeline per stream.

/// One audio stream the page opened, from its first packet's arrival on.
#[derive(Debug)]
pub struct PageAudioStreamTimeline {
    sample_rate: u32,
    channels: u32,
    first_sample_timestamp_ns: Option<i64>,
    samples_published: u64,
}

impl PageAudioStreamTimeline {
    pub fn new(sample_rate: u32, channels: u32) -> Self {
        Self {
            sample_rate,
            channels,
            first_sample_timestamp_ns: None,
            samples_published: 0,
        }
    }

    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// The monotonic instant of the next packet's first sample: the stream's
    /// first packet is stamped `arrived_at_ns`, and every later one where the
    /// samples before it end, so blocks tile the timeline without gaps.
    pub fn stamp_the_next_packet(&mut self, sample_count: u32, arrived_at_ns: i64) -> i64 {
        let stream_started_at_ns = *self.first_sample_timestamp_ns.get_or_insert(arrived_at_ns);
        let offset_ns =
            self.samples_published as u128 * 1_000_000_000 / u128::from(self.sample_rate);
        self.samples_published += u64::from(sample_count);
        stream_started_at_ns.saturating_add(i64::try_from(offset_ns).unwrap_or(i64::MAX))
    }

    /// The audio-block message one packet is published as
    /// (`docs/plan/ARCHITECTURE.md` §Media I/O): `samples` as little-endian
    /// interleaved `f32`, then the stream's rate, channel count, the
    /// per-channel sample count, `dtype` and the first sample's instant.
    pub fn audio_block_message_payload(
        &self,
        interleaved_samples: &[f32],
        sample_count: u32,
        first_sample_timestamp_ns: i64,
    ) -> Vec<u8> {
        let write_failure = "writing msgpack into a Vec cannot fail";
        let mut payload = Vec::with_capacity(interleaved_samples.len() * 4 + 128);
        rmp::encode::write_map_len(&mut payload, 6).expect(write_failure);
        rmp::encode::write_str(&mut payload, "samples").expect(write_failure);
        rmp::encode::write_bin_len(&mut payload, (interleaved_samples.len() * 4) as u32)
            .expect(write_failure);
        for sample in interleaved_samples {
            payload.extend_from_slice(&sample.to_le_bytes());
        }
        rmp::encode::write_str(&mut payload, "sample_rate").expect(write_failure);
        rmp::encode::write_uint(&mut payload, u64::from(self.sample_rate)).expect(write_failure);
        rmp::encode::write_str(&mut payload, "channels").expect(write_failure);
        rmp::encode::write_uint(&mut payload, u64::from(self.channels)).expect(write_failure);
        rmp::encode::write_str(&mut payload, "sample_count").expect(write_failure);
        rmp::encode::write_uint(&mut payload, u64::from(sample_count)).expect(write_failure);
        rmp::encode::write_str(&mut payload, "dtype").expect(write_failure);
        rmp::encode::write_str(&mut payload, "f32").expect(write_failure);
        rmp::encode::write_str(&mut payload, "first_sample_timestamp_ns").expect(write_failure);
        rmp::encode::write_sint(&mut payload, first_sample_timestamp_ns).expect(write_failure);
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_tile_the_timeline_from_the_first_arrival_whatever_later_ones_arrive_at() {
        let mut timeline = PageAudioStreamTimeline::new(48_000, 2);
        assert_eq!(timeline.stamp_the_next_packet(480, 1_000), 1_000);
        assert_eq!(
            timeline.stamp_the_next_packet(480, 99_999_999),
            1_000 + 10_000_000
        );
        assert_eq!(timeline.stamp_the_next_packet(240, 5), 1_000 + 20_000_000);
        assert_eq!(timeline.stamp_the_next_packet(1, 5), 1_000 + 25_000_000);
    }

    #[test]
    fn a_block_carries_the_wire_maps_keys_with_samples_as_binary() {
        let timeline = PageAudioStreamTimeline::new(48_000, 2);
        let payload = timeline.audio_block_message_payload(&[0.25, -0.5], 1, 77);
        let decoded = rmpv::decode::read_value(&mut payload.as_slice()).unwrap();
        let entries = decoded.as_map().expect("a named map");
        let entry = |key: &str| {
            entries
                .iter()
                .find(|(entry_key, _)| entry_key.as_str() == Some(key))
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("no {key:?}"))
        };
        assert_eq!(entries.len(), 6);
        let samples: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(entry("samples"), rmpv::Value::Binary(samples));
        assert_eq!(entry("sample_rate").as_u64(), Some(48_000));
        assert_eq!(entry("channels").as_u64(), Some(2));
        assert_eq!(entry("sample_count").as_u64(), Some(1));
        assert_eq!(entry("dtype").as_str(), Some("f32"));
        assert_eq!(entry("first_sample_timestamp_ns").as_i64(), Some(77));
    }
}
