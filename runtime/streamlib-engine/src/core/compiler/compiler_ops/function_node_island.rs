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

use std::collections::HashSet;

use serde_json::Value as JsonValue;

use crate::core::ProcessorUniqueId;
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    FunctionNodeHostComponent, FunctionNodePromotedToItsOwnProcessComponent,
    FunctionNodeRanCompiledComponent, Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    GraphWeight, HowItsReadersRanAFunctionNode, Link, LinkState, LinkStateComponent, LinkUniqueId,
    OutOfProcessLinkWireRepliesComponent, OutOfProcessLinkWiringComponent,
};
use crate::core::processors::OutOfProcessLinkWireReply;

/// How the compiler wires one link, by the kinds of its two ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HowALinkIsWired {
    /// Through an iceoryx2 channel: neither end is a function node.
    ThroughAChannel,
    /// As a function call between two function nodes of one island.
    AsACallInsideAFunctionNodeIsland,
    /// As the island upstream of it, carried to the helper the link feeds.
    AsTheIslandItsReaderEvaluatesAtEachRead,
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

/// The type of the one function node a cycle of function nodes closes
/// through: it hands out its input as it was at the last evaluation.
pub(crate) const FEEDBACK_FUNCTION_NODE_TYPE: &str = "tatolab.ops:Feedback";

/// Whether `node_id` is a `Feedback` node, whose input the island reads from
/// the last evaluation.
fn is_a_feedback_node(graph: &Graph, node_id: &ProcessorUniqueId) -> bool {
    graph
        .traversal()
        .v(node_id)
        .first()
        .is_some_and(|node| node.processor_type.to_string() == FEEDBACK_FUNCTION_NODE_TYPE)
}

/// Whether `node_id` is a function node, inside its readers' islands or
/// promoted out of them.
fn is_a_function_node(graph: &Graph, node_id: &ProcessorUniqueId) -> bool {
    graph.traversal().v(node_id).first().is_some_and(|node| {
        node.node_kind()
            .is_some_and(|node_kind| matches!(node_kind, NodeKind::Function { .. }))
    })
}

/// How `link_id` is wired, refusing a link into a function node from a node
/// that is not one, whether or not the function node was promoted.
pub(crate) fn how_a_link_is_wired(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<HowALinkIsWired> {
    let link = link_in(graph, link_id)?;
    let source_id = link.source().processor_id();
    let destination_id = link.to_port().processor_id();
    match (
        runs_inside_the_read_of_each_node_it_feeds(graph, source_id),
        runs_inside_the_read_of_each_node_it_feeds(graph, destination_id),
    ) {
        (false, false)
            if is_a_function_node(graph, destination_id)
                && !is_a_function_node(graph, source_id) =>
        {
            Err(Error::InvalidLink(format!(
                "link `{link_id}` feeds function node `{destination_id}` from `{source_id}`, \
                 which is not a function node; a function node's inputs come from other \
                 function nodes or from its pins in this release"
            )))
        }
        (false, false) => Ok(HowALinkIsWired::ThroughAChannel),
        (true, true) => Ok(HowALinkIsWired::AsACallInsideAFunctionNodeIsland),
        (true, false) => Ok(HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead),
        (false, true) => Err(Error::InvalidLink(format!(
            "link `{link_id}` feeds function node `{destination_id}` from `{source_id}`, which \
             is not a function node; an island's inputs come from other function nodes or \
             from its pins in this release"
        ))),
    }
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
    let answer_awaited_from_the_reader = reader_link_wiring
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(island)?;
    reader_link_wiring.tell_each_function_node_how_its_far_side_ran_it(
        function_node_ids.iter().filter_map(|function_node_id| {
            the_report_of_how_its_readers_ran_a_function_node(graph, function_node_id)
                .map(|how_it_ran| (function_node_id.to_string(), how_it_ran))
        }),
    );
    mark_an_island_link_by_whether_its_reader_owes_an_answer(
        graph,
        link_id,
        answer_awaited_from_the_reader,
    )
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
    let island_links: Vec<LinkUniqueId> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| !links_wired_this_compile.contains(&link.id))
        .filter(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
                && !runs_inside_the_read_of_each_node_it_feeds(graph, link.to_port().processor_id())
        })
        .map(|link| link.id.clone())
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
                the_report_of_how_its_readers_ran_a_function_node(graph, function_node_id)
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

