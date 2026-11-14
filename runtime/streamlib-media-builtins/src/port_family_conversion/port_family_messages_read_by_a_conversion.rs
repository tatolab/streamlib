// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Each family's message as a conversion node reads it off its input, refused
//! by name when its descriptor is not the family's.

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind,
};

/// A Channels block read off a message: its descriptor body, its sample type,
/// and its planar payload.
pub(crate) struct ChannelsBlockReadByAConversion<'payload> {
    pub(crate) body: ChannelsBlockDescriptorBody,
    pub(crate) sample_type: ChannelsBlockSampleType,
    pub(crate) planar_payload: &'payload [u8],
}

impl ChannelsBlockReadByAConversion<'_> {
    /// The last sample of channel `channel_index`, widened to `f64`; the
    /// block holds at least one sample per channel.
    pub(crate) fn last_sample_of_channel_as_f64(&self, channel_index: usize) -> f64 {
        let sample_byte_len = self.sample_type.sample_byte_len();
        let channel_run_end =
            (channel_index + 1) * self.body.samples_per_channel as usize * sample_byte_len;
        let last_sample = &self.planar_payload[channel_run_end - sample_byte_len..channel_run_end];
        match self.sample_type {
            ChannelsBlockSampleType::F32 => {
                let mut sample = [0u8; 4];
                sample.copy_from_slice(last_sample);
                f64::from(f32::from_le_bytes(sample))
            }
            ChannelsBlockSampleType::F64 => {
                let mut sample = [0u8; 8];
                sample.copy_from_slice(last_sample);
                f64::from_le_bytes(sample)
            }
        }
    }
}

/// The Channels block `payload_descriptor` and `payload` carry, refused naming
/// `node_type_name` and `input_port` when the descriptor is not a Channels
/// block's or the payload is not the length its body states.
pub(crate) fn channels_block_read_by_a_conversion<'payload>(
    node_type_name: &str,
    input_port: &str,
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &'payload [u8],
) -> Result<ChannelsBlockReadByAConversion<'payload>> {
    refuse_unless_the_descriptor_is(
        node_type_name,
        input_port,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::ChannelsBlock,
    )?;
    let body = ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body);
    let sample_type = ChannelsBlockSampleType::from_wire(body.sample_type).ok_or_else(|| {
        Error::PortError(format!(
            "{node_type_name}: a Channels block on `{input_port}` states sample type {}, which \
             is neither f32 (0) nor f64 (1)",
            body.sample_type
        ))
    })?;
    if body.planar_payload_byte_len() != Some(payload.len()) {
        return Err(Error::PortError(format!(
            "{node_type_name}: a Channels block on `{input_port}` states {} channels of {} \
             samples, which its {}-byte payload does not hold",
            body.channel_count,
            body.samples_per_channel,
            payload.len()
        )));
    }
    Ok(ChannelsBlockReadByAConversion {
        body,
        sample_type,
        planar_payload: payload,
    })
}

/// Refuse a message whose descriptor is not of `expected_kind`, naming both.
pub(crate) fn refuse_unless_the_descriptor_is(
    node_type_name: &str,
    input_port: &str,
    payload_descriptor: DataChannelPayloadDescriptor,
    expected_kind: DataChannelPayloadDescriptorKind,
) -> Result<()> {
    if payload_descriptor.kind == expected_kind as u32 {
        return Ok(());
    }
    let carried_kind = DataChannelPayloadDescriptorKind::from_wire(payload_descriptor.kind)
        .map_or_else(
            || format!("unknown kind {}", payload_descriptor.kind),
            |kind| format!("{kind:?}"),
        );
    Err(Error::PortError(format!(
        "{node_type_name}: a message on `{input_port}` carries a {carried_kind} descriptor; \
         `{input_port}` reads {expected_kind:?}"
    )))
}
