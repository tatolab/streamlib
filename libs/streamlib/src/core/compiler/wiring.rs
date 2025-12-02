//! Link wiring implementation for the compiler.

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::error::{Result, StreamError};
use crate::core::executor::execution_graph::ExecutionGraph;
use crate::core::executor::running::WiredLink;
use crate::core::executor::BoxedProcessor;
use crate::core::frames::{AudioFrame, DataFrame, VideoFrame};
use crate::core::graph::Graph;
use crate::core::link_channel::{LinkChannel, LinkId, LinkPortAddress, LinkPortType};

/// Wire a link by ID from the graph.
pub(super) fn wire_link(
    graph: &Arc<RwLock<Graph>>,
    execution_graph: &mut ExecutionGraph,
    link_channel: &mut LinkChannel,
    link_id: &LinkId,
) -> Result<()> {
    let (from_port, to_port) = {
        let graph_guard = graph.read();
        let link = graph_guard.get_link(link_id).ok_or_else(|| {
            StreamError::LinkNotFound(format!("Link '{}' not found in graph", link_id))
        })?;
        (link.from_port(), link.to_port())
    };

    wire_link_ports(graph, execution_graph, link_channel, &from_port, &to_port)?;
    Ok(())
}

/// Unwire a link by ID.
#[allow(dead_code)]
pub(crate) fn unwire_link(execution_graph: &mut ExecutionGraph, link_id: &LinkId) -> Result<()> {
    tracing::info!("Unwiring link: {}", link_id);

    let wired_link = execution_graph
        .get_link_runtime(link_id)
        .ok_or_else(|| StreamError::LinkNotFound(link_id.to_string()))?;

    let source_proc_id = wired_link.source.node.clone();
    let dest_proc_id = wired_link.target.node.clone();
    let source_port = wired_link.source.port.clone();
    let dest_port = wired_link.target.port.clone();

    let source_processor = execution_graph
        .get_processor_runtime(&source_proc_id)
        .and_then(|r| r.processor.clone());
    let dest_processor = execution_graph
        .get_processor_runtime(&dest_proc_id)
        .and_then(|r| r.processor.clone());

    if let Some(proc) = source_processor {
        let mut guard = proc.lock();
        if let Err(e) = guard.unwire_output_producer(&source_port, link_id) {
            tracing::warn!(
                "Failed to unwire output producer from {}.{}: {}",
                source_proc_id,
                source_port,
                e
            );
        }
    }

    if let Some(proc) = dest_processor {
        let mut guard = proc.lock();
        if let Err(e) = guard.unwire_input_consumer(&dest_port, link_id) {
            tracing::warn!(
                "Failed to unwire input consumer from {}.{}: {}",
                dest_proc_id,
                dest_port,
                e
            );
        }
    }

    execution_graph.remove_link_runtime(link_id);

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
    graph: &Arc<RwLock<Graph>>,
    execution_graph: &mut ExecutionGraph,
    link_channel: &mut LinkChannel,
    from_port: &str,
    to_port: &str,
) -> Result<LinkId> {
    let (source_proc_id, source_port) = parse_port_address(from_port)?;
    let (dest_proc_id, dest_port) = parse_port_address(to_port)?;

    let link = get_link_from_graph(graph, from_port, to_port)?;
    let link_id = link.id.clone();

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
        get_processor_pair(execution_graph, &source_proc_id, &dest_proc_id)?;

    validate_audio_compatibility(&source_processor, &dest_processor, from_port, to_port)?;

    let (source_port_type, _dest_port_type) = validate_port_types(
        &source_processor,
        &dest_processor,
        &source_port,
        &dest_port,
        from_port,
        to_port,
    )?;

    let source_addr = LinkPortAddress::new(source_proc_id.to_string(), source_port.to_string());
    let dest_addr = LinkPortAddress::new(dest_proc_id.to_string(), dest_port.to_string());
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
        execution_graph,
        &source_proc_id,
        &dest_proc_id,
        &source_port,
        &dest_port,
    )?;

    let wired = WiredLink::new(link, source_port_type, capacity);
    execution_graph.insert_link_runtime(link_id.clone(), wired);

    tracing::info!("Registered link: {}", link_id);
    Ok(link_id)
}

fn get_link_from_graph(
    graph: &Arc<RwLock<Graph>>,
    from_port: &str,
    to_port: &str,
) -> Result<crate::core::graph::Link> {
    let graph_guard = graph.read();
    let link_id = graph_guard.find_link(from_port, to_port).ok_or_else(|| {
        StreamError::InvalidLink(format!(
            "Link '{}' → '{}' not found in graph",
            from_port, to_port
        ))
    })?;
    graph_guard
        .find_link_by_id(&link_id)
        .cloned()
        .ok_or_else(|| StreamError::InvalidLink(format!("Link '{}' not found by ID", link_id)))
}

fn get_processor_pair(
    execution_graph: &ExecutionGraph,
    source_proc_id: &str,
    dest_proc_id: &str,
) -> Result<(Arc<Mutex<BoxedProcessor>>, Arc<Mutex<BoxedProcessor>>)> {
    let source_instance = execution_graph
        .get_processor_runtime(&source_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!("Source processor '{}' not found", source_proc_id))
        })?;

    let dest_instance = execution_graph
        .get_processor_runtime(&dest_proc_id.to_string())
        .ok_or_else(|| {
            StreamError::Configuration(format!(
                "Destination processor '{}' not found",
                dest_proc_id
            ))
        })?;

    let source_proc = source_instance.processor.as_ref().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Source processor '{}' has no processor reference",
            source_proc_id
        ))
    })?;

    let dest_proc = dest_instance.processor.as_ref().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Destination processor '{}' has no processor reference",
            dest_proc_id
        ))
    })?;

    Ok((Arc::clone(source_proc), Arc::clone(dest_proc)))
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
    execution_graph: &ExecutionGraph,
    source_proc_id: &str,
    dest_proc_id: &str,
    source_port: &str,
    dest_port: &str,
) -> Result<()> {
    let source_instance = execution_graph.get_processor_runtime(&source_proc_id.to_string());
    let dest_instance = execution_graph.get_processor_runtime(&dest_proc_id.to_string());

    if let (Some(src), Some(dst)) = (source_instance, dest_instance) {
        if let Some(src_proc) = src.processor.as_ref() {
            let mut source_guard = src_proc.lock();
            source_guard.set_output_process_function_invoke_send(
                source_port,
                dst.process_function_invoke_send.clone(),
            );

            tracing::debug!(
                "Wired process function invoke: {} ({}) → {} ({})",
                source_proc_id,
                source_port,
                dest_proc_id,
                dest_port
            );
        }
    }

    Ok(())
}
