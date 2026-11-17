// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod channel_service_naming_and_sizing;
mod link_destination_side;
mod link_source_side;

pub(crate) use link_source_side::{
    SourceSideHeldOpenForAReaderOnTheReservedSlot,
    open_the_source_side_for_a_reader_on_the_reserved_slot,
};

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::NodeRunId;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    Iceoryx2ServicesHeldOpenForLinkComponent, InputLinkPortRef, Link,
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkState, LinkStateComponent,
    LinkUniqueId, LossCountsOfPortsInThisProcess, NodeInstanceComponent, NodeLossCounts,
    NodeMetrics, OutOfProcessLinkWireRepliesComponent, OutOfProcessLinkWiringComponent,
    OutputLinkPortRef, ReadOfALinkByTheReaderOfAFunctionNodeIsland,
};
use crate::core::nodes::{NodeInstance, OutOfProcessLinkWireReply, OutOfProcessLinkWiringEnvelope};
use crate::iceoryx2::Iceoryx2Node;
use channel_service_naming_and_sizing::{
    RoomForAWindowedReaderTheSourcesGraphCannotSee, audio_windowing_declared_by_input_port_of,
    channel_service_name, delivery_resolution_of_input_port, destination_max_notifiers,
    node_class_import_path_of, notify_service_name_for,
    refuse_a_second_inbound_link_into_a_windowed_port,
    refuse_a_windowed_port_onto_a_channel_created_shallower_than_its_ring, resolve_channel_sizing,
    subscriber_ring_depth_of_input_port,
};
use link_destination_side::{
    DestinationOfALinkAsItsWiringReadsIt, open_the_destination_side_of_a_link,
    read_the_destination_of_a_link, release_the_destination_side_of_a_link,
};
use link_source_side::{
    OpenedSourceSideOfALink, open_the_source_side_of_a_link, release_the_source_side_of_a_link,
};

/// Open an iceoryx2 channel for a `connect()` link in the graph.
///
/// The data service is source-channel-keyed (single publisher, N subscribers);
/// the notify service is destination-keyed. Handles four endpoint combinations:
/// - Rust→Rust: full wiring (publisher + notifier on source, subscriber +
///   listener on dest).
/// - Rust→subprocess: source-side Rust wiring; the subprocess opens its own
///   subscriber from the wiring envelope.
/// - subprocess→Rust: dest-side Rust wiring; the subprocess opens its own
///   publisher from the wiring envelope.
/// - subprocess→subprocess: both sides open their own ports.
///
/// In every combination the engine creates both services and the link holds
/// them for as long as it stays in the graph, so no endpoint that opens later
/// can size them.
#[tracing::instrument(
    name = "compiler.open_iceoryx2_service",
    skip(graph, iceoryx2_node),
    fields(link_id = %link_id)
)]
pub(crate) fn open_iceoryx2_service(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let to_port = graph
        .traversal()
        .e(link_id)
        .first()
        .ok_or_else(|| Error::LinkNotFound(format!("Link '{}' not found in graph", link_id)))?
        .to_port()
        .clone();
    open_iceoryx2_service_read_at(graph, link_id, &to_port, iceoryx2_node)
}

/// Open the channel of `link_id`, into a shader node fused into the island of
/// another, at `host_input_port` of the node evaluating that island, under the
/// delivery profile of the fused node's own input port.
pub(crate) fn open_iceoryx2_service_into_the_host_of_a_shader_node_island(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    host_input_port: &InputLinkPortRef,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    open_iceoryx2_service_read_at(graph, link_id, host_input_port, iceoryx2_node)
}

/// Close the channel of `link_id` the node evaluating its shader node island
/// reads at `host_input_port`.
pub(crate) fn close_iceoryx2_service_into_the_host_of_a_shader_node_island(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    host_input_port: &InputLinkPortRef,
) -> Result<()> {
    let Some(source) = graph
        .traversal()
        .e(link_id)
        .first()
        .map(|link| link.source().clone())
    else {
        return Ok(());
    };
    release_the_source_side_of_a_link(graph, source.node_run_id(), source.port_name(), link_id);
    release_the_destination_side_of_a_link(
        graph,
        host_input_port.node_run_id(),
        host_input_port.port_name(),
        link_id,
    );
    if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
        link.remove::<Iceoryx2ServicesHeldOpenForLinkComponent>();
        link.remove::<OutOfProcessLinkWireRepliesComponent>();
        link.insert(LinkStateComponent(LinkState::Disconnected));
    }
    tracing::info!(
        "Closed iceoryx2 service: {} at {} (state: Disconnected)",
        link_id,
        host_input_port
    );
    Ok(())
}

