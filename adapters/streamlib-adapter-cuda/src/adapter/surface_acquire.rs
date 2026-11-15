// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The texture and surface acquires and the registry-lock snapshots they take.

use std::sync::Arc;

use streamlib_consumer_rhi::{VulkanRhiBuffer, VulkanRhiDevice, VulkanTextureLike};
use streamlib_surface_adapter::{AdapterError, StreamlibSurface};

use crate::state::SurfaceResource;
use crate::view::{CudaSurfaceGuard, CudaTextureGuard};

use super::{
    CudaSurfaceAdapter, ImageReadAcquired, ImageWriteAcquired, ReadAcquired, WriteAcquired,
};

impl<D: VulkanRhiDevice> CudaSurfaceAdapter<D> {
    pub(super) fn try_begin_read(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces
            .try_begin_read(id, |state| match &state.resource {
                // Consumer-side acquire waits on `produce_done` — the
                // peer-timeline's `current_value()` snapshot is taken inside
                // `finalize_read` (outside the registry lock).
                SurfaceResource::Buffer { pixel_buffer } => Ok(ReadAcquired {
                    peer_timeline: Arc::clone(&state.produce_done),
                    buffer: pixel_buffer.buffer(),
                    size: pixel_buffer.size(),
                }),
                SurfaceResource::Image { .. } => Err(AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_read: surface_id={id} was registered as an image-flavored \
                     surface; use acquire_texture (for cudaTextureObject_t) instead"
                    ),
                }),
            })
    }

    pub(super) fn try_begin_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces
            .try_begin_write(id, |state| match &state.resource {
                // Producer-side acquire waits on `consume_done` — confirms
                // the consumer has drained prior content before re-write.
                SurfaceResource::Buffer { pixel_buffer } => Ok(WriteAcquired {
                    peer_timeline: Arc::clone(&state.consume_done),
                    buffer: pixel_buffer.buffer(),
                    size: pixel_buffer.size(),
                }),
                SurfaceResource::Image { .. } => Err(AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_write: surface_id={id} was registered as an image-flavored \
                     surface; use acquire_surface (for cudaSurfaceObject_t) instead"
                    ),
                }),
            })
    }

    pub(super) fn try_begin_image_read(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ImageReadAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces
            .try_begin_read(id, |state| match &state.resource {
                SurfaceResource::Image { texture } => {
                    let image = texture.image().ok_or_else(|| AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_texture: surface_id={id} texture has no VkImage (placeholder?)"
                    ),
                })?;
                    Ok(ImageReadAcquired {
                        peer_timeline: Arc::clone(&state.produce_done),
                        image,
                        width: texture.width(),
                        height: texture.height(),
                        format: texture.format(),
                    })
                }
                SurfaceResource::Buffer { .. } => Err(AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_texture: surface_id={id} was registered as a buffer-flavored \
                     surface; use acquire_read (for DLPack capsules) instead"
                    ),
                }),
            })
    }

    pub(super) fn try_begin_image_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ImageWriteAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces
            .try_begin_write(id, |state| match &state.resource {
                SurfaceResource::Image { texture } => {
                    let image = texture.image().ok_or_else(|| AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_surface: surface_id={id} texture has no VkImage (placeholder?)"
                    ),
                })?;
                    Ok(ImageWriteAcquired {
                        peer_timeline: Arc::clone(&state.consume_done),
                        image,
                        width: texture.width(),
                        height: texture.height(),
                        format: texture.format(),
                    })
                }
                SurfaceResource::Buffer { .. } => Err(AdapterError::BackendRejected {
                    reason: format!(
                        "acquire_surface: surface_id={id} was registered as a buffer-flavored \
                     surface; use acquire_write (for DLPack capsules) instead"
                    ),
                }),
            })
    }

    /// Blocking acquire of read-only image access — the
    /// `cudaTextureObject_t` side of CUDA's texture interop.
    /// Returns a [`CudaTextureGuard`] scoped to the acquire window;
    /// drop releases the read holder and signals the timeline.
    pub fn acquire_texture<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<CudaTextureGuard<'g, D>, AdapterError> {
        let acquired = match self.try_begin_image_read(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: "writer".to_string(),
                });
            }
        };
        let view = self.finalize_image_read(surface.id, acquired)?;
        Ok(CudaTextureGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        })
    }

    /// Non-blocking variant of [`Self::acquire_texture`]. Returns
    /// `Ok(None)` on contention rather than blocking.
    pub fn try_acquire_texture<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<CudaTextureGuard<'g, D>>, AdapterError> {
        let acquired = match self.try_begin_image_read(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let view = self.finalize_image_read(surface.id, acquired)?;
        Ok(Some(CudaTextureGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        }))
    }

    /// Blocking acquire of read-write image access — the
    /// `cudaSurfaceObject_t` side of CUDA's texture interop. Returns
    /// a [`CudaSurfaceGuard`] scoped to the acquire window; drop
    /// releases the write hold and signals the timeline.
    pub fn acquire_surface<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<CudaSurfaceGuard<'g, D>, AdapterError> {
        let acquired = match self.try_begin_image_write(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: self.surfaces.describe_contention(surface.id),
                });
            }
        };
        let view = self.finalize_image_write(surface.id, acquired)?;
        Ok(CudaSurfaceGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        })
    }

    /// Non-blocking variant of [`Self::acquire_surface`].
    pub fn try_acquire_surface<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<CudaSurfaceGuard<'g, D>>, AdapterError> {
        let acquired = match self.try_begin_image_write(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let view = self.finalize_image_write(surface.id, acquired)?;
        Ok(Some(CudaSurfaceGuard {
            adapter: self,
            surface_id: surface.id,
            view,
        }))
    }
}
