// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link wiring implementation for the compiler.
//!
//! Wiring creates LinkInstances (ring buffers) between processor ports and sets up
//! process function invoke channels for reactive processing.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::error::{Result, StreamError};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, InputLinkPortRef,
    LinkInstanceComponent, LinkOutputToProcessorWriterAndReader, LinkState, LinkStateComponent,
    LinkTypeInfoComponent, LinkUniqueId, OutputLinkPortRef, ProcessorInstanceComponent,
};
use crate::core::links::LinkFactoryDelegate;
use crate::core::processors::ProcessorInstance;
use crate::core::schema_registry::SCHEMA_REGISTRY;
use crate::core::ProcessorUniqueId;
use crate::LinkCapacity;

/// Wire a link by ID from the graph.
pub fn wire_link(
    graph: &mut Graph,
    link_factory: &dyn LinkFactoryDelegate,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let (from_port, to_port) = {
        let link = graph.traversal_mut().e(link_id).first().ok_or_else(|| {
            StreamError::LinkNotFound(format!("Link '{}' not found in graph", link_id))
        })?;
        (link.from_port().clone(), link.to_port().clone())
    };

    wire_link_ports(graph, link_factory, &from_port, &to_port, link_id)?;
    Ok(())
}

/// Unwire a link by ID.
pub fn unwire_link(graph: &mut Graph, link_id: &LinkUniqueId) -> Result<()> {
    tracing::info!("Unwiring link: {}", link_id);

    let (from_port, to_port) = {
        let link = graph
            .traversal_mut()
            .e(link_id)
            .first_mut()
            .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
        (link.from_port(), link.to_port())
    };

    let (source_proc_id, source_port) =
        (from_port.processor_id.clone(), from_port.port_name.clone());
    let (dest_proc_id, dest_port) = (to_port.processor_id.clone(), to_port.port_name.clone());

    // Get processor instance arcs first (clone them to release borrow)
    let source_arc = graph
        .traversal()
        .v(&source_proc_id)
        .first()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        });

    let dest_arc = graph.traversal().v(&dest_proc_id).first().and_then(|node| {
        node.get::<ProcessorInstanceComponent>()
            .map(|i| i.0.clone())
    });

    // Now we can operate on the cloned Arcs without borrowing property_graph
    if let Some(arc) = source_arc {
        let mut guard = arc.lock();
        if let Err(e) = guard.remove_link_output_data_writer(&source_port, link_id) {
            tracing::warn!(
                "Failed to remove data writer from {}.{}: {}",
                source_proc_id,
                source_port,
                e
            );
        }
    }

    if let Some(arc) = dest_arc {
        let mut guard = arc.lock();
        if let Err(e) = guard.remove_link_input_data_reader(&dest_port, link_id) {
            tracing::warn!(
                "Failed to remove data reader from {}.{}: {}",
                dest_proc_id,
                dest_port,
                e
            );
        }
    }

    // Remove link components and set state to Disconnected
    if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
        link.remove::<LinkInstanceComponent>();
        link.remove::<LinkTypeInfoComponent>();
        link.insert(LinkStateComponent(LinkState::Disconnected));
    }

    tracing::info!("Unwired link: {} (state: Disconnected)", link_id);
    Ok(())
}

// ============================================================================
// Internal wiring implementation
// ============================================================================

fn wire_link_ports(
    graph: &mut Graph,
    link_factory: &dyn LinkFactoryDelegate,
    from_port: &OutputLinkPortRef,
    to_port: &InputLinkPortRef,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let (source_proc_id, source_port) =
        (from_port.processor_id.clone(), from_port.port_name.clone());
    let (dest_proc_id, dest_port) = (to_port.processor_id.clone(), to_port.port_name.clone());

    tracing::info!(
        "Wiring {} ({}:{}) → ({}:{}) [{}]",
        from_port,
        source_proc_id,
        source_port,
        dest_proc_id,
        dest_port,
        link_id
    );

    let (source_processor, dest_processor) =
        get_processor_pair(graph, &source_proc_id, &dest_proc_id)?;

    validate_audio_compatibility(&source_processor, &dest_processor, from_port, to_port)?;

    let source_schema_name = validate_schema_compatibility(
        &source_processor,
        &dest_processor,
        &source_port,
        &dest_port,
        from_port,
        to_port,
    )?;

    let capacity = get_default_capacity_for_schema(&source_schema_name);

    // Create link instance via factory using schema name
    // Factory returns pre-wrapped data writers/readers that include the link_id
    let creation_result = link_factory.create_by_schema(
        &source_schema_name,
        LinkCapacity::from(capacity),
        link_id,
    )?;

    // Store instance and type info as components on the link
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkInstanceComponent(creation_result.instance));
    link.insert(creation_result.type_info);

    // Wire pre-wrapped data writer to source processor
    wire_data_writer_to_processor(
        &source_processor,
        &source_port,
        creation_result.schema_name,
        creation_result.data_writer,
    )?;

    // Wire pre-wrapped data reader to destination processor
    wire_data_reader_to_processor(
        &dest_processor,
        &dest_port,
        creation_result.schema_name,
        creation_result.data_reader,
    )?;

    setup_link_output_to_processor_message_writer(
        graph,
        &source_proc_id,
        &dest_proc_id,
        &source_port,
    )?;

    // Set link state to Wired
    let link = graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!("Registered link: {} (state: Wired)", link_id);
    Ok(())
}

