// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Channels blocks the read-side window stage takes in and hands out.
//!
//! A block is its descriptor body — rate, channel count, samples per channel,
//! sample type, start sample — and a planar payload, each channel's run of
//! little-endian scalars after the last. The stage works in interleaved f32,
//! so a block is read frame by frame across its planes and a window is written
//! back plane by plane.

use super::super::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind,
};

/// One Channels block the stage consumed, borrowing its planar payload from
/// the sample it arrived in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChannelsBlockReadByTheWindowStage<'payload> {
    planar_sample_bytes: &'payload [u8],
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    /// Samples in each channel's run.
    pub(crate) sample_count: u32,
    sample_type: ChannelsBlockSampleType,
    /// The stamp of the block's first sample: the message's own.
    pub(crate) first_sample_timestamp_ns: i64,
}

/// Why a message could not be read as a Channels block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChannelsBlockWindowStageRefusal {
    /// The message is described as another kind.
    NotAChannelsBlock { descriptor_kind: u32 },
    /// The body states a sample type this engine does not know.
    UnknownSampleType { sample_type: u32 },
    /// The payload length does not agree with the body.
    PayloadLengthDisagreesWithTheBody {
        payload_bytes: usize,
        expected_bytes: usize,
        samples_per_channel: u32,
        channels: u32,
    },
}

impl std::fmt::Display for ChannelsBlockWindowStageRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelsBlockWindowStageRefusal::NotAChannelsBlock { descriptor_kind } => write!(
                formatter,
                "the message is described as kind {descriptor_kind}, and a windowed input \
                 reads Channels blocks only"
            ),
            ChannelsBlockWindowStageRefusal::UnknownSampleType { sample_type } => write!(
                formatter,
                "the block states sample type {sample_type}, and the stage reads f32 (0) or \
                 f64 (1)"
            ),
            ChannelsBlockWindowStageRefusal::PayloadLengthDisagreesWithTheBody {
                payload_bytes,
                expected_bytes,
                samples_per_channel,
                channels,
            } => write!(
                formatter,
                "the payload carries {payload_bytes} bytes but {samples_per_channel} samples × \
                 {channels} channels is {expected_bytes} bytes"
            ),
        }
    }
}

/// The Channels block body `payload_descriptor` states, or `None` when it
/// describes another kind.
pub(crate) fn channels_block_body_of(
    payload_descriptor: &DataChannelPayloadDescriptor,
) -> Option<ChannelsBlockDescriptorBody> {
    (DataChannelPayloadDescriptorKind::from_wire(payload_descriptor.kind)
        == Some(DataChannelPayloadDescriptorKind::ChannelsBlock))
    .then(|| ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body))
}

/// Read one message as a Channels block, refusing by name rather than
/// reshaping it.
pub(crate) fn read_a_channels_block_for_the_window_stage<'payload>(
    payload_descriptor: &DataChannelPayloadDescriptor,
    planar_payload: &'payload [u8],
    timestamp_ns: i64,
) -> Result<ChannelsBlockReadByTheWindowStage<'payload>, ChannelsBlockWindowStageRefusal> {
    let body = channels_block_body_of(payload_descriptor).ok_or(
        ChannelsBlockWindowStageRefusal::NotAChannelsBlock {
            descriptor_kind: payload_descriptor.kind,
        },
    )?;
    let sample_type = ChannelsBlockSampleType::from_wire(body.sample_type).ok_or(
        ChannelsBlockWindowStageRefusal::UnknownSampleType {
            sample_type: body.sample_type,
        },
    )?;
    let expected_bytes = body.planar_payload_byte_len().unwrap_or(usize::MAX);
    if planar_payload.len() != expected_bytes {
        return Err(
            ChannelsBlockWindowStageRefusal::PayloadLengthDisagreesWithTheBody {
                payload_bytes: planar_payload.len(),
                expected_bytes,
                samples_per_channel: body.samples_per_channel,
                channels: body.channel_count,
            },
        );
    }
    Ok(ChannelsBlockReadByTheWindowStage {
        planar_sample_bytes: planar_payload,
        sample_rate: body.sample_rate,
        channels: body.channel_count,
        sample_count: body.samples_per_channel,
        sample_type,
        first_sample_timestamp_ns: timestamp_ns,
    })
}