/// The identity the read of `link_id` by the reader of `island_link_id` is
/// wired under on both ends of the channel.
pub(crate) fn the_wiring_link_id_of_a_read_by_the_reader_of_an_island(
    link_id: &LinkUniqueId,
    island_link_id: &LinkUniqueId,
) -> LinkUniqueId {
    LinkUniqueId::from(format!("{link_id}_read_for_{island_link_id}"))
}

/// Open a read of the channel of `link_id`, into a function node, at
/// `reader_input_port` of the reader `island_link_id` carries the function
/// node's island to: the reader reads the link's messages there under the
/// delivery profile of the function node's own input port. The read is kept
/// on the link beside every other, and the link is stamped by the answers all
/// of them await; a read that cannot be opened is rolled back.
pub(crate) fn open_a_read_of_a_link_by_the_reader_of_a_function_node_island(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    island_link_id: &LinkUniqueId,
    reader_input_port: InputLinkPortRef,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let (from_port, to_port) = {
        let link = graph
            .traversal()
            .e(link_id)
            .first()
            .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
        (link.source().clone(), link.to_port().clone())
    };
    let wiring_link_id =
        the_wiring_link_id_of_a_read_by_the_reader_of_an_island(link_id, island_link_id);
    let destination =
        read_the_destination_of_a_link(graph, &reader_input_port, &to_port, &wiring_link_id)?;
    let opened_source_side = open_the_source_side_of_a_link(
        graph,
        &from_port,
        &wiring_link_id,
        &destination,
        iceoryx2_node,
    )?;
    let wire_reply_awaited_from_the_destination = match open_the_destination_side_of_a_link(
        graph,
        &wiring_link_id,
        &opened_source_side,
        destination,
    ) {
        Ok(wire_reply) => wire_reply,
        Err(refusal) => {
            release_both_sides_of_a_read_by_the_reader_of_an_island(
                graph,
                &from_port,
                &reader_input_port,
                &wiring_link_id,
            );
            return Err(refusal);
        }
    };
    let read = ReadOfALinkByTheReaderOfAFunctionNodeIsland {
        island_link_id: island_link_id.clone(),
        reader_input_port,
        wiring_link_id,
        wire_replies_awaited: opened_source_side
            .wire_reply_awaited_from_the_source
            .into_iter()
            .chain(wire_reply_awaited_from_the_destination)
            .collect(),
        services_held_open: Iceoryx2ServicesHeldOpenForLinkComponent {
            channel_data_service: opened_source_side.channel_data_service,
            destination_notify_service: opened_source_side.destination_notify_service,
        },
    };
    tracing::info!(
        channel = %opened_source_side.channel_service_name,
        "Opened a read of iceoryx2 channel [{}] at {}",
        link_id,
        read.reader_input_port
    );
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    if let Some(read_through) =
        link.get_mut::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
    {
        read_through.reads_by_island_link.push(read);
    } else {
        link.insert(LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent {
            reads_by_island_link: vec![read],
        });
    }
    stamp_a_link_by_the_answers_its_reads_await(link);
    Ok(())
}

