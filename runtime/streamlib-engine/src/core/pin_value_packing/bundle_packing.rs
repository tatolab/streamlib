// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The packing of a bundle. A bundle of small members, or of Lists of them, is one Records
//! message whose map holds each member by name; a bundle of Tensors is one Array frame with an
//! attribute named for each member. Either is one message with one timestamp. A bundle is packed
//! whole, each member its stored value leaves out filled with the member's default, and read as
//! the input's own bundle: only the members it asks for, so an output holding more connects.

use serde_json::{Map, Value};
use streamlib_ipc_types::DataChannelPayloadDescriptorKind;
use streamlib_node_schema::pin_type::BundleMember;

use super::{
    PackedPinValue, PackedPinValueRef, the_array_attribute_of_a_value,
    the_array_frame_of_attributes, the_records_message_of_a_record,
};
use crate::core::descriptors::PinType;

/// Whether a bundle of `members` is a bundle of Tensors, which travels as an Array frame.
pub(super) fn is_a_bundle_of_tensors(members: &[BundleMember]) -> bool {
    !members.is_empty()
        && members
            .iter()
            .all(|member| matches!(member.member_type, PinType::Tensor {}))
}

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
        (PinType::Bundle { members }, Value::Object(held)) => members
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
