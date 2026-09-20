// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading one frame's pixels out of this runtime's own memory, so a runtime
//! on another machine can be handed them.
//!
//! A surface id names a frame in this machine's pools and nothing anywhere
//! else, so the only thing that can cross is the picture itself. The frame is
//! claimed, copied out, and the claim released before the message is put: the
//! claim spans the copy alone, so a slow network never pins the producer's
//! pool slot — at cap the producer drops its own frame rather than waiting on
//! a peer.
//!
//! The copy door is [`SurfaceExportStaging`] at host-visible residency, whose
//! refill is a GPU copy into host-cached memory. Never a CPU memcpy out of a
//! pooled allocation's own mapping: that memory is write-combined, and a
//! 1080p RGBA read out of it cost 37 ms
//! (`docs/decisions/virtual-camera-sink.md`). Stagings are cached per pool
//! slot by the context, never built per frame — per-call pool churn after a
//! swapchain is a known driver failure.
//!
//! This whole door is Linux-only, because the staging is: a runtime on
//! another platform says once per port that its surface bags do not cross,
//! which is what every runtime said before the mesh carried a frame at all.

use std::sync::Arc;

use crate::core::runtime::mesh::a_frames_pixels_on_the_mesh::AMeshMessageCarryingAFramesPixels;
use crate::core::runtime::mesh::gpu_context_the_mesh_copies_frames_with::GpuContextTheMeshCopiesFramesWith;

/// Why one frame's pixels are not crossing, in the terms the port's log line
/// and its once-per-reason bookkeeping both use.
pub(super) enum WhyAFramesPixelsCannotCrossTheMesh {
    /// The runtime has no GPU context — it has not started, or it has
    /// stopped. Nothing is resolvable either way.
    ThisRuntimeHasNoGpuContextYet,
    /// The platform carries no export staging, so there is no door to copy a
    /// frame out through.
    ThisPlatformCannotReadAFrameOut,
    /// The producer has recycled the slot this id named, so its bytes are
    /// somebody else's picture now.
    TheProducerHasRecycledTheFrame(crate::core::Error),
    /// A format of more than one plane, which a one-buffer staging cannot
    /// read without dropping a plane.
    ItsFormatHasMoreThanOnePlane(String),
    /// Everything else the read refused, said in the words it refused with.
    ItsPixelsCannotBeReadOut(crate::core::Error),
}

impl WhyAFramesPixelsCannotCrossTheMesh {
    /// Which refusal this is, so a port says each of them once rather than
    /// saying the first one forever.
    pub(super) fn which_refusal_this_is(&self) -> &'static str {
        match self {
            Self::ThisRuntimeHasNoGpuContextYet => "no-gpu-context",
            Self::ThisPlatformCannotReadAFrameOut => "no-export-staging-on-this-platform",
            Self::TheProducerHasRecycledTheFrame(_) => "the-frame-was-recycled",
            Self::ItsFormatHasMoreThanOnePlane(_) => "more-than-one-plane",
            Self::ItsPixelsCannotBeReadOut(_) => "the-pixels-could-not-be-read",
        }
    }
}

impl std::fmt::Display for WhyAFramesPixelsCannotCrossTheMesh {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ThisRuntimeHasNoGpuContextYet => formatter.write_str(
                "this runtime has no GPU context, so it can resolve no surface of its own",
            ),
            Self::ThisPlatformCannotReadAFrameOut => formatter.write_str(
                "this platform carries no surface export staging, so there is no door to copy a \
                 frame out through",
            ),
            Self::TheProducerHasRecycledTheFrame(refusal) => {
                write!(formatter, "the producer has recycled the frame: {refusal}")
            }
            Self::ItsFormatHasMoreThanOnePlane(pixel_format) => write!(
                formatter,
                "its {pixel_format} backing is more than one plane, and a one-buffer export \
                 would carry only the first of them"
            ),
            Self::ItsPixelsCannotBeReadOut(refusal) => {
                write!(formatter, "its pixels could not be read out: {refusal}")
            }
        }
    }
}

/// Reads one port's frames out for the mesh, on the egress's own thread.
pub(super) struct ReadsAFramesPixelsOutForTheMesh {
    gpu_context_the_mesh_copies_frames_with: Arc<GpuContextTheMeshCopiesFramesWith>,
    /// The identity every claim this reader takes is charged to, minted on
    /// the first frame it reads. One per reader rather than one per frame, so
    /// dropping it gives back every claim it still holds — the backstop for a
    /// read that never reached its release.
    #[cfg(target_os = "linux")]
    claims_are_charged_to: Option<AFrameClaimHolderOnThisRuntime>,
}

