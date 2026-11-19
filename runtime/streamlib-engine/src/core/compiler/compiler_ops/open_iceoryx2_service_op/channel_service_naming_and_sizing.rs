// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Naming a link's channel and notify services, and sizing them: subscribers, notifiers, ring depth and the windowed-port refusals.

use crate::core::NodeRunId;
use crate::core::compiler::compiler_ops::function_node_island::{
    link_in, runs_inside_the_read_of_each_node_it_feeds,
    the_links_an_island_link_reads_from_outside_any_island,
    the_reader_input_ports_a_function_node_feeds, the_readers_a_link_into_reaches,
};
use crate::core::compiler::compiler_ops::function_node_pool::needs_the_function_node_pool;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, Iceoryx2ServicesHeldOpenForLinkComponent, Link,
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkUniqueId, OutputLinkPortRef,
};
use crate::iceoryx2::{
    AudioWindowDeclarationOfAnInputPort, ChannelSizing, DeliveryProfile, DeliveryResolution,
    Iceoryx2Node, RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
    WINDOWED_PORT_SUBSCRIBER_RING_DEPTH, audio_windowing_declared_by_input_port,
    delivery_profile_for_input_port, input_port_borrow_quota,
};
use streamlib_ipc_types::{MAX_DESTINATIONS_PER_CHANNEL, MAX_INBOUND_LINKS_PER_DESTINATION};

use super::link_still_counts_toward_its_ports;

pub(super) fn channel_service_name(source: &OutputLinkPortRef) -> Result<String> {
    let node_run_id = source.node_run_id();
    let port_name = source.port_name();
    crate::iceoryx2::source_channel_name(node_run_id.as_str(), port_name)
        .map(|name| name.into_string())
        .map_err(|why| {
            Error::Configuration(format!(
                "cannot derive channel name for source '{node_run_id}:{port_name}': {why}"
            ))
        })
}

/// Destination-keyed notify (Event) service name — `streamlib/{dest}/notify`.
///
/// Every source publishing into one of a destination's channels holds a
/// `Notifier` here; the destination waits on ONE `Listener` fd, so fan-in never
/// multiplies the fds a runner multiplexes. Subprocess SDKs derive this name the
/// same way.
pub(super) fn notify_service_name_for(dest_proc_id: &NodeRunId) -> String {
    format!("streamlib/{}/notify", dest_proc_id)
}

/// Every `connect()` link carrying from `source` — the links of the one channel
/// that source publishes to, since a channel keys on its source.
fn every_link_carrying_from<'a>(
    graph: &'a Graph,
    source: &'a OutputLinkPortRef,
) -> impl Iterator<Item = &'a Link> + use<'a> {
    graph
        .traversal()
        .e(())
        .iter()
        .filter(move |link| link.source() == source)
}

/// How many destinations the channel `source` publishes to feeds: the
/// `connect()` links carrying from it, a link into a function node counted
/// once for each reader input port its island is read at.
fn channel_destination_count(graph: &Graph, source: &OutputLinkPortRef) -> usize {
    every_link_carrying_from(graph, source)
        .map(|link| {
            let destination_id = link.to_port().node_run_id();
            if runs_inside_the_read_of_each_node_it_feeds(graph, destination_id) {
                the_reader_input_ports_a_function_node_feeds(graph, destination_id)
                    .len()
                    .max(1)
            } else {
                1
            }
        })
        .sum()
}

/// The `max_subscribers` every channel data service is created with:
/// [`MAX_DESTINATIONS_PER_CHANNEL`] destination slots plus
/// [`RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL`] — fixed, never the current
/// fan-out, because iceoryx2 pins the count at create time and a link connected
/// to a running source must fit a slot that already exists.
///
/// A source output port past the cap is refused here by name, before any
/// service is touched.
pub(super) fn channel_max_subscribers(graph: &Graph, source: &OutputLinkPortRef) -> Result<usize> {
    refuse_an_output_port_past_its_destination_cap(graph, source)?;
    Ok(MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL)
}

/// Refuse the output port `source` when its channel would feed more destinations than a
/// channel carries.
fn refuse_an_output_port_past_its_destination_cap(
    graph: &Graph,
    source: &OutputLinkPortRef,
) -> Result<()> {
    let destinations = channel_destination_count(graph, source);
    if destinations > MAX_DESTINATIONS_PER_CHANNEL {
        return Err(Error::Configuration(format!(
            "output port '{source}' would feed {destinations} destinations, and a channel \
             carries at most {MAX_DESTINATIONS_PER_CHANNEL}"
        )));
    }
    Ok(())
}

