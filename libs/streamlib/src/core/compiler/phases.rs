//! Compilation phase implementations.
//!
//! Each phase is a pure function that takes the required state and modifies the execution graph.

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::context::RuntimeContext;
use crate::core::delegates::{FactoryDelegate, ProcessorDelegate};
use crate::core::error::{Result, StreamError};
use crate::core::executor::delta::GraphDelta;
use crate::core::executor::execution_graph::ExecutionGraph;
use crate::core::executor::running::RunningProcessor;
use crate::core::executor::thread_runner::run_processor_loop;
use crate::core::executor::BoxedProcessor;
use crate::core::graph::{Graph, ProcessorId};
use crate::core::link_channel::{LinkChannel, ProcessFunctionEvent};
use crate::core::processors::ProcessorState;

/// Phase 1: CREATE - Instantiate processor instances from factory.
pub(super) fn phase_create(
    factory: &Arc<dyn FactoryDelegate>,
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    graph: &Arc<RwLock<Graph>>,
    execution_graph: &mut ExecutionGraph,
    delta: &GraphDelta,
) -> Result<()> {
    for proc_id in &delta.processors_to_add {
        tracing::info!("[Phase 1: CREATE] {}", proc_id);
        create_processor(factory, processor_delegate, graph, execution_graph, proc_id)?;
    }
    Ok(())
}

/// Phase 2: WIRE - Create ring buffers and connect ports.
pub(super) fn phase_wire(
    graph: &Arc<RwLock<Graph>>,
    execution_graph: &mut ExecutionGraph,
    link_channel: &mut LinkChannel,
    delta: &GraphDelta,
) -> Result<()> {
    for link_id in &delta.links_to_add {
        tracing::info!("[Phase 2: WIRE] {}", link_id);
        super::wiring::wire_link(graph, execution_graph, link_channel, link_id)?;
    }
    Ok(())
}

/// Phase 3: SETUP - Initialize processors (GPU, devices).
pub(super) fn phase_setup(
    execution_graph: &mut ExecutionGraph,
    runtime_context: &Arc<RuntimeContext>,
    delta: &GraphDelta,
) -> Result<()> {
    for proc_id in &delta.processors_to_add {
        tracing::info!("[Phase 3: SETUP] {}", proc_id);
        setup_processor(execution_graph, runtime_context, proc_id)?;
    }
    Ok(())
}

/// Phase 4: START - Spawn processor threads.
pub(super) fn phase_start(
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    execution_graph: &mut ExecutionGraph,
    delta: &GraphDelta,
) -> Result<()> {
    for proc_id in &delta.processors_to_add {
        tracing::info!("[Phase 4: START] {}", proc_id);
        start_processor(processor_delegate, execution_graph, proc_id)?;
    }
    Ok(())
}

// ============================================================================
// Phase 1: CREATE implementation
// ============================================================================

fn create_processor(
    factory: &Arc<dyn FactoryDelegate>,
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    graph: &Arc<RwLock<Graph>>,
    execution_graph: &mut ExecutionGraph,
    proc_id: &ProcessorId,
) -> Result<()> {
    let node = {
        let graph_guard = graph.read();
        graph_guard.get_processor(proc_id).cloned().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found in graph", proc_id))
        })?
    };

    // Delegate callback: will_create
    processor_delegate.will_create(&node)?;

    let processor = factory.create(&node)?;

    // Delegate callback: did_create
    processor_delegate.did_create(&node, &processor)?;

    create_processor_instance(graph, execution_graph, proc_id.clone(), processor)
}

fn create_processor_instance(
    graph: &Arc<RwLock<Graph>>,
    execution_graph: &mut ExecutionGraph,
    id: ProcessorId,
    processor: BoxedProcessor,
) -> Result<()> {
    let node = {
        let graph_guard = graph.read();
        graph_guard.get_processor(&id).cloned().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found in graph", id))
        })?
    };

    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
    let (process_function_invoke_send, process_function_invoke_receive) =
        crossbeam_channel::unbounded::<ProcessFunctionEvent>();

    let state = Arc::new(Mutex::new(ProcessorState::Idle));
    let processor_arc = Arc::new(Mutex::new(processor));

    let running = RunningProcessor::new(
        node,
        None,
        shutdown_tx,
        shutdown_rx,
        process_function_invoke_send,
        process_function_invoke_receive,
        state,
        Some(processor_arc),
    );

    execution_graph.insert_processor_runtime(id, running);

    Ok(())
}

// ============================================================================
// Phase 3: SETUP implementation
// ============================================================================

