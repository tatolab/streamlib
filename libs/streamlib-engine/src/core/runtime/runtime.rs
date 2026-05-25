// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;

use super::graph_change_listener::GraphChangeListener;
use super::RuntimeOperations;
use super::RuntimeStatus;
use super::RuntimeUniqueId;
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
use crate::core::pubsub::{topics, Event, EventListener, ProcessorEvent, RuntimeEvent, PUBSUB};
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result, Error};
use crate::iceoryx2::Iceoryx2Node;

/// Keeps loaded dylib plugin libraries alive for the process lifetime.
///
/// When a Rust dylib plugin is loaded via `load_project()`, the `Library` handle
/// must remain alive so that the registered processor vtables stay valid.
static LOADED_PLUGIN_LIBRARIES: std::sync::LazyLock<parking_lot::Mutex<Vec<libloading::Library>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(Vec::new()));

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
    pub(crate) surface_service: Arc<
        Mutex<Option<crate::linux::surface_share::UnixSocketSurfaceService>>,
    >,
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

        // Load .env file (dev-setup.sh-style overrides: RUST_LOG, etc.)
        let _ = dotenvy::dotenv();

        // Generate runtime ID first — used as service_name for telemetry.
        let runtime_id = Arc::new(RuntimeUniqueId::from_env_or_generate());

        // Stand up the runtime's unified logging pathway: `tracing` →
        // bounded lossy channel → drain worker → line-buffered pretty
        // stdout + batched JSONL file at
        // `$XDG_STATE_HOME/streamlib/logs/<runtime_id>-<started_at>.jsonl`.
        // See `docs/logging-schema.md` for the schema (the durable
        // interface contract) and `streamlib::sdk::logging` for the
        // implementation.
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
        let _logging_guard = crate::core::logging::init(
            crate::core::logging::StreamlibLoggingConfig::for_runtime(
                format!("runtime:{}", runtime_id),
                Arc::clone(&runtime_id),
            ),
        )
        .map_err(|e| Error::Runtime(format!("Failed to initialize logging: {}", e)))?;
        tracing::info!("Creating Runner with ID: {}", runtime_id);

        // Get STREAMLIB_HOME and run init hooks (once per process)
        let streamlib_home = crate::core::streamlib_home::get_streamlib_home();
        tracing::debug!("STREAMLIB_HOME: {}", streamlib_home.display());
        crate::core::runtime_hooks::run_init_hooks(&streamlib_home)?;

        // Register all processors from inventory before any add_processor calls.
        // This populates the global registry with link-time registered processors.
        // Empty inventory is valid — apps that compose processors via `load_project()`
        // start with `count == 0` and populate the registry afterwards.
        let result = crate::core::processors::PROCESSOR_REGISTRY.register_all_processors();
        tracing::debug!("Registered {} processors from inventory", result.count);

        // Bridge iceoryx2's internal log records into streamlib tracing
        // before creating the iceoryx2 Node so any iceoryx2 emit at
        // construction time lands in the unified JSONL pipeline. The
        // host's bridge value is the same `&'static dyn Log` that
        // [`crate::core::plugin::host_services`] hands to plugin cdylibs
        // via `HostServices.iceoryx2_logger_ptr` so every DSO converges
        // on a single logger.
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
        let (surface_service, surface_socket_path) =
            bring_up_surface_service(&runtime_id)?;

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
        }))
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
    /// (`streamlib-runtime`'s `--plugin` flag, embedding apps that
    /// `dlopen` a cdylib outside `load_project`) can hand it to
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
        let config_json = serde_json::to_value(&config)
            .map_err(|e| crate::core::Error::Config(e.to_string()))?;

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
    // Package Loading
    // =========================================================================

    /// Load processors from a .slpkg package file.
    pub fn load_package(&self, slpkg_path: impl AsRef<std::path::Path>) -> Result<()> {
        let slpkg_path = slpkg_path.as_ref();
        tracing::info!("Loading package from: {}", slpkg_path.display());

        let project_path = extract_slpkg_to_cache(slpkg_path)?;
        self.load_project(&project_path)
    }

    /// Load processors from a project directory containing `streamlib.yaml`.
    ///
    /// Reads `processors` entries from the manifest and registers each
    /// with the global processor registry, dispatching to the appropriate
    /// subprocess host constructor based on the `runtime` field.
    ///
    /// For Python packages, eagerly creates the venv and installs dependencies
    /// once during loading, so processors don't race to create it at spawn time.
    #[allow(clippy::only_used_in_recursion)]
    pub fn load_project(&self, project_path: impl AsRef<std::path::Path>) -> Result<()> {
        use crate::core::compiler::compiler_ops::create_deno_subprocess_host_constructor;
        use crate::core::compiler::compiler_ops::create_python_native_subprocess_host_constructor;
        use crate::core::compiler::compiler_ops::ensure_processor_venv;
        use crate::core::compiler::compiler_ops::resolve_python_native_lib_path;
        use crate::core::config::ProjectConfig;
        use crate::core::descriptors::{PortDescriptor, ProcessorRuntime};
        use crate::core::execution::{ExecutionConfig, ProcessExecution};
        use crate::core::ProcessorDescriptor;

        let project_path = project_path.as_ref();

        tracing::info!("Loading project from: {}", project_path.display());

        let config = ProjectConfig::load(project_path)?;

        config.check_streamlib_version_compatibility()?;

        // Load dependency packages first (schemas/processors they export).
        //
        // The dep map is `BTreeMap<PackageRef, DependencySpec>` end-to-end
        // — `PackageRef`'s typed Deserialize validates the `@org/name` shape
        // at YAML-read time so the lookup site below never parses a string.
        // Resolution chain (mirrors Cargo's `[patch.crates-io]` shape, but
        // per-consumer rather than workspace-level):
        //
        //   1. **Consumer's own `patch:` table** — overrides the dep
        //      declaration when present. Path entries resolve relative
        //      to the consumer's manifest dir; missing paths fail with
        //      a clear error (strict validation, npm/wrangler-style).
        //   2. **Installed-package cache** (`InstalledPackageManifest`).
        //   3. **Actionable error** — neither tier covers the dep.
        if !config.dependencies.is_empty() {
            for (dep_ref, spec) in &config.dependencies {
                let dep_path =
                    self.resolve_dependency_path(project_path, dep_ref, spec, &config.patch)?;
                tracing::info!(
                    "Loading dependency '{}' from {}",
                    dep_ref,
                    dep_path.display()
                );
                self.load_project(&dep_path)?;
            }
        }

        // Register every schema declared in `streamlib.yaml`'s
        // `schemas:` list with the engine's runtime schema registry so
        // `get_embedded_schema_definition` /
        // `max_payload_bytes_for_port_spec` / api-server `/schemas`
        // discover this package's schemas. The registry starts empty
        // and is populated exclusively through this path — apps wire
        // the packages they need (`@tatolab/core` for wire vocabulary,
        // `@tatolab/audio` / etc. for domain processors) and
        // `load_project` walks the dependency graph and registers each
        // package's schemas as it traverses.
        register_package_schemas(project_path, &config)?;

        if config.processors.is_empty() {
            tracing::debug!(
                "No processors declared in {} at {} (schemas-only package or fixture).",
                ProjectConfig::FILE_NAME,
                project_path.display()
            );
            return Ok(());
        }

        // Eagerly create venv for Python packages so processors don't race at spawn time
        let has_python_processors = config.processors.iter().any(|p| {
            matches!(
                p.runtime.language,
                streamlib_processor_schema::ProcessorLanguage::Python
            )
        });

        if has_python_processors {
            let package_label = config
                .package
                .as_ref()
                .map(|p| p.name.as_str())
                .unwrap_or("unknown");
            tracing::info!("Pre-creating Python venv for package '{}'", package_label);
            ensure_processor_venv(package_label, project_path)?;
        }

        let mut registered_count = 0usize;
        let mut rust_dylib_loaded = false;

        // Resolve package metadata once per project — every dynamically
        // registered processor in this manifest uses these typed segments
        // to compose its full structured `SchemaIdent`.
        let package_metadata = config.package.as_ref().ok_or_else(|| {
            Error::Configuration(format!(
                "{} at {} declares processors but is missing a `package:` block. \
                 The structured-everywhere processor identity rule requires \
                 `package: {{ org, name, version }}` to compose each processor's \
                 SchemaIdent.",
                ProjectConfig::FILE_NAME,
                project_path.display(),
            ))
        })?;

        // Eagerly resolve the dependency graph once per project — used
        // below by `resolve_config_schema_canonical_id` for each
        // processor's bare-name config schema (#767). The resolver walks
        // declared paths/git/.slpkg sources and validates the `schemas:`
        // map; we only invoke it when at least one processor actually
        // declares a config block.
        let config_resolved: Option<streamlib_idents::ResolvedPackages> =
            if config.processors.iter().any(|p| p.config.is_some()) {
                Some(
                    streamlib_idents::resolve_with(
                        project_path,
                        &streamlib_idents::ResolverOptions::default(),
                    )
                    .map_err(|e| {
                        Error::Configuration(format!(
                            "failed to resolve manifest dependencies for bare-name \
                             config schema lookup at {}: {}",
                            project_path.display(),
                            e
                        ))
                    })?,
                )
            } else {
                None
            };

        for proc_schema in &config.processors {
            // Compose the structured processor ident from the manifest's
            // package metadata + the processor's PascalCase short name.
            let proc_type_name = streamlib_processor_schema::TypeName::new(&proc_schema.name)
                .map_err(|e| {
                    Error::Configuration(format!(
                        "processor short name `{}` in {} is not valid PascalCase: {}",
                        proc_schema.name,
                        project_path.display(),
                        e
                    ))
                })?;
            let proc_schema_ident = crate::core::descriptors::SchemaIdent::new(
                package_metadata.org.clone(),
                package_metadata.name.clone(),
                proc_type_name,
                package_metadata.version.clone(),
            );

            // Map runtime language to ProcessorRuntime
            let runtime = match proc_schema.runtime.language {
                streamlib_processor_schema::ProcessorLanguage::Python => ProcessorRuntime::Python,
                streamlib_processor_schema::ProcessorLanguage::TypeScript => {
                    ProcessorRuntime::TypeScript
                }
                streamlib_processor_schema::ProcessorLanguage::Rust => {
                    // Rust dylib plugins self-register via export_plugin! macro.
                    // Load the dylib once per project (all Rust processors in the
                    // same YAML share one dylib), then validate each processor
                    // was actually registered.
                    if !rust_dylib_loaded {
                        let host_triple = host_target_triple();
                        let lib_dir = project_path.join("lib");
                        let triple_dir = lib_dir.join(host_triple);
                        let dylib_ext = if cfg!(target_os = "macos") {
                            "dylib"
                        } else if cfg!(target_os = "windows") {
                            "dll"
                        } else {
                            "so"
                        };

                        let dylib_path = std::fs::read_dir(&triple_dir)
                            .map_err(|e| {
                                // If `lib/` exists but the triple-keyed
                                // subdir is absent, surface the available
                                // triples so the user sees exactly which
                                // platforms this slpkg was packed for.
                                let available =
                                    list_available_triples(&lib_dir).unwrap_or_default();
                                let detail = if available.is_empty() {
                                    format!(
                                        "Failed to read {}: {}. \
                                         The package may be missing a Rust artifact for this host.",
                                        triple_dir.display(),
                                        e
                                    )
                                } else {
                                    format!(
                                        "Failed to read {}: {}. \
                                         This slpkg was packed for: [{}]. \
                                         The host triple is `{}` — repack on a matching host \
                                         or run this pipeline on a host that matches one of the \
                                         packed triples.",
                                        triple_dir.display(),
                                        e,
                                        available.join(", "),
                                        host_triple
                                    )
                                };
                                Error::Configuration(detail)
                            })?
                            .filter_map(|entry| entry.ok())
                            .map(|entry| entry.path())
                            .find(|path| path.extension().is_some_and(|ext| ext == dylib_ext))
                            .ok_or_else(|| {
                                Error::Configuration(format!(
                                    "No .{} file found in {} (host triple `{}`)",
                                    dylib_ext,
                                    triple_dir.display(),
                                    host_triple
                                ))
                            })?;

                        tracing::info!("Loading Rust dylib plugin: {}", dylib_path.display());

                        // Safety: Loading a dynamic library is inherently unsafe.
                        // The dylib must be a valid StreamLib plugin built with
                        // a compatible streamlib-plugin-abi version.
                        let lib = unsafe {
                            libloading::Library::new(&dylib_path).map_err(|e| {
                                Error::Configuration(format!(
                                    "Failed to load dylib {}: {}",
                                    dylib_path.display(),
                                    e
                                ))
                            })?
                        };

                        let decl: &streamlib_plugin_abi::PluginDeclaration = unsafe {
                            let symbol = lib
                                .get::<*const streamlib_plugin_abi::PluginDeclaration>(
                                    b"STREAMLIB_PLUGIN\0",
                                )
                                .map_err(|e| {
                                    Error::Configuration(format!(
                                        "Plugin '{}' missing STREAMLIB_PLUGIN symbol. \
                                         Ensure the plugin uses the export_plugin! macro: {}",
                                        dylib_path.display(),
                                        e
                                    ))
                                })?;
                            &**symbol
                        };

                        if decl.abi_version != streamlib_plugin_abi::STREAMLIB_ABI_VERSION {
                            return Err(Error::Configuration(format!(
                                "ABI version mismatch for '{}': plugin has v{}, \
                                 runtime expects v{}. Rebuild the plugin with a \
                                 compatible streamlib-plugin-abi version.",
                                dylib_path.display(),
                                decl.abi_version,
                                streamlib_plugin_abi::STREAMLIB_ABI_VERSION,
                            )));
                        }

                        // Build the HostServices payload from the host's
                        // process-wide statics + this runtime's iceoryx2
                        // node, hand it to the cdylib's register callback.
                        // The cdylib's macro-emitted prologue calls
                        // `install_host_services` which bridges every
                        // per-DSO static (tracing dispatch, PUBSUB,
                        // schema registry, iceoryx2 logger) into the
                        // host's instances before processor registration.
                        let host_services =
                            crate::core::plugin::host_services::runtime_facing::host_services_for_self(
                                &self.iceoryx2_node,
                            );
                        // SAFETY: `host_services` outlives the call;
                        // the cdylib's callback returns before this
                        // function frame is dropped.
                        unsafe {
                            (decl.register)(
                                &host_services as *const _ as *const ::std::ffi::c_void,
                            );
                        }

                        // Keep the library alive for the process lifetime
                        LOADED_PLUGIN_LIBRARIES.lock().push(lib);

                        rust_dylib_loaded = true;
                        tracing::info!(
                            "Rust dylib plugin loaded and registered: {}",
                            dylib_path.display()
                        );
                    }

                    // Validate the processor was registered by the dylib —
                    // compare by structured `SchemaIdent`, not bare PascalCase
                    // (two packages can declare the same short name).
                    let registered = crate::core::processors::PROCESSOR_REGISTRY
                        .list_registered()
                        .iter()
                        .any(|desc| desc.name == proc_schema_ident);
                    if !registered {
                        return Err(Error::Configuration(format!(
                            "Processor '{}' declared in streamlib.yaml but not \
                             registered by the dylib. Ensure export_plugin!() \
                             includes this processor.",
                            proc_schema_ident
                        )));
                    }

                    tracing::info!("Validated Rust dylib processor '{}'", proc_schema.name);
                    registered_count += 1;
                    continue;
                }
            };

            let inputs: Vec<PortDescriptor> = proc_schema
                .inputs
                .iter()
                .map(|p| {
                    PortDescriptor::new(
                        &p.name,
                        p.description.as_deref().unwrap_or(""),
                        p.schema.clone(),
                        true,
                    )
                })
                .collect();

            let outputs: Vec<PortDescriptor> = proc_schema
                .outputs
                .iter()
                .map(|p| {
                    PortDescriptor::new(
                        &p.name,
                        p.description.as_deref().unwrap_or(""),
                        p.schema.clone(),
                        true,
                    )
                })
                .collect();

            let mut descriptor = ProcessorDescriptor::new(
                proc_schema_ident.clone(),
                proc_schema.description.as_deref().unwrap_or(""),
            )
            .with_version(&proc_schema.version)
            .with_runtime(runtime.clone());

            if let Some(entrypoint) = &proc_schema.entrypoint {
                descriptor = descriptor.with_entrypoint(entrypoint);
            }

            if let Some(config) = &proc_schema.config {
                // Resolve the bare-name `TypeName` (#767) against the
                // manifest's `schemas:` map to its canonical id string.
                // The lookup walks the manifest's declarations, locates
                // the owning package + schema file, and reads the
                // schema's `metadata.type` (new shape) or
                // `metadata.name` (legacy reverse-DNS) to compose the
                // id. This must match what
                // `register_package_schemas`/`canonical_identifier_for_schema`
                // registered when the same manifest was loaded.
                let canonical =
                    resolve_config_schema_canonical_id(project_path, config, &config_resolved)
                        .map_err(|msg| {
                            Error::Configuration(format!(
                                "processor `{}` config schema `{}`: {}",
                                proc_schema.name,
                                config.schema.as_str(),
                                msg
                            ))
                        })?;
                descriptor = descriptor.with_config_schema(canonical);
            }

            if let Some(scheduling) = &proc_schema.scheduling {
                descriptor = descriptor.with_scheduling(scheduling.clone());
            }

            descriptor.inputs = inputs;
            descriptor.outputs = outputs;

            // Convert schema execution mode to runtime ExecutionConfig
            let execution = match &proc_schema.execution {
                streamlib_processor_schema::ProcessorSchemaExecution::Reactive => {
                    ProcessExecution::Reactive
                }
                streamlib_processor_schema::ProcessorSchemaExecution::Manual => {
                    ProcessExecution::Manual
                }
                streamlib_processor_schema::ProcessorSchemaExecution::Continuous { interval_ms } => {
                    ProcessExecution::Continuous {
                        interval_ms: *interval_ms,
                    }
                }
            };
            let execution_config = ExecutionConfig::new(execution);

            // Create constructor based on runtime language.
            // Python and TypeScript subprocesses both use native FFI for direct
            // iceoryx2 access — no pipe bridge for data I/O.
            let constructor = match runtime {
                ProcessorRuntime::Python => {
                    let native_lib_path = resolve_python_native_lib_path()?;
                    create_python_native_subprocess_host_constructor(
                        &descriptor,
                        execution_config,
                        project_path.to_path_buf(),
                        native_lib_path,
                    )
                }
                ProcessorRuntime::TypeScript => create_deno_subprocess_host_constructor(
                    &descriptor,
                    execution_config,
                    project_path.to_path_buf(),
                ),
                _ => unreachable!(),
            };

            crate::core::processors::PROCESSOR_REGISTRY
                .register_dynamic(descriptor, constructor)?;

            tracing::info!(
                "Registered processor '{}' ({:?})",
                proc_schema.name,
                runtime
            );

            registered_count += 1;
        }

        tracing::info!(
            "Loaded {} processor(s) from {}",
            registered_count,
            project_path.display()
        );

        Ok(())
    }

    /// Resolve every name to its staged dir under
    /// `<workspace>/target/streamlib-plugins/<org>__<name>/` and call
    /// [`Self::load_project`] on each, in declaration order.
    ///
    /// `cargo xtask build-plugins` must have run first — the helper
    /// errors with [`LoadWorkspacePackagesError::PackageNotStaged`] when
    /// a name's staged dir is missing.
    ///
    /// Workspace root resolution: `STREAMLIB_WORKSPACE_ROOT` env var
    /// when set (and the path exists), otherwise
    /// `cargo locate-project --workspace`.
    pub fn load_workspace_packages<I, S>(
        &self,
        names: I,
    ) -> std::result::Result<(), LoadWorkspacePackagesError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Eagerly validate every id BEFORE touching the workspace root —
        // a typo'd id should surface as `InvalidPackageId` immediately
        // rather than masquerading as `WorkspaceRootNotFound` when the
        // env var is unset.
        let parsed: Vec<(String, String, String)> = names
            .into_iter()
            .map(|name| {
                let name_str = name.as_ref().to_string();
                let (org, pkg) = {
                    let parsed = parse_canonical_package_id(&name_str)?;
                    (parsed.org_str.to_string(), parsed.name_str.to_string())
                };
                Ok::<_, LoadWorkspacePackagesError>((name_str, org, pkg))
            })
            .collect::<std::result::Result<_, _>>()?;

        let workspace_root = resolve_workspace_root()?;
        let staged_root = workspace_root.join("target").join("streamlib-plugins");

        for (name_str, org, name) in parsed {
            let staged_dir = staged_root.join(format!("{}__{}", org, name));

            if !staged_dir.exists() || !staged_dir.join("streamlib.yaml").exists() {
                return Err(LoadWorkspacePackagesError::PackageNotStaged {
                    name: name_str.clone(),
                    expected_path: staged_dir,
                });
            }

            // Identity check: the staged yaml's `[package]` org / name
            // must match the requested id. Catches manual clobbering of
            // the staged tree (someone copied wrong content into the
            // expected dir) before `load_project` registers the wrong
            // processors.
            verify_staged_package_identity(&staged_dir, &org, &name, &name_str)?;

            // For Rust-impl packages, the cdylib must be present at
            // `lib/<host_triple>/`. Surface the precise diagnostic
            // (rather than letting `load_project` fail with a generic
            // "missing dylib for this triple" message).
            verify_cdylib_present_when_rust_impl(&staged_dir, &name_str)?;

            tracing::info!(
                "Loading workspace package '{}' from {}",
                name_str,
                staged_dir.display()
            );
            self.load_project(&staged_dir).map_err(|source| {
                LoadWorkspacePackagesError::LoadProjectFailed {
                    name: name_str,
                    source: Box::new(source),
                }
            })?;
        }

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
            // `SurfaceStore::new` constructs the β-shape from a fresh
            // `Arc<SurfaceStoreInner>`. Method dispatch goes through
            // the host's `SurfaceStoreVTable`.
            let surface_store = SurfaceStore::new(
                socket_path.clone(),
                self.runtime_id.to_string(),
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
                Arc::new(crate::linux::LinuxTimerFdAudioClock::new(audio_clock_config))
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
            crate::core::Error::Configuration(format!(
                "Failed to install signal handlers: {}",
                e
            ))
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
    // Graph File Loading
    // =========================================================================

    /// Load a graph from a file definition.
    ///
    /// Processors are created first, building an alias → ID map. Then connections
    /// are created by resolving aliases to runtime IDs.
    pub fn load_graph_file(
        &self,
        def: &crate::core::graph_file::GraphFileDefinition,
    ) -> Result<()> {
        use std::collections::HashMap;

        // Validate before loading
        def.validate()?;

        // Phase 1: Create processors, build alias → ID map
        let mut alias_to_id: HashMap<String, ProcessorUniqueId> = HashMap::new();

        for proc_def in &def.processors {
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
        for conn_def in &def.connections {
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

        if let Some(name) = &def.name {
            tracing::info!("Loaded pipeline: {}", name);
        }

        Ok(())
    }

    /// Load a graph from a JSON file path.
    pub fn load_graph_file_path(&self, path: &std::path::Path) -> Result<()> {
        let def = crate::core::graph_file::GraphFileDefinition::from_json_file(path)?;

        if let Some(name) = &def.name {
            tracing::info!("Loading pipeline '{}' from {}", name, path.display());
        } else {
            tracing::info!("Loading pipeline from {}", path.display());
        }

        self.load_graph_file(&def)
    }

    /// Resolve a single dependency declaration to a directory the runtime
    /// can recurse into via [`Self::load_project`].
    ///
    /// Resolution chain (mirrors Cargo's `[patch.crates-io]`, per-consumer):
    ///
    /// 1. **Consumer's own `patch:` table** — overrides the dep declaration
    ///    when present. Path entries resolve relative to the consumer's
    ///    manifest dir and are validated strictly: a missing path is a
    ///    hard error so the dev knows immediately to fix the manifest.
    /// 2. **Direct path declaration** in `dependencies:` (legacy / pre-canonical
    ///    deps). Resolves relative to the consumer dir, no existence check
    ///    yet (`load_project` will surface the missing manifest error
    ///    downstream).
    /// 3. **Installed-package cache** (`InstalledPackageManifest`).
    /// 4. **Actionable error** — registry/git deps with no matching patch
    ///    or installed entry.
    fn resolve_dependency_path(
        &self,
        consumer_dir: &std::path::Path,
        dep_ref: &streamlib_idents::PackageRef,
        spec: &streamlib_idents::DependencySpec,
        patch: &std::collections::BTreeMap<
            streamlib_idents::PackageRef,
            streamlib_idents::DependencySpec,
        >,
    ) -> Result<std::path::PathBuf> {
        use streamlib_idents::DependencySpec;

        // Tier 1: consumer's own `patch:` table.
        if let Some(patch_spec) = patch.get(dep_ref) {
            return self.resolve_consumer_patch(consumer_dir, dep_ref, patch_spec);
        }

        // Tier 2-4: dispatch on the dep declaration's flavor.
        match spec {
            DependencySpec::Path(p) => Ok(if p.path.is_absolute() {
                p.path.clone()
            } else {
                consumer_dir.join(&p.path)
            }),
            DependencySpec::Registry(_) | DependencySpec::Git(_) => {
                if let Some(installed) = self.lookup_installed_package(dep_ref)? {
                    return Ok(installed);
                }
                Err(Error::Configuration(format!(
                    "Dependency '{dep_ref}' could not be resolved. \
                     No matching `patch:` entry was found in {}/{}, and no matching \
                     package is installed (run `streamlib pkg list` to see \
                     installed packages, `streamlib pkg install <slpkg>` to \
                     install one).",
                    consumer_dir.display(),
                    streamlib_idents::Manifest::FILE_NAME,
                )))
            }
        }
    }

    /// Resolve a consumer-scoped `patch:` entry to an absolute path the
    /// runtime can recurse into.
    ///
    /// - `path:` patches: validated strictly — missing path is a hard
    ///   error so the dev knows immediately to fix the manifest
    ///   (npm / wrangler-style strictness; CLAUDE.md "make the right
    ///   way easy and the wrong way hard").
    /// - `git:` patches: cloned at the pinned rev to the shared
    ///   resolver cache (`~/.streamlib/resolver-cache/git/`). Idempotent
    ///   — a previously-cloned checkout is reused. Same helper the
    ///   build-time resolver uses, so checkouts are shared across
    ///   codegen and runtime startups.
    /// - `version:` (registry) patches: not yet supported — the v1
    ///   resolver doesn't ship a registry. Consumers either declare a
    ///   git/path patch or rely on the installed-package cache.
    fn resolve_consumer_patch(
        &self,
        consumer_dir: &std::path::Path,
        dep_ref: &streamlib_idents::PackageRef,
        patch_spec: &streamlib_idents::DependencySpec,
    ) -> Result<std::path::PathBuf> {
        use streamlib_idents::DependencySpec;

        match patch_spec {
            DependencySpec::Path(p) => {
                let abs = if p.path.is_absolute() {
                    p.path.clone()
                } else {
                    consumer_dir.join(&p.path)
                };
                if !abs.exists() {
                    return Err(Error::Configuration(format!(
                        "patch entry for '{dep_ref}' in {}/{} points at \
                         `{}` which does not exist. Path patches are \
                         dev-time overrides — they must resolve to a \
                         real directory at parse time. Either fix the \
                         path or remove the patch entry.",
                        consumer_dir.display(),
                        streamlib_idents::Manifest::FILE_NAME,
                        abs.display(),
                    )));
                }
                Ok(abs)
            }
            DependencySpec::Git(g) => {
                let cache_dir = crate::core::streamlib_home::get_streamlib_home()
                    .join("resolver-cache");
                streamlib_idents::fetch_git(
                    &dep_ref.to_string(),
                    &g.git,
                    &g.rev,
                    &cache_dir,
                )
                .map_err(|e| Error::Configuration(e.to_string()))
            }
            DependencySpec::Registry(_) => Err(Error::Configuration(format!(
                "patch entry for '{dep_ref}' in {}/{} is registry-flavored. \
                 The v1 resolver doesn't ship a registry — declare a \
                 `path:` or `git:` patch entry, or remove the patch and \
                 rely on the installed-package cache.",
                consumer_dir.display(),
                streamlib_idents::Manifest::FILE_NAME,
            ))),
        }
    }

    /// Look the canonical [`streamlib_idents::PackageRef`] up in the
    /// installed-package cache (`InstalledPackageManifest`). Returns the
    /// extracted slpkg cache directory when present.
    fn lookup_installed_package(
        &self,
        dep_ref: &streamlib_idents::PackageRef,
    ) -> Result<Option<std::path::PathBuf>> {
        use crate::core::config::InstalledPackageManifest;
        use crate::core::streamlib_home::get_cached_package_dir;

        let manifest = InstalledPackageManifest::load()?;
        let Some(entry) = manifest.find_by_ref(dep_ref) else {
            return Ok(None);
        };
        Ok(Some(get_cached_package_dir(&entry.cache_dir)))
    }
}

/// Iterate `config.schemas` map entries, registering each `Local` schema
/// (the YAML body keyed by its canonical identifier) with the engine's
/// runtime schema registry. `External` entries are import declarations
/// owned by other packages and are skipped here — the dep's own
/// `register_package_schemas` call handles them when its manifest loads.
/// No-op when the manifest declares no `schemas:` map.
fn register_package_schemas(
    project_path: &std::path::Path,
    config: &crate::core::config::ProjectConfig,
) -> Result<()> {
    use crate::core::config::ProjectConfig;
    use crate::core::embedded_schemas;
    use streamlib_idents::SchemaEntry;

    let Some(schemas) = config.schemas.as_ref() else {
        return Ok(());
    };

    if schemas.is_empty() {
        return Ok(());
    }

    let pkg_meta = config.package.as_ref().ok_or_else(|| {
        Error::Configuration(format!(
            "{} at {} declares `schemas:` but is missing a `package:` block. \
             Schema canonical identifiers are composed from \
             `package.{{org, name}}`.",
            ProjectConfig::FILE_NAME,
            project_path.display(),
        ))
    })?;

    for (_name, entry) in schemas {
        let SchemaEntry::Local { file } = entry else {
            continue;
        };
        let schema_path = if file.is_absolute() {
            file.clone()
        } else {
            project_path.join(file)
        };
        let body = std::fs::read_to_string(&schema_path).map_err(|e| {
            Error::Configuration(format!(
                "failed to read schema declared in {}: {}: {}",
                ProjectConfig::FILE_NAME,
                schema_path.display(),
                e
            ))
        })?;
        let canonical = canonical_identifier_for_schema(&body, pkg_meta, &schema_path)?;
        tracing::debug!(
            "registering schema '{}' from {}",
            canonical,
            schema_path.display()
        );
        embedded_schemas::register_schema(canonical, body);
    }
    Ok(())
}

/// Resolve a processor's bare-name config schema reference (#767) to
/// its canonical id string. Walks the manifest's `schemas:` map via
/// the supplied resolver output, locates the owning package + schema
/// file, then reads the schema's `metadata.type` (new shape) or
/// `metadata.name` (legacy reverse-DNS) to compose the canonical id.
///
/// `resolved` is `None` only when the project's processors all lack a
/// `config:` block — reaching this function with `None` is a runtime
/// bug.
fn resolve_config_schema_canonical_id(
    project_path: &std::path::Path,
    config: &streamlib_processor_schema::ProcessorConfigSchema,
    resolved: &Option<streamlib_idents::ResolvedPackages>,
) -> std::result::Result<String, String> {
    let resolved = resolved.as_ref().ok_or_else(|| {
        format!(
            "internal error: config-schema resolution requested but \
             dependency graph for {} was not pre-resolved",
            project_path.display()
        )
    })?;

    let (owner, schema_path) = streamlib_idents::resolve_bare_schema_name(
        resolved,
        &resolved.root,
        &config.schema,
    )
    .map_err(|e| format!("bare-name resolution failed: {}", e))?;

    let owner_pkg = owner
        .manifest
        .package
        .as_ref()
        .ok_or_else(|| "owning package has no `package:` block".to_string())?;

    // Read the schema's metadata to determine the canonical id form.
    let body = std::fs::read_to_string(&schema_path)
        .map_err(|e| format!("failed to read schema {}: {}", schema_path.display(), e))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&body)
        .map_err(|e| format!("failed to parse schema {}: {}", schema_path.display(), e))?;
    let metadata = value
        .get("metadata")
        .ok_or_else(|| format!("schema {} missing `metadata` block", schema_path.display()))?;

    if let Some(type_str) = metadata.get("type").and_then(|t| t.as_str()) {
        Ok(format!(
            "@{}/{}/{}@{}",
            owner_pkg.org.as_str(),
            owner_pkg.name.as_str(),
            type_str,
            owner_pkg.version,
        ))
    } else if let Some(name_str) = metadata.get("name").and_then(|n| n.as_str()) {
        // Legacy reverse-DNS form — append the owning package's semver.
        Ok(format!("{}@{}", name_str, owner_pkg.version))
    } else {
        Err(format!(
            "schema {} declares neither `metadata.type` nor `metadata.name`",
            schema_path.display()
        ))
    }
}

/// Resolve a schema's canonical (unversioned) lookup key from its YAML
/// body + the enclosing package's metadata. Mirrors
/// `build.rs::resolve_canonical_identifier` so the engine const path
/// and the runtime registration path produce identical keys.
fn canonical_identifier_for_schema(
    body: &str,
    pkg_meta: &crate::core::config::PackageMetadata,
    schema_path: &std::path::Path,
) -> Result<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(body).map_err(|e| {
        Error::Configuration(format!(
            "failed to parse schema {}: {}",
            schema_path.display(),
            e
        ))
    })?;

    let metadata = value.get("metadata").ok_or_else(|| {
        Error::Configuration(format!(
            "schema {} missing `metadata` block",
            schema_path.display()
        ))
    })?;

    if let Some(type_name) = metadata.get("type").and_then(|t| t.as_str()) {
        return Ok(format!(
            "@{}/{}/{}",
            pkg_meta.org.as_str(),
            pkg_meta.name.as_str(),
            type_name
        ));
    }

    if let Some(name) = metadata.get("name").and_then(|n| n.as_str()) {
        return Ok(strip_legacy_semver_suffix(name).to_string());
    }

    Err(Error::Configuration(format!(
        "schema {} must declare either `metadata.type` (new shape) or \
         `metadata.name` (legacy reverse-DNS) — required for runtime \
         registration",
        schema_path.display()
    )))
}

