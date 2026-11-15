// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What every stream in the engine shares: the GPU context, the iceoryx2 node, the tokio runtime and the surface service.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;
use streamlib_runtime_client_contract::streamlib_runtime_directory::StreamlibRuntimeDirectory;

use crate::core::context::GpuContext;
use crate::core::runtime::MachineName;
use crate::core::runtime::RuntimeUniqueId;
use crate::core::runtime::processor_interpreters::processor_interpreter_launch_record::RuntimeUnitPathTheHostHandsTheEngineOnce;
use crate::core::runtime::streams_loaded_in_this_runtime::loaded_stream::{
    LoadedStreamInThisRuntime, LoadedStreamOutOfTheTableUntilItsEndIsRecorded,
};
use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

use super::{CreatedProjectFirstCommit, SetupHooksOfTheEngine, TokioRuntimeVariant};

/// What every stream loaded in one [`Runner`](super::Runner) shares: the engine's tokio
/// runtime, iceoryx2 node, surface-sharing service, GPU context, lend
/// directory and the table of loaded streams.
///
/// Fields drop in declaration order, so the hold on the process's logging
/// pathway, declared last, lets go after everything else is gone.
pub(crate) struct EngineResourcesSharedByEveryStream {
    /// Unique identifier for this runtime instance.
    pub(crate) runtime_id: Arc<RuntimeUniqueId>,
    /// The name this machine is addressed by, read once as the runtime starts.
    pub(crate) machine_name: Arc<MachineName>,
    /// When this engine's construction began, the origin of its uptime.
    pub(super) instant_this_engine_started: Instant,
    /// The streams loaded in this runtime, keyed by their stream id.
    pub(crate) streams_loaded_in_this_runtime:
        Mutex<BTreeMap<String, Arc<LoadedStreamInThisRuntime>>>,
    /// Every stream taken out of the table whose end may not be recorded yet,
    /// locked only inside the table's lock.
    pub(super) streams_out_of_the_table_until_their_end_is_recorded:
        Mutex<Vec<LoadedStreamOutOfTheTableUntilItsEndIsRecorded>>,
    /// The one GPU context, created by the first stream that starts.
    pub(super) gpu_context_created_by_the_first_stream_start: Mutex<Option<GpuContext>>,
    /// Hooks run once, when the GPU context is created, before any
    /// processor's `setup()` runs.
    pub(super) setup_hooks: Mutex<SetupHooksOfTheEngine>,
    /// The lend directory the host handed this runtime.
    pub(crate) processor_interpreter_lend_directory: RuntimeUnitPathTheHostHandsTheEngineOnce,
    /// The uv the host handed this runtime, which makes every project's `.venv`.
    pub(crate) runtime_unit_uv_executable: RuntimeUnitPathTheHostHandsTheEngineOnce,
    /// What the host commits a created project's files with, as its history's first commit.
    pub(crate) created_project_first_commit: OnceLock<CreatedProjectFirstCommit>,
    /// The project history the host records each move of a project's first-party packages
    /// in, `None` until the host hands one.
    pub(crate) project_history_recording_first_party_package_moves:
        Mutex<Option<Arc<dyn crate::core::runtime::ProjectHistoryRecordingFirstPartyPackageMoves>>>,
    /// The native services the host offered every node, such as the browser host.
    pub(crate) shared_native_services:
        Arc<crate::core::shared_native_services::SharedNativeServicesOfTheEngine>,
    /// Each stream's function-node states, kept across its loads.
    pub(crate) function_node_states_carried_for_every_stream:
        crate::core::runtime::FunctionNodeStatesCarriedForEveryStream,
    /// The store of each project a stream of this runtime opened, one per project.
    pub(crate) project_stores_open_in_this_runtime:
        Arc<crate::core::context::ProjectStoresOpenInThisRuntime>,
    /// Whether [`Runner::shut_down`](super::Runner::shut_down) has run.
    pub(super) shut_down: AtomicBool,
    /// Whether this engine holds the machine's shutdown signals now, and so
    /// walks every loaded stream to the machine's shutdown level as it waits.
    pub(super) owns_the_machine_shutdown_signals: AtomicBool,
    /// The runtime-internal surface-sharing service, alive for the engine's
    /// life; helper processes connect to it via `STREAMLIB_SURFACE_SOCKET`.
    #[cfg(target_os = "linux")]
    pub(super) surface_service:
        Mutex<Option<crate::linux::surface_share::UnixSocketSurfaceService>>,
    /// Path of the surface-sharing socket, inside the runtime directory.
    #[cfg(target_os = "linux")]
    pub(crate) surface_socket_path: PathBuf,
    /// The runtime-internal surface-sharing Mach service, alive for the
    /// engine's life; helper processes connect to it through
    /// `STREAMLIB_SURFACE_MACH_SERVICE`.
    #[cfg(target_os = "macos")]
    pub(super) mach_surface_share_service:
        Mutex<Option<crate::apple::surface_share::MachSurfaceShareService>>,
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
    pub(super) surface_share_registrations_by_owner:
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
    /// The engine's tokio runtime by handle: the `Runner`'s own, or an external one.
    pub(crate) tokio_runtime_variant: TokioRuntimeVariant,
    /// The runtime's own log, when the host asked for one; closed before the
    /// hold on the logging pathway lets go.
    pub(super) runtime_own_log: Option<crate::core::logging::TheRuntimesOwnLogWhileItsEngineLives>,
    /// This engine's hold on the process's logging pathway, which keeps the
    /// standard streams intercepted while any engine lives.
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    pub(super) process_logging_pathway_hold: crate::core::logging::ProcessLoggingPathwayHold,
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
                    #[cfg(target_os = "linux")]
                    self.serve_viewed_frames_from(Arc::new(created.clone()));
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

