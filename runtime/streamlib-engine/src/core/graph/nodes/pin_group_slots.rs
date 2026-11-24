// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The slots of a node's pin groups: a group that follows its wires holds one empty slot after
//! the last one a link reaches, and a group whose count a person sets holds as many as its count
//! input says, and never fewer than reach its last linked slot. Every link added or taken out of
//! a graph, and every value set on a node, gives the node the slots this holds.

use std::sync::Arc;

use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};

use super::{GraphNode, PortInfo};
use crate::core::PortDirection;
use crate::core::descriptors::{
    PIN_GROUP_MOST_SLOTS, PinGroupDescriptor, PinGroupSlotCount, PinGroupSlotPort,
};
use crate::core::graph::Link;

impl GraphNode {
    /// Hold `pin_groups`, the pin groups the node's type declares.
    pub(crate) fn hold_the_pin_groups_of_its_type(
        &mut self,
        pin_groups: Arc<[PinGroupDescriptor]>,
    ) {
        self.pin_groups = pin_groups;
    }

    /// The pin groups the node's type declares.
    pub fn pin_groups(&self) -> &[PinGroupDescriptor] {
        &self.pin_groups
    }

    /// The pin group and slot port `port_name` names on this node; `None` for a port of no
    /// group.
    pub fn the_pin_group_slot_of<'name>(
        &self,
        port_name: &'name str,
    ) -> Option<(&PinGroupDescriptor, PinGroupSlotPort<'name>)> {
        let slot_port = PinGroupSlotPort::named(port_name)?;
        self.pin_groups
            .iter()
            .find(|group| group.holds_the_slot_port(port_name))
            .map(|group| (group, slot_port))
    }

    /// How many slots each of its pin groups holds now, in the order they are declared.
    pub(crate) fn slot_counts_held(&self) -> Vec<usize> {
        self.pin_groups
            .iter()
            .map(|group| {
                self.ports
                    .inputs
                    .iter()
                    .chain(&self.ports.outputs)
                    .filter(|port| group.holds_the_slot_port(&port.name))
                    .filter_map(|port| PinGroupSlotPort::named(&port.name))
                    .map(|port| port.slot + 1)
                    .max()
                    .unwrap_or(0)
            })
            .collect()
    }

    /// The slot count the node's value of its input named after `group` sets, a group that
    /// follows its wires reading none; the input's default while it holds none.
    fn the_slot_count_set_for(&self, group: &PinGroupDescriptor) -> usize {
        let set = self.pins.get(&group.name).or_else(|| {
            self.ports
                .input_named(&group.name)
                .and_then(|input| input.pin_default.as_ref())
        });
        set.and_then(serde_json::Value::as_u64)
            .map_or(1, |count| count as usize)
    }

    /// Hold `slot_counts` slots in its pin groups, in the order they are declared: each port of a
    /// slot it still holds kept as it is, each port of a new slot as its group declares it, each
    /// slot beyond taken out. Answers whether a port came or went.
    fn hold_slot_counts(&mut self, slot_counts: &[usize]) -> bool {
        if self.slot_counts_held() == slot_counts {
            return false;
        }
        let pin_groups = Arc::clone(&self.pin_groups);
        let of_no_group = |port: &PortInfo| {
            !pin_groups
                .iter()
                .any(|group| group.holds_the_slot_port(&port.name))
        };
        for direction in [PortDirection::Input, PortDirection::Output] {
            let held = std::mem::take(self.ports.on_mut(direction));
            let (mut ports, mut slot_ports): (Vec<PortInfo>, Vec<PortInfo>) =
                held.into_iter().partition(of_no_group);
            for (group, count) in pin_groups.iter().zip(slot_counts) {
                for slot in 0..*count {
                    let (inputs, outputs) = group.the_ports_of_slot(slot);
                    let declared = match direction {
                        PortDirection::Input => inputs,
                        PortDirection::Output => outputs,
                    };
                    for port in &declared {
                        let kept = slot_ports
                            .iter()
                            .position(|held| held.name == port.name)
                            .map(|at| slot_ports.swap_remove(at));
                        ports.push(kept.unwrap_or_else(|| PortInfo::from(port)));
                    }
                }
            }
            *self.ports.on_mut(direction) = ports;
        }
        let values = self.the_values_its_port_facts_read();
        self.stream_facts_of_output_ports()
            .hold_the_output_ports(&self.ports.outputs, values.as_ref());
        true
    }
}

