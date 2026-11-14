// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A page's audio as Channels blocks: CEF's float packets, planar f32, placed
//! on one unbroken timeline per stream.

use tatolab_module_sdk::{ModuleHostChannelsBlock, ModuleHostChannelsBlockSampleType};

/// One audio stream the page opened, from its first packet's arrival on.
#[derive(Debug)]
pub struct PageAudioStreamTimeline {
    sample_rate: u32,
    channels: u32,
    first_sample_timestamp_ns: Option<i64>,
    samples_published: u64,
}

/// Where one packet sits on its stream's timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageAudioPacketPlacement {
    /// The stream-wide index of the packet's first sample.
    pub start_sample: u64,
    /// The monotonic instant of the packet's first sample.
    pub first_sample_monotonic_ns: i64,
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

    /// Place the next packet: the stream's first packet is stamped
    /// `arrived_at_ns`, and every later one where the samples before it end,
    /// so blocks tile the timeline without gaps.
    pub fn place_the_next_packet(
        &mut self,
        sample_count: u32,
        arrived_at_ns: i64,
    ) -> PageAudioPacketPlacement {
        let stream_started_at_ns = *self.first_sample_timestamp_ns.get_or_insert(arrived_at_ns);
        let start_sample = self.samples_published;
        let offset_ns = start_sample as u128 * 1_000_000_000 / u128::from(self.sample_rate);
        self.samples_published += u64::from(sample_count);
        PageAudioPacketPlacement {
            start_sample,
            first_sample_monotonic_ns: stream_started_at_ns
                .saturating_add(i64::try_from(offset_ns).unwrap_or(i64::MAX)),
        }
    }

    /// The Channels block a packet of `sample_count` frames placed at
    /// `start_sample` is published as, in planar f32.
    pub fn channels_block_of_a_packet(
        &self,
        start_sample: u64,
        sample_count: u32,
    ) -> ModuleHostChannelsBlock {
        ModuleHostChannelsBlock {
            start_sample,
            sample_rate: self.sample_rate,
            channel_count: self.channels,
            samples_per_channel: sample_count,
            sample_type: ModuleHostChannelsBlockSampleType::F32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_tile_the_timeline_from_the_first_arrival_whatever_later_ones_arrive_at() {
        let mut timeline = PageAudioStreamTimeline::new(48_000, 2);
        let placements: Vec<(u64, i64)> = [(480, 1_000), (480, 99_999_999), (240, 5), (1, 5)]
            .into_iter()
            .map(|(sample_count, arrived_at_ns)| {
                let placement = timeline.place_the_next_packet(sample_count, arrived_at_ns);
                (placement.start_sample, placement.first_sample_monotonic_ns)
            })
            .collect();
        assert_eq!(
            placements,
            [
                (0, 1_000),
                (480, 1_000 + 10_000_000),
                (960, 1_000 + 20_000_000),
                (1_200, 1_000 + 25_000_000),
            ]
        );
    }

    #[test]
    fn a_packet_is_a_planar_f32_block_of_the_streams_rate_and_channels() {
        let timeline = PageAudioStreamTimeline::new(48_000, 2);
        let block = timeline.channels_block_of_a_packet(960, 1);
        assert_eq!(
            block,
            ModuleHostChannelsBlock {
                start_sample: 960,
                sample_rate: 48_000,
                channel_count: 2,
                samples_per_channel: 1,
                sample_type: ModuleHostChannelsBlockSampleType::F32,
            }
        );
        let planar_payload =
            ModuleHostChannelsBlock::planar_f32_payload_of_interleaved_samples(&[0.25, -0.5], 2);
        assert_eq!(block.planar_payload_byte_len(), Some(planar_payload.len()));
    }
}
