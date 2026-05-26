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
                    let _ = &tokio_handle; // block_on now happens inside the
                    // ProcessorInstance::setup dispatch — VTable variant calls
                    // through extern "C" (cdylib block_ons on its own tokio
                    // handle pulled from ctx), LegacyDyn variant block_ons
                    // here via the ctx's tokio handle.
                    guard.setup(&full_ctx)
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

/// Run a processor's `__generated_setup` body.
///
/// Gate management for Rust-runtime processors moved one layer
/// inward — `ProcessorInstance::setup` now wraps its own dispatch
/// (cdylib variant uses
/// [`RuntimeContextFullAccess::with_cdylib_scope`] so the cdylib
/// body sees a `ScopeToken`-flavored FullAccess instead of the
/// host-only Boxed shape; in-process variant uses the same
/// `gpu_limited_access().escalate(...)` shape this function used to
/// provide). The historical serialization-via-gate invariant is
/// preserved at the new layer; the outer wrap that used to live
/// here would now trigger the gate's same-thread re-entry panic
/// when an inner call (a cdylib's mint-scope-token or any setup
/// body that itself uses `.escalate(...)`) also tried to acquire
/// it.
///
/// Subprocess hosts continue to skip the outer wrap entirely (per
/// #867 — wrapping their IPC wait would deadlock against the
/// bridge-reader thread's per-call escalates).
fn run_setup_phase<F>(runtime: ProcessorRuntime, gpu: &GpuContext, setup_body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    let _ = gpu;
    match runtime {
        ProcessorRuntime::Rust
        | ProcessorRuntime::Python
        | ProcessorRuntime::TypeScript => setup_body(),
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

    /// Regression for #867 — subprocess host setup must not hold the
    /// escalate gate against a concurrent escalate from the
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

    /// Mirror of [`subprocess_host_setup_phase_does_not_block_concurrent_escalate`]
    /// for the Rust-runtime arm.
    ///
    /// The historical Rust-branch wrap held the escalate gate around
    /// every `setup_body`, so a concurrent escalate from another
    /// thread blocked until the body returned. That wrap moved one
    /// layer inward in #1072 — [`crate::core::processors::ProcessorInstance::setup`]
    /// now owns the gate management (escalate-wrap for `LegacyDyn`,
    /// scope-token mint via
    /// [`crate::core::context::escalate_scope_registry::begin_escalate_scope`]
    /// for `VTable`). [`run_setup_phase`] is therefore a passthrough
    /// for every runtime, and a concurrent escalate from inside the
    /// body must complete promptly because no other gate-holder
    /// exists.
    ///
    /// Mentally revert the [`run_setup_phase`] change (re-add the
    /// outer `sandbox.escalate(...)` wrap) — this test hangs past
    /// the 500ms inner timeout because the wrap would re-acquire the
    /// gate held by the inner thread.
    #[test]
    fn rust_processor_setup_phase_does_not_hold_gate_against_concurrent_escalate() {
        const TEST: &str =
            "rust_processor_setup_phase_does_not_hold_gate_against_concurrent_escalate";
        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let gpu_handle = gpu.clone();
        let result = run_setup_phase(ProcessorRuntime::Rust, &gpu, || {
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
                         ProcessorInstance::setup, not run_setup_phase)"
                    ))
                })?
                .map_err(|e| Error::Runtime(format!("{TEST}: inner escalate failed: {e}")))
        });
        result.unwrap_or_else(|e| panic!("{TEST}: {e}"));
    }
}
