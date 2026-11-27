// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::NodeRunId;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, Iceoryx2ServicesHeldOpenForLinkComponent, Link, LinkState,
    LinkStateComponent, LinkUniqueId, NodeLossCounts, OutOfProcessLinkWireRepliesComponent,
    OutputLinkPortRef, SourceSideHeldOpenForAReaderOutsideItsStream, SourceSideOfALinkWiredOn,
};
use crate::core::nodes::{NodeInstance, OutOfProcessLinkWireReply, OutOfProcessLinkWiringEnvelope};
use crate::core::output_port_stream_facts::StreamFactsOfOutputPorts;
use crate::iceoryx2::{
    BundleMembersAtTheRead, ChannelEgressConfig, ChannelSizing, ChannelTrustTier,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DeliveryProfile, DeliveryResolution, Iceoryx2Node,
    Iceoryx2NotifyService, Iceoryx2Service, effective_channel_chunk_ceiling_bytes,
};
use streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION;

use super::{
    DestinationOfALinkAsItsWiringReadsIt, RoomForAWindowedReaderTheSourcesGraphCannotSee,
    channel_service_name, get_single_node, insert_loss_counts_on_graph_node_once,
    node_to_reclaim_from, notify_service_name_for, out_of_process_link_wiring_of,
    publish_loss_counts_on_graph_node,
    refuse_a_windowed_port_onto_a_channel_created_shallower_than_its_ring, resolve_channel_sizing,
};

