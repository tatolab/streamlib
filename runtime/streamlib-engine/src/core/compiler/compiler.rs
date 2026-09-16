// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::compiler::PendingOperation;
use crate::core::compiler::compilation_plan::CompilationPlan;
use crate::core::compiler::compile_phase::CompilePhase;
use crate::core::compiler::compile_result::CompileResult;
use crate::core::compiler::compiler_transaction::CompilerTransactionHandle;
use crate::core::compiler::processor_thread_shutdown::{
    AbandonedProcessorThreadStillRunning, DescriptionOfTheAbandonedProcessorThreads,
    ProcessorDisplayNameAndId, ProcessorThreadJoinBudgets,
    remove_processors_signalling_every_thread_before_joining_any,
};
use crate::core::context::RuntimeContext;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, Link, LinkState, LinkStateComponent,
    OutOfProcessLinkWireRepliesComponent, ProcessorReadyBarrierHandle, ProcessorUniqueId,
};
use crate::core::processors::PROCESSOR_REGISTRY;
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};

/// Compiles graph changes into running processor state.
pub struct Compiler {
    // Graph ownership (moved from Runtime)
    graph: Arc<RwLock<Graph>>,
    // Transaction accumulates operations until commit
    transaction: Arc<Mutex<Vec<PendingOperation>>>,
    /// Held for the whole of a commit: two batches compiling at once would
    /// interleave their spawn and wire phases against one graph.
    one_commit_at_a_time: Mutex<()>,
    /// Processor threads a removal abandoned. Each holds the engine alive
    /// beneath it until it returns, so teardown asks which still run.
    abandoned_processor_threads: Mutex<Vec<AbandonedProcessorThreadStillRunning>>,
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
            one_commit_at_a_time: Mutex::new(()),
            abandoned_processor_threads: Mutex::new(Vec::new()),
        }
    }

    /// The processors whose threads a removal abandoned and that have not
    /// returned since.
    pub fn processor_threads_abandoned_and_still_running(&self) -> Vec<ProcessorDisplayNameAndId> {
        let mut abandoned = self.abandoned_processor_threads.lock();
        abandoned.retain(|thread| !thread.join_handle.is_finished());
        abandoned
            .iter()
            .map(|thread| thread.processor.clone())
            .collect()
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

    /// The operations logged and not yet committed, in the order they were
    /// logged.
    #[cfg(test)]
    pub(crate) fn logged_pending_operations(&self) -> Vec<PendingOperation> {
        self.transaction.lock().clone()
    }

    /// Flush transaction. Callable from any thread - compile() is dispatched to main thread.
    #[tracing::instrument(name = "compiler.commit", skip_all)]
    pub fn commit(&self, runtime_ctx: &Arc<RuntimeContext>) -> Result<()> {
        let _one_commit_at_a_time = self.one_commit_at_a_time.lock();
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
        Self::compile(
            Arc::clone(&self.graph),
            operations,
            runtime_ctx,
            &self.abandoned_processor_threads,
        )
    }

    // =========================================================================
    // Compilation - ONE method, ALL logic inlined
    // =========================================================================

    /// Single compile method - ALL orchestration logic here, no helper methods.
    /// Calls compiler_ops::* for actual operations.
    ///
    /// A removal whose thread outlived its budget still removes the processor
    /// and lets the rest of the batch compile; the call then fails naming it.
    fn compile(
        graph_arc: Arc<RwLock<Graph>>,
        operations: Vec<PendingOperation>,
        runtime_ctx: &Arc<RuntimeContext>,
        abandoned_processor_threads: &Mutex<Vec<AbandonedProcessorThreadStillRunning>>,
    ) -> Result<()> {
        use crate::core::graph::{PendingDeletionComponent, ProcessorInstanceComponent};

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
                    let already_wired_or_awaiting_an_answer =
                        link.is_some_and(this_link_is_already_wired_or_awaiting_its_helpers_answer);
                    let pending_deletion = link
                        .map(|l| l.has::<PendingDeletionComponent>())
                        .unwrap_or(false);
                    drop(graph);

                    if pending_deletion {
                        tracing::debug!("AddLink({}): pending deletion, skipping add", id);
                    } else if exists && !already_wired_or_awaiting_an_answer {
                        plan.links_to_add.push(id);
                    } else if !exists {
                        tracing::warn!("AddLink({}): not in graph, skipping", id);
                    } else {
                        tracing::debug!(
                            "AddLink({}): already wired or awaiting its helper's answer, skipping",
                            id
                        );
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

        // A processor removal cascades its incident links out of the graph
        // (petgraph `remove_node`), so a link queued for wiring against a
        // processor this same batch removes would vanish before the WIRE
        // phase and surface a spurious LinkNotFound. Drop those doomed
        // link-adds now, before any phase runs.
        {
            let graph = graph_arc.read();
            plan.drop_link_adds_into_removed_processors(&graph);
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
        let abandoned_in_this_compile: Vec<ProcessorDisplayNameAndId> =
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
                        if let Err(e) =
                            super::compiler_ops::close_iceoryx2_service(&mut graph, link_id)
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
                        return Err(Error::GraphError("value was not dropped".into()));
                    }
                }

                let abandoned_by_this_removal =
                    remove_processors_signalling_every_thread_before_joining_any(
                        &graph_arc,
                        &plan.processors_to_remove,
                        ProcessorThreadJoinBudgets::ENGINE_CHOSEN,
                        crate::core::runtime::is_runtime_shutdown_forced,
                        abandoned_processor_threads,
                    )?;
                result.processors_removed += plan.processors_to_remove.len();
                abandoned_by_this_removal
            } else {
                Vec::new()
            };

        // =====================================================================
        // 3. Phase 1: PREPARE - Attach infrastructure components
        // =====================================================================
        let mut barrier_handles: Vec<(ProcessorUniqueId, ProcessorReadyBarrierHandle)> = vec![];

        if !plan.processors_to_add.is_empty() {
            tracing::debug!("[{}] Starting", CompilePhase::Prepare);
            for proc_id in &plan.processors_to_add {
                let mut graph = graph_arc.write();
                let node = graph.traversal().v(proc_id).first().ok_or_else(|| {
                    Error::ProcessorNotFound(format!("Processor '{}' not found", proc_id))
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
                        Error::LinkNotFound(format!("Link '{}' not found", link_id))
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

                super::compiler_ops::open_iceoryx2_service(
                    &mut graph,
                    link_id,
                    runtime_ctx.iceoryx2_node(),
                )?;

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
                    Error::ProcessorNotFound(format!(
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

        if !abandoned_in_this_compile.is_empty() {
            return Err(Error::Runtime(
                DescriptionOfTheAbandonedProcessorThreads(&abandoned_in_this_compile).to_string(),
            ));
        }
        Ok(())
    }
}

/// Whether an `AddLink` names a link this graph has already wired — or handed
/// to a helper that has not answered yet — so the plan passes it over rather
/// than wiring it a second time.
///
/// The second arm is not wired yet and says so in `graph`: it reads `Pending`
/// until the helper says it opened its port. Re-planning it as unadded would
/// open a second subscriber and notifier against the channel's caps for a link
/// the helper is already opening, which is why it counts as added here and
/// nowhere else.
fn this_link_is_already_wired_or_awaiting_its_helpers_answer(link: &Link) -> bool {
    link.get::<LinkStateComponent>()
        .is_some_and(|state| matches!(state.0, LinkState::Wired))
        || link.has::<OutOfProcessLinkWireRepliesComponent>()
}

#[cfg(test)]
mod already_wired_tests {
    use super::*;
    use crate::core::processors::OutOfProcessLinkWireReply;

    #[test]
    fn a_link_nothing_has_wired_yet_is_planned() {
        let link = Link::new("Psrc.out1", "Pdst.in1");
        assert!(!this_link_is_already_wired_or_awaiting_its_helpers_answer(
            &link
        ));
    }

    #[test]
    fn a_wired_link_is_not_planned_again() {
        let mut link = Link::new("Psrc.out1", "Pdst.in1");
        link.insert(LinkStateComponent(LinkState::Wired));
        assert!(this_link_is_already_wired_or_awaiting_its_helpers_answer(
            &link
        ));
    }

    /// Fail-without-fix: read `LinkStateComponent` alone and this link — the
    /// ordinary state of a live `connect` onto a running helper — is planned
    /// again on the next compile, wiring one link twice.
    #[test]
    fn a_link_a_helper_has_not_answered_for_is_not_planned_again() {
        let mut link = Link::new("Psrc.out1", "Pdst.in1");
        link.insert(LinkStateComponent(LinkState::Pending));
        link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
            OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
        ]));
        assert!(this_link_is_already_wired_or_awaiting_its_helpers_answer(
            &link
        ));
    }
}
