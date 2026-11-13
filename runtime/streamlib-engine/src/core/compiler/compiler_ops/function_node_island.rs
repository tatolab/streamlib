// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Evaluation islands: each connected run of function nodes, run inside the
//! read of the node it feeds.
//!
//! A link between two function nodes is a function call, and one from a
//! function node into a helper's input port carries the island upstream of it
//! to that helper: in its setup command, or handed over once it runs. An edit
//! that changes an island hands its reader the island again. Neither kind of
//! link opens a channel, and no function node is prepared or spawned.

use std::collections::{HashMap, HashSet};

use serde_json::Value as JsonValue;

use crate::core::ProcessorUniqueId;
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodeHostComponent, FunctionNodePromotedToItsOwnProcessComponent,
    FunctionNodeRanCompiledComponent, Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    GraphWeight, HowItsHelpersRanAFunctionNode, InputLinkPortRef, Link,
    LinkReadFromTheLastEvaluationComponent,
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkState, LinkStateComponent,
    LinkUniqueId, OutOfProcessLinkWireRepliesComponent, OutOfProcessLinkWiringComponent,
    the_outputs_a_link_carries_on,
};
use crate::core::processors::OutOfProcessLinkWireReply;
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
    node_id: &ProcessorUniqueId,
) -> bool {
    graph.traversal().v(node_id).first().is_some_and(|node| {
        node.node_kind()
            .is_some_and(|node_kind| matches!(node_kind, NodeKind::Function { .. }))
            && !node.has::<FunctionNodePromotedToItsOwnProcessComponent>()
    })
}

/// The links between function nodes that close a loop of them, each read as it
/// was at the last evaluation: the links a depth-first walk down the links,
/// from each function node no other feeds, finds pointing back at a node it is
/// still walking below. Nodes are walked in the order of the ids their stream
/// graph file gives them and a node's links in the order of what they feed, so
/// one graph names the same links at every compile. What is left without them
/// has no cycle.
pub(crate) fn the_links_read_from_the_last_evaluation(graph: &Graph) -> HashSet<LinkUniqueId> {
    let mut function_nodes_in_walk_order: Vec<(uuid::Uuid, ProcessorUniqueId)> = graph
        .traversal()
        .v(())
        .iter()
        .filter(|node| runs_inside_the_read_of_each_node_it_feeds(graph, &node.id))
        .map(|node| (node.node_uuid, node.id.clone()))
        .collect();
    function_nodes_in_walk_order.sort();
    let walk_position_of_each_function_node: HashMap<&ProcessorUniqueId, usize> =
        function_nodes_in_walk_order
            .iter()
            .enumerate()
            .map(|(walk_position, (_, node_id))| (node_id, walk_position))
            .collect();

    let mut links_out_of_each_function_node: Vec<Vec<(usize, &Link)>> =
        vec![Vec::new(); function_nodes_in_walk_order.len()];
    let mut is_fed_by_another_function_node = vec![false; function_nodes_in_walk_order.len()];
    for link in graph.traversal().e(()).iter() {
        let (Some(&source_position), Some(&fed_position)) = (
            walk_position_of_each_function_node.get(link.source().processor_id()),
            walk_position_of_each_function_node.get(link.to_port().processor_id()),
        ) else {
            continue;
        };
        links_out_of_each_function_node[source_position].push((fed_position, link));
        is_fed_by_another_function_node[fed_position] |= fed_position != source_position;
    }
    for links_out in &mut links_out_of_each_function_node {
        links_out.sort_by(|(fed, link), (other_fed, other_link)| {
            (fed, link.to_port().port_name(), link.source().port_name()).cmp(&(
                other_fed,
                other_link.to_port().port_name(),
                other_link.source().port_name(),
            ))
        });
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Walked {
        NotYet,
        StillBelowIt,
        Done,
    }
    let mut walked = vec![Walked::NotYet; function_nodes_in_walk_order.len()];
    let mut links_read_from_the_last_evaluation = HashSet::new();
    let fed_by_no_other_then_every_node = (0..walked.len())
        .filter(|&position| !is_fed_by_another_function_node[position])
        .chain(0..walked.len());
    for walk_start in fed_by_no_other_then_every_node {
        if walked[walk_start] != Walked::NotYet {
            continue;
        }
        walked[walk_start] = Walked::StillBelowIt;
        let mut path_with_the_next_link_of_each: Vec<(usize, usize)> = vec![(walk_start, 0)];
        while let Some((position, next_link)) = path_with_the_next_link_of_each.last_mut() {
            let Some(&(fed_position, link)) =
                links_out_of_each_function_node[*position].get(*next_link)
            else {
                walked[*position] = Walked::Done;
                path_with_the_next_link_of_each.pop();
                continue;
            };
            *next_link += 1;
            match walked[fed_position] {
                Walked::StillBelowIt => {
                    links_read_from_the_last_evaluation.insert(link.id.clone());
                }
                Walked::NotYet => {
                    walked[fed_position] = Walked::StillBelowIt;
                    path_with_the_next_link_of_each.push((fed_position, 0));
                }
                Walked::Done => {}
            }
        }
    }
    links_read_from_the_last_evaluation
}

/// Mark each link the island walk reads from the last evaluation, and unmark
/// each it no longer does, so `graph` shows which link of a loop is the one
/// read a step late.
pub(crate) fn note_which_links_read_the_last_evaluation(graph: &mut Graph) {
    let links_read_from_the_last_evaluation = the_links_read_from_the_last_evaluation(graph);
    for link_id in graph.traversal().e(()).ids() {
        let Some(link) = graph.traversal_mut().e(&link_id).first_mut() else {
            continue;
        };
        let is_marked = link.has::<LinkReadFromTheLastEvaluationComponent>();
        match (
            links_read_from_the_last_evaluation.contains(&link_id),
            is_marked,
        ) {
            (true, false) => link.insert(LinkReadFromTheLastEvaluationComponent),
            (false, true) => {
                link.remove::<LinkReadFromTheLastEvaluationComponent>();
            }
            _ => {}
        }
    }
}

/// Whether `node_id` is a function node, inside its readers' islands or
/// promoted out of them.
pub(crate) fn is_a_function_node(graph: &Graph, node_id: &ProcessorUniqueId) -> bool {
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
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id()),
            runs_inside_the_read_of_each_node_it_feeds(graph, link.to_port().processor_id()),
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
    let function_node_id = link_in(graph, link_id)?.to_port().processor_id().clone();
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
        .processor_id()
        .clone();
    let reader = graph
        .traversal()
        .v(&reader_id)
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(reader_id.to_string()))?;
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
) -> HashSet<ProcessorUniqueId> {
    let Some(island_output_node_id) = graph
        .traversal()
        .e(island_link_id)
        .first()
        .map(|island_link| island_link.source().processor_id().clone())
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
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
                && !runs_inside_the_read_of_each_node_it_feeds(graph, link.to_port().processor_id())
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
    let function_node_ids: Vec<ProcessorUniqueId> =
        the_function_nodes_an_island_link_evaluates(graph, island_link_id)
            .into_iter()
            .collect();
    for link_into_the_island in
        the_links_into_function_nodes_from_outside_any_island(graph, &function_node_ids)
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
                    .contains(link.to_port().processor_id());
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

/// The links into the function nodes `function_node_ids` from nodes outside
/// any island.
fn the_links_into_function_nodes_from_outside_any_island(
    graph: &Graph,
    function_node_ids: &[ProcessorUniqueId],
) -> Vec<LinkUniqueId> {
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            function_node_ids.contains(link.to_port().processor_id())
                && !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
        })
        .map(|link| link.id.clone())
        .collect()
}

