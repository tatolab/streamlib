// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Shader node islands: a shader node whose Image only one shader node reads
//! is fused into that reader's island, which one evaluator runs on the node
//! whose Image leaves it.

use std::collections::HashSet;
use std::sync::Arc;

use super::open_iceoryx2_service_op::{
    close_iceoryx2_service, close_iceoryx2_service_into_the_host_of_a_shader_node_island,
};
use super::shader_node_island_program::{
    ShaderNodeIslandMember, ShaderNodeIslandMemberInput, ShaderNodeIslandProgram,
};
use super::shader_node_program::ShaderNodeInputValueType;
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNode, GraphNodeWithComponents, InputLinkPortRef, Link,
    LinkState, LinkStateComponent, LinkUniqueId, NodeInstanceComponent, NodeMetrics,
    NodePauseGateComponent, NodeRunId, ShaderNodeFusedGroupComponent,
    ShaderNodeFusedGroupOfAnIsland, ShutdownChannelComponent, StateComponent,
    StepProgressOfOneNodeComponent, cast_exposed_name_to_url_safe,
};
use crate::core::nodes::NodeState;

/// The island a shader node evaluates, on the node whose Image leaves it.
pub(crate) struct ShaderNodeIslandHostedHereComponent {
    pub(crate) island: ShaderNodeIslandProgram,
    pub(crate) fused_group: Arc<ShaderNodeFusedGroupOfAnIsland>,
}

/// On a shader node fused into the island another node evaluates.
pub(crate) struct ShaderNodeFusedIntoTheIslandOfAnotherComponent;

/// Whether `node_id` is a shader node.
pub(crate) fn is_a_shader_node(graph: &Graph, node_id: &NodeRunId) -> bool {
    graph
        .traversal()
        .v(node_id)
        .first()
        .is_some_and(|node| node.node_kind() == Some(NodeKind::Shader))
}

/// The shader node `node_id` is fused into: the one shader node reading its
/// Image, when nothing else reads it.
fn the_shader_node_a_shader_node_is_fused_into(
    graph: &Graph,
    node_id: &NodeRunId,
) -> Option<NodeRunId> {
    if !is_a_shader_node(graph, node_id) {
        return None;
    }
    let links_out: Vec<&Link> = graph.traversal().v(node_id).out_e().iter().collect();
    let [only_link_out] = links_out.as_slice() else {
        return None;
    };
    let reader = only_link_out.to_port().node_run_id();
    is_a_shader_node(graph, reader).then(|| reader.clone())
}

/// Whether `link` is a call inside one shader node island.
pub(crate) fn is_a_call_inside_a_shader_node_island(graph: &Graph, link: &Link) -> bool {
    the_shader_node_a_shader_node_is_fused_into(graph, link.source().node_run_id()).is_some()
}

/// The node evaluating the island `node_id` belongs to; `node_id` itself when
/// its Image leaves its island.
pub(crate) fn the_host_of_the_shader_node_island_of(
    graph: &Graph,
    node_id: &NodeRunId,
) -> NodeRunId {
    let mut host = node_id.clone();
    let mut visited = HashSet::new();
    while visited.insert(host.clone()) {
        match the_shader_node_a_shader_node_is_fused_into(graph, &host) {
            Some(reader) => host = reader,
            None => break,
        }
    }
    host
}

/// Whether `node_id` is a shader node another node evaluates in its island.
pub(crate) fn is_fused_into_the_shader_node_island_of_another(
    graph: &Graph,
    node_id: &NodeRunId,
) -> bool {
    the_shader_node_a_shader_node_is_fused_into(graph, node_id).is_some()
}

/// The input port of its island's host that input `port` of fused shader node
/// `node_id` is read at, spelled in the cast form every mailbox read casts a
/// port name to.
pub(crate) fn the_host_input_port_a_fused_shader_node_input_is_read_at(
    host_id: &NodeRunId,
    node_id: &NodeRunId,
    port: &str,
) -> InputLinkPortRef {
    let port_name = format!("shader_node_island_input_{node_id}_{port}");
    let port_name = cast_exposed_name_to_url_safe(&port_name)
        .map(|cast_port_name| cast_port_name.into_owned())
        .unwrap_or(port_name);
    InputLinkPortRef::new(host_id.clone(), port_name)
}

