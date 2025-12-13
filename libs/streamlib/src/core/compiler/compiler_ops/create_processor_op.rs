// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::delegates::{FactoryDelegate, ProcessorDelegate};
use crate::core::error::{Result, StreamError};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, LinkOutputToProcessorWriterAndReader,
    ProcessorInstanceComponent, ProcessorPauseGateComponent, ProcessorUniqueId,
    ShutdownChannelComponent, StateComponent,
};

pub(crate) fn create_processor(
    factory: &Arc<dyn FactoryDelegate>,
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
) -> Result<()> {
    // Get node from the graph
    let node = graph.traversal().v(proc_id).first().ok_or_else(|| {
        StreamError::ProcessorNotFound(format!("Processor '{}' not found in graph", proc_id))
    })?;

    // Delegate callback: will_create
    processor_delegate.will_create(node)?;

    // Create processor instance via factory
    let processor = factory.create(node)?;

    // Delegate callback: did_create
    processor_delegate.did_create(node, &processor)?;

    // Attach components to processor node
    let processor_arc = Arc::new(Mutex::new(processor));

    let node_mut = graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", proc_id))
        })?;

    node_mut.insert(ProcessorInstanceComponent(processor_arc));
    node_mut.insert(ShutdownChannelComponent::new());
    node_mut.insert(LinkOutputToProcessorWriterAndReader::new());
    node_mut.insert(StateComponent::default());
    node_mut.insert(ProcessorPauseGateComponent::new());

    tracing::debug!("[{}] Created with components", proc_id);
    Ok(())
}
