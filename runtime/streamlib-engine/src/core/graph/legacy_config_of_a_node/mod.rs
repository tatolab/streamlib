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
use crate::core::nodes::{NodeSpec, NodeTypesOneStreamResolves};

/// How one node type splits its nodes' input values into a config and the values on its inputs.
pub(crate) struct LegacyConfigOfANodeType {
    fields_its_config_schema_lists: HashSet<String>,
    its_input_ports: HashSet<String>,
}

/// A node's input values split as its type takes them: a config whole, and a value on each of
/// its inputs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfigAndPinValues {
    /// The config the node takes whole.
    pub(crate) config: Value,
    /// The value on each of the node's inputs, by input name.
    pub(crate) pin_values: Map<String, Value>,
}

impl LegacyConfigOfANodeType {
    /// How `node_type` splits its nodes' input values, as `node_types` describes it. A config
    /// field a placeholder of a type not described knows of, one a node of it exposes, is a
    /// config field as one its schema lists is.
    pub(crate) fn of(
        node_types: &NodeTypesOneStreamResolves,
        node_type: &NodeClassImportPath,
    ) -> Self {
        let mut fields_its_config_schema_lists =
            fields_the_config_schema_of_a_node_type_lists(node_types, node_type);
        if let Some(placeholder) = node_types.placeholder_of_the_node_type_not_described(node_type)
        {
            fields_its_config_schema_lists.extend(placeholder.config_fields_its_nodes_expose());
        }
        Self {
            fields_its_config_schema_lists,
            its_input_ports: node_types
                .port_info(node_type)
                .map(|(inputs, _)| inputs.into_iter().map(|input| input.name).collect())
                .unwrap_or_default(),
        }
    }

    /// How a node known only by the inputs it has splits its input values: a value on each of
    /// `input_ports`, every other name a config field. A fragment node splits so, by the input
    /// ports its interface declares.
    pub(crate) fn of_the_input_ports(input_ports: impl IntoIterator<Item = String>) -> Self {
        Self {
            fields_its_config_schema_lists: HashSet::new(),
            its_input_ports: input_ports.into_iter().collect(),
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

    /// `input_values` split into the config a node holding them takes whole and the value on
    /// each of its inputs.
    pub(crate) fn split(&self, input_values: &Map<String, Value>) -> ConfigAndPinValues {
        let (config, pin_values) = input_values
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .partition(|(name, _)| self.is_a_config_field(name));
        ConfigAndPinValues {
            config: Value::Object(config),
            pin_values,
        }
    }
}

/// The fields the config schema of `node_type`, as `node_types` describes it, lists; none for
/// a type that describes no config schema.
pub(crate) fn fields_the_config_schema_of_a_node_type_lists(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
) -> HashSet<String> {
    node_types
        .descriptor(node_type)
        .and_then(|descriptor| descriptor.config_schema)
        .and_then(|config_schema| match config_schema.get("properties") {
            Some(Value::Object(properties)) => Some(properties.keys().cloned().collect()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The config a node of `node_type` holding `input_values` takes whole.
pub(crate) fn config_of_a_node(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
    input_values: &Map<String, Value>,
) -> Value {
    LegacyConfigOfANodeType::of(node_types, node_type).config_of(input_values)
}

/// The spec that places a node of `node_type` holding `input_values`, its config and the values
/// on its inputs split as its type takes them, carrying `studio`.
pub(crate) fn node_spec_holding_the_input_values(
    node_types: &NodeTypesOneStreamResolves,
    node_type: NodeClassImportPath,
    input_values: &Map<String, Value>,
    studio: Map<String, Value>,
) -> NodeSpec {
    let ConfigAndPinValues { config, pin_values } =
        LegacyConfigOfANodeType::of(node_types, &node_type).split(input_values);
    NodeSpec::new(node_type, config).with_pins_and_studio_fields(pin_values, studio)
}

/// The config a node shows where it is rendered as drawn over its running node: each of its
/// input values but those `pin_values`, the running node's, names.
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

/// The config of a node that has no input of its own to hold a value on, a variable or an
/// interface node: every input value it holds.
pub(crate) fn config_of_a_node_holding_no_pin_value(input_values: &Map<String, Value>) -> Value {
    Value::Object(input_values.clone())
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