/// Refuse `link`, which `graph` holds, when its wiring would refuse it for what a channel
/// carries: a second link into a port that windows, an output port past its destination cap,
/// or a destination past its inbound cap, the stream's function node pool among them, each
/// counted from the graph as the stream wires it.
pub(crate) fn refuse_a_link_a_channel_cannot_carry(graph: &Graph, link: &Link) -> Result<()> {
    let destination_id = link.to_port().node_run_id();
    let destination_port = link.to_port().port_name();
    if audio_windowing_declared_by_input_port_of(graph, destination_id, destination_port)?.is_some()
    {
        refuse_a_second_inbound_link_into_a_windowed_port(
            graph,
            destination_id,
            destination_port,
            &link.id,
        )?;
    }
    let an_island_is_read_through_it =
        runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id());
    if an_island_is_read_through_it {
        for read_link_id in the_links_an_island_link_reads_from_outside_any_island(graph, &link.id)
        {
            refuse_an_output_port_past_its_destination_cap(
                graph,
                link_in(graph, &read_link_id)?.source(),
            )?;
        }
    } else {
        refuse_an_output_port_past_its_destination_cap(graph, link.source())?;
    }
    for reader_id in the_readers_a_link_into_reaches(graph, destination_id) {
        refuse_a_destination_past_its_inbound_cap(
            &format!("node '{reader_id}'"),
            the_inbound_links_a_destination_holds_once_wired(graph, &reader_id),
        )?;
    }
    if an_island_is_read_through_it
        || runs_inside_the_read_of_each_node_it_feeds(graph, destination_id)
    {
        refuse_a_destination_past_its_inbound_cap(
            "the stream's function node pool",
            the_inbound_links_the_function_node_pool_holds_once_wired(graph),
        )?;
    }
    Ok(())
}

/// The inbound links `dest_proc_id` holds once the graph is wired: one for each link into it
/// through a channel, the stream's function node pool's among them, and one for each link from
/// outside any island that a link from an island it evaluates reads.
fn the_inbound_links_a_destination_holds_once_wired(
    graph: &Graph,
    dest_proc_id: &NodeRunId,
) -> usize {
    graph
        .traversal()
        .v(dest_proc_id)
        .in_e()
        .iter()
        .map(|link| {
            if runs_inside_the_read_of_each_node_it_feeds(graph, link.source().node_run_id())
                && !needs_the_function_node_pool(graph, &link.id)
            {
                the_links_an_island_link_reads_from_outside_any_island(graph, &link.id).len()
            } else {
                1
            }
        })
        .sum()
}

/// The inbound links the stream's function node pool holds once the graph is wired: one for
/// each link from outside any island that an island it carries reads.
fn the_inbound_links_the_function_node_pool_holds_once_wired(graph: &Graph) -> usize {
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| needs_the_function_node_pool(graph, &link.id))
        .map(|link| the_links_an_island_link_reads_from_outside_any_island(graph, &link.id).len())
        .sum()
}

/// Refuse `destination_named` when it would hold `inbound_links`, more than a destination
/// carries.
fn refuse_a_destination_past_its_inbound_cap(
    destination_named: &str,
    inbound_links: usize,
) -> Result<()> {
    if inbound_links > MAX_INBOUND_LINKS_PER_DESTINATION {
        return Err(Error::Configuration(format!(
            "{destination_named} would hold {inbound_links} inbound links, and a destination \
             carries at most {MAX_INBOUND_LINKS_PER_DESTINATION}"
        )));
    }
    Ok(())
}

/// Whether a channel created now makes room for a windowed reader the
/// source's graph cannot see when it sizes the channel: one a live graph
/// change may connect after the runtime's publisher opened the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoomForAWindowedReaderTheSourcesGraphCannotSee {
    Made,
    NotNeeded,
}

/// Derive the [`ChannelSizing`] for the channel keyed on `(source_proc_id,
/// source_port)`, refusing a source port past the destination cap.
pub(super) fn resolve_channel_sizing(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
    room_for_a_windowed_reader_the_sources_graph_cannot_see: RoomForAWindowedReaderTheSourcesGraphCannotSee,
) -> Result<ChannelSizing> {
    Ok(ChannelSizing {
        max_subscribers: channel_max_subscribers(graph, source)?,
        channel_service_creation_depth: channel_service_creation_depth(
            graph,
            iceoryx2_node,
            source,
            room_for_a_windowed_reader_the_sources_graph_cannot_see,
        )?,
    })
}

