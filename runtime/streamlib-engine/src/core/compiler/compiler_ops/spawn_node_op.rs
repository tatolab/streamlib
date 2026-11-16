// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

#[cfg(unix)]
use std::os::fd::OwnedFd;

use parking_lot::RwLock;

use crate::core::compiler::NodeThreadKind;
use crate::core::compiler::compiler_ops::open_iceoryx2_service_op::insert_loss_counts_on_graph_node_once;
use crate::core::compiler::scheduling::{SchedulingStrategy, scheduling_strategy_for_node};

use crate::core::context::{
    FullAccessGrant, GpuContext, IsolationTier, RuntimeContext, RuntimeContextFullAccess,
};
use crate::core::descriptors::NodeRuntime;
use crate::core::error::{Error, Result};
use crate::core::execution::{ProcessExecution, run_node_loop};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, NodeInstanceWithItsOutOfProcessLinkWiring, NodeLossCounts,
    NodePauseGateComponent, NodeReadyBarrierComponent, NodeRunId, ObservableNodeState,
    ShutdownChannelComponent, StateComponent, StepProgressOfOneNodeComponent,
    ThreadHandleComponent,
};
use crate::core::nodes::NodeState;

#[tracing::instrument(name = "compiler.spawn_node", skip(graph_arc, runtime_ctx), fields(node_run_id = node_run_id.as_ref()))]
pub(crate) fn spawn_node(
    graph_arc: Arc<RwLock<Graph>>,
    runtime_ctx: &Arc<RuntimeContext>,
    node_run_id: impl AsRef<str>,
) -> Result<()> {
    let node_run_id = node_run_id.as_ref();

    // Check if already has a thread or subprocess (already running)
    {
        let graph = graph_arc.read();
        let already_running = graph
            .traversal()
            .v(node_run_id)
            .first()
            .map(|n| n.has::<ThreadHandleComponent>())
            .unwrap_or(false);
        if already_running {
            return Ok(());
        }
    }

    let runtime = {
        let graph = graph_arc.read();
        let proc_type = graph
            .traversal()
            .v(node_run_id)
            .first()
            .map(|n| n.node_type().clone());
        proc_type
            .as_ref()
            .and_then(|ident| graph.node_types_this_stream_resolves().descriptor(ident))
            .map(|d| d.runtime.clone())
            .unwrap_or(NodeRuntime::Rust)
    };

    // Extract barrier before spawning (needed for both paths)
    let (barrier_component, proc_id_clone) = {
        let mut graph = graph_arc.write();
        let node_mut = graph
            .traversal_mut()
            .v(node_run_id)
            .first_mut()
            .ok_or_else(|| Error::NodeNotFound(format!("Node '{}' not found", node_run_id)))?;

        let barrier = node_mut
            .remove::<NodeReadyBarrierComponent>()
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "Node '{}' has no NodeReadyBarrierComponent",
                    node_run_id
                ))
            })?;

        (barrier, NodeRunId::from(node_run_id))
    };

    let strategy = {
        let graph = graph_arc.read();
        let node =
            graph.traversal().v(&proc_id_clone).first().ok_or_else(|| {
                Error::NodeNotFound(format!("Node '{}' not found", proc_id_clone))
            })?;
        scheduling_strategy_for_node(graph.node_types_this_stream_resolves(), node)
    };

    let runtime_label = match runtime {
        NodeRuntime::Rust => "Rust node",
        NodeRuntime::Python => "Python node",
    };
    tracing::info!(
        "[{}] Spawning {} with strategy: {}",
        node_run_id,
        runtime_label,
        strategy.description()
    );

    match strategy {
        SchedulingStrategy::DedicatedThread { priority } => {
            spawn_dedicated_thread(
                graph_arc,
                runtime_ctx,
                proc_id_clone,
                priority,
                barrier_component,
                runtime,
            )?;
        }
    }

    Ok(())
}

