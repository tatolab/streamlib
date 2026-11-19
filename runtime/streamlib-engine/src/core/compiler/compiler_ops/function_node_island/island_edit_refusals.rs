// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Refusing, before a commit changes anything, an edit an island cannot take.

use std::collections::{HashMap, HashSet};

use serde_json::Value as JsonValue;

use crate::core::NodeRunId;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    Graph, GraphNodeWithComponents, GraphWeight, InputLinkPortRef, Link, LinkUniqueId,
    OutOfProcessLinkWiringComponent, OutputLinkPortRef,
};
use crate::core::nodes::NodeTypesOneStreamResolves;

use super::{
    is_a_previous_value_node, is_fed_by_a_node_that_is_not_a_function_node, link_in,
    runs_inside_the_read_of_each_node_it_feeds, the_function_nodes_upstream_of,
    the_links_into_function_nodes_from_outside_any_island, the_readers_a_link_into_reaches,
};

/// Refuse, before a commit changes anything, an edit an island cannot take: a
/// second link into a port an island feeds, an island into a port declaring an
/// audio window, a removal that leaves a running reactive reader fed only by
/// islands, and a live config update to a function node.
pub(crate) fn refuse_an_edit_an_island_cannot_take(
    graph: &Graph,
    nodes_to_add: &[NodeRunId],
    nodes_to_remove: &[NodeRunId],
    links_to_add: &[LinkUniqueId],
    links_to_remove: &[LinkUniqueId],
    config_updates: &[(NodeRunId, JsonValue)],
) -> Result<()> {
    let is_a_running_reader = |node_id: &NodeRunId| {
        !runs_inside_the_read_of_each_node_it_feeds(graph, node_id)
            && !nodes_to_add.contains(node_id)
            && !nodes_to_remove.contains(node_id)
    };
    for link_id in links_to_add {
        let link = link_in(graph, link_id)?;
        refuse_a_link_an_island_cannot_take(graph, link.source(), link.to_port(), Some(link_id))?;
    }
    if let Some(reader_id) =
        the_reader_a_removal_strands(graph, links_to_remove, is_a_running_reader)?
    {
        let removal_named = match links_to_remove {
            [link_id] => format!("link `{link_id}`"),
            _ => format!("{} links", links_to_remove.len()),
        };
        return Err(the_refusal_of_a_removal_stranding(
            &removal_named,
            &reader_id,
        ));
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

/// Refuse a link from `from` into `to` an island cannot take: a second link into a port an
/// island feeds, an island into a port declaring an audio window, or a link closing a cycle of
/// function nodes. `held_as` names the link when `graph` already holds it, so it is counted
/// once; a link not yet made is judged as if `graph` held it.
pub(crate) fn refuse_a_link_an_island_cannot_take(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    held_as: Option<&LinkUniqueId>,
) -> Result<()> {
    refuse_a_second_link_into_a_port_an_island_feeds(graph, from, to, held_as)?;
    refuse_an_island_into_a_port_declaring_an_audio_window(graph, from, to)?;
    refuse_a_link_closing_a_cycle_of_function_nodes(graph, from, to, held_as)
}

/// Whether `link` is a link other than the one `held_as` names.
fn is_another_link(link: &Link, held_as: Option<&LinkUniqueId>) -> bool {
    held_as.is_none_or(|held_as| held_as.as_str() != link.id())
}

/// Refuse a link from `from` into `to`, before it is made, when it would feed a function node
/// island into a node of a type that runs in the app process, in the words the wiring of such a
/// link refuses it in; a stopped stream wires nothing, so this is what keeps the link out of
/// the graph it saves.
pub(crate) fn refuse_a_link_feeding_an_island_into_the_app_process(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<()> {
    let reader_id = to.node_run_id();
    if !runs_inside_the_read_of_each_node_it_feeds(graph, from.node_run_id())
        || runs_inside_the_read_of_each_node_it_feeds(graph, reader_id)
    {
        return Ok(());
    }
    let Some(reader) = graph.traversal().v(reader_id).first() else {
        return Ok(());
    };
    if !runs_each_of_its_nodes_in_the_app_process(
        graph.node_types_this_stream_resolves(),
        reader.node_type(),
    ) {
        return Ok(());
    }
    Err(the_refusal_of_an_island_fed_into_the_app_process(
        &format!("the link from `{from}` to `{to}`"),
        reader_id,
    ))
}

/// Whether every node of `node_type` runs in the app process: a natively compiled type, or a
/// view the stream's project brings. A type the stream's node code describes runs in a helper
/// process, a shader node on whichever evaluator its island has, and a type held as a
/// placeholder runs nowhere, so none of them is.
fn runs_each_of_its_nodes_in_the_app_process(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
) -> bool {
    node_types
        .placeholder_of_the_node_type_not_described(node_type)
        .is_none()
        && (node_types.is_a_natively_compiled_node_type(node_type)
            || node_types.is_a_view_type_of_this_streams_project(node_type))
}

/// The refusal of `link_named`, a link feeding a function node island into `reader_id`, which
/// runs in the app process.
pub(super) fn the_refusal_of_an_island_fed_into_the_app_process(
    link_named: &str,
    reader_id: &NodeRunId,
) -> Error {
    Error::InvalidLink(format!(
        "{link_named} feeds a function node island into `{reader_id}`, which runs in the app \
         process; a function node runs inside the read of a node in a helper process, and the \
         app process runs no node code"
    ))
}

/// Refuse a link from `from` into `to`, between two function nodes, when what it feeds already
/// reaches what it reads through other function nodes, naming the cycle: an island evaluates
/// each node after what it reads.
fn refuse_a_link_closing_a_cycle_of_function_nodes(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    held_as: Option<&LinkUniqueId>,
) -> Result<()> {
    let source_id = from.node_run_id();
    let destination_id = to.node_run_id();
    if !runs_inside_the_read_of_each_node_it_feeds(graph, source_id)
        || !runs_inside_the_read_of_each_node_it_feeds(graph, destination_id)
        || is_a_previous_value_node(graph, destination_id)
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
                "linking `{from}` to `{to}` closes a cycle of function nodes, {}; an island \
                 evaluates each node after what it reads, so a cycle closes only through a \
                 `PreviousValue` node, which hands out its input as it was at the last \
                 evaluation. To fix: link the loop's way back through a `PreviousValue` node",
                cycle.join(" → ")
            )));
        }
        if !function_nodes_followed.insert(following_id.clone()) {
            continue;
        }
        for downstream_link in graph.traversal().e(()).iter() {
            if !is_another_link(downstream_link, held_as)
                || !graph.stands(downstream_link)
                || downstream_link.source().node_run_id() != following_id
            {
                continue;
            }
            let downstream_id = downstream_link.to_port().node_run_id();
            if runs_inside_the_read_of_each_node_it_feeds(graph, downstream_id)
                && !is_a_previous_value_node(graph, downstream_id)
            {
                let mut longer_path = path.clone();
                longer_path.push(downstream_id.clone());
                paths_to_follow.push(longer_path);
            }
        }
    }
    Ok(())
}

