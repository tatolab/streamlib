// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in `ArrayToChannels`: one `[channels, samples]` attribute of each
//! Array frame as one Channels block.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody,
    ChannelsBlockSampleType, DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    array_frame_attribute_table_of,
};
use streamlib::sdk::processors::ReactiveProcessor;
use streamlib::sdk::schemars::JsonSchema;

use super::channels_to_array::CHANNELS_TO_ARRAY_SAMPLES_ATTRIBUTE_NAME;
use super::conversion_node_config_defaults::{
    forty_eight_thousand_samples_per_second, one_channel_named_value,
};
use super::conversion_node_config_refusals::refuse_a_channels_output_config;
use super::port_family_messages_read_by_a_conversion::refuse_unless_the_descriptor_is;

const ARRAY_INPUT_PORT: &str = "array";
const CHANNELS_OUTPUT_PORT: &str = "channels";

/// Configuration for [`ArrayToChannels`].
#[streamlib::sdk::processor_config]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct ArrayToChannelsConfig {
    #[serde(default = "one_channel_named_value")]
    #[description = "The names of the channels `channels` carries, one per row of the attribute."]
    pub channel_names: Vec<String>,
    #[serde(default = "forty_eight_thousand_samples_per_second")]
    #[description = "The sample rate `channels` states, in samples per second per channel."]
    pub sample_rate: u32,
    #[serde(default = "default_samples_attribute_name")]
    #[description = "The attribute of each Array frame read as the block's samples."]
    pub attribute: String,
}

fn default_samples_attribute_name() -> String {
    CHANNELS_TO_ARRAY_SAMPLES_ATTRIBUTE_NAME.to_string()
}

impl Default for ArrayToChannelsConfig {
    fn default() -> Self {
        Self {
            channel_names: one_channel_named_value(),
            sample_rate: forty_eight_thousand_samples_per_second(),
            attribute: default_samples_attribute_name(),
        }
    }
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Converts one attribute of each Array frame on `array`, `attribute` \
                  (`samples` by default), into one Channels block on `channels`. The attribute \
                  is a CPU `[channels, samples]` tensor of `float32` or `float64`, one row per \
                  name in `channel_names`, and its samples are copied once into the block. The \
                  blocks are stamped at `sample_rate`, their start samples counted from the \
                  first block. A frame without the attribute, or whose attribute is GPU-resident, \
                  of another dtype or of another shape, is refused and writes nothing.",
    execution = reactive,
    config = crate::port_family_conversion::array_to_channels::ArrayToChannelsConfig,
    input(
        "array",
        description = "The Array frames whose `attribute` is converted",
        delivery_profile = "ordered",
        family = Array
    ),
    output(
        "channels",
        description = "One Channels block per frame, one channel per name in `channel_names`",
        family = Channels,
        names = from_config("channel_names"),
        rate = from_config("sample_rate")
    ),
)]
pub struct ArrayToChannels {
    next_start_sample: u64,
}

impl ReactiveProcessor for ArrayToChannels::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        refuse_a_channels_output_config(
            "ArrayToChannels",
            &self.config.channel_names,
            self.config.sample_rate,
        )
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw(ARRAY_INPUT_PORT)? {
            let (payload_descriptor, payload) = match channels_block_of_the_array_frame(
                &self.config,
                self.next_start_sample,
                message.payload_descriptor,
                message.payload.bytes(),
            ) {
                Ok(block) => block,
                Err(refusal) => {
                    tracing::warn!(%refusal, "ArrayToChannels wrote nothing for a message");
                    continue;
                }
            };
            let block_body =
                ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body);
            self.next_start_sample += u64::from(block_body.samples_per_channel);
            self.outputs.write_bytes_described_by(
                CHANNELS_OUTPUT_PORT,
                &payload,
                message.timestamp_ns,
                message.group_boundary,
                payload_descriptor,
            )?;
        }
        Ok(())
    }
}

