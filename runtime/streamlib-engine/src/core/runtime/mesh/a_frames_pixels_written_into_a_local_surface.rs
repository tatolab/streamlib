// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Writing one frame's arriving pixels into a surface of this runtime's own,
//! and handing the bag on naming that one.
//!
//! No surface id, lease, lifetime state or write-back crosses the mesh: the
//! frame that lands here is a fresh local frame, minted from this runtime's
//! own pool, and the id it carries downstream resolves here and nowhere else.
//! Every other key in the bag is the producer's and crosses untouched.
//!
//! A pool is never freed once created, so what shapes a remote source may
//! mint pools of is bounded: a source that changed format or extent without
//! limit would grow this runtime's GPU memory for the rest of the run, and
//! nothing downstream would say why.

use std::collections::BTreeSet;
use std::sync::Arc;

use streamlib_consumer_rhi::PixelFormat;

use crate::core::runtime::mesh::a_bags_top_level_surface_id::the_top_level_surface_id_of_a_bag;
use crate::core::runtime::mesh::a_frames_pixels_on_the_mesh::AFramesPixelsOffTheMesh;
use crate::core::runtime::mesh::gpu_context_the_mesh_copies_frames_with::GpuContextTheMeshCopiesFramesWith;

/// How many distinct format-and-extent pairs one source's frames may mint
/// pools of on this runtime.
///
/// Engine-chosen; nothing authorable. Four rather than one, because a source
/// that legitimately changes shape — a camera renegotiating, a decoder
/// meeting a new stream — must not stop crossing the first time it does; and
/// four rather than unbounded, because pools are never freed.
const HOW_MANY_FORMAT_AND_EXTENT_PAIRS_ONE_SOURCE_MAY_MINT: usize = 4;

/// One format and extent this runtime has minted a pool for on a source's
/// behalf.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct AFormatAndExtentAPoolWasMintedFor {
    width: u32,
    height: u32,
    pixel_format_wire_name: &'static str,
}

impl std::fmt::Display for AFormatAndExtentAPoolWasMintedFor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}x{} {}",
            self.width, self.height, self.pixel_format_wire_name
        )
    }
}

/// Why one arriving frame is not landing, in the terms the ingress's log line
/// and its once-per-reason bookkeeping both use.
#[derive(Debug)]
pub(super) enum WhyAFramesPixelsCannotLandHere {
    /// The runtime has no GPU context — it has not started, or it has
    /// stopped. There is no pool to mint from either way.
    ThisRuntimeHasNoGpuContextYet,
    /// The source has already been given every pool shape it may have.
    ItIsOneShapeTooManyFromThisSource {
        this_shape: String,
        already_minted: String,
    },
    /// Every buffer in the pool of this shape is still being read.
    EveryBufferInItsPoolIsInUse(crate::core::Error),
    /// The mint refused for anything else, said in the words it refused with.
    NoLocalSurfaceCouldBeMinted(crate::core::Error),
    /// The pixels that arrived are not the number the freshly minted surface
    /// holds, so writing them would leave the frame part-written.
    ItsPixelsAreNotTheSizeOfTheSurfaceMinted {
        pixels_that_arrived: u64,
        the_surface_holds: u64,
    },
    /// The bag names no surface this engine can read, so there is nothing to
    /// hand the local id to. A sending runtime only ever builds this message
    /// for a bag that named one.
    ItsBagNamesNoSurfaceToReplace,
    /// The bag could not be written naming the local surface.
    ItsBagCouldNotBeRewritten(crate::core::Error),
}

impl WhyAFramesPixelsCannotLandHere {
    /// Which refusal this is, so an ingress says each of them once rather
    /// than saying the first one forever.
    pub(super) fn which_refusal_this_is(&self) -> &'static str {
        match self {
            Self::ThisRuntimeHasNoGpuContextYet => "no-gpu-context",
            Self::ItIsOneShapeTooManyFromThisSource { .. } => "too-many-shapes-from-one-source",
            Self::EveryBufferInItsPoolIsInUse(_) => "the-pool-is-at-its-cap",
            Self::NoLocalSurfaceCouldBeMinted(_) => "no-local-surface-could-be-minted",
            Self::ItsPixelsAreNotTheSizeOfTheSurfaceMinted { .. } => "the-wrong-number-of-pixels",
            Self::ItsBagNamesNoSurfaceToReplace => "its-bag-names-no-surface",
            Self::ItsBagCouldNotBeRewritten(_) => "its-bag-could-not-be-rewritten",
        }
    }
}

