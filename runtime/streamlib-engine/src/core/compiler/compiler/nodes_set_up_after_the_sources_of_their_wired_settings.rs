// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The nodes a compile builds are let set up in waves, so a node whose setting read at setup a
//! Value wire feeds is set up after the node that wire comes from: once the source has set up,
//! the value it wrote, or none, is what the node is built with. A node no wire into a setting
//! waits on is let set up in the first wave, as every node was before.

use std::time::{Duration, Instant};

use parking_lot::RwLock;
use uuid::Uuid;

use crate::core::compiler::compiler_ops::ReaderOfALinkIntoASettingComponent;
use crate::core::compiler::compiler_ops::unlinked_input_values_delivery::deliver_the_unlinked_input_values_of_each_node;
use crate::core::error::Result;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, NodeReadyBarrierHandle, NodeRunId,
    StateComponent,
};

/// The longest a node waits for the source of a wire into one of its settings to set up before
/// it is built with what that wire carried by then.
const LONGEST_WAIT_FOR_THE_SOURCE_OF_A_WIRE_INTO_A_SETTING: Duration = Duration::from_secs(30);

/// Let each node of `barrier_handles` set up, each delivered the values its unlinked inputs
/// read already: a node a wire into one of its settings read at setup comes into after the node
/// that wire comes from has set up, delivered again with each value that wire carried by then
/// as what it is built with.
pub(super) fn let_each_node_set_up_after_the_sources_of_the_wires_into_its_settings(
    graph_arc: &RwLock<Graph>,
    mut barrier_handles: Vec<(NodeRunId, NodeReadyBarrierHandle)>,
) -> Result<()> {
    let mut set_up_first = true;
    while !barrier_handles.is_empty() {
        let waiting_on_a_source =
            the_nodes_a_wire_into_a_setting_waits_on_among(&graph_arc.read(), &barrier_handles);
        let (later, now): (Vec<_>, Vec<_>) = barrier_handles
            .into_iter()
            .partition(|(node, _)| waiting_on_a_source.contains(node));
        let (now, later) = if now.is_empty() {
            (later, Vec::new())
        } else {
            (now, later)
        };
        if !set_up_first {
            let node_ids: Vec<NodeRunId> = now.iter().map(|(node, _)| node.clone()).collect();
            build_each_with_what_the_wires_into_its_settings_carried(graph_arc, &node_ids);
            deliver_the_unlinked_input_values_of_each_node(graph_arc, &node_ids)?;
        }
        let set_up_now: Vec<NodeRunId> = now.iter().map(|(node, _)| node.clone()).collect();
        for (node, handle) in now {
            tracing::trace!("[{}] Signaling CONTINUE", node);
            if handle.continue_sender.send(()).is_err() {
                tracing::warn!("[{}] Failed to signal CONTINUE", node);
            }
        }
        if !later.is_empty() {
            wait_until_each_has_set_up(graph_arc, &set_up_now);
        }
        set_up_first = false;
        barrier_handles = later;
    }
    Ok(())
}

/// Hold each value the wires into the settings of `nodes` carried, each reader handing over
/// what it has not yet, as what those nodes are built with.
pub(super) fn build_each_with_what_the_wires_into_its_settings_carried(
    graph_arc: &RwLock<Graph>,
    nodes: &[NodeRunId],
) {
    let graph = graph_arc.read();
    let mut node_uuids: Vec<Uuid> = Vec::new();
    for node_run_id in nodes {
        let Some(node) = graph.traversal().v(node_run_id).first() else {
            continue;
        };
        node_uuids.push(node.node_uuid);
        for link in graph.traversal().v(node_run_id).in_e().iter() {
            if let Some(reader) = link.get::<ReaderOfALinkIntoASettingComponent>() {
                reader.hand_over_what_it_carried();
            }
        }
    }
    graph
        .values_wires_carry_into_settings()
        .build_each_of_the_nodes_with_what_waits(&node_uuids);
}

/// Each of `barrier_handles`' nodes a wire into one of its settings read at setup comes into
/// from another of them, which is to set up first.
fn the_nodes_a_wire_into_a_setting_waits_on_among(
    graph: &Graph,
    barrier_handles: &[(NodeRunId, NodeReadyBarrierHandle)],
) -> Vec<NodeRunId> {
    let still_to_set_up = |node: &NodeRunId| barrier_handles.iter().any(|(held, _)| held == node);
    barrier_handles
        .iter()
        .filter(|(node, _)| {
            graph.traversal().v(node).in_e().iter().any(|link| {
                let to = link.to_port();
                link.has::<ReaderOfALinkIntoASettingComponent>()
                    && graph.is_a_setting_read_at_setup(to.node_run_id(), to.port_name())
                    && still_to_set_up(link.source().node_run_id())
                    && link.source().node_run_id() != node
            })
        })
        .map(|(node, _)| node.clone())
        .collect()
}

/// Wait until each of `nodes` has set up or failed, or the wait's bound passes.
fn wait_until_each_has_set_up(graph_arc: &RwLock<Graph>, nodes: &[NodeRunId]) {
    let states: Vec<_> = {
        let graph = graph_arc.read();
        nodes
            .iter()
            .filter_map(|node| {
                graph
                    .traversal()
                    .v(node)
                    .first()?
                    .get::<StateComponent>()
                    .map(StateComponent::clone_inner)
            })
            .collect()
    };
    let deadline = Instant::now() + LONGEST_WAIT_FOR_THE_SOURCE_OF_A_WIRE_INTO_A_SETTING;
    for state in states {
        state.wait_until_setup_resolved(deadline);
    }
}
