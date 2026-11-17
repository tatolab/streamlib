// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one check of a value set on a node's input, in both modes and before any value of a
//! transaction is applied. An input whose port names its pin type takes what the type takes; a
//! port not yet typed, and a setting a node type not yet typed holds in its config, keep the
//! checks they had before.

use serde_json::{Map, Value};

use crate::core::compiler::compiler_ops::input_port_pin_values::the_control_value_of_a_pin;
use crate::core::descriptors::{NodeClassImportPath, NodeKind, PinNumberRange};
use crate::core::graph::PortInfo;
use crate::core::graph::legacy_config_of_a_node::LegacyConfigOfANodeType;
use crate::core::nodes::NodeTypesOneStreamResolves;

mod live_input_values;

pub use live_input_values::{
    InputValueComesFrom, InputValueRead, InputValueSetLive, InputValueToSetLive,
    InputValuesSetLive, LiveInputValueSetRefusal, LiveInputValuesOfAStream,
};
pub(crate) use live_input_values::{
    the_input_port_takes_a_value, the_input_value_change,
    the_pin_values_with_the_live_ones_over_them, the_value_an_input_reads,
};

/// What kind of value a refused set sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputValueRefusalCode {
    /// The node has no input or setting of the name sent.
    PortNotFound,
    /// The input takes only a link.
    InputTakesNoValue,
    /// The value is not one the input takes.
    InputValueNotValid,
}

/// Why a value set on a node's input is refused, in the runtime's words, which name the node and
/// the input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{words}")]
pub struct InputValueRefusal {
    /// What kind of refusal it is.
    pub code: InputValueRefusalCode,
    /// Why, naming the node and the input.
    pub words: String,
}

impl InputValueRefusal {
    fn of(code: InputValueRefusalCode, words: String) -> Self {
        Self { code, words }
    }
}

/// One node a value is set on: its name, its type and every input value it holds once the set
/// is made.
pub struct NodeAnInputValueIsSetOn<'node> {
    /// The node's name.
    pub name: &'node str,
    /// The node's type.
    pub node_type: &'node NodeClassImportPath,
    /// Each input value the node holds once the set is made.
    pub input_values_after_the_set: &'node Map<String, Value>,
}

/// Refuse `value`, set on `node`'s input `input` with `None` taking it off, when the input does
/// not take it. A node of a type `node_types` does not describe keeps any value it is given.
pub fn refuse_an_input_value_its_input_does_not_take(
    node_types: &NodeTypesOneStreamResolves,
    node: &NodeAnInputValueIsSetOn<'_>,
    input: &str,
    value: Option<&Value>,
) -> Result<(), InputValueRefusal> {
    let node_type = node.node_type;
    if node_types
        .placeholder_of_the_node_type_not_described(node_type)
        .is_some()
    {
        return Ok(());
    }
    let Some((input_ports, _)) = node_types.port_info(node_type) else {
        return Ok(());
    };
    let legacy_config = LegacyConfigOfANodeType::of(node_types, node_type);
    let descriptor = node_types.descriptor(node_type);
    let config_schema = descriptor
        .as_ref()
        .and_then(|descriptor| descriptor.config_schema.clone());
    let the_input_port = input_ports
        .iter()
        .find(|port| port.name == input)
        .filter(|_| !legacy_config.is_a_config_field(input));
    if let Some(port) = the_input_port {
        let Some(value) = value else {
            return Ok(());
        };
        refuse_a_value_the_input_port_does_not_take(
            node.name,
            node_types.node_kind(node_type),
            port,
            value,
        )?;
        let declared_range = descriptor
            .as_ref()
            .and_then(|descriptor| {
                descriptor
                    .inputs
                    .iter()
                    .find(|declared| declared.name == input)
            })
            .and_then(|declared| declared.declared_pin())
            .and_then(|declaration| declaration.annotations.range);
        return match (declared_range, value.as_f64()) {
            (Some(range), Some(number)) => {
                refuse_a_number_outside_the_range(node.name, input, number, range)
            }
            _ => Ok(()),
        };
    }
    let config_schema_lists_it = config_schema
        .as_ref()
        .and_then(|config_schema| config_schema.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key(input));
    if !config_schema_lists_it {
        let mut its_inputs: Vec<String> =
            input_ports.iter().map(|port| port.name.clone()).collect();
        its_inputs.extend(
            config_schema
                .as_ref()
                .and_then(|config_schema| config_schema.get("properties"))
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|properties| properties.keys().cloned()),
        );
        return Err(InputValueRefusal::of(
            InputValueRefusalCode::PortNotFound,
            format!(
                "node `{}` (`{node_type}`) has no input `{input}`; its inputs are {}. To fix: \
                 name one of them",
                node.name,
                names_in_backticks(&its_inputs)
            ),
        ));
    }
    let config = legacy_config.config_of(node.input_values_after_the_set);
    crate::core::graph::refuse_a_config_its_node_types_config_schema_refuses(
        node.name, node_type, node_types, &config,
    )
    .map_err(|refusal| {
        InputValueRefusal::of(
            InputValueRefusalCode::InputValueNotValid,
            format!(
                "the value of input `{input}` of `{}` is {}, and its type refuses it: {refusal}",
                node.name,
                value.map_or_else(|| "taken off".to_owned(), Value::to_string)
            ),
        )
    })
}