impl ReadsAFramesPixelsOutForTheMesh {
    pub(super) fn reading_through(
        gpu_context_the_mesh_copies_frames_with: &Arc<GpuContextTheMeshCopiesFramesWith>,
    ) -> Self {
        Self {
            gpu_context_the_mesh_copies_frames_with: Arc::clone(
                gpu_context_the_mesh_copies_frames_with,
            ),
            #[cfg(target_os = "linux")]
            claims_are_charged_to: None,
        }
    }

    /// The message `bag_bytes` crosses as, with `surface_id`'s pixels behind
    /// it — or why they are not crossing.
    #[cfg(target_os = "linux")]
    pub(super) fn a_mesh_message_carrying_the_frame_this_bag_names(
        &mut self,
        surface_id: &str,
        bag_bytes: &[u8],
    ) -> std::result::Result<AMeshMessageCarryingAFramesPixels, WhyAFramesPixelsCannotCrossTheMesh>
    {
        use crate::core::context::SurfaceExportStagingResidency;
        use crate::core::runtime::mesh::a_frames_pixels_on_the_mesh::{
            AFramesPixelDescriptionOnTheMesh, a_mesh_message_carrying_a_frames_pixels,
        };

        let gpu_context = self
            .gpu_context_the_mesh_copies_frames_with
            .the_gpu_context_or_none()
            .ok_or(WhyAFramesPixelsCannotCrossTheMesh::ThisRuntimeHasNoGpuContextYet)?;

        if let Some(pixel_format) = a_backing_of_more_than_one_plane(&gpu_context, surface_id) {
            return Err(
                WhyAFramesPixelsCannotCrossTheMesh::ItsFormatHasMoreThanOnePlane(pixel_format),
            );
        }

        // Before the staging, and held until the copy is in the message: the
        // claim is what keeps the pool from rehanding this slot to its
        // producer while the copy reads it.
        let _claimed = self.claim_the_frame(&gpu_context, surface_id)?;

        let staging = gpu_context
            .surface_export_staging(surface_id, SurfaceExportStagingResidency::HostVisible)
            .map_err(a_read_refusal)?;
        gpu_context
            .refill_surface_export_staging(&staging, surface_id)
            .map_err(a_read_refusal)?;
        let staged_pixels = staging.staged_pixels_on_the_host().ok_or_else(|| {
            WhyAFramesPixelsCannotCrossTheMesh::ItsPixelsCannotBeReadOut(
                crate::core::Error::GpuError(format!(
                    "surface {surface_id}'s host-visible export staging is not mapped, so its \
                     pixels cannot be read out for the mesh"
                )),
            )
        })?;

        // The backing's own shape, never the bag's — a video bag names no
        // pixel format at all, and a receiver guessing one would hand the
        // wrong channel order downstream and never say so.
        let description = AFramesPixelDescriptionOnTheMesh {
            pixel_format: staging.pixel_format().ok_or_else(|| {
                WhyAFramesPixelsCannotCrossTheMesh::ItsPixelsCannotBeReadOut(
                    crate::core::Error::GpuError(format!(
                        "surface {surface_id}'s export staging carries no pixel shape, so a \
                         reading runtime would have no format to rebuild it under"
                    )),
                )
            })?,
            width: staging.surface_width(),
            height: staging.surface_height(),
            pixel_byte_length: staging.staging_byte_size(),
        };
        if staged_pixels.len() as u64 != description.pixel_byte_length {
            return Err(
                WhyAFramesPixelsCannotCrossTheMesh::ItsPixelsCannotBeReadOut(
                    crate::core::Error::GpuError(format!(
                        "surface {surface_id}'s export staging mapped {} bytes where it is sized for \
                     {}, so the frame would cross part-written",
                        staged_pixels.len(),
                        description.pixel_byte_length
                    )),
                ),
            );
        }

        Ok(a_mesh_message_carrying_a_frames_pixels(
            description,
            bag_bytes,
            |the_messages_pixel_tail| the_messages_pixel_tail.copy_from_slice(staged_pixels),
        ))
    }

