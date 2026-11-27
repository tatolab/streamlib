// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::compiler::PendingOperation;
use crate::core::compiler::compilation_plan::{
    CompilationPlan, FunctionNodePromotionOutOfTheIslandsOfTheReaderItCrashed,
};
use crate::core::compiler::compile_phase::CompilePhase;
use crate::core::compiler::compile_result::CompileResult;
use crate::core::compiler::compiler_ops::function_node_island::{
    HowALinkIsWired, close_the_channels_no_island_reads_any_more,
    hand_each_reader_the_island_a_compile_changed, how_a_link_is_wired,
    note_where_each_function_node_runs, open_the_channels_each_island_newly_reads,
    refuse_an_edit_an_island_cannot_take, the_function_nodes_among,
    unwire_a_link_from_a_function_node_island_into_its_reader,
    wire_a_call_inside_a_function_node_island,
    wire_a_link_from_a_function_node_island_into_its_reader,
    wire_a_link_into_a_function_node_island_through_its_reader,
};
use crate::core::compiler::compiler_ops::function_node_pool::{
    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool,
    hand_the_running_function_node_pool_its_carries, the_links_a_link_is_wired_as,
};
use crate::core::compiler::compiler_ops::function_node_promotion::{
    FunctionNodePromotionPlanned, WhatTheCompileCarryingAPromotionRemoves,
    place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own,
    plan_the_promotion_of_a_function_node_out_of_its_readers_islands,
    record_each_promotion_on_its_nodes_metrics,
};
use crate::core::compiler::compiler_ops::running_node_restart::{
    RunningNodesALiveEditRestarts, forget_the_build_of_each_node_restarted,
    hold_each_link_to_wire_again_as_pending, the_running_nodes_a_rebuild_restarts,
    unwire_each_link_of_the_nodes_restarted,
};
use crate::core::compiler::compiler_ops::shader_node_island::{
    hand_each_running_host_the_island_placed_on_it, place_each_shader_node_island_on_its_host,
    the_shader_node_islands_a_live_edit_places_anew,
};
use crate::core::compiler::compiler_ops::unlinked_input_values_delivery::deliver_the_unlinked_input_values_of_each_node;
use crate::core::compiler::compiler_transaction::CompilerTransactionHandle;
use crate::core::compiler::node_thread_shutdown::{
    AbandonedNodeThreadStillRunning, DescriptionOfTheAbandonedNodeThreads, NodeDisplayNameAndId,
    NodeThreadJoinBudgets, remove_nodes_signalling_every_thread_before_joining_any,
    stop_node_threads_signalling_every_thread_before_joining_any,
};
use crate::core::context::RuntimeContext;
use crate::core::error::{Error, Result};
use crate::core::graph::input_values::LiveInputValuesOfAStream;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, Link, LinkState, LinkStateComponent,
    LinkUniqueId, NodeReadyBarrierHandle, NodeRunId, OutOfProcessLinkWireRepliesComponent,
    OutOfProcessLinkWiringComponent, StateComponent,
};
use crate::core::nodes::{NodeState, NodeTypesOneStreamResolves};
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};
use crate::iceoryx2::Iceoryx2Node;
use nodes_set_up_after_the_sources_of_their_wired_settings::{
    build_each_with_what_the_wires_into_its_settings_carried,
    let_each_node_set_up_after_the_sources_of_the_wires_into_its_settings,
};

mod commit_of_the_logged_operations;
mod links_a_plan_adds_wired;
mod links_an_end_refused_wired_again;
mod links_whose_pin_types_differ;
mod nodes_set_up_after_the_sources_of_their_wired_settings;
mod transaction_held_for_one_commit;
pub(crate) use commit_of_the_logged_operations::{ACommitFailed, WhatTheHeldCommitFound};

