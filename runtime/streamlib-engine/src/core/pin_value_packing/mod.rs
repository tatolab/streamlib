// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one packing of a pin's value into the message it travels as on a link, and back. The
//! wire's own payload kind is the only layout vocabulary: a reader of a port knows its pin type,
//! and a decoder of a message knows the message's kind. JSON is read and written only at the
//! host's edges, a value set on an input, a value a panel writes, and a value a client is shown;
//! between nodes a value travels packed.
//!
//! A number, a Boolean, a Color, a Vector, a Rectangle and a Transform each pack as a control
//! message: a Channels block of one f64 sample on each of 1, 4, n, 4 and 16 channels at rate 0,
//! which states no regular rate, its parts in the order their stored form lists them. A
//! String, a Menu, an Asset and a List of them pack as a Records message `{"value": ...}`, and
//! a List of numbers or values of parts as an Array frame of one attribute. A bundle packs as
//! one message holding each member by name, as `bundle_packing` says.

use serde::de::DeserializeOwned;
use serde_json::Value;
use streamlib_ipc_types::{
    ArrayFrameAttributeResidency, ArrayFrameCpuAttributeToPack, ArrayFrameDescriptorBody,
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES,
    array_frame_attribute_table_of, array_frame_payload_of_cpu_attributes,
};
use streamlib_node_schema::pin_type::{
    ColorPinValue, RectanglePinValue, TransformPinValue, VectorComponentCount, VectorPinValue,
};
use streamlib_project_repository::FAMILIES_A_PANEL_WRITES;

use crate::core::descriptors::{PinType, PortFamily};

mod bundle_frame_packing;
#[cfg(test)]
mod bundle_frame_packing_tests;
mod bundle_packing;
#[cfg(test)]
mod tests;

pub use bundle_frame_packing::{
    BundleFrameReadAsItsBundle, HeavyMemberOfABundleFrame, HeavyMemberReadOffABundleFrame,
    the_bundle_a_bundle_frame_carries, the_bundle_frame_of_a_bundle,
};

pub use bundle_packing::{
    the_member_a_bundle_message_carries, the_members_of_a_bundle_a_records_message_carries,
    the_records_message_of_members_of_a_bundle,
};

use bundle_packing::{
    is_a_bundle_of_tensors, the_array_frame_of_a_bundle_of_tensors,
    the_bundle_a_records_message_carries, the_records_message_of_a_bundle, the_value_read_as,
};

/// The Array attribute a Tensor a client writes as a number or a list of numbers is carried as,
/// and the one attribute a List of numbers or values of parts travels as.
pub const VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME: &str = "values";

/// The key of the one entry of the Records message a String, a Menu, an Asset or a List of
/// them travels as.
pub const RECORDS_MESSAGE_VALUE_KEY: &str = "value";

/// The element type of an Array attribute a client writes.
const ARRAY_ATTRIBUTE_WRITTEN_BY_A_CLIENT_DTYPE: &str = "float64";

/// The sample type of an Audio block a client writes.
const AUDIO_BLOCK_WRITTEN_BY_A_CLIENT_SAMPLE_TYPE: ChannelsBlockSampleType =
    ChannelsBlockSampleType::F32;

/// The sample rate of a control message: none, as a value is no regular stream of samples.
const CONTROL_MESSAGE_SAMPLE_RATE: u32 = 0;

/// The value of a pin packed as the message it travels as: what the payload is, and its bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct PackedPinValue {
    /// What the payload is.
    pub payload_descriptor: DataChannelPayloadDescriptor,
    /// The payload's bytes.
    pub payload: Vec<u8>,
}

/// A message read off a link, borrowed: what its payload is, and its bytes.
#[derive(Debug, Clone, Copy)]
pub struct PackedPinValueRef<'message> {
    /// What the payload is.
    pub payload_descriptor: &'message DataChannelPayloadDescriptor,
    /// The payload's bytes.
    pub payload: &'message [u8],
}

