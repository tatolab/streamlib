// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The nodes a compile builds are let set up in waves, so a node whose setting read at setup a
//! Value wire feeds is set up after the node that wire comes from: once the source has set up,
//! the value it wrote, or none, is what the node is built with. A node no wire into a setting
//! waits on is let set up in the first wave, as every node was before.

use std::collections::HashSet;
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

/// The longest one compile's nodes wait, all waves together, for the sources of the wires into
/// their settings to set up before each is built with what its wires carried by then.
const LONGEST_WAIT_FOR_THE_SOURCES_OF_WIRES_INTO_SETTINGS: Duration = Duration::from_secs(30);

/// Let each node of `barrier_handles` set up, each delivered the values its unlinked inputs
/// read already: a node a wire into one of its settings read at setup comes into after the node
/// that wire comes from has set up, delivered again with each value that wire carried by then
/// as what it is built with. Nodes whose wires into their settings come from each other set up
/// together.
///
/// When a later wave's values are not handed over, the nodes of that wave and every wave after
/// it are never let set up, each failing as a node of a compile refused before its setup does,
/// and the refusal is answered as the compile's; the nodes of earlier waves are set up already.
pub(super) fn let_each_node_set_up_after_the_sources_of_the_wires_into_its_settings(
    graph_arc: &RwLock<Graph>,
    mut barrier_handles: Vec<(NodeRunId, NodeReadyBarrierHandle)>,
) -> Result<()> {
    let deadline = Instant::now() + LONGEST_WAIT_FOR_THE_SOURCES_OF_WIRES_INTO_SETTINGS;
    let mut a_wave_has_set_up = false;
    while !barrier_handles.is_empty() {
        let waiting_on_a_source =
            the_nodes_a_wire_into_a_setting_waits_on_among(&graph_arc.read(), &barrier_handles);
        let (mut later, mut now): (Vec<_>, Vec<_>) = barrier_handles
            .into_iter()
            .partition(|(node, _)| waiting_on_a_source.contains(node));
        if now.is_empty() {
            now = std::mem::take(&mut later);
        }
        let set_up_now: Vec<NodeRunId> = now.iter().map(|(node, _)| node.clone()).collect();
        if a_wave_has_set_up {
            build_each_with_what_the_wires_into_its_settings_carried(graph_arc, &set_up_now);
            deliver_the_unlinked_input_values_of_each_node(graph_arc, &set_up_now)?;
        }
        for (node, handle) in now {
            tracing::trace!("[{}] Signaling CONTINUE", node);
            if handle.continue_sender.send(()).is_err() {
                tracing::warn!("[{}] Failed to signal CONTINUE", node);
            }
        }
        if !later.is_empty() {
            wait_until_each_has_set_up(graph_arc, &set_up_now, deadline);
        }
        a_wave_has_set_up = true;
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

/// Each of `barrier_handles`' nodes that a wire into one of its settings read at setup comes
/// into from another of them, which is to set up first. Only a link into such a setting carries
/// the engine's reader.
fn the_nodes_a_wire_into_a_setting_waits_on_among(
    graph: &Graph,
    barrier_handles: &[(NodeRunId, NodeReadyBarrierHandle)],
) -> HashSet<NodeRunId> {
    let still_to_set_up: HashSet<&NodeRunId> =
        barrier_handles.iter().map(|(node, _)| node).collect();
    barrier_handles
        .iter()
        .filter(|(node, _)| {
            graph.traversal().v(node).in_e().iter().any(|link| {
                let source = link.source().node_run_id();
                link.has::<ReaderOfALinkIntoASettingComponent>()
                    && source != node
                    && still_to_set_up.contains(source)
            })
        })
        .map(|(node, _)| node.clone())
        .collect()
}

/// Wait until each of `nodes` has set up or failed, or `deadline` passes.
fn wait_until_each_has_set_up(graph_arc: &RwLock<Graph>, nodes: &[NodeRunId], deadline: Instant) {
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
    for state in states {
        state.wait_until_setup_resolved(deadline);
    }
}