fn spawn_dedicated_thread(
    graph_arc: Arc<RwLock<Graph>>,
    runtime_ctx: &Arc<RuntimeContext>,
    node_run_id: NodeRunId,
    priority: crate::core::execution::ThreadPriority,
    mut barrier: NodeReadyBarrierComponent,
    runtime: NodeRuntime,
) -> Result<()> {
    // Clone Arcs for thread
    let graph_arc_clone = Arc::clone(&graph_arc);
    let runtime_ctx_clone = Arc::clone(runtime_ctx);
    let proc_id_clone = node_run_id.clone();

    let (node_to_attach, node_type) = {
        let graph = graph_arc.read();
        let node = graph
            .traversal()
            .v(&node_run_id)
            .first()
            .ok_or_else(|| Error::NodeNotFound(format!("Node '{}' not found", node_run_id)))?;
        let node_type = node.node_type().clone();
        (
            NodeInstanceWithItsOutOfProcessLinkWiring::from(
                graph.node_types_this_stream_resolves().create(node)?,
            ),
            node_type,
        )
    };

    if let Some(output_writer_inner) = node_to_attach
        .node_instance
        .lock()
        .iceoryx2_output_writer_inner()
    {
        output_writer_inner.reach_the_stream_clock(Arc::clone(&runtime_ctx.stream_clock));
    }
    let node_arc_clone = Arc::clone(&node_to_attach.node_instance);
    let thread_kind = match runtime {
        NodeRuntime::Rust => NodeThreadKind::NativeNode,
        NodeRuntime::Python => NodeThreadKind::HelperProcessHost,
    };

    let isolation_tier = IsolationTier::TrustedInstalled;

    let thread = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let _entered_the_streams_log_route = runtime_ctx_clone
                .this_streams_log_route()
                .enter_on_this_thread();
            let current_thread = std::thread::current();
            let thread_id = current_thread.id();
            crate::core::runtime::mark_this_thread_as_a_node_execution_thread();

            tracing::info!("[{}] Thread started: id={:?}", proc_id_clone, thread_id);

            // Apply thread priority (platform-specific)
            // Skip for Manual mode - real work runs on OS-managed callback threads
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                let is_manual = node_arc_clone
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

            #[cfg(target_os = "linux")]
            {
                let is_manual = node_arc_clone
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
                    crate::linux::thread_priority::apply_thread_priority(priority)
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
                "[{}] Attaching NodeInstanceComponent to graph",
                proc_id_clone
            );
            let (paced_by_real_time, out_of_process_link_wiring) = {
                let node = node_arc_clone.lock();
                (
                    node.is_paced_by_real_time(),
                    node.out_of_process_link_wiring(),
                )
            };
            {
                let mut graph = graph_arc_clone.write();
                if let Some(out_of_process_link_wiring) = out_of_process_link_wiring {
                    insert_loss_counts_on_graph_node_once(
                        &mut graph,
                        &proc_id_clone,
                        NodeLossCounts::CountedInItsHelperProcess(
                            out_of_process_link_wiring.helper_placed_node_loss_counts(),
                        ),
                    );
                }
                if let Some(node) = graph.traversal_mut().v(&proc_id_clone).first_mut() {
                    if paced_by_real_time
                        && let Some(step_progress) = node.get::<StepProgressOfOneNodeComponent>()
                    {
                        step_progress.0.note_it_stands_outside_the_step_barrier();
                    }
                    node_to_attach.attach_to(node);
                }
            }
            tracing::trace!("[{}] NodeInstanceComponent attached", proc_id_clone);

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
            let (
                state_arc,
                shutdown_rx,
                shutdown_wake_fd,
                pause_gate_inner,
                step_progress,
                exec_config,
                execution_the_node_runs_in,
                node_display_name,
                node_uuid,
            ) = {
                let mut graph = graph_arc_clone.write();
                let node = match graph.traversal_mut().v(&proc_id_clone).first_mut() {
                    Some(n) => n,
                    None => {
                        tracing::error!("[{}] Node not found after wiring", proc_id_clone);
                        return;
                    }
                };
                let node_display_name = node.display_name.clone();
                let node_uuid = node.node_uuid;

                let state = match node.get::<StateComponent>() {
                    Some(s) => s.clone_inner(),
                    None => {
                        tracing::error!("[{}] No StateComponent", proc_id_clone);
                        return;
                    }
                };

                let (shutdown_rx, shutdown_wake_fd) =
                    match node.get_mut::<ShutdownChannelComponent>() {
                        Some(channel) => {
                            let rx = match channel.take_receiver() {
                                Some(rx) => rx,
                                None => {
                                    tracing::error!(
                                        "[{}] Shutdown receiver already taken",
                                        proc_id_clone
                                    );
                                    return;
                                }
                            };
                            let shutdown_wake_fd = clone_shutdown_wake_fd(channel, &proc_id_clone);
                            (rx, shutdown_wake_fd)
                        }
                        None => {
                            tracing::error!("[{}] No ShutdownChannelComponent", proc_id_clone);
                            return;
                        }
                    };

                let pause_gate_inner = match node.get::<NodePauseGateComponent>() {
                    Some(pg) => pg.clone_inner(),
                    None => {
                        tracing::error!("[{}] No NodePauseGateComponent", proc_id_clone);
                        return;
                    }
                };

                let step_progress = node
                    .get::<StepProgressOfOneNodeComponent>()
                    .map(|step_progress| Arc::clone(&step_progress.0))
                    .unwrap_or_default();

                let (exec_config, execution_the_node_runs_in) = {
                    let node = node_arc_clone.lock();
                    let exec_config = node.execution_config();
                    let execution_the_node_runs_in = node.out_of_process_link_wiring().map_or(
                        exec_config.execution,
                        |out_of_process_link_wiring| {
                            out_of_process_link_wiring.far_side_process_execution()
                        },
                    );
                    (exec_config, execution_the_node_runs_in)
                };

                (
                    state,
                    shutdown_rx,
                    shutdown_wake_fd,
                    pause_gate_inner,
                    step_progress,
                    exec_config,
                    execution_the_node_runs_in,
                    node_display_name,
                    node_uuid,
                )
            }; // Lock released here

            if execution_the_node_runs_in == ProcessExecution::Manual
                && !step_progress.stands_outside_the_step_barrier()
            {
                let admitted = runtime_ctx_clone
                    .stream_clock
                    .admit_unless_it_steps(|| step_progress.note_it_paces_its_own_callbacks());
                if !admitted {
                    let refusal = format!(
                        "the node `{node_display_name}` paces itself (execution = manual), \
                         so it cannot run in a stream whose clock steps (stepped, or scaled \
                         above 1×), whose steps every node runs on"
                    );
                    tracing::error!("[{}] {refusal}", proc_id_clone);
                    state_arc.fail_with(refusal);
                    return;
                }
            }

            if matches!(
                execution_the_node_runs_in,
                ProcessExecution::Continuous { .. }
            ) && !step_progress.stands_outside_the_step_barrier()
            {
                step_progress.note_it_finishes_every_step();
            }

            let node_context = runtime_ctx_clone
                .with_node_run_id(proc_id_clone.clone())
                .with_node_display_name(node_display_name)
                .with_node_uuid(node_uuid)
                .with_pause_gate(pause_gate_inner.clone())
                .with_node_state(Arc::clone(&state_arc));
            {
                let tokio_handle = runtime_ctx_clone.tokio_handle();
                let Some(full_access_grant) = full_access_grant_or_mark_untrusted_error(
                    isolation_tier,
                    &state_arc,
                    &proc_id_clone,
                ) else {
                    return;
                };
                let full_ctx = RuntimeContextFullAccess::new(&node_context, full_access_grant);
                let mut guard = node_arc_clone.lock();

                tracing::info!(
                    "[{}] Calling setup (thread id={:?}, runtime={:?})",
                    proc_id_clone,
                    thread_id,
                    runtime,
                );
                let setup_result = run_setup_phase(runtime, &runtime_ctx_clone.gpu, || {
                    let _ = &tokio_handle;
                    guard.setup(&full_ctx)
                });
                if let Err(e) = setup_result {
                    tracing::error!("[{}] Setup failed: {}", proc_id_clone, e);
                    state_arc.fail_with_the_setup_refusal(&e);
                    return;
                }

                if let Err(e) =
                    refuse_a_port_setup_left_awaiting_its_device_stream_format(&guard, &node_type)
                {
                    tracing::error!("[{}] Setup failed: {}", proc_id_clone, e);
                    state_arc.fail_with(format!("setup failed: {e}"));
                    return;
                }

                tracing::info!("[{}] Setup completed successfully", proc_id_clone);
            }

            {
                let _no_pause_or_resume_between_the_gate_and_the_state = graph_arc_clone.read();
                state_arc.transition_to(
                    if pause_gate_inner.load(std::sync::atomic::Ordering::Acquire) {
                        NodeState::Paused
                    } else {
                        NodeState::Running
                    },
                );
            }

            // === PHASE 5: Process loop ===
            tracing::trace!(
                "[{}] Entering process loop (thread id={:?})",
                proc_id_clone,
                thread_id
            );
            run_node_loop(
                proc_id_clone,
                node_arc_clone,
                shutdown_rx,
                shutdown_wake_fd,
                state_arc,
                pause_gate_inner,
                step_progress,
                exec_config,
                node_context,
                isolation_tier,
            );
        })
        .map_err(|e| Error::Runtime(format!("Failed to spawn thread: {}", e)))?;

    // Attach thread handle
    {
        let mut graph = graph_arc.write();
        let node = graph
            .traversal_mut()
            .v(&node_run_id)
            .first_mut()
            .ok_or_else(|| Error::NodeNotFound(format!("Node '{}' not found", node_run_id)))?;
        node.insert(ThreadHandleComponent {
            join_handle: thread,
            kind: thread_kind,
        });
    }

    Ok(())
}

