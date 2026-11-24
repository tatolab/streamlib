// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node's input values as `graph` renders them under the older keys `pins` and `config`.

use crate::core::nodes::NodeTypesOneStreamResolves;

use super::{GraphNodeOutput, NodeClassImportPath};

/// Which of a node's input values `graph` renders under the older keys `pins` and `config`, for
/// a client that reads no `input_values`: a value naming one of its type's inputs renders under
/// `pins`, and every other value, a setting its type's config schema lists among them, under
/// `config`. A node holds one map of input values; this is how that map is drawn, never how the
/// node takes it.
pub(crate) struct OlderGraphKeysOfANodeType {
    /// The name of each input the type declares, each name one of them had, less each setting
    /// its config schema lists.
    inputs_rendered_under_pins: std::collections::HashSet<String>,
}

impl OlderGraphKeysOfANodeType {
    /// The older keys of a node of `node_type`, as `node_types` describes it.
    pub(crate) fn of(
        node_types: &NodeTypesOneStreamResolves,
        node_type: &NodeClassImportPath,
    ) -> Self {
        let mut settings_rendered_under_config =
            node_types.the_settings_its_config_schema_lists(node_type);
        if let Some(placeholder) = node_types.placeholder_of_the_node_type_not_described(node_type)
        {
            settings_rendered_under_config.extend(placeholder.config_fields_its_nodes_expose());
        }
        Self {
            inputs_rendered_under_pins: node_types
                .port_info(node_type)
                .map(|ports| {
                    ports
                        .inputs
                        .into_iter()
                        .flat_map(|input| std::iter::once(input.name).chain(input.former_names))
                        .filter(|name| !settings_rendered_under_config.contains(name))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The older keys of a node known only by the inputs it has, `input_ports`; a fragment node
    /// is drawn so, by the input ports its interface declares.
    pub(crate) fn of_the_input_ports(input_ports: impl IntoIterator<Item = String>) -> Self {
        Self {
            inputs_rendered_under_pins: input_ports.into_iter().collect(),
        }
    }

    /// Whether the value named `name` renders under `config`: it names none of the inputs.
    pub(crate) fn renders_under_config(&self, name: &str) -> bool {
        !self.renders_under_pins(name)
    }

    /// Whether the value named `name` renders under `pins`: it names one of the inputs, or one of
    /// a slot of a pin group past the first, whose pin the first slot names.
    fn renders_under_pins(&self, name: &str) -> bool {
        self.inputs_rendered_under_pins.contains(name)
            || crate::core::descriptors::PinGroupSlotPort::named(name)
                .filter(|port| port.slot < crate::core::descriptors::PIN_GROUP_MOST_SLOTS)
                .is_some_and(|port| {
                    self.inputs_rendered_under_pins
                        .contains(&port.named_in_the_first_slot())
                })
    }
}

/// The values a node holds as `graph` renders them: `input_values`, as its graph document holds
/// them, and the older keys they are drawn under for a client that reads no `input_values`, the
/// `config` with its checksum and the `pins`.
pub(crate) struct InputValuesRendered {
    pub(crate) input_values: serde_json::Map<String, serde_json::Value>,
    pub(crate) config: serde_json::Value,
    pub(crate) config_checksum: u64,
    pub(crate) pins: serde_json::Map<String, serde_json::Value>,
}

impl InputValuesRendered {
    /// `input_values`, drawn under the older keys `older_keys` names.
    pub(crate) fn split_by(
        older_keys: &OlderGraphKeysOfANodeType,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        let (pins, config): (serde_json::Map<_, _>, serde_json::Map<_, _>) = input_values
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .partition(|(name, _)| older_keys.renders_under_pins(name));
        Self::drawn_as(input_values, serde_json::Value::Object(config), pins)
    }

    /// `input_values` of a node that has no input of its own to hold a value on, a variable or
    /// an interface node: every one renders under its `config`.
    pub(crate) fn of_a_node_holding_no_pin_value(
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self::of_a_node_holding_no_pin_value_whose_config_renders_as(
            input_values,
            serde_json::Value::Object(input_values.clone()),
        )
    }

    /// `input_values` of a node that has no input of its own to hold a value on, whose config
    /// renders as `config`.
    pub(crate) fn of_a_node_holding_no_pin_value_whose_config_renders_as(
        input_values: &serde_json::Map<String, serde_json::Value>,
        config: serde_json::Value,
    ) -> Self {
        Self::drawn_as(input_values, config, serde_json::Map::new())
    }

    fn drawn_as(
        input_values: &serde_json::Map<String, serde_json::Value>,
        config: serde_json::Value,
        pins: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            input_values: input_values.clone(),
            config_checksum: crate::core::utils::compute_json_checksum(&config),
            config,
            pins,
        }
    }
}

/// Redraw `node` from `input_values`, as a graph document holds them, over the values its inputs
/// already hold: every other value renders under its `config`. Its pins stay as they are, and its
/// input values are that config with its pins drawn over it, so a pin set since `input_values`
/// were read shows its own value.
pub(crate) fn redraw_a_node_over_its_pins(
    node: &mut GraphNodeOutput,
    input_values: &serde_json::Map<String, serde_json::Value>,
) {
    let config: serde_json::Map<String, serde_json::Value> = input_values
        .iter()
        .filter(|(name, _)| !node.pins.contains_key(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let drawn = config
        .iter()
        .chain(&node.pins)
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let config = serde_json::Value::Object(config);
    node.config_checksum = crate::core::utils::compute_json_checksum(&config);
    node.input_values = drawn;
    node.config = Some(config);
}