/// The function nodes upstream of `node_id` through function nodes, the
/// island its inputs evaluate.
fn the_function_nodes_upstream_of(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> Vec<ProcessorUniqueId> {
    let mut function_nodes_upstream = Vec::new();
    let mut nodes_to_follow = vec![node_id.clone()];
    let mut followed = HashSet::new();
    while let Some(following_id) = nodes_to_follow.pop() {
        if !followed.insert(following_id.clone()) {
            continue;
        }
        for link in graph.traversal().e(()).iter() {
            if link.to_port().processor_id() != &following_id {
                continue;
            }
            let upstream_id = link.source().processor_id();
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
    let reader_id = link_in(graph, link_id)?.to_port().processor_id().clone();
    let reader_link_wiring = graph
        .traversal()
        .v(&reader_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .map(|component| std::sync::Arc::clone(&component.0))
        .ok_or_else(|| {
            Error::InvalidLink(format!(
                "link `{link_id}` feeds a function node island into `{reader_id}`, which runs in \
                 the app process; a function node runs inside the read of a node in a helper \
                 process, and the app process runs no node code"
            ))
        })?;
    if reader_link_wiring.far_side_process_execution() == ProcessExecution::Reactive
        && !is_fed_by_a_node_that_is_not_a_function_node(graph, &reader_id)
    {
        return Err(Error::InvalidLink(format!(
            "`{reader_id}` runs reactively and only function node islands feed it, and an \
             island has no source to wake it, so it would never run. To fix: declare it \
             `execution=\"continuous\"` with an `interval_ms`, or link it a source"
        )));
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
        .v(link.to_port().processor_id())
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(link.to_port().processor_id().to_string()))?;
    let upstream_stream_facts_by_inbound_link = reader.upstream_stream_facts_by_inbound_link();
    upstream_stream_facts_by_inbound_link.forget_inbound_link(link_id);
    let island_output_node = graph
        .traversal()
        .v(link.source().processor_id())
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(link.source().processor_id().to_string()))?;
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
    for link_id in island_links {
        let reader_id = link_in(graph, &link_id)?.to_port().processor_id().clone();
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
    let hosts: Vec<(ProcessorUniqueId, FunctionNodeHostComponent)> = graph
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

fn the_function_node_ids_an_island_calls(island: &JsonValue) -> Vec<ProcessorUniqueId> {
    island
        .get("nodes")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|node| node.get("node").and_then(JsonValue::as_str))
        .map(ProcessorUniqueId::from)
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
/// `graph` renders on the node as `ran_compiled`; attached on its first reader
/// or its promotion, whichever comes first.
pub(crate) fn the_report_of_how_its_helpers_ran_a_function_node(
    graph: &mut Graph,
    function_node_id: &ProcessorUniqueId,
) -> Option<std::sync::Arc<HowItsHelpersRanAFunctionNode>> {
    let node = graph.traversal_mut().v(function_node_id).first_mut()?;
    if let Some(component) = node.get::<FunctionNodeRanCompiledComponent>() {
        return Some(std::sync::Arc::clone(&component.0));
    }
    let how_it_ran = std::sync::Arc::new(HowItsHelpersRanAFunctionNode::default());
    node.insert(FunctionNodeRanCompiledComponent(std::sync::Arc::clone(
        &how_it_ran,
    )));
    Some(how_it_ran)
}

/// Whether a node outside any island feeds `node_id`, directly or through an
/// island it evaluates, whose messages wake it.
fn is_fed_by_a_node_that_is_not_a_function_node(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> bool {
    let function_nodes_upstream = the_function_nodes_upstream_of(graph, node_id);
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            link.to_port().processor_id() == node_id
                || function_nodes_upstream.contains(link.to_port().processor_id())
        })
        .any(|link| {
            !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
        })
}

/// Unwire `link_id`, from a function node into its reader: close each read
/// its reader holds for it, forget the island it carries on the reader's setup
/// command and mark it disconnected.
pub(crate) fn unwire_a_link_from_a_function_node_island_into_its_reader(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let reader_id = link_in(graph, link_id)?.to_port().processor_id().clone();
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

/// The island upstream of `link_id` as its reader's setup command carries it:
/// the reader's input port, the island output feeding it, and every function
/// node it calls, each after the nodes it reads.
///
/// Wire contract with the helper bootstrap: `{"input_port", "link_id",
/// "output": {"node", "port"}, "nodes": [{"node", "name", "import_path",
/// "config", "pins": {input: value}, "links_into": [{"port", "from": {"node",
/// "port"} or {"reader_input_port"}}]}]}`, a stateful node's `"node_uuid"` and `"state_fields"`
/// beside its `"node"`, and
/// the `"exposed_config_fields"` of a node that takes config fields as inputs,
/// each a port a link of `"links_into"` may name.
pub(crate) fn the_island_feeding_a_link(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<JsonValue> {
    let link = link_in(graph, link_id)?;
    let island_output_node_id = link.source().processor_id().clone();
    let links_read_from_the_last_evaluation = the_links_read_from_the_last_evaluation(graph);
    let mut nodes_each_after_what_it_reads = Vec::new();
    let mut placed = HashSet::new();
    let mut still_to_place = vec![island_output_node_id.clone()];
    while let Some(node_id) = still_to_place.pop() {
        place_a_function_node_after_what_it_reads(
            graph,
            link_id,
            &node_id,
            &links_read_from_the_last_evaluation,
            &mut placed,
            &mut nodes_each_after_what_it_reads,
            &mut still_to_place,
        )?;
    }
    Ok(serde_json::json!({
        "input_port": link.to_port().port_name(),
        "link_id": link_id.to_string(),
        "output": {
            "node": island_output_node_id.to_string(),
            "port": link.source().port_name(),
        },
        "nodes": nodes_each_after_what_it_reads,
    }))
}

/// Place `node_id` in the island after every node it reads at this
/// evaluation. A node it reads only from the last evaluation is left on
/// `still_to_place`, to come after it.
fn place_a_function_node_after_what_it_reads(
    graph: &Graph,
    island_link_id: &LinkUniqueId,
    node_id: &ProcessorUniqueId,
    links_read_from_the_last_evaluation: &HashSet<LinkUniqueId>,
    placed: &mut HashSet<ProcessorUniqueId>,
    nodes_each_after_what_it_reads: &mut Vec<JsonValue>,
    still_to_place: &mut Vec<ProcessorUniqueId>,
) -> Result<()> {
    if placed.contains(node_id) {
        return Ok(());
    }
    let node = graph
        .traversal()
        .v(node_id)
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(node_id.to_string()))?;
    let import_path = node.processor_type.to_string();
    let node_name = node.display_name.clone();
    let config = node.config.clone().unwrap_or(JsonValue::Null);
    let pins = JsonValue::Object(node.pins.clone());

    let links_into_this_node: Vec<&Link> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| link.to_port().processor_id() == node_id)
        .collect();
    let mut links_into: Vec<JsonValue> = Vec::with_capacity(links_into_this_node.len());
    for link_into in links_into_this_node {
        let upstream_id = link_into.source().processor_id();
        if !runs_inside_the_read_of_each_node_it_feeds(graph, upstream_id) {
            links_into.push(serde_json::json!({
                "port": link_into.to_port().port_name(),
                "from": {
                    "reader_input_port":
                        the_reader_input_port_a_link_into_a_function_node_is_read_at(
                            &link_into.id,
                            island_link_id,
                        ),
                },
            }));
            continue;
        }
        if links_read_from_the_last_evaluation.contains(&link_into.id) {
            still_to_place.push(upstream_id.clone());
        } else {
            place_a_function_node_after_what_it_reads(
                graph,
                island_link_id,
                upstream_id,
                links_read_from_the_last_evaluation,
                placed,
                nodes_each_after_what_it_reads,
                still_to_place,
            )?;
        }
        links_into.push(serde_json::json!({
            "port": link_into.to_port().port_name(),
            "from": {
                "node": upstream_id.to_string(),
                "port": link_into.source().port_name(),
            },
        }));
    }
    placed.insert(node_id.clone());
    let mut island_node = serde_json::json!({
        "node": node_id.to_string(),
        "name": node_name,
        "import_path": import_path,
        "config": config,
        "pins": pins,
        "links_into": links_into,
    });
    let state_fields = the_state_fields_of_a_function_node(graph, &node.processor_type);
    if !state_fields.is_empty() {
        island_node["node_uuid"] = serde_json::json!(node.node_uuid);
        island_node["state_fields"] = serde_json::json!(state_fields);
    }
    if !node.exposed_config_fields.is_empty() {
        island_node["exposed_config_fields"] = serde_json::json!(node.exposed_config_fields);
    }
    nodes_each_after_what_it_reads.push(island_node);
    Ok(())
}

/// The state record fields a function node of `node_type` carries; none for a
/// stateless one.
pub(crate) fn the_state_fields_of_a_function_node(
    graph: &Graph,
    node_type: &crate::core::descriptors::ProcessorClassImportPath,
) -> Vec<String> {
    graph
        .node_types_this_stream_resolves()
        .descriptor(node_type)
        .map(|descriptor| descriptor.function_node_state_fields)
        .unwrap_or_default()
}

/// Refuse, before a commit changes anything, an edit an island cannot take: a
/// second link into a port an island feeds, an island into a port declaring an
/// audio window, a removal that leaves a running reactive reader fed only by
/// islands, and a live config update to a function node.
pub(crate) fn refuse_an_edit_an_island_cannot_take(
    graph: &Graph,
    processors_to_add: &[ProcessorUniqueId],
    processors_to_remove: &[ProcessorUniqueId],
    links_to_add: &[LinkUniqueId],
    links_to_remove: &[LinkUniqueId],
    config_updates: &[(ProcessorUniqueId, JsonValue)],
) -> Result<()> {
    let is_a_running_reader = |node_id: &ProcessorUniqueId| {
        !runs_inside_the_read_of_each_node_it_feeds(graph, node_id)
            && !processors_to_add.contains(node_id)
            && !processors_to_remove.contains(node_id)
    };
    for link_id in links_to_add {
        let link = link_in(graph, link_id)?;
        refuse_a_second_link_into_a_port_an_island_feeds(graph, link)?;
        refuse_an_island_into_a_port_declaring_an_audio_window(graph, link)?;
    }
    for link_id in links_to_remove {
        let link = link_in(graph, link_id)?;
        if runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id()) {
            continue;
        }
        let destination_id = link.to_port().processor_id();
        let readers_it_wakes = if runs_inside_the_read_of_each_node_it_feeds(graph, destination_id)
        {
            the_readers_a_function_node_feeds(graph, destination_id)
        } else {
            HashSet::from([destination_id.clone()])
        };
        for reader_id in &readers_it_wakes {
            if is_a_running_reader(reader_id)
                && is_reactive_and_left_fed_only_by_islands(graph, reader_id, links_to_remove)
            {
                return Err(Error::InvalidLink(format!(
                    "removing `{link_id}` leaves `{reader_id}`, which runs reactively, fed only \
                     by function node islands, which have no source to wake it, so it would \
                     never run again. To fix: link it another source first, or remove it"
                )));
            }
        }
    }
    for (updated_id, _) in config_updates {
        if runs_inside_the_read_of_each_node_it_feeds(graph, updated_id) {
            return Err(Error::Config(format!(
                "function node `{updated_id}`'s config goes to each reader of its island with \
                 the island, and a live config update is not carried to an island, so it is \
                 refused. To fix: stop the stream and start it with the new config"
            )));
        }
    }
    Ok(())
}

/// Refuse `link` when it and another link both feed one input port and either
/// comes from a function node: an island answers each read of its port, so it
/// shares the port with no other producer.
fn refuse_a_second_link_into_a_port_an_island_feeds(graph: &Graph, link: &Link) -> Result<()> {
    let links_into_the_port: Vec<&Link> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|other_link| {
            other_link.to_port().processor_id() == link.to_port().processor_id()
                && other_link.to_port().port_name() == link.to_port().port_name()
        })
        .collect();
    let an_island_feeds_the_port = links_into_the_port.iter().any(|link_into| {
        runs_inside_the_read_of_each_node_it_feeds(graph, link_into.source().processor_id())
    });
    if links_into_the_port.len() > 1 && an_island_feeds_the_port {
        return Err(Error::InvalidLink(format!(
            "`{}`'s input `{}` is fed by {} links, and one comes from a function node; an \
             island answers each read of its port, so it shares the port with no other link. \
             To fix: give each its own input port",
            link.to_port().processor_id(),
            link.to_port().port_name(),
            links_into_the_port.len()
        )));
    }
    Ok(())
}

/// Refuse `link` from a function node into an input port declaring an
/// `audio_window`: an island answers one control value per read, never a
/// window of audio samples.
fn refuse_an_island_into_a_port_declaring_an_audio_window(
    graph: &Graph,
    link: &Link,
) -> Result<()> {
    if !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id()) {
        return Ok(());
    }
    let reader_id = link.to_port().processor_id();
    let port_name = link.to_port().port_name();
    let declares_an_audio_window = graph
        .traversal()
        .v(reader_id)
        .first()
        .is_some_and(|reader| {
            reader
                .ports
                .inputs
                .iter()
                .any(|input| input.name == port_name && input.audio_window.is_some())
        });
    if declares_an_audio_window {
        return Err(Error::InvalidLink(format!(
            "`{reader_id}`'s input `{port_name}` declares an `audio_window`, and a function \
             node island answers one control value per read, never a window of samples. To \
             fix: link the island into a port with no `audio_window`"
        )));
    }
    Ok(())
}

/// The nodes outside any island that `function_node_id`'s outputs reach,
/// through the function nodes downstream of it.
fn the_readers_a_function_node_feeds(
    graph: &Graph,
    function_node_id: &ProcessorUniqueId,
) -> HashSet<ProcessorUniqueId> {
    the_reader_input_ports_a_function_node_feeds(graph, function_node_id)
        .into_iter()
        .map(|(reader_id, _)| reader_id)
        .collect()
}

/// Each input port, of a node outside any island, that `function_node_id`
/// reaches through the function nodes downstream of it.
pub(crate) fn the_reader_input_ports_a_function_node_feeds(
    graph: &Graph,
    function_node_id: &ProcessorUniqueId,
) -> HashSet<(ProcessorUniqueId, String)> {
    let mut reader_input_ports = HashSet::new();
    let mut function_nodes_to_follow = vec![function_node_id.clone()];
    let mut function_nodes_followed = HashSet::new();
    while let Some(following_id) = function_nodes_to_follow.pop() {
        if !function_nodes_followed.insert(following_id.clone()) {
            continue;
        }
        for link in graph.traversal().e(()).iter() {
            if link.source().processor_id() != &following_id {
                continue;
            }
            let destination_id = link.to_port().processor_id();
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

fn is_reactive_and_left_fed_only_by_islands(
    graph: &Graph,
    reader_id: &ProcessorUniqueId,
    links_to_remove: &[LinkUniqueId],
) -> bool {
    let runs_reactively = graph
        .traversal()
        .v(reader_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .is_some_and(|component| {
            component.0.far_side_process_execution() == ProcessExecution::Reactive
        });
    let links_left_into_the_reader: Vec<&Link> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            link.to_port().processor_id() == reader_id
                && !links_to_remove
                    .iter()
                    .any(|removed_link_id| removed_link_id.as_str() == link.id())
        })
        .collect();
    let island = the_function_nodes_upstream_of(graph, reader_id);
    let the_island_reads_a_link_left_from_outside_any_island =
        the_links_into_function_nodes_from_outside_any_island(graph, &island)
            .iter()
            .any(|link_id| !links_to_remove.contains(link_id));
    runs_reactively
        && !the_island_reads_a_link_left_from_outside_any_island
        && links_left_into_the_reader.iter().any(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
        })
        && links_left_into_the_reader.iter().all(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
        })
}

/// The function nodes among `node_ids`, which are never prepared or spawned.
pub(crate) fn the_function_nodes_among<'a>(
    graph: &Graph,
    node_ids: impl IntoIterator<Item = &'a ProcessorUniqueId>,
) -> HashSet<ProcessorUniqueId> {
    node_ids
        .into_iter()
        .filter(|node_id| runs_inside_the_read_of_each_node_it_feeds(graph, node_id))
        .cloned()
        .collect()
}

