// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A typed built-in, rendered as a class node is declared: each value input a field with its
//! type and default, each stream a pin, each described, so `stream_builder.add` checks the
//! values it is given against them. A built-in is typed when it takes no config and every port
//! states exactly one pin type, or carries the type one of its settings chooses.

use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::{BuiltInNodeDescriptor, BuiltInNodePortDescriptor, python_string_literal};

/// One name the generated module imports: the module it comes from, and the name.
pub(super) type ImportedName = (&'static str, &'static str);

const TYPING: &str = "typing";
const CLASS_NODE_PINS: &str = "._class_node_pins";
const DESCRIPTION: &str = "._description";
const INPUT_DECLARATION_ANNOTATIONS: &str = "._input_declaration_annotations";
const PIN_TYPES: &str = "._pin_types";
const PIN_TYPE_CHOICE: &str = "._pin_type_choice";

/// Whether `descriptor` is a typed built-in: its config schema lists no key, and each of its
/// ports states exactly one pin type or carries the type a setting chooses.
pub(super) fn is_a_typed_built_in(descriptor: &BuiltInNodeDescriptor) -> bool {
    let lists_a_config_key = descriptor
        .config_schema
        .as_ref()
        .and_then(|config_schema| config_schema.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|properties| !properties.is_empty());
    !lists_a_config_key
        && descriptor
            .inputs
            .iter()
            .chain(&descriptor.outputs)
            .all(|port| {
                port.exact_pin_type().is_some() || port.the_setting_choosing_its_type().is_some()
            })
}

/// The fields and pins of the typed built-in `descriptor`, each as the class body declares it,
/// and each name they import.
pub(super) fn render_fields_and_pins(
    descriptor: &BuiltInNodeDescriptor,
    refused_item_label: &str,
    imports: &mut BTreeSet<ImportedName>,
) -> Result<String> {
    if let Some(port) = descriptor
        .inputs
        .iter()
        .chain(&descriptor.outputs)
        .find(|port| {
            port.annotations
                .as_ref()
                .is_some_and(|annotations| !annotations.is_empty())
        })
    {
        bail!(
            "{refused_item_label}'s port `{}` states {:?} besides its type, which this generator \
             does not render on a field or pin",
            port.name,
            port.annotations
        );
    }
    let mut text = String::new();
    for input in &descriptor.inputs {
        text.push_str(&render_input(input, refused_item_label, imports)?);
    }
    for output in &descriptor.outputs {
        text.push_str(&render_pin(
            "OutputPin",
            output,
            Vec::new(),
            refused_item_label,
            imports,
        )?);
    }
    Ok(text)
}

fn render_input(
    input: &BuiltInNodePortDescriptor,
    refused_item_label: &str,
    imports: &mut BTreeSet<ImportedName>,
) -> Result<String> {
    let port_label = format!("{refused_item_label}'s input `{}`", input.name);
    if input.the_setting_choosing_its_type().is_some() {
        return render_pin("InputPin", input, Vec::new(), refused_item_label, imports);
    }
    if input.flow.as_deref() != Some("value") {
        let delivery = input
            .delivery_profile
            .as_deref()
            .map(|profile| {
                imports.insert((INPUT_DECLARATION_ANNOTATIONS, "StreamDelivery"));
                python_string_literal(profile, &port_label)
                    .map(|profile| format!("StreamDelivery({profile})"))
            })
            .transpose()?;
        return render_pin(
            "InputPin",
            input,
            delivery.into_iter().collect(),
            refused_item_label,
            imports,
        );
    }
    let mode = match input.input_mode.as_ref() {
        None => None,
        Some(input_mode) if input_mode.mode == "setting_only" => {
            if input_mode.read.as_deref() != Some("at_setup") {
                bail!(
                    "{port_label} is a setting read {:?}, and a built-in's setting is read at \
                     setup",
                    input_mode.read
                );
            }
            Some("SettingOnly")
        }
        Some(input_mode) if input_mode.mode == "wire_only" => Some("WireOnly"),
        Some(input_mode) if input_mode.mode == "wire_or_value" => None,
        Some(input_mode) => bail!("{port_label} takes its value as `{}`", input_mode.mode),
    };
    let pin_type = input
        .exact_pin_type()
        .expect("a typed built-in's setting states one pin type");
    let chooses_a_pin_type =
        pin_type.get("type").and_then(Value::as_str) == Some("pin_type_choice");
    let default = match input
        .default_value
        .as_ref()
        .filter(|value| !value.is_null())
    {
        Some(default) => python_literal(default, &port_label)?,
        None if chooses_a_pin_type => "None".to_owned(),
        None => {
            bail!("{port_label} holds no default, and a field holds its default until it is set")
        }
    };
    imports.insert((TYPING, "Annotated"));
    let mut annotations = vec![if chooses_a_pin_type {
        imports.insert((PIN_TYPE_CHOICE, "PinTypeChoice"));
        "PinTypeChoice | None".to_owned()
    } else {
        python_spelling_of_a_pin_type(pin_type, &port_label, imports)?
    }];
    if let Some(mode) = mode {
        imports.insert((INPUT_DECLARATION_ANNOTATIONS, mode));
        annotations.push(format!("{mode}()"));
    }
    annotations.push(description_annotation(input, &port_label, imports)?);
    let mut text = format!(
        "\n    {}: Annotated[\n",
        python_identifier(input, &port_label)?
    );
    for annotation in annotations {
        writeln!(text, "        {annotation},")?;
    }
    writeln!(text, "    ] = {default}")?;
    Ok(text)
}

