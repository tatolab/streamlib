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
    Graph, GraphState, LinkOutputToProcessorWriterAndReader, ProcessorId, ProcessorInstance,
    ProcessorPauseGate, ShutdownChannel, StateComponent, ThreadHandle,
};
use crate::core::processors::ProcessorState;

// ============================================================================
// Phase 1: CREATE implementation
// ============================================================================

pub(crate) fn create_processor(
    factory: &Arc<dyn FactoryDelegate>,
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    property_graph: &mut Graph,
    proc_id: &ProcessorId,
) -> Result<()> {
    // Get node from the underlying graph
    let node = property_graph.get_processor(proc_id).ok_or_else(|| {
        StreamError::ProcessorNotFound(format!("Processor '{}' not found in graph", proc_id))
    })?;

    // Delegate callback: will_create
    processor_delegate.will_create(&node)?;

    // Create processor instance via factory
    let processor = factory.create(&node)?;

    // Delegate callback: did_create
    processor_delegate.did_create(&node, &processor)?;

    // Ensure entity exists for this processor
    property_graph.ensure_processor_entity(proc_id);

    // Attach ECS components
    let processor_arc = Arc::new(Mutex::new(processor));
    property_graph.insert(proc_id, ProcessorInstance(processor_arc))?;
    property_graph.insert(proc_id, ShutdownChannel::new())?;
    property_graph.insert(proc_id, LinkOutputToProcessorWriterAndReader::new())?;
    property_graph.insert(proc_id, StateComponent::default())?;
    property_graph.insert(proc_id, ProcessorPauseGate::new())?;

    tracing::debug!("[{}] Created with ECS components", proc_id);
    Ok(())
}

// ============================================================================
// Phase 3: SETUP implementation
// ============================================================================

pub(crate) fn setup_processor(
    property_graph: &mut Graph,
    runtime_context: &Arc<RuntimeContext>,
    processor_id: &ProcessorId,
) -> Result<()> {
    // Get the processor instance component
    let instance = property_graph
        .get::<ProcessorInstance>(processor_id)
        .ok_or_else(|| {
            StreamError::NotFound(format!(
                "Processor '{}' has no ProcessorInstance component",
                processor_id
            ))
        })?;

    // Get pause gate to create processor-specific context
    let pause_gate = property_graph
        .get::<ProcessorPauseGate>(processor_id)
        .ok_or_else(|| {
            StreamError::NotFound(format!(
                "Processor '{}' has no ProcessorPauseGate component",
                processor_id
            ))
        })?;
    let processor_context = runtime_context.with_pause_gate(pause_gate.clone_inner());
    drop(pause_gate);

    tracing::trace!("[{}] Calling __generated_setup...", processor_id);
    let mut guard = instance.0.lock();
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
    processor_id: &ProcessorId,
) -> Result<()> {
    // Check if already has a thread (already running)
    if property_graph.has::<ThreadHandle>(processor_id) {
        return Ok(());
    }

    // Get the node to determine scheduling strategy
    let node = property_graph.get_processor(processor_id).ok_or_else(|| {
        StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
    })?;

    let strategy = scheduler.scheduling_strategy(&node);
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
        // TODO: Implement alternative scheduling strategies in separate branch
        // For now, all processors use dedicated threads
        SchedulingStrategy::MainThread => {
            // MainThread processors still get a dedicated thread - they dispatch internally
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
        SchedulingStrategy::WorkStealingPool => {
            // Fallback to dedicated thread until Rayon pool is implemented
            spawn_dedicated_thread(
                property_graph,
                processor_id,
                crate::core::delegates::ThreadPriority::Normal,
                None,
            )?;
        }
        SchedulingStrategy::Lightweight => {
            // Fallback to dedicated thread until inline execution is implemented
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
    processor_id: &ProcessorId,
    _priority: crate::core::delegates::ThreadPriority,
    _name: Option<String>,
) -> Result<()> {
    // Get required components
    let instance = property_graph
        .get::<ProcessorInstance>(processor_id)
        .ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' has no ProcessorInstance",
                processor_id
            ))
        })?;
    let processor_arc = instance.0.clone();
    drop(instance);

    // Get execution config
    let exec_config = processor_arc.lock().execution_config();

    // Get state component
    let state = property_graph
        .get::<StateComponent>(processor_id)
        .ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' has no StateComponent",
                processor_id
            ))
        })?;
    let state_arc = state.0.clone();
    drop(state);

    // Take shutdown receiver from the channel component
    let shutdown_rx = {
        let mut channel = property_graph
            .get_mut::<ShutdownChannel>(processor_id)
            .ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Processor '{}' has no ShutdownChannel",
                    processor_id
                ))
            })?;
        channel.take_receiver().ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' shutdown receiver already taken",
                processor_id
            ))
        })?
    };

    // Take message reader from LinkOutput
    let message_reader = {
        let mut writer_and_reader = property_graph
            .get_mut::<LinkOutputToProcessorWriterAndReader>(processor_id)
            .ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Processor '{}' has no LinkOutputToProcessorWriterAndReader",
                    processor_id
                ))
            })?;
        writer_and_reader.take_reader().ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' message reader already taken",
                processor_id
            ))
        })?
    };

    // Get pause gate
    let pause_gate = property_graph
        .get::<ProcessorPauseGate>(processor_id)
        .ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' has no ProcessorPauseGate",
                processor_id
            ))
        })?;
    let pause_gate_inner = pause_gate.clone_inner();
    drop(pause_gate);

    // Update state to Running
    *state_arc.lock() = ProcessorState::Running;

    // Spawn the thread
    let id_clone = processor_id.clone();
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

    // Attach thread handle component
    property_graph.insert(processor_id, ThreadHandle(thread))?;

    Ok(())
}

// ============================================================================
// Shutdown
// ============================================================================

/// Shutdown a running processor by removing its runtime components.
pub fn shutdown_processor(property_graph: &mut Graph, processor_id: &ProcessorId) -> Result<()> {
    // Check current state
    if let Some(state) = property_graph.get::<StateComponent>(processor_id) {
        let current = *state.0.lock();
        if current == ProcessorState::Stopped || current == ProcessorState::Stopping {
            return Ok(());
        }
        *state.0.lock() = ProcessorState::Stopping;
    }

    tracing::info!("[{}] Shutting down processor...", processor_id);

    // Send shutdown signal
    if let Some(channel) = property_graph.get::<ShutdownChannel>(processor_id) {
        let _ = channel.sender.send(());
    }

    // Join thread if exists
    if let Some(handle) = property_graph.remove::<ThreadHandle>(processor_id) {
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

    // Update state to stopped
    if let Some(state) = property_graph.get::<StateComponent>(processor_id) {
        *state.0.lock() = ProcessorState::Stopped;
    }

    tracing::info!("[{}] Processor shut down", processor_id);
    Ok(())
}

/// Shutdown all running processors in the graph.
pub fn shutdown_all_processors(property_graph: &mut Graph) -> Result<()> {
    // Get all processor IDs that have instances
    let processor_ids: Vec<ProcessorId> = property_graph.processor_ids().cloned().collect();

    for id in processor_ids {
        if let Err(e) = shutdown_processor(property_graph, &id) {
            tracing::warn!("Error shutting down processor {}: {}", id, e);
        }
    }

    property_graph.set_state(GraphState::Idle);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests will be added once the full integration is complete
}
