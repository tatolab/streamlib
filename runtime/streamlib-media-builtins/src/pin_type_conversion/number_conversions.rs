// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The built-in number conversions: a Float, an Integer and a Boolean, each cast into each of the
//! other two by one node, which reads each message it is sent and gives the newest cast. Each
//! states `conversion` with the cast [`the_number_cast`] makes, so a saved value retyped from one
//! into another is cast the way a message through the node is.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::{PinConversionSavedValueCast, PinType, the_number_cast};
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::{ChannelsBlockDescriptorBody, DataChannelPayloadDescriptor};
use streamlib::sdk::nodes::ReactiveNode;
use streamlib::sdk::pin_value_packing::{
    PackedPinValue, PackedPinValueRef, pack_pin_value, unpack_pin_value,
};

const BOOLEAN: PinType = PinType::Boolean {};
const INTEGER: PinType = PinType::Integer { exact_size: None };
const FLOAT: PinType = PinType::Float { exact_size: None };

/// The cast each number conversion supplies, the one [`the_number_cast`] makes.
pub const THE_NUMBER_CAST: PinConversionSavedValueCast = PinConversionSavedValueCast {
    cast: the_number_cast,
};

/// The message, `payload_descriptor` and `payload`, of a value of `from`, as the message of that
/// value cast into `into`, starting at the sample it starts at; or why it is not cast.
pub fn the_message_cast(
    from: &PinType,
    into: &PinType,
    payload_descriptor: &DataChannelPayloadDescriptor,
    payload: &[u8],
) -> std::result::Result<PackedPinValue, String> {
    let value = unpack_pin_value(
        from,
        PackedPinValueRef {
            payload_descriptor,
            payload,
        },
    )?;
    let start_sample =
        ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body).start_sample;
    pack_pin_value(into, &the_number_cast(&value, into)?, start_sample)
}

/// The input each number conversion reads.
const INPUT_PORT: &str = "value";
/// The output each number conversion writes.
const OUTPUT_PORT: &str = "cast";

/// Read each message on [`INPUT_PORT`] of `$node`, of the conversion `$name`, a value of `$from`,
/// and write it cast into `$into` on [`OUTPUT_PORT`]; a message not cast is logged and skipped.
macro_rules! cast_each_message {
    ($node:expr, $name:ident, $from:expr, $into:expr) => {{
        while let Some(message) = $node.inputs.read_raw(INPUT_PORT)? {
            match the_message_cast(
                &$from,
                &$into,
                &message.payload_descriptor,
                message.payload.bytes(),
            ) {
                Ok(cast) => $node.outputs.write_bytes_described_by(
                    OUTPUT_PORT,
                    &cast.payload,
                    message.timestamp_ns,
                    message.group_boundary,
                    cast.payload_descriptor,
                )?,
                Err(refusal) => {
                    tracing::warn!(
                        %refusal,
                        concat!(stringify!($name), " wrote nothing for a message")
                    )
                }
            }
        }
        Ok(())
    }};
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "value",
        description = "The Floats to cast",
        delivery_profile = "ordered",
        flow = Stream,
        pin_type = Float
    ),
    output(
        "cast",
        description = "Each Float truncated toward zero, held to an Integer's range",
        pin_type = Integer
    ),
)]
#[meta(
    conversion = true,
    description = "Casts each Float on `value` into an Integer on `cast`, truncated toward \
                   zero and held to the range an Integer holds: 2.9 becomes 2 and -2.9 becomes \
                   -2, as a link from a Float into an Integer reads it."
)]
pub struct FloatToInteger;

impl ReactiveNode for FloatToInteger::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        cast_each_message!(self, FloatToInteger, FLOAT, INTEGER)
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "value",
        description = "The Integers to cast",
        delivery_profile = "ordered",
        flow = Stream,
        pin_type = Integer
    ),
    output(
        "cast",
        description = "Each Integer as the Float of the same value",
        pin_type = Float
    ),
)]
#[meta(
    conversion = true,
    description = "Casts each Integer on `value` into the Float of the same value on `cast`: \
                   2 becomes 2.0. Every value an Integer holds is a Float exactly."
)]
pub struct IntegerToFloat;

impl ReactiveNode for IntegerToFloat::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        cast_each_message!(self, IntegerToFloat, INTEGER, FLOAT)
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "value",
        description = "The Booleans to cast",
        delivery_profile = "ordered",
        flow = Stream,
        pin_type = Boolean
    ),
    output(
        "cast",
        description = "Each Boolean as 0 or 1",
        pin_type = Integer
    ),
)]
#[meta(
    conversion = true,
    description = "Casts each Boolean on `value` into an Integer on `cast`: false becomes 0 \
                   and true becomes 1."
)]
pub struct BooleanToInteger;

impl ReactiveNode for BooleanToInteger::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        cast_each_message!(self, BooleanToInteger, BOOLEAN, INTEGER)
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "value",
        description = "The Integers to cast",
        delivery_profile = "ordered",
        flow = Stream,
        pin_type = Integer
    ),
    output(
        "cast",
        description = "Each Integer as false when it is 0, true otherwise",
        pin_type = Boolean
    ),
)]
#[meta(
    conversion = true,
    description = "Casts each Integer on `value` into a Boolean on `cast`: 0 becomes false \
                   and any other value true."
)]
pub struct IntegerToBoolean;

impl ReactiveNode for IntegerToBoolean::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        cast_each_message!(self, IntegerToBoolean, INTEGER, BOOLEAN)
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "value",
        description = "The Booleans to cast",
        delivery_profile = "ordered",
        flow = Stream,
        pin_type = Boolean
    ),
    output(
        "cast",
        description = "Each Boolean as 0.0 or 1.0",
        pin_type = Float
    ),
)]
#[meta(
    conversion = true,
    description = "Casts each Boolean on `value` into a Float on `cast`: false becomes 0.0 \
                   and true becomes 1.0."
)]
pub struct BooleanToFloat;

impl ReactiveNode for BooleanToFloat::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        cast_each_message!(self, BooleanToFloat, BOOLEAN, FLOAT)
    }
}

#[streamlib::sdk::node(
    built_in_node,
    execution = reactive,
    input(
        "value",
        description = "The Floats to cast",
        delivery_profile = "ordered",
        flow = Stream,
        pin_type = Float
    ),
    output(
        "cast",
        description = "Each Float as false when it is 0, true otherwise",
        pin_type = Boolean
    ),
)]
#[meta(
    conversion = true,
    description = "Casts each Float on `value` into a Boolean on `cast`: 0.0 becomes false \
                   and any other value true."
)]
pub struct FloatToBoolean;

impl ReactiveNode for FloatToBoolean::Node {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        cast_each_message!(self, FloatToBoolean, FLOAT, BOOLEAN)
    }
}
