// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link wiring implementation for the compiler.
//!
//! Wiring creates LinkInstances (ring buffers) between processor ports and sets up
//! process function invoke channels for reactive processing.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::error::{Result, StreamError};
use crate::core::frames::{AudioFrame, DataFrame, VideoFrame};
use crate::core::graph::{
    Graph, GraphEdge, GraphNode, LinkInstanceComponent, LinkOutputToProcessorWriterAndReader,
    LinkState, LinkStateComponent, LinkTypeInfoComponent, LinkUniqueId, ProcessorInstanceComponent,
};
use crate::core::links::{LinkFactoryDelegate, LinkPortType};
use crate::core::processors::BoxedProcessor;

/// Wire a link by ID from the graph.
pub fn wire_link(
    property_graph: &mut Graph,
    link_factory: &dyn LinkFactoryDelegate,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let (from_port, to_port) = {
        let link = property_graph
            .traversal()
            .e(link_id)
            .first()
            .ok_or_else(|| {
                StreamError::LinkNotFound(format!("Link '{}' not found in graph", link_id))
            })?;
        (link.from_port(), link.to_port())
    };

    wire_link_ports(property_graph, link_factory, &from_port, &to_port, link_id)?;
    Ok(())
}

/// Unwire a link by ID.
pub fn unwire_link(property_graph: &mut Graph, link_id: &LinkUniqueId) -> Result<()> {
    tracing::info!("Unwiring link: {}", link_id);

    let (from_port, to_port) = {
        let link = property_graph
            .traversal()
            .e(link_id)
            .first()
            .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
        (link.from_port(), link.to_port())
    };

    let (source_proc_id, source_port) = parse_port_address(&from_port)?;
    let (dest_proc_id, dest_port) = parse_port_address(&to_port)?;

    // Get processor instance arcs first (clone them to release borrow)
    let source_arc = property_graph
        .traversal()
        .v(&source_proc_id)
        .first()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        });

    let dest_arc = property_graph
        .traversal()
        .v(&dest_proc_id)
        .first()
        .and_then(|node| {
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
    if let Some(link) = property_graph.traversal().e(link_id).first() {
        link.remove::<LinkInstanceComponent>();
        link.remove::<LinkTypeInfoComponent>();
        link.insert(LinkStateComponent(LinkState::Disconnected));
    }

    tracing::info!("Unwired link: {} (state: Disconnected)", link_id);
    Ok(())
}

/// Parse a port address string into (processor_id, port_name).
pub fn parse_port_address(port: &str) -> Result<(String, String)> {
    let (proc_id, port_name) = port.split_once('.').ok_or_else(|| {
        StreamError::Configuration(format!(
            "Invalid port format '{}'. Expected 'processor_id.port_name'",
            port
        ))
    })?;
    Ok((proc_id.to_string(), port_name.to_string()))
}

// ============================================================================
// Internal wiring implementation
// ============================================================================

fn wire_link_ports(
    property_graph: &mut Graph,
    link_factory: &dyn LinkFactoryDelegate,
    from_port: &str,
    to_port: &str,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let (source_proc_id, source_port) = parse_port_address(from_port)?;
    let (dest_proc_id, dest_port) = parse_port_address(to_port)?;

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
        get_processor_pair(property_graph, &source_proc_id, &dest_proc_id)?;

    validate_audio_compatibility(&source_processor, &dest_processor, from_port, to_port)?;

    let (source_port_type, _dest_port_type) = validate_port_types(
        &source_processor,
        &dest_processor,
        &source_port,
        &dest_port,
        from_port,
        to_port,
    )?;

    let capacity = source_port_type.default_capacity();

    // Create link instance via factory
    let creation_result = link_factory.create(link_id.clone(), source_port_type, capacity)?;

    // Store instance and type info as components on the link
    let link = property_graph
        .traversal()
        .e(link_id)
        .first()
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkInstanceComponent(creation_result.instance));
    link.insert(creation_result.type_info);

    // Wire data writer to source processor
    wire_data_writer_to_processor(
        &source_processor,
        &source_port,
        link_id,
        source_port_type,
        creation_result.data_writer,
    )?;

    // Wire data reader to destination processor
    wire_data_reader_to_processor(
        &dest_processor,
        &dest_port,
        link_id,
        source_port_type,
        creation_result.data_reader,
    )?;

    setup_link_output_to_processor_message_writer(
        property_graph,
        &source_proc_id,
        &dest_proc_id,
        &source_port,
    )?;

    // Set link state to Wired
    let link = property_graph
        .traversal()
        .e(link_id)
        .first()
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;
    link.insert(LinkStateComponent(LinkState::Wired));

    tracing::info!("Registered link: {} (state: Wired)", link_id);
    Ok(())
}

