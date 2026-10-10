// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use streamlib_runtime_client_contract::streamlib_runtime_directory::StreamlibRuntimeDirectory;

use super::RuntimeName;
use super::RuntimeUniqueId;
use super::loaded_stream::LoadedStreamInThisRuntime;
use super::processor_interpreter_launch_record::ProcessorInterpreterLendDirectoryOfTheEngine;
use super::stream_actions_of_this_runtime::{LoadedStreamHolding, StreamActionsOfTheEngine};
use super::{RuntimeShutdownEscalation, StreamEnvironment};
use crate::core::context::GpuContext;
use crate::core::graph::{cast_exposed_name_to_url_safe, names_listed_for_a_refusal};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::signals::ScopedShutdownSignalOwnership;
use crate::core::{Error, Result};
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

/// What every stream loaded in one [`Runner`] shares: the engine's tokio
/// runtime, iceoryx2 node, surface-sharing service, GPU context, lend
/// directory and the table of loaded streams.
///
/// Fields drop in declaration order, so the hold on the process's logging
/// pathway, declared last, lets go after everything else is gone.
pub(crate) struct EngineResourcesSharedByEveryStream {
    /// Unique identifier for this runtime instance.
    pub(crate) runtime_id: Arc<RuntimeUniqueId>,
    /// The name this runtime's tap channels carry.
    pub(crate) runtime_name: Arc<RuntimeName>,
    /// The streams loaded in this runtime, keyed by their URL-safe cast name.
    pub(crate) streams_loaded_in_this_runtime:
        Mutex<BTreeMap<String, Arc<LoadedStreamInThisRuntime>>>,
    /// The one GPU context, created by the first stream that starts.
    gpu_context_created_by_the_first_stream_start: Mutex<Option<GpuContext>>,
    /// Hooks run once, when the GPU context is created, before any
    /// processor's `setup()` runs.
    setup_hooks: Mutex<SetupHooksOfTheEngine>,
    /// The lend directory the host handed this runtime.
    pub(crate) processor_interpreter_lend_directory: ProcessorInterpreterLendDirectoryOfTheEngine,
    /// Whether [`Runner::shut_down`] has run.
    shut_down: AtomicBool,
    /// Whether this engine holds the machine's shutdown signals now, and so
    /// walks every loaded stream to the machine's shutdown level as it waits.
    owns_the_machine_shutdown_signals: AtomicBool,
    /// The runtime-internal surface-sharing service, alive for the engine's
    /// life; helper processes connect to it via `STREAMLIB_SURFACE_SOCKET`.
    #[cfg(target_os = "linux")]
    surface_service: Mutex<Option<crate::linux::surface_share::UnixSocketSurfaceService>>,
    /// Path of the surface-sharing socket, inside the runtime directory.
    #[cfg(target_os = "linux")]
    pub(crate) surface_socket_path: PathBuf,
    /// The runtime-internal surface-sharing Mach service, alive for the
    /// engine's life; helper processes connect to it through
    /// `STREAMLIB_SURFACE_MACH_SERVICE`.
    #[cfg(target_os = "macos")]
    mach_surface_share_service: Mutex<Option<crate::apple::surface_share::MachSurfaceShareService>>,
    /// The Mach service's name and helper-process admissions.
    #[cfg(target_os = "macos")]
    pub(crate) surface_share_mach_service_rendezvous:
        crate::apple::surface_share::MachSurfaceShareServiceRendezvous,
    /// The surfaces cross-process consumers currently hold checked out, owned
    /// by the service above and read through each stream's surface store.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) surface_check_out_leases: Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
    /// The service's registrations by owner, from which a stopping stream's
    /// go: the service keeps a registration from this process past its
    /// connection's close.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    surface_share_registrations_by_owner:
        Arc<dyn crate::core::context::SurfaceShareRegistrationsByRuntime + Send + Sync>,
    /// The Mach service's table of engine timeline pairs by surface, shared
    /// with each stream's surface store.
    #[cfg(target_os = "macos")]
    pub(crate) surface_share_cross_process_timeline_pairs:
        Arc<crate::apple::surface_share::CrossProcessTimelinePairsBySurface>,
    /// The runtime directory this runtime resolved as it was built.
    pub(crate) runtime_directory: StreamlibRuntimeDirectory,
    /// iceoryx2 Node for creating Services, Publishers, and Subscribers.
    pub(crate) iceoryx2_node: Iceoryx2Node,
    /// Tokio runtime storage - either owned or external handle.
    pub(crate) tokio_runtime_variant: TokioRuntimeVariant,
    /// The runtime's own log, when the host asked for one; closed before the
    /// hold on the logging pathway lets go.
    runtime_own_log: Option<crate::core::logging::TheRuntimesOwnLogWhileItsEngineLives>,
    /// This engine's hold on the process's logging pathway, which keeps the
    /// standard streams intercepted while any engine lives.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    process_logging_pathway_hold: crate::core::logging::ProcessLoggingPathwayHold,
}

impl EngineResourcesSharedByEveryStream {
    /// The engine's GPU context — created now when no stream has started yet,
    /// its own surface store installed and the setup hooks run once — and the
    /// view `make_the_starting_streams_view` builds over it.
    ///
    /// The slot stays locked until the hooks have run, so no other stream's
    /// start commits a processor before them.
    pub(crate) fn gpu_context_view_for_a_starting_stream(
        &self,
        make_the_starting_streams_view: impl FnOnce(&GpuContext) -> Result<GpuContext>,
    ) -> Result<GpuContext> {
        let engine_gpu_context = {
            let mut gpu_context_slot = self.gpu_context_created_by_the_first_stream_start.lock();
            let engine_gpu_context = match gpu_context_slot.as_ref() {
                Some(engine_gpu_context) => engine_gpu_context.clone(),
                None => {
                    tracing::info!("[start] Initializing the engine's GPU context...");
                    let created = GpuContext::init_for_platform_sync()?;
                    tracing::info!("[start] The engine's GPU context is initialized");
                    gpu_context_slot.insert(created).clone()
                }
            };
            // Retried by every start until it connects, and the hooks wait for
            // it, because they register the engine's host surfaces through it.
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if engine_gpu_context.engines_own_surface_store().is_none() {
                engine_gpu_context.install_the_engines_surface_store(
                    self.a_connected_surface_store_owned_by(self.runtime_id.to_string())?,
                )?;
            }
            self.run_the_setup_hooks_once(&engine_gpu_context)?;
            engine_gpu_context
        };
        make_the_starting_streams_view(&engine_gpu_context)
    }

    /// Run every setup hook once — each, even after one fails — and report
    /// the first failure. A later call runs none.
    fn run_the_setup_hooks_once(&self, engine_gpu_context: &GpuContext) -> Result<()> {
        let hooks = match std::mem::replace(
            &mut *self.setup_hooks.lock(),
            SetupHooksOfTheEngine::RanAsTheGpuContextWasCreated,
        ) {
            SetupHooksOfTheEngine::WaitingForTheGpuContext(hooks) => hooks,
            SetupHooksOfTheEngine::RanAsTheGpuContextWasCreated => return Ok(()),
        };
        if hooks.is_empty() {
            return Ok(());
        }
        tracing::info!("[start] Running {} setup hook(s)", hooks.len());
        let mut first_failure = Ok(());
        for hook in hooks {
            if let Err(hook_failure) = hook(engine_gpu_context) {
                tracing::error!("[start] A setup hook failed: {hook_failure}");
                if first_failure.is_ok() {
                    first_failure = Err(hook_failure);
                }
            }
        }
        first_failure
    }

    /// A surface store that registers under `owner_key`, connected to the
    /// engine's surface-sharing service.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn a_connected_surface_store_owned_by(
        &self,
        owner_key: String,
    ) -> Result<crate::core::context::SurfaceStore> {
        use crate::core::context::SurfaceStore;

        let surface_share_address = self.surface_share_address();
        #[cfg(target_os = "linux")]
        let surface_store = SurfaceStore::new_reading_check_out_leases(
            surface_share_address.clone(),
            owner_key.clone(),
            Arc::clone(&self.surface_check_out_leases),
        );
        #[cfg(target_os = "macos")]
        let surface_store = SurfaceStore::new_sharing_the_mach_services_tables(
            surface_share_address.clone(),
            owner_key.clone(),
            Arc::clone(&self.surface_check_out_leases),
            Arc::clone(&self.surface_share_cross_process_timeline_pairs),
        );
        surface_store.connect().map_err(|connect_failure| {
            Error::Runtime(format!(
                "the surface store of `{owner_key}` failed to connect to the runtime-internal \
                 surface-sharing service at {surface_share_address}: {connect_failure}"
            ))
        })?;
        Ok(surface_store)
    }

    /// Release every surface registered under `owner_key` — a stopping
    /// stream's — from the engine's surface-sharing service.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn release_every_surface_registered_by(&self, owner_key: &str) {
        crate::core::context::surface_share_wire_verbs::release_every_surface_registered_by(
            self.surface_share_registrations_by_owner.as_ref(),
            owner_key,
        );
    }

    /// The address a stream's surface store connects to.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn surface_share_address(&self) -> String {
        #[cfg(target_os = "linux")]
        {
            self.surface_socket_path.to_string_lossy().to_string()
        }
        #[cfg(target_os = "macos")]
        {
            self.surface_share_mach_service_rendezvous
                .service_name()
                .to_string()
        }
    }

    /// Take `stream` out of the table, unless another stream has taken its
    /// name since.
    pub(crate) fn remove_from_the_stream_table(&self, stream: &Arc<LoadedStreamInThisRuntime>) {
        let mut streams = self.streams_loaded_in_this_runtime.lock();
        if streams
            .get(stream.stream_name())
            .is_some_and(|loaded| Arc::ptr_eq(loaded, stream))
        {
            streams.remove(stream.stream_name());
        }
    }

    fn every_loaded_stream(&self) -> Vec<Arc<LoadedStreamInThisRuntime>> {
        self.streams_loaded_in_this_runtime
            .lock()
            .values()
            .cloned()
            .collect()
    }
}

/// A hook run once with the engine's GPU context.
type SetupHookOfTheEngine = Box<dyn FnOnce(&GpuContext) -> Result<()> + Send>;

/// The engine's setup hooks: queued until the GPU context is created, then
/// run once.
enum SetupHooksOfTheEngine {
    WaitingForTheGpuContext(Vec<SetupHookOfTheEngine>),
    RanAsTheGpuContextWasCreated,
}

/// The engine: a table of loaded streams under one GPU context, iceoryx2
/// node, tokio runtime and surface service.
///
/// Every graph operation is a stream's: load a stream, then use the
/// [`LoadedStreamInThisRuntime`] it hands back.
pub struct Runner {
    pub(super) engine_resources_shared_by_every_stream: Arc<EngineResourcesSharedByEveryStream>,
    /// Where this runtime keeps its streams, and the lock its stream actions
    /// take one at a time.
    pub(super) stream_actions: StreamActionsOfTheEngine,
}

/// What a host chooses as it constructs a [`Runner`].
#[derive(Debug, Clone, Default)]
pub struct RunnerConstructionOptions {
    /// The runtime's name; else `STREAMLIB_RUNTIME_NAME`, else
    /// `<host name>-<app directory name>-<id>`.
    pub runtime_name: Option<String>,
    /// The standard stream the pretty log mirror writes to, when this runtime
    /// is the first in its process to install the process's logging pathway.
    pub pretty_log_mirror_stream: crate::core::logging::PrettyLogMirrorStandardStream,
    /// Where the records no stream emits are also written as JSONL, to
    /// `tatolabd-<started_at_millis>.jsonl`, rotated as a stream's log is.
    pub runtime_own_log_directory: Option<PathBuf>,
}

/// What a host chooses as it loads one stream into a [`Runner`].
#[derive(Debug, Clone, Default)]
pub struct OptionsForLoadingOneStream {
    /// The stream's name, overriding the one the graph carries. Required when
    /// the graph names none, and for an empty stream.
    pub stream_name: Option<String>,
    /// The stream's project directory, when no stream environment is given.
    pub project_directory: Option<PathBuf>,
    /// Where the stream's processor interpreters start; its project directory
    /// is the stream's project directory.
    pub stream_environment: Option<StreamEnvironment>,
}

impl OptionsForLoadingOneStream {
    /// A stream whose project lives in `project_directory` and starts no
    /// processor interpreter.
    pub fn in_project_directory(project_directory: impl Into<PathBuf>) -> Self {
        Self {
            project_directory: Some(project_directory.into()),
            ..Self::default()
        }
    }

    /// A stream whose processor interpreters start in `stream_environment`.
    pub fn in_stream_environment(stream_environment: StreamEnvironment) -> Self {
        Self {
            stream_environment: Some(stream_environment),
            ..Self::default()
        }
    }

    /// These options, loading the stream as `stream_name`.
    pub fn named(self, stream_name: impl Into<String>) -> Self {
        Self {
            stream_name: Some(stream_name.into()),
            ..self
        }
    }

    /// The stream's project directory: the stream environment's, else the one
    /// given, refused when the two disagree or neither is given.
    fn resolved_project_directory(&self, stream_name: &str) -> Result<PathBuf> {
        match (&self.stream_environment, &self.project_directory) {
            (Some(stream_environment), Some(project_directory))
                if *project_directory != stream_environment.project_directory =>
            {
                Err(Error::Configuration(format!(
                    "the stream `{stream_name}` was given the project directory `{}` and a \
                     stream environment in `{}`; a stream's environment lives in its project \
                     directory, so give one of them, or the same directory to both",
                    project_directory.display(),
                    stream_environment.project_directory.display()
                )))
            }
            (Some(stream_environment), _) => Ok(stream_environment.project_directory.clone()),
            (None, Some(project_directory)) => Ok(project_directory.clone()),
            (None, None) => Err(Error::Configuration(format!(
                "the stream `{stream_name}` was given no project directory; a stream's logs, \
                 caches and node identities live under its project, so load it with one"
            ))),
        }
    }
}

