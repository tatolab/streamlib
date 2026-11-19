// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The memory behind a GPU surface handle, shared by the handle and every
//! DLPack capsule minted from it.

use std::sync::Arc;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use pyo3::exceptions::PyNotImplementedError;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use pyo3::prelude::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use super::HostVisiblePixelPlaneView;
#[cfg(target_os = "linux")]
use super::pending_staged_write_back_to_surface::PendingStagedWriteBackToSurface;

/// The engine value plus the surface-store release it owes, held behind
/// an `Arc` shared by the Python handle and every DLPack capsule minted
/// from it.
///
/// Release runs in `Drop` rather than on `close()` precisely so a tensor
/// that outlives its frame keeps addressing live memory: the last holder
/// settles the debt, whether that is the handle or a capsule Python is
/// still holding.
pub(crate) struct GpuSurfaceOwnedMemory {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) checked_out_surface: HelperCheckedOutSurface,
    #[cfg(target_os = "linux")]
    pub(super) minted_surface_id: Option<String>,
    /// The one staged edit outstanding against this surface, whichever
    /// door staged it.
    ///
    /// It lives here, not on the handle or the scope, because the thing it
    /// is about is the *surface*: the handle's CPU door and every
    /// device-tensor scope that handle mints all publish over these same
    /// pixels out of different stagings, and each staging was filled
    /// before the others' edits existed. One cell they all reach is what
    /// makes the second door's refusal reach the first door's edit.
    #[cfg(target_os = "linux")]
    pub(super) pending_staged_write_back: PendingStagedWriteBackToSurface,
    /// Whether a device-tensor scope over this surface is entered. A second
    /// one would share the first's staging and its pending edit, and its
    /// exit would publish the first's edit early and leave the first's
    /// later writes unarmed.
    #[cfg(target_os = "linux")]
    pub(super) a_device_tensor_scope_is_entered: std::sync::atomic::AtomicBool,
}

impl GpuSurfaceOwnedMemory {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn new(
        checked_out_surface: HelperCheckedOutSurface,
        #[cfg_attr(
            not(target_os = "linux"),
            expect(
                unused_variables,
                reason = "only the Linux device and CPU-readback exports name the surface by its id"
            )
        )]
        minted_surface_id: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            checked_out_surface,
            #[cfg(target_os = "linux")]
            minted_surface_id,
            #[cfg(target_os = "linux")]
            pending_staged_write_back: PendingStagedWriteBackToSurface::new_unarmed(),
            #[cfg(target_os = "linux")]
            a_device_tensor_scope_is_entered: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The tensor storage buffer this surface is, if it is one.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn tensor_storage_buffer(
        &self,
    ) -> Option<&crate::python_helper_process_pixel_exchange::HelperCheckedOutStorageBuffer> {
        self.checked_out_surface.tensor_storage_buffer()
    }

    /// Off Linux no export staging exists: a macOS surface is its IOSurface's
    /// own mapping, and elsewhere `host_visible_pixel_plane` refuses, naming
    /// the platform.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn cpu_reach_goes_through_the_export_staging(&self) -> bool {
        false
    }

    /// No surface exchange exists here, so no handle reaches this.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn host_visible_pixel_plane(&self) -> PyResult<HostVisiblePixelPlaneView> {
        Err(PyNotImplementedError::new_err(
            "the GPU surface exchange is a Linux path: it needs the surface-share service and \
             the consumer-side import, and this platform has neither",
        ))
    }
}