impl PackedPinValue {
    /// This packed value, borrowed as a message read off a link.
    pub fn as_ref(&self) -> PackedPinValueRef<'_> {
        PackedPinValueRef {
            payload_descriptor: &self.payload_descriptor,
            payload: &self.payload,
        }
    }
}

/// The kind of message a value of `pin_type` travels as on a link; `None` for a setting's type,
/// which never takes a link, and a Connection, whose wire carries no message.
pub fn message_kind_of(pin_type: &PinType) -> Option<DataChannelPayloadDescriptorKind> {
    Some(match pin_type {
        PinType::Boolean {}
        | PinType::Integer { .. }
        | PinType::Float { .. }
        | PinType::Color {}
        | PinType::Vector { .. }
        | PinType::Rectangle {}
        | PinType::Transform {}
        | PinType::Audio {} => DataChannelPayloadDescriptorKind::ChannelsBlock,
        PinType::Texture { .. } => DataChannelPayloadDescriptorKind::ImageFrame,
        PinType::EncodedVideo { .. } | PinType::EncodedAudio { .. } => {
            DataChannelPayloadDescriptorKind::PacketsUnit
        }
        PinType::Tensor {} => DataChannelPayloadDescriptorKind::ArrayFrame,
        PinType::ListOf { .. } | PinType::Bundle { .. } if pin_type.holds_a_heavy_member() => {
            DataChannelPayloadDescriptorKind::BundleFrame
        }
        PinType::ListOf { element_type } if channel_count_of(element_type).is_some() => {
            DataChannelPayloadDescriptorKind::ArrayFrame
        }
        PinType::Bundle { members } if is_a_bundle_of_tensors(members) => {
            DataChannelPayloadDescriptorKind::ArrayFrame
        }
        PinType::String {}
        | PinType::Menu { .. }
        | PinType::Asset { .. }
        | PinType::ListOf { .. }
        | PinType::Bundle { .. }
        | PinType::Trigger {} => DataChannelPayloadDescriptorKind::RecordsMessage,
        PinType::Named { .. } => DataChannelPayloadDescriptorKind::Bytes,
        PinType::Connection { .. }
        | PinType::NodeReference { .. }
        | PinType::PinTypeChoice { .. } => return None,
    })
}

/// How many channels a value of `pin_type` packs as when it travels as a control message: one
/// for a number or a Boolean, four for a Color or a Rectangle, its size for a Vector and 16 for
/// a Transform; `None` for a type that travels as any other message.
pub fn channel_count_of(pin_type: &PinType) -> Option<usize> {
    match pin_type {
        PinType::Boolean {} | PinType::Integer { .. } | PinType::Float { .. } => Some(1),
        PinType::Color {} | PinType::Rectangle {} => Some(4),
        PinType::Vector { component_count } => Some(match component_count {
            VectorComponentCount::Two => 2,
            VectorComponentCount::Three => 3,
            VectorComponentCount::Four => 4,
        }),
        PinType::Transform {} => Some(16),
        _ => None,
    }
}

/// `value`, a value of `pin_type` in its JSON form, packed as the message it travels as, or why
/// it does not pack. A stored value is checked exactly as a set value is. A Tensor and an Audio
/// block, which hold no stored value, take what a client writes today: a Tensor a number or a
/// list of numbers nested to any depth, and an Audio block `{"samples": [[...], ...],
/// "sample_rate"}` channel by channel, starting at `next_start_sample`. A control message starts
/// at `next_start_sample` too.
///
/// A String, a Menu, an Asset, and a List of anything but numbers and values of parts, pack as
/// a Records message `{"value": ...}`, a Menu as its option name. A List of numbers, Colors,
/// Vectors, Rectangles or Transforms packs as an Array frame of one `float64` attribute,
/// `values`, of shape `[n]` for numbers or `[n, parts]`, each element's parts in order.
pub fn pack_pin_value(
    pin_type: &PinType,
    value: &Value,
    next_start_sample: u64,
) -> Result<PackedPinValue, String> {
    match pin_type {
        PinType::ListOf { .. } | PinType::Bundle { .. } if pin_type.holds_a_heavy_member() => {
            Err(a_bundle_frame_holds_no_json_value(pin_type))
        }
        PinType::Tensor {} => the_array_frame_of_attributes(&[the_array_attribute_of_a_value(
            VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME,
            value,
        )?]),
        PinType::Audio {} => the_audio_block_of_a_value(value, next_start_sample),
        _ if channel_count_of(pin_type).is_some() => Ok(control_message_of_channel_values(
            &the_channel_values_of_a_stored_value(pin_type, value)?,
            next_start_sample,
        )),
        PinType::ListOf { element_type } if channel_count_of(element_type).is_some() => {
            the_array_frame_of_a_list_of_parts(pin_type, element_type, value)
        }
        PinType::Bundle { members } if is_a_bundle_of_tensors(members) => {
            the_array_frame_of_a_bundle_of_tensors(pin_type, members, value)
        }
        PinType::Bundle { .. } => the_records_message_of_a_bundle(pin_type, value),
        PinType::String {}
        | PinType::Menu { .. }
        | PinType::Asset { .. }
        | PinType::ListOf { .. } => the_records_message_of_a_stored_value(pin_type, value),
        _ => Err(format!("a {pin_type} value does not pack onto a link yet")),
    }
}