/// The last slot index a link of node `node_index` reaches in `group`, on either end; `None`
/// when no link reaches one.
fn the_last_slot_its_links_reach(
    graph: &DiGraph<GraphNode, Link>,
    node_index: NodeIndex,
    group: &PinGroupDescriptor,
) -> Option<usize> {
    let into_it = graph
        .edges_directed(node_index, Direction::Incoming)
        .map(|edge| edge.weight().to_port().port_name());
    let out_of_it = graph
        .edges_directed(node_index, Direction::Outgoing)
        .map(|edge| edge.weight().source().port_name());
    into_it
        .chain(out_of_it)
        .filter(|port_name| group.holds_the_slot_port(port_name))
        .filter_map(|port_name| PinGroupSlotPort::named(port_name).map(|port| port.slot))
        .max()
}

/// Give node `node_index` of `graph` the slots its pin groups hold by its links and its values:
/// a group following its wires one empty slot after the last a link reaches, a group whose count
/// is set as many as its count input says and never fewer than reach its last linked slot, each
/// at most [`PIN_GROUP_MOST_SLOTS`]. A link pending deletion still reaches its slot until it is
/// taken out. Answers whether a port came or went.
pub(crate) fn reshape_the_pin_group_slots_of(
    graph: &mut DiGraph<GraphNode, Link>,
    node_index: NodeIndex,
) -> bool {
    let Some(node) = graph.node_weight(node_index) else {
        return false;
    };
    if node.pin_groups.is_empty() {
        return false;
    }
    let slot_counts: Vec<usize> = node
        .pin_groups
        .iter()
        .map(|group| {
            let past_the_last_linked =
                the_last_slot_its_links_reach(graph, node_index, group).map_or(0, |slot| slot + 1);
            let count = match group.slot_count() {
                PinGroupSlotCount::FollowsItsWires => past_the_last_linked + 1,
                PinGroupSlotCount::SetByItsInput => {
                    node.the_slot_count_set_for(group).max(past_the_last_linked)
                }
            };
            count.min(PIN_GROUP_MOST_SLOTS)
        })
        .collect();
    graph[node_index].hold_slot_counts(&slot_counts)
}

/// Grow the pin group of node `node_index` that holds the port `port_name` of `direction`, so it
/// holds that port, when the slot it names is past the group's last and within
/// [`PIN_GROUP_MOST_SLOTS`]: a saved graph's links reach their slots in any order. Any other
/// name leaves the node as it is.
pub(crate) fn grow_a_pin_group_to_hold_the_port(
    graph: &mut DiGraph<GraphNode, Link>,
    node_index: NodeIndex,
    direction: PortDirection,
    port_name: &str,
) {
    let Some(node) = graph.node_weight_mut(node_index) else {
        return;
    };
    if node
        .ports
        .the_one_a_name_finds(direction, port_name)
        .is_some()
    {
        return;
    }
    let Some(group_index) = node
        .pin_groups
        .iter()
        .position(|group| group.holds_the_slot_port(port_name))
    else {
        return;
    };
    let Some(slot_port) = PinGroupSlotPort::named(port_name) else {
        return;
    };
    if slot_port.slot >= PIN_GROUP_MOST_SLOTS {
        return;
    }
    let mut slot_counts = node.slot_counts_held();
    slot_counts[group_index] = slot_counts[group_index].max(slot_port.slot + 1);
    node.hold_slot_counts(&slot_counts);
}

/// Refuse `value` as the count of the pin group of node `node_index` named `input`, naming the
/// node and the link: a count that leaves out a slot a link reaches, `None` reading as the
/// input's default. Any other input, and a
/// count no link is past, is taken.
pub(crate) fn refuse_a_slot_count_leaving_out_a_linked_slot(
    graph: &DiGraph<GraphNode, Link>,
    node_index: NodeIndex,
    input: &str,
    value: Option<&serde_json::Value>,
) -> crate::core::Result<()> {
    let Some(node) = graph.node_weight(node_index) else {
        return Ok(());
    };
    let Some(group) = node.pin_groups.iter().find(|group| {
        group.name == input && group.slot_count() == PinGroupSlotCount::SetByItsInput
    }) else {
        return Ok(());
    };
    let count = match value {
        Some(value) => value.as_u64(),
        None => node
            .ports
            .input_named(input)
            .and_then(|input| input.pin_default.as_ref())
            .and_then(serde_json::Value::as_u64)
            .or(Some(1)),
    };
    let Some(count) = count else {
        return Ok(());
    };
    match the_last_slot_its_links_reach(graph, node_index, group) {
        Some(last) if count <= last as u64 => Err(crate::core::Error::Configuration(format!(
            "`{}` cannot hold {count} slots in `{input}`: a link reaches its slot {last}. To fix: \
             take out the links of each slot past {count} first",
            node.display_name
        ))),
        _ => Ok(()),
    }
}