pub(crate) struct Compiler {
    // Graph ownership (moved from Runtime)
    graph: Arc<RwLock<Graph>>,
    // Transaction accumulates operations until commit
    transaction: Arc<Mutex<Vec<PendingOperation>>>,
    /// Held for the whole of a commit: two batches compiling at once would
    /// interleave their spawn and wire phases against one graph.
    one_commit_at_a_time: Mutex<()>,
    abandoned_node_threads: Mutex<Vec<AbandonedNodeThreadStillRunning>>,
    /// While held, how many operations the transaction logged before the hold: a commit asked
    /// for meanwhile compiles nothing, so the operations logged under the hold are compiled
    /// together, as one commit, once it is released.
    transaction_held_for_one_commit: Mutex<Option<usize>>,
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

impl Compiler {
    /// Create a new compiler whose graph resolves the natively compiled node types alone.
    pub(crate) fn new() -> Self {
        Self::new_of_a_stream(
            Arc::new(NodeTypesOneStreamResolves::new()),
            LiveInputValuesOfAStream::default(),
        )
    }

    /// Create a new compiler of a stream, whose graph resolves node types through its stream's
    /// lookup and holds `live_input_values`, empty, as the values set live on its inputs.
    pub(crate) fn new_of_a_stream(
        node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
        live_input_values: LiveInputValuesOfAStream,
    ) -> Self {
        Self {
            graph: Arc::new(RwLock::new(Graph::new_of_a_stream(
                node_types_this_stream_resolves,
                live_input_values,
            ))),
            transaction: Arc::new(Mutex::new(Vec::new())),
            one_commit_at_a_time: Mutex::new(()),
            abandoned_node_threads: Mutex::new(Vec::new()),
            transaction_held_for_one_commit: Mutex::new(None),
        }
    }

    pub(crate) fn node_threads_abandoned_and_still_running(&self) -> Vec<NodeDisplayNameAndId> {
        let mut abandoned = self.abandoned_node_threads.lock();
        abandoned.retain(|thread| !thread.join_handle.is_finished());
        abandoned.iter().map(|thread| thread.node.clone()).collect()
    }

    // =========================================================================
    // Transaction API
    // =========================================================================

    /// Access graph and transaction for mutations. Callable from any thread.
    pub(crate) fn scope<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Graph, &CompilerTransactionHandle) -> R,
    {
        let mut graph = self.graph.write();
        let tx = CompilerTransactionHandle::new(Arc::clone(&self.transaction));
        f(&mut graph, &tx)
    }

    /// The graph's lock, for a change that edits the graph under it and then hands nodes what
    /// they read with it released, as a set of an input's value does; every other read or edit
    /// goes through [`Self::scope`] or [`Self::scope_reading`].
    pub(crate) fn graph_lock(&self) -> &RwLock<Graph> {
        &self.graph
    }

