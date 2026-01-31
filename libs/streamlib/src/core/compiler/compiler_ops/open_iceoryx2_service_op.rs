// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 service operations for the compiler.
//!
//! Opens iceoryx2 publish-subscribe services between processor ports.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::context::RuntimeContext;
use crate::core::error::{Result, StreamError};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkState, LinkStateComponent,
    LinkUniqueId, ProcessorInstanceComponent, SubprocessHandleComponent,
};
use crate::core::processors::{ProcessorInstance, PROCESSOR_REGISTRY};
use crate::core::ProcessorUniqueId;

/// Check if a processor is a subprocess (Python, TypeScript, etc.)
fn is_subprocess_processor(graph: &mut Graph, proc_id: &ProcessorUniqueId) -> bool {
    graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .map(|n| n.has::<SubprocessHandleComponent>())
        .unwrap_or(false)
}

/// Open an iceoryx2 service for a connection in the graph.
///
/// Handles four cases:
/// - Rust→Rust: Full wiring (publisher + OutputWriter, subscriber + InputMailboxes)
/// - Rust→Python: Only source-side wiring (publisher + OutputWriter). Python creates its own subscriber.
/// - Python→Rust: Only dest-side wiring (subscriber + InputMailboxes). Python creates its own publisher.
/// - Python→Python: Service created but no Rust-side wiring. Both subprocesses manage their own connections.
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
        open_iceoryx2_subprocess_to_subprocess(graph, &dest_proc_id, link_id, runtime_ctx)
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

    tracing::debug!(
        "Opening iceoryx2 service '{}' for connection {} -> {}",
        service_name,
        source_proc_id,
        dest_proc_id
    );

    // Create iceoryx2 Service, Publisher, and Subscriber using the Node
    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(&service_name)?;

    // Create Publisher for source processor (each upstream gets its own publisher)
    let publisher = service.create_publisher()?;
    tracing::debug!(
        "Created iceoryx2 Publisher for '{}' -> service '{}'",
        source_proc_id,
        service_name
    );

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

    tracing::debug!(
        "Output port '{}' has schema '{}'",
        source_port,
        output_schema
    );

    // Configure source OutputWriter with port mapping and publisher
    {
        let source_guard = source_processor.lock();
        if let Some(output_writer) = source_guard.get_iceoryx2_output_writer() {
            output_writer.add_connection(source_port, &output_schema, dest_port, publisher);
            tracing::debug!(
                "Configured OutputWriter port '{}' -> '{}' with Publisher",
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
            // Always add the port mapping
            // Default history of 1 - keeps only the most recent payload
            // Default read mode is SkipToLatest (optimal for video)
            input_mailboxes.add_port(dest_port, 1, Default::default());

            // Only set subscriber if this is the first connection to this destination
            // All subsequent connections reuse the same subscriber
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
fn open_iceoryx2_subprocess_to_subprocess(
    graph: &mut Graph,
    dest_proc_id: &ProcessorUniqueId,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
) -> Result<()> {
    let service_name = format!("streamlib/{}", dest_proc_id);

    tracing::debug!(
        "Opening iceoryx2 service '{}' for subprocess-to-subprocess connection",
        service_name
    );

    // Ensure the service exists (both subprocesses will open it independently)
    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let _service = iceoryx2_node.open_or_create_service(&service_name)?;

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
    _source_port: &str,
    dest_port: &str,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
) -> Result<()> {
    let service_name = format!("streamlib/{}", dest_proc_id);

    tracing::debug!(
        "Opening iceoryx2 service '{}' for subprocess({}) -> rust({}) connection",
        service_name,
        source_proc_id,
        dest_proc_id
    );

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(&service_name)?;

    // Source is subprocess - it creates its own publisher. No Rust-side source wiring.

    // Configure destination InputMailboxes with port (Rust side)
    {
        let mut dest_guard = dest_processor.lock();
        if let Some(input_mailboxes) = dest_guard.get_iceoryx2_input_mailboxes() {
            input_mailboxes.add_port(dest_port, 1, Default::default());

            if !input_mailboxes.has_subscriber() {
                let subscriber = service.create_subscriber()?;
                input_mailboxes.set_subscriber(subscriber);
                tracing::debug!(
                    "Created iceoryx2 Subscriber for '{}' on service '{}' (source is subprocess)",
                    dest_proc_id,
                    service_name
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

    tracing::debug!(
        "Opening iceoryx2 service '{}' for rust({}) -> subprocess({}) connection",
        service_name,
        source_proc_id,
        dest_proc_id
    );

    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(&service_name)?;

    // Create Publisher for source processor (Rust side)
    let publisher = service.create_publisher()?;

    // Look up schema for the output port
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

    // Configure source OutputWriter with port mapping and publisher
    {
        let source_guard = source_processor.lock();
        if let Some(output_writer) = source_guard.get_iceoryx2_output_writer() {
            output_writer.add_connection(source_port, &output_schema, dest_port, publisher);
            tracing::debug!(
                "Configured OutputWriter port '{}' -> '{}' with Publisher (dest is subprocess)",
                source_port,
                dest_port
            );
        }
    }

    // Dest is subprocess - it creates its own subscriber. No Rust-side dest wiring.

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