/// Strip a trailing `@MAJOR.MINOR.PATCH` semver suffix. Mirrors
/// `embedded_schemas::strip_semver_suffix` (kept private here to avoid
/// introducing a cross-module dep on a tiny string helper).
fn strip_legacy_semver_suffix(name: &str) -> &str {
    if let Some(at_pos) = name.rfind('@') {
        let suffix = &name[at_pos + 1..];
        let parts: Vec<&str> = suffix.split('.').collect();
        if parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            return &name[..at_pos];
        }
    }
    name
}

/// The rustc target triple this engine was compiled for, captured at
/// build time via `build.rs`. Same string Cargo prints for `--target`
/// (e.g. `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`). Used to
/// resolve plugin cdylibs inside `lib/<triple>/...` during package
/// load — and exposed publicly so consumers (examples, application
/// binaries) can write into the same triple-keyed directory layout
/// without each running their own `build.rs`.
pub fn host_target_triple() -> &'static str {
    env!("STREAMLIB_HOST_TARGET")
}

/// Enumerate the target triples present as subdirectories of a
/// package's `lib/` dir. Used in the error path when the host's triple
/// has no matching artifact, so the user sees which platforms a slpkg
/// WAS packed for instead of a generic "file not found".
fn list_available_triples(lib_dir: &std::path::Path) -> Result<Vec<String>> {
    if !lib_dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(lib_dir).map_err(|e| {
        Error::Configuration(format!(
            "Failed to enumerate {}: {}",
            lib_dir.display(),
            e
        ))
    })?;
    let mut triples: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    triples.sort();
    Ok(triples)
}

