// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 service operations for the compiler.
//!
//! Opens iceoryx2 publish-subscribe services between processor ports.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::context::RuntimeContext;
use crate::core::embedded_schemas::max_payload_bytes_for_schema;
use crate::core::error::{Result, StreamError};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkState, LinkStateComponent,
    LinkUniqueId, ProcessorInstanceComponent, SubprocessHandleComponent,
};
use crate::core::processors::{ProcessorInstance, PROCESSOR_REGISTRY};
use crate::core::ProcessorUniqueId;

use super::spawn_deno_subprocess_op::DenoSubprocessHostProcessor;
use super::spawn_python_native_subprocess_op::PythonNativeSubprocessHostProcessor;

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
        .map(|n| n.processor_type().to_string())
        .unwrap_or_default();

    // Check runtime type from descriptor
    if let Some(descriptor) = PROCESSOR_REGISTRY.descriptor(&proc_type) {
        if matches!(
            descriptor.runtime,
            crate::core::descriptors::ProcessorRuntime::TypeScript
        ) {
            return true;
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
        let link = graph.traversal_mut().e(link_id).first().ok_or_else(|| {
            StreamError::LinkNotFound(format!("Link '{}' not found in graph", link_id))
        })?;
        (link.from_port().clone(), link.to_port().clone())
    };

    let (source_proc_id, source_port) =
        (from_port.processor_id.clone(), from_port.port_name.clone());
    let (dest_proc_id, dest_port) = (to_port.processor_id.clone(), to_port.port_name.clone());

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
        .ok_or_else(|| StreamError::Configuration(format!("Processor '{}' not found", proc_id)))
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
    // the shared memory slot correctly via max_payload_bytes_for_schema.
    let output_schema = {
        let source_proc_type = graph
            .traversal_mut()
            .v(source_proc_id)
            .first()
            .map(|node| node.processor_type().to_string())
            .unwrap_or_default();

        PROCESSOR_REGISTRY
            .port_info(&source_proc_type)
            .and_then(|(_, outputs)| {
                outputs
                    .iter()
                    .find(|p| p.name == source_port)
                    .map(|p| p.data_type.clone())
            })
            .unwrap_or_default()
    };

    tracing::debug!(
        "Output port '{}' has schema '{}'",
        source_port,
        output_schema
    );

    // Create iceoryx2 Service (pub/sub) and paired Notify service (event/fd-wake).
    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(&service_name)?;
    let notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    // Create Publisher sized for this schema's declared max payload.
    let publisher = service.create_publisher(max_payload_bytes_for_schema(&output_schema))?;
    let notifier = notify_service.create_notifier()?;
    tracing::debug!(
        "Created iceoryx2 Publisher+Notifier for '{}' -> service '{}'",
        source_proc_id,
        service_name
    );

    // Configure source OutputWriter with port mapping, publisher, and notifier.
    {
        let source_guard = source_processor.lock();
        if let Some(output_writer) = source_guard.get_iceoryx2_output_writer() {
            output_writer.add_connection(
                source_port,
                &output_schema,
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

    // Configure destination InputMailboxes with port
    // Only create subscriber if destination doesn't already have one (first connection wins)
    {
        let mut dest_guard = dest_processor.lock();
        if let Some(input_mailboxes) = dest_guard.get_iceoryx2_input_mailboxes() {
            // Only add the port if the macro-generated code didn't already
            // configure it. The macro reads schema metadata (read_mode,
            // buffer_size) and sets the correct values per port type.
            // Overwriting here would discard the schema-driven settings.
            if !input_mailboxes.has_port(dest_port) {
                input_mailboxes.add_port(dest_port, 1, Default::default());
            }

            // Only set subscriber+listener if this is the first connection to this destination
            // All subsequent connections reuse the same pair (max_listeners=1 enforces this).
            if !input_mailboxes.has_subscriber() {
                let subscriber = service.create_subscriber()?;
                input_mailboxes.set_subscriber(subscriber);
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
            if !input_mailboxes.has_listener() {
                let listener = notify_service.create_listener()?;
                input_mailboxes.set_listener(listener);
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
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
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

    // Ensure both services exist (both subprocesses will open them independently).
    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let _service = iceoryx2_node.open_or_create_service(&service_name)?;
    let _notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    let output_schema = {
        let source_proc_type = graph
            .traversal_mut()
            .v(source_proc_id)
            .first()
            .map(|node| node.processor_type().to_string())
            .unwrap_or_default();

        PROCESSOR_REGISTRY
            .port_info(&source_proc_type)
            .and_then(|(_, outputs)| {
                outputs
                    .iter()
                    .find(|p| p.name == source_port)
                    .map(|p| p.data_type.clone())
            })
            .unwrap_or_default()
    };
    let max_payload = max_payload_bytes_for_schema(&output_schema);

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
                "schema_name": output_schema,
                "max_payload_bytes": max_payload,
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
                    "schema_name": output_schema,
                    "max_payload_bytes": max_payload,
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
                }));
        }
    }

    // Set link state to Wired
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
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

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(&service_name)?;
    let notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    // Source is subprocess - it creates its own publisher and notifier via FFI.
    // Store output wiring info on the subprocess processor so it can publish via FFI.
    {
        // Look up schema for the output port from the registry
        let output_schema = {
            let source_proc_type = graph
                .traversal_mut()
                .v(source_proc_id)
                .first()
                .map(|node| node.processor_type().to_string())
                .unwrap_or_default();

            PROCESSOR_REGISTRY
                .port_info(&source_proc_type)
                .and_then(|(_, outputs)| {
                    outputs
                        .iter()
                        .find(|p| p.name == source_port)
                        .map(|p| p.data_type.clone())
                })
                .unwrap_or_default()
        };

        let max_payload = max_payload_bytes_for_schema(&output_schema);
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
                "schema_name": output_schema,
                "max_payload_bytes": max_payload,
            }));
            tracing::debug!(
                "Stored output wiring on Deno processor '{}': port='{}', dest_port='{}', dest_service='{}', schema='{}'",
                source_proc_id, source_port, dest_port, service_name, output_schema
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
                    "schema_name": output_schema,
                    "max_payload_bytes": max_payload,
                }));
            tracing::debug!(
                "Stored output wiring on Python native processor '{}': port='{}', dest_port='{}', dest_service='{}', schema='{}'",
                source_proc_id, source_port, dest_port, service_name, output_schema
            );
        }
    }

    // Configure destination InputMailboxes with port (Rust side)
    {
        let mut dest_guard = dest_processor.lock();
        if let Some(input_mailboxes) = dest_guard.get_iceoryx2_input_mailboxes() {
            if !input_mailboxes.has_port(dest_port) {
                input_mailboxes.add_port(dest_port, 1, Default::default());
            }

            if !input_mailboxes.has_subscriber() {
                let subscriber = service.create_subscriber()?;
                input_mailboxes.set_subscriber(subscriber);
                tracing::debug!(
                    "Created iceoryx2 Subscriber for '{}' on service '{}' (source is subprocess)",
                    dest_proc_id,
                    service_name
                );
            }
            if !input_mailboxes.has_listener() {
                let listener = notify_service.create_listener()?;
                input_mailboxes.set_listener(listener);
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
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
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
    let output_schema = {
        let source_proc_type = graph
            .traversal_mut()
            .v(source_proc_id)
            .first()
            .map(|node| node.processor_type().to_string())
            .unwrap_or_default();

        PROCESSOR_REGISTRY
            .port_info(&source_proc_type)
            .and_then(|(_, outputs)| {
                outputs
                    .iter()
                    .find(|p| p.name == source_port)
                    .map(|p| p.data_type.clone())
            })
            .unwrap_or_default()
    };

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(&service_name)?;
    let notify_service = iceoryx2_node.open_or_create_notify_service(&notify_service_name)?;

    // Create Publisher sized for this schema's declared max payload.
    let max_payload = max_payload_bytes_for_schema(&output_schema);
    let publisher = service.create_publisher(max_payload)?;
    let notifier = notify_service.create_notifier()?;

    // Configure source OutputWriter with port mapping, publisher, and notifier.
    {
        let source_guard = source_processor.lock();
        if let Some(output_writer) = source_guard.get_iceoryx2_output_writer() {
            output_writer.add_connection(
                source_port,
                &output_schema,
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
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!(
        "Opened iceoryx2 service: {} [{}] (rust-to-subprocess, state: Wired)",
        service_name,
        link_id
    );
    Ok(())
}
