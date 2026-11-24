// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::NodeRunId;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    DeviceMatchedAudioWindowContractsComponent, Graph, GraphNodeWithComponents, InputLinkPortRef,
    LinkUniqueId, NodeLossCounts, OutputLinkPortRef, the_bundle_member_a_link_ends_at,
    the_bundle_member_a_link_starts_at,
};
use crate::core::nodes::{NodeInstance, OutOfProcessLinkWireReply, OutOfProcessLinkWiringEnvelope};
use crate::iceoryx2::{
    AudioWindowDeclarationOfAnInputPort, BundleMembersAtTheRead, ChannelSizing, DeliveryResolution,
    Iceoryx2NotifyService, Iceoryx2Service, InboundLinkName, MemberPutAtTheRead,
    MemberTakenAtTheRead, WhatTheMembersArePutInto, refuse_an_unsettled_match_device_sentinel,
};

use super::{
    OpenedSourceSideOfALink, RoomForAWindowedReaderTheSourcesGraphCannotSee,
    audio_windowing_declared_by_input_port_of, delivery_resolution_of_input_port,
    destination_max_notifiers, get_single_node, insert_loss_counts_on_graph_node_once,
    node_class_import_path_of, node_to_reclaim_from, notify_service_name_for,
    out_of_process_link_wiring_of, publish_loss_counts_on_graph_node,
    refuse_a_second_inbound_link_into_a_windowed_port, subscriber_ring_depth_of_input_port,
};

/// What the wiring of a link reads off its destination's graph: the input
/// port, how that port reads, whether its ports live out of process, and the
/// destination's notify service.
pub(crate) struct DestinationOfALinkAsItsWiringReadsIt {
    pub(super) dest_proc_id: NodeRunId,
    pub(super) dest_port: String,
    pub(super) link_wiring_of_a_destination_out_of_process:
        Option<Arc<OutOfProcessLinkWiringEnvelope>>,
    pub(super) audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
    pub(super) input_port_delivery: DeliveryResolution,
    pub(super) notify_service_name: String,
    pub(super) max_notifiers: usize,
    /// Whether a channel this destination is the first to open is made as deep
    /// as a windowed reader's ring, so one connected later fits it.
    pub(super) room_for_a_windowed_reader_joining_later:
        RoomForAWindowedReaderTheSourcesGraphCannotSee,
    /// The bundle members the link starts and ends at, which its subscriber takes from each
    /// message and puts into the bundle its port reads.
    pub(super) bundle_members_at_the_read: BundleMembersAtTheRead,
}

