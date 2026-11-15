// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Copying a staging back into its surface, after refusing a write of a frame the staging does not hold.

use super::*;

impl GpuContext {
    /// Copy a written staging buffer back into its source surface, so an
    /// in-place consumer-side edit is visible to every other holder.
    ///
    /// The destination is the backing kind the staging was minted over:
    /// a pooled allocation for a pool-backed staging — only when it is
    /// the surface's sole backing, because a frame its producer also
    /// published as a registered texture is still the producer's — or
    /// the registered texture itself for a texture-backed staging (a
    /// kernel output), via buffer→image plus the same layout dance the
    /// refill's read direction records.
    ///
    /// The pool arm is tested twice: the staging's `writable` is the
    /// capability the consumer was told about when it opened the export,
    /// and the registration test is re-run here. The two can disagree — a
    /// producer can register a texture over the slot after the export
    /// was opened, and while a recycled *frame id* is refused above, a
    /// registration alone advances no generation.
    ///
    /// That live test narrows the window; it does not close it. Nothing
    /// holds the registration and this write-back together, so a producer
    /// registering between the test and the submit still gets its slot
    /// written. Closing it takes the checkout lease, whose claim is
    /// atomic against pool acquire by construction — which a test here
    /// cannot be.
    pub fn copy_surface_export_staging_back_to_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        let destination = self.resolve_write_back_destination(
            staging,
            surface_id,
            StagedWriteBackCoverage::AnEditOfTheFrameReadIn,
        )?;
        self.submit_staging_copy_and_wait(
            staging,
            surface_id,
            FrameThisStagingHoldsAfterTheCopy::WhateverItAlreadyHeld,
            |recorder| Self::record_write_back(staging, &destination, recorder),
        )
    }

    /// Copy a staging the consumer wrote in full into its source surface,
    /// replacing every pixel the surface held.
    ///
    /// The upload twin of [`Self::copy_surface_export_staging_back_to_surface`]:
    /// every guard but the read-before-write rule, which protects an edit of
    /// the frame read in and has nothing to protect when no byte of the
    /// frame survives. Afterwards the staging holds this frame.
    pub fn copy_surface_export_staging_over_every_pixel_of_surface(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<Option<u64>> {
        let destination = self.resolve_write_back_destination(
            staging,
            surface_id,
            StagedWriteBackCoverage::EveryPixelOfTheFrame,
        )?;
        self.submit_staging_copy_and_wait(
            staging,
            surface_id,
            FrameThisStagingHoldsAfterTheCopy::TheFrameJustReadIn(surface_id),
            |recorder| Self::record_write_back(staging, &destination, recorder),
        )
    }

    /// Refuse publishing a staging that holds no frame, or another frame
    /// than the one named — the read-before-write rule both backing
    /// kinds share, because a write-back is an edit *of the frame that
    /// was read in* whichever memory it lands in.
    pub(super) fn refuse_a_write_of_a_frame_this_staging_does_not_hold(
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<()> {
        match staging.frame_last_read_into_this_staging.lock().as_deref() {
            None => Err(Error::GpuError(format!(
                "surface {surface_id}'s export staging has never been read into; publishing \
                 it would write uninitialised memory over a live frame — read the frame in \
                 before publishing an edit of it"
            ))),
            Some(read_in) if read_in != surface_id => Err(Error::GpuError(format!(
                "surface {surface_id}'s export staging currently holds frame {read_in}; \
                 publishing it would write that frame's pixels over this one — read this \
                 frame in before publishing an edit of it"
            ))),
            Some(_) => Ok(()),
        }
    }

    /// The guards every write-back runs, plus the backing the staged
    /// edit publishes into — the kind the staging was minted over.
    fn resolve_write_back_destination(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
        coverage: StagedWriteBackCoverage,
    ) -> Result<ResolvedWriteBackDestination> {
        if staging.residency == SurfaceExportStagingResidency::DisplayImportable {
            return Err(Error::GpuError(format!(
                "surface {surface_id}'s display staging is a viewer's copy of the frame; nothing \
                 is written back from it"
            )));
        }
        Self::refuse_a_surface_this_staging_does_not_export(staging, surface_id)?;
        self.refuse_a_retired_frame_id(surface_id)?;
        match staging.backing_kind_at_mint {
            SurfaceExportStagingBackingKindAtMint::PooledPixelBuffer {
                pooled_allocation_was_the_only_backing_at_mint,
            } => {
                if !pooled_allocation_was_the_only_backing_at_mint {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id}'s export is read-only: this frame is a pool \
                         member its producer still owns, and a pooled allocation publishes an \
                         edit only when it is the surface's sole backing"
                    )));
                }
                if self
                    .producer_registered_texture_for_surface_id(surface_id)
                    .is_some()
                {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} has gained a producer's registered texture since \
                         its export was opened read-write; the pooled allocation now backs \
                         someone else's frame and the staged edit cannot be published into it"
                    )));
                }
                coverage.refuse_a_write_this_staging_does_not_cover(staging, surface_id)?;
                let ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) =
                    self.resolve_device_export_source(surface_id)?
                else {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} no longer resolves to a pooled allocation; the \
                         staged edit has nowhere to be published"
                    )));
                };
                if pixel_buffer.plane_size(0) != staging.staging_byte_size {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} now resolves to a {}-byte buffer; the staged \
                         write is sized for {} — the edit cannot be published",
                        pixel_buffer.plane_size(0),
                        staging.staging_byte_size,
                    )));
                }
                Ok(ResolvedWriteBackDestination::PixelBuffer(pixel_buffer))
            }
            SurfaceExportStagingBackingKindAtMint::RegisteredTexture {
                texture_takes_a_recorded_copy_in,
            } => {
                if !texture_takes_a_recorded_copy_in {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id}'s export is read-only: its texture was allocated \
                         without \"copy_dst\" usage, so no staged edit may be copied into it — \
                         acquire the texture with that usage to write it through an export"
                    )));
                }
                coverage.refuse_a_write_this_staging_does_not_cover(staging, surface_id)?;
                let ResolvedSurfaceBacking::RegisteredTexture(registration) =
                    self.resolve_device_export_source(surface_id)?
                else {
                    return Err(Error::GpuError(format!(
                        "surface {surface_id} has gained a pooled backing since its \
                         texture-backed export was opened; the staged edit's destination is no \
                         longer the texture the consumer read — resolve the surface again"
                    )));
                };
                Self::refuse_a_texture_this_staging_cannot_copy_with(
                    staging,
                    surface_id,
                    registration.texture(),
                    SurfaceExportStagingTextureCopyDirection::WriteBackIntoSurface,
                )?;
                Ok(ResolvedWriteBackDestination::RegisteredTexture(
                    registration,
                ))
            }
        }
    }

    /// Record one staging→surface copy, into whichever backing kind the
    /// staging was minted over, answering the layout update the
    /// submission settles.
    fn record_write_back(
        staging: &SurfaceExportStaging,
        destination: &ResolvedWriteBackDestination,
        recorder: &mut RhiCommandRecorder,
    ) -> Result<Option<TextureLayoutSettledByThisCopy>> {
        let staging_buffer = staging.staging_buffer()?;
        match destination {
            ResolvedWriteBackDestination::PixelBuffer(pixel_buffer) => {
                recorder.record_copy_buffer_to_buffer(
                    staging_buffer.as_ref(),
                    pixel_buffer,
                    staging.staging_byte_size,
                )?;
                recorder.record_buffer_barrier_publishing_a_transfer_write(pixel_buffer)?;
                Ok(None)
            }
            ResolvedWriteBackDestination::RegisteredTexture(registration) => {
                let texture = registration.texture();
                let settled_layout = recorder.record_image_write_as_transfer_destination(
                    texture,
                    registration.current_layout(),
                    |recorder| {
                        recorder.record_copy_buffer_to_image(
                            staging_buffer.as_ref(),
                            texture,
                            VulkanLayout::TRANSFER_DST_OPTIMAL,
                            ImageCopyRegion::tightly_packed(texture.width(), texture.height()),
                        )
                    },
                )?;
                Ok(Some(TextureLayoutSettledByThisCopy {
                    registration: registration.clone(),
                    settled_layout,
                }))
            }
        }
    }
}
