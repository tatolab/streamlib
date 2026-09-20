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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    use serde_json::json;
    use streamlib_consumer_rhi::PixelFormat;

    use crate::core::context::GpuContext;
    use crate::core::runtime::mesh::a_bags_top_level_surface_id::the_top_level_surface_id_of_a_bag;
    use crate::core::runtime::mesh::a_frames_pixels_on_the_mesh::a_frames_pixels_off_the_mesh;
    use crate::core::runtime::mesh::a_frames_pixels_written_into_a_local_surface::WritesAFramesPixelsIntoALocalSurface;

    /// The device, or nothing — CI has no GPU, and these arms run on the rig.
    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu_context) => Some(gpu_context),
            Err(_) => {
                println!("{test_name}: no GPU device — skipping");
                None
            }
        }
    }

    /// The one cell both halves read, already holding `gpu_context`.
    fn the_mesh_copying_frames_with(
        gpu_context: &GpuContext,
    ) -> Arc<GpuContextTheMeshCopiesFramesWith> {
        let cell = Arc::new(GpuContextTheMeshCopiesFramesWith::default());
        cell.record_the_runtimes_gpu_context(gpu_context);
        cell
    }

    /// A picture no wrong copy passes for: every byte differs from its
    /// neighbours, so a copy off by one pixel, one row or one plane fails.
    fn a_picture_of(byte_count: usize) -> Vec<u8> {
        (0..byte_count).map(|at| (at % 251) as u8).collect()
    }

    /// The bag a video producer writes around a surface id.
    fn a_video_bag_naming(surface_id: &str, width: u32, height: u32) -> Vec<u8> {
        rmp_serde::to_vec_named(&json!({
            "surface_id": surface_id,
            "width": width,
            "height": height,
            "timestamp_ns": 1_726_000_000_000_000_000i64,
        }))
        .expect("a bag encodes")
    }

    /// What one pooled surface currently holds.
    fn the_pixels_in(buffer: &crate::core::rhi::PixelBuffer) -> Vec<u8> {
        let plane = buffer.plane_base_address(0);
        assert!(!plane.is_null(), "a pooled surface is host-mapped");
        // SAFETY: the plane's mapping is `plane_size(0)` bytes long and lives
        // as long as the buffer this borrows from.
        unsafe { std::slice::from_raw_parts(plane, buffer.plane_size(0) as usize) }.to_vec()
    }

    /// The whole crossing in one process: a frame read out of one pooled
    /// surface arrives byte for byte in another, under an id of the
    /// receiving runtime's own, with the rest of the producer's bag
    /// untouched.
    ///
    /// The two ends are one runtime here because CI has no second machine;
    /// what this pins is the pair of copies and the rewrite, which is what a
    /// second machine would exercise too. GPU-gated: skips with no device.
    #[test]
    fn a_frame_crosses_byte_for_byte_into_a_surface_of_this_runtimes_own() {
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 48;
        let Some(gpu_context) =
            gpu_or_skip("a_frame_crosses_byte_for_byte_into_a_surface_of_this_runtimes_own")
        else {
            return;
        };
        let cell = the_mesh_copying_frames_with(&gpu_context);

        let (produced_id, produced) = gpu_context
            .acquire_pixel_buffer(WIDTH, HEIGHT, PixelFormat::Rgba32)
            .expect("a frame to send");
        let picture = a_picture_of(produced.plane_size(0) as usize);
        // SAFETY: the picture is exactly `plane_size(0)` bytes, and the plane
        // is this buffer's own host mapping.
        unsafe {
            std::ptr::copy_nonoverlapping(
                picture.as_ptr(),
                produced.plane_base_address(0),
                picture.len(),
            )
        };
        let produced_id = produced_id.to_string();
        let bag = a_video_bag_naming(&produced_id, WIDTH, HEIGHT);

        let message = ReadsAFramesPixelsOutForTheMesh::reading_through(&cell)
            .a_mesh_message_carrying_the_frame_this_bag_names(&produced_id, &bag)
            .unwrap_or_else(|why| panic!("the frame must be readable out: {why}"));
        let arrived =
            a_frames_pixels_off_the_mesh(&message.message_bytes, message.description_bytes)
                .expect("the message this egress built reads back");
        assert_eq!(arrived.description.pixel_format, PixelFormat::Rgba32);
        assert_eq!(arrived.description.width, WIDTH);
        assert_eq!(arrived.description.height, HEIGHT);
        assert_eq!(arrived.bag_bytes, bag);
        assert_eq!(arrived.pixel_bytes, picture);

        let landed_bag = WritesAFramesPixelsIntoALocalSurface::minting_through(&cell)
            .a_bag_naming_the_local_surface_this_frame_landed_in(&arrived)
            .unwrap_or_else(|why| panic!("the frame must land: {why}"));

        let landed_id = the_top_level_surface_id_of_a_bag(&landed_bag)
            .expect("the bag handed downstream names a surface")
            .surface_id()
            .to_string();
        assert_ne!(
            landed_id, produced_id,
            "the frame must land in a surface of the receiving runtime's own, never under the \
             sender's id"
        );
        let read_back: serde_json::Value =
            rmp_serde::from_slice(&landed_bag).expect("the bag handed downstream is still one");
        assert_eq!(read_back["width"], json!(WIDTH));
        assert_eq!(read_back["height"], json!(HEIGHT));
        assert_eq!(
            read_back["timestamp_ns"],
            json!(1_726_000_000_000_000_000i64),
            "a stamp inside the bag crosses unchanged"
        );

        let landed = gpu_context
            .get_pixel_buffer(&landed_id)
            .expect("the id handed downstream resolves on this runtime");
        assert_eq!(
            the_pixels_in(&landed),
            picture,
            "the frame must arrive byte for byte"
        );
    }

    /// A multi-plane frame is refused by that name rather than crossing with
    /// its chroma dropped — a one-buffer export carries the first plane only.
    /// GPU-gated: skips with no device.
    #[test]
    fn a_multi_plane_frame_is_refused_by_name_rather_than_crossing_without_its_chroma() {
        let Some(gpu_context) = gpu_or_skip(
            "a_multi_plane_frame_is_refused_by_name_rather_than_crossing_without_its_chroma",
        ) else {
            return;
        };
        let cell = the_mesh_copying_frames_with(&gpu_context);
        let (nv12_id, _held) = gpu_context
            .acquire_pixel_buffer(64, 48, PixelFormat::Nv12VideoRange)
            .expect("an NV12 frame");
        let nv12_id = nv12_id.to_string();

        let refused = ReadsAFramesPixelsOutForTheMesh::reading_through(&cell)
            .a_mesh_message_carrying_the_frame_this_bag_names(
                &nv12_id,
                &a_video_bag_naming(&nv12_id, 64, 48),
            )
            .err()
            .expect("NV12 must be refused");

        assert_eq!(refused.which_refusal_this_is(), "more-than-one-plane");
        assert!(
            refused.to_string().contains("nv12"),
            "the refusal must name the format it refused: {refused}"
        );
    }

    /// An id whose slot the producer has recycled is refused by its own name,
    /// so a port that met it and then met something else says both.
    /// GPU-gated: skips with no device.
    #[test]
    fn a_recycled_frame_is_refused_by_its_own_name() {
        let Some(gpu_context) = gpu_or_skip("a_recycled_frame_is_refused_by_its_own_name") else {
            return;
        };
        let cell = the_mesh_copying_frames_with(&gpu_context);
        let (minted, _held) = gpu_context
            .acquire_pixel_buffer(32, 32, PixelFormat::Rgba32)
            .expect("a frame");
        let a_generation_the_slot_never_published = format!(
            "{}#{}",
            minted.pool_slot_id(),
            minted.frame_generation() + 7
        );

        let refused = ReadsAFramesPixelsOutForTheMesh::reading_through(&cell)
            .a_mesh_message_carrying_the_frame_this_bag_names(
                &a_generation_the_slot_never_published,
                &a_video_bag_naming(&a_generation_the_slot_never_published, 32, 32),
            )
            .err()
            .expect("a recycled frame must be refused");

        assert_eq!(refused.which_refusal_this_is(), "the-frame-was-recycled");
    }

    /// A runtime that has not started carries no frame, and says so as its
    /// own reason rather than as a read that failed.
    #[test]
    fn a_runtime_with_no_gpu_context_carries_no_frame_and_says_so_by_name() {
        let refused = ReadsAFramesPixelsOutForTheMesh::reading_through(&Arc::new(
            GpuContextTheMeshCopiesFramesWith::default(),
        ))
        .a_mesh_message_carrying_the_frame_this_bag_names("7#3", b"\x81\xaasurface_id\xa33#1")
        .err()
        .expect("a runtime with no context resolves nothing");

        assert_eq!(refused.which_refusal_this_is(), "no-gpu-context");
    }
}