/// What the wiring of link `link_id`, from `from_port`, reads off its destination's graph when
/// it is read at `to_port`, `delivery_declared_by` the input that declares how it is delivered
/// and the link's own input end; a member of a bundle that input end names is put into what
/// `put_into` says.
pub(crate) fn read_the_destination_of_a_link(
    dest_graph: &mut Graph,
    from_port: &OutputLinkPortRef,
    to_port: &InputLinkPortRef,
    delivery_declared_by: &InputLinkPortRef,
    link_id: &LinkUniqueId,
    put_into: WhatTheMembersArePutInto,
) -> Result<DestinationOfALinkAsItsWiringReadsIt> {
    let dest_proc_id = to_port.node_run_id().clone();
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
    let input_port_delivery = delivery_resolution_of_input_port(
        dest_graph,
        delivery_declared_by.node_run_id(),
        delivery_declared_by.port_name(),
    )?;
    let max_notifiers = destination_max_notifiers(dest_graph, &dest_proc_id)?;
    // Every destination is notified whatever mode it runs in. One that never
    // drains its listener costs its sources a counter increment per frame:
    // iceoryx2 counts repeat notifications in shared memory and sends no
    // further wakeup until the listener drains.
    let notify_service_name = notify_service_name_for(&dest_proc_id);
    let member_taken_at_the_read = the_bundle_member_a_link_starts_at(dest_graph, from_port)
        .map(|(bundle, member)| MemberTakenAtTheRead { bundle, member });
    if let (Some(member), None) = (from_port.member(), &member_taken_at_the_read) {
        return Err(Error::InvalidLink(format!(
            "the link from `{from_port}` starts at member `{member}`, which its output no longer \
             holds, so it is not wired"
        )));
    }
    let member_put_at_the_read = the_bundle_member_a_link_ends_at(dest_graph, delivery_declared_by)
        .map(|(bundle, member)| MemberPutAtTheRead {
            bundle,
            member,
            put_into,
        });
    if let (Some(member), None) = (delivery_declared_by.member(), &member_put_at_the_read) {
        return Err(Error::InvalidLink(format!(
            "the link into `{delivery_declared_by}` ends at member `{member}`, which its input no \
             longer holds, so it is not wired"
        )));
    }
    if let Some(member_put_at_the_read) = &member_put_at_the_read
        && member_put_at_the_read.put_into == WhatTheMembersArePutInto::TheMembersTheLinksCarried
        && member_put_at_the_read.bundle.holds_a_heavy_member()
    {
        return Err(Error::InvalidLink(format!(
            "the link into `{delivery_declared_by}` ends at member `{}` of a {}, which holds a \
             Texture, Audio or encoded member, and a function node's island reads no such \
             bundle, so it is not wired",
            member_put_at_the_read.member.name, member_put_at_the_read.bundle
        )));
    }
    Ok(DestinationOfALinkAsItsWiringReadsIt {
        dest_proc_id,
        dest_port,
        link_wiring_of_a_destination_out_of_process,
        audio_windowing,
        input_port_delivery,
        notify_service_name,
        max_notifiers,
        room_for_a_windowed_reader_joining_later:
            RoomForAWindowedReaderTheSourcesGraphCannotSee::NotNeeded,
        bundle_members_at_the_read: BundleMembersAtTheRead {
            taken_from_its_output: member_taken_at_the_read,
            put_into_its_input: member_put_at_the_read,
        },
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
                &destination.bundle_members_at_the_read,
            )?
        } else {
            let dest_node = get_single_node(dest_graph, &destination.dest_proc_id)?;
            wire_rust_dest(
                dest_graph,
                &destination.dest_proc_id,
                &dest_node,
                &destination.dest_port,
                link_id,
                &opened_source_side.channel_service_name,
                destination.input_port_delivery,
                &opened_source_side.channel_data_service,
                &opened_source_side.destination_notify_service,
                destination.audio_windowing,
                destination.bundle_members_at_the_read,
            )?;
            None
        };
    dest_graph
        .traversal()
        .v(&dest_proc_id)
        .first()
        .ok_or_else(|| Error::NodeNotFound(format!("destination node '{dest_proc_id}' not found")))?
        .upstream_stream_facts_by_inbound_link()
        .record_inbound_link(
            &dest_port,
            link_id,
            InboundLinkName::from(opened_source_side.channel_service_name.as_str()),
            &opened_source_side.source_output_port,
            Arc::clone(&opened_source_side.stream_facts_of_the_sources_output_ports),
        );
    Ok(wire_reply_awaited_from_the_destination)
}

#[tracing::instrument(
    name = "compiler.close_iceoryx2_service",
    skip(graph),
    fields(link_id = %link_id)
)]

/// Drop link `link_id`'s channel subscriber off input port `dest_port` of
/// `dest_proc_id` (and the port mailbox / shared listener when their last
/// inbound link went away).
pub(crate) fn release_the_destination_side_of_a_link(
    graph: &mut Graph,
    dest_proc_id: &NodeRunId,
    dest_port: &str,
    link_id: &LinkUniqueId,
) {
    if let Some(dest_node) = graph.traversal().v(dest_proc_id).first() {
        dest_node
            .upstream_stream_facts_by_inbound_link()
            .forget_inbound_link(link_id);
    }
    if let Some(dest_link_wiring) = out_of_process_link_wiring_of(graph, dest_proc_id) {
        dest_link_wiring.unwire_a_link_reporting_a_far_side_that_kept_its_port(
            crate::core::PortDirection::Input,
            dest_port,
            link_id.as_str(),
        );
    } else if let Some(dest_node) = node_to_reclaim_from(graph, dest_proc_id) {
        let dest_guard = dest_node.lock();
        if let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() {
            input_inner.remove_channel_link(link_id.as_str());
            tracing::debug!(
                dest = %dest_proc_id,
                "Reclaimed destination-side ports for disconnected link"
            );
        }
    }
}

