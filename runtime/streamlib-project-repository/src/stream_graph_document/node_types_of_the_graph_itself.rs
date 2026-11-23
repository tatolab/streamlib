// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node types a graph resolves itself, which no node code runs: its interface nodes
//! `GraphInput` and `GraphOutput`, and its variables `SetVariable` and `GetVariable`.

pub use streamlib_node_schema::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE, is_a_graph_interface_node_type,
    is_a_node_type_of_the_graph_itself,
};

/// The config key an interface node names the port it declares under.
pub const GRAPH_INTERFACE_PORT_CONFIG_KEY: &str = "port";

/// The config key an interface node names its port's family under.
pub const GRAPH_INTERFACE_FAMILY_CONFIG_KEY: &str = "family";

/// The one port an interface node has: a `GraphOutput`'s input, a
/// `GraphInput`'s output.
pub const GRAPH_INTERFACE_VALUE_PORT_NAME: &str = "value";
