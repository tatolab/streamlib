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

use crate::core::PortSchemaSpec;
use crate::core::ProcessorUniqueId;
use crate::core::context::RuntimeContext;
use crate::core::embedded_schemas::{
    delivery_profile_for_input_port, expected_payload_bytes_for_port_spec, port_schema_spec,
};
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkState, LinkStateComponent,
    LinkUniqueId, ProcessorInstanceComponent, SubprocessHandleComponent,
};
use crate::core::json_schema::SchemaIdentOutput;
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorInstance};
use crate::iceoryx2::{
    ChannelEgressConfig, ChannelTrustTier, Iceoryx2NotifyService, Iceoryx2Service,
    RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL, SchemaIdentWire, effective_channel_ceiling_bytes,
};

use super::spawn_deno_subprocess_op::DenoSubprocessHostProcessor;
use super::spawn_python_native_subprocess_op::PythonNativeSubprocessHostProcessor;

/// Render a port's structured schema spec as the JSON value embedded in the
/// subprocess wiring envelope: a structured `SchemaIdentOutput` object for
/// `Specific(...)`, or `Value::Null` for `Any` (the wildcard MoQ-style port).
/// Subprocess-side parsers branch on `null` to detect wildcard ports.
fn schema_ident_json(spec: &PortSchemaSpec) -> serde_json::Value {
    SchemaIdentOutput::from_port_spec(spec)
        .map(|s| serde_json::to_value(s).expect("SchemaIdentOutput must serialize cleanly"))
        .unwrap_or(serde_json::Value::Null)
}

