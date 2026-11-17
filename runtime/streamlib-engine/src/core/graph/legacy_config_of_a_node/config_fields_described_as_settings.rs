// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the node catalog says of the config of a node type not yet typed: each field its config
//! schema lists is an input that takes no wire, read when the node is set up, with the default
//! the schema states and a pin type only where the schema states one exactly.

use serde_json::Value;

use crate::core::descriptors::{IntegerSize, NodeDescriptor, PinType};
use crate::core::error::{Error, Result};
use crate::core::graph::exposed_config_field_input_ports::the_number_and_bool_fields_of;
use crate::core::nodes::NODE_REGISTRY;

/// One config field of a node type not yet typed, as the catalog describes it: an input that
/// takes no wire, read when the node is set up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfigFieldDescribedAsASetting {
    /// The field's name, which `set_node_input_value` names as the input.
    pub(crate) name: String,
    /// What the field's schema says of it.
    pub(crate) description: String,
    /// The value the schema states a node holds when it is not set; `None` when it states none.
    pub(crate) default_value: Option<Value>,
    /// The type the schema states exactly; `None` for a field whose schema holds free-form data,
    /// may be null, or states a type no pin type holds exactly.
    pub(crate) pin_type: Option<PinType>,
}

/// Each field the config schema of `descriptor` lists, in the order it lists them, described as
/// an input that takes no wire; none for a type that takes no config.
pub(crate) fn the_config_fields_described_as_settings(
    descriptor: &NodeDescriptor,
) -> Vec<ConfigFieldDescribedAsASetting> {
    let Some(config_schema) = descriptor.config_schema.as_ref() else {
        return Vec::new();
    };
    let Some(properties) = config_schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let defaults_of_the_whole_config = config_schema.get("default").and_then(Value::as_object);
    properties
        .iter()
        .map(|(name, field_schema)| {
            let field_schema =
                the_field_schema_with_its_reference_resolved(config_schema, field_schema);
            ConfigFieldDescribedAsASetting {
                name: name.clone(),
                description: field_schema
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                default_value: field_schema
                    .get("default")
                    .or_else(|| {
                        defaults_of_the_whole_config.and_then(|defaults| defaults.get(name))
                    })
                    .filter(|default| !default.is_null())
                    .cloned(),
                pin_type: the_pin_type_a_field_schema_states_exactly(&field_schema),
            }
        })
        .collect()
}

/// The number and Boolean config fields a node of `descriptor`'s type can expose as inputs, the
/// fields `set_exposed_config_fields` takes on it; none for a natively compiled type, whose code
/// reads no config field as an input.
pub(crate) fn the_config_fields_a_node_of_the_type_can_expose(
    descriptor: &NodeDescriptor,
) -> Vec<String> {
    if NODE_REGISTRY
        .descriptor(&descriptor.node_class_import_path)
        .is_some()
    {
        return Vec::new();
    }
    the_number_and_bool_fields_of(descriptor.config_schema.as_ref())
        .into_iter()
        .map(|(field, _)| field)
        .collect()
}

/// Refuse `descriptor` when its config schema lists a field named as one of its input ports: a
/// value set under that name would be the config's and the input's at once.
pub(crate) fn refuse_a_config_field_named_as_an_input_port(
    descriptor: &NodeDescriptor,
) -> Result<()> {
    let Some(properties) = descriptor
        .config_schema
        .as_ref()
        .and_then(|config_schema| config_schema.get("properties"))
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    match descriptor
        .inputs
        .iter()
        .find(|input| properties.contains_key(&input.name))
    {
        Some(input) => Err(Error::Configuration(format!(
            "node type `{}` names the config field `{name}` and the input port `{name}` alike; a \
             value set on `{name}` would be both. To fix: rename the config field or the input \
             port",
            descriptor.node_class_import_path,
            name = input.name
        ))),
        None => Ok(()),
    }
}

/// `field_schema` with a `$ref` into the config schema's definitions, or the single `allOf` a
/// schema wraps such a reference in to describe it, replaced by what it points to; the keys
/// written beside the reference win.
fn the_field_schema_with_its_reference_resolved(
    config_schema: &Value,
    field_schema: &Value,
) -> Value {
    let reference = field_schema
        .get("$ref")
        .or_else(|| match field_schema.get("allOf") {
            Some(Value::Array(all_of)) if all_of.len() == 1 => all_of[0].get("$ref"),
            _ => None,
        });
    let Some(pointed_to) = reference
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| config_schema.pointer(pointer))
        .and_then(Value::as_object)
    else {
        return field_schema.clone();
    };
    let mut resolved = pointed_to.clone();
    if let Some(beside_the_reference) = field_schema.as_object() {
        for (key, value) in beside_the_reference {
            if key != "$ref" && key != "allOf" {
                resolved.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(resolved)
}

/// The pin type `field_schema` states exactly: a Boolean, an Integer, a Float, a String, or a
/// Menu of the strings an `enum` or a `oneOf` of single strings lists. `None` for anything else,
/// so no guessed type refuses a value the schema takes today: an object, a list, a node
/// reference, a field that may be null, and an integer of 64 bits, which no single Integer
/// holds exactly.
fn the_pin_type_a_field_schema_states_exactly(field_schema: &Value) -> Option<PinType> {
    if let Some(option_names) = the_option_names_of_a_menu(field_schema) {
        return Some(PinType::Menu { option_names });
    }
    match field_schema.get("type").and_then(Value::as_str)? {
        "boolean" => Some(PinType::Boolean {}),
        "number" => Some(PinType::Float { exact_size: None }),
        "string" => Some(PinType::String {}),
        "integer" => match field_schema.get("format").and_then(Value::as_str) {
            None => Some(PinType::Integer { exact_size: None }),
            Some(format) => {
                the_integer_size_of_a_format(format).map(|exact_size| PinType::Integer {
                    exact_size: Some(exact_size),
                })
            }
        },
        _ => None,
    }
}

/// The size a single Integer holds a schema's integer `format` in; `None` for a 64-bit or
/// pointer-sized format, which holds numbers past 2^53.
fn the_integer_size_of_a_format(format: &str) -> Option<IntegerSize> {
    match format {
        "int8" => Some(IntegerSize::Int8),
        "int16" => Some(IntegerSize::Int16),
        "int32" => Some(IntegerSize::Int32),
        "uint8" => Some(IntegerSize::UInt8),
        "uint16" => Some(IntegerSize::UInt16),
        "uint32" => Some(IntegerSize::UInt32),
        _ => None,
    }
}

/// The options of a field whose schema takes one of a list of strings and nothing else: an
/// `enum` of strings, or a `oneOf` each of whose members takes one string, by `const` or by an
/// `enum` of one.
fn the_option_names_of_a_menu(field_schema: &Value) -> Option<Vec<String>> {
    if field_schema
        .get("type")
        .is_some_and(|stated| stated != "string")
    {
        return None;
    }
    let option_names: Vec<String> = if let Some(Value::Array(options)) = field_schema.get("enum") {
        options
            .iter()
            .map(|option| option.as_str().map(str::to_owned))
            .collect::<Option<_>>()?
    } else if let Some(Value::Array(members)) = field_schema.get("oneOf") {
        members
            .iter()
            .map(|member| match (member.get("const"), member.get("enum")) {
                (Some(Value::String(option)), None) => Some(option.clone()),
                (None, Some(Value::Array(options))) => match options.as_slice() {
                    [Value::String(option)] => Some(option.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Option<_>>()?
    } else {
        return None;
    };
    (!option_names.is_empty()).then_some(option_names)
}
