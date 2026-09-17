// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

use super::RuntimeOperations;
use super::RuntimeStatus;
use super::RuntimeUniqueId;
use super::StreamlibRuntimeDirectory;
use super::graph_change_listener::GraphChangeListener;
use crate::core::compiler::{Compiler, PendingOperation};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use crate::core::context::SoftwareAudioClock;
use crate::core::context::{
    AudioClockConfig, GpuContext, RuntimeContext, SharedAudioClock, TimeContext,
};
use crate::core::graph::{
    GraphNodeWithComponents, GraphState, LinkUniqueId, ObservableGraphReadiness,
    ProcessorPauseGateComponent, ProcessorUniqueId, StateComponent,
};
use crate::core::json_schema::LoadedCapabilityExtensionOutput;
use crate::core::processors::ProcessorSpec;
use crate::core::processors::ProcessorState;
use crate::core::pubsub::{Event, EventListener, PUBSUB, ProcessorEvent, RuntimeEvent, topics};
use crate::core::runtime::LoadedCapabilityExtensionRegistry;
use crate::core::signals::ScopedShutdownSignalOwnership;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};
use crate::iceoryx2::Iceoryx2Node;

/// Storage variant for tokio runtime in Runner.
///
/// Enables Runner to work both standalone (owning its runtime) and
/// integrated into existing tokio applications (using the current handle).
pub(crate) enum TokioRuntimeVariant {
    /// Runner owns the tokio Runtime (created when NOT in tokio context).
    OwnedTokioRuntime(TokioRuntimeShutDownWithinItsBudget),
    /// Runner uses an external tokio Handle (auto-detected when called from tokio context).
    ExternalTokioHandle(tokio::runtime::Handle),
}

impl TokioRuntimeVariant {
    /// Get a tokio Handle from either variant.
    pub(crate) fn handle(&self) -> tokio::runtime::Handle {
        match self {
            TokioRuntimeVariant::OwnedTokioRuntime(rt) => rt.handle().clone(),
            TokioRuntimeVariant::ExternalTokioHandle(h) => h.clone(),
        }
    }
}

/// How long dropping an owned tokio runtime waits for its tasks to stop.
///
/// A plain drop waits for every `spawn_blocking` task to return, without
/// limit, so one blocking call that never returns hangs the engine's teardown.
const OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// A tokio runtime the engine owns, shut down within
/// [`OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET`] when dropped.
pub(crate) struct TokioRuntimeShutDownWithinItsBudget(
    std::mem::ManuallyDrop<tokio::runtime::Runtime>,
);

impl TokioRuntimeShutDownWithinItsBudget {
    pub(crate) fn owning(runtime: tokio::runtime::Runtime) -> Self {
        Self(std::mem::ManuallyDrop::new(runtime))
    }

    pub(crate) fn handle(&self) -> &tokio::runtime::Handle {
        self.0.handle()
    }

    pub(crate) fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.0.block_on(future)
    }
}

impl Drop for TokioRuntimeShutDownWithinItsBudget {
    fn drop(&mut self) {
        crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
            "the engine's tokio runtime",
        );
        // SAFETY: taken once, here, as this value is dropped; nothing reads the
        // runtime afterwards.
        let runtime = unsafe { std::mem::ManuallyDrop::take(&mut self.0) };
        runtime.shutdown_timeout(OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET);
    }
}

/// The main stream processing runtime.
///
/// # Thread Safety
///
/// `Runner` is designed for concurrent access from multiple threads.
/// All public methods take `&self` (not `&mut self`), allowing the runtime
/// to be shared via `Arc<Runner>` without external synchronization.
///
/// Internal state uses fine-grained locking:
/// - Graph operations: `RwLock` (multiple readers OR one writer)
/// - Pending operations: `Mutex` (batched for compilation)
/// - Status: `Mutex` (lifecycle state)
/// - Runtime context: `Mutex<Option<...>>` (created on start, cleared on stop)
///
/// This means multiple threads can concurrently call `add_processor()`,
/// `connect()`, etc. without blocking each other on an outer lock.
pub struct Runner {
    /// Unique identifier for this runtime instance.
    pub(crate) runtime_id: Arc<RuntimeUniqueId>,
    /// Tokio runtime storage - either owned or external handle.
    pub(crate) tokio_runtime_variant: TokioRuntimeVariant,
    /// Compiles graph changes into running processors. Owns the graph and transaction.
    pub(crate) compiler: Arc<Compiler>,
    /// Runtime context (GPU, audio config). Created on start(), cleared on stop().
    /// Using Mutex<Option<...>> allows restart cycles with fresh context each time.
    pub(crate) runtime_context: Arc<Mutex<Option<Arc<RuntimeContext>>>>,
    /// Runtime lifecycle status. Protected by Mutex for interior mutability.
    pub(crate) status: Arc<Mutex<RuntimeStatus>>,
    /// Listener for graph changes that triggers compilation.
    /// Stored to keep subscription alive for runtime lifetime.
    _graph_change_listener: Arc<Mutex<dyn EventListener>>,
    /// iceoryx2 Node for creating Services, Publishers, and Subscribers.
    /// Created in new(); cloned into the RuntimeContext during start().
    pub(crate) iceoryx2_node: Iceoryx2Node,
    /// Per-runtime surface-sharing service. Bound to a unique Unix socket in
    /// `new()`; polyglot subprocesses connect to it via the
    /// `STREAMLIB_SURFACE_SOCKET` env var. Wrapped in `Mutex<Option<...>>`
    /// so `stop()` can drop it deterministically; the `Drop` impl on
    /// `UnixSocketSurfaceService` removes the socket file.
    #[cfg(target_os = "linux")]
    pub(crate) surface_service:
        Arc<Mutex<Option<crate::linux::surface_share::UnixSocketSurfaceService>>>,
    /// Path of the per-runtime surface-sharing socket, inside the runtime directory.
    #[cfg(target_os = "linux")]
    pub(crate) surface_socket_path: std::path::PathBuf,
    /// The runtime directory this runtime resolved as it started.
    pub(crate) runtime_directory: StreamlibRuntimeDirectory,
    /// The surfaces cross-process consumers currently hold checked out, owned
    /// by the service above and read by the pixel-buffer pool through the
    /// `SurfaceStore` `start()` hands it. Held here because the service is
    /// brought up in `new()` and the store is built in `start()`.
    #[cfg(target_os = "linux")]
    pub(crate) surface_check_out_leases: Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
    /// Logging guard — keeps the drain worker alive for the runtime's
    /// lifetime. On drop, flushes buffered JSONL records and
    /// `fdatasync`s the log file.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    _logging_guard: crate::core::logging::StreamlibLoggingGuard,
    /// Engine-extension hooks invoked exactly once during [`Self::start`],
    /// after the [`GpuContext`] is initialized and before any
    /// processor's `setup()` runs, for engine extensions whose
    /// construction needs the live GpuContext but whose registration must
    /// precede the first `process()` call. Drained on each `start()`.
    setup_hooks: Arc<Mutex<Vec<Box<dyn FnOnce(&GpuContext) -> Result<()> + Send>>>>,
    /// Optional pipeline name carried across snapshot load → save.
    /// Set by [`Self::load_graph_snapshot`] and read by
    /// [`Self::save_graph_snapshot`] so a snapshot loaded from disk
    /// can be re-saved with the same `name` without caller bookkeeping.
    /// `None` when the graph was built imperatively without a name.
    pipeline_name: Arc<Mutex<Option<String>>>,
}

impl Runner {
    pub fn new() -> Result<Arc<Self>> {
        // Cap per-thread timer slack at 1 ns on the calling thread before
        // spawning any worker. Linux defaults to 50 µs grouping for
        // `epoll_wait` / `nanosleep` / `futex` relative timeouts; new
        // threads inherit the creator's slack at clone time, so setting it
        // here propagates to the tokio worker pool, the logging drain
        // worker, the iceoryx2 node, and every processor thread spawned
        // later. SCHED_FIFO/RR threads (rtkit-promoted reactive processors)
        // bypass slack entirely per kernel design — this only affects
        // SCHED_OTHER waits. Same call QEMU has shipped in production
        // since 2013. Cannot fail for self per `prctl(2)`.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::prctl(libc::PR_SET_TIMERSLACK, 1u64, 0u64, 0u64, 0u64);
        }

