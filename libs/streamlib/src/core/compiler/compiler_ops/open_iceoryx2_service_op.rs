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
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, InputLinkPortRef, LinkState,
    LinkStateComponent, LinkUniqueId, OutputLinkPortRef, ProcessorInstanceComponent,
};
use crate::core::processors::{ProcessorInstance, PROCESSOR_REGISTRY};
use crate::core::ProcessorUniqueId;

/// Open an iceoryx2 service for a connection in the graph.
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

    let (source_processor, dest_processor) =
        get_processor_pair(graph, &source_proc_id, &dest_proc_id)?;

    tracing::info!(
        "Opening iceoryx2 service: {} ({}:{}) -> ({}:{}) [{}]",
        from_port,
        source_proc_id,
        source_port,
        dest_proc_id,
        dest_port,
        link_id
    );

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
    let source_arc = graph
        .traversal_mut()
        .v(source_proc_id)
        .first_mut()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        })
        .ok_or_else(|| {
            StreamError::Configuration(format!("Source processor '{}' not found", source_proc_id))
        })?;

    let dest_arc = graph
        .traversal_mut()
        .v(dest_proc_id)
        .first_mut()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        })
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Destination processor '{}' not found",
                dest_proc_id
            ))
        })?;

    Ok((source_arc, dest_arc))
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
    // Generate service name: "streamlib/{source_processor}/{dest_processor}"
    let service_name = format!("streamlib/{}/{}", source_proc_id, dest_proc_id);

    tracing::debug!("Creating iceoryx2 service '{}'", service_name);

    // Create iceoryx2 Service, Publisher, and Subscriber using the Node
    let iceoryx2_node = runtime_ctx.iceoryx2_node();
    let service = iceoryx2_node.open_or_create_service(&service_name)?;

    // Create Publisher for source processor
    let publisher = service.create_publisher()?;
    tracing::debug!(
        "Created iceoryx2 Publisher for service '{}'",
        service_name
    );

    // Create Subscriber for destination processor
    let subscriber = service.create_subscriber()?;
    tracing::debug!(
        "Created iceoryx2 Subscriber for service '{}'",
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

    // Configure source OutputWriter with port mapping and set the Publisher
    {
        let mut source_guard = source_processor.lock();
        if let Some(output_writer) = source_guard.get_iceoryx2_output_writer() {
            output_writer.add_port(source_port, &output_schema, dest_port);
            output_writer.set_publisher(publisher);
            tracing::debug!(
                "Configured OutputWriter port '{}' -> '{}' with Publisher",
                source_port,
                dest_port
            );
        }
    }

    // Configure destination InputMailboxes with port and set the Subscriber
    {
        let mut dest_guard = dest_processor.lock();
        if let Some(input_mailboxes) = dest_guard.get_iceoryx2_input_mailboxes() {
            // Default history of 1 - keeps only the most recent payload
            input_mailboxes.add_port(dest_port, 1);
            input_mailboxes.set_subscriber(subscriber);
            tracing::debug!("Configured InputMailboxes port '{}' with Subscriber", dest_port);
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
