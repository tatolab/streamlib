// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Rust node type's settings: a struct whose every field is an input read once, when the node
//! is set up, from its value or from a Value wire into it, so a change rebuilds the node. `#[node_settings]` derives
//! [`NodeSettings`] for the struct, and `#[node(settings = …)]` declares each field as one of
//! the node's inputs and builds the struct from the values they hold before the node is set up.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::descriptors::PortDescriptor;
use crate::pin_type::{IntegerSize, NodeInputMode, PinDeclaration, PinType, SettingReadTime};

#[diagnostic::on_unimplemented(
    message = "`{Self}` is a node `settings =` type but is not declared with `#[node_settings]`",
    note = "add `#[streamlib::sdk::node_settings]` to `{Self}`, which declares each of its fields as a setting the node reads at setup"
)]
pub trait NodeSettings: Default + Serialize + DeserializeOwned + Send + 'static {
    /// Each setting, in the order the struct declares its fields, as an input of the node read
    /// at setup, holding its field's default until it is set or a wire into it carries a value.
    fn setting_inputs() -> Vec<PortDescriptor>;

    /// The settings `input_values` hold, each input's value by its name: a setting they name
    /// no value for keeps its default, and every other value is left to the inputs it names.
    /// Refused, naming the setting, when a value is not of its field's type.
    fn read_from_the_input_values(input_values: &Map<String, Value>) -> Result<Self, String> {
        let mut settings = match serde_json::to_value(Self::default()) {
            Ok(Value::Object(defaults)) => defaults,
            Ok(other) => {
                return Err(format!(
                    "the settings serialize as {other}, and settings are a struct of named \
                     fields"
                ));
            }
            Err(refusal) => {
                return Err(format!(
                    "the settings' default does not serialize: {refusal}"
                ));
            }
        };
        for setting in Self::setting_inputs() {
            if let Some(value) = input_values
                .get(&setting.name)
                .filter(|value| !value.is_null())
            {
                settings.insert(setting.name, value.clone());
            }
        }
        serde_json::from_value(Value::Object(settings)).map_err(|refusal| {
            format!("a setting holds a value its field does not take: {refusal}")
        })
    }
}

/// The pin type a setting holding a value of this Rust type is declared with.
#[diagnostic::on_unimplemented(
    message = "a setting of type `{Self}` has no pin type",
    note = "a setting is a bool, a String, an integer of 8, 16 or 32 bits, an f32 or f64, a Vec of one, or an `Option<PinType>` choosing a pin type; a String chosen from a list is marked `#[options(\"…\", …)]`"
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

/// A setting chosen from a list of names, which `#[options("…", …)]` declares on a field.
#[diagnostic::on_unimplemented(
    message = "`#[options(…)]` marks a setting of type `{Self}`, and only a String setting is chosen from a list"
)]
pub trait SettingChosenFromOptions {}

impl SettingChosenFromOptions for String {}

/// The Menu a setting of type `T`, chosen from `option_names`, is declared with.
pub fn the_menu_of_a_setting<T: SettingChosenFromOptions>(option_names: &[&str]) -> PinType {
    PinType::Menu {
        option_names: option_names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// A setting choosing one of the bundles its stream's node types declare, which
/// `#[among_the_bundles_its_stream_declares]` declares on a field.
#[diagnostic::on_unimplemented(
    message = "`#[among_the_bundles_its_stream_declares]` marks a setting of type `{Self}`, and only an `Option<PinType>` setting chooses a pin type"
)]
pub trait SettingChoosingAPinType {}

impl SettingChoosingAPinType for Option<PinType> {}

/// The pin type choice a setting of type `T` is declared with when it chooses one of the bundles
/// its stream's node types declare, which the runtime lists for a placed node.
pub fn the_choice_among_the_bundles_its_stream_declares<T: SettingChoosingAPinType>() -> PinType {
    PinType::a_choice_among_the_bundles_its_stream_declares()
}

/// The input setting `name` is declared as: described by `description`, of `pin_type`, holding
/// `default` until it is set, taking no wire and read when its node is set up. A setting whose
/// default is `null` holds nothing until it is set, so it may be unset.
pub fn the_input_of_a_setting(
    name: &str,
    description: &str,
    pin_type: PinType,
    default: Value,
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
    .with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::AtSetup,
    });
    let mut setting = if may_be_unset {
        setting
    } else {
        setting.with_pin_default(default)
    };
    // The engine reads a wire into a setting in the node's place, so no message is delivered
    // to it.
    setting.delivery_profile = None;
    setting
}

/// The default of field `name` of the settings `defaults` serialize to; `Null` when they hold
/// none under that name.
pub fn the_default_of_a_setting(defaults: &Value, name: &str) -> Value {
    defaults.get(name).cloned().unwrap_or(Value::Null)
}
