// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The JSON Schema of the stored value one declared input takes: its type's form from
//! `stored_pin_values`, with what only the declaration knows folded in — an Integer's exact
//! size, a Menu's options, a Vector's size, a List's element, a bundle's members and a number's
//! range. A schema is self-contained, so a client checks a value with it alone.

use serde_json::{Map, Value, json};

use super::stored_value_check::integer_bounds;
use super::{
    BuiltInPinTypeName, BundleMember, DeclaredPinType, FloatSize, PinDeclaration, PinNumberRange,
    PinType, VectorComponentCount,
};

impl PinDeclaration {
    /// The JSON Schema of a stored value an input of this declaration takes, its range folded
    /// in, and `null` beside it on an input that may be unset; `None` where its type is found
    /// with the graph, and where no type it takes holds a stored value.
    pub fn value_json_schema(&self) -> Option<Value> {
        let range = self.annotations.range;
        let schema = match &self.pin_type {
            DeclaredPinType::Is { pin_type } => pin_type.value_json_schema(range)?,
            DeclaredPinType::OneOf { pin_types } => {
                let alternatives: Vec<Value> = pin_types
                    .iter()
                    .filter_map(|pin_type| pin_type.value_json_schema(range))
                    .collect();
                if alternatives.is_empty() {
                    return None;
                }
                json!({ "anyOf": alternatives })
            }
            DeclaredPinType::Any
            | DeclaredPinType::SameAsInput { .. }
            | DeclaredPinType::SetByInput { .. } => return None,
        };
        Some(if self.may_be_unset {
            json!({ "anyOf": [schema, { "type": "null" }] })
        } else {
            schema
        })
    }
}

impl PinType {
    /// The JSON Schema of this type's stored value, a number held within `range` where one is
    /// given; `None` for a type that holds no stored value.
    pub fn value_json_schema(&self, range: Option<PinNumberRange>) -> Option<Value> {
        let schema = match self {
            Self::Boolean {} => json!({ "type": "boolean" }),
            Self::Integer { exact_size } => {
                let (lowest, highest) = integer_bounds(*exact_size);
                let (lowest, highest) = whole_bounds_within(lowest, highest, range);
                json!({
                    "type": "integer",
                    "minimum": a_whole_json_number(lowest),
                    "maximum": a_whole_json_number(highest),
                })
            }
            Self::Float { exact_size } => {
                let mut schema = json!({ "type": "number" });
                let largest = match exact_size {
                    Some(FloatSize::Float32) => f64::from(f32::MAX),
                    Some(FloatSize::Float64) | None => f64::MAX,
                };
                bounded_within(
                    &mut schema,
                    the_tighter_lower_bound(-largest, range),
                    the_tighter_upper_bound(largest, range),
                );
                schema
            }
            Self::String {} => json!({ "type": "string" }),
            Self::Menu { option_names } => json!({ "type": "string", "enum": option_names }),
            Self::Color {} => an_object_of_numbers(&["red", "green", "blue", "alpha"], &[]),
            Self::Vector { component_count } => {
                let (components, unused): (&[&str], &[&str]) = match component_count {
                    VectorComponentCount::Two => (&["x", "y"], &["z", "w"]),
                    VectorComponentCount::Three => (&["x", "y", "z"], &["w"]),
                    VectorComponentCount::Four => (&["x", "y", "z", "w"], &[]),
                };
                let mut schema = an_object_of_numbers(components, &[]);
                for key in unused {
                    schema["properties"][*key] = json!({ "type": "null" });
                }
                schema
            }
            Self::Rectangle {} => {
                an_object_of_numbers(&["x", "y", "width", "height"], &["width", "height"])
            }
            Self::Transform {} => json!({
                "type": "array",
                "items": { "type": "number" },
                "minItems": 16,
                "maxItems": 16,
            }),
            Self::Asset { .. } => json!({
                "type": "object",
                "properties": {
                    "asset": { "type": "string", "format": "uuid" },
                    "from": { "type": "string", "format": "uuid" },
                },
                "required": ["asset"],
                "additionalProperties": false,
            }),
            Self::ListOf { element_type } => json!({
                "type": "array",
                "items": element_type.value_json_schema(None).unwrap_or(Value::Bool(false)),
            }),
            Self::Bundle { members } => a_bundle_object(members),
            Self::NodeReference { .. } => json!({
                "type": "object",
                "properties": { "node": { "type": "string" } },
                "required": ["node"],
                "additionalProperties": false,
            }),
            Self::PinTypeChoice { among } => a_pin_type_choice(among),
            Self::Texture { .. }
            | Self::Audio {}
            | Self::EncodedVideo { .. }
            | Self::EncodedAudio { .. }
            | Self::Tensor {}
            | Self::Trigger {}
            | Self::Named { .. } => return None,
        };
        Some(schema)
    }
}