fn refuse_a_port_setup_left_awaiting_its_device_stream_format(
    node: &crate::core::nodes::NodeInstance,
    node_type: &streamlib_node_schema::NodeClassImportPath,
) -> Result<()> {
    let Some(input_inner) = node.iceoryx2_input_mailboxes_inner() else {
        return Ok(());
    };
    let Some(port) = input_inner
        .input_ports_still_awaiting_their_device_stream_format()
        .into_iter()
        .min()
    else {
        return Ok(());
    };
    Err(crate::iceoryx2::refuse_an_unsettled_match_device_sentinel(
        node_type, &port,
    ))
}

fn full_access_grant_or_mark_untrusted_error(
    isolation_tier: IsolationTier,
    state_arc: &Arc<ObservableNodeState>,
    node_run_id: &NodeRunId,
) -> Option<FullAccessGrant> {
    match isolation_tier.grant_full_access() {
        Some(grant) => Some(grant),
        None => {
            tracing::warn!(
                "[{}] Untrusted isolation tier ({}): in-process FullAccess denied by \
                 construction — refusing privileged setup() (belongs behind the \
                 subprocess sandbox)",
                node_run_id,
                isolation_tier.as_str(),
            );
            state_arc.fail_with(format!(
                "refused: untrusted isolation tier {} — privileged setup() belongs behind the \
                 subprocess sandbox",
                isolation_tier.as_str()
            ));
            None
        }
    }
}

