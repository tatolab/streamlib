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
#[cfg(not(target_os = "macos"))]
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
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result, StreamError};
use crate::iceoryx2::Iceoryx2Node;

/// Keeps loaded dylib plugin libraries alive for the process lifetime.
///
/// When a Rust dylib plugin is loaded via `load_project()`, the `Library` handle
/// must remain alive so that the registered processor vtables stay valid.
static LOADED_PLUGIN_LIBRARIES: std::sync::LazyLock<parking_lot::Mutex<Vec<libloading::Library>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(Vec::new()));

/// ABI version for plugin compatibility checks.
///
/// Must match `streamlib_plugin_abi::STREAMLIB_ABI_VERSION`. Duplicated here to
/// avoid a cyclic dependency (streamlib-plugin-abi depends on streamlib).
const PLUGIN_ABI_VERSION: u32 = 1;

/// Plugin declaration exported by dynamic libraries.
///
/// Must match the layout of `streamlib_plugin_abi::PluginDeclaration`.
#[repr(C)]
struct PluginDeclaration {
    abi_version: u32,
    register: extern "C" fn(&'static crate::core::processors::ProcessorInstanceFactory),
}

/// Storage variant for tokio runtime in StreamRuntime.
///
/// Enables StreamRuntime to work both standalone (owning its runtime) and
/// integrated into existing tokio applications (using the current handle).
pub(crate) enum TokioRuntimeVariant {
    /// StreamRuntime owns the tokio Runtime (created when NOT in tokio context).
    OwnedTokioRuntime(tokio::runtime::Runtime),
    /// StreamRuntime uses an external tokio Handle (auto-detected when called from tokio context).
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
/// `StreamRuntime` is designed for concurrent access from multiple threads.
/// All public methods take `&self` (not `&mut self`), allowing the runtime
/// to be shared via `Arc<StreamRuntime>` without external synchronization.
///
/// Internal state uses fine-grained locking:
/// - Graph operations: `RwLock` (multiple readers OR one writer)
/// - Pending operations: `Mutex` (batched for compilation)
/// - Status: `Mutex` (lifecycle state)
/// - Runtime context: `Mutex<Option<...>>` (created on start, cleared on stop)
///
/// This means multiple threads can concurrently call `add_processor()`,
/// `connect()`, etc. without blocking each other on an outer lock.
pub struct StreamRuntime {
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
}

impl StreamRuntime {
    pub fn new() -> Result<Arc<Self>> {
        // Generate or retrieve runtime ID (checks STREAMLIB_RUNTIME_ID env var)
        let runtime_id = Arc::new(RuntimeUniqueId::from_env_or_generate());
        tracing::info!("Creating StreamRuntime with ID: {}", runtime_id);

        // Get STREAMLIB_HOME and run init hooks (once per process)
        let streamlib_home = crate::core::get_streamlib_home();
        tracing::debug!("STREAMLIB_HOME: {}", streamlib_home.display());
        crate::core::run_init_hooks(&streamlib_home)?;

        // Auto-detect tokio context (issue #92)
        // If inside tokio runtime: use current handle (external handle mode)
        // If outside tokio runtime: create owned runtime
        let tokio_runtime_variant = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                tracing::debug!("Detected existing tokio runtime, using external handle mode");
                TokioRuntimeVariant::ExternalTokioHandle(handle)
            }
            Err(_) => {
                // Create tokio runtime with default thread count (one per CPU core)
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        StreamError::Runtime(format!("Failed to create tokio runtime: {}", e))
                    })?;
                TokioRuntimeVariant::OwnedTokioRuntime(rt)
            }
        };

        // Register all processors from inventory before any add_processor calls.
        // This populates the global registry with link-time registered processors.
        let result = crate::core::processors::PROCESSOR_REGISTRY.register_all_processors()?;
        tracing::debug!("Registered {} processors from inventory", result.count);

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
        }))
    }

    /// Update a processor's configuration at runtime.
    pub fn update_processor_config<C: Serialize>(
        &self,
        processor_id: &ProcessorUniqueId,
        config: C,
    ) -> Result<()> {
        let config_json = serde_json::to_value(&config)
            .map_err(|e| crate::core::StreamError::Config(e.to_string()))?;

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

        // Load dependency packages first (schemas/processors they export)
        if !config.dependencies.is_empty() {
            use crate::core::config::InstalledPackageManifest;

            let manifest = InstalledPackageManifest::load()?;
            for dep_name in &config.dependencies {
                let entry = manifest.find_by_name(dep_name).ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Dependency '{}' is not installed. Install it with: streamlib pkg install <path.slpkg>",
                        dep_name
                    ))
                })?;
                let dep_path =
                    crate::core::streamlib_home::get_cached_package_dir(&entry.cache_dir);
                tracing::info!(
                    "Loading dependency '{}' from {}",
                    dep_name,
                    dep_path.display()
                );
                self.load_project(&dep_path)?;
            }
        }

        if config.processors.is_empty() {
            tracing::warn!(
                "No processors found in {} in {}",
                ProjectConfig::FILE_NAME,
                project_path.display()
            );
            return Ok(());
        }

        // Eagerly create venv for Python packages so processors don't race at spawn time
        let has_python_processors = config.processors.iter().any(|p| {
            matches!(
                p.runtime.language,
                streamlib_codegen_shared::ProcessorLanguage::Python
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

        for proc_schema in &config.processors {
            // Map runtime language to ProcessorRuntime
            let runtime = match proc_schema.runtime.language {
                streamlib_codegen_shared::ProcessorLanguage::Python => ProcessorRuntime::Python,
                streamlib_codegen_shared::ProcessorLanguage::TypeScript => {
                    ProcessorRuntime::TypeScript
                }
                streamlib_codegen_shared::ProcessorLanguage::Rust => {
                    // Rust dylib plugins self-register via export_plugin! macro.
                    // Load the dylib once per project (all Rust processors in the
                    // same YAML share one dylib), then validate each processor
                    // was actually registered.
                    if !rust_dylib_loaded {
                        let lib_dir = project_path.join("lib");
                        let dylib_ext = if cfg!(target_os = "macos") {
                            "dylib"
                        } else if cfg!(target_os = "windows") {
                            "dll"
                        } else {
                            "so"
                        };

                        let dylib_path = std::fs::read_dir(&lib_dir)
                            .map_err(|e| {
                                StreamError::Configuration(format!(
                                    "Failed to read lib/ directory at {}: {}",
                                    lib_dir.display(),
                                    e
                                ))
                            })?
                            .filter_map(|entry| entry.ok())
                            .map(|entry| entry.path())
                            .find(|path| path.extension().is_some_and(|ext| ext == dylib_ext))
                            .ok_or_else(|| {
                                StreamError::Configuration(format!(
                                    "No .{} file found in {}",
                                    dylib_ext,
                                    lib_dir.display()
                                ))
                            })?;

                        tracing::info!("Loading Rust dylib plugin: {}", dylib_path.display());

                        // Safety: Loading a dynamic library is inherently unsafe.
                        // The dylib must be a valid StreamLib plugin built with
                        // a compatible streamlib-plugin-abi version.
                        let lib = unsafe {
                            libloading::Library::new(&dylib_path).map_err(|e| {
                                StreamError::Configuration(format!(
                                    "Failed to load dylib {}: {}",
                                    dylib_path.display(),
                                    e
                                ))
                            })?
                        };

                        let decl: &PluginDeclaration = unsafe {
                            let symbol = lib
                                .get::<*const PluginDeclaration>(b"STREAMLIB_PLUGIN\0")
                                .map_err(|e| {
                                    StreamError::Configuration(format!(
                                        "Plugin '{}' missing STREAMLIB_PLUGIN symbol. \
                                         Ensure the plugin uses the export_plugin! macro: {}",
                                        dylib_path.display(),
                                        e
                                    ))
                                })?;
                            &**symbol
                        };

                        if decl.abi_version != PLUGIN_ABI_VERSION {
                            return Err(StreamError::Configuration(format!(
                                "ABI version mismatch for '{}': plugin has v{}, \
                                 runtime expects v{}. Rebuild the plugin with a \
                                 compatible streamlib-plugin-abi version.",
                                dylib_path.display(),
                                decl.abi_version,
                                PLUGIN_ABI_VERSION
                            )));
                        }

                        (decl.register)(&crate::core::processors::PROCESSOR_REGISTRY);

                        // Keep the library alive for the process lifetime
                        LOADED_PLUGIN_LIBRARIES.lock().push(lib);

                        rust_dylib_loaded = true;
                        tracing::info!(
                            "Rust dylib plugin loaded and registered: {}",
                            dylib_path.display()
                        );
                    }

                    // Validate the processor was registered by the dylib
                    let registered = crate::core::processors::PROCESSOR_REGISTRY
                        .list_registered()
                        .iter()
                        .any(|desc| desc.name == proc_schema.name);
                    if !registered {
                        return Err(StreamError::Configuration(format!(
                            "Processor '{}' declared in streamlib.yaml but not \
                             registered by the dylib. Ensure export_plugin!() \
                             includes this processor.",
                            proc_schema.name
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
                        &p.schema,
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
                        &p.schema,
                        true,
                    )
                })
                .collect();

            let mut descriptor = ProcessorDescriptor::new(
                &proc_schema.name,
                proc_schema.description.as_deref().unwrap_or(""),
            )
            .with_version(&proc_schema.version)
            .with_runtime(runtime.clone());

            if let Some(entrypoint) = &proc_schema.entrypoint {
                descriptor = descriptor.with_entrypoint(entrypoint);
            }

            if let Some(config) = &proc_schema.config {
                descriptor = descriptor.with_config_schema(&config.schema);
            }

            descriptor.inputs = inputs;
            descriptor.outputs = outputs;

            // Convert schema execution mode to runtime ExecutionConfig
            let execution = match &proc_schema.execution {
                streamlib_codegen_shared::ProcessorSchemaExecution::Reactive => {
                    ProcessExecution::Reactive
                }
                streamlib_codegen_shared::ProcessorSchemaExecution::Manual => {
                    ProcessExecution::Manual
                }
                streamlib_codegen_shared::ProcessorSchemaExecution::Continuous { interval_ms } => {
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

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Start the runtime.
    ///
    /// Takes `&Arc<Self>` to allow passing the runtime to processors via RuntimeContext.
    /// Processors can then call runtime operations directly without indirection.
    pub fn start(self: &Arc<Self>) -> Result<()> {
        // Load .env file if present (development environment variables)
        if let Ok(path) = dotenvy::dotenv() {
            tracing::info!("[start] Loaded environment from {}", path.display());
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

        // Create shared timing context - clock starts now
        let time = Arc::new(TimeContext::new());

        // Create iceoryx2 Node for cross-process communication
        tracing::info!("[start] Creating iceoryx2 Node...");
        let iceoryx2_node = Iceoryx2Node::new()?;
        tracing::info!("[start] iceoryx2 Node created");

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
            #[cfg(not(target_os = "macos"))]
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

            // Cleanup SurfaceStore (macOS only) - releases all surfaces and disconnects XPC
            #[cfg(target_os = "macos")]
            {
                ctx.gpu.clear_surface_store();
                tracing::debug!("[stop] SurfaceStore cleared");
            }
        }

        // Clear runtime context - allows fresh context on next start().
        // This enables per-session tracking (e.g., AI agents analyzing runtime state).
        *self.runtime_context.lock() = None;
        tracing::debug!("[stop] Runtime context cleared");

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
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                StreamError::Runtime(format!(
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
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                StreamError::Runtime(format!(
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
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node
                .get::<ProcessorPauseGateComponent>()
                .ok_or_else(|| StreamError::ProcessorNotFound(processor_id.to_string()))?;

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
            crate::core::StreamError::Configuration(format!(
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

        let listener = ShutdownListener {
            flag: shutdown_flag_clone.clone(),
        };
        PUBSUB.subscribe(
            topics::RUNTIME_GLOBAL,
            Arc::new(parking_lot::Mutex::new(listener)),
        );

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
                .map_err(|_| StreamError::GraphError("Unable to serialize graph".into()))
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
                StreamError::GraphError(format!("Unknown processor alias: '{}'", from.alias))
            })?;
            let to_id = alias_to_id.get(to.alias).ok_or_else(|| {
                StreamError::GraphError(format!("Unknown processor alias: '{}'", to.alias))
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
}

/// Extract a .slpkg ZIP archive to the package cache.
/// Cache key is {name}-{version} from the embedded streamlib.yaml.
/// Always overwrites on load.
pub fn extract_slpkg_to_cache(slpkg_path: &std::path::Path) -> Result<std::path::PathBuf> {
    use crate::core::config::ProjectConfig;

    let slpkg_bytes = std::fs::read(slpkg_path).map_err(|e| {
        StreamError::Configuration(format!("Failed to read {}: {}", slpkg_path.display(), e))
    })?;

    let cursor = std::io::Cursor::new(&slpkg_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| StreamError::Configuration(format!("Failed to open .slpkg archive: {}", e)))?;

    // Read streamlib.yaml from archive to get name + version
    let manifest_yaml = {
        let mut manifest_file = archive.by_name(ProjectConfig::FILE_NAME).map_err(|e| {
            StreamError::Configuration(format!(
                ".slpkg archive missing {}: {}",
                ProjectConfig::FILE_NAME,
                e
            ))
        })?;
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut manifest_file, &mut contents)
            .map_err(|e| StreamError::Configuration(format!("Failed to read manifest: {}", e)))?;
        contents
    };

    let config: ProjectConfig = serde_yaml::from_str(&manifest_yaml)
        .map_err(|e| StreamError::Configuration(format!("Failed to parse manifest: {}", e)))?;

    let package = config.package.as_ref().ok_or_else(|| {
        StreamError::Configuration("streamlib.yaml missing [package] section".to_string())
    })?;

    let cache_key = format!("{}-{}", package.name, package.version);
    let cache_dir = crate::core::streamlib_home::get_cached_package_dir(&cache_key);

    // Always overwrite
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)
            .map_err(|e| StreamError::Configuration(format!("Failed to clear cache dir: {}", e)))?;
    }

    tracing::info!(
        "Extracting {} to {}",
        slpkg_path.display(),
        cache_dir.display()
    );
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| StreamError::Configuration(format!("Failed to create cache dir: {}", e)))?;

    // Re-open archive (cursor consumed by manifest read)
    let cursor = std::io::Cursor::new(&slpkg_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
        StreamError::Configuration(format!("Failed to re-open .slpkg archive: {}", e))
    })?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            StreamError::Configuration(format!("Failed to read archive entry: {}", e))
        })?;

        let file_name = file.name().to_string();

        // Security: reject path traversal
        if file_name.contains("..") || file_name.starts_with('/') {
            return Err(StreamError::Configuration(format!(
                "Invalid path in .slpkg archive: {}",
                file_name
            )));
        }

        let output_path = cache_dir.join(&file_name);

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                StreamError::Configuration(format!("Failed to create directory: {}", e))
            })?;
        }

        let mut output_file = std::fs::File::create(&output_path).map_err(|e| {
            StreamError::Configuration(format!("Failed to create {}: {}", output_path.display(), e))
        })?;

        std::io::copy(&mut file, &mut output_file).map_err(|e| {
            StreamError::Configuration(format!("Failed to extract {}: {}", file_name, e))
        })?;
    }

    Ok(cache_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let _runtime = StreamRuntime::new();
        // Runtime creates successfully
    }

    #[test]
    fn test_new_outside_tokio_creates_owned_runtime() {
        // Outside tokio context - creates owned runtime
        let runtime = StreamRuntime::new().unwrap();
        assert!(matches!(
            runtime.tokio_runtime_variant,
            TokioRuntimeVariant::OwnedTokioRuntime(_)
        ));
    }

    #[test]
    fn test_new_inside_tokio_uses_external_handle() {
        // Inside tokio context - auto-detects and uses external handle
        let temp_rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = temp_rt.block_on(async { StreamRuntime::new() });
        assert!(result.is_ok());
        let runtime = result.unwrap();
        assert!(matches!(
            runtime.tokio_runtime_variant,
            TokioRuntimeVariant::ExternalTokioHandle(_)
        ));
    }

    #[test]
    fn test_sync_methods_work_inside_tokio() {
        // Verify sync methods work when called from tokio context
        let temp_rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        temp_rt.block_on(async {
            let runtime = StreamRuntime::new().unwrap();
            // Sync methods should work (use spawn + channel internally)
            let json = runtime.to_json().unwrap();
            assert!(json["nodes"].is_array());
        });
    }
}