/// The depth the channel keyed on `(source_proc_id, source_port)` is created
/// at, or was.
///
/// A live channel keeps the depth it was created at, which is the only depth an
/// opener can ask for. A channel yet to be created is deep enough for a consumer
/// of any delivery profile, whichever connects first, and
/// [`WINDOWED_PORT_SUBSCRIBER_RING_DEPTH`] deep when any of its destinations
/// windows. Each subscriber then takes its own
/// port's ring inside it, and a shallower ring saves nothing — iceoryx2 sizes
/// the sample pool from the service's depth and subscriber count.
pub(super) fn channel_service_creation_depth(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
    room_for_a_windowed_reader_the_sources_graph_cannot_see: RoomForAWindowedReaderTheSourcesGraphCannotSee,
) -> Result<usize> {
    if let Some(live_creation_depth) =
        creation_depth_of_the_live_channel(graph, iceoryx2_node, source)?
    {
        return Ok(live_creation_depth);
    }
    if room_for_a_windowed_reader_the_sources_graph_cannot_see
        == RoomForAWindowedReaderTheSourcesGraphCannotSee::Made
    {
        return Ok(WINDOWED_PORT_SUBSCRIBER_RING_DEPTH);
    }
    for link in every_link_carrying_from(graph, source)
        .filter(|link| link_still_counts_toward_its_ports(link))
    {
        let destination = link.to_port();
        let destination_node_run_id = destination.node_run_id();
        if audio_windowing_declared_by_input_port_of(
            graph,
            destination_node_run_id,
            destination.port_name(),
        )?
        .is_some()
        {
            return Ok(WINDOWED_PORT_SUBSCRIBER_RING_DEPTH);
        }
    }
    Ok(DeliveryProfile::ORDERED_DEPTH)
}

/// The depth the live channel keyed on `(source_proc_id, source_port)` was
/// created at, or `None` while no channel exists.
///
/// Read off the service a link out of that port holds open, and otherwise off
/// whatever still holds the service with no link behind it — the runtime's publisher, or a helper
/// that has not yet released its port.
fn creation_depth_of_the_live_channel(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
) -> Result<Option<usize>> {
    let held_open_by_a_link = every_link_carrying_from(graph, source).find_map(|link| {
        link.get::<Iceoryx2ServicesHeldOpenForLinkComponent>()
            .or_else(|| {
                link.get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
                    .and_then(|read_through| read_through.reads_by_island_link.first())
                    .map(|read| &read.services_held_open)
            })
            .map(|held| held.channel_data_service.channel_service_creation_depth())
    });
    if held_open_by_a_link.is_some() {
        return Ok(held_open_by_a_link);
    }
    Ok(iceoryx2_node
        .open_existing_channel_service(&channel_service_name(source)?)?
        .map(|held_elsewhere| held_elsewhere.channel_service_creation_depth()))
}

/// The ring a destination input port's subscriber takes: the windowed ring for a
/// port that declares a window contract, and its delivery profile's depth
/// otherwise.
pub(super) fn subscriber_ring_depth_of_input_port(
    dest_input_port_delivery: DeliveryResolution,
    audio_windowing: Option<&AudioWindowDeclarationOfAnInputPort>,
) -> usize {
    match audio_windowing {
        Some(_) => WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
        None => dest_input_port_delivery.depth,
    }
}

/// The `max_notifiers` every destination-keyed notify service is created with:
/// [`MAX_INBOUND_LINKS_PER_DESTINATION`], fixed for the same reason the
/// channel's subscriber count is — the service is created with the first
/// inbound link and a later one must fit a notifier slot that already exists.
///
/// A destination past the cap is refused here by name, counted as an edit
/// counts it.
pub(super) fn destination_max_notifiers(graph: &Graph, dest_proc_id: &NodeRunId) -> Result<usize> {
    refuse_a_destination_past_its_inbound_cap(
        &format!("node '{dest_proc_id}'"),
        the_inbound_links_a_destination_holds_once_wired(graph, dest_proc_id),
    )?;
    Ok(MAX_INBOUND_LINKS_PER_DESTINATION)
}

