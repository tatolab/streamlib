// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Built-in `AudioToTensor`: each Channels block as one Array frame holding
//! its samples as a `[channels, samples]` attribute.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ArrayFrameCpuAttributeToPack, DataChannelPayloadDescriptor,
    array_frame_payload_of_cpu_attributes,
};
use streamlib::sdk::nodes::ReactiveNode;

use super::messages_read_by_a_conversion::channels_block_read_by_a_conversion;

const CHANNELS_INPUT_PORT: &str = "channels";
const ARRAY_OUTPUT_PORT: &str = "array";

/// The one attribute each Array frame `AudioToTensor` writes holds.
pub const AUDIO_TO_TENSOR_SAMPLES_ATTRIBUTE_NAME: &str = "samples";

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "channels",
        description = "The Channels blocks to convert",
        delivery_profile = "ordered",
        pin_type = Audio
    ),
    output(
        "array",
        description = "One Array frame per block, its samples as the `samples` attribute",
        pin_type = Tensor,
        residency = "cpu"
    ),
)]
#[meta(
    conversion = true,
    description = "Converts each Channels block on `channels` into one Array frame on `array`, \
                   whose one CPU attribute `samples` holds the block's samples as a `[channels, \
                   samples]` tensor of the block's sample type, `float32` or `float64`. The \
                   samples are copied once, from the block into the frame. The block's sample \
                   rate, start sample and channel names are not carried: an Array frame states \
                   none of them."
)]
pub struct AudioToTensor;

impl ReactiveNode for AudioToTensor::Node {
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
                    tracing::warn!(%refusal, "AudioToTensor wrote nothing for a message")
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
        "AudioToTensor",
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
            name: AUDIO_TO_TENSOR_SAMPLES_ATTRIBUTE_NAME,
            dtype,
            shape: &shape,
            data: block.planar_payload,
        }])
        .map_err(|refusal| Error::PortError(format!("AudioToTensor: {refusal}")))?;
    Ok((
        DataChannelPayloadDescriptor::array_frame(array_body),
        array_payload,
    ))
}
