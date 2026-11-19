// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Evaluation islands: each connected run of function nodes, run inside the
//! read of the node it feeds.
//!
//! A link between two function nodes is a function call, and one from a
//! function node into a helper's input port carries the island upstream of it
//! to that helper: in its setup command, or handed over once it runs. An edit
//! that changes an island hands its reader the island again. Neither kind of
//! link opens a channel, and no function node is prepared or spawned.

mod island_edit_refusals;
mod island_placement;

pub(crate) use island_edit_refusals::{
    is_a_reactive_reader_only_islands_feed, refuse_a_link_an_island_cannot_take,
    refuse_a_link_feeding_an_island_into_the_app_process,
    refuse_a_link_leaving_a_reactive_reader_fed_only_by_islands,
    refuse_an_edit_an_island_cannot_take,
    the_island_links_into_a_reactive_reader_only_islands_feed, the_reader_a_removal_strands,
    the_refusal_of_a_removal_stranding,
};
use island_edit_refusals::{
    the_refusal_of_a_reactive_reader_fed_only_by_islands,
    the_refusal_of_an_island_fed_into_the_app_process,
};
pub(crate) use island_placement::{the_island_feeding_a_link, the_state_fields_of_a_function_node};

use std::collections::HashSet;

use serde_json::Value as JsonValue;
use streamlib_project_repository::PREVIOUS_VALUE_NODE_TYPE;

use crate::core::NodeRunId;
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    FunctionNodeHostComponent, FunctionNodePromotedToItsOwnProcessComponent,
    FunctionNodeRanCompiledComponent, Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    HowItsHelpersRanAFunctionNode, InputLinkPortRef, Link,
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkState, LinkStateComponent,
    LinkUniqueId, OutOfProcessLinkWireRepliesComponent, OutOfProcessLinkWiringComponent,
    StateComponent, the_outputs_a_link_carries_on,
};
use crate::core::nodes::OutOfProcessLinkWireReply;
use crate::iceoryx2::{Iceoryx2Node, InboundLinkName};

use super::open_iceoryx2_service_op::{
    close_a_read_of_a_link_by_the_reader_of_a_function_node_island,
    open_a_read_of_a_link_by_the_reader_of_a_function_node_island,
};
use super::shader_node_island::{
    is_a_call_inside_a_shader_node_island, where_a_link_into_a_fused_shader_node_is_read,
};

/// How the compiler wires one link, by the kinds of its two ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HowALinkIsWired {
    /// Through an iceoryx2 channel: neither end runs inside a reader's read.
    ThroughAChannel,
    /// As a function call between two function nodes of one island.
    AsACallInsideAFunctionNodeIsland,
    /// As the island upstream of it, carried to the helper the link feeds.
    AsTheIslandItsReaderEvaluatesAtEachRead,
    /// Through an iceoryx2 channel into the reader of the function node's
    /// island, at an input port of the reader's own opened for the link: a
    /// link from a node outside any island into a function node.
    ThroughAChannelReadByTheReaderOfItsFunctionNodeIsland,
    /// As a call between two shader nodes of one island, inside one kernel
    /// or between two kernels of one submission.
    AsACallInsideAShaderNodeIsland,
    /// Through an iceoryx2 channel into this input port of the node
    /// evaluating the shader node island the link's reader is fused into.
    ThroughAChannelIntoTheHostOfItsShaderNodeIsland(InputLinkPortRef),
}

/// The input port of the reader `island_link_id` carries its island to that
/// the channel of `link_id`, into one of the island's function nodes, is wired
/// into: one for each island link, so two islands one reader evaluates never
/// take each other's messages.
///
/// Wire contract with the helper bootstrap: an island node's `links_into`
/// entry names it as `{"port", "from": {"reader_input_port"}}`.
pub(crate) fn the_reader_input_port_a_link_into_a_function_node_is_read_at(
    link_id: &LinkUniqueId,
    island_link_id: &LinkUniqueId,
) -> String {
    format!("function_node_island_input_{link_id}_{island_link_id}")
}

/// Whether `node_id` is a function node that runs inside the read of each
/// node it feeds: every function node, until one is promoted to a process of
/// its own.
pub(crate) fn runs_inside_the_read_of_each_node_it_feeds(
    graph: &Graph,
    node_id: &NodeRunId,
) -> bool {
    graph.traversal().v(node_id).first().is_some_and(|node| {
        node.node_kind()
            .is_some_and(|node_kind| matches!(node_kind, NodeKind::Function { .. }))
            && !node.has::<FunctionNodePromotedToItsOwnProcessComponent>()
    })
}