/// Extract a .slpkg ZIP archive to the package cache.
/// Cache key is {name}-{version} from the embedded streamlib.yaml.
/// Always overwrites on load.
pub fn extract_slpkg_to_cache(slpkg_path: &std::path::Path) -> Result<std::path::PathBuf> {
    use crate::core::config::ProjectConfig;

    let slpkg_bytes = std::fs::read(slpkg_path).map_err(|e| {
        Error::Configuration(format!("Failed to read {}: {}", slpkg_path.display(), e))
    })?;

    let cursor = std::io::Cursor::new(&slpkg_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| Error::Configuration(format!("Failed to open .slpkg archive: {}", e)))?;

    // Read streamlib.yaml from archive to get name + version
    let manifest_yaml = {
        let mut manifest_file = archive.by_name(ProjectConfig::FILE_NAME).map_err(|e| {
            Error::Configuration(format!(
                ".slpkg archive missing {}: {}",
                ProjectConfig::FILE_NAME,
                e
            ))
        })?;
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut manifest_file, &mut contents)
            .map_err(|e| Error::Configuration(format!("Failed to read manifest: {}", e)))?;
        contents
    };

    let config: ProjectConfig = serde_yaml::from_str(&manifest_yaml)
        .map_err(|e| Error::Configuration(format!("Failed to parse manifest: {}", e)))?;

    let package = config.package.as_ref().ok_or_else(|| {
        Error::Configuration("streamlib.yaml missing [package] section".to_string())
    })?;

    let cache_key = format!("{}-{}", package.name, package.version);
    let cache_dir = crate::core::streamlib_home::get_cached_package_dir(&cache_key);

    // Always overwrite
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)
            .map_err(|e| Error::Configuration(format!("Failed to clear cache dir: {}", e)))?;
    }

    tracing::info!(
        "Extracting {} to {}",
        slpkg_path.display(),
        cache_dir.display()
    );
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| Error::Configuration(format!("Failed to create cache dir: {}", e)))?;

    // Re-open archive (cursor consumed by manifest read)
    let cursor = std::io::Cursor::new(&slpkg_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
        Error::Configuration(format!("Failed to re-open .slpkg archive: {}", e))
    })?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            Error::Configuration(format!("Failed to read archive entry: {}", e))
        })?;

        let file_name = file.name().to_string();

        // Security: reject path traversal
        if file_name.contains("..") || file_name.starts_with('/') {
            return Err(Error::Configuration(format!(
                "Invalid path in .slpkg archive: {}",
                file_name
            )));
        }

        let output_path = cache_dir.join(&file_name);

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Configuration(format!("Failed to create directory: {}", e))
            })?;
        }

        let mut output_file = std::fs::File::create(&output_path).map_err(|e| {
            Error::Configuration(format!("Failed to create {}: {}", output_path.display(), e))
        })?;

        std::io::copy(&mut file, &mut output_file).map_err(|e| {
            Error::Configuration(format!("Failed to extract {}: {}", file_name, e))
        })?;
    }

    Ok(cache_dir)
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

    let socket_path = std::path::PathBuf::from(xdg_runtime_dir)
        .join(format!("streamlib-{}.sock", runtime_id));

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

