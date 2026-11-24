// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The packing of a bundle. A bundle of small members, or of Lists of them, is one Records
//! message whose map holds each member by name; a bundle of Tensors is one Array frame with an
//! attribute named for each member. Either is one message with one timestamp. A bundle is packed
//! whole, each member its stored value leaves out filled with the member's default, and read as
//! the input's own bundle: only the members it asks for, so an output holding more connects.

use serde_json::{Map, Value};
use streamlib_ipc_types::{
    ArrayFrameAttributeResidency, ArrayFrameCpuAttributeToPack, ArrayFrameDescriptorBody,
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind, array_frame_attribute_table_of,
    array_frame_payload_of_cpu_attributes,
};
use streamlib_node_schema::pin_type::{BundleMember, BundleMessage};

use super::bundle_frame_packing::the_member_a_bundle_frame_carries;
use super::{
    PackedPinValue, PackedPinValueRef, VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME,
    the_array_attribute_of_a_value, the_array_frame_of_attributes, the_records_message_of_a_record,
};
use crate::core::descriptors::PinType;

/// `value`, a stored value of the bundle `pin_type` of small members, as the Records message of
/// its members by name, each one `value` leaves out filled with its default.
pub(super) fn the_records_message_of_a_bundle(
    pin_type: &PinType,
    value: &Value,
) -> Result<PackedPinValue, String> {
    pin_type
        .check_value(value)
        .map_err(|not_taken| format!("`{value}` is not {}", not_taken.takes))?;
    the_records_message_of_a_record(&the_value_read_as(pin_type, value)?)
}

/// The stored value of the bundle `pin_type` the Records message `message` carries: the members
/// it asks for, by name, any other member the message holds left unread, checked as a set value
/// is.
pub(super) fn the_bundle_a_records_message_carries(
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
    let read = the_value_read_as(pin_type, &record)?;
    pin_type
        .check_value(&read)
        .map_err(|not_taken| format!("`{read}` is not {}", not_taken.takes))?;
    Ok(read)
}

/// `value` as `pin_type` reads it: a bundle's object holding only the members the bundle names,
/// each one `value` leaves out given its default, and a List's elements each read so. Any other
/// type reads `value` as it is. Fails naming a member that is neither held nor defaulted.
pub(super) fn the_value_read_as(pin_type: &PinType, value: &Value) -> Result<Value, String> {
    match (pin_type, value) {
        (PinType::Bundle { members, .. }, Value::Object(held)) => members
            .iter()
            .map(|member| {
                let member_value = match (held.get(&member.name), &member.default_value) {
                    (Some(held), _) => the_value_read_as(&member.member_type, held)?,
                    (None, Some(default_value)) => default_value.clone(),
                    (None, None) => {
                        return Err(format!(
                            "the bundle holds no member `{}`, which has no default",
                            member.name
                        ));
                    }
                };
                Ok((member.name.clone(), member_value))
            })
            .collect::<Result<Map<String, Value>, String>>()
            .map(Value::Object),
        (PinType::Bundle { .. }, value) => Err(format!(
            "a {pin_type} is an object of its members, and `{value}` is none"
        )),
        (PinType::ListOf { element_type }, Value::Array(elements)) => elements
            .iter()
            .map(|element| the_value_read_as(element_type, element))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Ok(value.clone()),
    }
}

/// `value`, an object holding each member of the bundle of Tensors `members` by name, as an
/// Array frame of one attribute named for each member, in the order the bundle declares them.
/// Each member is a number or a list of numbers nested to any depth, as a Tensor a client writes.
pub(super) fn the_array_frame_of_a_bundle_of_tensors(
    pin_type: &PinType,
    members: &[BundleMember],
    value: &Value,
) -> Result<PackedPinValue, String> {
    let Value::Object(held) = value else {
        return Err(format!(
            "a {pin_type} is an object of its members, and `{value}` is none"
        ));
    };
    if let Some(unknown) = held
        .keys()
        .find(|key| !members.iter().any(|member| &member.name == *key))
    {
        return Err(format!("a {pin_type} has no member `{unknown}`"));
    }
    let attributes = members
        .iter()
        .map(|member| {
            let tensor = held
                .get(&member.name)
                .ok_or_else(|| format!("the bundle holds no member `{}`", member.name))?;
            the_array_attribute_of_a_value(&member.name, tensor)
        })
        .collect::<Result<Vec<_>, _>>()?;
    the_array_frame_of_attributes(&attributes)
}

