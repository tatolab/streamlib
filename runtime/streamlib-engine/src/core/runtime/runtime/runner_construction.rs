// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Building the engine: its tokio runtime, GPU context, iceoryx2 node and surface service, and the setup hooks run once the GPU context exists.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;
use streamlib_runtime_client_contract::streamlib_runtime_directory::StreamlibRuntimeDirectory;

use crate::core::context::GpuContext;
use crate::core::runtime::MachineName;
use crate::core::runtime::RuntimeUniqueId;
use crate::core::runtime::node_interpreters::node_interpreter_launch_record::RuntimeUnitPathTheHostHandsTheEngineOnce;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::StreamActionsOfTheEngine;
use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

#[cfg(target_os = "linux")]
use super::runner_linux_surface_share::bring_up_surface_service;
#[cfg(target_os = "macos")]
use super::runner_macos_surface_share::bring_up_mach_surface_share_service;
use super::{
    EngineResourcesSharedByEveryStream, Runner, RunnerConstructionOptions, SetupHooksOfTheEngine,
    TokioRuntimeShutDownWithinItsBudget, TokioRuntimeVariant,
};

impl Runner {
    /// Build a runtime with the default construction options.
    pub fn new() -> Result<Arc<Self>> {
        Self::new_with_construction_options(RunnerConstructionOptions::default())
    }

    /// Build a runtime as `construction_options` choose.
    pub fn new_with_construction_options(
        construction_options: RunnerConstructionOptions,
    ) -> Result<Arc<Self>> {
        let instant_this_engine_started = Instant::now();
        let RunnerConstructionOptions {
            pretty_log_mirror_stream,
            runtime_own_log_directory,
        } = construction_options;
        #[cfg(target_os = "linux")]
        unsafe {
            libc::prctl(libc::PR_SET_TIMERSLACK, 1u64, 0u64, 0u64, 0u64);
        }

        // Auto-detect tokio context FIRST — telemetry exporters need a Tokio handle.
        let (tokio_runtime_variant, _tokio_runtime_the_runner_owns) =
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => (TokioRuntimeVariant::ExternalTokioHandle(handle), None),
                Err(_) => {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            Error::Runtime(format!("Failed to create tokio runtime: {}", e))
                        })?;
                    let owned = TokioRuntimeShutDownWithinItsBudget::owning(rt);
                    (
                        TokioRuntimeVariant::TokioRuntimeTheRunnerOwns(owned.handle().clone()),
                        Some(owned),
                    )
                }
            };

        // Load a local .env if present (RUST_LOG and other dev overrides).
        let _ = dotenvy::dotenv();

        // The id names every stream's log file, so a pinned one is refused
        // before the runtime writes anything.
        let runtime_id = Arc::new(RuntimeUniqueId::from_env_or_generate()?);

        // The JSONL schema is `RuntimeLogEvent`.
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
        tracing::info!("Creating Runner with ID: {runtime_id}");
        let machine_name = Arc::new(MachineName::of_this_machine());

        let runtime_directory = StreamlibRuntimeDirectory::resolve()
            .map_err(|refusal| Error::Runtime(refusal.to_string()))?;
        tracing::info!(
            "StreamLib runtime directory: {}",
            runtime_directory.path().display()
        );

        let streamlib_home =
            streamlib_runtime_client_contract::streamlib_home::get_streamlib_home();
        tracing::debug!("STREAMLIB_HOME: {}", streamlib_home.display());

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

        let runner = Arc::new(Self {
            _tokio_runtime_the_runner_owns,
            engine_resources_shared_by_every_stream: Arc::new(EngineResourcesSharedByEveryStream {
                runtime_id,
                machine_name,
                instant_this_engine_started,
                streams_loaded_in_this_runtime: Mutex::new(BTreeMap::new()),
                streams_out_of_the_table_until_their_end_is_recorded: Mutex::new(Vec::new()),
                gpu_context_created_by_the_first_stream_start: Mutex::new(None),
                setup_hooks: Mutex::new(SetupHooksOfTheEngine::WaitingForTheGpuContext(Vec::new())),
                node_interpreter_lend_directory:
                    RuntimeUnitPathTheHostHandsTheEngineOnce::naming("the lend directory"),
                runtime_unit_uv_executable: RuntimeUnitPathTheHostHandsTheEngineOnce::naming(
                    "the runtime unit's uv",
                ),
                extension_code_checkers: Default::default(),
                core_project_history_directory: RuntimeUnitPathTheHostHandsTheEngineOnce::naming(
                    "the core project's history directory",
                ),
                created_project_first_commit: OnceLock::new(),
                project_history_recording_first_party_package_moves: Mutex::new(None),
                shared_native_services:
                    crate::core::shared_native_services::SharedNativeServicesOfTheEngine::in_directory(
                        runtime_directory.shared_native_services_directory(),
                    ),
                function_node_states_carried_for_every_stream: Default::default(),
                project_stores_open_in_this_runtime: Default::default(),
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
        });
        runner
            .stream_actions
            .stream_action_queue_of_each_project
            .run_every_action_on(Arc::downgrade(&runner));
        Ok(runner)
    }

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
}
