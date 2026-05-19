// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

#[cfg(unix)]
use std::os::fd::OwnedFd;

use parking_lot::{Mutex, RwLock};

use crate::core::compiler::scheduling::{scheduling_strategy_for_processor, SchedulingStrategy};
use crate::core::context::{
    GpuContext, GpuContextLimitedAccess, RuntimeContext, RuntimeContextFullAccess,
};
use crate::core::descriptors::ProcessorRuntime;
use crate::core::error::{Result, Error};
use crate::core::execution::run_processor_loop;
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorInstanceComponent, ProcessorPauseGateComponent,
    ProcessorReadyBarrierComponent, ProcessorUniqueId, ShutdownChannelComponent, StateComponent,
    SubprocessHandleComponent, ThreadHandleComponent,
};
use crate::core::processors::{ProcessorInstanceFactory, ProcessorState, PROCESSOR_REGISTRY};

/// Spawn a processor thread.
///
/// The thread will:
/// 1. Create processor instance via factory
/// 2. Attach ProcessorInstanceComponent to graph
/// 3. Signal READY via barrier
/// 4. Wait for CONTINUE via barrier (wiring happens here)
/// 5. Call setup (no locks held - safe to call runtime ops)
/// 6. Run processor loop
#[tracing::instrument(name = "compiler.spawn_processor", skip(graph_arc, factory, runtime_ctx), fields(processor_id = processor_id.as_ref()))]
pub(crate) fn spawn_processor(
    graph_arc: Arc<RwLock<Graph>>,
    factory: &ProcessorInstanceFactory,
    runtime_ctx: &Arc<RuntimeContext>,
    processor_id: impl AsRef<str>,
) -> Result<()> {
    let processor_id = processor_id.as_ref();

    // Check if already has a thread or subprocess (already running)
    {
        let graph = graph_arc.read();
        let already_running = graph
            .traversal()
            .v(processor_id)
            .first()
            .map(|n| n.has::<ThreadHandleComponent>() || n.has::<SubprocessHandleComponent>())
            .unwrap_or(false);
        if already_running {
            return Ok(());
        }
    }

    // Check processor runtime to determine dispatch path
    let runtime = {
        let graph = graph_arc.read();
        let proc_type = graph
            .traversal()
            .v(processor_id)
            .first()
            .map(|n| n.processor_type().clone());
        proc_type
            .as_ref()
            .and_then(|ident| PROCESSOR_REGISTRY.descriptor(ident))
            .map(|d| d.runtime.clone())
            .unwrap_or(ProcessorRuntime::Rust)
    };

    // Extract barrier before spawning (needed for both paths)
    let (barrier_component, proc_id_clone) = {
        let mut graph = graph_arc.write();
        let node_mut = graph
            .traversal_mut()
            .v(processor_id)
            .first_mut()
            .ok_or_else(|| {
                Error::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
            })?;

        let barrier = node_mut
            .remove::<ProcessorReadyBarrierComponent>()
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "Processor '{}' has no ProcessorReadyBarrierComponent",
                    processor_id
                ))
            })?;

        (barrier, ProcessorUniqueId::from(processor_id))
    };

    // Same strategy resolution for all three runtimes — Python and TypeScript
    // are hosted by Rust subprocess processors whose host thread participates
    // in the same scheduling regime as native Rust processors.
    let strategy = {
        let graph = graph_arc.read();
        let node = graph.traversal().v(&proc_id_clone).first().ok_or_else(|| {
            Error::ProcessorNotFound(format!("Processor '{}' not found", proc_id_clone))
        })?;
        scheduling_strategy_for_processor(node)
    };

    let runtime_label = match runtime {
        ProcessorRuntime::Rust => "Rust processor",
        ProcessorRuntime::Python => "Python subprocess host",
        ProcessorRuntime::TypeScript => "Deno subprocess host",
    };
    tracing::info!(
        "[{}] Spawning {} with strategy: {}",
        processor_id,
        runtime_label,
        strategy.description()
    );

    match strategy {
        SchedulingStrategy::DedicatedThread { priority } => {
            spawn_dedicated_thread(
                graph_arc,
                factory,
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
    factory: &ProcessorInstanceFactory,
    runtime_ctx: &Arc<RuntimeContext>,
    processor_id: ProcessorUniqueId,
    priority: crate::core::execution::ThreadPriority,
    mut barrier: ProcessorReadyBarrierComponent,
    runtime: ProcessorRuntime,
) -> Result<()> {
    // Clone Arcs for thread
    let graph_arc_clone = Arc::clone(&graph_arc);
    let runtime_ctx_clone = Arc::clone(runtime_ctx);
    let proc_id_clone = processor_id.clone();

    // Create processor instance now (with lock) since factory needs node reference
    let processor_arc = {
        let graph = graph_arc.read();
        let node = graph.traversal().v(&processor_id).first().ok_or_else(|| {
            Error::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;
        let processor = factory.create(node)?;
        Arc::new(Mutex::new(processor))
    };

    let processor_arc_clone = Arc::clone(&processor_arc);

    // 4 MB stack — FramePayload is 128 KB inline (MAX_PAYLOAD_SIZE) and
    // multiple instances may be on the stack during IPC read/write operations.
    //
    // No `.name()` set on the Builder — Linux's `pthread_setname_np`
    // truncates at 15 chars and most apps that name threads use fixed
    // role names (Postgres `walwriter`, nginx `worker process`,
    // Chrome `v8.IO`), not unique-per-instance ones. Custom thread
    // naming for streamlib processors isn't worth the API surface;
    // tracing spans + the processor id in log lines provide the same
    // observability without OS-level truncation.
    let thread = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let current_thread = std::thread::current();
            let thread_id = current_thread.id();

            tracing::info!(
                "[{}] Thread started: id={:?}",
                proc_id_clone,
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

            #[cfg(target_os = "linux")]
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
            let (state_arc, shutdown_rx, shutdown_eventfd, pause_gate_inner, exec_config) = {
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

                let (shutdown_rx, shutdown_eventfd) = match node
                    .get_mut::<ShutdownChannelComponent>()
                {
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
                        let eventfd = clone_shutdown_eventfd(channel, &proc_id_clone);
                        (rx, eventfd)
                    }
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

                (
                    state,
                    shutdown_rx,
                    shutdown_eventfd,
                    pause_gate_inner,
                    exec_config,
                )
            }; // Lock released here

            // === PHASE 4: Setup ===
            // Create processor-specific context with both processor ID and pause gate
            let processor_context = runtime_ctx_clone
                .with_processor_id(proc_id_clone.clone())
                .with_pause_gate(pause_gate_inner.clone());
            {
                let tokio_handle = runtime_ctx_clone.tokio_handle();
                let full_ctx = RuntimeContextFullAccess::new(&processor_context);
                let mut guard = processor_arc_clone.lock();

                tracing::info!(
                    "[{}] Calling setup (thread id={:?}, runtime={:?})",
                    proc_id_clone,
                    thread_id,
                    runtime,
                );
                let setup_result = run_setup_phase(runtime, &runtime_ctx_clone.gpu, || {
                    tokio_handle.block_on(guard.__generated_setup(&full_ctx))
                });
                if let Err(e) = setup_result {
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
                "[{}] Entering process loop (thread id={:?})",
                proc_id_clone,
                thread_id
            );
            run_processor_loop(
                proc_id_clone,
                processor_arc_clone,
                shutdown_rx,
                shutdown_eventfd,
                state_arc,
                pause_gate_inner,
                exec_config,
                processor_context,
            );
        })
        .map_err(|e| Error::Runtime(format!("Failed to spawn thread: {}", e)))?;

    // Attach thread handle
    {
        let mut graph = graph_arc.write();
        let node = graph
            .traversal_mut()
            .v(&processor_id)
            .first_mut()
            .ok_or_else(|| {
                Error::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
            })?;
        node.insert(ThreadHandleComponent(thread));
    }

    Ok(())
}

/// Run a processor's `__generated_setup` body under the right lock
/// discipline for its `runtime`.
fn run_setup_phase<F>(runtime: ProcessorRuntime, gpu: &GpuContext, setup_body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    match runtime {
        // Rust setup may directly create video sessions / DPB images /
        // swapchain; the wrap serializes that work via the setup mutex
        // and waits device idle on exit (#304).
        ProcessorRuntime::Rust => {
            let sandbox = GpuContextLimitedAccess::new(gpu.clone());
            sandbox.escalate(|_full_gpu| setup_body())
        }
        // Subprocess host setup does no host-side GPU work — it spawns
        // the child, constructs the bridge, sends a `setup` lifecycle,
        // then blocks on the subprocess's `ready` reply. Wrapping that
        // IPC wait in escalate would hold `processor_setup_lock` against
        // every FullAccess escalate the subprocess issues during its own
        // init: the bridge-reader thread dispatches each
        // `escalate_request` inline through `sandbox.escalate(|full|
        // ...)` and deadlocks on the same mutex. Per-call bridge-handler
        // escalates still acquire the lock + wait device idle on their
        // own, so GPU-resource serialization is preserved (#867).
        ProcessorRuntime::Python | ProcessorRuntime::TypeScript => setup_body(),
    }
}

#[cfg(target_os = "linux")]
fn clone_shutdown_eventfd(
    channel: &ShutdownChannelComponent,
    proc_id: &ProcessorUniqueId,
) -> Option<OwnedFd> {
    match channel.try_clone_shutdown_eventfd() {
        Ok(fd) => Some(fd),
        Err(e) => {
            tracing::warn!(
                "[{}] Failed to clone shutdown eventfd, reactive runner will fall back \
                 to channel-only shutdown: {}",
                proc_id,
                e
            );
            None
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn clone_shutdown_eventfd(
    _channel: &ShutdownChannelComponent,
    _proc_id: &ProcessorUniqueId,
) -> Option<OwnedFd> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::GpuContext;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!("{test_name}: no GPU device ({e}) — skipping");
                None
            }
        }
    }

    /// Regression for #867 — subprocess host setup must not hold
    /// `processor_setup_lock` against a concurrent escalate from the
    /// bridge-reader thread.
    ///
    /// Reproduces the deadlock the engine fix prevents: a setup body
    /// (simulating a subprocess host's `__generated_setup` IPC wait)
    /// blocks on another thread that's trying to acquire its own
    /// `sandbox.escalate` (simulating a bridge-reader handler). With
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

        for runtime in [ProcessorRuntime::Python, ProcessorRuntime::TypeScript] {
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
                             processor_setup_lock against bridge-reader-thread escalate \
                             dispatch (#867)"
                        ))
                    })?
                    .map_err(|e| Error::Runtime(format!("{TEST}: inner escalate failed: {e}")))
            });
            result.unwrap_or_else(|e| panic!("{TEST} (runtime={runtime:?}): {e}"));
        }
    }

    /// Positive control: Rust processors STILL wrap setup in
    /// `sandbox.escalate`, so a concurrent escalate from another
    /// thread is correctly serialized against the outer wrap. Locks
    /// the asymmetry: the fix is targeted at subprocess hosts only.
    /// Verifies both halves — the inner escalate is blocked while
    /// the outer body is running, AND the inner escalate succeeds
    /// once the outer body returns.
    #[test]
    fn rust_processor_setup_phase_holds_setup_lock_against_concurrent_escalate() {
        const TEST: &str = "rust_processor_setup_phase_holds_setup_lock_against_concurrent_escalate";
        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let gpu_handle = gpu.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let inner_thread = {
            let sandbox = GpuContextLimitedAccess::new(gpu_handle);
            thread::spawn(move || {
                let inner = sandbox.escalate(|_full| Ok::<(), Error>(()));
                let _ = done_tx.send(inner);
            })
        };

        let outer_result: Result<()> = run_setup_phase(ProcessorRuntime::Rust, &gpu, || {
            // The Rust-branch wrap holds the setup lock here, so the
            // inner escalate from the spawned thread must NOT
            // complete until this body returns and the outer escalate
            // releases.
            match done_rx.recv_timeout(Duration::from_millis(500)) {
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::Runtime(
                    format!("{TEST}: inner thread dropped without responding"),
                )),
                Ok(_) => Err(Error::Runtime(format!(
                    "{TEST}: inner escalate completed while the outer Rust wrap was \
                     supposed to be holding processor_setup_lock — Rust-branch wrap \
                     regression"
                ))),
            }
        });
        outer_result.unwrap();

        // After the outer wrap releases, the spawned inner escalate
        // must now succeed within bounded time — closes the asymmetry
        // by proving the lock was held briefly, not permanently.
        let inner_result = done_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| {
                panic!("{TEST}: inner escalate did not complete within 5s of outer release")
            });
        inner_result.unwrap_or_else(|e| {
            panic!("{TEST}: inner escalate eventually failed: {e}")
        });
        inner_thread.join().unwrap_or_else(|_| {
            panic!("{TEST}: spawned inner-escalate thread panicked")
        });
    }
}
