// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::{Result, StreamError};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, LinkOutputToProcessorWriterAndReader,
    ProcessorPauseGateComponent, ProcessorReadyBarrierComponent, ProcessorReadyBarrierHandle,
    ProcessorUniqueId, ShutdownChannelComponent, StateComponent,
};

/// Attach infrastructure components to a processor node.
/// Returns a barrier handle for coordinating with the processor thread.
pub(crate) fn prepare_processor(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
) -> Result<ProcessorReadyBarrierHandle> {
    let node_mut = graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", proc_id))
        })?;

    // Create barrier for synchronization with processor thread
    let (barrier_component, barrier_handle) = ProcessorReadyBarrierComponent::new();

    // Attach infrastructure components (NO ProcessorInstanceComponent - thread creates it)
    node_mut.insert(barrier_component);
    node_mut.insert(ShutdownChannelComponent::new());
    node_mut.insert(LinkOutputToProcessorWriterAndReader::new());
    node_mut.insert(StateComponent::default());
    node_mut.insert(ProcessorPauseGateComponent::new());

    tracing::debug!("[{}] Infrastructure components attached", proc_id);
    Ok(barrier_handle)
}
