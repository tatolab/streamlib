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

/// The setting of a `GraphInput` and a `GraphOutput` that chooses the pin type of its `value`
/// port; unset, the first port linked to it decides it.
pub const GRAPH_INTERFACE_PIN_TYPE_SETTING: &str = "pin_type";

/// The one port an interface node has: a `GraphOutput`'s input, a
/// `GraphInput`'s output.
pub const GRAPH_INTERFACE_VALUE_PORT_NAME: &str = "value";