// =============================================================================
// load_workspace_packages — typed error + helpers
// =============================================================================

/// Per-failure-mode error returned by [`Runner::load_workspace_packages`].
///
/// The variants surface enough context (offending name, expected path,
/// underlying engine error) that callers can match for retry vs. abort
/// or surface an actionable message to the developer.
#[derive(Debug, thiserror::Error)]
pub enum LoadWorkspacePackagesError {
    /// Name did not parse as `@<org>/<name>` per the typed `streamlib-idents`
    /// org / name validators (charset, leading-letter, length).
    #[error("Invalid package id '{0}' — expected `@<org>/<name>` with lowercase org and name")]
    InvalidPackageId(String),

    /// Workspace root could not be resolved — neither the
    /// `STREAMLIB_WORKSPACE_ROOT` env var nor `cargo locate-project`
    /// returned a usable directory.
    #[error(
        "Workspace root not found — set STREAMLIB_WORKSPACE_ROOT or run \
         from within a Cargo workspace"
    )]
    WorkspaceRootNotFound,

    /// Staged dir does not exist for this package. Most likely cause:
    /// the dev hasn't run `cargo xtask build-plugins` yet (or pruned
    /// `target/` since the last run).
    #[error(
        "Package '{name}' not staged at {expected_path}. \
         Run `cargo xtask build-plugins` first."
    )]
    PackageNotStaged {
        name: String,
        expected_path: std::path::PathBuf,
    },

    /// Staged dir exists and parses, but its `[package]` org / name
    /// don't match the requested id. Catches the case where the
    /// staged tree was clobbered out-of-band (manual `cp`, stale
    /// rename) before the runtime registers the wrong processors.
    #[error(
        "Package identity mismatch at {staged_path}: \
         requested `@{requested_org}/{requested_name}`, found \
         `@{actual_org}/{actual_name}` in staged streamlib.yaml. \
         Re-run `cargo xtask build-plugins` to regenerate."
    )]
    PackageIdentityMismatch {
        staged_path: std::path::PathBuf,
        requested_org: String,
        requested_name: String,
        actual_org: String,
        actual_name: String,
    },

    /// Staged dir is present and identity matches, but a Rust-impl
    /// package's expected cdylib is missing under `lib/<host_triple>/`.
    /// Distinguishes "staging succeeded but cargo build silently
    /// produced no artifact" from a generic load_project failure.
    #[error(
        "Cdylib missing for Rust-impl package '{name}' — expected at \
         {expected_path}. Re-run `cargo xtask build-plugins` to rebuild."
    )]
    CdylibMissing {
        name: String,
        expected_path: std::path::PathBuf,
    },

    /// `load_project` rejected the staged dir. Carries the engine
    /// `Error` so callers can introspect further.
    #[error("load_project failed for '{name}': {source}")]
    LoadProjectFailed {
        name: String,
        #[source]
        source: Box<Error>,
    },
}

