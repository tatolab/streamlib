// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};

use crate::core::compiler::PendingOperation;
use crate::core::compiler::compilation_plan::CompilationPlan;
use crate::core::compiler::compile_phase::CompilePhase;
use crate::core::compiler::compile_result::CompileResult;
use crate::core::compiler::compiler_ops::ProcessorConfigUpdateOutcome;
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
    LinkUniqueId, OutOfProcessLinkWireRepliesComponent, ProcessorReadyBarrierHandle,
    ProcessorUniqueId,
};
use crate::core::processors::NodeTypesOneStreamResolves;
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};
use crate::core::runtime::{LinksReadingAStreamToRePlan, LoadedStreamInThisRuntime};
use crate::iceoryx2::Iceoryx2Node;

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
    /// Counted after each compile lets go of the graph and before it wakes
    /// the links in other streams waiting on it.
    compiles_of_this_graph_finished: AtomicU64,
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Compiler {
    /// Create a new compiler whose graph resolves the natively compiled node types alone.
    pub fn new() -> Self {
        Self::new_resolving_node_types_through(Arc::new(NodeTypesOneStreamResolves::new()))
    }

    /// Create a new compiler whose graph resolves node types through its stream's lookup.
    pub fn new_resolving_node_types_through(
        node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
    ) -> Self {
        Self {
            graph: Arc::new(RwLock::new(Graph::new_resolving_node_types_through(
                node_types_this_stream_resolves,
            ))),
            transaction: Arc::new(Mutex::new(Vec::new())),
            one_commit_at_a_time: Mutex::new(()),
            abandoned_processor_threads: Mutex::new(Vec::new()),
            compiles_of_this_graph_finished: AtomicU64::new(0),
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

    /// How many compiles of this graph have finished.
    pub(crate) fn how_many_compiles_of_this_graph_finished(&self) -> u64 {
        self.compiles_of_this_graph_finished.load(Ordering::SeqCst)
    }

    /// Count a compile of this graph as finished, as a compile does before it
    /// wakes the links waiting on it.
    #[cfg(test)]
    pub(crate) fn count_a_compile_of_this_graph_as_finished_for_a_test(&self) {
        self.compiles_of_this_graph_finished
            .fetch_add(1, Ordering::SeqCst);
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

    /// Access graph and transaction for mutations, waiting at most `bound` for
    /// the graph's lock; `None` when it was not free within it.
    pub(crate) fn scope_within<F, R>(&self, bound: std::time::Duration, f: F) -> Option<R>
    where
        F: FnOnce(&mut Graph, &CompilerTransactionHandle) -> R,
    {
        let mut graph = self.graph.try_write_for(bound)?;
        let tx = CompilerTransactionHandle::new(Arc::clone(&self.transaction));
        Some(f(&mut graph, &tx))
    }

    /// Run a commit's WIRE phase over `links_to_add` of this compiler's graph
    /// as `reading_stream`, without the processors a start leaves running;
    /// how many it wired.
    #[cfg(test)]
    pub(crate) fn wire_every_link_a_plan_adds_for_a_test(
        &self,
        links_to_add: &[LinkUniqueId],
        reading_stream: &Arc<LoadedStreamInThisRuntime>,
    ) -> Result<usize> {
        wire_every_link_a_plan_adds(
            &self.graph,
            links_to_add,
            Some(reading_stream),
            &reading_stream
                .engine_resources_shared_by_every_stream()
                .iceoryx2_node,
            reading_stream.loaded_stream_identity(),
        )
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

        Self::compile(
            Arc::clone(&self.graph),
            operations,
            runtime_ctx,
            &self.abandoned_processor_threads,
            &self.compiles_of_this_graph_finished,
        )
    }

    /// Compile `operations` against `graph_arc`, then wake the links in other
    /// streams that read a processor it created, or else those whose last
    /// attempt found this stream's graph not free within the reconcile's bound.
    ///
    /// A removal whose thread outlived its budget still removes the processor
    /// and lets the rest of the batch compile; the call then fails naming it.
    fn compile(
        graph_arc: Arc<RwLock<Graph>>,
        operations: Vec<PendingOperation>,
        runtime_ctx: &Arc<RuntimeContext>,
        abandoned_processor_threads: &Mutex<Vec<AbandonedProcessorThreadStillRunning>>,
        compiles_of_this_graph_finished: &AtomicU64,
    ) -> Result<()> {
        let mut result = CompileResult::default();
        let compiled = Self::compile_counting_into(
            graph_arc,
            operations,
            runtime_ctx,
            abandoned_processor_threads,
            &mut result,
        );
        compiles_of_this_graph_finished.fetch_add(1, Ordering::SeqCst);
        // Processors this compile created may be the sources links in other
        // streams wait on, and they run whether or not the rest of it failed.
        if let Some(this_stream) = runtime_ctx.this_stream() {
            let which = if result.processors_created > 0 {
                LinksReadingAStreamToRePlan::EveryOneStillToBeWired
            } else {
                LinksReadingAStreamToRePlan::OnlyThoseItsGraphDidNotAnswer
            };
            this_stream
                .engine_resources_shared_by_every_stream()
                .spawn_a_re_plan_of_the_links_reading_the_stream_on_every_other_stream(
                    this_stream.stream_name(),
                    which,
                );
            crate::core::pubsub::OutputPortExposureChange::StreamCompiled {
                stream_name: this_stream.stream_name().to_string(),
            }
            .publish();
        }
        compiled
    }

    /// Compile `operations` against `graph_arc`, counting what it did into
    /// `result` as it goes, so a compile that fails part-way still says what
    /// it created.
    fn compile_counting_into(
        graph_arc: Arc<RwLock<Graph>>,
        operations: Vec<PendingOperation>,
        runtime_ctx: &Arc<RuntimeContext>,
        abandoned_processor_threads: &Mutex<Vec<AbandonedProcessorThreadStillRunning>>,
        result: &mut CompileResult,
    ) -> Result<()> {
        let the_stream_the_compile_belongs_to = runtime_ctx.loaded_stream_identity();
        // =====================================================================
        // 1. Validate and categorize operations
        // =====================================================================
        let mut plan = the_plan_these_operations_make(&graph_arc, operations);

        // A processor removal cascades its incident links out of the graph
        // (petgraph `remove_node`), so a link queued for wiring against a
        // processor this same batch removes would vanish before the WIRE
        // phase and surface a spurious LinkNotFound. Drop those doomed
        // link-adds now, before any phase runs.
        {
            let graph = graph_arc.read();
            plan.drop_link_adds_into_removed_processors(&graph);
        }
        plan.hand_config_updates_to_processors_this_batch_constructs(&mut graph_arc.write());

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
        the_stream_the_compile_belongs_to
            .publish_on_this_streams_topic(RuntimeEvent::CompilerWillCompile);

        // =====================================================================
        // 2. Handle removals FIRST (before adding new processors)
        // =====================================================================
        let abandoned_in_this_compile: Vec<ProcessorDisplayNameAndId> = if !plan
            .links_to_remove
            .is_empty()
            || !plan.processors_to_remove.is_empty()
        {
            tracing::debug!(
                "[commit] Removing {} processors, {} links",
                plan.processors_to_remove.len(),
                plan.links_to_remove.len()
            );

            // Unwire links first (before removing processors)
            for link_id in &plan.links_to_remove {
                if unwire_and_remove_a_link(&graph_arc, link_id, the_stream_the_compile_belongs_to)?
                {
                    result.links_unwired += 1;
                }
            }

            let abandoned_by_this_removal =
                remove_processors_signalling_every_thread_before_joining_any(
                    &graph_arc,
                    the_stream_the_compile_belongs_to,
                    &plan.processors_to_remove,
                    ProcessorThreadJoinBudgets::ENGINE_CHOSEN,
                    || runtime_ctx.this_streams_shutdown_escalation().is_forced(),
                    runtime_ctx.this_streams_teardown_progress_note(),
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

                the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
                    RuntimeEvent::CompilerWillCreateProcessor {
                        processor_id: proc_id.clone(),
                        processor_type: processor_type.clone(),
                    },
                );

                tracing::info!("[{}] Preparing {}", CompilePhase::Prepare, proc_id);

                let barrier_handle = super::compiler_ops::prepare_processor(&mut graph, proc_id)?;
                barrier_handles.push((proc_id.clone(), barrier_handle));

                the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
                    RuntimeEvent::CompilerDidCreateProcessor {
                        processor_id: proc_id.clone(),
                        processor_type,
                    },
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
                super::compiler_ops::spawn_processor(Arc::clone(&graph_arc), runtime_ctx, proc_id)?;
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
            result.links_wired += wire_every_link_a_plan_adds(
                &graph_arc,
                &plan.links_to_add,
                runtime_ctx.this_stream().as_ref(),
                runtime_ctx.iceoryx2_node(),
                the_stream_the_compile_belongs_to,
            )?;
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
        for (proc_id, config_to_apply) in plan.config_updates {
            match super::compiler_ops::apply_processor_config_update(
                &graph_arc,
                &proc_id,
                config_to_apply,
            )? {
                ProcessorConfigUpdateOutcome::TakenAndRecordedOnTheNode => {
                    tracing::info!("[CONFIG] Updated config for {}", proc_id);
                    result.configs_updated += 1;
                    the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
                        RuntimeEvent::ProcessorConfigDidChange {
                            processor_id: proc_id,
                        },
                    );
                }
                ProcessorConfigUpdateOutcome::ProcessorNoLongerInTheGraph => {}
            }
        }

        // Mark the graph as compiled
        graph_arc.write().mark_compiled();

        the_stream_the_compile_belongs_to
            .publish_on_this_streams_topic(RuntimeEvent::CompilerDidCompile);
        tracing::info!("Compile complete: {}", result);

        if !abandoned_in_this_compile.is_empty() {
            return Err(Error::Runtime(
                DescriptionOfTheAbandonedProcessorThreads(&abandoned_in_this_compile).to_string(),
            ));
        }
        Ok(())
    }
}

/// Wire each of `links_to_add` in `graph_arc`, telling the stream's topic
/// before and after; how many were wired. A link in this stream that cannot be
/// wired fails the phase; a link reading another stream is reconciled against
/// that stream through `reading_stream`, and one that waits or was refused
/// says so in its own state and never fails it.
fn wire_every_link_a_plan_adds(
    graph_arc: &RwLock<Graph>,
    links_to_add: &[LinkUniqueId],
    reading_stream: Option<&Arc<LoadedStreamInThisRuntime>>,
    iceoryx2_node: &Iceoryx2Node,
    the_stream_the_compile_belongs_to: &LoadedStreamIdentity,
) -> Result<usize> {
    let mut links_wired = 0;
    for link_id in links_to_add {
        let (from_port, to_port, reads_another_stream) = {
            let graph = graph_arc.read();
            let link = graph
                .traversal()
                .e(link_id)
                .first()
                .ok_or_else(|| Error::LinkNotFound(format!("Link '{}' not found", link_id)))?;
            (
                link.source().to_string(),
                link.to_port().to_string(),
                link.source_in_this_stream().is_none(),
            )
        };

        let publish_that_the_link_will_be_wired = || {
            the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
                RuntimeEvent::CompilerWillWireLink {
                    link_id: link_id.to_string(),
                    from_port: from_port.clone(),
                    to_port: to_port.clone(),
                },
            );
        };

        let wired = if reads_another_stream {
            wire_a_link_reading_another_stream(
                reading_stream,
                link_id,
                publish_that_the_link_will_be_wired,
            )
        } else {
            publish_that_the_link_will_be_wired();
            tracing::info!("[{}] Opening service {}", CompilePhase::Wire, link_id);
            super::compiler_ops::open_iceoryx2_service(
                &mut graph_arc.write(),
                link_id,
                iceoryx2_node,
            )?;
            true
        };
        if !wired {
            continue;
        }

        the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
            RuntimeEvent::CompilerDidWireLink {
                link_id: link_id.to_string(),
                from_port,
                to_port,
            },
        );

        links_wired += 1;
    }
    Ok(links_wired)
}

/// Reconcile link `link_id` of `reading_stream`, which reads another stream,
/// against that stream, calling `before_it_opens_the_channel` only once the
/// reconcile goes on to wire it; whether it is wired now or handed to its
/// out-of-process ends. A link that waits or was refused says so in its own
/// state and never fails the commit.
fn wire_a_link_reading_another_stream(
    reading_stream: Option<&Arc<LoadedStreamInThisRuntime>>,
    link_id: &LinkUniqueId,
    before_it_opens_the_channel: impl FnOnce(),
) -> bool {
    use super::compiler_ops::HowALinkReadingAnotherStreamWasReconciled;

    let Some(reading_stream) = reading_stream else {
        tracing::warn!(
            "[{}] {} reads another stream, and the stream compiling it is gone; not wired",
            CompilePhase::Wire,
            link_id
        );
        return false;
    };
    let how_it_was_reconciled =
        super::compiler_ops::reconcile_a_link_reading_another_stream_announcing_its_wiring(
            reading_stream,
            link_id,
            before_it_opens_the_channel,
        );
    tracing::info!(
        "[{}] {} reads another stream: {:?}",
        CompilePhase::Wire,
        link_id,
        how_it_was_reconciled
    );
    matches!(
        how_it_was_reconciled,
        HowALinkReadingAnotherStreamWasReconciled::WiredNow(_)
    )
}

/// Unwire link `link_id` while the graph holds it, telling the stream's topic
/// before and after, then take its edge out of the graph; whether it was there
/// to unwire. A link this stream cannot close is still taken out, with a
/// warning naming it.
pub(super) fn unwire_and_remove_a_link(
    graph_arc: &RwLock<Graph>,
    link_id: &LinkUniqueId,
    the_stream_the_compile_belongs_to: &LoadedStreamIdentity,
) -> Result<bool> {
    let mut graph = graph_arc.write();
    let unwired = if let Some(link) = graph
        .traversal()
        .e(())
        .filter(|link| link.id == *link_id)
        .first()
    {
        let from_port = link.source().to_string();
        let to_port = link.to_port().to_string();

        the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
            RuntimeEvent::CompilerWillUnwireLink {
                link_id: link_id.to_string(),
                from_port: from_port.clone(),
                to_port: to_port.clone(),
            },
        );

        tracing::info!("[CLOSE SERVICE] {}", link_id);
        if let Err(e) = super::compiler_ops::close_iceoryx2_service(&mut graph, link_id) {
            tracing::warn!("Failed to close service {}: {}", link_id, e);
        }

        the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
            RuntimeEvent::CompilerDidUnwireLink {
                link_id: link_id.to_string(),
                from_port,
                to_port,
            },
        );
        true
    } else {
        false
    };
    drop(graph);

    let mut graph = graph_arc.write();
    if graph.traversal_mut().e(link_id).drop().exists() {
        return Err(Error::GraphError("value was not dropped".into()));
    }
    Ok(unwired)
}

