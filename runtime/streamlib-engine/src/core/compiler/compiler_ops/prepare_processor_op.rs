// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorPauseGateComponent, ProcessorReadyBarrierComponent,
    ProcessorReadyBarrierHandle, ProcessorUniqueId, ShutdownChannelComponent, StateComponent,
};
use crate::core::processors::ProcessorState;

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
        .ok_or_else(|| Error::ProcessorNotFound(format!("Processor '{}' not found", proc_id)))?;

    // Create barrier for synchronization with processor thread
    let (barrier_component, barrier_handle) = ProcessorReadyBarrierComponent::new();

    // Attach infrastructure components (NO ProcessorInstanceComponent - thread creates it)
    node_mut.insert(barrier_component);
    node_mut.insert(ShutdownChannelComponent::new());
    node_mut.insert(ProcessorPauseGateComponent::new());

    // Transitioned, never re-inserted: `add_v` attached the state when the node
    // was added, and a fresh component would strand every waiter already
    // holding the old one.
    if let Some(state) = node_mut.get::<StateComponent>() {
        state.transition_to(ProcessorState::Idle);
    }

    tracing::debug!("[{}] Infrastructure components attached", proc_id);
    Ok(barrier_handle)
}