/// How a load watched for a machine shutdown request ended.
pub enum StreamLoadObservingMachineShutdownRequests {
    /// The stream loaded and no machine shutdown was requested.
    Loaded(Arc<LoadedStreamInThisRuntime>),
    /// A machine shutdown was requested before the load or while it ran; the
    /// stream was never loaded.
    AbandonedForAMachineShutdownRequest,
}

/// What [`Runner::wait_until_every_stream_has_ended`] saw once every stream had
/// ended.
pub struct EveryStreamEndedDuringTheWait {
    /// Every stream loaded at any point of the wait, in the order it was first
    /// seen.
    pub streams_seen_during_the_wait: Vec<Arc<LoadedStreamInThisRuntime>>,
    /// How the first of them to end with a failure ended, or `Ok` when none did.
    pub how_the_first_failed_stream_ended: Result<()>,
}

impl Runner {
    /// Build a runtime named from `STREAMLIB_RUNTIME_NAME` or the default.
    pub fn new() -> Result<Arc<Self>> {
        Self::new_with_construction_options(RunnerConstructionOptions::default())
    }

    /// Build a runtime as `construction_options` choose.
    pub fn new_with_construction_options(
        construction_options: RunnerConstructionOptions,
    ) -> Result<Arc<Self>> {
        let RunnerConstructionOptions {
            runtime_name,
            pretty_log_mirror_stream,
            runtime_own_log_directory,
        } = construction_options;
        // Cap per-thread timer slack at 1 ns on the calling thread before
        // spawning any worker. Linux defaults to 50 µs grouping for
        // `epoll_wait` / `nanosleep` / `futex` relative timeouts; new
        // threads inherit the creator's slack at clone time, so setting it
        // here propagates to the tokio worker pool, the logging drain
        // worker, the iceoryx2 node, and every processor thread spawned
        // later. SCHED_FIFO/RR threads (rtkit-promoted reactive processors)
        // bypass slack entirely per kernel design — this only affects
        // SCHED_OTHER waits. Cannot fail for self per `prctl(2)`.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::prctl(libc::PR_SET_TIMERSLACK, 1u64, 0u64, 0u64, 0u64);
        }

        // Auto-detect tokio context FIRST — telemetry exporters need a Tokio handle.
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

        // The id names every stream's log file, so a pinned one is refused
        // before the runtime writes anything.
        let runtime_id = Arc::new(RuntimeUniqueId::from_env_or_generate()?);

        // A name the caller cannot use in a port address is a wiring error,
        // and refusing it here costs nothing that has to be undone.
        let resolved_runtime_name =
            RuntimeName::from_configuration_environment_or_default(runtime_name)?;

        // See `docs/logging-schema.md` for the schema.
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
        let process_logging_pathway_hold = crate::core::logging::hold_the_process_logging_pathway(
            crate::core::logging::StreamlibLoggingConfig {
                pretty_log_mirror_stream: Some(pretty_log_mirror_stream),
                ..crate::core::logging::StreamlibLoggingConfig::for_runtime("streamlib-runtime")
            },
        )
        .map_err(|e| Error::Runtime(format!("Failed to initialize logging: {}", e)))?;
        let runtime_own_log = runtime_own_log_directory.as_deref().and_then(|directory| {
            crate::core::logging::TheRuntimesOwnLogWhileItsEngineLives::open(
                runtime_id.as_str(),
                directory,
            )
        });
        let runtime_name = Arc::new(
            resolved_runtime_name
                .take_the_runtime_name_warning_when_the_default_carries_the_stand_in_host_name(),
        );
        tracing::info!("Creating Runner named {runtime_name} with ID: {runtime_id}");

        let runtime_directory = StreamlibRuntimeDirectory::resolve()
            .map_err(|refusal| Error::Runtime(refusal.to_string()))?;
        tracing::info!(
            "StreamLib runtime directory: {}",
            runtime_directory.path().display()
        );

        let streamlib_home =
            streamlib_runtime_client_contract::streamlib_home::get_streamlib_home();
        tracing::debug!("STREAMLIB_HOME: {}", streamlib_home.display());
        crate::core::runtime_hooks::run_init_hooks(&streamlib_home)?;

        // Bridge iceoryx2's internal log records into streamlib tracing
        // before creating the iceoryx2 Node so any iceoryx2 emit at
        // construction time lands in the unified JSONL pipeline.
        crate::core::logging::install_iceoryx2_log_bridge_at_the_engines_configured_level();

        // Binding the surface-sharing service is also the refusal of a second
        // live runtime with this id, so it runs before the iceoryx2 node.
        #[cfg(target_os = "linux")]
        let (
            surface_service,
            surface_socket_path,
            surface_check_out_leases,
            surface_share_registrations_by_owner,
        ) = bring_up_surface_service(&runtime_directory, &runtime_id)?;
        #[cfg(target_os = "macos")]
        let (
            mach_surface_share_service,
            surface_share_mach_service_rendezvous,
            surface_check_out_leases,
            surface_share_cross_process_timeline_pairs,
            surface_share_registrations_by_owner,
        ) = bring_up_mach_surface_share_service(&runtime_id)?;

        crate::iceoryx2::warn_when_posix_shared_memory_is_short_for_a_runtime();

        tracing::info!("[new] Creating iceoryx2 Node...");
        let iceoryx2_node = Iceoryx2Node::new(
            &runtime_directory.iceoryx2_domain_root(),
            &format!("streamlib-runtime/{runtime_id}"),
        )?;
        tracing::info!("[new] iceoryx2 Node created");

