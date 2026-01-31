// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::compiler::compilation_plan::CompilationPlan;
use crate::core::compiler::compile_phase::CompilePhase;
use crate::core::compiler::compile_result::CompileResult;
use crate::core::compiler::compiler_transaction::CompilerTransactionHandle;
use crate::core::compiler::PendingOperation;
use crate::core::context::RuntimeContext;
use crate::core::error::{Result, StreamError};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, LinkStateComponent,
    ProcessorReadyBarrierHandle, ProcessorUniqueId,
};
use crate::core::processors::PROCESSOR_REGISTRY;
use crate::core::pubsub::{topics, Event, RuntimeEvent, PUBSUB};

/// Compiles graph changes into running processor state.
pub struct Compiler {
    // Graph ownership (moved from Runtime)
    graph: Arc<RwLock<Graph>>,
    // Transaction accumulates operations until commit
    transaction: Arc<Mutex<Vec<PendingOperation>>>,
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Compiler {
    /// Create a new compiler.
    pub fn new() -> Self {
        Self {
            graph: Arc::new(RwLock::new(Graph::new())),
            transaction: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // =========================================================================
    // Transaction API
    // =========================================================================

    /// Access graph and transaction for mutations. Callable from any thread.
    pub fn scope<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Graph, &CompilerTransactionHandle) -> R,
    {
        let mut graph = self.graph.write();
        let tx = CompilerTransactionHandle::new(Arc::clone(&self.transaction));
        f(&mut graph, &tx)
    }

    /// Flush transaction. Callable from any thread - compile() is dispatched to main thread.
    pub fn commit(&self, runtime_ctx: &Arc<RuntimeContext>) -> Result<()> {
        let operations = std::mem::take(&mut *self.transaction.lock());
        if operations.is_empty() {
            tracing::info!("[commit] No pending operations");
            return Ok(());
        }

        tracing::debug!(
            "[commit] Processing {} pending operations (batched)",
            operations.len()
        );

        // Compile directly - processors handle their own runtime thread needs
        // via RuntimeContext::run_on_runtime_thread_blocking() in their setup() if required.
        // This avoids forcing all compilation to runtime thread when most processors
        // don't need it (only Apple framework processors like Camera, Display).
        Self::compile(Arc::clone(&self.graph), operations, runtime_ctx)
    }

    // =========================================================================
    // Compilation - ONE method, ALL logic inlined
    // =========================================================================

    /// Single compile method - ALL orchestration logic here, no helper methods.
    /// Calls compiler_ops::* for actual operations.
    fn compile(
        graph_arc: Arc<RwLock<Graph>>,
        operations: Vec<PendingOperation>,
        runtime_ctx: &Arc<RuntimeContext>,
    ) -> Result<()> {
        use crate::core::graph::{
            PendingDeletionComponent, ProcessorInstanceComponent, ShutdownChannelComponent,
            StateComponent, SubprocessHandleComponent, ThreadHandleComponent,
        };
        use crate::core::processors::ProcessorState;

        let mut result = CompileResult::default();

        // =====================================================================
        // 1. Validate and categorize operations
        // =====================================================================
        let mut plan = CompilationPlan::default();

        for op in operations {
            match op {
                PendingOperation::AddProcessor(id) => {
                    let graph = graph_arc.read();
                    let exists = graph.traversal().v(&id).exists();
                    let running = graph
                        .traversal()
                        .v(&id)
                        .first()
                        .map(|n| n.has::<ProcessorInstanceComponent>())
                        .unwrap_or(false);
                    let pending_deletion = graph
                        .traversal()
                        .v(&id)
                        .first()
                        .map(|n| n.has::<PendingDeletionComponent>())
                        .unwrap_or(false);
                    drop(graph);

                    if pending_deletion {
                        tracing::debug!("AddProcessor({}): pending deletion, skipping add", id);
                    } else if exists && !running {
                        plan.processors_to_add.push(id);
                    } else if !exists {
                        tracing::warn!("AddProcessor({}): not in graph, skipping", id);
                    } else {
                        tracing::debug!("AddProcessor({}): already running, skipping", id);
                    }
                }
                PendingOperation::RemoveProcessor(id) => {
                    plan.processors_to_remove.push(id);
                }
                PendingOperation::AddLink(id) => {
                    let graph = graph_arc.read();
                    let link = graph.traversal().e(&id).first();
                    let exists = link.is_some();
                    let wired = link
                        .and_then(|l| l.get::<LinkStateComponent>())
                        .map(|s| matches!(s.0, crate::core::graph::LinkState::Wired))
                        .unwrap_or(false);
                    let pending_deletion = link
                        .map(|l| l.has::<PendingDeletionComponent>())
                        .unwrap_or(false);
                    drop(graph);

                    if pending_deletion {
                        tracing::debug!("AddLink({}): pending deletion, skipping add", id);
                    } else if exists && !wired {
                        plan.links_to_add.push(id);
                    } else if !exists {
                        tracing::warn!("AddLink({}): not in graph, skipping", id);
                    } else {
                        tracing::debug!("AddLink({}): already wired, skipping", id);
                    }
                }
                PendingOperation::RemoveLink(id) => {
                    plan.links_to_remove.push(id);
                }
                PendingOperation::UpdateProcessorConfig(id) => {
                    plan.config_updates.push(id);
                }
            }
        }

        // Early return if nothing to do
        if plan.is_empty() {
            tracing::debug!("No changes to compile");
            return Ok(());
        }

        tracing::info!(
            "Compiling: +{} -{} processors, +{} -{} links, {} config updates",
            plan.processors_to_add.len(),
            plan.processors_to_remove.len(),
            plan.links_to_add.len(),
            plan.links_to_remove.len(),
            plan.config_updates.len(),
        );

        // Publish compile start event
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::CompilerWillCompile),
        );

        // =====================================================================
        // 2. Handle removals FIRST (before adding new processors)
        // =====================================================================
        if !plan.links_to_remove.is_empty() || !plan.processors_to_remove.is_empty() {
            tracing::debug!(
                "[commit] Removing {} processors, {} links",
                plan.processors_to_remove.len(),
                plan.links_to_remove.len()
            );

            // Unwire links first (before removing processors)
            for link_id in &plan.links_to_remove {
                let mut graph = graph_arc.write();
                if let Some(link) = graph
                    .traversal()
                    .e(())
                    .filter(|link| link.id == *link_id)
                    .first()
                {
                    let from_port = link.from_port().to_string();
                    let to_port = link.to_port().to_string();

                    PUBSUB.publish(
                        topics::RUNTIME_GLOBAL,
                        &Event::RuntimeGlobal(RuntimeEvent::CompilerWillUnwireLink {
                            link_id: link_id.to_string(),
                            from_port: from_port.clone(),
                            to_port: to_port.clone(),
                        }),
                    );

                    tracing::info!("[CLOSE SERVICE] {}", link_id);
                    if let Err(e) = super::compiler_ops::close_iceoryx2_service(&mut graph, link_id)
                    {
                        tracing::warn!("Failed to close service {}: {}", link_id, e);
                    }

                    PUBSUB.publish(
                        topics::RUNTIME_GLOBAL,
                        &Event::RuntimeGlobal(RuntimeEvent::CompilerDidUnwireLink {
                            link_id: link_id.to_string(),
                            from_port,
                            to_port,
                        }),
                    );

                    result.links_unwired += 1;
                }
                drop(graph);

                // Clean up graph after unwiring
                let mut graph = graph_arc.write();
                if graph.traversal_mut().e(link_id).drop().exists() {
                    return Err(StreamError::GraphError("value was not dropped".into()));
                }
            }

            // Shutdown and remove processors
            for proc_id in &plan.processors_to_remove {
                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::CompilerWillDestroyProcessor {
                        processor_id: proc_id.clone(),
                    }),
                );

                tracing::info!("[REMOVE] {}", proc_id);

                // Phase 1: Signal shutdown, extract thread/subprocess handles (with lock)
                // Lock is released before join() to avoid deadlock - processors may be
                // waiting on runtime operations that need this lock to complete.
                let (thread_handle, subprocess_handle) = {
                    let mut graph = graph_arc.write();
                    if let Some(node) = graph.traversal_mut().v(proc_id).first_mut() {
                        // Set state to stopping
                        if let Some(state) = node.get::<StateComponent>() {
                            *state.0.lock() = ProcessorState::Stopping;
                        }
                        // Send shutdown signal
                        if let Some(channel) = node.get::<ShutdownChannelComponent>() {
                            let _ = channel.sender.send(());
                        }
                        // Extract thread and subprocess handles
                        let th = node.remove::<ThreadHandleComponent>();
                        let sh = node.remove::<SubprocessHandleComponent>();
                        (th, sh)
                    } else {
                        (None, None)
                    }
                }; // Lock released here

                // Phase 2: Wait for thread/subprocess to exit (no lock held)
                // Processor can now complete any pending runtime operations before exiting.
                if let Some(handle) = thread_handle {
                    match handle.0.join() {
                        Ok(_) => {
                            tracing::info!("[{}] Processor thread joined successfully", proc_id);
                        }
                        Err(panic_err) => {
                            tracing::error!(
                                "[{}] Processor thread panicked: {:?}",
                                proc_id,
                                panic_err
                            );
                        }
                    }
                }

                // Subprocess shutdown: SIGTERM → wait with timeout → SIGKILL
                if let Some(mut handle) = subprocess_handle {
                    let child_pid = handle.child.id();
                    tracing::info!(
                        "[{}] Shutting down Python subprocess (pid={})",
                        proc_id,
                        child_pid
                    );

                    // Send SIGTERM for graceful shutdown (subprocess_runner has signal handler)
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(child_pid as i32, libc::SIGTERM);
                    }

                    // Wait with timeout (5s)
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    loop {
                        match handle.child.try_wait() {
                            Ok(Some(status)) => {
                                tracing::info!(
                                    "[{}] Python subprocess exited: {}",
                                    proc_id,
                                    status
                                );
                                break;
                            }
                            Ok(None) if std::time::Instant::now() < deadline => {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                            _ => {
                                tracing::warn!(
                                    "[{}] Python subprocess did not exit gracefully, killing (pid={})",
                                    proc_id,
                                    child_pid
                                );
                                let _ = handle.child.kill();
                                let _ = handle.child.wait();
                                break;
                            }
                        }
                    }

                    // Clean up config temp file
                    let _ = std::fs::remove_file(&handle.config_path);
                }

                // Phase 3: Cleanup (re-acquire lock)
                {
                    let mut graph = graph_arc.write();
                    if let Some(node) = graph.traversal_mut().v(proc_id).first_mut() {
                        if let Some(state) = node.get::<StateComponent>() {
                            *state.0.lock() = ProcessorState::Stopped;
                        }
                    }
                    // Remove from graph
                    if graph.traversal_mut().v(proc_id).drop().exists() {
                        return Err(StreamError::GraphError("value was not dropped".into()));
                    }
                }

                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::CompilerDidDestroyProcessor {
                        processor_id: proc_id.clone(),
                    }),
                );

                result.processors_removed += 1;
            }
        }

        // =====================================================================
        // 3. Phase 1: PREPARE - Attach infrastructure components
        // =====================================================================
        let mut barrier_handles: Vec<(ProcessorUniqueId, ProcessorReadyBarrierHandle)> = vec![];

        if !plan.processors_to_add.is_empty() {
            tracing::debug!("[{}] Starting", CompilePhase::Prepare);
            for proc_id in &plan.processors_to_add {
                let mut graph = graph_arc.write();
                let node = graph.traversal().v(proc_id).first().ok_or_else(|| {
                    StreamError::ProcessorNotFound(format!("Processor '{}' not found", proc_id))
                })?;

                let processor_type = node.processor_type.clone();

                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::CompilerWillCreateProcessor {
                        processor_id: proc_id.clone(),
                        processor_type: processor_type.clone(),
                    }),
                );

                tracing::info!("[{}] Preparing {}", CompilePhase::Prepare, proc_id);

                let barrier_handle = super::compiler_ops::prepare_processor(&mut graph, proc_id)?;
                barrier_handles.push((proc_id.clone(), barrier_handle));

                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::CompilerDidCreateProcessor {
                        processor_id: proc_id.clone(),
                        processor_type,
                    }),
                );

                result.processors_created += 1;
            }
            tracing::debug!("[{}] Completed", CompilePhase::Prepare);
        }

        // =====================================================================
        // 4. Phase 2: SPAWN - Spawn processor threads
        // =====================================================================
        if !plan.processors_to_add.is_empty() {
            tracing::debug!("[{}] Starting", CompilePhase::Spawn);
            for proc_id in &plan.processors_to_add {
                tracing::info!("[{}] Spawning {}", CompilePhase::Spawn, proc_id);
                super::compiler_ops::spawn_processor(
                    Arc::clone(&graph_arc),
                    &PROCESSOR_REGISTRY,
                    runtime_ctx,
                    proc_id,
                )?;
            }
            tracing::debug!("[{}] Completed", CompilePhase::Spawn);
        }

        // =====================================================================
        // 5. Wait for all processors to signal READY (instances attached)
        // =====================================================================
        for (proc_id, handle) in &barrier_handles {
            tracing::trace!("[{}] Waiting for READY signal", proc_id);
            if handle.ready_receiver.recv().is_err() {
                tracing::warn!("[{}] Processor failed during instance creation", proc_id);
            }
        }

        // =====================================================================
        // 6. Phase 3: WIRE - Create ring buffers and connect ports
        // =====================================================================
        if !plan.links_to_add.is_empty() {
            tracing::debug!("[{}] Starting", CompilePhase::Wire);
            for link_id in &plan.links_to_add {
                let mut graph = graph_arc.write();
                let (from_port, to_port) = {
                    let link = graph.traversal().e(link_id).first().ok_or_else(|| {
                        StreamError::LinkNotFound(format!("Link '{}' not found", link_id))
                    })?;
                    (link.from_port().to_string(), link.to_port().to_string())
                };

                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::CompilerWillWireLink {
                        link_id: link_id.to_string(),
                        from_port: from_port.clone(),
                        to_port: to_port.clone(),
                    }),
                );

                tracing::info!("[{}] Opening service {}", CompilePhase::Wire, link_id);

                super::compiler_ops::open_iceoryx2_service(&mut graph, link_id, runtime_ctx)?;

                PUBSUB.publish(
                    topics::RUNTIME_GLOBAL,
                    &Event::RuntimeGlobal(RuntimeEvent::CompilerDidWireLink {
                        link_id: link_id.to_string(),
                        from_port,
                        to_port,
                    }),
                );

                result.links_wired += 1;
            }
            tracing::debug!("[{}] Completed", CompilePhase::Wire);
        }

        // =====================================================================
        // 7. Signal all processors to CONTINUE (wiring complete, run setup)
        // =====================================================================
        for (proc_id, handle) in barrier_handles {
            tracing::trace!("[{}] Signaling CONTINUE", proc_id);
            if handle.continue_sender.send(()).is_err() {
                tracing::warn!("[{}] Failed to signal CONTINUE", proc_id);
            }
        }

        // =====================================================================
        // 8. Config updates - for each config_update
        // =====================================================================
        for proc_id in plan.config_updates {
            let graph = graph_arc.read();
            let config_json = match graph.traversal().v(&proc_id).first() {
                Some(node) => match &node.config {
                    Some(config) => config.clone(),
                    None => {
                        tracing::debug!("[CONFIG] {} has no config to update", proc_id);
                        continue;
                    }
                },
                None => {
                    tracing::warn!("[CONFIG] Processor {} not found in graph", proc_id);
                    continue;
                }
            };

            let processor_arc = graph
                .traversal()
                .v(&proc_id)
                .first()
                .and_then(|node| {
                    node.get::<ProcessorInstanceComponent>()
                        .map(|i| i.0.clone())
                })
                .ok_or_else(|| {
                    StreamError::ProcessorNotFound(format!(
                        "Processor '{}' not found for config update",
                        proc_id
                    ))
                })?;
            drop(graph);

            {
                let mut guard = processor_arc.lock();
                guard.apply_config_json(&config_json)?;
            }

            tracing::info!("[CONFIG] Updated config for {}", proc_id);
            result.configs_updated += 1;
        }

        // Mark the graph as compiled
        graph_arc.write().mark_compiled();

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::CompilerDidCompile),
        );
        tracing::info!("Compile complete: {}", result);

        Ok(())
    }
}
