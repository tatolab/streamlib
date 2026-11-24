// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The settings a node type's config schema lists, a view's `settings` or a test's: each field is
//! a setting of the type, an input that takes no wire, read when the node is set up, with the
//! default the schema states, a pin type only where the schema states one exactly, and the
//! field's own schema as the schema of a value it takes. A node holds each as one of its input
//! values; `graph` still renders them under its `config`.

use std::borrow::Cow;
use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::core::descriptors::{
    IntegerSize, NodeClassImportPath, NodeDescriptor, PinAnnotations, PinNumberRange,
    PinProjectAssetAnnotation, PinType,
};
use crate::core::error::{Error, Result};
use crate::core::nodes::is_a_natively_compiled_node_type;

/// Each field `config_schema` lists, by name, with its schema; `None` for no schema, or one
/// listing no `properties`.
fn the_properties_of_a_config_schema(config_schema: Option<&Value>) -> Option<&Map<String, Value>> {
    config_schema?.get("properties")?.as_object()
}

/// The name of each setting `config_schema` lists; none for no schema.
pub(crate) fn the_settings_a_config_schema_lists(config_schema: Option<&Value>) -> HashSet<String> {
    the_properties_of_a_config_schema(config_schema)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

/// What `schema` says of what it describes, its `description`; empty when it says nothing.
fn the_description_of(schema: &Value) -> &str {
    schema
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The schema keyword a config field holding a project asset's UUID as a bare string carries;
/// its value is the field's [`PinProjectAssetAnnotation`].
const PROJECT_ASSET_SCHEMA_KEYWORD: &str = "x-tatolab-project-asset";

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
    /// What the schema states of the field besides its type: on a String holding a project
    /// asset's UUID, the assets it is picked from; on a number, the range it holds.
    pub(crate) annotations: PinAnnotations,
    /// The JSON Schema of a value the field takes: its own schema, standing alone, the config
    /// schema's definitions it refers to carried beside it.
    pub(crate) field_value_json_schema: Value,
}

/// Each field the config schema of `descriptor` lists, in the order it lists them, described as
/// an input that takes no wire; none for a type that takes no config.
pub(crate) fn the_config_fields_described_as_settings(
    descriptor: &NodeDescriptor,
) -> Vec<ConfigFieldDescribedAsASetting> {
    let Some(config_schema) = descriptor.config_schema.as_ref() else {
        return Vec::new();
    };
    let Some(properties) = the_properties_of_a_config_schema(Some(config_schema)) else {
        return Vec::new();
    };
    let defaults_of_the_whole_config = config_schema.get("default").and_then(Value::as_object);
    properties
        .iter()
        .map(|(name, field_schema)| {
            let field_schema =
                the_field_schema_with_its_reference_resolved(config_schema, field_schema);
            let pin_type = the_pin_type_a_field_schema_states_exactly(&field_schema);
            let annotations = PinAnnotations {
                project_asset: pin_type
                    .as_ref()
                    .filter(|pin_type| matches!(pin_type, PinType::String {}))
                    .and_then(|_| the_project_asset_annotation_of(&field_schema)),
                range: pin_type
                    .as_ref()
                    .filter(|pin_type| {
                        matches!(pin_type, PinType::Integer { .. } | PinType::Float { .. })
                    })
                    .and_then(|_| the_number_range_of(&field_schema)),
                ..PinAnnotations::default()
            };
            ConfigFieldDescribedAsASetting {
                name: name.clone(),
                description: the_description_of(&field_schema).to_owned(),
                default_value: field_schema
                    .get("default")
                    .or_else(|| {
                        defaults_of_the_whole_config.and_then(|defaults| defaults.get(name))
                    })
                    .filter(|default| !default.is_null())
                    .cloned(),
                field_value_json_schema: the_field_schema_standing_alone(
                    config_schema,
                    &field_schema,
                ),
                pin_type,
                annotations,
            }
        })
        .collect()
}

/// The keywords a config schema holds the definitions its fields refer to under.
const DEFINITIONS_KEYWORDS: [&str; 2] = ["$defs", "definitions"];

/// `field_schema`, a field of `config_schema` with its own reference resolved, as a schema that
/// stands alone: each set of definitions the config schema holds is carried beside it when it
/// still refers to one, so a `#/$defs/...` inside it resolves as it did in the config schema.
fn the_field_schema_standing_alone(config_schema: &Value, field_schema: &Value) -> Value {
    let mut standing_alone = field_schema.clone();
    if !refers_to_a_definition(field_schema) {
        return standing_alone;
    }
    if let Value::Object(keywords) = &mut standing_alone {
        for definitions_keyword in DEFINITIONS_KEYWORDS {
            if let Some(definitions) = config_schema.get(definitions_keyword) {
                keywords
                    .entry(definitions_keyword)
                    .or_insert_with(|| definitions.clone());
            }
        }
    }
    standing_alone
}

/// Whether `schema` holds a `$ref` anywhere inside it.
fn refers_to_a_definition(schema: &Value) -> bool {
    match schema {
        Value::Object(keywords) => keywords
            .iter()
            .any(|(keyword, value)| keyword == "$ref" || refers_to_a_definition(value)),
        Value::Array(members) => members.iter().any(refers_to_a_definition),
        _ => false,
    }
}

/// The range a number field's schema holds it within, by its `minimum` and `maximum`; `None`
/// when it states neither.
fn the_number_range_of(field_schema: &Value) -> Option<PinNumberRange> {
    let range = PinNumberRange {
        minimum: field_schema.get("minimum").and_then(Value::as_f64),
        maximum: field_schema.get("maximum").and_then(Value::as_f64),
    };
    (range.minimum.is_some() || range.maximum.is_some()).then_some(range)
}

/// The config fields a node of `node_type`, whose config schema is `config_schema`, can expose
/// as inputs, each with its description: the number and Boolean fields, which
/// `set_exposed_config_fields` takes; none for a natively compiled type, whose code reads no
/// config field as an input.
pub(crate) fn the_config_fields_a_node_of_the_type_can_expose(
    node_type: &NodeClassImportPath,
    config_schema: Option<&Value>,
) -> Vec<(String, String)> {
    if is_a_natively_compiled_node_type(node_type) {
        return Vec::new();
    }
    the_properties_of_a_config_schema(config_schema)
        .into_iter()
        .flatten()
        .filter(|(_, field_schema)| {
            matches!(
                field_schema.get("type").and_then(Value::as_str),
                Some("number" | "integer" | "boolean")
            )
        })
        .map(|(field, field_schema)| (field.clone(), the_description_of(field_schema).to_owned()))
        .collect()
}

/// Refuse `descriptor` when its config schema lists a field named as one of its input ports: a
/// value set under that name would be the config's and the input's at once.
pub(crate) fn refuse_a_config_field_named_as_an_input_port(
    descriptor: &NodeDescriptor,
) -> Result<()> {
    let Some(properties) = the_properties_of_a_config_schema(descriptor.config_schema.as_ref())
    else {
        return Ok(());
    };
    for input in &descriptor.inputs {
        if properties.contains_key(&input.name) {
            return Err(Error::Configuration(format!(
                "node type `{}` names the config field `{name}` and the input port `{name}` \
                 alike; a value set on `{name}` would be both. To fix: rename the config field \
                 or the input port",
                descriptor.node_class_import_path,
                name = input.name
            )));
        }
        let former_names = input
            .declared_pin()
            .map(|declared| declared.former_names)
            .unwrap_or_default();
        if let Some(former_name) = former_names
            .iter()
            .find(|former_name| properties.contains_key(former_name.as_str()))
        {
            return Err(Error::Configuration(format!(
                "node type `{}` names the config field `{former_name}`, and the input port `{}` \
                 lists `{former_name}` as a name it had; a value saved under `{former_name}` \
                 would be both. To fix: rename the config field or drop the name the port had",
                descriptor.node_class_import_path, input.name
            )));
        }
    }
    Ok(())
}

/// `field_schema` with a `$ref` into the config schema's definitions, or the single `allOf` a
/// schema wraps such a reference in to describe it, replaced by what it points to; the keys
/// written beside the reference win.
fn the_field_schema_with_its_reference_resolved<'schema>(
    config_schema: &Value,
    field_schema: &'schema Value,
) -> Cow<'schema, Value> {
    let reference = field_schema
        .get("$ref")
        .or_else(|| match field_schema.get("allOf") {
            Some(Value::Array(all_of)) => match all_of.as_slice() {
                [only] => only.get("$ref"),
                _ => None,
            },
            _ => None,
        });
    let Some(pointed_to) = reference
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| config_schema.pointer(pointer))
        .and_then(Value::as_object)
    else {
        return Cow::Borrowed(field_schema);
    };
    let mut resolved = pointed_to.clone();
    if let Some(beside_the_reference) = field_schema.as_object() {
        for (key, value) in beside_the_reference {
            if key != "$ref" && key != "allOf" {
                resolved.insert(key.clone(), value.clone());
            }
        }
    }
    Cow::Owned(Value::Object(resolved))
}