/// The drain order, ring depth and borrow quota of one destination input port,
/// from the delivery profile and borrow quota that port declares.
///
/// Resolved per destination port, never per channel: consumers of one output
/// port read it under whatever profiles and quotas they each declare.
pub(super) fn delivery_resolution_of_input_port(
    graph: &mut Graph,
    dest_proc_id: &NodeRunId,
    dest_port: &str,
) -> Result<DeliveryResolution> {
    let dest_type = node_class_import_path_of(graph, dest_proc_id)?;
    let node_types = graph.node_types_this_stream_resolves();
    let declared_borrow_quota = node_types.port_info(&dest_type).and_then(|ports| {
        ports
            .input_named(dest_port)
            .and_then(|port| port.borrow_quota)
    });
    Ok(DeliveryResolution {
        borrow_quota: input_port_borrow_quota(dest_port, declared_borrow_quota)?,
        ..delivery_profile_for_input_port(node_types, &dest_type, dest_port)?.resolve()
    })
}

pub(super) fn node_class_import_path_of(
    graph: &Graph,
    proc_id: &NodeRunId,
) -> Result<NodeClassImportPath> {
    graph
        .traversal()
        .v(proc_id)
        .first()
        .map(|node| node.node_type().clone())
        .ok_or_else(|| Error::NodeNotFound(format!("Node '{proc_id}' not found")))
}

/// The window declaration this destination's input port carries, if it carries
/// one.
///
/// Reads the destination's registered class, the same way the delivery profile
/// is read: the declaration is the whole answer and nothing is inferred.
pub(super) fn audio_windowing_declared_by_input_port_of(
    graph: &Graph,
    dest_proc_id: &NodeRunId,
    dest_port: &str,
) -> Result<Option<AudioWindowDeclarationOfAnInputPort>> {
    let Ok(dest_type) = node_class_import_path_of(graph, dest_proc_id) else {
        return Ok(None);
    };
    audio_windowing_declared_by_input_port(
        graph.node_types_this_stream_resolves(),
        &dest_type,
        dest_port,
    )
}

/// Refuse a second inbound link into a port that windows, naming the port and
/// both links.
///
/// Fan-in legally interleaves N producers' blocks in one mailbox today, and two
/// sample streams interleaved into one accumulator is plausible-looking wrong
/// audio — the worst outcome available to a contract whose whole promise is
/// that a window is exact.
pub(super) fn refuse_a_second_inbound_link_into_a_windowed_port(
    graph: &Graph,
    dest_proc_id: &NodeRunId,
    dest_port: &str,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let already_inbound = graph
        .traversal()
        .v(dest_proc_id)
        .in_e()
        .iter()
        .filter(|link| link.to_port().port_name() == dest_port)
        .filter(|link| link_still_counts_toward_its_ports(link))
        .map(|link| link.id.to_string())
        .find(|inbound| inbound != link_id.as_str());
    let Some(first) = already_inbound else {
        return Ok(());
    };

    Err(Error::Configuration(format!(
        "input port '{dest_port}' on '{dest_proc_id}' declares an `audio_window` contract \
         and already has inbound link '{first}'; link '{link_id}' would make a second. A \
         windowed port accepts exactly one inbound link — two sample streams interleaved \
         into one accumulator is not a mix, it is garbage windows. Fan the producers into \
         separate windowed ports, or drop the contract from this one."
    )))
}

/// Refuse a windowed port wired onto a channel created shallower than the ring
/// it reads through, naming the port, the link, both depths and the fix.
///
/// A channel's depth is fixed for its life, and a subscriber cannot take a ring
/// deeper than it.
pub(super) fn refuse_a_windowed_port_onto_a_channel_created_shallower_than_its_ring(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
    dest_proc_id: &NodeRunId,
    dest_port: &str,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let Some(live_creation_depth) =
        creation_depth_of_the_live_channel(graph, iceoryx2_node, source)?
    else {
        return Ok(());
    };
    if live_creation_depth >= WINDOWED_PORT_SUBSCRIBER_RING_DEPTH {
        return Ok(());
    }
    Err(Error::Configuration(format!(
        "input port '{dest_port}' on '{dest_proc_id}' declares an `audio_window` contract, \
         and link '{link_id}' would wire it onto the running channel of \
         '{source}', created {live_creation_depth} bags deep. A windowed \
         port reads through a {WINDOWED_PORT_SUBSCRIBER_RING_DEPTH}-bag ring, and a channel \
         keeps its depth for as long as anything holds it open. Connect the windowed consumer \
         before the channel's other links, so the channel is created \
         {WINDOWED_PORT_SUBSCRIBER_RING_DEPTH} bags deep."
    )))
}