impl std::fmt::Display for WhyAFramesPixelsCannotLandHere {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ThisRuntimeHasNoGpuContextYet => formatter.write_str(
                "this runtime has no GPU context, so it has no pool to mint a local surface from",
            ),
            Self::ItIsOneShapeTooManyFromThisSource {
                this_shape,
                already_minted,
            } => write!(
                formatter,
                "it is {this_shape}, and this runtime has already minted pools for the \
                 {HOW_MANY_FORMAT_AND_EXTENT_PAIRS_ONE_SOURCE_MAY_MINT} shapes one source may \
                 have ({already_minted}); a pool is never freed, so no more are minted for it"
            ),
            Self::EveryBufferInItsPoolIsInUse(refusal) => write!(formatter, "{refusal}"),
            Self::NoLocalSurfaceCouldBeMinted(refusal) => write!(
                formatter,
                "no local surface could be minted for it: {refusal}"
            ),
            Self::ItsPixelsAreNotTheSizeOfTheSurfaceMinted {
                pixels_that_arrived,
                the_surface_holds,
            } => write!(
                formatter,
                "{pixels_that_arrived} bytes of pixels arrived for a surface that holds \
                 {the_surface_holds}, so the frame would land part-written"
            ),
            Self::ItsBagNamesNoSurfaceToReplace => formatter.write_str(
                "its bag names no surface this engine can read, so the local one has nowhere \
                 to go",
            ),
            Self::ItsBagCouldNotBeRewritten(refusal) => write!(
                formatter,
                "its bag could not be written naming the local surface: {refusal}"
            ),
        }
    }
}

/// Mints one source's arriving frames into local surfaces, on the ingress's
/// own writing thread.
pub(super) struct WritesAFramesPixelsIntoALocalSurface {
    gpu_context_the_mesh_copies_frames_with: Arc<GpuContextTheMeshCopiesFramesWith>,
    pools_minted_for_this_source: BTreeSet<AFormatAndExtentAPoolWasMintedFor>,
}

impl WritesAFramesPixelsIntoALocalSurface {
    pub(super) fn minting_through(
        gpu_context_the_mesh_copies_frames_with: &Arc<GpuContextTheMeshCopiesFramesWith>,
    ) -> Self {
        Self {
            gpu_context_the_mesh_copies_frames_with: Arc::clone(
                gpu_context_the_mesh_copies_frames_with,
            ),
            pools_minted_for_this_source: BTreeSet::new(),
        }
    }

    /// The bag this frame is handed downstream as — the producer's own,
    /// naming a surface of this runtime's — or why the frame is not landing.
    pub(super) fn a_bag_naming_the_local_surface_this_frame_landed_in(
        &mut self,
        arrived: &AFramesPixelsOffTheMesh<'_>,
    ) -> std::result::Result<Vec<u8>, WhyAFramesPixelsCannotLandHere> {
        let gpu_context = self
            .gpu_context_the_mesh_copies_frames_with
            .the_gpu_context_or_none()
            .ok_or(WhyAFramesPixelsCannotLandHere::ThisRuntimeHasNoGpuContextYet)?;

        // Before the mint, because the mint is what creates the pool this
        // bounds: refusing after one would have already grown the runtime.
        self.refuse_a_shape_too_many(
            arrived.description.width,
            arrived.description.height,
            arrived.description.pixel_format,
        )?;

        let (local_surface_id, local_surface) = gpu_context
            .acquire_pixel_buffer(
                arrived.description.width,
                arrived.description.height,
                arrived.description.pixel_format,
            )
            .map_err(|cannot_mint| match cannot_mint {
                at_cap @ crate::core::Error::EveryPixelBufferInThePoolIsInUse { .. } => {
                    WhyAFramesPixelsCannotLandHere::EveryBufferInItsPoolIsInUse(at_cap)
                }
                other => WhyAFramesPixelsCannotLandHere::NoLocalSurfaceCouldBeMinted(other),
            })?;

        let the_surface_holds = local_surface.plane_size(0);
        if arrived.pixel_bytes.len() as u64 != the_surface_holds {
            return Err(
                WhyAFramesPixelsCannotLandHere::ItsPixelsAreNotTheSizeOfTheSurfaceMinted {
                    pixels_that_arrived: arrived.pixel_bytes.len() as u64,
                    the_surface_holds,
                },
            );
        }
        let plane = local_surface.plane_base_address(0);
        if plane.is_null() {
            return Err(WhyAFramesPixelsCannotLandHere::NoLocalSurfaceCouldBeMinted(
                crate::core::Error::GpuError(format!(
                    "the surface {local_surface_id} this runtime minted for an arriving frame is \
                     not mapped, so its pixels cannot be written into it"
                )),
            ));
        }
        // SAFETY: the plane's mapping is `plane_size(0)` bytes long, which the
        // check above proved is exactly what arrived, and the arriving bytes
        // are a separate allocation the mesh owns.
        unsafe {
            std::ptr::copy_nonoverlapping(
                arrived.pixel_bytes.as_ptr(),
                plane,
                the_surface_holds as usize,
            )
        };

        the_top_level_surface_id_of_a_bag(arrived.bag_bytes)
            .ok_or(WhyAFramesPixelsCannotLandHere::ItsBagNamesNoSurfaceToReplace)?
            .a_bag_naming_this_surface_instead(&local_surface_id.to_string())
            .map_err(WhyAFramesPixelsCannotLandHere::ItsBagCouldNotBeRewritten)
    }