    /// Read the graph under its read lock, logging nothing.
    pub(crate) fn scope_reading<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Graph) -> R,
    {
        f(&self.graph.read())
    }

    /// The operations logged and not yet committed, in the order they were
    /// logged.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn logged_pending_operations(&self) -> Vec<PendingOperation> {
        self.transaction.lock().clone()
    }

    /// Take every operation logged, for a commit to compile.
    pub(super) fn take_the_logged_operations_to_compile(&self) -> Vec<PendingOperation> {
        std::mem::take(&mut *self.transaction.lock())
    }

    fn compile(
        graph_arc: Arc<RwLock<Graph>>,
        operations: Vec<PendingOperation>,
        runtime_ctx: &Arc<RuntimeContext>,
        abandoned_node_threads: &Mutex<Vec<AbandonedNodeThreadStillRunning>>,
    ) -> std::result::Result<(), ACommitFailed> {
        let readers_awaiting_a_promotion: Vec<NodeRunId> = operations
            .iter()
            .filter_map(|operation| match operation {
                PendingOperation::PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
                    reader_id,
                    ..
                } => Some(reader_id.clone()),
                _ => None,
            })
            .collect();
        let mut result = CompileResult::default();
        let compiled = Self::compile_counting_into(
            Arc::clone(&graph_arc),
            operations,
            runtime_ctx,
            abandoned_node_threads,
            &mut result,
        );
        if !readers_awaiting_a_promotion.is_empty() {
            let outcome = compiled.as_ref().copied().map_err(ToString::to_string);
            let graph = graph_arc.read();
            for reader_id in &readers_awaiting_a_promotion {
                if let Some(reader_link_wiring) = graph
                    .traversal()
                    .v(reader_id)
                    .first()
                    .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
                {
                    reader_link_wiring
                        .0
                        .settle_the_function_node_promotion_out_of_its_islands(outcome.clone());
                }
            }
        }
        compiled.map_err(|refusal| ACommitFailed {
            refusal,
            after_changing_what_runs: result.began_changing_what_runs,
        })
    }

    /// Compile `operations` against `graph_arc`, counting what it did into
    /// `result` as it goes, so a compile that fails part-way still says what
    /// it created.
    fn compile_counting_into(
        graph_arc: Arc<RwLock<Graph>>,
        operations: Vec<PendingOperation>,
        runtime_ctx: &Arc<RuntimeContext>,
        abandoned_node_threads: &Mutex<Vec<AbandonedNodeThreadStillRunning>>,
        result: &mut CompileResult,
    ) -> Result<()> {
        let the_stream_the_compile_belongs_to = runtime_ctx.loaded_stream_identity();
        // =====================================================================
        // 1. Validate and categorize operations
        // =====================================================================
        let mut plan = the_plan_these_operations_make(&graph_arc, operations);
        let node_ids_to_remove = plan.node_ids_to_remove();
        let promotions_planned = plan_every_function_node_promotion(&graph_arc, &mut plan);

        {
            let graph = graph_arc.read();
            plan.drop_link_adds_into_removed_nodes(&graph);
        }
        links_whose_pin_types_differ::judge_the_links_each_type_choice_reaches(
            &graph_arc.read(),
            &mut plan,
        );
        refuse_an_edit_an_island_cannot_take(
            &graph_arc.read(),
            &plan.nodes_to_add,
            &node_ids_to_remove,
            &plan.links_to_add,
            &plan.links_to_remove,
        )?;
        // A link this commit starts holding unwired for its pin types is no link a restart
        // wires again; one already held so changes nothing here, so restarts nothing again. A
        // link an end refused is only opened again, which changes the shape of no island.
        let links_changed: Vec<LinkUniqueId> = {
            let graph = graph_arc.read();
            [
                plan.links_to_add.as_slice(),
                plan.links_to_remove.as_slice(),
            ]
            .concat()
            .into_iter()
            .filter(|link_id| !plan.links_to_wire_again.contains(link_id))
            .chain(
                plan.links_judged_again
                    .differing
                    .iter()
                    .map(|(link_id, _)| link_id)
                    .filter(|link_id| !graph.the_pin_types_of_the_link_differ(link_id))
                    .cloned(),
            )
            .collect()
        };
        let (shader_node_islands_placed_anew, nodes_restarted) = {
            let graph = graph_arc.read();
            let shader_node_islands_placed_anew = the_shader_node_islands_a_live_edit_places_anew(
                &graph,
                &plan.nodes_to_add,
                &node_ids_to_remove,
                &links_changed,
            )?;
            let nodes_restarted = shader_node_islands_placed_anew
                .the_nodes_and_links_built_again_in_their_new_place()
                .joined_with(the_running_nodes_a_rebuild_restarts(
                    &graph,
                    &plan.nodes_to_rebuild,
                    &plan.nodes_to_add,
                    &node_ids_to_remove,
                    &links_changed,
                )?);
            (shader_node_islands_placed_anew, nodes_restarted)
        };
        plan.add_each_node_and_link_restarted(&nodes_restarted);
        place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
            &mut graph_arc.write(),
            &plan.nodes_to_add,
            &node_ids_to_remove,
        )?;
        let carries_handed_to_the_running_pool = {
            let mut graph = graph_arc.write();
            let carries_handed_to_the_running_pool =
                carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
                    &mut graph,
                    &mut plan.nodes_to_add,
                    &mut plan.links_to_add,
                )?;
            plan.links_to_remove = std::mem::take(&mut plan.links_to_remove)
                .iter()
                .flat_map(|link_id| the_links_a_link_is_wired_as(&graph, link_id))
                .collect();
            carries_handed_to_the_running_pool
        };

        // Early return if nothing to do
        if plan.is_empty() {
            tracing::debug!("No changes to compile");
            return Ok(());
        }

        tracing::info!(
            "Compiling: +{} -{} nodes, +{} -{} links, {} held unwired",
            plan.nodes_to_add.len(),
            plan.nodes_to_remove.len(),
            plan.links_to_add.len(),
            plan.links_to_remove.len(),
            plan.links_to_hold_unwired.len(),
        );

        // Publish compile start event
        the_stream_the_compile_belongs_to
            .publish_on_this_streams_topic(RuntimeEvent::CompilerWillCompile);
        result.began_changing_what_runs = true;

        links_whose_pin_types_differ::hold_each_link_unwired(&mut graph_arc.write(), &plan)?;

        let abandoned_in_this_compile: Vec<NodeDisplayNameAndId> = if !plan
            .links_to_remove
            .is_empty()
            || !plan.nodes_to_remove.is_empty()
        {
            tracing::debug!(
                "[commit] Removing {} nodes, {} links",
                plan.nodes_to_remove.len(),
                plan.links_to_remove.len()
            );

            for link_id in &plan.links_to_remove {
                if unwire_and_remove_a_link(&graph_arc, link_id, the_stream_the_compile_belongs_to)?
                {
                    result.links_unwired += 1;
                }
            }

            let abandoned_by_this_removal =
                remove_nodes_signalling_every_thread_before_joining_any(
                    &graph_arc,
                    the_stream_the_compile_belongs_to,
                    &plan.nodes_to_remove,
                    NodeThreadJoinBudgets::ENGINE_CHOSEN,
                    || runtime_ctx.this_streams_shutdown_escalation().is_forced(),
                    runtime_ctx.this_streams_teardown_progress_note(),
                    abandoned_node_threads,
                )?;
            result.nodes_removed += plan.nodes_to_remove.len();
            abandoned_by_this_removal
        } else {
            Vec::new()
        };

        let abandoned_in_this_compile = [
            abandoned_in_this_compile,
            stop_each_node_restarted(
                &graph_arc,
                runtime_ctx,
                abandoned_node_threads,
                &nodes_restarted,
                &mut plan,
            )?,
        ]
        .concat();

        // =====================================================================
        // 3. Phase 1: PREPARE - Attach infrastructure components
        // =====================================================================
        let mut barrier_handles: Vec<(NodeRunId, NodeReadyBarrierHandle)> = vec![];
        let function_nodes_run_inside_their_readers =
            the_function_nodes_among(&graph_arc.read(), &plan.nodes_to_add);
        let shader_nodes_placed = {
            // A node whose place changes is among the nodes to add already, restarted, unless
            // its old thread was abandoned, which leaves it unbuilt and unplaced.
            let shader_nodes_to_place = [
                plan.nodes_to_add.as_slice(),
                &shader_node_islands_placed_anew.hosts_handed_their_island_anew,
                &shader_node_islands_placed_anew.fused_nodes_placed_anew,
            ]
            .concat();
            place_each_shader_node_island_on_its_host(
                &mut graph_arc.write(),
                &shader_nodes_to_place,
            )?
        };
        plan.leave_unbuilt_each_node_and_unwired_each_of_its_links(
            &graph_arc.read(),
            &shader_nodes_placed.in_a_cycle_no_node_evaluates,
        );
        let shader_nodes_fused_into_another = &shader_nodes_placed.fused_into_another;
        let nodes_to_build: Vec<NodeRunId> = plan
            .nodes_to_add
            .iter()
            .filter(|proc_id| {
                !function_nodes_run_inside_their_readers.contains(*proc_id)
                    && !shader_nodes_fused_into_another.contains(*proc_id)
            })
            .cloned()
            .collect();

        if !nodes_to_build.is_empty() {
            tracing::debug!("[{}] Starting", CompilePhase::Prepare);
            for proc_id in &nodes_to_build {
                let mut graph = graph_arc.write();
                let node =
                    graph.traversal().v(proc_id).first().ok_or_else(|| {
                        Error::NodeNotFound(format!("Node '{}' not found", proc_id))
                    })?;

                let node_type = node.node_type.clone();

                the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
                    RuntimeEvent::CompilerWillCreateNode {
                        node_run_id: proc_id.clone(),
                        node_type: node_type.clone(),
                    },
                );

                tracing::info!("[{}] Preparing {}", CompilePhase::Prepare, proc_id);

                let barrier_handle = super::compiler_ops::prepare_node(
                    &mut graph,
                    proc_id,
                    the_stream_the_compile_belongs_to,
                )?;
                barrier_handles.push((proc_id.clone(), barrier_handle));

                the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
                    RuntimeEvent::CompilerDidCreateNode {
                        node_run_id: proc_id.clone(),
                        node_type,
                    },
                );

                result.nodes_created += 1;
            }
            tracing::debug!("[{}] Completed", CompilePhase::Prepare);
        }

        if !nodes_to_build.is_empty() {
            tracing::debug!("[{}] Starting", CompilePhase::Spawn);
            for proc_id in &nodes_to_build {
                tracing::info!("[{}] Spawning {}", CompilePhase::Spawn, proc_id);
                super::compiler_ops::spawn_node(Arc::clone(&graph_arc), runtime_ctx, proc_id)?;
            }
            tracing::debug!("[{}] Completed", CompilePhase::Spawn);
        }

        for (proc_id, handle) in &barrier_handles {
            tracing::trace!("[{}] Waiting for READY signal", proc_id);
            if handle.ready_receiver.recv().is_err() {
                tracing::warn!("[{}] Node failed during instance creation", proc_id);
            }
        }

        // =====================================================================
        // 6. Phase 3: WIRE - Create ring buffers and connect ports
        // =====================================================================
        // A running pool takes the islands it newly carries before the links into and out of
        // their ports are wired, so its helper has declared those ports by then.
        if let Some(carries) = &carries_handed_to_the_running_pool {
            hand_the_running_function_node_pool_its_carries(&graph_arc, carries)?;
        }
        // A link an end refused is closed only once the plan's links to add are final, so none
        // is closed that this compile then leaves unwired.
        links_an_end_refused_wired_again::unwire_each_link_an_end_refused_keeping_it(
            &mut graph_arc.write(),
            &plan,
            &nodes_restarted.links,
        )?;
        if !plan.links_to_add.is_empty() {
            tracing::debug!("[{}] Starting", CompilePhase::Wire);
            result.links_wired += links_a_plan_adds_wired::wire_every_link_a_plan_adds(
                &graph_arc,
                &plan.links_to_add,
                &plan.links_to_wire_again,
                runtime_ctx.iceoryx2_node(),
                the_stream_the_compile_belongs_to,
            )?;
            tracing::debug!("[{}] Completed", CompilePhase::Wire);
        }
        hand_each_running_host_the_island_placed_on_it(
            &graph_arc.read(),
            &shader_node_islands_placed_anew.hosts_handed_their_island_anew,
        );
        open_the_channels_each_island_newly_reads(
            &mut graph_arc.write(),
            runtime_ctx.iceoryx2_node(),
        )?;
        hand_each_reader_the_island_a_compile_changed(&mut graph_arc.write(), &plan.links_to_add)?;
        close_the_channels_no_island_reads_any_more(&mut graph_arc.write())?;
        note_where_each_function_node_runs(&mut graph_arc.write());
        build_each_with_what_the_wires_into_its_settings_carried(&graph_arc, &plan.nodes_to_add);
        deliver_the_unlinked_input_values_of_each_node(&graph_arc, &plan.nodes_to_add)?;

        // A function node has no thread whose setup would settle its state: it
        // runs once it is wired into the read of each node it feeds. A shader
        // node fused into another's island runs once that node does.
        {
            let graph = graph_arc.read();
            for function_node_id in function_nodes_run_inside_their_readers
                .iter()
                .chain(shader_nodes_fused_into_another)
            {
                if let Some(state) = graph
                    .traversal()
                    .v(function_node_id)
                    .first()
                    .and_then(|node| node.get::<StateComponent>())
                {
                    state.transition_to_only_from(NodeState::Pending, NodeState::Running);
                }
            }
        }

        for promotion_planned in &promotions_planned {
            record_each_promotion_on_its_nodes_metrics(&mut graph_arc.write(), promotion_planned);
        }

        let_each_node_set_up_after_the_sources_of_the_wires_into_its_settings(
            &graph_arc,
            barrier_handles,
        )?;

        // Mark the graph as compiled
        graph_arc.write().mark_compiled();

        the_stream_the_compile_belongs_to
            .publish_on_this_streams_topic(RuntimeEvent::CompilerDidCompile);
        tracing::info!("Compile complete: {}", result);

        if !abandoned_in_this_compile.is_empty() {
            return Err(Error::Runtime(
                DescriptionOfTheAbandonedNodeThreads(&abandoned_in_this_compile).to_string(),
            ));
        }
        Ok(())
    }
}

