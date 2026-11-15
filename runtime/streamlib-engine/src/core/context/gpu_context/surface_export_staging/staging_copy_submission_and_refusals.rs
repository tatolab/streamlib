// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Submitting a staging copy and waiting on it, publishing the layout it settled, and refusing a surface or texture a staging cannot copy with.

use super::*;

impl GpuContext {
    /// Record + submit one staging copy and wait (bounded) for it to
    /// land, returning the timeline value a consumer in another process
    /// waits for — `None` where the staging's timeline does not cross.
    ///
    /// The lock/begin/record/submit/wait choreography lives here once:
    /// the signal value must be drawn and submitted under the recorder
    /// lock (strictly-increasing timeline values), the lock must drop
    /// before the host wait, and any record failure must abort the
    /// recording — a recorder left mid-recording refuses every later
    /// `begin`, which would brick this surface's export for the life of
    /// the cache entry.
    pub(super) fn submit_staging_copy_and_wait(
        &self,
        staging: &SurfaceExportStaging,
        surface_id: &str,
        holds_after_this_copy: FrameThisStagingHoldsAfterTheCopy<'_>,
        record_copy: impl FnOnce(
            &mut RhiCommandRecorder,
        ) -> Result<Option<TextureLayoutSettledByThisCopy>>,
    ) -> Result<Option<u64>> {
        let mut submission = staging.refill_submission.lock();
        submission.recorder.begin()?;
        let texture_layout_settled_by_this_copy = match record_copy(&mut submission.recorder) {
            Ok(settled) => settled,
            Err(record_failure) => {
                submission.recorder.abort_recording();
                return Err(record_failure);
            }
        };
        let signal_value = submission.next_signal_value;
        submission.next_signal_value += 1;
        if let Err(submit_failure) = submission
            .recorder
            .submit_signaling_timeline(&staging.refill_done_timeline, signal_value)
        {
            submission.recorder.abort_recording();
            return Err(submit_failure);
        }
        // Under the recorder guard, with the submit that made it true: the
        // guard serialises submission order, and these copies retire in that
        // order on the one queue. Recorded outside it, two refills of the same
        // slot at different generations could land their copies in one order
        // and their bookkeeping in the other — leaving the staging holding one
        // frame while the field named another, which is the write the frame
        // check exists to refuse.
        if let FrameThisStagingHoldsAfterTheCopy::TheFrameJustReadIn(surface_id) =
            holds_after_this_copy
        {
            *staging.frame_last_read_into_this_staging.lock() = Some(surface_id.to_string());
        }
        if let Some(settled) = &texture_layout_settled_by_this_copy {
            settled.registration.update_layout(settled.settled_layout);
        }
        drop(submission);
        staging
            .refill_done_timeline
            .wait(signal_value, STAGING_REFILL_WAIT_TIMEOUT_NS)?;
        if let Some(settled) = texture_layout_settled_by_this_copy {
            self.publish_the_layout_a_staging_copy_settled(surface_id, &settled);
        }
        Ok(staging
            .refill_done_timeline_crosses_processes
            .then_some(signal_value))
    }

    /// Publish the layout a copy left a registered texture in to the
    /// surface-share service, so the next reader in another process acquires
    /// it from that layout; a surface the service does not hold is
    /// in-process only and is skipped.
    fn publish_the_layout_a_staging_copy_settled(
        &self,
        surface_id: &str,
        settled: &TextureLayoutSettledByThisCopy,
    ) {
        if let Some(surface_store) = self.surface_store()
            && let Err(publish_failure) =
                surface_store.update_image_layout(surface_id, settled.settled_layout)
        {
            tracing::debug!(
                "the layout a staging copy left surface {surface_id} in was not published to \
                 surface-share: {publish_failure}"
            );
        }
    }

    /// Refuse a surface whose pool slot this staging was not opened for.
    pub(super) fn refuse_a_surface_this_staging_does_not_export(
        staging: &SurfaceExportStaging,
        surface_id: &str,
    ) -> Result<()> {
        if crate::core::rhi::pool_slot_key_of_surface_id(surface_id) != staging.source_surface_key {
            return Err(Error::GpuError(format!(
                "surface {surface_id} is not the surface this {} export staging was opened for \
                 ({}); the staged copy would carry another surface's pixels",
                staging.residency, staging.source_surface_key
            )));
        }
        Ok(())
    }

    /// Refuse a registered texture this staging cannot legally copy with
    /// in `direction` — the guard both texture-arm copies share, run
    /// against the freshly resolved registration so a rotating producer's
    /// re-registration is judged, never a snapshot.
    ///
    /// Three refusals, in order of severity: an image whose usage forbids
    /// the copy (recording it anyway is a Vulkan spec violation the
    /// driver never reports); a format change (two same-size formats
    /// would silently swap channels); a geometry change (the staging is
    /// sized for the old extent).
    pub(super) fn refuse_a_texture_this_staging_cannot_copy_with(
        staging: &SurfaceExportStaging,
        surface_named: &str,
        texture: &crate::core::rhi::Texture,
        direction: SurfaceExportStagingTextureCopyDirection,
    ) -> Result<()> {
        let copy_is_legal = match direction {
            SurfaceExportStagingTextureCopyDirection::RefillIntoStaging => {
                texture.supports_transfer_read()
            }
            SurfaceExportStagingTextureCopyDirection::WriteBackIntoSurface => {
                texture.supports_transfer_write()
            }
        };
        if !copy_is_legal {
            let missing_usage = match direction {
                SurfaceExportStagingTextureCopyDirection::RefillIntoStaging => "copy_src",
                SurfaceExportStagingTextureCopyDirection::WriteBackIntoSurface => "copy_dst",
            };
            return Err(Error::GpuError(format!(
                "surface {surface_named}'s texture was allocated without {missing_usage:?} \
                 usage, so no copy may touch it — acquire the texture with that usage to \
                 export it"
            )));
        }
        let export_pixel_format = export_pixel_shape_for_texture(texture.format())?;
        // Format identity, not just byte size: two 4-byte formats (RGBA
        // vs BGRA) match on size, and staging the bytes of one under the
        // label of the other swaps channels.
        if Some(export_pixel_format) != staging.pixel_format {
            return Err(Error::GpuError(format!(
                "surface {surface_named} was re-registered as {:?}; {} presents {:?} — {}",
                export_pixel_format,
                direction.staged_subject(),
                staging.pixel_format,
                direction.remedy(),
            )));
        }
        let export_bytes_per_pixel = export_bytes_per_pixel_for_pixel_format(export_pixel_format)?;
        if u64::from(texture.width())
            * u64::from(texture.height())
            * u64::from(export_bytes_per_pixel)
            != staging.staging_byte_size
        {
            return Err(Error::GpuError(format!(
                "surface {surface_named} was re-registered with different geometry ({}x{}); {} \
                 is sized for {}x{} — {}",
                texture.width(),
                texture.height(),
                direction.staged_subject(),
                staging.surface_width,
                staging.surface_height,
                direction.remedy(),
            )));
        }
        Ok(())
    }
}
