// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node types a graph resolves itself, which no node code runs: `GraphInput`,
//! `GraphOutput`, `SetVariable` and `GetVariable`. A person or an agent picks them like any
//! node type, filed under the Graph category.

use crate::core::descriptors::{NodeClassImportPath, NodeClassShortName, NodeDescriptor};
use crate::core::error::{Error, Result};

use super::graph_interface_node_types::{
    the_node_type_of_a_graph_input, the_node_type_of_a_graph_output,
};
use super::graph_variable_node_types::{
    the_node_type_of_a_get_variable, the_node_type_of_a_set_variable,
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