fn link_in<'graph>(graph: &'graph Graph, link_id: &LinkUniqueId) -> Result<&'graph Link> {
    graph
        .traversal()
        .e(link_id)
        .first()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::descriptors::{
        FunctionNodeLanguage, PortDescriptor, PortFamily, PortFamilyDeclaration,
        ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
    };
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::processors::{
        HowANodeOfADescribedTypeRuns, NodeTypesOneStreamResolves, OutOfProcessLinkWiringEnvelope,
        ProcessorSpec,
    };

    pub(crate) fn a_function_node_type(
        import_path: &str,
        input_ports: &[&str],
        output_ports: &[&str],
    ) -> ProcessorDescriptor {
        let import_path = ProcessorClassImportPath::new(import_path).unwrap();
        let short_name = import_path.as_str().rsplit(':').next().unwrap().to_string();
        let mut descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new(short_name).unwrap(),
            import_path,
            "a function node type for a test",
        );
        descriptor.node_kind = NodeKind::Function {
            language: FunctionNodeLanguage::Python,
        };
        for input_port in input_ports {
            descriptor = descriptor.with_input(
                PortDescriptor::new(
                    *input_port,
                    "",
                    true,
                    PortFamilyDeclaration::of_family(PortFamily::Channels),
                )
                .with_delivery_profile("newest"),
            );
        }
        for output_port in output_ports {
            descriptor = descriptor.with_output(PortDescriptor::new(
                *output_port,
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            ));
        }
        descriptor
    }

    /// A graph resolving `lfo`, `remap` and `mix`, three function node types.
    pub(crate) fn a_graph_resolving_three_function_node_types() -> Graph {
        a_graph_resolving_these_function_node_types([
            a_function_node_type("test_ops:lfo", &[], &["result"]),
            a_function_node_type("test_ops:remap", &["x"], &["result"]),
            a_function_node_type("test_ops:mix", &["a", "b"], &["result"]),
        ])
    }

    /// A graph resolving each of `function_node_types`.
    pub(crate) fn a_graph_resolving_these_function_node_types(
        function_node_types: impl IntoIterator<Item = ProcessorDescriptor>,
    ) -> Graph {
        let node_types = NodeTypesOneStreamResolves::new();
        for descriptor in function_node_types {
            node_types
                .register_a_type_described_in_this_streams_processor_interpreter(
                    descriptor,
                    HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                        processor_once_promoted_to_its_own_process: Box::new(|node| {
                            Err(Error::Configuration(format!(
                                "`{}` builds no processor in this test",
                                node.id
                            )))
                        }),
                    },
                )
                .unwrap();
        }
        crate::core::test_support::ensure_test_mocks_registered();
        Graph::new_resolving_node_types_through(Arc::new(node_types))
    }

    pub(crate) fn add_a_node(
        graph: &mut Graph,
        import_path: &str,
        config: JsonValue,
    ) -> ProcessorUniqueId {
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                ProcessorClassImportPath::new(import_path).unwrap(),
                config,
            ))
            .unwrap()
            .first()
            .unwrap()
            .id
            .clone()
    }

    pub(crate) fn link(
        graph: &mut Graph,
        from: (&ProcessorUniqueId, &str),
        to: (&ProcessorUniqueId, &str),
    ) -> LinkUniqueId {
        graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(from.0, from.1),
                InputLinkPortRef::new(to.0, to.1),
            )
            .first()
            .unwrap()
            .id
            .clone()
    }

    pub(crate) fn a_helper_reader_before_its_setup(graph: &mut Graph) -> ProcessorUniqueId {
        a_helper_reader_before_its_setup_driven_in(
            graph,
            ProcessExecution::Continuous { interval_ms: 10 },
        )
    }

    pub(crate) fn a_helper_reader_before_its_setup_driven_in(
        graph: &mut Graph,
        far_side_process_execution: ProcessExecution,
    ) -> ProcessorUniqueId {
        let reader_id = add_a_node(
            graph,
            crate::core::test_support::MockInputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        graph
            .traversal_mut()
            .v(&reader_id)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
                OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    far_side_process_execution,
                ),
            )));
        reader_id
    }

    pub(crate) fn reader_setup_command_ports(
        graph: &Graph,
        reader_id: &ProcessorUniqueId,
    ) -> JsonValue {
        graph
            .traversal()
            .v(reader_id)
            .first()
            .unwrap()
            .get::<OutOfProcessLinkWiringComponent>()
            .unwrap()
            .0
            .as_setup_command_ports()
    }

    #[test]
    fn each_link_is_wired_by_the_kinds_of_its_two_ends() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let source = add_a_node(
            &mut graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        let call = link(&mut graph, (&lfo, "result"), (&remap, "x"));
        let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
        assert_eq!(
            how_a_link_is_wired(&graph, &call).unwrap(),
            HowALinkIsWired::AsACallInsideAFunctionNodeIsland
        );
        assert_eq!(
            how_a_link_is_wired(&graph, &into_the_reader).unwrap(),
            HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead
        );
        let into_a_function_node_from_a_class_node =
            link(&mut graph, (&source, "out1"), (&remap, "x"));
        assert_eq!(
            how_a_link_is_wired(&graph, &into_a_function_node_from_a_class_node).unwrap(),
            HowALinkIsWired::ThroughAChannelReadByTheReaderOfItsFunctionNodeIsland
        );
    }

    #[test]
    fn the_island_lists_each_node_after_what_it_reads_once_with_its_config() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(
            &mut graph,
            "test_ops:lfo",
            serde_json::json!({"rate_hz": 2.0}),
        );
        let low = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let high = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&lfo, "result"), (&low, "x"));
        link(&mut graph, (&lfo, "result"), (&high, "x"));
        link(&mut graph, (&low, "result"), (&mix, "a"));
        link(&mut graph, (&high, "result"), (&mix, "b"));
        let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

        let island = the_island_feeding_a_link(&graph, &into_the_reader).unwrap();
        assert_eq!(island["input_port"], "in1");
        assert_eq!(island["output"]["node"], mix.to_string());
        let nodes_in_order: Vec<&str> = island["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["node"].as_str().unwrap())
            .collect();
        assert_eq!(nodes_in_order.len(), 4, "{nodes_in_order:?}");
        let position = |node_id: &ProcessorUniqueId| {
            nodes_in_order
                .iter()
                .position(|listed| *listed == node_id.as_str())
                .unwrap()
        };
        assert!(position(&lfo) < position(&low) && position(&lfo) < position(&high));
        assert!(position(&low) < position(&mix) && position(&high) < position(&mix));
        assert_eq!(island["nodes"][0]["config"]["rate_hz"], 2.0);
        assert_eq!(island["nodes"][0]["import_path"], "test_ops:lfo");
    }

    #[test]
    fn a_link_into_a_helper_carries_its_island_on_the_setup_command_and_is_wired() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));

        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();

        let islands = reader_setup_command_ports(&graph, &reader)["islands"].clone();
        assert_eq!(islands.as_array().unwrap().len(), 1);
        assert_eq!(islands[0]["link_id"], into_the_reader.to_string());
        assert!(matches!(
            graph
                .traversal()
                .e(&into_the_reader)
                .first()
                .unwrap()
                .get::<LinkStateComponent>()
                .map(|state| state.0),
            Some(LinkState::Wired)
        ));

        unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
            .unwrap();
        assert_eq!(
            reader_setup_command_ports(&graph, &reader)["islands"],
            serde_json::json!([])
        );
    }

    /// `graph` renders `ran_compiled` on each node an island carries: `null`
    /// once its reader is wired, then what the reader's helper reported.
    #[test]
    fn each_island_node_renders_how_its_readers_helper_ran_it() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&lfo, "result"), (&remap, "x"));
        let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();
        let ran_compiled = |graph: &Graph, node_id: &ProcessorUniqueId| {
            graph
                .traversal()
                .v(node_id)
                .first()
                .unwrap()
                .serialize_components()
                .get("ran_compiled")
                .cloned()
        };
        assert_eq!(ran_compiled(&graph, &lfo), Some(JsonValue::Null));
        assert_eq!(ran_compiled(&graph, &remap), Some(JsonValue::Null));

        graph
            .traversal()
            .v(&reader)
            .first()
            .unwrap()
            .get::<OutOfProcessLinkWiringComponent>()
            .unwrap()
            .0
            .note_how_its_far_side_ran_its_function_nodes(Some(&serde_json::json!({
                lfo.to_string(): true,
                remap.to_string(): false,
            })));
        assert_eq!(ran_compiled(&graph, &lfo), Some(JsonValue::Bool(true)));
        assert_eq!(ran_compiled(&graph, &remap), Some(JsonValue::Bool(false)));
    }

    #[test]
    fn graph_names_each_function_nodes_host_as_the_readers_its_island_feeds() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let first_reader = a_helper_reader_before_its_setup(&mut graph);
        let second_reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&lfo, "result"), (&remap, "x"));
        link(&mut graph, (&remap, "result"), (&first_reader, "in1"));
        link(&mut graph, (&lfo, "result"), (&second_reader, "in1"));

        note_where_each_function_node_runs(&mut graph);

        let host = |node_id: &ProcessorUniqueId| {
            graph
                .traversal()
                .v(node_id)
                .first()
                .unwrap()
                .serialize_components()
                .get("host")
                .cloned()
        };
        let name_of = |node_id: &ProcessorUniqueId| {
            graph
                .traversal()
                .v(node_id)
                .first()
                .unwrap()
                .display_name
                .clone()
        };
        let mut both_readers = vec![name_of(&first_reader), name_of(&second_reader)];
        both_readers.sort();
        assert_ne!(
            both_readers[0], both_readers[1],
            "each reader has its own name"
        );
        assert_eq!(
            host(&lfo),
            Some(serde_json::json!({ "inside_the_read_of": both_readers }))
        );
        assert_eq!(
            host(&remap),
            Some(serde_json::json!({ "inside_the_read_of": [name_of(&first_reader)] }))
        );
        assert_eq!(host(&first_reader), None, "a class node names no host");
    }

    #[test]
    fn a_reactive_reader_fed_only_by_islands_is_refused_naming_continuous_execution() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader =
            a_helper_reader_before_its_setup_driven_in(&mut graph, ProcessExecution::Reactive);
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        let refusal = wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("execution=\"continuous\""), "{refusal}");
    }

    fn the_refusal_of_a_commit(
        graph: &Graph,
        processors_to_add: &[ProcessorUniqueId],
        processors_to_remove: &[ProcessorUniqueId],
        links_to_add: &[LinkUniqueId],
        links_to_remove: &[LinkUniqueId],
        config_updates: &[(ProcessorUniqueId, JsonValue)],
    ) -> Option<String> {
        refuse_an_edit_an_island_cannot_take(
            graph,
            processors_to_add,
            processors_to_remove,
            links_to_add,
            links_to_remove,
            config_updates,
        )
        .err()
        .map(|refusal| refusal.to_string())
    }

    /// A reader whose helper is past its setup command, handing every later
    /// link and island to `delivery`.
    fn a_helper_reader_past_its_setup(
        graph: &mut Graph,
        delivery: &crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery,
    ) -> ProcessorUniqueId {
        let reader = a_helper_reader_before_its_setup(graph);
        the_link_wiring_of(graph, &reader)
            .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
            .unwrap();
        reader
    }

    fn the_link_wiring_of(
        graph: &Graph,
        node_id: &ProcessorUniqueId,
    ) -> Arc<OutOfProcessLinkWiringEnvelope> {
        Arc::clone(
            &graph
                .traversal()
                .v(node_id)
                .first()
                .unwrap()
                .get::<OutOfProcessLinkWiringComponent>()
                .unwrap()
                .0,
        )
    }

    fn the_node_ids_of_an_island(island: &JsonValue) -> Vec<String> {
        island["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["node"].as_str().unwrap().to_string())
            .collect()
    }

    fn link_state_of(graph: &Graph, link_id: &LinkUniqueId) -> LinkState {
        graph
            .traversal()
            .e(link_id)
            .first()
            .unwrap()
            .get::<LinkStateComponent>()
            .unwrap()
            .0
    }

    #[test]
    fn a_function_node_linked_live_into_a_running_reader_is_handed_over_as_its_island() {
        let delivery =
            crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_past_its_setup(&mut graph, &delivery);
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));

        assert!(
            the_refusal_of_a_commit(
                &graph,
                std::slice::from_ref(&lfo),
                &[],
                std::slice::from_ref(&into_the_reader),
                &[],
                &[]
            )
            .is_none(),
            "a running reader takes a live link from a function node"
        );
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();

        let handed_over = delivery.late_fed_function_node_islands.lock().clone();
        assert_eq!(handed_over.len(), 1);
        assert_eq!(handed_over[0]["input_port"], "in1");
        assert_eq!(handed_over[0]["link_id"], into_the_reader.as_str());
        assert_eq!(
            the_node_ids_of_an_island(&handed_over[0]),
            vec![lfo.to_string()]
        );
        assert!(
            handed_over[0]["nodes"][0]["breadcrumb_number"].is_u64(),
            "the reader writes each node's breadcrumb while calling it"
        );
        assert_eq!(
            link_state_of(&graph, &into_the_reader),
            LinkState::Pending,
            "pending until the reader says it evaluates the island"
        );
    }

    #[test]
    fn a_pin_edit_on_an_island_node_hands_its_running_reader_the_island_carrying_it() {
        let delivery =
            crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_past_its_setup(&mut graph, &delivery);
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();
        assert_eq!(
            delivery.late_fed_function_node_islands.lock()[0]["nodes"][0]["pins"],
            serde_json::json!({})
        );

        graph
            .traversal_mut()
            .v(&lfo)
            .first_mut()
            .unwrap()
            .pins
            .insert("rate_hz".to_string(), serde_json::json!(4.0));
        hand_each_reader_the_island_a_compile_changed(&mut graph, &[]).unwrap();

        let handed_over = delivery.late_fed_function_node_islands.lock().clone();
        assert_eq!(
            handed_over.len(),
            2,
            "the pin edit re-sends the island once"
        );
        assert_eq!(
            handed_over[1]["nodes"][0]["pins"],
            serde_json::json!({ "rate_hz": 4.0 })
        );
        hand_each_reader_the_island_a_compile_changed(&mut graph, &[]).unwrap();
        assert_eq!(
            delivery.late_fed_function_node_islands.lock().len(),
            2,
            "an island no edit changed is not re-sent"
        );
    }

    #[test]
    fn an_edit_inside_an_island_hands_its_running_reader_the_island_again_and_no_other() {
        let delivery =
            crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let untouched_lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
        let untouched_into_the_reader =
            link(&mut graph, (&untouched_lfo, "result"), (&reader, "in2"));
        for island_link in [&into_the_reader, &untouched_into_the_reader] {
            wire_a_link_from_a_function_node_island_into_its_reader(
                &mut graph,
                island_link,
                &Iceoryx2Node::for_this_test_process(),
            )
            .unwrap();
        }
        the_link_wiring_of(&graph, &reader)
            .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
            .unwrap();

        hand_each_reader_the_island_a_compile_changed(&mut graph, &[]).unwrap();
        assert!(
            delivery.late_fed_function_node_islands.lock().is_empty(),
            "an island no edit changed is not handed over again"
        );

        let into_the_remap = link(&mut graph, (&lfo, "result"), (&remap, "x"));
        wire_a_call_inside_a_function_node_island(&mut graph, &into_the_remap).unwrap();
        hand_each_reader_the_island_a_compile_changed(&mut graph, &[into_the_remap]).unwrap();

        let handed_over = delivery.late_fed_function_node_islands.lock().clone();
        assert_eq!(
            handed_over.len(),
            1,
            "only the island the edit changed: {handed_over:?}"
        );
        assert_eq!(handed_over[0]["link_id"], into_the_reader.as_str());
        assert_eq!(
            the_node_ids_of_an_island(&handed_over[0]),
            vec![lfo.to_string(), remap.to_string()],
            "each node after what it reads"
        );
        assert_eq!(link_state_of(&graph, &into_the_reader), LinkState::Pending);
        assert_eq!(
            link_state_of(&graph, &untouched_into_the_reader),
            LinkState::Wired
        );
    }

    #[test]
    fn unwiring_an_island_link_tells_its_running_reader_to_stop_evaluating_it() {
        let delivery =
            crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_past_its_setup(&mut graph, &delivery);
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();

        assert!(
            the_refusal_of_a_commit(
                &graph,
                &[],
                std::slice::from_ref(&lfo),
                &[],
                std::slice::from_ref(&into_the_reader),
                &[]
            )
            .is_none(),
            "a running reader's island may be removed live"
        );
        unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
            .unwrap();
        assert_eq!(
            delivery.unfed_function_node_islands.lock().clone(),
            vec![("in1".to_string(), into_the_reader.to_string())]
        );
        assert!(
            the_link_wiring_of(&graph, &reader)
                .the_function_node_island_recorded_for_a_link(into_the_reader.as_str())
                .is_none(),
            "a later setup command no longer carries it"
        );
    }

    #[test]
    fn a_link_closing_a_loop_of_function_nodes_is_taken_live() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        link(&mut graph, (&lfo, "result"), (&mix, "a"));
        link(&mut graph, (&mix, "result"), (&remap, "x"));
        let closing_the_loop = link(&mut graph, (&remap, "result"), (&mix, "b"));

        assert_eq!(
            the_refusal_of_a_commit(
                &graph,
                &[],
                &[],
                std::slice::from_ref(&closing_the_loop),
                &[],
                &[]
            ),
            None
        );
    }

    #[test]
    fn the_link_of_a_loop_pointing_back_toward_what_feeds_it_reads_the_last_evaluation() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_loop = link(&mut graph, (&lfo, "result"), (&mix, "a"));
        let around = link(&mut graph, (&mix, "result"), (&remap, "x"));
        let back = link(&mut graph, (&remap, "result"), (&mix, "b"));
        let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));

        assert_eq!(
            the_links_read_from_the_last_evaluation(&graph),
            HashSet::from([back.clone()])
        );

        note_which_links_read_the_last_evaluation(&mut graph);
        let is_marked = |graph: &Graph, link_id: &LinkUniqueId| {
            link_in(graph, link_id)
                .unwrap()
                .serialize_components()
                .get("reads_the_last_evaluation")
                .cloned()
        };
        assert_eq!(is_marked(&graph, &back), Some(JsonValue::Bool(true)));
        for read_at_this_evaluation in [&into_the_loop, &around, &into_the_reader] {
            assert_eq!(is_marked(&graph, read_at_this_evaluation), None);
        }

        graph.traversal_mut().e(&around).drop();
        note_which_links_read_the_last_evaluation(&mut graph);
        assert_eq!(
            is_marked(&graph, &back),
            None,
            "a link that closes no loop any more is unmarked"
        );
    }

    #[test]
    fn an_island_closing_a_loop_evaluates_what_the_link_back_feeds_before_what_it_reads() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&lfo, "result"), (&mix, "a"));
        link(&mut graph, (&mix, "result"), (&remap, "x"));
        link(&mut graph, (&remap, "result"), (&mix, "b"));
        let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

        let island = the_island_feeding_a_link(&graph, &into_the_reader).unwrap();

        let order = the_node_ids_of_an_island(&island);
        let position_of = |node_id: &ProcessorUniqueId| {
            order
                .iter()
                .position(|each| each == node_id.as_str())
                .unwrap()
        };
        assert_eq!(order.len(), 3, "each node once: {order:?}");
        assert!(position_of(&lfo) < position_of(&mix), "{order:?}");
        assert!(position_of(&mix) < position_of(&remap), "{order:?}");
        let links_into_mix = island["nodes"][position_of(&mix)]["links_into"]
            .as_array()
            .unwrap();
        assert!(
            links_into_mix.contains(&serde_json::json!(
                { "port": "b", "from": { "node": remap.to_string(), "port": "result" } }
            )),
            "the node still names what it reads, which the reader takes from the last evaluation"
        );
    }

    #[test]
    fn a_function_node_linked_into_itself_reads_its_own_last_evaluation() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_itself = link(&mut graph, (&mix, "result"), (&mix, "a"));
        let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

        assert_eq!(
            the_links_read_from_the_last_evaluation(&graph),
            HashSet::from([into_itself])
        );
        let island = the_island_feeding_a_link(&graph, &into_the_reader).unwrap();
        assert_eq!(the_node_ids_of_an_island(&island), [mix.to_string()]);
    }

    #[test]
    fn one_graph_names_the_same_link_of_a_loop_however_its_links_were_added() {
        let the_link_back_when_links_are_added = |in_this_order: [usize; 3]| {
            let mut graph = a_graph_resolving_three_function_node_types();
            let node_uuids = [1_u128, 2, 3].map(uuid::Uuid::from_u128);
            let nodes = node_uuids.map(|node_uuid| {
                let node_id = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
                graph
                    .traversal_mut()
                    .v(&node_id)
                    .first_mut()
                    .unwrap()
                    .node_uuid = node_uuid;
                node_id
            });
            let mut link_back = None;
            for source in in_this_order {
                let fed = (source + 1) % 3;
                let link_id = link(&mut graph, (&nodes[source], "result"), (&nodes[fed], "a"));
                if source == 2 {
                    link_back = Some(link_id);
                }
            }
            (
                the_links_read_from_the_last_evaluation(&graph),
                link_back.unwrap(),
            )
        };

        for in_this_order in [[0, 1, 2], [2, 1, 0], [1, 2, 0]] {
            let (read_from_the_last_evaluation, link_back) =
                the_link_back_when_links_are_added(in_this_order);
            assert_eq!(
                read_from_the_last_evaluation,
                HashSet::from([link_back]),
                "a loop no node feeds is walked from its lowest node id: {in_this_order:?}"
            );
        }
    }

    #[test]
    fn a_live_config_update_to_a_function_node_is_refused_by_name() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let refusal = the_refusal_of_a_commit(
            &graph,
            &[],
            &[],
            &[],
            &[],
            &[(lfo, serde_json::json!({"rate_hz": 3.0}))],
        )
        .expect("refused");
        assert!(
            refusal.contains("a live config update is not carried to an island"),
            "{refusal}"
        );
    }

    #[test]
    fn a_second_link_into_a_port_an_island_feeds_is_refused_by_name() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let first_lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let second_lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let first = link(&mut graph, (&first_lfo, "result"), (&reader, "in1"));
        let second = link(&mut graph, (&second_lfo, "result"), (&reader, "in1"));
        let refusal = the_refusal_of_a_commit(
            &graph,
            &[first_lfo, second_lfo, reader],
            &[],
            &[first, second],
            &[],
            &[],
        )
        .expect("refused");
        assert!(
            refusal.contains("shares the port with no other link"),
            "{refusal}"
        );
    }

    #[test]
    fn an_island_into_a_port_declaring_an_audio_window_is_refused_by_name() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let windowed_consumer = add_a_node(
            &mut graph,
            crate::core::test_support::MockWindowedAudioConsumerProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        let into_the_window = link(&mut graph, (&lfo, "result"), (&windowed_consumer, "audio"));
        let refusal = the_refusal_of_a_commit(
            &graph,
            &[lfo, windowed_consumer],
            &[],
            &[into_the_window],
            &[],
            &[],
        )
        .expect("refused");
        assert!(refusal.contains("declares an `audio_window`"), "{refusal}");
    }

    #[test]
    fn removing_the_last_source_of_a_reactive_reader_an_island_feeds_is_refused() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let source = add_a_node(
            &mut graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        let reader =
            a_helper_reader_before_its_setup_driven_in(&mut graph, ProcessExecution::Reactive);
        link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        let from_the_source = link(&mut graph, (&source, "out2"), (&reader, "in2"));
        let refusal = the_refusal_of_a_commit(&graph, &[], &[], &[], &[from_the_source], &[])
            .expect("refused");
        assert!(refusal.contains("would never run again"), "{refusal}");
    }

    #[test]
    fn an_island_into_a_node_in_the_app_process_is_refused_by_name() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let native_reader = add_a_node(
            &mut graph,
            crate::core::test_support::MockInputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&native_reader, "in1"));
        let refusal = wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("runs in the app process"), "{refusal}");
    }

    #[test]
    fn an_island_wired_while_its_readers_setup_command_is_being_built_rides_that_command() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        the_link_wiring_of(&graph, &reader)
            .hold_every_later_link_until_the_setup_command_goes_out();
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();
        assert_eq!(link_state_of(&graph, &into_the_reader), LinkState::Wired);
        let setup_command_islands =
            the_link_wiring_of(&graph, &reader).as_setup_command_ports()["islands"].clone();
        assert_eq!(
            setup_command_islands[0]["link_id"],
            into_the_reader.as_str()
        );
    }

    /// A class node in a helper of its own, whose `out1` feeds `remap`'s `x`,
    /// and `remap` feeding a helper reader's `in1`.
    pub(crate) fn a_class_node_feeding_remap_into_a_helper_reader(
        graph: &mut Graph,
    ) -> (
        ProcessorUniqueId,
        ProcessorUniqueId,
        LinkUniqueId,
        LinkUniqueId,
    ) {
        let source = add_a_node(
            graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        graph
            .traversal_mut()
            .v(&source)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
                OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    ProcessExecution::Reactive,
                ),
            )));
        let remap = add_a_node(graph, "test_ops:remap", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(graph);
        let into_the_island = link(graph, (&source, "out1"), (&remap, "x"));
        let into_the_reader = link(graph, (&remap, "result"), (&reader, "in1"));
        (remap, reader, into_the_island, into_the_reader)
    }

    /// The inbound link a reader's input port is asked about by `inbound_link`
    /// answers when one is recorded there by that name, and is refused when
    /// none is.
    fn the_reader_input_port_records_an_inbound_link_named(
        graph: &Graph,
        reader: &ProcessorUniqueId,
        input_port: &str,
        inbound_link: &str,
    ) -> bool {
        graph
            .traversal()
            .v(reader)
            .first()
            .unwrap()
            .upstream_stream_facts_by_inbound_link()
            .stream_facts_of_inbound_link(input_port, Some(inbound_link))
            .is_ok()
    }

    #[test]
    fn a_reader_reaches_the_stream_facts_of_the_class_node_an_island_hands_on_unread() {
        let mut gate = a_function_node_type("test_ops:gate", &["value"], &["result"]);
        gate.outputs[0].family.family_follows_input_port = Some("value".into());
        let mut graph = a_graph_resolving_these_function_node_types([
            gate,
            a_function_node_type("test_ops:remap", &["x"], &["result"]),
        ]);
        let source = add_a_node(
            &mut graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        graph
            .traversal_mut()
            .v(&source)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
                OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    ProcessExecution::Reactive,
                ),
            )));
        let gate = add_a_node(&mut graph, "test_ops:gate", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&source, "out1"), (&gate, "value"));
        let into_the_reader = link(&mut graph, (&gate, "result"), (&reader, "in1"));
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();
        let channel_of_the_class_node =
            crate::iceoryx2::source_channel_name(source.as_str(), "out1")
                .unwrap()
                .into_string();

        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &iceoryx2_node,
        )
        .unwrap();
        assert!(
            the_reader_input_port_records_an_inbound_link_named(
                &graph,
                &reader,
                "in1",
                &channel_of_the_class_node
            ),
            "the reader's input port reads the class node's output through the gate"
        );

        unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
            .unwrap();
        assert!(
            !the_reader_input_port_records_an_inbound_link_named(
                &graph,
                &reader,
                "in1",
                &channel_of_the_class_node
            ),
            "an unwired island link leaves no inbound link behind"
        );
    }

    #[test]
    fn an_island_handing_out_values_of_its_own_records_no_upstream_output_at_its_reader() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let (_remap, reader, into_the_island, into_the_reader) =
            a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
        let source = link_in(&graph, &into_the_island)
            .unwrap()
            .source()
            .processor_id()
            .clone();

        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();
        assert!(!the_reader_input_port_records_an_inbound_link_named(
            &graph,
            &reader,
            "in1",
            &crate::iceoryx2::source_channel_name(source.as_str(), "out1")
                .unwrap()
                .into_string()
        ));
    }

    #[test]
    fn a_class_node_linked_into_an_island_is_read_at_a_hidden_input_of_its_reader() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let (remap, reader, into_the_island, into_the_reader) =
            a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();

        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &iceoryx2_node,
        )
        .unwrap();

        let reads = the_reads_of(&graph, &into_the_island);
        assert_eq!(
            reads.len(),
            1,
            "the link into the island is read at its reader once"
        );
        let read_at = reads[0].clone();
        assert_eq!(read_at.processor_id(), &reader);
        assert_eq!(
            read_at.port_name(),
            the_reader_input_port_a_link_into_a_function_node_is_read_at(
                &into_the_island,
                &into_the_reader
            )
        );
        let island = &reader_setup_command_ports(&graph, &reader)["islands"][0];
        let remap_in_the_island = island["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node"] == remap.as_str())
            .unwrap();
        assert_eq!(
            remap_in_the_island["links_into"],
            serde_json::json!([{
                "port": "x",
                "from": {"reader_input_port": read_at.port_name()},
            }])
        );
        let source = link_in(&graph, &into_the_island)
            .unwrap()
            .source()
            .processor_id()
            .clone();
        let ports_of_both_ends_wired_once = [
            reader_setup_command_ports(&graph, &source),
            reader_setup_command_ports(&graph, &reader),
        ];

        wire_a_link_into_a_function_node_island_through_its_reader(
            &mut graph,
            &into_the_island,
            &iceoryx2_node,
        )
        .unwrap();
        assert_eq!(
            link_state_of(&graph, &into_the_island),
            LinkState::Wired,
            "a link its reader already opened is wired once"
        );
        assert_eq!(
            [
                reader_setup_command_ports(&graph, &source),
                reader_setup_command_ports(&graph, &reader),
            ],
            ports_of_both_ends_wired_once,
            "wiring it again adds no entry at either end"
        );
    }

    /// The reader input ports `link_id` is read at, in island link order.
    fn the_reads_of(graph: &Graph, link_id: &LinkUniqueId) -> Vec<InputLinkPortRef> {
        graph
            .traversal()
            .e(link_id)
            .first()
            .unwrap()
            .get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
            .map(|read_through| {
                read_through
                    .reads_by_island_link
                    .iter()
                    .map(|read| read.reader_input_port.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `reader_input_port` the island `island_link_id` carries names for
    /// `function_node_id`'s input `port`.
    fn the_reader_input_port_an_island_names(
        graph: &Graph,
        island_link_id: &LinkUniqueId,
        function_node_id: &ProcessorUniqueId,
    ) -> JsonValue {
        let island = the_island_feeding_a_link(graph, island_link_id).unwrap();
        island["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node"] == function_node_id.as_str())
            .unwrap()["links_into"][0]["from"]["reader_input_port"]
            .clone()
    }

    #[test]
    fn an_island_reading_a_class_node_that_feeds_two_readers_is_read_at_each_of_them() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let (remap, reader, into_the_island, into_the_reader) =
            a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
        let second_reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_second_reader = link(&mut graph, (&remap, "result"), (&second_reader, "in1"));
        assert_eq!(
            the_refusal_of_a_commit(
                &graph,
                &[],
                &[],
                std::slice::from_ref(&into_the_second_reader),
                &[],
                &[],
            ),
            None
        );
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();

        for island_link in [&into_the_reader, &into_the_second_reader] {
            wire_a_link_from_a_function_node_island_into_its_reader(
                &mut graph,
                island_link,
                &iceoryx2_node,
            )
            .unwrap();
        }

        let reads = the_reads_of(&graph, &into_the_island);
        assert_eq!(
            reads,
            vec![
                InputLinkPortRef::new(
                    reader.clone(),
                    the_reader_input_port_a_link_into_a_function_node_is_read_at(
                        &into_the_island,
                        &into_the_reader
                    )
                ),
                InputLinkPortRef::new(
                    second_reader.clone(),
                    the_reader_input_port_a_link_into_a_function_node_is_read_at(
                        &into_the_island,
                        &into_the_second_reader
                    )
                ),
            ]
        );
        for (island_link, read_at) in [&into_the_reader, &into_the_second_reader]
            .into_iter()
            .zip(&reads)
        {
            assert_eq!(
                the_reader_input_port_an_island_names(&graph, island_link, &remap),
                read_at.port_name(),
                "each island names the input its own reader reads the link at"
            );
        }
        let source = link_in(&graph, &into_the_island)
            .unwrap()
            .source()
            .processor_id()
            .clone();
        let wiring_link_ids_at_the_source: HashSet<String> =
            reader_setup_command_ports(&graph, &source)["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|output_link| output_link["link_id"].as_str().unwrap().to_string())
                .collect();
        assert_eq!(
            wiring_link_ids_at_the_source.len(),
            2,
            "the source notifies each read under its own wiring identity"
        );

        unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
            .unwrap();

        assert_eq!(
            the_reads_of(&graph, &into_the_island),
            vec![reads[1].clone()]
        );
        assert_eq!(link_state_of(&graph, &into_the_island), LinkState::Wired);
    }

    #[test]
    fn an_island_reading_a_class_node_that_feeds_two_input_ports_of_one_reader_is_read_at_each() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let (remap, reader, into_the_island, into_in1) =
            a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
        let into_in2 = link(&mut graph, (&remap, "result"), (&reader, "in2"));
        assert_eq!(
            the_refusal_of_a_commit(&graph, &[], &[], std::slice::from_ref(&into_in2), &[], &[]),
            None
        );
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();

        for island_link in [&into_in1, &into_in2] {
            wire_a_link_from_a_function_node_island_into_its_reader(
                &mut graph,
                island_link,
                &iceoryx2_node,
            )
            .unwrap();
        }

        let reads = the_reads_of(&graph, &into_the_island);
        assert_eq!(reads.len(), 2);
        assert!(
            reads
                .iter()
                .all(|read_at| read_at.processor_id() == &reader)
        );
        assert_ne!(
            reads[0].port_name(),
            reads[1].port_name(),
            "two islands of one reader never share a read"
        );
        let islands = reader_setup_command_ports(&graph, &reader)["islands"].clone();
        assert_eq!(islands.as_array().map(Vec::len), Some(2));

        close_the_channels_no_island_reads_any_more(&mut graph).unwrap();
        assert_eq!(the_reads_of(&graph, &into_the_island).len(), 2);
    }

    #[test]
    fn a_class_node_link_wired_after_two_readers_opens_a_read_at_each() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let (remap, reader, into_the_island, into_the_reader) =
            a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
        let second_reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_second_reader = link(&mut graph, (&remap, "result"), (&second_reader, "in1"));
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();

        wire_a_link_into_a_function_node_island_through_its_reader(
            &mut graph,
            &into_the_island,
            &iceoryx2_node,
        )
        .unwrap();
        for island_link in [&into_the_reader, &into_the_second_reader] {
            wire_a_link_from_a_function_node_island_into_its_reader(
                &mut graph,
                island_link,
                &iceoryx2_node,
            )
            .unwrap();
        }

        let readers: HashSet<ProcessorUniqueId> = the_reads_of(&graph, &into_the_island)
            .iter()
            .map(|read_at| read_at.processor_id().clone())
            .collect();
        assert_eq!(readers, HashSet::from([reader, second_reader]));
    }

    pub(crate) fn a_class_node_feeding(graph: &mut Graph) -> ProcessorUniqueId {
        let source = add_a_node(
            graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        graph
            .traversal_mut()
            .v(&source)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
                OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    ProcessExecution::Reactive,
                ),
            )));
        source
    }

    fn is_read_at_a_hidden_input_of(graph: &Graph, link_id: &LinkUniqueId) -> bool {
        !the_reads_of(graph, link_id).is_empty()
    }

    #[test]
    fn removing_the_one_class_node_link_into_an_island_is_refused_when_it_leaves_its_reactive_reader_idle()
     {
        let mut graph = a_graph_resolving_three_function_node_types();
        let source = a_class_node_feeding(&mut graph);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let reader =
            a_helper_reader_before_its_setup_driven_in(&mut graph, ProcessExecution::Reactive);
        let into_the_island = link(&mut graph, (&source, "out1"), (&remap, "x"));
        link(&mut graph, (&remap, "result"), (&reader, "in1"));

        let refusal = the_refusal_of_a_commit(
            &graph,
            &[],
            &[],
            &[],
            std::slice::from_ref(&into_the_island),
            &[],
        )
        .expect("the reader's only wake source is the class node link");
        assert!(refusal.contains(&format!("leaves `{reader}`")), "{refusal}");
    }

    #[test]
    fn unwiring_one_island_of_a_reader_keeps_the_class_node_link_another_of_its_islands_reads() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let (_remap, reader, into_the_first_island, into_the_reader_at_in1) =
            a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
        let second_source = a_class_node_feeding(&mut graph);
        let second_remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let into_the_second_island =
            link(&mut graph, (&second_source, "out1"), (&second_remap, "x"));
        let into_the_reader_at_in2 = link(&mut graph, (&second_remap, "result"), (&reader, "in2"));
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();
        for island_link in [&into_the_reader_at_in1, &into_the_reader_at_in2] {
            wire_a_link_from_a_function_node_island_into_its_reader(
                &mut graph,
                island_link,
                &iceoryx2_node,
            )
            .unwrap();
        }

        unwire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader_at_in1,
        )
        .unwrap();

        assert!(!is_read_at_a_hidden_input_of(
            &graph,
            &into_the_first_island
        ));
        assert!(is_read_at_a_hidden_input_of(
            &graph,
            &into_the_second_island
        ));
    }

    #[test]
    fn a_class_node_link_into_a_function_node_that_joins_a_read_island_is_opened_at_its_reader() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &iceoryx2_node,
        )
        .unwrap();
        let source = a_class_node_feeding(&mut graph);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let into_the_reader_less_remap = link(&mut graph, (&source, "out1"), (&remap, "x"));
        wire_a_link_into_a_function_node_island_through_its_reader(
            &mut graph,
            &into_the_reader_less_remap,
            &iceoryx2_node,
        )
        .unwrap();
        assert!(!is_read_at_a_hidden_input_of(
            &graph,
            &into_the_reader_less_remap
        ));

        link(&mut graph, (&remap, "result"), (&mix, "a"));
        open_the_channels_each_island_newly_reads(&mut graph, &iceoryx2_node).unwrap();

        assert!(is_read_at_a_hidden_input_of(
            &graph,
            &into_the_reader_less_remap
        ));
    }

    #[test]
    fn a_class_node_link_into_a_function_node_an_edit_takes_out_of_its_island_is_closed() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));
        let source = a_class_node_feeding(&mut graph);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let into_remap = link(&mut graph, (&source, "out1"), (&remap, "x"));
        let remap_into_mix = link(&mut graph, (&remap, "result"), (&mix, "a"));
        let iceoryx2_node = Iceoryx2Node::for_this_test_process();
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &iceoryx2_node,
        )
        .unwrap();
        assert!(is_read_at_a_hidden_input_of(&graph, &into_remap));

        graph.traversal_mut().e(&remap_into_mix).drop();
        close_the_channels_no_island_reads_any_more(&mut graph).unwrap();

        assert!(!is_read_at_a_hidden_input_of(&graph, &into_remap));
        assert_eq!(link_state_of(&graph, &into_remap), LinkState::Wired);
    }

    #[test]
    fn a_class_node_link_its_island_still_reads_stays_open() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let (_remap, _reader, into_the_island, into_the_reader) =
            a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            &into_the_reader,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();

        close_the_channels_no_island_reads_any_more(&mut graph).unwrap();

        assert!(is_read_at_a_hidden_input_of(&graph, &into_the_island));
    }
}