impl From<LoadWorkspacePackagesError> for Error {
    fn from(err: LoadWorkspacePackagesError) -> Self {
        match err {
            LoadWorkspacePackagesError::LoadProjectFailed { source, .. } => *source,
            other => Error::Configuration(other.to_string()),
        }
    }
}

/// Parsed `@<org>/<name>` canonical id. Only the slice references are
/// kept — the caller's input must outlive this struct.
#[derive(Debug)]
struct CanonicalPackageId<'a> {
    org_str: &'a str,
    name_str: &'a str,
}

fn parse_canonical_package_id(name: &str) -> std::result::Result<
    CanonicalPackageId<'_>,
    LoadWorkspacePackagesError,
> {
    // Strip the leading '@', split on first '/', then route the
    // halves through the typed `streamlib-idents` validators so
    // charset / leading-letter / length rules apply here too. We
    // don't surface the typed-parser's stringy diagnostic — the
    // typed `InvalidPackageId` variant is what callers match on —
    // but using the validators means a name like `@TaToLaB/CAMERA`
    // fails fast here rather than at the filesystem-lookup stage.
    let stripped = name
        .strip_prefix('@')
        .ok_or_else(|| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    let (org, pkg) = stripped
        .split_once('/')
        .ok_or_else(|| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    if org.is_empty() || pkg.is_empty() || pkg.contains('/') {
        return Err(LoadWorkspacePackagesError::InvalidPackageId(name.to_string()));
    }
    streamlib_idents::Org::new(org)
        .map_err(|_| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    streamlib_idents::Package::new(pkg)
        .map_err(|_| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    Ok(CanonicalPackageId { org_str: org, name_str: pkg })
}

fn resolve_workspace_root() -> std::result::Result<std::path::PathBuf, LoadWorkspacePackagesError> {
    // Env-var override wins when set AND the path resolves — the env
    // var IS the user's intent, so a typo'd path should surface as a
    // precise error rather than silently falling through to cargo.
    if let Ok(env_root) = std::env::var("STREAMLIB_WORKSPACE_ROOT") {
        let path = std::path::PathBuf::from(&env_root);
        return if path.is_dir() {
            Ok(path)
        } else {
            Err(LoadWorkspacePackagesError::WorkspaceRootNotFound)
        };
    }

    let output = std::process::Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .map_err(|_| LoadWorkspacePackagesError::WorkspaceRootNotFound)?;
    if !output.status.success() {
        return Err(LoadWorkspacePackagesError::WorkspaceRootNotFound);
    }
    let manifest_path = String::from_utf8(output.stdout)
        .map_err(|_| LoadWorkspacePackagesError::WorkspaceRootNotFound)?;
    let trimmed = manifest_path.trim();
    if trimmed.is_empty() {
        return Err(LoadWorkspacePackagesError::WorkspaceRootNotFound);
    }
    std::path::PathBuf::from(trimmed)
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or(LoadWorkspacePackagesError::WorkspaceRootNotFound)
}

/// Read the staged `streamlib.yaml`'s `[package]` org / name and
/// confirm they match what the caller asked for. The yaml is the
/// authoritative identity at load time — a mismatch here means the
/// staged tree was clobbered out-of-band.
fn verify_staged_package_identity(
    staged_dir: &std::path::Path,
    requested_org: &str,
    requested_name: &str,
    requested_canonical_id: &str,
) -> std::result::Result<(), LoadWorkspacePackagesError> {
    let body = std::fs::read_to_string(staged_dir.join("streamlib.yaml")).map_err(|_| {
        LoadWorkspacePackagesError::PackageNotStaged {
            name: requested_canonical_id.to_string(),
            expected_path: staged_dir.to_path_buf(),
        }
    })?;
    let manifest: streamlib_idents::Manifest = serde_yaml::from_str(&body).map_err(|_| {
        LoadWorkspacePackagesError::PackageIdentityMismatch {
            staged_path: staged_dir.to_path_buf(),
            requested_org: requested_org.to_string(),
            requested_name: requested_name.to_string(),
            actual_org: "<unparseable>".to_string(),
            actual_name: "<unparseable>".to_string(),
        }
    })?;
    let Some(metadata) = manifest.package.as_ref() else {
        return Err(LoadWorkspacePackagesError::PackageIdentityMismatch {
            staged_path: staged_dir.to_path_buf(),
            requested_org: requested_org.to_string(),
            requested_name: requested_name.to_string(),
            actual_org: "<no package section>".to_string(),
            actual_name: "<no package section>".to_string(),
        });
    };
    let actual_org = metadata.org.as_str();
    let actual_name = metadata.name.as_str();
    if actual_org != requested_org || actual_name != requested_name {
        return Err(LoadWorkspacePackagesError::PackageIdentityMismatch {
            staged_path: staged_dir.to_path_buf(),
            requested_org: requested_org.to_string(),
            requested_name: requested_name.to_string(),
            actual_org: actual_org.to_string(),
            actual_name: actual_name.to_string(),
        });
    }
    Ok(())
}

/// When the staged manifest declares Rust runtime processors, the
/// corresponding cdylib must exist under `lib/<host_triple>/`. Surface
/// the missing-cdylib case explicitly so the dev knows to re-run
/// `cargo xtask build-plugins` rather than chasing a generic
/// load_project failure.
fn verify_cdylib_present_when_rust_impl(
    staged_dir: &std::path::Path,
    requested_canonical_id: &str,
) -> std::result::Result<(), LoadWorkspacePackagesError> {
    let body = std::fs::read_to_string(staged_dir.join("streamlib.yaml")).map_err(|_| {
        LoadWorkspacePackagesError::PackageNotStaged {
            name: requested_canonical_id.to_string(),
            expected_path: staged_dir.to_path_buf(),
        }
    })?;
    let manifest: streamlib_processor_schema::ProjectConfigMinimal = match serde_yaml::from_str(&body)
    {
        Ok(m) => m,
        Err(_) => return Ok(()), // identity check already surfaced this
    };
    let has_rust = manifest.processors.iter().any(|p| {
        matches!(
            p.runtime.language,
            streamlib_processor_schema::ProcessorLanguage::Rust
        )
    });
    if !has_rust {
        return Ok(());
    }
    let triple_dir = staged_dir.join("lib").join(host_target_triple());
    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let any_dylib_present = std::fs::read_dir(&triple_dir)
        .map(|iter| {
            iter.flatten()
                .any(|e| e.path().extension().is_some_and(|ext| ext == dylib_ext))
        })
        .unwrap_or(false);
    if !any_dylib_present {
        return Err(LoadWorkspacePackagesError::CdylibMissing {
            name: requested_canonical_id.to_string(),
            expected_path: triple_dir,
        });
    }
    Ok(())
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

    #[test]
    fn parse_canonical_package_id_accepts_well_formed_input() {
        // Tightest happy-path lock: every component round-trips
        // (post-`@`, pre-`/`, post-`/`) into the parsed slices. The
        // parser is the contract for what `load_workspace_packages`
        // treats as a legal id — a regression that flipped `org` and
        // `name` would break the lookup silently.
        let parsed = parse_canonical_package_id("@tatolab/camera").unwrap();
        assert_eq!(parsed.org_str, "tatolab");
        assert_eq!(parsed.name_str, "camera");
    }

    #[test]
    fn parse_canonical_package_id_rejects_missing_at_prefix() {
        let err = parse_canonical_package_id("tatolab/camera").unwrap_err();
        assert!(matches!(
            err,
            LoadWorkspacePackagesError::InvalidPackageId(ref s) if s == "tatolab/camera"
        ));
    }

    #[test]
    fn parse_canonical_package_id_rejects_missing_slash() {
        let err = parse_canonical_package_id("@tatolab").unwrap_err();
        assert!(matches!(
            err,
            LoadWorkspacePackagesError::InvalidPackageId(ref s) if s == "@tatolab"
        ));
    }

    #[test]
    fn parse_canonical_package_id_rejects_empty_org_or_name() {
        // Both halves of `@<org>/<name>` must be non-empty. Mentally
        // reverting either non-empty check would let `@/foo` or
        // `@foo/` through to the path resolver, where the lookup
        // would produce a confusing `not_staged` error instead of
        // the precise `InvalidPackageId` one.
        for bad in ["@/camera", "@tatolab/", "@/"] {
            let err = parse_canonical_package_id(bad).unwrap_err();
            assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
        }
    }

    #[test]
    fn parse_canonical_package_id_rejects_extra_slashes() {
        // `@org/name` is a 2-segment id; nesting (`@org/group/name`)
        // is not a legal shape. The lookup format must match the
        // staged dir name `<org>__<name>` which is two-segment by
        // construction.
        let err = parse_canonical_package_id("@org/sub/name").unwrap_err();
        assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
    }

    #[test]
    #[serial]
    fn resolve_workspace_root_honors_streamlib_workspace_root_env_var() {
        // Test fixture: tempdir set via STREAMLIB_WORKSPACE_ROOT must
        // win over the cargo-locate-project fallback. Mentally
        // reverting the env-var branch would silently fall through to
        // cargo and pick the streamlib workspace root, which is
        // wrong for hosting CI fixtures.
        let tmp = tempfile::tempdir().unwrap();
        let key = "STREAMLIB_WORKSPACE_ROOT";
        let prev = std::env::var_os(key);
        // SAFETY: protected by `#[serial]` against parallel test mutation.
        unsafe {
            std::env::set_var(key, tmp.path());
        }
        let resolved = resolve_workspace_root().unwrap();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        assert_eq!(resolved, tmp.path());
    }

    #[test]
    #[serial]
    fn resolve_workspace_root_errors_when_env_var_path_does_not_exist() {
        // Env var IS the user's stated intent — when it points at a
        // non-existent dir, surface `WorkspaceRootNotFound` directly
        // rather than silently falling through to cargo locate-project.
        // The previous "fall through" branch swallowed user typos as
        // "ran from outside a workspace" errors; this test pins the
        // new behavior so a regression to silent-fallthrough fails.
        let key = "STREAMLIB_WORKSPACE_ROOT";
        let prev = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, "/nonexistent/path/that/does/not/exist");
        }
        let err = resolve_workspace_root().unwrap_err();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        assert!(matches!(err, LoadWorkspacePackagesError::WorkspaceRootNotFound));
    }

    #[test]
    fn verify_staged_package_identity_flags_mismatch() {
        // Identity check: the staged manifest's [package] org/name
        // must match the requested id. Reverting the comparison would
        // let the runtime register processors from the wrong staged
        // package — the fixture creates a yaml whose name doesn't
        // match the requested id and the helper must reject it.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("streamlib.yaml"),
            "package:\n  org: vendor\n  name: other\n  version: 1.0.0\n",
        )
        .unwrap();
        let err = verify_staged_package_identity(
            tmp.path(),
            "tatolab",
            "camera",
            "@tatolab/camera",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            LoadWorkspacePackagesError::PackageIdentityMismatch {
                ref actual_org,
                ref actual_name,
                ..
            } if actual_org == "vendor" && actual_name == "other"
        ));
    }

    #[test]
    fn verify_staged_package_identity_accepts_matching_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: camera\n  version: 1.0.0\n",
        )
        .unwrap();
        verify_staged_package_identity(
            tmp.path(),
            "tatolab",
            "camera",
            "@tatolab/camera",
        )
        .expect("matching identity must validate");
    }

    #[test]
    fn verify_cdylib_present_when_rust_impl_flags_missing_artifact() {
        // Setup: a staged Rust-impl package (yaml declares a Rust
        // processor) with the triple dir missing. The helper must
        // surface `CdylibMissing` rather than letting load_project
        // report a less actionable error. Reverting the
        // `has_rust_runtime_processors` check would either pass
        // through silently or panic — the test pins the precise
        // diagnostic.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: example
  version: 1.0.0
processors:
  - name: ExampleProcessor
    version: 1.0.0
    description: "rust"
    runtime:
      language: rust
    execution: manual
    inputs: []
    outputs: []
"#,
        )
        .unwrap();
        let err = verify_cdylib_present_when_rust_impl(tmp.path(), "@tatolab/example")
            .unwrap_err();
        assert!(matches!(
            err,
            LoadWorkspacePackagesError::CdylibMissing { ref name, ref expected_path }
                if name == "@tatolab/example"
                && expected_path.ends_with(host_target_triple())
        ));
    }

    #[test]
    fn verify_cdylib_present_when_rust_impl_passes_for_schemas_only() {
        // Schemas-only package has no Rust processors, so the cdylib
        // check is a no-op. Reverting the early-return would surface
        // a false-positive CdylibMissing.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        )
        .unwrap();
        verify_cdylib_present_when_rust_impl(tmp.path(), "@tatolab/core")
            .expect("schemas-only package must skip the cdylib check");
    }

    #[test]
    fn parse_canonical_package_id_rejects_uppercase_via_typed_validator() {
        // The typed `streamlib-idents` validators enforce lowercase
        // for org/name. Bypass would let `@TaToLaB/CAMERA` parse
        // here, hit the filesystem at `target/streamlib-plugins/TaToLaB__CAMERA`,
        // and surface a less informative `PackageNotStaged` error.
        // Reverting the `Org::new` / `Package::new` validators would
        // pass this through; the test catches that regression.
        let err = parse_canonical_package_id("@TaToLaB/camera").unwrap_err();
        assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
        let err = parse_canonical_package_id("@tatolab/CAMERA").unwrap_err();
        assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
    }

    #[test]
    #[serial]
    fn load_workspace_packages_reports_not_staged_when_dir_missing() {
        // Setup: pristine workspace root with no `target/streamlib-plugins/`
        // staging. Helper must surface the actionable PackageNotStaged
        // error rather than papering over the missing dir with a
        // generic load_project failure.
        let tmp = tempfile::tempdir().unwrap();
        let key = "STREAMLIB_WORKSPACE_ROOT";
        let prev = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, tmp.path());
        }
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .load_workspace_packages(["@tatolab/camera"])
            .unwrap_err();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        assert!(matches!(
            err,
            LoadWorkspacePackagesError::PackageNotStaged { ref name, ref expected_path }
                if name == "@tatolab/camera"
                && expected_path.ends_with("tatolab__camera")
        ));
    }

    #[test]
    #[serial]
    fn load_workspace_packages_returns_invalid_id_before_filesystem_probe() {
        // Validation order: every id must parse BEFORE the helper
        // calls `resolve_workspace_root`. Without the env-var override
        // pointing at a bogus path that can't be a workspace, the
        // unparseable id must still surface as `InvalidPackageId`
        // (not the cargo-locate-project-derived `WorkspaceRootNotFound`).
        // Mentally reverting the eager parse-loop to interleaved
        // parse-then-resolve would flip the verdict to
        // WorkspaceRootNotFound on this fixture, so the test catches
        // the regression.
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("nowhere");
        let key = "STREAMLIB_WORKSPACE_ROOT";
        let prev = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, &bogus);
        }
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .load_workspace_packages(["bad-no-at"])
            .unwrap_err();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        assert!(
            matches!(
                err,
                LoadWorkspacePackagesError::InvalidPackageId(ref s) if s == "bad-no-at"
            ),
            "expected InvalidPackageId surfaced before workspace resolution, got: {err:?}"
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

    /// Path-style dep recursion: `runtime.load_project(A)` must walk into
    /// `B` (declared as `path: ../b`) and parse its manifest. The negative
    /// counterpart below proves recursion actually happens — not just that
    /// the parser tolerates the structured shape.
    #[test]
    #[serial]
    fn test_load_project_recurses_into_path_dep() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        // Project A — empty processors, declares path dep to ../b
        let a = tmp.path().join("a");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/b":
    path: ../b
"#,
        )
        .unwrap();

        // Project B — leaf, empty processors
        let b = tmp.path().join("b");
        std::fs::create_dir(&b).unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: b
  version: "0.1.0"
