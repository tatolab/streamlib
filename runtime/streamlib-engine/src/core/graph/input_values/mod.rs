// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one check of a value set on a node's input, in both modes and before any value of a
//! transaction is applied. An input whose port names its pin type takes what the type takes; a
//! port not yet typed keeps the checks it had before, and a setting its type's config schema
//! lists takes what the schema states of it.

use serde_json::{Map, Value};

use crate::core::descriptors::{NodeClassImportPath, NodeKind, PinNumberRange};
use crate::core::graph::PortInfo;
use crate::core::nodes::{NodeTypesOneStreamResolves, the_config_fields_described_as_settings};

mod bundle_input_read;
mod live_input_values;
mod live_input_values_kept_in_the_project_store;
mod values_wires_carry_into_settings;

pub(crate) use bundle_input_read::{
    ValuesABundleInputHolds, the_bundle_members_of, the_input_values_a_live_set_is_checked_against,
};
pub use live_input_values::{
    InputValueSetLive, InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
    LiveInputValuesOfAStream, a_live_set_replaces_an_earlier_one,
    the_words_of_an_input_set_twice_live,
};
pub(crate) use live_input_values::{
    TakesAWireRendered, the_input_port_takes_a_value, the_input_value_change,
    the_lifetime_of_the_input, the_node_reads_a_value_set_on_the_input_whole,
    the_pin_values_with_the_live_ones_over_them, the_read_of_an_input_a_link_feeds,
    the_settings_rendered_reading_the_values_held, the_unlinked_input_values,
    the_value_a_setting_reads_with_no_wire, the_value_an_input_reads,
};
pub(crate) use live_input_values_kept_in_the_project_store::LiveInputValuesKeptInTheProjectStore;
pub(crate) use values_wires_carry_into_settings::forget_what_a_link_into_a_setting_carried;
pub(crate) use values_wires_carry_into_settings::{
    ValueAWireCarriedIntoASetting, ValuesWiresCarryIntoSettings,
};

