// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Restarting running nodes inside a live edit's compile: a shader node whose island the edit
//! changes, and a node whose setting read at setup the edit changes. Each is stopped, its links
//! unwired and its build forgotten, then built and wired again by the same compile, while every
//! other node keeps running.

use super::function_node_island::{
    HowALinkIsWired, how_a_link_is_wired, unwire_a_link_from_a_function_node_island_into_its_reader,
};
use super::open_iceoryx2_service_op::{
    close_iceoryx2_service, close_iceoryx2_service_into_the_host_of_a_shader_node_island,
};
use super::shader_node_island::{
    ShaderNodeFusedIntoTheIslandOfAnotherComponent, ShaderNodeIslandHostedHereComponent,
    the_host_input_port_a_fused_shader_node_input_is_read_at,
    the_host_the_island_of_a_fused_node_was_placed_on,
};
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, HelperProcessRestartsComponent,
    LinkPinTypesDifferComponent, LinkState, LinkStateComponent, LinkUniqueId,
    NodeInstanceComponent, NodeMetrics, NodePauseGateComponent, NodeRunId,
    OutOfProcessLinkWiringComponent, ShaderNodeFusedGroupComponent, ShutdownChannelComponent,
    StateComponent, StepProgressOfOneNodeComponent,
};
use crate::core::nodes::NodeState;

/// The running nodes a live edit restarts, and the links rewired with them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RunningNodesALiveEditRestarts {
    pub(crate) nodes: Vec<NodeRunId>,
    pub(crate) links: Vec<LinkUniqueId>,
}