/// Resolve a port's structured schema spec into the iceoryx2 wire-routing
/// tag. `Any` ports yield the default zero-segment wire bytes (unset routing
/// tag — preserves the existing wildcard semantics). `Specific(...)` ports
/// build the wire bytes directly from the validated structured fields.
fn schema_ident_wire_for_spec(spec: &PortSchemaSpec) -> SchemaIdentWire {
    match spec {
        PortSchemaSpec::Any => SchemaIdentWire::default(),
        PortSchemaSpec::Specific(ident) => SchemaIdentWire::from_segments(
            ident.org.as_str(),
            ident.package.as_str(),
            ident.r#type.as_str(),
            ident.version.major,
            ident.version.minor,
            ident.version.patch,
        )
        .expect("validated SchemaIdent fits in SchemaIdentWire bounds"),
        // `Named` should never reach this site — runtime startup +
        // proc-macro expansion both resolve bare-name port refs to
        // `Specific(SchemaIdent)` against the enclosing manifest's
        // `schemas:` map (#767). A `Named` here is a runtime bug.
        PortSchemaSpec::Named(name) => panic!(
            "PortSchemaSpec::Named(`{}`) reached iceoryx2 service open — \
             must be resolved before this site",
            name.as_str()
        ),
    }
}

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

    let channel_service_name = channel_service_name(&source_proc_id, &source_port)?;
    let notify_service_name = notify_service_name_for(&dest_proc_id);

    tracing::info!(
        channel = %channel_service_name,
        notify = %notify_service_name,
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

    // Resolve schemas + channel sizing. The channel carries one publisher (the
    // source), so its slot size derives from the source output schema; its
    // subscriber count is the compile-time destination fan-out plus the reserved
    // tap slot. Ring depth, overflow policy, and consumer drain order all derive
    // from the single delivery profile the channel's destinations agree on.
    let output_schema = resolve_output_schema(graph, &source_proc_id, &source_port);
    let dest_schema = resolve_port_schema(
        graph,
        &dest_proc_id,
        &dest_port,
        crate::core::PortDirection::Input,
    );
    let expected_payload = expected_payload_bytes_for_port_spec(&output_schema)?;
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
    let ChannelSizing {
        max_subscribers,
        max_queued_messages,
        enable_safe_overflow,
        drain_order,
    } = resolve_channel_sizing(graph, &source_proc_id, &source_port)?;
    let max_notifiers = destination_fanin(graph, &dest_proc_id);

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(
        &channel_service_name,
        max_subscribers,
        max_queued_messages,
        enable_safe_overflow,
    )?;
    let notify_service =
        iceoryx2_node.open_or_create_notify_service(&notify_service_name, max_notifiers)?;

    // Source side: install the single channel publisher (first link out of this
    // port) and append this link's destination notifier.
    if source_is_subprocess {
        wire_subprocess_source(
            graph,
            &source_proc_id,
            &source_port,
            &channel_service_name,
            &notify_service_name,
            &output_schema,
            expected_payload,
            channel_ceiling_bytes,
            max_queued_messages,
            max_subscribers,
            max_notifiers,
        )?;
    } else {
        let source_processor = get_single_processor(graph, &source_proc_id)?;
        wire_rust_source(
            &source_processor,
            &source_port,
            link_id,
            &output_schema,
            &service,
            &notify_service,
            ChannelEgressConfig {
                service_name: channel_service_name.clone(),
                trust_tier,
                expected_payload_bytes: expected_payload,
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
            &notify_service_name,
            drain_order,
            max_queued_messages,
            max_subscribers,
            max_notifiers,
        )?;
    } else {
        let dest_processor = get_single_processor(graph, &dest_proc_id)?;
        wire_rust_dest(
            &dest_processor,
            &dest_port,
            link_id,
            &dest_schema,
            drain_order,
            max_queued_messages,
            &service,
            &notify_service,
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
/// and dest-side subscriber (plus the destination listener and any orphaned
/// port mailbox / channel publisher) must be dropped so their iceoryx2 services
/// are released. Otherwise a reconnect of a persistent endpoint re-appends past
/// the notify service's create-time `max_notifiers` cap
/// (`ExceedsMaxSupportedNotifiers`) and the stale, shallower-sized data service
/// collides with a deeper-ring reopen (`DoesNotSupportRequestedMinBufferSize`)
/// — the two #1549 errors, one cause.
///
/// Rust→Rust reclaim is complete here (both ports are host-owned). A subprocess
/// endpoint opens its own ports from the wiring envelope and has no host-reachable
/// drop path, so its live reclaim needs a Python/Deno cdylib drop entry point
/// (tracked follow-up); this op does not touch a running subprocess's ports.
#[tracing::instrument(name = "compiler.close_iceoryx2_service", skip(graph), fields(link_id = %link_id))]
pub fn close_iceoryx2_service(graph: &mut Graph, link_id: &LinkUniqueId) -> Result<()> {
    tracing::info!("Closing iceoryx2 service: {}", link_id);

    let Some((source_proc_id, source_port, dest_proc_id)) = graph
        .traversal_mut()
        .e(link_id)
        .first()
        .map(|link| {
            (
                link.from_port().processor_id.clone(),
                link.from_port().port_name.clone(),
                link.to_port().processor_id.clone(),
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
    if !source_is_subprocess {
        match get_single_processor(graph, &source_proc_id) {
            Ok(source_processor) => {
                let source_guard = source_processor.lock();
                if let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() {
                    let channel_released =
                        output_inner.remove_channel_link(&source_port, link_id.as_str());
                    tracing::debug!(
                        source = %source_proc_id,
                        port = %source_port,
                        channel_released,
                        "Reclaimed source-side egress for disconnected link"
                    );
                }
            }
            Err(error) => tracing::warn!(
                proc_id = %source_proc_id,
                error = %error,
                "close_iceoryx2_service: processor missing; port not reclaimed"
            ),
        }
    }

    // Destination side: drop this link's channel subscriber (and the port
    // mailbox / shared listener when their last inbound link went away).
    if !dest_is_subprocess {
        match get_single_processor(graph, &dest_proc_id) {
            Ok(dest_processor) => {
                let dest_guard = dest_processor.lock();
                if let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() {
                    input_inner.remove_channel_link(link_id.as_str());
                    tracing::debug!(
                        dest = %dest_proc_id,
                        "Reclaimed destination-side ports for disconnected link"
                    );
                }
            }
            Err(error) => tracing::warn!(
                proc_id = %dest_proc_id,
                error = %error,
                "close_iceoryx2_service: processor missing; port not reclaimed"
            ),
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
/// channel identity (`streamlib_idents::source_channel_name`). A grammar-illegal
/// port name surfaces as a named [`Error::Configuration`] here rather than an
/// opaque iceoryx2 `Invalid service name` deep in the FFI.
fn channel_service_name(source_proc_id: &ProcessorUniqueId, source_port: &str) -> Result<String> {
    streamlib_idents::source_channel_name(source_proc_id.as_str(), source_port)
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
/// verifies `max_subscribers` / `enable_safe_overflow` on reopen).
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
/// a mismatched `max_subscribers` / `subscriber_max_buffer_size` /
/// `enable_safe_overflow` would be rejected by iceoryx2 on open.
pub(crate) struct ChannelSizing {
    /// Compile-time destination count plus the reserved tap slot.
    pub(crate) max_subscribers: usize,
    /// Ring depth (`subscriber_max_buffer_size`) — the agreed delivery profile's depth.
    pub(crate) max_queued_messages: usize,
    /// Overflow policy — `true` drops-oldest (realtime), `false` back-pressures (lossless).
    pub(crate) enable_safe_overflow: bool,
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
        enable_safe_overflow: delivery.overflow.enable_safe_overflow(),
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
/// derivation is the same `streamlib_idents::source_channel_name` the compiler
/// op keys the service on, so a match here is exact (including the
/// hash-legalized over-budget form).
pub(crate) fn find_channel_source_port(
    graph: &mut Graph,
    channel_service_name: &str,
) -> Option<(ProcessorUniqueId, String)> {
    graph.traversal_mut().e(()).iter().find_map(|link| {
        let source = link.from_port();
        let derived =
            streamlib_idents::source_channel_name(source.processor_id.as_str(), &source.port_name)
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

/// The channel's [`DeliveryProfile`], agreed across every destination the
/// channel feeds.
///
/// A channel's single publisher shares one ring config
/// (depth + `enable_safe_overflow`) across all subscribers, so its
/// destinations must resolve to one delivery profile. A channel whose
/// destinations disagree (`latest` vs `lossless`, say) is genuinely ambiguous —
/// a named [`Error::Configuration`] rather than a silent pick. A channel with a
/// single destination (the common case) uses that destination's profile.
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
            None => crate::iceoryx2::DeliveryProfile::Latest,
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
    Ok(agreed.unwrap_or(crate::iceoryx2::DeliveryProfile::Latest))
}

/// Check if a processor is a subprocess (Python-native, TypeScript, etc.).
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

    let proc_type = graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .map(|n| n.processor_type().clone());

    if let Some(proc_type) = proc_type.as_ref() {
        if let Some(descriptor) = PROCESSOR_REGISTRY.descriptor(proc_type) {
            if matches!(
                descriptor.runtime,
                crate::core::descriptors::ProcessorRuntime::TypeScript
            ) {
                return true;
            }
        }
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
        let mut guard = proc_arc.lock();
        if guard
            .as_any_mut()
            .downcast_mut::<PythonNativeSubprocessHostProcessor>()
            .is_some()
        {
            return true;
        }
    }

    false
}

/// Resolve the wire schema declared on a source processor's output port.
fn resolve_output_schema(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> PortSchemaSpec {
    resolve_port_schema(
        graph,
        source_proc_id,
        source_port,
        crate::core::PortDirection::Output,
    )
}

/// Resolve the [`PortSchemaSpec`] on one port of a graph node, in either
/// direction. Returns [`PortSchemaSpec::Any`] when the node is absent.
fn resolve_port_schema(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
    port: &str,
    direction: crate::core::PortDirection,
) -> PortSchemaSpec {
    let proc_type = graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .map(|node| node.processor_type().clone());

    match proc_type {
        Some(ident) => port_schema_spec(&ident, port, direction),
        None => PortSchemaSpec::Any,
    }
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
fn wire_rust_source(
    source_processor: &Arc<Mutex<ProcessorInstance>>,
    source_port: &str,
    link_id: &LinkUniqueId,
    output_schema: &PortSchemaSpec,
    service: &Iceoryx2Service,
    notify_service: &Iceoryx2NotifyService,
    egress_config: ChannelEgressConfig,
) -> Result<()> {
    let source_guard = source_processor.lock();
    let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() else {
        return Ok(());
    };

    if !output_inner.has_channel_publisher(source_port) {
        let publisher = service.create_publisher(egress_config.expected_payload_bytes)?;
        output_inner.set_channel_publisher(
            source_port,
            schema_ident_wire_for_spec(output_schema),
            publisher,
            egress_config,
        );
        tracing::debug!(
            "Installed channel publisher for source output port '{}'",
            source_port
        );
    }

    let notifier = notify_service.create_notifier()?;
    output_inner.add_channel_notifier(source_port, link_id.as_str(), notifier);
    Ok(())
}

/// Subscribe the Rust destination to the channel bound to its local input port,
/// and ensure its single listener exists.
fn wire_rust_dest(
    dest_processor: &Arc<Mutex<ProcessorInstance>>,
    dest_port: &str,
    link_id: &LinkUniqueId,
    dest_schema: &PortSchemaSpec,
    drain_order: crate::iceoryx2::ReadMode,
    depth: usize,
    service: &Iceoryx2Service,
    notify_service: &Iceoryx2NotifyService,
) -> Result<()> {
    let dest_guard = dest_processor.lock();
    let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() else {
        return Ok(());
    };

    if !input_inner.has_port(dest_port) {
        input_inner.add_port(dest_port, depth, drain_order);
    }
    input_inner.set_port_expected_schema_ident(dest_port, schema_ident_wire_for_spec(dest_schema));

    let subscriber = service.create_subscriber()?;
    input_inner.add_channel_subscriber(dest_port, link_id.as_str(), subscriber);
    tracing::debug!(
        "Bound channel subscriber to destination input port '{}'",
        dest_port
    );

    if !input_inner.has_listener() {
        let listener = notify_service.create_listener()?;
        input_inner.set_listener(listener);
        tracing::debug!("Created listener for destination on its notify service");
    }
    Ok(())
}

/// Record this link's source-side wiring on a subprocess host processor so the
/// subprocess opens its own channel publisher + destination notifier from the
/// envelope. One entry per link — the subprocess installs the single publisher
/// once (keyed by source port) and appends a notifier per entry.
#[allow(clippy::too_many_arguments)]
fn wire_subprocess_source(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
    channel_service_name: &str,
    notify_service_name: &str,
    output_schema: &PortSchemaSpec,
    expected_payload: usize,
    channel_ceiling_bytes: usize,
    max_queued_messages: usize,
    max_subscribers: usize,
    notify_max_notifiers: usize,
) -> Result<()> {
    let entry = serde_json::json!({
        "name": source_port,
        "channel_service_name": channel_service_name,
        "dest_notify_service_name": notify_service_name,
        "schema": schema_ident_json(output_schema),
        "expected_payload_bytes": expected_payload,
        "max_payload_bytes_per_channel": channel_ceiling_bytes,
        "max_queued_messages": max_queued_messages,
        "max_subscribers": max_subscribers,
        "notify_max_notifiers": notify_max_notifiers,
    });

    let source_proc_arc = get_single_processor(graph, source_proc_id)?;
    let mut source_guard = source_proc_arc.lock();
    if let Some(deno_host) = source_guard
        .as_any_mut()
        .downcast_mut::<DenoSubprocessHostProcessor>()
    {
        deno_host.output_port_wiring.push(entry);
    } else if let Some(python_native_host) = source_guard
        .as_any_mut()
        .downcast_mut::<PythonNativeSubprocessHostProcessor>()
    {
        python_native_host.output_port_wiring.push(entry);
    }
    Ok(())
}

/// Record this link's dest-side wiring on a subprocess host processor so the
/// subprocess opens its own channel subscriber (bound to its local input port)
/// from the envelope.
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
) -> Result<()> {
    // The dest reader no longer carries a payload-size hint: the subprocess read
    // buffer starts at the default and grows to the frame it actually receives
    // (PowerOfTwo segment growth on the publisher side, grow-and-retry on read).
    // The drain order is the delivery profile's, resolved host-side; the
    // subprocess maps the string back to its `*_input_set_read_mode` integer.
    let entry = serde_json::json!({
        "name": dest_port,
        "channel_service_name": channel_service_name,
        "notify_service_name": notify_service_name,
        "read_mode": drain_order.as_manifest_str(),
        "max_queued_messages": max_queued_messages,
        "max_subscribers": max_subscribers,
        "notify_max_notifiers": notify_max_notifiers,
    });

    let dest_proc_arc = get_single_processor(graph, dest_proc_id)?;
    let mut dest_guard = dest_proc_arc.lock();
    if let Some(deno_host) = dest_guard
        .as_any_mut()
        .downcast_mut::<DenoSubprocessHostProcessor>()
    {
        deno_host.input_port_wiring.push(entry);
    } else if let Some(python_native_host) = dest_guard
        .as_any_mut()
        .downcast_mut::<PythonNativeSubprocessHostProcessor>()
    {
        python_native_host.input_port_wiring.push(entry);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::SchemaIdent;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::processors::ProcessorSpec;

    /// Look up a registered mock processor's structured ident by its
    /// PascalCase short name.
    fn lookup_registered_ident(short: &str) -> SchemaIdent {
        crate::core::test_support::ensure_test_mocks_registered();
        PROCESSOR_REGISTRY
            .list_registered()
            .into_iter()
            .find(|d| d.name.r#type.as_str() == short)
            .map(|d| d.name)
            .unwrap_or_else(|| {
                panic!(
                    "processor with PascalCase short name `{}` must be in the registry",
                    short
                )
            })
    }

    fn add_mock_output_only(graph: &mut Graph) -> String {
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                lookup_registered_ident("TestMockOutputOnlyProcessor"),
                serde_json::Value::Null,
            ))
            .first()
            .expect("mock_output_only_processor must be in the registry")
            .id
            .to_string()
    }

    fn add_mock_input_only(graph: &mut Graph) -> String {
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                lookup_registered_ident("TestMockInputOnlyProcessor"),
                serde_json::Value::Null,
            ))
            .first()
            .expect("mock_input_only_processor must be in the registry")
            .id
            .to_string()
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

        let channel_name = streamlib_idents::source_channel_name(&src_id, "out1")
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
    /// to CONFLICTING delivery profiles (`lossless` vs `latest`) is genuinely
    /// ambiguous: a channel's single publisher shares one ring config across
    /// every subscriber. `channel_delivery_profile` surfaces this as a named
    /// [`Error::Configuration`], not a silent first-connection-wins pick.
    ///
    /// Revert lock: drop the conflict branch (return the first destination's
    /// profile) and this returns `Ok(_)` — the `expect_err` fails.
    #[test]
    fn conflicting_destination_profile_is_a_configuration_error() {
        use crate::core::descriptors::{PortDescriptor, ProcessorDescriptor};
        use streamlib_idents::{Org, Package, SemVer, TypeName};
        use streamlib_processor_schema::PortSchemaSpec;

        let register_sink = |pkg: &str, profile: &str| -> SchemaIdent {
            let ident = SchemaIdent::new(
                Org::new("tatolab").unwrap(),
                Package::new(pkg).unwrap(),
                TypeName::new("ProfileSink").unwrap(),
                SemVer::new(1, 0, 0),
            );
            let mut desc = ProcessorDescriptor::new(ident.clone(), "conflicting-profile sink");
            desc.inputs.push(
                PortDescriptor::iceoryx2("in1", "input", PortSchemaSpec::Any)
                    .with_delivery_profile(profile),
            );
            // Idempotent: a duplicate ident (re-run in the same process) errors;
            // the first registration is the one that stands.
            let _ = PROCESSOR_REGISTRY.register_descriptor_only(desc);
            ident
        };

        let lossless_ident = register_sink("test-conflicting-profile-lossless", "lossless");
        let latest_ident = register_sink("test-conflicting-profile-latest", "latest");

        let mut graph = Graph::new();
        let src_id = add_mock_output_only(&mut graph);
        let lossless_dest = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(lossless_ident, serde_json::Value::Null))
            .first()
            .expect("lossless sink node")
            .id
            .to_string();
        let latest_dest = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(latest_ident, serde_json::Value::Null))
            .first()
            .expect("latest sink node")
            .id
            .to_string();

        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&lossless_dest, "in1"),
        );
        graph.traversal_mut().add_e(
            OutputLinkPortRef::new(&src_id, "out1"),
            InputLinkPortRef::new(&latest_dest, "in1"),
        );

        let src_uid: ProcessorUniqueId = src_id.as_str().into();
        let err = channel_delivery_profile(&mut graph, &src_uid, "out1")
            .expect_err("conflicting delivery profiles must be a configuration error");
        assert!(
            matches!(err, Error::Configuration(_)),
            "conflicting destination profile must surface as Error::Configuration; got {err:?}",
        );
    }

    /// Wire-time integration lock: a registered output port carrying a
    /// `PortSchemaSpec::Specific(ident)` whose canonical id is NOT in the runtime
    /// schema registry (the "forgot to call `runtime.add_module(...)`" footgun)
    /// surfaces a typed configuration error pointing at `add_module` rather than
    /// silently deferring the failure to first publish.
    #[test]
    fn unregistered_specific_port_schema_surfaces_typed_error_at_wire_time() {
        use crate::core::descriptors::{
            CodeExamples, PortDescriptor, ProcessorDescriptor, ProcessorRuntime,
            ProcessorScheduling,
        };
        use crate::core::embedded_schemas::expected_payload_bytes_for_port_spec;
        use streamlib_idents::{Org, Package, SemVer, TypeName};
        use streamlib_processor_schema::PortSchemaSpec;

        let processor_ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-wire-time-registry-miss").unwrap(),
            TypeName::new("CarryingProcessor").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let unloaded_schema_ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-wire-time-unloaded-schema-pkg").unwrap(),
            TypeName::new("UnloadedWireType").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let unloaded_spec = PortSchemaSpec::Specific(unloaded_schema_ident.clone());

        let descriptor = ProcessorDescriptor {
            name: processor_ident.clone(),
            description: "wire-time registry-miss regression mock".into(),
            version: "1.0.0".into(),
            repository: String::new(),
            runtime: ProcessorRuntime::Rust,
            entrypoint: None,
            config_schema: None,
            scheduling: ProcessorScheduling::default(),
            inputs: Vec::new(),
            outputs: vec![PortDescriptor::iceoryx2(
                "out_unloaded",
                "carries UnloadedWireType",
                unloaded_spec,
            )],
            examples: CodeExamples::default(),
        };
        PROCESSOR_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("register_descriptor_only must accept a fresh ident");

        let (_, outputs) = PROCESSOR_REGISTRY
            .port_info(&processor_ident)
            .expect("port_info must return the descriptor's ports");
        let output_spec = outputs
            .iter()
            .find(|p| p.name == "out_unloaded")
            .map(|p| p.data_type.clone())
            .expect("descriptor advertises `out_unloaded`");

        let err = expected_payload_bytes_for_port_spec(&output_spec)
            .expect_err("registry miss must surface as Err at wire time");
        let msg = err.to_string();
        assert!(
            msg.contains("@tatolab/test-wire-time-unloaded-schema-pkg/UnloadedWireType"),
            "error must name the missing canonical id; got: {msg}"
        );
        assert!(
            msg.contains("add_module"),
            "error must point at `runtime.add_module(...)` as the fix; got: {msg}"
        );
        assert!(
            matches!(err, crate::core::error::Error::Configuration(_)),
            "registry miss at wire time must surface as Error::Configuration; got: {err:?}"
        );
    }
}