/// The source's side of one link once opened: the services the link holds,
/// and where the source's end was wired.
pub(crate) struct OpenedSourceSideOfALink {
    pub(super) channel_service_name: String,
    pub(super) channel_sizing: ChannelSizing,
    pub(super) channel_data_service: Iceoryx2Service,
    pub(super) destination_notify_service: Iceoryx2NotifyService,
    pub(super) wire_reply_awaited_from_the_source: Option<Arc<OutOfProcessLinkWireReply>>,
    wired_on: Option<SourceSideOfALinkWiredOn>,
    pub(super) source_output_port: String,
    pub(super) stream_facts_of_the_sources_output_ports: Arc<StreamFactsOfOutputPorts>,
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
    /// Have a native source send each kept last message to a reader that connected after it,
    /// so a destination opened in this process reads its value at once. A helper source keeps
    /// the last message of the ports its setup command names, and its own scheduling loop sends
    /// them on its next pass.
    pub(crate) fn send_each_kept_message_to_a_reader_it_missed(&self) {
        if let Some(SourceSideOfALinkWiredOn::ANativeNodesOutputWriter(output_writer)) =
            &self.wired_on
        {
            output_writer.send_each_kept_message_to_a_reader_it_missed();
        }
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
        // thread.
        if wire_replies_awaited_from_its_out_of_process_ends.is_empty() {
            // Answers left from an earlier wiring of it no longer speak for it.
            link.remove::<OutOfProcessLinkWireRepliesComponent>();
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
/// port past its destination cap, are refused before any service is opened.
pub(crate) fn open_the_source_side_of_a_link(
    source_graph: &mut Graph,
    source: &OutputLinkPortRef,
    link_id: &LinkUniqueId,
    destination: &DestinationOfALinkAsItsWiringReadsIt,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<OpenedSourceSideOfALink> {
    let source_proc_id = source.node_run_id().clone();
    let source_port = source.port_name();
    let channel_service_name = channel_service_name(source)?;
    let link_wiring_of_a_source_out_of_process =
        out_of_process_link_wiring_of(source_graph, &source_proc_id);
    let stream_facts_of_the_sources_output_ports = source_graph
        .traversal()
        .v(&source_proc_id)
        .first()
        .map(|source_node| Arc::clone(source_node.stream_facts_of_output_ports()))
        .ok_or_else(|| Error::NodeNotFound(format!("source node '{source_proc_id}' not found")))?;
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
    let room_for_a_windowed_reader_the_sources_graph_cannot_see =
        destination.room_for_a_windowed_reader_joining_later;
    let channel_sizing = resolve_channel_sizing(
        source_graph,
        iceoryx2_node,
        source,
        room_for_a_windowed_reader_the_sources_graph_cannot_see,
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
                destination.input_port_delivery,
                link_id,
            )?;
            (
                wire_reply,
                Some(SourceSideOfALinkWiredOn::AHelperProcessesLinkWiring(
                    source_link_wiring,
                )),
            )
        } else {
            let source_node = get_single_node(source_graph, &source_proc_id)?;
            let output_writer = wire_rust_source(
                source_graph,
                &source_proc_id,
                &source_node,
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
                output_writer.map(SourceSideOfALinkWiredOn::ANativeNodesOutputWriter),
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
        stream_facts_of_the_sources_output_ports,
    })
}

/// The input port a reader of a channel's reserved slot is named as when a
/// source's side is opened for it, for the log lines that name a link's
/// destination.
const A_RESERVED_SLOT_READER_AS_A_DESTINATION_PORT: &str = "a-reserved-slot-reader";

/// What a reader of an output port's channel through its reserved slot — the
/// runtime's publisher or a viewer — holds open on the port's source: the
/// channel's name and sizing, the source's side, and the services. Dropping it
/// releases the source's side, then the services.
pub(crate) struct SourceSideHeldOpenForAReaderOnTheReservedSlot {
    pub(crate) channel_service_name: String,
    /// The notify service the source notifies on each write, whose one
    /// listener is the reader's.
    pub(crate) wake_service_name: String,
    /// Where a helper source answers whether it opened its port; `None` for a
    /// source with nothing to answer.
    pub(crate) wire_reply_awaited_from_the_source: Option<Arc<OutOfProcessLinkWireReply>>,
    _source_side: SourceSideHeldOpenForAReaderOutsideItsStream,
    _services: Iceoryx2ServicesHeldOpenForLinkComponent,
}

/// Open the source's side of output port `source` on `source_graph` for a
/// reader of its channel's reserved slot, as a link's source side is opened, whether or not a
/// link out of the port exists: the channel's single publisher is installed
/// when none is, and held until what this hands back drops. A channel this is
/// the first to open is made as deep as a windowed reader's ring, so a live
/// graph change may still connect one; one already open keeps its depth. The
/// reader's end is a destination of its own, notified through a notify
/// service no listener waits on, since it polls the reserved slot.
pub(crate) fn open_the_source_side_for_a_reader_on_the_reserved_slot(
    source_graph: &mut Graph,
    source: &OutputLinkPortRef,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<SourceSideHeldOpenForAReaderOnTheReservedSlot> {
    let the_reserved_slot_reader = NodeRunId::new();
    let link_id = LinkUniqueId::new();
    let destination = DestinationOfALinkAsItsWiringReadsIt {
        notify_service_name: notify_service_name_for(&the_reserved_slot_reader),
        dest_proc_id: the_reserved_slot_reader,
        dest_port: A_RESERVED_SLOT_READER_AS_A_DESTINATION_PORT.to_string(),
        link_wiring_of_a_destination_out_of_process: None,
        audio_windowing: None,
        input_port_delivery: DeliveryProfile::Ordered.resolve(),
        max_notifiers: MAX_INBOUND_LINKS_PER_DESTINATION,
        room_for_a_windowed_reader_joining_later:
            RoomForAWindowedReaderTheSourcesGraphCannotSee::Made,
        bundle_members_at_the_read: BundleMembersAtTheRead::default(),
    };
    let opened = open_the_source_side_of_a_link(
        source_graph,
        source,
        &link_id,
        &destination,
        iceoryx2_node,
    )?;
    let wired_on = opened.wired_on.as_ref().ok_or_else(|| {
        Error::Link(format!(
            "output port '{source}' belongs to a node holding no output writer, so it \
             publishes nothing"
        ))
    })?;
    let source_side = SourceSideHeldOpenForAReaderOutsideItsStream::released_when_dropped(
        wired_on,
        source.port_name(),
        &link_id,
    );
    Ok(SourceSideHeldOpenForAReaderOnTheReservedSlot {
        channel_service_name: opened.channel_service_name,
        wake_service_name: destination.notify_service_name.clone(),
        wire_reply_awaited_from_the_source: opened.wire_reply_awaited_from_the_source,
        _source_side: source_side,
        _services: Iceoryx2ServicesHeldOpenForLinkComponent {
            channel_data_service: opened.channel_data_service,
            destination_notify_service: opened.destination_notify_service,
        },
    })
}

/// Drop link `link_id`'s notifier off output port `source_port` of
/// `source_proc_id`, and the port's publisher when the link was its last.
pub(super) fn release_the_source_side_of_a_link(
    graph: &mut Graph,
    source_proc_id: &NodeRunId,
    source_port: &str,
    link_id: &LinkUniqueId,
) {
    if let Some(source_link_wiring) = out_of_process_link_wiring_of(graph, source_proc_id) {
        source_link_wiring.unwire_a_link_reporting_a_far_side_that_kept_its_port(
            crate::core::PortDirection::Output,
            source_port,
            link_id.as_str(),
        );
    } else if let Some(source_node) = node_to_reclaim_from(graph, source_proc_id) {
        let source_guard = source_node.lock();
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

pub(super) fn wire_rust_source(
    graph: &mut Graph,
    source_proc_id: &NodeRunId,
    source_node: &Arc<Mutex<NodeInstance>>,
    source_port: &str,
    link_id: &LinkUniqueId,
    service: &Iceoryx2Service,
    notify_service: &Iceoryx2NotifyService,
    egress_config: ChannelEgressConfig,
) -> Result<Option<Arc<crate::iceoryx2::OutputWriterInner>>> {
    let source_guard = source_node.lock();
    let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() else {
        return Ok(None);
    };

    if !output_inner.has_channel_publisher(source_port) {
        let publisher = service.create_publisher(egress_config.expected_payload_bytes)?;
        output_inner.set_channel_publisher(source_port, service, publisher, egress_config);
        tracing::debug!(
            "Installed channel publisher for source output port '{}'",
            source_port
        );
    }

    let notifier = notify_service.create_notifier()?;
    output_inner.add_channel_link(source_port, link_id.as_str(), notifier);
    publish_loss_counts_on_graph_node(graph, source_proc_id, &source_guard);
    Ok(Some(output_inner))
}

pub(super) fn wire_subprocess_source(
    graph: &mut Graph,
    link_wiring: &OutOfProcessLinkWiringEnvelope,
    source_proc_id: &NodeRunId,
    source_port: &str,
    channel_service_name: &str,
    notify_service_name: &str,
    expected_payload: usize,
    channel_ceiling_bytes: usize,
    channel_sizing: ChannelSizing,
    notify_max_notifiers: usize,
    destination_input_port_delivery: DeliveryResolution,
    link_id: &LinkUniqueId,
) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
    link_wiring
        .output_frames_kept_by_its_output_links()
        .note_a_wired_output_link(
            link_id.as_str(),
            destination_input_port_delivery.depth,
            destination_input_port_delivery.borrow_quota,
        );
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
    let loss_counts = link_wiring.helper_placed_node_loss_counts();
    entry["output_port_wiring_generation"] =
        serde_json::json!(loss_counts.note_outbound_link(source_port, link_id.as_str()));
    let wire_reply = link_wiring
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            crate::core::PortDirection::Output,
            entry,
        )
        .inspect_err(|_| {
            link_wiring
                .output_frames_kept_by_its_output_links()
                .forget_an_unwired_output_link(link_id.as_str());
        })?;
    insert_loss_counts_on_graph_node_once(
        graph,
        source_proc_id,
        NodeLossCounts::CountedInItsHelperProcess(loss_counts),
    );
    Ok(wire_reply)
}
