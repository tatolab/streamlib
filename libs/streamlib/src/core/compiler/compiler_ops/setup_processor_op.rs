// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::core::context::RuntimeContext;
use crate::core::error::{Result, StreamError};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorInstanceComponent, ProcessorPauseGateComponent,
    ProcessorUniqueId,
};

pub(crate) fn setup_processor(
    graph: &mut Graph,
    runtime_context: &Arc<RuntimeContext>,
    processor_id: &ProcessorUniqueId,
) -> Result<()> {
    // Get processor instance and pause gate
    let node = graph.traversal().v(processor_id).first().ok_or_else(|| {
        StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
    })?;

    let instance = node.get::<ProcessorInstanceComponent>().ok_or_else(|| {
        StreamError::NotFound(format!(
            "Processor '{}' has no ProcessorInstance component",
            processor_id
        ))
    })?;
    let processor_arc = instance.0.clone();

    let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
        StreamError::NotFound(format!(
            "Processor '{}' has no ProcessorPauseGate component",
            processor_id
        ))
    })?;
    let pause_gate_inner = pause_gate.clone_inner();

    let processor_context = runtime_context.with_pause_gate(pause_gate_inner);

    tracing::trace!("[{}] Calling __generated_setup...", processor_id);
    let mut guard = processor_arc.lock();
    guard.__generated_setup(&processor_context)?;
    tracing::trace!("[{}] __generated_setup completed", processor_id);

    Ok(())
}