/// The pin type `field_schema` states exactly: a Boolean, an Integer of 8, 16 or 32 bits, a
/// Float, a String, or a Menu of the strings an `enum` or a `oneOf` of single strings lists.
/// `None` for anything else, so no guessed type refuses a value the schema takes today: an
/// object, a list, a node reference, a field that may be null, and an integer of no stated size
/// or of 64 bits, which holds numbers past the 2^53 a single Integer holds.
fn the_pin_type_a_field_schema_states_exactly(field_schema: &Value) -> Option<PinType> {
    if let Some(option_names) = the_option_names_of_a_menu(field_schema) {
        return Some(PinType::Menu { option_names });
    }
    match field_schema.get("type").and_then(Value::as_str)? {
        "boolean" => Some(PinType::Boolean {}),
        "number" => Some(PinType::Float { exact_size: None }),
        "string" => Some(PinType::String {}),
        "integer" => field_schema
            .get("format")
            .and_then(Value::as_str)
            .and_then(the_integer_size_of_a_format)
            .map(|exact_size| PinType::Integer {
                exact_size: Some(exact_size),
            }),
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

/// The assets a field holding a project asset's UUID is picked from, as its schema's
/// `x-tatolab-project-asset` keyword states them; `None` for a field that carries no such keyword,
/// or one naming a medium no asset holds.
fn the_project_asset_annotation_of(field_schema: &Value) -> Option<PinProjectAssetAnnotation> {
    field_schema
        .get(PROJECT_ASSET_SCHEMA_KEYWORD)
        .and_then(|keyword| serde_json::from_value(keyword.clone()).ok())
}
