use std::sync::Arc;

use parking_lot::Mutex;

use super::BoxedProcessor;
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::link_channel::ProcessFunctionEvent;
use crate::core::processors::ProcessorState;

type ProcessorId = String;

/// Run the processor thread main loop based on execution mode.
pub(super) fn run_processor_loop(
    id: ProcessorId,
    processor: Arc<Mutex<BoxedProcessor>>,
    shutdown_rx: crossbeam_channel::Receiver<()>,
    process_function_invoke_receive: crossbeam_channel::Receiver<ProcessFunctionEvent>,
    state: Arc<Mutex<ProcessorState>>,
    exec_config: ExecutionConfig,
) {
    tracing::info!(
        "[{}] Thread started with {}",
        id,
        exec_config.execution.description()
    );

    tracing::trace!("[{}] About to enter execution mode loop", id);

    match exec_config.execution {
        ProcessExecution::Continuous { interval_ms } => {
            tracing::trace!(
                "[{}] Entering continuous mode (interval={}ms)",
                id,
                interval_ms
            );
            run_continuous_mode(&id, &processor, &shutdown_rx, interval_ms);
        }
        ProcessExecution::Reactive => {
            tracing::trace!("[{}] Entering reactive mode", id);
            run_reactive_mode(
                &id,
                &processor,
                &shutdown_rx,
                &process_function_invoke_receive,
            );
        }
        ProcessExecution::Manual => {
            tracing::trace!("[{}] Entering manual mode", id);
            run_manual_mode(
                &id,
                &processor,
                &shutdown_rx,
                &process_function_invoke_receive,
            );
        }
    }

    tracing::trace!("[{}] Exited execution mode loop, calling teardown", id);

    // Teardown
    {
        let mut guard = processor.lock();
        tracing::trace!("[{}] Calling __generated_teardown...", id);
        if let Err(e) = guard.__generated_teardown() {
            tracing::warn!("[{}] Teardown error: {}", id, e);
        }
        tracing::trace!("[{}] __generated_teardown completed", id);
    }

    *state.lock() = ProcessorState::Stopped;
    tracing::debug!("[{}] Thread stopped", id);
}

fn run_continuous_mode(
    id: &ProcessorId,
    processor: &Arc<Mutex<BoxedProcessor>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    interval_ms: u32,
) {
    let sleep_duration = if interval_ms > 0 {
        std::time::Duration::from_millis(interval_ms as u64)
    } else {
        std::time::Duration::from_micros(100)
    };

    tracing::debug!(
        "[{}] Continuous mode: process() called every {:?}",
        id,
        sleep_duration
    );

    loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }

        {
            let mut guard = processor.lock();
            if let Err(e) = guard.process() {
                tracing::warn!("[{}] Process error: {}", id, e);
            }
        }

        std::thread::sleep(sleep_duration);
    }
}

fn run_reactive_mode(
    id: &ProcessorId,
    processor: &Arc<Mutex<BoxedProcessor>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    process_function_invoke_receive: &crossbeam_channel::Receiver<ProcessFunctionEvent>,
) {
    tracing::debug!("[{}] Reactive mode: waiting for input data...", id);

    loop {
        crossbeam_channel::select! {
            recv(shutdown_rx) -> _ => break,
            recv(process_function_invoke_receive) -> msg => {
                if let Ok(event) = msg {
                    if event == ProcessFunctionEvent::StopProcessing {
                        break;
                    }
                    let mut guard = processor.lock();
                    if let Err(e) = guard.process() {
                        tracing::warn!("[{}] Process error: {}", id, e);
                    }
                }
            }
        }
    }
}

fn run_manual_mode(
    id: &ProcessorId,
    processor: &Arc<Mutex<BoxedProcessor>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    process_function_invoke_receive: &crossbeam_channel::Receiver<ProcessFunctionEvent>,
) {
    tracing::info!(
        "[{}] Manual mode: calling process() once, then YOU control timing",
        id
    );

    // Initial process call to let processor set up callbacks/threads
    tracing::trace!(
        "[{}] Manual mode: acquiring lock for initial process()...",
        id
    );
    {
        let mut guard = processor.lock();
        tracing::trace!("[{}] Manual mode: lock acquired, calling process()...", id);
        if let Err(e) = guard.process() {
            tracing::warn!("[{}] Initial process error: {}", id, e);
        }
        tracing::trace!("[{}] Manual mode: process() returned", id);
    }
    tracing::trace!("[{}] Manual mode: lock released after process()", id);

    tracing::debug!(
        "[{}] Manual mode: runtime will NOT call process() again",
        id
    );

    tracing::trace!("[{}] Manual mode: entering wait loop for shutdown", id);

    // Wait for shutdown - processor manages its own timing
    loop {
        crossbeam_channel::select! {
            recv(shutdown_rx) -> _ => {
                tracing::trace!("[{}] Manual mode: received shutdown signal", id);
                break;
            },
            recv(process_function_invoke_receive) -> msg => {
                if let Ok(event) = msg {
                    tracing::trace!("[{}] Manual mode: received process event {:?}", id, event);
                    if event == ProcessFunctionEvent::StopProcessing {
                        break;
                    }
                }
            }
            default(std::time::Duration::from_millis(100)) => {}
        }
    }
    tracing::trace!("[{}] Manual mode: exiting wait loop", id);
}
