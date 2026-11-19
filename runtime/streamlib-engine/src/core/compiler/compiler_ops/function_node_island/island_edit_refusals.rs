// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Refusing, before a commit changes anything, an edit an island cannot take.

use std::collections::HashSet;

use serde_json::Value as JsonValue;

use crate::core::NodeRunId;
use crate::core::descriptors::{NodeClassImportPath, PinConversion};
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    Graph, GraphNodePorts, GraphNodeWithComponents, GraphWeight, InputLinkPortRef, Link,
    LinkUniqueId, OutOfProcessLinkWiringComponent, OutputLinkPortRef,
};
use crate::core::nodes::NodeTypesOneStreamResolves;

use super::{
    is_a_previous_value_node, link_in, runs_inside_the_read_of_each_node_it_feeds,
    the_function_nodes_upstream_of, the_links_into_function_nodes_from_outside_any_island,
    the_readers_a_function_node_feeds,
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
    for link_id in links_to_remove {
        let link = link_in(graph, link_id)?;
        if runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id()) {
            continue;
        }
        let destination_id = link.to_port().node_run_id();
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

/// Refuse the two links a node of `conversion`, not yet in `graph`, would carry: from `from` into
/// its input, and from its output into `to`, as
/// [`refuse_a_link_feeding_an_island_into_the_app_process`] and
/// [`refuse_a_link_an_island_cannot_take`] refuse each once the node and both links are made.
/// The node runs inside no island, as every convert node a transaction places does: a built-in,
/// natively compiled.
pub(crate) fn refuse_the_links_through_a_node_not_yet_added(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    conversion: &PinConversion,
) -> Result<()> {
    let (node_type, input_port) = (&conversion.node_type, &conversion.input_port_name);
    let node_not_yet_added = NodeRunId::new();
    let into_it = InputLinkPortRef::new(node_not_yet_added.clone(), input_port);
    let out_of_it = OutputLinkPortRef::new(node_not_yet_added, &conversion.output_port_name);
    let node_types = graph.node_types_this_stream_resolves();
    if runs_inside_the_read_of_each_node_it_feeds(graph, from.node_run_id()) {
        if runs_each_of_its_nodes_in_the_app_process(node_types, node_type) {
            return Err(the_refusal_of_an_island_fed_into_the_app_process(
                &format!(
                    "the link from `{from}` into the `{node_type}` node placed between it and \
                     `{to}`"
                ),
                node_type,
            ));
        }
        refuse_an_island_into_an_input_declaring_an_audio_window(
            node_types.port_info(node_type).as_ref(),
            node_type,
            input_port,
        )?;
    }
    refuse_a_link_an_island_cannot_take(graph, from, &into_it, None)?;
    refuse_a_link_feeding_an_island_into_the_app_process(graph, &out_of_it, to)?;
    refuse_a_link_an_island_cannot_take(graph, &out_of_it, to, None)
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

/// The refusal of `link_named`, a link feeding a function node island into `reader`, which
/// runs in the app process.
pub(super) fn the_refusal_of_an_island_fed_into_the_app_process(
    link_named: &str,
    reader: &dyn std::fmt::Display,
) -> Error {
    Error::InvalidLink(format!(
        "{link_named} feeds a function node island into `{reader}`, which runs in the app \
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
    refuse_an_island_into_an_input_declaring_an_audio_window(
        graph
            .traversal()
            .v(reader_id)
            .first()
            .map(|reader| &reader.ports),
        reader_id,
        to.port_name(),
    )
}

/// Refuse a link from a function node into the input `port_name` of `reader`, whose ports are
/// `reader_ports`, when that input declares an `audio_window`.
fn refuse_an_island_into_an_input_declaring_an_audio_window(
    reader_ports: Option<&GraphNodePorts>,
    reader: &dyn std::fmt::Display,
    port_name: &str,
) -> Result<()> {
    let declares_an_audio_window = reader_ports.is_some_and(|ports| {
        ports
            .inputs
            .iter()
            .any(|input| input.name == port_name && input.audio_window.is_some())
    });
    if declares_an_audio_window {
        return Err(Error::InvalidLink(format!(
            "`{reader}`'s input `{port_name}` declares an `audio_window`, and a function \
             node island answers one control value per read, never a window of samples. To \
             fix: link the island into a port with no `audio_window`"
        )));
    }
    Ok(())
}

fn is_reactive_and_left_fed_only_by_islands(
    graph: &Graph,
    reader_id: &NodeRunId,
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
    runs_reactively
        && !the_island_reads_a_link_left_from_outside_any_island
        && links_left_into_the_reader.iter().any(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
        })
        && links_left_into_the_reader.iter().all(|link| {
            runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
        })
}
