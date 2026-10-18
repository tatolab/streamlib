// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 service operations for the compiler.
//!
//! Opens the channel-centric iceoryx2 publish-subscribe services between
//! processor ports. A channel is keyed on its **source output port**
//! (`{source_processor}/{source_output_port}`), so one source output port maps
//! to exactly one iceoryx2 data service: ONE publisher fans a single zero-copy
//! loan out to its N compile-time-known subscribers (one per `connect()` link),
//! plus one reserved slot for a phase-3.5 tap. The paired Event (notify) service
//! stays destination-keyed (`streamlib/{dest}/notify`) so a destination waits on
//! ONE listener fd regardless of fan-in.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::ProcessorUniqueId;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    DeviceMatchedAudioWindowContractsComponent, ExposedOutputPortsComponent, Graph,
    GraphEdgeWithComponents, GraphNodeWithComponents, Iceoryx2ServicesHeldOpenForLinkComponent,
    InputLinkPortRef, Link, LinkReadingAnotherStreamComponent, LinkState, LinkStateComponent,
    LinkUniqueId, LossCountsOfPortsInThisProcess, OutOfProcessLinkWireRepliesComponent,
    OutOfProcessLinkWiringComponent, OutputLinkPortRef, ProcessorInstanceComponent,
    ProcessorLossCounts, ProcessorMetrics, SourceSideOfALinkWiredOn,
};
use crate::core::output_port_catalog_entries::CatalogEntriesOfOutputPorts;
use crate::core::processors::{
    OutOfProcessLinkWireReply, OutOfProcessLinkWiringEnvelope, ProcessorInstance,
};
use crate::iceoryx2::{
    AudioWindowDeclarationOfAnInputPort, ChannelEgressConfig, ChannelSizing, ChannelTrustTier,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DeliveryProfile, DeliveryResolution, Iceoryx2Node,
    Iceoryx2NotifyService, Iceoryx2Service, InboundLinkName,
    RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL, WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
    audio_windowing_declared_by_input_port, delivery_profile_for_input_port,
    effective_channel_chunk_ceiling_bytes, refuse_an_unsettled_match_device_sentinel,
};
use streamlib_ipc_types::{MAX_DESTINATIONS_PER_CHANNEL, MAX_INBOUND_LINKS_PER_DESTINATION};

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
pub fn open_iceoryx2_service(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let (from_port, to_port) = {
        let link =
            graph.traversal_mut().e(link_id).first().ok_or_else(|| {
                Error::LinkNotFound(format!("Link '{}' not found in graph", link_id))
            })?;
        let from_port = link.source_in_this_stream().cloned().ok_or_else(|| {
            Error::InvalidLink(format!(
                "link '{link_id}' reads `{}` in another stream, whose output port this stream's \
                 graph does not hold",
                link.source()
            ))
        })?;
        (from_port, link.to_port().clone())
    };

    let destination = read_the_destination_of_a_link(graph, &to_port, link_id)?;
    let opened_source_side =
        open_the_source_side_of_a_link(graph, &from_port, link_id, &destination, iceoryx2_node)?;
    let wire_reply_awaited_from_the_destination =
        open_the_destination_side_of_a_link(graph, link_id, &opened_source_side, destination)?;

    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    opened_source_side
        .hold_its_services_on_the_link_and_stamp_it(link, wire_reply_awaited_from_the_destination);
    Ok(())
}

/// What the wiring of a link reads off its destination's graph: the input
/// port, how that port reads, whether its ports live out of process, and the
/// destination's notify service.
pub(crate) struct DestinationOfALinkAsItsWiringReadsIt {
    dest_proc_id: ProcessorUniqueId,
    dest_port: String,
    link_wiring_of_a_destination_out_of_process: Option<Arc<OutOfProcessLinkWiringEnvelope>>,
    audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
    input_port_delivery: DeliveryResolution,
    notify_service_name: String,
    max_notifiers: usize,
}

/// Read what wiring link `link_id` into `to_port` needs of its destination,
/// off the destination's graph, refusing a second inbound link into a port
/// that windows and a destination past its notifier cap.
///
/// A `match_device` sentinel is not refused here: the compiler wires every
/// link before it releases any processor into `setup()`, which is where the
/// only format that can settle one comes from.
pub(crate) fn read_the_destination_of_a_link(
    dest_graph: &mut Graph,
    to_port: &InputLinkPortRef,
    link_id: &LinkUniqueId,
) -> Result<DestinationOfALinkAsItsWiringReadsIt> {
    let dest_proc_id = to_port.processor_id().clone();
    let dest_port = to_port.port_name().to_string();
    let link_wiring_of_a_destination_out_of_process =
        out_of_process_link_wiring_of(dest_graph, &dest_proc_id);
    let audio_windowing =
        audio_windowing_declared_by_input_port_of(dest_graph, &dest_proc_id, &dest_port)?;
    if audio_windowing.is_some() {
        refuse_a_second_inbound_link_into_a_windowed_port(
            dest_graph,
            &dest_proc_id,
            &dest_port,
            link_id,
        )?;
        tracing::debug!(
            dest = %dest_proc_id,
            port = %dest_port,
            windowing = ?audio_windowing,
            "Destination input port declares a window contract; its reads run through the stage"
        );
    }
    let input_port_delivery =
        delivery_resolution_of_input_port(dest_graph, &dest_proc_id, &dest_port)?;
    let max_notifiers = destination_max_notifiers(dest_graph, &dest_proc_id)?;
    // Every destination is notified whatever mode it runs in. One that never
    // drains its listener costs its sources a counter increment per frame:
    // iceoryx2 counts repeat notifications in shared memory and sends no
    // further wakeup until the listener drains.
    let notify_service_name = notify_service_name_for(&dest_proc_id);
    Ok(DestinationOfALinkAsItsWiringReadsIt {
        dest_proc_id,
        dest_port,
        link_wiring_of_a_destination_out_of_process,
        audio_windowing,
        input_port_delivery,
        notify_service_name,
        max_notifiers,
    })
}

/// The source's side of one link once opened: the services the link holds,
/// and where the source's end was wired.
pub(crate) struct OpenedSourceSideOfALink {
    channel_service_name: String,
    channel_sizing: ChannelSizing,
    channel_data_service: Iceoryx2Service,
    destination_notify_service: Iceoryx2NotifyService,
    wire_reply_awaited_from_the_source: Option<Arc<OutOfProcessLinkWireReply>>,
    wired_on: Option<SourceSideOfALinkWiredOn>,
    source_output_port: String,
    catalog_entries_of_the_sources_output_ports: Arc<CatalogEntriesOfOutputPorts>,
}

/// How a link stands once both its sides are opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HowAnOpenedLinkStands {
    /// Wholly wired: nothing out of process has to answer for it.
    Wired,
    /// Handed to an out-of-process end that has not answered yet.
    PendingItsOutOfProcessEndsAnswers,
}

impl OpenedSourceSideOfALink {
    /// Where the source's end of the link was wired, when the engine holds
    /// it.
    pub(crate) fn wired_on(&self) -> Option<&SourceSideOfALinkWiredOn> {
        self.wired_on.as_ref()
    }

    /// Hold the link's services on `link` and stamp it `Wired`, or `Pending`
    /// with the cells its out-of-process ends answer into — the source's, then
    /// the destination's.
    pub(crate) fn hold_its_services_on_the_link_and_stamp_it(
        self,
        link: &mut Link,
        wire_reply_awaited_from_the_destination: Option<Arc<OutOfProcessLinkWireReply>>,
    ) -> HowAnOpenedLinkStands {
        let link_id = link.id.clone();
        link.insert_component_without_rendering_it(Iceoryx2ServicesHeldOpenForLinkComponent {
            channel_data_service: self.channel_data_service,
            destination_notify_service: self.destination_notify_service,
        });

        // Every out-of-process end this link was handed to and has not answered
        // for. Empty is a link wholly in the app process, or one carried in a far
        // side's startup envelope — either way wired the moment this returns.
        let wire_replies_awaited_from_its_out_of_process_ends: Vec<_> = self
            .wire_reply_awaited_from_the_source
            .into_iter()
            .chain(wire_reply_awaited_from_the_destination)
            .collect();

        // A link an out-of-process end has not answered for is `Pending`, not
        // `Wired`: the engine has sent the wiring, and only that end opening its
        // own port makes the link carry anything. `graph` reads the answers off
        // the component below, which the end fills from its bridge's reader
        // thread. `docs/plan/ARCHITECTURE.md` §Processor model, the
        // `[local-transport-hardening]` entry.
        if wire_replies_awaited_from_its_out_of_process_ends.is_empty() {
            link.insert(LinkStateComponent(LinkState::Wired));
            tracing::info!(
                channel = %self.channel_service_name,
                "Opened iceoryx2 channel: [{}] (state: Wired)",
                link_id
            );
            HowAnOpenedLinkStands::Wired
        } else {
            link.insert(LinkStateComponent(LinkState::Pending));
            link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(
                wire_replies_awaited_from_its_out_of_process_ends,
            ));
            tracing::info!(
                channel = %self.channel_service_name,
                "Opened iceoryx2 channel: [{}] (state: Pending, awaiting its helper's answer)",
                link_id
            );
            HowAnOpenedLinkStands::PendingItsOutOfProcessEndsAnswers
        }
    }
}

/// Open the source's side of link `link_id` from output port `source` of
/// `source_graph` into `destination`: the channel the source publishes to and
/// the destination's notify service, then the source's single publisher (the
/// first link out of the port installs it) and this link's notifier on it.
///
/// A windowed destination onto a live channel shallower than its ring, and a
/// port past its destination cap — counting readers registered from other
/// streams — are refused before any service is opened.
pub(crate) fn open_the_source_side_of_a_link(
    source_graph: &mut Graph,
    source: &OutputLinkPortRef,
    link_id: &LinkUniqueId,
    destination: &DestinationOfALinkAsItsWiringReadsIt,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<OpenedSourceSideOfALink> {
    let source_proc_id = source.processor_id().clone();
    let source_port = source.port_name();
    let channel_service_name = channel_service_name(source)?;
    let link_wiring_of_a_source_out_of_process =
        out_of_process_link_wiring_of(source_graph, &source_proc_id);
    let catalog_entries_of_the_sources_output_ports = source_graph
        .traversal()
        .v(&source_proc_id)
        .first()
        .map(|source_node| Arc::clone(source_node.catalog_entries_of_output_ports()))
        .ok_or_else(|| {
            Error::ProcessorNotFound(format!("source processor '{source_proc_id}' not found"))
        })?;
    let destination_windows_its_reads = destination.audio_windowing.is_some();
    if destination_windows_its_reads {
        refuse_a_windowed_port_onto_a_channel_created_shallower_than_its_ring(
            source_graph,
            iceoryx2_node,
            source,
            &destination.dest_proc_id,
            &destination.dest_port,
            link_id,
        )?;
    }

    tracing::info!(
        channel = %channel_service_name,
        notify = %destination.notify_service_name,
        "Opening iceoryx2 channel: {} -> ({}:{}) [{}] (source_subprocess={}, dest_subprocess={})",
        source,
        destination.dest_proc_id,
        destination.dest_port,
        link_id,
        link_wiring_of_a_source_out_of_process.is_some(),
        destination
            .link_wiring_of_a_destination_out_of_process
            .is_some(),
    );

    // A channel touching a subprocess on either end crosses a trust boundary and
    // gets the tighter untrusted-session ceiling; a host-to-host channel is
    // trusted. The ceiling is the graceful, observable layer in front of the
    // subprocess cgroup `memory.max` hard backstop.
    let trust_tier = if link_wiring_of_a_source_out_of_process.is_some()
        || destination
            .link_wiring_of_a_destination_out_of_process
            .is_some()
    {
        ChannelTrustTier::UntrustedSession
    } else {
        ChannelTrustTier::Trusted
    };
    // The tier default is the structural ceiling; an operator raises or lowers it
    // per deployment through the tier's node-level env override.
    let channel_ceiling_bytes = effective_channel_chunk_ceiling_bytes(trust_tier);
    let windowed_destination_outside_the_sources_graph = if destination_windows_its_reads
        && !source_graph
            .traversal()
            .v(&destination.dest_proc_id)
            .exists()
    {
        WindowedDestinationOutsideTheSourcesGraph::Present
    } else {
        WindowedDestinationOutsideTheSourcesGraph::Absent
    };
    let channel_sizing = resolve_channel_sizing(
        source_graph,
        iceoryx2_node,
        source,
        windowed_destination_outside_the_sources_graph,
    )?;

    let channel_data_service = iceoryx2_node.open_or_create_service(
        &channel_service_name,
        channel_sizing.max_subscribers,
        channel_sizing.channel_service_creation_depth,
    )?;
    let destination_notify_service = iceoryx2_node.open_or_create_notify_service(
        &destination.notify_service_name,
        destination.max_notifiers,
    )?;

    let (wire_reply_awaited_from_the_source, wired_on) =
        if let Some(source_link_wiring) = link_wiring_of_a_source_out_of_process {
            let wire_reply = wire_subprocess_source(
                source_graph,
                &source_link_wiring,
                &source_proc_id,
                source_port,
                &channel_service_name,
                &destination.notify_service_name,
                DEFAULT_EXPECTED_PAYLOAD_BYTES,
                channel_ceiling_bytes,
                channel_sizing,
                destination.max_notifiers,
                link_id,
            )?;
            (
                wire_reply,
                Some(SourceSideOfALinkWiredOn::AHelperProcessesLinkWiring(
                    source_link_wiring,
                )),
            )
        } else {
            let source_processor = get_single_processor(source_graph, &source_proc_id)?;
            let output_writer = wire_rust_source(
                source_graph,
                &source_proc_id,
                &source_processor,
                source_port,
                link_id,
                &channel_data_service,
                &destination_notify_service,
                ChannelEgressConfig {
                    service_name: channel_service_name.clone(),
                    trust_tier,
                    expected_payload_bytes: DEFAULT_EXPECTED_PAYLOAD_BYTES,
                    chunk_ceiling_bytes: channel_ceiling_bytes,
                },
            )?;
            (
                None,
                output_writer.map(SourceSideOfALinkWiredOn::ANativeProcessorsOutputWriter),
            )
        };

    Ok(OpenedSourceSideOfALink {
        channel_service_name,
        channel_sizing,
        channel_data_service,
        destination_notify_service,
        wire_reply_awaited_from_the_source,
        wired_on,
        source_output_port: source_port.to_string(),
        catalog_entries_of_the_sources_output_ports,
    })
}

/// Open the destination's side of link `link_id` on `dest_graph`: subscribe
/// the destination's input port to the channel `opened_source_side` opened,
/// and make sure its single listener exists. Hands back the cell a destination
/// out of process answers into.
pub(crate) fn open_the_destination_side_of_a_link(
    dest_graph: &mut Graph,
    link_id: &LinkUniqueId,
    opened_source_side: &OpenedSourceSideOfALink,
    destination: DestinationOfALinkAsItsWiringReadsIt,
) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
    let dest_proc_id = destination.dest_proc_id.clone();
    let dest_port = destination.dest_port.clone();
    let wire_reply_awaited_from_the_destination =
        if let Some(dest_link_wiring) = &destination.link_wiring_of_a_destination_out_of_process {
            wire_subprocess_dest(
                dest_graph,
                dest_link_wiring,
                &destination.dest_proc_id,
                &destination.dest_port,
                &opened_source_side.channel_service_name,
                &destination.notify_service_name,
                destination.input_port_delivery,
                opened_source_side.channel_sizing,
                destination.max_notifiers,
                link_id,
                destination.audio_windowing,
            )?
        } else {
            let dest_processor = get_single_processor(dest_graph, &destination.dest_proc_id)?;
            wire_rust_dest(
                dest_graph,
                &destination.dest_proc_id,
                &dest_processor,
                &destination.dest_port,
                link_id,
                &opened_source_side.channel_service_name,
                destination.input_port_delivery,
                &opened_source_side.channel_data_service,
                &opened_source_side.destination_notify_service,
                destination.audio_windowing,
            )?;
            None
        };
    dest_graph
        .traversal()
        .v(&dest_proc_id)
        .first()
        .ok_or_else(|| {
            Error::ProcessorNotFound(format!("destination processor '{dest_proc_id}' not found"))
        })?
        .upstream_catalog_entries_by_inbound_link()
        .record_inbound_link(
            &dest_port,
            link_id,
            InboundLinkName::from(opened_source_side.channel_service_name.as_str()),
            &opened_source_side.source_output_port,
            Arc::clone(&opened_source_side.catalog_entries_of_the_sources_output_ports),
        );
    Ok(wire_reply_awaited_from_the_destination)
}