/// Unwire every link of the nodes a live edit restarts, stop their threads, and
/// forget their builds, so the compile builds them again, a shader node in the
/// place it takes in the islands now; the nodes an abandoned thread still runs. Such a
/// node, abandoned by this stop or an earlier one, is left unbuilt, its links
/// with it, since that thread still writes the node's state.
fn stop_each_node_restarted(
    graph_arc: &Arc<RwLock<Graph>>,
    runtime_ctx: &Arc<RuntimeContext>,
    abandoned_node_threads: &Mutex<Vec<AbandonedNodeThreadStillRunning>>,
    nodes_restarted: &RunningNodesALiveEditRestarts,
    plan: &mut CompilationPlan,
) -> Result<Vec<NodeDisplayNameAndId>> {
    if nodes_restarted.is_empty() {
        return Ok(Vec::new());
    }
    if !nodes_restarted.nodes.is_empty() {
        tracing::info!(
            "Restarting {} running node(s) a live edit rebuilds",
            nodes_restarted.nodes.len()
        );
    }
    unwire_each_link_of_the_nodes_restarted(&mut graph_arc.write(), nodes_restarted)?;
    stop_node_threads_signalling_every_thread_before_joining_any(
        graph_arc,
        runtime_ctx.loaded_stream_identity(),
        &nodes_restarted.nodes,
        NodeThreadJoinBudgets::ENGINE_CHOSEN,
        || runtime_ctx.this_streams_shutdown_escalation().is_forced(),
        runtime_ctx.this_streams_teardown_progress_note(),
        abandoned_node_threads,
    );
    let still_held_by_an_abandoned_thread = the_restarted_nodes_an_abandoned_thread_still_runs(
        abandoned_node_threads,
        &nodes_restarted.nodes,
    );
    let abandoned_node_ids: Vec<NodeRunId> = still_held_by_an_abandoned_thread
        .iter()
        .map(|node| node.node_run_id.clone())
        .collect();
    let mut graph = graph_arc.write();
    plan.leave_unbuilt_each_node_and_unwired_each_of_its_links(&graph, &abandoned_node_ids);
    let rebuilt_node_ids: Vec<NodeRunId> = nodes_restarted
        .nodes
        .iter()
        .filter(|node_id| !abandoned_node_ids.contains(node_id))
        .cloned()
        .collect();
    forget_the_build_of_each_node_restarted(&mut graph, &rebuilt_node_ids);
    hold_each_link_to_wire_again_as_pending(&mut graph, nodes_restarted, &plan.links_to_add);
    Ok(still_held_by_an_abandoned_thread)
}