/// Refuse a link from `from` into `to` when it and another link both feed one input port and
/// either comes from a function node: an island answers each read of its port, so it shares
/// the port with no other producer.
fn refuse_a_second_link_into_a_port_an_island_feeds(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    held_as: Option<&LinkUniqueId>,
) -> Result<()> {
    let other_links_into_the_port: Vec<&Link> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|other_link| {
            is_another_link(other_link, held_as)
                && graph.stands(other_link)
                && other_link.to_port().node_run_id() == to.node_run_id()
                && other_link.to_port().port_name() == to.port_name()
        })
        .collect();
    let an_island_feeds_the_port =
        runs_inside_the_read_of_each_node_it_feeds(graph, from.node_run_id())
            || other_links_into_the_port.iter().any(|link_into| {
                runs_inside_the_read_of_each_node_it_feeds(graph, link_into.source().node_run_id())
            });
    if !other_links_into_the_port.is_empty() && an_island_feeds_the_port {
        return Err(Error::InvalidLink(format!(
            "`{}`'s input `{}` is fed by {} links, and one comes from a function node; an \
             island answers each read of its port, so it shares the port with no other link. \
             To fix: give each its own input port",
            to.node_run_id(),
            to.port_name(),
            other_links_into_the_port.len() + 1
        )));
    }
    Ok(())
}

/// Refuse a link from `from`, a function node, into `to`, an input port declaring an
/// `audio_window`: an island answers one control value per read, never a window of audio
/// samples.
fn refuse_an_island_into_a_port_declaring_an_audio_window(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
) -> Result<()> {
    if !runs_inside_the_read_of_each_node_it_feeds(graph, from.node_run_id()) {
        return Ok(());
    }
    let reader_id = to.node_run_id();
    let port_name = to.port_name();
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

/// The reader, one `is_a_reader_judged` takes, that removing `links_to_remove` leaves running
/// reactively, fed only by function node islands, which have no source to wake it.
pub(crate) fn the_reader_a_removal_strands(
    graph: &Graph,
    links_to_remove: &[LinkUniqueId],
    is_a_reader_judged: impl Fn(&NodeRunId) -> bool,
) -> Result<Option<NodeRunId>> {
    for link_id in links_to_remove {
        let link = link_in(graph, link_id)?;
        if runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id()) {
            continue;
        }
        if let Some(reader_id) =
            the_readers_a_link_into_reaches(graph, link.to_port().node_run_id())
                .into_iter()
                .find(|reader_id| {
                    is_a_reader_judged(reader_id)
                        && is_reactive_and_left_fed_only_by_islands(
                            graph,
                            reader_id,
                            links_to_remove,
                        )
                })
        {
            return Ok(Some(reader_id));
        }
    }
    Ok(None)
}

