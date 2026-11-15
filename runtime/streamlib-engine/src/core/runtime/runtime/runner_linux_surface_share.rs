// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The engine's surface-sharing socket on Linux.

use std::sync::Arc;

use streamlib_runtime_client_contract::streamlib_runtime_directory::StreamlibRuntimeDirectory;

use crate::core::runtime::RuntimeUniqueId;
use crate::core::{Error, Result};

use super::{EngineResourcesSharedByEveryStream, Runner};

impl Runner {
    /// Path of the runtime-internal surface-sharing Unix socket, bound during
    /// [`Runner::new`] at `<runtime directory>/surface-share-<runtime_id>.sock`.
    pub fn surface_socket_path(&self) -> &std::path::Path {
        &self
            .engine_resources_shared_by_every_stream
            .surface_socket_path
    }
}

/// Compute the per-runtime surface-sharing socket path, refuse to start if
/// another live runtime is already bound there, clean up an orphan socket
/// from a prior crashed runtime, bring the listener up, and remove the
/// surface-sharing sockets every other dead runtime left behind.
pub(super) fn bring_up_surface_service(
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
    match crate::linux::surface_share::remove_the_surface_share_sockets_left_by_dead_runtimes(
        runtime_directory,
    ) {
        Ok(removed_socket_paths) if !removed_socket_paths.is_empty() => tracing::info!(
            removed_socket_count = removed_socket_paths.len(),
            "[new] Removed the surface-sharing sockets runtimes that died without unbinding left behind"
        ),
        Ok(_) => {}
        Err(sweep_failure) => tracing::warn!(
            "[new] The surface-sharing sockets of dead runtimes were not swept: {sweep_failure}"
        ),
    }

    Ok((
        service,
        socket_path,
        check_out_leases,
        registrations_by_owner,
    ))
}

/// Stop the surface-sharing socket service here rather than leaving it to its
/// `Drop`, so the socket file is gone before the engine's shutdown returns.
pub(super) fn stop_the_surface_share_service(engine: &EngineResourcesSharedByEveryStream) {
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