    /// Refuse a shape beyond the handful one source may mint pools of, and
    /// record every shape that is allowed through.
    fn refuse_a_shape_too_many(
        &mut self,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    ) -> std::result::Result<(), WhyAFramesPixelsCannotLandHere> {
        let this_shape = AFormatAndExtentAPoolWasMintedFor {
            width,
            height,
            pixel_format_wire_name: pixel_format.wire_name(),
        };
        if self.pools_minted_for_this_source.contains(&this_shape) {
            return Ok(());
        }
        if self.pools_minted_for_this_source.len()
            >= HOW_MANY_FORMAT_AND_EXTENT_PAIRS_ONE_SOURCE_MAY_MINT
        {
            return Err(
                WhyAFramesPixelsCannotLandHere::ItIsOneShapeTooManyFromThisSource {
                    this_shape: this_shape.to_string(),
                    already_minted: self
                        .pools_minted_for_this_source
                        .iter()
                        .map(AFormatAndExtentAPoolWasMintedFor::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                },
            );
        }
        self.pools_minted_for_this_source.insert(this_shape);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_writer() -> WritesAFramesPixelsIntoALocalSurface {
        WritesAFramesPixelsIntoALocalSurface::minting_through(&Arc::new(
            GpuContextTheMeshCopiesFramesWith::default(),
        ))
    }

    /// A source that keeps to one shape, or to a handful, is never refused —
    /// and a shape already admitted costs nothing the second time.
    #[test]
    fn a_source_keeping_to_a_handful_of_shapes_is_never_refused() {
        let mut writer = a_writer();
        for _ in 0..3 {
            for (width, height, pixel_format) in [
                (1920, 1080, PixelFormat::Rgba32),
                (1920, 1080, PixelFormat::Bgra32),
                (640, 480, PixelFormat::Rgba32),
                (64, 64, PixelFormat::Gray8),
            ] {
                assert!(
                    writer
                        .refuse_a_shape_too_many(width, height, pixel_format)
                        .is_ok(),
                    "{width}x{height} {pixel_format:?} is one of the four this source may have"
                );
            }
        }
    }

    /// The fifth shape is refused by name, listing the four already minted —
    /// a pool is never freed, so this is the one thing standing between a
    /// remote source and this runtime's whole GPU memory.
    #[test]
    fn a_shape_beyond_the_handful_is_refused_by_name_listing_the_ones_already_minted() {
        let mut writer = a_writer();
        for extent in 1..=HOW_MANY_FORMAT_AND_EXTENT_PAIRS_ONE_SOURCE_MAY_MINT as u32 {
            writer
                .refuse_a_shape_too_many(extent * 16, 64, PixelFormat::Rgba32)
                .expect("the shapes up to the bound are admitted");
        }

        let refused = writer
            .refuse_a_shape_too_many(9999, 64, PixelFormat::Bgra32)
            .expect_err("the shape past the bound is refused");

        assert_eq!(
            refused.which_refusal_this_is(),
            "too-many-shapes-from-one-source"
        );
        let said = refused.to_string();
        assert!(
            said.contains("9999x64 bgra32"),
            "the refusal must name the shape it refused: {said}"
        );
        assert!(
            said.contains("16x64 rgba32") && said.contains("64x64 rgba32"),
            "the refusal must list what this source already has: {said}"
        );
        assert!(
            writer
                .refuse_a_shape_too_many(32, 64, PixelFormat::Rgba32)
                .is_ok(),
            "a shape already minted for must keep crossing after another was refused"
        );
    }

    /// A runtime with no GPU context refuses rather than reaching for a pool
    /// that is not there — the state every ingress is in before `start()`.
    #[test]
    fn a_runtime_with_no_gpu_context_refuses_by_name() {
        let refused = a_writer()
            .a_bag_naming_the_local_surface_this_frame_landed_in(&AFramesPixelsOffTheMesh {
                description: crate::core::runtime::mesh::a_frames_pixels_on_the_mesh::AFramesPixelDescriptionOnTheMesh {
                    pixel_format: PixelFormat::Rgba32,
                    width: 2,
                    height: 2,
                    pixel_byte_length: 16,
                },
                bag_bytes: b"\x81\xaasurface_id\xa33#1",
                pixel_bytes: &[0; 16],
            })
            .expect_err("a runtime with no context mints nothing");
        assert_eq!(refused.which_refusal_this_is(), "no-gpu-context");
    }
}