        Ok(Arc::new(Self {
            engine_resources_shared_by_every_stream: Arc::new(EngineResourcesSharedByEveryStream {
                runtime_id,
                runtime_name,
                streams_loaded_in_this_runtime: Mutex::new(BTreeMap::new()),
                gpu_context_created_by_the_first_stream_start: Mutex::new(None),
                setup_hooks: Mutex::new(SetupHooksOfTheEngine::WaitingForTheGpuContext(Vec::new())),
                processor_interpreter_lend_directory:
                    ProcessorInterpreterLendDirectoryOfTheEngine::default(),
                shut_down: AtomicBool::new(false),
                owns_the_machine_shutdown_signals: AtomicBool::new(false),
                #[cfg(target_os = "linux")]
                surface_service: Mutex::new(Some(surface_service)),
                #[cfg(target_os = "linux")]
                surface_socket_path,
                #[cfg(target_os = "macos")]
                mach_surface_share_service: Mutex::new(Some(mach_surface_share_service)),
                #[cfg(target_os = "macos")]
                surface_share_mach_service_rendezvous,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                surface_check_out_leases,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                surface_share_registrations_by_owner,
                #[cfg(target_os = "macos")]
                surface_share_cross_process_timeline_pairs,
                runtime_directory,
                iceoryx2_node,
                tokio_runtime_variant,
                runtime_own_log,
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
                process_logging_pathway_hold,
            }),
            stream_actions: StreamActionsOfTheEngine::default(),
        }))
    }

    /// Register a one-shot hook to run with the engine's GPU context when the
    /// first stream to start creates it, before any processor's `setup()`
    /// runs. Hooks fire FIFO, every one even after one fails; the first
    /// failure aborts that stream's `start()`. Refused once the hooks have
    /// run.
    pub fn install_setup_hook<F>(&self, hook: F) -> Result<()>
    where
        F: FnOnce(&GpuContext) -> Result<()> + Send + 'static,
    {
        match &mut *self
            .engine_resources_shared_by_every_stream
            .setup_hooks
            .lock()
        {
            SetupHooksOfTheEngine::WaitingForTheGpuContext(hooks) => {
                hooks.push(Box::new(hook));
                Ok(())
            }
            SetupHooksOfTheEngine::RanAsTheGpuContextWasCreated => Err(Error::Configuration(
                "a setup hook was installed after the engine's GPU context was created and its \
                 setup hooks had run; install every setup hook before the first stream starts"
                    .into(),
            )),
        }
    }

    /// Path of the runtime-internal surface-sharing Unix socket, bound during
    /// [`Runner::new`] at `<runtime directory>/surface-share-<runtime_id>.sock`.
    #[cfg(target_os = "linux")]
    pub fn surface_socket_path(&self) -> &std::path::Path {
        &self
            .engine_resources_shared_by_every_stream
            .surface_socket_path
    }

    /// The runtime-internal surface-sharing Mach service's name and
    /// helper-process admissions, registered during [`Runner::new`].
    #[cfg(target_os = "macos")]
    pub fn surface_share_mach_service_rendezvous(
        &self,
    ) -> &crate::apple::surface_share::MachSurfaceShareServiceRendezvous {
        &self
            .engine_resources_shared_by_every_stream
            .surface_share_mach_service_rendezvous
    }

    /// Unique identifier for this runtime instance.
    pub fn runtime_id(&self) -> &RuntimeUniqueId {
        &self.engine_resources_shared_by_every_stream.runtime_id
    }

    /// The name this runtime's tap channels carry.
    pub fn runtime_name(&self) -> &RuntimeName {
        &self.engine_resources_shared_by_every_stream.runtime_name
    }

    /// The active segment of the runtime's own log, `None` when it keeps none.
    pub fn runtime_own_log_path(&self) -> Option<&std::path::Path> {
        self.engine_resources_shared_by_every_stream
            .runtime_own_log
            .as_ref()
            .and_then(|runtime_own_log| runtime_own_log.jsonl_log_path())
    }

    /// The runtime directory this runtime resolved as it was built.
    pub fn runtime_directory(&self) -> &StreamlibRuntimeDirectory {
        &self
            .engine_resources_shared_by_every_stream
            .runtime_directory
    }

    /// This runtime's iceoryx2 node.
    pub fn iceoryx2_node(&self) -> &Iceoryx2Node {
        &self.engine_resources_shared_by_every_stream.iceoryx2_node
    }

    /// The engine's tokio runtime handle, which a host serving this runtime's
    /// local API runs it on.
    pub fn tokio_handle(&self) -> tokio::runtime::Handle {
        self.engine_resources_shared_by_every_stream
            .tokio_runtime_variant
            .handle()
    }

    /// Hand fds 1 and 2 back to the process now, for a caller about to leave
    /// this engine alive rather than drop it.
    pub fn stop_intercepting_the_standard_streams(&self) {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
        self.engine_resources_shared_by_every_stream
            .process_logging_pathway_hold
            .stop_intercepting_the_standard_streams();
    }

    /// Hand this runtime the lend directory — the directory holding the
    /// `tatolab/runtime/` package every stream's processor interpreters
    /// borrow. The host calls it once, before any load; a second call is
    /// refused.
    pub fn set_processor_interpreter_lend_directory(&self, lend_directory: PathBuf) -> Result<()> {
        self.engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .set(lend_directory)
    }

    // =========================================================================
    // The stream table
    // =========================================================================

    /// Load `graph` as a stream: built in a scope of its own and published to
    /// the stream table only once every node, link and exposure is in.
    ///
    /// The stream's name — `load_options`' else the graph's, cast URL-safe —
    /// is refused when it casts to nothing, when there is none, and when a
    /// stream of that name is loaded; a graph holding no node is refused. A
    /// load refused anywhere after that is torn down through the stream's own
    /// stop and inserts nothing.
    pub fn load_stream_from_graph_snapshot(
        &self,
        graph: &GraphSnapshot,
        load_options: OptionsForLoadingOneStream,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let stream = self.build_the_stream_a_graph_load_names(
            graph,
            load_options,
            LoadedStreamHolding::Attached,
        )?;
        self.load_the_graph_into_the_stream(&stream, graph)?;
        self.insert_a_complete_stream(stream)
    }

    /// Load `graph` as a stream on a thread of its own while this one watches
    /// for a machine shutdown request. Once one is seen the stream being
    /// loaded — which is in no table the machine's walk reaches — is walked to
    /// the machine's level and its describes interrupted. A load a machine
    /// shutdown cut short is abandoned without a refusal.
    pub fn load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested(
        &self,
        graph: &GraphSnapshot,
        load_options: OptionsForLoadingOneStream,
    ) -> Result<StreamLoadObservingMachineShutdownRequests> {
        self.load_stream_held_as_unless_a_machine_shutdown_is_requested(
            graph,
            load_options,
            LoadedStreamHolding::Attached,
        )
    }

    /// [`Self::load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested`],
    /// the stream held as `holding` from the moment it enters the table.
    pub(super) fn load_stream_held_as_unless_a_machine_shutdown_is_requested(
        &self,
        graph: &GraphSnapshot,
        load_options: OptionsForLoadingOneStream,
        holding: LoadedStreamHolding,
    ) -> Result<StreamLoadObservingMachineShutdownRequests> {
        use std::sync::mpsc::RecvTimeoutError;

        use crate::core::runtime::{
            RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL, is_the_machines_shutdown_requested,
        };

        let stream = match self.build_the_stream_a_graph_load_names(graph, load_options, holding) {
            Ok(stream) => stream,
            // Read after the build, so a request landing during it abandons
            // the load rather than refusing it.
            Err(build_refusal) if is_the_machines_shutdown_requested() => {
                tracing::info!(
                    "a machine shutdown was requested before the stream loaded, so it was never \
                     loaded; building it reported: {build_refusal}"
                );
                return Ok(
                    StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest,
                );
            }
            Err(build_refusal) => return Err(build_refusal),
        };
        let load_outcome = std::thread::scope(|scope| {
            // Never sent on: the loading thread's end drops it, a panic included.
            let (load_ended_sender, load_ended_receiver) = std::sync::mpsc::channel::<()>();
            let stream_being_loaded = &stream;
            let loading = scope.spawn(move || {
                let load_outcome = self.load_the_graph_into_the_stream(stream_being_loaded, graph);
                drop(load_ended_sender);
                load_outcome
            });
            loop {
                // Repeated, because the load forgets an interrupt that lands
                // before it begins.
                if is_the_machines_shutdown_requested() {
                    walk_a_stream_being_loaded_to_the_machines_shutdown_level(&stream);
                }
                if let Err(RecvTimeoutError::Disconnected) = load_ended_receiver
                    .recv_timeout(RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL)
                {
                    break;
                }
            }
            loading
                .join()
                .unwrap_or_else(|load_panic| std::panic::resume_unwind(load_panic))
        });

        if !is_the_machines_shutdown_requested() {
            load_outcome?;
            return self
                .insert_a_complete_stream(stream)
                .map(StreamLoadObservingMachineShutdownRequests::Loaded);
        }
        match load_outcome {
            Ok(()) => {
                tracing::info!(
                    "a machine shutdown was requested while the stream `{}` loaded, so it was \
                     never loaded or started",
                    stream.stream_name()
                );
                tear_down_a_stream_that_never_entered_the_table(&stream);
            }
            Err(interrupted_load) => tracing::info!(
                "a machine shutdown was requested while the stream loaded, so it was never \
                 started; the interrupted load reported: {interrupted_load}"
            ),
        }
        Ok(StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest)
    }

    /// Load an empty stream a Rust host builds in code, published to the
    /// stream table at once. `load_options` must name it.
    pub fn load_an_empty_stream(
        &self,
        load_options: OptionsForLoadingOneStream,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.load_an_empty_stream_with_its_teardown_watchdog_budget(
            load_options,
            super::ENGINE_TEARDOWN_WATCHDOG_BUDGET,
        )
    }

    /// Load an empty stream whose teardown watchdog fires after
    /// `teardown_watchdog_budget` rather than the engine's budget.
    #[cfg(test)]
    pub(crate) fn load_an_empty_stream_whose_teardown_watchdog_fires_after(
        &self,
        load_options: OptionsForLoadingOneStream,
        teardown_watchdog_budget: Duration,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.load_an_empty_stream_with_its_teardown_watchdog_budget(
            load_options,
            teardown_watchdog_budget,
        )
    }

    fn load_an_empty_stream_with_its_teardown_watchdog_budget(
        &self,
        load_options: OptionsForLoadingOneStream,
        teardown_watchdog_budget: Duration,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.refuse_a_load_while_the_machine_shuts_down(load_options.stream_name.as_deref())?;
        let stream_name = the_cast_name_of_the_stream_a_load_names(
            load_options.stream_name.as_deref(),
            "an empty stream",
        )?;
        let stream = self.build_a_stream(
            stream_name,
            load_options,
            teardown_watchdog_budget,
            LoadedStreamHolding::Attached,
        )?;
        self.insert_a_complete_stream(stream)
    }

    /// The loaded stream `stream_name` names once cast, refused naming the
    /// streams that are loaded — a name that casts to nothing is refused as
    /// one that cannot name a stream.
    pub fn loaded_stream_named(&self, stream_name: &str) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let cast = cast_exposed_name_to_url_safe(stream_name).map_err(|casts_to_nothing| {
            Error::NotFound(format!(
                "cannot name a stream `{stream_name}`: {casts_to_nothing}. Loaded: {}",
                self.loaded_stream_names_listed_for_a_refusal()
            ))
        })?;
        self.loaded_stream_of_the_cast_name(&cast).ok_or_else(|| {
            Error::NotFound(format!(
                "no stream named `{stream_name}` is loaded in this runtime. Loaded: {}",
                self.loaded_stream_names_listed_for_a_refusal()
            ))
        })
    }

    /// The loaded stream `stream_cast`, already cast, names.
    pub(super) fn loaded_stream_of_the_cast_name(
        &self,
        stream_cast: &str,
    ) -> Option<Arc<LoadedStreamInThisRuntime>> {
        self.engine_resources_shared_by_every_stream
            .streams_loaded_in_this_runtime
            .lock()
            .get(stream_cast)
            .cloned()
    }

    /// Every loaded stream, read under one lock of the stream table.
    pub fn every_loaded_stream(&self) -> Vec<Arc<LoadedStreamInThisRuntime>> {
        self.engine_resources_shared_by_every_stream
            .every_loaded_stream()
    }

    /// The cast names of the loaded streams, in order.
    pub fn names_of_the_loaded_streams(&self) -> Vec<String> {
        self.engine_resources_shared_by_every_stream
            .streams_loaded_in_this_runtime
            .lock()
            .keys()
            .cloned()
            .collect()
    }

    /// Shut the stream `stream_name` names down and wait until it has ended —
    /// stopped and out of the table, or abandoned by its watchdog — reporting
    /// how it ended.
    pub fn unload_stream(&self, stream_name: &str) -> Result<()> {
        let stream = self.loaded_stream_named(stream_name)?;
        request_a_streams_shutdown_and_wait_until_it_has_ended(
            &stream,
            "the stream is being unloaded",
        );
        stream.how_this_stream_ended_as_a_waiter_reports_it()
    }

    /// Raise the machine's shutdown level to graceful, exactly as a first
    /// signal does: whoever owns the machine's shutdown signals walks every
    /// loaded stream to it.
    pub fn request_the_shutdown_of_every_loaded_stream(&self, reason: &str) -> Result<()> {
        crate::core::runtime::request_the_shutdown_of_every_loaded_stream(reason)
    }

    // =========================================================================
    // The machine's shutdown signals and the waits
    // =========================================================================

    /// Run `run` while owning the machine's shutdown signals (SIGINT, SIGTERM,
    /// SIGHUP) — the waits inside it walk every loaded stream to the machine's
    /// shutdown level — then clear what this run's interrupts escalated. Fails
    /// if another owner in this process already holds them.
    pub fn run_owning_the_machine_shutdown_signals<R>(
        &self,
        run: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        let run_outcome = {
            let _shutdown_signals = take_shutdown_signal_ownership()?;
            let _owned_by_this_engine = TheMachineShutdownSignalsOwnedByOneEngine::mark(
                &self.engine_resources_shared_by_every_stream,
            );
            run()
        };
        crate::core::runtime::take_the_machines_shutdown_escalation();
        run_outcome
    }

    /// Block until `stream` has ended — its own shutdown request, a machine
    /// shutdown, an unload or its watchdog — reporting how it ended.
    pub fn wait_until_the_stream_ends(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
    ) -> Result<()> {
        self.block_until(&|| stream.has_ended());
        stream.how_this_stream_ended_as_a_waiter_reports_it()
    }

    /// Block until every loaded stream has ended, including each loaded while
    /// the wait runs, returning every stream it saw and the first that ended
    /// with a failure.
    pub fn wait_until_every_stream_has_ended(&self) -> EveryStreamEndedDuringTheWait {
        let streams_seen_during_the_wait: Mutex<Vec<Arc<LoadedStreamInThisRuntime>>> =
            Mutex::new(Vec::new());
        self.block_until(&|| {
            let mut streams_seen = streams_seen_during_the_wait.lock();
            for stream in self
                .engine_resources_shared_by_every_stream
                .every_loaded_stream()
            {
                if !streams_seen.iter().any(|seen| Arc::ptr_eq(seen, &stream)) {
                    streams_seen.push(stream);
                }
            }
            streams_seen.iter().all(|stream| stream.has_ended())
        });
        let streams_seen_during_the_wait = streams_seen_during_the_wait.into_inner();
        let how_the_first_failed_stream_ended = streams_seen_during_the_wait
            .iter()
            .map(|stream| stream.how_this_stream_ended_as_a_waiter_reports_it())
            .find(Result::is_err)
            .unwrap_or(Ok(()));
        EveryStreamEndedDuringTheWait {
            streams_seen_during_the_wait,
            how_the_first_failed_stream_ended,
        }
    }

    /// Block until the machine's shutdown is requested — a signal, or the
    /// request the Rust SDK makes — with zero, one or many streams loaded.
    pub fn wait_until_a_machine_shutdown_is_requested(&self) {
        self.block_until(&crate::core::runtime::is_the_machines_shutdown_requested);
    }

    /// Block, as the waits above do, until `self` is the one reference left to
    /// this engine or `budget` has passed; whether it is.
    pub fn wait_until_this_reference_alone_holds_the_engine(
        self: &Arc<Self>,
        budget: Duration,
    ) -> bool {
        let deadline = Instant::now() + budget;
        self.block_until(&|| Arc::strong_count(self) == 1 || Instant::now() >= deadline);
        Arc::strong_count(self) == 1
    }

    /// Poll until `has_ended` holds — driving the window event pump where the
    /// platform needs the first thread to, and, while this engine owns the
    /// machine's shutdown signals, walking every loaded stream to the
    /// machine's shutdown level on the way.
    fn block_until(&self, has_ended: &dyn Fn() -> bool) {
        let observe = || {
            if self
                .engine_resources_shared_by_every_stream
                .owns_the_machine_shutdown_signals
                .load(Ordering::SeqCst)
            {
                self.walk_every_loaded_stream_to_the_machines_shutdown_level();
            }
            if has_ended() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };

        // The drive lasts the whole wait, so the pump keeps answering a
        // stream's teardown — run off this thread — until the stream ends.
        #[cfg(target_os = "macos")]
        {
            use crate::core::window_event_pump::{
                WindowEventPumpDriveOnTheFirstThreadOutcome,
                drive_the_window_event_pump_on_the_first_thread_until,
            };

            let drive_outcome = drive_the_window_event_pump_on_the_first_thread_until(
                crate::core::runtime::RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL,
                &observe,
                || {
                    if let Err(e) = self
                        .shut_every_loaded_stream_down_and_wait_until_each_has_ended(
                            "the application is terminating",
                        )
                    {
                        tracing::error!(
                            "a stream failed to stop as the application terminated: {e}"
                        );
                    }
                },
            );
            if drive_outcome
                == WindowEventPumpDriveOnTheFirstThreadOutcome::DrivenUntilTheObservationBroke
            {
                return;
            }
        }

        while observe().is_continue() {
            std::thread::sleep(
                crate::core::runtime::RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL,
            );
        }
    }

    /// Move every loaded stream to the machine's shutdown level at once:
    /// graceful asks each for its shutdown, forced forces each.
    fn walk_every_loaded_stream_to_the_machines_shutdown_level(&self) {
        let machines_level = crate::core::runtime::the_machines_shutdown_escalation();
        if machines_level < RuntimeShutdownEscalation::Graceful {
            return;
        }
        for stream in self
            .engine_resources_shared_by_every_stream
            .every_loaded_stream()
        {
            stream.ask_for_this_streams_shutdown("the machine is shutting down every stream");
            if machines_level >= RuntimeShutdownEscalation::Forced {
                stream.force_this_streams_shutdown("the machine's shutdown was forced");
            }
        }
    }

    /// Ask every loaded stream for its shutdown at once — each on its own
    /// thread, under its own watchdog — and wait until each has ended,
    /// reporting the first that ended with a failure.
    fn shut_every_loaded_stream_down_and_wait_until_each_has_ended(
        &self,
        reason: &str,
    ) -> Result<()> {
        let every_stream = self
            .engine_resources_shared_by_every_stream
            .every_loaded_stream();
        for stream in &every_stream {
            stream.ask_for_this_streams_shutdown(reason);
        }
        let mut first_failed_end = Ok(());
        for stream in &every_stream {
            request_a_streams_shutdown_and_wait_until_it_has_ended(stream, reason);
            if let Err(end_failure) = stream.how_this_stream_ended_as_a_waiter_reports_it()
                && first_failed_end.is_ok()
            {
                first_failed_end = Err(end_failure);
            }
        }
        first_failed_end
    }

    // =========================================================================
    // Engine shutdown
    // =========================================================================

    /// Shut every loaded stream down at once, each under its own watchdog,
    /// then stop the surface-sharing service. Idempotent; dropping the
    /// `Runner` runs it. A load after it is refused.
    pub fn shut_down(&self) -> Result<()> {
        let engine = &self.engine_resources_shared_by_every_stream;
        if engine.shut_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let first_failed_end = self.shut_every_loaded_stream_down_and_wait_until_each_has_ended(
            "the engine is shutting down",
        );
        // The streams stopped off this thread, so the windows they handed back
        // are released here when this is the first thread.
        #[cfg(target_os = "macos")]
        crate::core::window_event_pump::release_the_windows_handed_back_while_the_event_pump_was_not_driven();

        // The Mach service thread answers helpers' reports against the
        // timeline pairs, so it is joined before the pairs go, and both go
        // while the device that made their semaphores still exists.
        #[cfg(target_os = "macos")]
        {
            crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
                "the surface-sharing service",
            );
            if let Some(mut mach_surface_share_service) =
                engine.mach_surface_share_service.lock().take()
            {
                mach_surface_share_service.stop();
            }
            engine.surface_share_cross_process_timeline_pairs.clear();
        }
        // Stopped here rather than left to its `Drop`, so the socket file is
        // gone before this returns.
        #[cfg(target_os = "linux")]
        {
            crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
                "the surface-sharing service",
            );
            if let Some(mut surface_service) = engine.surface_service.lock().take() {
                surface_service.stop();
                tracing::debug!(
                    "[shut_down] Runtime-internal surface-sharing service stopped at {}",
                    engine.surface_socket_path.display()
                );
            }
        }
        tracing::info!("[shut_down] The engine is shut down");
        first_failed_end
    }

    // =========================================================================
    // Loading
    // =========================================================================

    fn build_the_stream_a_graph_load_names(
        &self,
        graph: &GraphSnapshot,
        load_options: OptionsForLoadingOneStream,
        holding: LoadedStreamHolding,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let requested_stream_name = load_options
            .stream_name
            .as_deref()
            .or(graph.stream.as_deref());
        self.refuse_a_load_while_the_machine_shuts_down(requested_stream_name)?;
        let stream_name =
            the_cast_name_of_the_stream_a_load_names(requested_stream_name, "the graph")?;
        if graph.nodes.is_empty() {
            return Err(Error::GraphError(format!(
                "the stream `{stream_name}` holds no node — a stream whose function adds \
                 nothing compiles to an empty graph, and there is nothing to run. Add a node \
                 with `stream_builder.add(...)`"
            )));
        }
        self.build_a_stream(
            stream_name,
            load_options,
            super::ENGINE_TEARDOWN_WATCHDOG_BUDGET,
            holding,
        )
    }

    fn build_a_stream(
        &self,
        stream_name: String,
        load_options: OptionsForLoadingOneStream,
        teardown_watchdog_budget: Duration,
        holding: LoadedStreamHolding,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.refuse_a_load_once_the_engine_is_shut_down(&stream_name)?;
        self.refuse_a_stream_name_already_loaded(&stream_name)?;
        let project_directory = load_options.resolved_project_directory(&stream_name)?;
        LoadedStreamInThisRuntime::new(
            Arc::clone(&self.engine_resources_shared_by_every_stream),
            stream_name,
            project_directory,
            load_options.stream_environment,
            teardown_watchdog_budget,
            holding,
        )
    }

    /// Load `graph` into `stream`, which is not in the table; a refused load
    /// tears the stream down through its own stop.
    fn load_the_graph_into_the_stream(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
        graph: &GraphSnapshot,
    ) -> Result<()> {
        let lend_directory = self
            .engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get();
        if let Err(load_refusal) =
            stream.load_graph_snapshot_into_this_stream(graph, lend_directory)
        {
            tear_down_a_stream_that_never_entered_the_table(stream);
            return Err(load_refusal);
        }
        Ok(())
    }

    fn insert_a_complete_stream(
        &self,
        stream: Arc<LoadedStreamInThisRuntime>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let mut streams = self
            .engine_resources_shared_by_every_stream
            .streams_loaded_in_this_runtime
            .lock();
        if let Err(refusal) = self.refuse_a_load_once_the_engine_is_shut_down(stream.stream_name())
        {
            drop(streams);
            tear_down_a_stream_that_never_entered_the_table(&stream);
            return Err(refusal);
        }
        if let Some(first) = streams.get(stream.stream_name()) {
            let refusal = a_stream_name_already_loaded_refusal(
                first,
                LOAD_UNDER_ANOTHER_NAME_IN_CODE_OR_ON_THE_COMMAND_LINE,
            );
            drop(streams);
            tear_down_a_stream_that_never_entered_the_table(&stream);
            return Err(refusal);
        }
        streams.insert(stream.stream_name().to_string(), Arc::clone(&stream));
        Ok(stream)
    }

    fn refuse_a_stream_name_already_loaded(&self, stream_name: &str) -> Result<()> {
        match self
            .engine_resources_shared_by_every_stream
            .streams_loaded_in_this_runtime
            .lock()
            .get(stream_name)
        {
            Some(first) => Err(a_stream_name_already_loaded_refusal(
                first,
                LOAD_UNDER_ANOTHER_NAME_IN_CODE_OR_ON_THE_COMMAND_LINE,
            )),
            None => Ok(()),
        }
    }

    fn refuse_a_load_once_the_engine_is_shut_down(&self, stream_name: &str) -> Result<()> {
        if !self
            .engine_resources_shared_by_every_stream
            .shut_down
            .load(Ordering::SeqCst)
        {
            return Ok(());
        }
        Err(Error::Runtime(format!(
            "the stream `{stream_name}` was not loaded: this runtime's engine has shut down; \
             build a new one to load it"
        )))
    }

    fn refuse_a_load_while_the_machine_shuts_down(
        &self,
        requested_stream_name: Option<&str>,
    ) -> Result<()> {
        if !crate::core::runtime::is_the_machines_shutdown_requested() {
            return Ok(());
        }
        Err(Error::Runtime(format!(
            "the stream `{}` was not loaded: the machine is shutting every stream down",
            requested_stream_name.unwrap_or("(unnamed)")
        )))
    }

    pub(super) fn loaded_stream_names_listed_for_a_refusal(&self) -> String {
        names_listed_for_a_refusal(self.names_of_the_loaded_streams(), "none")
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        if let Err(shut_down_failure) = self.shut_down() {
            tracing::error!("the engine shut down with a failure: {shut_down_failure}");
        }
    }
}