/// Where `function_node_id`'s readers note how they ran it, which `graph`
/// renders on the node as `ran_compiled`; attached on its first reader.
fn the_report_of_how_its_readers_ran_a_function_node(
    graph: &mut Graph,
    function_node_id: &ProcessorUniqueId,
) -> Option<std::sync::Arc<HowItsReadersRanAFunctionNode>> {
    let node = graph.traversal_mut().v(function_node_id).first_mut()?;
    if let Some(component) = node.get::<FunctionNodeRanCompiledComponent>() {
        return Some(std::sync::Arc::clone(&component.0));
    }
    let how_it_ran = std::sync::Arc::new(HowItsReadersRanAFunctionNode::default());
    node.insert(FunctionNodeRanCompiledComponent(std::sync::Arc::clone(
        &how_it_ran,
    )));
    Some(how_it_ran)
}

fn is_fed_by_a_node_that_is_not_a_function_node(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> bool {
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| link.to_port().processor_id() == node_id)
        .any(|link| {
            !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
        })
}

/// Unwire `link_id`, from a function node into its reader: forget the island
/// it carries on the reader's setup command and mark it disconnected.
pub(crate) fn unwire_a_link_from_a_function_node_island_into_its_reader(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let reader_id = link_in(graph, link_id)?.to_port().processor_id().clone();
    if let Some(reader_link_wiring) = graph
        .traversal()
        .v(&reader_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
    {
        reader_link_wiring
            .0
            .forget_a_function_node_island(link_id.as_str());
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
/// "port"}}]}]}`, and a stateful node's `"state_fields"` beside its `"node"`.
pub(crate) fn the_island_feeding_a_link(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<JsonValue> {
    let link = link_in(graph, link_id)?;
    let island_output_node_id = link.source().processor_id().clone();
    let mut nodes_each_after_what_it_reads = Vec::new();
    let mut placed = HashSet::new();
    let mut read_at_the_last_evaluation_by_a_feedback_node = vec![island_output_node_id.clone()];
    while let Some(node_id) = read_at_the_last_evaluation_by_a_feedback_node.pop() {
        place_a_function_node_after_what_it_reads(
            graph,
            &node_id,
            &mut placed,
            &mut Vec::new(),
            &mut nodes_each_after_what_it_reads,
            &mut read_at_the_last_evaluation_by_a_feedback_node,
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

fn place_a_function_node_after_what_it_reads(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
    placed: &mut HashSet<ProcessorUniqueId>,
    on_the_path_from_the_output: &mut Vec<ProcessorUniqueId>,
    nodes_each_after_what_it_reads: &mut Vec<JsonValue>,
    read_at_the_last_evaluation_by_a_feedback_node: &mut Vec<ProcessorUniqueId>,
) -> Result<()> {
    if placed.contains(node_id) {
        return Ok(());
    }
    if let Some(cycle_start) = on_the_path_from_the_output
        .iter()
        .position(|on_the_path| on_the_path == node_id)
    {
        let cycle: Vec<String> = on_the_path_from_the_output[cycle_start..]
            .iter()
            .chain(std::iter::once(node_id))
            .map(ToString::to_string)
            .collect();
        return Err(Error::InvalidGraph(format!(
            "function nodes {} read each other in a cycle; an island evaluates each node after \
             what it reads, so a cycle closes only through a `Feedback` node, which hands out \
             its input as it was at the last evaluation",
            cycle.join(" → ")
        )));
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

    on_the_path_from_the_output.push(node_id.clone());
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
            return Err(Error::InvalidLink(format!(
                "function node `{node_id}` reads `{upstream_id}`, which is not a function \
                 node; an island's inputs come from other function nodes or from its pins in \
                 this release"
            )));
        }
        if is_a_feedback_node(graph, node_id) {
            read_at_the_last_evaluation_by_a_feedback_node.push(upstream_id.clone());
        } else {
            place_a_function_node_after_what_it_reads(
                graph,
                upstream_id,
                placed,
                on_the_path_from_the_output,
                nodes_each_after_what_it_reads,
                read_at_the_last_evaluation_by_a_feedback_node,
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
    on_the_path_from_the_output.pop();

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
        island_node["state_fields"] = serde_json::json!(state_fields);
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
        refuse_a_link_closing_a_cycle_of_function_nodes(graph, link)?;
    }
    for link_id in links_to_remove {
        let link = link_in(graph, link_id)?;
        if runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id()) {
            continue;
        }
        let reader_id = link.to_port().processor_id();
        if is_a_running_reader(reader_id)
            && is_reactive_and_left_fed_only_by_islands(graph, reader_id, links_to_remove)
        {
            return Err(Error::InvalidLink(format!(
                "removing `{link_id}` leaves `{reader_id}`, which runs reactively, fed only by \
                 function node islands, which have no source to wake it, so it would never run \
                 again. To fix: link it another source first, or remove it"
            )));
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

/// Refuse `link`, between two function nodes, when what it feeds already
/// reaches what it reads through other function nodes, naming the cycle: an
/// island evaluates each node after what it reads.
fn refuse_a_link_closing_a_cycle_of_function_nodes(graph: &Graph, link: &Link) -> Result<()> {
    let source_id = link.source().processor_id();
    let destination_id = link.to_port().processor_id();
    if !runs_inside_the_read_of_each_node_it_feeds(graph, source_id)
        || !runs_inside_the_read_of_each_node_it_feeds(graph, destination_id)
        || is_a_feedback_node(graph, destination_id)
    {
        return Ok(());
    }
    let mut paths_to_follow = vec![vec![destination_id.clone()]];
    let mut function_nodes_followed = HashSet::new();
    while let Some(path) = paths_to_follow.pop() {
        let Some(following_id) = path.last() else {
            continue;
        };
        if following_id == source_id {
            let cycle: Vec<String> = std::iter::once(source_id.to_string())
                .chain(path.iter().map(ToString::to_string))
                .collect();
            return Err(Error::InvalidGraph(format!(
                "linking `{}` closes a cycle of function nodes, {}; an island evaluates each \
                 node after what it reads, so a cycle closes only through a `Feedback` node, \
                 which hands out its input as it was at the last evaluation. To fix: link the \
                 loop's way back through a `Feedback` node",
                link.id,
                cycle.join(" → ")
            )));
        }
        if !function_nodes_followed.insert(following_id.clone()) {
            continue;
        }
        for downstream_link in graph.traversal().e(()).iter() {
            if downstream_link.id == link.id
                || downstream_link.source().processor_id() != following_id
            {
                continue;
            }
            let downstream_id = downstream_link.to_port().processor_id();
            if runs_inside_the_read_of_each_node_it_feeds(graph, downstream_id)
                && !is_a_feedback_node(graph, downstream_id)
            {
                let mut longer_path = path.clone();
                longer_path.push(downstream_id.clone());
                paths_to_follow.push(longer_path);
            }
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
    let mut readers = HashSet::new();
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
                readers.insert(destination_id.clone());
            }
        }
    }
    readers
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
    runs_reactively
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
            descriptor = descriptor.with_input(PortDescriptor::new(
                *input_port,
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            ));
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
        let into_a_function_node_from_a_process_node =
            link(&mut graph, (&source, "out1"), (&remap, "x"));
        let refusal = how_a_link_is_wired(&graph, &into_a_function_node_from_a_process_node)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("which is not a function node"),
            "{refusal}"
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
    fn a_cycle_of_function_nodes_is_refused_naming_it() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let first = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let second = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&first, "result"), (&second, "a"));
        link(&mut graph, (&second, "result"), (&first, "x"));
        let into_the_reader = link(&mut graph, (&second, "result"), (&reader, "in1"));
        let refusal = the_island_feeding_a_link(&graph, &into_the_reader)
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("in a cycle"), "{refusal}");
    }

    #[test]
    fn a_link_into_a_helper_carries_its_island_on_the_setup_command_and_is_wired() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));

        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
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
        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
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
        let refusal =
            wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
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
        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
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
        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
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
            wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, island_link)
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
        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
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
    fn a_link_closing_a_cycle_of_function_nodes_is_refused_naming_the_cycle() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        link(&mut graph, (&lfo, "result"), (&mix, "a"));
        link(&mut graph, (&mix, "result"), (&remap, "x"));
        let closing_the_cycle = link(&mut graph, (&remap, "result"), (&mix, "b"));

        let refusal = the_refusal_of_a_commit(
            &graph,
            &[],
            &[],
            std::slice::from_ref(&closing_the_cycle),
            &[],
            &[],
        )
        .expect("refused");
        assert!(
            refusal.contains("closes a cycle of function nodes"),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("{remap} → {mix} → {remap}")),
            "{refusal}"
        );
        assert!(refusal.contains("`Feedback` node"), "{refusal}");
    }

    fn a_graph_resolving_the_test_function_node_types_and_feedback() -> Graph {
        a_graph_resolving_these_function_node_types([
            a_function_node_type("test_ops:lfo", &[], &["result"]),
            a_function_node_type("test_ops:remap", &["x"], &["result"]),
            a_function_node_type("test_ops:mix", &["a", "b"], &["result"]),
            a_function_node_type(FEEDBACK_FUNCTION_NODE_TYPE, &["value"], &["result"]),
        ])
    }

    #[test]
    fn a_link_closing_a_loop_through_feedback_is_taken_live() {
        let mut graph = a_graph_resolving_the_test_function_node_types_and_feedback();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let feedback = add_a_node(&mut graph, FEEDBACK_FUNCTION_NODE_TYPE, JsonValue::Null);
        link(&mut graph, (&lfo, "result"), (&mix, "a"));
        link(&mut graph, (&feedback, "result"), (&mix, "b"));
        let closing_the_loop = link(&mut graph, (&mix, "result"), (&feedback, "value"));

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
    fn an_island_closing_a_loop_through_feedback_evaluates_feedback_before_what_it_reads() {
        let mut graph = a_graph_resolving_the_test_function_node_types_and_feedback();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let feedback = add_a_node(&mut graph, FEEDBACK_FUNCTION_NODE_TYPE, JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&lfo, "result"), (&mix, "a"));
        link(&mut graph, (&feedback, "result"), (&mix, "b"));
        link(&mut graph, (&mix, "result"), (&feedback, "value"));
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
        assert!(position_of(&feedback) < position_of(&mix), "{order:?}");
        assert!(position_of(&lfo) < position_of(&mix), "{order:?}");
        let feedback_node = &island["nodes"][position_of(&feedback)];
        assert_eq!(
            feedback_node["links_into"],
            serde_json::json!([{ "port": "value", "from": { "node": mix.to_string(), "port": "result" } }]),
            "`Feedback` still names what it reads, which the reader takes from the last evaluation"
        );
    }

    #[test]
    fn a_loop_whose_way_back_skips_feedback_is_still_refused() {
        let mut graph = a_graph_resolving_the_test_function_node_types_and_feedback();
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let feedback = add_a_node(&mut graph, FEEDBACK_FUNCTION_NODE_TYPE, JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&feedback, "result"), (&mix, "a"));
        link(&mut graph, (&mix, "result"), (&remap, "x"));
        link(&mut graph, (&remap, "result"), (&mix, "b"));
        let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

        let refusal = the_island_feeding_a_link(&graph, &into_the_reader)
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("in a cycle"), "{refusal}");
        assert!(refusal.contains("`Feedback` node"), "{refusal}");
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
        let refusal =
            wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
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
        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
            .unwrap();
        assert_eq!(link_state_of(&graph, &into_the_reader), LinkState::Wired);
        let setup_command_islands =
            the_link_wiring_of(&graph, &reader).as_setup_command_ports()["islands"].clone();
        assert_eq!(
            setup_command_islands[0]["link_id"],
            into_the_reader.as_str()
        );
    }
}
