// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node holds one map of input values. A node type not yet typed still takes a config whole
//! and a value on each unlinked input apart; this module alone says which of a node's input
//! values are its config and which are the values on its inputs, and puts the two back into
//! one map.
//!
//! A name the type's config schema lists is a config field. Any other name that is one of the
//! type's input ports is that input's value, and every name left is a config field, so the
//! type's config schema refuses a misspelled setting, and a type the runtime does not have keeps
//! every value it was saved with.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::core::descriptors::NodeClassImportPath;
use crate::core::nodes::NodeTypesOneStreamResolves;

/// How one node type splits its nodes' input values into a config and the values on its inputs.
pub(crate) struct LegacyConfigOfANodeType {
    fields_its_config_schema_lists: HashSet<String>,
    its_input_ports: HashSet<String>,
}

impl LegacyConfigOfANodeType {
    /// How `node_type` splits its nodes' input values, as `node_types` describes it.
    pub(crate) fn of(
        node_types: &NodeTypesOneStreamResolves,
        node_type: &NodeClassImportPath,
    ) -> Self {
        Self {
            fields_its_config_schema_lists: node_types
                .descriptor(node_type)
                .and_then(|descriptor| descriptor.config_schema)
                .and_then(|config_schema| match config_schema.get("properties") {
                    Some(Value::Object(properties)) => Some(properties.keys().cloned().collect()),
                    _ => None,
                })
                .unwrap_or_default(),
            its_input_ports: node_types
                .port_info(node_type)
                .map(|(inputs, _)| inputs.into_iter().map(|input| input.name).collect())
                .unwrap_or_default(),
        }
    }

    fn is_a_config_field(&self, name: &str) -> bool {
        self.fields_its_config_schema_lists.contains(name) || !self.its_input_ports.contains(name)
    }

    /// The config a node holding `input_values` takes whole.
    pub(crate) fn config_of(&self, input_values: &Map<String, Value>) -> Value {
        Value::Object(
            input_values
                .iter()
                .filter(|(name, _)| self.is_a_config_field(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        )
    }

    /// The value on each input of a node holding `input_values`, by input name.
    pub(crate) fn pin_values_of(&self, input_values: &Map<String, Value>) -> Map<String, Value> {
        input_values
            .iter()
            .filter(|(name, _)| !self.is_a_config_field(name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

/// The config a node of `node_type` holding `input_values` takes whole.
pub(crate) fn config_of_a_node(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
    input_values: &Map<String, Value>,
) -> Value {
    LegacyConfigOfANodeType::of(node_types, node_type).config_of(input_values)
}

/// The config a node shows where it is rendered as drawn, with no node type at hand: each of
/// its input values but those `pin_values` names. A fragment, a variable and an interface node
/// take every value they hold as their config.
pub(crate) fn config_of_a_node_as_drawn(
    input_values: &Map<String, Value>,
    pin_values: &Map<String, Value>,
) -> Value {
    Value::Object(
        input_values
            .iter()
            .filter(|(name, _)| !pin_values.contains_key(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    )
}

/// The one map of input values a node taking `config` whole and `pin_values` on its inputs
/// holds; a pin value wins over a config field of its name.
pub(crate) fn input_values_of_a_config_and_pin_values(
    config: Option<&Value>,
    pin_values: &Map<String, Value>,
) -> Map<String, Value> {
    let mut input_values = Map::new();
    streamlib_project_repository::fold_into_input_values(
        &mut input_values,
        pin_values.clone(),
        match config {
            Some(Value::Object(config)) => config.clone(),
            _ => Map::new(),
        },
    );
    input_values
}

#[cfg(test)]
mod tests;