/// Whether `node_id` is a `PreviousValue` node, whose input the island reads
/// from the last evaluation.
pub(crate) fn is_a_previous_value_node(graph: &Graph, node_id: &NodeRunId) -> bool {
    graph
        .traversal()
        .v(node_id)
        .first()
        .is_some_and(|node| node.node_type.to_string() == PREVIOUS_VALUE_NODE_TYPE)
}

/// Whether `node_id` is a function node, inside its readers' islands or
/// promoted out of them.
pub(crate) fn is_a_function_node(graph: &Graph, node_id: &NodeRunId) -> bool {
    graph.traversal().v(node_id).first().is_some_and(|node| {
        node.node_kind()
            .is_some_and(|node_kind| matches!(node_kind, NodeKind::Function { .. }))
    })
}

/// How `link_id` is wired, by whether each of its ends runs inside the read
/// of each node it feeds.
pub(crate) fn how_a_link_is_wired(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<HowALinkIsWired> {
    let link = link_in(graph, link_id)?;
    if is_a_call_inside_a_shader_node_island(graph, link) {
        return Ok(HowALinkIsWired::AsACallInsideAShaderNodeIsland);
    }
    if let Some(host_input_port) = where_a_link_into_a_fused_shader_node_is_read(graph, link) {
        return Ok(
            HowALinkIsWired::ThroughAChannelIntoTheHostOfItsShaderNodeIsland(host_input_port),
        );
    }
    Ok(
        match (
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id()),
            runs_inside_the_read_of_each_node_it_feeds(graph, link.to_port().node_run_id()),
        ) {
            (false, false) => HowALinkIsWired::ThroughAChannel,
            (true, true) => HowALinkIsWired::AsACallInsideAFunctionNodeIsland,
            (true, false) => HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead,
            (false, true) => HowALinkIsWired::ThroughAChannelReadByTheReaderOfItsFunctionNodeIsland,
        },
    )
}

/// Wire `link_id`, from a node outside any island into a function node: open
/// a read of its channel at the reader of each island link whose island calls
/// that node, which reads it at each evaluation; or mark it wired with no
/// channel while no reader evaluates the node yet, and the island link that
/// carries it to a reader opens that read.
pub(crate) fn wire_a_link_into_a_function_node_island_through_its_reader(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let function_node_id = link_in(graph, link_id)?.to_port().node_run_id().clone();
    let island_links_calling_it: Vec<LinkUniqueId> = the_island_links(graph)
        .into_iter()
        .filter(|island_link_id| {
            the_function_nodes_an_island_link_evaluates(graph, island_link_id)
                .contains(&function_node_id)
        })
        .collect();
    if island_links_calling_it.is_empty() {
        return mark_a_link(graph, link_id, LinkState::Wired);
    }
    for island_link_id in island_links_calling_it {
        open_a_read_of_a_link_at_the_reader_of_an_island_link_unless_open(
            graph,
            link_id,
            &island_link_id,
            iceoryx2_node,
        )?;
    }
    Ok(())
}

fn open_a_read_of_a_link_at_the_reader_of_an_island_link_unless_open(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    island_link_id: &LinkUniqueId,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    if is_read_by_the_reader_of_an_island_link(graph, link_id, island_link_id) {
        return Ok(());
    }
    let reader_id = link_in(graph, island_link_id)?
        .to_port()
        .node_run_id()
        .clone();
    let reader = graph
        .traversal()
        .v(&reader_id)
        .first()
        .ok_or_else(|| Error::NodeNotFound(reader_id.to_string()))?;
    if !reader.has::<OutOfProcessLinkWiringComponent>() {
        return Err(Error::InvalidLink(format!(
            "link `{link_id}` feeds a function node island read by `{reader_id}`, which runs in \
             the app process; a function node runs inside the read of a node in a helper \
             process, and the app process runs no node code"
        )));
    }
    tracing::info!(
        "Opening a read of {link_id} at `{reader_id}`, which evaluates the function node island \
         {island_link_id} carries"
    );
    open_a_read_of_a_link_by_the_reader_of_a_function_node_island(
        graph,
        link_id,
        island_link_id,
        InputLinkPortRef::new(
            reader_id,
            the_reader_input_port_a_link_into_a_function_node_is_read_at(link_id, island_link_id),
        ),
        iceoryx2_node,
    )
}

