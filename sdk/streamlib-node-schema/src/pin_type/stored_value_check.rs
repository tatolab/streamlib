// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Whether a JSON value is a stored value of a pin type, in the form `stored_pin_values` gives
//! each type. The check is exact: nothing is filled in, converted or clamped.

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::stored_pin_values::{
    AssetPinValue, ColorPinValue, RectanglePinValue, TransformPinValue, VectorPinValue,
};
use super::{
    DeclaredPinType, FloatSize, IntegerSize, PinDeclaration, PinType, PinTypesAChoiceTakes,
    VectorComponentCount,
};
use crate::node_reference_in_config::NodeReferenceInConfig;

/// The largest whole number a JSON number holds exactly, 2^53.
const LARGEST_EXACT_WHOLE_NUMBER: i64 = 1 << 53;

/// Why a value is not a stored value of a pin type: what the type takes, and one value it does.
#[derive(Debug, Clone, PartialEq)]
pub struct PinValueNotTaken {
    /// What the type takes, in words: `a whole number from 0 to 255`.
    pub takes: String,
    /// One value the type takes, as a caller would send it.
    pub example: Value,
}

impl PinDeclaration {
    /// `value` taken as a stored value of a port of this declaration, or what it takes instead:
    /// `null` on an input that may be unset, which then reads as nothing; else a value of the
    /// type it declares, or of one of the types it takes. A type found with the graph, as the
    /// type of a link or the one an input chooses, is checked once the graph resolves it.
    pub fn check_value(&self, value: &Value) -> Result<(), PinValueNotTaken> {
        if self.may_be_unset && value.is_null() {
            return Ok(());
        }
        match &self.pin_type {
            DeclaredPinType::Is { pin_type } => pin_type.check_value(value),
            DeclaredPinType::OneOf { pin_types } => {
                if pin_types
                    .iter()
                    .any(|pin_type| pin_type.is_a_stored_value(value))
                {
                    return Ok(());
                }
                Err(PinValueNotTaken {
                    takes: pin_types
                        .iter()
                        .map(PinType::what_it_takes)
                        .collect::<Vec<_>>()
                        .join(", or "),
                    example: pin_types
                        .first()
                        .map_or(Value::Null, PinType::an_example_value),
                })
            }
            DeclaredPinType::Any
            | DeclaredPinType::SameAsInput { .. }
            | DeclaredPinType::SetByInput { .. } => Ok(()),
        }
    }
}

impl PinType {
    /// Whether an input of this type holds a value while no link feeds it. A type that carries
    /// only messages, such as a Texture, an Audio stream or a Trigger, holds none, nor does a
    /// List of one. A bundle holding one holds the values of its other members, while a link
    /// carries the heavy member.
    pub fn holds_a_stored_value(&self) -> bool {
        match self {
            Self::ListOf { .. } if self.holds_a_heavy_member() => false,
            Self::Bundle { members, .. } if self.holds_a_heavy_member() => members
                .iter()
                .any(|member| !member.is_carried_only_by_a_link()),
            Self::Texture { .. }
            | Self::Audio {}
            | Self::EncodedVideo { .. }
            | Self::EncodedAudio { .. }
            | Self::Tensor {}
            | Self::Trigger {}
            | Self::Named { .. }
            | Self::Connection { .. } => false,
            Self::Boolean {}
            | Self::Integer { .. }
            | Self::Float { .. }
            | Self::String {}
            | Self::Menu { .. }
            | Self::Color {}
            | Self::Vector { .. }
            | Self::Rectangle {}
            | Self::Transform {}
            | Self::Asset { .. }
            | Self::ListOf { .. }
            | Self::Bundle { .. }
            | Self::NodeReference { .. }
            | Self::PinTypeChoice { .. } => true,
        }
    }

    /// `value` taken as a stored value of this type, or what the type takes instead. A type
    /// that holds no stored value takes none, but a Trigger takes `null`, which a live set
    /// sends to fire it.
    pub fn check_value(&self, value: &Value) -> Result<(), PinValueNotTaken> {
        if matches!(self, Self::Trigger {}) && value.is_null() {
            return Ok(());
        }
        if self.is_a_stored_value(value) {
            return Ok(());
        }
        Err(PinValueNotTaken {
            takes: self.what_it_takes(),
            example: self.an_example_value(),
        })
    }

