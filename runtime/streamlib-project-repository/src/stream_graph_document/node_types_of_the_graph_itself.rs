// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node types a graph resolves itself, which no node code runs: its interface nodes
//! `GraphInput` and `GraphOutput`, and its variables `SetVariable` and `GetVariable`.

use crate::stream_graph_default_port_and_variable_names::{
    GET_VARIABLE_NODE_TYPE, SET_VARIABLE_NODE_TYPE,
};

pub use streamlib_node_schema::{GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE};

/// The config key an interface node names the port it declares under.
pub const GRAPH_INTERFACE_PORT_CONFIG_KEY: &str = "port";

/// The config key an interface node names its port's family under.
pub const GRAPH_INTERFACE_FAMILY_CONFIG_KEY: &str = "family";

/// The one port an interface node has: a `GraphOutput`'s input, a
/// `GraphInput`'s output.
pub const GRAPH_INTERFACE_VALUE_PORT_NAME: &str = "value";

/// Whether `node_type` is `GraphInput` or `GraphOutput`.
pub fn is_a_graph_interface_node_type(node_type: &str) -> bool {
    matches!(node_type, GRAPH_INPUT_NODE_TYPE | GRAPH_OUTPUT_NODE_TYPE)
}

/// Whether `node_type` is one the graph resolves itself, which no node code runs: `GraphInput`,
/// `GraphOutput`, `SetVariable` or `GetVariable`.
pub fn is_a_node_type_of_the_graph_itself(node_type: &str) -> bool {
    is_a_graph_interface_node_type(node_type)
        || matches!(node_type, SET_VARIABLE_NODE_TYPE | GET_VARIABLE_NODE_TYPE)
}