/// The URL-safe cast of the stream name a load names, refused when it casts
/// to nothing and when there is none.
pub(super) fn the_cast_name_of_the_stream_a_load_names(
    requested_stream_name: Option<&str>,
    what_is_loaded: &str,
) -> Result<String> {
    let Some(requested_stream_name) = requested_stream_name else {
        return Err(Error::GraphError(format!(
            "{what_is_loaded} names no stream; give it a name — \
             `OptionsForLoadingOneStream::named`, or `--name` on the `tatolab` command line"
        )));
    };
    cast_exposed_name_to_url_safe(requested_stream_name)
        .map(|cast| cast.into_owned())
        .map_err(|casts_to_nothing| {
            Error::GraphError(format!(
                "cannot load the stream `{requested_stream_name}`: {casts_to_nothing}"
            ))
        })
}

/// How a load refused for a name already loaded is told to go on, in code or
/// on the command line.
const LOAD_UNDER_ANOTHER_NAME_IN_CODE_OR_ON_THE_COMMAND_LINE: &str = "load this one under another \
     name — `OptionsForLoadingOneStream::named`, or `--name` on the `tatolab` command line";

/// The refusal of a load under the name `first` is loaded as, naming its
/// project and ending with `how_to_load_this_one`.
pub(super) fn a_stream_name_already_loaded_refusal(
    first: &LoadedStreamInThisRuntime,
    how_to_load_this_one: &str,
) -> Error {
    Error::GraphError(format!(
        "a stream named `{}` is already loaded in this runtime, from `{}`; {how_to_load_this_one}",
        first.stream_name(),
        first.project_directory().display()
    ))
}

/// Walk a stream a load is still building — in no table the machine's walk
/// reaches — to the machine's shutdown level, and interrupt its describes.
fn walk_a_stream_being_loaded_to_the_machines_shutdown_level(stream: &LoadedStreamInThisRuntime) {
    let machines_level = crate::core::runtime::the_machines_shutdown_escalation();
    stream
        .this_streams_shutdown_escalation()
        .raise_to_graceful();
    if machines_level >= RuntimeShutdownEscalation::Forced {
        stream.this_streams_shutdown_escalation().raise_to_forced();
    }
    stream.interrupt_every_processor_interpreter_describe();
}

/// Ask `stream` for its shutdown and block until it has ended. Once its
/// shutdown thread has started, the stream's watchdog bounds the wait; until
/// then each poll asks again, retrying the thread's spawn. A machine shutdown
/// forced meanwhile forces this stream too, whichever thread is waiting on the
/// machine.
pub(super) fn request_a_streams_shutdown_and_wait_until_it_has_ended(
    stream: &LoadedStreamInThisRuntime,
    reason: &str,
) {
    loop {
        stream.ask_for_this_streams_shutdown(reason);
        if crate::core::runtime::the_machines_shutdown_escalation()
            >= RuntimeShutdownEscalation::Forced
        {
            stream.force_this_streams_shutdown("the machine's shutdown was forced");
        }
        if stream.wait_for_this_streams_end_within(
            crate::core::runtime::RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL,
        ) {
            return;
        }
    }
}

/// Marks an engine the owner of the machine's shutdown signals until dropped.
struct TheMachineShutdownSignalsOwnedByOneEngine<'engine>(
    &'engine EngineResourcesSharedByEveryStream,
);

impl<'engine> TheMachineShutdownSignalsOwnedByOneEngine<'engine> {
    fn mark(engine: &'engine EngineResourcesSharedByEveryStream) -> Self {
        engine
            .owns_the_machine_shutdown_signals
            .store(true, Ordering::SeqCst);
        Self(engine)
    }
}

impl Drop for TheMachineShutdownSignalsOwnedByOneEngine<'_> {
    fn drop(&mut self) {
        self.0
            .owns_the_machine_shutdown_signals
            .store(false, Ordering::SeqCst);
    }
}

/// Stop a stream a refused load built, which never entered the table, and
/// close its JSONL log.
fn tear_down_a_stream_that_never_entered_the_table(stream: &Arc<LoadedStreamInThisRuntime>) {
    if let Err(teardown_failure) = stream.stop() {
        stream.log_route().run_entered(|| {
            tracing::error!(
                "the stream `{}`, refused before it was loaded, failed to tear down: \
                 {teardown_failure}",
                stream.stream_name()
            )
        });
    }
    stream.log_route().close_the_jsonl_log_file();
}

/// Own SIGINT, SIGTERM and SIGHUP until the returned value drops.
fn take_shutdown_signal_ownership() -> Result<ScopedShutdownSignalOwnership> {
    ScopedShutdownSignalOwnership::take_until_dropped().map_err(|ownership_failure| {
        Error::Configuration(format!(
            "Failed to own shutdown signals: {}",
            ownership_failure
        ))
    })
}

/// Compute the per-runtime surface-sharing socket path, refuse to start if
/// another live runtime is already bound there, clean up an orphan socket
/// from a prior crashed runtime, and bring the listener up.
#[cfg(target_os = "linux")]
fn bring_up_surface_service(
    runtime_directory: &StreamlibRuntimeDirectory,
    runtime_id: &RuntimeUniqueId,
) -> Result<(
    crate::linux::surface_share::UnixSocketSurfaceService,
    std::path::PathBuf,
    Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
    Arc<dyn crate::core::context::SurfaceShareRegistrationsByRuntime + Send + Sync>,
)> {
    use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};

    use crate::core::unix_socket_path_cleared_for_bind::{
        UnixSocketPathClearedForBind, UnixSocketPathRefusedForBind, clear_unix_socket_path_for_bind,
    };

    let socket_path = runtime_directory.surface_share_socket_path(runtime_id);

    let cleared = clear_unix_socket_path_for_bind(&socket_path).map_err(|refusal| match refusal {
        UnixSocketPathRefusedForBind::HeldByALiveProcess { .. } => Error::Runtime(format!(
            "Surface-sharing socket: {refusal}; each runtime needs a unique runtime_id, so check \
             for a duplicate STREAMLIB_RUNTIME_ID or another runtime in the same session"
        )),
        _ => Error::Runtime(format!("Surface-sharing socket: {refusal}")),
    })?;
    if cleared == UnixSocketPathClearedForBind::StaleSocketFileRemoved {
        tracing::warn!(
            "[new] Removed stale surface-sharing socket left by prior runtime: {}",
            socket_path.display()
        );
    }

    let state = SurfaceShareState::new();
    let check_out_leases = Arc::clone(state.check_out_leases());
    let registrations_by_owner: Arc<
        dyn crate::core::context::SurfaceShareRegistrationsByRuntime + Send + Sync,
    > = Arc::new(state.clone());
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
        service,
        socket_path,
        check_out_leases,
        registrations_by_owner,
    ))
}

