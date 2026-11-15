// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Registering host surfaces with the adapter and looking up their resources and timelines.

use std::sync::Arc;

use streamlib_consumer_rhi::{DevicePrivilege, VulkanRhiDevice, VulkanTextureLike};
use streamlib_surface_adapter::{AdapterError, SurfaceId};

use crate::state::{
    HostImageSurfaceRegistration, HostSurfaceRegistration, SurfaceResource, SurfaceState,
};

use super::CudaSurfaceAdapter;

impl<D: VulkanRhiDevice> CudaSurfaceAdapter<D> {
    /// Register an allocated (or imported) surface with this adapter.
    ///
    /// `id` is assigned by the caller (typically from the surface-share
    /// service); it MUST be unique across the adapter's lifetime.
    /// Returns [`AdapterError::SurfaceAlreadyRegistered`] if `id` is
    /// already registered.
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration<D::Privilege>,
    ) -> Result<(), AdapterError> {
        let inserted = self.surfaces.register(
            id,
            SurfaceState {
                resource: SurfaceResource::Buffer {
                    pixel_buffer: registration.pixel_buffer,
                },
                produce_done: registration.produce_done,
                consume_done: registration.consume_done,
                read_holders: 0,
                write_held: false,
                current_signal_value: 0,
            },
        );
        if !inserted {
            return Err(AdapterError::SurfaceAlreadyRegistered { surface_id: id });
        }
        Ok(())
    }

    /// Register an image-flavored surface for the CUDA texture / surface
    /// object path.
    ///
    /// Validates that the texture's format is in the CUDA-mappable
    /// subset (`Rgba8Unorm` / `Rgba16Float` / `Rgba32Float`) at
    /// registration time so the cdylib's later
    /// `cudaExternalMemoryGetMappedMipmappedArray` call doesn't trip on
    /// `CUDA_ERROR_INVALID_VALUE` for an unmappable format. The
    /// host-side allocator (`HostVulkanTexture::new_opaque_fd_export`)
    /// enforces the same check at construction; the adapter's check is
    /// defense in depth and produces a typed
    /// [`AdapterError::BackendRejected`] with a usage-correction hint
    /// for the surface_id.
    ///
    /// Errors:
    /// - [`AdapterError::SurfaceAlreadyRegistered`] — `id` is already
    ///   registered (either flavor).
    /// - [`AdapterError::BackendRejected`] — the texture's format isn't
    ///   in the CUDA-mappable subset.
    pub fn register_host_image_surface(
        &self,
        id: SurfaceId,
        registration: HostImageSurfaceRegistration<D::Privilege>,
    ) -> Result<(), AdapterError> {
        let format = registration.texture.format();
        if !format.is_cuda_mappable() {
            return Err(AdapterError::BackendRejected {
                reason: format!(
                    "register_host_image_surface: surface_id={id}: format {format:?} is not \
                     CUDA-mappable; allowed: Rgba8Unorm, Rgba16Float, Rgba32Float \
                     (cudaExternalMemoryGetMappedMipmappedArray accepts only this subset)"
                ),
            });
        }
        let inserted = self.surfaces.register(
            id,
            SurfaceState {
                resource: SurfaceResource::Image {
                    texture: registration.texture,
                },
                produce_done: registration.produce_done,
                consume_done: registration.consume_done,
                read_holders: 0,
                write_held: false,
                current_signal_value: 0,
            },
        );
        if !inserted {
            return Err(AdapterError::SurfaceAlreadyRegistered { surface_id: id });
        }
        Ok(())
    }

    /// Drop a registered surface (either flavor). Pending guards keep
    /// the underlying `Arc<TimelineSemaphore>` and resource `Arc` alive;
    /// the next acquire returns [`AdapterError::SurfaceNotFound`].
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        self.surfaces.unregister(id).is_some()
    }

    /// Snapshot the registry size — primarily for tests and
    /// observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.len()
    }

    /// Power-user accessor: the registered pixel-buffer Arc for a
    /// buffer-flavored surface. Used by the carve-out test in
    /// the carve-out tests to call
    /// `export_opaque_fd_memory()` on the underlying buffer; future
    /// cdylib work will route this through the surface-share service
    /// instead. Returns `None` when the surface isn't registered OR
    /// when the surface was registered with an image-flavored
    /// registration (use [`Self::surface_texture`] for those).
    pub fn surface_pixel_buffer(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::Buffer>> {
        self.surfaces
            .with(id, |state| match &state.resource {
                SurfaceResource::Buffer { pixel_buffer } => Some(Arc::clone(pixel_buffer)),
                SurfaceResource::Image { .. } => None,
            })
            .flatten()
    }

    /// Power-user accessor: the registered texture Arc for an
    /// image-flavored surface. Symmetric counterpart to
    /// [`Self::surface_pixel_buffer`]. Returns `None` when the surface
    /// isn't registered OR when the surface was registered with a
    /// buffer-flavored registration.
    pub fn surface_texture(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::Texture>> {
        self.surfaces
            .with(id, |state| match &state.resource {
                SurfaceResource::Image { texture } => Some(Arc::clone(texture)),
                SurfaceResource::Buffer { .. } => None,
            })
            .flatten()
    }

    /// Power-user accessor: the registered `produce_done` timeline Arc
    /// for a surface (either flavor). Used by the carve-out test to call
    /// `export_opaque_fd()` on the underlying timeline; cdylib work
    /// routes this through the surface-share service.
    pub fn surface_produce_done(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>> {
        self.surfaces
            .with(id, |state| Arc::clone(&state.produce_done))
    }

    /// Power-user accessor: the registered `consume_done` timeline Arc
    /// for a surface (either flavor). Symmetric counterpart to
    /// [`Self::surface_produce_done`].
    pub fn surface_consume_done(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>> {
        self.surfaces
            .with(id, |state| Arc::clone(&state.consume_done))
    }
}
