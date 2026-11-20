// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod graph_node;
mod graph_node_ports;
mod node_run_id;
mod pin_group_slots;
mod port_info;

pub use graph_node::GraphNode;
pub(crate) use graph_node::the_refusal_of_a_link_into_a_setting;
pub use graph_node_ports::{GraphNodePorts, InputHoldingAValueUnderANameItHad};
pub use node_run_id::NodeRunId;
pub(crate) use pin_group_slots::{
    grow_a_pin_group_to_hold_the_port, refuse_a_slot_count_leaving_out_a_linked_slot,
    reshape_the_pin_group_slots_of,
};
pub(crate) use port_info::PortAnnotations;
pub use port_info::{PinTypeAsDeclared, PortInfo};