fn get_processor_pair(
    property_graph: &Graph,
    source_proc_id: &str,
    dest_proc_id: &str,
) -> Result<(Arc<Mutex<BoxedProcessor>>, Arc<Mutex<BoxedProcessor>>)> {
    let source_arc = property_graph
        .traversal()
        .v(&source_proc_id.to_string())
        .first()
        .and_then(|node| {
            node.get::<ProcessorInstanceComponent>()
                .map(|i| i.0.clone())
        })
        .ok_or_else(|| {
            StreamError::Configuration(format!("Source processor '{}' not found", source_proc_id))
        })?;

    let dest_arc = property_graph
        .traversal()
        .v(&dest_proc_id.to_string())
        .first()
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
    source_processor: &Arc<Mutex<BoxedProcessor>>,
    dest_processor: &Arc<Mutex<BoxedProcessor>>,
    from_port: &str,
    to_port: &str,
) -> Result<()> {
    let source_guard = source_processor.lock();
    let dest_guard = dest_processor.lock();

    let source_descriptor = source_guard.descriptor_instance();
    let dest_descriptor = dest_guard.descriptor_instance();

    if let (Some(source_desc), Some(dest_desc)) = (source_descriptor, dest_descriptor) {
        if let (Some(source_audio), Some(dest_audio)) = (
            &source_desc.audio_requirements,
            &dest_desc.audio_requirements,
        ) {
            if !source_audio.compatible_with(dest_audio) {
                let error_msg = source_audio.compatibility_error(dest_audio);
                return Err(StreamError::Configuration(format!(
                    "Audio requirements incompatible: {} → {}: {}",
                    from_port, to_port, error_msg
                )));
            }
        }
    }

    Ok(())
}

fn validate_port_types(
    source_processor: &Arc<Mutex<BoxedProcessor>>,
    dest_processor: &Arc<Mutex<BoxedProcessor>>,
    source_port: &str,
    dest_port: &str,
    from_port: &str,
    to_port: &str,
) -> Result<(LinkPortType, LinkPortType)> {
    let source_guard = source_processor.lock();
    let dest_guard = dest_processor.lock();

    let src_type = source_guard
        .get_output_port_type(source_port)
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Source processor does not have output port '{}'",
                source_port
            ))
        })?;

    let dst_type = dest_guard.get_input_port_type(dest_port).ok_or_else(|| {
        StreamError::Configuration(format!(
            "Destination processor does not have input port '{}'",
            dest_port
        ))
    })?;

    if !src_type.compatible_with(&dst_type) {
        return Err(StreamError::Configuration(format!(
            "Port type mismatch: {} ({:?}) → {} ({:?})",
            from_port, src_type, to_port, dst_type
        )));
    }

    Ok((src_type, dst_type))
}

/// Wrapper for passing LinkOutputDataWriter with its LinkUniqueId through Box<dyn Any>.
pub struct LinkOutputDataWriterWrapper<T: crate::core::LinkPortMessage> {
    pub link_id: LinkUniqueId,
    pub data_writer: crate::core::LinkOutputDataWriter<T>,
}

