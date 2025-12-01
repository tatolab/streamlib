use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::error::{Result, StreamError};
use crate::core::executor::running::RunningProcessor;
use crate::core::executor::thread_runner::run_processor_loop;
use crate::core::graph::ProcessorId;
use crate::core::link_channel::LinkWakeupEvent;
use crate::core::processors::ProcessorState;

use super::{BoxedProcessor, SimpleExecutor};

/// Create a processor instance from graph definition.
pub(super) fn create_processor(executor: &mut SimpleExecutor, proc_id: &ProcessorId) -> Result<()> {
    let node = {
        let graph = executor.graph_ref()?;
        let graph_guard = graph.read();
        graph_guard.get_processor(proc_id).cloned().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found in graph", proc_id))
        })?
    };

    let factory = executor.factory_ref()?;
    let processor = factory.create(&node)?;
    create_processor_instance(executor, proc_id.clone(), processor)
}

/// Setup a processor (call after creation and wiring).
pub(super) fn setup_processor(
    executor: &mut SimpleExecutor,
    processor_id: &ProcessorId,
) -> Result<()> {
    let ctx = executor.runtime_ctx()?.clone();
    let exec_graph = executor.exec_graph_mut()?;

    let instance = exec_graph
        .get_processor_runtime_mut(processor_id)
        .ok_or_else(|| StreamError::NotFound(format!("Processor '{}' not found", processor_id)))?;

    if let Some(proc_ref) = &instance.processor {
        tracing::trace!("[{}] Calling __generated_setup...", processor_id);
        let mut guard = proc_ref.lock();
        guard.__generated_setup(&ctx)?;
        tracing::trace!("[{}] __generated_setup completed", processor_id);
    }

    Ok(())
}

/// Start a processor thread.
pub(super) fn start_processor(
    executor: &mut SimpleExecutor,
    processor_id: &ProcessorId,
) -> Result<()> {
    let exec_graph = executor.exec_graph_mut()?;

    let instance = exec_graph
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

    // Create new channels for this processor's thread
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
    let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded::<LinkWakeupEvent>();

    // Update the instance with new channels
    instance.shutdown_tx = shutdown_tx;
    instance.wakeup_tx = wakeup_tx;

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
                wakeup_rx,
                state_clone,
                exec_config,
            );
        })
        .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

    instance.thread = Some(thread);

    Ok(())
}

/// Shutdown a running processor.
pub(super) fn shutdown_processor(
    executor: &mut SimpleExecutor,
    processor_id: &ProcessorId,
) -> Result<()> {
    let exec_graph = executor.exec_graph_mut()?;

    let instance = exec_graph
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

// ============================================================================
// Internal helpers
// ============================================================================

fn create_processor_instance(
    executor: &mut SimpleExecutor,
    id: ProcessorId,
    processor: BoxedProcessor,
) -> Result<()> {
    let node = {
        let graph = executor.graph_ref()?;
        let graph_guard = graph.read();
        graph_guard.get_processor(&id).cloned().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found in graph", id))
        })?
    };

    let (shutdown_tx, _shutdown_rx) = crossbeam_channel::bounded(1);
    let (wakeup_tx, _wakeup_rx) = crossbeam_channel::unbounded::<LinkWakeupEvent>();

    let state = Arc::new(Mutex::new(ProcessorState::Idle));
    let processor_arc = Arc::new(Mutex::new(processor));

    let running = RunningProcessor::new(
        node,
        None,
        shutdown_tx,
        wakeup_tx,
        state,
        Some(processor_arc),
    );

    let exec_graph = executor.exec_graph_mut()?;
    exec_graph.insert_processor_runtime(id, running);

    Ok(())
}