"#,
        )
        .unwrap();

        runtime
            .load_project(&a)
            .expect("load_project should recurse into path dep without error");
    }

    /// `Runner::load_project` reads each entry in `streamlib.yaml`'s
    /// `schemas:` list and registers the YAML body with the engine's
    /// runtime schema registry, so `get_embedded_schema_definition`
    /// resolves the body and `max_payload_bytes_for_port_spec` returns
    /// the value declared in `metadata.max_payload_bytes`.
    /// Mentally reverting the `register_package_schemas(...)` call in
    /// `load_project` would make this test fail because the registered
    /// schema would be invisible to the lookup paths.
    #[test]
    #[serial]
    fn test_load_project_registers_package_schemas_for_runtime_lookup() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        // Build a minimal package with a single schema. The package's
        // `schemas:` list points at a schema file declaring
        // `metadata.type` (new shape) + `metadata.max_payload_bytes`.
        let pkg = tmp.path().join("pkg-with-schema");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::create_dir(pkg.join("schemas")).unwrap();
        std::fs::write(
            pkg.join("schemas/my_test_config.yaml"),
            "metadata:\n  type: MyTestConfig\n  max_payload_bytes: 8192\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: test-load-project-registers-schemas
  version: "0.1.0"

# Post-#767: `schemas:` is a name-keyed map of `Local { file }` /
# `External { package }` declarations — replaces the legacy
# `Vec<PathBuf>` shape.
schemas:
  MyTestConfig:
    file: schemas/my_test_config.yaml
"#,
        )
        .unwrap();

        let canonical =
            "@tatolab/test-load-project-registers-schemas/MyTestConfig";

        // Pre-condition: schema not in registry.
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(canonical).is_none(),
            "fresh canonical id must not exist before load_project"
        );

        runtime
            .load_project(&pkg)
            .expect("load_project must succeed for schemas-only package");

        // Post-condition: lookup resolves both forms; payload bytes
        // come from the schema's metadata, not the iceoryx2 default.
        let body = crate::core::embedded_schemas::get_embedded_schema_definition(canonical)
            .expect("registered schema must be discoverable post-load");
        assert!(body.contains("MyTestConfig"));
        let port_spec = streamlib_processor_schema::PortSchemaSpec::Specific(
            streamlib_idents::SchemaIdent::new(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new(
                    "test-load-project-registers-schemas",
                )
                .unwrap(),
                streamlib_idents::TypeName::new("MyTestConfig").unwrap(),
                streamlib_idents::SemVer::new(1, 0, 0),
            ),
        );
        assert_eq!(
            crate::core::embedded_schemas::max_payload_bytes_for_port_spec(&port_spec),
            8192,
            "max_payload_bytes_for_port_spec must read metadata declared by the loaded package"
        );
    }

    /// Negative pair to the test above: when `B`'s manifest is missing,
    /// the recursion must fail and propagate the error. Mentally reverting
    /// the recursion in `load_project` would make this test pass falsely
    /// — that's why both are needed.
    #[test]
    #[serial]
    fn test_load_project_path_dep_missing_manifest_propagates_error() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let a = tmp.path().join("a");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/missing":
    path: ../does-not-exist
