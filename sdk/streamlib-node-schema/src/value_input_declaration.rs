// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a value input of a Rust node function is declared with: the pin type of the value it
//! holds, a list it is chosen from, or the bundles its stream declares, and the input itself.

use serde_json::Value;

use crate::descriptors::PortDescriptor;
use crate::pin_type::{IntegerSize, NodeInputMode, PinDeclaration, PinType, SettingReadTime};

/// The pin type a setting holding a value of this Rust type is declared with.
#[diagnostic::on_unimplemented(
    message = "a setting of type `{Self}` has no pin type",
    note = "a setting is a bool, a String, an integer of 8, 16 or 32 bits, an f32 or f64, a Vec of one, or an `Option<PinType>` choosing a pin type; and a value input of a node written as one function chooses a String from a list with `options = [\"…\", …]`"
)]
pub trait SettingPinType {
    /// The pin type a setting of this type holds.
    fn setting_pin_type() -> PinType;
}

impl SettingPinType for bool {
    fn setting_pin_type() -> PinType {
        PinType::Boolean {}
    }
}

impl SettingPinType for String {
    fn setting_pin_type() -> PinType {
        PinType::String {}
    }
}

impl SettingPinType for f32 {
    fn setting_pin_type() -> PinType {
        PinType::Float { exact_size: None }
    }
}

impl SettingPinType for f64 {
    fn setting_pin_type() -> PinType {
        PinType::Float { exact_size: None }
    }
}

macro_rules! integer_setting_pin_type {
    ($($rust_type:ty => $size:ident),* $(,)?) => {
        $(
            impl SettingPinType for $rust_type {
                fn setting_pin_type() -> PinType {
                    PinType::Integer { exact_size: Some(IntegerSize::$size) }
                }
            }
        )*
    };
}

integer_setting_pin_type! {
    i8 => Int8,
    i16 => Int16,
    i32 => Int32,
    u8 => UInt8,
    u16 => UInt16,
    u32 => UInt32,
}

/// A setting choosing any pin type, which holds none until one is chosen.
impl SettingPinType for Option<PinType> {
    fn setting_pin_type() -> PinType {
        PinType::a_choice_of_any()
    }
}

impl<Element: SettingPinType> SettingPinType for Vec<Element> {
    fn setting_pin_type() -> PinType {
        PinType::ListOf {
            element_type: Box::new(Element::setting_pin_type()),
        }
    }
}

/// A value input chosen from a list of names, which `options = ["…", …]` declares.
#[diagnostic::on_unimplemented(
    message = "`options = […]` marks a value input of type `{Self}`, and only a String is chosen from a list"
)]
pub trait SettingChosenFromOptions {}

impl SettingChosenFromOptions for String {}

/// The Menu a setting of type `T`, chosen from `option_names`, is declared with.
pub fn the_menu_of_a_setting<T: SettingChosenFromOptions>(option_names: &[&str]) -> PinType {
    PinType::Menu {
        option_names: option_names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// A value input choosing one of the bundles its stream's node types declare, which
/// `among_the_bundles_its_stream_declares = true` declares on a value input.
#[diagnostic::on_unimplemented(
    message = "`among_the_bundles_its_stream_declares = true` marks a value input of type `{Self}`, and only an `Option<PinType>` chooses a pin type"
)]
pub trait SettingChoosingAPinType {}

impl SettingChoosingAPinType for Option<PinType> {}

/// The pin type choice a setting of type `T` is declared with when it chooses one of the bundles
/// its stream's node types declare, which the runtime lists for a placed node.
pub fn the_choice_among_the_bundles_its_stream_declares<T: SettingChoosingAPinType>() -> PinType {
    PinType::a_choice_among_the_bundles_its_stream_declares()
}

/// The input `name` taking no wire, declared as: described by `description`, of `pin_type`,
/// holding `default` until it is set, and read at `read` — at setup for a setting, or each run
/// for a value input of a node written as one function; either way a set reaches the node
/// where it runs. An input whose default is `null` holds nothing until it is set, so it may be
/// unset.
pub fn the_input_taking_no_wire(
    name: &str,
    description: &str,
    pin_type: PinType,
    default: Value,
    read: SettingReadTime,
) -> PortDescriptor {
    let may_be_unset = default.is_null();
    let setting = PortDescriptor::new(
        name,
        description,
        false,
        PinDeclaration {
            may_be_unset,
            ..PinDeclaration::exactly(pin_type)
        },
    )
    .with_input_mode(NodeInputMode::SettingOnly { read });
    let mut setting = if may_be_unset {
        setting
    } else {
        setting.with_pin_default(default)
    };
    // The engine hands the node its value, so no message is delivered to it.
    setting.delivery_profile = None;
    setting
}