/// The number a Channels input reads of `pin_value`: the number, or 1 and 0 for a bool.
pub(crate) fn the_control_value_of_a_pin(pin_value: &Value) -> Option<f64> {
    match pin_value {
        Value::Bool(pin_is_on) => Some(if *pin_is_on { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

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

/// Refuse `value`, set on `node`'s input `input`, or on its member `member` when one is named,
/// with `None` taking it off, when the input does not take it. A member is checked by the type
/// its bundle declares for it, and the bundle the input holds after the set by the input's type.
/// A node of a type `node_types` does not describe keeps any value it is given.
pub fn refuse_an_input_value_its_input_does_not_take(
    node_types: &NodeTypesOneStreamResolves,
    node: &NodeAnInputValueIsSetOn<'_>,
    input: &str,
    member: Option<&str>,
    value: Option<&Value>,
) -> Result<(), InputValueRefusal> {
    let node_type = node.node_type;
    if node_types
        .placeholder_of_the_node_type_not_described(node_type)
        .is_some()
    {
        return Ok(());
    }
    let Some(ports) = node_types.port_info(node_type).map(|ports| {
        ports.resolved_against_the_input_values(node_type, node.input_values_after_the_set)
    }) else {
        return Ok(());
    };
    let settings_its_config_schema_lists = node_types
        .descriptor(node_type)
        .map(|descriptor| the_config_fields_described_as_settings(&descriptor))
        .unwrap_or_default();
    let the_input_port = ports
        .the_input_of_the_type_a_name_finds(input)
        .map(|port| port.typed_by_its_setting_in(&ports.inputs, node.input_values_after_the_set));
    let the_input_port = the_input_port.as_deref();
    if let Some(member) = member {
        return refuse_a_member_value_its_bundle_input_does_not_take(
            node,
            node_types.node_kind(node_type),
            the_input_port,
            input,
            member,
            value,
        );
    }
    if let Some(port) = the_input_port {
        if port.is_a_trigger() {
            return Err(the_refusal_of_a_set_on_a_trigger_input(
                node.name, input, value,
            ));
        }
        let Some(value) = value else {
            return Ok(());
        };
        refuse_a_value_the_input_port_does_not_take(
            node.name,
            node_types.node_kind(node_type),
            port,
            value,
        )?;
        return match port.range() {
            Some(range) => refuse_a_value_holding_a_number_outside_the_range(
                node.name, &port.name, value, range,
            ),
            None => Ok(()),
        };
    }
    let Some(setting) = settings_its_config_schema_lists
        .iter()
        .find(|setting| setting.name == input)
    else {
        let its_inputs: Vec<String> = ports
            .inputs
            .iter()
            .map(|port| port.name.clone())
            .chain(
                settings_its_config_schema_lists
                    .iter()
                    .map(|setting| setting.name.clone()),
            )
            .collect();
        return Err(InputValueRefusal::of(
            InputValueRefusalCode::PortNotFound,
            format!(
                "node `{}` (`{node_type}`) has no input `{input}`; its inputs are {}. To fix: \
                 name one of them",
                node.name,
                names_in_backticks(&its_inputs)
            ),
        ));
    };
    let Some(value) = value else {
        return Ok(());
    };
    refuse_a_value_the_schema_of_a_setting_refuses(&setting.field_value_json_schema, value).map_err(
        |refusal| {
            InputValueRefusal::of(
                InputValueRefusalCode::InputValueNotValid,
                format!(
                    "the value of input `{input}` of `{}` is {value}, and its type refuses it: \
                     node `{}`'s setting `{input}` is refused by what the config schema of its \
                     type `{node_type}` states of it: {refusal}",
                    node.name, node.name
                ),
            )
        },
    )
}

/// Refuse the input values of node `node_name` of `node_type` when the config schema its type
/// states refuses its settings, the values naming no input port of the type, read whole: a value
/// a setting's schema refuses, a setting the schema requires and no value holds, or a name the
/// schema is closed to, as a view's is. A value on an input port is left to the checks of that
/// port, and a `null` value holds nothing.
pub(crate) fn refuse_input_values_the_config_schema_of_its_type_refuses(
    node_types: &NodeTypesOneStreamResolves,
    node_name: &str,
    node_type: &NodeClassImportPath,
    input_values: &Map<String, Value>,
) -> crate::core::error::Result<()> {
    let (Some(config_schema), Some(ports)) = (
        node_types
            .descriptor(node_type)
            .and_then(|descriptor| descriptor.config_schema),
        node_types.port_info(node_type),
    ) else {
        return Ok(());
    };
    let settings: Map<String, Value> = input_values
        .iter()
        .filter(|(name, value)| {
            !value.is_null() && ports.the_input_of_the_type_a_name_finds(name).is_none()
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let refused = |refusal: String| crate::core::error::Error::NodeConfigRefused {
        node_name: node_name.to_owned(),
        node_type: node_type.clone(),
        refusal,
    };
    let validator = jsonschema::validator_for(&config_schema).map_err(|unreadable| {
        refused(format!(
            "its type publishes a config schema the runtime cannot read: {unreadable}"
        ))
    })?;
    let settings = Value::Object(settings);
    let what_the_schema_refuses: Vec<String> = validator
        .iter_errors(&settings)
        .map(|refusal| {
            let setting = refusal.instance_path().to_string();
            if setting.is_empty() {
                refusal.to_string()
            } else {
                format!("`{}`: {refusal}", setting.trim_start_matches('/'))
            }
        })
        .collect();
    if what_the_schema_refuses.is_empty() {
        return Ok(());
    }
    Err(refused(format!(
        "the config schema of its type refuses its settings: {}",
        what_the_schema_refuses.join("; ")
    )))
}

/// Refuse `value` when `setting_schema`, the schema a node type's config schema states of one
/// of its settings, refuses it, saying what it refuses; a schema the runtime cannot read refuses
/// nothing it could check.
fn refuse_a_value_the_schema_of_a_setting_refuses(
    setting_schema: &Value,
    value: &Value,
) -> Result<(), String> {
    let Ok(validator) = jsonschema::validator_for(setting_schema) else {
        return Ok(());
    };
    let what_the_schema_refuses: Vec<String> = validator
        .iter_errors(value)
        .map(|refusal| refusal.to_string())
        .collect();
    if what_the_schema_refuses.is_empty() {
        Ok(())
    } else {
        Err(what_the_schema_refuses.join("; "))
    }
}

/// Refuse `value`, set on member `member` of input `input` of `node`, `port` when the input is a
/// port, with `None` taking it off: refused on an input that is no bundle, for a member the
/// bundle does not declare, for a value the member's type does not take, and when the bundle the
/// input reads after the set, its held members over the input's default and each member's own,
/// leaves out a member, or is not one the input takes.
fn refuse_a_member_value_its_bundle_input_does_not_take(
    node: &NodeAnInputValueIsSetOn<'_>,
    node_kind: Option<NodeKind>,
    port: Option<&PortInfo>,
    input: &str,
    member: &str,
    value: Option<&Value>,
) -> Result<(), InputValueRefusal> {
    let node_name = node.name;
    let Some((port, members)) = port.and_then(|port| Some((port, the_bundle_members_of(port)?)))
    else {
        return Err(InputValueRefusal::of(
            InputValueRefusalCode::InputValueNotValid,
            format!(
                "input `{input}` of `{node_name}` is no bundle, so it has no member `{member}` to \
                 set. To fix: leave `member` out to set the whole input"
            ),
        ));
    };
    let Some(declared) = members.iter().find(|declared| declared.name == member) else {
        return Err(InputValueRefusal::of(
            InputValueRefusalCode::PortNotFound,
            format!(
                "input `{input}` of `{node_name}` is a bundle with no member `{member}`; its \
                 members are {}. To fix: name one of them",
                names_in_backticks(
                    &members
                        .iter()
                        .map(|declared| declared.name.clone())
                        .collect::<Vec<_>>()
                )
            ),
        ));
    };
    let read_after_the_set = node.input_values_after_the_set.get(input).map(|held| {
        ValuesABundleInputHolds {
            live: None,
            saved: Some(held),
            input_default: port.pin_default.as_ref(),
        }
        .the_bundle_read(members)
        .unwrap_or_else(|| Value::Object(Map::new()))
    });
    match value {
        Some(value) => declared
            .member_type
            .check_value(value)
            .map_err(|not_taken| {
                InputValueRefusal::of(
                    InputValueRefusalCode::InputValueNotValid,
                    format!(
                        "the value of member `{member}` of input `{input}` of `{node_name}` is \
                     {value}, and `{member}` takes {}. To fix: send {}",
                        not_taken.takes, not_taken.example
                    ),
                )
            })?,
        None if read_after_the_set
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|read| !read.contains_key(member)) =>
        {
            return Err(InputValueRefusal::of(
                InputValueRefusalCode::InputValueNotValid,
                format!(
                    "member `{member}` of input `{input}` of `{node_name}` declares no default, \
                     nor does the input's default hold it, so the bundle the input holds may not \
                     leave it out. To fix: set `{member}` to a value, or set the whole input"
                ),
            ));
        }
        None => {}
    }
    match &read_after_the_set {
        Some(read) => refuse_a_value_the_input_port_does_not_take(node_name, node_kind, port, read),
        None => Ok(()),
    }
}

/// Why a set on Trigger input `input` of the node named `node_name` is refused, which holds no
/// value: a set of `value` takes none, and a set taking a value off has none to save or take off.
/// Only a live set of no value is taken, and fires the input.
pub(crate) fn the_refusal_of_a_set_on_a_trigger_input(
    node_name: &str,
    input: &str,
    value: Option<&Value>,
) -> InputValueRefusal {
    let to_fire_it = "To fire it: send `set_node_input_value` with no value in a `live` \
                      transaction of value sets alone, which fires it once";
    match value.filter(|value| !value.is_null()) {
        Some(value) => InputValueRefusal::of(
            InputValueRefusalCode::InputValueNotValid,
            format!(
                "the value of input `{input}` of `{node_name}` is {value}, and `{input}` is a \
                 Trigger, which carries an event and no value. {to_fire_it}"
            ),
        ),
        None => InputValueRefusal::of(
            InputValueRefusalCode::InputTakesNoValue,
            format!(
                "input `{input}` of `{node_name}` is a Trigger, which carries an event and holds \
                 no value to save. {to_fire_it}"
            ),
        ),
    }
}

/// Refuse `value` set on `port`, an input port of the node named `node_name` that runs as
/// `node_kind`, when the port does not take it: what its pin type takes and, on a node that
/// does not read a value set on the input whole, a number or a bool on a Channels input, which
/// is all such a node's read carries; `null` on an input that may be unset, which reads as nothing; on a port
/// that names no pin type, a function node takes any value.
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
            Some(pin) => format!(
                "input `{input}` of `{node_name}` carries {}, and a pin value is a control value, \
                 which only an input of a number, a Boolean or a value of parts reads. To fix: \
                 link `{input}` instead",
                pin.pin_type
            ),
            None => format!(
                "input `{input}` of `{node_name}` carries no pin type yet, and a pin value is a \
                 control value, which only an input of a number, a Boolean or a value of parts \
                 reads. To fix: link `{input}` instead"
            ),
        };
        return Err(InputValueRefusal::of(
            InputValueRefusalCode::InputTakesNoValue,
            words,
        ));
    }
    if port.is_a_setting() && port.pin.is_none() {
        // A setting its schema alone types is checked by that schema when its node is set up.
        return Ok(());
    }
    if let Some(checked) = port.check_value(value) {
        checked.map_err(|not_taken| {
            InputValueRefusal::of(
                InputValueRefusalCode::InputValueNotValid,
                format!(
                    "the value of input `{input}` of `{node_name}` is {value}, and `{input}` \
                     takes {}. To fix: send {}",
                    not_taken.takes, not_taken.example
                ),
            )
        })?;
        // A setting's value lands in the node's settings, never on a Channels wire, so its pin
        // type alone judges it.
        if value.is_null()
            || port.is_a_setting()
            || the_node_reads_a_value_set_on_the_input_whole(node_kind, port)
        {
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

/// Refuse `value` set on input `input` of `node_name` when a number it holds lies outside
/// `range`, naming the first that does; it is never clamped. A range bounds a number, or every
/// number of an array, nested arrays included.
fn refuse_a_value_holding_a_number_outside_the_range(
    node_name: &str,
    input: &str,
    value: &Value,
    range: PinNumberRange,
) -> Result<(), InputValueRefusal> {
    let mut numbers = Vec::new();
    every_number_a_value_holds(value, &mut numbers);
    let Some(number) = numbers.into_iter().find(|number| {
        range.minimum.is_some_and(|minimum| *number < minimum)
            || range.maximum.is_some_and(|maximum| *number > maximum)
    }) else {
        return Ok(());
    };
    let what_it_is = if value.is_number() {
        format!("is {number}")
    } else {
        format!("is {value}, which holds {number}")
    };
    let takes = match (range.minimum, range.maximum) {
        (Some(minimum), Some(maximum)) => format!("a number from {minimum} to {maximum}"),
        (Some(minimum), None) => format!("a number of {minimum} or more"),
        (None, Some(maximum)) => format!("a number of {maximum} or less"),
        (None, None) => "a number".to_owned(),
    };
    Err(InputValueRefusal::of(
        InputValueRefusalCode::InputValueNotValid,
        format!(
            "the value of input `{input}` of `{node_name}` {what_it_is}, and `{input}` takes {takes}"
        ),
    ))
}

/// Push each number `value` holds onto `numbers`, in order: itself when it is one, and each
/// number of an array, nested arrays included. An object's members are not ranged.
fn every_number_a_value_holds(value: &Value, numbers: &mut Vec<f64>) {
    match value {
        Value::Number(number) => numbers.extend(number.as_f64()),
        Value::Array(elements) => {
            for element in elements {
                every_number_a_value_holds(element, numbers);
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Object(_) => {}
    }
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
mod connection_input_tests;
#[cfg(test)]
mod function_and_shader_node_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod values_wires_carry_into_settings_tests;