/// The input port of its island's host that input `port` of member `member_id` is read at: the
/// port itself on the host, the cast fused port on any other member.
pub(crate) fn the_host_input_port_a_member_input_is_read_at(
    host_id: &NodeRunId,
    member_id: &NodeRunId,
    port: &str,
) -> String {
    if member_id == host_id {
        return port.to_owned();
    }
    the_host_input_port_a_fused_shader_node_input_is_read_at(host_id, member_id, port)
        .port_name()
        .to_string()
}

/// Each member of the shader node island `host_id` hosts: the host, then each shader node fused
/// into it, each once and after the member that calls it.
pub(crate) fn the_members_of_the_shader_node_island_hosted_by(
    graph: &Graph,
    host_id: &NodeRunId,
) -> Vec<NodeRunId> {
    let mut members = vec![host_id.clone()];
    let mut next_to_walk = 0;
    while let Some(member_id) = members.get(next_to_walk).cloned() {
        next_to_walk += 1;
        let links_in: Vec<&Link> = graph.traversal().v(&member_id).in_e().iter().collect();
        for link in links_in {
            let source = link.source().node_run_id();
            if is_a_call_inside_a_shader_node_island(graph, link) && !members.contains(source) {
                members.push(source.clone());
            }
        }
    }
    members
}

/// Where a link from outside an island into one of its members is read: the
/// host's input port it is wired to.
pub(crate) fn where_a_link_into_a_fused_shader_node_is_read(
    graph: &Graph,
    link: &Link,
) -> Option<InputLinkPortRef> {
    let reader = link.to_port().node_run_id();
    if is_a_call_inside_a_shader_node_island(graph, link)
        || !is_fused_into_the_shader_node_island_of_another(graph, reader)
    {
        return None;
    }
    Some(the_host_input_port_a_fused_shader_node_input_is_read_at(
        &the_host_of_the_shader_node_island_of(graph, reader),
        reader,
        link.to_port().port_name(),
    ))
}

/// Read the island hosted by `host_id`, members in evaluation order, each
/// member's inputs in signature order, with the node each member is.
pub(crate) fn the_shader_node_island_hosted_by(
    graph: &Graph,
    host_id: &NodeRunId,
) -> Result<(ShaderNodeIslandProgram, Vec<NodeRunId>)> {
    let mut members: Vec<ShaderNodeIslandMember> = Vec::new();
    let mut member_nodes_in_evaluation_order: Vec<NodeRunId> = Vec::new();
    add_the_member_after_every_member_it_reads(
        graph,
        host_id,
        host_id,
        &mut members,
        &mut member_nodes_in_evaluation_order,
    )?;
    let island = ShaderNodeIslandProgram::of_the_members(members).map_err(Error::Configuration)?;
    Ok((island, member_nodes_in_evaluation_order))
}

fn add_the_member_after_every_member_it_reads(
    graph: &Graph,
    host_id: &NodeRunId,
    node_id: &NodeRunId,
    members: &mut Vec<ShaderNodeIslandMember>,
    member_nodes_in_evaluation_order: &mut Vec<NodeRunId>,
) -> Result<usize> {
    let node = graph
        .traversal()
        .v(node_id)
        .first()
        .ok_or_else(|| Error::NodeNotFound(node_id.to_string()))?;
    let program = graph
        .node_types_this_stream_resolves()
        .shader_node_program_of(&node.node_type)
        .ok_or_else(|| {
            Error::Configuration(format!(
                "shader node `{}` is of type `{}`, which this stream holds no shader program for",
                node.display_name, node.node_type
            ))
        })?;
    let links_in: Vec<&Link> = graph.traversal().v(node_id).in_e().iter().collect();
    let mut inputs = Vec::with_capacity(program.inputs.len());
    for signature_input in &program.inputs {
        let link_in = links_in
            .iter()
            .find(|link| link.to_port().port_name() == signature_input.port);
        let read_at =
            the_host_input_port_a_member_input_is_read_at(host_id, node_id, &signature_input.port);
        let input = match signature_input.value_type {
            ShaderNodeInputValueType::ImageFrame => match link_in {
                Some(link) if is_a_call_inside_a_shader_node_island(graph, link) => {
                    ShaderNodeIslandMemberInput::ImageFromTheMember(
                        add_the_member_after_every_member_it_reads(
                            graph,
                            host_id,
                            link.source().node_run_id(),
                            members,
                            member_nodes_in_evaluation_order,
                        )?,
                    )
                }
                _ => ShaderNodeIslandMemberInput::ImageReadAt(read_at),
            },
            ShaderNodeInputValueType::Float => ShaderNodeIslandMemberInput::FloatReadAt {
                input_port: read_at,
                value_until_one_is_read: pin_value_of_a_shader_node_input(
                    node,
                    &signature_input.port,
                ),
            },
        };
        inputs.push(input);
    }
    members.push(ShaderNodeIslandMember {
        node_name: node.display_name.clone(),
        program,
        inputs,
    });
    member_nodes_in_evaluation_order.push(node_id.clone());
    Ok(members.len() - 1)
}

