// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Shader node islands: a shader node whose Image only one shader node reads
//! is fused into that reader's island, which one evaluator runs on the node
//! whose Image leaves it.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;

use super::running_node_restart::RunningNodesALiveEditRestarts;
use super::shader_node_island_program::{
    ShaderNodeIslandMember, ShaderNodeIslandMemberInput, ShaderNodeIslandProgram,
};
use super::shader_node_program::ShaderNodeInputValueType;
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNode, GraphNodeWithComponents, InputLinkPortRef, Link, LinkUniqueId, NodeRunId,
    ShaderNodeFusedGroupComponent, ShaderNodeFusedGroupOfAnIsland, StateComponent,
    cast_exposed_name_to_url_safe,
};

/// The island a shader node evaluates, on the node whose Image leaves it, with where its
/// evaluator, once running, takes the island placed on it anew.
pub(crate) struct ShaderNodeIslandHostedHereComponent {
    pub(crate) island: ShaderNodeIslandProgram,
    pub(crate) fused_group: Arc<ShaderNodeFusedGroupOfAnIsland>,
    pub(crate) handed_to_its_evaluator: Arc<ShaderNodeIslandHandedToItsEvaluator>,
}

/// An island with the fused group it is evaluated as, handed over to a running evaluator.
pub(crate) struct ShaderNodeIslandWithItsFusedGroup {
    pub(crate) island: ShaderNodeIslandProgram,
    pub(crate) fused_group: Arc<ShaderNodeFusedGroupOfAnIsland>,
}

/// An island, with its fused group, handed to the evaluator that runs on its host, which takes
/// it in place of the one it evaluates at its next read.
#[derive(Default)]
pub(crate) struct ShaderNodeIslandHandedToItsEvaluator(
    Mutex<Option<ShaderNodeIslandWithItsFusedGroup>>,
);

impl ShaderNodeIslandHandedToItsEvaluator {
    /// Hand `handed_over` over, in place of any handed before and not taken.
    pub(crate) fn hand_over(&self, handed_over: ShaderNodeIslandWithItsFusedGroup) {
        *self.0.lock() = Some(handed_over);
    }

    /// The island and fused group last handed over and not yet taken.
    pub(crate) fn take(&self) -> Option<ShaderNodeIslandWithItsFusedGroup> {
        self.0.lock().take()
    }
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
    let links_out: Vec<&Link> = the_links_out_of_that_stand(graph, node_id);
    let [only_link_out] = links_out.as_slice() else {
        return None;
    };
    let reader = only_link_out.to_port().node_run_id();
    is_a_shader_node(graph, reader).then(|| reader.clone())
}

/// The links out of `node_id` that stand, which the islands of the graph are read through: a
/// link or node pending deletion is gone once the edit that removes it is compiled.
fn the_links_out_of_that_stand<'graph>(
    graph: &'graph Graph,
    node_id: &NodeRunId,
) -> Vec<&'graph Link> {
    graph
        .traversal()
        .v(node_id)
        .out_e()
        .iter()
        .filter(|link| graph.stands(link))
        .collect()
}

/// The links into `node_id` that stand, as [`the_links_out_of_that_stand`] reads them.
fn the_links_into_that_stand<'graph>(
    graph: &'graph Graph,
    node_id: &NodeRunId,
) -> Vec<&'graph Link> {
    graph
        .traversal()
        .v(node_id)
        .in_e()
        .iter()
        .filter(|link| graph.stands(link))
        .collect()
}

/// Every link into or out of `node_id`, standing or not.
fn the_links_of<'graph>(
    graph: &'graph Graph,
    node_id: &NodeRunId,
) -> impl Iterator<Item = &'graph Link> + 'graph {
    graph
        .traversal()
        .v(node_id)
        .in_e()
        .iter()
        .chain(graph.traversal().v(node_id).out_e().iter())
}

/// Whether `link`, which stands, is a call inside one shader node island.
pub(crate) fn is_a_call_inside_a_shader_node_island(graph: &Graph, link: &Link) -> bool {
    the_shader_node_a_shader_node_is_fused_into(graph, link.source().node_run_id()).is_some()
}

/// Whether `link` is wired as a call inside a shader node island as the islands are placed now:
/// its source was placed fused into the island of another.
pub(crate) fn is_wired_as_a_call_inside_a_shader_node_island(graph: &Graph, link: &Link) -> bool {
    was_placed_fused_into_the_island_of_another(graph, link.source().node_run_id())
}

