// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How a function node input reads the messages a link from outside its island carries: a
//! control value, the parts of a value, a record or a stored value, off the newest message.

use super::*;

/// How a function node input reads the messages a link from outside its
/// island carries.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum HowAnInputReadsALinkFromOutsideItsIsland {
    /// As the number a control message carries: the first sample of its first
    /// channel.
    ControlValue,
    /// As the parts of a value of the pin type, one on each channel of a
    /// control message, read with the free conversion a link allows.
    Parts(PinType),
    /// As the dict a Records message carries.
    Record,
    /// As the record `{"value": <stored JSON form>}` of a value of the pin type, which a
    /// String, a Menu, an Asset and a List travel as within an island, read off the message its
    /// type packs as.
    StoredValue(PinType),
    /// As the message itself, handed on unread: nothing at an evaluation no new
    /// message arrived before.
    MessageHandedOnUnread,
}

impl HowAnInputReadsALinkFromOutsideItsIsland {
    /// How an input of `value_type`, as the bootstrap names it, reads a link.
    pub(super) fn of_the_value_type(value_type: &str) -> std::result::Result<Self, String> {
        match value_type {
            "float" | "int" | "bool" => Ok(Self::ControlValue),
            "records" => Ok(Self::Record),
            "unread_message" => Ok(Self::MessageHandedOnUnread),
            value_type => the_pin_type_a_value_type_carries_in_parts(value_type)
                .map(Self::Parts)
                .ok_or_else(|| {
                    format!(
                        "an input of value type {value_type:?} cannot read a link from a node \
                         outside its island; it reads a number, a Color, a Vector, a \
                         Rectangle, a Transform, a dict or an `UnreadMessage`"
                    )
                }),
        }
    }
}

/// The pin type of a value the bootstrap names `value_type` and carries as its
/// parts: a Color, a Vector of each size, a Rectangle or a Transform.
///
/// Wire contract with the helper bootstrap: these value type names.
pub(crate) fn the_pin_type_a_value_type_carries_in_parts(value_type: &str) -> Option<PinType> {
    Some(match value_type {
        "color" => PinType::Color {},
        "vector2" => PinType::Vector {
            component_count: VectorComponentCount::Two,
        },
        "vector3" => PinType::Vector {
            component_count: VectorComponentCount::Three,
        },
        "vector4" => PinType::Vector {
            component_count: VectorComponentCount::Four,
        },
        "rectangle" => PinType::Rectangle {},
        "transform" => PinType::Transform {},
        _ => return None,
    })
}

/// The parts a control message carries, read as `read_as` reads them: one
/// number, the first sample of its first channel, for `None`; the parts of a
/// value of the pin type for `Some`, with the free conversion a link allows.
pub(super) fn the_parts_a_message_carries(
    message: &MessageReadFromInputPort,
    read_as: Option<&PinType>,
    node_id: &str,
    reader_input_port: &str,
) -> Result<Vec<f64>> {
    let channel_values = the_channel_values_a_control_message_carries(PackedPinValueRef {
        payload_descriptor: &message.payload_descriptor,
        payload: message.payload.bytes(),
    });
    let read = match (channel_values, read_as) {
        (Ok(channel_values), None) => Ok(channel_values.into_iter().take(1).collect()),
        (Ok(channel_values), Some(pin_type)) => {
            the_channel_values_a_pin_type_reads(pin_type, &channel_values)
        }
        (Err(refusal), _) => Err(refusal),
    };
    read.map_err(|refusal| {
        Error::Runtime(format!(
            "function node `{node_id}` reads {} off the link it reads at \
             {reader_input_port:?}, and {refusal}",
            read_as.map_or_else(|| "a number".to_owned(), |pin_type| format!("a {pin_type}"))
        ))
    })
}

/// The Records message payload `{"value": <stored JSON form>}` of the value of
/// `pin_type` `message` carries, read as `pin_type` packs it.
pub(super) fn the_record_of_a_stored_value_a_message_carries(
    message: &MessageReadFromInputPort,
    pin_type: &PinType,
    node_id: &str,
    reader_input_port: &str,
) -> Result<Vec<u8>> {
    unpack_pin_value(
        pin_type,
        PackedPinValueRef {
            payload_descriptor: &message.payload_descriptor,
            payload: message.payload.bytes(),
        },
    )
    .and_then(|stored_value| the_records_message_of_a_stored_value(pin_type, &stored_value))
    .map(|record| record.payload)
    .map_err(|refusal| {
        Error::Runtime(format!(
            "function node `{node_id}` reads a {pin_type} off the link it reads at \
             {reader_input_port:?}, and {refusal}"
        ))
    })
}

/// The record `record`, `{"value": <stored JSON form>}` as an island carries a
/// value of `pin_type`, packed as `pin_type` travels on a link.
pub(super) fn the_stored_value_a_record_carries_packed(
    record: &Bound<'_, PyAny>,
    pin_type: &PinType,
    what_it_is: &str,
) -> Result<PackedPinValue> {
    let payload = records_message_payload_of(what_it_is, record, &[])
        .map_err(|refusal| Error::Runtime(refusal.to_string()))?;
    the_stored_value_a_records_message_carries(
        pin_type,
        PackedPinValueRef {
            payload_descriptor: &DataChannelPayloadDescriptor::records_message(),
            payload: &payload,
        },
    )
    .and_then(|stored_value| pack_pin_value(pin_type, &stored_value, 0))
    .map_err(|refusal| Error::Runtime(format!("{what_it_is} is a {pin_type}, and {refusal}")))
}

/// The number a control message carries: the first sample of its first channel.
pub(super) fn the_control_value_a_message_carries(
    message: &MessageReadFromInputPort,
    node_id: &str,
    reader_input_port: &str,
) -> Result<f64> {
    Ok(the_parts_a_message_carries(message, None, node_id, reader_input_port)?[0])
}

/// Reads the newest message at one of the reader's input ports, past any older
/// ones still queued there; `None` when none arrived since the last read.
pub(super) type NewestMessageReaderOfTheReadersInputPorts =
    Box<dyn Fn(&str) -> Result<Option<MessageReadFromInputPort>> + Send + Sync>;

/// The most messages one read of the newest message takes off a port, so a
/// publisher that keeps up with the reads cannot hold an evaluation in the
/// loop; any past it wait for the next read.
const MOST_MESSAGES_ONE_READ_OF_THE_NEWEST_TAKES: usize = 64;

pub(super) fn the_newest_message_read_from(
    input_mailboxes: &InputMailboxesInner,
    reader_input_port: &str,
) -> Result<Option<MessageReadFromInputPort>> {
    let mut newest: Option<MessageReadFromInputPort> = None;
    for _ in 0..MOST_MESSAGES_ONE_READ_OF_THE_NEWEST_TAKES {
        let Some(mut message) = input_mailboxes.read_raw(reader_input_port)? else {
            break;
        };
        if let Some(replaced) = newest {
            message.lost_since_last_read = message
                .lost_since_last_read
                .saturating_add(replaced.lost_since_last_read);
        }
        newest = Some(message);
    }
    Ok(newest)
}
