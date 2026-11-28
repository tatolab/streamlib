// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a value input of a Rust node function is declared with: the pin type of the value it
//! holds, a list it is chosen from, or the bundles its stream declares, and the input itself.

use serde_json::Value;

use crate::descriptors::PortDescriptor;
use crate::pin_type::{IntegerSize, NodeInputMode, PinDeclaration, PinType, SettingReadTime};

/// The pin type a value input holding a value of this Rust type is declared with.
#[diagnostic::on_unimplemented(
    message = "a value input of type `{Self}` has no pin type",
    note = "a value input holds a bool, a String, an integer of 8, 16 or 32 bits, an f32 or f64, a Vec of one, or an `Option<PinType>` choosing a pin type; and it chooses a String from a list with `options = [\"…\", …]`"
)]
pub trait ValueInputPinType {
    /// The pin type a value input of this type holds.
    fn value_input_pin_type() -> PinType;
}

impl ValueInputPinType for bool {
    fn value_input_pin_type() -> PinType {
        PinType::Boolean {}
    }
}

impl ValueInputPinType for String {
    fn value_input_pin_type() -> PinType {
        PinType::String {}
    }
}

impl ValueInputPinType for f32 {
    fn value_input_pin_type() -> PinType {
        PinType::Float { exact_size: None }
    }
}

impl ValueInputPinType for f64 {
    fn value_input_pin_type() -> PinType {
        PinType::Float { exact_size: None }
    }
}

macro_rules! integer_value_input_pin_type {
    ($($rust_type:ty => $size:ident),* $(,)?) => {
        $(
            impl ValueInputPinType for $rust_type {
                fn value_input_pin_type() -> PinType {
                    PinType::Integer { exact_size: Some(IntegerSize::$size) }
                }
            }
        )*
    };
}

integer_value_input_pin_type! {
    i8 => Int8,
    i16 => Int16,
    i32 => Int32,
    u8 => UInt8,
    u16 => UInt16,
    u32 => UInt32,
}

/// A value input choosing any pin type, which holds none until one is chosen.
impl ValueInputPinType for Option<PinType> {
    fn value_input_pin_type() -> PinType {
        PinType::a_choice_of_any()
    }
}

impl<Element: ValueInputPinType> ValueInputPinType for Vec<Element> {
    fn value_input_pin_type() -> PinType {
        PinType::ListOf {
            element_type: Box::new(Element::value_input_pin_type()),
        }
    }
}

/// A value input chosen from a list of names, which `options = ["…", …]` declares.
#[diagnostic::on_unimplemented(
    message = "`options = […]` marks a value input of type `{Self}`, and only a String is chosen from a list"
)]
pub trait ValueInputChosenFromOptions {}

impl ValueInputChosenFromOptions for String {}

/// The Menu a value input of type `T`, chosen from `option_names`, is declared with.
pub fn the_menu_of_a_value_input<T: ValueInputChosenFromOptions>(option_names: &[&str]) -> PinType {
    PinType::Menu {
        option_names: option_names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// A value input choosing one of the bundles its stream's node types declare, which
/// `among_the_bundles_its_stream_declares = true` declares on a value input.
#[diagnostic::on_unimplemented(
    message = "`among_the_bundles_its_stream_declares = true` marks a value input of type `{Self}`, and only an `Option<PinType>` chooses a pin type"
)]
pub trait ValueInputChoosingAPinType {}

impl ValueInputChoosingAPinType for Option<PinType> {}

/// The pin type choice a value input of type `T` is declared with when it chooses one of the bundles
/// its stream's node types declare, which the runtime lists for a placed node.
pub fn the_choice_among_the_bundles_its_stream_declares<T: ValueInputChoosingAPinType>() -> PinType
{
    PinType::a_choice_among_the_bundles_its_stream_declares()
}

/// The value input `name` taking no wire, declared as: described by `description`, of
/// `pin_type`, holding `default` until it is set, and read by the node each run, so a set reaches
/// it where it runs. An input whose default is `null` holds nothing until it is set, so it may
/// be unset.
pub fn the_input_taking_no_wire(
    name: &str,
    description: &str,
    pin_type: PinType,
    default: Value,
) -> PortDescriptor {
    let may_be_unset = default.is_null();
    let value_input = PortDescriptor::new(
        name,
        description,
        false,
        PinDeclaration {
            may_be_unset,
            ..PinDeclaration::exactly(pin_type)
        },
    )
    .with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::EachRun,
    });
    let mut value_input = if may_be_unset {
        value_input
    } else {
        value_input.with_pin_default(default)
    };
    // The engine hands the node its value, so no message is delivered to it.
    value_input.delivery_profile = None;
    value_input
}