/// Close the read of `link_id` the reader of `island_link_id` holds, if it
/// holds one, and stamp the link by the answers the reads left await: `Wired`
/// with no channel once none is left.
pub(crate) fn close_a_read_of_a_link_by_the_reader_of_a_function_node_island(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    island_link_id: &LinkUniqueId,
) -> Result<()> {
    let Some(link) = graph.traversal_mut().e(link_id).first_mut() else {
        return Ok(());
    };
    let source = link.source().clone();
    let Some(read_through) =
        link.get_mut::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
    else {
        return Ok(());
    };
    let Some(read_index) = read_through
        .reads_by_island_link
        .iter()
        .position(|read| &read.island_link_id == island_link_id)
    else {
        return Ok(());
    };
    let closed_read = read_through.reads_by_island_link.remove(read_index);
    let no_read_is_left = read_through.reads_by_island_link.is_empty();
    if no_read_is_left {
        link.remove::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>();
    }
    stamp_a_link_by_the_answers_its_reads_await(link);
    tracing::info!(
        "Closing the read of {link_id} at {}",
        closed_read.reader_input_port
    );
    release_both_sides_of_a_read_by_the_reader_of_an_island(
        graph,
        &source,
        &closed_read.reader_input_port,
        &closed_read.wiring_link_id,
    );
    Ok(())
}

/// Stamp `link` `Wired`, or `Pending` with every answer its reads still
/// await.
fn stamp_a_link_by_the_answers_its_reads_await(link: &mut Link) {
    let wire_replies_awaited: Vec<Arc<OutOfProcessLinkWireReply>> = link
        .get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
        .map(|read_through| {
            read_through
                .reads_by_island_link
                .iter()
                .flat_map(|read| read.wire_replies_awaited.iter().cloned())
                .collect()
        })
        .unwrap_or_default();
    if wire_replies_awaited.is_empty() {
        link.remove::<OutOfProcessLinkWireRepliesComponent>();
        link.insert(LinkStateComponent(LinkState::Wired));
    } else {
        link.insert(LinkStateComponent(LinkState::Pending));
        link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(
            wire_replies_awaited,
        ));
    }
}

fn release_both_sides_of_a_read_by_the_reader_of_an_island(
    graph: &mut Graph,
    source: &OutputLinkPortRef,
    reader_input_port: &InputLinkPortRef,
    wiring_link_id: &LinkUniqueId,
) {
    release_the_source_side_of_a_link(
        graph,
        source.node_run_id(),
        source.port_name(),
        wiring_link_id,
    );
    release_the_destination_side_of_a_link(
        graph,
        reader_input_port.node_run_id(),
        reader_input_port.port_name(),
        wiring_link_id,
    );
}

fn open_iceoryx2_service_read_at(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    read_at: &InputLinkPortRef,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let (from_port, to_port) = {
        let link =
            graph.traversal_mut().e(link_id).first().ok_or_else(|| {
                Error::LinkNotFound(format!("Link '{}' not found in graph", link_id))
            })?;
        (link.source().clone(), link.to_port().clone())
    };

    let destination = read_the_destination_of_a_link(graph, read_at, &to_port, link_id)?;
    let opened_source_side =
        open_the_source_side_of_a_link(graph, &from_port, link_id, &destination, iceoryx2_node)?;
    let wire_reply_awaited_from_the_destination =
        open_the_destination_side_of_a_link(graph, link_id, &opened_source_side, destination)?;
    opened_source_side.send_each_kept_message_to_a_reader_it_missed();

    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    opened_source_side
        .hold_its_services_on_the_link_and_stamp_it(link, wire_reply_awaited_from_the_destination);
    Ok(())
}