/// Reclaim one `connect()` link's iceoryx2 ports on `disconnect`.
///
/// Stamping [`LinkState::Disconnected`] is not enough: the source-side notifier
/// and dest-side subscriber (plus listener, orphaned mailbox, channel publisher)
/// must be dropped, and the services the link held released, else a reconnect
/// re-appends past the notify service's create-time `max_notifiers` cap
/// (`ExceedsMaxSupportedNotifiers`).
///
/// An endpoint whose ports live out of process owns them itself, so its half is
/// reclaimed through its wiring envelope — which forgets the entry a reconnect
/// would otherwise be set up with twice and asks a running far side to drop its
/// port — without taking that processor's lock.
///
/// A link reading another stream releases the source's side through the
/// wiring it holds, never through the source stream's graph.
#[tracing::instrument(
    name = "compiler.close_iceoryx2_service",
    skip(graph),
    fields(link_id = %link_id)
)]
pub fn close_iceoryx2_service(graph: &mut Graph, link_id: &LinkUniqueId) -> Result<()> {
    tracing::info!("Closing iceoryx2 service: {}", link_id);

    let Some((source_in_this_stream, dest_proc_id, dest_port)) =
        graph.traversal_mut().e(link_id).first().map(|link| {
            (
                link.source_in_this_stream().map(|source| {
                    (
                        source.processor_id().clone(),
                        source.port_name().to_string(),
                    )
                }),
                link.to_port().processor_id().clone(),
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
    match &source_in_this_stream {
        Some((source_proc_id, source_port)) => {
            release_the_source_side_of_a_link_in_this_stream(
                graph,
                source_proc_id,
                source_port,
                link_id,
            );
        }
        None => {
            let wiring_of_a_link_reading_another_stream = graph
                .traversal_mut()
                .e(link_id)
                .first_mut()
                .and_then(|link| link.get_mut::<LinkReadingAnotherStreamComponent>())
                .and_then(LinkReadingAnotherStreamComponent::take_wiring);
            drop(wiring_of_a_link_reading_another_stream);
        }
    }

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

/// Drop link `link_id`'s notifier off output port `source_port` of
/// `source_proc_id` in this graph, and the port's publisher when the link was
/// its last.
fn release_the_source_side_of_a_link_in_this_stream(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
    link_id: &LinkUniqueId,
) {
    if let Some(source_link_wiring) = out_of_process_link_wiring_of(graph, source_proc_id) {
        source_link_wiring.unwire_a_link_reporting_a_far_side_that_kept_its_port(
            crate::core::PortDirection::Output,
            source_port,
            link_id.as_str(),
        );
    } else if let Some(source_processor) = processor_to_reclaim_from(graph, source_proc_id) {
        let source_guard = source_processor.lock();
        if let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() {
            let channel_released = output_inner.remove_channel_link(source_port, link_id.as_str());
            tracing::debug!(
                source = %source_proc_id,
                port = %source_port,
                channel_released,
                "Reclaimed source-side egress for disconnected link"
            );
        }
    }
}

/// Drop link `link_id`'s channel subscriber off input port `dest_port` of
/// `dest_proc_id` (and the port mailbox / shared listener when their last
/// inbound link went away).
pub(crate) fn release_the_destination_side_of_a_link(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
    link_id: &LinkUniqueId,
) {
    if let Some(dest_node) = graph.traversal().v(dest_proc_id).first() {
        dest_node
            .upstream_catalog_entries_by_inbound_link()
            .forget_inbound_link(link_id);
    }
    if let Some(dest_link_wiring) = out_of_process_link_wiring_of(graph, dest_proc_id) {
        dest_link_wiring.unwire_a_link_reporting_a_far_side_that_kept_its_port(
            crate::core::PortDirection::Input,
            dest_port,
            link_id.as_str(),
        );
    } else if let Some(dest_processor) = processor_to_reclaim_from(graph, dest_proc_id) {
        let dest_guard = dest_processor.lock();
        if let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() {
            input_inner.remove_channel_link(link_id.as_str());
            tracing::debug!(
                dest = %dest_proc_id,
                "Reclaimed destination-side ports for disconnected link"
            );
        }
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// The channel service name a link's bags ride on:
/// `{source_processor}/{source_output_port}`
/// ([`crate::iceoryx2::source_channel_name`], the single source of truth for
/// channel identity). One source, one channel, one publisher, N subscribers. A
/// grammar-illegal port name surfaces as a named [`Error::Configuration`] here
/// rather than an opaque iceoryx2 `Invalid service name` deep in the FFI.
pub(crate) fn channel_service_name(source: &OutputLinkPortRef) -> Result<String> {
    let processor_id = source.processor_id();
    let port_name = source.port_name();
    crate::iceoryx2::source_channel_name(processor_id.as_str(), port_name)
        .map(|name| name.into_string())
        .map_err(|why| {
            Error::Configuration(format!(
                "cannot derive channel name for source '{processor_id}:{port_name}': {why}"
            ))
        })
}

/// Destination-keyed notify (Event) service name — `streamlib/{dest}/notify`.
///
/// Every source publishing into one of a destination's channels holds a
/// `Notifier` here; the destination waits on ONE `Listener` fd, so fan-in never
/// multiplies the fds a runner multiplexes. Subprocess SDKs derive this name the
/// same way.
fn notify_service_name_for(dest_proc_id: &ProcessorUniqueId) -> String {
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
        .filter(move |link| link.source_in_this_stream() == Some(source))
}

/// How many destinations the channel `source` publishes to feeds: the
/// `connect()` links carrying from it in this graph, and the readers from other
/// streams registered against it.
fn channel_destination_count(graph: &Graph, source: &OutputLinkPortRef) -> usize {
    let readers_from_other_streams = graph
        .traversal()
        .v(source.processor_id())
        .first()
        .and_then(|node| node.get::<ExposedOutputPortsComponent>())
        .map_or(0, |exposed_ports| {
            exposed_ports.readers_registered_against(source.port_name())
        });
    every_link_carrying_from(graph, source).count() + readers_from_other_streams
}

/// The `max_subscribers` every channel data service is created with:
/// [`MAX_DESTINATIONS_PER_CHANNEL`] destination slots plus
/// [`RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL`] — fixed, never the current
/// fan-out, because iceoryx2 pins the count at create time and a link connected
/// to a running source must fit a slot that already exists.
///
/// A source output port past the cap is refused here by name, before any
/// service is touched.
fn channel_max_subscribers(graph: &Graph, source: &OutputLinkPortRef) -> Result<usize> {
    let destinations = channel_destination_count(graph, source);
    if destinations > MAX_DESTINATIONS_PER_CHANNEL {
        return Err(Error::Configuration(format!(
            "output port '{source}' would feed {destinations} destinations, and a channel \
             carries at most {MAX_DESTINATIONS_PER_CHANNEL}"
        )));
    }
    Ok(MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL)
}

/// Whether a destination being wired from a source, held in a graph other
/// than the source's, windows its reads — a reader the source's graph cannot
/// see when it sizes the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowedDestinationOutsideTheSourcesGraph {
    Present,
    Absent,
}

/// Derive the [`ChannelSizing`] for the channel keyed on `(source_proc_id,
/// source_port)` — the single derivation both the service-open compiler op and
/// the `tap` op share, refusing a source port past the destination cap.
pub(crate) fn resolve_channel_sizing(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
    windowed_destination_outside_the_sources_graph: WindowedDestinationOutsideTheSourcesGraph,
) -> Result<ChannelSizing> {
    Ok(ChannelSizing {
        max_subscribers: channel_max_subscribers(graph, source)?,
        channel_service_creation_depth: channel_service_creation_depth(
            graph,
            iceoryx2_node,
            source,
            windowed_destination_outside_the_sources_graph,
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
/// windows, in this graph or outside it. Each subscriber then takes its own
/// port's ring inside it, and a shallower ring saves nothing — iceoryx2 sizes
/// the sample pool from the service's depth and subscriber count.
fn channel_service_creation_depth(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
    windowed_destination_outside_the_sources_graph: WindowedDestinationOutsideTheSourcesGraph,
) -> Result<usize> {
    if let Some(live_creation_depth) =
        creation_depth_of_the_live_channel(graph, iceoryx2_node, source)?
    {
        return Ok(live_creation_depth);
    }
    if windowed_destination_outside_the_sources_graph
        == WindowedDestinationOutsideTheSourcesGraph::Present
    {
        return Ok(WINDOWED_PORT_SUBSCRIBER_RING_DEPTH);
    }
    for link in every_link_carrying_from(graph, source)
        .filter(|link| link_still_counts_toward_its_ports(link))
    {
        let destination = link.to_port();
        let destination_processor_id = destination.processor_id();
        if audio_windowing_declared_by_input_port_of(
            graph,
            destination_processor_id,
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
/// whatever still holds the service with no link behind it — a tap, or a helper
/// that has not yet released its port.
fn creation_depth_of_the_live_channel(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
) -> Result<Option<usize>> {
    let held_open_by_a_link = every_link_carrying_from(graph, source).find_map(|link| {
        link.get::<Iceoryx2ServicesHeldOpenForLinkComponent>()
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
fn subscriber_ring_depth_of_input_port(
    dest_input_port_delivery: DeliveryResolution,
    audio_windowing: Option<&AudioWindowDeclarationOfAnInputPort>,
) -> usize {
    match audio_windowing {
        Some(_) => WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
        None => dest_input_port_delivery.depth,
    }
}

/// Resolve the address a caller names a port by — `[<machine>/][<stream>/]<node>/<port>`
/// — to the port in the stream `this_stream` on `this_machine` that a link
/// carries from.
///
/// An address that does not parse is refused as [`Error::InvalidPortAddress`];
/// one naming another machine as [`Error::PortAddressNamesAnotherMachine`];
/// one naming another stream as [`Error::InvalidPortAddress`] naming both, since
/// a stream taps its own ports. A channel's iceoryx2 data service only exists
/// once a link has wired its source, so an address that neither a link of this
/// stream nor a reader from another stream reads is genuinely untappable and
/// answers [`Error::TapChannelNotFound`].
pub(crate) fn find_the_source_a_caller_named(
    graph: &Graph,
    this_machine: &crate::core::runtime::MachineName,
    this_stream: &str,
    port_address: &str,
) -> Result<OutputLinkPortRef> {
    let not_found = || Error::TapChannelNotFound(port_address.to_string());
    let address = crate::core::graph::PortAddress::parse(port_address)?;
    address.refuse_when_it_names_another_machine(this_machine)?;
    if let Some(stream) = address.the_stream_it_names_other_than(this_stream) {
        return Err(Error::InvalidPortAddress(format!(
            "`{port_address}` names a port in the stream `{stream}`, and this tap reads the \
             stream `{this_stream}`; tap `{stream}` to read its ports"
        )));
    }
    let processor_id = graph
        .traversal()
        .v_with_node_name(address.node_name())
        .first()
        .ok_or_else(not_found)?
        .id
        .clone();
    let source = OutputLinkPortRef::new(processor_id, address.port_name());
    if channel_destination_count(graph, &source) > 0 {
        Ok(source)
    } else {
        Err(not_found())
    }
}

/// The `max_notifiers` every destination-keyed notify service is created with:
/// [`MAX_INBOUND_LINKS_PER_DESTINATION`], fixed for the same reason the
/// channel's subscriber count is — the service is created with the first
/// inbound link and a later one must fit a notifier slot that already exists.
///
/// A destination past the cap is refused here by name.
fn destination_max_notifiers(graph: &mut Graph, dest_proc_id: &ProcessorUniqueId) -> Result<usize> {
    let inbound_links = graph.traversal_mut().v(dest_proc_id).in_e().iter().count();
    if inbound_links > MAX_INBOUND_LINKS_PER_DESTINATION {
        return Err(Error::Configuration(format!(
            "processor '{dest_proc_id}' would hold {inbound_links} inbound links, and a \
             destination carries at most {MAX_INBOUND_LINKS_PER_DESTINATION}"
        )));
    }
    Ok(MAX_INBOUND_LINKS_PER_DESTINATION)
}

/// The drain order and ring depth of one destination input port, from the
/// delivery profile that port declares.
///
/// Resolved per destination port, never per channel: consumers of one output
/// port read it under whatever profiles they each declare.
fn delivery_resolution_of_input_port(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
) -> Result<DeliveryResolution> {
    let dest_type = processor_class_import_path_of(graph, dest_proc_id)?;
    Ok(delivery_profile_for_input_port(
        graph.node_types_this_stream_resolves(),
        &dest_type,
        dest_port,
    )?
    .resolve())
}

/// The class a processor in the graph was added as, refused by name when the
/// graph has no such processor.
fn processor_class_import_path_of(
    graph: &Graph,
    proc_id: &ProcessorUniqueId,
) -> Result<ProcessorClassImportPath> {
    graph
        .traversal()
        .v(proc_id)
        .first()
        .map(|node| node.processor_type().clone())
        .ok_or_else(|| Error::ProcessorNotFound(format!("Processor '{proc_id}' not found")))
}

/// The window declaration this destination's input port carries, if it carries
/// one.
///
/// Reads the destination's registered class, the same way the delivery profile
/// is read: the declaration is the whole answer and nothing is inferred.
fn audio_windowing_declared_by_input_port_of(
    graph: &Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
) -> Result<Option<AudioWindowDeclarationOfAnInputPort>> {
    let Ok(dest_type) = processor_class_import_path_of(graph, dest_proc_id) else {
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
fn refuse_a_second_inbound_link_into_a_windowed_port(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let already_inbound = graph
        .traversal_mut()
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
fn refuse_a_windowed_port_onto_a_channel_created_shallower_than_its_ring(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source: &OutputLinkPortRef,
    dest_proc_id: &ProcessorUniqueId,
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

/// The link wiring the node of a processor whose ports live out of process
/// carries, or `None` for one the engine wires itself — read off the node, never
/// through the processor's own lock, which a helper holds across its setup.
fn out_of_process_link_wiring_of(
    graph: &Graph,
    proc_id: &ProcessorUniqueId,
) -> Option<Arc<OutOfProcessLinkWiringEnvelope>> {
    graph
        .traversal()
        .v(proc_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .map(|component| Arc::clone(&component.0))
}

/// The processor whose ports one side of a disconnect must release, or `None`
/// with the reason said out loud.
///
/// A missing processor is not an error worth failing the disconnect over — the
/// link is going away regardless — but it does mean a port stays held, which is
/// only ever visible in the log.
fn processor_to_reclaim_from(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
) -> Option<Arc<Mutex<ProcessorInstance>>> {
    get_single_processor(graph, proc_id)
        .inspect_err(|error| {
            tracing::warn!(
                proc_id = %proc_id,
                error = %error,
                "close_iceoryx2_service: processor missing; port not reclaimed"
            )
        })
        .ok()
}

fn get_single_processor(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
) -> Result<Arc<Mutex<ProcessorInstance>>> {
    graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        })
        .ok_or_else(|| Error::Configuration(format!("Processor '{}' not found", proc_id)))
}

/// Install (once) the source's single channel publisher, append this link's
/// destination notifier onto the Rust source's [`OutputWriterInner`], and
/// publish its loss counts onto its graph node. Hands back that output writer,
/// or `None` for a processor that holds none.
///
/// [`OutputWriterInner`]: crate::iceoryx2::OutputWriterInner
#[allow(clippy::too_many_arguments)]
fn wire_rust_source(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_processor: &Arc<Mutex<ProcessorInstance>>,
    source_port: &str,
    link_id: &LinkUniqueId,
    service: &Iceoryx2Service,
    notify_service: &Iceoryx2NotifyService,
    egress_config: ChannelEgressConfig,
) -> Result<Option<Arc<crate::iceoryx2::OutputWriterInner>>> {
    let source_guard = source_processor.lock();
    let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() else {
        return Ok(None);
    };

    if !output_inner.has_channel_publisher(source_port) {
        let publisher = service.create_publisher(egress_config.expected_payload_bytes)?;
        output_inner.set_channel_publisher(source_port, publisher, egress_config);
        tracing::debug!(
            "Installed channel publisher for source output port '{}'",
            source_port
        );
    }

    let notifier = notify_service.create_notifier()?;
    output_inner.add_channel_link(source_port, link_id.as_str(), notifier);
    publish_loss_counts_on_processor_node(graph, source_proc_id, &source_guard);
    Ok(Some(output_inner))
}

/// Subscribe the Rust destination to the channel bound to its local input port,
/// ensure its single listener exists, and publish its loss counts onto its graph
/// node.
///
/// A plain port's subscriber ring and mailbox take the port's own delivery
/// resolution. A windowed port's subscriber takes the windowed ring and its
/// mailbox is sized from its contract.
#[allow(clippy::too_many_arguments)]
fn wire_rust_dest(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_processor: &Arc<Mutex<ProcessorInstance>>,
    dest_port: &str,
    link_id: &LinkUniqueId,
    channel_service_name: &str,
    dest_input_port_delivery: DeliveryResolution,
    service: &Iceoryx2Service,
    notify_service: &Iceoryx2NotifyService,
    audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
) -> Result<()> {
    let dest_guard = dest_processor.lock();
    let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() else {
        return Ok(());
    };
    let DeliveryResolution {
        drain_order,
        depth: input_port_ring_depth,
    } = dest_input_port_delivery;
    let subscriber_ring_depth =
        subscriber_ring_depth_of_input_port(dest_input_port_delivery, audio_windowing.as_ref());

    if !input_inner.has_port(dest_port) {
        match audio_windowing {
            None => input_inner.add_port(dest_port, input_port_ring_depth, drain_order),
            Some(AudioWindowDeclarationOfAnInputPort::StatedOutright(contract)) => {
                input_inner.add_windowed_port(dest_port, drain_order, contract)
            }
            // A sentinel this processor already settled — a link wired after
            // its `setup()` ran — windows from the settled values rather than
            // waiting for a `setup()` that has been and gone.
            Some(AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream) => {
                match input_inner
                    .device_matched_audio_window_contracts()
                    .settled_for_input_port(dest_port)
                {
                    Some(contract) => {
                        input_inner.add_windowed_port(dest_port, drain_order, contract)
                    }
                    None => input_inner.add_port_awaiting_its_device_stream_format(
                        dest_port,
                        input_port_ring_depth,
                        drain_order,
                    ),
                }
            }
        }
    }

    let subscriber = service.create_subscriber(subscriber_ring_depth)?;
    input_inner.add_channel_subscriber(
        dest_port,
        link_id.as_str(),
        &InboundLinkName::from(channel_service_name),
        subscriber,
    );
    tracing::debug!(
        "Bound channel subscriber to destination input port '{}'",
        dest_port
    );

    if !input_inner.has_listener() {
        let listener = notify_service.create_listener()?;
        input_inner.set_listener(listener);
        tracing::debug!("Created listener for destination on its notify service");
    }
    publish_loss_counts_on_processor_node(graph, dest_proc_id, &dest_guard);
    publish_device_matched_audio_window_contracts_on_destination_node(
        graph,
        dest_proc_id,
        &input_inner,
    );
    Ok(())
}

/// Share a processor's per-inbound-link dropped-bag and discarded-sample counts
/// and per-output-port refused-bag counts onto its graph node, so `graph` reads
/// them live off the ports that count them.
///
/// Inserted with the processor's first wired link at either end and left alone
/// after: each count is one shared object for the whole processor, and a link or
/// channel wired later mints its own zeroed entry inside it. A producer that only
/// produces carries its node's metrics as a destination does.
fn publish_loss_counts_on_processor_node(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
    processor: &ProcessorInstance,
) {
    let input_inner = processor.iceoryx2_input_mailboxes_inner();
    insert_loss_counts_on_processor_node_once(
        graph,
        proc_id,
        ProcessorLossCounts::CountedByPortsInThisProcess(LossCountsOfPortsInThisProcess {
            dropped_bag_counts_by_inbound_link: input_inner
                .as_ref()
                .map(|input_inner| input_inner.dropped_bag_counts_by_inbound_link())
                .unwrap_or_default(),
            discarded_sample_counts_by_inbound_link: input_inner
                .map(|input_inner| input_inner.discarded_sample_counts_by_inbound_link())
                .unwrap_or_default(),
            bag_counts_by_output_port: processor
                .iceoryx2_output_writer_inner()
                .map(|output_inner| output_inner.bag_counts_by_output_port())
                .unwrap_or_default(),
        }),
    );
}

/// Insert `loss_counts` as `proc_id`'s node metrics, unless its first wired link
/// already did.
fn insert_loss_counts_on_processor_node_once(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
    loss_counts: ProcessorLossCounts,
) {
    let Some(node) = graph.traversal_mut().v(proc_id).first_mut() else {
        return;
    };
    if node.has::<ProcessorMetrics>() {
        return;
    }
    node.insert(ProcessorMetrics {
        loss_counts,
        ..Default::default()
    });
}

/// Share the destination's settled `match_device` contracts onto its graph
/// node, so `graph` renders a port's resolved values off the port that resolved
/// them.
///
/// Same shape and same reason as the dropped-bag counts beside it: one shared
/// object, inserted with the destination's first inbound link, read live rather
/// than copied at wiring time — which matters more here than there, because at
/// wiring time there is nothing to copy yet.
fn publish_device_matched_audio_window_contracts_on_destination_node(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    input_inner: &crate::iceoryx2::InputMailboxesInner,
) {
    let Some(node) = graph.traversal_mut().v(dest_proc_id).first_mut() else {
        return;
    };
    if node.has::<DeviceMatchedAudioWindowContractsComponent>() {
        return;
    }
    node.insert_component_without_rendering_it(DeviceMatchedAudioWindowContractsComponent(
        input_inner.device_matched_audio_window_contracts(),
    ));
}

/// Record this link's source-side wiring on a processor whose transport lives
/// out of process, so it opens its own channel publisher + destination notifier
/// from the envelope. One entry per link — the far side installs the single
/// publisher once (keyed by source port) and appends a notifier per entry.
///
/// Hands back the cell this end's answer will land in, or `None` where the
/// entry rides the far side's startup envelope instead and its `ready`
/// confirms it.
#[allow(clippy::too_many_arguments)]
fn wire_subprocess_source(
    graph: &mut Graph,
    link_wiring: &OutOfProcessLinkWiringEnvelope,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
    channel_service_name: &str,
    notify_service_name: &str,
    expected_payload: usize,
    channel_ceiling_bytes: usize,
    channel_sizing: ChannelSizing,
    notify_max_notifiers: usize,
    link_id: &LinkUniqueId,
) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
    let mut entry = serde_json::json!({
        "name": source_port,
        "link_id": link_id.to_string(),
        "channel_service_name": channel_service_name,
        "dest_notify_service_name": notify_service_name,
        "expected_payload_bytes": expected_payload,
        "max_payload_bytes_per_channel": channel_ceiling_bytes,
        "channel_service_creation_depth": channel_sizing.channel_service_creation_depth,
        "max_subscribers": channel_sizing.max_subscribers,
        "notify_max_notifiers": notify_max_notifiers,
    });

    // The generation rides the entry, so the far side writes this channel's
    // refusals where the parent reads them for this channel and no earlier one.
    let loss_counts = link_wiring.helper_placed_processor_loss_counts();
    entry["output_port_wiring_generation"] =
        serde_json::json!(loss_counts.note_outbound_link(source_port, link_id.as_str()));
    let wire_reply = link_wiring.record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
        crate::core::PortDirection::Output,
        entry,
    )?;
    insert_loss_counts_on_processor_node_once(
        graph,
        source_proc_id,
        ProcessorLossCounts::CountedInItsHelperProcess(loss_counts),
    );
    Ok(wire_reply)
}

/// Record this link's dest-side wiring on a processor whose transport lives out
/// of process, so it opens its own channel subscriber (bound to its local input
/// port) from the envelope.
///
/// The envelope's `inbound_link_name` is the channel's own name: a helper
/// knows each link into it by the channel that carries it.
///
/// Hands back the cell this end's answer will land in, on the same terms as
/// [`wire_subprocess_source`].
#[allow(clippy::too_many_arguments)]
fn wire_subprocess_dest(
    graph: &mut Graph,
    link_wiring: &OutOfProcessLinkWiringEnvelope,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
    channel_service_name: &str,
    notify_service_name: &str,
    dest_input_port_delivery: DeliveryResolution,
    channel_sizing: ChannelSizing,
    notify_max_notifiers: usize,
    link_id: &LinkUniqueId,
    audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
    // The dest reader carries no payload-size hint: the publisher's segment
    // grows to whatever frame it sends, and the reader copies each out whole.
    // The drain order is the port's own delivery profile's, resolved host-side.
    // The creation depth is what the child opens the service with, and the ring
    // depth what its subscriber takes — the windowed ring for a windowed port,
    // whose mailbox the child sizes from the contract, and otherwise the
    // profile's, which a plain port's mailbox takes too.
    let mut entry = serde_json::json!({
        "name": dest_port,
        "link_id": link_id.to_string(),
        "channel_service_name": channel_service_name,
        "inbound_link_name": InboundLinkName::from(channel_service_name).as_str(),
        "notify_service_name": notify_service_name,
        "read_mode": dest_input_port_delivery.drain_order.as_manifest_str(),
        "channel_service_creation_depth": channel_sizing.channel_service_creation_depth,
        "input_port_ring_depth": subscriber_ring_depth_of_input_port(
            dest_input_port_delivery,
            audio_windowing.as_ref(),
        ),
        "max_subscribers": channel_sizing.max_subscribers,
        "notify_max_notifiers": notify_max_notifiers,
    });
    // The window contract rides the envelope beside `read_mode`, or the child's
    // own stage windows nothing. The values go over resolved, so the child
    // reads one shape and never a sentinel it could not settle — and a helper
    // can never settle one: the format comes from a device stream a processor
    // opens in the app process, and nothing crosses to say so. A sentinel on a
    // helper-placed port is therefore refused here, where the destination's
    // placement is known.
    let into_a_windowed_port = audio_windowing.is_some();
    match audio_windowing {
        None => {}
        Some(AudioWindowDeclarationOfAnInputPort::StatedOutright(contract)) => {
            entry["audio_window"] =
                serde_json::to_value(contract.as_declared_values()).map_err(|render_failure| {
                    Error::Configuration(format!(
                        "the window contract on input port '{dest_port}' could not be rendered \
                         onto the helper wiring envelope: {render_failure}"
                    ))
                })?;
        }
        Some(AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream) => {
            let dest_type = processor_class_import_path_of(graph, dest_proc_id)?;
            return Err(refuse_an_unsettled_match_device_sentinel(
                &dest_type, dest_port,
            ));
        }
    }

    // The slot and generation ride the entry, so the far side writes this
    // link's counts where the parent reads them for this wiring and no other.
    let loss_counts = link_wiring.helper_placed_processor_loss_counts();
    let assigned = loss_counts.assign_inbound_link_slot(link_id.as_str(), into_a_windowed_port)?;
    entry["loss_count_slot"] = serde_json::json!(assigned.slot);
    entry["wiring_generation"] = serde_json::json!(assigned.wiring_generation);
    let wire_reply = link_wiring.record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
        crate::core::PortDirection::Input,
        entry,
    )?;
    insert_loss_counts_on_processor_node_once(
        graph,
        dest_proc_id,
        ProcessorLossCounts::CountedInItsHelperProcess(loss_counts),
    );
    Ok(wire_reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::execution::{ExecutionConfig, ProcessExecution};
    use crate::core::graph::ProcessorInstanceWithItsOutOfProcessLinkWiring;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use crate::core::processors::{
        DynGeneratedProcessor, OutOfProcessLinkWireOutcome, ProcessorSpec,
    };
    use crate::core::test_support::{
        CapturedTracingWarnings, ReclaimedLink, RecordingOutOfProcessFarSideLinkDelivery,
        add_mock_input_only, add_mock_output_only,
    };
    use crate::core::{ProcessorDescriptor, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
    use crate::iceoryx2::MessageGroupBoundary;

    /// A host whose transport lives out of process — the shape the processor
    /// interpreter spawn host has — with its far side already past its setup
    /// command.
    struct OutOfProcessSpawnHostStub {
        link_wiring: Arc<OutOfProcessLinkWiringEnvelope>,
    }

    impl OutOfProcessSpawnHostStub {
        /// A stub whose far side drives its processor in
        /// `far_side_process_execution` and records what it is handed in
        /// `far_side`.
        fn with_a_far_side_past_its_setup_command(
            far_side: RecordingOutOfProcessFarSideLinkDelivery,
            far_side_process_execution: ProcessExecution,
        ) -> Self {
            let link_wiring = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                far_side_process_execution,
            ));
            link_wiring
                .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), far_side)
                .expect("a stub's setup command always goes out");
            Self { link_wiring }
        }
    }

    /// Reactive, the mode a Python class with inputs takes unless it says otherwise.
    impl Default for OutOfProcessSpawnHostStub {
        fn default() -> Self {
            Self::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery::default(),
                ProcessExecution::Reactive,
            )
        }
    }

    impl DynGeneratedProcessor for OutOfProcessSpawnHostStub {
        fn __generated_setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn name(&self) -> &str {
            "OutOfProcessSpawnHostStub"
        }
        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            None
        }
        fn execution_config(&self) -> ExecutionConfig {
            ExecutionConfig::new(ProcessExecution::Manual)
        }
        fn has_iceoryx2_outputs(&self) -> bool {
            false
        }
        fn has_iceoryx2_inputs(&self) -> bool {
            false
        }
        fn set_iceoryx2_resources(
            &mut self,
            _output_writer: Option<crate::iceoryx2::OutputWriter>,
            _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
        ) -> Result<()> {
            Ok(())
        }
        fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
            None
        }
        fn iceoryx2_input_mailboxes_inner(
            &self,
        ) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
            None
        }
        fn out_of_process_link_wiring(&self) -> Option<Arc<OutOfProcessLinkWiringEnvelope>> {
            Some(Arc::clone(&self.link_wiring))
        }
        fn apply_config_json(&mut self, _config_json: &serde_json::Value) -> Result<()> {
            Ok(())
        }
        fn to_runtime_json(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn config_json(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    /// Attach `instance` to `proc_id` through the attach the spawn op takes, so
    /// the wiring path can reach it.
    fn attach_processor_instance(
        graph: &mut Graph,
        proc_id: &str,
        instance: ProcessorInstance,
    ) -> Arc<Mutex<ProcessorInstance>> {
        ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(
            graph
                .traversal_mut()
                .v(proc_id)
                .first_mut()
                .expect("the node must exist"),
        )
    }

    /// Record one link's wiring on both out-of-process endpoints, exactly as
    /// the compiler op's subprocess branches do.
    ///
    /// Shared so the wiring a disconnect has to undo is byte-for-byte the
    /// wiring the connect laid down; the arguments are positional and both
    /// helpers carry `#[allow(clippy::too_many_arguments)]`, so a second copy
    /// is a slip waiting to happen.
    fn record_wiring_for_both_out_of_process_endpoints(
        graph: &mut Graph,
        source_id: &str,
        dest_id: &str,
        link_id: &LinkUniqueId,
    ) {
        let source_link_wiring = out_of_process_link_wiring_of(graph, &source_id.into())
            .expect("a helper stub's node carries its link wiring");
        wire_subprocess_source(
            graph,
            &source_link_wiring,
            &source_id.into(),
            "out1",
            "pabc/out1",
            "pdef/notify",
            4096,
            1 << 20,
            sizing_of_a_two_subscriber_test_channel(),
            1,
            link_id,
        )
        .expect("recording source wiring must succeed");
        let dest_link_wiring = out_of_process_link_wiring_of(graph, &dest_id.into())
            .expect("a helper stub's node carries its link wiring");
        wire_subprocess_dest(
            graph,
            &dest_link_wiring,
            &dest_id.into(),
            "in1",
            "pabc/out1",
            "pdef/notify",
            DeliveryProfile::Newest.resolve(),
            sizing_of_a_two_subscriber_test_channel(),
            1,
            link_id,
            None,
        )
        .expect("recording dest wiring must succeed");
    }

    /// The wiring path reaches a host it cannot name — the whole point of the
    /// seam. Mentally revert `wire_subprocess_source` / `wire_subprocess_dest`
    /// to downcasting on the two engine-side host types and both vectors stay
    /// empty, because this host is neither of them.
    #[test]
    fn link_wiring_reaches_a_host_the_engine_cannot_downcast_to() {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        let source_instance = attach_processor_instance(
            &mut graph,
            &source_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let link_id: LinkUniqueId = "L-seam-test".into();

        record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

        let recorded_source_ports = source_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports();
        assert_eq!(
            recorded_source_ports["outputs"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            recorded_source_ports["outputs"][0]["channel_service_name"],
            serde_json::json!("pabc/out1"),
        );

        let recorded_dest_ports = dest_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports();
        assert_eq!(recorded_dest_ports["inputs"].as_array().unwrap().len(), 1);
        assert_eq!(
            recorded_dest_ports["inputs"][0]["read_mode"],
            serde_json::json!("skip_to_latest"),
        );
    }

    /// What `graph` renders under `metrics` for `proc_id`, or `None` for no key.
    fn rendered_metrics_in(graph: &mut Graph, proc_id: &str) -> Option<serde_json::Value> {
        graph
            .traversal_mut()
            .v(proc_id)
            .first()
            .expect("the processor's node must be in the graph")
            .serialize_components()
            .get("metrics")
            .cloned()
    }

    /// The input entry a helper stub was handed for `link_id`.
    fn input_entry_recorded_on(
        instance: &Arc<Mutex<ProcessorInstance>>,
        link_id: &LinkUniqueId,
    ) -> serde_json::Value {
        instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports()["inputs"]
            .as_array()
            .expect("the envelope renders its inputs as an array")
            .iter()
            .find(|entry| entry["link_id"] == serde_json::json!(link_id.as_str()))
            .cloned()
            .expect("the link's entry is on the envelope")
    }

    /// Create a helper stub's board as its spawn would, and open its writer as
    /// its helper would, from a node of the helper's own.
    fn board_of_a_helper_stub_spawn(
        instance: &Arc<Mutex<ProcessorInstance>>,
        proc_id: &str,
        output_port_names: &[&str],
    ) -> (
        Arc<crate::iceoryx2::HelperProcessLossCountBoardWriter>,
        Iceoryx2Node,
    ) {
        let output_port_names: Vec<String> = output_port_names
            .iter()
            .map(|name| name.to_string())
            .collect();
        let setup_command_board = instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .create_the_loss_count_board_for_this_helper_spawn_on(
                &Iceoryx2Node::for_this_test_process(),
                proc_id,
                output_port_names.clone(),
            )
            .expect("the spawn creates its board");
        assert_eq!(
            setup_command_board["output_ports"],
            serde_json::json!(output_port_names),
        );
        let helper_node = Iceoryx2Node::for_this_test_process();
        let writer = helper_node
            .open_helper_process_loss_count_board_writer(
                setup_command_board["service_name"]
                    .as_str()
                    .expect("the setup command names the board"),
                &output_port_names,
            )
            .expect("the helper opens the board its setup command names");
        (Arc::new(writer), helper_node)
    }

    /// A helper-placed destination's node renders the counts its helper wrote
    /// on the slot its link was given, and a helper-placed producer's node the
    /// refusals its helper wrote per output port — zero before the helper has
    /// written anything, never an absent key.
    ///
    /// Fail-without-fix: leave `wire_subprocess_dest` publishing nothing and
    /// the destination renders no `metrics` key at all, so a helper that lost
    /// most of its bags reads like a healthy one.
    #[test]
    fn a_helper_placed_destinations_node_renders_the_counts_its_helper_wrote_on_its_slot() {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        let source_instance = attach_processor_instance(
            &mut graph,
            &source_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let link_id: LinkUniqueId = "L-helper-placed".into();
        record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

        assert_eq!(
            rendered_metrics_in(&mut graph, &dest_id),
            Some(serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": { "L-helper-placed": 0 },
                "refused_bags_by_output_port": {},
                "written_bags_by_output_port": {}
            })),
            "a wired link renders zero before its helper's board exists"
        );

        let entry = input_entry_recorded_on(&dest_instance, &link_id);
        assert_eq!(entry["loss_count_slot"], serde_json::json!(0));
        assert_eq!(entry["wiring_generation"], serde_json::json!(1));

        let (dest_board_writer, _dest_helper_node) =
            board_of_a_helper_stub_spawn(&dest_instance, &dest_id, &[]);
        dest_board_writer
            .claim_inbound_link_slot(
                crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                    slot: 0,
                    wiring_generation: 1,
                },
            )
            .unwrap()
            .mirror_dropped_bags(5);
        assert_eq!(
            rendered_metrics_in(&mut graph, &dest_id),
            Some(serde_json::json!({
                "frames_dropped": 5,
                "dropped_bags_by_link": { "L-helper-placed": 5 },
                "refused_bags_by_output_port": {},
                "written_bags_by_output_port": {}
            })),
        );

        let output_entry = source_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports()["outputs"][0]
            .clone();
        let output_port_wiring_generation = output_entry["output_port_wiring_generation"]
            .as_u64()
            .expect("the output entry carries its channel's generation");
        let (source_board_writer, _source_helper_node) =
            board_of_a_helper_stub_spawn(&source_instance, &source_id, &["out1"]);
        let out1_mirror = source_board_writer
            .claim_output_port_entry("out1", output_port_wiring_generation)
            .expect("the source's board carries its declared port");
        out1_mirror.mirror_refused_bags(2);
        out1_mirror.mirror_written_bags(61);
        assert_eq!(
            rendered_metrics_in(&mut graph, &source_id),
            Some(serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": {},
                "refused_bags_by_output_port": { "out1": 2 },
                "written_bags_by_output_port": { "out1": 61 }
            })),
        );
    }

    /// A link disconnected and connected again takes the slot it had with a new
    /// generation, renders from zero, and ignores a write its helper made under
    /// the old wiring.
    ///
    /// Fail-without-fix: render a slot whatever generation is written on it and
    /// the reconnected link shows the old wiring's nine bags, a count outliving
    /// the link it named.
    #[test]
    fn a_reconnected_link_reuses_its_slot_from_zero_and_a_write_from_the_old_wiring_is_ignored() {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        attach_processor_instance(
            &mut graph,
            &source_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
        record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);
        let (board_writer, _helper_node) =
            board_of_a_helper_stub_spawn(&dest_instance, &dest_id, &[]);
        let first_wiring = board_writer
            .claim_inbound_link_slot(
                crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                    slot: 0,
                    wiring_generation: 1,
                },
            )
            .unwrap();
        first_wiring.mirror_dropped_bags(7);

        close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect succeeds");
        assert_eq!(
            rendered_metrics_in(&mut graph, &dest_id)
                .map(|metrics| metrics["dropped_bags_by_link"].clone()),
            Some(serde_json::json!({})),
            "a disconnected link's count goes with it"
        );

        record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);
        let entry = input_entry_recorded_on(&dest_instance, &link_id);
        assert_eq!(entry["loss_count_slot"], serde_json::json!(0));
        assert_eq!(entry["wiring_generation"], serde_json::json!(2));

        board_writer
            .claim_inbound_link_slot(
                crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                    slot: 0,
                    wiring_generation: 1,
                },
            )
            .unwrap()
            .mirror_dropped_bags(9);
        assert_eq!(
            rendered_metrics_in(&mut graph, &dest_id)
                .map(|metrics| metrics["dropped_bags_by_link"].clone()),
            Some(serde_json::json!({ link_id.as_str(): 0 })),
            "a slot written under the old generation renders nothing for the new wiring"
        );

        board_writer
            .claim_inbound_link_slot(
                crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                    slot: 0,
                    wiring_generation: 2,
                },
            )
            .unwrap()
            .mirror_dropped_bags(1);
        assert_eq!(
            rendered_metrics_in(&mut graph, &dest_id)
                .map(|metrics| metrics["dropped_bags_by_link"].clone()),
            Some(serde_json::json!({ link_id.as_str(): 1 })),
        );
    }

    /// The keys of `rendered`, sorted.
    fn rendered_metrics_keys(rendered: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = rendered
            .as_object()
            .expect("metrics render as an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    /// A helper-placed processor's node renders exactly the keys an
    /// app-process one does, into a plain port and into a windowed one alike.
    ///
    /// Fail-without-fix: give a helper's windowed link no sample count, or
    /// every helper link one, and the key sets differ.
    #[test]
    fn a_helper_placed_processors_metrics_carry_exactly_the_keys_an_app_process_ones_do() {
        let mut native_plain = NativeLinkWiredForLossCounting::wire(
            "metrics-keys-plain",
            DeliveryProfile::Ordered.resolve(),
            crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        );
        let dest_id = native_plain.dest_id.clone();
        let native_plain_keys = rendered_metrics_keys(
            &native_plain
                .rendered_metrics_of(&dest_id)
                .expect("an app-process destination renders metrics"),
        );
        let native_windowed_keys = rendered_metrics_keys(
            &NativeLinkIntoAWindowedPort::wire().rendered_metrics_of_the_destination(),
        );

        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let helper_plain_id = add_mock_input_only(&mut graph);
        let helper_windowed_id = add_mock_windowed_audio_consumer(&mut graph);
        for proc_id in [&source_id, &helper_plain_id, &helper_windowed_id] {
            attach_processor_instance(
                &mut graph,
                proc_id,
                ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
            );
        }
        record_wiring_for_both_out_of_process_endpoints(
            &mut graph,
            &source_id,
            &helper_plain_id,
            &"L-helper-plain".into(),
        );
        let declared = audio_windowing_declared_by_input_port_of(
            &graph,
            &helper_windowed_id.as_str().into(),
            "audio",
        )
        .expect("the mock's contract resolves");
        let dest_link_wiring =
            out_of_process_link_wiring_of(&graph, &helper_windowed_id.as_str().into())
                .expect("a helper stub's node carries its link wiring");
        wire_subprocess_dest(
            &mut graph,
            &dest_link_wiring,
            &helper_windowed_id.as_str().into(),
            "audio",
            "pabc/out1",
            "pdef/notify",
            DeliveryProfile::Ordered.resolve(),
            ChannelSizing {
                max_subscribers: 2,
                channel_service_creation_depth: WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
            },
            1,
            &"L-helper-windowed".into(),
            declared,
        )
        .expect("a windowed helper port wires");

        let helper_plain_keys = rendered_metrics_keys(
            &rendered_metrics_in(&mut graph, &helper_plain_id)
                .expect("a helper-placed destination renders metrics"),
        );
        let helper_windowed_keys = rendered_metrics_keys(
            &rendered_metrics_in(&mut graph, &helper_windowed_id)
                .expect("a helper-placed windowed destination renders metrics"),
        );
        assert_eq!(helper_plain_keys, native_plain_keys);
        assert_eq!(helper_windowed_keys, native_windowed_keys);
        assert!(
            helper_windowed_keys.contains(&"discarded_samples_by_link".to_string()),
            "the windowed rendering is the one carrying a sample count: {helper_windowed_keys:?}"
        );
    }

    /// Disconnecting a link whose endpoints both live out of process reclaims
    /// BOTH halves through the same seam that wired them, each told its own
    /// local port and direction — and each host's envelope forgets the link, so
    /// the next setup does not re-send it beside the reconnect's own entry.
    ///
    /// Revert lock: reclaim either endpoint through engine-side ports instead
    /// of its envelope and that side records no reclaim at all, which is the
    /// leak — a live helper child keeps the notifier and appends another on
    /// reconnect, until the notify service's create-time `max_notifiers` cap
    /// is exhausted (`ExceedsMaxSupportedNotifiers`).
    #[test]
    fn disconnecting_an_out_of_process_link_reclaims_both_endpoints() {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);

        let source_reclaims: Arc<Mutex<Vec<ReclaimedLink>>> = Arc::default();
        let dest_reclaims: Arc<Mutex<Vec<ReclaimedLink>>> = Arc::default();
        let source_instance = attach_processor_instance(
            &mut graph,
            &source_id,
            ProcessorInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery {
                        reclaimed_links: source_reclaims.clone(),
                        ..Default::default()
                    },
                    ProcessExecution::Reactive,
                ),
            )),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery {
                        reclaimed_links: dest_reclaims.clone(),
                        ..Default::default()
                    },
                    ProcessExecution::Reactive,
                ),
            )),
        );

        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&source_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            )
            .first()
            .expect("the link must exist")
            .id
            .clone();

        record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

        close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

        assert_eq!(
            *source_reclaims.lock(),
            [ReclaimedLink {
                port_direction: crate::core::PortDirection::Output,
                local_port_name: "out1".to_string(),
                link_id: link_id.to_string(),
            }],
            "the source host must be asked to drop its publisher-side link, by its own port",
        );
        assert_eq!(
            *dest_reclaims.lock(),
            [ReclaimedLink {
                port_direction: crate::core::PortDirection::Input,
                local_port_name: "in1".to_string(),
                link_id: link_id.to_string(),
            }],
            "the destination host must be asked to drop its subscriber, by its own port",
        );

        for (label, instance) in [("source", &source_instance), ("dest", &dest_instance)] {
            let ports = instance
                .lock()
                .out_of_process_link_wiring()
                .expect("the stub records its own wiring")
                .as_setup_command_ports();
            assert!(
                ports["inputs"].as_array().unwrap().is_empty()
                    && ports["outputs"].as_array().unwrap().is_empty(),
                "the {label} envelope must carry nothing for a disconnected link; got {ports}",
            );
        }
    }

    /// Wiring a link records it on the envelope AND hands the same entry to a
    /// far side past its setup command, so a far side that already read its
    /// envelope opens the port now. Revert lock: record the entry without
    /// handing it over and both vectors stay empty — the link never reaches a
    /// running helper.
    #[test]
    fn wiring_an_out_of_process_link_hands_each_endpoint_its_entry() {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);

        let source_late_wired: Arc<Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>> =
            Arc::default();
        let dest_late_wired: Arc<Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>> =
            Arc::default();
        let source_instance = attach_processor_instance(
            &mut graph,
            &source_id,
            ProcessorInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery {
                        late_wired_links: source_late_wired.clone(),
                        ..Default::default()
                    },
                    ProcessExecution::Reactive,
                ),
            )),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery {
                        late_wired_links: dest_late_wired.clone(),
                        ..Default::default()
                    },
                    ProcessExecution::Reactive,
                ),
            )),
        );

        let link_id: LinkUniqueId = "L-wired-late".into();
        record_wiring_for_both_out_of_process_endpoints(&mut graph, &source_id, &dest_id, &link_id);

        let source_handed = source_late_wired.lock();
        let [(source_direction, source_entry)] = &source_handed[..] else {
            panic!("the source host must be handed exactly one entry; got {source_handed:?}");
        };
        assert_eq!(*source_direction, crate::core::PortDirection::Output);
        assert_eq!(
            *source_entry,
            source_instance
                .lock()
                .out_of_process_link_wiring()
                .expect("the stub records its own wiring")
                .as_setup_command_ports()["outputs"][0],
            "the entry handed to the source is the one its envelope recorded",
        );

        let dest_handed = dest_late_wired.lock();
        let [(dest_direction, dest_entry)] = &dest_handed[..] else {
            panic!("the destination host must be handed exactly one entry; got {dest_handed:?}");
        };
        assert_eq!(*dest_direction, crate::core::PortDirection::Input);
        assert_eq!(
            *dest_entry,
            dest_instance
                .lock()
                .out_of_process_link_wiring()
                .expect("the stub records its own wiring")
                .as_setup_command_ports()["inputs"][0],
            "the entry handed to the destination is the one its envelope recorded",
        );
    }

    /// The same seam answers the "does the engine wire this one itself?"
    /// question, so a helper-hosted processor is not handed engine-side
    /// publishers it could never use.
    #[test]
    fn a_host_with_an_out_of_process_transport_is_recognised_as_a_subprocess() {
        let mut graph = Graph::new();
        let helper_hosted_id = add_mock_output_only(&mut graph);
        attach_processor_instance(
            &mut graph,
            &helper_hosted_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        assert!(out_of_process_link_wiring_of(&graph, &helper_hosted_id.as_str().into()).is_some());

        let engine_hosted_id = add_mock_input_only(&mut graph);
        attach_mock_instance::<crate::core::test_support::MockInputOnlyProcessor::Processor>(
            &mut graph,
            &engine_hosted_id,
        );
        assert!(out_of_process_link_wiring_of(&graph, &engine_hosted_id.as_str().into()).is_none());
    }

    /// A link with one endpoint in each world reclaims each end its own way —
    /// the branch is per endpoint, not per link. This is the shape the MVP
    /// graph is actually made of: a Python helper wired to a native built-in.
    ///
    /// Revert lock: key either branch off the *other* endpoint (or off
    /// `source_is_subprocess || dest_is_subprocess`) and one side is reclaimed
    /// through machinery it does not own — the engine-side publisher survives,
    /// or the helper is never told.
    #[test]
    fn a_link_between_an_engine_endpoint_and_a_helper_reclaims_each_its_own_way() {
        use crate::core::test_support::MockOutputOnlyProcessor;

        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let (source, source_output, _) =
            attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
        let source_output = source_output.expect("an output-only mock holds an output writer");

        let dest_id = add_mock_input_only(&mut graph);
        let dest_reclaims: Arc<Mutex<Vec<ReclaimedLink>>> = Arc::default();
        attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery {
                        reclaimed_links: dest_reclaims.clone(),
                        ..Default::default()
                    },
                    ProcessExecution::Reactive,
                ),
            )),
        );

        let link_id = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&source_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            )
            .first()
            .expect("the link must exist")
            .id
            .clone();

        let (channel, notify_service) =
            open_test_link_services("mixed-endpoints", DeliveryProfile::ORDERED_DEPTH);
        wire_rust_source(
            &mut graph,
            &source_id.as_str().into(),
            &source,
            "out1",
            &link_id,
            &channel,
            &notify_service,
            ChannelEgressConfig {
                service_name: unique_service_name("mixed-endpoints"),
                trust_tier: ChannelTrustTier::UntrustedSession,
                expected_payload_bytes: 4096,
                chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
            },
        )
        .expect("the engine-side source wires");
        let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
            .expect("a helper stub's node carries its link wiring");
        wire_subprocess_dest(
            &mut graph,
            &dest_link_wiring,
            &dest_id.as_str().into(),
            "in1",
            "pabc/out1",
            "pdef/notify",
            DeliveryProfile::Newest.resolve(),
            sizing_of_a_two_subscriber_test_channel(),
            1,
            &link_id,
            None,
        )
        .expect("recording dest wiring must succeed");
        assert!(source_output.has_channel_publisher("out1"));

        close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

        assert!(
            !source_output.has_channel_publisher("out1"),
            "the engine-side source must be reclaimed through its own writer, as it always was",
        );
        assert_eq!(
            *dest_reclaims.lock(),
            [ReclaimedLink {
                port_direction: crate::core::PortDirection::Input,
                local_port_name: "in1".to_string(),
                link_id: link_id.to_string(),
            }],
            "the helper destination must be asked to drop the subscriber it opened itself",
        );
    }

    /// Attach a live instance of `P` to `proc_id`, holding the iceoryx2
    /// resources its declared ports call for — the state the factory leaves a
    /// host-run processor in before the wiring op reaches it.
    fn attach_mock_instance<P>(
        graph: &mut Graph,
        proc_id: &str,
    ) -> (
        Arc<Mutex<ProcessorInstance>>,
        Option<Arc<crate::iceoryx2::OutputWriterInner>>,
        Option<Arc<crate::iceoryx2::InputMailboxesInner>>,
    )
    where
        P: crate::core::GeneratedProcessor + DynGeneratedProcessor + Send + 'static,
        P::Config: Default,
    {
        let mut instance = ProcessorInstance::new(Box::new(
            P::from_config(Default::default())
                .expect("the mock constructs from its default config"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let output_inner = instance.iceoryx2_output_writer_inner();
        let input_inner = instance.iceoryx2_input_mailboxes_inner();
        let instance = attach_processor_instance(graph, proc_id, instance);
        (instance, output_inner, input_inner)
    }

    /// Sizing for a test channel with room for one destination and the tap.
    fn sizing_of_a_two_subscriber_test_channel() -> ChannelSizing {
        ChannelSizing {
            max_subscribers: 2,
            channel_service_creation_depth: DeliveryProfile::ORDERED_DEPTH,
        }
    }

    /// A service name no concurrent test — or an earlier run that recycled this
    /// pid — can collide with on iceoryx2's machine-global `/dev/shm` namespace.
    /// A collision surfaces as `DoesNotSupportRequestedMinBufferSize` against
    /// the stale service, not as a clean failure.
    fn unique_service_name(tag: &str) -> String {
        format!(
            "test/wiring/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    /// The channel and notify services one wired link needs, on one node, the
    /// channel created `channel_service_creation_depth` deep.
    fn open_test_link_services(
        tag: &str,
        channel_service_creation_depth: usize,
    ) -> (
        crate::iceoryx2::Iceoryx2Service,
        crate::iceoryx2::Iceoryx2NotifyService,
    ) {
        let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
        let channel = node
            .open_or_create_service(
                &unique_service_name(&format!("{tag}/channel")),
                2,
                channel_service_creation_depth,
            )
            .expect("the channel service must open");
        let notify = node
            .open_or_create_notify_service(&unique_service_name(&format!("{tag}/notify")), 1)
            .expect("the notify service must open");
        (channel, notify)
    }

    /// What one helper destination was wired with, beside what each of its two
    /// sources — one in the app process, one a helper — was told about it.
    struct HelperDestinationFedByBothKindsOfSource {
        notifiers_the_engine_source_holds: usize,
        dest_notify_service_name_the_helper_source_was_handed: serde_json::Value,
        notify_service_names_the_destination_was_handed: Vec<serde_json::Value>,
        // Held so the services outlive the assertions.
        _graph: Graph,
    }

    /// Wire an app-process source and a helper source into one helper
    /// destination whose child runs `far_side_process_execution`, through the op
    /// the compiler runs.
    fn wire_both_kinds_of_source_into_a_helper_destination_driven_in(
        far_side_process_execution: ProcessExecution,
    ) -> HelperDestinationFedByBothKindsOfSource {
        use crate::core::test_support::MockOutputOnlyProcessor;

        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let engine_source_id = add_mock_output_only(&mut graph);
        let (_, engine_source_output, _) = attach_mock_instance::<MockOutputOnlyProcessor::Processor>(
            &mut graph,
            &engine_source_id,
        );
        let helper_source_id = add_mock_output_only(&mut graph);
        let helper_source = attach_processor_instance(
            &mut graph,
            &helper_source_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let dest_id = add_mock_input_only(&mut graph);
        let dest = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery::default(),
                    far_side_process_execution,
                ),
            )),
        );

        let from_the_engine_source =
            add_link_from_out1_to_in1(&mut graph, &engine_source_id, &dest_id);
        open_iceoryx2_service(&mut graph, &from_the_engine_source, &node)
            .expect("the engine source's link wires");
        let from_the_helper_source =
            add_link_from_out1_to_in1(&mut graph, &helper_source_id, &dest_id);
        open_iceoryx2_service(&mut graph, &from_the_helper_source, &node)
            .expect("the helper source's link wires");

        let helper_source_output_entry = helper_source
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports()["outputs"][0]
            .clone();
        HelperDestinationFedByBothKindsOfSource {
            notifiers_the_engine_source_holds: engine_source_output
                .expect("an output-only mock holds an output writer")
                .channel_notifier_count("out1"),
            dest_notify_service_name_the_helper_source_was_handed:
                helper_source_output_entry["dest_notify_service_name"].clone(),
            notify_service_names_the_destination_was_handed: [
                &from_the_engine_source,
                &from_the_helper_source,
            ]
            .into_iter()
            .map(|link_id| input_entry_recorded_on(&dest, link_id)["notify_service_name"].clone())
            .collect(),
            _graph: graph,
        }
    }

    /// A helper destination is notified by both kinds of source whatever mode
    /// its child runs in, on the service its own wiring opens its listener on.
    ///
    /// Fail-without-fix: notify only a reactive destination again and a
    /// `continuous` or `manual` helper's app-process source holds no notifier
    /// while its helper source is handed an empty name.
    #[test]
    fn a_helper_destination_is_notified_by_both_kinds_of_source_whatever_its_child_runs() {
        for far_side_process_execution in [
            ProcessExecution::Reactive,
            ProcessExecution::Continuous { interval_ms: 0 },
            ProcessExecution::Manual,
        ] {
            let wired = wire_both_kinds_of_source_into_a_helper_destination_driven_in(
                far_side_process_execution,
            );
            assert_eq!(
                wired.notifiers_the_engine_source_holds, 1,
                "an app-process source must notify a {far_side_process_execution:?} helper"
            );
            assert_ne!(
                wired.dest_notify_service_name_the_helper_source_was_handed,
                serde_json::json!(""),
                "a helper source must notify a {far_side_process_execution:?} helper"
            );
            assert_eq!(
                wired.notify_service_names_the_destination_was_handed,
                vec![
                    wired
                        .dest_notify_service_name_the_helper_source_was_handed
                        .clone();
                    2
                ],
                "a helper source notifies the service a {far_side_process_execution:?} helper \
                 listens on"
            );
        }
    }

    /// Wire one Rust→Rust link end to end the way the compiler op does, into a
    /// reactive or a self-driven destination, and hand back the two sides'
    /// iceoryx2 state.
    fn wire_one_test_link<Destination>(
        tag: &str,
        destination_is_reactive: bool,
    ) -> (
        Arc<crate::iceoryx2::OutputWriterInner>,
        Arc<crate::iceoryx2::InputMailboxesInner>,
    )
    where
        Destination: crate::core::GeneratedProcessor + DynGeneratedProcessor + Send + 'static,
        Destination::Config: Default,
    {
        use crate::core::test_support::MockOutputOnlyProcessor;

        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let (source, source_output, _) =
            attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
        let source_output = source_output.expect("an output-only mock holds an output writer");

        let dest_id = if destination_is_reactive {
            add_mock_reactive_input_only(&mut graph)
        } else {
            add_mock_input_only(&mut graph)
        };
        let dest_unique_id: ProcessorUniqueId = dest_id.as_str().into();
        let (dest, _, dest_input) = attach_mock_instance::<Destination>(&mut graph, &dest_id);
        let dest_input = dest_input.expect("an input-only mock holds input mailboxes");

        let (channel, notify_service) =
            open_test_link_services(tag, DeliveryProfile::ORDERED_DEPTH);
        let link_id: LinkUniqueId = format!("L-{tag}").as_str().into();

        wire_rust_source(
            &mut graph,
            &source_id.as_str().into(),
            &source,
            "out1",
            &link_id,
            &channel,
            &notify_service,
            ChannelEgressConfig {
                service_name: unique_service_name(tag),
                trust_tier: ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
            },
        )
        .expect("the source side wires");
        wire_rust_dest(
            &mut graph,
            &dest_unique_id,
            &dest,
            "in1",
            &link_id,
            "psource/out1",
            DeliveryProfile::Newest.resolve(),
            &channel,
            &notify_service,
            None,
        )
        .expect("the destination side wires");

        (source_output, dest_input)
    }

    /// One native link wired through both sides of the op the way the compiler
    /// runs them, with the source's refusals and the destination's losses
    /// counted where `graph` reads them.
    struct NativeLinkWiredForLossCounting {
        graph: Graph,
        source_id: ProcessorUniqueId,
        dest_id: ProcessorUniqueId,
        link_id: LinkUniqueId,
        source_output: Arc<crate::iceoryx2::OutputWriterInner>,
        dest_input: Arc<crate::iceoryx2::InputMailboxesInner>,
        // Held so the channel outlives the test's writes.
        _channel: crate::iceoryx2::Iceoryx2Service,
    }

    impl NativeLinkWiredForLossCounting {
        fn wire(tag: &str, dest_delivery: DeliveryResolution, source_ceiling_bytes: usize) -> Self {
            use crate::core::test_support::{MockInputOnlyProcessor, MockOutputOnlyProcessor};

            let mut graph = Graph::new();
            let source_id = add_mock_output_only(&mut graph);
            let (source, source_output, _) =
                attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
            let dest_id = add_mock_input_only(&mut graph);
            let (dest, _, dest_input) =
                attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut graph, &dest_id);
            let source_id: ProcessorUniqueId = source_id.as_str().into();
            let dest_id: ProcessorUniqueId = dest_id.as_str().into();

            let (channel, notify_service) =
                open_test_link_services(tag, DeliveryProfile::ORDERED_DEPTH);
            let link_id: LinkUniqueId = format!("L-{tag}").as_str().into();
            wire_rust_source(
                &mut graph,
                &source_id,
                &source,
                "out1",
                &link_id,
                &channel,
                &notify_service,
                ChannelEgressConfig {
                    service_name: unique_service_name(tag),
                    trust_tier: ChannelTrustTier::Trusted,
                    expected_payload_bytes: 4096,
                    chunk_ceiling_bytes: source_ceiling_bytes,
                },
            )
            .expect("the source side wires");
            wire_rust_dest(
                &mut graph,
                &dest_id,
                &dest,
                "in1",
                &link_id,
                "psource/out1",
                dest_delivery,
                &channel,
                &notify_service,
                None,
            )
            .expect("the destination side wires");

            Self {
                graph,
                source_id,
                dest_id,
                link_id,
                source_output: source_output.expect("an output-only mock holds an output writer"),
                dest_input: dest_input.expect("an input-only mock holds input mailboxes"),
                _channel: channel,
            }
        }

        /// What `graph` renders under `metrics` for `proc_id`, or `None` for no key.
        fn rendered_metrics_of(
            &mut self,
            proc_id: &ProcessorUniqueId,
        ) -> Option<serde_json::Value> {
            self.graph
                .traversal_mut()
                .v(proc_id)
                .first()
                .expect("the processor's node must be in the graph")
                .serialize_components()
                .get("metrics")
                .cloned()
        }

        fn write_bags(&self, bag_count: usize) {
            for bag in 0..bag_count {
                self.source_output
                    .write_bytes_with_timestamp(
                        "out1",
                        b"a bag",
                        bag as i64,
                        MessageGroupBoundary::StartsGroup,
                    )
                    .expect("the source publishes onto the wired channel");
            }
        }
    }

    /// What the control plane serves for a dropping run: the destination's node
    /// carries its per-inbound-link dropped-bag counts, live off the mailboxes
    /// that did the evicting.
    ///
    /// Driven through the real seam end to end — the destination-side wiring
    /// the compiler op runs, a real channel service, the source's own
    /// `write_bytes_with_timestamp`, the destination's own `receive_pending` — so what is
    /// asserted is the rendering a `GET /api/graph` reader gets, not a counter
    /// poked by hand. Fail-without-fix: drop the publish step from
    /// `wire_rust_dest` and the node renders no `metrics` at all, so a run that
    /// lost three of its four bags reads exactly like a healthy one.
    #[test]
    fn a_dropping_destinations_node_renders_each_inbound_links_losses() {
        const DESTINATION_MAILBOX_DEPTH: usize = 1;
        const FRAMES_PUBLISHED: usize = 4;

        let mut link = NativeLinkWiredForLossCounting::wire(
            "dropped-bag-counts",
            DeliveryResolution {
                drain_order: crate::iceoryx2::ReadMode::ReadNextInOrder,
                depth: DESTINATION_MAILBOX_DEPTH,
            },
            crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        );
        let dest_id = link.dest_id.clone();
        let link_id = link.link_id.to_string();

        assert_eq!(
            link.rendered_metrics_of(&dest_id).unwrap()["dropped_bags_by_link"],
            serde_json::json!({ link_id.as_str(): 0 }),
            "a wired link that has lost nothing must render a zero, not go missing"
        );

        // Taken off the subscriber after every bag: the subscriber's ring is as
        // deep as the mailbox, so what is counted here is eviction alone.
        for _ in 0..FRAMES_PUBLISHED {
            link.write_bags(1);
            link.dest_input.receive_pending();
        }

        let metrics = link.rendered_metrics_of(&dest_id).unwrap();
        assert_eq!(
            metrics["dropped_bags_by_link"],
            serde_json::json!({ link_id.as_str(): FRAMES_PUBLISHED - DESTINATION_MAILBOX_DEPTH }),
            "the node must read the counts live, off the mailboxes that evicted"
        );
        assert_eq!(
            metrics["frames_dropped"],
            serde_json::json!(FRAMES_PUBLISHED - DESTINATION_MAILBOX_DEPTH),
            "the total must be the per-link counts summed, never a second tally"
        );
        assert!(
            metrics.get("discarded_samples_by_link").is_none(),
            "a link into a port that declares no window contract carries no sample count; \
             got {metrics}"
        );
    }

    /// An `ordered` consumer that stops reading shows exactly the bags its
    /// subscriber ring overwrote, under the link they were lost from.
    ///
    /// The consumer reads one bag, then nothing while the source writes six
    /// past its ring, then receives once: the ring hands over the newest
    /// sixteen and the jump in their sequence numbers is the six it lost. The
    /// mailbox is as deep as the ring, so nothing is evicted there and the
    /// count is the ring's alone. Fail-without-fix: drop the gap from
    /// `receive_pending` and the node renders a zero for a run that lost six.
    #[test]
    fn an_ordered_consumer_that_stops_reading_renders_exactly_the_bags_its_ring_overwrote() {
        const BAGS_PAST_THE_RING: usize = 6;

        let mut link = NativeLinkWiredForLossCounting::wire(
            "ring-overrun/ordered",
            DeliveryProfile::Ordered.resolve(),
            crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        );
        let dest_id = link.dest_id.clone();
        let link_id = link.link_id.to_string();

        link.write_bags(1);
        link.dest_input
            .read_raw("in1")
            .unwrap()
            .expect("the consumer reads its first bag");
        link.write_bags(DeliveryProfile::ORDERED_DEPTH + BAGS_PAST_THE_RING);
        link.dest_input.receive_pending();

        let metrics = link.rendered_metrics_of(&dest_id).unwrap();
        assert_eq!(
            metrics["dropped_bags_by_link"],
            serde_json::json!({ link_id.as_str(): BAGS_PAST_THE_RING }),
        );
        assert_eq!(
            metrics["frames_dropped"],
            serde_json::json!(BAGS_PAST_THE_RING)
        );
        assert_eq!(
            link.dest_input.drain("in1").len(),
            DeliveryProfile::ORDERED_DEPTH,
            "counted plus delivered accounts for every bag written after the first"
        );
    }

    /// A message lost on a running `ordered` link is told to the reader's next
    /// read, beside the group flag the producer wrote, and to no read after it.
    ///
    /// The consumer reads one message, then nothing while the source writes six
    /// past its ring. Fail-without-fix: leave the reads naming no loss and the
    /// consumer reads sixteen messages that look contiguous over a gap of six.
    #[test]
    fn a_message_lost_on_a_running_ordered_link_is_told_to_the_next_read() {
        const BAGS_PAST_THE_RING: usize = 6;

        let mut link = NativeLinkWiredForLossCounting::wire(
            "lost-since-last-read/ordered",
            DeliveryProfile::Ordered.resolve(),
            crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        );
        let dest_id = link.dest_id.clone();
        let link_id = link.link_id.to_string();
        let write_one_message = |group_boundary| {
            link.source_output
                .write_bytes_with_timestamp("out1", b"a message", 0, group_boundary)
                .expect("the source publishes onto the wired channel");
        };

        write_one_message(MessageGroupBoundary::StartsGroup);
        let first = link
            .dest_input
            .read_raw("in1")
            .unwrap()
            .expect("the consumer reads its first message");
        for _ in 0..DeliveryProfile::ORDERED_DEPTH + BAGS_PAST_THE_RING {
            write_one_message(MessageGroupBoundary::ContinuesGroup);
        }
        let after_the_overrun: Vec<_> =
            std::iter::from_fn(|| link.dest_input.read_raw("in1").unwrap()).collect();

        assert_eq!(first.lost_since_last_read, 0);
        assert_eq!(first.group_boundary, MessageGroupBoundary::StartsGroup);
        assert_eq!(after_the_overrun.len(), DeliveryProfile::ORDERED_DEPTH);
        assert_eq!(
            after_the_overrun
                .iter()
                .map(|message| message.lost_since_last_read)
                .collect::<Vec<_>>(),
            std::iter::once(BAGS_PAST_THE_RING as u64)
                .chain(std::iter::repeat_n(0, DeliveryProfile::ORDERED_DEPTH - 1))
                .collect::<Vec<_>>(),
        );
        assert!(
            after_the_overrun
                .iter()
                .all(|message| message.group_boundary == MessageGroupBoundary::ContinuesGroup)
        );
        assert_eq!(
            link.rendered_metrics_of(&dest_id).unwrap()["dropped_bags_by_link"],
            serde_json::json!({ link_id.as_str(): BAGS_PAST_THE_RING }),
            "telling the reader leaves the cumulative count `graph` renders as it was"
        );
    }

    /// A `newest` consumer passing over bags is the profile working, in its
    /// ring and in its mailbox alike, and its link shows no loss for either.
    ///
    /// Fail-without-fix: count the ring gap whatever the read mode and the link
    /// shows six; count every mailbox eviction whatever the read mode and it
    /// shows the evictions of the second burst too.
    #[test]
    fn a_newest_consumer_renders_no_loss_for_bags_passed_over_in_its_ring_or_its_mailbox() {
        const BAGS_PAST_THE_RING: usize = 6;

        let mut link = NativeLinkWiredForLossCounting::wire(
            "ring-overrun/newest",
            DeliveryProfile::Newest.resolve(),
            crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        );
        let dest_id = link.dest_id.clone();
        let link_id = link.link_id.to_string();

        link.write_bags(1);
        link.dest_input.receive_pending();
        link.write_bags(DeliveryProfile::NEWEST_DEPTH + BAGS_PAST_THE_RING);
        link.dest_input.receive_pending();
        for _ in 0..DeliveryProfile::NEWEST_DEPTH + BAGS_PAST_THE_RING {
            link.write_bags(1);
            link.dest_input.receive_pending();
        }

        let metrics = link.rendered_metrics_of(&dest_id).unwrap();
        assert_eq!(
            metrics["dropped_bags_by_link"],
            serde_json::json!({ link_id.as_str(): 0 }),
        );
        assert_eq!(metrics["frames_dropped"], serde_json::json!(0));
    }

    /// A producer that only produces carries metrics too, and a write its
    /// output port refused at the channel ceiling shows under that port — on the
    /// producer, since the refused bag never reached any link.
    ///
    /// Fail-without-fix: attach the metrics from destination wiring alone and
    /// the source node renders no `metrics` key, so a camera refusing every
    /// frame reads as healthy.
    #[test]
    fn a_producers_node_renders_the_writes_its_output_port_refused_at_the_ceiling() {
        const CEILING_BYTES: usize = 1024;

        let mut link = NativeLinkWiredForLossCounting::wire(
            "refused-at-the-ceiling",
            DeliveryProfile::Ordered.resolve(),
            CEILING_BYTES,
        );
        let source_id = link.source_id.clone();
        let dest_id = link.dest_id.clone();
        let link_id = link.link_id.to_string();

        assert_eq!(
            link.rendered_metrics_of(&source_id),
            Some(serde_json::json!({
                "frames_dropped": 0,
                "dropped_bags_by_link": {},
                "refused_bags_by_output_port": { "out1": 0 },
                "written_bags_by_output_port": { "out1": 0 }
            })),
            "a wired output port that has refused nothing renders a zero"
        );

        link.source_output
            .write_bytes_with_timestamp(
                "out1",
                &[0u8; CEILING_BYTES],
                0,
                MessageGroupBoundary::StartsGroup,
            )
            .expect_err("a bag past the ceiling is refused");
        link.write_bags(1);
        link.dest_input.receive_pending();

        assert_eq!(
            link.rendered_metrics_of(&source_id).unwrap()["refused_bags_by_output_port"],
            serde_json::json!({ "out1": 1 }),
        );
        assert_eq!(
            link.rendered_metrics_of(&source_id).unwrap()["written_bags_by_output_port"],
            serde_json::json!({ "out1": 1 }),
            "the refused bag is not counted as written; the one sent is"
        );
        assert_eq!(
            link.rendered_metrics_of(&dest_id).unwrap()["dropped_bags_by_link"],
            serde_json::json!({ link_id.as_str(): 0 }),
            "the refused bag is the producer's loss and no link's"
        );
    }

    /// Every destination is notified and holds a listener, a self-driven one
    /// that never drains it included: iceoryx2 counts repeat notifications in
    /// shared memory and sends no further wakeup until the listener drains, so
    /// an undrained listener costs its source nothing but that count.
    ///
    /// Fail-without-fix: notify only a reactive destination again and the
    /// `manual` destination's link carries no notifier and opens no listener.
    #[test]
    fn a_destination_is_notified_and_listens_whatever_it_runs() {
        use crate::core::test_support::{MockInputOnlyProcessor, MockReactiveInputOnlyProcessor};

        const SELF_DRIVEN: &str = "self-driven";
        const REACTIVE: &str = "reactive";
        for (tag, (source_output, dest_input)) in [
            (
                SELF_DRIVEN,
                wire_one_test_link::<MockInputOnlyProcessor::Processor>(SELF_DRIVEN, false),
            ),
            (
                REACTIVE,
                wire_one_test_link::<MockReactiveInputOnlyProcessor::Processor>(REACTIVE, true),
            ),
        ] {
            assert!(source_output.has_channel_publisher("out1"));
            assert!(dest_input.has_port("in1"));
            assert_eq!(
                source_output.channel_notifier_count("out1"),
                1,
                "a {tag} destination's link must carry its notifier"
            );
            assert!(
                dest_input.has_listener(),
                "a {tag} destination must hold the listener its sources notify"
            );
        }
    }

    fn add_mock_ordered_input_only(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockOrderedInputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            )).expect("the node is named")
            .first()
            .expect("mock_ordered_input_only_processor must be in the registry")
            .id
            .to_string()
    }

    /// Add a link from `source_id`'s `out1` to `dest_id`'s `in1`.
    fn add_link_from_out1_to_in1(
        graph: &mut Graph,
        source_id: &str,
        dest_id: &str,
    ) -> LinkUniqueId {
        add_link_from_out1_to(graph, source_id, dest_id, "in1")
    }

    /// Add a link from `source_id`'s `out1` to `dest_id`'s `dest_port`.
    fn add_link_from_out1_to(
        graph: &mut Graph,
        source_id: &str,
        dest_id: &str,
        dest_port: &str,
    ) -> LinkUniqueId {
        graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(source_id, "out1"),
                InputLinkPortRef::new(dest_id, dest_port),
            )
            .first()
            .expect("the link must exist")
            .id
            .clone()
    }

    /// A graph holding one link, wired by the op, from `out1` to `in1` of two
    /// processors that are both helper stubs, so the engine opens no port of
    /// its own on it. Hands back the source's id and the link.
    fn graph_with_one_wired_link_between_two_helper_stubs() -> (Graph, String, LinkUniqueId) {
        let (graph, source_id, link_id, _) = graph_and_owed_answers_of_a_helper_to_helper_link();
        (graph, source_id, link_id)
    }

    /// The same graph, with the answers both helper ends still owe — which is
    /// what a test plays a helper with, since the op does not wait for them.
    fn graph_and_owed_answers_of_a_helper_to_helper_link() -> (
        Graph,
        String,
        LinkUniqueId,
        Vec<Arc<OutOfProcessLinkWireReply>>,
    ) {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        let answers_owed: Arc<Mutex<Vec<Arc<OutOfProcessLinkWireReply>>>> = Arc::default();
        for helper_id in [&source_id, &dest_id] {
            attach_processor_instance(
                &mut graph,
                helper_id,
                ProcessorInstance::new(Box::new(
                    OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                        RecordingOutOfProcessFarSideLinkDelivery {
                            wire_answers_owed: answers_owed.clone(),
                            ..Default::default()
                        },
                        ProcessExecution::Reactive,
                    ),
                )),
            );
        }
        let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
        open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
            .expect("the helper-to-helper link wires");
        let answers_owed = answers_owed.lock().clone();
        (graph, source_id, link_id, answers_owed)
    }

    /// The depth the live channel `source_id`'s `out1` publishes to was created
    /// at, as a helper opening its own end would find it.
    fn creation_depth_a_helper_opening_out1_finds(source_id: &str) -> usize {
        let channel_service_name = channel_service_name(&OutputLinkPortRef::new(source_id, "out1"))
            .expect("the mock's output port derives a channel name");
        Iceoryx2Node::for_this_test_process()
            .open_or_create_service(
                &channel_service_name,
                MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
                DeliveryProfile::NEWEST_DEPTH,
            )
            .expect("a helper opening at its own port's depth joins or creates the service")
            .channel_service_creation_depth()
    }

    /// A running output port that already feeds a `newest` consumer takes an
    /// `ordered` one, and each reads at its own port's depth.
    ///
    /// Ten bags published while neither reads: the `newest` port's ring holds
    /// four, so its mailbox takes four and evicts none, while the `ordered` port
    /// receives all ten. Fail-without-fix: restore the one-profile-per-channel
    /// refusal and the second `open_iceoryx2_service` is refused; give every
    /// subscriber the service's depth and the `newest` port's mailbox evicts six.
    #[test]
    fn a_newest_and_an_ordered_consumer_share_one_running_output_port_each_at_its_own_depth() {
        use crate::core::test_support::{
            MockInputOnlyProcessor, MockOrderedInputOnlyProcessor, MockOutputOnlyProcessor,
        };
        const BAGS_PUBLISHED_WHILE_NEITHER_CONSUMER_READS: usize = 10;

        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let (_, source_output, _) =
            attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
        let source_output = source_output.expect("an output-only mock holds an output writer");
        let newest_consumer_id = add_mock_input_only(&mut graph);
        let (_, _, newest_consumer_input) = attach_mock_instance::<MockInputOnlyProcessor::Processor>(
            &mut graph,
            &newest_consumer_id,
        );
        let newest_consumer_input =
            newest_consumer_input.expect("an input-only mock holds input mailboxes");
        let ordered_consumer_id = add_mock_ordered_input_only(&mut graph);
        let (_, _, ordered_consumer_input) = attach_mock_instance::<
            MockOrderedInputOnlyProcessor::Processor,
        >(&mut graph, &ordered_consumer_id);
        let ordered_consumer_input =
            ordered_consumer_input.expect("an input-only mock holds input mailboxes");

        let newest_link = add_link_from_out1_to_in1(&mut graph, &source_id, &newest_consumer_id);
        open_iceoryx2_service(&mut graph, &newest_link, &node).expect("the newest consumer wires");
        source_output
            .write_bytes_with_timestamp(
                "out1",
                b"a bag the port carried before the ordered consumer",
                0,
                MessageGroupBoundary::StartsGroup,
            )
            .expect("the port runs with one consumer");
        newest_consumer_input.receive_pending();
        newest_consumer_input.drain("in1");

        let ordered_link = add_link_from_out1_to_in1(&mut graph, &source_id, &ordered_consumer_id);
        open_iceoryx2_service(&mut graph, &ordered_link, &node)
            .expect("an ordered consumer connects onto the running port");

        for bag in 1..=BAGS_PUBLISHED_WHILE_NEITHER_CONSUMER_READS {
            source_output
                .write_bytes_with_timestamp(
                    "out1",
                    b"a bag both consumers are fed",
                    bag as i64,
                    MessageGroupBoundary::StartsGroup,
                )
                .expect("the port publishes to both consumers");
        }
        newest_consumer_input.receive_pending();
        ordered_consumer_input.receive_pending();

        assert_eq!(
            newest_consumer_input.drain("in1").len(),
            DeliveryProfile::NEWEST_DEPTH,
            "the newest port holds its own depth"
        );
        assert_eq!(
            graph
                .traversal_mut()
                .v(ProcessorUniqueId::from(newest_consumer_id.as_str()))
                .first()
                .expect("the newest consumer is in the graph")
                .serialize_components()["metrics"]["dropped_bags_by_link"][newest_link.as_str()],
            serde_json::json!(0),
            "the newest port's ring, not its mailbox, held it to its depth"
        );
        assert_eq!(
            ordered_consumer_input.drain("in1").len(),
            BAGS_PUBLISHED_WHILE_NEITHER_CONSUMER_READS,
            "the ordered port receives every bag its deeper ring holds"
        );
    }

    /// The processor a stream's spawn creates for node `proc_id`, through the
    /// node types the stream resolves.
    fn the_processor_a_spawn_creates_for(graph: &mut Graph, proc_id: &str) -> ProcessorInstance {
        graph
            .node_types_this_stream_resolves()
            .create(
                graph
                    .traversal()
                    .v(proc_id)
                    .first()
                    .expect("the node is in the graph"),
            )
            .expect("the node's type creates its processor")
    }

    /// A native reader reads the catalog entry its upstream output port carries
    /// now off the inbound link the compiler wired, and `graph` renders that
    /// entry under the upstream's port; disconnecting the link leaves the
    /// reader nothing to read. Both processors are created the way a stream's
    /// spawn creates them, so the writer and the mailboxes reach their nodes'
    /// entries only if that path hands them over.
    #[test]
    fn a_native_reader_reads_its_upstreams_current_catalog_entry_off_the_wired_link() {
        use crate::core::descriptors::{CatalogEntry, VideoEntry};

        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let source = the_processor_a_spawn_creates_for(&mut graph, &source_id);
        let source_output = crate::iceoryx2::OutputWriter::from_inner_arc(
            source
                .iceoryx2_output_writer_inner()
                .expect("an output-only mock holds an output writer"),
        );
        attach_processor_instance(&mut graph, &source_id, source);
        let dest_id = add_mock_input_only(&mut graph);
        let dest = the_processor_a_spawn_creates_for(&mut graph, &dest_id);
        let dest_input = crate::iceoryx2::InputMailboxes::from_inner_arc(
            dest.iceoryx2_input_mailboxes_inner()
                .expect("an input-only mock holds input mailboxes"),
        );
        attach_processor_instance(&mut graph, &dest_id, dest);

        let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
        open_iceoryx2_service(&mut graph, &link_id, &node).expect("the link wires");
        assert_eq!(
            dest_input
                .inbound_link_catalog_entry("in1", None)
                .expect("a port fed by one link answers"),
            None,
            "the upstream port declared no entry"
        );

        let encoded_video = CatalogEntry::from(VideoEntry {
            codec: "avc1.64001f".to_string(),
            coded_width: Some(1280),
            coded_height: Some(720),
            description: None,
        });
        source_output
            .set_catalog_entry("out1", encoded_video.clone())
            .expect("an entry hang reads replaces the port's");
        let inbound_link_name = dest_input
            .inbound_link_names("in1")
            .pop()
            .expect("the wired link is named");
        assert_eq!(
            dest_input
                .inbound_link_catalog_entry("in1", Some(&inbound_link_name))
                .expect("the named link answers"),
            Some(encoded_video.clone())
        );
        let rendered_source =
            serde_json::to_value(crate::core::json_schema::ProcessorNodeOutput::from(
                graph
                    .traversal()
                    .v(ProcessorUniqueId::from(source_id.as_str()))
                    .first()
                    .expect("the source is in the graph"),
            ))
            .expect("the node renders");
        assert_eq!(
            rendered_source["ports"]["outputs"][0]["catalog_entry"],
            serde_json::to_value(&encoded_video).expect("the entry serializes")
        );

        source_output
            .set_catalog_entry(
                "out1",
                CatalogEntry {
                    section: crate::core::descriptors::CatalogSection::Video,
                    entry: serde_json::json!({"codedWidth": 1280}),
                },
            )
            .expect_err("an entry with no codec is refused");

        close_iceoryx2_service(&mut graph, &link_id).expect("the link disconnects");
        assert_eq!(
            dest_input
                .inbound_link_catalog_entry("in1", None)
                .expect("an unfed port answers"),
            None
        );
    }

    /// The first consumer of a channel does not size it: one first wired to a
    /// `newest` port is created at the depth any consumer needs.
    ///
    /// Fail-without-fix: size the service by the first consumer's profile and
    /// the link's held service states four.
    #[test]
    fn a_channel_first_wired_to_a_newest_consumer_is_created_deep_enough_for_any_consumer() {
        use crate::core::test_support::{MockInputOnlyProcessor, MockOutputOnlyProcessor};

        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
        let newest_consumer_id = add_mock_input_only(&mut graph);
        attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut graph, &newest_consumer_id);
        let newest_link = add_link_from_out1_to_in1(&mut graph, &source_id, &newest_consumer_id);

        open_iceoryx2_service(&mut graph, &newest_link, &node).expect("the newest consumer wires");

        let held_creation_depth = graph
            .traversal_mut()
            .e(&newest_link)
            .first()
            .expect("the link is in the graph")
            .get::<Iceoryx2ServicesHeldOpenForLinkComponent>()
            .expect("a wired link holds its services")
            .channel_data_service
            .channel_service_creation_depth();
        assert_eq!(held_creation_depth, DeliveryProfile::ORDERED_DEPTH);
    }

    /// On a link between two helpers the engine opens no port of its own, yet
    /// it still decides the channel's size: a helper opening its end — first,
    /// and asking for only its own port's depth — joins the service the engine
    /// created.
    ///
    /// Fail-without-fix: drop the held services from the link and the service
    /// is gone when the op returns, so the helper creates it at four.
    #[test]
    fn a_helper_opening_first_joins_the_channel_the_engine_created_between_two_helpers() {
        let (_graph_holding_the_link, source_id, _) =
            graph_with_one_wired_link_between_two_helper_stubs();

        assert_eq!(
            creation_depth_a_helper_opening_out1_finds(&source_id),
            DeliveryProfile::ORDERED_DEPTH,
        );
    }

    /// What one link reports to `graph`, and why where that is an error.
    fn link_state_in_graph(
        graph: &Graph,
        link_id: &LinkUniqueId,
    ) -> (crate::core::json_schema::LinkStateOutput, Option<String>) {
        let link = graph
            .traversal()
            .e(link_id)
            .first()
            .expect("the link must be in the graph");
        let rendered = crate::core::json_schema::LinkOutput::of_a_link(
            link,
            &crate::core::json_schema::NodeNamesByProcessorId::holding_no_node(),
        );
        (rendered.state, rendered.error_reason)
    }

    /// A link handed to a helper is `pending` until that helper says it opened
    /// its port, and both ends of a helper-to-helper link have to say so.
    ///
    /// Fail-without-fix: stamp `Wired` in the op as before and the first
    /// assertion reads `wired` with neither helper having opened anything.
    #[test]
    fn a_link_handed_to_a_helper_is_pending_until_the_helper_says_it_opened_its_port() {
        use crate::core::json_schema::LinkStateOutput;
        let (graph, _, link_id, answers_owed) = graph_and_owed_answers_of_a_helper_to_helper_link();
        let [source_answer, dest_answer] = &answers_owed[..] else {
            panic!(
                "both helper ends owe an answer; got {} ",
                answers_owed.len()
            );
        };

        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Pending, None),
            "`connect` returns before either helper has opened anything"
        );

        source_answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Pending, None),
            "one end's answer does not wire a link the other end never opened"
        );

        dest_answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Wired, None)
        );
    }

    /// A helper that could not open its port leaves the link in error with its
    /// own reason, and `graph` renders that reason.
    #[test]
    fn a_helper_that_cannot_open_its_port_leaves_the_link_in_error_with_its_reason() {
        use crate::core::json_schema::LinkStateOutput;
        let (graph, _, link_id, answers_owed) = graph_and_owed_answers_of_a_helper_to_helper_link();

        answers_owed[1].note_the_far_sides_answer(
            OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: "ExceedsMaxSupportedSubscribers".to_string(),
            },
        );

        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (
                LinkStateOutput::Error,
                Some("ExceedsMaxSupportedSubscribers".to_string())
            ),
            "a refusal is the link's answer even while the other end is still silent, and the \
             reason is what the caller of a live `connect` has to read"
        );

        answers_owed[0].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        assert_eq!(
            link_state_in_graph(&graph, &link_id).0,
            LinkStateOutput::Error,
            "the other end opening does not rescue a link one end refused"
        );
    }

    /// The reason lives as long as the link does, and goes with it.
    #[test]
    fn a_disconnected_link_stops_rendering_the_reason_it_was_refused_for() {
        use crate::core::json_schema::LinkStateOutput;
        let (mut graph, _, link_id, answers_owed) =
            graph_and_owed_answers_of_a_helper_to_helper_link();
        answers_owed[0].note_the_far_sides_answer(
            OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: "no such service".to_string(),
            },
        );

        close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Disconnected, None),
        );
    }

    /// A link wholly inside the app process waits on nobody and is wired the
    /// moment the op returns — the startup envelope's arm is the same one,
    /// since a helper with no bridge yet hands back no answer to wait on.
    #[test]
    fn a_link_no_helper_has_to_answer_for_is_wired_as_soon_as_it_is_opened() {
        use crate::core::json_schema::LinkStateOutput;
        use crate::core::test_support::{MockInputOnlyProcessor, MockOutputOnlyProcessor};

        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
        attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut graph, &dest_id);
        let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);

        open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
            .expect("an app-process link wires");

        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Wired, None)
        );
    }

    /// A link whose source and destination are the same helper waits on two
    /// answers, because the engine hands that helper both of its ends.
    ///
    /// `connect` accepts an output wired to its own processor's input, so this
    /// is reachable rather than theoretical. Fail-without-fix: hand the link
    /// one cell and the first answer reports it `wired` while the other end
    /// may not have opened at all.
    #[test]
    fn a_link_between_one_helpers_own_ports_waits_on_both_of_its_ends() {
        use crate::core::json_schema::LinkStateOutput;
        crate::core::test_support::ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let helper_id = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the node is named")
            .first()
            .expect("the four-port mock must be in the registry")
            .id
            .to_string();
        let answers_owed: Arc<Mutex<Vec<Arc<OutOfProcessLinkWireReply>>>> = Arc::default();
        attach_processor_instance(
            &mut graph,
            &helper_id,
            ProcessorInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery {
                        wire_answers_owed: answers_owed.clone(),
                        ..Default::default()
                    },
                    ProcessExecution::Reactive,
                ),
            )),
        );
        let link_id = add_link_from_out1_to_in1(&mut graph, &helper_id, &helper_id);

        open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
            .expect("a link between one helper's own ports wires");

        let answers_owed = answers_owed.lock().clone();
        assert_eq!(
            answers_owed.len(),
            2,
            "the engine hands this helper both ends of the link, so it owes two answers"
        );
        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Pending, None)
        );

        answers_owed[0].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Pending, None),
            "one end opening does not wire a link whose other end is the same helper"
        );

        answers_owed[1].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Wired, None)
        );
    }

    /// How long the op may take while a helper's processor lock is held before
    /// it counts as waiting on that lock.
    const LONGEST_A_WIRING_OP_MAY_TAKE_BESIDE_A_HELPER_SETTING_UP: std::time::Duration =
        std::time::Duration::from_secs(5);

    /// Run `wiring_op` on another thread while this one holds both processor
    /// locks, the way each helper's spawn thread holds its own across a setup
    /// still importing, and hand back what it returned — or `None` if it was
    /// still waiting when the budget ran out, once the locks are let go.
    fn run_while_both_processor_locks_are_held<T: Send>(
        source: &Arc<Mutex<ProcessorInstance>>,
        dest: &Arc<Mutex<ProcessorInstance>>,
        wiring_op: impl FnOnce() -> T + Send,
    ) -> Option<T> {
        let source_setting_up = source.lock();
        let dest_setting_up = dest.lock();
        std::thread::scope(|scope| {
            let (wiring_op_returned_tx, wiring_op_returned_rx) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let _ = wiring_op_returned_tx.send(wiring_op());
            });
            let returned = wiring_op_returned_rx
                .recv_timeout(LONGEST_A_WIRING_OP_MAY_TAKE_BESIDE_A_HELPER_SETTING_UP)
                .ok();
            drop(source_setting_up);
            drop(dest_setting_up);
            returned
        })
    }

    /// Two helper stubs whose far sides record into one delivery, and a link
    /// from the source's `out1` to the destination's `in1` not yet wired.
    struct TwoHelperStubsAndAnUnwiredLinkBetweenThem {
        graph: Graph,
        source: Arc<Mutex<ProcessorInstance>>,
        dest: Arc<Mutex<ProcessorInstance>>,
        link_id: LinkUniqueId,
        far_side: RecordingOutOfProcessFarSideLinkDelivery,
    }

    impl TwoHelperStubsAndAnUnwiredLinkBetweenThem {
        fn new() -> Self {
            let mut graph = Graph::new();
            let source_id = add_mock_output_only(&mut graph);
            let dest_id = add_mock_input_only(&mut graph);
            let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
            let mut attach_a_helper_stub = |proc_id: &str| {
                attach_processor_instance(
                    &mut graph,
                    proc_id,
                    ProcessorInstance::new(Box::new(
                        OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                            far_side.clone(),
                            ProcessExecution::Reactive,
                        ),
                    )),
                )
            };
            let source = attach_a_helper_stub(&source_id);
            let dest = attach_a_helper_stub(&dest_id);
            let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
            Self {
                graph,
                source,
                dest,
                link_id,
                far_side,
            }
        }
    }

    /// A live `connect` onto helpers still inside their setup — their processor
    /// locks held — wires the link without waiting for either lock, so the
    /// graph lock the compiler holds around it is never held across an import.
    ///
    /// Fail-without-fix: classify an out-of-process endpoint through its
    /// processor's lock and the op waits out the budget.
    #[test]
    fn a_link_between_helpers_still_setting_up_is_handed_over_without_their_processor_locks() {
        use crate::core::json_schema::LinkStateOutput;
        let TwoHelperStubsAndAnUnwiredLinkBetweenThem {
            mut graph,
            source,
            dest,
            link_id,
            far_side,
        } = TwoHelperStubsAndAnUnwiredLinkBetweenThem::new();

        let wired = run_while_both_processor_locks_are_held(&source, &dest, || {
            open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
        })
        .expect("the op returns while both helpers still hold their processor locks");

        wired.expect("the link wires");
        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Pending, None)
        );
        let handed_over = far_side.late_wired_links.lock();
        let directions: Vec<crate::core::PortDirection> = handed_over
            .iter()
            .map(|(direction, _)| *direction)
            .collect();
        assert_eq!(
            directions,
            [
                crate::core::PortDirection::Output,
                crate::core::PortDirection::Input
            ],
            "both helpers are handed their end of the link"
        );
    }

    /// A disconnect of a link between helpers whose processor locks are held
    /// reclaims both ends without waiting for either lock.
    ///
    /// Fail-without-fix: reclaim an out-of-process endpoint through its
    /// processor's lock and the op waits out the budget.
    #[test]
    fn a_disconnect_between_helpers_still_setting_up_reclaims_both_ends_without_their_processor_locks()
     {
        use crate::core::json_schema::LinkStateOutput;
        let TwoHelperStubsAndAnUnwiredLinkBetweenThem {
            mut graph,
            source,
            dest,
            link_id,
            far_side,
        } = TwoHelperStubsAndAnUnwiredLinkBetweenThem::new();
        open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
            .expect("the link wires");

        let closed = run_while_both_processor_locks_are_held(&source, &dest, || {
            close_iceoryx2_service(&mut graph, &link_id)
        })
        .expect("the op returns while both helpers still hold their processor locks");

        closed.expect("the disconnect succeeds");
        assert_eq!(
            link_state_in_graph(&graph, &link_id),
            (LinkStateOutput::Disconnected, None)
        );
        assert_eq!(
            far_side.reclaimed_links.lock().len(),
            2,
            "both helpers are told to drop their end of the link"
        );
    }

    /// A disconnected link lets go of the services it held, so a channel whose
    /// last link went is created afresh by whoever opens it next.
    ///
    /// Fail-without-fix: keep the held services on a disconnected link and the
    /// reopen below still finds the old service's depth.
    #[test]
    fn a_disconnected_link_releases_the_services_it_held() {
        let (mut graph, source_id, link_id) = graph_with_one_wired_link_between_two_helper_stubs();

        close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

        assert_eq!(
            creation_depth_a_helper_opening_out1_finds(&source_id),
            DeliveryProfile::NEWEST_DEPTH,
            "nothing may still hold the channel of a link that is gone"
        );
    }

    /// A link reading another stream is removed the way the compiler removes
    /// any link: it is closed without a warning and its edge leaves the graph,
    /// while its reader's other links — one into the same input from a source
    /// in this stream, one out of the reader's own output — stay wired, the
    /// reader's output keeping its publisher and that link's notifier.
    ///
    /// Fail-without-fix: close the edge's two graph endpoints, which for this
    /// link are both the reader, and the reader's own links are unwired.
    #[test]
    fn removing_a_link_reading_another_stream_takes_its_edge_and_leaves_its_readers_other_links_wired()
     {
        use crate::core::compiler::compiler::unwire_and_remove_a_link;
        use crate::core::graph::OutputPortInAnotherStreamOnThisMachine;
        use crate::core::pubsub::LoadedStreamIdentity;
        use crate::core::runtime::LoadedStreamTag;
        use crate::core::test_support::{
            MockInputOnlyProcessor, MockOutputOnlyProcessor, MockProcessor,
        };

        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
        let reader_id = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                MockProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the node is named")
            .first()
            .expect("the reader is added")
            .id
            .to_string();
        let (_, reader_output, reader_input) =
            attach_mock_instance::<MockProcessor::Processor>(&mut graph, &reader_id);
        let reader_output = reader_output.expect("the reader holds an output writer");
        let reader_input = reader_input.expect("the reader holds input mailboxes");
        let downstream_id = add_mock_input_only(&mut graph);
        attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut graph, &downstream_id);

        let link_into_the_reader = add_link_from_out1_to_in1(&mut graph, &source_id, &reader_id);
        let link_out_of_the_reader =
            add_link_from_out1_to_in1(&mut graph, &reader_id, &downstream_id);
        for link_id in [&link_into_the_reader, &link_out_of_the_reader] {
            open_iceoryx2_service(&mut graph, link_id, &node).expect("a link in this stream wires");
        }
        let link_reading_another_stream = graph
            .traversal_mut()
            .add_e_reading_another_stream(
                OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "out1").unwrap(),
                InputLinkPortRef::new(reader_id.as_str(), "in1"),
            )
            .first()
            .expect("the link reading another stream is added")
            .id
            .clone();

        let graph = parking_lot::RwLock::new(graph);
        let the_stream = LoadedStreamIdentity {
            runtime_id: "Rremoval".to_string(),
            stream_name: "b".to_string(),
            stream_tag: LoadedStreamTag::numbered_for_a_test(1),
        };
        let (unwired, warnings) = CapturedTracingWarnings::captured_while(|| {
            unwire_and_remove_a_link(&graph, &link_reading_another_stream, &the_stream)
        });

        assert!(unwired.expect("the link is removed"));
        assert!(warnings.is_empty(), "{warnings:?}");
        let graph = graph.read();
        assert!(!graph.traversal().e(&link_reading_another_stream).exists());
        for link_id in [&link_into_the_reader, &link_out_of_the_reader] {
            assert!(
                matches!(
                    graph
                        .traversal()
                        .e(link_id)
                        .first()
                        .and_then(|link| link.get::<LinkStateComponent>())
                        .map(|state| state.0),
                    Some(LinkState::Wired)
                ),
                "{link_id} stays wired"
            );
        }
        assert_eq!(reader_input.inbound_link_names("in1").len(), 1);
        assert!(reader_output.has_channel_publisher("out1"));
        assert_eq!(reader_output.channel_notifier_count("out1"), 1);
    }

    fn add_mock_reactive_input_only(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockReactiveInputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            )).expect("the node is named")
            .first()
            .expect("mock_reactive_input_only_processor must be in the registry")
            .id
            .to_string()
    }

    fn add_mock_windowed_audio_consumer(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockWindowedAudioConsumerProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            )).expect("the node is named")
            .first()
            .expect("mock_windowed_audio_consumer_processor must be in the registry")
            .id
            .to_string()
    }

    fn add_mock_device_matched_audio_consumer(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockDeviceMatchedAudioConsumerProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            )).expect("the node is named")
            .first()
            .expect("mock_device_matched_audio_consumer_processor must be in the registry")
            .id
            .to_string()
    }

    /// A port declaring the five values resolves to them at wire time, and the
    /// mailbox that port gets is sized by the contract rather than the profile.
    #[test]
    fn a_windowed_destinations_contract_resolves_at_wire_time() {
        let mut graph = Graph::new();
        let dest_id = add_mock_windowed_audio_consumer(&mut graph);

        let windowing =
            audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "audio")
                .expect("a declared contract resolves")
                .expect("the port declares one");

        let AudioWindowDeclarationOfAnInputPort::StatedOutright(contract) = windowing else {
            panic!("a port stating five values resolves to them at wire time");
        };
        assert_eq!(contract.sample_rate, 16_000);
        assert_eq!(contract.channels, Some(1));
        assert_eq!(contract.window_size, 512);
        assert_eq!(contract.hop, 512);
    }

    /// A port with no contract resolves to none, and nothing about it moves.
    #[test]
    fn a_port_declaring_no_contract_resolves_to_none() {
        let mut graph = Graph::new();
        let dest_id = add_mock_reactive_input_only(&mut graph);

        let windowing =
            audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "in1")
                .expect("resolution succeeds");
        assert!(windowing.is_none());
    }

    /// `match_device` settles at `setup()` from the device stream the declaring
    /// processor opened — and the compiler wires every link before it releases
    /// any processor into `setup()`. So wire time is not where a sentinel is
    /// judged: the port is wired awaiting its device, and the refusal belongs
    /// where nothing can settle it (a helper-placed destination) or where
    /// nothing did (after `setup()` returned).
    #[test]
    fn a_match_device_contract_wires_awaiting_its_device_rather_than_refusing() {
        let mut graph = Graph::new();
        let dest_id = add_mock_device_matched_audio_consumer(&mut graph);

        let windowing =
            audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "audio")
                .expect("a sentinel is not a wiring error by itself")
                .expect("the port declares one");

        assert_eq!(
            windowing,
            AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream
        );
    }

    /// The whole loop the sentinel needs, at the seam that has to close it: an
    /// app-process destination declaring `match_device` is wired awaiting its
    /// device, its node is given the shared settled contracts, and once its own
    /// `setup()` settles one `graph` renders those values on the port that
    /// settled them.
    ///
    /// Mentally revert the publish and the node keeps rendering the sentinel
    /// for the whole run — a port whose `graph` entry is a declaration nobody
    /// can act on rather than the format it is actually converting to.
    #[test]
    fn a_settled_contract_reaches_graph_on_the_port_that_settled_it() {
        use crate::core::test_support::MockDeviceMatchedAudioConsumerProcessor;

        let mut graph = Graph::new();
        let dest_id = add_mock_device_matched_audio_consumer(&mut graph);
        let dest_unique_id: ProcessorUniqueId = dest_id.as_str().into();
        let (dest, _, dest_input) = attach_mock_instance::<
            MockDeviceMatchedAudioConsumerProcessor::Processor,
        >(&mut graph, &dest_unique_id.to_string());
        let dest_input = dest_input.expect("a windowed consumer holds input mailboxes");

        let (channel, notify_service) =
            open_test_link_services("match-device-graph", WINDOWED_PORT_SUBSCRIBER_RING_DEPTH);
        wire_rust_dest(
            &mut graph,
            &dest_unique_id,
            &dest,
            "audio",
            &"L-match-device".into(),
            "psource/audio_out",
            DeliveryProfile::Ordered.resolve(),
            &channel,
            &notify_service,
            Some(AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream),
        )
        .expect("a sentinel wires rather than refusing");

        assert_eq!(
            dest_input.input_ports_still_awaiting_their_device_stream_format(),
            vec!["audio".to_string()],
            "the wiring runs before setup(), so the port waits rather than windowing"
        );
        assert_eq!(
            rendered_audio_window_of(&mut graph, &dest_unique_id),
            serde_json::json!({ "resolved_from": "match_device" }),
            "nothing has opened a device yet, and a guess in its place would be a lie"
        );

        dest_input
            .settle_a_ports_device_matched_audio_window_contract(
                "audio",
                &crate::iceoryx2::AudioWindowContractMatchingADeviceStream {
                    device_stream_format: crate::core::context::AudioStreamFormat {
                        sample_rate: 44_100,
                        channels: 2,
                        sample_format: crate::core::context::AudioSampleFormat::F32,
                    },
                    window_size_in_per_channel_samples: 441,
                    hop_in_per_channel_samples: 441,
                },
            )
            .expect("the processor's own setup() settles it");

        assert_eq!(
            rendered_audio_window_of(&mut graph, &dest_unique_id),
            serde_json::json!({
                "resolved_from": "device",
                "sample_rate": 44_100,
                "channels": 2,
                "dtype": "f32",
                "window_size": 441,
                "hop": 441,
            }),
            "graph renders what the device gave, live, off the port that settled it"
        );
    }

    /// One node's `"audio"` input port as `graph` renders its window contract.
    fn rendered_audio_window_of(
        graph: &mut Graph,
        proc_id: &ProcessorUniqueId,
    ) -> serde_json::Value {
        let node = graph
            .traversal_mut()
            .v(proc_id)
            .first()
            .expect("the node must be in the graph");
        serde_json::to_value(crate::core::json_schema::ProcessorNodeOutput::from(node))
            .expect("a node renders")["ports"]["inputs"][0]["audio_window"]
            .clone()
    }

    /// A `match_device` port on a helper-placed destination is refused where it
    /// is wired, not left to wait.
    ///
    /// A child can never settle one: the format comes from a device stream a
    /// processor opens in the app process, and the wiring envelope carries five
    /// resolved values or nothing. Waiting for a `setup()` that has no way to
    /// answer would be a port that silently hands its reader nothing for the
    /// whole run, which is the failure this refusal exists instead of.
    #[test]
    fn a_match_device_port_on_a_helper_placed_destination_is_refused_at_wire_time() {
        let mut graph = Graph::new();
        let dest_id = add_mock_device_matched_audio_consumer(&mut graph);
        attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );

        let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
            .expect("a helper stub's node carries its link wiring");
        let refusal = wire_subprocess_dest(
            &mut graph,
            &dest_link_wiring,
            &dest_id.as_str().into(),
            "audio",
            "pabc/out1",
            "pdef/notify",
            DeliveryProfile::Ordered.resolve(),
            sizing_of_a_two_subscriber_test_channel(),
            1,
            &"L-helper-windowed".into(),
            Some(AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream),
        )
        .expect_err("a helper child cannot settle a sentinel")
        .to_string();

        assert!(
            refusal.contains("match_device")
                && refusal.contains("setup()")
                && refusal.contains("audio"),
            "the refusal must name the sentinel, where it resolves, and the port; got {refusal}"
        );
    }

    /// A contract the parent settled rides the envelope as five values, so the
    /// child opens its own stage on the same numbers and never sees a sentinel.
    #[test]
    fn a_settled_contract_reaches_a_helper_placed_destination_as_five_values() {
        let mut graph = Graph::new();
        let dest_id = add_mock_device_matched_audio_consumer(&mut graph);
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );

        let settled = crate::iceoryx2::ResolvedAudioWindowContract::from_a_device_stream_format(
            &crate::iceoryx2::AudioWindowContractMatchingADeviceStream {
                device_stream_format: crate::core::context::AudioStreamFormat {
                    sample_rate: 48_000,
                    channels: 2,
                    sample_format: crate::core::context::AudioSampleFormat::F32,
                },
                window_size_in_per_channel_samples: 480,
                hop_in_per_channel_samples: 480,
            },
        )
        .expect("a device format settles a contract");

        let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
            .expect("a helper stub's node carries its link wiring");
        wire_subprocess_dest(
            &mut graph,
            &dest_link_wiring,
            &dest_id.as_str().into(),
            "audio",
            "pabc/out1",
            "pdef/notify",
            DeliveryProfile::Ordered.resolve(),
            sizing_of_a_two_subscriber_test_channel(),
            1,
            &"L-helper-settled".into(),
            Some(AudioWindowDeclarationOfAnInputPort::StatedOutright(settled)),
        )
        .expect("a settled contract renders onto the envelope");

        let recorded = dest_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports();
        assert_eq!(
            recorded["inputs"][0]["audio_window"],
            serde_json::json!({
                "sample_rate": 48_000,
                "channels": 2,
                "dtype": "f32",
                "window_size": 480,
                "hop": 480,
            }),
            "the child reads five values, never the sentinel that produced them"
        );
    }

    /// Fan-in legally interleaves N producers' blocks in one mailbox, and two
    /// sample streams interleaved into one accumulator is not a mix — it is
    /// garbage windows. A windowed port takes exactly one inbound link.
    #[test]
    fn a_second_inbound_link_into_a_windowed_port_is_refused_naming_the_port_and_both_links() {
        let mut graph = Graph::new();
        let first_source = add_mock_output_only(&mut graph);
        let second_source = add_mock_output_only(&mut graph);
        let dest_id = add_mock_windowed_audio_consumer(&mut graph);

        let mut wired = Vec::new();
        for source in [&first_source, &second_source] {
            let link = graph
                .traversal_mut()
                .add_e(
                    OutputLinkPortRef::new(source, "out1"),
                    InputLinkPortRef::new(&dest_id, "audio"),
                )
                .first()
                .expect("the link is added")
                .id
                .to_string();
            wired.push(link);
        }

        let dest_unique_id: ProcessorUniqueId = dest_id.as_str().into();
        // The first link is fine on its own.
        let second_link: LinkUniqueId = wired[1].as_str().into();
        let refusal = refuse_a_second_inbound_link_into_a_windowed_port(
            &mut graph,
            &dest_unique_id,
            "audio",
            &second_link,
        )
        .expect_err("a windowed port accepts exactly one inbound link");

        let rendered = refusal.to_string();
        assert!(
            rendered.contains("audio")
                && rendered.contains(&wired[0])
                && rendered.contains(&wired[1]),
            "the refusal must name the port and both links; got {rendered}"
        );
    }

    /// The port's only inbound link must not refuse itself.
    #[test]
    fn the_one_inbound_link_a_windowed_port_takes_is_not_refused() {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_windowed_audio_consumer(&mut graph);
        let link = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&source_id, "out1"),
                InputLinkPortRef::new(&dest_id, "audio"),
            )
            .first()
            .expect("the link is added")
            .id
            .to_string();

        refuse_a_second_inbound_link_into_a_windowed_port(
            &mut graph,
            &dest_id.as_str().into(),
            "audio",
            &link.as_str().into(),
        )
        .expect("one inbound link is what a windowed port takes");
    }

    /// A link on its way out of the graph is not a second inbound one, or a
    /// disconnect followed by a reconnect would refuse itself.
    #[test]
    fn a_disconnected_link_does_not_count_against_a_windowed_ports_one_inbound_link() {
        let mut graph = Graph::new();
        let old_source = add_mock_output_only(&mut graph);
        let new_source = add_mock_output_only(&mut graph);
        let dest_id = add_mock_windowed_audio_consumer(&mut graph);

        let departed = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&old_source, "out1"),
                InputLinkPortRef::new(&dest_id, "audio"),
            )
            .first()
            .expect("the link is added")
            .id
            .to_string();
        graph
            .traversal_mut()
            .e(LinkUniqueId::from(departed.as_str()))
            .first_mut()
            .expect("the departing link is in the graph")
            .insert(LinkStateComponent(LinkState::Disconnected));

        let reconnecting = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(&new_source, "out1"),
                InputLinkPortRef::new(&dest_id, "audio"),
            )
            .first()
            .expect("the link is added")
            .id
            .to_string();

        refuse_a_second_inbound_link_into_a_windowed_port(
            &mut graph,
            &dest_id.as_str().into(),
            "audio",
            &reconnecting.as_str().into(),
        )
        .expect("a reconnect past a disconnected link is one inbound link, not two");
    }

    /// The depth the channel a wired link holds open was created at.
    fn creation_depth_of_the_channel_held_by(graph: &Graph, link_id: &LinkUniqueId) -> usize {
        graph
            .traversal()
            .e(link_id)
            .first()
            .expect("the link is in the graph")
            .get::<Iceoryx2ServicesHeldOpenForLinkComponent>()
            .expect("a wired link holds its services")
            .channel_data_service
            .channel_service_creation_depth()
    }

    /// `frames` of mono audio at 16 kHz, stamped at `first_sample_timestamp_ns`,
    /// as the bag body a source writes.
    fn a_mono_16k_audio_block(frames: usize, first_sample_timestamp_ns: i64) -> Vec<u8> {
        crate::iceoryx2::encode_an_audio_block_onto_the_wire(
            &vec![0.25; frames],
            16_000,
            1,
            frames as u32,
            crate::iceoryx2::AudioBlockSampleDtype::F32,
            first_sample_timestamp_ns,
        )
        .expect("an audio block encodes")
    }

    /// An app-process windowed consumer added to the graph and linked, unwired,
    /// from a source's `out1` into its `audio` port.
    struct WindowedConsumerLinkedFromOut1 {
        dest_id: String,
        link_id: LinkUniqueId,
        dest_input: Arc<crate::iceoryx2::InputMailboxesInner>,
    }

    fn add_a_windowed_consumer_linked_from_out1(
        graph: &mut Graph,
        source_id: &str,
    ) -> WindowedConsumerLinkedFromOut1 {
        use crate::core::test_support::MockWindowedAudioConsumerProcessor;

        let dest_id = add_mock_windowed_audio_consumer(graph);
        let (_, _, dest_input) =
            attach_mock_instance::<MockWindowedAudioConsumerProcessor::Processor>(graph, &dest_id);
        let link_id = add_link_from_out1_to(graph, source_id, &dest_id, "audio");
        WindowedConsumerLinkedFromOut1 {
            dest_id,
            link_id,
            dest_input: dest_input.expect("a windowed consumer holds input mailboxes"),
        }
    }

    /// An app-process source whose `out1` is linked, unwired, to an app-process
    /// `newest` consumer. Hands back the source's id and the link.
    fn add_a_source_linked_to_a_plain_consumer(graph: &mut Graph) -> (String, LinkUniqueId) {
        use crate::core::test_support::{MockInputOnlyProcessor, MockOutputOnlyProcessor};

        let source_id = add_mock_output_only(graph);
        attach_mock_instance::<MockOutputOnlyProcessor::Processor>(graph, &source_id);
        let plain_consumer_id = add_mock_input_only(graph);
        attach_mock_instance::<MockInputOnlyProcessor::Processor>(graph, &plain_consumer_id);
        let plain_link = add_link_from_out1_to_in1(graph, &source_id, &plain_consumer_id);
        (source_id, plain_link)
    }

    /// An app-process source wired through the op to an app-process windowed
    /// consumer, with the handles a test drives the link through.
    struct NativeLinkIntoAWindowedPort {
        graph: Graph,
        dest_id: ProcessorUniqueId,
        link_id: LinkUniqueId,
        source_output: Arc<crate::iceoryx2::OutputWriterInner>,
        dest_input: Arc<crate::iceoryx2::InputMailboxesInner>,
    }

    impl NativeLinkIntoAWindowedPort {
        fn wire() -> Self {
            use crate::core::test_support::MockOutputOnlyProcessor;

            let mut graph = Graph::new();
            let source_id = add_mock_output_only(&mut graph);
            let (_, source_output, _) =
                attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
            let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);
            open_iceoryx2_service(
                &mut graph,
                &windowed.link_id,
                &Iceoryx2Node::for_this_test_process(),
            )
            .expect("the windowed consumer wires");

            Self {
                graph,
                dest_id: windowed.dest_id.as_str().into(),
                link_id: windowed.link_id,
                source_output: source_output.expect("an output-only mock holds an output writer"),
                dest_input: windowed.dest_input,
            }
        }

        fn rendered_metrics_of_the_destination(&self) -> serde_json::Value {
            self.graph
                .traversal()
                .v(&self.dest_id)
                .first()
                .expect("the destination's node must be in the graph")
                .serialize_components()["metrics"]
                .clone()
        }
    }

    /// A channel created for a windowed destination is created at the windowed
    /// ring depth, never at the profile's and never at its mailbox's.
    ///
    /// Fail-without-fix: keep creating every channel at `ORDERED_DEPTH` and the
    /// held service states 16.
    #[test]
    fn a_channel_created_for_a_windowed_destination_holds_the_windowed_ring_depth() {
        let link = NativeLinkIntoAWindowedPort::wire();

        assert_eq!(
            creation_depth_of_the_channel_held_by(&link.graph, &link.link_id),
            WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
        );
    }

    /// A windowed consumer added beside a plain one before either is wired sizes
    /// the channel even when the compiler opens the plain link first — which is
    /// the fix the live refusal names.
    #[test]
    fn a_windowed_consumer_added_beside_a_plain_one_sizes_the_channel_even_when_the_plain_link_opens_first()
     {
        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let (source_id, plain_link) = add_a_source_linked_to_a_plain_consumer(&mut graph);
        let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);

        open_iceoryx2_service(&mut graph, &plain_link, &node).expect("the plain consumer wires");
        open_iceoryx2_service(&mut graph, &windowed.link_id, &node)
            .expect("the windowed consumer joins the channel created for it");

        assert_eq!(
            creation_depth_of_the_channel_held_by(&graph, &plain_link),
            WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
        );
    }

    /// A windowed consumer connected onto a channel already running at the
    /// profile's depth is refused by name, before any port is opened for it.
    ///
    /// Fail-without-fix: drop the refusal and iceoryx2 refuses the 64-slot
    /// subscriber with an error naming neither the port nor the fix.
    #[test]
    fn a_windowed_consumer_connected_onto_a_running_shallower_channel_is_refused_naming_both_depths()
     {
        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let (source_id, plain_link) = add_a_source_linked_to_a_plain_consumer(&mut graph);
        open_iceoryx2_service(&mut graph, &plain_link, &node).expect("the plain consumer wires");
        let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);

        let refusal = open_iceoryx2_service(&mut graph, &windowed.link_id, &node)
            .expect_err("a windowed port cannot read through a ring its channel cannot hold")
            .to_string();

        assert_the_refusal_names_the_port_the_link_both_depths_and_the_fix(
            &refusal,
            &windowed.link_id,
        );
        assert!(
            !windowed.dest_input.has_subscribers(),
            "a refused link leaves no port half-wired"
        );
    }

    /// A windowed-onto-shallow refusal names the `audio` port, the link, the
    /// channel's 16 and the ring's 64 in the phrases the message uses, and the
    /// fix.
    fn assert_the_refusal_names_the_port_the_link_both_depths_and_the_fix(
        refusal: &str,
        windowed_link: &LinkUniqueId,
    ) {
        let channel_depth = format!("created {} bags deep", DeliveryProfile::ORDERED_DEPTH);
        let ring_depth = format!("{}-bag ring", WINDOWED_PORT_SUBSCRIBER_RING_DEPTH);
        assert!(
            refusal.contains("'audio'")
                && refusal.contains(windowed_link.as_str())
                && refusal.contains(&channel_depth)
                && refusal.contains(&ring_depth)
                && refusal
                    .contains("Connect the windowed consumer before the channel's other links"),
            "the refusal must name the port, the link, both depths and the fix; got {refusal}"
        );
    }

    /// A channel no link holds any more can still be alive in a tap, or in a
    /// helper yet to release its port, at the depth it was created at. A
    /// windowed consumer connected onto it is refused by name there too.
    ///
    /// Fail-without-fix: read the depth off the graph's links alone and the op
    /// asks iceoryx2 for a 64-deep channel, which refuses with an error naming
    /// neither the port nor the fix.
    #[test]
    fn a_windowed_consumer_onto_a_shallower_channel_only_a_tap_still_holds_is_refused_naming_both_depths()
     {
        let node = Iceoryx2Node::for_this_test_process();
        let mut graph = Graph::new();
        let (source_id, plain_link) = add_a_source_linked_to_a_plain_consumer(&mut graph);
        open_iceoryx2_service(&mut graph, &plain_link, &node).expect("the plain consumer wires");
        let _held_open_the_way_a_tap_holds_it = node
            .open_or_create_service(
                &channel_service_name(&OutputLinkPortRef::new(source_id.as_str(), "out1"))
                    .expect("the mock's output port derives a channel name"),
                MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
                DeliveryProfile::ORDERED_DEPTH,
            )
            .expect("a tap joins the running channel");
        close_iceoryx2_service(&mut graph, &plain_link).expect("the plain consumer disconnects");
        let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);

        let refusal = open_iceoryx2_service(&mut graph, &windowed.link_id, &node)
            .expect_err("the channel the tap holds is still 16 deep")
            .to_string();

        assert_the_refusal_names_the_port_the_link_both_depths_and_the_fix(
            &refusal,
            &windowed.link_id,
        );
    }

    /// A stalled windowed consumer reads through a 64-bag ring: seventy bags
    /// written past its last receive lose exactly six, counted on its link.
    ///
    /// The mailbox the contract sizes holds far more than seventy, so every loss
    /// counted here is the ring's. Fail-without-fix: give the subscriber the
    /// profile's 16-bag ring and the link counts 54.
    #[test]
    fn a_stalled_windowed_consumer_counts_what_its_sixty_four_bag_ring_overwrote() {
        const BAGS_PAST_THE_RING: usize = 6;
        let link = NativeLinkIntoAWindowedPort::wire();

        let write_one_block = || {
            link.source_output
                .write_bytes_with_timestamp(
                    "out1",
                    &a_mono_16k_audio_block(160, 0),
                    0,
                    MessageGroupBoundary::StartsGroup,
                )
                .expect("the source publishes onto the windowed channel");
        };
        write_one_block();
        link.dest_input.receive_pending();
        for _ in 0..WINDOWED_PORT_SUBSCRIBER_RING_DEPTH + BAGS_PAST_THE_RING {
            write_one_block();
        }
        link.dest_input.receive_pending();

        assert_eq!(
            link.rendered_metrics_of_the_destination()["dropped_bags_by_link"],
            serde_json::json!({ link.link_id.as_str(): BAGS_PAST_THE_RING }),
        );
    }

    /// A stalled windowed consumer's next window is told of the bags its ring
    /// overwrote, and no window after it is.
    ///
    /// Each block is one 512-sample window. Fail-without-fix: leave a window's
    /// losses at the stage's zero and every window reads as though nothing was
    /// lost.
    #[test]
    fn a_stalled_windowed_consumers_next_window_is_told_what_its_ring_overwrote() {
        const BAGS_PAST_THE_RING: usize = 6;
        const WINDOW_FRAMES: usize = 512;
        const WINDOW_DURATION_NS: i64 = 32_000_000;
        let link = NativeLinkIntoAWindowedPort::wire();
        let write_block = |block_index: usize| {
            link.source_output
                .write_bytes_with_timestamp(
                    "out1",
                    &a_mono_16k_audio_block(WINDOW_FRAMES, block_index as i64 * WINDOW_DURATION_NS),
                    0,
                    MessageGroupBoundary::StartsGroup,
                )
                .expect("the source publishes onto the windowed channel");
        };

        write_block(0);
        let first_window = link
            .dest_input
            .read_raw("audio")
            .unwrap()
            .expect("one block is one window");
        for block_index in 1..=WINDOWED_PORT_SUBSCRIBER_RING_DEPTH + BAGS_PAST_THE_RING {
            write_block(block_index);
        }
        let (losses_told_after_the_overrun, _) = CapturedTracingWarnings::captured_while(|| {
            std::iter::from_fn(|| link.dest_input.read_raw("audio").unwrap())
                .map(|window| window.lost_since_last_read)
                .collect::<Vec<_>>()
        });

        assert_eq!(first_window.lost_since_last_read, 0);
        assert_eq!(
            losses_told_after_the_overrun.first(),
            Some(&(BAGS_PAST_THE_RING as u64))
        );
        assert_eq!(
            losses_told_after_the_overrun.iter().sum::<u64>(),
            BAGS_PAST_THE_RING as u64,
            "each loss is told once; got {losses_told_after_the_overrun:?}"
        );
    }

    /// A gap flush at a windowed destination shows the samples it discarded
    /// under the link that fed them, beside that link's dropped bags.
    ///
    /// Three hundred samples wait in the stage for a 512-sample window when a
    /// block arrives a second late. Fail-without-fix: leave the flush uncounted
    /// and the link renders no loss for a run that threw audio away.
    #[test]
    fn a_gap_flush_at_a_windowed_destination_renders_its_discarded_samples_under_its_link() {
        let link = NativeLinkIntoAWindowedPort::wire();
        let link_id = link.link_id.to_string();

        assert_eq!(
            link.rendered_metrics_of_the_destination()["discarded_samples_by_link"],
            serde_json::json!({ link_id.as_str(): 0 }),
            "a link into a windowed port renders a zero before it has lost anything"
        );

        link.source_output
            .write_bytes_with_timestamp(
                "out1",
                &a_mono_16k_audio_block(300, 0),
                0,
                MessageGroupBoundary::StartsGroup,
            )
            .expect("the source publishes");
        assert!(
            link.dest_input
                .read_raw("audio")
                .expect("the read succeeds")
                .is_none(),
            "300 of 512 samples is not a window"
        );
        link.source_output
            .write_bytes_with_timestamp(
                "out1",
                &a_mono_16k_audio_block(300, 1_000_000_000),
                0,
                MessageGroupBoundary::StartsGroup,
            )
            .expect("the source publishes");
        let (_, warnings) = CapturedTracingWarnings::captured_while(|| {
            link.dest_input
                .read_raw("audio")
                .expect("the read succeeds")
        });

        let [warning] = warnings.as_slice() else {
            panic!("the flush says so in exactly one warning; got {warnings:?}");
        };
        assert!(
            warning.contains("port=audio")
                && warning.contains(&format!("link={link_id}"))
                && warning.contains("discarded_samples=300"),
            "the warning names the port, the link and the count; got {warning}"
        );
        let metrics = link.rendered_metrics_of_the_destination();
        assert_eq!(
            metrics["discarded_samples_by_link"],
            serde_json::json!({ link_id.as_str(): 300 }),
        );
        assert_eq!(
            metrics["frames_dropped"],
            serde_json::json!(0),
            "discarded samples never enter the bag total"
        );
    }

    /// A helper-placed windowed port is handed the windowed ring depth as its
    /// port depth; its mailbox is still sized from its contract in the child.
    #[test]
    fn a_helper_placed_windowed_port_is_handed_the_windowed_ring_depth() {
        let mut graph = Graph::new();
        let dest_id = add_mock_windowed_audio_consumer(&mut graph);
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let Some(declared) =
            audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "audio")
                .expect("the mock's contract resolves")
        else {
            panic!("the mock's audio port declares a window contract");
        };

        let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
            .expect("a helper stub's node carries its link wiring");
        wire_subprocess_dest(
            &mut graph,
            &dest_link_wiring,
            &dest_id.as_str().into(),
            "audio",
            "pabc/out1",
            "pdef/notify",
            DeliveryProfile::Ordered.resolve(),
            ChannelSizing {
                max_subscribers: 2,
                channel_service_creation_depth: WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
            },
            1,
            &"L-helper-windowed-ring".into(),
            Some(declared),
        )
        .expect("a windowed helper port wires");

        let recorded = dest_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports();
        assert_eq!(
            recorded["inputs"][0]["input_port_ring_depth"],
            serde_json::json!(WINDOWED_PORT_SUBSCRIBER_RING_DEPTH),
        );
    }

    /// The channel service name a link's source output port publishes to is
    /// source-centric (`{source}/{port}`), NOT destination-centric: channel
    /// identity keys on the source only.
    /// Mentally revert to `streamlib/{dest}` and this fails — the derived name is
    /// a pure function of the source processor id + output port.
    #[test]
    fn channel_service_name_is_source_port_shaped() {
        let name = channel_service_name(&OutputLinkPortRef::new("Pabc123", "video_out"))
            .expect("legal source port derives a channel name");
        assert_eq!(name, "pabc123/video_out");
    }

    /// Every channel is created for the fixed destination cap plus the tap
    /// slot, whatever it feeds today: iceoryx2 pins `max_subscribers` at
    /// create time, so a link connected to a running source has to fit a slot
    /// that already existed. Mentally revert to sizing from the current
    /// fan-out and the first late connect onto a wired port fails to reopen
    /// the service. Past the cap the port is refused by name, before any
    /// service is touched.
    #[test]
    fn channel_max_subscribers_is_the_fixed_cap_plus_the_taps_reservation_and_refuses_past_it() {
        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);
        let src_uid: ProcessorUniqueId = src_id.as_str().into();
        let connect_one_more_destination = |graph: &mut Graph| {
            let dest_id = add_mock_input_only(graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        };

        for _ in 0..3 {
            connect_one_more_destination(&mut graph);
        }
        assert_eq!(
            channel_max_subscribers(&graph, &OutputLinkPortRef::new(src_uid.clone(), "out1"))
                .expect("three destinations fit the cap"),
            MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
            "the channel is sized for the cap plus the tap's slot, not for the three \
             destinations it feeds today",
        );

        for _ in 3..=MAX_DESTINATIONS_PER_CHANNEL {
            connect_one_more_destination(&mut graph);
        }
        let refused =
            channel_max_subscribers(&graph, &OutputLinkPortRef::new(src_uid.clone(), "out1"))
                .expect_err("one destination past the cap is refused");
        assert!(
            refused.to_string().contains("at most"),
            "the refusal names the cap; got {refused}"
        );
    }

    /// The tap op reconstructs the exact `max_subscribers` the compiler op
    /// opened the service with via the shared [`resolve_channel_sizing`].
    /// iceoryx2 verifies `max_subscribers` on the tap's publisher-free reopen,
    /// so a drift here would make every tap fail to open.
    #[test]
    fn resolve_channel_sizing_recovers_service_open_max_subscribers() {
        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);
        for _ in 0..2 {
            let dest_id = add_mock_input_only(&mut graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        }
        let src_uid: ProcessorUniqueId = src_id.as_str().into();

        let sizing = resolve_channel_sizing(
            &graph,
            &Iceoryx2Node::for_this_test_process(),
            &OutputLinkPortRef::new(src_uid.clone(), "out1"),
            WindowedDestinationOutsideTheSourcesGraph::Absent,
        )
        .expect("sizing resolves for a wired channel");
        assert_eq!(
            sizing.max_subscribers,
            channel_max_subscribers(&graph, &OutputLinkPortRef::new(src_uid.clone(), "out1"))
                .expect("two destinations fit the cap"),
            "resolve_channel_sizing must agree with channel_max_subscribers — the \
             single derivation both the service-open op and the tap op share",
        );
    }

    fn the_machine_these_tests_run_on() -> crate::core::runtime::MachineName {
        crate::core::runtime::MachineName::from_a_hostname("desk").expect("`desk` names a machine")
    }

    /// A port in this stream is named by its address with or without this
    /// stream and this machine in front, and resolves to the exact source a
    /// link carries from; its iceoryx2 channel name, an address naming no
    /// node, and a port no link carries from all answer `TapChannelNotFound`.
    #[test]
    fn a_port_in_this_stream_is_named_by_its_address_and_not_by_its_channel() {
        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        );
        let source_node_name = graph
            .traversal()
            .v(src_id.as_str())
            .first()
            .expect("the source node is in the graph")
            .display_name
            .clone();
        let this_machine = the_machine_these_tests_run_on();

        for address in [
            format!("{source_node_name}/out1"),
            format!("main/{source_node_name}/out1"),
            format!("desk/main/{source_node_name}/out1"),
        ] {
            let resolved = find_the_source_a_caller_named(&graph, &this_machine, "main", &address)
                .unwrap_or_else(|refusal| panic!("{address}: {refusal}"));
            assert_eq!(resolved.processor_id().as_str(), src_id.as_str());
            assert_eq!(resolved.port_name(), "out1");
        }

        let its_channel = crate::iceoryx2::source_channel_name(&src_id, "out1")
            .expect("source port derives a channel name");
        for unresolvable in [
            its_channel.as_str().to_string(),
            "main/nosuchnode/out1".to_string(),
            format!("main/{source_node_name}/out2"),
        ] {
            assert!(
                matches!(
                    find_the_source_a_caller_named(&graph, &this_machine, "main", &unresolvable),
                    Err(Error::TapChannelNotFound(_))
                ),
                "{unresolvable:?} must not resolve to any source port",
            );
        }
    }

    /// A tap reads a port on this machine, so an address naming another
    /// machine is refused naming it rather than reported as a channel nobody
    /// publishes.
    #[test]
    fn a_tap_naming_another_machine_is_refused_naming_that_machine() {
        let refused = find_the_source_a_caller_named(
            &Graph::new(),
            &the_machine_these_tests_run_on(),
            "main",
            "rig/main/camera-source/video",
        )
        .expect_err("a channel on another machine is nothing this machine taps");

        assert!(
            matches!(refused, Error::PortAddressNamesAnotherMachine { ref machine, .. } if machine == "rig"),
            "refused as an address naming another machine, got {refused:?}"
        );
        assert!(refused.to_string().contains("`rig`"), "{refused}");
    }

    /// A stream taps its own ports, so an address naming another stream is
    /// refused naming both streams, and one that does not parse is refused as
    /// an address.
    #[test]
    fn a_tap_naming_another_stream_or_no_address_is_refused_by_name() {
        let refused = find_the_source_a_caller_named(
            &Graph::new(),
            &the_machine_these_tests_run_on(),
            "main",
            "studio/camera-source/video",
        )
        .expect_err("a port in another stream is not this stream's to tap");
        assert!(
            matches!(refused, Error::InvalidPortAddress(_)),
            "{refused:?}"
        );
        let refused = refused.to_string();
        assert!(
            refused.contains("`studio`") && refused.contains("`main`"),
            "{refused}"
        );

        let refused = find_the_source_a_caller_named(
            &Graph::new(),
            &the_machine_these_tests_run_on(),
            "main",
            "video",
        )
        .expect_err("one part is no address");
        assert!(
            matches!(refused, Error::InvalidPortAddress(_)),
            "{refused:?}"
        );
    }

    /// A port whose only readers are links in other streams is tapped by its
    /// address, since those links installed its publisher; once their
    /// registrations are dropped it answers `TapChannelNotFound` again.
    ///
    /// Fail-without-fix: count the links of this stream alone and the tap is
    /// refused a channel another stream's link keeps live.
    #[test]
    fn a_port_read_only_from_other_streams_is_named_by_its_address() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::core::graph::{
            OutputPortExposureLevel, OutputPortReaderOutsideItsStream, ReaderOfAnExposedOutputPort,
        };

        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);
        let source_node_name = graph
            .traversal()
            .v(src_id.as_str())
            .first()
            .expect("the source node is in the graph")
            .display_name
            .clone();
        let address = format!("main/{source_node_name}/out1");
        let this_machine = the_machine_these_tests_run_on();
        assert!(
            matches!(
                find_the_source_a_caller_named(&graph, &this_machine, "main", &address),
                Err(Error::TapChannelNotFound(_))
            ),
            "a port nothing reads has no channel to tap"
        );

        let still_registered = Arc::new(AtomicBool::new(true));
        let mut exposed_ports = ExposedOutputPortsComponent::default();
        let _ = exposed_ports.set_level("out1", OutputPortExposureLevel::Private);
        assert!(
            exposed_ports
                .register_reader(
                    "out1",
                    ReaderOfAnExposedOutputPort::new(
                        Arc::clone(&still_registered),
                        OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
                        Box::new(|_why| {}),
                    ),
                )
                .is_ok()
        );
        graph
            .traversal_mut()
            .v(src_id.as_str())
            .first_mut()
            .expect("the source is in its graph")
            .insert_component_without_rendering_it(exposed_ports);

        let resolved = find_the_source_a_caller_named(&graph, &this_machine, "main", &address)
            .expect("a port another stream's link reads is tapped by its address");
        assert_eq!(resolved.processor_id().as_str(), src_id.as_str());
        assert_eq!(resolved.port_name(), "out1");

        still_registered.store(false, Ordering::Release);
        assert!(
            matches!(
                find_the_source_a_caller_named(&graph, &this_machine, "main", &address),
                Err(Error::TapChannelNotFound(_))
            ),
            "a reader whose registration was dropped keeps no channel tappable"
        );
    }

    /// The destination-keyed notify service is created for the fixed inbound
    /// cap, not the fan-in of the day: it is created with the first inbound
    /// link and iceoryx2 verifies `max_notifiers` on every reopen, so a source
    /// connected to a running destination has to fit a notifier slot that
    /// already existed. Past the cap the destination is refused by name.
    #[test]
    fn destination_max_notifiers_is_the_fixed_cap_and_refuses_past_it() {
        let mut graph = Graph::new();
        let dest_id = add_mock_input_only(&mut graph);
        let dest_uid: ProcessorUniqueId = dest_id.as_str().into();
        let connect_one_more_source = |graph: &mut Graph| {
            let src_id = add_mock_output_only(graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        };

        for _ in 0..3 {
            connect_one_more_source(&mut graph);
        }
        assert_eq!(
            destination_max_notifiers(&mut graph, &dest_uid)
                .expect("three inbound links fit the cap"),
            MAX_INBOUND_LINKS_PER_DESTINATION,
        );

        for _ in 3..=MAX_INBOUND_LINKS_PER_DESTINATION {
            connect_one_more_source(&mut graph);
        }
        let refused = destination_max_notifiers(&mut graph, &dest_uid)
            .expect_err("one inbound link past the cap is refused");
        assert!(
            refused.to_string().contains("at most"),
            "the refusal names the cap; got {refused}"
        );
    }

    /// One helper-placed destination's recorded input entry, wired through the
    /// op's own subprocess branch.
    fn one_helper_destinations_recorded_input_entry(
        channel_service_name: &str,
    ) -> serde_json::Value {
        let mut graph = Graph::new();
        let dest_id = add_mock_input_only(&mut graph);
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
        );
        let link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
            .expect("a helper stub's node carries its link wiring");
        wire_subprocess_dest(
            &mut graph,
            &link_wiring,
            &dest_id.as_str().into(),
            "in1",
            channel_service_name,
            "pdef/notify",
            DeliveryProfile::Newest.resolve(),
            sizing_of_a_two_subscriber_test_channel(),
            1,
            &LinkUniqueId::from("L-envelope"),
            None,
        )
        .expect("recording dest wiring must succeed");

        dest_instance
            .lock()
            .out_of_process_link_wiring()
            .expect("the stub records its own wiring")
            .as_setup_command_ports()["inputs"][0]
            .clone()
    }

    /// A helper knows a link by the channel it subscribes to.
    #[test]
    fn the_envelope_hands_a_helper_its_channel_as_the_name_it_knows_the_link_by() {
        let entry = one_helper_destinations_recorded_input_entry("pcam/video");

        assert_eq!(
            entry["channel_service_name"],
            serde_json::json!("pcam/video")
        );
        assert_eq!(entry["inbound_link_name"], serde_json::json!("pcam/video"));
    }

    /// A native destination knows a link by the channel it subscribes to.
    #[test]
    fn a_native_destination_knows_each_link_by_its_channel() {
        use crate::core::test_support::MockInputOnlyProcessor;

        let (_source_output, dest_input) =
            wire_one_test_link::<MockInputOnlyProcessor::Processor>("inbound-link-name", false);

        assert_eq!(
            dest_input.inbound_link_names("in1"),
            vec![InboundLinkName::from("psource/out1")]
        );
    }

    /// The two halves of the wiring op run on two graphs, the way a link
    /// reading another stream is wired: the source's side on the source
    /// stream's graph, the destination's side on the reading stream's, where
    /// the link alone lives.
    mod wiring_across_two_graphs {
        use std::sync::atomic::AtomicBool;

        use super::*;
        use crate::core::graph::{
            OutputPortExposureLevel, OutputPortInAnotherStreamOnThisMachine,
            OutputPortReaderOutsideItsStream, ReaderOfAnExposedOutputPort,
            SourceSideOfALinkReadingAnotherStream,
        };
        use crate::core::json_schema::LinkStateOutput;
        use crate::core::test_support::{MockInputOnlyProcessor, MockOutputOnlyProcessor};

        /// A link added to `reading_graph` reading `out1` of a node in another
        /// stream into `in1` of `dest_id`, as an edge from that node to itself.
        fn a_link_reading_another_graph_into(
            reading_graph: &mut Graph,
            dest_id: &str,
        ) -> LinkUniqueId {
            reading_graph
                .traversal_mut()
                .add_e_reading_another_stream(
                    OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "out1").unwrap(),
                    InputLinkPortRef::new(dest_id, "in1"),
                )
                .first()
                .expect("the link reading another graph is added")
                .id
                .clone()
        }

        /// Wire link `link_id` of `reading_graph` from `out1` of `source_id`
        /// in `source_graph`, through the two halves a reconcile runs, and
        /// hold the link's services on it.
        fn wire_across(
            source_graph: &mut Graph,
            source_id: &str,
            reading_graph: &mut Graph,
            link_id: &LinkUniqueId,
        ) -> Result<(HowAnOpenedLinkStands, Option<SourceSideOfALinkWiredOn>)> {
            let to_port = reading_graph
                .traversal()
                .e(link_id)
                .first()
                .expect("the link is in the reading graph")
                .to_port()
                .clone();
            let destination = read_the_destination_of_a_link(reading_graph, &to_port, link_id)?;
            let opened_source_side = open_the_source_side_of_a_link(
                source_graph,
                &OutputLinkPortRef::new(source_id, "out1"),
                link_id,
                &destination,
                &Iceoryx2Node::for_this_test_process(),
            )?;
            let wire_reply_awaited_from_the_destination = open_the_destination_side_of_a_link(
                reading_graph,
                link_id,
                &opened_source_side,
                destination,
            )?;
            let wired_on = opened_source_side.wired_on().cloned();
            let link = reading_graph
                .traversal_mut()
                .e(link_id)
                .first_mut()
                .expect("the link is in the reading graph");
            let how_it_stands = opened_source_side.hold_its_services_on_the_link_and_stamp_it(
                link,
                wire_reply_awaited_from_the_destination,
            );
            Ok((how_it_stands, wired_on))
        }

        /// A native source in one graph and a native reader in another, and
        /// the link reading the source into the reader, not yet wired.
        struct NativeSourceAndReaderInTwoGraphs {
            source_graph: Graph,
            source_id: String,
            source_output: Arc<crate::iceoryx2::OutputWriterInner>,
            reading_graph: Graph,
            dest_input: Arc<crate::iceoryx2::InputMailboxesInner>,
            link_id: LinkUniqueId,
        }

        impl NativeSourceAndReaderInTwoGraphs {
            fn new() -> Self {
                let mut source_graph = Graph::new();
                let source_id = add_mock_output_only(&mut source_graph);
                let (_, source_output, _) = attach_mock_instance::<
                    MockOutputOnlyProcessor::Processor,
                >(&mut source_graph, &source_id);
                let mut reading_graph = Graph::new();
                let dest_id = add_mock_input_only(&mut reading_graph);
                let (_, _, dest_input) = attach_mock_instance::<MockInputOnlyProcessor::Processor>(
                    &mut reading_graph,
                    &dest_id,
                );
                let link_id = a_link_reading_another_graph_into(&mut reading_graph, &dest_id);
                Self {
                    source_graph,
                    source_id,
                    source_output: source_output
                        .expect("an output-only mock holds an output writer"),
                    reading_graph,
                    dest_input: dest_input.expect("an input-only mock holds input mailboxes"),
                    link_id,
                }
            }

            fn wire(
                &mut self,
            ) -> Result<(HowAnOpenedLinkStands, Option<SourceSideOfALinkWiredOn>)> {
                wire_across(
                    &mut self.source_graph,
                    &self.source_id,
                    &mut self.reading_graph,
                    &self.link_id,
                )
            }
        }

        /// A windowed destination the source's graph does not hold creates
        /// the channel as deep as its ring, as one in the source's own graph
        /// does.
        ///
        /// Fail-without-fix: size the channel off the source's graph alone and
        /// it is created at the ordered depth, shallower than the ring.
        #[test]
        fn a_windowed_destination_in_another_graph_creates_the_channel_as_deep_as_its_ring() {
            let mut source_graph = Graph::new();
            let source_id = add_mock_output_only(&mut source_graph);
            attach_mock_instance::<MockOutputOnlyProcessor::Processor>(
                &mut source_graph,
                &source_id,
            );
            let mut reading_graph = Graph::new();
            let dest_id = add_mock_windowed_audio_consumer(&mut reading_graph);
            let link_id = reading_graph
                .traversal_mut()
                .add_e_reading_another_stream(
                    OutputPortInAnotherStreamOnThisMachine::new("a", "microphone", "out1").unwrap(),
                    InputLinkPortRef::new(&dest_id, "audio"),
                )
                .first()
                .expect("the link reading another graph is added")
                .id
                .clone();
            let destination = read_the_destination_of_a_link(
                &mut reading_graph,
                &InputLinkPortRef::new(&dest_id, "audio"),
                &link_id,
            )
            .expect("the windowed destination is read");

            let _opened_source_side = open_the_source_side_of_a_link(
                &mut source_graph,
                &OutputLinkPortRef::new(&source_id, "out1"),
                &link_id,
                &destination,
                &Iceoryx2Node::for_this_test_process(),
            )
            .expect("the source's side opens");

            assert_eq!(
                creation_depth_a_helper_opening_out1_finds(&source_id),
                WINDOWED_PORT_SUBSCRIBER_RING_DEPTH
            );
        }

        /// Fail-without-fix: open both halves on the reading graph, as the
        /// in-stream op does, and the source is not found there.
        #[test]
        fn bags_written_in_one_graph_reach_a_destination_wired_from_another_graph() {
            let mut two_graphs = NativeSourceAndReaderInTwoGraphs::new();

            let (how_it_stands, _) = two_graphs.wire().expect("the link wires across two graphs");

            assert_eq!(how_it_stands, HowAnOpenedLinkStands::Wired);
            assert!(
                two_graphs
                    .source_graph
                    .traversal()
                    .e(())
                    .iter()
                    .next()
                    .is_none(),
                "the source's graph holds no edge for a link another stream holds"
            );
            for bag in 0..3 {
                two_graphs
                    .source_output
                    .write_bytes_with_timestamp(
                        "out1",
                        b"a bag read from another stream",
                        bag,
                        MessageGroupBoundary::StartsGroup,
                    )
                    .expect("the source publishes onto the channel the other graph wired");
            }
            two_graphs.dest_input.receive_pending();
            assert_eq!(two_graphs.dest_input.drain("in1").len(), 3);
            assert_eq!(
                link_state_in_graph(&two_graphs.reading_graph, &two_graphs.link_id),
                (LinkStateOutput::Wired, None)
            );
        }

        /// The first reader of a port nothing in its own stream reads
        /// installs the port's publisher, and releasing that reader's side
        /// takes the publisher away again.
        #[test]
        fn the_first_reader_from_another_stream_installs_the_publisher_and_its_release_drops_it() {
            let mut two_graphs = NativeSourceAndReaderInTwoGraphs::new();
            assert!(!two_graphs.source_output.has_channel_publisher("out1"));

            let (_, wired_on) = two_graphs.wire().expect("the link wires across two graphs");

            assert!(two_graphs.source_output.has_channel_publisher("out1"));
            assert_eq!(two_graphs.source_output.channel_notifier_count("out1"), 1);
            let source_side = SourceSideOfALinkReadingAnotherStream::released_when_dropped(
                wired_on
                    .as_ref()
                    .expect("a native source's side is its output writer"),
                "out1",
                &two_graphs.link_id,
            );
            drop(source_side);
            assert_eq!(two_graphs.source_output.channel_notifier_count("out1"), 0);
            assert!(
                !two_graphs.source_output.has_channel_publisher("out1"),
                "the last link out of the port takes its publisher with it"
            );
        }

        /// Readers from other streams registered against a port count toward
        /// its channel's destination cap, in its own stream's wiring too.
        ///
        /// Fail-without-fix: count the links in the source's graph alone and
        /// the 33rd destination is wired past a channel created for 32.
        #[test]
        fn a_reader_from_another_stream_counts_toward_the_channels_destination_cap() {
            let mut source_graph = Graph::new();
            let source_id = add_mock_output_only(&mut source_graph);
            attach_mock_instance::<MockOutputOnlyProcessor::Processor>(
                &mut source_graph,
                &source_id,
            );
            let mut exposed_ports = ExposedOutputPortsComponent::default();
            let _ = exposed_ports.set_level("out1", OutputPortExposureLevel::Private);
            let registrations: Vec<Arc<AtomicBool>> = (0..MAX_DESTINATIONS_PER_CHANNEL)
                .map(|_| {
                    let still_registered = Arc::new(AtomicBool::new(true));
                    assert!(
                        exposed_ports
                            .register_reader(
                                "out1",
                                ReaderOfAnExposedOutputPort::new(
                                    Arc::clone(&still_registered),
                                    OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
                                    Box::new(|_why| {}),
                                ),
                            )
                            .is_ok()
                    );
                    still_registered
                })
                .collect();
            source_graph
                .traversal_mut()
                .v(source_id.as_str())
                .first_mut()
                .expect("the source is in its graph")
                .insert_component_without_rendering_it(exposed_ports);
            let dest_id = add_mock_input_only(&mut source_graph);
            attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut source_graph, &dest_id);
            let link_in_its_own_stream =
                add_link_from_out1_to_in1(&mut source_graph, &source_id, &dest_id);

            let refusal = open_iceoryx2_service(
                &mut source_graph,
                &link_in_its_own_stream,
                &Iceoryx2Node::for_this_test_process(),
            )
            .expect_err("a 33rd destination is refused");
            assert!(
                refusal.to_string().contains(&format!(
                    "would feed {} destinations",
                    MAX_DESTINATIONS_PER_CHANNEL + 1
                )),
                "{refusal}"
            );

            registrations[0].store(false, std::sync::atomic::Ordering::Release);
            open_iceoryx2_service(
                &mut source_graph,
                &link_in_its_own_stream,
                &Iceoryx2Node::for_this_test_process(),
            )
            .expect("a reader whose registration was dropped frees its slot");
        }

        /// A helper-placed source in one graph is handed a link whose
        /// destination is in another, notifying that destination's service.
        #[test]
        fn a_helper_source_in_one_graph_is_handed_a_link_into_another_graphs_destination() {
            let mut source_graph = Graph::new();
            let source_id = add_mock_output_only(&mut source_graph);
            let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
            attach_processor_instance(
                &mut source_graph,
                &source_id,
                ProcessorInstance::new(Box::new(
                    OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                        far_side.clone(),
                        ProcessExecution::Reactive,
                    ),
                )),
            );
            let mut reading_graph = Graph::new();
            let dest_id = add_mock_input_only(&mut reading_graph);
            attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut reading_graph, &dest_id);
            let link_id = a_link_reading_another_graph_into(&mut reading_graph, &dest_id);

            let (how_it_stands, wired_on) =
                wire_across(&mut source_graph, &source_id, &mut reading_graph, &link_id)
                    .expect("the link wires across two graphs");

            assert_eq!(
                how_it_stands,
                HowAnOpenedLinkStands::PendingItsOutOfProcessEndsAnswers
            );
            assert!(matches!(
                wired_on,
                Some(SourceSideOfALinkWiredOn::AHelperProcessesLinkWiring(_))
            ));
            let handed_over = far_side.late_wired_links.lock();
            let [(crate::core::PortDirection::Output, entry)] = &handed_over[..] else {
                panic!("the helper source is handed the link's output end alone: {handed_over:?}");
            };
            assert_eq!(entry["link_id"], serde_json::json!(link_id.as_str()));
            assert_eq!(
                entry["dest_notify_service_name"],
                serde_json::json!(format!("streamlib/{dest_id}/notify"))
            );
        }

        /// A link between helpers in two graphs waits on both helpers'
        /// answers, read off the cells its reading graph holds.
        #[test]
        fn a_link_from_another_stream_waits_on_both_helpers_answers() {
            let answers_owed: Arc<Mutex<Vec<Arc<OutOfProcessLinkWireReply>>>> = Arc::default();
            let a_helper_stub = || {
                ProcessorInstance::new(Box::new(
                    OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                        RecordingOutOfProcessFarSideLinkDelivery {
                            wire_answers_owed: answers_owed.clone(),
                            ..Default::default()
                        },
                        ProcessExecution::Reactive,
                    ),
                ))
            };
            let mut source_graph = Graph::new();
            let source_id = add_mock_output_only(&mut source_graph);
            attach_processor_instance(&mut source_graph, &source_id, a_helper_stub());
            let mut reading_graph = Graph::new();
            let dest_id = add_mock_input_only(&mut reading_graph);
            attach_processor_instance(&mut reading_graph, &dest_id, a_helper_stub());
            let link_id = a_link_reading_another_graph_into(&mut reading_graph, &dest_id);

            wire_across(&mut source_graph, &source_id, &mut reading_graph, &link_id)
                .expect("the link wires across two graphs");

            let answers_owed = answers_owed.lock().clone();
            assert_eq!(answers_owed.len(), 2, "each helper owes its own answer");
            assert_eq!(
                link_state_in_graph(&reading_graph, &link_id),
                (LinkStateOutput::Pending, None)
            );
            answers_owed[0]
                .note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
            assert_eq!(
                link_state_in_graph(&reading_graph, &link_id),
                (LinkStateOutput::Pending, None),
                "the source's helper opening does not wire a link its reader never opened"
            );
            answers_owed[1]
                .note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
            assert_eq!(
                link_state_in_graph(&reading_graph, &link_id),
                (LinkStateOutput::Wired, None)
            );
        }

        /// A source processor that is gone makes releasing its side a no-op,
        /// and the release never kept it alive; one still there is told.
        #[test]
        fn a_dead_source_processor_makes_release_a_no_op() {
            for source_processor_still_there in [true, false] {
                let mut source_graph = Graph::new();
                let source_id = add_mock_output_only(&mut source_graph);
                let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
                let source_instance = attach_processor_instance(
                    &mut source_graph,
                    &source_id,
                    ProcessorInstance::new(Box::new(
                        OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                            far_side.clone(),
                            ProcessExecution::Reactive,
                        ),
                    )),
                );
                let mut reading_graph = Graph::new();
                let dest_id = add_mock_input_only(&mut reading_graph);
                attach_mock_instance::<MockInputOnlyProcessor::Processor>(
                    &mut reading_graph,
                    &dest_id,
                );
                let link_id = a_link_reading_another_graph_into(&mut reading_graph, &dest_id);
                let (_, wired_on) =
                    wire_across(&mut source_graph, &source_id, &mut reading_graph, &link_id)
                        .expect("the link wires across two graphs");
                let source_side = SourceSideOfALinkReadingAnotherStream::released_when_dropped(
                    wired_on
                        .as_ref()
                        .expect("a helper source's side is its link wiring"),
                    "out1",
                    &link_id,
                );
                drop(wired_on);
                if !source_processor_still_there {
                    drop(source_instance);
                    drop(source_graph);
                    drop(source_side);
                    assert!(
                        far_side.reclaimed_links.lock().is_empty(),
                        "a source processor that is gone is told nothing"
                    );
                    continue;
                }

                drop(source_side);
                assert_eq!(
                    far_side
                        .reclaimed_links
                        .lock()
                        .iter()
                        .map(|reclaimed| reclaimed.link_id.clone())
                        .collect::<Vec<_>>(),
                    vec![link_id.to_string()],
                    "a source processor still there is told to drop its side"
                );
                drop(source_instance);
            }
        }
    }
}