/// The message member `member` of the bundle `bundle` travels as on its own, taken from
/// `message`, a message of the whole bundle: a small member's value, or its default, packed as
/// its own type travels; a Tensor member's attribute as an Array frame of that one attribute,
/// renamed `values`; a Texture, Audio or encoded member's row of a Bundle frame as the message it
/// is on a wire of its own. Fails saying why the message holds no such member.
pub fn the_member_a_bundle_message_carries(
    bundle: &PinType,
    member: &BundleMember,
    message: PackedPinValueRef<'_>,
) -> Result<PackedPinValue, String> {
    let Some(travels_as) = bundle.the_message_a_bundle_travels_as() else {
        return Err(format!("a {bundle} is no bundle, so it holds no member"));
    };
    match travels_as {
        BundleMessage::RecordsMap => {}
        BundleMessage::ArrayFrameOfTensors => {
            return the_tensor_member_an_array_frame_carries(&member.name, message);
        }
        BundleMessage::BundleFrame => {
            return the_member_a_bundle_frame_carries(bundle, member, message);
        }
    }
    if DataChannelPayloadDescriptorKind::from_wire(message.payload_descriptor.kind)
        != Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    {
        return Err(format!(
            "a {bundle} travels as a Records message, and the message is none"
        ));
    }
    let record: Value = rmp_serde::from_slice(message.payload)
        .map_err(|why| format!("the Records message does not decode: {why}"))?;
    let member_value = match (record.get(&member.name), &member.default_value) {
        (Some(held), _) => the_value_read_as(&member.member_type, held)?,
        (None, Some(default_value)) => default_value.clone(),
        (None, None) => {
            return Err(format!(
                "the bundle's message holds no member `{}`, which has no default",
                member.name
            ));
        }
    };
    super::pack_pin_value(&member.member_type, &member_value, 0)
}

/// The Array frame of the one attribute `member` names in `message`, an Array frame of a bundle
/// of Tensors, under the name a Tensor travels as.
fn the_tensor_member_an_array_frame_carries(
    member: &str,
    message: PackedPinValueRef<'_>,
) -> Result<PackedPinValue, String> {
    if DataChannelPayloadDescriptorKind::from_wire(message.payload_descriptor.kind)
        != Some(DataChannelPayloadDescriptorKind::ArrayFrame)
    {
        return Err(
            "a bundle of Tensors travels as an Array frame, and the message is none".into(),
        );
    }
    let body = ArrayFrameDescriptorBody::from_descriptor_body(&message.payload_descriptor.body);
    let rows = array_frame_attribute_table_of(body, message.payload).map_err(|why| why.reason)?;
    let row = rows
        .iter()
        .find(|row| row.name == member)
        .ok_or_else(|| format!("the bundle's Array frame holds no attribute `{member}`"))?;
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = row.residency
    else {
        return Err(format!(
            "member `{member}` is on the GPU, and a member is taken from its bundle on the CPU"
        ));
    };
    let data = message
        .payload
        .get(payload_byte_offset as usize..)
        .and_then(|from_it| from_it.get(..byte_len as usize))
        .ok_or_else(|| format!("member `{member}` lies past the end of its message"))?;
    let (body, payload) = array_frame_payload_of_cpu_attributes(&[ArrayFrameCpuAttributeToPack {
        name: VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME,
        dtype: &row.dtype,
        shape: &row.shape,
        data,
    }])
    .map_err(|why| why.to_string())?;
    Ok(PackedPinValue {
        payload_descriptor: DataChannelPayloadDescriptor::array_frame(body),
        payload,
    })
}

/// `held`, some members of the bundle `pin_type` by name, as the Records message of those members
/// alone, each read as its member's type reads it; or why one is no member the bundle declares,
/// or its member's type does not take it.
pub fn the_records_message_of_members_of_a_bundle(
    pin_type: &PinType,
    held: &Map<String, Value>,
) -> Result<PackedPinValue, String> {
    the_records_message_of_a_record(&Value::Object(the_members_read_as(pin_type, held)?))
}

/// The members of the bundle `pin_type` the Records message `message` of some of its members
/// carries, by name, each read as its member's type reads it; or why it carries none so.
pub fn the_members_of_a_bundle_a_records_message_carries(
    pin_type: &PinType,
    message: PackedPinValueRef<'_>,
) -> Result<Map<String, Value>, String> {
    if DataChannelPayloadDescriptorKind::from_wire(message.payload_descriptor.kind)
        != Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    {
        return Err(format!(
            "the members of a {pin_type} travel as a Records message, and the message is none"
        ));
    }
    let record: Value = rmp_serde::from_slice(message.payload)
        .map_err(|why| format!("the Records message does not decode: {why}"))?;
    let Value::Object(held) = record else {
        return Err("the Records message holds no map".to_owned());
    };
    the_members_read_as(pin_type, &held)
}

/// `held`, some members of the bundle `pin_type` by name, each read as its member's type reads it
/// and checked as a set value is; or why one is no member the bundle declares, or is refused.
fn the_members_read_as(
    pin_type: &PinType,
    held: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let PinType::Bundle { members, .. } = pin_type else {
        return Err(format!("a {pin_type} holds no members"));
    };
    held.iter()
        .map(|(name, value)| {
            let member = members
                .iter()
                .find(|member| &member.name == name)
                .ok_or_else(|| format!("a {pin_type} has no member `{name}`"))?;
            let read = the_value_read_as(&member.member_type, value)?;
            member.member_type.check_value(&read).map_err(|not_taken| {
                format!(
                    "member `{name}` takes {}, and `{read}` is none",
                    not_taken.takes
                )
            })?;
            Ok((name.clone(), read))
        })
        .collect()
}
