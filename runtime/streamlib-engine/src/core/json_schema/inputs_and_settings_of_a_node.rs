// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Which of a node's input values name one of its inputs, and which a setting.

use crate::core::nodes::NodeTypesOneStreamResolves;

use super::{GraphNodeOutput, NodeClassImportPath};

/// Which of a node's input values name one of its type's inputs, and which a setting its type's
/// config schema lists. A node holds one map of input values; this is how that map is read,
/// never how the node takes it.
pub(crate) struct InputsAndSettingsOfANodeType {
    /// The name of each input the type declares, each name one of them had, less each setting
    /// its config schema lists.
    inputs: std::collections::HashSet<String>,
}

impl InputsAndSettingsOfANodeType {
    /// The inputs and settings of a node of `node_type`, as `node_types` describes it.
    pub(crate) fn of(
        node_types: &NodeTypesOneStreamResolves,
        node_type: &NodeClassImportPath,
    ) -> Self {
        let mut settings = node_types.the_settings_its_config_schema_lists(node_type);
        if let Some(placeholder) = node_types.placeholder_of_the_node_type_not_described(node_type)
        {
            settings.extend(placeholder.config_fields_its_nodes_expose());
        }
        Self {
            inputs: node_types
                .port_info(node_type)
                .map(|ports| {
                    ports
                        .inputs
                        .into_iter()
                        .flat_map(|input| std::iter::once(input.name).chain(input.former_names))
                        .filter(|name| !settings.contains(name))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Whether the value named `name` is a setting's: it names none of the inputs.
    pub(crate) fn is_a_settings_value(&self, name: &str) -> bool {
        !self.names_an_input(name)
    }

    /// Whether `name` names one of the inputs, or one of a slot of a pin group past the first,
    /// whose pin the first slot names.
    fn names_an_input(&self, name: &str) -> bool {
        self.inputs.contains(name)
            || crate::core::descriptors::PinGroupSlotPort::named(name)
                .filter(|port| port.slot < crate::core::descriptors::PIN_GROUP_MOST_SLOTS)
                .is_some_and(|port| self.inputs.contains(&port.named_in_the_first_slot()))
    }
}

/// The names of the inputs `node` renders, whose values the running node holds.
fn the_inputs_rendered_on(node: &GraphNodeOutput) -> std::collections::HashSet<&str> {
    node.ports
        .inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect()
}

/// The values `node` holds on no input it renders: the settings, as `graph` draws them.
pub(crate) fn the_values_held_on_no_input_of(
    node: &GraphNodeOutput,
) -> serde_json::Map<String, serde_json::Value> {
    let inputs = the_inputs_rendered_on(node);
    node.input_values
        .iter()
        .filter(|(name, _)| !inputs.contains(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// Redraw `node` from `input_values`, as a graph document holds them, over the values its inputs
/// already hold: each value on an input it renders stays as it is, so a value set since
/// `input_values` were read shows itself, and every other value is the one `input_values` holds.
pub(crate) fn redraw_a_node_over_the_values_its_inputs_hold(
    node: &mut GraphNodeOutput,
    input_values: &serde_json::Map<String, serde_json::Value>,
) {
    let inputs = the_inputs_rendered_on(node);
    let drawn = input_values
        .iter()
        .filter(|(name, _)| !inputs.contains(name.as_str()))
        .chain(
            node.input_values
                .iter()
                .filter(|(name, _)| inputs.contains(name.as_str())),
        )
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    node.input_values = drawn;
}