/// Register the per-runtime surface-sharing Mach service, refusing to start
/// if another live runtime already holds this id's name. A crashed runtime's
/// name went with its process, so there is nothing stale to clean up.
#[cfg(target_os = "macos")]
fn bring_up_mach_surface_share_service(
    runtime_id: &RuntimeUniqueId,
) -> Result<(
    crate::apple::surface_share::MachSurfaceShareService,
    crate::apple::surface_share::MachSurfaceShareServiceRendezvous,
    Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
    Arc<crate::apple::surface_share::CrossProcessTimelinePairsBySurface>,
    Arc<dyn crate::core::context::SurfaceShareRegistrationsByRuntime + Send + Sync>,
)> {
    use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};

    let service_name = MachSurfaceShareService::service_name_for_runtime(runtime_id.as_str());
    let state = IOSurfaceShareState::new();
    let check_out_leases = Arc::clone(state.check_out_leases());
    let cross_process_timeline_pairs = Arc::clone(state.cross_process_timeline_pairs());
    let registrations_by_owner: Arc<
        dyn crate::core::context::SurfaceShareRegistrationsByRuntime + Send + Sync,
    > = Arc::new(state.clone());
    let mut service = MachSurfaceShareService::new(state, service_name.clone());
    service.start().map_err(|start_failure| {
        if start_failure.kind() == std::io::ErrorKind::AddrInUse {
            Error::Runtime(format!(
                "Surface-sharing Mach service '{service_name}' is already registered by a live \
                 process. Each Runner requires a unique runtime_id; check for a duplicate \
                 STREAMLIB_RUNTIME_ID env var or another runtime in the same session."
            ))
        } else {
            Error::Runtime(format!(
                "Failed to start runtime-internal surface-sharing service '{service_name}': \
                 {start_failure}"
            ))
        }
    })?;
    let rendezvous = service.rendezvous();

    Ok((
        service,
        rendezvous,
        check_out_leases,
        cross_process_timeline_pairs,
        registrations_by_owner,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::{HowALoadedStreamEnded, RuntimeStatus};
    use serial_test::serial;

    /// An empty stream named `stream_name` loaded into `runner`, its project in
    /// `project_directory`.
    fn an_empty_stream_loaded_into(
        runner: &Runner,
        project_directory: &std::path::Path,
        stream_name: &str,
    ) -> Arc<LoadedStreamInThisRuntime> {
        runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory)
                    .named(stream_name),
            )
            .expect("an empty stream loads")
    }

    /// A project directory the test owns, removed with everything its streams
    /// wrote when the test drops it.
    fn a_project_directory_this_test_owns() -> tempfile::TempDir {
        crate::core::test_support::a_temporary_directory_at_owner_only_mode()
            .expect("a temporary project directory")
    }

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

    /// Nothing refuses a runtime name another live runtime already carries:
    /// two runners named alike both construct, both stay up together, and each
    /// reports the name it was given wherever the name is read.
    #[test]
    #[serial]
    fn two_runners_given_one_runtime_name_both_construct() {
        let shared_runtime_name = "one-name-two-runners";

        let project_directory = a_project_directory_this_test_owns();
        let first = Runner::new_with_construction_options(RunnerConstructionOptions {
            runtime_name: Some(shared_runtime_name.to_string()),
            ..RunnerConstructionOptions::default()
        })
        .expect("the first runner constructs");
        let second = Runner::new_with_construction_options(RunnerConstructionOptions {
            runtime_name: Some(shared_runtime_name.to_string()),
            ..RunnerConstructionOptions::default()
        })
        .expect("a second runner given the same name constructs beside the first");

        assert_ne!(first.runtime_id(), second.runtime_id());
        for runner in [&first, &second] {
            assert_eq!(runner.runtime_name().as_str(), shared_runtime_name);
            let stream = an_empty_stream_loaded_into(runner, project_directory.path(), "main");
            assert_eq!(
                <LoadedStreamInThisRuntime as crate::core::runtime::RuntimeOperations>::this_runtimes_name(
                    &stream
                ),
                shared_runtime_name
            );
            assert_eq!(
                stream.to_json().expect("the graph serializes")["runtime_name"],
                shared_runtime_name
            );
        }
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
            runtime
                .engine_resources_shared_by_every_stream
                .tokio_runtime_variant,
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
            runtime
                .engine_resources_shared_by_every_stream
                .tokio_runtime_variant,
            TokioRuntimeVariant::ExternalTokioHandle(_)
        ));
    }

    /// Fail-without-fix: the request wrote the configuration onto the node
    /// before the processor was asked, so one it refused at commit stayed in
    /// `graph` as though it had been taken.
    #[test]
    #[serial]
    fn a_requested_configuration_waits_for_the_commit_rather_than_landing_on_the_graph_node() {
        use crate::core::compiler::PendingOperation;
        use crate::core::processors::ProcessorSpec;
        use crate::core::test_support::{MockOutputOnlyProcessor, ensure_test_mocks_registered};

        ensure_test_mocks_registered();
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
        let processor_id = stream
            .add_processor(ProcessorSpec::new(
                MockOutputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the mock is added");

        stream
            .update_processor_config(&processor_id, serde_json::json!({"gain": 3}))
            .expect("the update is queued");

        let config_on_the_node = stream.compiler.scope(|graph, _tx| {
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
            stream.compiler.logged_pending_operations().iter().any(|op| matches!(
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
            let project_directory = a_project_directory_this_test_owns();
            let runner = Runner::new().unwrap();
            let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
            // Sync methods should work (use spawn + channel internally)
            let json = stream.to_json().unwrap();
            assert!(json["nodes"].is_array());
        });
    }

    // =========================================================================
    // Per-runtime surface-sharing service (#428)
    // =========================================================================

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

    #[cfg(target_os = "macos")]
    mod runtime_internal_mach_surface_share {
        use super::*;
        use streamlib_surface_client::SurfaceShareMachServiceConnection;

        fn a_unique_pinned_runtime_id() -> String {
            format!("Rmachtest{}", uuid::Uuid::new_v4().simple())
        }

        #[test]
        #[serial]
        fn a_runtime_registers_a_surface_share_mach_service_this_process_can_use() {
            let runtime = Runner::new().expect("runtime should construct");
            let service_name = runtime
                .surface_share_mach_service_rendezvous()
                .service_name()
                .to_string();
            assert!(
                service_name.ends_with(runtime.runtime_id().as_str()),
                "the service name {service_name} is keyed by the runtime id"
            );
            let connection =
                SurfaceShareMachServiceConnection::connect(&service_name, Duration::from_secs(5))
                    .expect("this process connects to its own runtime's service");
            let (answer, ports) = connection
                .send_request_with_ports(
                    &serde_json::json!({"op": "lookup", "surface_id": "no-such"}),
                    Vec::new(),
                )
                .expect("round-trip");
            assert!(answer.get("error").is_some());
            assert!(ports.is_empty());
        }

        #[test]
        #[serial]
        fn a_second_runtime_pinned_to_a_live_runtimes_id_is_refused_and_a_dropped_one_frees_it() {
            let pinned_id = a_unique_pinned_runtime_id();
            with_environment_variables_set(
                &[("STREAMLIB_RUNTIME_ID", std::ffi::OsStr::new(&pinned_id))],
                || {
                    let first = Runner::new().expect("first runtime");
                    let refusal = Runner::new()
                        .err()
                        .expect("a second runtime under a live id is refused")
                        .to_string();
                    assert!(
                        refusal.contains("already registered by a live process"),
                        "{refusal}"
                    );
                    drop(first);
                    Runner::new().expect("the id is free once its runtime is gone");
                },
            );
        }
    }

    #[cfg(target_os = "linux")]
    mod runtime_internal_surface_share {
        use super::*;
        use std::os::unix::net::UnixStream;
        use streamlib_surface_client::{MAX_DMA_BUF_PLANES, send_request_with_fds};

        /// Replace XDG_RUNTIME_DIR with a fresh tempdir for the duration of the
        /// closure. Tests using this must be `#[serial]` so no other runtime
        /// construct reads the mutated env.
        fn with_isolated_xdg_runtime_dir<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
            let tmp = crate::core::test_support::a_temporary_directory_at_owner_only_mode()
                .expect("tempdir");
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
                streamlib_runtime_client_contract::streamlib_runtime_directory::current_process_uid(
                )
            ));
            assert!(
                runtime.surface_socket_path().starts_with(&fallback),
                "socket {} should be under {}",
                runtime.surface_socket_path().display(),
                fallback.display()
            );
            let iceoryx2_config = runtime.iceoryx2_node().config();
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
            let base = crate::core::test_support::at_owner_only_mode(
                tempfile::Builder::new()
                    .prefix("sl")
                    .tempdir_in("/tmp")
                    .expect("tempdir under /tmp"),
            )
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
            .surface_share_socket_path(&pinned_id);
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
            assert!(
                refusal.contains("duplicate STREAMLIB_RUNTIME_ID"),
                "the surface-sharing socket is keyed by runtime id: {refusal}"
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

                streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
                    &xdg.join("streamlib"),
                    streamlib_runtime_client_contract::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
                )
                .expect("create runtime directory");
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

    /// A stream's shutdown thread stops a stream an embedding host already
    /// stopped, so the second `stop()` must be a no-op rather than a second
    /// full teardown that republishes the transition to every subscriber.
    ///
    /// Mental-revert: removing the already-stopped early return in `stop()`
    /// makes the second call publish `RuntimeStopping`/`RuntimeStopped` again
    /// and fails the counts below.
    #[test]
    #[serial_test::serial]
    fn stopping_an_already_stopped_stream_is_a_no_op() {
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
                    Event::OfALoadedStream {
                        event: RuntimeEvent::RuntimeStopping,
                        ..
                    } => counts.stopping += 1,
                    Event::OfALoadedStream {
                        event: RuntimeEvent::RuntimeStopped,
                        ..
                    } => counts.stopped += 1,
                    _ => {}
                }
                Ok(())
            }
        }

        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("a runner boots without a graph");
        let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
        let counts = Arc::new(Mutex::new(StopTransitionCounter::default()));
        let listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(CountingListener(Arc::clone(&counts))));
        PUBSUB
            .subscribe(
                &topics::loaded_stream(stream.loaded_stream_identity()),
                Arc::clone(&listener),
            )
            .expect("subscribe establishes the subscriber");

        stream.stop().expect("the first stop succeeds");
        stream.stop().expect("the second stop succeeds");

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

    // =========================================================================
    // Two streams loaded in one engine
    // =========================================================================

    /// How long a test waits for a stream to end before calling it hung.
    const A_STREAM_ENDS_WITHIN: Duration = Duration::from_secs(10);

    /// Records every event published on one stream's topic.
    struct EveryEventOnOneStreamsTopic(Arc<Mutex<Vec<crate::core::pubsub::Event>>>);

    impl crate::core::pubsub::EventListener for EveryEventOnOneStreamsTopic {
        fn on_event(&mut self, event: &crate::core::pubsub::Event) -> Result<()> {
            self.0.lock().push(event.clone());
            Ok(())
        }
    }

    /// Subscribe a recorder to `stream`'s topic, returning what it records and
    /// the listener that keeps the subscription alive.
    fn record_every_event_on_the_topic_of(
        stream: &LoadedStreamInThisRuntime,
    ) -> (
        Arc<Mutex<Vec<crate::core::pubsub::Event>>>,
        Arc<Mutex<dyn crate::core::pubsub::EventListener>>,
    ) {
        use crate::core::pubsub::{PUBSUB, topics};

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let listener: Arc<Mutex<dyn crate::core::pubsub::EventListener>> = Arc::new(Mutex::new(
            EveryEventOnOneStreamsTopic(Arc::clone(&recorded)),
        ));
        PUBSUB
            .subscribe(
                &topics::loaded_stream(stream.loaded_stream_identity()),
                Arc::clone(&listener),
            )
            .expect("subscribe establishes the subscriber");
        (recorded, listener)
    }

    /// Publish a sentinel on `stream`'s topic and wait for `recorded` to hold
    /// it, returning everything recorded before it. A listener's queue is FIFO,
    /// so the sentinel arriving means every event published before it that
    /// reached this topic already has.
    fn recorded_before_a_sentinel_on_the_topic_of(
        stream: &LoadedStreamInThisRuntime,
        recorded: &Mutex<Vec<crate::core::pubsub::Event>>,
    ) -> Vec<crate::core::pubsub::Event> {
        use crate::core::pubsub::{Event, PUBSUB, topics};

        let topic = topics::loaded_stream(stream.loaded_stream_identity());
        let sentinel = Event::custom(&topic, serde_json::json!({"sentinel": true}));
        PUBSUB.publish(&topic, &sentinel);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let recorded = recorded.lock();
                if let Some(sentinel_at) = recorded.iter().position(|event| *event == sentinel) {
                    return recorded[..sentinel_at].to_vec();
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the sentinel published on the stream's topic never arrived"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// One stream's shutdown ends that stream and takes it out of the table,
    /// and leaves the other loaded, unrequested and as it was.
    #[test]
    #[serial]
    fn one_streams_shutdown_ends_it_and_leaves_the_other_loaded() {
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let shut_down = an_empty_stream_loaded_into(&runner, project_directory.path(), "shut-down");
        let left_running =
            an_empty_stream_loaded_into(&runner, project_directory.path(), "left-running");
        let (recorded_by_the_other, _listener) = record_every_event_on_the_topic_of(&left_running);

        shut_down.ask_for_this_streams_shutdown("the test shuts one stream down");
        assert!(
            shut_down.wait_for_this_streams_end_within(A_STREAM_ENDS_WITHIN),
            "the stream whose shutdown was requested never ended"
        );

        assert_eq!(
            shut_down.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::Stopped)
        );
        assert_eq!(shut_down.status(), RuntimeStatus::Stopped);
        assert_eq!(runner.names_of_the_loaded_streams(), ["left-running"]);
        assert!(!left_running.has_ended());
        assert_eq!(left_running.status(), RuntimeStatus::Initial);
        assert_eq!(
            left_running.this_streams_shutdown_escalation().escalation(),
            RuntimeShutdownEscalation::NotRequested
        );
        assert!(
            recorded_before_a_sentinel_on_the_topic_of(&left_running, &recorded_by_the_other)
                .is_empty(),
            "the other stream heard the shut-down stream's events"
        );

        shut_down.ask_for_this_streams_shutdown("a second request");
        assert_eq!(
            shut_down.this_streams_shutdown_escalation().escalation(),
            RuntimeShutdownEscalation::Graceful,
            "a second request escalates nothing further"
        );
    }

    /// A graph change in one stream is logged on its own compiler and heard
    /// by its own listener; the other stream's transaction, graph and topic
    /// see nothing.
    #[test]
    #[serial]
    fn one_streams_graph_change_reaches_only_its_own_compiler_and_topic() {
        use crate::core::compiler::PendingOperation;
        use crate::core::processors::ProcessorSpec;
        use crate::core::pubsub::{Event, RuntimeEvent};
        use crate::core::test_support::{MockOutputOnlyProcessor, ensure_test_mocks_registered};

        ensure_test_mocks_registered();
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let changed = an_empty_stream_loaded_into(&runner, project_directory.path(), "changed");
        let untouched = an_empty_stream_loaded_into(&runner, project_directory.path(), "untouched");
        let (recorded_by_the_changed, _changed_listener) =
            record_every_event_on_the_topic_of(&changed);
        let (recorded_by_the_untouched, _untouched_listener) =
            record_every_event_on_the_topic_of(&untouched);

        let processor_id = changed
            .add_processor(ProcessorSpec::new(
                MockOutputOnlyProcessor::processor_class_import_path(),
                serde_json::Value::Null,
            ))
            .expect("the mock is added");

        assert!(
            changed
                .compiler
                .logged_pending_operations()
                .iter()
                .any(|op| matches!(op, PendingOperation::AddProcessor(id) if *id == processor_id)),
            "the change is logged on its own stream's compiler"
        );
        assert!(
            untouched.compiler.logged_pending_operations().is_empty(),
            "another stream's change reached this stream's compiler"
        );
        assert_eq!(untouched.to_json().unwrap()["nodes"], serde_json::json!([]));
        assert!(
            recorded_before_a_sentinel_on_the_topic_of(&changed, &recorded_by_the_changed)
                .iter()
                .any(|event| matches!(
                    event,
                    Event::OfALoadedStream {
                        event: RuntimeEvent::GraphDidChange,
                        stream,
                    } if stream.stream_name == "changed"
                )),
            "the changed stream's listener never heard its graph change"
        );
        assert!(
            recorded_before_a_sentinel_on_the_topic_of(&untouched, &recorded_by_the_untouched)
                .is_empty(),
            "the untouched stream's listener heard another stream's graph change"
        );
    }

    /// A stream whose teardown outlives its watchdog is unloaded, its helper
    /// process groups killed and no other stream's, and every other stream is
    /// left alive. The budget is injected so the test never waits fifteen
    /// seconds.
    #[test]
    #[serial]
    fn the_watchdog_on_one_streams_teardown_unloads_it_and_leaves_the_other_alive() {
        use crate::core::runtime::{
            deregister_a_helper_process_group, register_a_helper_process_group,
        };
        use crate::core::test_support::a_process_parked_in_a_process_group_of_its_own;

        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let hung = runner
            .load_an_empty_stream_whose_teardown_watchdog_fires_after(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named("hung"),
                Duration::from_millis(300),
            )
            .expect("the stream loads");
        let alive = an_empty_stream_loaded_into(&runner, project_directory.path(), "alive");
        let mut hung_streams_helper = a_process_parked_in_a_process_group_of_its_own();
        let mut alive_streams_helper = a_process_parked_in_a_process_group_of_its_own();
        assert!(register_a_helper_process_group(
            hung_streams_helper.id() as i32,
            hung.stream_tag()
        ));
        assert!(register_a_helper_process_group(
            alive_streams_helper.id() as i32,
            alive.stream_tag()
        ));

        // The stop takes this lock after it marks the stream stopping, so
        // holding it hangs the teardown where no budget of its own reaches.
        let the_hung_teardown_waits_on = hung.runtime_context.lock();
        hung.ask_for_this_streams_shutdown("the test hangs this stream's teardown");
        let ended = hung.wait_for_this_streams_end_within(A_STREAM_ENDS_WITHIN);
        let how_it_ended = hung.how_this_stream_ended();
        let names_once_it_ended = runner.names_of_the_loaded_streams();
        let hung_helper_exited = (0..500).any(|_| {
            if hung_streams_helper.try_wait().ok().flatten().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
            false
        });
        let alive_helper_still_running = alive_streams_helper.try_wait().ok().flatten().is_none();
        drop(the_hung_teardown_waits_on);
        deregister_a_helper_process_group(hung_streams_helper.id() as i32);
        deregister_a_helper_process_group(alive_streams_helper.id() as i32);
        let _ = alive_streams_helper.kill();
        let _ = alive_streams_helper.wait();
        let _ = hung_streams_helper.wait();

        assert!(ended, "the watchdog never ended the hung stream");
        assert!(
            matches!(
                how_it_ended,
                Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog { .. })
            ),
            "the hung stream ended as {how_it_ended:?}"
        );
        assert_eq!(names_once_it_ended, ["alive"]);
        assert!(
            hung_helper_exited,
            "the watchdog left the hung stream's helper process group running"
        );
        assert!(
            alive_helper_still_running,
            "the watchdog killed another stream's helper process group"
        );
        assert!(!alive.has_ended());
        assert_eq!(alive.status(), RuntimeStatus::Initial);
        assert!(!alive.this_streams_shutdown_escalation().is_requested());

        let released_by = std::time::Instant::now() + A_STREAM_ENDS_WITHIN;
        while hung.status() != RuntimeStatus::Stopped {
            assert!(
                std::time::Instant::now() < released_by,
                "the released teardown never finished"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(
                hung.how_this_stream_ended(),
                Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog { .. })
            ),
            "a teardown that finished after its watchdog fired rewrote how the stream ended"
        );
    }

    /// A call naming a stream that is not loaded is refused naming the loaded
    /// streams; a call's name is cast before it is looked up.
    #[test]
    #[serial]
    fn a_call_naming_a_stream_not_loaded_is_refused_naming_the_loaded_streams() {
        use crate::core::runtime::OperationsOnTheStreamsLoadedInThisRuntime;

        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let camera = an_empty_stream_loaded_into(&runner, project_directory.path(), "camera");
        an_empty_stream_loaded_into(&runner, project_directory.path(), "microphone");

        for refusal in [
            runner
                .runtime_operations_of_the_stream_a_call_names("display")
                .err()
                .expect("a stream not loaded is refused"),
            runner
                .runtime_operations_of_the_stream_a_call_names("")
                .err()
                .expect("a name that casts to nothing names no loaded stream"),
            runner
                .runtime_operations_of_the_stream_a_call_names("..")
                .err()
                .expect("a name that casts to nothing names no loaded stream"),
        ] {
            let refusal = refusal.to_string();
            assert!(
                refusal.contains("camera") && refusal.contains("microphone"),
                "the refusal must name the loaded streams: {refusal}"
            );
        }
        for name_that_casts_to_nothing in ["", ".."] {
            let refusal = runner
                .runtime_operations_of_the_stream_a_call_names(name_that_casts_to_nothing)
                .err()
                .expect("a name that casts to nothing is refused")
                .to_string();
            assert!(
                refusal.contains(&format!(
                    "cannot name a stream `{name_that_casts_to_nothing}`"
                )),
                "the refusal must say the name cannot name a stream: {refusal}"
            );
        }
        assert!(
            runner
                .node_catalog_of_the_stream_a_call_names("display")
                .is_err()
        );
        assert!(
            runner
                .node_types_described_in_the_interpreter_of_the_stream_a_call_names("display")
                .is_err()
        );
        assert!(Arc::ptr_eq(
            &runner
                .loaded_stream_named("Camera")
                .expect("a call's name is cast"),
            &camera
        ));
    }

    /// The machine's shutdown request, seen by the engine that owns the
    /// machine's signals, walks every loaded stream to its end.
    #[test]
    #[serial]
    fn the_machines_shutdown_walks_every_loaded_stream_to_its_end() {
        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");
        let second = an_empty_stream_loaded_into(&runner, project_directory.path(), "second");

        let (every_stream_ended, every_stream_has_ended) = std::sync::mpsc::channel();
        let waiting_runner = Arc::clone(&runner);
        std::thread::spawn(move || {
            let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
                waiting_runner.request_the_shutdown_of_every_loaded_stream(
                    "the test shuts the machine down",
                )?;
                waiting_runner
                    .wait_until_every_stream_has_ended()
                    .how_the_first_failed_stream_ended
            });
            let _ = every_stream_ended.send(outcome);
        });
        every_stream_has_ended
            .recv_timeout(A_STREAM_ENDS_WITHIN)
            .expect("the machine's shutdown never ended every stream")
            .expect("every stream ended cleanly");

        for stream in [&first, &second] {
            assert_eq!(
                stream.how_this_stream_ended(),
                Some(HowALoadedStreamEnded::Stopped)
            );
        }
        assert!(runner.names_of_the_loaded_streams().is_empty());
    }

    /// A stream loaded while the wait for every stream runs is waited for and
    /// returned with the others, so a teardown scanning what the wait returns
    /// cannot miss it.
    ///
    /// Only the wait's own walk to the machine's shutdown ends that stream, so
    /// the wait has polled while it was loaded.
    #[test]
    #[serial]
    fn a_stream_loaded_while_every_stream_is_awaited_is_among_the_streams_the_wait_returns() {
        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");

        let (the_wait_began, the_wait_has_begun) = std::sync::mpsc::channel();
        let (every_stream_ended, every_stream_has_ended) = std::sync::mpsc::channel();
        let waiting_runner = Arc::clone(&runner);
        std::thread::spawn(move || {
            let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
                let _ = the_wait_began.send(());
                Ok(waiting_runner.wait_until_every_stream_has_ended())
            });
            let _ = every_stream_ended.send(outcome);
        });
        the_wait_has_begun
            .recv_timeout(A_STREAM_ENDS_WITHIN)
            .expect("the waiting thread never began its wait");
        let loaded_during_the_wait = an_empty_stream_loaded_into(
            &runner,
            project_directory.path(),
            "loaded-during-the-wait",
        );
        runner.unload_stream("first").expect("first unloads");
        runner
            .request_the_shutdown_of_every_loaded_stream("the test shuts the machine down")
            .expect("the machine's shutdown is requested");

        let every_stream_ended_during_the_wait = every_stream_has_ended
            .recv_timeout(A_STREAM_ENDS_WITHIN)
            .expect("the wait never returned once every stream had ended")
            .expect("the waiting thread owned the machine's shutdown signals");
        every_stream_ended_during_the_wait
            .how_the_first_failed_stream_ended
            .expect("every stream ended cleanly");
        for stream in [&first, &loaded_during_the_wait] {
            assert!(
                every_stream_ended_during_the_wait
                    .streams_seen_during_the_wait
                    .iter()
                    .any(|seen| Arc::ptr_eq(seen, stream)),
                "the wait returned no `{}`",
                stream.stream_name()
            );
        }
        assert_eq!(
            loaded_during_the_wait.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::Stopped)
        );
    }

    /// The wait `tatolabd` blocks on returns only once the machine's shutdown
    /// is requested, and walks every loaded stream to it on the way.
    #[test]
    #[serial]
    fn the_wait_for_a_machine_shutdown_returns_once_one_is_requested_and_ends_every_stream() {
        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");

        let (wait_returned, the_wait_has_returned) = std::sync::mpsc::channel();
        let waiting_runner = Arc::clone(&runner);
        std::thread::spawn(move || {
            let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
                waiting_runner.wait_until_a_machine_shutdown_is_requested();
                Ok(())
            });
            let _ = wait_returned.send(outcome);
        });
        assert!(
            the_wait_has_returned
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "the wait returned before any machine shutdown was requested"
        );
        assert!(!stream.has_ended());

        crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
            "the test shuts the machine down",
        )
        .unwrap();

        the_wait_has_returned
            .recv_timeout(A_STREAM_ENDS_WITHIN)
            .expect("the wait never saw the machine's shutdown request")
            .expect("the wait owned the machine's shutdown signals");
        assert!(stream.wait_for_this_streams_end_within(A_STREAM_ENDS_WITHIN));
    }

    /// The wait for the last other reference returns once another holder
    /// hands the engine back.
    #[test]
    #[serial]
    fn the_wait_for_the_last_other_reference_returns_once_it_is_dropped() {
        let runner = Runner::new().expect("Runner::new");
        let held_by_another_holder = Arc::clone(&runner);
        let other_holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(held_by_another_holder);
        });

        assert!(runner.wait_until_this_reference_alone_holds_the_engine(A_STREAM_ENDS_WITHIN));
        other_holder
            .join()
            .expect("the other holder dropped its reference");
    }

    /// The wait for the last other reference gives up once its budget passes.
    #[test]
    #[serial]
    fn the_wait_for_the_last_other_reference_stops_at_its_budget() {
        let runner = Runner::new().expect("Runner::new");
        let _held_past_the_budget = Arc::clone(&runner);

        let wait_started = Instant::now();
        assert!(
            !runner.wait_until_this_reference_alone_holds_the_engine(Duration::from_millis(30))
        );
        assert!(wait_started.elapsed() < A_STREAM_ENDS_WITHIN);
    }

    /// The wait for the last other reference, inside a run that owns the
    /// machine's shutdown signals, walks every loaded stream to the machine's
    /// shutdown level while it waits.
    #[test]
    #[serial]
    fn the_wait_for_the_last_other_reference_walks_every_loaded_stream_to_the_machines_shutdown() {
        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");

        let waiting_runner = Arc::clone(&runner);
        let waiting_stream = Arc::clone(&stream);
        let (wait_returned, the_wait_has_returned) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = waiting_runner.run_owning_the_machine_shutdown_signals(|| {
                waiting_runner.request_the_shutdown_of_every_loaded_stream(
                    "the test shuts the machine down",
                )?;
                let alone = waiting_runner
                    .wait_until_this_reference_alone_holds_the_engine(Duration::from_secs(2));
                Ok((alone, waiting_stream.has_ended()))
            });
            let _ = wait_returned.send(outcome);
        });

        let (alone, stream_ended_during_the_wait) = the_wait_has_returned
            .recv_timeout(A_STREAM_ENDS_WITHIN)
            .expect("the wait never returned")
            .expect("the wait owned the machine's shutdown signals");
        assert!(!alone, "the test's own reference still holds the engine");
        assert!(
            stream_ended_during_the_wait,
            "the wait never walked the loaded stream to the machine's shutdown"
        );
    }

    /// Poll `escalation_of` until it reports `wanted` or `within` passes,
    /// returning the last level it reported.
    fn the_escalation_once_it_reaches(
        wanted: RuntimeShutdownEscalation,
        within: Duration,
        escalation_of: impl Fn() -> RuntimeShutdownEscalation,
    ) -> RuntimeShutdownEscalation {
        let deadline = std::time::Instant::now() + within;
        loop {
            let reached = escalation_of();
            if reached >= wanted || std::time::Instant::now() >= deadline {
                return reached;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A machine shutdown forced while an unload waits on a stream forces
    /// that stream, though no thread owns the machine's shutdown signals to
    /// walk it there.
    ///
    /// Fail-without-fix: the unload's wait only ever asks for the graceful
    /// step, so the stream stays `Graceful` and a native callback it holds
    /// keeps its whole graceful join budget.
    #[test]
    #[serial]
    fn a_forced_machine_shutdown_forces_a_stream_an_unload_is_waiting_on() {
        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let unloading = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloading");

        // The stop takes this lock after it marks the stream stopping, so
        // holding it keeps the unload waiting until the test lets go.
        let the_held_teardown_waits_on = unloading.runtime_context.lock();
        let (unload_returned, the_unload_has_returned) = std::sync::mpsc::channel();
        let unloading_runner = Arc::clone(&runner);
        std::thread::spawn(move || {
            let _ = unload_returned.send(unloading_runner.unload_stream("unloading"));
        });
        let asked_for = the_escalation_once_it_reaches(
            RuntimeShutdownEscalation::Graceful,
            A_STREAM_ENDS_WITHIN,
            || unloading.this_streams_shutdown_escalation().escalation(),
        );

        crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
        crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
        let forced_to = the_escalation_once_it_reaches(
            RuntimeShutdownEscalation::Forced,
            Duration::from_secs(2),
            || unloading.this_streams_shutdown_escalation().escalation(),
        );
        let returned_while_held = the_unload_has_returned.try_recv().is_ok();
        drop(the_held_teardown_waits_on);
        let unload_outcome = the_unload_has_returned.recv_timeout(A_STREAM_ENDS_WITHIN);

        assert_eq!(asked_for, RuntimeShutdownEscalation::Graceful);
        assert_eq!(
            forced_to,
            RuntimeShutdownEscalation::Forced,
            "the unload's wait left the stream at its graceful step after the machine's \
             shutdown was forced"
        );
        assert!(
            !returned_while_held,
            "the unload returned while the stream's teardown was still held"
        );
        unload_outcome
            .expect("the unload never returned once its teardown was let go")
            .expect("the stream unloads cleanly");
        assert_eq!(
            unloading.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::Stopped)
        );
        assert!(runner.names_of_the_loaded_streams().is_empty());
    }

    /// Set once [`CallbackHeldUntilTheTestLetsGo`] is inside its callback.
    static THE_HELD_CALLBACK_HAS_BEGUN: AtomicBool = AtomicBool::new(false);

    /// Set by the test to let [`CallbackHeldUntilTheTestLetsGo`] return.
    static THE_HELD_CALLBACK_MAY_RETURN: AtomicBool = AtomicBool::new(false);

    /// A source whose first callback does not return until the test lets it,
    /// or for a minute.
    #[crate::processor(execution = continuous(interval_ms = 5))]
    pub(crate) struct CallbackHeldUntilTheTestLetsGo;

    impl crate::core::ContinuousProcessor for CallbackHeldUntilTheTestLetsGo::Processor {
        fn process(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
        ) -> Result<()> {
            THE_HELD_CALLBACK_HAS_BEGUN.store(true, Ordering::SeqCst);
            let held_until = std::time::Instant::now() + Duration::from_secs(60);
            while !THE_HELD_CALLBACK_MAY_RETURN.load(Ordering::SeqCst)
                && std::time::Instant::now() < held_until
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }
    }

    /// An unload of a stream whose native processor holds its callback returns
    /// well inside the graceful join budget once the machine's shutdown is
    /// forced, the thread abandoned rather than waited out.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn a_forced_machine_shutdown_abandons_a_held_native_callback_an_unload_is_waiting_on() {
        use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};

        /// The engine-chosen join budget of a native processor thread under a
        /// graceful shutdown.
        const NATIVE_PROCESSOR_THREAD_GRACEFUL_JOIN_BUDGET: Duration = Duration::from_secs(5);

        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        THE_HELD_CALLBACK_HAS_BEGUN.store(false, Ordering::SeqCst);
        THE_HELD_CALLBACK_MAY_RETURN.store(false, Ordering::SeqCst);
        PROCESSOR_REGISTRY.register::<CallbackHeldUntilTheTestLetsGo::Processor>();
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let holding = an_empty_stream_loaded_into(&runner, project_directory.path(), "holding");
        holding
            .add_processor(ProcessorSpec::new(
                CallbackHeldUntilTheTestLetsGo::processor_class_import_path(),
                serde_json::json!({}),
            ))
            .expect("the holding source is added");
        holding.start().expect("the stream starts");
        let callback_begun_by = std::time::Instant::now() + Duration::from_secs(30);
        while !THE_HELD_CALLBACK_HAS_BEGUN.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < callback_begun_by,
                "the holding source never entered its callback"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let (unload_returned, the_unload_has_returned) = std::sync::mpsc::channel();
        let unloading_runner = Arc::clone(&runner);
        let unload_began = std::time::Instant::now();
        std::thread::spawn(move || {
            let outcome = unloading_runner.unload_stream("holding");
            let _ = unload_returned.send((outcome, unload_began.elapsed()));
        });
        the_escalation_once_it_reaches(
            RuntimeShutdownEscalation::Graceful,
            A_STREAM_ENDS_WITHIN,
            || holding.this_streams_shutdown_escalation().escalation(),
        );
        crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
        crate::core::runtime::escalate_the_machines_shutdown_for_a_delivered_signal("unit test");
        let unload_outcome = the_unload_has_returned.recv_timeout(A_STREAM_ENDS_WITHIN);
        let abandoned_threads = holding
            .processor_threads_abandoned_and_still_running()
            .len();
        THE_HELD_CALLBACK_MAY_RETURN.store(true, Ordering::SeqCst);

        let (_ended_reporting, unloaded_in) = unload_outcome
            .expect("the unload never returned after the machine's shutdown was forced");
        assert_eq!(
            holding.this_streams_shutdown_escalation().escalation(),
            RuntimeShutdownEscalation::Forced
        );
        assert!(
            unloaded_in < NATIVE_PROCESSOR_THREAD_GRACEFUL_JOIN_BUDGET / 2,
            "the unload took {unloaded_in:?}; a forced shutdown abandons a held native callback \
             well inside the {NATIVE_PROCESSOR_THREAD_GRACEFUL_JOIN_BUDGET:?} graceful budget"
        );
        assert_eq!(
            abandoned_threads, 1,
            "the held callback's thread is abandoned, not joined"
        );
        assert!(runner.names_of_the_loaded_streams().is_empty());
    }

    /// A stream loaded while the machine is shutting every stream down is
    /// refused by name.
    #[test]
    #[serial]
    fn a_load_while_the_machine_shuts_down_is_refused_by_name() {
        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
            "the test shuts the machine down",
        )
        .expect("the machine's shutdown is requested");

        let Err(refusal) = runner.load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                .named("too-late"),
        ) else {
            panic!("a stream loaded while the machine shuts down");
        };

        let refusal = refusal.to_string();
        assert!(refusal.contains("too-late"), "{refusal}");
        assert!(refusal.contains("shutting"), "{refusal}");
        assert!(runner.names_of_the_loaded_streams().is_empty());
    }

    /// A watched load the machine's shutdown reaches before or while the
    /// stream is built is abandoned, never refused: its host exits as an
    /// interrupt rather than as a refusal.
    #[test]
    #[serial]
    fn a_watched_load_the_machines_shutdown_reaches_while_it_builds_is_abandoned() {
        let _machine_level_cleared =
            crate::core::runtime::TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop(
            );
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
            "the test shuts the machine down",
        )
        .expect("the machine's shutdown is requested");
        let graph = GraphSnapshot::from_graph_document(serde_json::json!({
            "nodes": [{
                "name": "source",
                "type": TickCountingInTheFirstStream::processor_class_import_path().as_str()
            }]
        }))
        .expect("the test graph reads");

        let load_outcome = runner
            .load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested(
                &graph,
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named("too-late"),
            )
            .expect("a load the machine's shutdown reached is abandoned, not refused");

        assert!(matches!(
            load_outcome,
            StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest
        ));
        assert!(runner.names_of_the_loaded_streams().is_empty());
    }

    /// Unloading a stream stops it and frees its name for the next load; the
    /// other stream stays loaded.
    #[test]
    #[serial]
    fn an_unloaded_stream_frees_its_name_and_leaves_the_other_loaded() {
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let unloaded = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloaded");
        let kept = an_empty_stream_loaded_into(&runner, project_directory.path(), "kept");

        runner
            .unload_stream("unloaded")
            .expect("the stream unloads cleanly");

        assert_eq!(
            unloaded.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::Stopped)
        );
        assert_eq!(runner.names_of_the_loaded_streams(), ["kept"]);
        assert!(!kept.has_ended());
        let reloaded = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloaded");
        assert!(!Arc::ptr_eq(&reloaded, &unloaded));
    }

    /// Counts the ticks of [`TickCountingInTheFirstStream`].
    static TICKS_IN_THE_FIRST_STREAM: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// Counts the ticks of [`TickCountingInTheSecondStream`].
    static TICKS_IN_THE_SECOND_STREAM: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// What [`TickCountingInTheFirstStream`] logs on its first tick.
    const TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD: &str =
        "token-of-the-first-streams-processor-thread";

    /// What [`TickCountingInTheSecondStream`] logs on its first tick.
    const TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD: &str =
        "token-of-the-second-streams-processor-thread";

    /// A source that counts its own ticks, for the first stream.
    #[crate::processor(execution = continuous(interval_ms = 5))]
    pub(crate) struct TickCountingInTheFirstStream;

    impl crate::core::ContinuousProcessor for TickCountingInTheFirstStream::Processor {
        fn process(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
        ) -> Result<()> {
            if TICKS_IN_THE_FIRST_STREAM.fetch_add(1, Ordering::SeqCst) == 0 {
                tracing::info!("{TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD}");
            }
            Ok(())
        }
    }

    /// A source that counts its own ticks, for the second stream.
    #[crate::processor(execution = continuous(interval_ms = 5))]
    pub(crate) struct TickCountingInTheSecondStream;

    impl crate::core::ContinuousProcessor for TickCountingInTheSecondStream::Processor {
        fn process(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
        ) -> Result<()> {
            if TICKS_IN_THE_SECOND_STREAM.fetch_add(1, Ordering::SeqCst) == 0 {
                tracing::info!("{TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD}");
            }
            Ok(())
        }
    }

    /// Two started streams hold the engine's one Vulkan device, each one's
    /// processor thread logs only to its own stream's file, and one's
    /// shutdown leaves the other started and processing, its graph changes
    /// still committing on its own compiler.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn two_started_streams_share_one_vulkan_device_and_one_shutdown_leaves_the_other_processing() {
        use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};

        PROCESSOR_REGISTRY.register::<TickCountingInTheFirstStream::Processor>();
        PROCESSOR_REGISTRY.register::<TickCountingInTheSecondStream::Processor>();
        let project_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let runner = Runner::new().expect("Runner::new");
        let load_named = |stream_name: &str| {
            runner
                .load_an_empty_stream(
                    OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                        .named(stream_name),
                )
                .expect("an empty stream loads")
        };
        let first = load_named("first");
        let second = load_named("second");
        let first_log = first.jsonl_log_path().expect("first logs").to_path_buf();
        let second_log = second.jsonl_log_path().expect("second logs").to_path_buf();
        first
            .add_processor(ProcessorSpec::new(
                TickCountingInTheFirstStream::processor_class_import_path(),
                serde_json::json!({}),
            ))
            .expect("the first stream's source is added");
        second
            .add_processor(ProcessorSpec::new(
                TickCountingInTheSecondStream::processor_class_import_path(),
                serde_json::json!({}),
            ))
            .expect("the second stream's source is added");
        first.start().expect("the first stream starts");
        second.start().expect("the second stream starts");
        for stream in [&first, &second] {
            stream
                .wait_until_every_processor_is_running(Duration::from_secs(30))
                .expect("the stream's source runs");
        }

        let device_of = |stream: &LoadedStreamInThisRuntime| {
            Arc::clone(
                stream
                    .runtime_context_while_started()
                    .expect("a started stream has its runtime context")
                    .gpu
                    .device(),
            )
        };
        assert!(
            Arc::ptr_eq(&device_of(&first), &device_of(&second)),
            "two streams in one engine must hold one Vulkan device"
        );

        first.ask_for_this_streams_shutdown("the test shuts the first stream down");
        runner
            .wait_until_the_stream_ends(&first)
            .expect("the first stream stops cleanly");
        assert_eq!(first.status(), RuntimeStatus::Stopped);

        let first_ticks_once_stopped = TICKS_IN_THE_FIRST_STREAM.load(Ordering::SeqCst);
        let second_ticks_once_the_first_stopped = TICKS_IN_THE_SECOND_STREAM.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            TICKS_IN_THE_FIRST_STREAM.load(Ordering::SeqCst),
            first_ticks_once_stopped,
            "the stopped stream's processor kept running"
        );
        assert!(
            TICKS_IN_THE_SECOND_STREAM.load(Ordering::SeqCst) > second_ticks_once_the_first_stopped,
            "the other stream stopped processing when the first shut down"
        );
        assert_eq!(second.status(), RuntimeStatus::Started);
        assert_eq!(runner.names_of_the_loaded_streams(), ["second"]);

        second
            .add_processor(ProcessorSpec::new(
                TickCountingInTheSecondStream::processor_class_import_path(),
                serde_json::json!({}),
            ))
            .expect("a live add into the running stream");
        second
            .wait_until_every_processor_is_running(Duration::from_secs(30))
            .expect("the live add commits on the running stream's own compiler");
        assert_eq!(
            second.to_json().unwrap()["nodes"].as_array().map(Vec::len),
            Some(2)
        );

        runner.shut_down().expect("the engine shuts down");
        assert!(second.has_ended());

        for (stream_log, stream_name, own_token, other_token) in [
            (
                &first_log,
                "first",
                TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD,
                TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD,
            ),
            (
                &second_log,
                "second",
                TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD,
                TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD,
            ),
        ] {
            let records = every_record_of_the_stream_log_at(stream_log);
            for record in &records {
                assert_eq!(record.stream.as_deref(), Some(stream_name), "{record:?}");
                assert_ne!(
                    record.message, other_token,
                    "the log of `{stream_name}` holds the other stream's processor record"
                );
            }
            assert!(
                records.iter().any(|record| record.message == own_token),
                "the log of `{stream_name}` lacks its processor thread's record: {records:#?}"
            );
        }
    }

    /// Check in a memfd under `owner_key` over `connection`, as a surface
    /// store registering under that owner does.
    #[cfg(target_os = "linux")]
    fn check_in_a_surface_under(connection: &std::os::unix::net::UnixStream, owner_key: &str) {
        let name = std::ffi::CString::new("a-surface-a-stream-registered").unwrap();
        let memfd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(memfd >= 0, "memfd_create failed");
        let contents = b"pixels";
        let written = unsafe { libc::write(memfd, contents.as_ptr().cast(), contents.len()) };
        assert_eq!(written, contents.len() as isize);
        let (answer, _) = streamlib_surface_client::send_request_with_fds(
            connection,
            &serde_json::json!({
                "op": "check_in",
                "runtime_id": owner_key,
                "width": 16,
                "height": 16,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
            }),
            &[memfd],
            0,
        )
        .expect("the check-in is answered");
        unsafe { libc::close(memfd) };
        assert!(
            answer.get("surface_id").is_some(),
            "the check-in was refused: {answer}"
        );
    }

    /// An unloaded stream's surface registrations leave the engine's
    /// surface-sharing service, which outlives the stream and keeps a
    /// registration from this process past its connection's close; the other
    /// stream's and the engine's own stay.
    ///
    /// Mental-revert: without the release in the stream's stop, the unloaded
    /// stream's registration stays until the engine shuts down.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn an_unloaded_streams_surface_registrations_go_and_the_others_stay() {
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let unloaded = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloaded");
        let staying = an_empty_stream_loaded_into(&runner, project_directory.path(), "staying");
        let unloaded_streams_owner_key = unloaded.loaded_stream_identity().to_string();
        let staying_streams_owner_key = staying.loaded_stream_identity().to_string();
        let the_engines_owner_key = runner.runtime_id().to_string();
        let connection =
            streamlib_surface_client::connect_to_surface_share_socket(runner.surface_socket_path())
                .expect("the engine's surface-sharing service accepts a connection");
        for owner_key in [
            &unloaded_streams_owner_key,
            &staying_streams_owner_key,
            &the_engines_owner_key,
        ] {
            check_in_a_surface_under(&connection, owner_key);
        }

        runner
            .unload_stream("unloaded")
            .expect("the stream unloads");

        let registrations = &runner
            .engine_resources_shared_by_every_stream
            .surface_share_registrations_by_owner;
        assert!(
            registrations
                .surface_ids_by_runtime(&unloaded_streams_owner_key)
                .is_empty(),
            "the unloaded stream's registration outlived it"
        );
        assert_eq!(
            registrations
                .surface_ids_by_runtime(&staying_streams_owner_key)
                .len(),
            1,
            "another stream's registration went with the unloaded one"
        );
        assert_eq!(
            registrations
                .surface_ids_by_runtime(&the_engines_owner_key)
                .len(),
            1,
            "the engine's own registration went with the unloaded stream"
        );
    }

    /// Two streams loaded under one name in one runtime — the second loaded
    /// once the first left the table — publish on different topics and own
    /// different surface registrations.
    ///
    /// Mental-revert: an identity of the runtime id and the name alone gives
    /// both one topic, so a late event of the first reaches the second's
    /// listener.
    #[test]
    #[serial]
    fn a_stream_loaded_under_the_name_of_one_that_left_has_its_own_topic() {
        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
        runner
            .unload_stream("main")
            .expect("the first stream unloads");
        let second = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");

        assert_ne!(first.stream_tag(), second.stream_tag());
        assert_ne!(
            crate::core::pubsub::topics::loaded_stream(first.loaded_stream_identity()),
            crate::core::pubsub::topics::loaded_stream(second.loaded_stream_identity())
        );
        assert_ne!(
            first.loaded_stream_identity().to_string(),
            second.loaded_stream_identity().to_string()
        );
    }

    /// Every record the JSONL log at `path` holds.
    fn every_record_of_the_stream_log_at(
        path: &std::path::Path,
    ) -> Vec<streamlib_runtime_client_contract::runtime_log_event::RuntimeLogEvent> {
        crate::core::logging::one_stream_log_file_written_on_a_test_thread::read_every_record_of_a_jsonl_log(path)
    }

    /// Each stream's records — from its own threads, from a thread it spawned,
    /// and from engine code acting on it — land in its own JSONL file under its
    /// project and never in the other's, and a record no stream emitted lands
    /// in neither. Each file is whole on disk once its stream is unloaded.
    #[test]
    #[serial]
    fn two_streams_log_each_to_its_own_file_and_a_record_no_stream_emitted_to_neither() {
        let project_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let runner = Runner::new().expect("Runner::new");
        let load_named = |stream_name: &str| {
            runner
                .load_an_empty_stream(
                    OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                        .named(stream_name),
                )
                .expect("an empty stream loads")
        };
        let first = load_named("first");
        let second = load_named("second");
        let first_log = first.jsonl_log_path().expect("first logs").to_path_buf();
        let second_log = second.jsonl_log_path().expect("second logs").to_path_buf();
        assert_ne!(first_log, second_log);
        for stream_log in [&first_log, &second_log] {
            assert_eq!(
                stream_log.parent(),
                Some(
                    project_directory
                        .path()
                        .join(".streamlib")
                        .join("logs")
                        .as_path()
                )
            );
        }

        first.log_route().run_entered(|| {
            std::thread::spawn(
                crate::core::logging::carrying_this_threads_loaded_stream_log_route(|| {
                    tracing::info!("token-from-a-thread-the-first-stream-spawned")
                }),
            )
            .join()
            .expect("the spawned thread logs")
        });
        second
            .log_route()
            .run_entered(|| tracing::info!("token-of-the-second-stream"));
        tracing::info!("token-no-stream-emitted");
        runner.unload_stream("first").expect("first unloads");
        runner.unload_stream("second").expect("second unloads");

        let runtime_id = runner.runtime_id().to_string();
        let first_token = "token-from-a-thread-the-first-stream-spawned";
        let second_token = "token-of-the-second-stream";
        for (stream_log, stream_name, own_token, other_stream_name, other_token) in [
            (&first_log, "first", first_token, "second", second_token),
            (&second_log, "second", second_token, "first", first_token),
        ] {
            let records = every_record_of_the_stream_log_at(stream_log);
            for record in &records {
                assert_eq!(record.stream.as_deref(), Some(stream_name), "{record:?}");
                assert_eq!(record.runtime_id, runtime_id, "{record:?}");
                assert!(
                    !record.message.contains(&format!("`{other_stream_name}`"))
                        && record.message != other_token
                        && record.message != "token-no-stream-emitted",
                    "the log of `{stream_name}` holds a record it did not emit: {record:?}"
                );
            }
            for message_the_stream_logged in [
                format!("Loading the stream `{stream_name}`"),
                format!("[stop] The stream `{stream_name}` stopped"),
                own_token.to_string(),
            ] {
                assert!(
                    records
                        .iter()
                        .any(|record| record.message.starts_with(&message_the_stream_logged)),
                    "the log of `{stream_name}` lacks `{message_the_stream_logged}`: {records:#?}"
                );
            }
        }
    }

    /// A second runner in one process logs fully: each runner's stream writes
    /// its own file, every record carrying that runner's id.
    #[test]
    #[serial]
    fn a_second_runner_in_one_process_logs_to_its_own_stream_files() {
        let project_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let first_runner = Runner::new().expect("the first runner constructs");
        let second_runner = Runner::new().expect("the second runner constructs");
        let mut stream_logs_by_runtime_id = Vec::new();
        for runner in [&first_runner, &second_runner] {
            let stream = runner
                .load_an_empty_stream(
                    OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                        .named("main"),
                )
                .expect("an empty stream loads");
            stream_logs_by_runtime_id.push((
                runner.runtime_id().to_string(),
                stream
                    .jsonl_log_path()
                    .expect("each runner's stream logs")
                    .to_path_buf(),
            ));
            runner.unload_stream("main").expect("the stream unloads");
        }

        assert_ne!(
            stream_logs_by_runtime_id[0].1,
            stream_logs_by_runtime_id[1].1
        );
        for (runtime_id, stream_log) in &stream_logs_by_runtime_id {
            let records = every_record_of_the_stream_log_at(stream_log);
            assert!(
                records
                    .iter()
                    .any(|record| record.message == "[stop] The stream `main` stopped"),
                "the stream of runtime {runtime_id} wrote no record of its stop: {records:#?}"
            );
            for record in &records {
                assert_eq!(&record.runtime_id, runtime_id, "{record:?}");
                assert_eq!(record.stream.as_deref(), Some("main"), "{record:?}");
            }
        }
    }

    /// A stream's records are numbered from 1 in the order its route wrote
    /// them, readable by sequence number while the stream is loaded, and its
    /// JSONL file holds the same records with no number added.
    #[test]
    #[serial]
    fn a_streams_records_are_read_by_sequence_number_and_its_jsonl_file_carries_none() {
        let project_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let runner = Runner::new().expect("Runner::new");
        let stream = runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named("main"),
            )
            .expect("an empty stream loads");
        stream.log_route().run_entered(|| {
            for token_index in 0..3 {
                tracing::info!("token-of-the-stream-{token_index}");
            }
        });
        let stream_log = stream
            .jsonl_log_path()
            .expect("the stream logs")
            .to_path_buf();
        stream.log_route().close_the_jsonl_log_file();

        let every_record = stream.log_records_after(0, 4096);
        assert_eq!(every_record.records_no_longer_held, 0);
        let sequences: Vec<u64> = every_record.records.iter().map(|r| r.sequence).collect();
        assert_eq!(sequences, (1..=sequences.len() as u64).collect::<Vec<_>>());
        assert_eq!(every_record.next_after, sequences.len() as u64);
        let token_sequences: Vec<u64> = every_record
            .records
            .iter()
            .filter(|numbered| {
                numbered.record["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with("token-of-the-stream-"))
            })
            .map(|numbered| numbered.sequence)
            .collect();
        assert_eq!(token_sequences.len(), 3, "{every_record:#?}");

        let after_the_first_token = stream.log_records_after(token_sequences[0], 1);
        assert_eq!(after_the_first_token.records.len(), 1);
        assert_eq!(
            after_the_first_token.records[0].sequence,
            token_sequences[0] + 1
        );

        let lines_on_disk: Vec<serde_json::Value> = std::fs::read_to_string(&stream_log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSONL line"))
            .collect();
        assert!(!lines_on_disk.is_empty());
        for (line_on_disk, numbered) in lines_on_disk.iter().zip(&every_record.records) {
            assert_eq!(line_on_disk, &numbered.record);
            assert!(line_on_disk.get("sequence").is_none(), "{line_on_disk}");
        }
        runner.unload_stream("main").expect("the stream unloads");
    }

    /// With a runtime own-log directory, the records no stream emitted are
    /// written to `tatolabd-<started_at_millis>.jsonl` there, stamped with the
    /// runtime's id and no stream, and a stream's records are not.
    #[test]
    #[serial]
    fn the_records_no_stream_emitted_land_in_the_runtimes_own_log_and_a_streams_do_not() {
        let project_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let state_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let runtime_own_log_directory = state_directory.path().join("logs");
        let runner = Runner::new_with_construction_options(RunnerConstructionOptions {
            runtime_own_log_directory: Some(runtime_own_log_directory.clone()),
            ..RunnerConstructionOptions::default()
        })
        .expect("the runner constructs");
        let runtime_own_log = runner
            .runtime_own_log_path()
            .expect("the runtime keeps its own log")
            .to_path_buf();
        assert_eq!(
            runtime_own_log.parent(),
            Some(runtime_own_log_directory.as_path())
        );
        let file_name = runtime_own_log
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            file_name.starts_with("tatolabd-") && file_name.ends_with(".jsonl"),
            "{file_name}"
        );

        let stream = runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named("main"),
            )
            .expect("an empty stream loads");
        stream
            .log_route()
            .run_entered(|| tracing::info!("token-of-the-stream"));
        tracing::info!("token-no-stream-emitted");
        runner.unload_stream("main").expect("the stream unloads");
        let runtime_id = runner.runtime_id().to_string();
        drop(stream);
        drop(runner);

        let records = every_record_of_the_stream_log_at(&runtime_own_log);
        assert!(
            records
                .iter()
                .any(|record| record.message == "token-no-stream-emitted"
                    && record.runtime_id == runtime_id
                    && record.stream.is_none()),
            "the runtime's own log lacks the record no stream emitted: {records:#?}"
        );
        for record in &records {
            assert_eq!(record.stream, None, "{record:?}");
            assert_ne!(record.message, "token-of-the-stream", "{record:?}");
        }
    }

    /// The setup hooks run once, with the engine's GPU context and its own
    /// surface store, when the first stream's start creates it — every hook
    /// even after one fails, that start reporting the failure — and a hook
    /// installed after them is refused.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[serial]
    fn the_setup_hooks_run_once_each_even_after_one_fails_and_a_later_one_is_refused() {
        use std::sync::atomic::AtomicUsize;

        let project_directory = a_project_directory_this_test_owns();
        let runner = Runner::new().expect("Runner::new");
        let hook_runs = Arc::new(AtomicUsize::new(0));
        let hooks_saw_the_engines_surface_store = Arc::new(AtomicBool::new(true));
        for hook_fails in [true, false] {
            let hook_runs = Arc::clone(&hook_runs);
            let hooks_saw_the_engines_surface_store =
                Arc::clone(&hooks_saw_the_engines_surface_store);
            runner
                .install_setup_hook(move |engine_gpu_context| {
                    hook_runs.fetch_add(1, Ordering::SeqCst);
                    if cfg!(any(target_os = "linux", target_os = "macos"))
                        && engine_gpu_context.surface_store().is_none()
                    {
                        hooks_saw_the_engines_surface_store.store(false, Ordering::SeqCst);
                    }
                    if hook_fails {
                        return Err(Error::Configuration("the first setup hook fails".into()));
                    }
                    Ok(())
                })
                .expect("a hook installed before any start is queued");
        }

        let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "first");
        let first_start_failure = first
            .start()
            .expect_err("the failing hook aborts the start that ran it");
        assert!(
            first_start_failure
                .to_string()
                .contains("the first setup hook fails"),
            "{first_start_failure}"
        );
        assert_eq!(
            hook_runs.load(Ordering::SeqCst),
            2,
            "a failing hook stopped the rest"
        );
        assert!(hooks_saw_the_engines_surface_store.load(Ordering::SeqCst));

        let second = an_empty_stream_loaded_into(&runner, project_directory.path(), "second");
        second.start().expect("the second stream starts");
        assert_eq!(hook_runs.load(Ordering::SeqCst), 2, "the hooks ran again");
        assert!(
            runner.install_setup_hook(|_| Ok(())).is_err(),
            "a hook installed after the hooks ran was queued where nothing runs it"
        );

        runner.shut_down().expect("the engine shuts down");
    }
}