/// Wrapper for passing LinkInputDataReader with its LinkUniqueId through Box<dyn Any>.
pub struct LinkInputDataReaderWrapper<T: crate::core::LinkPortMessage> {
    pub link_id: LinkUniqueId,
    pub data_reader: crate::core::LinkInputDataReader<T>,
}

fn wire_data_writer_to_processor(
    processor: &Arc<Mutex<BoxedProcessor>>,
    port_name: &str,
    link_id: &LinkUniqueId,
    port_type: LinkPortType,
    data_writer: Box<dyn std::any::Any + Send>,
) -> Result<()> {
    let mut guard = processor.lock();

    match port_type {
        LinkPortType::Audio => {
            let writer = data_writer
                .downcast::<crate::core::LinkOutputDataWriter<AudioFrame>>()
                .map_err(|_| StreamError::Link("Failed to downcast audio data writer".into()))?;
            guard.add_link_output_data_writer(
                port_name,
                Box::new(LinkOutputDataWriterWrapper {
                    link_id: link_id.clone(),
                    data_writer: *writer,
                }),
            )?;
        }
        LinkPortType::Video => {
            let writer = data_writer
                .downcast::<crate::core::LinkOutputDataWriter<VideoFrame>>()
                .map_err(|_| StreamError::Link("Failed to downcast video data writer".into()))?;
            guard.add_link_output_data_writer(
                port_name,
                Box::new(LinkOutputDataWriterWrapper {
                    link_id: link_id.clone(),
                    data_writer: *writer,
                }),
            )?;
        }
        LinkPortType::Data => {
            let writer = data_writer
                .downcast::<crate::core::LinkOutputDataWriter<DataFrame>>()
                .map_err(|_| StreamError::Link("Failed to downcast data writer".into()))?;
            guard.add_link_output_data_writer(
                port_name,
                Box::new(LinkOutputDataWriterWrapper {
                    link_id: link_id.clone(),
                    data_writer: *writer,
                }),
            )?;
        }
    }

    Ok(())
}

fn wire_data_reader_to_processor(
    processor: &Arc<Mutex<BoxedProcessor>>,
    port_name: &str,
    link_id: &LinkUniqueId,
    port_type: LinkPortType,
    data_reader: Box<dyn std::any::Any + Send>,
) -> Result<()> {
    let mut guard = processor.lock();

    match port_type {
        LinkPortType::Audio => {
            let reader = data_reader
                .downcast::<crate::core::LinkInputDataReader<AudioFrame>>()
                .map_err(|_| StreamError::Link("Failed to downcast audio data reader".into()))?;
            guard.add_link_input_data_reader(
                port_name,
                Box::new(LinkInputDataReaderWrapper {
                    link_id: link_id.clone(),
                    data_reader: *reader,
                }),
            )?;
        }
        LinkPortType::Video => {
            let reader = data_reader
                .downcast::<crate::core::LinkInputDataReader<VideoFrame>>()
                .map_err(|_| StreamError::Link("Failed to downcast video data reader".into()))?;
            guard.add_link_input_data_reader(
                port_name,
                Box::new(LinkInputDataReaderWrapper {
                    link_id: link_id.clone(),
                    data_reader: *reader,
                }),
            )?;
        }
        LinkPortType::Data => {
            let reader = data_reader
                .downcast::<crate::core::LinkInputDataReader<DataFrame>>()
                .map_err(|_| StreamError::Link("Failed to downcast data reader".into()))?;
            guard.add_link_input_data_reader(
                port_name,
                Box::new(LinkInputDataReaderWrapper {
                    link_id: link_id.clone(),
                    data_reader: *reader,
                }),
            )?;
        }
    }

    Ok(())
}

fn setup_link_output_to_processor_message_writer(
    property_graph: &mut Graph,
    source_proc_id: &str,
    dest_proc_id: &str,
    source_port: &str,
) -> Result<()> {
    // Get destination's message writer
    let message_writer = property_graph
        .traversal()
        .v(&dest_proc_id.to_string())
        .first()
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
    let source_arc = property_graph
        .traversal()
        .v(&source_proc_id.to_string())
        .first()
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