/// The JSON form of the value of `pin_type` `message` carries, read as `pin_type` reads it, or
/// why it does not read: a control message of another size is read with the free conversion the
/// link verdict allows, as is each element of a List of numbers or values of parts; a value a
/// Records message carries is checked as a set value is, so a Menu refuses a name that is none
/// of its options.
pub fn unpack_pin_value(
    pin_type: &PinType,
    message: PackedPinValueRef<'_>,
) -> Result<Value, String> {
    match pin_type {
        PinType::ListOf { .. } | PinType::Bundle { .. } if pin_type.holds_a_heavy_member() => {
            Err(a_bundle_frame_holds_no_json_value(pin_type))
        }
        _ if channel_count_of(pin_type).is_some() => {
            let read = the_channel_values_a_pin_type_reads(
                pin_type,
                &the_channel_values_a_control_message_carries(message)?,
            )?;
            Ok(the_stored_value_of_channel_values(pin_type, &read))
        }
        PinType::ListOf { element_type } if channel_count_of(element_type).is_some() => {
            the_list_of_parts_an_array_frame_carries(element_type, message)
        }
        PinType::Bundle { members } if !is_a_bundle_of_tensors(members) => {
            the_bundle_a_records_message_carries(pin_type, message)
        }
        PinType::String {}
        | PinType::Menu { .. }
        | PinType::Asset { .. }
        | PinType::ListOf { .. } => the_stored_value_a_records_message_carries(pin_type, message),
        _ => Err(format!("a {pin_type} value does not unpack off a link yet")),
    }
}

/// Why a value of `pin_type`, a List or a bundle holding a Texture, Audio or an encoded unit,
/// has no JSON form to pack or unpack: it holds no stored value, and travels only as the Bundle
/// frame a node sends.
fn a_bundle_frame_holds_no_json_value(pin_type: &PinType) -> String {
    format!(
        "a {pin_type} holds a Texture, Audio or encoded member, so it travels only as the Bundle \
         frame a node sends and has no stored value to pack or unpack"
    )
}

/// `value`, a stored value of `pin_type`, as the Records message `{"value": value}`, which a
/// String, a Menu, an Asset and a List of them travel as, and a helper's island carries any List
/// as, each bundle element made whole; or why `pin_type` does not take it.
pub fn the_records_message_of_a_stored_value(
    pin_type: &PinType,
    value: &Value,
) -> Result<PackedPinValue, String> {
    pin_type
        .check_value(value)
        .map_err(|not_taken| format!("`{value}` is not {}", not_taken.takes))?;
    the_records_message_of_a_record(
        &serde_json::json!({ RECORDS_MESSAGE_VALUE_KEY: the_value_read_as(pin_type, value)? }),
    )
}

