// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A JSON value written onto a StudioView output, as the payload of the output's family.
//!
//! An Array value is a number, a rectangular list of numbers, or an object of such, each an
//! attribute by its key; a bare number or list is the attribute `value`. Every attribute is
//! `float64`, and a bare number is of shape `[1]`. A Channels value is `{"sample_rate", "channels"}`,
//! each channel a list of samples of one length, written as `float32`. A Records value is an
//! object, sent as a MessagePack map.

use serde_json::Value;

use crate::core::descriptors::PortFamily;
use crate::core::error::{Error, Result};
use crate::iceoryx2::{
    ArrayFrameCpuAttributeToPack, ChannelsBlockDescriptorBody, ChannelsBlockSampleType,
    DataChannelPayloadDescriptor, array_frame_payload_of_cpu_attributes,
};

/// The attribute a bare number or list written onto an Array output is.
pub(super) const ARRAY_VALUE_ATTRIBUTE_NAME: &str = "value";

/// The payload a value written onto an output becomes, with the samples per channel it carries.
pub(super) struct StudioViewOutputPayload {
    pub(super) payload_descriptor: DataChannelPayloadDescriptor,
    pub(super) payload: Vec<u8>,
    pub(super) samples_per_channel: u64,
}

/// `value` as the payload of `family`, a Channels block starting at `start_sample`; refused
/// naming `port` and what the family takes.
pub(super) fn the_payload_of_a_value_written_onto(
    port: &str,
    family: PortFamily,
    value: &Value,
    start_sample: u64,
) -> Result<StudioViewOutputPayload> {
    let refused =
        |why: String| Error::Configuration(format!("output `{port}` ({family:?}): {why}"));
    match family {
        PortFamily::Array => the_array_frame_of(value).map_err(refused),
        PortFamily::Channels => the_channels_block_of(value, start_sample).map_err(refused),
        PortFamily::Records => the_records_message_of(value).map_err(refused),
        PortFamily::Image | PortFamily::Packets => Err(refused(format!(
            "a written value is Array, Channels or Records; this runtime takes no {family:?} \
             value written onto a StudioView output"
        ))),
    }
}

/// One attribute of an Array value: its dimensions, outermost first, and its elements in order.
struct ArrayValueAttribute {
    name: String,
    shape: Vec<u64>,
    elements: Vec<f64>,
}

fn the_shape_and_elements_of(
    value: &Value,
    shape: &mut Vec<u64>,
    depth: usize,
    elements: &mut Vec<f64>,
) -> std::result::Result<(), String> {
    match value {
        Value::Number(number) => {
            if depth != shape.len() {
                return Err("a list of numbers is rectangular: every row is as deep".into());
            }
            elements.push(number.as_f64().ok_or("a number is a 64-bit float")?);
            Ok(())
        }
        Value::Array(items) => {
            match shape.get(depth) {
                Some(&length) if length != items.len() as u64 => {
                    return Err("a list of numbers is rectangular: every row is as long".into());
                }
                Some(_) => {}
                None if depth == shape.len() && elements.is_empty() => {
                    shape.push(items.len() as u64)
                }
                None => {
                    return Err("a list of numbers is rectangular: every row is as deep".into());
                }
            }
            for item in items {
                the_shape_and_elements_of(item, shape, depth + 1, elements)?;
            }
            Ok(())
        }
        _ => Err("an attribute is a number or a list of numbers".into()),
    }
}

fn the_array_attribute(
    name: &str,
    value: &Value,
) -> std::result::Result<ArrayValueAttribute, String> {
    let mut shape = Vec::new();
    let mut elements = Vec::new();
    the_shape_and_elements_of(value, &mut shape, 0, &mut elements)
        .map_err(|why| format!("attribute `{name}`: {why}"))?;
    if shape.is_empty() {
        shape.push(1);
    }
    Ok(ArrayValueAttribute {
        name: name.to_string(),
        shape,
        elements,
    })
}