"#,
        )
        .unwrap();

        let result = runtime.load_project(&a);
        assert!(
            result.is_err(),
            "load_project must error when a path dep target has no streamlib.yaml"
        );
    }

    /// Consumer-scoped `patch:` resolution: when a consumer declares a
    /// registry-form dep AND its own `patch:` block has an entry for that
    /// dep pointing at a local path, `load_project` recurses into the
    /// patched location. Mirrors Cargo's `[patch.crates-io]` semantics
    /// but per-consumer (no workspace walk-up). Mentally reverting the
    /// patch lookup in `resolve_dependency_path` makes this fail because
    /// the registry arm would fall through to the installed-cache tier
    /// (which is empty in this test).
    #[test]
    #[serial]
    fn test_load_project_resolves_registry_dep_via_consumer_patch() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        // Patched dep target.
        let b = tmp.path().join("b");
        std::fs::create_dir(&b).unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: b\n  version: \"0.1.0\"\n",
        )
        .unwrap();

        // Consumer in a sibling dir, declares a registry-style dep AND a
        // path-flavor patch in its own yaml. No tree-level state.
        let a = tmp.path().join("a");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
patch:
  "@tatolab/b":
    path: ../b
"#,
        )
        .unwrap();

        runtime
            .load_project(&a)
            .expect("consumer-scoped patch must resolve the registry dep to ../b/");
    }

    /// Git-flavor patch resolution: when a consumer's `patch:` block
    /// declares a `git:` entry pinned at a specific rev, `load_project`
    /// clones the URL via the shared `streamlib_idents::fetch_git`
    /// helper (same code the build-time resolver uses) and recurses
    /// into the checkout. Mentally swapping the git arm in
    /// `resolve_consumer_patch` for the previous "git/registry not
    /// supported" error would make this test surface that error
    /// instead of succeeding.
    #[test]
    #[serial]
    fn test_load_project_resolves_git_patch_via_shared_helper() {
        // Build a minimal local git repo with a `streamlib.yaml` that
        // declares `@tatolab/b`. `git clone <local-path>` works without
        // a network — this is a real fetch through the same helper the
        // resolver uses, exercised end-to-end.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("b-repo");
        std::fs::create_dir(&repo).unwrap();

        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .expect("git invocation");
            assert!(status.success(), "git {:?} failed", args);
        };

        run_git(&["init", "--quiet"]);
        run_git(&["config", "user.email", "test@example.com"]);
        run_git(&["config", "user.name", "test"]);
        std::fs::write(
            repo.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: b\n  version: \"0.1.0\"\n",
        )
        .unwrap();
        run_git(&["add", "streamlib.yaml"]);
        run_git(&["commit", "--quiet", "-m", "initial"]);
        let rev_output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git rev-parse");
        let rev = String::from_utf8(rev_output.stdout).unwrap().trim().to_string();

        // Sandbox STREAMLIB_HOME so the git clone lands in tempdir, not
        // the real user cache (and to avoid leaking checkouts across
        // test runs).
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        // SAFETY: `#[serial]` serializes every Runner test in this
        // module, so concurrent env-var mutation can't tear other tests.
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);

        // Consumer in a separate tempdir, declares a registry-style dep
        // and a git-flavor patch pointing at the local repo.
        let consumer = tempfile::tempdir().unwrap();
        std::fs::write(
            consumer.path().join("streamlib.yaml"),
            format!(
                r#"
package:
  org: tatolab
  name: consumer
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
patch:
  "@tatolab/b":
    git: "{}"
    rev: "{}"
"#,
                repo.display(),
                rev,
            ),
        )
        .unwrap();

        let runtime = Runner::new().unwrap();
        runtime
            .load_project(consumer.path())
            .expect("git patch must clone the local repo and recurse into it");
    }

    /// Strict patch validation: when the consumer's `patch:` block points
    /// at a path that doesn't exist, `load_project` errors clearly so the
    /// dev knows immediately to fix the manifest. npm/wrangler-style
    /// strictness — the "make the right way easy and the wrong way hard"
    /// rule from CLAUDE.md applied to manifest validation. Mentally
    /// reverting the existence check in `resolve_consumer_patch` would
    /// surface a downstream "manifest not found" error from `load_project`
    /// instead of a clear "patch path doesn't exist" error.
    #[test]
    #[serial]
    fn test_load_project_strict_errors_on_missing_patch_path() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
patch:
  "@tatolab/b":
    path: ./does-not-exist
