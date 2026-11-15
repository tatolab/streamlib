// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Opening a surface's export staging, sharing it with another process, exporting its OPAQUE_FD, and evicting it with its surface.

use super::*;

impl GpuContext {
    /// The export staging for `surface_id` at `residency`, created on
    /// first ask and cached on this context — it dies with the context and
    /// is evicted by [`Self::unregister_texture`], never by a rotating
    /// re-registration, which the per-refill source resolve absorbs.
    ///
    /// Keyed by residency as well as surface, so asking for a CPU-readable
    /// staging never hands back the device-local one a CUDA consumer is
    /// already importing.
    pub fn surface_export_staging(
        &self,
        surface_id: &str,
        residency: SurfaceExportStagingResidency,
    ) -> Result<Arc<SurfaceExportStaging>> {
        self.refuse_a_retired_frame_id(surface_id)?;
        let source_surface_key = crate::core::rhi::pool_slot_key_of_surface_id(surface_id);
        if let Some(existing) = self
            .surface_export_stagings
            .lock()
            .get(source_surface_key)
            .and_then(|by_residency| by_residency.get(&residency))
        {
            return Ok(Arc::clone(existing));
        }

        let source = self.resolve_device_export_source(surface_id)?;
        let source_takes_a_write_back =
            self.resolved_backing_takes_a_write_back(surface_id, &source);
        let shape = match source {
            ResolvedSurfaceBacking::RegisteredTexture(registration) => {
                let texture = registration.texture();
                let pixel_format = export_pixel_shape_for_texture(texture.format())?;
                let export_bytes_per_pixel = export_bytes_per_pixel_for_pixel_format(pixel_format)?;
                let (surface_width, surface_height) = (texture.width(), texture.height());
                SurfaceExportStagingShape {
                    staging_byte_size: u64::from(surface_width)
                        * u64::from(surface_height)
                        * u64::from(export_bytes_per_pixel),
                    surface_width,
                    surface_height,
                    pixel_format,
                    backing_kind_at_mint:
                        SurfaceExportStagingBackingKindAtMint::RegisteredTexture {
                            texture_takes_a_recorded_copy_in: source_takes_a_write_back,
                        },
                    display_importable_image_format: Some(texture.format()),
                }
            }
            ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) => {
                let pixel_format = pixel_buffer.format();
                export_bytes_per_pixel_for_pixel_format(pixel_format)?;
                let staging_byte_size = pixel_buffer.plane_size(0);
                if staging_byte_size == 0 {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} resolves to a zero-byte plane; nothing to export"
                    )));
                }
                SurfaceExportStagingShape {
                    staging_byte_size,
                    surface_width: pixel_buffer.width,
                    surface_height: pixel_buffer.height,
                    pixel_format,
                    backing_kind_at_mint:
                        SurfaceExportStagingBackingKindAtMint::PooledPixelBuffer {
                            pooled_allocation_was_the_only_backing_at_mint:
                                source_takes_a_write_back,
                        },
                    display_importable_image_format:
                        display_importable_image_format_of_pixel_format(pixel_format),
                }
            }
        };

        let vulkan_device = self.device().vulkan_device();
        // The one line the residency decides. Both buffers are OPAQUE_FD so
        // their check-out and import path is identical; only the memory
        // properties differ, and with them who can read it.
        let staging_memory = match residency {
            SurfaceExportStagingResidency::DeviceLocal => {
                SurfaceExportStagingMemory::OpaqueFdBuffer(Arc::new(
                    HostVulkanBuffer::new_opaque_fd_export_device_local(
                        vulkan_device,
                        shape.staging_byte_size,
                    )?,
                ))
            }
            SurfaceExportStagingResidency::HostVisible => {
                SurfaceExportStagingMemory::OpaqueFdBuffer(Arc::new(
                    HostVulkanBuffer::new_opaque_fd_export_host_cached(
                        vulkan_device,
                        shape.staging_byte_size,
                    )?,
                ))
            }
            #[cfg(target_os = "linux")]
            SurfaceExportStagingResidency::DisplayImportable => {
                SurfaceExportStagingMemory::DisplayImportableImage(
                    self.acquire_a_display_importable_staging_image(surface_id, &shape)?,
                )
            }
            #[cfg(not(target_os = "linux"))]
            SurfaceExportStagingResidency::DisplayImportable => {
                return Err(Error::NotSupported(format!(
                    "surface {surface_id}: a display's staging is a DMA-BUF image, which only \
                     Linux allocates; an IOSurface frame crosses to a display as it is"
                )));
            }
        };
        // A device that exports no timeline (the software device) refuses an
        // exportable one at creation; its copies are retired on the host
        // before they return, so an in-process timeline is all it needs. A
        // display waits on no Vulkan timeline, so its staging's stays here.
        let refill_done_timeline_crosses_processes = vulkan_device.supports_semaphore_export()
            && residency != SurfaceExportStagingResidency::DisplayImportable;
        let refill_done_timeline = if refill_done_timeline_crosses_processes {
            self.create_exportable_timeline_semaphore(0)?
        } else {
            self.create_timeline_semaphore(0)?
        };
        let refill_submission = Mutex::new(RefillSubmission {
            recorder: self.create_command_recorder("surface_export_staging_refill")?,
            next_signal_value: 1,
        });

        let staging = Arc::new(SurfaceExportStaging {
            source_surface_key: source_surface_key.to_string(),
            residency,
            staging_memory,
            exporting_device_uuid: vulkan_device.physical_device_uuid(),
            refill_done_timeline,
            refill_done_timeline_crosses_processes,
            refill_submission,
            staging_byte_size: shape.staging_byte_size,
            surface_width: shape.surface_width,
            surface_height: shape.surface_height,
            pixel_format: Some(shape.pixel_format),
            backing_kind_at_mint: shape.backing_kind_at_mint,
            frame_last_read_into_this_staging: Mutex::new(None),
            #[cfg(target_os = "linux")]
            surface_share_registration_id: Mutex::new(None),
        });
        // Double-check under the insert lock: a concurrent asker for the
        // same slot and residency may have published one while this thread
        // was allocating. The loser's freshly-built staging drops here
        // rather than replacing the entry other holders (and the wheel's
        // import memo) key on.
        let mut stagings = self.surface_export_stagings.lock();
        let by_residency = stagings.entry(source_surface_key.to_string()).or_default();
        if let Some(published) = by_residency.get(&residency) {
            return Ok(Arc::clone(published));
        }
        by_residency.insert(residency, Arc::clone(&staging));
        Ok(staging)
    }

    /// A DMA-BUF image at the source's extent, in the colour format of its
    /// byte order, allocated through the render-target probe — refused where
    /// the probe settles on LINEAR, which a display on NVIDIA shows as black.
    #[cfg(target_os = "linux")]
    fn acquire_a_display_importable_staging_image(
        &self,
        surface_id: &str,
        shape: &SurfaceExportStagingShape,
    ) -> Result<Texture> {
        let image_format = shape.display_importable_image_format.ok_or_else(|| {
            Error::GpuError(format!(
                "surface {surface_id} is {:?}, which no single-plane colour image carries, so a \
                 display cannot be handed a copy of it",
                shape.pixel_format
            ))
        })?;
        let staging_image = self.acquire_render_target_dma_buf_image(
            shape.surface_width,
            shape.surface_height,
            image_format,
        )?;
        if !staging_image
            .vulkan_inner()
            .is_a_dma_buf_image_with_a_tiled_drm_format_modifier()
        {
            return Err(Error::GpuError(format!(
                "this device allocates {image_format:?} render targets only with the LINEAR \
                 modifier, which a display imports as black; surface {surface_id} cannot be \
                 shown in a viewer on this device"
            )));
        }
        Ok(staging_image)
    }

    /// The surface-share id of memory holding the frame `frame_id` names the
    /// way a viewer reads it by `read_by`. A display import answers the
    /// frame's own pool slot when that is already a DMA-BUF image with a
    /// tiled modifier, and otherwise the slot's display staging; a CPU read
    /// answers the slot's host-visible staging. A staging is refilled from
    /// this frame on the GPU unless it already holds it, and every copy
    /// retires on the host before this answers.
    ///
    /// `layout_the_producer_left_the_frame_in` is the layout the frame's
    /// message states, read in place of the registration's when present.
    #[cfg(target_os = "linux")]
    pub(crate) fn surface_share_id_a_viewer_reads_the_frame_through(
        &self,
        frame_id: &str,
        read_by: streamlib_surface_client::ViewedFrameReadBy,
        layout_the_producer_left_the_frame_in: Option<VulkanLayout>,
    ) -> Result<String> {
        self.refuse_a_retired_frame_id(frame_id)?;
        let residency = match read_by {
            streamlib_surface_client::ViewedFrameReadBy::DisplayImport => {
                if let ResolvedSurfaceBacking::RegisteredTexture(registration) =
                    self.resolve_device_export_source(frame_id)?
                    && registration
                        .texture()
                        .vulkan_inner()
                        .is_a_dma_buf_image_with_a_tiled_drm_format_modifier()
                {
                    return Ok(crate::core::rhi::pool_slot_key_of_surface_id(frame_id).to_string());
                }
                SurfaceExportStagingResidency::DisplayImportable
            }
            streamlib_surface_client::ViewedFrameReadBy::CpuRead => {
                SurfaceExportStagingResidency::HostVisible
            }
        };
        let staging = self.surface_export_staging(frame_id, residency)?;
        let staging_already_holds_the_frame =
            staging.frame_last_read_into_this_staging.lock().as_deref() == Some(frame_id);
        if !staging_already_holds_the_frame {
            self.refill_a_staging_a_viewer_reads(
                &staging,
                frame_id,
                layout_the_producer_left_the_frame_in,
            )?;
        }
        let (shared_id, _) = self.share_surface_export_staging(&staging)?;
        Ok(shared_id)
    }

    /// Publish this staging and its refill timeline to the surface-share
    /// service, and answer with the id they are registered under.
    ///
    /// This is how a consumer one process away reaches the export: it
    /// checks the id out, receives the staging's OPAQUE_FD — and the
    /// timeline's fd where the device exports one — over SCM_RIGHTS,
    /// imports or maps the memory, and waits on the timeline for the value
    /// each refill returns. The
    /// published id is derived from the source surface's *and* the
    /// residency, so three registrations never collide — the source is
    /// already registered under its own id with its DMA-BUF planes, and
    /// the two residencies are separate allocations of their own.
    ///
    /// Registers at most once per staging; later calls answer with the
    /// same id.
    ///
    /// Answers with the pixel shape it validated as well as the id, so a
    /// caller building a consumer-side layout does not re-derive — and
    /// cannot disagree about — what this already refused without.
    #[cfg(target_os = "linux")]
    pub fn share_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(String, PixelFormat)> {
        let pixel_format = staging.pixel_format.ok_or_else(|| {
            Error::GpuError(format!(
                "the {} export staging for surface {} carries no pixel shape; a consumer would \
                 have no layout to import it under",
                staging.residency, staging.source_surface_key
            ))
        })?;
        let mut registration_id = staging.surface_share_registration_id.lock();
        if let Some(already_registered) = registration_id.as_ref() {
            return Ok((already_registered.clone(), pixel_format));
        }
        let surface_store = self.engines_own_surface_store().ok_or_else(|| {
            Error::GpuError(
                "this runtime has no surface-share service, so a surface export cannot reach \
                 another process"
                    .into(),
            )
        })?;
        let shared_id = format!(
            "{}-{}",
            staging.source_surface_key,
            staging.residency.surface_share_id_suffix()
        );
        // `host_inner`-direct: the plugin ABI is not grown a vtable slot for
        // this surface. A cdylib caller panics at the guard.
        match &staging.staging_memory {
            SurfaceExportStagingMemory::OpaqueFdBuffer(staging_buffer) => {
                surface_store.host_inner().register_surface_export_staging(
                    &shared_id,
                    staging_buffer,
                    staging.staging_byte_size,
                    staging.surface_width,
                    staging.surface_height,
                    pixel_format,
                    staging.exporting_device_uuid,
                    staging
                        .refill_done_timeline_another_process_waits_on()
                        .map(Arc::as_ref),
                )?
            }
            // Every refill releases the image to the external queue family
            // in GENERAL, so that is the layout an importer finds it in.
            SurfaceExportStagingMemory::DisplayImportableImage(staging_image) => surface_store
                .host_inner()
                .register_texture(&shared_id, staging_image, None, None, VulkanLayout::GENERAL)?,
        }
        *registration_id = Some(shared_id.clone());
        Ok((shared_id, pixel_format))
    }

    /// Drop every cached export staging for `surface_id` — both
    /// residencies, because the surface they staged is going away and
    /// neither survives it. Outstanding consumers keep theirs alive
    /// through their own `Arc`s.
    ///
    /// A staging that was published to the surface-share service is released
    /// from it here too: the service refuses duplicate ids, so leaving the
    /// dead entry behind would permanently block a later staging for a reused
    /// surface id from ever being shared — and the service would keep holding
    /// dups of an fd pair nothing refills.
    pub(crate) fn evict_surface_export_stagings(&self, surface_id: &str) {
        // One removal takes every residency: they are the surface's inner
        // map, and the surface they stage is going away.
        let evicted = self
            .surface_export_stagings
            .lock()
            .remove(crate::core::rhi::pool_slot_key_of_surface_id(surface_id));
        for staging in evicted.into_iter().flat_map(HashMap::into_values) {
            if let Some(shared_id) = staging.surface_share_registration_id.lock().take()
                && let Some(surface_store) = self.engines_own_surface_store()
            {
                // Best-effort: the service also releases everything with the
                // connection, so a failure here is deferred cleanup, not a leak
                // for the life of the process.
                if let Err(release_failure) = surface_store.host_inner().release(&shared_id) {
                    tracing::debug!(
                        %shared_id,
                        %release_failure,
                        "releasing an evicted surface export staging from surface-share failed"
                    );
                }
            }
        }
    }

    /// Export the staging buffer's OPAQUE_FD plus byte size and the
    /// exporting device's UUID — what an importer needs at either
    /// residency, and the CUDA import triple at `DeviceLocal`. The fd
    /// transfers to the caller.
    pub fn export_surface_export_staging_opaque_fd(
        &self,
        staging: &SurfaceExportStaging,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        let fd = staging.staging_buffer()?.export_opaque_fd_memory()?;
        Ok((fd, staging.staging_byte_size, staging.exporting_device_uuid))
    }
}
