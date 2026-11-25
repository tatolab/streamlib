// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `family` setting of a `GraphInput` and a `GraphOutput`, which named the family of its
//! port while it chose no pin type. A saved document opens with the port choosing the pin type
//! that family implied: a Texture for Image, a Tensor for Array, and for Channels the type of
//! the value a `GraphInput` saved. Any other family implied no one type, so the port takes the
//! type of its first link, as one that names nothing does.

use serde_json::{Map, Value, json};

use super::NODE_INPUT_VALUES_KEY;
use crate::stream_graph_document::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_PIN_TYPE_SETTING, GRAPH_INTERFACE_VALUE_PORT_NAME,
    GRAPH_OUTPUT_NODE_TYPE,
};

/// The setting an interface node named its port's family under.
const THE_FAMILY_SETTING: &str = "family";

/// The name a `GraphInput`'s saved `value` had before it was an input.
const THE_FORMER_NAME_OF_THE_VALUE: &str = "default";

/// Turn the family `node`, a `GraphInput` or a `GraphOutput`, saved into the pin type it implied,
/// in place; whether it held one. A node already choosing a pin type keeps it, and the family is
/// dropped.
pub(super) fn the_family_of_an_interface_node_chosen(node: &mut Map<String, Value>) -> bool {
    let is_an_interface_node = node
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|node_type| {
            [GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE].contains(&node_type)
        });
    if !is_an_interface_node {
        return false;
    }
    let Some(input_values) = node
        .get_mut(NODE_INPUT_VALUES_KEY)
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let Some(family) = input_values.remove(THE_FAMILY_SETTING) else {
        return false;
    };
    let chooses_a_pin_type = input_values
        .get(GRAPH_INTERFACE_PIN_TYPE_SETTING)
        .is_some_and(|chosen| !chosen.is_null());
    if !chooses_a_pin_type
        && let Some(implied) = family
            .as_str()
            .and_then(|family| the_pin_type_a_family_implied(family, the_saved_value(input_values)))
    {
        input_values.insert(GRAPH_INTERFACE_PIN_TYPE_SETTING.to_owned(), implied);
    }
    true
}

/// The value a `GraphInput` saved: its `value`, else its `default` before it was an input.
fn the_saved_value(input_values: &Map<String, Value>) -> Option<&Value> {
    input_values
        .get(GRAPH_INTERFACE_VALUE_PORT_NAME)
        .or_else(|| input_values.get(THE_FORMER_NAME_OF_THE_VALUE))
        .filter(|value| !value.is_null())
}

/// The pin type a port of `family` implied, holding `saved` when it saved a value; `None` for a
/// family that implied no one type.
fn the_pin_type_a_family_implied(family: &str, saved: Option<&Value>) -> Option<Value> {
    match (family, saved) {
        ("image", _) => Some(json!({"type": "texture"})),
        ("array", _) => Some(json!({"type": "tensor"})),
        ("channels", Some(Value::Bool(_))) => Some(json!({"type": "boolean"})),
        ("channels", Some(Value::Number(_))) => Some(json!({"type": "float"})),
        ("channels", Some(Value::Object(_))) => Some(json!({"type": "audio"})),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