        // Auto-detect tokio context FIRST — telemetry exporters need a Tokio handle.
        // If inside tokio runtime: use current handle (external handle mode)
        // If outside tokio runtime: create owned runtime
        let tokio_runtime_variant = match tokio::runtime::Handle::try_current() {
            Ok(handle) => TokioRuntimeVariant::ExternalTokioHandle(handle),
            Err(_) => {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        Error::Runtime(format!("Failed to create tokio runtime: {}", e))
                    })?;
                TokioRuntimeVariant::OwnedTokioRuntime(TokioRuntimeShutDownWithinItsBudget::owning(
                    rt,
                ))
            }
        };

        // Load a local .env if present (RUST_LOG and other dev overrides).
        let _ = dotenvy::dotenv();

        // The id names the log file opened below, so a pinned one is refused
        // before the runtime writes anything.
        let runtime_id = Arc::new(RuntimeUniqueId::from_env_or_generate()?);

        // Stand up the runtime's unified logging pathway: `tracing` →
        // bounded lossy channel → drain worker → line-buffered pretty
        // stdout + batched JSONL file at
        // `<STREAMLIB_HOME>/.streamlib/logs/<runtime_id>-<started_at>.jsonl`.
        // See `docs/logging-schema.md` for the schema (the durable
        // interface contract) and `streamlib::sdk::logging` for the
        // implementation.
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
        let _logging_guard =
            crate::core::logging::init(crate::core::logging::StreamlibLoggingConfig::for_runtime(
                format!("runtime:{}", runtime_id),
                Arc::clone(&runtime_id),
            ))
            .map_err(|e| Error::Runtime(format!("Failed to initialize logging: {}", e)))?;
        tracing::info!("Creating Runner with ID: {}", runtime_id);

        let runtime_directory = StreamlibRuntimeDirectory::resolve()?;
        tracing::info!(
            "StreamLib runtime directory: {}",
            runtime_directory.path().display()
        );

        // Get STREAMLIB_HOME and run init hooks (once per process)
        let streamlib_home = crate::core::streamlib_home::get_streamlib_home();
        tracing::debug!("STREAMLIB_HOME: {}", streamlib_home.display());
        crate::core::runtime_hooks::run_init_hooks(&streamlib_home)?;

        // The engine substrate is empty by construction — there are no
        // compile-time-linked processors. Callers populate the
        // `PROCESSOR_REGISTRY` after `Runner::new()` returns, by calling
        // `PROCESSOR_REGISTRY.register::<P>()` in process.

        // Bridge iceoryx2's internal log records into streamlib tracing
        // before creating the iceoryx2 Node so any iceoryx2 emit at
        // construction time lands in the unified JSONL pipeline.
        crate::core::logging::install_iceoryx2_log_bridge_at_the_engines_configured_level();

        // Bring up the per-runtime surface-sharing service. Each runtime owns
        // a unique Unix socket in the runtime directory that its polyglot
        // subprocesses connect to via STREAMLIB_SURFACE_SOCKET. Binding it is
        // also the refusal of a second live runtime with this id, so it runs
        // before the runtime creates its iceoryx2 node.
        #[cfg(target_os = "linux")]
        let (surface_service, surface_socket_path, surface_check_out_leases) =
            bring_up_surface_service(&runtime_directory, &runtime_id)?;

        tracing::info!("[new] Creating iceoryx2 Node...");
        let iceoryx2_node = Iceoryx2Node::new(
            &runtime_directory.iceoryx2_domain_root(),
            &format!("streamlib-runtime/{runtime_id}"),
        )?;
        tracing::info!("[new] iceoryx2 Node created");

        // Create Arc-wrapped components
        let compiler = Arc::new(Compiler::new());
        let runtime_context = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(RuntimeStatus::Initial));

        // Create listener with cloned Arc references
        let listener = GraphChangeListener::new(
            Arc::clone(&status),
            Arc::clone(&runtime_context),
            Arc::clone(&compiler),
        );
        let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(listener));

        // Subscribe to graph changes
        PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener))?;

        Ok(Arc::new(Self {
            runtime_id,
            tokio_runtime_variant,
            compiler,
            runtime_context,
            status,
            _graph_change_listener: listener,
            iceoryx2_node,
            #[cfg(target_os = "linux")]
            surface_service,
            #[cfg(target_os = "linux")]
            surface_socket_path,
            runtime_directory,
            #[cfg(target_os = "linux")]
            surface_check_out_leases,
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
            _logging_guard,
            setup_hooks: Arc::new(Mutex::new(Vec::new())),
            pipeline_name: Arc::new(Mutex::new(None)),
        }))
    }

    /// Register a one-shot hook to run during [`Self::start`], after the
    /// [`GpuContext`] is initialized and before any processor's
    /// `setup()` runs. The hook receives the live `Arc<GpuContext>`,
    /// giving caller code a window to register engine extensions before
    /// any processor runs. Hooks fire FIFO; a hook returning `Err` aborts
    /// `start()` with the same error.
    pub fn install_setup_hook<F>(&self, hook: F)
    where
        F: FnOnce(&GpuContext) -> Result<()> + Send + 'static,
    {
        self.setup_hooks.lock().push(Box::new(hook));
    }

    /// Path of the per-runtime surface-sharing Unix socket.
    ///
    /// Bound during [`Runner::new`] at
    /// `<runtime directory>/surface-share-<runtime_id>.sock`. Polyglot
    /// subprocesses spawned by this runtime inherit this path via the
    /// `STREAMLIB_SURFACE_SOCKET` env var so their `streamlib-surface-client`
    /// connects to the runtime-internal service.
    #[cfg(target_os = "linux")]
    pub fn surface_socket_path(&self) -> &std::path::Path {
        &self.surface_socket_path
    }

    /// Unique identifier for this runtime instance.
    pub fn runtime_id(&self) -> &RuntimeUniqueId {
        &self.runtime_id
    }

    /// This runtime's iceoryx2 node.
    pub fn iceoryx2_node(&self) -> &Iceoryx2Node {
        &self.iceoryx2_node
    }

    /// Path of the JSONL log file this runtime is writing to, if any.
    /// Returns `None` on platforms where the logging pathway is not
    /// installed, or when the caller opted out of JSONL output.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    pub fn jsonl_log_path(&self) -> Option<&std::path::Path> {
        self._logging_guard.jsonl_path()
    }

    /// Update a processor's configuration at runtime.
    pub fn update_processor_config<C: Serialize>(
        &self,
        processor_id: &ProcessorUniqueId,
        config: C,
    ) -> Result<()> {
        let config_json =
            serde_json::to_value(&config).map_err(|e| crate::core::Error::Config(e.to_string()))?;

        self.compiler.scope(|_graph, tx| {
            tx.log(PendingOperation::UpdateProcessorConfig {
                processor_id: processor_id.clone(),
                config_to_apply: config_json,
            });
        });

        // Notify listeners that graph changed (triggers commit via GraphChangeListener)
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
        );

        Ok(())
    }

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Start the runtime.
    ///
    /// Takes `&Arc<Self>` to allow passing the runtime to processors via RuntimeContext.
    /// Processors can then call runtime operations directly without indirection.
    #[tracing::instrument(name = "runtime.start", skip_all)]
    pub fn start(self: &Arc<Self>) -> Result<()> {
        *self.status.lock() = RuntimeStatus::Starting;
        tracing::info!("[start] Starting runtime");
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarting),
        );

        // Initialize GPU context FIRST, before any platform app setup.
        // wgpu's Metal backend uses async operations that need to complete
        // before NSApplication configuration changes thread behavior.
        // Always create fresh context on start - enables tracking per session.
        tracing::info!("[start] Initializing GPU context...");
        let gpu = GpuContext::init_for_platform_sync()?;
        tracing::info!("[start] GPU context initialized");

        // Initialize SurfaceStore for cross-process GPU surface sharing (macOS only)
        #[cfg(target_os = "macos")]
        {
            use crate::core::context::SurfaceStore;

            if let Ok(xpc_service_name) = std::env::var("STREAMLIB_XPC_SERVICE_NAME") {
                tracing::info!(
                    "[start] Initializing SurfaceStore with XPC service '{}'...",
                    xpc_service_name
                );
                let surface_store =
                    SurfaceStore::new(xpc_service_name, self.runtime_id.to_string());
                if let Err(e) = surface_store.connect() {
                    tracing::warn!(
                        "[start] SurfaceStore XPC connection failed (surface sharing disabled): {}",
                        e
                    );
                } else {
                    gpu.set_surface_store(surface_store);
                    tracing::info!("[start] SurfaceStore initialized");
                }
            } else {
                tracing::debug!(
                    "[start] STREAMLIB_XPC_SERVICE_NAME not set, surface sharing disabled"
                );
            }
        }

        // Initialize SurfaceStore for cross-process GPU surface sharing (Linux).
        // Connects to the runtime-internal surface-sharing service that
        // `new()` already brought up — fail fast if the connection fails,
        // because the service is guaranteed to be running.
        #[cfg(target_os = "linux")]
        {
            use crate::core::context::SurfaceStore;

            let socket_path = self.surface_socket_path.to_string_lossy().to_string();
            tracing::info!(
                "[start] Initializing SurfaceStore against runtime-internal Unix socket '{}'...",
                socket_path
            );
            // `SurfaceStore::new` constructs the handle from a fresh
            // `Arc<SurfaceStoreInner>`.
            let surface_store = SurfaceStore::new_reading_check_out_leases(
                socket_path.clone(),
                self.runtime_id.to_string(),
                Arc::clone(&self.surface_check_out_leases),
            );
            surface_store.connect().map_err(|e| {
                Error::Runtime(format!(
                    "Failed to connect to runtime-internal surface-sharing service at {}: {}",
                    socket_path, e
                ))
            })?;
            gpu.set_surface_store(surface_store);
            tracing::info!("[start] SurfaceStore initialized against runtime-internal broker");
        }

        // Drain pre-start hooks now — after the GpuContext is FULLY live
        // (device + SurfaceStore) but before any processor setup runs.
        // Adapter bridges and surface registrations happen here so
        // processors that issue escalate ops or `resolve_surface` lookups
        // in their first `process()` find everything already in place.
        let hooks: Vec<Box<dyn FnOnce(&GpuContext) -> Result<()> + Send>> = {
            let mut guard = self.setup_hooks.lock();
            std::mem::take(&mut *guard)
        };
        if !hooks.is_empty() {
            tracing::info!("[start] Running {} setup hook(s)", hooks.len());
            for hook in hooks {
                hook(&gpu)?;
            }
        }

        // Create shared timing context - clock starts now
        let time = Arc::new(TimeContext::new());

        let iceoryx2_node = self.iceoryx2_node.clone();

        // Create audio clock - platform-specific for best precision. It paces
        // deviceless audio only, so whatever needs it is what starts it — a
        // graph with no audio in it never runs the timer.
        let audio_clock_config = AudioClockConfig::default();
        let audio_clock: SharedAudioClock = {
            #[cfg(target_os = "macos")]
            {
                tracing::info!(
                    "[start] Creating CoreAudioClock (GCD): {}Hz, {} samples/tick",
                    audio_clock_config.sample_rate,
                    audio_clock_config.buffer_size
                );
                Arc::new(crate::apple::CoreAudioClock::new(audio_clock_config))
            }
            #[cfg(target_os = "linux")]
            {
                tracing::info!(
                    "[start] Creating LinuxTimerFdAudioClock: {}Hz, {} samples/tick",
                    audio_clock_config.sample_rate,
                    audio_clock_config.buffer_size
                );
                Arc::new(crate::linux::LinuxTimerFdAudioClock::new(
                    audio_clock_config,
                ))
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                tracing::info!(
                    "[start] Creating SoftwareAudioClock: {}Hz, {} samples/tick",
                    audio_clock_config.sample_rate,
                    audio_clock_config.buffer_size
                );
                Arc::new(SoftwareAudioClock::new(audio_clock_config))
            }
        };

        // Pass runtime directly to RuntimeContext. Processors call runtime operations
        // directly - this is safe because processor lifecycle methods (setup, process)
        // run on their own threads with no locks held.
        let runtime_ops: Arc<dyn RuntimeOperations> =
            Arc::clone(self) as Arc<dyn RuntimeOperations>;
        let runtime_ctx = Arc::new(RuntimeContext::new(
            gpu,
            time,
            Arc::clone(&self.runtime_id),
            runtime_ops,
            self.tokio_runtime_variant.handle(),
            iceoryx2_node,
            Arc::clone(&audio_clock),
            self.runtime_directory.clone(),
            #[cfg(target_os = "linux")]
            self.surface_socket_path.clone(),
        ));
        *self.runtime_context.lock() = Some(Arc::clone(&runtime_ctx));

        // Platform-specific setup (macOS NSApplication, Windows Win32, etc.)
        // RuntimeContext handles all platform-specific details internally.
        runtime_ctx.ensure_platform_ready()?;

        // Set graph state to Running
        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Running);
        });

        // Mark runtime as started so commit will actually compile
        *self.status.lock() = RuntimeStatus::Started;

        // Compile any pending changes directly (includes Phase 4: START)
        // This ensures all queued operations are processed before start() returns.
        // After this, GraphChangeListener handles commits asynchronously.
        tracing::info!("[start] Committing pending graph operations");
        self.compiler.commit(&runtime_ctx)?;

        tracing::info!("[start] Runtime started (platform verified)");
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted),
        );

        Ok(())
    }

    /// Stop the runtime.
    ///
    /// Runs every step of the teardown even when removing the processors
    /// failed — a processor thread abandoned past its budget included — and
    /// reports that failure once the rest is down.
    #[tracing::instrument(name = "runtime.stop", skip_all)]
    pub fn stop(&self) -> Result<()> {
        // Idempotent, and claimed under one lock acquisition so two concurrent
        // callers cannot both pass the check: the run loop stops the runtime
        // itself and an embedding host tears down afterwards, and without this
        // every subscriber sees the Stopping/Stopped pair twice.
        {
            let mut runtime_status = self.status.lock();
            if *runtime_status == RuntimeStatus::Stopped {
                tracing::debug!("[stop] Already stopped");
                return Ok(());
            }
            *runtime_status = RuntimeStatus::Stopping;
        }

        tracing::info!("[stop] Beginning graceful shutdown");
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopping),
        );

        // Queue removal of all processors and commit
        let runtime_ctx = self.runtime_context.lock().clone();
        let processor_count = self.compiler.scope(|graph, tx| {
            let processor_ids: Vec<ProcessorUniqueId> = graph.traversal().v(()).ids();
            let count = processor_ids.len();
            for proc_id in processor_ids {
                tx.log(PendingOperation::RemoveProcessor(proc_id));
            }
            graph.set_state(GraphState::Idle);
            count
        });
        tracing::info!("[stop] Queued removal of {} processor(s)", processor_count);

        let mut processor_removal_outcome = Ok(());
        if let Some(ctx) = runtime_ctx {
            tracing::debug!("[stop] Committing processor teardown");
            processor_removal_outcome = self.compiler.commit(&ctx);
            if let Err(removal_failure) = &processor_removal_outcome {
                tracing::error!("[stop] Removing the processors failed: {removal_failure}");
            }
            tracing::debug!("[stop] Processor teardown complete");

            crate::core::runtime::note_what_the_engine_teardown_is_waiting_on("the audio clock");
            tracing::debug!("[stop] Stopping audio clock");
            if let Err(e) = ctx.audio_clock().stop() {
                tracing::warn!("[stop] Failed to stop audio clock: {}", e);
            }

            // Cleanup SurfaceStore - releases all surfaces and disconnects
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
                    "the GPU context's surface store",
                );
                ctx.gpu.clear_surface_store();
                tracing::debug!("[stop] SurfaceStore cleared");
            }
        }

        // Clear runtime context - allows fresh context on next start().
        // This enables per-session tracking (e.g., AI agents analyzing runtime state).
        *self.runtime_context.lock() = None;
        tracing::debug!("[stop] Runtime context cleared");

        // Tear down the per-runtime surface-sharing service. The Drop impl
        // on UnixSocketSurfaceService also stops it, but doing it here makes
        // the socket file disappear before stop() returns — important for
        // tests that immediately re-bind a new runtime on the same path.
        #[cfg(target_os = "linux")]
        {
            crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
                "the surface-sharing service",
            );
            if let Some(mut svc) = self.surface_service.lock().take() {
                svc.stop();
                tracing::debug!(
                    "[stop] Runtime-internal surface-sharing service stopped at {}",
                    self.surface_socket_path.display()
                );
            }
        }

        *self.status.lock() = RuntimeStatus::Stopped;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped),
        );

        tracing::info!("[stop] Graceful shutdown complete");
        processor_removal_outcome
    }

    /// Hand fds 1 and 2 back to the process now, for a caller about to leave
    /// this engine alive rather than drop it.
    pub fn stop_intercepting_the_standard_streams(&self) {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
        self._logging_guard.stop_intercepting_the_standard_streams();
    }

    /// The processors whose threads were abandoned past their shutdown budget
    /// and have not returned since. Each one holds this engine alive.
    pub fn processor_threads_abandoned_and_still_running(
        &self,
    ) -> Vec<crate::core::runtime::ProcessorDisplayNameAndId> {
        self.compiler
            .processor_threads_abandoned_and_still_running()
    }

    // =========================================================================
    // Per-Processor Pause/Resume
    // =========================================================================

    /// Pause a specific processor.
    pub fn pause_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        self.compiler.scope(|graph, _tx| {
            // Validate processor exists
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                Error::Runtime(format!(
                    "Processor '{}' has no ProcessorPauseGate",
                    processor_id
                ))
            })?;

            // Check if already paused
            if pause_gate.is_paused() {
                return Ok(()); // Already paused, no-op
            }

            // Set the pause gate
            pause_gate
                .clone_inner()
                .store(true, std::sync::atomic::Ordering::Release);

            // Update processor state
            if let Some(state) = node.get::<crate::core::graph::StateComponent>() {
                state.transition_to(ProcessorState::Paused);
            }

            // Publish event
            let event = Event::processor(processor_id, ProcessorEvent::Paused);
            PUBSUB.publish(&event.topic(), &event);

            tracing::info!("[{}] Processor paused", processor_id);
            Ok(())
        })
    }

    /// Resume a specific processor.
    pub fn resume_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        self.compiler.scope(|graph, _tx| {
            // Validate processor exists
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                Error::Runtime(format!(
                    "Processor '{}' has no ProcessorPauseGate",
                    processor_id
                ))
            })?;

            // Check if already running
            if !pause_gate.is_paused() {
                return Ok(()); // Already running, no-op
            }

            // Clear the pause gate
            pause_gate
                .clone_inner()
                .store(false, std::sync::atomic::Ordering::Release);

            // Update processor state
            if let Some(state) = node.get::<crate::core::graph::StateComponent>() {
                state.transition_to(ProcessorState::Running);
            }

            // Publish event
            let event = Event::processor(processor_id, ProcessorEvent::Resumed);
            PUBSUB.publish(&event.topic(), &event);

            tracing::info!("[{}] Processor resumed", processor_id);
            Ok(())
        })
    }

    /// Check if a specific processor is paused.
    pub fn is_processor_paused(&self, processor_id: &ProcessorUniqueId) -> Result<bool> {
        self.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node
                .get::<ProcessorPauseGateComponent>()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            Ok(pause_gate.is_paused())
        })
    }

    // =========================================================================
    // Graph readiness
    // =========================================================================

    /// Take hold of every processor's state, to wait on without the graph.
    ///
    /// The graph lock is released before anything blocks on what it hands
    /// back, which is the whole point: the transitions being waited for are
    /// made by processor threads that need that lock.
    pub fn observable_graph_readiness(&self) -> ObservableGraphReadiness {
        ObservableGraphReadiness::new(self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(())
                .iter()
                .filter_map(|node| {
                    Some((node.id.clone(), node.get::<StateComponent>()?.clone_inner()))
                })
                .collect()
        }))
    }

    /// Block until every processor in the graph has finished `setup` and
    /// reached `Running`, giving up after `timeout`.
    ///
    /// This is what "the graph is up" means for a processor in a helper
    /// process: its `setup` is the call that waits for the child to register
    /// and wire its ports, so a publisher that starts only after this returns
    /// cannot lose bags to a link nobody has attached to yet.
    pub fn wait_until_every_processor_is_running(&self, timeout: Duration) -> Result<()> {
        self.observable_graph_readiness()
            .wait_until_every_processor_is_running(timeout)
    }

    // =========================================================================
    // Runtime-level Pause/Resume (all processors)
    // =========================================================================

    /// Pause the runtime (all processors).
    pub fn pause(&self) -> Result<()> {
        *self.status.lock() = RuntimeStatus::Pausing;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimePausing),
        );

        // Get all processor IDs
        let processor_ids: Vec<ProcessorUniqueId> = self
            .compiler
            .scope(|graph, _tx| graph.traversal().v(()).ids());

        // Pause each processor
        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.pause_processor(processor_id) {
                tracing::warn!("[{}] Failed to pause: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        // Set graph state to Paused
        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Paused);
        });

        *self.status.lock() = RuntimeStatus::Paused;
        if failures.is_empty() {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimePaused),
            );
        } else {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimePauseFailed {
                    error: format!("{} processor(s) rejected pause", failures.len()),
                }),
            );
        }

        Ok(())
    }

    /// Resume the runtime (all processors).
    pub fn resume(&self) -> Result<()> {
        *self.status.lock() = RuntimeStatus::Starting;
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeResuming),
        );

        // Get all processor IDs
        let processor_ids: Vec<ProcessorUniqueId> = self
            .compiler
            .scope(|graph, _tx| graph.traversal().v(()).ids());

        // Resume each processor
        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.resume_processor(processor_id) {
                tracing::warn!("[{}] Failed to resume: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        // Set graph state to Running
        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Running);
        });

        *self.status.lock() = RuntimeStatus::Started;
        if failures.is_empty() {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeResumed),
            );
        } else {
            PUBSUB.publish(
                topics::RUNTIME_GLOBAL,
                &Event::RuntimeGlobal(RuntimeEvent::RuntimeResumeFailed {
                    error: format!("{} processor(s) rejected resume", failures.len()),
                }),
            );
        }

        Ok(())
    }

    /// Block until shutdown signal (Ctrl+C, SIGTERM, SIGHUP, Cmd+Q) or a
    /// [`request_runtime_shutdown`](crate::core::runtime::request_runtime_shutdown).
    pub fn wait_for_signal(self: &Arc<Self>) -> Result<()> {
        self.wait_for_signal_with(|_| ControlFlow::Continue(()))
    }

    /// Own the shutdown signals, [`start`](Self::start), block until shutdown,
    /// and tear down — the whole run in one call.
    ///
    /// Preferred over `start()` followed by
    /// [`wait_for_signal`](Self::wait_for_signal), because signal ownership
    /// spans startup here: a Ctrl-C arriving while the graph is still coming up
    /// reaches the request funnel rather than whatever disposition was
    /// installed before.
    pub fn start_and_wait_for_shutdown(self: &Arc<Self>) -> Result<()> {
        let run_outcome = {
            let _shutdown_signals = Self::take_shutdown_signal_ownership()?;
            self.start().and_then(|()| {
                self.wait_for_shutdown_observation_with(|_| ControlFlow::Continue(()))
            })
        };
        Self::clear_the_shutdown_escalation_this_run_observed();
        run_outcome
    }

    /// [`start`](Self::start) and block until a shutdown is requested, tearing
    /// nothing down — for an embedding host that holds the shutdown signals
    /// through its own teardown and the engine's drop, so a second or third
    /// interrupt still escalates wherever that teardown is.
    ///
    /// On macOS the run loop is an `NSApplication` loop that stops the runtime
    /// and terminates the process instead of returning.
    pub fn start_and_block_until_shutdown_is_requested(
        self: &Arc<Self>,
        _shutdown_signals_held_by_the_caller: &ScopedShutdownSignalOwnership,
    ) -> Result<()> {
        self.start()?;
        self.block_until_shutdown_is_observed_with(|_| ControlFlow::Continue(()))
    }

    /// Take any request that landed after the run loop stopped observing, and
    /// how far this run's interrupts escalated.
    ///
    /// Called once shutdown-signal ownership has dropped, so no further signal
    /// can reach the funnel.
    fn clear_the_shutdown_escalation_this_run_observed() {
        crate::core::runtime::take_runtime_shutdown_escalation();
    }

    /// Own SIGINT, SIGTERM and SIGHUP until the returned value drops.
    ///
    /// Fails if another run loop in this process already owns them.
    pub fn take_shutdown_signal_ownership() -> Result<ScopedShutdownSignalOwnership> {
        crate::core::signals::ScopedShutdownSignalOwnership::take_until_dropped().map_err(
            |ownership_failure| {
                crate::core::Error::Configuration(format!(
                    "Failed to own shutdown signals: {}",
                    ownership_failure
                ))
            },
        )
    }

    /// Block until shutdown signal, with periodic callback for dynamic control.
    ///
    /// This is the run-loop owner: it observes both the `RuntimeShutdown`
    /// event and the shutdown escalation, then runs the normal teardown.
    /// The escalation is polled as well as the event because a request published
    /// before this subscriber was wired up leaves no event to receive.
    pub fn wait_for_signal_with<F>(self: &Arc<Self>, callback: F) -> Result<()>
    where
        F: FnMut(&Self) -> ControlFlow<()>,
    {
        let wait_outcome = {
            // Held only for the wait, so the dispositions are handed back once
            // the teardown inside has run.
            let _shutdown_signals = Self::take_shutdown_signal_ownership()?;
            self.wait_for_shutdown_observation_with(callback)
        };
        Self::clear_the_shutdown_escalation_this_run_observed();
        wait_outcome
    }

    /// The wait loop and the teardown after it, for callers that already own
    /// the shutdown signals.
    fn wait_for_shutdown_observation_with<F>(self: &Arc<Self>, callback: F) -> Result<()>
    where
        F: FnMut(&Self) -> ControlFlow<()>,
    {
        self.block_until_shutdown_is_observed_with(callback)?;
        self.stop()
    }

    /// The wait loop alone.
    fn block_until_shutdown_is_observed_with<F>(self: &Arc<Self>, mut callback: F) -> Result<()>
    where
        F: FnMut(&Self) -> ControlFlow<()>,
    {
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = Arc::clone(&shutdown_flag);

        // Listener that sets shutdown flag when RuntimeShutdown received
        struct ShutdownListener {
            flag: Arc<AtomicBool>,
        }

        impl EventListener for ShutdownListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                if let Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown) = event {
                    self.flag.store(true, Ordering::SeqCst);
                }
                Ok(())
            }
        }

        let shutdown_listener: Arc<parking_lot::Mutex<dyn EventListener>> =
            Arc::new(parking_lot::Mutex::new(ShutdownListener {
                flag: shutdown_flag_clone.clone(),
            }));
        PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&shutdown_listener))?;

        // On macOS, run the NSApplication event loop (required for GUI)
        #[cfg(target_os = "macos")]
        {
            let runtime = Arc::clone(self);
            let runtime_for_callback = Arc::clone(self);
            let shutdown_flag_for_callback = Arc::clone(&shutdown_flag);
            crate::apple::runtime_ext::run_macos_event_loop(
                move || {
                    // Called by applicationWillTerminate before app exits
                    if let Err(e) = runtime.stop() {
                        tracing::error!("Failed to stop runtime during shutdown: {}", e);
                    }
                },
                move || {
                    // The NSApplication loop has no shutdown-flag hook of its
                    // own — `ControlFlow::Break` is its only exit — so the
                    // shutdown observation rides in on the periodic callback,
                    // which routes through `app.terminate` →
                    // `applicationWillTerminate` → the stop callback above.
                    let control_flow = if runtime_shutdown_observed(&shutdown_flag_for_callback) {
                        ControlFlow::Break(())
                    } else {
                        callback(&runtime_for_callback)
                    };
                    if control_flow.is_break() {
                        crate::core::runtime::take_runtime_shutdown_escalation();
                    }
                    control_flow
                },
            );
            // Note: run_macos_event_loop never returns - app terminates after stop callback
            Ok(())
        }

        // Non-macOS: poll loop
        #[cfg(not(target_os = "macos"))]
        {
            while !runtime_shutdown_observed(&shutdown_flag) {
                // Call user callback
                if let ControlFlow::Break(()) = callback(self) {
                    break;
                }

                // Small sleep to avoid busy-waiting
                std::thread::sleep(
                    crate::core::runtime::RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL,
                );
            }

            Ok(())
        }
    }

    pub fn status(&self) -> RuntimeStatus {
        *self.status.lock()
    }

    // =========================================================================
    // RuntimeOperations delegation (inherent methods for ergonomic API)
    // =========================================================================

    /// Add a processor to the graph.
    pub fn add_processor(&self, spec: impl Into<ProcessorSpec>) -> Result<ProcessorUniqueId> {
        <Self as RuntimeOperations>::add_processor(self, spec.into())
    }

    /// Remove a processor from the graph.
    pub fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        <Self as RuntimeOperations>::remove_processor(self, processor_id)
    }

    /// Connect two ports.
    pub fn connect(
        &self,
        from: impl Into<OutputLinkPortRef>,
        to: impl Into<InputLinkPortRef>,
    ) -> Result<LinkUniqueId> {
        <Self as RuntimeOperations>::connect(self, from.into(), to.into())
    }

    /// Disconnect a link.
    pub fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        <Self as RuntimeOperations>::disconnect(self, link_id)
    }

    /// Ask whoever owns the run loop to shut the runtime down, with a
    /// human-readable `reason` logged for attribution.
    pub fn request_runtime_shutdown(&self, reason: &str) -> Result<()> {
        <Self as RuntimeOperations>::request_runtime_shutdown(self, reason)
    }

    // =========================================================================
    // Introspection
    // =========================================================================

    /// Export graph state as JSON including topology, processor states, metrics, and buffer levels.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        let extensions: Vec<_> = LoadedCapabilityExtensionRegistry::of_this_process()
            .registered()
            .into_iter()
            .map(LoadedCapabilityExtensionOutput::from)
            .collect();
        self.compiler.scope(|graph, _tx| {
            serde_json::to_value(graph.to_graph_response(extensions))
                .map_err(|_| Error::GraphError("Unable to serialize graph".into()))
        })
    }

    // =========================================================================
    // Graph Snapshot Save / Load
    // =========================================================================

    /// Load a graph snapshot into this runtime.
    ///
    /// Processors are created first, building an alias → ID map. Then
    /// connections are created by resolving aliases to runtime IDs.
    /// The snapshot's `name` is stashed on the runtime so a subsequent
    /// [`Self::save_graph_snapshot`] re-emits it without caller
    /// bookkeeping.
    ///
    /// Assumes every referenced processor type is already registered (it
    /// validates and fails on an unregistered type). For the turnkey case —
    /// resolve and build referenced packages by version first — use
    pub fn load_graph_snapshot(
        &self,
        snapshot: &crate::core::graph_snapshot::GraphSnapshot,
    ) -> Result<()> {
        use std::collections::HashMap;

        // Validate before loading
        snapshot.validate()?;

        // Phase 1: Create processors, build alias → ID map
        let mut alias_to_id: HashMap<String, ProcessorUniqueId> = HashMap::new();

        for proc_def in &snapshot.processors {
            let spec = proc_def.to_processor_spec();
            let id = self.add_processor(spec)?;

            alias_to_id.insert(proc_def.alias.clone(), id.clone());

            tracing::info!(
                "Created processor '{}' ({}) → {}",
                proc_def.alias,
                proc_def.processor_type,
                id
            );
        }

        // Phase 2: Create connections, resolving aliases
        for conn_def in &snapshot.connections {
            let from = conn_def.parse_from()?;
            let to = conn_def.parse_to()?;

            let from_id = alias_to_id.get(from.alias).ok_or_else(|| {
                Error::GraphError(format!("Unknown processor alias: '{}'", from.alias))
            })?;
            let to_id = alias_to_id.get(to.alias).ok_or_else(|| {
                Error::GraphError(format!("Unknown processor alias: '{}'", to.alias))
            })?;

            self.connect(
                OutputLinkPortRef::new(from_id, from.port_name),
                InputLinkPortRef::new(to_id, to.port_name),
            )?;

            tracing::info!(
                "Connected {}.{} → {}.{}",
                from.alias,
                from.port_name,
                to.alias,
                to.port_name
            );
        }

        *self.pipeline_name.lock() = snapshot.name.clone();

        if let Some(name) = &snapshot.name {
            tracing::info!("Loaded pipeline: {}", name);
        }

        Ok(())
    }

    /// Load a graph snapshot from a JSON file path.
    ///
    /// Assumes referenced processor types are already registered; for the
    /// turnkey path that resolves missing modules by version, use
    pub fn load_graph_snapshot_from_path(&self, path: &std::path::Path) -> Result<()> {
        let snapshot = crate::core::graph_snapshot::GraphSnapshot::from_json_file(path)?;

        if let Some(name) = &snapshot.name {
            tracing::info!("Loading pipeline '{}' from {}", name, path.display());
        } else {
            tracing::info!("Loading pipeline from {}", path.display());
        }

        self.load_graph_snapshot(&snapshot)
    }

    /// Snapshot the live graph as a [`GraphSnapshot`].
    ///
    /// Walks every processor node and link, regenerates per-node
    /// aliases deterministically from each node's
    /// PascalCase short name (with `_2`, `_3`, … on collision in
    /// node-iteration order), and emits the structured snapshot the
    /// load side accepts. The current `pipeline_name` is included
    /// when present so `load → save` preserves it without caller
    /// bookkeeping.
    ///
    /// `display_name` rides the snapshot only when the live node's
    /// display name differs from its processor type's PascalCase
    /// short name — i.e. only when a caller explicitly overrode the
    /// default — so the user-intent distinction survives round-trips.
    pub fn save_graph_snapshot(&self) -> Result<crate::core::graph_snapshot::GraphSnapshot> {
        use std::collections::HashMap;

        use crate::core::graph::default_display_name_for;
        use crate::core::graph_snapshot::{
            ConnectionDefinition, GraphSnapshot, ProcessorDefinition,
        };

        self.compiler.scope(|graph, _tx| {
            // Deterministic aliasing — camelCase the type's PascalCase
            // short name (e.g. CameraProcessor → cameraProcessor) and
            // suffix `_2`, `_3` … on collision in node-iteration order.
            let mut alias_counts: HashMap<String, u32> = HashMap::new();
            let mut id_to_alias: HashMap<String, String> = HashMap::new();
            let mut processors: Vec<ProcessorDefinition> = Vec::new();

            for node in graph.traversal().v(()).iter() {
                let default_name = default_display_name_for(&node.processor_type);
                let base = pascal_to_camel(&default_name);

                let count = alias_counts.entry(base.clone()).or_insert(0);
                *count += 1;
                let alias = if *count == 1 {
                    base.clone()
                } else {
                    format!("{}_{}", base, count)
                };

                id_to_alias.insert(node.id.to_string(), alias.clone());

                let display_name =
                    (node.display_name != default_name).then(|| node.display_name.clone());

                processors.push(ProcessorDefinition {
                    alias,
                    processor_type: node.processor_type.clone(),
                    config: node.config.clone().unwrap_or(serde_json::Value::Null),
                    display_name,
                });
            }

            let mut connections: Vec<ConnectionDefinition> = Vec::new();
            for link in graph.traversal().e(()).iter() {
                let from_alias = id_to_alias
                    .get(link.source.processor_id.as_str())
                    .ok_or_else(|| {
                        Error::GraphError(format!(
                            "Link source processor '{}' missing from snapshot alias map",
                            link.source.processor_id
                        ))
                    })?;
                let to_alias = id_to_alias
                    .get(link.target.processor_id.as_str())
                    .ok_or_else(|| {
                        Error::GraphError(format!(
                            "Link target processor '{}' missing from snapshot alias map",
                            link.target.processor_id
                        ))
                    })?;
                connections.push(ConnectionDefinition {
                    from: format!("{}.{}", from_alias, link.source.port_name),
                    to: format!("{}.{}", to_alias, link.target.port_name),
                });
            }

            Ok(GraphSnapshot {
                name: self.pipeline_name.lock().clone(),
                processors,
                connections,
            })
        })
    }

    /// Snapshot the live graph and write it to a JSON file path.
    pub fn save_graph_snapshot_to_path(&self, path: &std::path::Path) -> Result<()> {
        let snapshot = self.save_graph_snapshot()?;
        snapshot.to_json_file(path)?;
        if let Some(name) = &snapshot.name {
            tracing::info!("Saved pipeline '{}' to {}", name, path.display());
        } else {
            tracing::info!("Saved pipeline to {}", path.display());
        }
        Ok(())
    }

    /// Set or clear the pipeline name carried into the next
    /// [`Self::save_graph_snapshot`]. Imperative-build callers use
    /// this when they want their snapshots to round-trip with a
    /// label; snapshot loaders set it automatically.
    pub fn set_pipeline_name(&self, name: Option<String>) {
        *self.pipeline_name.lock() = name;
    }

    /// Current pipeline name, if any. Set by
    /// [`Self::load_graph_snapshot`] or [`Self::set_pipeline_name`].
    pub fn pipeline_name(&self) -> Option<String> {
        self.pipeline_name.lock().clone()
    }
}