pub(crate) fn close_iceoryx2_service(graph: &mut Graph, link_id: &LinkUniqueId) -> Result<()> {
    tracing::info!("Closing iceoryx2 service: {}", link_id);

    let island_links_reading_it: Vec<LinkUniqueId> = graph
        .traversal()
        .e(link_id)
        .first()
        .and_then(|link| link.get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>())
        .map(|read_through| {
            read_through
                .reads_by_island_link
                .iter()
                .map(|read| read.island_link_id.clone())
                .collect()
        })
        .unwrap_or_default();
    if !island_links_reading_it.is_empty() {
        for island_link_id in &island_links_reading_it {
            close_a_read_of_a_link_by_the_reader_of_a_function_node_island(
                graph,
                link_id,
                island_link_id,
            )?;
        }
        if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
            link.remove::<OutOfProcessLinkWireRepliesComponent>();
            link.insert(LinkStateComponent(LinkState::Disconnected));
        }
        tracing::info!("Closed every read of {} (state: Disconnected)", link_id);
        return Ok(());
    }

    let Some((source_proc_id, source_port, dest_proc_id, dest_port)) =
        graph.traversal_mut().e(link_id).first().map(|link| {
            (
                link.source().node_run_id().clone(),
                link.source().port_name().to_string(),
                link.to_port().node_run_id().clone(),
                link.to_port().port_name().to_string(),
            )
        })
    else {
        tracing::warn!(
            "close_iceoryx2_service: link '{}' is not in the graph; nothing to reclaim",
            link_id
        );
        return Ok(());
    };

    // Source side: drop this link's destination notifier (and the channel
    // publisher when this was the source port's last outbound link).
    release_the_source_side_of_a_link(graph, &source_proc_id, &source_port, link_id);

    release_the_destination_side_of_a_link(graph, &dest_proc_id, &dest_port, link_id);

    if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
        link.remove::<Iceoryx2ServicesHeldOpenForLinkComponent>();
        // The answers go with the link: a refusal is rendered until the link is
        // disconnected, and this is that point.
        link.remove::<OutOfProcessLinkWireRepliesComponent>();
        link.insert(LinkStateComponent(LinkState::Disconnected));
    }
    tracing::info!("Closed iceoryx2 service: {} (state: Disconnected)", link_id);
    Ok(())
}

/// Whether a link still counts toward the ports it joins: one on its way out of
/// the graph, or one that failed, does not, or a disconnect followed by a
/// reconnect of the same port would be judged against itself.
fn link_still_counts_toward_its_ports(link: &Link) -> bool {
    link.get::<LinkStateComponent>()
        .map(|state| {
            !matches!(
                state.0,
                LinkState::Disconnecting | LinkState::Disconnected | LinkState::Error
            )
        })
        .unwrap_or(true)
}

fn out_of_process_link_wiring_of(
    graph: &Graph,
    proc_id: &NodeRunId,
) -> Option<Arc<OutOfProcessLinkWiringEnvelope>> {
    graph
        .traversal()
        .v(proc_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .map(|component| Arc::clone(&component.0))
}

fn node_to_reclaim_from(
    graph: &mut Graph,
    proc_id: &NodeRunId,
) -> Option<Arc<Mutex<NodeInstance>>> {
    get_single_node(graph, proc_id)
        .inspect_err(|error| {
            tracing::warn!(
                proc_id = %proc_id,
                error = %error,
                "close_iceoryx2_service: node missing; port not reclaimed"
            )
        })
        .ok()
}

fn get_single_node(graph: &mut Graph, proc_id: &NodeRunId) -> Result<Arc<Mutex<NodeInstance>>> {
    graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .and_then(|node| node.get::<NodeInstanceComponent>().map(|i| i.0.clone()))
        .ok_or_else(|| Error::Configuration(format!("Node '{}' not found", proc_id)))
}

fn publish_loss_counts_on_graph_node(graph: &mut Graph, proc_id: &NodeRunId, node: &NodeInstance) {
    let input_inner = node.iceoryx2_input_mailboxes_inner();
    insert_loss_counts_on_graph_node_once(
        graph,
        proc_id,
        NodeLossCounts::CountedByPortsInThisProcess(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: input_inner
                .as_ref()
                .map(|input_inner| input_inner.dropped_bag_counts_by_inbound_link())
                .unwrap_or_default(),
            discarded_sample_counts_by_inbound_link: input_inner
                .map(|input_inner| input_inner.discarded_sample_counts_by_inbound_link())
                .unwrap_or_default(),
            bag_counts_by_output_port: node
                .iceoryx2_output_writer_inner()
                .map(|output_inner| output_inner.bag_counts_by_output_port())
                .unwrap_or_default(),
        }),
    );
}

/// Insert `loss_counts` as `proc_id`'s node metrics, unless its first wired link
/// already did.
pub(crate) fn insert_loss_counts_on_graph_node_once(
    graph: &mut Graph,
    proc_id: &NodeRunId,
    loss_counts: NodeLossCounts,
) {
    let Some(node) = graph.traversal_mut().v(proc_id).first_mut() else {
        return;
    };
    if node.has::<NodeMetrics>() {
        return;
    }
    node.insert(NodeMetrics {
        loss_counts,
        ..Default::default()
    });
}

#[cfg(test)]
mod tests;
