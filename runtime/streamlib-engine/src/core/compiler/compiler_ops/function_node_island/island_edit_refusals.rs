// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Refusing, before a commit changes anything, an edit an island cannot take.

use std::collections::HashSet;

use serde_json::Value as JsonValue;

use crate::core::ProcessorUniqueId;
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    Graph, GraphNodeWithComponents, GraphWeight, Link, LinkUniqueId,
    OutOfProcessLinkWiringComponent,
};

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

/// Refuse `link`, between two function nodes, when what it feeds already
/// reaches what it reads through other function nodes, naming the cycle: an
/// island evaluates each node after what it reads.
fn refuse_a_link_closing_a_cycle_of_function_nodes(graph: &Graph, link: &Link) -> Result<()> {
    let source_id = link.source().processor_id();
    let destination_id = link.to_port().processor_id();
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
                "linking `{}` closes a cycle of function nodes, {}; an island evaluates each \
                 node after what it reads, so a cycle closes only through a `PreviousValue` node, \
                 which hands out its input as it was at the last evaluation. To fix: link the \
                 loop's way back through a `PreviousValue` node",
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