fn setup_processor(
    execution_graph: &mut ExecutionGraph,
    runtime_context: &Arc<RuntimeContext>,
    processor_id: &ProcessorId,
) -> Result<()> {
    let instance = execution_graph
        .get_processor_runtime_mut(processor_id)
        .ok_or_else(|| StreamError::NotFound(format!("Processor '{}' not found", processor_id)))?;

    if let Some(proc_ref) = &instance.processor {
        tracing::trace!("[{}] Calling __generated_setup...", processor_id);
        let mut guard = proc_ref.lock();
        guard.__generated_setup(runtime_context)?;
        tracing::trace!("[{}] __generated_setup completed", processor_id);
    }

    Ok(())
}

// ============================================================================
// Phase 4: START implementation
// ============================================================================

fn start_processor(
    processor_delegate: &Arc<dyn ProcessorDelegate>,
    execution_graph: &mut ExecutionGraph,
    processor_id: &ProcessorId,
) -> Result<()> {
    let instance = execution_graph
        .get_processor_runtime_mut(processor_id)
        .ok_or_else(|| StreamError::NotFound(format!("Processor '{}' not found", processor_id)))?;

    // Already running?
    if instance.thread.is_some() {
        return Ok(());
    }

    let processor_arc = instance
        .processor
        .as_ref()
        .ok_or_else(|| {
            StreamError::Runtime(format!("Processor '{}' has no instance", processor_id))
        })?
        .clone();

    let exec_config = processor_arc.lock().execution_config();
    tracing::info!(
        "[{}] Starting with {}",
        processor_id,
        exec_config.execution.description()
    );

    // Delegate callback: will_start
    processor_delegate.will_start(processor_id)?;

    // Take the receivers from the instance (they were created in Phase 1)
    let shutdown_rx = instance.shutdown_rx.take().ok_or_else(|| {
        StreamError::Runtime(format!(
            "Processor '{}' shutdown_rx already taken",
            processor_id
        ))
    })?;
    let process_function_invoke_receive = instance
        .process_function_invoke_receive
        .take()
        .ok_or_else(|| {
            StreamError::Runtime(format!(
                "Processor '{}' process_function_invoke_receive already taken",
                processor_id
            ))
        })?;

    let processor_clone = Arc::clone(&processor_arc);
    let state_clone = Arc::clone(&instance.state);
    let id_clone = processor_id.clone();

    *instance.state.lock() = ProcessorState::Running;

    let thread = std::thread::Builder::new()
        .name(format!("processor-{}", processor_id))
        .spawn(move || {
            run_processor_loop(
                id_clone,
                processor_clone,
                shutdown_rx,
                process_function_invoke_receive,
                state_clone,
                exec_config,
            );
        })
        .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

    instance.thread = Some(thread);

    // Delegate callback: did_start
    processor_delegate.did_start(processor_id)?;

    Ok(())
}

// ============================================================================
// Shutdown (used by SimpleExecutor lifecycle)
// ============================================================================

/// Shutdown a running processor.
#[allow(dead_code)] // Will be used when SimpleExecutor lifecycle is migrated
pub(crate) fn shutdown_processor(
    execution_graph: &mut ExecutionGraph,
    processor_id: &ProcessorId,
) -> Result<()> {
    let instance = execution_graph
        .get_processor_runtime_mut(processor_id)
        .ok_or_else(|| StreamError::NotFound(format!("Processor '{}' not found", processor_id)))?;

    let current_state = *instance.state.lock();
    if current_state == ProcessorState::Stopped || current_state == ProcessorState::Stopping {
        return Ok(());
    }

    *instance.state.lock() = ProcessorState::Stopping;

    tracing::info!("[{}] Shutting down processor...", processor_id);

    instance.shutdown_tx.send(()).map_err(|_| {
        StreamError::Runtime(format!(
            "Failed to send shutdown signal to processor '{}'",
            processor_id
        ))
    })?;

    if let Some(handle) = instance.thread.take() {
        match handle.join() {
            Ok(_) => {
                tracing::info!("[{}] Processor thread joined successfully", processor_id);
                *instance.state.lock() = ProcessorState::Stopped;
            }
            Err(panic_err) => {
                tracing::error!(
                    "[{}] Processor thread panicked: {:?}",
                    processor_id,
                    panic_err
                );
                *instance.state.lock() = ProcessorState::Stopped;
                return Err(StreamError::Runtime(format!(
                    "Processor '{}' thread panicked",
                    processor_id
                )));
            }
        }
    }

    tracing::info!("[{}] Processor shut down", processor_id);
    Ok(())
}