/// `record` as the Records message of its msgpack map, or why it is too large for one.
fn the_records_message_of_a_record(record: &Value) -> Result<PackedPinValue, String> {
    let payload = rmp_serde::to_vec_named(record)
        .map_err(|why| format!("the value does not encode: {why}"))?;
    if payload.len() > RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES {
        return Err(format!(
            "the value encodes to {} bytes, past the {RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES} a \
             Records message holds",
            payload.len()
        ));
    }
    Ok(PackedPinValue {
        payload_descriptor: DataChannelPayloadDescriptor::records_message(),
        payload,
    })
}

/// The stored value of `pin_type` the Records message `message` carries under `value`, each
/// bundle element read as `pin_type`'s own, checked as a set value is; or why it carries none.
pub fn the_stored_value_a_records_message_carries(
    pin_type: &PinType,
    message: PackedPinValueRef<'_>,
) -> Result<Value, String> {
    if DataChannelPayloadDescriptorKind::from_wire(message.payload_descriptor.kind)
        != Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    {
        return Err(format!(
            "a {pin_type} travels as a Records message, and the message is none"
        ));
    }
    let record: Value = rmp_serde::from_slice(message.payload)
        .map_err(|why| format!("the Records message does not decode: {why}"))?;
    let Value::Object(mut record) = record else {
        return Err("the Records message holds no map".to_owned());
    };
    let carried = record.remove(RECORDS_MESSAGE_VALUE_KEY).ok_or_else(|| {
        format!("a {pin_type} travels as `{{\"{RECORDS_MESSAGE_VALUE_KEY}\": ...}}`, and the message holds no `{RECORDS_MESSAGE_VALUE_KEY}`")
    })?;
    let value = the_value_read_as(pin_type, &carried)?;
    pin_type
        .check_value(&value)
        .map_err(|not_taken| format!("`{value}` is not {}", not_taken.takes))?;
    Ok(value)
}

/// `value`, a stored List of `element_type`, a number or a value of parts, as an Array frame of
/// one attribute holding each element's parts in order.
fn the_array_frame_of_a_list_of_parts(
    pin_type: &PinType,
    element_type: &PinType,
    value: &Value,
) -> Result<PackedPinValue, String> {
    pin_type
        .check_value(value)
        .map_err(|not_taken| format!("`{value}` is not {}", not_taken.takes))?;
    let elements = value.as_array().map(Vec::as_slice).unwrap_or_default();
    let parts_per_element = channel_count_of(element_type).unwrap_or(1);
    let mut data = Vec::with_capacity(elements.len() * parts_per_element * 8);
    for element in elements {
        for part in the_channel_values_of_a_stored_value(element_type, element)? {
            data.extend_from_slice(&part.to_le_bytes());
        }
    }
    let shape = match parts_per_element {
        1 => vec![elements.len() as u64],
        parts => vec![elements.len() as u64, parts as u64],
    };
    the_array_frame_of_attributes(&[ArrayAttributeOfAJsonValue {
        name: VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME.to_owned(),
        shape,
        data,
    }])
}