/// Each of `node_ids` whose thread, abandoned by this compile or an earlier
/// one, has not returned. Such a node keeps no thread handle, so a stop finds
/// nothing to abandon, yet its old thread still writes its state.
fn the_restarted_nodes_an_abandoned_thread_still_runs(
    abandoned_node_threads: &Mutex<Vec<AbandonedNodeThreadStillRunning>>,
    node_ids: &[NodeRunId],
) -> Vec<NodeDisplayNameAndId> {
    let mut abandoned = abandoned_node_threads.lock();
    abandoned.retain(|thread| !thread.join_handle.is_finished());
    let mut still_running: Vec<NodeDisplayNameAndId> = Vec::new();
    for thread in abandoned.iter() {
        if node_ids.contains(&thread.node.node_run_id) && !still_running.contains(&thread.node) {
            still_running.push(thread.node.clone());
        }
    }
    still_running
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

        let unwired = match how_a_link_is_wired(&graph, link_id) {
            Ok(HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead) => {
                unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, link_id)
            }
            Ok(
                HowALinkIsWired::AsACallInsideAFunctionNodeIsland
                | HowALinkIsWired::AsACallInsideAShaderNodeIsland,
            ) => Ok(()),
            Ok(HowALinkIsWired::ThroughAChannelIntoTheHostOfItsShaderNodeIsland(
                host_input_port,
            )) => {
                super::compiler_ops::close_iceoryx2_service_into_the_host_of_a_shader_node_island(
                    &mut graph,
                    link_id,
                    &host_input_port,
                )
            }
            Ok(HowALinkIsWired::ThroughAChannelReadByTheReaderOfItsFunctionNodeIsland) => {
                let read_through_a_reader = graph.traversal().e(link_id).first().is_some_and(
                    |link| {
                        link.has::<crate::core::graph::LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
                    },
                );
                if read_through_a_reader {
                    super::compiler_ops::close_iceoryx2_service(&mut graph, link_id)
                } else {
                    Ok(())
                }
            }
            Ok(HowALinkIsWired::ThroughAChannel) | Err(_) => {
                tracing::info!("[CLOSE SERVICE] {}", link_id);
                super::compiler_ops::close_iceoryx2_service(&mut graph, link_id)
            }
        };
        if let Err(e) = unwired {
            tracing::warn!("Failed to unwire {}: {}", link_id, e);
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
    crate::core::graph::input_values::forget_what_a_link_into_a_setting_carried(&graph, link_id);
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
    use crate::core::graph::{NodeInstanceComponent, PendingDeletionComponent};

    let mut plan = CompilationPlan::default();
    let mut links_queued_in_this_batch = std::collections::HashSet::new();

    for op in operations {
        match op {
            PendingOperation::AddNode(id) => {
                let graph = graph_arc.read();
                let exists = graph.traversal().v(&id).exists();
                let running = graph
                    .traversal()
                    .v(&id)
                    .first()
                    .map(|n| n.has::<NodeInstanceComponent>())
                    .unwrap_or(false);
                let pending_deletion = graph
                    .traversal()
                    .v(&id)
                    .first()
                    .map(|n| n.has::<PendingDeletionComponent>())
                    .unwrap_or(false);
                drop(graph);

                if pending_deletion {
                    tracing::debug!("AddNode({}): pending deletion, skipping add", id);
                } else if exists && !running {
                    plan.nodes_to_add.push(id);
                } else if !exists {
                    tracing::warn!("AddNode({}): not in graph, skipping", id);
                } else {
                    tracing::debug!("AddNode({}): already running, skipping", id);
                }
            }
            PendingOperation::RemoveNode(id, why) => plan.remove_the_node(id, why),
            PendingOperation::AddLink(id) => {
                let graph = graph_arc.read();
                let link = graph.traversal().e(&id).first();
                let exists = link.is_some();
                let already_wired_or_awaiting_an_answer =
                    link.is_some_and(this_link_is_already_wired_or_awaiting_its_helpers_answer);
                let refused_by_a_far_side =
                    link.is_some_and(crate::core::graph::an_end_refused_the_link);
                let pending_deletion = link
                    .map(|l| l.has::<PendingDeletionComponent>())
                    .unwrap_or(false);
                drop(graph);

                if pending_deletion {
                    tracing::debug!("AddLink({}): pending deletion, skipping add", id);
                } else if links_queued_in_this_batch.contains(&id) {
                    tracing::debug!("AddLink({}): already queued in this batch, skipping", id);
                } else if refused_by_a_far_side {
                    tracing::info!("AddLink({}): an end refused it, so it is wired again", id);
                    links_queued_in_this_batch.insert(id.clone());
                    plan.links_to_wire_again.push(id.clone());
                    plan.links_to_add.push(id);
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
            PendingOperation::RebuildNode(node_run_id) => {
                if !plan.nodes_to_rebuild.contains(&node_run_id) {
                    plan.nodes_to_rebuild.push(node_run_id);
                }
            }
            PendingOperation::JudgeTheLinksTheTypeChoicesOfTheNodeReach(node_run_id) => {
                if !plan
                    .nodes_whose_type_choices_are_judged_again
                    .contains(&node_run_id)
                {
                    plan.nodes_whose_type_choices_are_judged_again
                        .push(node_run_id);
                }
            }
            PendingOperation::PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
                function_node_id,
                reader_id,
                promotion_request_number,
                reason,
            } => {
                plan.function_node_promotions.push(
                    FunctionNodePromotionOutOfTheIslandsOfTheReaderItCrashed {
                        function_node_id,
                        reader_id,
                        promotion_request_number,
                        reason,
                    },
                );
            }
        }
    }
    plan
}

fn plan_every_function_node_promotion(
    graph_arc: &RwLock<Graph>,
    plan: &mut CompilationPlan,
) -> Vec<FunctionNodePromotionPlanned> {
    let mut promotions_planned = Vec::new();
    for promotion in std::mem::take(&mut plan.function_node_promotions) {
        let Some(reader_link_wiring) = graph_arc
            .read()
            .traversal()
            .v(&promotion.reader_id)
            .first()
            .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
            .map(|component| Arc::clone(&component.0))
        else {
            tracing::warn!(
                "Function node {} is not promoted: its reader {} runs no helper process",
                promotion.function_node_id,
                promotion.reader_id,
            );
            continue;
        };
        if !reader_link_wiring.claim_the_awaited_function_node_promotion_out_of_its_islands(
            promotion.promotion_request_number,
        ) {
            tracing::warn!(
                "Function node {} is not promoted: {}'s host no longer waits on that request",
                promotion.function_node_id,
                promotion.reader_id,
            );
            continue;
        }
        let promotion_planned =
            match plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
                &mut graph_arc.write(),
                &promotion.function_node_id,
                &promotion.reader_id,
                &promotion.reason,
                &WhatTheCompileCarryingAPromotionRemoves {
                    nodes: &plan.node_ids_to_remove(),
                    links: &plan.links_to_remove,
                },
            ) {
                Ok(promotion_planned) => promotion_planned,
                Err(refusal) => {
                    reader_link_wiring.settle_the_function_node_promotion_out_of_its_islands(Err(
                        refusal.to_string(),
                    ));
                    continue;
                }
            };
        tracing::info!(
            "Promoting function node {} and {} downstream of it out of {}'s islands",
            promotion.function_node_id,
            promotion_planned.nodes_to_add.len() - 1,
            promotion.reader_id,
        );
        for node_run_id in &promotion_planned.nodes_to_add {
            if !plan.nodes_to_add.contains(node_run_id) {
                plan.nodes_to_add.push(node_run_id.clone());
            }
        }
        for link_id in &promotion_planned.links_to_add {
            if !plan.links_to_add.contains(link_id) {
                plan.links_to_add.push(link_id.clone());
            }
        }
        promotions_planned.push(promotion_planned);
    }
    promotions_planned
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
mod already_wired_tests;

#[cfg(test)]
mod links_an_end_refused_wired_again_tests;

#[cfg(test)]
mod abandoned_shader_node_restart_tests;

#[cfg(test)]
mod transaction_held_for_one_commit_tests;

#[cfg(test)]
mod nodes_set_up_after_the_sources_of_their_wired_settings_tests;