fn run_setup_phase<F>(runtime: NodeRuntime, gpu: &GpuContext, setup_body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    let _ = gpu;
    match runtime {
        NodeRuntime::Rust | NodeRuntime::Python => setup_body(),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn clone_shutdown_wake_fd(
    channel: &ShutdownChannelComponent,
    proc_id: &NodeRunId,
) -> Option<OwnedFd> {
    match channel.try_clone_shutdown_wake_fd() {
        Ok(fd) => Some(fd),
        Err(e) => {
            tracing::warn!(
                "[{}] Failed to clone the shutdown wake descriptor, reactive runner will \
                 fall back to channel-only shutdown: {}",
                proc_id,
                e
            );
            None
        }
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn clone_shutdown_wake_fd(
    _channel: &ShutdownChannelComponent,
    _proc_id: &NodeRunId,
) -> Option<OwnedFd> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::nodes::NodeInstance;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn a_setup_that_settled_no_match_device_contract_is_refused_naming_the_port() {
        let node = a_device_matched_audio_consumer_whose_port_is(|mailboxes| {
            mailboxes.add_port_awaiting_its_device_stream_format(
                "audio",
                crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
                crate::iceoryx2::ReadMode::ReadNextInOrder,
            )
        });

        let refusal = refuse_a_port_setup_left_awaiting_its_device_stream_format(
            &node,
            &a_device_matched_node_type(),
        )
        .expect_err("nothing settled the sentinel")
        .to_string();

        assert!(
            refusal.contains("match_device")
                && refusal.contains("setup()")
                && refusal.contains("audio"),
            "the refusal must name the sentinel, where it resolves, and the port; got {refusal}"
        );
    }

    #[test]
    fn a_setup_that_settled_its_contract_is_not_refused() {
        let node = a_device_matched_audio_consumer_whose_port_is(|mailboxes| {
            mailboxes.add_port_awaiting_its_device_stream_format(
                "audio",
                crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
                crate::iceoryx2::ReadMode::ReadNextInOrder,
            );
            mailboxes
                .settle_a_ports_device_matched_audio_window_contract(
                    "audio",
                    &crate::iceoryx2::AudioWindowContractMatchingADeviceStream {
                        device_stream_format: crate::core::context::AudioStreamFormat {
                            sample_rate: 48_000,
                            channels: 2,
                            sample_format: crate::core::context::AudioSampleFormat::F32,
                        },
                        window_size_in_per_channel_samples: 480,
                        hop_in_per_channel_samples: 480,
                    },
                )
                .expect("a device format settles the contract");
        });

        refuse_a_port_setup_left_awaiting_its_device_stream_format(
            &node,
            &a_device_matched_node_type(),
        )
        .expect("a settled port is not a wiring error");
    }

    #[test]
    fn a_node_holding_no_input_mailboxes_is_not_refused() {
        use crate::core::nodes::GeneratedNode;

        crate::core::test_support::ensure_test_mocks_registered();
        let node = NodeInstance::new(Box::new(
            crate::core::test_support::MockOutputOnlyNode::Node::from_config(Default::default())
                .expect("the mock constructs from its default config"),
        ));

        refuse_a_port_setup_left_awaiting_its_device_stream_format(
            &node,
            &a_device_matched_node_type(),
        )
        .expect("a node with no input mailboxes has no sentinel to settle");
    }

    fn a_device_matched_node_type() -> streamlib_node_schema::NodeClassImportPath {
        crate::core::test_support::MockDeviceMatchedAudioConsumerNode::Node::node_class_import_path(
        )
    }

    /// The `match_device` mock, with its input mailboxes put into whatever state
    /// `arrange` leaves them in.
    fn a_device_matched_audio_consumer_whose_port_is(
        arrange: impl FnOnce(&crate::iceoryx2::InputMailboxesInner),
    ) -> NodeInstance {
        use crate::core::nodes::GeneratedNode;

        crate::core::test_support::ensure_test_mocks_registered();
        let mut node = NodeInstance::new(Box::new(
            crate::core::test_support::MockDeviceMatchedAudioConsumerNode::Node::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
        node.install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        arrange(
            &node
                .iceoryx2_input_mailboxes_inner()
                .expect("a windowed consumer holds input mailboxes"),
        );
        node
    }

    #[test]
    fn untrusted_tier_gate_refuses_and_marks_error() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Idle));
        let proc_id = NodeRunId::from("test.untrusted");
        let grant =
            full_access_grant_or_mark_untrusted_error(IsolationTier::Untrusted, &state, &proc_id);
        assert!(
            grant.is_none(),
            "untrusted tier must not yield a FullAccess grant"
        );
        assert_eq!(
            state.current(),
            NodeState::Error,
            "the refused untrusted setup must mark the node Error"
        );
    }

    #[test]
    fn trusted_tier_gate_yields_grant_and_leaves_state() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Idle));
        let proc_id = NodeRunId::from("test.trusted");
        let grant = full_access_grant_or_mark_untrusted_error(
            IsolationTier::TrustedInstalled,
            &state,
            &proc_id,
        );
        assert!(
            grant.is_some(),
            "trusted tier must yield a FullAccess grant"
        );
        assert_eq!(
            state.current(),
            NodeState::Idle,
            "the trusted path must not touch node state"
        );
    }

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!("{test_name}: no GPU device ({e}) — skipping");
                None
            }
        }
    }

    /// Regression for #867 — subprocess host setup must not hold the
    /// escalate gate against a concurrent escalate from the
    /// bridge's escalate worker.
    ///
    /// Reproduces the deadlock the engine fix prevents: a setup body
    /// (simulating a subprocess host's `__generated_setup` IPC wait)
    /// blocks on another thread that's trying to acquire its own
    /// `sandbox.escalate` (simulating the bridge's escalate worker). With
    /// the fix, `run_setup_phase` for `Python` / `TypeScript` skips
    /// the outer wrap so the concurrent escalate proceeds and the
    /// setup body completes. Mentally revert the runtime branch
    /// (always wrap in `sandbox.escalate`) — this test hangs past
    /// the 5-second timeout and fails.
    #[test]
    fn subprocess_host_setup_phase_does_not_block_concurrent_escalate() {
        const TEST: &str = "subprocess_host_setup_phase_does_not_block_concurrent_escalate";
        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let runtime = NodeRuntime::Python;
        let gpu_handle = gpu.clone();
        let result = run_setup_phase(runtime.clone(), &gpu, || {
            let sandbox = GpuContextLimitedAccess::new(gpu_handle);
            let (done_tx, done_rx) = mpsc::channel();
            thread::spawn(move || {
                let inner = sandbox.escalate(|_full| Ok::<(), Error>(()));
                let _ = done_tx.send(inner);
            });
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| {
                    Error::Runtime(format!(
                        "{TEST}: concurrent escalate did not complete within 5s \
                         (runtime={runtime:?}) — outer setup-phase wrap is holding \
                         node_setup_lock against the bridge's escalate worker \
                         (#867)"
                    ))
                })?
                .map_err(|e| Error::Runtime(format!("{TEST}: inner escalate failed: {e}")))
        });
        result.unwrap_or_else(|e| panic!("{TEST} (runtime={runtime:?}): {e}"));
    }

    /// Mirror of [`subprocess_host_setup_phase_does_not_block_concurrent_escalate`]
    /// for the Rust-runtime arm.
    ///
    /// The historical Rust-branch wrap held the escalate gate around
    /// every `setup_body`, so a concurrent escalate from another
    /// thread blocked until the body returned. [`run_setup_phase`] is
    /// a passthrough for every runtime, and a concurrent escalate
    /// from inside the body must complete promptly because no other
    /// gate-holder exists.
    ///
    /// Mentally revert the [`run_setup_phase`] change (re-add the
    /// outer `sandbox.escalate(...)` wrap) — this test hangs past
    /// the 500ms inner timeout because the wrap would re-acquire the
    /// gate held by the inner thread.
    #[test]
    fn rust_node_setup_phase_does_not_hold_gate_against_concurrent_escalate() {
        const TEST: &str = "rust_node_setup_phase_does_not_hold_gate_against_concurrent_escalate";
        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let gpu_handle = gpu.clone();
        let result = run_setup_phase(NodeRuntime::Rust, &gpu, || {
            let sandbox = GpuContextLimitedAccess::new(gpu_handle);
            let (done_tx, done_rx) = mpsc::channel();
            thread::spawn(move || {
                let inner = sandbox.escalate(|_full| Ok::<(), Error>(()));
                let _ = done_tx.send(inner);
            });
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| {
                    Error::Runtime(format!(
                        "{TEST}: concurrent escalate did not complete within 5s — \
                         run_setup_phase's Rust arm appears to be holding the \
                         escalate gate against a worker-thread escalate dispatch \
                         (regression of #1072 — gate management belongs in \
                         NodeInstance::setup, not run_setup_phase)"
                    ))
                })?
                .map_err(|e| Error::Runtime(format!("{TEST}: inner escalate failed: {e}")))
        });
        result.unwrap_or_else(|e| panic!("{TEST}: {e}"));
    }
}