/// The List of `element_type`, a number or a value of parts, the Array frame `message` carries
/// in its one CPU attribute of shape `[n]` or `[n, parts]`, `float64` or `float32`.
fn the_list_of_parts_an_array_frame_carries(
    element_type: &PinType,
    message: PackedPinValueRef<'_>,
) -> Result<Value, String> {
    if DataChannelPayloadDescriptorKind::from_wire(message.payload_descriptor.kind)
        != Some(DataChannelPayloadDescriptorKind::ArrayFrame)
    {
        return Err(format!(
            "a List of {element_type} travels as an Array frame, and the message is none"
        ));
    }
    let body = ArrayFrameDescriptorBody::from_descriptor_body(&message.payload_descriptor.body);
    let rows = array_frame_attribute_table_of(body, message.payload).map_err(|why| why.reason)?;
    let [row] = rows.as_slice() else {
        return Err(format!(
            "a List travels as an Array frame of one attribute, and the message holds {}",
            rows.len()
        ));
    };
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = row.residency
    else {
        return Err(format!(
            "Array attribute {:?} is on the GPU, and a List is read on the CPU",
            row.name
        ));
    };
    let (element_count, parts_per_element) = match row.shape.as_slice() {
        [element_count] => (*element_count as usize, 1),
        [element_count, parts] => (*element_count as usize, *parts as usize),
        shape => {
            return Err(format!(
                "a List's Array attribute has the shape [n] or [n, parts], and {:?} has {shape:?}",
                row.name
            ));
        }
    };
    let sample_bytes = match row.dtype.as_str() {
        "float64" => 8,
        "float32" => 4,
        dtype => {
            return Err(format!(
                "a List's Array attribute holds float64 or float32, and {:?} holds {dtype}",
                row.name
            ));
        }
    };
    let data = &message.payload[payload_byte_offset as usize..][..byte_len as usize];
    if data.len() != element_count * parts_per_element * sample_bytes {
        return Err(format!(
            "Array attribute {:?} states the shape {:?} and holds {} bytes",
            row.name,
            row.shape,
            data.len()
        ));
    }
    let numbers: Vec<f64> = data
        .chunks_exact(sample_bytes)
        .map(|sample| match sample_bytes {
            8 => f64::from_le_bytes(sample.try_into().unwrap_or_default()),
            _ => f64::from(f32::from_le_bytes(sample.try_into().unwrap_or_default())),
        })
        .collect();
    numbers
        .chunks(parts_per_element.max(1))
        .take(element_count)
        .map(|parts| {
            the_channel_values_a_pin_type_reads(element_type, parts)
                .map(|read| the_stored_value_of_channel_values(element_type, &read))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// The numbers `value`, a stored value of `pin_type` in its JSON form, packs as, one per
/// channel in the order its stored form lists them: 1 or 0 for a Boolean; or why `pin_type`
/// does not take it.
pub fn the_channel_values_of_a_stored_value(
    pin_type: &PinType,
    value: &Value,
) -> Result<Vec<f64>, String> {
    pin_type
        .check_value(value)
        .map_err(|not_taken| format!("`{value}` is not {}", not_taken.takes))?;
    let read_as = |what: &str| format!("`{value}` does not read as {what}");
    Ok(match pin_type {
        PinType::Boolean {} => vec![f64::from(u8::from(value.as_bool().unwrap_or_default()))],
        PinType::Integer { .. } | PinType::Float { .. } => {
            vec![value.as_f64().ok_or_else(|| read_as("a number"))?]
        }
        PinType::Color {} => {
            let ColorPinValue {
                red,
                green,
                blue,
                alpha,
            } = read_value(value).ok_or_else(|| read_as("a Color"))?;
            vec![red, green, blue, alpha]
        }
        PinType::Vector { .. } => {
            let VectorPinValue { x, y, z, w } =
                read_value(value).ok_or_else(|| read_as("a Vector"))?;
            [Some(x), Some(y), z, w].into_iter().flatten().collect()
        }
        PinType::Rectangle {} => {
            let RectanglePinValue {
                x,
                y,
                width,
                height,
            } = read_value(value).ok_or_else(|| read_as("a Rectangle"))?;
            vec![x, y, width, height]
        }
        PinType::Transform {} => {
            let TransformPinValue(numbers) =
                read_value(value).ok_or_else(|| read_as("a Transform"))?;
            numbers.to_vec()
        }
        _ => return Err(format!("a {pin_type} value packs as no control message")),
    })
}

/// The JSON form of the value of `pin_type` whose parts are `channel_values`, as many as
/// [`channel_count_of`] gives, in the order its stored form lists them: an Integer rounded half
/// away from zero, and a Boolean true for any number but 0.
fn the_stored_value_of_channel_values(pin_type: &PinType, channel_values: &[f64]) -> Value {
    let number = |index: usize| channel_values.get(index).copied().unwrap_or_default();
    match pin_type {
        PinType::Boolean {} => Value::Bool(number(0) != 0.0),
        PinType::Integer { .. } => Value::from(number(0).round() as i64),
        PinType::Color {} => serde_json::json!({
            "red": number(0), "green": number(1), "blue": number(2), "alpha": number(3),
        }),
        PinType::Vector { .. } => {
            let mut vector = serde_json::Map::new();
            for (index, key) in ["x", "y", "z", "w"]
                .into_iter()
                .take(channel_values.len())
                .enumerate()
            {
                vector.insert(key.to_owned(), Value::from(number(index)));
            }
            Value::Object(vector)
        }
        PinType::Rectangle {} => serde_json::json!({
            "x": number(0), "y": number(1), "width": number(2), "height": number(3),
        }),
        PinType::Transform {} => Value::from(channel_values.to_vec()),
        _ => Value::from(number(0)),
    }
}

fn read_value<T: DeserializeOwned>(value: &Value) -> Option<T> {
    T::deserialize(value).ok()
}

/// A control message of `channel_values`, one f64 sample on each channel at rate 0, starting at
/// `start_sample`.
pub fn control_message_of_channel_values(
    channel_values: &[f64],
    start_sample: u64,
) -> PackedPinValue {
    PackedPinValue {
        payload_descriptor: DataChannelPayloadDescriptor::channels_block(
            ChannelsBlockDescriptorBody {
                start_sample,
                sample_rate: CONTROL_MESSAGE_SAMPLE_RATE,
                channel_count: u32::try_from(channel_values.len()).unwrap_or(u32::MAX),
                samples_per_channel: 1,
                sample_type: ChannelsBlockSampleType::F64 as u32,
            },
        ),
        payload: channel_values
            .iter()
            .flat_map(|channel_value| channel_value.to_le_bytes())
            .collect(),
    }
}

/// The first sample of each channel of `message`, a Channels block of f32 or f64 samples, as an
/// f64, which is the value a control message carries; or why `message` carries none.
pub fn the_channel_values_a_control_message_carries(
    message: PackedPinValueRef<'_>,
) -> Result<Vec<f64>, String> {
    if DataChannelPayloadDescriptorKind::from_wire(message.payload_descriptor.kind)
        != Some(DataChannelPayloadDescriptorKind::ChannelsBlock)
    {
        return Err("the message is no Channels block".to_owned());
    }
    let block = ChannelsBlockDescriptorBody::from_descriptor_body(&message.payload_descriptor.body);
    let Some(sample_type) = ChannelsBlockSampleType::from_wire(block.sample_type) else {
        return Err("the message names a sample type this build does not know".to_owned());
    };
    let sample_bytes = match sample_type {
        ChannelsBlockSampleType::F64 => 8,
        ChannelsBlockSampleType::F32 => 4,
    };
    if block.channel_count == 0 || block.samples_per_channel == 0 {
        return Err("the message carries no sample".to_owned());
    }
    let channel_bytes = sample_bytes * block.samples_per_channel as usize;
    (0..block.channel_count as usize)
        .map(|channel| {
            let first_sample = message
                .payload
                .get(channel * channel_bytes..channel * channel_bytes + sample_bytes)
                .ok_or_else(|| "the message is shorter than its block states".to_owned())?;
            Ok(match sample_type {
                ChannelsBlockSampleType::F64 => {
                    f64::from_le_bytes(first_sample.try_into().unwrap_or_default())
                }
                ChannelsBlockSampleType::F32 => f64::from(f32::from_le_bytes(
                    first_sample.try_into().unwrap_or_default(),
                )),
            })
        })
        .collect()
}

/// The parts of a value of `pin_type` read from `channel_values`, one sent on each channel of a
/// control message, with the free conversion the link verdict allows: a number reads the first
/// channel; a Vector takes one number as every component, drops the components past its size and
/// pads the missing ones with 0; a Color takes one number as red, green and blue with an alpha of
/// 1, and reads a Vector's x, y, z and w as red, green, blue and alpha, padding a missing alpha
/// with 1 and any other part with 0. A Rectangle and a Transform read exactly their own size.
pub fn the_channel_values_a_pin_type_reads(
    pin_type: &PinType,
    channel_values: &[f64],
) -> Result<Vec<f64>, String> {
    let Some(channel_count) = channel_count_of(pin_type) else {
        return Err(format!("a {pin_type} reads no control message"));
    };
    let Some(&first) = channel_values.first() else {
        return Err("the control message carries no channel".to_owned());
    };
    Ok(match pin_type {
        PinType::Boolean {} | PinType::Integer { .. } | PinType::Float { .. } => vec![first],
        PinType::Vector { .. } if channel_values.len() == 1 => vec![first; channel_count],
        PinType::Color {} if channel_values.len() == 1 => vec![first, first, first, 1.0],
        PinType::Vector { .. } | PinType::Color {} => (0..channel_count)
            .map(|index| match channel_values.get(index) {
                Some(&channel_value) => channel_value,
                None if index == 3 && matches!(pin_type, PinType::Color {}) => 1.0,
                None => 0.0,
            })
            .collect(),
        _ if channel_values.len() == channel_count => channel_values.to_vec(),
        _ => {
            return Err(format!(
                "a {pin_type} reads {channel_count} channels, and the control message carries {}",
                channel_values.len()
            ));
        }
    })
}

/// The message `value`, written by a client onto an output of `family` that names no pin type,
/// is sent as: a Records output takes a JSON object; an Array output a Tensor, a number or a list
/// of numbers nested to any depth, as its `values` attribute, or an object of such attributes by
/// name; a Channels output a number or a bool as a control message, or an Audio block. Image and
/// Packets are not written by a client.
pub fn the_message_a_value_written_onto_an_output_is(
    family: PortFamily,
    value: &Value,
    next_start_sample: u64,
) -> Result<PackedPinValue, String> {
    match family {
        PortFamily::Records => {
            if !value.is_object() {
                return Err("a Records value is a JSON object".to_string());
            }
            let payload = rmp_serde::to_vec_named(value)
                .map_err(|why| format!("the record does not encode: {why}"))?;
            Ok(PackedPinValue {
                payload_descriptor: DataChannelPayloadDescriptor::records_message(),
                payload,
            })
        }
        PortFamily::Array => match value {
            Value::Object(attributes_by_name) if !attributes_by_name.is_empty() => {
                the_array_frame_of_attributes(
                    &attributes_by_name
                        .iter()
                        .map(|(name, attribute)| the_array_attribute_of_a_value(name, attribute))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Value::Object(_) => Err("an Array value names at least one attribute".into()),
            _ => pack_pin_value(&PinType::Tensor {}, value, next_start_sample),
        },
        PortFamily::Channels => match value {
            Value::Number(_) => pack_pin_value(
                &PinType::Float { exact_size: None },
                value,
                next_start_sample,
            ),
            Value::Bool(_) => pack_pin_value(&PinType::Boolean {}, value, next_start_sample),
            Value::Object(_) => pack_pin_value(&PinType::Audio {}, value, next_start_sample),
            _ => Err(
                "a Channels value is a number, a bool or `{\"samples\": [[...], ...], \"sample_rate\"}`"
                    .to_string(),
            ),
        },
        PortFamily::Image | PortFamily::Packets => Err(why_a_client_never_writes(family)),
    }
}

/// Why a client never writes a value onto an output of `family`.
pub(crate) fn why_a_client_never_writes(family: PortFamily) -> String {
    let [families_listed @ .., last] = FAMILIES_A_PANEL_WRITES.map(PortFamily::declaration_name);
    format!(
        "a client writes {} and {last} values, never {}",
        families_listed.join(", "),
        family.declaration_name()
    )
}

/// An Array attribute read off a JSON value: its shape, outermost first, and its elements.
struct ArrayAttributeOfAJsonValue {
    name: String,
    shape: Vec<u64>,
    data: Vec<u8>,
}

fn the_array_frame_of_attributes(
    attributes: &[ArrayAttributeOfAJsonValue],
) -> Result<PackedPinValue, String> {
    let attributes_to_pack: Vec<ArrayFrameCpuAttributeToPack<'_>> = attributes
        .iter()
        .map(|attribute| ArrayFrameCpuAttributeToPack {
            name: &attribute.name,
            dtype: ARRAY_ATTRIBUTE_WRITTEN_BY_A_CLIENT_DTYPE,
            shape: &attribute.shape,
            data: &attribute.data,
        })
        .collect();
    let (body, payload) = array_frame_payload_of_cpu_attributes(&attributes_to_pack)
        .map_err(|why| why.to_string())?;
    Ok(PackedPinValue {
        payload_descriptor: DataChannelPayloadDescriptor::array_frame(body),
        payload,
    })
}

fn the_array_attribute_of_a_value(
    name: &str,
    value: &Value,
) -> Result<ArrayAttributeOfAJsonValue, String> {
    let mut shape = Vec::new();
    let mut level = value;
    while let Value::Array(elements) = level {
        shape.push(elements.len() as u64);
        match elements.first() {
            Some(first) => level = first,
            None => break,
        }
    }
    let mut elements = Vec::new();
    the_elements_of_a_value_shaped(value, &shape, &mut elements).map_err(|why| {
        format!("attribute `{name}` is a number or a list of numbers of one shape: {why}")
    })?;
    if shape.is_empty() {
        shape.push(1);
    }
    Ok(ArrayAttributeOfAJsonValue {
        name: name.to_string(),
        shape,
        data: elements
            .iter()
            .flat_map(|element: &f64| element.to_le_bytes())
            .collect(),
    })
}

fn the_elements_of_a_value_shaped(
    value: &Value,
    shape: &[u64],
    elements: &mut Vec<f64>,
) -> Result<(), String> {
    match (value, shape.split_first()) {
        (Value::Number(number), None) => {
            elements.push(number.as_f64().ok_or("a number is out of range")?);
            Ok(())
        }
        (Value::Array(nested), Some((&length, inner_shape))) if nested.len() as u64 == length => {
            nested.iter().try_for_each(|nested| {
                the_elements_of_a_value_shaped(nested, inner_shape, elements)
            })
        }
        (Value::Array(_), Some(_)) => Err("its lists differ in length".into()),
        _ => Err(format!("`{value}` is not a number at that depth")),
    }
}

fn the_audio_block_of_a_value(value: &Value, start_sample: u64) -> Result<PackedPinValue, String> {
    let Value::Object(block) = value else {
        return Err("an Audio block is `{\"samples\": [[...], ...], \"sample_rate\"}`".to_string());
    };
    let channels = block
        .get("samples")
        .and_then(Value::as_array)
        .ok_or("a Channels block is `{\"samples\": [[...], ...], \"sample_rate\"}`")?
        .iter()
        .map(|channel| {
            channel
                .as_array()
                .ok_or("each channel of `samples` is a list of numbers")?
                .iter()
                .map(|sample| sample.as_f64().ok_or("each sample is a number"))
                .collect::<Result<Vec<f64>, _>>()
        })
        .collect::<Result<Vec<Vec<f64>>, _>>()?;
    let sample_rate = match block.get("sample_rate") {
        None => 0,
        Some(rate) => rate
            .as_u64()
            .and_then(|rate| u32::try_from(rate).ok())
            .ok_or("`sample_rate` is a whole number of samples per second")?,
    };
    if let Some(key) = block
        .keys()
        .find(|key| !matches!(key.as_str(), "samples" | "sample_rate"))
    {
        return Err(format!("a Channels block takes no `{key}`"));
    }
    let samples_per_channel = channels.first().map_or(0, Vec::len);
    if samples_per_channel == 0
        || channels
            .iter()
            .any(|channel| channel.len() != samples_per_channel)
    {
        return Err("every channel holds the same number of samples, at least one".into());
    }
    let channel_count = u32::try_from(channels.len()).map_err(|_| "too many channels")?;
    let samples_per_channel =
        u32::try_from(samples_per_channel).map_err(|_| "too many samples per channel")?;
    Ok(PackedPinValue {
        payload_descriptor: DataChannelPayloadDescriptor::channels_block(
            ChannelsBlockDescriptorBody {
                start_sample,
                sample_rate,
                channel_count,
                samples_per_channel,
                sample_type: AUDIO_BLOCK_WRITTEN_BY_A_CLIENT_SAMPLE_TYPE as u32,
            },
        ),
        payload: channels
            .iter()
            .flatten()
            .flat_map(|sample| (*sample as f32).to_le_bytes())
            .collect(),
    })
}
