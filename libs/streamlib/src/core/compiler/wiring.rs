//! Link wiring implementation for the compiler.
//!
//! Wiring creates LinkInstances (ring buffers) between processor ports and sets up
//! process function invoke channels for reactive processing.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::error::{Result, StreamError};
use crate::core::frames::{AudioFrame, DataFrame, VideoFrame};
use crate::core::graph::{
    LinkOutputToProcessorWriterAndReader, LinkState, ProcessorInstance, PropertyGraph,
};
use crate::core::links::{LinkId, LinkInstanceManager, LinkPortAddress, LinkPortType};
use crate::core::processors::BoxedProcessor;

/// Wire a link by ID from the graph.
pub fn wire_link(
    property_graph: &mut PropertyGraph,
    link_instance_manager: &mut LinkInstanceManager,
    link_id: &LinkId,
) -> Result<()> {
    let (from_port, to_port) = {
        let link = property_graph.get_link(link_id).ok_or_else(|| {
            StreamError::LinkNotFound(format!("Link '{}' not found in graph", link_id))
        })?;
        (link.from_port(), link.to_port())
    };

    wire_link_ports(
        property_graph,
        link_instance_manager,
        &from_port,
        &to_port,
        link_id,
    )?;
    Ok(())
}

/// Unwire a link by ID.
#[allow(dead_code)]
pub fn unwire_link(property_graph: &mut PropertyGraph, link_id: &LinkId) -> Result<()> {
    tracing::info!("Unwiring link: {}", link_id);

    let link = property_graph
        .get_link(link_id)
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;

    let (source_proc_id, source_port) = parse_port_address(&link.from_port())?;
    let (dest_proc_id, dest_port) = parse_port_address(&link.to_port())?;

    // Get processor instance arcs first (clone them to release borrow)
    let source_arc = property_graph
        .get::<ProcessorInstance>(&source_proc_id)
        .map(|instance| instance.0.clone());
    let dest_arc = property_graph
        .get::<ProcessorInstance>(&dest_proc_id)
        .map(|instance| instance.0.clone());

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

    // Set link state to Disconnected (keep entity for state queries)
    if let Err(e) = property_graph.set_link_state(link_id, LinkState::Disconnected) {
        tracing::warn!("Failed to set link state to Disconnected: {}", e);
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
    property_graph: &mut PropertyGraph,
    link_instance_manager: &mut LinkInstanceManager,
    from_port: &str,
    to_port: &str,
    link_id: &LinkId,
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

    let source_addr = LinkPortAddress::new(source_proc_id.clone(), source_port.clone());
    let dest_addr = LinkPortAddress::new(dest_proc_id.clone(), dest_port.clone());
    let capacity = source_port_type.default_capacity();

    create_link_instance(
        link_instance_manager,
        source_port_type,
        &source_addr,
        &dest_addr,
        capacity,
        link_id,
        &source_processor,
        &dest_processor,
        &source_port,
        &dest_port,
    )?;

    setup_link_output_to_processor_message_writer(
        property_graph,
        &source_proc_id,
        &dest_proc_id,
        &source_port,
    )?;

    // Create link entity and set state to Wired
    property_graph.ensure_link_entity(link_id);
    property_graph.set_link_state(link_id, LinkState::Wired)?;

    tracing::info!("Registered link: {} (state: Wired)", link_id);
    Ok(())
}

fn get_processor_pair(
    property_graph: &PropertyGraph,
    source_proc_id: &str,
    dest_proc_id: &str,
) -> Result<(Arc<Mutex<BoxedProcessor>>, Arc<Mutex<BoxedProcessor>>)> {
    let source_instance = property_graph
        .get::<ProcessorInstance>(&source_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!("Source processor '{}' not found", source_proc_id))
        })?;

    let dest_instance = property_graph
        .get::<ProcessorInstance>(&dest_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Destination processor '{}' not found",
                dest_proc_id
            ))
        })?;

    Ok((Arc::clone(&source_instance.0), Arc::clone(&dest_instance.0)))
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

/// Wrapper for passing LinkOutputDataWriter with its LinkId through Box<dyn Any>.
pub struct LinkOutputDataWriterWrapper<T: crate::core::LinkPortMessage> {
    pub link_id: LinkId,
    pub data_writer: crate::core::LinkOutputDataWriter<T>,
}