impl ChannelsBlockReadByTheWindowStage<'_> {
    /// The block's samples as f32, interleaved frame by frame across its
    /// planes.
    pub(crate) fn interleaved_samples_as_f32(&self) -> impl Iterator<Item = f32> + '_ {
        let sample_byte_len = self.sample_type.sample_byte_len();
        let plane_byte_len = self.sample_count as usize * sample_byte_len;
        let channels = self.channels as usize;
        (0..self.sample_count as usize).flat_map(move |frame| {
            (0..channels).map(move |channel| {
                let at = channel * plane_byte_len + frame * sample_byte_len;
                sample_as_f32(
                    self.sample_type,
                    &self.planar_sample_bytes[at..at + sample_byte_len],
                )
            })
        })
    }
}

fn sample_as_f32(sample_type: ChannelsBlockSampleType, scalar: &[u8]) -> f32 {
    match sample_type {
        ChannelsBlockSampleType::F32 => {
            f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]])
        }
        ChannelsBlockSampleType::F64 => f64::from_le_bytes([
            scalar[0], scalar[1], scalar[2], scalar[3], scalar[4], scalar[5], scalar[6], scalar[7],
        ]) as f32,
    }
}

/// One emitted window as a Channels block: its descriptor and its planar
/// payload of `sample_type` scalars, from the stage's interleaved f32 ones.
pub(crate) fn channels_block_of_one_window(
    interleaved_samples: &[f32],
    sample_rate: u32,
    channels: u32,
    samples_per_channel: u32,
    sample_type: ChannelsBlockSampleType,
    start_sample: u64,
) -> (DataChannelPayloadDescriptor, Vec<u8>) {
    let channel_count = channels as usize;
    let mut planar_payload =
        Vec::with_capacity(interleaved_samples.len() * sample_type.sample_byte_len());
    for channel in 0..channel_count {
        for scalar in interleaved_samples
            .iter()
            .skip(channel)
            .step_by(channel_count)
        {
            match sample_type {
                ChannelsBlockSampleType::F32 => {
                    planar_payload.extend_from_slice(&scalar.to_le_bytes())
                }
                ChannelsBlockSampleType::F64 => {
                    planar_payload.extend_from_slice(&f64::from(*scalar).to_le_bytes())
                }
            }
        }
    }
    (
        DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
            start_sample,
            sample_rate,
            channel_count: channels,
            samples_per_channel,
            sample_type: sample_type as u32,
        }),
        planar_payload,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_written_by_the_stage_reads_back_as_the_window_it_wrote() {
        let interleaved = [-1.0f32, -0.5, 0.0, 0.25, 0.5, 1.0];
        for sample_type in [ChannelsBlockSampleType::F32, ChannelsBlockSampleType::F64] {
            let (descriptor, payload) =
                channels_block_of_one_window(&interleaved, 48_000, 2, 3, sample_type, 960);
            let block = read_a_channels_block_for_the_window_stage(&descriptor, &payload, 77)
                .expect("a block the stage wrote reads back");
            assert_eq!(
                (block.sample_rate, block.channels, block.sample_count),
                (48_000, 2, 3)
            );
            assert_eq!(block.first_sample_timestamp_ns, 77);
            assert_eq!(
                block.interleaved_samples_as_f32().collect::<Vec<_>>(),
                interleaved
            );
            assert_eq!(
                channels_block_body_of(&descriptor).map(|body| body.start_sample),
                Some(960)
            );
        }
    }

    #[test]
    fn a_window_is_written_plane_by_plane() {
        let (_, payload) = channels_block_of_one_window(
            &[1.0, 10.0, 2.0, 20.0],
            16_000,
            2,
            2,
            ChannelsBlockSampleType::F32,
            0,
        );
        let (scalars, _) = payload.as_chunks::<4>();
        let planar: Vec<f32> = scalars
            .iter()
            .map(|scalar| f32::from_le_bytes(*scalar))
            .collect();
        assert_eq!(planar, [1.0, 2.0, 10.0, 20.0]);
    }

    #[test]
    fn a_message_of_another_kind_is_refused_naming_its_kind() {
        let refusal = read_a_channels_block_for_the_window_stage(
            &DataChannelPayloadDescriptor::bytes(),
            &[],
            0,
        )
        .expect_err("a Bytes message is no Channels block");
        assert!(
            matches!(
                refusal,
                ChannelsBlockWindowStageRefusal::NotAChannelsBlock { .. }
            ),
            "{refusal}"
        );
    }

    #[test]
    fn a_payload_that_disagrees_with_the_body_is_refused_naming_both_lengths() {
        let (descriptor, payload) =
            channels_block_of_one_window(&[0.0; 8], 16_000, 2, 4, ChannelsBlockSampleType::F32, 0);
        let refusal = read_a_channels_block_for_the_window_stage(
            &descriptor,
            &payload[..payload.len() - 4],
            0,
        )
        .expect_err("a short payload is refused");
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("28") && refusal.contains("32"),
            "{refusal}"
        );
    }
}
