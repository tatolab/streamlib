// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Making a mock node's input a setting, read at setup or each run, for a test of a wire into
//! one.

use uuid::Uuid;

use crate::core::descriptors::{NodeInputMode, PinFlow, PinType, ResolvedPin, SettingReadTime};
use crate::core::graph::Graph;

/// Make input `in1` of node `node_id` a Float setting read at setup, and answer the node's UUID.
pub(crate) fn make_in1_a_setting_read_at_setup(graph: &mut Graph, node_id: &str) -> Uuid {
    make_in1_a_setting_read(graph, node_id, SettingReadTime::AtSetup)
}

/// Make input `in1` of node `node_id` a Float setting read each run, and answer the node's UUID.
pub(crate) fn make_in1_a_setting_read_each_run(graph: &mut Graph, node_id: &str) -> Uuid {
    make_in1_a_setting_read(graph, node_id, SettingReadTime::EachRun)
}

fn make_in1_a_setting_read(graph: &mut Graph, node_id: &str, read: SettingReadTime) -> Uuid {
    let node = graph
        .traversal_mut()
        .v(node_id)
        .first_mut()
        .expect("the node is in the graph");
    let in1 = node
        .ports
        .inputs
        .iter_mut()
        .find(|port| port.name == "in1")
        .expect("the mock has an input `in1`");
    in1.pin = Some(ResolvedPin {
        pin_type: PinType::Float { exact_size: None },
        flow: PinFlow::Value,
    });
    in1.input_mode = Some(NodeInputMode::SettingOnly { read });
    node.node_uuid
}
