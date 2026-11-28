// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Restarting running nodes inside a live edit's compile: a shader node whose place in the
//! islands the edit changes, a host now fused into another's island or a fused node now
//! evaluating its own. Each is stopped, its links unwired and its build forgotten, then built
//! and wired again by the same compile, while every other node keeps running. No input's value
//! restarts a node.

use super::function_node_island::{
    HowALinkIsWired, how_a_link_is_wired, unwire_a_link_from_a_function_node_island_into_its_reader,
};
use super::open_iceoryx2_service_op::{
    close_iceoryx2_service, close_iceoryx2_service_into_the_host_of_a_shader_node_island,
};
use super::shader_node_island::{
    ShaderNodeFusedIntoTheIslandOfAnotherComponent, ShaderNodeIslandHostedHereComponent,
};
use crate::core::error::Result;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, HelperProcessRestartsComponent,
    LinkState, LinkStateComponent, LinkUniqueId, NodeInstanceComponent, NodeMetrics,
    NodePauseGateComponent, NodeRunId, OutOfProcessLinkWiringComponent,
    ShaderNodeFusedGroupComponent, ShutdownChannelComponent, StateComponent,
    StepProgressOfOneNodeComponent,
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
        self.nodes.is_empty() && self.links.is_empty()
    }
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
        if graph.traversal().e(link_id).first().is_none() {
            continue;
        }
        match how_a_link_is_wired(graph, link_id)? {
            HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead => {
                unwire_a_link_from_a_function_node_island_into_its_reader(graph, link_id)?;
            }
            HowALinkIsWired::AsACallInsideAShaderNodeIsland => {
                if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
                    link.insert(LinkStateComponent(LinkState::Disconnected));
                }
            }
            HowALinkIsWired::ThroughAChannelIntoTheHostOfItsShaderNodeIsland(host_input_port) => {
                close_iceoryx2_service_into_the_host_of_a_shader_node_island(
                    graph,
                    link_id,
                    &host_input_port,
                )?;
            }
            HowALinkIsWired::ThroughAChannel
            | HowALinkIsWired::AsACallInsideAFunctionNodeIsland
            | HowALinkIsWired::ThroughAChannelReadByTheReaderOfItsFunctionNodeIsland => {
                close_iceoryx2_service(graph, link_id)?;
            }
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
