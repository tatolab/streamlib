// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `GetVariable` names the `SetVariable` it reads by that node's UUID. One saved when a get
//! named its variable opens naming the setter of that variable on its level instead, so
//! renaming the variable later keeps it reading. A get whose variable no setter on its level
//! sets, such as one reading a setter of an enclosing level, or two set, keeps its variable as
//! written.

use serde_json::Value;

use super::NODE_INPUT_VALUES_KEY;
use crate::stream_graph_default_port_and_variable_names::{
    GET_VARIABLE_NODE_TYPE, GET_VARIABLE_SET_VARIABLE_CONFIG_KEY, GRAPH_VARIABLE_NAME_CONFIG_KEY,
    SET_VARIABLE_NODE_TYPE, the_input_values_naming_the_setter,
};

/// Name each get of `nodes`, one level's, that names a variable by the UUID of the level's one
/// setter of it; whether any was.
pub(super) fn the_gets_of_the_level_named_by_their_setter(nodes: &mut [Value]) -> bool {
    let setters: Vec<(String, String)> = nodes
        .iter()
        .filter(|node| the_type_of(node) == Some(SET_VARIABLE_NODE_TYPE))
        .filter_map(|setter| {
            let uuid = setter.get("id").and_then(Value::as_str)?;
            let variable = the_variable_named_in(setter)?;
            Some((variable.to_owned(), uuid.to_owned()))
        })
        .collect();
    let mut any_changed = false;
    for get in nodes
        .iter_mut()
        .filter(|node| the_type_of(node) == Some(GET_VARIABLE_NODE_TYPE))
    {
        let Some(input_values) = get
            .get_mut(NODE_INPUT_VALUES_KEY)
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        if input_values.contains_key(GET_VARIABLE_SET_VARIABLE_CONFIG_KEY) {
            continue;
        }
        let Some(variable) = input_values
            .get(GRAPH_VARIABLE_NAME_CONFIG_KEY)
            .and_then(Value::as_str)
        else {
            continue;
        };
        let mut setters_of_it = setters.iter().filter(|(set, _)| set == variable);
        let (Some((_, setter_uuid)), None) = (setters_of_it.next(), setters_of_it.next()) else {
            continue;
        };
        *input_values = the_input_values_naming_the_setter(setter_uuid);
        any_changed = true;
    }
    any_changed
}

fn the_type_of(node: &Value) -> Option<&str> {
    node.get("type").and_then(Value::as_str)
}

fn the_variable_named_in(node: &Value) -> Option<&str> {
    node.get(NODE_INPUT_VALUES_KEY)?
        .get(GRAPH_VARIABLE_NAME_CONFIG_KEY)?
        .as_str()
}

#[cfg(test)]
mod tests;