    fn is_a_stored_value(&self, value: &Value) -> bool {
        match self {
            Self::Boolean {} => value.is_boolean(),
            Self::Integer { exact_size } => {
                let (lowest, highest) = integer_bounds(*exact_size);
                match value {
                    Value::Number(number) => the_whole_number_of(number)
                        .is_some_and(|whole| (lowest..=highest).contains(&whole)),
                    _ => false,
                }
            }
            Self::Float { exact_size } => value.as_f64().is_some_and(|number| {
                number.is_finite()
                    && (*exact_size != Some(FloatSize::Float32)
                        || number.abs() <= f64::from(f32::MAX))
            }),
            Self::String {} => value.is_string(),
            Self::Menu { option_names } => value
                .as_str()
                .is_some_and(|chosen| option_names.iter().any(|option| option == chosen)),
            Self::Color {} => reads_as::<ColorPinValue>(value),
            Self::Vector { component_count } => {
                serde_json::from_value::<VectorPinValue>(value.clone()).is_ok_and(|vector| {
                    match component_count {
                        VectorComponentCount::Two => vector.z.is_none() && vector.w.is_none(),
                        VectorComponentCount::Three => vector.z.is_some() && vector.w.is_none(),
                        VectorComponentCount::Four => vector.z.is_some() && vector.w.is_some(),
                    }
                })
            }
            Self::Rectangle {} => serde_json::from_value::<RectanglePinValue>(value.clone())
                .is_ok_and(|rectangle| rectangle.width >= 0.0 && rectangle.height >= 0.0),
            Self::Transform {} => reads_as::<TransformPinValue>(value),
            Self::Asset { .. } => reads_as::<AssetPinValue>(value),
            Self::ListOf { element_type } => value.as_array().is_some_and(|elements| {
                elements
                    .iter()
                    .all(|element| element_type.is_a_stored_value(element))
            }),
            Self::Bundle { members, .. } => value.as_object().is_some_and(|stored| {
                stored
                    .keys()
                    .all(|key| members.iter().any(|member| &member.name == key))
                    && members.iter().all(|member| match stored.get(&member.name) {
                        Some(member_value) => member.member_type.is_a_stored_value(member_value),
                        None => !member.a_stored_value_must_name_it(),
                    })
            }),
            Self::NodeReference { .. } => reads_as::<NodeReferenceInConfig>(value),
            Self::PinTypeChoice { .. } => serde_json::from_value::<PinType>(value.clone())
                .is_ok_and(|chosen| {
                    self.the_pin_types_a_choice_takes()
                        .is_some_and(|choice| choice.takes(&chosen))
                }),
            Self::Texture { .. }
            | Self::Audio {}
            | Self::EncodedVideo { .. }
            | Self::EncodedAudio { .. }
            | Self::Tensor {}
            | Self::Trigger {}
            | Self::Named { .. }
            | Self::Connection { .. } => false,
        }
    }

