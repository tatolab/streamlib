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
use crate::core::RuntimeContext;

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
    runtime_ctx: RuntimeContext,
) {
    tracing::info!(
        "[{}] Thread started ({})",
        id,
        exec_config.execution.description()
    );

    match exec_config.execution {
        ProcessExecution::Continuous { interval_ms } => {
            run_continuous_mode(
                &id,
                &processor,
                &shutdown_rx,
                &pause_gate,
                interval_ms,
                &runtime_ctx,
            );
        }
        ProcessExecution::Reactive => {
            run_reactive_mode(
                &id,
                &processor,
                &shutdown_rx,
                &message_reader,
                &pause_gate,
                &runtime_ctx,
            );
        }
        ProcessExecution::Manual => {
            run_manual_mode(
                &id,
                &processor,
                &shutdown_rx,
                &message_reader,
                &pause_gate,
                &runtime_ctx,
            );
        }
    }

    // Teardown
    tracing::info!("[{}] Invoking teardown()...", id);
    {
        let mut guard = processor.lock();
        match runtime_ctx
            .tokio_handle()
            .block_on(guard.__generated_teardown())
        {
            Ok(()) => tracing::info!("[{}] teardown() completed successfully", id),
            Err(e) => tracing::warn!("[{}] teardown() failed: {}", id, e),
        }
    }

    *state.lock() = ProcessorState::Stopped;
    tracing::info!("[{}] Thread stopped", id);
}

fn run_continuous_mode(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    pause_gate: &Arc<AtomicBool>,
    interval_ms: u32,
    runtime_ctx: &RuntimeContext,
) {
    let sleep_duration = if interval_ms > 0 {
        std::time::Duration::from_millis(interval_ms as u64)
    } else {
        std::time::Duration::from_micros(100)
    };

    let mut was_paused = false;

    loop {
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[{}] Received shutdown signal", id);
            break;
        }

        let is_paused = pause_gate.load(Ordering::Acquire);

        if is_paused && !was_paused {
            tracing::info!("[{}] Invoking on_pause()...", id);
            let mut guard = processor.lock();
            match runtime_ctx
                .tokio_handle()
                .block_on(guard.__generated_on_pause())
            {
                Ok(()) => tracing::info!("[{}] on_pause() completed successfully", id),
                Err(e) => tracing::warn!("[{}] on_pause() failed: {}", id, e),
            }
            was_paused = true;
        } else if !is_paused && was_paused {
            tracing::info!("[{}] Invoking on_resume()...", id);
            let mut guard = processor.lock();
            match runtime_ctx
                .tokio_handle()
                .block_on(guard.__generated_on_resume())
            {
                Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
                Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
            }
            was_paused = false;
        }

        if is_paused {
            std::thread::sleep(PAUSE_CHECK_INTERVAL);
            continue;
        }

        {
            let mut guard = processor.lock();
            if let Err(e) = guard.process() {
                tracing::warn!("[{}] process() failed: {}", id, e);
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
    runtime_ctx: &RuntimeContext,
) {
    let mut was_paused = false;

    loop {
        crossbeam_channel::select! {
            recv(shutdown_rx) -> _ => {
                tracing::info!("[{}] Received shutdown signal", id);
                break;
            },
            recv(message_reader) -> msg => {
                if let Ok(message) = msg {
                    if message == LinkOutputToProcessorMessage::StopProcessingNow {
                        tracing::info!("[{}] Received StopProcessingNow message", id);
                        break;
                    }

                    let is_paused = pause_gate.load(Ordering::Acquire);

                    if is_paused && !was_paused {
                        tracing::info!("[{}] Invoking on_pause()...", id);
                        let mut guard = processor.lock();
                        match runtime_ctx.tokio_handle().block_on(guard.__generated_on_pause()) {
                            Ok(()) => tracing::info!("[{}] on_pause() completed successfully", id),
                            Err(e) => tracing::warn!("[{}] on_pause() failed: {}", id, e),
                        }
                        was_paused = true;
                    } else if !is_paused && was_paused {
                        tracing::info!("[{}] Invoking on_resume()...", id);
                        let mut guard = processor.lock();
                        match runtime_ctx.tokio_handle().block_on(guard.__generated_on_resume()) {
                            Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
                            Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
                        }
                        was_paused = false;
                    }

                    if is_paused {
                        continue;
                    }

                    let mut guard = processor.lock();
                    if let Err(e) = guard.process() {
                        tracing::warn!("[{}] process() failed: {}", id, e);
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
    pause_gate: &Arc<AtomicBool>,
    runtime_ctx: &RuntimeContext,
) {
    // Call start() - for callback-driven processors this returns immediately
    // after registering callbacks with OS (AVFoundation, CoreAudio, CVDisplayLink)
    tracing::info!("[{}] Invoking start()...", id);
    {
        let mut guard = processor.lock();
        match guard.start() {
            Ok(()) => tracing::info!("[{}] start() completed successfully", id),
            Err(e) => {
                tracing::warn!("[{}] start() failed: {}", id, e);
                return;
            }
        }
    }

    // Wait for shutdown signal - this thread is just a lifecycle manager
    // Real work happens on OS-managed callback threads
    let mut was_paused = false;

    loop {
        crossbeam_channel::select! {
            recv(shutdown_rx) -> _ => {
                tracing::info!("[{}] Received shutdown signal", id);
                break;
            },
            recv(message_reader) -> msg => {
                if let Ok(LinkOutputToProcessorMessage::StopProcessingNow) = msg {
                    tracing::info!("[{}] Received StopProcessingNow", id);
                    break;
                }
            }
            default(std::time::Duration::from_millis(100)) => {
                // Periodic check for pause/resume state changes
                let is_paused = pause_gate.load(Ordering::Acquire);

                if is_paused && !was_paused {
                    tracing::info!("[{}] Invoking on_pause()...", id);
                    let mut guard = processor.lock();
                    match runtime_ctx.tokio_handle().block_on(guard.__generated_on_pause()) {
                        Ok(()) => tracing::info!("[{}] on_pause() completed successfully", id),
                        Err(e) => tracing::warn!("[{}] on_pause() failed: {}", id, e),
                    }
                    was_paused = true;
                } else if !is_paused && was_paused {
                    tracing::info!("[{}] Invoking on_resume()...", id);
                    let mut guard = processor.lock();
                    match runtime_ctx.tokio_handle().block_on(guard.__generated_on_resume()) {
                        Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
                        Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
                    }
                    was_paused = false;
                }
            }
        }
    }

    // Call stop() - stops callbacks and waits for in-flight work
    tracing::info!("[{}] Invoking stop()...", id);
    {
        let mut guard = processor.lock();
        match guard.stop() {
            Ok(()) => tracing::info!("[{}] stop() completed successfully", id),
            Err(e) => tracing::warn!("[{}] stop() failed: {}", id, e),
        }
    }
}
