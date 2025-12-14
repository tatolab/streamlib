// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor thread runner.
//!
//! Handles the main loop for processor threads based on their execution mode.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::ProcessorUniqueId;
use crate::core::links::LinkOutputToProcessorMessage;
use crate::core::processors::{ProcessorInstance, ProcessorState};

/// Duration to sleep when paused (avoids busy-waiting).
const PAUSE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Run the processor thread main loop based on execution mode.
pub fn run_processor_loop(
    id: ProcessorUniqueId,
    processor: Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: crossbeam_channel::Receiver<()>,
    message_reader: crossbeam_channel::Receiver<LinkOutputToProcessorMessage>,
    state: Arc<Mutex<ProcessorState>>,
    pause_gate: Arc<AtomicBool>,
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
            run_continuous_mode(&id, &processor, &shutdown_rx, &pause_gate, interval_ms);
        }
        ProcessExecution::Reactive => {
            tracing::trace!("[{}] Entering reactive mode", id);
            run_reactive_mode(&id, &processor, &shutdown_rx, &message_reader, &pause_gate);
        }
        ProcessExecution::Manual => {
            tracing::trace!("[{}] Entering manual mode", id);
            // Manual mode doesn't use the pause gate in the thread runner -
            // the processor is responsible for checking RuntimeContext::is_paused()
            run_manual_mode(&id, &processor, &shutdown_rx, &message_reader);
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
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    pause_gate: &Arc<AtomicBool>,
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

        // Check pause gate before processing
        if pause_gate.load(Ordering::Acquire) {
            tracing::trace!("[{}] Paused, skipping process()", id);
            std::thread::sleep(PAUSE_CHECK_INTERVAL);
            continue;
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
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    message_reader: &crossbeam_channel::Receiver<LinkOutputToProcessorMessage>,
    pause_gate: &Arc<AtomicBool>,
) {
    tracing::debug!("[{}] Reactive mode: waiting for input data...", id);

    loop {
        crossbeam_channel::select! {
            recv(shutdown_rx) -> _ => break,
            recv(message_reader) -> msg => {
                if let Ok(message) = msg {
                    if message == LinkOutputToProcessorMessage::StopProcessingNow {
                        break;
                    }

                    // Check pause gate before processing
                    if pause_gate.load(Ordering::Acquire) {
                        tracing::trace!("[{}] Paused, discarding incoming data", id);
                        continue;
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
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    message_reader: &crossbeam_channel::Receiver<LinkOutputToProcessorMessage>,
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
            recv(message_reader) -> msg => {
                if let Ok(message) = msg {
                    tracing::trace!("[{}] Manual mode: received message {:?}", id, message);
                    if message == LinkOutputToProcessorMessage::StopProcessingNow {
                        break;
                    }
                }
            }
            default(std::time::Duration::from_millis(100)) => {}
        }
    }
    tracing::trace!("[{}] Manual mode: exiting wait loop", id);
}