/// `island_link_id`'s island output node and every function node upstream of
/// it, the nodes its reader evaluates.
fn the_function_nodes_an_island_link_evaluates(
    graph: &Graph,
    island_link_id: &LinkUniqueId,
) -> HashSet<NodeRunId> {
    let Some(island_output_node_id) = graph
        .traversal()
        .e(island_link_id)
        .first()
        .map(|island_link| island_link.source().node_run_id().clone())
    else {
        return HashSet::new();
    };
    let mut function_nodes = HashSet::from([island_output_node_id.clone()]);
    function_nodes.extend(the_function_nodes_upstream_of(
        graph,
        &island_output_node_id,
    ));
    function_nodes
}

/// Each link from a function node into a node outside any island, which
/// carries the island its reader evaluates.
fn the_island_links(graph: &Graph) -> Vec<LinkUniqueId> {
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
                && !runs_inside_the_read_of_each_node_it_feeds(graph, link.to_port().node_run_id())
        })
        .map(|link| link.id.clone())
        .collect()
}

fn is_read_by_the_reader_of_an_island_link(
    graph: &Graph,
    link_id: &LinkUniqueId,
    island_link_id: &LinkUniqueId,
) -> bool {
    graph.traversal().e(link_id).first().is_some_and(|link| {
        link.get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
            .is_some_and(|read_through| {
                read_through
                    .the_read_for_the_island_link(island_link_id)
                    .is_some()
            })
    })
}

/// Open, at the reader of `island_link_id`, a read of each link from outside
/// any island into the function nodes its island calls that this reader does
/// not read yet.
fn open_the_reads_an_island_link_needs_at_its_reader(
    graph: &mut Graph,
    island_link_id: &LinkUniqueId,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    for link_into_the_island in
        the_links_an_island_link_reads_from_outside_any_island(graph, island_link_id)
    {
        open_a_read_of_a_link_at_the_reader_of_an_island_link_unless_open(
            graph,
            &link_into_the_island,
            island_link_id,
            iceoryx2_node,
        )?;
    }
    Ok(())
}

/// Open each read of a link from outside any island that an island an edit
/// changed now needs, at that island's reader, before the reader is handed the
/// island.
pub(crate) fn open_the_channels_each_island_newly_reads(
    graph: &mut Graph,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    for island_link_id in the_island_links(graph) {
        open_the_reads_an_island_link_needs_at_its_reader(graph, &island_link_id, iceoryx2_node)?;
    }
    Ok(())
}

/// Close each read of a link from outside any island whose island link an edit
/// has since removed, or left without the function node the link feeds; a link
/// left with no read stays wired with no channel until a reader evaluates that
/// node again.
pub(crate) fn close_the_channels_no_island_reads_any_more(graph: &mut Graph) -> Result<()> {
    let island_links: HashSet<LinkUniqueId> = the_island_links(graph).into_iter().collect();
    let mut reads_no_island_needs: Vec<(LinkUniqueId, LinkUniqueId)> = Vec::new();
    for link in graph.traversal().e(()).iter() {
        let Some(read_through) =
            link.get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
        else {
            continue;
        };
        for read in &read_through.reads_by_island_link {
            let still_needed = island_links.contains(&read.island_link_id)
                && the_function_nodes_an_island_link_evaluates(graph, &read.island_link_id)
                    .contains(link.to_port().node_run_id());
            if !still_needed {
                reads_no_island_needs.push((link.id.clone(), read.island_link_id.clone()));
            }
        }
    }
    for (link_id, island_link_id) in reads_no_island_needs {
        tracing::info!(
            "Closing the read of {link_id} for {island_link_id}: that island no longer \
             evaluates the function node it feeds"
        );
        close_a_read_of_a_link_by_the_reader_of_a_function_node_island(
            graph,
            &link_id,
            &island_link_id,
        )?;
    }
    Ok(())
}