/// Subscribe the Rust destination to the channel bound to its local input port,
/// ensure its single listener exists, and publish its loss counts onto its graph
/// node.
///
/// A plain port's subscriber ring and mailbox take the port's own delivery
/// resolution. A windowed port's subscriber takes the windowed ring and its
/// mailbox is sized from its contract.
pub(super) fn wire_rust_dest(
    graph: &mut Graph,
    dest_proc_id: &NodeRunId,
    dest_node: &Arc<Mutex<NodeInstance>>,
    dest_port: &str,
    link_id: &LinkUniqueId,
    channel_service_name: &str,
    dest_input_port_delivery: DeliveryResolution,
    service: &Iceoryx2Service,
    notify_service: &Iceoryx2NotifyService,
    audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
    bundle_members_at_the_read: BundleMembersAtTheRead,
) -> Result<()> {
    let dest_guard = dest_node.lock();
    let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() else {
        return Ok(());
    };
    let DeliveryResolution {
        drain_order,
        depth: input_port_ring_depth,
        borrow_quota,
    } = dest_input_port_delivery;
    let subscriber_ring_depth =
        subscriber_ring_depth_of_input_port(dest_input_port_delivery, audio_windowing.as_ref());

    if !input_inner.has_a_mailbox(dest_port) {
        match audio_windowing {
            None => input_inner.add_port(dest_port, input_port_ring_depth, drain_order),
            Some(AudioWindowDeclarationOfAnInputPort::StatedOutright(contract)) => {
                input_inner.add_windowed_port(dest_port, drain_order, contract)
            }
            Some(AudioWindowDeclarationOfAnInputPort::MatchesItsNodesDeviceStream) => {
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

    input_inner.set_input_port_borrow_quota(dest_port, borrow_quota)?;

    let subscriber = service.create_subscriber(subscriber_ring_depth)?;
    input_inner.add_channel_subscriber(
        dest_port,
        link_id.as_str(),
        &InboundLinkName::from(channel_service_name),
        subscriber,
        bundle_members_at_the_read,
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
    publish_loss_counts_on_graph_node(graph, dest_proc_id, &dest_guard);
    publish_device_matched_audio_window_contracts_on_destination_node(
        graph,
        dest_proc_id,
        &input_inner,
    );
    Ok(())
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
    dest_proc_id: &NodeRunId,
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

/// `member`, a bundle member input port `dest_port` reads, as the helper wiring envelope carries
/// it.
fn the_member_on_the_wiring_envelope(
    member: &impl serde::Serialize,
    dest_port: &str,
) -> Result<serde_json::Value> {
    serde_json::to_value(member).map_err(|render_failure| {
        Error::Configuration(format!(
            "the member input port '{dest_port}' reads could not be rendered onto the helper \
             wiring envelope: {render_failure}"
        ))
    })
}

pub(super) fn wire_subprocess_dest(
    graph: &mut Graph,
    link_wiring: &OutOfProcessLinkWiringEnvelope,
    dest_proc_id: &NodeRunId,
    dest_port: &str,
    channel_service_name: &str,
    notify_service_name: &str,
    dest_input_port_delivery: DeliveryResolution,
    channel_sizing: ChannelSizing,
    notify_max_notifiers: usize,
    link_id: &LinkUniqueId,
    audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
    bundle_members_at_the_read: &BundleMembersAtTheRead,
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
        "input_port_borrow_quota": dest_input_port_delivery.borrow_quota,
        "channel_service_creation_depth": channel_sizing.channel_service_creation_depth,
        "input_port_ring_depth": subscriber_ring_depth_of_input_port(
            dest_input_port_delivery,
            audio_windowing.as_ref(),
        ),
        "max_subscribers": channel_sizing.max_subscribers,
        "notify_max_notifiers": notify_max_notifiers,
    });
    if let Some(member_taken_at_the_read) = &bundle_members_at_the_read.taken_from_its_output {
        entry["member_taken_at_the_read"] =
            the_member_on_the_wiring_envelope(member_taken_at_the_read, dest_port)?;
    }
    if let Some(member_put_at_the_read) = &bundle_members_at_the_read.put_into_its_input {
        entry["member_put_at_the_read"] =
            the_member_on_the_wiring_envelope(member_put_at_the_read, dest_port)?;
    }
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
        Some(AudioWindowDeclarationOfAnInputPort::MatchesItsNodesDeviceStream) => {
            let dest_type = node_class_import_path_of(graph, dest_proc_id)?;
            return Err(refuse_an_unsettled_match_device_sentinel(
                &dest_type, dest_port,
            ));
        }
    }

    // The slot and generation ride the entry, so the far side writes this
    // link's counts where the parent reads them for this wiring and no other.
    let loss_counts = link_wiring.helper_placed_node_loss_counts();
    let assigned = loss_counts.assign_inbound_link_slot(link_id.as_str(), into_a_windowed_port)?;
    entry["loss_count_slot"] = serde_json::json!(assigned.slot);
    entry["wiring_generation"] = serde_json::json!(assigned.wiring_generation);
    let wire_reply = link_wiring.record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
        crate::core::PortDirection::Input,
        entry,
    )?;
    insert_loss_counts_on_graph_node_once(
        graph,
        dest_proc_id,
        NodeLossCounts::CountedInItsHelperProcess(loss_counts),
    );
    Ok(wire_reply)
}
