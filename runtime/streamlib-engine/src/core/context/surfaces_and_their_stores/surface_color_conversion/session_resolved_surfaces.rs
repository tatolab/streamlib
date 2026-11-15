// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the session keeps resolved per surface: forgetting one, and the test-only probes.

use super::SurfaceColorConversionSession;
#[cfg(all(test, target_os = "linux"))]
use super::{
    WhyAForeignDmaBufIsReadThroughAHostMapping, foreign_dma_buf_read_through_a_host_mapping,
};
#[cfg(all(test, target_os = "linux"))]
use crate::core::context::GpuContext;
#[cfg(all(test, target_os = "linux"))]
use crate::core::{Error, Result};

impl SurfaceColorConversionSession {
    /// Resolve `surface_id` as a foreign device's DMA-BUF read through a host
    /// mapping, the way a device that cannot import the buffer resolves it.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn resolve_a_foreign_dma_buf_through_a_host_mapping(
        &mut self,
        gpu: &GpuContext,
        surface_id: &str,
    ) -> Result<()> {
        let store = gpu
            .surface_store()
            .ok_or_else(|| Error::Configuration("no surface-share service is connected".into()))?;
        let registration = store.lookup_dma_buf_registration_without_importing_it(surface_id)?;
        let resolved = foreign_dma_buf_read_through_a_host_mapping(
            gpu,
            surface_id,
            registration,
            WhyAForeignDmaBufIsReadThroughAHostMapping::TheDmaBufImportFailed(
                "a test resolves it so".to_string(),
            ),
        )?;
        self.sources_by_surface_id
            .insert(surface_id.to_string(), resolved);
        Ok(())
    }

    /// Whether this session keeps a resolved source for `surface_id`.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn keeps_a_source_for(&self, surface_id: &str) -> bool {
        self.sources_by_surface_id.contains_key(surface_id)
    }

    /// Forget what this session resolved for `surface_id`, as a source or a
    /// destination, answering whether it held anything.
    pub(crate) fn forget_resolved_surface(&mut self, surface_id: &str) -> bool {
        let forgot_a_source = self.sources_by_surface_id.remove(surface_id).is_some()
            | self
                .rgba_or_bgra_texture_sources_by_surface_id
                .remove(surface_id)
                .is_some();
        #[cfg(target_os = "macos")]
        let forgot_a_destination = self
            .nv12_iosurface_destinations_by_surface_id
            .remove(surface_id)
            .is_some();
        #[cfg(target_os = "linux")]
        let forgot_a_destination = self
            .yuyv_pixel_buffer_destinations_by_surface_id
            .remove(surface_id)
            .is_some();
        forgot_a_source || forgot_a_destination
    }
}
