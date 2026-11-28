// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values a node's port facts read, such as the channel names an output states: each input
//! value it holds, and each input its node is handed holding its default while it holds no value.

use serde_json::{Map, Value};

use super::PortInfo;

/// The values the port facts of a node holding `input_values` on its inputs `inputs` read: each
/// value it holds, and the default of each input of `inputs` its node is handed that holds none.
pub(crate) fn the_values_port_facts_read(
    inputs: &[PortInfo],
    input_values: &Map<String, Value>,
) -> Value {
    let mut values = input_values.clone();
    for input in inputs.iter().filter(|input| input.is_handed_to_its_node()) {
        if let (false, Some(default)) =
            (values.contains_key(&input.name), input.pin_default.as_ref())
        {
            values.insert(input.name.clone(), default.clone());
        }
    }
    Value::Object(values)
}

#[cfg(test)]
mod tests;