/// The Channels block holding `config.attribute` of the Array frame
/// `payload_descriptor` and `payload` carry, starting at `start_sample`.
pub fn channels_block_of_the_array_frame(
    config: &ArrayToChannelsConfig,
    start_sample: u64,
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
) -> Result<(DataChannelPayloadDescriptor, Vec<u8>)> {
    refuse_unless_the_descriptor_is(
        "ArrayToChannels",
        ARRAY_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ArrayFrame,
    )?;
    let attribute_table = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
        payload,
    )
    .map_err(|refusal| Error::PortError(format!("ArrayToChannels: {refusal}")))?;
    let attribute_name = config.attribute.as_str();
    let attribute = attribute_table
        .iter()
        .find(|row| row.name == attribute_name)
        .ok_or_else(|| {
            Error::PortError(format!(
                "ArrayToChannels: an Array frame on `array` has no attribute {attribute_name:?}; \
                 it has {:?}",
                attribute_table
                    .iter()
                    .map(|row| row.name.as_str())
                    .collect::<Vec<_>>()
            ))
        })?;
    let sample_type = ChannelsBlockSampleType::from_array_dtype_wire_name(&attribute.dtype)
        .ok_or_else(|| {
            Error::PortError(format!(
                "ArrayToChannels: attribute {attribute_name:?} is {}; a Channels block carries \
                 float32 or float64",
                attribute.dtype
            ))
        })?;
    let channel_count = config.channel_names.len() as u64;
    let [attribute_channel_count, samples_per_channel] = attribute.shape[..] else {
        return Err(Error::PortError(format!(
            "ArrayToChannels: attribute {attribute_name:?} has shape {:?}; it is read as \
             [channels, samples]",
            attribute.shape
        )));
    };
    if attribute_channel_count != channel_count {
        return Err(Error::PortError(format!(
            "ArrayToChannels: attribute {attribute_name:?} holds {attribute_channel_count} \
             channels, and `channel_names` names {channel_count}"
        )));
    }
    let samples_per_channel = u32::try_from(samples_per_channel).map_err(|_| {
        Error::PortError(format!(
            "ArrayToChannels: attribute {attribute_name:?} holds {samples_per_channel} samples \
             per channel, past what one Channels block states"
        ))
    })?;
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = attribute.residency
    else {
        return Err(Error::PortError(format!(
            "ArrayToChannels: attribute {attribute_name:?} is GPU-resident; this node reads a \
             CPU attribute"
        )));
    };
    let block_body = ChannelsBlockDescriptorBody {
        start_sample,
        sample_rate: config.sample_rate,
        channel_count: channel_count as u32,
        samples_per_channel,
        sample_type: sample_type as u32,
    };
    if block_body.planar_payload_byte_len() != Some(byte_len as usize) {
        return Err(Error::PortError(format!(
            "ArrayToChannels: attribute {attribute_name:?} states {byte_len} bytes, which is \
             not its shape's"
        )));
    }
    let attribute_data =
        &payload[payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize];
    Ok((
        DataChannelPayloadDescriptor::channels_block(block_body),
        attribute_data.to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use streamlib::sdk::iceoryx2::{
        ArrayFrameAttributeRow, ArrayFrameCpuAttributeToPack, GpuPoolSlotFrameDescriptorBody,
        array_frame_payload_of_cpu_attributes,
    };

    use super::*;
    use crate::port_family_conversion::channels_to_array::array_frame_of_the_channels_block;

    fn stereo_at_48_khz() -> ArrayToChannelsConfig {
        ArrayToChannelsConfig {
            channel_names: vec!["left".into(), "right".into()],
            sample_rate: 48_000,
            attribute: "samples".into(),
        }
    }

    fn array_frame_of(
        attributes: &[ArrayFrameCpuAttributeToPack<'_>],
    ) -> (DataChannelPayloadDescriptor, Vec<u8>) {
        let (body, payload) = array_frame_payload_of_cpu_attributes(attributes).unwrap();
        (DataChannelPayloadDescriptor::array_frame(body), payload)
    }

    #[test]
    fn a_channels_block_converted_to_an_array_and_back_keeps_its_samples() {
        let samples: Vec<u8> = [0.25f64, 0.5, -0.25, -0.5]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let block_descriptor =
            DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
                start_sample: 0,
                sample_rate: 48_000,
                channel_count: 2,
                samples_per_channel: 2,
                sample_type: ChannelsBlockSampleType::F64 as u32,
            });
        let (array_descriptor, array_payload) =
            array_frame_of_the_channels_block(block_descriptor, &samples).unwrap();

        let (block_descriptor_back, samples_back) = channels_block_of_the_array_frame(
            &stereo_at_48_khz(),
            480,
            array_descriptor,
            &array_payload,
        )
        .unwrap();

        assert_eq!(samples_back, samples);
        assert_eq!(
            ChannelsBlockDescriptorBody::from_descriptor_body(&block_descriptor_back.body),
            ChannelsBlockDescriptorBody {
                start_sample: 480,
                sample_rate: 48_000,
                channel_count: 2,
                samples_per_channel: 2,
                sample_type: ChannelsBlockSampleType::F64 as u32,
            }
        );
    }

    #[test]
    fn the_configured_attribute_is_the_one_read() {
        let gains = [1.0f32, 2.0, 3.0, 4.0];
        let gain_bytes: Vec<u8> = gains.iter().flat_map(|gain| gain.to_le_bytes()).collect();
        let (descriptor, payload) = array_frame_of(&[
            ArrayFrameCpuAttributeToPack {
                name: "labels",
                dtype: "uint8",
                shape: &[2],
                data: &[1, 2],
            },
            ArrayFrameCpuAttributeToPack {
                name: "gains",
                dtype: "float32",
                shape: &[2, 2],
                data: &gain_bytes,
            },
        ]);
        let config = ArrayToChannelsConfig {
            attribute: "gains".into(),
            ..stereo_at_48_khz()
        };

        let (_, samples) =
            channels_block_of_the_array_frame(&config, 0, descriptor, &payload).unwrap();

        assert_eq!(samples, gain_bytes);
    }

    #[test]
    fn a_frame_without_the_attribute_is_refused_naming_the_attributes_it_has() {
        let (descriptor, payload) = array_frame_of(&[ArrayFrameCpuAttributeToPack {
            name: "boxes",
            dtype: "float32",
            shape: &[2, 1],
            data: &[0; 8],
        }]);

        let refusal =
            channels_block_of_the_array_frame(&stereo_at_48_khz(), 0, descriptor, &payload)
                .unwrap_err()
                .to_string();

        assert!(refusal.contains("\"samples\""), "{refusal}");
        assert!(refusal.contains("\"boxes\""), "{refusal}");
    }

    #[test]
    fn an_attribute_of_another_dtype_or_channel_count_is_refused() {
        let (int_descriptor, int_payload) = array_frame_of(&[ArrayFrameCpuAttributeToPack {
            name: "samples",
            dtype: "int32",
            shape: &[2, 1],
            data: &[0; 8],
        }]);
        let (mono_descriptor, mono_payload) = array_frame_of(&[ArrayFrameCpuAttributeToPack {
            name: "samples",
            dtype: "float32",
            shape: &[1, 2],
            data: &[0; 8],
        }]);

        let dtype_refusal =
            channels_block_of_the_array_frame(&stereo_at_48_khz(), 0, int_descriptor, &int_payload)
                .unwrap_err()
                .to_string();
        let channel_count_refusal = channels_block_of_the_array_frame(
            &stereo_at_48_khz(),
            0,
            mono_descriptor,
            &mono_payload,
        )
        .unwrap_err()
        .to_string();

        assert!(dtype_refusal.contains("int32"), "{dtype_refusal}");
        assert!(
            channel_count_refusal.contains("holds 1 channels"),
            "{channel_count_refusal}"
        );
    }

    #[test]
    fn a_gpu_resident_attribute_is_refused() {
        let row = ArrayFrameAttributeRow {
            name: "samples".into(),
            dtype: "float32".into(),
            shape: vec![2, 4],
            residency: ArrayFrameAttributeResidency::GpuPoolSlot(GpuPoolSlotFrameDescriptorBody {
                pool: [7; 16],
                slot: 0,
                generation: 1,
                timeline_value: 0,
                image_layout: 0,
            }),
        };
        let payload = row.to_row_bytes().unwrap().to_vec();
        let descriptor = DataChannelPayloadDescriptor::array_frame(ArrayFrameDescriptorBody {
            attribute_count: 1,
        });

        let refusal =
            channels_block_of_the_array_frame(&stereo_at_48_khz(), 0, descriptor, &payload)
                .unwrap_err()
                .to_string();

        assert!(refusal.contains("GPU-resident"), "{refusal}");
    }
}