/// Whether the run loop should stop: the `RuntimeShutdown` event was received,
/// or the shutdown escalation has been raised. One predicate so both `#[cfg]` arms of
/// [`Runner::wait_for_signal_with`] observe the same set of sources.
fn runtime_shutdown_observed(event_shutdown_flag: &AtomicBool) -> bool {
    event_shutdown_flag.load(Ordering::SeqCst)
        || crate::core::runtime::is_runtime_shutdown_requested()
}

/// PascalCase → camelCase for snapshot alias generation.
///
/// `CameraProcessor → cameraProcessor`; `BGRAFileSource → bGRAFileSource`
/// (only the first character is lowercased — the alias just needs to
/// be deterministic and human-readable, not perfectly idiomatic). The
/// alias is local to the snapshot and consumed by `to_processor_spec`
/// on load; the actual processor identity rides the `processor_type`
/// field.
fn pascal_to_camel(short: &str) -> String {
    let mut chars = short.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Compute the per-runtime surface-sharing socket path, refuse to start if
/// another live runtime is already bound there, clean up an orphan socket
/// from a prior crashed runtime, and bring the listener up.
#[cfg(target_os = "linux")]
fn bring_up_surface_service(
    runtime_directory: &StreamlibRuntimeDirectory,
    runtime_id: &RuntimeUniqueId,
) -> Result<(
    Arc<Mutex<Option<crate::linux::surface_share::UnixSocketSurfaceService>>>,
    std::path::PathBuf,
    Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
)> {
    use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};

    let socket_path = runtime_directory.surface_share_socket_path(runtime_id);

    if socket_path.exists() {
        match std::os::unix::net::UnixStream::connect(&socket_path) {
            Ok(_) => {
                return Err(Error::Runtime(format!(
                    "Surface-sharing socket {} is already bound by a live process. \
                     Each Runner requires a unique runtime_id; check for a \
                     duplicate STREAMLIB_RUNTIME_ID env var or another runtime in \
                     the same session.",
                    socket_path.display()
                )));
            }
            Err(_) => {
                std::fs::remove_file(&socket_path).map_err(|e| {
                    Error::Runtime(format!(
                        "Found stale surface-sharing socket {} from a prior crashed \
                         runtime but failed to remove it: {}",
                        socket_path.display(),
                        e
                    ))
                })?;
                tracing::warn!(
                    "[new] Removed stale surface-sharing socket left by prior runtime: {}",
                    socket_path.display()
                );
            }
        }
    }

    let state = SurfaceShareState::new();
    let check_out_leases = Arc::clone(state.check_out_leases());
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().map_err(|e| {
        Error::Runtime(format!(
            "Failed to start runtime-internal surface-sharing service at {}: {}",
            socket_path.display(),
            e
        ))
    })?;

    tracing::info!(
        "[new] Runtime-internal surface-sharing service bound at {}",
        socket_path.display()
    );

    Ok((
        Arc::new(Mutex::new(Some(service))),
        socket_path,
        check_out_leases,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // All Runner::new() tests are `#[serial]` because the runtime
    // reads/writes process-global env vars (XDG_RUNTIME_DIR,
    // STREAMLIB_RUNTIME_ID) and every runtime's listeners share the
    // process-wide event bus. The test module's `#[serial]` default group
    // serializes every test that constructs a Runner so nobody reads env
    // mid-mutation.

    #[test]
    #[serial]
    fn test_runtime_creation() {
        let _runtime = Runner::new();
        // Runtime creates successfully
    }

    /// Locks one half of the empty-substrate invariant from issue
    /// #793 / the All-Dynamic Package Loading milestone:
    /// `Runner::new()` itself must not walk any compile-time-linked
    /// registration source. The other half — the `#[processor]` macro
    /// not emitting `inventory::submit!(FactoryRegistration { ... })`
    /// — is locked by `xtask check-no-inventory-submit` in CI, not by
    /// this test.
    ///
    /// Together the two locks make regression impossible: even if a
    /// future agent re-introduces the macro emission, `Runner::new()`
    /// has nothing to walk it with, and even if a future agent re-adds
    /// a registry-walking call to `Runner::new()`, the CI gate refuses
    /// any `inventory::submit!(FactoryRegistration ...)` for it to find.
    ///
    /// `PROCESSOR_REGISTRY` is a process-global `LazyLock` and earlier
    /// tests in the same binary may have populated it, so the
    /// assertion is over the *delta* `Runner::new()` introduces, not
    /// the absolute size.
    #[test]
    #[serial]
    fn runner_new_registers_zero_processors() {
        use crate::core::processors::PROCESSOR_REGISTRY;
        let before = PROCESSOR_REGISTRY.list_registered().len();
        let _runtime = Runner::new().expect("Runner::new");
        let after = PROCESSOR_REGISTRY.list_registered().len();
        assert_eq!(
            after, before,
            "Runner::new() must not register any processors — the engine \
             substrate ships empty (issue #793). Delta: {before} → {after}."
        );
    }

    /// `graph`'s third key answers about the process, not about which runtime
    /// asked: the extension hooks run once per process, so a runtime built
    /// after them reports what they registered just as the first one does.
    ///
    /// The registry is a process-global like `PROCESSOR_REGISTRY` above, so
    /// this asserts membership rather than the whole list.
    #[test]
    #[serial]
    fn to_json_renders_every_capability_this_process_registered() {
        LoadedCapabilityExtensionRegistry::of_this_process()
            .register(crate::core::runtime::LoadedCapabilityExtension {
                name: "a-capability-only-this-test-registers".to_string(),
                version: "3.1.4".to_string(),
                distribution: "streamlib-test-only".to_string(),
            })
            .expect("the capability registers");

        let runtime = Runner::new().expect("Runner::new");
        let rendered = runtime.to_json().expect("the graph serializes");

        assert!(
            rendered["extensions"]
                .as_array()
                .expect("extensions is always an array")
                .contains(&serde_json::json!({
                    "name": "a-capability-only-this-test-registers",
                    "version": "3.1.4",
                    "distribution": "streamlib-test-only",
                })),
            "to_json must render what the process registered, got: {}",
            rendered["extensions"]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn runner_new_caps_timer_slack_to_one_nanosecond() {
        // Linux default is 50_000 ns. `Runner::new` calls
        // `prctl(PR_SET_TIMERSLACK, 1)` as its very first step;
        // `prctl(PR_GET_TIMERSLACK)` on the same thread should report 1.
        let _runtime = Runner::new().expect("Runner::new");
        let slack = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK) };
        assert_eq!(
            slack, 1,
            "expected timer slack 1 ns after Runner::new (got {})",
            slack
        );
    }

    #[test]
    #[serial]
    fn test_new_outside_tokio_creates_owned_runtime() {
        // Outside tokio context - creates owned runtime
        let runtime = Runner::new().unwrap();
        assert!(matches!(
            runtime.tokio_runtime_variant,
            TokioRuntimeVariant::OwnedTokioRuntime(_)
        ));
    }

    /// Fail-without-fix: a plain drop of the runtime waits for the blocking task
    /// below for its whole minute.
    #[test]
    fn an_owned_tokio_runtime_whose_blocking_task_never_returns_still_drops_within_its_budget() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime builds");
        let (blocking_task_started, blocking_task_has_started) = std::sync::mpsc::channel();
        runtime.spawn_blocking(move || {
            let _ = blocking_task_started.send(());
            std::thread::sleep(Duration::from_secs(60));
        });
        blocking_task_has_started
            .recv_timeout(Duration::from_secs(5))
            .expect("the blocking task starts");

        let owned = TokioRuntimeShutDownWithinItsBudget::owning(runtime);
        let started = std::time::Instant::now();
        drop(owned);

        assert!(
            started.elapsed() < OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET + Duration::from_secs(1),
            "dropping the runtime took {:?}",
            started.elapsed()
        );
    }

    #[test]
    #[serial]
    fn test_new_inside_tokio_uses_external_handle() {
        // Inside tokio context - auto-detects and uses external handle
        let temp_rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = temp_rt.block_on(async { Runner::new() });
        assert!(result.is_ok());
        let runtime = result.unwrap();
        assert!(matches!(
            runtime.tokio_runtime_variant,
            TokioRuntimeVariant::ExternalTokioHandle(_)
        ));
    }

    /// Fail-without-fix: the request wrote the configuration onto the node
    /// before the processor was asked, so one it refused at commit stayed in
    /// `graph` as though it had been taken.
    #[test]
    #[serial]
    fn a_requested_configuration_waits_for_the_commit_rather_than_landing_on_the_graph_node() {
        use crate::core::test_support::{MockOutputOnlyProcessor, ensure_test_mocks_registered};

        ensure_test_mocks_registered();
        let runtime = Runner::new().expect("Runner::new");
        let processor_id = runtime
            .add_processor(ProcessorSpec::new(
                MockOutputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the mock is added");

        runtime
            .update_processor_config(&processor_id, serde_json::json!({"gain": 3}))
            .expect("the update is queued");

        let config_on_the_node = runtime.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(&processor_id)
                .first()
                .expect("the node is in the graph")
                .config
                .clone()
        });
        assert_eq!(config_on_the_node, Some(serde_json::Value::Null));
        assert!(
            runtime.compiler.logged_pending_operations().iter().any(|op| matches!(
                op,
                PendingOperation::UpdateProcessorConfig { processor_id: queued_for, config_to_apply }
                    if *queued_for == processor_id && *config_to_apply == serde_json::json!({"gain": 3})
            )),
            "the update carries its configuration to the commit"
        );
    }

    #[test]
    #[serial]
    fn test_sync_methods_work_inside_tokio() {
        // Verify sync methods work when called from tokio context
        let temp_rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        temp_rt.block_on(async {
            let runtime = Runner::new().unwrap();
            // Sync methods should work (use spawn + channel internally)
            let json = runtime.to_json().unwrap();
            assert!(json["nodes"].is_array());
        });
    }

    // =========================================================================
    // Per-runtime surface-sharing service (#428)
    // =========================================================================

    #[cfg(target_os = "linux")]
    mod runtime_internal_surface_share {
        use super::*;
        use std::os::unix::net::UnixStream;
        use streamlib_surface_client::{MAX_DMA_BUF_PLANES, send_request_with_fds};

        /// Each variable's value before a test set it, put back on drop — so a test
        /// that panics still leaves the environment as it found it.
        struct EnvironmentVariablesRestoredOnDrop {
            previous_values: Vec<(String, Option<std::ffi::OsString>)>,
        }

        impl Drop for EnvironmentVariablesRestoredOnDrop {
            fn drop(&mut self) {
                // SAFETY: serialized via #[serial]; no concurrent env mutation.
                unsafe {
                    for (name, previous_value) in self.previous_values.drain(..) {
                        match previous_value {
                            Some(value) => std::env::set_var(name, value),
                            None => std::env::remove_var(name),
                        }
                    }
                }
            }
        }

        /// Set each variable for the duration of the closure, restoring what was
        /// there before. Tests using this must be `#[serial]`.
        fn with_environment_variables_set<F: FnOnce() -> R, R>(
            variables: &[(&str, &std::ffi::OsStr)],
            f: F,
        ) -> R {
            let _restored_on_drop = EnvironmentVariablesRestoredOnDrop {
                previous_values: variables
                    .iter()
                    .map(|(name, _)| (name.to_string(), std::env::var_os(name)))
                    .collect(),
            };
            // SAFETY: serialized via #[serial]; no concurrent env mutation.
            unsafe {
                for (name, value) in variables {
                    std::env::set_var(name, value);
                }
            }
            f()
        }

        /// Replace XDG_RUNTIME_DIR with a fresh tempdir for the duration of the
        /// closure. Tests using this must be `#[serial]` so no other runtime
        /// construct reads the mutated env.
        fn with_isolated_xdg_runtime_dir<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
            let tmp = tempfile::tempdir().expect("tempdir");
            with_environment_variables_set(&[("XDG_RUNTIME_DIR", tmp.path().as_os_str())], || {
                f(tmp.path())
            })
        }

        #[test]
        #[serial]
        fn runtime_brings_up_internal_surface_share_service() {
            with_isolated_xdg_runtime_dir(|xdg| {
                let runtime = Runner::new().expect("runtime should construct");
                let socket_path = runtime.surface_socket_path();
                assert!(
                    socket_path.exists(),
                    "expected socket file at {}",
                    socket_path.display()
                );
                assert!(
                    socket_path.starts_with(xdg.join("streamlib")),
                    "socket {} should be under the runtime directory in XDG_RUNTIME_DIR {}",
                    socket_path.display(),
                    xdg.display()
                );

                // Round-trip a request through the runtime-internal service to prove
                // it is actually serving — check_out for an unknown surface_id is
                // the lightest-weight op that exercises the wire path end-to-end.
                let stream = UnixStream::connect(socket_path).expect("connect to runtime broker");
                let req = serde_json::json!({
                    "op": "check_out",
                    "surface_id": "ping-no-such-surface",
                });
                let (resp, fds) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
                    .expect("round-trip");
                assert!(fds.is_empty());
                assert!(
                    resp.get("error").and_then(|v| v.as_str()).is_some(),
                    "expected error for missing surface, got {:?}",
                    resp
                );
            });
        }

        #[test]
        #[serial]
        fn a_runtime_started_with_xdg_runtime_dir_unset_keeps_its_socket_and_domain_in_the_per_user_fallback()
         {
            let prev = std::env::var_os("XDG_RUNTIME_DIR");
            // SAFETY: serialized via #[serial].
            unsafe {
                std::env::remove_var("XDG_RUNTIME_DIR");
            }

            let result = Runner::new();

            // Restore env before asserting so a panic doesn't leak state.
            unsafe {
                if let Some(v) = prev {
                    std::env::set_var("XDG_RUNTIME_DIR", v);
                }
            }

            let runtime = result.expect("a runtime starts with XDG_RUNTIME_DIR unset");
            let fallback = std::path::PathBuf::from(format!(
                "/tmp/streamlib-{}",
                crate::core::runtime::current_process_uid()
            ));
            assert!(
                runtime.surface_socket_path().starts_with(&fallback),
                "socket {} should be under {}",
                runtime.surface_socket_path().display(),
                fallback.display()
            );
            let iceoryx2_config = runtime.iceoryx2_node.config();
            assert_eq!(
                iceoryx2_config.global.root_path().as_bytes_const(),
                fallback.join("iox2").as_os_str().as_encoded_bytes()
            );
            assert_eq!(
                iceoryx2_config.global.prefix.as_bytes_const(),
                crate::iceoryx2::engine_owned_iceoryx2_prefix_for_this_user().as_bytes()
            );
        }

        #[test]
        #[serial]
        fn two_runtimes_coexist_without_collision() {
            with_isolated_xdg_runtime_dir(|_| {
                let r1 = Runner::new().expect("first runtime");
                let r2 = Runner::new().expect("second runtime");

                let p1 = r1.surface_socket_path().to_path_buf();
                let p2 = r2.surface_socket_path().to_path_buf();

                assert_ne!(p1, p2, "each runtime must own a distinct socket path");
                assert!(p1.exists(), "first socket missing: {}", p1.display());
                assert!(p2.exists(), "second socket missing: {}", p2.display());

                // Both should serve a round-trip independently.
                for path in [&p1, &p2] {
                    let stream = UnixStream::connect(path).expect("connect");
                    let req = serde_json::json!({
                        "op": "check_out",
                        "surface_id": "no-such",
                    });
                    let (resp, _) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
                        .expect("round-trip");
                    assert!(resp.get("error").is_some());
                }
            });
        }

        fn iceoryx2_nodes_in_domain(domain_root: &std::path::Path) -> usize {
            let config = crate::iceoryx2::engine_owned_iceoryx2_config(domain_root)
                .expect("the test domain root fits the socket-path budget");
            let mut nodes = 0;
            iceoryx2::node::Node::<iceoryx2::prelude::ipc::Service>::list(&config, |_| {
                nodes += 1;
                iceoryx2::prelude::CallbackProgression::Continue
            })
            .expect("list the domain's iceoryx2 nodes");
            nodes
        }

        /// Set in the child process this test re-runs itself in.
        const RUNTIME_BUILT_AND_DROPPED_IN_A_CHILD_PROCESS_ENVIRONMENT_VARIABLE: &str =
            "STREAMLIB_TEST_BUILD_AND_DROP_ONE_RUNTIME";

        /// A child process builds and drops the only runtime it ever makes, then
        /// exits, and the parent reads the domain it leaves. A child keeps the
        /// check independent of every runtime this test binary built before:
        /// a static that pins only the first runtime's node pins this one.
        ///
        /// Mental-revert: parking a clone of the runner's node anywhere static
        /// keeps the node past the drop, and the exited child leaves its files.
        #[test]
        #[serial]
        fn a_dropped_runtime_leaves_no_iceoryx2_node_in_its_domain() {
            if std::env::var_os(RUNTIME_BUILT_AND_DROPPED_IN_A_CHILD_PROCESS_ENVIRONMENT_VARIABLE)
                .is_some()
            {
                drop(Runner::new().expect("runtime"));
                return;
            }

            with_isolated_xdg_runtime_dir(|_| {
                let domain_root = StreamlibRuntimeDirectory::resolve()
                    .expect("the isolated runtime directory")
                    .iceoryx2_domain_root();
                let child = crate::core::test_support::rerun_this_test_in_a_child_process(
                    "core::runtime::runtime::tests::runtime_internal_surface_share::a_dropped_runtime_leaves_no_iceoryx2_node_in_its_domain",
                    RUNTIME_BUILT_AND_DROPPED_IN_A_CHILD_PROCESS_ENVIRONMENT_VARIABLE,
                    std::ffi::OsStr::new("1"),
                );
                assert!(
                    child.status.success(),
                    "the child failed to build and drop a runtime: {}\n{}",
                    String::from_utf8_lossy(&child.stdout),
                    String::from_utf8_lossy(&child.stderr),
                );

                assert_eq!(
                    iceoryx2_nodes_in_domain(&domain_root),
                    0,
                    "a runtime torn down gracefully must take its iceoryx2 node's files with it"
                );
            });
        }

        /// The runtime directory is placed so its iceoryx2 domain root sits one
        /// byte past the socket-path budget: creating a node there is refused by
        /// name, so a refusal naming the live runtime instead is proof the
        /// duplicate check ran before any node was attempted.
        ///
        /// Mental-revert: creating the node before binding the surface socket
        /// turns the refusal below into the budget refusal.
        #[test]
        #[serial]
        fn a_runtime_pinned_to_a_live_runtimes_id_is_refused_before_it_creates_an_iceoryx2_node() {
            let base = tempfile::Builder::new()
                .prefix("sl")
                .tempdir_in("/tmp")
                .expect("tempdir under /tmp");
            let domain_root_suffix = "/streamlib/iox2";
            let xdg_runtime_dir_bytes =
                crate::iceoryx2::ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES
                    - crate::iceoryx2::engine_owned_iceoryx2_prefix_for_this_user().len()
                    - domain_root_suffix.len()
                    + 1;
            let base_bytes = base.path().as_os_str().len();
            assert!(
                xdg_runtime_dir_bytes > base_bytes + 1,
                "{} is too long to build a runtime directory past the budget under",
                base.path().display()
            );
            let xdg = base
                .path()
                .join("x".repeat(xdg_runtime_dir_bytes - base_bytes - 1));
            let pinned_id = format!("duplicate-{}", std::process::id());
            let live_runtimes_socket_path = with_environment_variables_set(
                &[("XDG_RUNTIME_DIR", xdg.as_os_str())],
                StreamlibRuntimeDirectory::resolve,
            )
            .expect("the runtime directory under the padded XDG_RUNTIME_DIR")
            .surface_share_socket_path(&RuntimeUniqueId::from(pinned_id.as_str()));
            let live_runtimes_socket =
                std::os::unix::net::UnixListener::bind(&live_runtimes_socket_path)
                    .expect("bind the live runtime's socket");

            let refusal = with_environment_variables_set(
                &[
                    ("XDG_RUNTIME_DIR", xdg.as_os_str()),
                    ("STREAMLIB_RUNTIME_ID", std::ffi::OsStr::new(&pinned_id)),
                ],
                || Runner::new().map(|_| ()),
            )
            .expect_err("a second runtime with a live runtime's id must be refused")
            .to_string();

            assert!(
                refusal.contains("already bound by a live process"),
                "the refusal must name the live runtime, not a node failure: {refusal}"
            );
            drop(live_runtimes_socket);
        }

        /// Mental-revert: validating the pinned id after the runtime directory
        /// resolves leaves that directory behind for a runtime that never ran.
        #[test]
        #[serial]
        fn a_malformed_pinned_runtime_id_is_refused_before_the_runtime_makes_anything() {
            with_isolated_xdg_runtime_dir(|xdg| {
                let refusal = with_environment_variables_set(
                    &[("STREAMLIB_RUNTIME_ID", std::ffi::OsStr::new("../escape"))],
                    || Runner::new().map(|_| ()),
                )
                .expect_err("a runtime id that could leave its directory must be refused")
                .to_string();

                assert!(
                    refusal.contains("STREAMLIB_RUNTIME_ID") && refusal.contains("'/'"),
                    "{refusal}"
                );
                assert!(
                    !xdg.join("streamlib").exists(),
                    "a refused runtime must not have made its runtime directory"
                );
            });
        }

        #[test]
        #[serial]
        fn polyglot_subprocess_inherits_socket_env() {
            with_isolated_xdg_runtime_dir(|_| {
                let runtime = Runner::new().expect("runtime");
                let socket_path = runtime.surface_socket_path().to_path_buf();

                // Mirror what the spawn ops do: build a Command with the env
                // var set from the runtime's socket path. The spawn ops use
                // `ctx.surface_socket_path()` which returns the same value as
                // `runtime.surface_socket_path()` — this test exercises the
                // contract that polyglot subprocesses see the runtime's socket.
                let output = std::process::Command::new("printenv")
                    .arg("STREAMLIB_SURFACE_SOCKET")
                    .env("STREAMLIB_SURFACE_SOCKET", &socket_path)
                    .output()
                    .expect("spawn printenv");

                assert!(
                    output.status.success(),
                    "printenv exited non-zero: stdout={:?} stderr={:?}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let inherited = String::from_utf8_lossy(&output.stdout).trim().to_string();
                assert_eq!(inherited, socket_path.to_string_lossy());
            });
        }

        #[test]
        #[serial]
        fn stale_socket_from_dead_runtime_is_cleaned_up() {
            with_isolated_xdg_runtime_dir(|xdg| {
                // Pin the runtime ID so we can pre-create a file at the exact
                // path the runtime will compute.
                let pinned_id = format!("test-stale-socket-{}", std::process::id());
                let prev = std::env::var_os("STREAMLIB_RUNTIME_ID");
                // SAFETY: serialized via #[serial].
                unsafe {
                    std::env::set_var("STREAMLIB_RUNTIME_ID", &pinned_id);
                }

                std::fs::create_dir(xdg.join("streamlib")).expect("create runtime directory");
                let stale_path = xdg
                    .join("streamlib")
                    .join(format!("surface-share-{pinned_id}.sock"));
                std::fs::write(&stale_path, b"orphan-from-prior-crashed-runtime")
                    .expect("write orphan");
                assert!(stale_path.exists());

                let runtime_result = Runner::new();

                // Restore env before asserting.
                unsafe {
                    match prev {
                        Some(v) => std::env::set_var("STREAMLIB_RUNTIME_ID", v),
                        None => std::env::remove_var("STREAMLIB_RUNTIME_ID"),
                    }
                }

                let runtime = runtime_result
                    .expect("runtime should clean up an orphan socket and bind successfully");
                let bound = runtime.surface_socket_path();
                assert_eq!(bound, stale_path.as_path());
                assert!(
                    bound.exists(),
                    "service should be bound at {}",
                    bound.display()
                );

                // The path is now a Unix socket, not a regular file — connect
                // should succeed against the runtime-internal service.
                let stream = UnixStream::connect(bound).expect("connect to fresh service");
                let req = serde_json::json!({
                    "op": "check_out",
                    "surface_id": "no-such",
                });
                let (resp, _) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
                    .expect("round-trip");
                assert!(resp.get("error").is_some());
            });
        }
    }

    /// An embedding host tears the engine down after the run loop already
    /// stopped it, so the second `stop()` must be a no-op rather than a second
    /// full teardown that republishes the transition to every subscriber.
    ///
    /// Mental-revert: removing the already-stopped early return in `stop()`
    /// makes the second call publish `RuntimeStopping`/`RuntimeStopped` again
    /// and fails the counts below.
    #[test]
    #[serial_test::serial]
    fn stopping_an_already_stopped_runtime_is_a_no_op() {
        use crate::core::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};

        #[derive(Default)]
        struct StopTransitionCounter {
            stopping: usize,
            stopped: usize,
        }

        struct CountingListener(Arc<Mutex<StopTransitionCounter>>);

        impl EventListener for CountingListener {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                let mut counts = self.0.lock();
                match event {
                    Event::RuntimeGlobal(RuntimeEvent::RuntimeStopping) => counts.stopping += 1,
                    Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped) => counts.stopped += 1,
                    _ => {}
                }
                Ok(())
            }
        }

        let runner = Runner::new().expect("a runner boots without a graph");
        let counts = Arc::new(Mutex::new(StopTransitionCounter::default()));
        let listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(CountingListener(Arc::clone(&counts))));
        PUBSUB
            .subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener))
            .expect("subscribe establishes the subscriber");

        runner.stop().expect("the first stop succeeds");
        runner.stop().expect("the second stop succeeds");

        // Delivery is not synchronous with the publish, so wait for the first
        // teardown's pair to land before counting — otherwise a duplicate that
        // simply arrived late would read as "published once".
        let delivery_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < delivery_deadline {
            if counts.lock().stopped >= 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));

        let observed = counts.lock();
        assert_eq!(
            observed.stopping, 1,
            "RuntimeStopping must be published exactly once across two stops",
        );
        assert_eq!(
            observed.stopped, 1,
            "RuntimeStopped must be published exactly once across two stops",
        );
    }
}