/// The plan `operations` make against `graph_arc`: each add the graph still
/// holds and has not built or wired, queued once however often it was
/// logged, and each removal and config update as logged.
fn the_plan_these_operations_make(
    graph_arc: &RwLock<Graph>,
    operations: Vec<PendingOperation>,
) -> CompilationPlan {
    use crate::core::graph::{PendingDeletionComponent, ProcessorInstanceComponent};

    let mut plan = CompilationPlan::default();
    let mut links_queued_in_this_batch = std::collections::HashSet::new();

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
                // A link reading another stream is reconciled on every add:
                // the reconcile alone knows whether its wiring still stands
                // against the source stream's load.
                let already_wired_or_awaiting_an_answer = link.is_some_and(|link| {
                    link.source_in_this_stream().is_some()
                        && this_link_is_already_wired_or_awaiting_its_helpers_answer(link)
                });
                let pending_deletion = link
                    .map(|l| l.has::<PendingDeletionComponent>())
                    .unwrap_or(false);
                drop(graph);

                if pending_deletion {
                    tracing::debug!("AddLink({}): pending deletion, skipping add", id);
                } else if links_queued_in_this_batch.contains(&id) {
                    tracing::debug!("AddLink({}): already queued in this batch, skipping", id);
                } else if exists && !already_wired_or_awaiting_an_answer {
                    links_queued_in_this_batch.insert(id.clone());
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
            PendingOperation::UpdateProcessorConfig {
                processor_id,
                config_to_apply,
            } => {
                plan.config_updates.push((processor_id, config_to_apply));
            }
        }
    }
    plan
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
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::processors::OutOfProcessLinkWireReply;

    #[test]
    fn a_link_nothing_has_wired_yet_is_planned() {
        let link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        assert!(!this_link_is_already_wired_or_awaiting_its_helpers_answer(
            &link
        ));
    }

    #[test]
    fn a_wired_link_is_not_planned_again() {
        let mut link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
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
        let mut link = Link::between(
            OutputLinkPortRef::new("Psrc", "out1"),
            InputLinkPortRef::new("Pdst", "in1"),
        );
        link.insert(LinkStateComponent(LinkState::Pending));
        link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
            OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
        ]));
        assert!(this_link_is_already_wired_or_awaiting_its_helpers_answer(
            &link
        ));
    }

    /// A link a batch logs twice — a live connect and the start that commits
    /// it, or two re-plans of a link reading another stream — is planned
    /// once, in the order it was first logged.
    ///
    /// Fail-without-fix: queue each `AddLink` as logged and both links are
    /// planned twice, so the WIRE phase opens a second subscriber and notifier
    /// for one link.
    #[test]
    fn a_link_queued_twice_in_one_batch_is_wired_once() {
        use crate::core::graph::OutputPortInAnotherStreamOnThisMachine;
        use crate::core::test_support::{add_mock_input_only, add_mock_output_only};

        let mut graph = Graph::new();
        let source = add_mock_output_only(&mut graph);
        let reader = add_mock_input_only(&mut graph);
        let link_in_this_stream = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(source.as_str(), "out1"),
                InputLinkPortRef::new(reader.as_str(), "in1"),
            )
            .first()
            .expect("the link in this stream is added")
            .id
            .clone();
        let link_reading_another_stream = graph
            .traversal_mut()
            .add_e_reading_another_stream(
                OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "out1").unwrap(),
                InputLinkPortRef::new(reader.as_str(), "in1"),
            )
            .first()
            .expect("the link reading another stream is added")
            .id
            .clone();

        let plan = the_plan_these_operations_make(
            &RwLock::new(graph),
            vec![
                PendingOperation::AddLink(link_in_this_stream.clone()),
                PendingOperation::AddLink(link_reading_another_stream.clone()),
                PendingOperation::AddLink(link_in_this_stream.clone()),
                PendingOperation::AddLink(link_reading_another_stream.clone()),
            ],
        );

        assert_eq!(
            plan.links_to_add,
            vec![link_in_this_stream, link_reading_another_stream]
        );
    }
}