    /// See the Linux arm: no export staging, so no frame crosses from here.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn a_mesh_message_carrying_the_frame_this_bag_names(
        &mut self,
        _surface_id: &str,
        _bag_bytes: &[u8],
    ) -> std::result::Result<AMeshMessageCarryingAFramesPixels, WhyAFramesPixelsCannotCrossTheMesh>
    {
        let _ = &self.gpu_context_the_mesh_copies_frames_with;
        Err(WhyAFramesPixelsCannotCrossTheMesh::ThisPlatformCannotReadAFrameOut)
    }

    /// Claim `surface_id` against the pool rehanding its slot, for as long as
    /// the returned guard lives.
    ///
    /// `None` where this runtime keeps no lease table — a runtime with no
    /// surface-share service has no cross-process consumer to arbitrate
    /// against, and the refill's own recycled-frame refusal is then the whole
    /// guard.
    #[cfg(target_os = "linux")]
    fn claim_the_frame(
        &mut self,
        gpu_context: &crate::core::context::GpuContext,
        surface_id: &str,
    ) -> std::result::Result<
        Option<AFrameClaimedWhileItsPixelsAreRead<'_>>,
        WhyAFramesPixelsCannotCrossTheMesh,
    > {
        use crate::core::context::SurfaceStore;

        if self.claims_are_charged_to.is_none() {
            let Some(leases) = gpu_context
                .surface_store()
                .as_ref()
                .and_then(SurfaceStore::check_out_leases)
                .cloned()
            else {
                return Ok(None);
            };
            let holder = leases.mint_holder_id();
            self.claims_are_charged_to = Some(AFrameClaimHolderOnThisRuntime { leases, holder });
        }
        let charged_to = self
            .claims_are_charged_to
            .as_ref()
            .expect("the branch above records one or returns");
        charged_to
            .leases
            .record_check_out_lease(surface_id, charged_to.holder)
            .map_err(|cannot_claim| match cannot_claim {
                recycled @ crate::core::Error::SurfaceFrameRecycled { .. } => {
                    WhyAFramesPixelsCannotCrossTheMesh::TheProducerHasRecycledTheFrame(recycled)
                }
                other => WhyAFramesPixelsCannotCrossTheMesh::ItsPixelsCannotBeReadOut(other),
            })?;
        Ok(Some(AFrameClaimedWhileItsPixelsAreRead {
            charged_to,
            surface_id: surface_id.to_string(),
        }))
    }
}

/// Everything the mesh's claims on this runtime's frames are charged to.
#[cfg(target_os = "linux")]
struct AFrameClaimHolderOnThisRuntime {
    leases: Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
    holder: crate::core::context::SurfaceCheckOutLeaseHolderId,
}

#[cfg(target_os = "linux")]
impl Drop for AFrameClaimHolderOnThisRuntime {
    fn drop(&mut self) {
        // The backstop for a read that panicked past its own guard: a claim
        // nobody gives back pins its pool slot for the rest of the run, and
        // the producer then drops every frame it wanted that slot for.
        if let Err(cannot_release) = self
            .leases
            .release_every_check_out_lease_held_by(self.holder)
        {
            tracing::warn!(
                "the mesh could not give back the frame claims it still held ({}): \
                 {cannot_release}",
                self.holder
            );
        }
    }
}

/// One frame claimed for exactly as long as its pixels are being read.
#[cfg(target_os = "linux")]
struct AFrameClaimedWhileItsPixelsAreRead<'a> {
    charged_to: &'a AFrameClaimHolderOnThisRuntime,
    surface_id: String,
}

#[cfg(target_os = "linux")]
impl Drop for AFrameClaimedWhileItsPixelsAreRead<'_> {
    fn drop(&mut self) {
        if let Err(cannot_release) = self
            .charged_to
            .leases
            .release_one_check_out_lease(&self.surface_id, self.charged_to.holder)
        {
            tracing::warn!(
                "the mesh could not give the frame {} back to its pool, so the producer will \
                 never be rehanded that slot: {cannot_release}",
                self.surface_id
            );
        }
    }
}

/// The format of `surface_id`'s backing when it carries more than one plane,
/// and `None` otherwise — including for an id that resolves to nothing, whose
/// refusal the staging below says in its own words.
///
/// Read ahead of the staging only so that this refusal is named: the staging
/// refuses a multi-plane source too, and a port that met one and then met a
/// recycled frame would otherwise say only the first of the two.
#[cfg(target_os = "linux")]
fn a_backing_of_more_than_one_plane(
    gpu_context: &crate::core::context::GpuContext,
    surface_id: &str,
) -> Option<String> {
    use crate::core::context::surface_export_staging::ResolvedBlitSource;
    use streamlib_consumer_rhi::TextureFormat;

    match gpu_context.resolve_device_export_source(surface_id).ok()? {
        ResolvedBlitSource::PixelBuffer(pixel_buffer) => {
            let pixel_format = pixel_buffer.format();
            (pixel_format.plane_count() > 1).then(|| pixel_format.wire_name().to_string())
        }
        ResolvedBlitSource::RegisteredTexture(registration) => {
            (registration.texture().format() == TextureFormat::Nv12).then(|| "nv12".to_string())
        }
    }
}

/// The refusal every read failure that is not one of the named ones takes.
#[cfg(target_os = "linux")]
fn a_read_refusal(refusal: crate::core::Error) -> WhyAFramesPixelsCannotCrossTheMesh {
    match refusal {
        recycled @ crate::core::Error::SurfaceFrameRecycled { .. } => {
            WhyAFramesPixelsCannotCrossTheMesh::TheProducerHasRecycledTheFrame(recycled)
        }
        other => WhyAFramesPixelsCannotCrossTheMesh::ItsPixelsCannotBeReadOut(other),
    }
}