/// Wrapper for passing LinkInputDataReader with its LinkId through Box<dyn Any>.
pub struct LinkInputDataReaderWrapper<T: crate::core::LinkPortMessage> {
    pub link_id: LinkId,
    pub data_reader: crate::core::LinkInputDataReader<T>,
}

#[allow(clippy::too_many_arguments)]
fn create_link_instance(
    link_instance_manager: &mut LinkInstanceManager,
    port_type: LinkPortType,
    source_addr: &LinkPortAddress,
    dest_addr: &LinkPortAddress,
    capacity: usize,
    link_id: &LinkId,
    source_processor: &Arc<Mutex<BoxedProcessor>>,
    dest_processor: &Arc<Mutex<BoxedProcessor>>,
    source_port: &str,
    dest_port: &str,
) -> Result<()> {
    match port_type {
        LinkPortType::Audio => {
            let (data_writer, data_reader) = link_instance_manager
                .create_link_instance::<AudioFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                    link_id.clone(),
                )?;

            let mut source_guard = source_processor.lock();
            source_guard.add_link_output_data_writer(
                source_port,
                Box::new(LinkOutputDataWriterWrapper {
                    link_id: link_id.clone(),
                    data_writer,
                }),
            )?;
            drop(source_guard);

            let mut dest_guard = dest_processor.lock();
            dest_guard.add_link_input_data_reader(
                dest_port,
                Box::new(LinkInputDataReaderWrapper {
                    link_id: link_id.clone(),
                    data_reader,
                }),
            )?;
        }
        LinkPortType::Video => {
            let (data_writer, data_reader) = link_instance_manager
                .create_link_instance::<VideoFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                    link_id.clone(),
                )?;

            let mut source_guard = source_processor.lock();
            source_guard.add_link_output_data_writer(
                source_port,
                Box::new(LinkOutputDataWriterWrapper {
                    link_id: link_id.clone(),
                    data_writer,
                }),
            )?;
            drop(source_guard);

            let mut dest_guard = dest_processor.lock();
            dest_guard.add_link_input_data_reader(
                dest_port,
                Box::new(LinkInputDataReaderWrapper {
                    link_id: link_id.clone(),
                    data_reader,
                }),
            )?;
        }
        LinkPortType::Data => {
            let (data_writer, data_reader) = link_instance_manager
                .create_link_instance::<DataFrame>(
                    source_addr.clone(),
                    dest_addr.clone(),
                    capacity,
                    link_id.clone(),
                )?;

            let mut source_guard = source_processor.lock();
            source_guard.add_link_output_data_writer(
                source_port,
                Box::new(LinkOutputDataWriterWrapper {
                    link_id: link_id.clone(),
                    data_writer,
                }),
            )?;
            drop(source_guard);

            let mut dest_guard = dest_processor.lock();
            dest_guard.add_link_input_data_reader(
                dest_port,
                Box::new(LinkInputDataReaderWrapper {
                    link_id: link_id.clone(),
                    data_reader,
                }),
            )?;
        }
    }
    Ok(())
}

fn setup_link_output_to_processor_message_writer(
    property_graph: &PropertyGraph,
    source_proc_id: &str,
    dest_proc_id: &str,
    source_port: &str,
) -> Result<()> {
    // Get destination's message writer
    let dest_writer_and_reader = property_graph
        .get::<LinkOutputToProcessorWriterAndReader>(&dest_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Destination processor '{}' has no LinkOutputToProcessorWriterAndReader",
                dest_proc_id
            ))
        })?;
    let message_writer = dest_writer_and_reader.writer.clone();
    drop(dest_writer_and_reader);

    // Get source processor and set its output's message writer
    let source_instance = property_graph
        .get::<ProcessorInstance>(&source_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Source processor '{}' has no ProcessorInstance",
                source_proc_id
            ))
        })?;

    let mut source_guard = source_instance.0.lock();
    source_guard.set_link_output_to_processor_message_writer(source_port, message_writer);

    tracing::debug!(
        "Set up message writer: {} ({}) → {}",
        source_proc_id,
        source_port,
        dest_proc_id
    );

    Ok(())
}
