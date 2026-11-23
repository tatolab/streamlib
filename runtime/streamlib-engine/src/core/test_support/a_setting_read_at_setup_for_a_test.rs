// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Making a mock node's input a setting it reads at setup, for a test of a wire into one.

use uuid::Uuid;

use crate::core::descriptors::{NodeInputMode, PinFlow, PinType, ResolvedPin, SettingReadTime};
use crate::core::graph::Graph;

/// Make input `in1` of node `node_id` a Float setting read at setup, and answer the node's UUID.
pub(crate) fn make_in1_a_setting_read_at_setup(graph: &mut Graph, node_id: &str) -> Uuid {
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
    in1.input_mode = Some(NodeInputMode::SettingOnly {
        read: SettingReadTime::AtSetup,
    });
    node.node_uuid
}
