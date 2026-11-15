// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The limited capability's in-process passthroughs to the surface export staging.

use super::*;

// In-process passthroughs on the limited capability, `host_inner`-direct
// like `create_exportable_timeline_semaphore`: a per-frame export must be
// reachable from `process` without escalating. The cdylib arm does not
// implement these, so a cdylib caller panics at the `host_inner` guard.
impl crate::core::context::GpuContextLimitedAccess {
    /// See [`GpuContext::share_surface_export_staging`].
    #[cfg(target_os = "linux")]
    pub fn share_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(String, PixelFormat)> {
        self.host_inner().share_surface_export_staging(staging)
    }

    /// See [`GpuContext::surface_export_staging`].
    pub fn surface_export_staging(
        &self,
        surface_id: &str,
        residency: SurfaceExportStagingResidency,
    ) -> Result<Arc<SurfaceExportStaging>> {
        self.host_inner()
            .surface_export_staging(surface_id, residency)
    }

    /// See [`GpuContext::refill_surface_export_staging`].
    pub fn refill_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        self.host_inner()
            .refill_surface_export_staging(staging, surface_id)
    }

    /// See [`GpuContext::copy_surface_export_staging_back_to_surface`].
    pub fn copy_surface_export_staging_back_to_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        self.host_inner()
            .copy_surface_export_staging_back_to_surface(staging, surface_id)
    }

    /// See [`GpuContext::copy_surface_export_staging_over_every_pixel_of_surface`].
    pub fn copy_surface_export_staging_over_every_pixel_of_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        self.host_inner()
            .copy_surface_export_staging_over_every_pixel_of_surface(staging, surface_id)
    }

    /// See [`GpuContext::export_surface_export_staging_opaque_fd`].
    pub fn export_surface_export_staging_opaque_fd(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        self.host_inner()
            .export_surface_export_staging_opaque_fd(staging)
    }
}
