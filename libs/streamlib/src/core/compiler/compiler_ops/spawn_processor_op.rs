// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::compiler::scheduling::{scheduling_strategy_for_processor, SchedulingStrategy};
use crate::core::context::RuntimeContext;
use crate::core::error::{Result, StreamError};
use crate::core::execution::run_processor_loop;
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorInstanceComponent, ProcessorPauseGateComponent,
    ProcessorReadyBarrierComponent, ProcessorUniqueId, ShutdownChannelComponent, StateComponent,
    ThreadHandleComponent,
};
use crate::core::processors::{ProcessorInstanceFactory, ProcessorState};

/// Spawn a processor thread.
///
/// The thread will:
/// 1. Create processor instance via factory
/// 2. Attach ProcessorInstanceComponent to graph
/// 3. Signal READY via barrier
/// 4. Wait for CONTINUE via barrier (wiring happens here)
/// 5. Call setup (no locks held - safe to call runtime ops)
/// 6. Run processor loop
pub(crate) fn spawn_processor(
    graph_arc: Arc<RwLock<Graph>>,
    factory: &ProcessorInstanceFactory,
    runtime_ctx: &Arc<RuntimeContext>,
    processor_id: impl AsRef<str>,
) -> Result<()> {
    let processor_id = processor_id.as_ref();

    // Check if already has a thread (already running)
    {
        let graph = graph_arc.read();
        let has_thread = graph
            .traversal()
            .v(processor_id)
            .first()
            .map(|n| n.has::<ThreadHandleComponent>())
            .unwrap_or(false);
        if has_thread {
            return Ok(());
        }
    }

    // Extract scheduling strategy and barrier before spawning
    let (strategy, barrier_component, proc_id_clone) = {
        let mut graph = graph_arc.write();
        let node = graph.traversal().v(processor_id).first().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;

        let strategy = scheduling_strategy_for_processor(node);

        // Extract barrier component from node
        let node_mut = graph
            .traversal_mut()
            .v(processor_id)
            .first_mut()
            .ok_or_else(|| {
                StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
            })?;

        let barrier = node_mut
            .remove::<ProcessorReadyBarrierComponent>()
            .ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Processor '{}' has no ProcessorReadyBarrierComponent",
                    processor_id
                ))
            })?;

        (strategy, barrier, ProcessorUniqueId::from(processor_id))
    };

    tracing::info!(
        "[{}] Spawning with strategy: {}",
        processor_id,
        strategy.description()
    );

    match strategy {
        SchedulingStrategy::DedicatedThread { priority, name: _ } => {
            spawn_dedicated_thread(
                graph_arc,
                factory,
                runtime_ctx,
                proc_id_clone,
                priority,
                barrier_component,
            )?;
        }
    }

    Ok(())
}