/// Each link from outside any island that island link `island_link_id`, which `graph` holds,
/// reads at its reader: one into a function node the island link evaluates.
pub(crate) fn the_links_an_island_link_reads_from_outside_any_island(
    graph: &Graph,
    island_link_id: &LinkUniqueId,
) -> Vec<LinkUniqueId> {
    let function_node_ids: Vec<NodeRunId> =
        the_function_nodes_an_island_link_evaluates(graph, island_link_id)
            .into_iter()
            .collect();
    the_links_into_function_nodes_from_outside_any_island(graph, &function_node_ids)
}

/// The links into the function nodes `function_node_ids` from nodes outside
/// any island.
fn the_links_into_function_nodes_from_outside_any_island(
    graph: &Graph,
    function_node_ids: &[NodeRunId],
) -> Vec<LinkUniqueId> {
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            function_node_ids.contains(link.to_port().node_run_id())
                && !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
        })
        .map(|link| link.id.clone())
        .collect()
}

/// The function nodes upstream of `node_id` through function nodes, the
/// island its inputs evaluate.
fn the_function_nodes_upstream_of(graph: &Graph, node_id: &NodeRunId) -> Vec<NodeRunId> {
    let mut function_nodes_upstream = Vec::new();
    let mut nodes_to_follow = vec![node_id.clone()];
    let mut followed = HashSet::new();
    while let Some(following_id) = nodes_to_follow.pop() {
        if !followed.insert(following_id.clone()) {
            continue;
        }
        for link in graph.traversal().e(()).iter() {
            if link.to_port().node_run_id() != &following_id {
                continue;
            }
            let upstream_id = link.source().node_run_id();
            if runs_inside_the_read_of_each_node_it_feeds(graph, upstream_id) {
                function_nodes_upstream.push(upstream_id.clone());
                nodes_to_follow.push(upstream_id.clone());
            }
        }
    }
    function_nodes_upstream
}

