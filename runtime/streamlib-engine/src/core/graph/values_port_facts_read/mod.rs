// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values a node's port facts read, such as the channel names an output states: each input
//! value it holds, and each setting it reads at setup holding its default while it holds no value.

use serde_json::{Map, Value};

use super::PortInfo;

/// The values the port facts of a node holding `input_values` on its inputs `inputs` read: each
/// value it holds, and the default of each setting of `inputs` read at setup that holds none.
pub(crate) fn the_values_port_facts_read(
    inputs: &[PortInfo],
    input_values: &Map<String, Value>,
) -> Value {
    let mut values = input_values.clone();
    for setting in inputs
        .iter()
        .filter(|input| input.is_a_setting_read_at_setup())
    {
        if let (false, Some(default)) = (
            values.contains_key(&setting.name),
            setting.pin_default.as_ref(),
        ) {
            values.insert(setting.name.clone(), default.clone());
        }
    }
    Value::Object(values)
}

#[cfg(test)]
mod tests;
