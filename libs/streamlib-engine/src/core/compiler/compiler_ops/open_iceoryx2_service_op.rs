// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 service operations for the compiler.
//!
//! Opens iceoryx2 publish-subscribe services between processor ports.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::PortSchemaSpec;
use crate::core::ProcessorUniqueId;
use crate::core::context::RuntimeContext;
use crate::core::embedded_schemas::{
    max_payload_bytes_for_port_spec, max_queued_messages_for_port_spec, overflow_for_input_port,
};
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkState, LinkStateComponent,
    LinkUniqueId, ProcessorInstanceComponent, SubprocessHandleComponent,
};
use crate::core::json_schema::SchemaIdentOutput;
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorInstance};
use crate::iceoryx2::{MAX_FANIN_PER_DESTINATION, SchemaIdentWire};

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
fn schema_ident_wire_for_producer(spec: &PortSchemaSpec) -> SchemaIdentWire {
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

use super::spawn_deno_subprocess_op::DenoSubprocessHostProcessor;
use super::spawn_python_native_subprocess_op::PythonNativeSubprocessHostProcessor;

/// Resolve the iceoryx2 service-level `enable_safe_overflow` flag from
/// the destination input port's declared overflow policy. Falls back to
/// the engine-wide realtime default (`drop_oldest` →
/// `enable_safe_overflow(true)`) when the destination processor or
/// port can't be located (legitimate when the destination is a
/// subprocess processor whose registry entry doesn't carry per-port
/// overflow yet — those default to drop-oldest, the realtime
/// invariant).
fn resolve_enable_safe_overflow(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    dest_port: &str,
) -> Result<bool> {
    let dest_proc_type = graph
        .traversal_mut()
        .v(dest_proc_id)
        .first()
        .map(|node| node.processor_type().clone());

    let overflow = match dest_proc_type.as_ref() {
        Some(ident) => overflow_for_input_port(ident, dest_port)?,
        None => crate::iceoryx2::Overflow::default(),
    };
    Ok(overflow.enable_safe_overflow())
}

/// Check if a processor is a subprocess (Python, TypeScript, etc.)
fn is_subprocess_processor(graph: &mut Graph, proc_id: &ProcessorUniqueId) -> bool {
    // Check for SubprocessHandleComponent (legacy path)
    let has_component = graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .map(|n| n.has::<SubprocessHandleComponent>())
        .unwrap_or(false);
    if has_component {
        return true;
    }

    // Check if TypeScript or Python-native runtime (FFI manages own iceoryx2)
    let proc_type = graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .map(|n| n.processor_type().clone());

    // Check runtime type from descriptor
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

    // Check if this is a Python native host (by downcasting the processor instance)
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

/// Open an iceoryx2 service for a connection in the graph.
///
/// Handles four cases:
/// - Rust→Rust: Full wiring (publisher + OutputWriter, subscriber + InputMailboxes)
/// - Rust→Python: Only source-side wiring (publisher + OutputWriter). Python creates its own subscriber.
/// - Python→Rust: Only dest-side wiring (subscriber + InputMailboxes). Python creates its own publisher.
/// - Python→Python: Service created but no Rust-side wiring. Both subprocesses manage their own connections.
#[tracing::instrument(name = "compiler.open_iceoryx2_service", skip(graph, runtime_ctx), fields(link_id = %link_id))]
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

    reject_overcap_destination_fanin(graph, &dest_proc_id)?;

    let source_is_subprocess = is_subprocess_processor(graph, &source_proc_id);
    let dest_is_subprocess = is_subprocess_processor(graph, &dest_proc_id);

    tracing::info!(
        "Opening iceoryx2 service: {} ({}:{}) -> ({}:{}) [{}] (source_subprocess={}, dest_subprocess={})",
        from_port,
        source_proc_id,
        source_port,
        dest_proc_id,
        dest_port,
        link_id,
        source_is_subprocess,
        dest_is_subprocess,
    );

    if source_is_subprocess && dest_is_subprocess {
        // Both are subprocesses - just create the service and mark as wired.
        // Both subprocesses handle their own pub/sub connections.
        open_iceoryx2_subprocess_to_subprocess(
            graph,
            &source_proc_id,
            &dest_proc_id,
            &source_port,
            &dest_port,
            link_id,
            runtime_ctx,
        )
    } else if source_is_subprocess {
        // Source is subprocess, dest is Rust - only configure dest side
        let dest_processor = get_single_processor(graph, &dest_proc_id)?;
        open_iceoryx2_subprocess_to_rust(
            graph,
            &dest_processor,
            &source_proc_id,
            &dest_proc_id,
            &source_port,
            &dest_port,
            link_id,
            runtime_ctx,
        )
    } else if dest_is_subprocess {
        // Source is Rust, dest is subprocess - only configure source side
        let source_processor = get_single_processor(graph, &source_proc_id)?;
        open_iceoryx2_rust_to_subprocess(
            graph,
            &source_processor,
            &source_proc_id,
            &dest_proc_id,
            &source_port,
            &dest_port,
            link_id,
            runtime_ctx,
        )
    } else {
        // Both are Rust - full wiring (original path)
        let (source_processor, dest_processor) =
            get_processor_pair(graph, &source_proc_id, &dest_proc_id)?;
        open_iceoryx2_pubsub(
            graph,
            &source_processor,
            &dest_processor,
            &source_proc_id,
            &dest_proc_id,
            &source_port,
            &dest_port,
            link_id,
            runtime_ctx,
        )
    }
}

/// Close an iceoryx2 service by link ID.
#[tracing::instrument(name = "compiler.close_iceoryx2_service", skip(graph), fields(link_id = %link_id))]
pub fn close_iceoryx2_service(graph: &mut Graph, link_id: &LinkUniqueId) -> Result<()> {
    tracing::info!("Closing iceoryx2 service: {}", link_id);

    // Set link state to Disconnected
    if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
        link.insert(LinkStateComponent(LinkState::Disconnected));
    }

    tracing::info!("Closed iceoryx2 service: {} (state: Disconnected)", link_id);
    Ok(())
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Notify (Event) service name paired 1:1 with a destination's pub/sub service.
///
/// The shape mirrors the existing destination-centric pub/sub naming
/// (`streamlib/<dest_proc_id>`) — every upstream Notifier feeding a destination
/// signals the same Listener, giving the destination's runner a single fd to
/// wait on regardless of fan-in. Subprocess SDKs derive this name the same way.
fn notify_service_name_for(dest_proc_id: &ProcessorUniqueId) -> String {
    format!("streamlib/{}/notify", dest_proc_id)
}

/// Reject wiring that would push a destination's fan-in past
/// [`MAX_FANIN_PER_DESTINATION`].
///
/// The new link is already in the graph by the time this runs, so the
/// incoming-edge count IS the post-wiring fan-in. Without this check the
/// (cap+1)th wiring fails inside iceoryx2's `notifier_builder().create()` /
/// `publisher_builder().create()` — opaque, non-actionable, deep inside the
/// FFI. Rejecting here surfaces a configuration error naming the destination.
fn reject_overcap_destination_fanin(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
) -> Result<()> {
    let fanin = graph.traversal_mut().v(dest_proc_id).in_e().iter().count();
    if fanin > MAX_FANIN_PER_DESTINATION {
        return Err(Error::Configuration(format!(
            "destination processor '{}' would have {} upstream sources, \
             exceeding the per-destination iceoryx2 fan-in cap of {} \
             (max_publishers / max_notifiers).",
            dest_proc_id, fanin, MAX_FANIN_PER_DESTINATION,
        )));
    }
    Ok(())
}

/// Resolve the wire schema declared on a source processor's output port.
///
/// Returns the port's `data_type` ([`PortSchemaSpec`]) from
/// [`PROCESSOR_REGISTRY`], or the default ([`PortSchemaSpec::Any`]) when the
/// processor type or named port can't be resolved — the downstream
/// port-spec metadata helpers treat that as "unconstrained" and substitute
/// engine defaults.
fn resolve_output_schema(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> PortSchemaSpec {
    let source_proc_type = graph
        .traversal_mut()
        .v(source_proc_id)
        .first()
        .map(|node| node.processor_type().clone());

    source_proc_type
        .as_ref()
        .and_then(|ident| PROCESSOR_REGISTRY.port_info(ident))
        .and_then(|(_, outputs)| {
            outputs
                .iter()
                .find(|p| p.name == source_port)
                .map(|p| p.data_type.clone())
        })
        .unwrap_or_default()
}

/// Deepest `max_queued_messages` across all of a destination's inbound links.
///
/// The per-destination iceoryx2 pub/sub service (`streamlib/{dest}`) is shared
/// by every inbound link, so its `subscriber_max_buffer_size` must satisfy the
/// DEEPEST consumer. Sizing it from only the link currently being wired lets a
/// later, deeper link's `open_or_create` trip
/// `DoesNotSupportRequestedMinBufferSize` against the already-created service —
/// the order-dependent failure this resolves. iceoryx2 permits opening an
/// existing service with a *smaller* requested buffer, so creating at the max
/// makes every inbound link fit regardless of wiring order. Each publisher
/// still sizes its own shared-memory slot to its own payload
/// ([`max_payload_bytes_for_port_spec`]); only the shared ring depth is unified.
///
/// The link currently being wired is already in the graph (see
/// [`reject_overcap_destination_fanin`]), so the result is always ≥ that link's
/// own declared depth — never a regression below the pre-unification sizing.
fn max_queued_messages_for_dest(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
) -> Result<usize> {
    // Collect (source, port) for every inbound link first to release the
    // traversal borrow before re-traversing per edge to resolve schemas.
    let inbound: Vec<(ProcessorUniqueId, String)> = graph
        .traversal_mut()
        .v(dest_proc_id)
        .in_e()
        .iter()
        .map(|link| {
            (
                link.from_port().processor_id.clone(),
                link.from_port().port_name.clone(),
            )
        })
        .collect();

    let inbound_count = inbound.len();
    let mut max_depth = 0usize;
    for (source_proc_id, source_port) in &inbound {
        let schema = resolve_output_schema(graph, source_proc_id, source_port);
        max_depth = max_depth.max(max_queued_messages_for_port_spec(&schema)?);
    }

    tracing::debug!(
        dest = %dest_proc_id,
        max_queued_messages = max_depth,
        inbound_links = inbound_count,
        "sized shared per-destination iceoryx2 service to its deepest inbound link",
    );
    Ok(max_depth)
}

fn get_processor_pair(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    dest_proc_id: &ProcessorUniqueId,
) -> Result<(Arc<Mutex<ProcessorInstance>>, Arc<Mutex<ProcessorInstance>>)> {
    let source_arc = get_single_processor(graph, source_proc_id)?;
    let dest_arc = get_single_processor(graph, dest_proc_id)?;
    Ok((source_arc, dest_arc))
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

#[allow(clippy::too_many_arguments)]
fn open_iceoryx2_pubsub(
    graph: &mut Graph,
    source_processor: &Arc<Mutex<ProcessorInstance>>,
    dest_processor: &Arc<Mutex<ProcessorInstance>>,
    source_proc_id: &ProcessorUniqueId,
    dest_proc_id: &ProcessorUniqueId,
    source_port: &str,
    dest_port: &str,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
) -> Result<()> {
    // Service name is destination-centric: all upstream processors publish to the same service
    // This allows multiple inputs to a single processor to share one subscriber
    let service_name = format!("streamlib/{}", dest_proc_id);
    let notify_service_name = notify_service_name_for(dest_proc_id);

    tracing::debug!(
        "Opening iceoryx2 service '{}' for connection {} -> {}",
        service_name,
        source_proc_id,
        dest_proc_id
    );

    // Look up schema for the output port before creating the publisher so we can size
    // the shared memory slot correctly via max_payload_bytes_for_port_spec.
    let output_schema = resolve_output_schema(graph, source_proc_id, source_port);

    tracing::debug!(
        "Output port '{}' has schema '{}'",
        source_port,
        output_schema
    );

    // Create iceoryx2 Service (pub/sub) and paired Notify service (event/fd-wake).
    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let max_queued_messages = max_queued_messages_for_dest(graph, dest_proc_id)?;
    let max_payload = max_payload_bytes_for_port_spec(&output_schema)?;
    let enable_safe_overflow = resolve_enable_safe_overflow(graph, dest_proc_id, dest_port)?;
    let service = iceoryx2_node.open_or_create_service(
        &service_name,
        max_queued_messages,
        enable_safe_overflow,
    )?;
    let notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    // Create Publisher sized for this schema's declared max payload.
    let publisher = service.create_publisher(max_payload)?;
    let notifier = notify_service.create_notifier()?;
    tracing::debug!(
        "Created iceoryx2 Publisher+Notifier for '{}' -> service '{}' (enable_safe_overflow={})",
        source_proc_id,
        service_name,
        enable_safe_overflow
    );

    // Configure source OutputWriterInner with port mapping,
    // publisher, and notifier (issue #894 — host operates on the
    // inner Arc directly, no FFI hop).
    {
        let source_guard = source_processor.lock();
        if let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() {
            output_inner.add_connection(
                source_port,
                schema_ident_wire_for_producer(&output_schema),
                dest_port,
                publisher,
                notifier,
            );
            tracing::debug!(
                "Configured OutputWriter port '{}' -> '{}' with Publisher+Notifier",
                source_port,
                dest_port
            );
        }
    }

    // Configure destination InputMailboxesInner with port
    // (issue #894 — host operates on the inner Arc directly).
    // Only create subscriber if destination doesn't already have one
    // (first connection wins).
    {
        let dest_guard = dest_processor.lock();
        if let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() {
            // Only add the port if the macro-generated code didn't already
            // configure it. The macro reads schema metadata (read_mode,
            // buffer_size) and sets the correct values per port type.
            // Overwriting here would discard the schema-driven settings.
            if !input_inner.has_port(dest_port) {
                input_inner.add_port(dest_port, 1, Default::default());
            }

            // Plumb the schema's `metadata.max_payload_bytes` to the
            // destination port so the cdylib's v2
            // `max_payload_for_port` vtable slot returns the same
            // bound the publisher uses to size the iceoryx2 slot.
            // The cdylib's read_raw then allocates exactly this
            // size — no truncation, no retry, no silent drop.
            input_inner.set_port_max_payload_bytes(dest_port, max_payload);

            // Only set subscriber+listener if this is the first connection to this destination
            // All subsequent connections reuse the same pair (max_listeners=1 enforces this).
            if !input_inner.has_subscriber() {
                let subscriber = service.create_subscriber()?;
                input_inner.set_subscriber(subscriber);
                tracing::debug!(
                    "Created iceoryx2 Subscriber for '{}' on service '{}'",
                    dest_proc_id,
                    service_name
                );
            } else {
                tracing::debug!(
                    "Reusing existing Subscriber for '{}' (adding port '{}')",
                    dest_proc_id,
                    dest_port
                );
            }
            if !input_inner.has_listener() {
                let listener = notify_service.create_listener()?;
                input_inner.set_listener(listener);
                tracing::debug!(
                    "Created iceoryx2 Listener for '{}' on notify service '{}'",
                    dest_proc_id,
                    notify_service_name
                );
            }
        }
    }

    // Set link state to Wired
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!(
        "Opened iceoryx2 service: {} [{}] (state: Wired)",
        service_name,
        link_id
    );
    Ok(())
}

/// Both source and dest are subprocesses - create the iceoryx2 service but no Rust-side wiring.
#[allow(clippy::too_many_arguments)]
fn open_iceoryx2_subprocess_to_subprocess(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    dest_proc_id: &ProcessorUniqueId,
    source_port: &str,
    dest_port: &str,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
) -> Result<()> {
    let service_name = format!("streamlib/{}", dest_proc_id);
    let notify_service_name = notify_service_name_for(dest_proc_id);

    tracing::debug!(
        "Opening iceoryx2 service '{}' for subprocess-to-subprocess connection",
        service_name
    );

    let output_schema = resolve_output_schema(graph, source_proc_id, source_port);
    let max_payload = max_payload_bytes_for_port_spec(&output_schema)?;
    let max_queued_messages = max_queued_messages_for_dest(graph, dest_proc_id)?;
    let enable_safe_overflow = resolve_enable_safe_overflow(graph, dest_proc_id, dest_port)?;

    // Ensure both services exist (both subprocesses will open them independently).
    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let _service = iceoryx2_node.open_or_create_service(
        &service_name,
        max_queued_messages,
        enable_safe_overflow,
    )?;
    let _notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    // Store output wiring info on the source subprocess
    {
        let source_proc_arc = get_single_processor(graph, source_proc_id)?;
        let mut source_guard = source_proc_arc.lock();
        if let Some(deno_host) = source_guard
            .as_any_mut()
            .downcast_mut::<DenoSubprocessHostProcessor>()
        {
            deno_host.output_port_wiring.push(serde_json::json!({
                "name": source_port,
                "dest_port": dest_port,
                "dest_service_name": service_name,
                "dest_notify_service_name": notify_service_name,
                "schema": schema_ident_json(&output_schema),
                "max_payload_bytes": max_payload,
                "max_queued_messages": max_queued_messages,
            }));
        } else if let Some(python_native_host) = source_guard
            .as_any_mut()
            .downcast_mut::<PythonNativeSubprocessHostProcessor>(
        ) {
            python_native_host
                .output_port_wiring
                .push(serde_json::json!({
                    "name": source_port,
                    "dest_port": dest_port,
                    "dest_service_name": service_name,
                    "dest_notify_service_name": notify_service_name,
                    "schema": schema_ident_json(&output_schema),
                    "max_payload_bytes": max_payload,
                    "max_queued_messages": max_queued_messages,
                }));
        }
    }

    // Store input wiring info on the dest subprocess
    {
        let dest_proc_arc = get_single_processor(graph, dest_proc_id)?;
        let mut dest_guard = dest_proc_arc.lock();
        if let Some(deno_host) = dest_guard
            .as_any_mut()
            .downcast_mut::<DenoSubprocessHostProcessor>()
        {
            deno_host.input_port_wiring.push(serde_json::json!({
                "name": dest_port,
                "service_name": service_name,
                "notify_service_name": notify_service_name,
                "read_mode": "skip_to_latest",
                "max_payload_bytes": max_payload,
                "max_queued_messages": max_queued_messages,
            }));
        } else if let Some(python_native_host) = dest_guard
            .as_any_mut()
            .downcast_mut::<PythonNativeSubprocessHostProcessor>(
        ) {
            python_native_host
                .input_port_wiring
                .push(serde_json::json!({
                    "name": dest_port,
                    "service_name": service_name,
                    "notify_service_name": notify_service_name,
                    "read_mode": "skip_to_latest",
                    "max_payload_bytes": max_payload,
                    "max_queued_messages": max_queued_messages,
                }));
        }
    }

    // Set link state to Wired
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!(
        "Opened iceoryx2 service: {} [{}] (subprocess-to-subprocess, state: Wired)",
        service_name,
        link_id
    );
    Ok(())
}

/// Source is subprocess, dest is Rust - only configure dest side (subscriber + InputMailboxes).
#[allow(clippy::too_many_arguments)]
fn open_iceoryx2_subprocess_to_rust(
    graph: &mut Graph,
    dest_processor: &Arc<Mutex<ProcessorInstance>>,
    source_proc_id: &ProcessorUniqueId,
    dest_proc_id: &ProcessorUniqueId,
    source_port: &str,
    dest_port: &str,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
) -> Result<()> {
    let service_name = format!("streamlib/{}", dest_proc_id);
    let notify_service_name = notify_service_name_for(dest_proc_id);

    tracing::debug!(
        "Opening iceoryx2 service '{}' for subprocess({}) -> rust({}) connection",
        service_name,
        source_proc_id,
        dest_proc_id
    );

    // Look up schema for the output port from the registry
    let output_schema = resolve_output_schema(graph, source_proc_id, source_port);
    let max_payload = max_payload_bytes_for_port_spec(&output_schema)?;
    let max_queued_messages = max_queued_messages_for_dest(graph, dest_proc_id)?;
    let enable_safe_overflow = resolve_enable_safe_overflow(graph, dest_proc_id, dest_port)?;

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(
        &service_name,
        max_queued_messages,
        enable_safe_overflow,
    )?;
    let notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    // Source is subprocess - it creates its own publisher and notifier via FFI.
    // Store output wiring info on the subprocess processor so it can publish via FFI.
    {
        let source_proc_arc = get_single_processor(graph, source_proc_id)?;
        let mut source_guard = source_proc_arc.lock();
        if let Some(deno_host) = source_guard
            .as_any_mut()
            .downcast_mut::<DenoSubprocessHostProcessor>()
        {
            deno_host.output_port_wiring.push(serde_json::json!({
                "name": source_port,
                "dest_port": dest_port,
                "dest_service_name": service_name,
                "dest_notify_service_name": notify_service_name,
                "schema": schema_ident_json(&output_schema),
                "max_payload_bytes": max_payload,
                "max_queued_messages": max_queued_messages,
            }));
            tracing::debug!(
                "Stored output wiring on Deno processor '{}': port='{}', dest_port='{}', dest_service='{}', schema='{}'",
                source_proc_id,
                source_port,
                dest_port,
                service_name,
                output_schema
            );
        } else if let Some(python_native_host) = source_guard
            .as_any_mut()
            .downcast_mut::<PythonNativeSubprocessHostProcessor>(
        ) {
            python_native_host
                .output_port_wiring
                .push(serde_json::json!({
                    "name": source_port,
                    "dest_port": dest_port,
                    "dest_service_name": service_name,
                    "dest_notify_service_name": notify_service_name,
                    "schema": schema_ident_json(&output_schema),
                    "max_payload_bytes": max_payload,
                    "max_queued_messages": max_queued_messages,
                }));
            tracing::debug!(
                "Stored output wiring on Python native processor '{}': port='{}', dest_port='{}', dest_service='{}', schema='{}'",
                source_proc_id,
                source_port,
                dest_port,
                service_name,
                output_schema
            );
        }
    }

    // Configure destination InputMailboxesInner with port (Rust
    // side; issue #894 — host operates on the inner Arc directly).
    {
        let dest_guard = dest_processor.lock();
        if let Some(input_inner) = dest_guard.iceoryx2_input_mailboxes_inner() {
            if !input_inner.has_port(dest_port) {
                input_inner.add_port(dest_port, 1, Default::default());
            }

            // Plumb the schema's `metadata.max_payload_bytes` to the
            // destination port so the cdylib's v2
            // `max_payload_for_port` vtable slot honors the same
            // bound the publisher uses. See the same call in the
            // local-source branch above for full rationale.
            input_inner.set_port_max_payload_bytes(dest_port, max_payload);

            if !input_inner.has_subscriber() {
                let subscriber = service.create_subscriber()?;
                input_inner.set_subscriber(subscriber);
                tracing::debug!(
                    "Created iceoryx2 Subscriber for '{}' on service '{}' (source is subprocess)",
                    dest_proc_id,
                    service_name
                );
            }
            if !input_inner.has_listener() {
                let listener = notify_service.create_listener()?;
                input_inner.set_listener(listener);
                tracing::debug!(
                    "Created iceoryx2 Listener for '{}' on notify service '{}' (source is subprocess)",
                    dest_proc_id,
                    notify_service_name
                );
            }
        }
    }

    // Set link state to Wired
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!(
        "Opened iceoryx2 service: {} [{}] (subprocess-to-rust, state: Wired)",
        service_name,
        link_id
    );
    Ok(())
}

/// Source is Rust, dest is subprocess - only configure source side (publisher + OutputWriter).
#[allow(clippy::too_many_arguments)]
fn open_iceoryx2_rust_to_subprocess(
    graph: &mut Graph,
    source_processor: &Arc<Mutex<ProcessorInstance>>,
    source_proc_id: &ProcessorUniqueId,
    dest_proc_id: &ProcessorUniqueId,
    source_port: &str,
    dest_port: &str,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
) -> Result<()> {
    let service_name = format!("streamlib/{}", dest_proc_id);
    let notify_service_name = notify_service_name_for(dest_proc_id);

    tracing::debug!(
        "Opening iceoryx2 service '{}' for rust({}) -> subprocess({}) connection",
        service_name,
        source_proc_id,
        dest_proc_id
    );

    // Look up schema before creating the publisher to size the slot correctly.
    let output_schema = resolve_output_schema(graph, source_proc_id, source_port);

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let max_payload = max_payload_bytes_for_port_spec(&output_schema)?;
    let max_queued_messages = max_queued_messages_for_dest(graph, dest_proc_id)?;
    let enable_safe_overflow = resolve_enable_safe_overflow(graph, dest_proc_id, dest_port)?;
    let service = iceoryx2_node.open_or_create_service(
        &service_name,
        max_queued_messages,
        enable_safe_overflow,
    )?;
    let notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    // Create Publisher sized for this schema's declared max payload.
    let publisher = service.create_publisher(max_payload)?;
    let notifier = notify_service.create_notifier()?;

    // Configure source OutputWriterInner with port mapping,
    // publisher, and notifier (issue #894 — host operates on the
    // inner Arc directly).
    {
        let source_guard = source_processor.lock();
        if let Some(output_inner) = source_guard.iceoryx2_output_writer_inner() {
            output_inner.add_connection(
                source_port,
                schema_ident_wire_for_producer(&output_schema),
                dest_port,
                publisher,
                notifier,
            );
            tracing::debug!(
                "Configured OutputWriter port '{}' -> '{}' with Publisher+Notifier (dest is subprocess)",
                source_port,
                dest_port
            );
        }
    }

    // Dest is subprocess - it creates its own subscriber+listener. No Rust-side dest wiring.
    // Store input wiring info on the subprocess processor so it can subscribe via FFI.
    {
        let dest_proc_arc = get_single_processor(graph, dest_proc_id)?;
        let mut dest_guard = dest_proc_arc.lock();
        if let Some(deno_host) = dest_guard
            .as_any_mut()
            .downcast_mut::<DenoSubprocessHostProcessor>()
        {
            deno_host.input_port_wiring.push(serde_json::json!({
                "name": dest_port,
                "service_name": service_name,
                "notify_service_name": notify_service_name,
                "read_mode": "skip_to_latest",
                "max_payload_bytes": max_payload,
                "max_queued_messages": max_queued_messages,
            }));
            tracing::debug!(
                "Stored input wiring on Deno processor '{}': port='{}', service='{}'",
                dest_proc_id,
                dest_port,
                service_name
            );
        } else if let Some(python_native_host) = dest_guard
            .as_any_mut()
            .downcast_mut::<PythonNativeSubprocessHostProcessor>(
        ) {
            python_native_host
                .input_port_wiring
                .push(serde_json::json!({
                    "name": dest_port,
                    "service_name": service_name,
                    "notify_service_name": notify_service_name,
                    "read_mode": "skip_to_latest",
                    "max_payload_bytes": max_payload,
                    "max_queued_messages": max_queued_messages,
                }));
            tracing::debug!(
                "Stored input wiring on Python native processor '{}': port='{}', service='{}'",
                dest_proc_id,
                dest_port,
                service_name
            );
        }
    }

    // Set link state to Wired
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!(
        "Opened iceoryx2 service: {} [{}] (rust-to-subprocess, state: Wired)",
        service_name,
        link_id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::SchemaIdent;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::processors::ProcessorSpec;

    /// Look up a registered mock processor's structured ident by its
    /// PascalCase short name. The mock processors live in
    /// [`crate::core::test_support`] and are registered explicitly via
    /// `ensure_test_mocks_registered()`; their full ident is composed
    /// from the engine `streamlib.yaml`'s `package:` block, so reading
    /// the version off the registry rather than hardcoding it keeps
    /// these tests robust to package-version bumps.
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

    #[test]
    fn rejects_destination_with_overcap_fanin() {
        let mut graph = Graph::new();
        let dest_id = add_mock_input_only(&mut graph);

        // Wire MAX_FANIN_PER_DESTINATION + 1 distinct upstream sources into the
        // same destination port. petgraph permits parallel edges, so all share
        // the destination's `in1`.
        for _ in 0..=MAX_FANIN_PER_DESTINATION {
            let src_id = add_mock_output_only(&mut graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        }

        let dest_uid: ProcessorUniqueId = dest_id.as_str().into();
        let err = reject_overcap_destination_fanin(&mut graph, &dest_uid)
            .expect_err("fan-in cap+1 must be rejected");

        let msg = err.to_string();
        assert!(
            msg.contains(dest_id.as_str()),
            "error must name the destination ('{dest_id}'); got: {msg}"
        );
        assert!(
            msg.contains(&MAX_FANIN_PER_DESTINATION.to_string()),
            "error must name the cap ('{MAX_FANIN_PER_DESTINATION}'); got: {msg}"
        );
    }

    #[test]
    fn accepts_destination_at_fanin_cap() {
        let mut graph = Graph::new();
        let dest_id = add_mock_input_only(&mut graph);

        for _ in 0..MAX_FANIN_PER_DESTINATION {
            let src_id = add_mock_output_only(&mut graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        }

        let dest_uid: ProcessorUniqueId = dest_id.as_str().into();
        reject_overcap_destination_fanin(&mut graph, &dest_uid)
            .expect("fan-in == cap must succeed");
    }

    /// `max_queued_messages_for_dest` walks ALL of a destination's inbound
    /// links (not just the link currently being wired) and returns the
    /// deepest declared depth, so the shared per-destination service is
    /// sized to satisfy every inbound link regardless of wiring order. The
    /// engine's schema-free mocks declare `out1: any`, so every inbound link
    /// resolves to the default depth — this locks the multi-edge
    /// enumeration, the re-traversal borrow-collect, and the
    /// resolve-to-default contract (a regression to 0, a panic on the
    /// re-traversal borrow, or skipping edges all fail here). The
    /// mismatched-depth discrimination — where `.max()` and `.min()`
    /// actually differ — is locked by the sibling
    /// [`max_queued_messages_for_dest_sizes_to_deepest_inbound_link`], which
    /// fans two distinct declared depths into one destination.
    #[test]
    fn max_queued_messages_for_dest_spans_all_inbound_links() {
        let mut graph = Graph::new();
        let dest_id = add_mock_input_only(&mut graph);

        // Three distinct upstream sources fan into the same destination port.
        for _ in 0..3 {
            let src_id = add_mock_output_only(&mut graph);
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out1"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        }

        let dest_uid: ProcessorUniqueId = dest_id.as_str().into();
        let depth = max_queued_messages_for_dest(&mut graph, &dest_uid)
            .expect("sizing the shared service across inbound links must succeed");
        assert_eq!(
            depth,
            crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES,
            "schema-free `any` mock sources resolve to the default depth; the \
             helper must return it across all inbound links, never 0 or a panic",
        );
    }

    /// Wire-time integration lock: when a processor's registered output
    /// port carries a `PortSchemaSpec::Specific(ident)` whose canonical
    /// id is NOT in the runtime schema registry (the "forgot to call
    /// `runtime.add_module(...)`" footgun), the helper chain
    /// `port_info → data_type → max_payload_bytes_for_port_spec`
    /// surfaces a typed configuration error pointing at `add_module`
    /// rather than silently falling back to the iceoryx2 default and
    /// deferring the failure to first publish.
    ///
    /// Locks the registry-miss-vs-add-module boundary at the same
    /// shape `open_iceoryx2_pubsub` exercises: descriptor declares port
    /// schema → `PROCESSOR_REGISTRY.port_info(...)` reads it → the
    /// resolver gates allocation on registry membership. The compiler-
    /// enforced `?` operator at every helper call site in this module
    /// guarantees the error propagates out of `open_iceoryx2_service`,
    /// out of `compile_phase`, out of `Runner::start()`. Reverting the
    /// helper signature back to infallible `usize` would fail compilation
    /// of every call site immediately.
    #[test]
    fn unregistered_specific_port_schema_surfaces_typed_error_at_wire_time() {
        use crate::core::descriptors::{
            CodeExamples, PortDescriptor, ProcessorDescriptor, ProcessorRuntime,
            ProcessorScheduling,
        };
        use crate::core::embedded_schemas::max_payload_bytes_for_port_spec;
        use streamlib_idents::{Org, Package, SemVer, TypeName};
        use streamlib_processor_schema::PortSchemaSpec;

        // Mint a processor identity (the carrying processor) that's
        // unique to this test so the registry can hold it across runs.
        let processor_ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-wire-time-registry-miss").unwrap(),
            TypeName::new("CarryingProcessor").unwrap(),
            SemVer::new(1, 0, 0),
        );
        // Mint a wire schema identity whose package was NEVER loaded
        // via `runtime.add_module(...)`. The processor declares an
        // output port carrying this schema.
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

        // Drive the exact lookup chain the open_iceoryx2_pubsub /
        // open_iceoryx2_rust_to_subprocess / open_iceoryx2_subprocess_to_*
        // helpers run when wiring a service.
        let (_, outputs) = PROCESSOR_REGISTRY
            .port_info(&processor_ident)
            .expect("port_info must return the descriptor's ports");
        let output_spec = outputs
            .iter()
            .find(|p| p.name == "out_unloaded")
            .map(|p| p.data_type.clone())
            .expect("descriptor advertises `out_unloaded`");

        let err = max_payload_bytes_for_port_spec(&output_spec)
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

    /// The load-bearing half of the iceoryx2 sizing fix: when a destination
    /// has inbound links of DIFFERENT declared depths, the shared
    /// per-destination service is sized to the DEEPEST one. Two sources fan
    /// into one destination at depths 4 and 64 (both ≠ the engine default of
    /// 16); the helper must return 64.
    ///
    /// This locks the order-dependent `DoesNotSupportRequestedMinBufferSize`
    /// crash directly: iceoryx2 rejects opening an existing service with a
    /// LARGER subscriber buffer than it was created with, so creating the
    /// shared service from the shallow link (4) then reopening it for the
    /// deep link (64) fails. Sizing to the max up front avoids it.
    ///
    /// Mentally revert `.max()` → `.min()` (or "use only one link's depth")
    /// and this returns 4, failing. The schema-free `any` sibling test
    /// cannot catch that — every `any` source resolves to the same default,
    /// so min and max coincide. This test deliberately gives the two inbound
    /// links DIFFERENT depths so only `.max()` produces 64.
    #[test]
    fn max_queued_messages_for_dest_sizes_to_deepest_inbound_link() {
        use crate::core::descriptors::{
            CodeExamples, PortDescriptor, ProcessorDescriptor, ProcessorRuntime,
            ProcessorScheduling,
        };
        use crate::core::embedded_schemas::register_schema;
        use streamlib_idents::{Org, Package, SemVer, TypeName};
        use streamlib_processor_schema::PortSchemaSpec;

        // Two wire schemas with distinct, non-default ring depths.
        register_schema(
            "@test/qdepth-shallow/ShallowFrame",
            "metadata:\n  type: ShallowFrame\n  max_queued_messages: 4\n",
        );
        register_schema(
            "@test/qdepth-deep/DeepFrame",
            "metadata:\n  type: DeepFrame\n  max_queued_messages: 64\n",
        );
        let shallow_schema = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("test").unwrap(),
            Package::new("qdepth-shallow").unwrap(),
            TypeName::new("ShallowFrame").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        let deep_schema = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("test").unwrap(),
            Package::new("qdepth-deep").unwrap(),
            TypeName::new("DeepFrame").unwrap(),
            SemVer::new(1, 0, 0),
        ));

        // Register an output-only source whose `out` port carries `carries`,
        // then return its processor ident. A closure (not a nested `fn`) so
        // the test's `use` imports are in scope.
        let register_source =
            |type_name: &str, pkg: &str, carries: PortSchemaSpec| -> SchemaIdent {
                let ident = SchemaIdent::new(
                    Org::new("tatolab").unwrap(),
                    Package::new(pkg).unwrap(),
                    TypeName::new(type_name).unwrap(),
                    SemVer::new(1, 0, 0),
                );
                let descriptor = ProcessorDescriptor {
                    name: ident.clone(),
                    description: "qdepth source mock".into(),
                    version: "1.0.0".into(),
                    repository: String::new(),
                    runtime: ProcessorRuntime::Rust,
                    entrypoint: None,
                    config_schema: None,
                    scheduling: ProcessorScheduling::default(),
                    inputs: Vec::new(),
                    outputs: vec![PortDescriptor::iceoryx2(
                        "out",
                        "carries a depth-tagged frame",
                        carries,
                    )],
                    examples: CodeExamples::default(),
                };
                PROCESSOR_REGISTRY
                    .register_descriptor_only(descriptor)
                    .expect("register_descriptor_only accepts a fresh ident");
                ident
            };

        let shallow_src = register_source(
            "QDepthShallowSource",
            "test-qdepth-shallow-src",
            shallow_schema,
        );
        let deep_src = register_source("QDepthDeepSource", "test-qdepth-deep-src", deep_schema);

        let mut graph = Graph::new();
        let dest_id = add_mock_input_only(&mut graph);

        // Wire the SHALLOW source first, the DEEP source second: a naive
        // "first inbound link" regression would pick 4, "last" would pick 64,
        // and only the correct `.max()` over all inbound links is robust to
        // ordering while returning 64.
        for ident in [&shallow_src, &deep_src] {
            let src_id = graph
                .traversal_mut()
                .add_v(ProcessorSpec::new(ident.clone(), serde_json::Value::Null))
                .first()
                .expect("descriptor-only source registers as a graph vertex")
                .id
                .to_string();
            graph.traversal_mut().add_e(
                OutputLinkPortRef::new(&src_id, "out"),
                InputLinkPortRef::new(&dest_id, "in1"),
            );
        }

        let dest_uid: ProcessorUniqueId = dest_id.as_str().into();
        let depth = max_queued_messages_for_dest(&mut graph, &dest_uid)
            .expect("sizing across mismatched-depth inbound links must succeed");
        assert_eq!(
            depth, 64,
            "shared per-destination service must size to the DEEPEST inbound link \
             (deep=64, shallow=4, default=16); got {depth}. Reverting `.max()` to \
             `.min()` yields 4 — the order-dependent \
             DoesNotSupportRequestedMinBufferSize regression this locks.",
        );
    }
}