fn render_pin(
    pin_form: &'static str,
    port: &BuiltInNodePortDescriptor,
    stated_besides: Vec<String>,
    refused_item_label: &str,
    imports: &mut BTreeSet<ImportedName>,
) -> Result<String> {
    let direction = if pin_form == "InputPin" {
        "input"
    } else {
        "output"
    };
    let port_label = format!("{refused_item_label}'s {direction} `{}`", port.name);
    imports.insert((TYPING, "Annotated"));
    imports.insert((CLASS_NODE_PINS, pin_form));
    let mut annotations = match port.the_setting_choosing_its_type() {
        Some(setting_name) => {
            imports.insert((PIN_TYPE_CHOICE, "PinTypeFromSetting"));
            vec![
                "object".to_owned(),
                format!(
                    "PinTypeFromSetting({})",
                    python_string_literal(setting_name, &port_label)?
                ),
            ]
        }
        None => vec![python_spelling_of_a_pin_type(
            port.exact_pin_type()
                .expect("a typed built-in's port states one pin type"),
            &port_label,
            imports,
        )?],
    };
    annotations.extend(stated_besides);
    annotations.push(description_annotation(port, &port_label, imports)?);
    let mut text = format!(
        "\n    {}: {pin_form}[\n        Annotated[\n",
        python_identifier(port, &port_label)?
    );
    for annotation in annotations {
        writeln!(text, "            {annotation},")?;
    }
    text.push_str("        ]\n    ]\n");
    Ok(text)
}

fn description_annotation(
    port: &BuiltInNodePortDescriptor,
    port_label: &str,
    imports: &mut BTreeSet<ImportedName>,
) -> Result<String> {
    imports.insert((DESCRIPTION, "Description"));
    Ok(format!(
        "Description({})",
        python_string_literal(&port.description, port_label)?
    ))
}

fn python_identifier<'port>(
    port: &'port BuiltInNodePortDescriptor,
    port_label: &str,
) -> Result<&'port str> {
    let name = port.name.as_str();
    if name.is_empty()
        || name.starts_with(|c: char| c.is_ascii_digit() || c == '_')
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        bail!("{port_label} is not a public Python identifier");
    }
    Ok(name)
}

/// How a class node writes `pin_type`, the contract's form of a pin type.
fn python_spelling_of_a_pin_type(
    pin_type: &Value,
    port_label: &str,
    imports: &mut BTreeSet<ImportedName>,
) -> Result<String> {
    let type_name = pin_type.get("type").and_then(Value::as_str).unwrap_or("");
    let exact_size = pin_type.get("exact_size").and_then(Value::as_str);
    let mut a_pin_type_class = |class_name: &'static str| {
        imports.insert((PIN_TYPES, class_name));
        Ok(class_name.to_owned())
    };
    match (type_name, exact_size) {
        ("boolean", None) => Ok("bool".to_owned()),
        ("string", None) => Ok("str".to_owned()),
        ("integer", None) => Ok("int".to_owned()),
        ("float", None) => Ok("float".to_owned()),
        ("integer", Some("int8")) => a_pin_type_class("Int8"),
        ("integer", Some("int16")) => a_pin_type_class("Int16"),
        ("integer", Some("int32")) => a_pin_type_class("Int32"),
        ("integer", Some("uint8")) => a_pin_type_class("UInt8"),
        ("integer", Some("uint16")) => a_pin_type_class("UInt16"),
        ("integer", Some("uint32")) => a_pin_type_class("UInt32"),
        ("float", Some("float32")) => a_pin_type_class("Float32"),
        ("float", Some("float64")) => a_pin_type_class("Float64"),
        ("tensor", None) => a_pin_type_class("Tensor"),
        ("audio", None) => a_pin_type_class("Audio"),
        // A class node names a Texture alone; the formats an input takes are the runtime's to
        // check when a link is made, and no value of one is stored.
        ("texture", None) => a_pin_type_class("Texture"),
        ("trigger", None) => a_pin_type_class("Trigger"),
        ("list_of", None) => {
            let element_type = pin_type
                .get("element_type")
                .filter(|element_type| element_type.is_object())
                .ok_or_else(|| anyhow::anyhow!("{port_label} is a List naming no element type"))?;
            Ok(format!(
                "list[{}]",
                python_spelling_of_a_pin_type(element_type, port_label, imports)?
            ))
        }
        ("menu", None) => {
            let option_names = pin_type
                .get("option_names")
                .and_then(Value::as_array)
                .filter(|option_names| !option_names.is_empty())
                .ok_or_else(|| anyhow::anyhow!("{port_label} is a Menu listing no option"))?;
            let literals = option_names
                .iter()
                .map(|option_name| match option_name {
                    Value::String(option_name) => python_string_literal(option_name, port_label),
                    other => bail!("{port_label} is a Menu listing the option {other}"),
                })
                .collect::<Result<Vec<_>>>()?;
            imports.insert((TYPING, "Literal"));
            Ok(format!("Literal[{}]", literals.join(", ")))
        }
        _ => bail!(
            "{port_label} is of the pin type {pin_type}, which this generator does not render"
        ),
    }
}

/// `value`, a field's default, as Python: a scalar, or a list of them.
fn python_literal(value: &Value, port_label: &str) -> Result<String> {
    match value {
        Value::Bool(true) => Ok("True".to_owned()),
        Value::Bool(false) => Ok("False".to_owned()),
        Value::Number(number) => Ok(number.to_string()),
        Value::String(string) => python_string_literal(string, port_label),
        Value::Array(items) => Ok(format!(
            "[{}]",
            items
                .iter()
                .map(|item| python_literal(item, port_label))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        )),
        other => {
            bail!("{port_label} has the default {other}, which this generator does not render")
        }
    }
}