/// Wire `link_id`, from a function node into a helper's input port: record the
/// island upstream of it for the helper, hand it over if the helper already
/// runs, and mark the link wired, or pending until that helper answers.
///
/// Refused by name when the destination runs in the app process, which runs no
/// node code.
pub(crate) fn wire_a_link_from_a_function_node_island_into_its_reader(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let island = the_island_feeding_a_link(graph, link_id)?;
    let reader_id = link_in(graph, link_id)?.to_port().node_run_id().clone();
    let reader_link_wiring = graph
        .traversal()
        .v(&reader_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .map(|component| std::sync::Arc::clone(&component.0))
        .ok_or_else(|| {
            the_refusal_of_an_island_fed_into_the_app_process(
                &format!("link `{link_id}`"),
                &reader_id,
            )
        })?;
    if is_a_reactive_reader_only_islands_feed(graph, &reader_id) {
        return Err(the_refusal_of_a_reactive_reader_fed_only_by_islands(
            &reader_id,
        ));
    }
    let function_node_ids = the_function_node_ids_an_island_calls(&island);
    open_the_reads_an_island_link_needs_at_its_reader(graph, link_id, iceoryx2_node)?;
    let answer_awaited_from_the_reader = reader_link_wiring
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(island)?;
    reader_link_wiring.tell_each_function_node_how_its_far_side_ran_it(
        function_node_ids.iter().filter_map(|function_node_id| {
            the_report_of_how_its_helpers_ran_a_function_node(graph, function_node_id)
                .map(|how_it_ran| (function_node_id.to_string(), how_it_ran))
        }),
    );
    record_the_stream_facts_an_island_link_carries_on(graph, link_id)?;
    mark_an_island_link_by_whether_its_reader_owes_an_answer(
        graph,
        link_id,
        answer_awaited_from_the_reader,
    )
}

/// Record, at the reader of island link `link_id`, the output whose messages
/// the island hands on unread through an output that follows an input, so the
/// reader reaches that output's stream facts at its input port. A link whose
/// island hands out values of its own, or carries on more than one output or
/// only another function node's, records none.
fn record_the_stream_facts_an_island_link_carries_on(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let link = link_in(graph, link_id)?;
    let reader_input_port = link.to_port().port_name();
    let reader = graph
        .traversal()
        .v(link.to_port().node_run_id())
        .first()
        .ok_or_else(|| Error::NodeNotFound(link.to_port().node_run_id().to_string()))?;
    let upstream_stream_facts_by_inbound_link = reader.upstream_stream_facts_by_inbound_link();
    upstream_stream_facts_by_inbound_link.forget_inbound_link(link_id);
    let island_output_node = graph
        .traversal()
        .v(link.source().node_run_id())
        .first()
        .ok_or_else(|| Error::NodeNotFound(link.source().node_run_id().to_string()))?;
    let outputs_carried_on =
        the_outputs_a_link_carries_on(graph, island_output_node, link.source().port_name());
    let [(carried_on_node, carried_on_port, _)] = outputs_carried_on.as_slice() else {
        return Ok(());
    };
    if is_a_function_node(graph, &carried_on_node.id) {
        return Ok(());
    }
    let inbound_link_name =
        crate::iceoryx2::source_channel_name(carried_on_node.id.as_str(), carried_on_port)
            .map_err(|why| {
                Error::Configuration(format!(
                    "cannot derive the channel name of `{}.{carried_on_port}`: {why}",
                    carried_on_node.id
                ))
            })?;
    upstream_stream_facts_by_inbound_link.record_inbound_link(
        reader_input_port,
        link_id,
        InboundLinkName::from(inbound_link_name.into_string().as_str()),
        carried_on_port,
        std::sync::Arc::clone(carried_on_node.stream_facts_of_output_ports()),
    );
    Ok(())
}

/// Hand each reader whose island a compile changed that island again, in
/// place of the one it evaluates: an island link wired before this compile,
/// whose island now differs from the one its reader was handed, is re-recorded
/// and, for a reader that already runs, handed over.
///
/// The links `links_wired_this_compile` carried their islands as they were
/// wired, so they are not handed again.
pub(crate) fn hand_each_reader_the_island_a_compile_changed(
    graph: &mut Graph,
    links_wired_this_compile: &[LinkUniqueId],
) -> Result<()> {
    let island_links: Vec<LinkUniqueId> = the_island_links(graph)
        .into_iter()
        .filter(|link_id| !links_wired_this_compile.contains(link_id))
        .collect();
    hand_each_reader_its_island_again_when_it_changed(graph, island_links)
}

/// Hand each reader of an island evaluating one of the function nodes `function_node_ids` that
/// island again, once, when it differs from the one its reader was handed, as a value it reads
/// changed.
pub(crate) fn hand_each_reader_an_island_evaluating_the_nodes_again_when_it_changed(
    graph: &mut Graph,
    function_node_ids: &[NodeRunId],
) -> Result<()> {
    let island_links: Vec<LinkUniqueId> = the_island_links(graph)
        .into_iter()
        .filter(|link_id| {
            let evaluated = the_function_nodes_an_island_link_evaluates(graph, link_id);
            function_node_ids
                .iter()
                .any(|function_node_id| evaluated.contains(function_node_id))
        })
        .collect();
    hand_each_reader_its_island_again_when_it_changed(graph, island_links)
}

/// Hand the reader of each of `island_links` its island again, in place of the one it
/// evaluates, when the two differ: re-recorded and, for a reader that already runs, handed over.
fn hand_each_reader_its_island_again_when_it_changed(
    graph: &mut Graph,
    island_links: Vec<LinkUniqueId>,
) -> Result<()> {
    for link_id in island_links {
        let reader_id = link_in(graph, &link_id)?.to_port().node_run_id().clone();
        let Some(reader_link_wiring) = graph
            .traversal()
            .v(&reader_id)
            .first()
            .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
            .map(|component| std::sync::Arc::clone(&component.0))
        else {
            continue;
        };
        record_the_stream_facts_an_island_link_carries_on(graph, &link_id)?;
        let island_as_it_is_now = the_island_feeding_a_link(graph, &link_id)?;
        if reader_link_wiring
            .the_function_node_island_recorded_for_a_link(link_id.as_str())
            .as_ref()
            == Some(&island_as_it_is_now)
        {
            continue;
        }
        tracing::info!(
            "Handing `{reader_id}` the function node island of {link_id} again, as this edit \
             changed it"
        );
        let function_node_ids = the_function_node_ids_an_island_calls(&island_as_it_is_now);
        let answer_awaited_from_the_reader = reader_link_wiring
            .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
                island_as_it_is_now,
            )?;
        reader_link_wiring.tell_each_function_node_how_its_far_side_ran_it(
            function_node_ids.iter().filter_map(|function_node_id| {
                the_report_of_how_its_helpers_ran_a_function_node(graph, function_node_id)
                    .map(|how_it_ran| (function_node_id.to_string(), how_it_ran))
            }),
        );
        mark_an_island_link_by_whether_its_reader_owes_an_answer(
            graph,
            &link_id,
            answer_awaited_from_the_reader,
        )?;
    }
    Ok(())
}

