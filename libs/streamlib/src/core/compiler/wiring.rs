//! Link wiring implementation for the compiler.
//!
//! Wiring creates ring buffers between processor ports and sets up
//! process function invoke channels for reactive processing.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::error::{Result, StreamError};
use crate::core::frames::{AudioFrame, DataFrame, VideoFrame};
use crate::core::graph::{ProcessInvokeChannel, ProcessorInstance, PropertyGraph};
use crate::core::link_channel::{LinkChannel, LinkId, LinkPortAddress, LinkPortType};
use crate::core::processors::BoxedProcessor;

/// Wire a link by ID from the graph.
pub fn wire_link(
    property_graph: &mut PropertyGraph,
    link_channel: &mut LinkChannel,
    link_id: &LinkId,
) -> Result<()> {
    let (from_port, to_port) = {
        let link = property_graph.get_link(link_id).ok_or_else(|| {
            StreamError::LinkNotFound(format!("Link '{}' not found in graph", link_id))
        })?;
        (link.from_port(), link.to_port())
    };

    wire_link_ports(property_graph, link_channel, &from_port, &to_port, link_id)?;
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
        if let Err(e) = guard.unwire_output_producer(&source_port, link_id) {
            tracing::warn!(
                "Failed to unwire output producer from {}.{}: {}",
                source_proc_id,
                source_port,
                e
            );
        }
    }

    if let Some(arc) = dest_arc {
        let mut guard = arc.lock();
        if let Err(e) = guard.unwire_input_consumer(&dest_port, link_id) {
            tracing::warn!(
                "Failed to unwire input consumer from {}.{}: {}",
                dest_proc_id,
                dest_port,
                e
            );
        }
    }

    // Remove link entity if we're tracking link components
    property_graph.remove_link_entity(link_id);

    tracing::info!("Unwired link: {}", link_id);
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
    link_channel: &mut LinkChannel,
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

    create_link_channel(
        link_channel,
        source_port_type,
        &source_addr,
        &dest_addr,
        capacity,
        &source_processor,
        &dest_processor,
        &source_port,
        &dest_port,
    )?;

    setup_process_function_invoke_channel(
        property_graph,
        &source_proc_id,
        &dest_proc_id,
        &source_port,
    )?;

    // Create link entity for tracking
    property_graph.ensure_link_entity(link_id);

    tracing::info!("Registered link: {}", link_id);
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

#[allow(clippy::too_many_arguments)]
fn create_link_channel(
    link_channel: &mut LinkChannel,
    port_type: LinkPortType,
    source_addr: &LinkPortAddress,
    dest_addr: &LinkPortAddress,
    capacity: usize,
    source_processor: &Arc<Mutex<BoxedProcessor>>,
    dest_processor: &Arc<Mutex<BoxedProcessor>>,
    source_port: &str,
    dest_port: &str,
) -> Result<()> {
    match port_type {
        LinkPortType::Audio => {
            let (producer, consumer) = link_channel.create_channel::<AudioFrame>(
                source_addr.clone(),
                dest_addr.clone(),
                capacity,
            )?;

            let mut source_guard = source_processor.lock();
            source_guard.wire_output_producer(source_port, Box::new(producer))?;
            drop(source_guard);

            let mut dest_guard = dest_processor.lock();
            dest_guard.wire_input_consumer(dest_port, Box::new(consumer))?;
        }
        LinkPortType::Video => {
            let (producer, consumer) = link_channel.create_channel::<VideoFrame>(
                source_addr.clone(),
                dest_addr.clone(),
                capacity,
            )?;

            let mut source_guard = source_processor.lock();
            source_guard.wire_output_producer(source_port, Box::new(producer))?;
            drop(source_guard);

            let mut dest_guard = dest_processor.lock();
            dest_guard.wire_input_consumer(dest_port, Box::new(consumer))?;
        }
        LinkPortType::Data => {
            let (producer, consumer) = link_channel.create_channel::<DataFrame>(
                source_addr.clone(),
                dest_addr.clone(),
                capacity,
            )?;

            let mut source_guard = source_processor.lock();
            source_guard.wire_output_producer(source_port, Box::new(producer))?;
            drop(source_guard);

            let mut dest_guard = dest_processor.lock();
            dest_guard.wire_input_consumer(dest_port, Box::new(consumer))?;
        }
    }
    Ok(())
}

fn setup_process_function_invoke_channel(
    property_graph: &PropertyGraph,
    source_proc_id: &str,
    dest_proc_id: &str,
    source_port: &str,
) -> Result<()> {
    // Get destination's process invoke channel sender
    let dest_channel = property_graph
        .get::<ProcessInvokeChannel>(&dest_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Destination processor '{}' has no ProcessInvokeChannel",
                dest_proc_id
            ))
        })?;
    let sender = dest_channel.sender.clone();
    drop(dest_channel);

    // Get source processor and set its output's invoke sender
    let source_instance = property_graph
        .get::<ProcessorInstance>(&source_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Source processor '{}' has no ProcessorInstance",
                source_proc_id
            ))
        })?;

    let mut source_guard = source_instance.0.lock();
    source_guard.set_output_process_function_invoke_send(source_port, sender);

    tracing::debug!(
        "Wired process function invoke: {} ({}) → {}",
        source_proc_id,
        source_port,
        dest_proc_id
    );

    Ok(())
}
