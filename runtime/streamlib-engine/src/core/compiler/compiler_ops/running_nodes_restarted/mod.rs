// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The running nodes a live edit stops and builds again while the stream keeps
//! running: shader nodes whose islands it places anew, and each node a rebuild
//! names so it reads a setting it takes only when it is set up.

use std::sync::Arc;

use super::open_iceoryx2_service_op::{
    close_iceoryx2_service, close_iceoryx2_service_into_the_host_of_a_shader_node_island,
};
use super::shader_node_island::{
    ShaderNodeFusedIntoTheIslandOfAnotherComponent, ShaderNodeIslandHostedHereComponent,
    is_a_shader_node, the_host_input_port_a_fused_shader_node_input_is_read_at,
};
use crate::core::descriptors::NodeKind;
use crate::core::error::Result;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, Link, LinkState, LinkStateComponent,
    LinkUniqueId, NodeInstanceComponent, NodeMetrics, NodePauseGateComponent, NodeRunId,
    ShaderNodeFusedGroupComponent, ShutdownChannelComponent, StateComponent,
    StepProgressOfOneNodeComponent, ThreadHandleComponent,
};
use crate::core::nodes::NodeState;

/// The running nodes a live edit stops and builds again, and the links rewired
/// with them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RunningNodesALiveEditRestarts {
    pub(crate) nodes: Vec<NodeRunId>,
    pub(crate) links: Vec<LinkUniqueId>,
}

impl RunningNodesALiveEditRestarts {
    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The nodes and links of `self` and of `other`, each once, in order.
    pub(crate) fn joined_with(mut self, other: Self) -> Self {
        self.nodes.extend(other.nodes);
        self.nodes.sort();
        self.nodes.dedup();
        self.links.extend(other.links);
        self.links.sort();
        self.links.dedup();
        self
    }
}

/// Each of `nodes_rebuilt` that runs on a thread of its own and that the edit
/// neither adds nor removes, with every link into or out of it that the edit
/// keeps and does not change itself. A shader node is left to
/// [`super::shader_node_island::the_running_shader_nodes_a_live_edit_restarts`],
/// which restarts its island with it, and a function node, which runs inside
/// the nodes it feeds, reads nothing at setup. A node not built, as on a
/// stopped stream, is built from the values it holds when it starts.
pub(crate) fn the_running_nodes_a_rebuild_restarts(
    graph: &Graph,
    nodes_rebuilt: &[NodeRunId],
    nodes_to_add: &[NodeRunId],
    nodes_to_remove: &[NodeRunId],
    links_changed: &[LinkUniqueId],
) -> RunningNodesALiveEditRestarts {
    let mut nodes: Vec<NodeRunId> = nodes_rebuilt
        .iter()
        .filter(|node_id| !nodes_to_add.contains(node_id) && !nodes_to_remove.contains(node_id))
        .filter(|node_id| !is_a_shader_node(graph, node_id))
        .filter(|node_id| {
            graph.traversal().v(*node_id).first().is_some_and(|node| {
                node.has::<ThreadHandleComponent>()
                    && !matches!(node.node_kind(), Some(NodeKind::Function { .. }))
            })
        })
        .cloned()
        .collect();
    nodes.sort();
    nodes.dedup();
    let mut links: Vec<LinkUniqueId> = nodes
        .iter()
        .flat_map(|node_id| {
            let links_of_the_node: Vec<&Link> = graph
                .traversal()
                .v(node_id)
                .in_e()
                .iter()
                .chain(graph.traversal().v(node_id).out_e().iter())
                .collect();
            links_of_the_node
                .into_iter()
                .filter(|link| {
                    !nodes_to_remove.contains(link.source().node_run_id())
                        && !nodes_to_remove.contains(link.to_port().node_run_id())
                        && !links_changed.contains(&link.id)
                })
                .map(|link| link.id.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    links.sort();
    links.dedup();
    RunningNodesALiveEditRestarts { nodes, links }
}

/// Close the channel of each link of the nodes a live edit restarts the way it
/// was wired before the edit, keeping the link in the graph so the
/// compile wires it again: a call inside an island has no channel, and a link
/// into a fused node was read at its island's host.
pub(crate) fn unwire_each_link_of_the_nodes_restarted(
    graph: &mut Graph,
    restarted: &RunningNodesALiveEditRestarts,
) -> Result<()> {
    for link_id in &restarted.links {
        let Some((source_id, reader_id, reader_port)) =
            graph.traversal().e(link_id).first().map(|link| {
                (
                    link.source().node_run_id().clone(),
                    link.to_port().node_run_id().clone(),
                    link.to_port().port_name().to_string(),
                )
            })
        else {
            continue;
        };
        let was_placed_fused_into_another =
            |node_id: &NodeRunId| {
                graph.traversal().v(node_id).first().is_some_and(|node| {
                    node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>()
                })
            };
        if was_placed_fused_into_another(&source_id) {
            if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
                link.insert(LinkStateComponent(LinkState::Disconnected));
            }
            continue;
        }
        let host_it_was_read_at = was_placed_fused_into_another(&reader_id)
            .then(|| the_host_the_island_of_a_fused_node_was_placed_on(graph, &reader_id))
            .flatten();
        match host_it_was_read_at {
            Some(host_id) => close_iceoryx2_service_into_the_host_of_a_shader_node_island(
                graph,
                link_id,
                &the_host_input_port_a_fused_shader_node_input_is_read_at(
                    &host_id,
                    &reader_id,
                    &reader_port,
                ),
            )?,
            None => close_iceoryx2_service(graph, link_id)?,
        }
    }
    Ok(())
}

/// The node evaluating the island fused shader node `node_id` was placed in at
/// its start, read off the fused group they share.
pub(crate) fn the_host_the_island_of_a_fused_node_was_placed_on(
    graph: &Graph,
    node_id: &NodeRunId,
) -> Option<NodeRunId> {
    let fused_group = Arc::clone(
        &graph
            .traversal()
            .v(node_id)
            .first()?
            .get::<ShaderNodeFusedGroupComponent>()?
            .0,
    );
    graph
        .traversal()
        .v(())
        .ids()
        .into_iter()
        .find(|candidate_id| {
            graph
                .traversal()
                .v(candidate_id)
                .first()
                .is_some_and(|candidate| {
                    candidate
                        .get::<ShaderNodeIslandHostedHereComponent>()
                        .is_some_and(|hosted| Arc::ptr_eq(&hosted.fused_group, &fused_group))
                })
        })
}

/// Forget the build of each of `nodes`, stopped by a live edit that restarts
/// it, so the compile prepares and spawns it again as a node it adds, from the
/// values its inputs hold now.
pub(crate) fn forget_the_build_of_each_node_restarted(graph: &mut Graph, nodes: &[NodeRunId]) {
    for node_id in nodes {
        let Some(node) = graph.traversal_mut().v(node_id).first_mut() else {
            continue;
        };
        node.remove::<NodeInstanceComponent>();
        node.remove::<NodeMetrics>();
        node.remove::<ShutdownChannelComponent>();
        node.remove::<NodePauseGateComponent>();
        node.remove::<StepProgressOfOneNodeComponent>();
        node.remove::<ShaderNodeIslandHostedHereComponent>();
        node.remove::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>();
        node.remove::<ShaderNodeFusedGroupComponent>();
        if let Some(state) = node.get::<StateComponent>() {
            state.transition_to(NodeState::Pending);
        }
    }
}