fn the_array_frame_of(value: &Value) -> std::result::Result<StudioViewOutputPayload, String> {
    let attributes = match value {
        Value::Object(attributes) if attributes.is_empty() => {
            return Err("an Array value names at least one attribute".into());
        }
        Value::Object(attributes) => attributes
            .iter()
            .map(|(name, attribute)| the_array_attribute(name, attribute))
            .collect::<std::result::Result<Vec<_>, _>>()?,
        _ => vec![the_array_attribute(ARRAY_VALUE_ATTRIBUTE_NAME, value)?],
    };
    let data_of_each_attribute: Vec<Vec<u8>> = attributes
        .iter()
        .map(|attribute| {
            attribute
                .elements
                .iter()
                .flat_map(|element| element.to_le_bytes())
                .collect()
        })
        .collect();
    let to_pack: Vec<ArrayFrameCpuAttributeToPack<'_>> = attributes
        .iter()
        .zip(&data_of_each_attribute)
        .map(|(attribute, data)| ArrayFrameCpuAttributeToPack {
            name: &attribute.name,
            dtype: ChannelsBlockSampleType::F64.array_dtype_wire_name(),
            shape: &attribute.shape,
            data,
        })
        .collect();
    let (body, payload) =
        array_frame_payload_of_cpu_attributes(&to_pack).map_err(|refusal| refusal.to_string())?;
    Ok(StudioViewOutputPayload {
        payload_descriptor: DataChannelPayloadDescriptor::array_frame(body),
        payload,
        samples_per_channel: 0,
    })
}

fn the_channels_block_of(
    value: &Value,
    start_sample: u64,
) -> std::result::Result<StudioViewOutputPayload, String> {
    let sample_rate = value
        .get("sample_rate")
        .and_then(Value::as_u64)
        .and_then(|sample_rate| u32::try_from(sample_rate).ok())
        .filter(|sample_rate| *sample_rate > 0)
        .ok_or("a Channels value states its `sample_rate`, a whole number of samples a second")?;
    let channels = value
        .get("channels")
        .and_then(Value::as_array)
        .filter(|channels| !channels.is_empty())
        .ok_or("a Channels value holds `channels`, a list of at least one channel")?;
    let mut samples_per_channel = None;
    let mut planar_payload = Vec::new();
    for (channel_index, channel) in channels.iter().enumerate() {
        let samples = channel
            .as_array()
            .ok_or_else(|| format!("channel {channel_index} is a list of samples"))?;
        if *samples_per_channel.get_or_insert(samples.len()) != samples.len() {
            return Err("every channel holds as many samples".into());
        }
        for sample in samples {
            let sample = sample.as_f64().ok_or_else(|| {
                format!("channel {channel_index} holds a sample that is no number")
            })?;
            planar_payload.extend_from_slice(&(sample as f32).to_le_bytes());
        }
    }
    let samples_per_channel = samples_per_channel.unwrap_or_default();
    Ok(StudioViewOutputPayload {
        payload_descriptor: DataChannelPayloadDescriptor::channels_block(
            ChannelsBlockDescriptorBody {
                start_sample,
                sample_rate,
                channel_count: channels.len() as u32,
                samples_per_channel: u32::try_from(samples_per_channel)
                    .map_err(|_| "a channel holds more samples than a block carries")?,
                sample_type: ChannelsBlockSampleType::F32 as u32,
            },
        ),
        payload: planar_payload,
        samples_per_channel: samples_per_channel as u64,
    })
}

fn the_records_message_of(value: &Value) -> std::result::Result<StudioViewOutputPayload, String> {
    if !value.is_object() {
        return Err("a Records value is an object of fields".into());
    }
    let payload = rmp_serde::to_vec_named(value).map_err(|refusal| refusal.to_string())?;
    Ok(StudioViewOutputPayload {
        payload_descriptor: DataChannelPayloadDescriptor::records_message(),
        payload,
        samples_per_channel: 0,
    })
}