/// The refusal of removing `removal_named`, which leaves `reader_id` running reactively, fed
/// only by function node islands.
pub(crate) fn the_refusal_of_a_removal_stranding(
    removal_named: &str,
    reader_id: &NodeRunId,
) -> Error {
    Error::InvalidLink(format!(
        "removing {removal_named} leaves `{reader_id}`, which runs reactively, fed only by \
         function node islands, which have no source to wake it, so it would never run again. \
         To fix: link it another source first, or disconnect the function node link into \
         `{reader_id}` first, or remove it"
    ))
}

/// Refuse `link`, which `graph` holds, when it feeds a function node island into a reader that
/// runs reactively and that only islands feed.
pub(crate) fn refuse_a_link_leaving_a_reactive_reader_fed_only_by_islands(
    graph: &Graph,
    link: &Link,
) -> Result<()> {
    if !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id()) {
        return Ok(());
    }
    match the_readers_a_link_into_reaches(graph, link.to_port().node_run_id())
        .iter()
        .find(|reader_id| is_a_reactive_reader_only_islands_feed(graph, reader_id))
    {
        Some(reader_id) => Err(the_refusal_of_a_reactive_reader_fed_only_by_islands(
            reader_id,
        )),
        None => Ok(()),
    }
}

/// Each link `graph` holds from a function node into a reader that runs reactively and that
/// only islands feed, with why the engine cannot wire it: judged once every link of a saved
/// graph or a batch of edits is made, so the order they are made in never matters, against the
/// graph left once each node and link pending deletion leaves.
pub(crate) fn the_island_links_into_a_reactive_reader_only_islands_feed(
    graph: &Graph,
) -> impl Iterator<Item = (LinkUniqueId, Error)> + '_ {
    let links: Vec<&Link> = graph.traversal().e(()).iter().collect();
    let links_leaving: Vec<LinkUniqueId> = links
        .iter()
        .filter(|link| !graph.stands(link))
        .map(|link| link.id.clone())
        .collect();
    let leaving: HashSet<&str> = links_leaving.iter().map(LinkUniqueId::as_str).collect();
    let links_standing: Vec<&Link> = links
        .into_iter()
        .filter(|link| !leaving.contains(link.id.as_str()))
        .collect();
    let mut stranded_by_reader: HashMap<NodeRunId, bool> = HashMap::new();
    links_standing.into_iter().filter_map(move |link| {
        let reader_id = link.to_port().node_run_id();
        let strands_its_reader =
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
                && !runs_inside_the_read_of_each_node_it_feeds(graph, reader_id)
                && *stranded_by_reader
                    .entry(reader_id.clone())
                    .or_insert_with(|| {
                        if links_leaving.is_empty() {
                            is_a_reactive_reader_only_islands_feed(graph, reader_id)
                        } else {
                            is_reactive_and_left_fed_only_by_islands(
                                graph,
                                reader_id,
                                &links_leaving,
                            )
                        }
                    });
        strands_its_reader.then(|| {
            (
                link.id.clone(),
                the_refusal_of_a_reactive_reader_fed_only_by_islands(reader_id),
            )
        })
    })
}

/// Whether `reader_id` runs reactively and only function node islands feed it, so no source
/// wakes it.
pub(crate) fn is_a_reactive_reader_only_islands_feed(graph: &Graph, reader_id: &NodeRunId) -> bool {
    runs_reactively(graph, reader_id)
        && !is_fed_by_a_node_that_is_not_a_function_node(graph, reader_id)
}

/// The refusal of a link feeding a function node island into `reader_id`, which runs
/// reactively and which only islands feed.
pub(super) fn the_refusal_of_a_reactive_reader_fed_only_by_islands(reader_id: &NodeRunId) -> Error {
    Error::InvalidLink(format!(
        "`{reader_id}` runs reactively and only function node islands feed it, and an island \
         has no source to wake it, so it would never run. To fix: declare it \
         `execution=\"continuous\"` with an `interval_ms`, or link it a source"
    ))
}

/// Whether `reader_id` runs reactively: as the helper it runs in is driven, or, before it runs,
/// as its class declares.
fn runs_reactively(graph: &Graph, reader_id: &NodeRunId) -> bool {
    let Some(reader) = graph.traversal().v(reader_id).first() else {
        return false;
    };
    let execution = reader
        .get::<OutOfProcessLinkWiringComponent>()
        .map(|component| component.0.far_side_process_execution())
        .or_else(|| {
            graph
                .node_types_this_stream_resolves()
                .the_execution_a_described_class_declares(reader.node_type())
        });
    execution == Some(ProcessExecution::Reactive)
}

fn is_reactive_and_left_fed_only_by_islands(
    graph: &Graph,
    reader_id: &NodeRunId,
    links_to_remove: &[LinkUniqueId],
) -> bool {
    let links_left_into_the_reader: Vec<&Link> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            link.to_port().node_run_id() == reader_id
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
    runs_reactively(graph, reader_id)
        && !the_island_reads_a_link_left_from_outside_any_island
        && links_left_into_the_reader.iter().any(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
        })
        && links_left_into_the_reader.iter().all(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
        })
}