/// Refuse `value` set on `port`, an input port of the node named `node_name` that runs as
/// `node_kind`, when the port does not take it: what its pin type takes and, on a node that is
/// no function or shader node, a number or a bool on a Channels input, which is all such a
/// node's read carries; on a port that names no pin type, a function node takes any value.
pub(crate) fn refuse_a_value_the_input_port_does_not_take(
    node_name: &str,
    node_kind: Option<NodeKind>,
    port: &PortInfo,
    value: &Value,
) -> Result<(), InputValueRefusal> {
    let input = &port.name;
    if !the_input_port_takes_a_value(node_kind, port) {
        let words = match &port.pin {
            Some(pin) if !pin.pin_type.holds_a_stored_value() => format!(
                "input `{input}` of `{node_name}` is wire only and takes no value. To fix: link it"
            ),
            _ => format!(
                "input `{input}` of `{node_name}` carries {}, and a pin value is a control value, \
                 which only a Channels input reads. To fix: link `{input}` instead",
                port.family.family
            ),
        };
        return Err(InputValueRefusal::of(
            InputValueRefusalCode::InputTakesNoValue,
            words,
        ));
    }
    if let Some(pin) = &port.pin {
        pin.pin_type.check_value(value).map_err(|not_taken| {
            InputValueRefusal::of(
                InputValueRefusalCode::InputValueNotValid,
                format!(
                    "the value of input `{input}` of `{node_name}` is {value}, and `{input}` \
                     takes {}. To fix: send {}",
                    not_taken.takes, not_taken.example
                ),
            )
        })?;
        if matches!(
            node_kind,
            Some(NodeKind::Function { .. } | NodeKind::Shader)
        ) {
            return Ok(());
        }
    } else if matches!(node_kind, Some(NodeKind::Function { .. })) {
        return Ok(());
    }
    if the_control_value_of_a_pin(value).is_none() {
        return Err(InputValueRefusal::of(
            InputValueRefusalCode::InputValueNotValid,
            format!(
                "the pin value of input `{input}` of `{node_name}` is {value}, and a Channels \
                 input reads a number or a bool"
            ),
        ));
    }
    Ok(())
}

/// Whether a node of `node_type` still takes a config: its config schema lists a field, or
/// `node_types` does not describe the type.
pub fn node_type_takes_a_config(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
) -> bool {
    node_types
        .placeholder_of_the_node_type_not_described(node_type)
        .is_some()
        || node_types.port_info(node_type).is_none()
        || LegacyConfigOfANodeType::of(node_types, node_type).takes_a_config()
}

/// Whether an exposure of `exposed_config_fields` on a node of `node_type` is recorded: true for
/// a type that still takes a config; false for a type that takes none, on which each name must
/// be one of its inputs.
pub fn an_exposure_of_config_fields_is_recorded_on(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
    exposed_config_fields: &[String],
) -> crate::core::Result<bool> {
    if node_type_takes_a_config(node_types, node_type) {
        return Ok(true);
    }
    let input_ports = node_types
        .port_info(node_type)
        .map(|(input_ports, _)| input_ports)
        .unwrap_or_default();
    let not_inputs: Vec<String> = exposed_config_fields
        .iter()
        .filter(|name| !input_ports.iter().any(|port| &port.name == *name))
        .cloned()
        .collect();
    if !not_inputs.is_empty() {
        return Err(crate::core::Error::Configuration(format!(
            "`{node_type}` takes no config, so it has no config field to expose, and {} {} not \
             one of its inputs, which are {}",
            names_in_backticks(&not_inputs),
            if not_inputs.len() == 1 { "is" } else { "are" },
            names_in_backticks(
                &input_ports
                    .iter()
                    .map(|port| port.name.clone())
                    .collect::<Vec<_>>()
            )
        )));
    }
    Ok(false)
}

/// Refuse `number`, set on the input `input` of the node named `node_name`, when it lies outside
/// `range`; it is never clamped.
fn refuse_a_number_outside_the_range(
    node_name: &str,
    input: &str,
    number: f64,
    range: PinNumberRange,
) -> Result<(), InputValueRefusal> {
    let below = range.minimum.is_some_and(|minimum| number < minimum);
    let above = range.maximum.is_some_and(|maximum| number > maximum);
    if !below && !above {
        return Ok(());
    }
    let takes = match (range.minimum, range.maximum) {
        (Some(minimum), Some(maximum)) => format!("a number from {minimum} to {maximum}"),
        (Some(minimum), None) => format!("a number of {minimum} or more"),
        (None, Some(maximum)) => format!("a number of {maximum} or less"),
        (None, None) => "a number".to_owned(),
    };
    Err(InputValueRefusal::of(
        InputValueRefusalCode::InputValueNotValid,
        format!(
            "the value of input `{input}` of `{node_name}` is {number}, and `{input}` takes {takes}"
        ),
    ))
}

fn names_in_backticks(names: &[String]) -> String {
    if names.is_empty() {
        return "none".to_owned();
    }
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod function_and_shader_node_tests;
#[cfg(test)]
mod tests;