/// The lower of a type's own bound and `range`'s, whichever is higher.
fn the_tighter_lower_bound(own: f64, range: Option<PinNumberRange>) -> f64 {
    range
        .and_then(|range| range.minimum)
        .map_or(own, |minimum| minimum.max(own))
}

/// The upper of a type's own bound and `range`'s, whichever is lower.
fn the_tighter_upper_bound(own: f64, range: Option<PinNumberRange>) -> f64 {
    range
        .and_then(|range| range.maximum)
        .map_or(own, |maximum| maximum.min(own))
}

/// An Integer's own bounds, `lowest` and `highest`, held within `range` too, each rounded in to
/// the nearest whole number; exact, since an Integer's bounds pass 2^53.
fn whole_bounds_within(lowest: i128, highest: i128, range: Option<PinNumberRange>) -> (i128, i128) {
    let minimum = range.and_then(|range| range.minimum);
    let maximum = range.and_then(|range| range.maximum);
    (
        minimum.map_or(lowest, |minimum| (minimum.ceil() as i128).max(lowest)),
        maximum.map_or(highest, |maximum| (maximum.floor() as i128).min(highest)),
    )
}

/// `whole` as a JSON number, exact; every Integer bound fits an `i64` or a `u64`.
fn a_whole_json_number(whole: i128) -> Value {
    i64::try_from(whole)
        .map(Value::from)
        .or_else(|_| u64::try_from(whole).map(Value::from))
        .unwrap_or_else(|_| json!(whole as f64))
}

/// `schema` given `minimum` and `maximum`, a whole bound written as a whole number.
fn bounded_within(schema: &mut Value, minimum: f64, maximum: f64) {
    schema["minimum"] = a_json_number(minimum);
    schema["maximum"] = a_json_number(maximum);
}

/// `number` as JSON: a whole number within 2^63 as one, else as a float.
fn a_json_number(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() < 9.2e18 {
        json!(number as i64)
    } else {
        json!(number)
    }
}

/// An object holding exactly the number `keys`, each of `at_least_zero` no less than 0.
fn an_object_of_numbers(keys: &[&str], at_least_zero: &[&str]) -> Value {
    let properties: Map<String, Value> = keys
        .iter()
        .map(|key| {
            let schema = if at_least_zero.contains(key) {
                json!({ "type": "number", "minimum": 0 })
            } else {
                json!({ "type": "number" })
            };
            ((*key).to_owned(), schema)
        })
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": keys,
        "additionalProperties": false,
    })
}

/// An object keyed by `members`' names, each in its own type's form; one with a default may be
/// left out.
fn a_bundle_object(members: &[BundleMember]) -> Value {
    let properties: Map<String, Value> = members
        .iter()
        .map(|member| {
            let mut schema = member
                .member_type
                .value_json_schema(None)
                .unwrap_or(Value::Bool(false));
            if let (Some(default_value), Value::Object(schema)) =
                (&member.default_value, &mut schema)
            {
                schema.insert("default".to_owned(), default_value.clone());
            }
            (member.name.clone(), schema)
        })
        .collect();
    let required: Vec<&str> = members
        .iter()
        .filter(|member| member.default_value.is_none())
        .map(|member| member.name.as_str())
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// A pin type: one of `among`, each as the contract writes it, when it names any; else any
/// pin type by a built-in `type` tag.
fn a_pin_type_choice(among: &[PinType]) -> Value {
    if !among.is_empty() {
        return json!({
            "enum": among
                .iter()
                .filter_map(|pin_type| serde_json::to_value(pin_type).ok())
                .collect::<Vec<_>>(),
        });
    }
    json!({
        "type": "object",
        "properties": {
            "type": { "enum": BuiltInPinTypeName::EVERY_NAME },
        },
        "required": ["type"],
    })
}
