// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Compilation phase implementations.
//!
//! Individual processor operations for each compilation phase.
//! The Compiler orchestrates these operations with event publishing.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::context::RuntimeContext;
use crate::core::delegates::{
    FactoryDelegate, ProcessorDelegate, SchedulerDelegate, SchedulingStrategy,
};
use crate::core::error::{Result, StreamError};
use crate::core::execution::run_processor_loop;
use crate::core::graph::{
    Graph, GraphNode, GraphState, LinkOutputToProcessorWriterAndReader, ProcessorInstanceComponent,
    ProcessorPauseGateComponent, ProcessorUniqueId, ShutdownChannelComponent, StateComponent,
    ThreadHandleComponent,
};
use crate::core::processors::ProcessorState;

// ============================================================================
// Phase 1: CREATE implementation
// ============================================================================

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

// ============================================================================
// Phase 3: SETUP implementation
// ============================================================================

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

// ============================================================================
// Phase 4: START implementation
// ============================================================================

pub(crate) fn start_processor(
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    scheduler: &Arc<dyn SchedulerDelegate>,
    property_graph: &mut Graph,
    processor_id: impl AsRef<str>,
) -> Result<()> {
    let processor_id = processor_id.as_ref();
    // Check if already has a thread (already running)
    let has_thread = property_graph
        .traversal()
        .v(processor_id)
        .first()
        .map(|n| n.has::<ThreadHandleComponent>())
        .unwrap_or(false);

    if has_thread {
        return Ok(());
    }

    // Get the node to determine scheduling strategy
    let node = property_graph
        .traversal()
        .v(processor_id)
        .first()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;

    let strategy = scheduler.scheduling_strategy(node);
    tracing::info!(
        "[{}] Starting with strategy: {}",
        processor_id,
        strategy.description()
    );

    // Delegate callback: will_start
    processor_delegate.will_start(processor_id)?;

    match strategy {
        SchedulingStrategy::DedicatedThread { priority, name } => {
            spawn_dedicated_thread(property_graph, processor_id, priority, name)?;
        }
        SchedulingStrategy::MainThread => {
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
        SchedulingStrategy::WorkStealingPool => {
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
        SchedulingStrategy::Lightweight => {
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
    }

    // Delegate callback: did_start
    processor_delegate.did_start(processor_id)?;

    Ok(())
}

fn spawn_dedicated_thread(
    property_graph: &mut Graph,
    processor_id: impl AsRef<str>,
    _priority: crate::core::delegates::ThreadPriority,
    _name: Option<String>,
) -> Result<()> {
    let processor_id = processor_id.as_ref();
    // Get mutable node and extract all required data
    let node = property_graph
        .traversal_mut()
        .v(processor_id)
        .first_mut()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;

    let instance = node.get::<ProcessorInstanceComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no ProcessorInstance",
            processor_id
        ))
    })?;
    let processor_arc = instance.0.clone();

    let state = node.get::<StateComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no StateComponent",
            processor_id
        ))
    })?;
    let state_arc = state.0.clone();

    let channel = node.get_mut::<ShutdownChannelComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no ShutdownChannel",
            processor_id
        ))
    })?;
    let shutdown_rx = channel.take_receiver().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' shutdown receiver already taken",
            processor_id
        ))
    })?;

    let writer_and_reader = node
        .get_mut::<LinkOutputToProcessorWriterAndReader>()
        .ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' has no LinkOutputToProcessorWriterAndReader",
                processor_id
            ))
        })?;
    let message_reader = writer_and_reader.take_reader().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' message reader already taken",
            processor_id
        ))
    })?;

    let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' has no ProcessorPauseGate",
            processor_id
        ))
    })?;
    let pause_gate_inner = pause_gate.clone_inner();

    // Get execution config
    let exec_config = processor_arc.lock().execution_config();

    // Update state to Running
    *state_arc.lock() = ProcessorState::Running;

    // Spawn the thread
    let id_clone: ProcessorUniqueId = processor_id.into();
    let thread = std::thread::Builder::new()
        .name(format!("processor-{}", processor_id))
        .spawn(move || {
            run_processor_loop(
                id_clone,
                processor_arc,
                shutdown_rx,
                message_reader,
                state_arc,
                pause_gate_inner,
                exec_config,
            );
        })
        .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

    // Attach thread handle - need to get node again since we consumed the reference
    let node = property_graph
        .traversal_mut()
        .v(processor_id)
        .first_mut()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;
    node.insert(ThreadHandleComponent(thread));

    Ok(())
}

// ============================================================================
// Shutdown
// ============================================================================

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