fn spawn_dedicated_thread(
    graph_arc: Arc<RwLock<Graph>>,
    factory: &ProcessorInstanceFactory,
    runtime_ctx: &Arc<RuntimeContext>,
    processor_id: ProcessorUniqueId,
    priority: crate::core::execution::ThreadPriority,
    mut barrier: ProcessorReadyBarrierComponent,
) -> Result<()> {
    // Clone Arcs for thread
    let graph_arc_clone = Arc::clone(&graph_arc);
    let runtime_ctx_clone = Arc::clone(runtime_ctx);
    let proc_id_clone = processor_id.clone();

    // Create processor instance now (with lock) since factory needs node reference
    let processor_arc = {
        let graph = graph_arc.read();
        let node = graph.traversal().v(&processor_id).first().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;
        let processor = factory.create(node)?;
        Arc::new(Mutex::new(processor))
    };

    let processor_arc_clone = Arc::clone(&processor_arc);

    let thread_name = format!("processor-{}", processor_id);

    let thread = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            let current_thread = std::thread::current();
            let thread_name = current_thread.name().unwrap_or("unnamed");
            let thread_id = current_thread.id();

            tracing::info!(
                "[{}] Thread started: name='{}', id={:?}",
                proc_id_clone,
                thread_name,
                thread_id
            );

            // Apply thread priority (platform-specific)
            // Skip for Manual mode - real work runs on OS-managed callback threads
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                let is_manual = processor_arc_clone
                    .lock()
                    .execution_config()
                    .execution
                    .is_manual();
                if is_manual {
                    tracing::info!(
                        "[{}] Manual mode: skipping thread priority (callbacks use OS threads)",
                        proc_id_clone
                    );
                } else if let Err(e) =
                    crate::apple::thread_priority::apply_thread_priority(priority)
                {
                    tracing::warn!(
                        "[{}] Failed to apply {:?} thread priority: {}",
                        proc_id_clone,
                        priority,
                        e
                    );
                }
            }

            // === PHASE 1: Attach instance to graph ===
            tracing::trace!(
                "[{}] Attaching ProcessorInstanceComponent to graph",
                proc_id_clone
            );
            {
                let mut graph = graph_arc_clone.write();
                if let Some(node) = graph.traversal_mut().v(&proc_id_clone).first_mut() {
                    node.insert(ProcessorInstanceComponent(processor_arc_clone.clone()));
                }
            }
            tracing::trace!("[{}] ProcessorInstanceComponent attached", proc_id_clone);

            // === PHASE 2: Signal ready, wait for wiring ===
            tracing::trace!(
                "[{}] Signaling READY to compiler (instance attached)",
                proc_id_clone
            );
            barrier.signal_ready();
            tracing::trace!(
                "[{}] Waiting for CONTINUE signal (wiring in progress)...",
                proc_id_clone
            );
            barrier.wait_for_continue();
            tracing::trace!(
                "[{}] Received CONTINUE signal (wiring complete)",
                proc_id_clone
            );

            // === PHASE 3: Extract components for setup and loop ===
            let (state_arc, shutdown_rx, pause_gate_inner, exec_config) = {
                let mut graph = graph_arc_clone.write();
                let node = match graph.traversal_mut().v(&proc_id_clone).first_mut() {
                    Some(n) => n,
                    None => {
                        tracing::error!("[{}] Node not found after wiring", proc_id_clone);
                        return;
                    }
                };

                let state = match node.get::<StateComponent>() {
                    Some(s) => s.0.clone(),
                    None => {
                        tracing::error!("[{}] No StateComponent", proc_id_clone);
                        return;
                    }
                };

                let shutdown_rx = match node.get_mut::<ShutdownChannelComponent>() {
                    Some(channel) => match channel.take_receiver() {
                        Some(rx) => rx,
                        None => {
                            tracing::error!("[{}] Shutdown receiver already taken", proc_id_clone);
                            return;
                        }
                    },
                    None => {
                        tracing::error!("[{}] No ShutdownChannelComponent", proc_id_clone);
                        return;
                    }
                };

                let pause_gate_inner = match node.get::<ProcessorPauseGateComponent>() {
                    Some(pg) => pg.clone_inner(),
                    None => {
                        tracing::error!("[{}] No ProcessorPauseGateComponent", proc_id_clone);
                        return;
                    }
                };

                let exec_config = processor_arc_clone.lock().execution_config();

                (state, shutdown_rx, pause_gate_inner, exec_config)
            }; // Lock released here

            // === PHASE 4: Setup (NO LOCK HELD - safe to call runtime ops) ===
            // Create processor-specific context with both processor ID and pause gate
            let processor_context = runtime_ctx_clone
                .with_processor_id(proc_id_clone.clone())
                .with_pause_gate(pause_gate_inner.clone());
            {
                let tokio_handle = runtime_ctx_clone.tokio_handle();

                tracing::info!(
                    "[{}] Calling setup on thread '{}' (id={:?}) - no locks held",
                    proc_id_clone,
                    thread_name,
                    thread_id
                );
                let mut guard = processor_arc_clone.lock();
                if let Err(e) =
                    tokio_handle.block_on(guard.__generated_setup(processor_context.clone()))
                {
                    tracing::error!("[{}] Setup failed: {}", proc_id_clone, e);
                    *state_arc.lock() = ProcessorState::Error;
                    return;
                }
                tracing::info!("[{}] Setup completed successfully", proc_id_clone);
            }

            // Update state to Running
            *state_arc.lock() = ProcessorState::Running;

            // === PHASE 5: Process loop ===
            tracing::trace!(
                "[{}] Entering process loop on thread '{}' (id={:?})",
                proc_id_clone,
                thread_name,
                thread_id
            );
            run_processor_loop(
                proc_id_clone,
                processor_arc_clone,
                shutdown_rx,
                state_arc,
                pause_gate_inner,
                exec_config,
                processor_context,
            );
        })
        .map_err(|e| StreamError::Runtime(format!("Failed to spawn thread: {}", e)))?;

    // Attach thread handle
    {
        let mut graph = graph_arc.write();
        let node = graph
            .traversal_mut()
            .v(&processor_id)
            .first_mut()
            .ok_or_else(|| {
                StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
            })?;
        node.insert(ThreadHandleComponent(thread));
    }

    Ok(())
}