    fn what_it_takes(&self) -> String {
        match self {
            Self::Boolean {} => "true or false".to_owned(),
            Self::Integer { exact_size } => {
                let (lowest, highest) = integer_bounds(*exact_size);
                format!("a whole number from {lowest} to {highest}")
            }
            Self::Float {
                exact_size: Some(FloatSize::Float32),
            } => "a number a 32-bit float holds".to_owned(),
            Self::Float { .. } => "a number".to_owned(),
            Self::String {} => "text".to_owned(),
            Self::Menu { option_names } => format!(
                "one of {}",
                option_names
                    .iter()
                    .map(|option| format!("`{option}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Color {} => "a colour, `{red, green, blue, alpha}`".to_owned(),
            Self::Vector { component_count } => format!(
                "a vector, `{}`",
                match component_count {
                    VectorComponentCount::Two => "{x, y}",
                    VectorComponentCount::Three => "{x, y, z}",
                    VectorComponentCount::Four => "{x, y, z, w}",
                }
            ),
            Self::Rectangle {} => {
                "a rectangle, `{x, y, width, height}` with no width or height below 0".to_owned()
            }
            Self::Transform {} => "a transform, a list of 16 numbers row by row".to_owned(),
            Self::Asset { .. } => "an asset, `{\"asset\": \"<uuid>\"}`".to_owned(),
            Self::ListOf { element_type } => {
                format!("a list, each element {}", element_type.what_it_takes())
            }
            Self::Bundle { members, .. } => {
                let names = |carried_only_by_a_link: bool| {
                    members
                        .iter()
                        .filter(|member| {
                            member.is_carried_only_by_a_link() == carried_only_by_a_link
                        })
                        .map(|member| format!("`{}`", member.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let carried_only_by_a_link = names(true);
                if carried_only_by_a_link.is_empty() {
                    format!("an object of the members {}", names(false))
                } else {
                    format!(
                        "an object of the members {}, as {carried_only_by_a_link} takes only a link",
                        names(false)
                    )
                }
            }
            Self::NodeReference { .. } => "a node reference, `{\"node\": \"<name>\"}`".to_owned(),
            Self::PinTypeChoice { .. } => match self.the_pin_types_a_choice_takes() {
                Some(PinTypesAChoiceTakes::OneOfTheBundlesItsStreamDeclares([])) => {
                    "one of the bundles its stream's node types declare, and they declare none"
                        .to_owned()
                }
                Some(PinTypesAChoiceTakes::OneOfTheBundlesItsStreamDeclares(bundles)) => format!(
                    "one of the bundles its stream's node types declare: {}",
                    bundles
                        .iter()
                        .map(PinType::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                _ => "a pin type, `{\"type\": \"<name>\"}`".to_owned(),
            },
            Self::Trigger {} => {
                "no value: a live set with no value, or `null`, fires it once".to_owned()
            }
            Self::Texture { .. }
            | Self::Audio {}
            | Self::EncodedVideo { .. }
            | Self::EncodedAudio { .. }
            | Self::Tensor {}
            | Self::Named { .. }
            | Self::Connection { .. } => "no value, only a link".to_owned(),
        }
    }

    fn an_example_value(&self) -> Value {
        match self {
            Self::Boolean {} => json!(true),
            Self::Integer { .. } => json!(0),
            Self::Float { .. } => json!(0.5),
            Self::String {} => json!(""),
            Self::Menu { option_names } => option_names
                .first()
                .map_or(Value::Null, |first| json!(first)),
            Self::Color {} => json!({ "red": 1.0, "green": 1.0, "blue": 1.0, "alpha": 1.0 }),
            Self::Vector { component_count } => match component_count {
                VectorComponentCount::Two => json!({ "x": 0.0, "y": 0.0 }),
                VectorComponentCount::Three => json!({ "x": 0.0, "y": 0.0, "z": 0.0 }),
                VectorComponentCount::Four => json!({ "x": 0.0, "y": 0.0, "z": 0.0, "w": 0.0 }),
            },
            Self::Rectangle {} => json!({ "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0 }),
            Self::Transform {} => json!([
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0
            ]),
            Self::Asset { .. } => json!({ "asset": "00000000-0000-0000-0000-000000000000" }),
            Self::ListOf { .. } => json!([]),
            Self::Bundle { members, .. } => Value::Object(
                members
                    .iter()
                    .filter(|member| !member.is_carried_only_by_a_link())
                    .map(|member| (member.name.clone(), member.member_type.an_example_value()))
                    .collect(),
            ),
            Self::NodeReference { .. } => json!({ "node": "<name>" }),
            Self::PinTypeChoice { .. } => match self.the_pin_types_a_choice_takes() {
                Some(
                    PinTypesAChoiceTakes::OneOf([first, ..])
                    | PinTypesAChoiceTakes::OneOfTheBundlesItsStreamDeclares([first, ..]),
                ) => serde_json::to_value(first).unwrap_or(Value::Null),
                Some(PinTypesAChoiceTakes::OneOfTheBundlesItsStreamDeclares([])) => Value::Null,
                _ => json!({ "type": "float" }),
            },
            Self::Texture { .. }
            | Self::Audio {}
            | Self::EncodedVideo { .. }
            | Self::EncodedAudio { .. }
            | Self::Tensor {}
            | Self::Trigger {}
            | Self::Named { .. }
            | Self::Connection { .. } => Value::Null,
        }
    }
}

fn reads_as<StoredValue: DeserializeOwned>(value: &Value) -> bool {
    serde_json::from_value::<StoredValue>(value.clone()).is_ok()
}

/// The lowest and highest whole number an Integer of `exact_size` holds; exact to 2^53 when it
/// states no size.
pub(super) fn integer_bounds(exact_size: Option<IntegerSize>) -> (i128, i128) {
    match exact_size {
        None => (
            -i128::from(LARGEST_EXACT_WHOLE_NUMBER),
            i128::from(LARGEST_EXACT_WHOLE_NUMBER),
        ),
        Some(IntegerSize::Int8) => (i8::MIN.into(), i8::MAX.into()),
        Some(IntegerSize::Int16) => (i16::MIN.into(), i16::MAX.into()),
        Some(IntegerSize::Int32) => (i32::MIN.into(), i32::MAX.into()),
        Some(IntegerSize::Int64) => (i64::MIN.into(), i64::MAX.into()),
        Some(IntegerSize::UInt8) => (0, u8::MAX.into()),
        Some(IntegerSize::UInt16) => (0, u16::MAX.into()),
        Some(IntegerSize::UInt32) => (0, u32::MAX.into()),
        Some(IntegerSize::UInt64) => (0, u64::MAX.into()),
    }
}

/// The whole number `number` holds; `None` for one with a fraction.
pub(super) fn the_whole_number_of(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

/// `whole` as a JSON number, which holds every whole number an Integer of any size holds; one
/// past those is held at the nearest end.
pub(super) fn a_whole_json_number(whole: i128) -> Value {
    match (i64::try_from(whole), u64::try_from(whole)) {
        (Ok(signed), _) => Value::from(signed),
        (_, Ok(unsigned)) => Value::from(unsigned),
        _ if whole < 0 => Value::from(i64::MIN),
        _ => Value::from(u64::MAX),
    }
}