/// The value input `port` of shader node `node` takes while nothing arrives
/// on it: its pin value, else its declared default, else zero.
pub(crate) fn pin_value_of_a_shader_node_input(node: &GraphNode, port: &str) -> f32 {
    node.pins
        .get(port)
        .or_else(|| {
            node.ports
                .inputs
                .iter()
                .find(|input| input.name == port)
                .and_then(|input| input.pin_default.as_ref())
        })
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0) as f32
}

pub(crate) fn place_each_shader_node_island_on_its_host(
    graph: &mut Graph,
    nodes_to_add: &[NodeRunId],
) -> Result<HashSet<NodeRunId>> {
    let mut fused_into_another: HashSet<NodeRunId> = HashSet::new();
    let mut islands_read = Vec::new();
    for node_id in nodes_to_add {
        if !is_a_shader_node(graph, node_id) {
            continue;
        }
        if is_fused_into_the_shader_node_island_of_another(graph, node_id) {
            if is_fused_into_the_shader_node_island_of_another(
                graph,
                &the_host_of_the_shader_node_island_of(graph, node_id),
            ) {
                return Err(Error::Configuration(format!(
                    "shader node `{node_id}` reads its own Image through the shader nodes it \
                     feeds, so no node evaluates their island. To fix: break the cycle"
                )));
            }
            fused_into_another.insert(node_id.clone());
            continue;
        }
        islands_read.push((
            node_id.clone(),
            the_shader_node_island_hosted_by(graph, node_id)?,
        ));
    }
    for (host_id, (island, member_ids)) in islands_read {
        let host_name = island.output_member().node_name.clone();
        let member_names: Vec<String> = island
            .members()
            .iter()
            .map(|member| member.node_name.clone())
            .collect();
        let fused_group = Arc::new(ShaderNodeFusedGroupOfAnIsland::of_the_members(
            host_name,
            member_names,
        ));
        for member_id in member_ids {
            let Some(member) = graph.traversal_mut().v(&member_id).first_mut() else {
                continue;
            };
            member.insert(ShaderNodeFusedGroupComponent(Arc::clone(&fused_group)));
            if member_id != host_id {
                member.insert_component_without_rendering_it(
                    ShaderNodeFusedIntoTheIslandOfAnotherComponent,
                );
            }
        }
        if let Some(host) = graph.traversal_mut().v(&host_id).first_mut() {
            host.insert_component_without_rendering_it(ShaderNodeIslandHostedHereComponent {
                island,
                fused_group,
            });
        }
    }
    Ok(fused_into_another)
}

/// The running shader nodes a live edit restarts so their islands are placed
/// again, and the links rewired with them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RunningShaderNodesALiveEditRestarts {
    pub(crate) nodes: Vec<NodeRunId>,
    pub(crate) links: Vec<LinkUniqueId>,
}

