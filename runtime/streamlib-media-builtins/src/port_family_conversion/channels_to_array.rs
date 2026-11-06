// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in `ChannelsToArray`: each Channels block as one Array frame holding
//! its samples as a `[channels, samples]` attribute.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameCpuAttributeToPack, DataChannelPayloadDescriptor,
    array_frame_payload_of_cpu_attributes,
};
use streamlib::sdk::processors::ReactiveProcessor;

use super::port_family_messages_read_by_a_conversion::channels_block_read_by_a_conversion;

const CHANNELS_INPUT_PORT: &str = "channels";
const ARRAY_OUTPUT_PORT: &str = "array";

/// The one attribute each Array frame `ChannelsToArray` writes holds.
pub const CHANNELS_TO_ARRAY_SAMPLES_ATTRIBUTE_NAME: &str = "samples";

#[streamlib::sdk::processor(
    built_in_node,
    description = "Converts each Channels block on `channels` into one Array frame on `array`, \
                  whose one CPU attribute `samples` holds the block's samples as a `[channels, \
                  samples]` tensor of the block's sample type, `float32` or `float64`. The \
                  samples are copied once, from the block into the frame. The block's sample \
                  rate, start sample and channel names are not carried: an Array frame states \
                  none of them.",
    execution = reactive,
    input(
        "channels",
        description = "The Channels blocks to convert",
        delivery_profile = "ordered",
        family = Channels
    ),
    output(
        "array",
        description = "One Array frame per block, its samples as the `samples` attribute",
        family = Array,
        names = ["samples"],
        residency = "cpu"
    ),
)]
pub struct ChannelsToArray;

impl ReactiveProcessor for ChannelsToArray::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw(CHANNELS_INPUT_PORT)? {
            match array_frame_of_the_channels_block(
                message.payload_descriptor,
                message.payload.bytes(),
            ) {
                Ok((payload_descriptor, payload)) => self.outputs.write_bytes_described_by(
                    ARRAY_OUTPUT_PORT,
                    &payload,
                    message.timestamp_ns,
                    message.group_boundary,
                    payload_descriptor,
                )?,
                Err(refusal) => {
                    tracing::warn!(%refusal, "ChannelsToArray wrote nothing for a message")
                }
            }
        }
        Ok(())
    }
}

/// The Array frame holding the Channels block `payload_descriptor` and
/// `payload` carry, as its `samples` attribute.
pub fn array_frame_of_the_channels_block(
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
) -> Result<(DataChannelPayloadDescriptor, Vec<u8>)> {
    let block = channels_block_read_by_a_conversion(
        "ChannelsToArray",
        CHANNELS_INPUT_PORT,
        payload_descriptor,
        payload,
    )?;
    let dtype = block.sample_type.array_dtype_wire_name();
    let shape = [
        u64::from(block.body.channel_count),
        u64::from(block.body.samples_per_channel),
    ];
    let (array_body, array_payload) =
        array_frame_payload_of_cpu_attributes(&[ArrayFrameCpuAttributeToPack {
            name: CHANNELS_TO_ARRAY_SAMPLES_ATTRIBUTE_NAME,
            dtype,
            shape: &shape,
            data: block.planar_payload,
        }])
        .map_err(|refusal| Error::PortError(format!("ChannelsToArray: {refusal}")))?;
    Ok((
        DataChannelPayloadDescriptor::array_frame(array_body),
        array_payload,
    ))
}

#[cfg(test)]
mod tests {
    use streamlib::sdk::iceoryx2::{
        ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody,
        ChannelsBlockSampleType, array_frame_attribute_table_of,
    };

    use super::*;

    fn two_channel_f32_block_of_three_samples() -> (DataChannelPayloadDescriptor, Vec<u8>) {
        let samples: [f32; 6] = [0.5, 1.0, 1.5, -0.5, -1.0, -1.5];
        (
            DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
                start_sample: 960,
                sample_rate: 48_000,
                channel_count: 2,
                samples_per_channel: 3,
                sample_type: ChannelsBlockSampleType::F32 as u32,
            }),
            samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
        )
    }

    #[test]
    fn a_channels_block_becomes_one_samples_attribute_of_channels_by_samples() {
        let (block_descriptor, block_payload) = two_channel_f32_block_of_three_samples();

        let (array_descriptor, array_payload) =
            array_frame_of_the_channels_block(block_descriptor, &block_payload).unwrap();

        let rows = array_frame_attribute_table_of(
            ArrayFrameDescriptorBody::from_descriptor_body(&array_descriptor.body),
            &array_payload,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "samples");
        assert_eq!(rows[0].dtype, "float32");
        assert_eq!(rows[0].shape, vec![2, 3]);
        let ArrayFrameAttributeResidency::Cpu {
            payload_byte_offset,
            byte_len,
        } = rows[0].residency
        else {
            panic!("a CPU attribute");
        };
        assert_eq!(
            &array_payload[payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize],
            &block_payload[..]
        );
    }

    #[test]
    fn a_message_that_is_not_a_channels_block_is_refused_naming_its_kind() {
        let refusal = array_frame_of_the_channels_block(
            DataChannelPayloadDescriptor::records_message(),
            &[0x80],
        )
        .unwrap_err()
        .to_string();

        assert!(refusal.contains("RecordsMessage"), "{refusal}");
        assert!(refusal.contains("ChannelsBlock"), "{refusal}");
    }

    #[test]
    fn a_block_whose_payload_is_short_of_its_stated_samples_is_refused() {
        let (block_descriptor, block_payload) = two_channel_f32_block_of_three_samples();

        let refusal = array_frame_of_the_channels_block(block_descriptor, &block_payload[..20])
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("20-byte payload"), "{refusal}");
    }
}