    /// Serve viewers' asks for frames their display imports from `source`
    /// through the surface-sharing service, while it runs.
    #[cfg(target_os = "linux")]
    pub(crate) fn serve_viewed_frames_from(
        &self,
        source: Arc<dyn crate::linux::surface_share::ViewedFrameSource>,
    ) {
        if let Some(surface_service) = self.surface_service.lock().as_ref() {
            surface_service.serve_viewed_frames_from(source);
        }
    }

    /// Run every setup hook once — each, even after one fails — and report
    /// the first failure. A later call runs none.
    pub(super) fn run_the_setup_hooks_once(&self, engine_gpu_context: &GpuContext) -> Result<()> {
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
    /// name since, into the streams whose end is not yet recorded — under the
    /// table's lock, so a wait always finds it in one or the other.
    pub(crate) fn remove_from_the_stream_table(&self, stream: &Arc<LoadedStreamInThisRuntime>) {
        let mut streams = self.streams_loaded_in_this_runtime.lock();
        let it_is_this_stream = streams
            .get(stream.stream_id())
            .is_some_and(|loaded| Arc::ptr_eq(loaded, stream));
        if it_is_this_stream {
            streams.remove(stream.stream_id());
            let mut streams_out_of_the_table = self
                .streams_out_of_the_table_until_their_end_is_recorded
                .lock();
            streams_out_of_the_table.retain(|left| !left.has_its_end_recorded());
            streams_out_of_the_table
                .push(LoadedStreamOutOfTheTableUntilItsEndIsRecorded::leaving_the_table(stream));
        }
    }

    /// The loaded stream whose id is exactly `stream_id`, if one is loaded.
    pub(crate) fn loaded_stream_of_the_id_when_loaded(
        &self,
        stream_id: &str,
    ) -> Option<Arc<LoadedStreamInThisRuntime>> {
        self.streams_loaded_in_this_runtime
            .lock()
            .get(stream_id)
            .cloned()
    }

    pub(crate) fn every_loaded_stream(&self) -> Vec<Arc<LoadedStreamInThisRuntime>> {
        self.streams_loaded_in_this_runtime
            .lock()
            .values()
            .cloned()
            .collect()
    }

    /// Every loaded stream, then every stream out of the table whose end is
    /// not yet recorded that anything still holds, read under the table's lock.
    pub(super) fn every_stream_still_held_whose_end_is_not_yet_recorded(
        &self,
    ) -> Vec<Arc<LoadedStreamInThisRuntime>> {
        let streams = self.streams_loaded_in_this_runtime.lock();
        let streams_out_of_the_table = self
            .streams_out_of_the_table_until_their_end_is_recorded
            .lock();
        streams
            .values()
            .cloned()
            .chain(
                streams_out_of_the_table
                    .iter()
                    .filter(|left| !left.has_its_end_recorded())
                    .filter_map(|left| left.stream_while_anything_still_holds_it()),
            )
            .collect()
    }
}