impl RunningShaderNodesALiveEditRestarts {
    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// The running shader nodes whose islands a link added or removed while the
/// stream runs changes — one between two shader nodes, or one into or out of a
/// node fused into another's island — with every running shader node linked
/// to them through other shader nodes, and every other link into or out of
/// them. A shader node holds no state, so each is stopped and built again
/// with its island placed anew. A link going with a node the same edit
/// removes changes no island that keeps running.
///
/// Refused by name, changing nothing, when one of those links joins a function
/// node, whose wiring into an island is placed once, when its reader starts.
pub(crate) fn the_running_shader_nodes_a_live_edit_restarts(
    graph: &Graph,
    nodes_to_add: &[NodeRunId],
    nodes_to_remove: &[NodeRunId],
    links_changed: &[LinkUniqueId],
) -> Result<RunningShaderNodesALiveEditRestarts> {
    let already_running =
        |node_id: &NodeRunId| !nodes_to_add.contains(node_id) && !nodes_to_remove.contains(node_id);
    let mut shader_nodes_to_visit: Vec<NodeRunId> = Vec::new();
    for link_id in links_changed {
        let Some(link) = graph.traversal().e(link_id).first() else {
            continue;
        };
        let (source_id, reader_id) = (link.source().node_run_id(), link.to_port().node_run_id());
        if nodes_to_remove.contains(source_id) || nodes_to_remove.contains(reader_id) {
            continue;
        }
        let ends_in_a_running_island = [source_id, reader_id].into_iter().any(|node_id| {
            already_running(node_id)
                && graph.traversal().v(node_id).first().is_some_and(|node| {
                    node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>()
                })
        });
        let joins_running_shader_nodes = is_a_shader_node(graph, source_id)
            && is_a_shader_node(graph, reader_id)
            && (already_running(source_id) || already_running(reader_id));
        if ends_in_a_running_island || joins_running_shader_nodes {
            shader_nodes_to_visit.extend(
                [source_id, reader_id]
                    .into_iter()
                    .filter(|node_id| is_a_shader_node(graph, node_id))
                    .cloned(),
            );
        }
    }

    let mut shader_nodes_visited: HashSet<NodeRunId> = HashSet::new();
    let mut nodes = Vec::new();
    while let Some(node_id) = shader_nodes_to_visit.pop() {
        if nodes_to_remove.contains(&node_id) || !shader_nodes_visited.insert(node_id.clone()) {
            continue;
        }
        if already_running(&node_id) {
            nodes.push(node_id.clone());
        }
        let linked_shader_nodes = graph
            .traversal()
            .v(&node_id)
            .out_e()
            .iter()
            .map(|link| link.to_port().node_run_id().clone())
            .chain(
                graph
                    .traversal()
                    .v(&node_id)
                    .in_e()
                    .iter()
                    .map(|link| link.source().node_run_id().clone()),
            )
            .filter(|linked| is_a_shader_node(graph, linked))
            .collect::<Vec<_>>();
        shader_nodes_to_visit.extend(linked_shader_nodes);
    }
    nodes.sort();

    let mut links = Vec::new();
    for node_id in &nodes {
        let links_of_the_node: Vec<&Link> = graph
            .traversal()
            .v(node_id)
            .in_e()
            .iter()
            .chain(graph.traversal().v(node_id).out_e().iter())
            .collect();
        for link in links_of_the_node {
            if links.contains(&link.id) {
                continue;
            }
            let (source_id, reader_id) =
                (link.source().node_run_id(), link.to_port().node_run_id());
            if nodes_to_remove.contains(source_id) || nodes_to_remove.contains(reader_id) {
                continue;
            }
            let function_node_joined =
                [source_id, reader_id].into_iter().find(|node_id| {
                    graph.traversal().v(*node_id).first().is_some_and(|node| {
                        matches!(node.node_kind(), Some(NodeKind::Function { .. }))
                    })
                });
            if let Some(function_node_id) = function_node_joined {
                let name_of = |node_id: &NodeRunId| {
                    graph
                        .traversal()
                        .v(node_id)
                        .first()
                        .map_or_else(|| node_id.to_string(), |node| node.display_name.clone())
                };
                return Err(Error::Configuration(format!(
                    "the edit changes the fused group of running shader node `{}`, and its link \
                     `{}` joins function node `{}`, whose place in an island is read once, when \
                     its nodes start. To fix: stop the stream, make the edit, and run it again",
                    name_of(node_id),
                    link.id,
                    name_of(function_node_id),
                )));
            }
            if !links_changed.contains(&link.id) {
                links.push(link.id.clone());
            }
        }
    }
    links.sort();
    Ok(RunningShaderNodesALiveEditRestarts { nodes, links })
}

/// Close the channel of each link of the shader nodes a live edit restarts the
/// way it was wired before the edit, keeping the link in the graph so the
/// compile wires it again: a call inside an island has no channel, and a link
/// into a fused node was read at its island's host.
pub(crate) fn unwire_each_link_of_the_shader_nodes_restarted(
    graph: &mut Graph,
    restarted: &RunningShaderNodesALiveEditRestarts,
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
fn the_host_the_island_of_a_fused_node_was_placed_on(
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

pub(crate) fn forget_the_build_of_each_shader_node_restarted(
    graph: &mut Graph,
    nodes: &[NodeRunId],
) {
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

#[cfg(test)]
mod tests;
