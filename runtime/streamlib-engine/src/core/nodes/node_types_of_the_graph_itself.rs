// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node types a graph resolves itself, which no node code runs: `GraphInput`,
//! `GraphOutput`, `SetVariable` and `GetVariable`. A person or an agent picks them like any
//! node type, filed under the Graph category.

use crate::core::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PortDescriptor,
};
use crate::core::error::{Error, Result};
use crate::core::graph::graph_interface_resolution::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE,
};
use crate::core::graph::{
    PortInfo, cast_exposed_name_to_url_safe, the_refusal_of_a_link_into_a_setting,
};

use super::graph_interface_node_types::{
    the_node_type_of_a_graph_input, the_node_type_of_a_graph_output, the_settings_of_a_graph_input,
    the_settings_of_a_graph_output,
};
use super::graph_variable_node_types::{
    the_node_type_of_a_get_variable, the_node_type_of_a_set_variable,
    the_settings_of_a_graph_variable_node,
};

/// The category every node type the graph resolves itself is filed under.
pub const GRAPH_NODE_TYPE_CATEGORY: &str = "Graph";

/// The descriptor of every node type the graph resolves itself, in the order a listing names
/// them: `GraphInput`, `GraphOutput`, `SetVariable` and `GetVariable`.
pub fn the_node_types_of_the_graph_itself() -> Vec<NodeDescriptor> {
    [
        the_node_type_of_a_graph_input(),
        the_node_type_of_a_graph_output(),
        the_node_type_of_a_set_variable(),
        the_node_type_of_a_get_variable(),
    ]
    .into_iter()
    .map(|descriptor| descriptor.expect("the names of a node type of the graph itself are valid"))
    .collect()
}

/// The descriptor of the node type `node_type` the graph resolves itself, of short name
/// `short_name`, stating `description`, filed under [`GRAPH_NODE_TYPE_CATEGORY`], with no
/// ports yet.
pub(super) fn node_type_of_the_graph_itself(
    short_name: &str,
    node_type: &str,
    description: &str,
) -> Result<NodeDescriptor> {
    Ok(NodeDescriptor::new(
        NodeClassShortName::new(short_name.to_string())
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        NodeClassImportPath::new(node_type.to_string())
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        description.to_string(),
    )
    .with_category(GRAPH_NODE_TYPE_CATEGORY))
}

/// Refuse a link into `input` of node `node`, a node of `node_type`, which the graph resolves
/// itself, when `input` is one of the settings its type declares, which take no wire by the one
/// rule; any other port passes.
pub(crate) fn refuse_a_link_into_a_setting_of_a_node_of_the_graph_itself(
    node_type: &str,
    node: &str,
    input: &str,
) -> Result<()> {
    let node_type = NodeClassImportPath::new(node_type)
        .map_err(|refusal| Error::Configuration(refusal.to_string()))?;
    let settings: &[PortDescriptor] = match node_type.as_str() {
        GRAPH_INPUT_NODE_TYPE => the_settings_of_a_graph_input(),
        GRAPH_OUTPUT_NODE_TYPE => the_settings_of_a_graph_output(),
        _ => the_settings_of_a_graph_variable_node(&node_type).unwrap_or_default(),
    };
    let input_cast = cast_exposed_name_to_url_safe(input)?;
    match settings.iter().find(|setting| setting.name == input_cast) {
        Some(setting) if !PortInfo::from(setting).takes_a_wire_on_a_node_of(&node_type) => {
            Err(the_refusal_of_a_link_into_a_setting(input, node))
        }
        _ => Ok(()),
    }
}
