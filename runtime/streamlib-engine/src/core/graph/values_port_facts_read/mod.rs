// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values a node's port facts read, such as the channel names an output states: its config,
//! and each setting it reads at setup, holding the value set on it or else its default.

use serde_json::{Map, Value};

use super::PortInfo;

/// The values the port facts of a node holding `config` and, on its inputs `inputs`, the values
/// `values_set` read: `config`, each setting of `inputs` read at setup laid over it holding its
/// value in `values_set` or else its default. `None` when the node holds no config and has no
/// such setting.
pub(crate) fn the_values_port_facts_read(
    config: Option<&Value>,
    inputs: &[PortInfo],
    values_set: &Map<String, Value>,
) -> Option<Value> {
    let mut settings_read_at_setup = inputs
        .iter()
        .filter(|input| input.is_a_setting_read_at_setup())
        .filter_map(|setting| {
            values_set
                .get(&setting.name)
                .or(setting.pin_default.as_ref())
                .map(|value| (setting.name.clone(), value.clone()))
        })
        .peekable();
    if settings_read_at_setup.peek().is_none() {
        return config.cloned();
    }
    let mut values = match config {
        Some(Value::Object(config)) => config.clone(),
        _ => Map::new(),
    };
    values.extend(settings_read_at_setup);
    Some(Value::Object(values))
}

#[cfg(test)]
mod tests;