/// Whether `node_id` was placed fused into the island another node evaluates.
fn was_placed_fused_into_the_island_of_another(graph: &Graph, node_id: &NodeRunId) -> bool {
    graph
        .traversal()
        .v(node_id)
        .first()
        .is_some_and(|node| node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>())
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
        for link in the_links_into_that_stand(graph, &member_id) {
            let source = link.source().node_run_id();
            if is_a_call_inside_a_shader_node_island(graph, link) && !members.contains(source) {
                members.push(source.clone());
            }
        }
    }
    members
}

/// Where a link from outside an island into one of its members is read, as the islands are
/// placed now: the input port of the host its reader was placed in the island of.
pub(crate) fn where_a_link_into_a_fused_shader_node_is_read(
    graph: &Graph,
    link: &Link,
) -> Option<InputLinkPortRef> {
    let reader = link.to_port().node_run_id();
    if is_wired_as_a_call_inside_a_shader_node_island(graph, link)
        || !was_placed_fused_into_the_island_of_another(graph, reader)
    {
        return None;
    }
    Some(the_host_input_port_a_fused_shader_node_input_is_read_at(
        &the_host_the_island_of_a_fused_node_was_placed_on(graph, reader)?,
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
    let island = ShaderNodeIslandProgram::of_the_members(members)
        .map_err(|why| the_refusal_of_a_fused_group_not_built(graph, host_id, why))?;
    Ok((island, member_nodes_in_evaluation_order))
}

/// The refusal of the fused group `host_id` evaluates, which cannot be built for `why`.
fn the_refusal_of_a_fused_group_not_built(
    graph: &Graph,
    host_id: &NodeRunId,
    why: String,
) -> Error {
    Error::ShaderNodeFusedGroupNotBuilt {
        evaluated_by: graph
            .traversal()
            .v(host_id)
            .first()
            .map_or_else(|| host_id.to_string(), |host| host.display_name.clone()),
        why,
    }
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
            the_refusal_of_a_fused_group_not_built(
                graph,
                host_id,
                format!(
                    "shader node `{}` is of type `{}`, which this stream holds no shader \
                     program for",
                    node.display_name, node.node_type
                ),
            )
        })?;
    let links_in: Vec<&Link> = the_links_into_that_stand(graph, node_id);
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
            ShaderNodeInputValueType::Numbers(_) => ShaderNodeIslandMemberInput::NumbersReadAt {
                input_port: read_at,
                numbers_until_one_is_read: numbers_of_a_shader_node_input(
                    node,
                    &signature_input.port,
                    &signature_input.value_type,
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

/// The numbers input `port` of shader node `node`, taking values of
/// `value_type`, holds while nothing arrives on it: those of its pin value,
/// else of its declared default, else of zero.
pub(crate) fn numbers_of_a_shader_node_input(
    node: &GraphNode,
    port: &str,
    value_type: &ShaderNodeInputValueType,
) -> Vec<f32> {
    value_type.numbers_a_stored_value_holds(node.input_values.get(port).or_else(|| {
        node.ports
            .inputs
            .iter()
            .find(|input| input.name == port)
            .and_then(|input| input.pin_default.as_ref())
    }))
}

/// The shader nodes of a compile placed on the islands that evaluate them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ShaderNodesPlacedOnTheirIslands {
    /// Each shader node another node evaluates in its island, which runs once that node does.
    pub(crate) fused_into_another: HashSet<NodeRunId>,
    /// Each shader node that reads its own Image through the shader nodes it feeds, failed with
    /// why, so no node evaluates it and it runs nothing.
    pub(crate) in_a_cycle_no_node_evaluates: Vec<NodeRunId>,
}

/// Place the island of each shader node of `nodes_to_place` on the node that evaluates it: the
/// nodes a compile adds, and the running ones a live edit places anew. A host already placed
/// keeps where its running evaluator takes the island placed on it now. A shader node reading
/// its own Image through the shader nodes it feeds is failed with why on its node, rather than
/// the compile refused, so the rest of the stream plays.
pub(crate) fn place_each_shader_node_island_on_its_host(
    graph: &mut Graph,
    nodes_to_place: &[NodeRunId],
) -> Result<ShaderNodesPlacedOnTheirIslands> {
    let mut placed = ShaderNodesPlacedOnTheirIslands::default();
    let mut islands_read = Vec::new();
    for node_id in nodes_to_place {
        if !is_a_shader_node(graph, node_id) {
            continue;
        }
        if is_fused_into_the_shader_node_island_of_another(graph, node_id) {
            if is_fused_into_the_shader_node_island_of_another(
                graph,
                &the_host_of_the_shader_node_island_of(graph, node_id),
            ) {
                if let Some(state) = graph
                    .traversal()
                    .v(node_id)
                    .first()
                    .and_then(|node| node.get::<StateComponent>())
                {
                    state.fail_with(format!(
                        "shader node `{node_id}` reads its own Image through the shader nodes \
                         it feeds, so no node evaluates their island. To fix: break the cycle"
                    ));
                }
                placed.in_a_cycle_no_node_evaluates.push(node_id.clone());
                continue;
            }
            placed.fused_into_another.insert(node_id.clone());
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
            let handed_to_its_evaluator = host
                .get::<ShaderNodeIslandHostedHereComponent>()
                .map(|hosted| Arc::clone(&hosted.handed_to_its_evaluator))
                .unwrap_or_default();
            host.insert_component_without_rendering_it(ShaderNodeIslandHostedHereComponent {
                island,
                fused_group,
                handed_to_its_evaluator,
            });
        }
    }
    Ok(placed)
}

/// What a live edit changes of the shader node islands already running, read before the edit
/// changes anything, so a fused group that would not build refuses the edit whole.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ShaderNodeIslandsALiveEditPlacesAnew {
    /// Each running host that hosts an island after the edit too: its evaluator keeps running
    /// and takes the island placed on it anew.
    pub(crate) hosts_handed_their_island_anew: Vec<NodeRunId>,
    /// Each running shader node fused into the island of another before and after the edit,
    /// placed in the island it is fused into now.
    pub(crate) fused_nodes_placed_anew: Vec<NodeRunId>,
    /// Each running shader node whose place changes: a host now fused into another's island,
    /// or in a cycle, whose evaluator stops, or a node now evaluating an island of its own,
    /// whose evaluator is built.
    pub(crate) nodes_whose_place_changes: Vec<NodeRunId>,
    /// Each link of the running shader nodes placed anew that the edit neither adds nor removes
    /// and that is wired another way once they are, or joins a node whose place changes: closed
    /// as it was wired and wired again.
    pub(crate) links_wired_anew: Vec<LinkUniqueId>,
}

impl ShaderNodeIslandsALiveEditPlacesAnew {
    /// Each running shader node the edit places anew, whatever its place.
    pub(crate) fn the_running_nodes_placed_anew(&self) -> Vec<NodeRunId> {
        [
            self.hosts_handed_their_island_anew.as_slice(),
            self.fused_nodes_placed_anew.as_slice(),
            self.nodes_whose_place_changes.as_slice(),
        ]
        .concat()
    }

    /// The nodes whose place changes, to stop where they ran and build where they run now,
    /// with the links wired anew, closed and wired again with them.
    pub(crate) fn the_nodes_and_links_built_again_in_their_new_place(
        &self,
    ) -> RunningNodesALiveEditRestarts {
        RunningNodesALiveEditRestarts {
            nodes: self.nodes_whose_place_changes.clone(),
            links: self.links_wired_anew.clone(),
        }
    }
}

/// How a link touching a shader node island is wired.
#[derive(Debug, PartialEq, Eq)]
enum HowALinkOfAShaderNodeIslandIsWired {
    /// As a call between two members of one island.
    AsACallInsideTheIsland,
    /// Through a channel into this input port of the host of its reader's island.
    IntoTheHostOfItsReader(InputLinkPortRef),
    /// Through a channel between its two ends' own ports.
    BetweenItsOwnEnds,
}

/// How `link` is wired as the islands are placed now.
fn how_a_link_of_a_shader_node_island_is_wired_now(
    graph: &Graph,
    link: &Link,
) -> HowALinkOfAShaderNodeIslandIsWired {
    if is_wired_as_a_call_inside_a_shader_node_island(graph, link) {
        return HowALinkOfAShaderNodeIslandIsWired::AsACallInsideTheIsland;
    }
    where_a_link_into_a_fused_shader_node_is_read(graph, link).map_or(
        HowALinkOfAShaderNodeIslandIsWired::BetweenItsOwnEnds,
        HowALinkOfAShaderNodeIslandIsWired::IntoTheHostOfItsReader,
    )
}

/// How `link`, which stands, is wired once the islands are placed as the links that stand read
/// them.
fn how_a_link_of_a_shader_node_island_is_wired_once_placed_anew(
    graph: &Graph,
    link: &Link,
) -> HowALinkOfAShaderNodeIslandIsWired {
    if is_a_call_inside_a_shader_node_island(graph, link) {
        return HowALinkOfAShaderNodeIslandIsWired::AsACallInsideTheIsland;
    }
    let reader = link.to_port().node_run_id();
    if !is_fused_into_the_shader_node_island_of_another(graph, reader) {
        return HowALinkOfAShaderNodeIslandIsWired::BetweenItsOwnEnds;
    }
    HowALinkOfAShaderNodeIslandIsWired::IntoTheHostOfItsReader(
        the_host_input_port_a_fused_shader_node_input_is_read_at(
            &the_host_of_the_shader_node_island_of(graph, reader),
            reader,
            link.to_port().port_name(),
        ),
    )
}

/// The running shader node islands a live edit places anew: every running shader node linked,
/// through shader nodes, to a shader node whose Image a link added or removed changes the
/// readers of, or to a node the edit removes. Each fused group the edit leaves, of those and of
/// the shader nodes it adds, is built first, so one that does not build refuses the edit with
/// [`Error::ShaderNodeFusedGroupNotBuilt`] before anything changes.
///
/// A host that stays one keeps its evaluator, which takes its new island; a node whose place
/// changes is stopped where it ran and built where it runs now; nothing else restarts.
pub(crate) fn the_shader_node_islands_a_live_edit_places_anew(
    graph: &Graph,
    nodes_to_add: &[NodeRunId],
    nodes_to_remove: &[NodeRunId],
    links_changed: &[LinkUniqueId],
) -> Result<ShaderNodeIslandsALiveEditPlacesAnew> {
    let shader_nodes_reached =
        the_shader_nodes_an_edit_reaches(graph, nodes_to_add, nodes_to_remove, links_changed);
    let place_of: Vec<(&NodeRunId, ShaderNodePlaceOnceEdited)> = shader_nodes_reached
        .iter()
        .map(|node_id| (node_id, the_place_once_edited_of(graph, node_id)))
        .collect();
    refuse_unless_each_fused_group_reached_builds(graph, &place_of)?;

    let already_running =
        |node_id: &NodeRunId| !nodes_to_add.contains(node_id) && !nodes_to_remove.contains(node_id);
    let mut placed_anew = ShaderNodeIslandsALiveEditPlacesAnew::default();
    for (node_id, place) in place_of
        .iter()
        .filter(|(node_id, _)| already_running(node_id))
    {
        let Some(node) = graph.traversal().v(*node_id).first() else {
            continue;
        };
        let to_list = match place {
            ShaderNodePlaceOnceEdited::HostsAnIsland
                if node.has::<ShaderNodeIslandHostedHereComponent>() =>
            {
                &mut placed_anew.hosts_handed_their_island_anew
            }
            ShaderNodePlaceOnceEdited::FusedInto(_)
                if node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>() =>
            {
                &mut placed_anew.fused_nodes_placed_anew
            }
            _ => &mut placed_anew.nodes_whose_place_changes,
        };
        to_list.push((*node_id).clone());
    }
    placed_anew.links_wired_anew =
        the_links_wired_anew(graph, &placed_anew, links_changed, already_running);
    Ok(placed_anew)
}

/// Where a shader node is placed once an edit is compiled, as the links that stand read it.
#[derive(Debug, PartialEq, Eq)]
enum ShaderNodePlaceOnceEdited {
    /// It evaluates an island of its own.
    HostsAnIsland,
    /// It is fused into the island this host evaluates.
    FusedInto(NodeRunId),
    /// It is fused into a cycle of shader nodes, which no island evaluates.
    InACycle,
}

fn the_place_once_edited_of(graph: &Graph, node_id: &NodeRunId) -> ShaderNodePlaceOnceEdited {
    let host_id = the_host_of_the_shader_node_island_of(graph, node_id);
    if is_fused_into_the_shader_node_island_of_another(graph, &host_id) {
        ShaderNodePlaceOnceEdited::InACycle
    } else if host_id == *node_id {
        ShaderNodePlaceOnceEdited::HostsAnIsland
    } else {
        ShaderNodePlaceOnceEdited::FusedInto(host_id)
    }
}

/// Every shader node, sorted, linked through shader nodes to a shader node whose Image a link
/// in `links_changed` changes the readers of, to a node in `nodes_to_remove`, or to a shader
/// node in `nodes_to_add`; the nodes removed are left out.
fn the_shader_nodes_an_edit_reaches(
    graph: &Graph,
    nodes_to_add: &[NodeRunId],
    nodes_to_remove: &[NodeRunId],
    links_changed: &[LinkUniqueId],
) -> Vec<NodeRunId> {
    let mut shader_nodes_to_visit: Vec<NodeRunId> = Vec::new();
    for link in links_changed
        .iter()
        .filter_map(|link_id| graph.traversal().e(link_id).first())
        .chain(
            nodes_to_remove
                .iter()
                .flat_map(|node_id| the_links_of(graph, node_id)),
        )
    {
        let (source_id, reader_id) = (link.source().node_run_id(), link.to_port().node_run_id());
        if is_a_shader_node(graph, source_id) {
            shader_nodes_to_visit.push(source_id.clone());
            if is_a_shader_node(graph, reader_id) {
                shader_nodes_to_visit.push(reader_id.clone());
            }
        }
    }
    shader_nodes_to_visit.extend(
        nodes_to_add
            .iter()
            .filter(|node_id| is_a_shader_node(graph, node_id))
            .cloned(),
    );

    let mut shader_nodes_reached = HashSet::new();
    while let Some(node_id) = shader_nodes_to_visit.pop() {
        if nodes_to_remove.contains(&node_id) || !shader_nodes_reached.insert(node_id.clone()) {
            continue;
        }
        shader_nodes_to_visit.extend(
            the_links_of(graph, &node_id)
                .map(|link| {
                    if link.source().node_run_id() == &node_id {
                        link.to_port().node_run_id().clone()
                    } else {
                        link.source().node_run_id().clone()
                    }
                })
                .filter(|linked| is_a_shader_node(graph, linked)),
        );
    }
    let mut shader_nodes_reached: Vec<NodeRunId> = shader_nodes_reached.into_iter().collect();
    shader_nodes_reached.sort();
    shader_nodes_reached
}

/// Build the island of each host among `place_of`, refusing with
/// [`Error::ShaderNodeFusedGroupNotBuilt`] at the first that does not build.
fn refuse_unless_each_fused_group_reached_builds(
    graph: &Graph,
    place_of: &[(&NodeRunId, ShaderNodePlaceOnceEdited)],
) -> Result<()> {
    let mut hosts_built = HashSet::new();
    for (node_id, place) in place_of {
        let host_id = match place {
            ShaderNodePlaceOnceEdited::HostsAnIsland => *node_id,
            ShaderNodePlaceOnceEdited::FusedInto(host_id) => host_id,
            ShaderNodePlaceOnceEdited::InACycle => continue,
        };
        if hosts_built.insert(host_id) {
            the_shader_node_island_hosted_by(graph, host_id)?;
        }
    }
    Ok(())
}

/// Each link, sorted, of the running shader nodes `placed_anew` places that the edit leaves
/// standing and does not change itself, between two running nodes, and that joins a node whose
/// place changes or is wired another way once the islands are placed anew.
fn the_links_wired_anew(
    graph: &Graph,
    placed_anew: &ShaderNodeIslandsALiveEditPlacesAnew,
    links_changed: &[LinkUniqueId],
    already_running: impl Fn(&NodeRunId) -> bool,
) -> Vec<LinkUniqueId> {
    let mut links_wired_anew: Vec<LinkUniqueId> = Vec::new();
    for node_id in placed_anew.the_running_nodes_placed_anew() {
        for link in the_links_of(graph, &node_id) {
            let (source_id, reader_id) =
                (link.source().node_run_id(), link.to_port().node_run_id());
            if links_changed.contains(&link.id)
                || links_wired_anew.contains(&link.id)
                || !graph.stands(link)
                || !already_running(source_id)
                || !already_running(reader_id)
                || graph.the_pin_types_of_the_link_differ(&link.id)
            {
                continue;
            }
            let joins_a_node_whose_place_changes = [source_id, reader_id]
                .into_iter()
                .any(|end| placed_anew.nodes_whose_place_changes.contains(end));
            if joins_a_node_whose_place_changes
                || how_a_link_of_a_shader_node_island_is_wired_now(graph, link)
                    != how_a_link_of_a_shader_node_island_is_wired_once_placed_anew(graph, link)
            {
                links_wired_anew.push(link.id.clone());
            }
        }
    }
    links_wired_anew.sort();
    links_wired_anew
}

/// Hand the evaluator running on each of `hosts` the island placed on its host now, which it
/// takes in place of the one it evaluates at its next read.
pub(crate) fn hand_each_running_host_the_island_placed_on_it(graph: &Graph, hosts: &[NodeRunId]) {
    for host_id in hosts {
        let Some(hosted) = graph
            .traversal()
            .v(host_id)
            .first()
            .and_then(|host| host.get::<ShaderNodeIslandHostedHereComponent>())
        else {
            continue;
        };
        tracing::info!(
            "Handing `{host_id}` the fused group of shader nodes this edit placed on it, which \
             it takes at its next frame"
        );
        hosted
            .handed_to_its_evaluator
            .hand_over(ShaderNodeIslandWithItsFusedGroup {
                island: hosted.island.clone(),
                fused_group: Arc::clone(&hosted.fused_group),
            });
    }
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

#[cfg(test)]
mod tests;
