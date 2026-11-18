// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Built-in `TensorToAudio`: one `[channels, samples]` attribute of each
//! Array frame as one Channels block.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody,
    ChannelsBlockSampleType, DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    array_frame_attribute_table_of,
};
use streamlib::sdk::nodes::ReactiveNode;
use streamlib::sdk::schemars::JsonSchema;

use super::audio_to_tensor::AUDIO_TO_TENSOR_SAMPLES_ATTRIBUTE_NAME;
use super::conversion_node_config_defaults::{
    forty_eight_thousand_samples_per_second, one_channel_named_value,
};
use super::conversion_node_config_refusals::refuse_a_channels_output_config;
use super::port_family_messages_read_by_a_conversion::refuse_unless_the_descriptor_is;

const ARRAY_INPUT_PORT: &str = "array";
const CHANNELS_OUTPUT_PORT: &str = "channels";

/// Configuration for [`TensorToAudio`].
#[streamlib::sdk::node_config]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct TensorToAudioConfig {
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
    AUDIO_TO_TENSOR_SAMPLES_ATTRIBUTE_NAME.to_string()
}

impl Default for TensorToAudioConfig {
    fn default() -> Self {
        Self {
            channel_names: one_channel_named_value(),
            sample_rate: forty_eight_thousand_samples_per_second(),
            attribute: default_samples_attribute_name(),
        }
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    config = crate::pin_type_conversion::tensor_to_audio::TensorToAudioConfig,
    input(
        "array",
        description = "The Array frames whose `attribute` is converted",
        delivery_profile = "ordered",
        pin_type = Tensor
    ),
    output(
        "channels",
        description = "One Channels block per frame, one channel per name in `channel_names`",
        pin_type = Audio,
        names = from_config("channel_names"),
        rate = from_config("sample_rate")
    ),
)]
#[meta(
    conversion = true,
    description = "Converts one attribute of each Array frame on `array`, `attribute` \
                   (`samples` by default), into one Channels block on `channels`. The attribute \
                   is a CPU `[channels, samples]` tensor of `float32` or `float64`, one row per \
                   name in `channel_names`, and its samples are copied once into the block. The \
                   blocks are stamped at `sample_rate`, their start samples counted from the \
                   first block. A frame without the attribute, or whose attribute is GPU-resident, \
                   of another dtype or of another shape, is refused and writes nothing."
)]
pub struct TensorToAudio {
    next_start_sample: u64,
}

impl ReactiveNode for TensorToAudio::Node {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        refuse_a_channels_output_config(
            "TensorToAudio",
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
                    tracing::warn!(%refusal, "TensorToAudio wrote nothing for a message");
                    continue;
                }
            };
            let samples_per_channel =
                ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body)
                    .samples_per_channel;
            self.outputs.write_bytes_described_by(
                CHANNELS_OUTPUT_PORT,
                &payload,
                message.timestamp_ns,
                message.group_boundary,
                payload_descriptor,
            )?;
            self.next_start_sample += u64::from(samples_per_channel);
        }
        Ok(())
    }
}

/// The Channels block holding `config.attribute` of the Array frame
/// `payload_descriptor` and `payload` carry, starting at `start_sample`.
pub fn channels_block_of_the_array_frame(
    config: &TensorToAudioConfig,
    start_sample: u64,
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
) -> Result<(DataChannelPayloadDescriptor, Vec<u8>)> {
    refuse_unless_the_descriptor_is(
        "TensorToAudio",
        ARRAY_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ArrayFrame,
    )?;
    let attribute_table = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
        payload,
    )
    .map_err(|refusal| Error::PortError(format!("TensorToAudio: {refusal}")))?;
    let attribute_name = config.attribute.as_str();
    let attribute = attribute_table
        .iter()
        .find(|row| row.name == attribute_name)
        .ok_or_else(|| {
            Error::PortError(format!(
                "TensorToAudio: an Array frame on `array` has no attribute {attribute_name:?}; \
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
                "TensorToAudio: attribute {attribute_name:?} is {}; a Channels block carries \
                 float32 or float64",
                attribute.dtype
            ))
        })?;
    let channel_count = config.channel_names.len() as u64;
    let [attribute_channel_count, samples_per_channel] = attribute.shape[..] else {
        return Err(Error::PortError(format!(
            "TensorToAudio: attribute {attribute_name:?} has shape {:?}; it is read as \
             [channels, samples]",
            attribute.shape
        )));
    };
    if attribute_channel_count != channel_count {
        return Err(Error::PortError(format!(
            "TensorToAudio: attribute {attribute_name:?} holds {attribute_channel_count} \
             channels, and `channel_names` names {channel_count}"
        )));
    }
    let samples_per_channel = u32::try_from(samples_per_channel).map_err(|_| {
        Error::PortError(format!(
            "TensorToAudio: attribute {attribute_name:?} holds {samples_per_channel} samples \
             per channel, past what one Channels block states"
        ))
    })?;
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = attribute.residency
    else {
        return Err(Error::PortError(format!(
            "TensorToAudio: attribute {attribute_name:?} is GPU-resident; this node reads a \
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
            "TensorToAudio: attribute {attribute_name:?} states {byte_len} bytes, which is \
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