fn get_processor_pair(
    property_graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    dest_proc_id: &ProcessorUniqueId,
) -> Result<(Arc<Mutex<ProcessorInstance>>, Arc<Mutex<ProcessorInstance>>)> {
    let source_arc = property_graph
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

    let dest_arc = property_graph
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

fn validate_audio_compatibility(
    _source_processor: &Arc<Mutex<ProcessorInstance>>,
    _dest_processor: &Arc<Mutex<ProcessorInstance>>,
    _from_port: &OutputLinkPortRef,
    _to_port: &InputLinkPortRef,
) -> Result<()> {
    // Audio requirements validation removed - compatibility is now handled
    // at the schema level rather than through processor descriptors
    Ok(())
}

fn validate_schema_compatibility(
    source_processor: &Arc<Mutex<ProcessorInstance>>,
    dest_processor: &Arc<Mutex<ProcessorInstance>>,
    source_port: &str,
    dest_port: &str,
    from_port: &OutputLinkPortRef,
    to_port: &InputLinkPortRef,
) -> Result<String> {
    let source_guard = source_processor.lock();
    let dest_guard = dest_processor.lock();

    let source_schema = source_guard
        .get_output_schema_name(source_port)
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Source processor does not have output port '{}'",
                source_port
            ))
        })?;

    let dest_schema = dest_guard.get_input_schema_name(dest_port).ok_or_else(|| {
        StreamError::Configuration(format!(
            "Destination processor does not have input port '{}'",
            dest_port
        ))
    })?;

    // Check schema compatibility via registry
    if !SCHEMA_REGISTRY.compatible(source_schema, dest_schema) {
        return Err(StreamError::Configuration(format!(
            "Schema mismatch: {} ({}) → {} ({})",
            from_port, source_schema, to_port, dest_schema
        )));
    }

    Ok(source_schema.to_string())
}

fn get_default_capacity_for_schema(schema_name: &str) -> usize {
    SCHEMA_REGISTRY.get_default_capacity(schema_name)
}

fn wire_data_writer_to_processor(
    processor: &Arc<Mutex<ProcessorInstance>>,
    port_name: &str,
    schema_name: &str,
    data_writer: Box<dyn std::any::Any + Send>,
) -> Result<()> {
    // Data writer is pre-wrapped with link_id by the schema factory
    let mut guard = processor.lock();
    guard.add_link_output_data_writer(port_name, schema_name, data_writer)?;
    Ok(())
}

fn wire_data_reader_to_processor(
    processor: &Arc<Mutex<ProcessorInstance>>,
    port_name: &str,
    schema_name: &str,
    data_reader: Box<dyn std::any::Any + Send>,
) -> Result<()> {
    // Data reader is pre-wrapped with link_id by the schema factory
    let mut guard = processor.lock();
    guard.add_link_input_data_reader(port_name, schema_name, data_reader)?;
    Ok(())
}

fn setup_link_output_to_processor_message_writer(
    graph: &mut Graph,
    source_proc_id: &ProcessorUniqueId,
    dest_proc_id: &ProcessorUniqueId,
    source_port: &str,
) -> Result<()> {
    // Get destination's message writer
    let message_writer = graph
        .traversal_mut()
        .v(dest_proc_id)
        .first_mut()
        .and_then(|node| {
            node.get::<LinkOutputToProcessorWriterAndReader>()
                .map(|w| w.writer.clone())
        })
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Destination processor '{}' has no LinkOutputToProcessorWriterAndReader",
                dest_proc_id
            ))
        })?;

    // Get source processor and set its output's message writer
    let source_arc = graph
        .traversal_mut()
        .v(source_proc_id)
        .first_mut()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        })
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Source processor '{}' has no ProcessorInstance",
                source_proc_id
            ))
        })?;

    let mut source_guard = source_arc.lock();
    source_guard.set_link_output_to_processor_message_writer(source_port, message_writer);

    tracing::debug!(
        "Set up message writer: {} ({}) → {}",
        source_proc_id,
        source_port,
        dest_proc_id
    );

    Ok(())
}
