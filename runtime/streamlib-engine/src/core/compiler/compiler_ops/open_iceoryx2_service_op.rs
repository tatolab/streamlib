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
    DeviceMatchedAudioWindowContractsComponent, Graph, GraphEdgeWithComponents,
    GraphNodeWithComponents, Iceoryx2ServicesHeldOpenForLinkComponent, Link, LinkState,
    LinkStateComponent, LinkUniqueId, OutOfProcessLinkWireRepliesComponent,
    ProcessorInstanceComponent, ProcessorMetrics,
};
use crate::core::processors::{OutOfProcessLinkWireReply, ProcessorInstance};
use crate::iceoryx2::{
    AudioWindowDeclarationOfAnInputPort, ChannelEgressConfig, ChannelSizing, ChannelTrustTier,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DeliveryProfile, DeliveryResolution, Iceoryx2Node,
    Iceoryx2NotifyService, Iceoryx2Service, InboundLinkName,
    RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL, WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
    audio_windowing_declared_by_input_port, delivery_profile_for_input_port,
    effective_channel_ceiling_bytes, refuse_an_unsettled_match_device_sentinel,
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
        (link.from_port().clone(), link.to_port().clone())
    };

    let (source_proc_id, source_port) =
        (from_port.processor_id.clone(), from_port.port_name.clone());
    let (dest_proc_id, dest_port) = (to_port.processor_id.clone(), to_port.port_name.clone());

    let source_is_subprocess = is_subprocess_processor(graph, &source_proc_id);
    let dest_is_subprocess = is_subprocess_processor(graph, &dest_proc_id);

    // A windowed destination is read and refused before any service is opened:
    // a second link into a port that windows, or one onto a channel too shallow
    // for its ring, must not leave half-wired iceoryx2 ports behind. A
    // `match_device` sentinel is not refused here — the compiler wires every
    // link before it releases any processor into `setup()`, which is where the
    // only format that can settle one comes from.
    let dest_audio_windowing =
        audio_windowing_declared_by_input_port_of(graph, &dest_proc_id, &dest_port)?;
    if dest_audio_windowing.is_some() {
        refuse_a_second_inbound_link_into_a_windowed_port(
            graph,
            &dest_proc_id,
            &dest_port,
            link_id,
        )?;
        refuse_a_windowed_port_onto_a_channel_created_shallower_than_its_ring(
            graph,
            iceoryx2_node,
            &source_proc_id,
            &source_port,
            &dest_proc_id,
            &dest_port,
            link_id,
        )?;
        tracing::debug!(
            dest = %dest_proc_id,
            port = %dest_port,
            windowing = ?dest_audio_windowing,
            "Destination input port declares a window contract; its reads run through the stage"
        );
    }

    let channel_service_name = channel_service_name(&source_proc_id, &source_port)?;

    // A notifier aimed at a destination that never drains its listener fills
    // that listener's queue and then silently stops being delivered for the
    // rest of the run, one iceoryx2 warning per frame (#1764). The notify
    // service exists to wake a waiting destination, so a destination that does
    // not wait gets none of it: no service, no notifier, no listener.
    let notify_service_name =
        destination_consumes_notifications(graph, &dest_proc_id, dest_is_subprocess)
            .then(|| notify_service_name_for(&dest_proc_id));

    tracing::info!(
        channel = %channel_service_name,
        notify = notify_service_name.as_deref().unwrap_or("<destination drains no listener>"),
        "Opening iceoryx2 channel: {} ({}:{}) -> ({}:{}) [{}] (source_subprocess={}, dest_subprocess={})",
        from_port,
        source_proc_id,
        source_port,
        dest_proc_id,
        dest_port,
        link_id,
        source_is_subprocess,
        dest_is_subprocess,
    );

    // A channel touching a subprocess on either end crosses a trust boundary and
    // gets the tighter untrusted-session ceiling; a host-to-host channel is
    // trusted. The ceiling is the graceful, observable layer in front of the
    // subprocess cgroup `memory.max` hard backstop.
    let trust_tier = if source_is_subprocess || dest_is_subprocess {
        ChannelTrustTier::UntrustedSession
    } else {
        ChannelTrustTier::Trusted
    };
    // The tier default is the structural ceiling; an operator raises or lowers it
    // per deployment through the tier's node-level env override.
    let channel_ceiling_bytes = effective_channel_ceiling_bytes(trust_tier);
    let channel_sizing =
        resolve_channel_sizing(graph, iceoryx2_node, &source_proc_id, &source_port)?;
    let dest_input_port_delivery =
        delivery_resolution_of_input_port(graph, &dest_proc_id, &dest_port)?;
    let max_notifiers = destination_max_notifiers(graph, &dest_proc_id)?;

    let service = iceoryx2_node.open_or_create_service(
        &channel_service_name,
        channel_sizing.max_subscribers,
        channel_sizing.channel_service_creation_depth,
    )?;
    let notify_service = notify_service_name
        .as_deref()
        .map(|name| iceoryx2_node.open_or_create_notify_service(name, max_notifiers))
        .transpose()?;

    // Every out-of-process end this link was handed to and has not answered
    // for. Empty is a link wholly in the app process, or one carried in a far
    // side's startup envelope — either way wired the moment this op returns.
    let mut wire_replies_awaited_from_its_out_of_process_ends = Vec::new();

    // Source side: install the single channel publisher (first link out of this
    // port) and append this link's destination notifier.
    if source_is_subprocess {
        wire_replies_awaited_from_its_out_of_process_ends.extend(wire_subprocess_source(
            graph,
            &source_proc_id,
            &source_port,
            &channel_service_name,
            notify_service_name.as_deref().unwrap_or(""),
            DEFAULT_EXPECTED_PAYLOAD_BYTES,
            channel_ceiling_bytes,
            channel_sizing,
            max_notifiers,
            link_id,
        )?);
    } else {
        let source_processor = get_single_processor(graph, &source_proc_id)?;
        wire_rust_source(
            graph,
            &source_proc_id,
            &source_processor,
            &source_port,
            link_id,
            &service,
            notify_service.as_ref(),
            ChannelEgressConfig {
                service_name: channel_service_name.clone(),
                trust_tier,
                expected_payload_bytes: DEFAULT_EXPECTED_PAYLOAD_BYTES,
                ceiling_bytes: channel_ceiling_bytes,
            },
        )?;
    }

    // Destination side: subscribe to the channel bound to this local input port,
    // and ensure the destination's single listener exists.
    if dest_is_subprocess {
        wire_replies_awaited_from_its_out_of_process_ends.extend(wire_subprocess_dest(
            graph,
            &dest_proc_id,
            &dest_port,
            &channel_service_name,
            notify_service_name.as_deref().unwrap_or(""),
            dest_input_port_delivery,
            channel_sizing,
            max_notifiers,
            link_id,
            dest_audio_windowing,
        )?);
    } else {
        let dest_processor = get_single_processor(graph, &dest_proc_id)?;
        wire_rust_dest(
            graph,
            &dest_proc_id,
            &dest_processor,
            &dest_port,
            link_id,
            &InboundLinkName::from(channel_service_name.as_str()),
            dest_input_port_delivery,
            &service,
            notify_service.as_ref(),
            dest_audio_windowing,
        )?;
    }

    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    link.insert_component_without_rendering_it(Iceoryx2ServicesHeldOpenForLinkComponent {
        channel_data_service: service,
        destination_notify_service: notify_service,
    });

    // A link an out-of-process end has not answered for is `Pending`, not
    // `Wired`: the engine has sent the wiring, and only that end opening its
    // own port makes the link carry anything. `graph` reads the answers off
    // the component below, which the end fills from its bridge's reader
    // thread. `docs/plan/ARCHITECTURE.md` §Processor model, the
    // `[local-transport-hardening]` entry.
    if wire_replies_awaited_from_its_out_of_process_ends.is_empty() {
        link.insert(LinkStateComponent(LinkState::Wired));
        tracing::info!(
            channel = %channel_service_name,
            "Opened iceoryx2 channel: [{}] (state: Wired)",
            link_id
        );
    } else {
        link.insert(LinkStateComponent(LinkState::Pending));
        link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(
            wire_replies_awaited_from_its_out_of_process_ends,
        ));
        tracing::info!(
            channel = %channel_service_name,
            "Opened iceoryx2 channel: [{}] (state: Pending, awaiting its helper's answer)",
            link_id
        );
    }
    Ok(())
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
/// reclaimed through [`DynGeneratedProcessor::unwire_out_of_process_link`] —
/// the host drops the far side's port and forgets the wiring envelope entry a
/// reconnect would otherwise be set up with twice.
///
/// [`DynGeneratedProcessor::unwire_out_of_process_link`]: crate::core::processors::DynGeneratedProcessor::unwire_out_of_process_link
#[tracing::instrument(name = "compiler.close_iceoryx2_service", skip(graph), fields(link_id = %link_id))]
pub fn close_iceoryx2_service(graph: &mut Graph, link_id: &LinkUniqueId) -> Result<()> {
    tracing::info!("Closing iceoryx2 service: {}", link_id);

    let Some((source_proc_id, source_port, dest_proc_id, dest_port)) =
        graph.traversal_mut().e(link_id).first().map(|link| {
            (
                link.from_port().processor_id.clone(),
                link.from_port().port_name.clone(),
                link.to_port().processor_id.clone(),
                link.to_port().port_name.clone(),
            )
        })
    else {
        tracing::warn!(
            "close_iceoryx2_service: link '{}' not in graph; nothing to reclaim",
            link_id
        );
        return Ok(());
    };

    let source_is_subprocess = is_subprocess_processor(graph, &source_proc_id);
    let dest_is_subprocess = is_subprocess_processor(graph, &dest_proc_id);

    // Source side: drop this link's destination notifier (and the channel
    // publisher when this was the source port's last outbound link).
    if let Some(source_processor) = processor_to_reclaim_from(graph, &source_proc_id) {
        let mut source_guard = source_processor.lock();
        if source_is_subprocess {
            unwire_out_of_process_endpoint(
                &mut source_guard,
                crate::core::PortDirection::Output,
                &source_proc_id,
                &source_port,
                link_id,
            );
        } else if let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() {
            let channel_released = output_inner.remove_channel_link(&source_port, link_id.as_str());
            tracing::debug!(
                source = %source_proc_id,
                port = %source_port,
                channel_released,
                "Reclaimed source-side egress for disconnected link"
            );
        }
    }

    // Destination side: drop this link's channel subscriber (and the port
    // mailbox / shared listener when their last inbound link went away).
    if let Some(dest_processor) = processor_to_reclaim_from(graph, &dest_proc_id) {
        let mut dest_guard = dest_processor.lock();
        if dest_is_subprocess {
            unwire_out_of_process_endpoint(
                &mut dest_guard,
                crate::core::PortDirection::Input,
                &dest_proc_id,
                &dest_port,
                link_id,
            );
        } else if let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() {
            input_inner.remove_channel_link(link_id.as_str());
            tracing::debug!(
                dest = %dest_proc_id,
                "Reclaimed destination-side ports for disconnected link"
            );
        }
    }

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

// ============================================================================
// Internal helpers
// ============================================================================

/// The channel service name a source output port publishes to —
/// `{source_processor}/{source_output_port}`, the single source of truth for
/// channel identity ([`crate::iceoryx2::source_channel_name`]). A grammar-illegal
/// port name surfaces as a named [`Error::Configuration`] here rather than an
/// opaque iceoryx2 `Invalid service name` deep in the FFI.
fn channel_service_name(source_proc_id: &ProcessorUniqueId, source_port: &str) -> Result<String> {
    crate::iceoryx2::source_channel_name(source_proc_id.as_str(), source_port)
        .map(|name| name.into_string())
        .map_err(|source| {
            Error::Configuration(format!(
                "cannot derive channel name for source '{}:{}': {}",
                source_proc_id, source_port, source
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

/// Every `connect()` link leaving `source_port` — the links of the one channel
/// that port publishes to, since a channel keys on its source output port.
fn links_out_of_source_output_port<'a>(
    graph: &'a Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &'a str,
) -> impl Iterator<Item = &'a Link> + use<'a> {
    graph
        .traversal()
        .v(source_proc_id)
        .out_e()
        .iter()
        .filter(move |link| link.from_port().port_name == source_port)
}

/// How many `connect()` links leave `source_port` — the destinations its
/// channel feeds.
fn channel_destination_count(
    graph: &Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> usize {
    links_out_of_source_output_port(graph, source_proc_id, source_port).count()
}

/// The `max_subscribers` every channel data service is created with:
/// [`MAX_DESTINATIONS_PER_CHANNEL`] destination slots plus
/// [`RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL`] — fixed, never the current
/// fan-out, because iceoryx2 pins the count at create time and a link
/// connected to a running source must fit a slot that already exists.
///
/// A source output port past the cap is refused here by name, before any
/// service is touched.
fn channel_max_subscribers(
    graph: &Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Result<usize> {
    let destinations = channel_destination_count(graph, source_proc_id, source_port);
    if destinations > MAX_DESTINATIONS_PER_CHANNEL {
        return Err(Error::Configuration(format!(
            "output port '{source_proc_id}:{source_port}' would feed {destinations} \
             destinations, and a channel carries at most {MAX_DESTINATIONS_PER_CHANNEL}"
        )));
    }
    Ok(MAX_DESTINATIONS_PER_CHANNEL + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL)
}

/// Derive the [`ChannelSizing`] for the channel keyed on `(source_proc_id,
/// source_port)` — the single derivation both the service-open compiler op and
/// the `tap` op share, refusing a source port past the destination cap.
pub(crate) fn resolve_channel_sizing(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Result<ChannelSizing> {
    Ok(ChannelSizing {
        max_subscribers: channel_max_subscribers(graph, source_proc_id, source_port)?,
        channel_service_creation_depth: channel_service_creation_depth(
            graph,
            iceoryx2_node,
            source_proc_id,
            source_port,
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
/// windows. Each subscriber then takes its own port's ring inside it, and a
/// shallower ring saves nothing — iceoryx2 sizes the sample pool from the
/// service's depth and subscriber count.
fn channel_service_creation_depth(
    graph: &Graph,
    iceoryx2_node: &Iceoryx2Node,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Result<usize> {
    if let Some(live_creation_depth) =
        creation_depth_of_the_live_channel(graph, iceoryx2_node, source_proc_id, source_port)?
    {
        return Ok(live_creation_depth);
    }
    for link in links_out_of_source_output_port(graph, source_proc_id, source_port)
        .filter(|link| link_still_counts_toward_its_ports(link))
    {
        let destination = link.to_port();
        if audio_windowing_declared_by_input_port_of(
            graph,
            &destination.processor_id,
            &destination.port_name,
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
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Result<Option<usize>> {
    let held_open_by_a_link = links_out_of_source_output_port(graph, source_proc_id, source_port)
        .find_map(|link| {
            link.get::<Iceoryx2ServicesHeldOpenForLinkComponent>()
                .map(|held| held.channel_data_service.channel_service_creation_depth())
        });
    if held_open_by_a_link.is_some() {
        return Ok(held_open_by_a_link);
    }
    Ok(iceoryx2_node
        .open_existing_channel_service(&channel_service_name(source_proc_id, source_port)?)?
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

/// Reverse-resolve a channel data-service name to the `(source_proc_id,
/// source_port)` that publishes to it, by scanning the graph's links for the
/// one whose source output port derives that channel name.
///
/// A channel's iceoryx2 data service only exists once a `connect()` has wired
/// its source output port, so a channel with no outbound link is genuinely
/// untappable — the caller maps `None` to [`Error::TapChannelNotFound`]. The
/// derivation is the same [`crate::iceoryx2::source_channel_name`] the compiler
/// op keys the service on, so a match here is exact (including the
/// hash-legalized over-budget form).
pub(crate) fn find_channel_source_port(
    graph: &mut Graph,
    channel_service_name: &str,
) -> Option<(ProcessorUniqueId, String)> {
    graph.traversal_mut().e(()).iter().find_map(|link| {
        let source = link.from_port();
        let derived =
            crate::iceoryx2::source_channel_name(source.processor_id.as_str(), &source.port_name)
                .ok()?;
        (derived.as_str() == channel_service_name)
            .then(|| (source.processor_id.clone(), source.port_name.clone()))
    })
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

/// Whether the destination ever drains the listener a notify service exists to
/// wake — the only condition under which opening one is worth anything.
///
/// Reactive is the sole host execution mode that waits on the listener fd.
/// Continuous and Manual drive themselves and poll their mailboxes, so a
/// notifier pointed at them fills the listener's queue and then stops being
/// delivered for the rest of the run, one iceoryx2 warning per frame (#1764).
///
/// The answer is a property of the destination's class, so it is the same for
/// every inbound link and the incremental `open_or_create` calls agree.
///
/// A destination out of process is assumed to drain, and that assumption is
/// only true of a reactive one. Every subprocess host reports `Manual` here —
/// that is the host thread's own mode, not the child's — and the child's
/// declared mode reaches no wiring-time surface, so this cannot yet ask. A
/// helper destination explicitly declared `continuous` or `manual` therefore
/// still gets a notifier its runner never drains, which is #1764 unfixed for
/// that one shape. Reaching it takes an author writing a non-reactive
/// execution mode onto a class that has input ports: Python defaults such a
/// class to `reactive`, which is what every scaffolded and in-tree helper
/// processor is.
fn destination_consumes_notifications(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_is_subprocess: bool,
) -> bool {
    if dest_is_subprocess {
        return true;
    }
    // A destination the graph cannot resolve is wired as before; the wiring
    // path itself reports the missing processor.
    get_single_processor(graph, dest_proc_id)
        .map(|dest_processor| {
            dest_processor
                .lock()
                .execution_config()
                .execution
                .is_reactive()
        })
        .unwrap_or(true)
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
    Ok(delivery_profile_for_input_port(&dest_type, dest_port)?.resolve())
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
    audio_windowing_declared_by_input_port(&dest_type, dest_port)
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
        .filter(|link| link.to_port().port_name == dest_port)
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
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let Some(live_creation_depth) =
        creation_depth_of_the_live_channel(graph, iceoryx2_node, source_proc_id, source_port)?
    else {
        return Ok(());
    };
    if live_creation_depth >= WINDOWED_PORT_SUBSCRIBER_RING_DEPTH {
        return Ok(());
    }
    Err(Error::Configuration(format!(
        "input port '{dest_port}' on '{dest_proc_id}' declares an `audio_window` contract, \
         and link '{link_id}' would wire it onto the running channel of \
         '{source_proc_id}:{source_port}', created {live_creation_depth} bags deep. A windowed \
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

/// Check if a processor is a subprocess.
fn is_subprocess_processor(graph: &mut Graph, proc_id: &ProcessorUniqueId) -> bool {
    graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        })
        .is_some_and(|proc_arc| proc_arc.lock().out_of_process_link_wiring().is_some())
}

/// Reclaim one link on an endpoint that owns its ports out of process: forget
/// the wiring the far side would be set up with again, then ask it to drop the
/// port it opened from that wiring.
///
/// The envelope is pruned here rather than by the host, so the record and the
/// erase stay on the same side of the seam — a host supplies the envelope and
/// the compiler op is the only thing that ever writes to it.
///
/// A failure is reported and swallowed, like every other reclaim failure here:
/// the disconnect is already happening, the other endpoint still has ports to
/// release, and refusing to stamp the link `Disconnected` over an unreachable
/// far side would leave the graph claiming a link that no longer carries data.
fn unwire_out_of_process_endpoint(
    processor: &mut ProcessorInstance,
    port_direction: crate::core::PortDirection,
    proc_id: &ProcessorUniqueId,
    local_port_name: &str,
    link_id: &LinkUniqueId,
) {
    if let Some(link_wiring) = processor.out_of_process_link_wiring() {
        link_wiring.remove_link(link_id.as_str());
    }
    match processor.unwire_out_of_process_link(port_direction, local_port_name, link_id.as_str()) {
        Ok(()) => tracing::debug!(
            proc_id = %proc_id,
            port = %local_port_name,
            port_direction = %port_direction,
            "Asked an out-of-process endpoint to reclaim its ports for a disconnected link"
        ),
        Err(error) => tracing::warn!(
            proc_id = %proc_id,
            port = %local_port_name,
            port_direction = %port_direction,
            error = %error,
            "close_iceoryx2_service: an out-of-process endpoint did not reclaim its ports; \
             a reconnect of this link may exhaust its channel's notifier or subscriber slots"
        ),
    }
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
/// publish its loss counts onto its graph node.
///
/// `notify_service` is `None` when the destination never drains a listener, and
/// the link is then wired for data only.
#[allow(clippy::too_many_arguments)]
fn wire_rust_source(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_processor: &Arc<Mutex<ProcessorInstance>>,
    source_port: &str,
    link_id: &LinkUniqueId,
    service: &Iceoryx2Service,
    notify_service: Option<&Iceoryx2NotifyService>,
    egress_config: ChannelEgressConfig,
) -> Result<()> {
    let source_guard = source_processor.lock();
    let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() else {
        return Ok(());
    };

    if !output_inner.has_channel_publisher(source_port) {
        let publisher = service.create_publisher(egress_config.expected_payload_bytes)?;
        output_inner.set_channel_publisher(source_port, publisher, egress_config);
        tracing::debug!(
            "Installed channel publisher for source output port '{}'",
            source_port
        );
    }

    let notifier = notify_service
        .map(|notify_service| notify_service.create_notifier())
        .transpose()?;
    output_inner.add_channel_link(source_port, link_id.as_str(), notifier);
    publish_loss_counts_on_processor_node(graph, source_proc_id, &source_guard);
    Ok(())
}

/// Subscribe the Rust destination to the channel bound to its local input port,
/// ensure its single listener exists, and publish its loss counts onto its graph
/// node.
///
/// A plain port's subscriber ring and mailbox take the port's own delivery
/// resolution. A windowed port's subscriber takes the windowed ring and its
/// mailbox is sized from its contract. `notify_service` is `None` when this
/// destination never drains a listener, and no listener is created for it.
#[allow(clippy::too_many_arguments)]
fn wire_rust_dest(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_processor: &Arc<Mutex<ProcessorInstance>>,
    dest_port: &str,
    link_id: &LinkUniqueId,
    inbound_link_name: &InboundLinkName,
    dest_input_port_delivery: DeliveryResolution,
    service: &Iceoryx2Service,
    notify_service: Option<&Iceoryx2NotifyService>,
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
    input_inner.add_channel_subscriber(dest_port, link_id.as_str(), inbound_link_name, subscriber);
    tracing::debug!(
        "Bound channel subscriber to destination input port '{}'",
        dest_port
    );

    if let Some(notify_service) = notify_service {
        if !input_inner.has_listener() {
            let listener = notify_service.create_listener()?;
            input_inner.set_listener(listener);
            tracing::debug!("Created listener for destination on its notify service");
        }
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
///
/// A processor whose ports live out of process never reaches here — it counts in
/// its own process, and its node carries no metrics at all rather than a zero
/// the parent cannot stand behind.
fn publish_loss_counts_on_processor_node(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
    processor: &ProcessorInstance,
) {
    let Some(node) = graph.traversal_mut().v(proc_id).first_mut() else {
        return;
    };
    if node.has::<ProcessorMetrics>() {
        return;
    }
    let input_inner = processor.iceoryx2_input_mailboxes_inner();
    node.insert(ProcessorMetrics {
        dropped_bag_counts_by_inbound_link: input_inner
            .as_ref()
            .map(|input_inner| input_inner.dropped_bag_counts_by_inbound_link())
            .unwrap_or_default(),
        discarded_sample_counts_by_inbound_link: input_inner
            .map(|input_inner| input_inner.discarded_sample_counts_by_inbound_link())
            .unwrap_or_default(),
        refused_bag_counts_by_output_port: processor
            .iceoryx2_output_writer_inner()
            .map(|output_inner| output_inner.refused_bag_counts_by_output_port())
            .unwrap_or_default(),
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
/// An empty `notify_service_name` is the wire's way of saying the destination
/// drains no listener, so the far side opens no notifier for this link. Every
/// SDK reads it that way.
///
/// Hands back the cell this end's answer will land in, or `None` where the
/// entry rides the far side's startup envelope instead and its `ready`
/// confirms it.
#[allow(clippy::too_many_arguments)]
fn wire_subprocess_source(
    graph: &mut Graph,
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
    // `enable_safe_overflow` is a wire fact, not a knob: iceoryx2 verifies it on
    // every reopen, so an SDK opening this service from its own bindings must
    // request the same value the engine did.
    let entry = serde_json::json!({
        "name": source_port,
        "link_id": link_id.to_string(),
        "enable_safe_overflow": true,
        "channel_service_name": channel_service_name,
        "dest_notify_service_name": notify_service_name,
        "expected_payload_bytes": expected_payload,
        "max_payload_bytes_per_channel": channel_ceiling_bytes,
        "channel_service_creation_depth": channel_sizing.channel_service_creation_depth,
        "max_subscribers": channel_sizing.max_subscribers,
        "notify_max_notifiers": notify_max_notifiers,
    });

    let source_proc_arc = get_single_processor(graph, source_proc_id)?;
    let mut source_processor = source_proc_arc.lock();
    let Some(link_wiring) = source_processor.out_of_process_link_wiring() else {
        // Classification and capability must agree: a processor reaches here
        // because `is_subprocess_processor` said so, and an instance that then
        // exposes no envelope would leave the link marked wired with nothing
        // ever recorded — no frames, no error, nothing to debug from.
        return Err(Error::Configuration(format!(
            "processor '{source_proc_id}' is classified as out-of-process but exposes no \
             link-wiring envelope; its output port '{source_port}' would never be wired"
        )));
    };
    link_wiring.record(crate::core::PortDirection::Output, entry.clone());
    // The envelope is read once, at setup; a far side already past it is
    // handed the entry directly.
    source_processor.wire_out_of_process_link(crate::core::PortDirection::Output, &entry)
}

/// Record this link's dest-side wiring on a processor whose transport lives out
/// of process, so it opens its own channel subscriber (bound to its local input
/// port) from the envelope.
///
/// Hands back the cell this end's answer will land in, on the same terms as
/// [`wire_subprocess_source`].
#[allow(clippy::too_many_arguments)]
fn wire_subprocess_dest(
    graph: &mut Graph,
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
    // The dest reader carries no payload-size hint: the subprocess read buffer
    // starts at the default and grows to the frame it actually receives
    // (PowerOfTwo segment growth on the publisher side, grow-and-retry on read).
    // The drain order is the port's own delivery profile's, resolved host-side.
    // The creation depth is what the child opens the service with, and the ring
    // depth what its subscriber takes — the windowed ring for a windowed port,
    // whose mailbox the child sizes from the contract, and otherwise the
    // profile's, which a plain port's mailbox takes too.
    // `enable_safe_overflow` is the same wire fact the source side records.
    let mut entry = serde_json::json!({
        "name": dest_port,
        "link_id": link_id.to_string(),
        "enable_safe_overflow": true,
        "channel_service_name": channel_service_name,
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

    let dest_proc_arc = get_single_processor(graph, dest_proc_id)?;
    let mut dest_processor = dest_proc_arc.lock();
    let Some(link_wiring) = dest_processor.out_of_process_link_wiring() else {
        return Err(Error::Configuration(format!(
            "processor '{dest_proc_id}' is classified as out-of-process but exposes no \
             link-wiring envelope; its input port '{dest_port}' would never be wired"
        )));
    };
    link_wiring.record(crate::core::PortDirection::Input, entry.clone());
    dest_processor.wire_out_of_process_link(crate::core::PortDirection::Input, &entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::execution::ExecutionConfig;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use crate::core::processors::{
        DynGeneratedProcessor, OutOfProcessLinkWireOutcome, ProcessorSpec,
    };
    use crate::core::test_support::CapturedTracingWarnings;
    use crate::core::{ProcessorDescriptor, RuntimeContextFullAccess, RuntimeContextLimitedAccess};

    /// One reclaim the engine asked an out-of-process endpoint for. Named
    /// rather than a tuple so a swapped port and link id fails the assert
    /// instead of passing it.
    #[derive(Debug, PartialEq, Eq)]
    struct ReclaimedLink {
        port_direction: crate::core::PortDirection,
        local_port_name: String,
        link_id: String,
    }

    /// A host whose transport lives out of process and which is neither of the
    /// engine's own subprocess hosts — the shape the wheel's helper spawn host
    /// has, from a crate this one cannot name.
    #[derive(Default)]
    struct OutOfCrateHelperSpawnHostStub {
        link_wiring: crate::core::processors::OutOfProcessLinkWiringEnvelope,
        /// Shared with the test, which is the only way to see what the engine
        /// asked of a host it cannot downcast to.
        reclaimed_links: Arc<Mutex<Vec<ReclaimedLink>>>,
        /// Every link the engine handed this host after its setup, with the
        /// direction it was wired in.
        late_wired_links: Arc<Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>>,
        /// The answer cells this host handed back, in the order the engine
        /// asked for them — how a test plays a far side that has not answered
        /// yet, opened its port, or refused.
        wire_answers_owed: Arc<Mutex<Vec<Arc<OutOfProcessLinkWireReply>>>>,
    }

    impl DynGeneratedProcessor for OutOfCrateHelperSpawnHostStub {
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
            "OutOfCrateHelperSpawnHostStub"
        }
        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            None
        }
        fn execution_config(&self) -> ExecutionConfig {
            ExecutionConfig::new(crate::core::execution::ProcessExecution::Manual)
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
        fn out_of_process_link_wiring(
            &mut self,
        ) -> Option<&mut crate::core::processors::OutOfProcessLinkWiringEnvelope> {
            Some(&mut self.link_wiring)
        }
        fn unwire_out_of_process_link(
            &mut self,
            port_direction: crate::core::PortDirection,
            local_port_name: &str,
            link_id: &str,
        ) -> Result<()> {
            self.reclaimed_links.lock().push(ReclaimedLink {
                port_direction,
                local_port_name: local_port_name.to_string(),
                link_id: link_id.to_string(),
            });
            Ok(())
        }
        fn wire_out_of_process_link(
            &mut self,
            port_direction: crate::core::PortDirection,
            link_wiring: &serde_json::Value,
        ) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
            self.late_wired_links
                .lock()
                .push((port_direction, link_wiring.clone()));
            let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
            self.wire_answers_owed.lock().push(Arc::clone(&reply));
            Ok(Some(reply))
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

    /// Attach `instance` to `proc_id` the way the spawn op does, so the wiring
    /// path can reach it.
    fn attach_processor_instance(
        graph: &mut Graph,
        proc_id: &str,
        instance: ProcessorInstance,
    ) -> Arc<Mutex<ProcessorInstance>> {
        let instance = Arc::new(Mutex::new(instance));
        graph
            .traversal_mut()
            .v(proc_id)
            .first_mut()
            .expect("the node must exist")
            .insert(ProcessorInstanceComponent(instance.clone()));
        instance
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
        wire_subprocess_source(
            graph,
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
        wire_subprocess_dest(
            graph,
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
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
        assert_eq!(
            recorded_source_ports["outputs"][0]["enable_safe_overflow"],
            serde_json::json!(true),
            "the envelope states the overflow mode iceoryx2 verifies on open; an SDK \
             that opens this service from its own bindings has nothing else to read it from"
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
        assert_eq!(
            recorded_dest_ports["inputs"][0]["enable_safe_overflow"],
            serde_json::json!(true),
            "both ends of the link state the same overflow mode — iceoryx2 rejects a \
             reopen that disagrees"
        );
    }

    /// A helper-placed destination's node carries no metrics at all.
    ///
    /// Its mailboxes are its own process's, so the parent counts none of its
    /// evictions and has nothing to render. Rendering an empty map or a zero
    /// here would say "this processor lost nothing", which the parent cannot
    /// know — the absent key is what makes it readable as unanswered rather
    /// than as healthy. The gap itself is plan-level (ARCHITECTURE.md's
    /// counting entry is unconditional); this locks the shape chosen for it so
    /// nobody closes it later with a zero.
    #[test]
    fn a_helper_placed_destinations_node_carries_no_metrics_rather_than_a_zero() {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        let dest_unique_id: ProcessorUniqueId = dest_id.as_str().into();
        attach_processor_instance(
            &mut graph,
            &source_id,
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
        );
        attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
        );

        record_wiring_for_both_out_of_process_endpoints(
            &mut graph,
            &source_id,
            &dest_id,
            &"L-helper-placed".into(),
        );

        assert!(
            graph
                .traversal_mut()
                .v(&dest_unique_id)
                .first()
                .expect("the destination node must be in the graph")
                .serialize_components()
                .get("metrics")
                .is_none(),
            "a destination the parent holds no mailboxes for must render no metrics key",
        );
    }

    /// Disconnecting a link whose endpoints both live out of process reclaims
    /// BOTH halves through the same seam that wired them, each told its own
    /// local port and direction — and each host's envelope forgets the link, so
    /// the next setup does not re-send it beside the reconnect's own entry.
    ///
    /// Revert lock: restore either `if !source_is_subprocess` /
    /// `if !dest_is_subprocess` guard and that side records no reclaim at all,
    /// which is the leak — a live helper child keeps the notifier and appends
    /// another on reconnect, until the notify service's create-time
    /// `max_notifiers` cap is exhausted (`ExceedsMaxSupportedNotifiers`).
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub {
                reclaimed_links: source_reclaims.clone(),
                ..Default::default()
            })),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub {
                reclaimed_links: dest_reclaims.clone(),
                ..Default::default()
            })),
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

    /// Wiring a link records it on the envelope AND hands the same entry to
    /// the host, so a far side that already read its envelope at setup opens
    /// the port now. Revert lock: drop the `wire_out_of_process_link` call
    /// after `record` and both vectors stay empty — the link the graph then
    /// reports `Wired` never reaches a running helper.
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub {
                late_wired_links: source_late_wired.clone(),
                ..Default::default()
            })),
        );
        let dest_instance = attach_processor_instance(
            &mut graph,
            &dest_id,
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub {
                late_wired_links: dest_late_wired.clone(),
                ..Default::default()
            })),
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
        );
        assert!(is_subprocess_processor(
            &mut graph,
            &helper_hosted_id.as_str().into()
        ));

        let engine_hosted_id = add_mock_input_only(&mut graph);
        assert!(!is_subprocess_processor(
            &mut graph,
            &engine_hosted_id.as_str().into()
        ));
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub {
                reclaimed_links: dest_reclaims.clone(),
                ..Default::default()
            })),
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
            open_test_link_services("mixed-endpoints", true, DeliveryProfile::ORDERED_DEPTH);
        wire_rust_source(
            &mut graph,
            &source_id.as_str().into(),
            &source,
            "out1",
            &link_id,
            &channel,
            notify_service.as_ref(),
            ChannelEgressConfig {
                service_name: unique_service_name("mixed-endpoints"),
                trust_tier: ChannelTrustTier::UntrustedSession,
                expected_payload_bytes: 4096,
                ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            },
        )
        .expect("the engine-side source wires");
        wire_subprocess_dest(
            &mut graph,
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
        destination_consumes_notifications: bool,
        channel_service_creation_depth: usize,
    ) -> (
        crate::iceoryx2::Iceoryx2Service,
        Option<crate::iceoryx2::Iceoryx2NotifyService>,
    ) {
        let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
        let channel = node
            .open_or_create_service(
                &unique_service_name(&format!("{tag}/channel")),
                2,
                channel_service_creation_depth,
            )
            .expect("the channel service must open");
        let notify = destination_consumes_notifications.then(|| {
            node.open_or_create_notify_service(&unique_service_name(&format!("{tag}/notify")), 1)
                .expect("the notify service must open")
        });
        (channel, notify)
    }

    /// The decision behind #1764: only a destination that will actually wait on
    /// its listener is worth opening a notify service for.
    ///
    /// Manual and Continuous destinations drive themselves and poll their
    /// mailboxes — a notifier aimed at one fills its listener and then floods
    /// the terminal for the rest of the run. Revert this predicate to a
    /// constant `true` and every `DisplayWindow`-shaped sink is back to that.
    #[test]
    fn only_a_reactive_or_out_of_process_destination_consumes_notifications() {
        use crate::core::test_support::{MockInputOnlyProcessor, MockReactiveInputOnlyProcessor};

        let mut graph = Graph::new();

        let self_driven_id = add_mock_input_only(&mut graph);
        attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut graph, &self_driven_id);
        assert!(
            !destination_consumes_notifications(&mut graph, &self_driven_id.as_str().into(), false),
            "a manual destination never drains its listener, so it must get no notifier"
        );

        let woken_id = add_mock_reactive_input_only(&mut graph);
        attach_mock_instance::<MockReactiveInputOnlyProcessor::Processor>(&mut graph, &woken_id);
        assert!(
            destination_consumes_notifications(&mut graph, &woken_id.as_str().into(), false),
            "a reactive destination waits on its listener fd and must keep its notifier"
        );

        // Out of process the runner selects on the fd whatever mode the class
        // declares, so the host cannot decide this from execution mode alone.
        let helper_hosted_id = add_mock_input_only(&mut graph);
        attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut graph, &helper_hosted_id);
        assert!(
            destination_consumes_notifications(&mut graph, &helper_hosted_id.as_str().into(), true),
            "a subprocess destination drains its own listener and must keep its notifier"
        );
    }

    /// Wire one Rust→Rust link end to end the way the compiler op does, with or
    /// without the notify service, and hand back the two sides' iceoryx2 state.
    fn wire_one_test_link<Destination>(
        tag: &str,
        destination_consumes_notifications: bool,
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

        let dest_id = if destination_consumes_notifications {
            add_mock_reactive_input_only(&mut graph)
        } else {
            add_mock_input_only(&mut graph)
        };
        let dest_unique_id: ProcessorUniqueId = dest_id.as_str().into();
        let (dest, _, dest_input) = attach_mock_instance::<Destination>(&mut graph, &dest_id);
        let dest_input = dest_input.expect("an input-only mock holds input mailboxes");

        let (channel, notify_service) = open_test_link_services(
            tag,
            destination_consumes_notifications,
            DeliveryProfile::ORDERED_DEPTH,
        );
        let link_id: LinkUniqueId = format!("L-{tag}").as_str().into();

        wire_rust_source(
            &mut graph,
            &source_id.as_str().into(),
            &source,
            "out1",
            &link_id,
            &channel,
            notify_service.as_ref(),
            ChannelEgressConfig {
                service_name: unique_service_name(tag),
                trust_tier: ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            },
        )
        .expect("the source side wires");
        wire_rust_dest(
            &mut graph,
            &dest_unique_id,
            &dest,
            "in1",
            &link_id,
            &InboundLinkName::from("psource/out1"),
            DeliveryProfile::Newest.resolve(),
            &channel,
            notify_service.as_ref(),
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

            let (channel, _) = open_test_link_services(tag, false, DeliveryProfile::ORDERED_DEPTH);
            let link_id: LinkUniqueId = format!("L-{tag}").as_str().into();
            wire_rust_source(
                &mut graph,
                &source_id,
                &source,
                "out1",
                &link_id,
                &channel,
                None,
                ChannelEgressConfig {
                    service_name: unique_service_name(tag),
                    trust_tier: ChannelTrustTier::Trusted,
                    expected_payload_bytes: 4096,
                    ceiling_bytes: source_ceiling_bytes,
                },
            )
            .expect("the source side wires");
            wire_rust_dest(
                &mut graph,
                &dest_id,
                &dest,
                "in1",
                &link_id,
                &InboundLinkName::from("psource/out1"),
                dest_delivery,
                &channel,
                None,
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
                    .write_raw("out1", b"a bag", bag as i64)
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
    /// `write_raw`, the destination's own `receive_pending` — so what is
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
            crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
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
            crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
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
            crate::iceoryx2::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
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
                "refused_bags_by_output_port": { "out1": 0 }
            })),
            "a wired output port that has refused nothing renders a zero"
        );

        link.source_output
            .write_raw("out1", &[0u8; CEILING_BYTES], 0)
            .expect_err("a bag past the ceiling is refused");
        link.write_bags(1);
        link.dest_input.receive_pending();

        assert_eq!(
            link.rendered_metrics_of(&source_id).unwrap()["refused_bags_by_output_port"],
            serde_json::json!({ "out1": 1 }),
        );
        assert_eq!(
            link.rendered_metrics_of(&dest_id).unwrap()["dropped_bags_by_link"],
            serde_json::json!({ link_id.as_str(): 0 }),
            "the refused bag is the producer's loss and no link's"
        );
    }

    /// The decision reaches the ports: no notify service means the source
    /// installs its channel publisher and no notifier, and the destination
    /// subscribes with no listener. Data wiring is untouched either way — the
    /// frames still flow, which is why #1764 cost terminal output and not video.
    #[test]
    fn a_destination_that_consumes_nothing_is_wired_for_data_only() {
        use crate::core::test_support::MockInputOnlyProcessor;

        let (source_output, dest_input) =
            wire_one_test_link::<MockInputOnlyProcessor::Processor>("data-only", false);

        assert!(
            source_output.has_channel_publisher("out1"),
            "the data path must be wired exactly as before"
        );
        assert!(
            dest_input.has_port("in1"),
            "the destination's mailbox must be wired exactly as before"
        );
        assert!(
            !dest_input.has_listener(),
            "a destination that never drains must hold no listener at all"
        );
        assert_eq!(
            source_output.channel_notifier_count("out1"),
            0,
            "the source must hold no notifier aimed at a destination that never drains"
        );
    }

    /// The same seam with a consuming destination still opens both ends —
    /// the fix removes notifiers only where nobody reads them.
    #[test]
    fn a_destination_that_consumes_notifications_keeps_its_notifier_and_listener() {
        use crate::core::test_support::MockReactiveInputOnlyProcessor;

        let (source_output, dest_input) =
            wire_one_test_link::<MockReactiveInputOnlyProcessor::Processor>("notified", true);

        assert_eq!(
            source_output.channel_notifier_count("out1"),
            1,
            "a reactive destination's link must still carry its notifier"
        );
        assert!(
            dest_input.has_listener(),
            "a reactive destination must still hold the listener its runner waits on"
        );
    }

    fn add_mock_output_only(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .first()
            .expect("mock_output_only_processor must be in the registry")
            .id
            .to_string()
    }

    fn add_mock_input_only(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockInputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .first()
            .expect("mock_input_only_processor must be in the registry")
            .id
            .to_string()
    }

    fn add_mock_ordered_input_only(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockOrderedInputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
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
                ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub {
                    wire_answers_owed: answers_owed.clone(),
                    ..Default::default()
                })),
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
        let channel_service_name = channel_service_name(&source_id.into(), "out1")
            .expect("the mock's output port derives a channel name");
        Iceoryx2Node::for_this_test_process()
            .open_or_create_service(
                &channel_service_name,
                MAX_DESTINATIONS_PER_CHANNEL + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
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
            .write_raw(
                "out1",
                b"a bag the port carried before the ordered consumer",
                0,
            )
            .expect("the port runs with one consumer");
        newest_consumer_input.receive_pending();
        newest_consumer_input.drain("in1");

        let ordered_link = add_link_from_out1_to_in1(&mut graph, &source_id, &ordered_consumer_id);
        open_iceoryx2_service(&mut graph, &ordered_link, &node)
            .expect("an ordered consumer connects onto the running port");

        for bag in 1..=BAGS_PUBLISHED_WHILE_NEITHER_CONSUMER_READS {
            source_output
                .write_raw("out1", b"a bag both consumers are fed", bag as i64)
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
        let rendered = crate::core::json_schema::LinkOutput::from(link);
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
            .first()
            .expect("the four-port mock must be in the registry")
            .id
            .to_string();
        let answers_owed: Arc<Mutex<Vec<Arc<OutOfProcessLinkWireReply>>>> = Arc::default();
        attach_processor_instance(
            &mut graph,
            &helper_id,
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub {
                wire_answers_owed: answers_owed.clone(),
                ..Default::default()
            })),
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

    fn add_mock_reactive_input_only(graph: &mut Graph) -> String {
        crate::core::test_support::ensure_test_mocks_registered();
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                crate::core::test_support::MockReactiveInputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
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
            ))
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
            ))
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

        let (channel, _) = open_test_link_services(
            "match-device-graph",
            false,
            WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
        );
        wire_rust_dest(
            &mut graph,
            &dest_unique_id,
            &dest,
            "audio",
            &"L-match-device".into(),
            &InboundLinkName::from("psource/audio_out"),
            DeliveryProfile::Ordered.resolve(),
            &channel,
            None,
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
        );

        let refusal = wire_subprocess_dest(
            &mut graph,
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
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

        wire_subprocess_dest(
            &mut graph,
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
                &channel_service_name(&source_id.as_str().into(), "out1")
                    .expect("the mock's output port derives a channel name"),
                MAX_DESTINATIONS_PER_CHANNEL + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
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
                .write_raw("out1", &a_mono_16k_audio_block(160, 0), 0)
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
            .write_raw("out1", &a_mono_16k_audio_block(300, 0), 0)
            .expect("the source publishes");
        assert!(
            link.dest_input
                .read_raw("audio")
                .expect("the read succeeds")
                .is_none(),
            "300 of 512 samples is not a window"
        );
        link.source_output
            .write_raw("out1", &a_mono_16k_audio_block(300, 1_000_000_000), 0)
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
            ProcessorInstance::new(Box::new(OutOfCrateHelperSpawnHostStub::default())),
        );
        let Some(declared) =
            audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "audio")
                .expect("the mock's contract resolves")
        else {
            panic!("the mock's audio port declares a window contract");
        };

        wire_subprocess_dest(
            &mut graph,
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
    /// source-centric (`{source}/{port}`), NOT destination-centric. This is the
    /// transport inversion (#1419): channel identity keys on the source only.
    /// Mentally revert to `streamlib/{dest}` and this fails — the derived name is
    /// a pure function of the source processor id + output port.
    #[test]
    fn channel_service_name_is_source_port_shaped() {
        let name = channel_service_name(&"Pabc123".into(), "video_out")
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
    fn channel_max_subscribers_is_the_fixed_cap_plus_tap_and_refuses_past_it() {
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
            channel_max_subscribers(&graph, &src_uid, "out1")
                .expect("three destinations fit the cap"),
            MAX_DESTINATIONS_PER_CHANNEL + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
            "the channel is sized for the cap, not for the three it feeds today",
        );

        for _ in 3..=MAX_DESTINATIONS_PER_CHANNEL {
            connect_one_more_destination(&mut graph);
        }
        let refused = channel_max_subscribers(&graph, &src_uid, "out1")
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
            &src_uid,
            "out1",
        )
        .expect("sizing resolves for a wired channel");
        assert_eq!(
            sizing.max_subscribers,
            channel_max_subscribers(&graph, &src_uid, "out1")
                .expect("two destinations fit the cap"),
            "resolve_channel_sizing must agree with channel_max_subscribers — the \
             single derivation both the service-open op and the tap op share",
        );
    }

    /// A wired channel's data-service name reverse-resolves to the exact
    /// `(source_proc, source_port)` that publishes to it; an unknown name
    /// resolves to `None` (the tap op maps that to `TapChannelNotFound`).
    /// Round-trips through the same `source_channel_name` the compiler op keys
    /// the service on.
    #[test]
    fn find_channel_source_port_round_trips_and_misses() {
        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&dest_id, "in1"),
        );

        let channel_name = crate::iceoryx2::source_channel_name(&src_id, "out1")
            .expect("source port derives a channel name")
            .into_string();

        // The reverse lookup returns the graph node's original processor id (the
        // channel name lowercases it only for the wire), so it round-trips to the
        // id we wired, not its lowercased channel form.
        let (resolved_proc, resolved_port) =
            find_channel_source_port(&mut graph, &channel_name).expect("wired channel resolves");
        assert_eq!(resolved_proc.as_str(), src_id.as_str());
        assert_eq!(resolved_port, "out1");

        assert!(
            find_channel_source_port(&mut graph, "nosuch/channel").is_none(),
            "an unwired / unknown channel name must not resolve to any source port",
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
}
