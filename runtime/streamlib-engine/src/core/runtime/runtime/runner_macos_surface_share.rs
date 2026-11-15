// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The engine's surface-sharing Mach service on macOS.

use std::sync::Arc;

use crate::core::runtime::RuntimeUniqueId;
use crate::core::{Error, Result};

use super::{EngineResourcesSharedByEveryStream, Runner};

impl Runner {
    /// The runtime-internal surface-sharing Mach service's name and
    /// helper-process admissions, registered during [`Runner::new`].
    pub fn surface_share_mach_service_rendezvous(
        &self,
    ) -> &crate::apple::surface_share::MachSurfaceShareServiceRendezvous {
        &self
            .engine_resources_shared_by_every_stream
            .surface_share_mach_service_rendezvous
    }
}

/// Register the per-runtime surface-sharing Mach service, refusing to start
/// if another live runtime already holds this id's name. A crashed runtime's
/// name went with its process, so there is nothing stale to clean up.
pub(super) fn bring_up_mach_surface_share_service(
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

/// Stop the Mach service and drop the timeline pairs. The Mach service thread
/// answers helpers' reports against the timeline pairs, so it is joined before
/// the pairs go, and both go while the device that made their semaphores still
/// exists.
pub(super) fn stop_the_surface_share_service(engine: &EngineResourcesSharedByEveryStream) {
    crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
        "the surface-sharing service",
    );
    if let Some(mut mach_surface_share_service) = engine.mach_surface_share_service.lock().take() {
        mach_surface_share_service.stop();
    }
    engine.surface_share_cross_process_timeline_pairs.clear();
}
