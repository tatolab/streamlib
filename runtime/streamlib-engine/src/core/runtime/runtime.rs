// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde::Serialize;

use super::RuntimeOperations;
use super::RuntimeStatus;
use super::RuntimeUniqueId;
use super::graph_change_listener::GraphChangeListener;
use crate::core::compiler::{Compiler, PendingOperation};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use crate::core::context::SoftwareAudioClock;
use crate::core::context::{
    AudioClockConfig, GpuContext, RuntimeContext, SharedAudioClock, TimeContext,
};
use crate::core::graph::{
    GraphNodeWithComponents, GraphState, LinkUniqueId, ProcessorPauseGateComponent,
    ProcessorUniqueId,
};
use crate::core::processors::ProcessorSpec;
use crate::core::processors::ProcessorState;
use crate::core::pubsub::{Event, EventListener, PUBSUB, ProcessorEvent, RuntimeEvent, topics};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};
use crate::iceoryx2::Iceoryx2Node;

/// Storage variant for tokio runtime in Runner.
///
/// Enables Runner to work both standalone (owning its runtime) and
/// integrated into existing tokio applications (using the current handle).
pub(crate) enum TokioRuntimeVariant {
    /// Runner owns the tokio Runtime (created when NOT in tokio context).
    OwnedTokioRuntime(tokio::runtime::Runtime),
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
    /// Created in new() so PUBSUB can initialize before start().
    pub(crate) iceoryx2_node: Iceoryx2Node,
    /// Per-runtime surface-sharing service. Bound to a unique Unix socket in
    /// `new()`; polyglot subprocesses connect to it via the
    /// `STREAMLIB_SURFACE_SOCKET` env var. Wrapped in `Mutex<Option<...>>`
    /// so `stop()` can drop it deterministically; the `Drop` impl on
    /// `UnixSocketSurfaceService` removes the socket file.
    #[cfg(target_os = "linux")]
    pub(crate) surface_service:
        Arc<Mutex<Option<crate::linux::surface_share::UnixSocketSurfaceService>>>,
    /// Path of the per-runtime surface-sharing socket
    /// (`$XDG_RUNTIME_DIR/streamlib-<runtime_uuid>.sock`).
    #[cfg(target_os = "linux")]
    pub(crate) surface_socket_path: std::path::PathBuf,
    /// Logging guard — keeps the drain worker alive for the runtime's
    /// lifetime. On drop, flushes buffered JSONL records and
    /// `fdatasync`s the log file.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    _logging_guard: crate::core::logging::StreamlibLoggingGuard,
    /// Engine-extension hooks invoked exactly once during [`Self::start`],
    /// after the [`GpuContext`] is initialized and before any
    /// processor's `setup()` runs. Used to register host-side bridges
    /// (e.g. [`crate::core::context::CpuReadbackBridge`]) whose
    /// construction needs the live GpuContext but whose registration
    /// must precede the first `process()` call. Drained on each `start()`.
    setup_hooks: Arc<Mutex<Vec<Box<dyn FnOnce(&GpuContext) -> Result<()> + Send>>>>,
    /// Optional pipeline name carried across snapshot load → save.
    /// Set by [`Self::load_graph_snapshot`] and read by
    /// [`Self::save_graph_snapshot`] so a snapshot loaded from disk
    /// can be re-saved with the same `name` without caller bookkeeping.
    /// `None` when the graph was built imperatively without a name.
    pipeline_name: Arc<Mutex<Option<String>>>,
    /// Injected build seam. `None` by construction
    /// ([`Runner::new`]) — the engine never shells out to a toolchain.
    /// A [`BuildPolicy`] requiring a (re)build fails loud when this is
    /// absent. Wired via [`Runner::new_with_orchestrator`] /
    /// [`Runner::set_build_orchestrator`] (or the SDK `auto-build`
    /// feature). Mirrors the [`setup_hooks`] injection shape.
    ///
    /// [`BuildPolicy`]: crate::core::runtime::module_loader::BuildPolicy
    /// [`setup_hooks`]: Self::setup_hooks
    pub(crate) build_orchestrator:
        Arc<Mutex<Option<Arc<dyn crate::core::runtime::module_loader::BuildOrchestrator>>>>,
    /// Modules whose loads have not yet settled, keyed by canonical
    /// `@org/name` with the owning load's id. Inserted when
    /// [`Runner::add_module`] spawns a load; removed when that same
    /// load's task finishes (id-guarded so an earlier load's completion
    /// can't untrack a later load of the same package ref).
    /// [`Runner::start`] refuses to run the graph while any entry remains.
    pub(crate) loading_modules: Arc<
        Mutex<
            std::collections::HashMap<
                streamlib_idents::PackageRef,
                (u64, streamlib_idents::ModuleIdent),
            >,
        >,
    >,
    /// Runtime-lifetime single-version-per-package resolution memo, keyed
    /// by `@org/name`. Populated by the live module walker on every
    /// [`Runner::add_module`] call; persists across calls so a diamond
    /// version divergence — or two successive / concurrent `add_module`s
    /// resolving different concrete versions of the same package —
    /// dedupes to the first-resolved winner (single-version model: a later
    /// encounter at a different version warns and reuses the winner rather
    /// than double-registering; if the two are incompatible it surfaces at
    /// compile-on-install for source packages, or at runtime for prebuilt
    /// slots). Lives for the runtime's lifetime; [`Runner::remove_module`]
    /// clears a removed package's entry so a later `add_module` re-resolves
    /// it from scratch.
    /// The memo is Runner-scoped while the schema / processor registries
    /// it protects are process-global statics — a second [`Runner`] in
    /// the same process carries its own memo and does not see this one's
    /// resolutions (pre-existing registry topology, unchanged here).
    ///
    /// [`Runner::add_module`]: Self::add_module
    pub(crate) resolution_memo: Arc<crate::core::runtime::module_loader::ResolutionMemo>,
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
                TokioRuntimeVariant::OwnedTokioRuntime(rt)
            }
        };

        // Make the host's tokio handle available to the
        // `HOST_RUNTIME_OPS_VTABLE` callbacks. Cdylib-side
        // `RuntimeOpsShim` methods post submit-with-completion calls
        // that the host's vtable spawns onto this handle; the cdylib
        // awaits the completion through its own tokio runtime via a
        // `oneshot` bridge. Plugins never see this handle directly.
        crate::core::plugin::host_services::install_host_runtime_tokio_handle(
            tokio_runtime_variant.handle(),
        );

        // Load a local .env if present (RUST_LOG and other dev overrides).
        let _ = dotenvy::dotenv();

        // Generate runtime ID first — used as service_name for telemetry.
        let runtime_id = Arc::new(RuntimeUniqueId::from_env_or_generate());

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

        // Get STREAMLIB_HOME and run init hooks (once per process)
        let streamlib_home = crate::core::streamlib_home::get_streamlib_home();
        tracing::debug!("STREAMLIB_HOME: {}", streamlib_home.display());
        crate::core::runtime_hooks::run_init_hooks(&streamlib_home)?;

        // The engine substrate is empty by construction — there are no
        // compile-time-linked processors. Callers populate the
        // `PROCESSOR_REGISTRY` after `Runner::new()` returns via
        // `runtime.add_module(...)` / `runtime.add_module_with(...)`
        // (which dlopen plugin cdylibs and register through the host's
        // `processor_register` callback) or via direct
        // `PROCESSOR_REGISTRY.register::<P>()` calls in-process.

        // Bridge iceoryx2's internal log records into streamlib tracing
        // before creating the iceoryx2 Node so any iceoryx2 emit at
        // construction time lands in the unified JSONL pipeline. The
        // host's bridge value is the same `&'static dyn Log` that
        // [`crate::core::plugin::host_services`] hands to plugin cdylibs
        // via `HostServices.iceoryx2_logger_ptr` so the host and every
        // plugin converge on a single logger.
        crate::core::logging::iceoryx2_log_bridge::install_iceoryx2_log_bridge_for_self();

        // Create iceoryx2 Node early so PUBSUB can initialize before start().
        // The node is cloned into RuntimeContext during start().
        tracing::info!("[new] Creating iceoryx2 Node...");
        let iceoryx2_node = Iceoryx2Node::new()?;
        tracing::info!("[new] iceoryx2 Node created");

        // Initialize global PUBSUB with iceoryx2 backend.
        // Must happen before any subscribe() calls (GraphChangeListener below).
        PUBSUB.init(&runtime_id, iceoryx2_node.clone());

        // Bring up the per-runtime surface-sharing service. Each runtime owns
        // a unique Unix socket at $XDG_RUNTIME_DIR/streamlib-<uuid>.sock that
        // its polyglot subprocesses connect to via STREAMLIB_SURFACE_SOCKET.
        // No external daemon is required.
        #[cfg(target_os = "linux")]
        let (surface_service, surface_socket_path) = bring_up_surface_service(&runtime_id)?;

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
        PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener));

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
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
            _logging_guard,
            setup_hooks: Arc::new(Mutex::new(Vec::new())),
            pipeline_name: Arc::new(Mutex::new(None)),
            build_orchestrator: Arc::new(Mutex::new(None)),
            loading_modules: Arc::new(Mutex::new(std::collections::HashMap::new())),
            resolution_memo: Arc::new(crate::core::runtime::module_loader::ResolutionMemo::new()),
        }))
    }

    /// Construct a runtime with a [`BuildOrchestrator`] wired so that
    /// build-requiring module loads ([`Strategy::Path`] /
    /// [`Strategy::Git`] with a non-`NeverBuild` [`BuildPolicy`]) can
    /// materialize from source. The conventional construction for dev
    /// loops, runtime-authoring hosts (AI agents, CLIs, daemons), and CI
    /// — the SDK's `auto-build` feature wires the default polyglot
    /// orchestrator here for you.
    ///
    /// [`BuildOrchestrator`]: crate::core::runtime::module_loader::BuildOrchestrator
    /// [`Strategy::Path`]: crate::core::runtime::module_loader::Strategy::Path
    /// [`Strategy::Git`]: crate::core::runtime::module_loader::Strategy::Git
    /// [`BuildPolicy`]: crate::core::runtime::module_loader::BuildPolicy
    pub fn new_with_orchestrator(
        orchestrator: impl crate::core::runtime::module_loader::BuildOrchestrator,
    ) -> Result<Arc<Self>> {
        let runner = Self::new()?;
        runner.set_build_orchestrator(orchestrator);
        Ok(runner)
    }

    /// Wire (or replace) the [`BuildOrchestrator`] after construction.
    /// Frozen `.slpkg`-only deployments never call this and are
    /// therefore compiler-free by construction.
    ///
    /// [`BuildOrchestrator`]: crate::core::runtime::module_loader::BuildOrchestrator
    pub fn set_build_orchestrator(
        &self,
        orchestrator: impl crate::core::runtime::module_loader::BuildOrchestrator,
    ) {
        *self.build_orchestrator.lock() = Some(Arc::new(orchestrator));
    }

    /// Register a one-shot hook to run during [`Self::start`], after the
    /// [`GpuContext`] is initialized and before any processor's
    /// `setup()` runs. The hook receives the live `Arc<GpuContext>`,
    /// giving caller code a window to register engine extensions —
    /// today the canonical use is wiring a
    /// [`crate::core::context::CpuReadbackBridge`] via
    /// [`crate::core::context::GpuContext::set_cpu_readback_bridge`]
    /// before subprocess processors fire their first
    /// `acquire_cpu_readback`. Hooks fire FIFO; a hook returning `Err`
    /// aborts `start()` with the same error.
    pub fn install_setup_hook<F>(&self, hook: F)
    where
        F: FnOnce(&GpuContext) -> Result<()> + Send + 'static,
    {
        self.setup_hooks.lock().push(Box::new(hook));
    }

    /// Path of the per-runtime surface-sharing Unix socket.
    ///
    /// Bound during [`Runner::new`] at
    /// `$XDG_RUNTIME_DIR/streamlib-<runtime_uuid>.sock`. Polyglot
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

    /// This runtime's iceoryx2 node. Exposed so external loaders
    /// (embedding apps that `dlopen` a cdylib outside `add_module`)
    /// can hand it to
    /// [`crate::core::plugin::host_services::runtime_facing::host_services_for_self`]
    /// when assembling the `HostServices` payload for a plugin
    /// register callback.
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

        // Update config in graph and queue operation
        self.compiler.scope(|graph, tx| {
            if let Some(processor) = graph.traversal_mut().v(processor_id).first_mut() {
                processor.set_config(config_json);
            }

            tx.log(PendingOperation::UpdateProcessorConfig(
                processor_id.clone(),
            ));
        });

        // Publish event
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::ProcessorConfigDidChange {
                processor_id: processor_id.clone(),
            }),
        );

        // Notify listeners that graph changed (triggers commit via GraphChangeListener)
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
        );

        Ok(())
    }

    // =========================================================================
    // Module Loading — see `core/runtime/module_loader/` for the impl.
    // =========================================================================
    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Start the runtime.
    ///
    /// Takes `&Arc<Self>` to allow passing the runtime to processors via RuntimeContext.
    /// Processors can then call runtime operations directly without indirection.
    #[tracing::instrument(name = "runtime.start", skip_all)]
    pub fn start(self: &Arc<Self>) -> Result<()> {
        // Hard barrier: refuse to run the graph while any module load is
        // still in flight (its processor types may not be registered
        // yet). Await pending loads — e.g. via `await_modules` — first.
        let pending = self.pending_module_loads();
        if !pending.is_empty() {
            return Err(
                crate::core::runtime::module_loader::AddModuleError::ModulesStillLoading {
                    idents: pending,
                }
                .into(),
            );
        }

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
            // `SurfaceStore::new` constructs the PluginAbiObject from a fresh
            // `Arc<SurfaceStoreInner>`. Method dispatch goes through
            // the host's `SurfaceStoreVTable`.
            let surface_store = SurfaceStore::new(socket_path.clone(), self.runtime_id.to_string());
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

        // Clone iceoryx2 Node (created in new() for early PUBSUB initialization)
        let iceoryx2_node = self.iceoryx2_node.clone();

        // Create audio clock - platform-specific for best precision
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
            #[cfg(target_os = "linux")]
            self.surface_socket_path.clone(),
        ));
        *self.runtime_context.lock() = Some(Arc::clone(&runtime_ctx));

        // Platform-specific setup (macOS NSApplication, Windows Win32, etc.)
        // RuntimeContext handles all platform-specific details internally.
        runtime_ctx.ensure_platform_ready()?;

        // Start the audio clock
        tracing::info!("[start] Starting audio clock");
        audio_clock.start()?;

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
    #[tracing::instrument(name = "runtime.stop", skip_all)]
    pub fn stop(&self) -> Result<()> {
        tracing::info!("[stop] Beginning graceful shutdown");
        *self.status.lock() = RuntimeStatus::Stopping;
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

        if let Some(ctx) = runtime_ctx {
            tracing::debug!("[stop] Committing processor teardown");
            self.compiler.commit(&ctx)?;
            tracing::debug!("[stop] Processor teardown complete");

            // Stop the audio clock
            tracing::debug!("[stop] Stopping audio clock");
            if let Err(e) = ctx.audio_clock().stop() {
                tracing::warn!("[stop] Failed to stop audio clock: {}", e);
            }

            // Cleanup SurfaceStore - releases all surfaces and disconnects
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
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
        Ok(())
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
                *state.0.lock() = ProcessorState::Paused;
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
                *state.0.lock() = ProcessorState::Running;
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

    /// Block until shutdown signal (Ctrl+C, SIGTERM, Cmd+Q).
    pub fn wait_for_signal(self: &Arc<Self>) -> Result<()> {
        self.wait_for_signal_with(|_| ControlFlow::Continue(()))
    }

    /// Block until shutdown signal, with periodic callback for dynamic control.
    pub fn wait_for_signal_with<F>(self: &Arc<Self>, mut callback: F) -> Result<()>
    where
        F: FnMut(&Self) -> ControlFlow<()>,
    {
        // Install signal handlers
        crate::core::signals::install_signal_handlers().map_err(|e| {
            crate::core::Error::Configuration(format!("Failed to install signal handlers: {}", e))
        })?;

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
        PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&shutdown_listener));

        // On macOS, run the NSApplication event loop (required for GUI)
        #[cfg(target_os = "macos")]
        {
            let runtime = Arc::clone(self);
            let runtime_for_callback = Arc::clone(self);
            crate::apple::runtime_ext::run_macos_event_loop(
                move || {
                    // Called by applicationWillTerminate before app exits
                    if let Err(e) = runtime.stop() {
                        tracing::error!("Failed to stop runtime during shutdown: {}", e);
                    }
                },
                move || callback(&runtime_for_callback),
            );
            // Note: run_macos_event_loop never returns - app terminates after stop callback
            Ok(())
        }

        // Non-macOS: poll loop
        #[cfg(not(target_os = "macos"))]
        {
            while !shutdown_flag.load(Ordering::SeqCst) {
                // Call user callback
                if let ControlFlow::Break(()) = callback(self) {
                    break;
                }

                // Small sleep to avoid busy-waiting
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            // Auto-stop on exit
            self.stop()?;

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

    // =========================================================================
    // Introspection
    // =========================================================================

    /// Export graph state as JSON including topology, processor states, metrics, and buffer levels.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        self.compiler.scope(|graph, _tx| {
            serde_json::to_value(&*graph)
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
    /// resolve and build referenced packages from the registry first — use
    /// [`Self::load_graph_snapshot_with_resolving`].
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
    /// turnkey path that resolves missing modules from the registry, use
    /// [`Self::load_graph_snapshot_from_path_with_resolving`].
    pub fn load_graph_snapshot_from_path(&self, path: &std::path::Path) -> Result<()> {
        let snapshot = crate::core::graph_snapshot::GraphSnapshot::from_json_file(path)?;

        if let Some(name) = &snapshot.name {
            tracing::info!("Loading pipeline '{}' from {}", name, path.display());
        } else {
            tracing::info!("Loading pipeline from {}", path.display());
        }

        self.load_graph_snapshot(&snapshot)
    }

    /// Like [`Runner::load_graph_snapshot`], but first resolves and loads —
    /// from the registry — any module referenced by the snapshot whose
    /// processor type isn't registered yet, so a graph snapshot is
    /// self-contained: `streamlib-runtime --snapshot graph.json` brings up a
    /// full pipeline turnkey instead of failing on the first unregistered
    /// processor type (the bare runtime only registers the api-server
    /// in-process at boot).
    ///
    /// One `add_module` per referenced package (deduped), resolved from the
    /// registry at its highest published version and built on the host —
    /// requires a build orchestrator (e.g. `RunnerAutoBuild::with_auto_build`).
    /// Fails loud, naming the package, if a referenced module can't resolve.
    pub async fn load_graph_snapshot_with_resolving(
        &self,
        snapshot: &crate::core::graph_snapshot::GraphSnapshot,
    ) -> Result<()> {
        use crate::core::processors::PROCESSOR_REGISTRY;
        use crate::core::runtime::module_loader::{BuildPolicy, Strategy};
        use streamlib_idents::{ModuleIdent, SemVerRange};

        // NB: do NOT validate() here — validate() rejects unregistered processor
        // types, which is exactly what this pass resolves. Reading the structured
        // processor_type fields needs no validation; load_graph_snapshot below
        // validates (structure + registration) once the modules are loaded.

        // The unique packages whose processor types aren't already registered
        // (e.g. the api-server type is registered in-process at boot). One
        // add_module per package — a snapshot may reference several
        // processors from the same package.
        let mut seen: std::collections::HashSet<streamlib_idents::PackageRef> =
            std::collections::HashSet::new();
        let mut to_load: Vec<ModuleIdent> = Vec::new();
        for proc_def in &snapshot.processors {
            let ty = &proc_def.processor_type;
            if PROCESSOR_REGISTRY.port_info(ty).is_some() {
                continue;
            }
            let module = ModuleIdent::new(ty.org.clone(), ty.package.clone(), SemVerRange::Any);
            if seen.insert(module.package_ref()) {
                to_load.push(module);
            }
        }

        for module in to_load {
            tracing::info!(
                "Snapshot: resolving module '{}' from registry",
                module.package_ref()
            );
            self.add_module_with(
                module.clone(),
                Strategy::Registry {
                    version_req: SemVerRange::Any,
                    build: BuildPolicy::IfStale,
                },
            )
            .await
            .map_err(|e| {
                Error::GraphError(format!(
                    "snapshot module resolution failed for '{}': {e}",
                    module.package_ref()
                ))
            })?;
        }

        self.load_graph_snapshot(snapshot)
    }

    /// Path variant of [`Runner::load_graph_snapshot_with_resolving`].
    pub async fn load_graph_snapshot_from_path_with_resolving(
        &self,
        path: &std::path::Path,
    ) -> Result<()> {
        let snapshot = crate::core::graph_snapshot::GraphSnapshot::from_json_file(path)?;
        if let Some(name) = &snapshot.name {
            tracing::info!(
                "Loading pipeline '{}' (resolving modules) from {}",
                name,
                path.display()
            );
        } else {
            tracing::info!(
                "Loading pipeline (resolving modules) from {}",
                path.display()
            );
        }
        self.load_graph_snapshot_with_resolving(&snapshot).await
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
                let short = node.processor_type.r#type.as_str();
                let base = pascal_to_camel(short);

                let count = alias_counts.entry(base.clone()).or_insert(0);
                *count += 1;
                let alias = if *count == 1 {
                    base.clone()
                } else {
                    format!("{}_{}", base, count)
                };

                id_to_alias.insert(node.id.to_string(), alias.clone());

                let display_name =
                    (node.display_name.as_str() != short).then(|| node.display_name.clone());

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
    runtime_id: &RuntimeUniqueId,
) -> Result<(
    Arc<Mutex<Option<crate::linux::surface_share::UnixSocketSurfaceService>>>,
    std::path::PathBuf,
)> {
    use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};

    let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        Error::Runtime(
            "XDG_RUNTIME_DIR is not set. The runtime needs a writable directory \
             for its per-runtime surface-sharing socket — typically /run/user/<uid>. \
             Set XDG_RUNTIME_DIR or run under a session manager that provides it."
                .to_string(),
        )
    })?;

    let socket_path =
        std::path::PathBuf::from(xdg_runtime_dir).join(format!("streamlib-{}.sock", runtime_id));

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

    let mut service = UnixSocketSurfaceService::new(SurfaceShareState::new(), socket_path.clone());
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

    Ok((Arc::new(Mutex::new(Some(service))), socket_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // All Runner::new() tests are `#[serial]` because the runtime
    // reads/writes process-global env vars (XDG_RUNTIME_DIR,
    // STREAMLIB_RUNTIME_ID) and its PUBSUB/iceoryx2/telemetry plumbing
    // races when multiple runtimes construct concurrently. The test
    // module's `#[serial]` default group serializes every test that
    // constructs a Runner so nobody reads env mid-mutation.

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

        /// Replace XDG_RUNTIME_DIR with a fresh tempdir for the duration of the
        /// closure. Tests using this must be `#[serial]` so no other runtime
        /// construct reads the mutated env.
        fn with_isolated_xdg_runtime_dir<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
            let prev = std::env::var_os("XDG_RUNTIME_DIR");
            let tmp = tempfile::tempdir().expect("tempdir");
            // SAFETY: tests are serialized via #[serial]; no concurrent env mutation.
            unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
            }
            let result = f(tmp.path());
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                    None => std::env::remove_var("XDG_RUNTIME_DIR"),
                }
            }
            result
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
                    socket_path.starts_with(xdg),
                    "socket {} should be under XDG_RUNTIME_DIR {}",
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
        fn runtime_fails_fast_when_xdg_runtime_dir_missing() {
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

            let err = match result {
                Err(e) => e,
                Ok(_) => panic!("runtime should refuse to start without XDG_RUNTIME_DIR"),
            };
            let msg = err.to_string();
            assert!(
                msg.contains("XDG_RUNTIME_DIR"),
                "error should name XDG_RUNTIME_DIR; got: {msg}"
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

                let stale_path = xdg.join(format!("streamlib-{pinned_id}.sock"));
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
}
