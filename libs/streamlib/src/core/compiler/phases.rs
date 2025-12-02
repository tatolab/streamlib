//! Compilation phase implementations.
//!
//! Each phase attaches ECS components to processor entities in the PropertyGraph.
//! This replaces the old approach of creating RunningProcessor structs.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::compiler::delta::GraphDelta;
use crate::core::context::RuntimeContext;
use crate::core::delegates::{
    FactoryDelegate, ProcessorDelegate, SchedulerDelegate, SchedulingStrategy,
};
use crate::core::error::{Result, StreamError};
use crate::core::execution::run_processor_loop;
use crate::core::graph::{
    GraphState, LightweightMarker, MainThreadMarker, ProcessInvokeChannel, ProcessorId,
    ProcessorInstance, PropertyGraph, RayonPoolMarker, ShutdownChannel, StateComponent,
    ThreadHandle,
};
use crate::core::link_channel::LinkChannel;
use crate::core::processors::ProcessorState;

/// Phase 1: CREATE - Instantiate processor instances from factory.
///
/// Attaches `ProcessorInstance`, `ShutdownChannel`, `ProcessInvokeChannel`, and
/// `StateComponent` to each processor entity.
pub fn phase_create(
    factory: &Arc<dyn FactoryDelegate>,
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    property_graph: &mut PropertyGraph,
    delta: &GraphDelta,
) -> Result<()> {
    for proc_id in &delta.processors_to_add {
        tracing::info!("[Phase 1: CREATE] {}", proc_id);
        create_processor(factory, processor_delegate, property_graph, proc_id)?;
    }
    Ok(())
}

/// Phase 2: WIRE - Create ring buffers and connect ports.
pub fn phase_wire(
    property_graph: &mut PropertyGraph,
    link_channel: &mut LinkChannel,
    delta: &GraphDelta,
) -> Result<()> {
    for link_id in &delta.links_to_add {
        tracing::info!("[Phase 2: WIRE] {}", link_id);
        super::wiring::wire_link(property_graph, link_channel, link_id)?;
    }
    Ok(())
}

/// Phase 3: SETUP - Initialize processors (GPU, devices).
pub fn phase_setup(
    property_graph: &mut PropertyGraph,
    runtime_context: &Arc<RuntimeContext>,
    delta: &GraphDelta,
) -> Result<()> {
    for proc_id in &delta.processors_to_add {
        tracing::info!("[Phase 3: SETUP] {}", proc_id);
        setup_processor(property_graph, runtime_context, proc_id)?;
    }
    Ok(())
}

/// Phase 4: START - Spawn processor threads based on scheduler strategy.
///
/// Attaches `ThreadHandle` (for dedicated threads), `MainThreadMarker`,
/// `RayonPoolMarker`, or `LightweightMarker` based on scheduling strategy.
pub fn phase_start(
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    scheduler: &Arc<dyn SchedulerDelegate>,
    property_graph: &mut PropertyGraph,
    delta: &GraphDelta,
) -> Result<()> {
    for proc_id in &delta.processors_to_add {
        tracing::info!("[Phase 4: START] {}", proc_id);
        start_processor(processor_delegate, scheduler, property_graph, proc_id)?;
    }
    Ok(())
}

// ============================================================================
// Phase 1: CREATE implementation
// ============================================================================

fn create_processor(
    factory: &Arc<dyn FactoryDelegate>,
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    property_graph: &mut PropertyGraph,
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
    property_graph.insert(proc_id, ProcessInvokeChannel::new())?;
    property_graph.insert(proc_id, StateComponent::default())?;

    tracing::debug!("[{}] Created with ECS components", proc_id);
    Ok(())
}

// ============================================================================
// Phase 3: SETUP implementation
// ============================================================================

fn setup_processor(
    property_graph: &mut PropertyGraph,
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

    tracing::trace!("[{}] Calling __generated_setup...", processor_id);
    let mut guard = instance.0.lock();
    guard.__generated_setup(runtime_context)?;
    tracing::trace!("[{}] __generated_setup completed", processor_id);

    Ok(())
}

// ============================================================================
// Phase 4: START implementation
// ============================================================================

fn start_processor(
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    scheduler: &Arc<dyn SchedulerDelegate>,
    property_graph: &mut PropertyGraph,
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
        SchedulingStrategy::MainThread => {
            // Mark as main thread processor - will be scheduled differently
            property_graph.insert(processor_id, MainThreadMarker)?;
            schedule_on_main_thread(property_graph, processor_id)?;
        }
        SchedulingStrategy::WorkStealingPool => {
            // Mark for Rayon pool scheduling
            property_graph.insert(processor_id, RayonPoolMarker)?;
            // TODO: Register with Rayon pool
        }
        SchedulingStrategy::Lightweight => {
            // Mark as lightweight - runs inline
            property_graph.insert(processor_id, LightweightMarker)?;
        }
    }

    // Delegate callback: did_start
    processor_delegate.did_start(processor_id)?;

    Ok(())
}

fn spawn_dedicated_thread(
    property_graph: &mut PropertyGraph,
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

    // Take process invoke receiver
    let process_invoke_rx = {
        let mut channel = property_graph
            .get_mut::<ProcessInvokeChannel>(processor_id)
            .ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Processor '{}' has no ProcessInvokeChannel",
                    processor_id
                ))
            })?;
        channel.take_receiver().ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' process invoke receiver already taken",
                processor_id
            ))
        })?
    };

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
                process_invoke_rx,
                state_arc,
                exec_config,
            );
        })
        .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

    // Attach thread handle component
    property_graph.insert(processor_id, ThreadHandle(thread))?;

    Ok(())
}

fn schedule_on_main_thread(
    property_graph: &mut PropertyGraph,
    processor_id: &ProcessorId,
) -> Result<()> {
    // For main thread processors, we don't spawn a thread
    // Instead, they'll be driven by the main thread event loop
    // Just update state to Running
    let state = property_graph
        .get::<StateComponent>(processor_id)
        .ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' has no StateComponent",
                processor_id
            ))
        })?;
    *state.0.lock() = ProcessorState::Running;
    Ok(())
}

// ============================================================================
// Shutdown
// ============================================================================

/// Shutdown a running processor by removing its runtime components.
pub fn shutdown_processor(
    property_graph: &mut PropertyGraph,
    processor_id: &ProcessorId,
) -> Result<()> {
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
pub fn shutdown_all_processors(property_graph: &mut PropertyGraph) -> Result<()> {
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