/// Note on each function node where it runs, which `graph` renders as its
/// `host`: inside the read of each node its island feeds, or in a helper
/// process of its own.
pub(crate) fn note_where_each_function_node_runs(graph: &mut Graph) {
    let hosts: Vec<(NodeRunId, FunctionNodeHostComponent)> = graph
        .traversal()
        .v(())
        .ids()
        .into_iter()
        .filter(|node_id| is_a_function_node(graph, node_id))
        .map(|node_id| {
            let host = if runs_inside_the_read_of_each_node_it_feeds(graph, &node_id) {
                let mut reader_names: Vec<String> =
                    the_readers_a_function_node_feeds(graph, &node_id)
                        .iter()
                        .filter_map(|reader_id| {
                            graph
                                .traversal()
                                .v(reader_id)
                                .first()
                                .map(|reader| reader.display_name.clone())
                        })
                        .collect();
                reader_names.sort();
                FunctionNodeHostComponent::InsideTheReadOf(reader_names)
            } else {
                FunctionNodeHostComponent::InAHelperProcessOfItsOwn
            };
            (node_id, host)
        })
        .collect();
    for (function_node_id, host) in hosts {
        if let Some(node) = graph.traversal_mut().v(&function_node_id).first_mut() {
            node.insert(host);
        }
    }
}

fn the_function_node_ids_an_island_calls(island: &JsonValue) -> Vec<NodeRunId> {
    island
        .get("nodes")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|node| node.get("node").and_then(JsonValue::as_str))
        .map(NodeRunId::from)
        .collect()
}

/// Mark an island link wired, or pending on the answer its running reader owes
/// for the island it was just handed, which `graph` reads as it lands.
fn mark_an_island_link_by_whether_its_reader_owes_an_answer(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    answer_awaited_from_the_reader: Option<std::sync::Arc<OutOfProcessLinkWireReply>>,
) -> Result<()> {
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    match answer_awaited_from_the_reader {
        None => link.insert(LinkStateComponent(LinkState::Wired)),
        Some(answer_cell) => {
            link.insert(LinkStateComponent(LinkState::Pending));
            link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
                answer_cell,
            ]));
        }
    }
    Ok(())
}

/// Where the helpers evaluating `function_node_id` note how they ran it, which
/// `graph` renders on the node as `ran_compiled`, and what its calls raised,
/// which it renders as the node's `process_raising_because`; attached on its
/// first reader or its promotion, whichever comes first.
pub(crate) fn the_report_of_how_its_helpers_ran_a_function_node(
    graph: &mut Graph,
    function_node_id: &NodeRunId,
) -> Option<std::sync::Arc<HowItsHelpersRanAFunctionNode>> {
    let node = graph.traversal_mut().v(function_node_id).first_mut()?;
    if let Some(component) = node.get::<FunctionNodeRanCompiledComponent>() {
        return Some(std::sync::Arc::clone(&component.0));
    }
    let function_node_state = node.get::<StateComponent>()?.clone_inner();
    let how_it_ran = std::sync::Arc::new(HowItsHelpersRanAFunctionNode::showing_its_raises_on(
        function_node_state,
    ));
    node.insert(FunctionNodeRanCompiledComponent(std::sync::Arc::clone(
        &how_it_ran,
    )));
    Some(how_it_ran)
}

/// Whether a node outside any island feeds `node_id`, directly or through an
/// island it evaluates, whose messages wake it.
fn is_fed_by_a_node_that_is_not_a_function_node(graph: &Graph, node_id: &NodeRunId) -> bool {
    let function_nodes_upstream = the_function_nodes_upstream_of(graph, node_id);
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            link.to_port().node_run_id() == node_id
                || function_nodes_upstream.contains(link.to_port().node_run_id())
        })
        .any(|link| !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id()))
}