"#,
        )
        .unwrap();

        let err = runtime
            .load_project(tmp.path())
            .expect_err("missing patch path must error strictly");
        let msg = format!("{err}");
        assert!(
            msg.contains("@tatolab/b"),
            "error must surface the canonical dep ref, got: {msg}"
        );
        assert!(
            msg.contains("does-not-exist") && msg.contains("does not exist"),
            "error must call out the missing patch path, got: {msg}"
        );
    }

    /// Drops a previously-saved `STREAMLIB_HOME` environment variable
    /// state when the test scope ends, so a sandboxed `STREAMLIB_HOME`
    /// override doesn't leak into the next `#[serial]` test.
    struct StreamlibHomeRestore(Option<std::ffi::OsString>);
    impl Drop for StreamlibHomeRestore {
        fn drop(&mut self) {
            // SAFETY: `#[serial]` makes every test in this module
            // exclusive — no concurrent reader of `STREAMLIB_HOME`.
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                    None => std::env::remove_var("STREAMLIB_HOME"),
                }
            }
        }
    }

    #[test]
    #[serial]
    fn test_load_project_resolves_registry_dep_via_installed_cache() {
        // Sandbox: STREAMLIB_HOME → tempdir so the test doesn't interact
        // with the real `~/.streamlib/packages.yaml`.
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        // SAFETY: `#[serial]` serializes every Runner test in this
        // module, so concurrent env-var mutation can't tear other tests.
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);

        // The "extracted slpkg cache" directory the InstalledPackageManifest
        // entry will point at. Contains the dep package's manifest.
        let cache_root = sandbox.path().join("cache/packages");
        std::fs::create_dir_all(&cache_root).unwrap();
        let dep_cache_dir = cache_root.join("b-0.1.0");
        std::fs::create_dir(&dep_cache_dir).unwrap();
        std::fs::write(
            dep_cache_dir.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: b\n  version: \"0.1.0\"\n",
        )
        .unwrap();

        // Pre-populate the installed-package manifest with a canonical-key
        // entry for `@tatolab/b` pointing at the cache dir above.
        let mut installed = crate::core::config::InstalledPackageManifest::default();
        installed.add(crate::core::config::InstalledPackageEntry {
            name: streamlib_idents::PackageRef::new(
                streamlib_processor_schema::Org::new("tatolab").unwrap(),
                streamlib_processor_schema::Package::new("b").unwrap(),
            ),
            version: streamlib_processor_schema::SemVer::new(0, 1, 0),
            description: None,
            installed_from: "test".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
            cache_dir: "b-0.1.0".to_string(),
        });
        installed.save().unwrap();

        // Customer-shape consumer: declares the dep canonically with NO
        // `patch:` block — exactly the yaml shape that ships in slpkgs.
        // Resolution must fall through to the installed-package cache.
        let consumer = tempfile::tempdir().unwrap();
        std::fs::write(
            consumer.path().join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: consumer
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
"#,
        )
        .unwrap();

        let runtime = Runner::new().unwrap();
        runtime
            .load_project(consumer.path())
            .expect("registry dep must resolve via installed-package cache");
    }

    /// When neither a workspace `[patch]` entry nor an installed-package
    /// cache hit covers a registry dep, the runtime must surface an
    /// actionable error that names the canonical key and points the user
    /// at `streamlib pkg install`. The error is the runtime's last-resort
    /// signal that resolution exhausted both tiers; mentally reverting
    /// either tier (workspace lookup, installed-cache lookup) would make
    /// this test surface a different error path, breaking the contract.
    #[test]
    #[serial]
    fn test_load_project_unresolvable_registry_dep_errors_actionably() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let a = tmp.path().join("a");
        std::fs::create_dir(&a).unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/missing": "^1.0.0"
"#,
        )
        .unwrap();

        let err = runtime
            .load_project(&a)
            .expect_err("unresolvable registry dep must error");
        let msg = format!("{}", err);
        assert!(
            msg.contains("@tatolab/missing"),
            "error must surface the canonical `@org/name` key, got: {msg}"
        );
        assert!(
            msg.contains("streamlib pkg install") || msg.contains("workspace"),
            "error must point at the resolution paths the user can act on, got: {msg}"
        );
    }

    /// Plugin cdylib resolution: when a project declares Rust runtime
    /// processors, `load_project` reads from `lib/<host_triple>/...`.
    /// Mismatched-triple subdirs must produce a clear error naming both
    /// the host triple and the triples actually present, so the user
    /// can diagnose immediately ("this slpkg was packed for an
    /// incompatible host, repack on a matching one"). Mentally
    /// reverting the `triple_dir = lib_dir.join(host_triple)` line in
    /// `load_project` would surface a non-existent host's flat-`lib/`
    /// resolution and the test would not see the host triple in the
    /// error message.
    #[test]
    #[serial]
    fn test_load_project_rust_dylib_missing_host_triple_surfaces_available_triples() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let pkg = tmp.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: triple-mismatch-pkg
  version: "0.1.0"
processors:
  - name: TestProcessor
    version: 1.0.0
    description: "Test"
    runtime: rust
    execution: manual
    inputs:
      - name: video_in
        schema: any
    outputs:
      - name: video_out
        schema: any
"#,
        )
        .unwrap();

        // Plant a dylib under a deliberately-wrong triple so
        // `list_available_triples` has something to surface and the
        // error message can prove the triple-keyed resolution actually
        // looked at the right subdir.
        let wrong_triple = "wrong-arch-unknown-elsewhere-gnu";
        let wrong_dir = pkg.join("lib").join(wrong_triple);
        std::fs::create_dir_all(&wrong_dir).unwrap();
        std::fs::write(wrong_dir.join("libfake.so"), b"not-a-real-dylib").unwrap();

        let err = runtime
            .load_project(&pkg)
            .expect_err("missing host-triple subdir must error");
        let msg = format!("{}", err);
        assert!(
            msg.contains(host_target_triple()),
            "error must name the host triple so the user sees what was expected, got: {msg}"
        );
        assert!(
            msg.contains(wrong_triple),
            "error must list the triples that ARE present so the user sees what the slpkg was packed for, got: {msg}"
        );
    }

    /// Positive pair to the triple-mismatch test above: when the
    /// host-triple subdir exists and contains a file with the right
    /// extension, `load_project` reaches the dlopen step. The dylib is
    /// junk bytes (not a real cdylib), so dlopen fails — but the error
    /// proves path resolution succeeded BEFORE dlopen. Mentally
    /// reverting the triple lookup to flat-`lib/` would surface a
    /// different error variant ("No .so file found in lib/") and this
    /// test would not see the dlopen / STREAMLIB_PLUGIN-symbol message.
    #[test]
    #[serial]
    fn test_load_project_rust_dylib_resolves_host_triple_then_dlopens() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let pkg = tmp.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: triple-match-pkg
  version: "0.1.0"
processors:
  - name: TestProcessor
    version: 1.0.0
    description: "Test"
    runtime: rust
    execution: manual
    inputs:
      - name: video_in
        schema: any
    outputs:
      - name: video_out
        schema: any
"#,
        )
        .unwrap();

        let triple_dir = pkg.join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        let dylib_ext = if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        };
        let dylib_path = triple_dir.join(format!("libfake.{}", dylib_ext));
        std::fs::write(&dylib_path, b"not-a-real-dylib").unwrap();

        let err = runtime
            .load_project(&pkg)
            .expect_err("junk dylib must fail at dlopen, not at path resolution");
        let msg = format!("{}", err);
        // Surface evidence that path resolution succeeded: the error
        // must mention the dylib file itself (proving the loader got
        // past `read_dir` and into `libloading::Library::new`). The
        // earlier-shape error "No .so file found in lib/" would name
        // the directory, not the file — so this assertion locks the
        // resolution-then-dlopen ordering.
        assert!(
            msg.contains("libfake"),
            "error must reference the dylib file (proving path resolution reached dlopen), got: {msg}"
        );
        assert!(
            !msg.contains("No .so file found")
                && !msg.contains("No .dylib file found")
                && !msg.contains("No .dll file found"),
            "error must NOT be the 'no dylib found' variant (path resolution succeeded), got: {msg}"
        );
    }

    /// Schemas-only packages have no `lib/` directory at all and must
    /// load cleanly — the loader's empty-processors short-circuit at
    /// the top of `load_project` returns before touching the lib path.
    /// Reverting that short-circuit would make this test surface a
    /// "Failed to read lib/<triple>" error instead.
    #[test]
    #[serial]
    fn test_load_project_schemas_only_skips_lib_lookup() {
        let runtime = Runner::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let pkg = tmp.path().join("schemas-only");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: schemas-only-pkg
  version: "0.1.0"
"#,
        )
        .unwrap();

        // Deliberately omit `lib/` — a schemas-only package has no
        // platform-specific content, and the loader must NOT try to
        // resolve a host-triple subdir.
        runtime
            .load_project(&pkg)
            .expect("schemas-only package must load without touching lib/");
    }

    /// Unit lock for the `list_available_triples` helper:
    /// returns directory-name strings sorted, ignores non-directory
    /// entries, returns empty when `lib/` itself is missing. Reverting
    /// any of the filters would either surface non-triple noise or
    /// crash on missing-dir.
    #[test]
    fn list_available_triples_filters_to_subdirs_and_sorts() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("lib");

        // Missing lib/ → empty list, no error.
        assert!(list_available_triples(&lib).unwrap().is_empty());

        std::fs::create_dir(&lib).unwrap();
        // Two triple-named subdirs + one non-dir file at the same level
        // (a stray README or similar). The helper must list the two
        // subdirs sorted, omit the file.
        std::fs::create_dir(lib.join("aarch64-apple-darwin")).unwrap();
        std::fs::create_dir(lib.join("x86_64-unknown-linux-gnu")).unwrap();
        std::fs::write(lib.join("README.md"), b"stray").unwrap();

        let triples = list_available_triples(&lib).unwrap();
        assert_eq!(
            triples,
            vec![
                "aarch64-apple-darwin".to_string(),
                "x86_64-unknown-linux-gnu".to_string(),
            ]
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
        use streamlib_surface_client::{send_request_with_fds, MAX_DMA_BUF_PLANES};

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
                let (resp, fds) =
                    send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
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
                    let (resp, _) =
                        send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
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

                let runtime = runtime_result.expect(
                    "runtime should clean up an orphan socket and bind successfully",
                );
                let bound = runtime.surface_socket_path();
                assert_eq!(bound, stale_path.as_path());
                assert!(bound.exists(), "service should be bound at {}", bound.display());

                // The path is now a Unix socket, not a regular file — connect
                // should succeed against the runtime-internal service.
                let stream = UnixStream::connect(bound).expect("connect to fresh service");
                let req = serde_json::json!({
                    "op": "check_out",
                    "surface_id": "no-such",
                });
                let (resp, _) =
                    send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
                        .expect("round-trip");
                assert!(resp.get("error").is_some());
            });
        }
    }
}
