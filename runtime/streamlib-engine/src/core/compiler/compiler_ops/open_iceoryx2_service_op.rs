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
use crate::core::context::RuntimeContext;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    DeviceMatchedAudioWindowContractsComponent, Graph, GraphEdgeWithComponents,
    GraphNodeWithComponents, LinkState, LinkStateComponent, LinkUniqueId,
    ProcessorInstanceComponent, ProcessorMetrics, SubprocessHandleComponent,
};
use crate::core::processors::ProcessorInstance;
use crate::iceoryx2::{
    AudioWindowDeclarationOfAnInputPort, ChannelEgressConfig, ChannelTrustTier,
    DEFAULT_EXPECTED_PAYLOAD_BYTES, Iceoryx2NotifyService, Iceoryx2Service, InboundLinkName,
    RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL, audio_windowing_declared_by_input_port,
    delivery_profile_for_input_port, effective_channel_ceiling_bytes,
    refuse_an_unsettled_match_device_sentinel,
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
/// - subprocess→subprocess: both sides open their own ports; the host only
///   pre-creates the services so their sizing is fixed once.
#[tracing::instrument(
    name = "compiler.open_iceoryx2_service",
    skip(graph, runtime_ctx),
    fields(link_id = %link_id)
)]
pub fn open_iceoryx2_service(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
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
    // a second link into a port that windows must not leave half-wired iceoryx2
    // ports behind. A `match_device` sentinel is not refused here — the
    // compiler wires every link before it releases any processor into `setup()`,
    // which is where the only format that can settle one comes from.
    let dest_audio_windowing =
        audio_windowing_declared_by_input_port_of(graph, &dest_proc_id, &dest_port)?;
    if dest_audio_windowing.is_some() {
        refuse_a_second_inbound_link_into_a_windowed_port(
            graph,
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
    // Subscriber count is the compile-time destination fan-out plus the reserved
    // tap slot. Ring depth and consumer drain order both derive from the single
    // delivery profile the channel's destinations agree on.
    let ChannelSizing {
        max_subscribers,
        max_queued_messages,
        drain_order,
    } = resolve_channel_sizing(graph, &source_proc_id, &source_port)?;
    let max_notifiers = destination_fanin(graph, &dest_proc_id);

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(
        &channel_service_name,
        max_subscribers,
        max_queued_messages,
    )?;
    let notify_service = notify_service_name
        .as_deref()
        .map(|name| iceoryx2_node.open_or_create_notify_service(name, max_notifiers))
        .transpose()?;

    // Source side: install the single channel publisher (first link out of this
    // port) and append this link's destination notifier.
    if source_is_subprocess {
        wire_subprocess_source(
            graph,
            &source_proc_id,
            &source_port,
            &channel_service_name,
            notify_service_name.as_deref().unwrap_or(""),
            DEFAULT_EXPECTED_PAYLOAD_BYTES,
            channel_ceiling_bytes,
            max_queued_messages,
            max_subscribers,
            max_notifiers,
            link_id,
        )?;
    } else {
        let source_processor = get_single_processor(graph, &source_proc_id)?;
        wire_rust_source(
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
        wire_subprocess_dest(
            graph,
            &dest_proc_id,
            &dest_port,
            &channel_service_name,
            notify_service_name.as_deref().unwrap_or(""),
            drain_order,
            max_queued_messages,
            max_subscribers,
            max_notifiers,
            link_id,
            dest_audio_windowing,
        )?;
    } else {
        let dest_processor = get_single_processor(graph, &dest_proc_id)?;
        wire_rust_dest(
            graph,
            &dest_proc_id,
            &dest_processor,
            &dest_port,
            link_id,
            &InboundLinkName::from(channel_service_name.as_str()),
            drain_order,
            max_queued_messages,
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
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!(
        channel = %channel_service_name,
        "Opened iceoryx2 channel: [{}] (state: Wired)",
        link_id
    );
    Ok(())
}

/// Reclaim one `connect()` link's iceoryx2 ports on `disconnect`.
///
/// Stamping [`LinkState::Disconnected`] is not enough: the source-side notifier
/// and dest-side subscriber (plus listener, orphaned mailbox, channel publisher)
/// must be dropped to release their iceoryx2 services, else a reconnect re-appends
/// past the notify service's create-time `max_notifiers` cap
/// (`ExceedsMaxSupportedNotifiers`) and the stale, shallower-sized data service
/// collides with a deeper-ring reopen (`DoesNotSupportRequestedMinBufferSize`).
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

/// The `(dest_proc_id, dest_port)` set a channel feeds — every `connect()` link
/// out of `source_port`. This predicate IS the definition of a channel's
/// membership: a channel keys on its source output port, so its destinations are
/// exactly the links leaving that port.
///
/// The full graph is built by the time the compiler op runs, so this outbound
/// set is stable — every link out of the same source port sees the same set,
/// which is what lets the incremental `open_or_create` calls agree (iceoryx2
/// verifies `max_subscribers` on reopen).
fn channel_destinations(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Vec<(ProcessorUniqueId, String)> {
    graph
        .traversal_mut()
        .v(source_proc_id)
        .out_e()
        .iter()
        .filter(|link| link.from_port().port_name == source_port)
        .map(|link| {
            (
                link.to_port().processor_id.clone(),
                link.to_port().port_name.clone(),
            )
        })
        .collect()
}

/// The `max_subscribers` a channel data service must be created with: the count
/// of destinations the channel feeds (each is one destination subscriber) plus
/// [`RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL`].
fn channel_max_subscribers(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> usize {
    channel_destinations(graph, source_proc_id, source_port).len()
        + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL
}

/// The iceoryx2 sizing a channel data service is opened with — the fixed
/// parameters iceoryx2 verifies on every reopen of the same service name.
///
/// Both the compiler op (which creates the service with a publisher) and the
/// phase-3.5 `tap` op (which reopens it publisher-free to add a reserved-slot
/// subscriber) derive this from the SAME graph state via
/// [`resolve_channel_sizing`], so their `open_or_create_service` calls agree —
/// a mismatched `max_subscribers` / `subscriber_max_buffer_size` would be
/// rejected by iceoryx2 on open.
pub(crate) struct ChannelSizing {
    /// Compile-time destination count plus the reserved tap slot.
    pub(crate) max_subscribers: usize,
    /// Ring depth (`subscriber_max_buffer_size`) — the agreed delivery profile's depth.
    pub(crate) max_queued_messages: usize,
    /// The agreed delivery profile's consumer drain order.
    pub(crate) drain_order: crate::iceoryx2::ReadMode,
}

/// Derive the [`ChannelSizing`] for the channel keyed on `(source_proc_id,
/// source_port)` from the current graph — the single derivation both the
/// service-open compiler op and the `tap` op share so their `open_or_create`
/// calls request identical, iceoryx2-verified parameters.
pub(crate) fn resolve_channel_sizing(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Result<ChannelSizing> {
    let delivery = channel_delivery_profile(graph, source_proc_id, source_port)?.resolve();
    Ok(ChannelSizing {
        max_subscribers: channel_max_subscribers(graph, source_proc_id, source_port),
        max_queued_messages: delivery.depth,
        drain_order: delivery.drain_order,
    })
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

/// The destination's compile-time fan-in — the count of inbound `connect()`
/// links — which sizes `max_notifiers` on its destination-keyed notify service.
fn destination_fanin(graph: &mut Graph, dest_proc_id: &ProcessorUniqueId) -> usize {
    graph.traversal_mut().v(dest_proc_id).in_e().iter().count()
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

/// The channel's [`DeliveryProfile`], agreed across every destination the
/// channel feeds.
///
/// A channel's single publisher shares one ring depth across all subscribers
/// and its destinations drain in one order, so they must resolve to one
/// delivery profile. A channel whose destinations disagree (`newest` vs
/// `ordered`, say) is genuinely ambiguous in both — a named
/// [`Error::Configuration`] rather than a silent pick. A channel with a single
/// destination (the common case) uses that destination's profile.
///
/// [`DeliveryProfile`]: crate::iceoryx2::DeliveryProfile
fn channel_delivery_profile(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Result<crate::iceoryx2::DeliveryProfile> {
    // Collected up front so the traversal borrow is released before re-traversing
    // per edge to read each destination's processor type.
    let destinations = channel_destinations(graph, source_proc_id, source_port);

    let mut agreed: Option<crate::iceoryx2::DeliveryProfile> = None;
    for (dest_proc_id, dest_port) in &destinations {
        let dest_type = graph
            .traversal_mut()
            .v(dest_proc_id)
            .first()
            .map(|node| node.processor_type().clone());
        let profile = match dest_type.as_ref() {
            Some(ident) => delivery_profile_for_input_port(ident, dest_port)?,
            None => crate::iceoryx2::DeliveryProfile::Newest,
        };
        match agreed {
            None => agreed = Some(profile),
            Some(prev) if prev != profile => {
                return Err(Error::Configuration(format!(
                    "channel '{}:{}' feeds destinations with conflicting delivery \
                     profiles — '{}' vs '{}'. A channel's single publisher shares \
                     one ring config across all subscribers; give the destinations \
                     the same input-port delivery profile, or fan them out through \
                     distinct source ports.",
                    source_proc_id,
                    source_port,
                    prev.as_manifest_str(),
                    profile.as_manifest_str(),
                )));
            }
            Some(_) => {}
        }
    }

    // Every wired link has at least the current destination, so `agreed` is Some;
    // the realtime default is the correct fallback if the outbound set were empty.
    Ok(agreed.unwrap_or(crate::iceoryx2::DeliveryProfile::Newest))
}

/// The window declaration this destination's input port carries, if it carries
/// one.
///
/// Reads the destination's registered class, the same way the delivery profile
/// is read: the declaration is the whole answer and nothing is inferred.
fn audio_windowing_declared_by_input_port_of(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
) -> Result<Option<AudioWindowDeclarationOfAnInputPort>> {
    let Some(dest_type) = graph
        .traversal_mut()
        .v(dest_proc_id)
        .first()
        .map(|node| node.processor_type().clone())
    else {
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
        // A link on its way out of the graph is not a second inbound one, or a
        // disconnect followed by a reconnect of the same port would refuse
        // itself.
        .filter(|link| {
            link.get::<LinkStateComponent>()
                .map(|state| {
                    !matches!(
                        state.0,
                        LinkState::Disconnecting | LinkState::Disconnected | LinkState::Error
                    )
                })
                .unwrap_or(true)
        })
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

/// Check if a processor is a subprocess.
fn is_subprocess_processor(graph: &mut Graph, proc_id: &ProcessorUniqueId) -> bool {
    let has_component = graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .map(|n| n.has::<SubprocessHandleComponent>())
        .unwrap_or(false);
    if has_component {
        return true;
    }

    if let Some(proc_arc) = graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        })
    {
        if proc_arc.lock().out_of_process_link_wiring().is_some() {
            return true;
        }
    }

    false
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

/// Install (once) the source's single channel publisher and append this link's
/// destination notifier onto the Rust source's [`OutputWriterInner`].
///
/// `notify_service` is `None` when the destination never drains a listener, and
/// the link is then wired for data only.
fn wire_rust_source(
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
    Ok(())
}

/// Subscribe the Rust destination to the channel bound to its local input port,
/// ensure its single listener exists, and publish its dropped-bag counts onto
/// its graph node.
///
/// `notify_service` is `None` when this destination never drains a listener, and
/// no listener is created for it.
#[allow(clippy::too_many_arguments)]
fn wire_rust_dest(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_processor: &Arc<Mutex<ProcessorInstance>>,
    dest_port: &str,
    link_id: &LinkUniqueId,
    inbound_link_name: &InboundLinkName,
    drain_order: crate::iceoryx2::ReadMode,
    depth: usize,
    service: &Iceoryx2Service,
    notify_service: Option<&Iceoryx2NotifyService>,
    audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
) -> Result<()> {
    let dest_guard = dest_processor.lock();
    let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() else {
        return Ok(());
    };

    if !input_inner.has_port(dest_port) {
        match audio_windowing {
            None => input_inner.add_port(dest_port, depth, drain_order),
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
                        depth,
                        drain_order,
                    ),
                }
            }
        }
    }

    let subscriber = service.create_subscriber()?;
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
    publish_dropped_bag_counts_on_destination_node(graph, dest_proc_id, &input_inner);
    publish_device_matched_audio_window_contracts_on_destination_node(
        graph,
        dest_proc_id,
        &input_inner,
    );
    Ok(())
}

/// Share the destination's per-inbound-link dropped-bag counts onto its graph
/// node, so `graph` reads them live off the mailboxes that do the evicting.
///
/// Inserted with the destination's first inbound link and left alone after: the
/// counts are one shared object for the whole processor, and a link wired later
/// mints its own zeroed entry inside it.
///
/// A destination whose mailboxes live out of process never reaches here — it
/// counts its evictions in its own process, and its node carries no metrics at
/// all rather than a zero the parent cannot stand behind.
fn publish_dropped_bag_counts_on_destination_node(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    input_inner: &Arc<crate::iceoryx2::InputMailboxesInner>,
) {
    let Some(node) = graph.traversal_mut().v(dest_proc_id).first_mut() else {
        return;
    };
    if node.has::<ProcessorMetrics>() {
        return;
    }
    node.insert(ProcessorMetrics {
        dropped_bag_counts_by_inbound_link: input_inner.dropped_bag_counts_by_inbound_link(),
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
#[allow(clippy::too_many_arguments)]
fn wire_subprocess_source(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
    channel_service_name: &str,
    notify_service_name: &str,
    expected_payload: usize,
    channel_ceiling_bytes: usize,
    max_queued_messages: usize,
    max_subscribers: usize,
    notify_max_notifiers: usize,
    link_id: &LinkUniqueId,
) -> Result<()> {
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
        "max_queued_messages": max_queued_messages,
        "max_subscribers": max_subscribers,
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
    link_wiring.record(crate::core::PortDirection::Output, entry);
    Ok(())
}

/// Record this link's dest-side wiring on a processor whose transport lives out
/// of process, so it opens its own channel subscriber (bound to its local input
/// port) from the envelope.
#[allow(clippy::too_many_arguments)]
fn wire_subprocess_dest(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
    channel_service_name: &str,
    notify_service_name: &str,
    drain_order: crate::iceoryx2::ReadMode,
    max_queued_messages: usize,
    max_subscribers: usize,
    notify_max_notifiers: usize,
    link_id: &LinkUniqueId,
    audio_windowing: Option<AudioWindowDeclarationOfAnInputPort>,
) -> Result<()> {
    // The dest reader no longer carries a payload-size hint: the subprocess read
    // buffer starts at the default and grows to the frame it actually receives
    // (PowerOfTwo segment growth on the publisher side, grow-and-retry on read).
    // The drain order is the delivery profile's, resolved host-side; the
    // subprocess maps the string back to its `*_input_set_read_mode` integer.
    // `enable_safe_overflow` is the same wire fact the source side records.
    let mut entry = serde_json::json!({
        "name": dest_port,
        "link_id": link_id.to_string(),
        "enable_safe_overflow": true,
        "channel_service_name": channel_service_name,
        "notify_service_name": notify_service_name,
        "read_mode": drain_order.as_manifest_str(),
        "max_queued_messages": max_queued_messages,
        "max_subscribers": max_subscribers,
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
            let dest_type = graph
                .traversal_mut()
                .v(dest_proc_id)
                .first()
                .map(|node| node.processor_type().clone())
                .ok_or_else(|| {
                    Error::ProcessorNotFound(format!("Processor '{dest_proc_id}' not found"))
                })?;
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
    link_wiring.record(crate::core::PortDirection::Input, entry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::execution::ExecutionConfig;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use crate::core::processors::{DynGeneratedProcessor, PROCESSOR_REGISTRY, ProcessorSpec};
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
            8,
            2,
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
            crate::iceoryx2::ReadMode::SkipToLatest,
            8,
            2,
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

        let (channel, notify_service) = open_test_link_services("mixed-endpoints", true);
        wire_rust_source(
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
            crate::iceoryx2::ReadMode::SkipToLatest,
            8,
            2,
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

    /// The channel and notify services one wired link needs, on one node.
    fn open_test_link_services(
        tag: &str,
        destination_consumes_notifications: bool,
    ) -> (
        crate::iceoryx2::Iceoryx2Service,
        Option<crate::iceoryx2::Iceoryx2NotifyService>,
    ) {
        let node = crate::iceoryx2::Iceoryx2Node::new().expect("an iceoryx2 node must open");
        let channel = node
            .open_or_create_service(&unique_service_name(&format!("{tag}/channel")), 2, 8)
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

        let (channel, notify_service) =
            open_test_link_services(tag, destination_consumes_notifications);
        let link_id: LinkUniqueId = format!("L-{tag}").as_str().into();

        wire_rust_source(
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
            crate::iceoryx2::ReadMode::SkipToLatest,
            8,
            &channel,
            notify_service.as_ref(),
            None,
        )
        .expect("the destination side wires");

        (source_output, dest_input)
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
        use crate::core::test_support::{MockInputOnlyProcessor, MockOutputOnlyProcessor};

        const DESTINATION_MAILBOX_DEPTH: usize = 1;
        const FRAMES_PUBLISHED: usize = 4;

        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let (source, source_output, _) =
            attach_mock_instance::<MockOutputOnlyProcessor::Processor>(&mut graph, &source_id);
        let source_output = source_output.expect("an output-only mock holds an output writer");
        let dest_id = add_mock_input_only(&mut graph);
        let dest_unique_id: ProcessorUniqueId = dest_id.as_str().into();
        let (dest, _, dest_input) =
            attach_mock_instance::<MockInputOnlyProcessor::Processor>(&mut graph, &dest_id);
        let dest_input = dest_input.expect("an input-only mock holds input mailboxes");

        let (channel, _) = open_test_link_services("dropped-bag-counts", false);
        let link_id: LinkUniqueId = "L-dropping".into();
        wire_rust_source(
            &source,
            "out1",
            &link_id,
            &channel,
            None,
            ChannelEgressConfig {
                service_name: unique_service_name("dropped-bag-counts"),
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
            crate::iceoryx2::ReadMode::ReadNextInOrder,
            DESTINATION_MAILBOX_DEPTH,
            &channel,
            None,
            None,
        )
        .expect("the destination side wires");

        let rendered_metrics = |graph: &mut Graph| -> serde_json::Value {
            graph
                .traversal_mut()
                .v(&dest_unique_id)
                .first()
                .expect("the destination node must be in the graph")
                .serialize_components()["metrics"]
                .clone()
        };

        assert_eq!(
            rendered_metrics(&mut graph)["dropped_bags_by_link"],
            serde_json::json!({ "L-dropping": 0 }),
            "a wired link that has lost nothing must render a zero, not go missing"
        );

        for frame in 0..FRAMES_PUBLISHED {
            source_output
                .write_raw("out1", b"a bag the destination never reads", frame as i64)
                .expect("the source publishes onto the wired channel");
        }
        dest_input.receive_pending();

        let metrics = rendered_metrics(&mut graph);
        assert_eq!(
            metrics["dropped_bags_by_link"],
            serde_json::json!({ "L-dropping": FRAMES_PUBLISHED - DESTINATION_MAILBOX_DEPTH }),
            "the node must read the counts live, off the mailboxes that evicted"
        );
        assert_eq!(
            metrics["frames_dropped"],
            serde_json::json!(FRAMES_PUBLISHED - DESTINATION_MAILBOX_DEPTH),
            "the total must be the per-link counts summed, never a second tally"
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

        let windowing = audio_windowing_declared_by_input_port_of(
            &mut graph,
            &dest_id.as_str().into(),
            "audio",
        )
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
            audio_windowing_declared_by_input_port_of(&mut graph, &dest_id.as_str().into(), "in1")
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

        let windowing = audio_windowing_declared_by_input_port_of(
            &mut graph,
            &dest_id.as_str().into(),
            "audio",
        )
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

        let (channel, _) = open_test_link_services("match-device-graph", false);
        wire_rust_dest(
            &mut graph,
            &dest_unique_id,
            &dest,
            "audio",
            &"L-match-device".into(),
            &InboundLinkName::from("psource/audio_out"),
            crate::iceoryx2::ReadMode::ReadNextInOrder,
            crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
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
            crate::iceoryx2::ReadMode::ReadNextInOrder,
            8,
            2,
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
            crate::iceoryx2::ReadMode::ReadNextInOrder,
            8,
            2,
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

    /// A source output port feeding N destinations opens ONE channel sized for
    /// `N + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL` subscribers — the 1→N
    /// fan-out subscriber count. Mentally revert the outbound-edge count to a
    /// fixed `1` (the pre-inversion single-subscriber destination service) and
    /// this returns the wrong count; drop the reserved tap term and the tap slot
    /// disappears.
    #[test]
    fn channel_max_subscribers_counts_destinations_plus_tap() {
        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);

        // Three distinct destinations subscribe to the SAME source output port.
        for _ in 0..3 {
            let dest_id = add_mock_input_only(&mut graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        }

        let src_uid: ProcessorUniqueId = src_id.as_str().into();
        let subs = channel_max_subscribers(&mut graph, &src_uid, "out1");
        assert_eq!(
            subs,
            3 + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
            "one source port feeding 3 destinations must size the channel for 3 \
             subscribers plus the reserved tap slot",
        );
    }

    /// The tap op reconstructs the exact `max_subscribers` the compiler op
    /// opened the service with — `destinations + reserved tap` — via the shared
    /// [`resolve_channel_sizing`]. iceoryx2 verifies `max_subscribers` on the
    /// tap's publisher-free reopen, so a drift here would make every tap fail to
    /// open. Mentally revert the reserved-tap term in `channel_max_subscribers`
    /// and this count drops below what the service was created with.
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

        let sizing = resolve_channel_sizing(&mut graph, &src_uid, "out1")
            .expect("sizing resolves for a wired channel");
        assert_eq!(
            sizing.max_subscribers,
            2 + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL,
            "the tap must reopen the service with the same max_subscribers the \
             compiler op created it with (2 destinations + reserved tap)",
        );
        assert_eq!(
            sizing.max_subscribers,
            channel_max_subscribers(&mut graph, &src_uid, "out1"),
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

    /// The destination fan-in (inbound link count) sizes the destination-keyed
    /// notify service's `max_notifiers` — the N→1 fan-in half. Three sources fan
    /// into one destination; the notify service must accept three notifiers.
    #[test]
    fn destination_fanin_counts_inbound_links() {
        let mut graph = Graph::new();
        let dest_id = add_mock_input_only(&mut graph);
        for _ in 0..3 {
            let src_id = add_mock_output_only(&mut graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        }
        let dest_uid: ProcessorUniqueId = dest_id.as_str().into();
        assert_eq!(destination_fanin(&mut graph, &dest_uid), 3);
    }

    /// A source output port feeding two destinations whose input ports resolve
    /// to CONFLICTING delivery profiles (`ordered` vs `newest`) is genuinely
    /// ambiguous: a channel's single publisher shares one ring config across
    /// every subscriber. `channel_delivery_profile` surfaces this as a named
    /// [`Error::Configuration`], not a silent first-connection-wins pick.
    ///
    /// Revert lock: drop the conflict branch (return the first destination's
    /// profile) and this returns `Ok(_)` — the `expect_err` fails.
    #[test]
    fn conflicting_destination_profile_is_a_configuration_error() {
        use crate::core::descriptors::{
            PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
        };

        // One sink per profile, under distinct import paths: the registry keys
        // on the path, so two sinks sharing one would collide and the second
        // registration — the `newest` half this test needs — would be
        // discarded, leaving both destinations agreeing on `ordered` and no
        // conflict to detect.
        let register_sink = |profile: &str| -> ProcessorClassImportPath {
            let import_path =
                ProcessorClassImportPath::new(format!("{}::ProfileSink_{profile}", module_path!()))
                    .unwrap();
            let mut desc = ProcessorDescriptor::new(
                ProcessorClassShortName::new("ProfileSink").unwrap(),
                import_path.clone(),
                "conflicting-profile sink",
            );
            desc.inputs
                .push(PortDescriptor::iceoryx2("in1", "input").with_delivery_profile(profile));
            // Idempotent: a duplicate path (re-run in the same process) errors;
            // the first registration is the one that stands.
            let _ = PROCESSOR_REGISTRY.register_descriptor_only(desc);
            import_path
        };

        let ordered_ident = register_sink("ordered");
        let newest_ident = register_sink("newest");

        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);
        let ordered_dest = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(ordered_ident, serde_json::Value::Null))
            .first()
            .expect("ordered sink node")
            .id
            .to_string();
        let newest_dest = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(newest_ident, serde_json::Value::Null))
            .first()
            .expect("newest sink node")
            .id
            .to_string();

        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&ordered_dest, "in1"),
        );
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&newest_dest, "in1"),
        );

        let src_uid: ProcessorUniqueId = src_id.as_str().into();
        let err = channel_delivery_profile(&mut graph, &src_uid, "out1")
            .expect_err("conflicting delivery profiles must be a configuration error");
        assert!(
            matches!(err, Error::Configuration(_)),
            "conflicting destination profile must surface as Error::Configuration; got {err:?}",
        );
    }
}