impl RunningNodesALiveEditRestarts {
    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The nodes and links either restarts, each once, in sorted order.
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

/// The running nodes of `nodes_to_rebuild` the compile restarts so each reads its settings read
/// at setup as the graph holds them now, with every link into or out of them that the edit does
/// not add or remove and whose two ends still carry types that connect; one whose ends differ
/// stays unwired. A node the same edit adds or removes is built or taken out instead, so it
/// is not restarted.
///
/// A link into it from a function node island it evaluates itself is restarted with it, the
/// island fed to it again as it sets up, and so is a channel into it from a function node the
/// pool or a helper of its own evaluates. Refused by name, changing nothing, for a function or
/// shader node, which reads no setting at setup, and for a node one of whose other links joins a
/// function node otherwise: a link out of it into an island another node evaluates, whose place
/// in that island is read once, when that node starts, or into a function node run in a process
/// of its own.
pub(crate) fn the_running_nodes_a_rebuild_restarts(
    graph: &Graph,
    nodes_to_rebuild: &[NodeRunId],
    nodes_to_add: &[NodeRunId],
    nodes_to_remove: &[NodeRunId],
    links_changed: &[LinkUniqueId],
) -> Result<RunningNodesALiveEditRestarts> {
    let name_of = |node_id: &NodeRunId| {
        graph
            .traversal()
            .v(node_id)
            .first()
            .map_or_else(|| node_id.to_string(), |node| node.display_name.clone())
    };
    let kind_of = |node_id: &NodeRunId| {
        graph
            .traversal()
            .v(node_id)
            .first()
            .and_then(|node| node.node_kind())
    };
    let mut restarted = RunningNodesALiveEditRestarts::default();
    for node_id in nodes_to_rebuild {
        if nodes_to_add.contains(node_id)
            || nodes_to_remove.contains(node_id)
            || !graph.traversal().v(node_id).exists()
        {
            continue;
        }
        if let Some(kind @ (NodeKind::Function { .. } | NodeKind::Shader)) = kind_of(node_id) {
            return Err(Error::Configuration(format!(
                "`{}` is a {} node, which reads no setting at setup, so no edit rebuilds it",
                name_of(node_id),
                kind.name(),
            )));
        }
        let links_of_the_node: Vec<(LinkUniqueId, NodeRunId, NodeRunId)> = graph
            .traversal()
            .v(node_id)
            .in_e()
            .iter()
            .chain(graph.traversal().v(node_id).out_e().iter())
            .map(|link| {
                (
                    link.id.clone(),
                    link.source().node_run_id().clone(),
                    link.to_port().node_run_id().clone(),
                )
            })
            .collect();
        for (link_id, source_id, reader_id) in links_of_the_node {
            if links_changed.contains(&link_id)
                || graph
                    .traversal()
                    .e(&link_id)
                    .first()
                    .is_some_and(|link| link.has::<LinkPinTypesDifferComponent>())
                || nodes_to_remove.contains(&source_id)
                || nodes_to_remove.contains(&reader_id)
            {
                continue;
            }
            let other_end = if source_id == *node_id {
                &reader_id
            } else {
                &source_id
            };
            if matches!(kind_of(other_end), Some(NodeKind::Function { .. }))
                && !is_rewired_with_the_node_it_feeds(graph, &link_id, &reader_id, node_id)
            {
                return Err(Error::Configuration(format!(
                    "`{}` is rebuilt to read a setting read at setup, and its link `{link_id}` \
                     joins function node `{}`, whose place in an island is read once, when its \
                     nodes start. To fix: stop the stream, make the edit, and run it again",
                    name_of(node_id),
                    name_of(other_end),
                )));
            }
            restarted.links.push(link_id);
        }
        restarted.nodes.push(node_id.clone());
    }
    Ok(restarted.joined_with(RunningNodesALiveEditRestarts::default()))
}

/// Whether link `link_id`, which joins a function node, is wired again with its reader `reader_id`
/// when `restarted` restarts: `restarted` reads it, as an island it evaluates itself or as a
/// channel from a function node the pool or a helper of its own evaluates.
fn is_rewired_with_the_node_it_feeds(
    graph: &Graph,
    link_id: &LinkUniqueId,
    reader_id: &NodeRunId,
    restarted: &NodeRunId,
) -> bool {
    reader_id == restarted
        && matches!(
            how_a_link_is_wired(graph, link_id),
            Ok(HowALinkIsWired::ThroughAChannel
                | HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead)
        )
}

/// Close the channel of each link of the nodes a live edit restarts the way it was wired before
/// the edit, keeping the link in the graph so the compile wires it again: a call inside an
/// island has no channel, a link into a fused node was read at its island's host, and an island
/// its reader evaluates is taken off that reader.
pub(crate) fn unwire_each_link_of_the_nodes_restarted(
    graph: &mut Graph,
    restarted: &RunningNodesALiveEditRestarts,
) -> Result<()> {
    unwire_each_link_keeping_it(graph, &restarted.links)
}

/// Close the channel of each of `links` the way it was wired, keeping it in the graph, as
/// [`unwire_each_link_of_the_nodes_restarted`] does for the links of a node restarted.
pub(crate) fn unwire_each_link_keeping_it(graph: &mut Graph, links: &[LinkUniqueId]) -> Result<()> {
    for link_id in links {
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
        if how_a_link_is_wired(graph, link_id)
            .is_ok_and(|how| how == HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead)
        {
            unwire_a_link_from_a_function_node_island_into_its_reader(graph, link_id)?;
            continue;
        }
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

/// Hold each link of `restarted` that `links_to_add` wires again in the same compile as pending:
/// a channel out of a restarted node is sized by the links it is wired for, and a link left
/// disconnected does not count, so a channel into a windowed reader would be opened shallower
/// than the reader's ring.
pub(crate) fn hold_each_link_to_wire_again_as_pending(
    graph: &mut Graph,
    restarted: &RunningNodesALiveEditRestarts,
    links_to_add: &[LinkUniqueId],
) {
    for link_id in restarted
        .links
        .iter()
        .filter(|link_id| links_to_add.contains(link_id))
    {
        if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
            link.insert(LinkStateComponent(LinkState::Pending));
        }
    }
}

/// Forget what building each of `nodes` attached, its thread already stopped, so the compile
/// prepares and spawns it again as a node just added; its state reads `Pending` until then.
pub(crate) fn forget_the_build_of_each_node_restarted(graph: &mut Graph, nodes: &[NodeRunId]) {
    for node_id in nodes {
        let Some(node) = graph.traversal_mut().v(node_id).first_mut() else {
            continue;
        };
        node.remove::<NodeInstanceComponent>();
        node.remove::<OutOfProcessLinkWiringComponent>();
        node.remove::<HelperProcessRestartsComponent>();
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

#[cfg(test)]
mod tests;
