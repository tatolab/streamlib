// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::Result;
use crate::core::graph::{
    Graph, GraphNodeWithComponents, GraphState, ProcessorUniqueId, ShutdownChannelComponent,
    StateComponent, ThreadHandleComponent,
};
use crate::core::processors::ProcessorState;

/// Shutdown a running processor by removing its runtime components.
pub fn shutdown_processor(
    property_graph: &mut Graph,
    processor_id: &ProcessorUniqueId,
) -> Result<()> {
    // Check current state and set to stopping
    let node = match property_graph.traversal_mut().v(processor_id).first_mut() {
        Some(n) => n,
        None => return Ok(()), // Processor not found, nothing to shut down
    };

    if let Some(state) = node.get::<StateComponent>() {
        let current = *state.0.lock();
        if current == ProcessorState::Stopped || current == ProcessorState::Stopping {
            return Ok(()); // Already stopped or stopping
        }
        *state.0.lock() = ProcessorState::Stopping;
    }

    tracing::info!("[{}] Shutting down processor...", processor_id);

    // Send shutdown signal
    if let Some(channel) = node.get::<ShutdownChannelComponent>() {
        let _ = channel.sender.send(());
    }

    // Take thread handle
    let thread_handle = node.remove::<ThreadHandleComponent>();

    // Join thread if exists
    if let Some(handle) = thread_handle {
        match handle.0.join() {
            Ok(_) => {
                tracing::info!("[{}] Processor thread joined successfully", processor_id);
            }
            Err(panic_err) => {
                tracing::error!(
                    "[{}] Processor thread panicked: {:?}",
                    processor_id,
                    panic_err
                );
            }
        }
    }

    // Update state to stopped - need to get node again
    if let Some(node) = property_graph.traversal().v(processor_id).first() {
        if let Some(state) = node.get::<StateComponent>() {
            *state.0.lock() = ProcessorState::Stopped;
        }
    }

    tracing::info!("[{}] Processor shut down", processor_id);
    Ok(())
}

/// Shutdown all running processors in the graph.
pub fn shutdown_all_processors(property_graph: &mut Graph) -> Result<()> {
    // Get all processor IDs first
    let processor_ids: Vec<ProcessorUniqueId> = property_graph.traversal().v(()).ids();

    for id in processor_ids {
        if let Err(e) = shutdown_processor(property_graph, &id) {
            tracing::warn!("Error shutting down processor {}: {}", id, e);
        }
    }

    property_graph.set_state(GraphState::Idle);
    Ok(())
}