/// Unwire `link_id`, from a function node into its reader: close each read
/// its reader holds for it, forget the island it carries on the reader's setup
/// command and mark it disconnected.
pub(crate) fn unwire_a_link_from_a_function_node_island_into_its_reader(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let reader_id = link_in(graph, link_id)?.to_port().node_run_id().clone();
    let links_the_unwired_island_link_reads: Vec<LinkUniqueId> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            link.get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
                .is_some_and(|read_through| {
                    read_through.the_read_for_the_island_link(link_id).is_some()
                })
        })
        .map(|link| link.id.clone())
        .collect();
    for link_read_for_the_unwired_island_link in links_the_unwired_island_link_reads {
        close_a_read_of_a_link_by_the_reader_of_a_function_node_island(
            graph,
            &link_read_for_the_unwired_island_link,
            link_id,
        )?;
    }
    if let Some(reader) = graph.traversal().v(&reader_id).first() {
        reader
            .upstream_stream_facts_by_inbound_link()
            .forget_inbound_link(link_id);
        if let Some(reader_link_wiring) = reader.get::<OutOfProcessLinkWiringComponent>() {
            reader_link_wiring
                .0
                .forget_a_function_node_island(link_id.as_str());
        }
    }
    mark_a_link(graph, link_id, LinkState::Disconnected)
}

/// Mark `link_id`, a call between two function nodes, wired.
pub(crate) fn wire_a_call_inside_a_function_node_island(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    mark_a_link(graph, link_id, LinkState::Wired)
}

fn mark_a_link(graph: &mut Graph, link_id: &LinkUniqueId, link_state: LinkState) -> Result<()> {
    graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?
        .insert(LinkStateComponent(link_state));
    Ok(())
}

/// The nodes outside any island that `function_node_id`'s outputs reach,
/// through the function nodes downstream of it.
fn the_readers_a_function_node_feeds(
    graph: &Graph,
    function_node_id: &NodeRunId,
) -> HashSet<NodeRunId> {
    the_reader_input_ports_a_function_node_feeds(graph, function_node_id)
        .into_iter()
        .map(|(reader_id, _)| reader_id)
        .collect()
}

/// The nodes outside any island that a link into `destination_id` reaches: the readers a
/// function node feeds, or the node itself.
pub(crate) fn the_readers_a_link_into_reaches(
    graph: &Graph,
    destination_id: &NodeRunId,
) -> HashSet<NodeRunId> {
    if runs_inside_the_read_of_each_node_it_feeds(graph, destination_id) {
        the_readers_a_function_node_feeds(graph, destination_id)
    } else {
        HashSet::from([destination_id.clone()])
    }
}

/// Each input port, of a node outside any island, that `function_node_id`
/// reaches through the function nodes downstream of it.
pub(crate) fn the_reader_input_ports_a_function_node_feeds(
    graph: &Graph,
    function_node_id: &NodeRunId,
) -> HashSet<(NodeRunId, String)> {
    let mut reader_input_ports = HashSet::new();
    let mut function_nodes_to_follow = vec![function_node_id.clone()];
    let mut function_nodes_followed = HashSet::new();
    while let Some(following_id) = function_nodes_to_follow.pop() {
        if !function_nodes_followed.insert(following_id.clone()) {
            continue;
        }
        for link in graph.traversal().e(()).iter() {
            if link.source().node_run_id() != &following_id {
                continue;
            }
            let destination_id = link.to_port().node_run_id();
            if runs_inside_the_read_of_each_node_it_feeds(graph, destination_id) {
                function_nodes_to_follow.push(destination_id.clone());
            } else {
                reader_input_ports.insert((
                    destination_id.clone(),
                    link.to_port().port_name().to_string(),
                ));
            }
        }
    }
    reader_input_ports
}

/// The function nodes among `node_ids`, which are never prepared or spawned.
pub(crate) fn the_function_nodes_among<'a>(
    graph: &Graph,
    node_ids: impl IntoIterator<Item = &'a NodeRunId>,
) -> HashSet<NodeRunId> {
    node_ids
        .into_iter()
        .filter(|node_id| runs_inside_the_read_of_each_node_it_feeds(graph, node_id))
        .cloned()
        .collect()
}

pub(crate) fn link_in<'graph>(
    graph: &'graph Graph,
    link_id: &LinkUniqueId,
) -> Result<&'graph Link> {
    graph
        .traversal()
        .e(link_id)
        .first()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))
}

#[cfg(test)]
pub(crate) mod tests;
