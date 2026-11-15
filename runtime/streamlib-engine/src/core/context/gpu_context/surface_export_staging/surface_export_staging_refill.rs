// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Refilling a staging from its surface's current frame, including the display-importable image a viewer reads.

use super::*;

impl GpuContext {
    /// Copy `surface_id`'s current pixels into the staging buffer.
    /// Resolves the source fresh — a rotating producer's latest
    /// registration, not a snapshot — and refuses an id whose frame the
    /// producer has recycled, because the slot's bytes are then somebody
    /// else's picture. The copy is retired on the host before this
    /// returns; the answer is the timeline value a consumer in another
    /// process waits for, `None` where the device exports no timeline.
    pub fn refill_surface_export_staging(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        let source = self.resolve_refill_source(staging, surface_id)?;
        self.submit_staging_copy_and_wait(
            staging,
            surface_id,
            FrameThisStagingHoldsAfterTheCopy::TheFrameJustReadIn(surface_id),
            |recorder| Self::record_refill(staging, &source, None, recorder),
        )
    }

    /// [`Self::refill_surface_export_staging`] for a staging a viewer reads,
    /// with the layout the frame's message states read in place of the
    /// registration's when present.
    #[cfg(target_os = "linux")]
    pub(super) fn refill_a_staging_a_viewer_reads(
        &self,
        staging: &SurfaceExportStaging,
        frame_id: &str,
        layout_the_producer_left_the_frame_in: Option<VulkanLayout>,
    ) -> Result<()> {
        let source = self.resolve_refill_source(staging, frame_id)?;
        self.submit_staging_copy_and_wait(
            staging,
            frame_id,
            FrameThisStagingHoldsAfterTheCopy::TheFrameJustReadIn(frame_id),
            |recorder| {
                Self::record_refill(
                    staging,
                    &source,
                    layout_the_producer_left_the_frame_in,
                    recorder,
                )
            },
        )?;
        Ok(())
    }

    /// The guards every refill runs, plus the freshly-resolved source.
    /// Resolves fresh — a rotating producer's latest registration, not a
    /// snapshot — and refuses an id whose frame the producer has
    /// recycled, because the slot's bytes are then somebody else's
    /// picture.
    fn resolve_refill_source(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<ResolvedSurfaceBacking> {
        Self::refuse_a_surface_this_staging_does_not_export(staging, surface_id)?;
        self.refuse_a_retired_frame_id(surface_id)?;
        self.resolve_device_export_source(surface_id)
    }

    /// Record one surface→staging copy, answering the layout update the
    /// submission settles. Geometry is re-checked here rather than at
    /// resolve: a re-registration between the two would otherwise size
    /// the copy from a shape the staging no longer has.
    fn record_refill(
        staging: &SurfaceExportStaging,
        source: &ResolvedSurfaceBacking,
        layout_the_producer_left_the_frame_in: Option<VulkanLayout>,
        recorder: &mut RhiCommandRecorder,
    ) -> Result<Option<TextureLayoutSettledByThisCopy>> {
        let staging_buffer = match &staging.staging_memory {
            SurfaceExportStagingMemory::OpaqueFdBuffer(staging_buffer) => staging_buffer,
            SurfaceExportStagingMemory::DisplayImportableImage(staging_image) => {
                Self::record_display_importable_refill(
                    staging,
                    staging_image,
                    source,
                    layout_the_producer_left_the_frame_in,
                    recorder,
                )?;
                return Ok(None);
            }
        };
        match source {
            ResolvedSurfaceBacking::RegisteredTexture(registration) => {
                let texture = registration.texture();
                Self::refuse_a_texture_this_staging_cannot_copy_with(
                    staging,
                    &staging.source_surface_key,
                    texture,
                    SurfaceExportStagingTextureCopyDirection::RefillIntoStaging,
                )?;
                let settled_layout = recorder.record_image_read_as_transfer_source(
                    texture,
                    layout_the_producer_left_the_frame_in
                        .unwrap_or_else(|| registration.current_layout()),
                    |recorder| {
                        recorder.record_copy_image_to_buffer(
                            texture,
                            VulkanLayout::TRANSFER_SRC_OPTIMAL,
                            staging_buffer.as_ref(),
                            ImageCopyRegion::tightly_packed(texture.width(), texture.height()),
                        )
                    },
                )?;
                Ok(Some(TextureLayoutSettledByThisCopy {
                    registration: registration.clone(),
                    settled_layout,
                }))
            }
            ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) => {
                if pixel_buffer.plane_size(0) != staging.staging_byte_size {
                    return Err(Error::GpuError(format!(
                        "surface {} now resolves to a {}-byte buffer; the cached staging is \
                         sized for {} — resolve the surface again",
                        staging.source_surface_key,
                        pixel_buffer.plane_size(0),
                        staging.staging_byte_size,
                    )));
                }
                recorder.record_buffer_barrier_before_a_transfer_read(pixel_buffer)?;
                recorder.record_copy_buffer_to_buffer(
                    pixel_buffer,
                    staging_buffer.as_ref(),
                    staging.staging_byte_size,
                )?;
                Ok(None)
            }
        }
    }

    /// Record one surface→display-image copy and release the image to the
    /// external queue family in GENERAL. The image's old contents are
    /// discarded (every pixel is rewritten), so it is transitioned from
    /// UNDEFINED and needs no acquire back from the importer.
    ///
    /// A texture source's layout is left where it was: an image-to-image
    /// copy refuses a source that was never written, and returns any other
    /// to its own layout.
    fn record_display_importable_refill(
        staging: &SurfaceExportStaging,
        staging_image: &Texture,
        source: &ResolvedSurfaceBacking,
        layout_the_producer_left_the_frame_in: Option<VulkanLayout>,
        recorder: &mut RhiCommandRecorder,
    ) -> Result<()> {
        let staging_image_layout = match source {
            ResolvedSurfaceBacking::RegisteredTexture(registration) => {
                let texture = registration.texture();
                Self::refuse_a_texture_this_staging_cannot_copy_with(
                    staging,
                    &staging.source_surface_key,
                    texture,
                    SurfaceExportStagingTextureCopyDirection::RefillIntoStaging,
                )?;
                recorder.record_copy_image_to_image(
                    texture,
                    layout_the_producer_left_the_frame_in
                        .unwrap_or_else(|| registration.current_layout()),
                    staging_image,
                    VulkanLayout::UNDEFINED,
                )?
            }
            ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) => {
                if pixel_buffer.plane_size(0) != staging.staging_byte_size {
                    return Err(Error::GpuError(format!(
                        "surface {} now resolves to a {}-byte buffer; its display staging \
                         holds {}x{} tightly packed pixels ({} bytes) — resolve the surface again",
                        staging.source_surface_key,
                        pixel_buffer.plane_size(0),
                        staging.surface_width,
                        staging.surface_height,
                        staging.staging_byte_size,
                    )));
                }
                recorder.record_buffer_barrier_before_a_transfer_read(pixel_buffer)?;
                recorder.record_image_write_as_transfer_destination(
                    staging_image,
                    VulkanLayout::UNDEFINED,
                    |recorder| {
                        recorder.record_copy_buffer_to_image(
                            pixel_buffer,
                            staging_image,
                            VulkanLayout::TRANSFER_DST_OPTIMAL,
                            ImageCopyRegion::tightly_packed(
                                staging.surface_width,
                                staging.surface_height,
                            ),
                        )
                    },
                )?
            }
        };
        recorder
            .record_image_release_to_the_external_queue_family(staging_image, staging_image_layout)
    }
}
