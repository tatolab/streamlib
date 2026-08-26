// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The pixel half of the control plane's `exchange` verb: a published
//! surface id in, that frame's pixels on the host out.
//!
//! Order inside one call is the contract — resolve, claim, convert and
//! copy under the claim, release. Encoding happens in the caller, after
//! this returns, so the claim window is the conversion and the copy off
//! the producer's slot, and nothing else.
//!
//! **What the claim covers, exactly.** A pooled backing is claimed: the
//! resolved [`PixelBuffer`] *is* the refcount clone the ring reads before
//! it rehands a slot ([`GpuContext::acquire_pixel_buffer`]), so holding it
//! is what keeps a producer from recycling the frame mid-copy and handing
//! the caller half of one frame and half of the next. That refcount is a
//! same-address-space test, and it is the whole seam here because this
//! runs in the process that owns the pool — every producer, helper
//! children included, acquires through it. A surface that resolves only
//! through a cross-process `lookup` carries no pixel extent and is refused
//! by name rather than claimed.
//!
//! A **texture backing is not claimed**, because there is nothing to claim
//! it with: holding the registration keeps the texture alive, but a
//! producer's ring rewrites its slots in place, and the texture pool gates
//! reuse on its own in-use flag rather than on that refcount. A
//! texture-backed exchange therefore rides its producer's ring depth the
//! way a pooled one rides pool depth — the same exposure the shipped
//! export-staging refill has, not a new one.
//!
//! Every pixel conversion runs in the RHI or not at all: a YUV camera
//! frame goes through the engine's color converter, an RGBA pool frame
//! through a buffer→image copy, a texture backing through the present
//! compositor's sampled blit — which is also where the optional long-edge
//! downscale rides. Nothing walks pixels on the CPU.

use crate::core::color::resolve_color_defaults;
use crate::core::context::GpuContext;
use crate::core::context::surface_export_staging::ResolvedBlitSource;
use crate::core::error::{Error, Result};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, SourceLayoutInfo, Texture, TextureDescriptor, TextureFormat,
    TextureReadbackDescriptor, TextureSourceLayout, TextureUsages, VulkanLayout,
    pixel_format_color_kind,
};
use crate::host_rhi::{VulkanAccess, VulkanStage};
use crate::vulkan::rhi::{ImageCopyRegion, PresentScalingMode};

/// The one image format the exchange hands back. Every backing is
/// normalized to it in the RHI, so a caller never has to know what the
/// producer happened to publish.
const EXCHANGE_IMAGE_TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

/// How long the host waits on one exchange's GPU→CPU copy before calling
/// it a stall. A control-plane read is a single frame's copy on a queue
/// that is otherwise making progress; past this the device is wedged, not
/// slow, and a caller waiting forever learns nothing.
const EXCHANGE_READBACK_WAIT_TIMEOUT_NANOSECONDS: u64 = 2_000_000_000;

/// One published frame's pixels, copied to the host as tightly-packed
/// RGBA8, with the extent they were encoded at and the extent the surface
/// itself carries.
pub(crate) struct PublishedSurfaceFrameHostRgba8Image {
    /// Tightly-packed RGBA8, `image_pixel_width * image_pixel_height * 4`
    /// bytes.
    pub(crate) rgba8_pixel_bytes: Vec<u8>,
    /// Width of the copied image — below the source width exactly when a
    /// downscale cap applied.
    pub(crate) image_pixel_width: u32,
    /// Height of the copied image.
    pub(crate) image_pixel_height: u32,
    /// Width the surface's own backing carries, downscale or not.
    pub(crate) source_surface_pixel_width: u32,
    /// Height the surface's own backing carries, downscale or not.
    pub(crate) source_surface_pixel_height: u32,
}

/// The extent an image is copied at under an optional long-edge cap.
///
/// Never upscales and never changes the aspect: both axes take the same
/// ratio, and a cap at or above the long edge is the source extent
/// unchanged. A zero cap reads as "no cap" rather than as a zero-pixel
/// image, because a caller spelling `0` is declining the dial.
pub(crate) fn downscaled_image_extent_under_long_edge_cap(
    source_surface_pixel_width: u32,
    source_surface_pixel_height: u32,
    downscale_long_edge_pixel_cap: Option<u32>,
) -> (u32, u32) {
    let long_edge = source_surface_pixel_width.max(source_surface_pixel_height);
    let Some(cap) = downscale_long_edge_pixel_cap.filter(|cap| *cap > 0) else {
        return (source_surface_pixel_width, source_surface_pixel_height);
    };
    if long_edge <= cap || long_edge == 0 {
        return (source_surface_pixel_width, source_surface_pixel_height);
    }
    let scale_down = f64::from(cap) / f64::from(long_edge);
    let scaled = |edge: u32| ((f64::from(edge) * scale_down).round() as u32).max(1);
    (
        scaled(source_surface_pixel_width),
        scaled(source_surface_pixel_height),
    )
}

/// One RGBA8 image the readback can copy straight out, and the layout it
/// is sitting in.
///
/// The layout travels with the texture because only the step that recorded
/// the last barrier knows it, and [`crate::vulkan::rhi::VulkanTextureReadback::submit`]
/// both transitions out of it and restores back to it — a caller that
/// re-derived it would read a texture through the wrong layout the moment
/// a terminal barrier changed.
struct ExchangeImageTextureReadyForReadback {
    texture: Texture,
    layout_the_last_barrier_left_it_in: TextureSourceLayout,
}

impl ExchangeImageTextureReadyForReadback {
    /// The same layout as the barrier vocabulary spells it, for a step that
    /// records rather than reads.
    fn vulkan_layout(&self) -> VulkanLayout {
        VulkanLayout(
            self.layout_the_last_barrier_left_it_in
                .to_vulkan_layout_raw(),
        )
    }
}

impl GpuContext {
    /// Claim `published_surface_id`'s frame, copy it to the host as RGBA8,
    /// and release the claim before returning.
    ///
    /// A retired `<slot>#<generation>` id is refused before any bytes
    /// move, so a caller that outwaited the pool gets the recycled-frame
    /// error and taps a newer bag — never the slot's newer pixels.
    pub(crate) fn copy_published_surface_frame_to_host_rgba8_image(
        &self,
        published_surface_id: &str,
        downscale_long_edge_pixel_cap: Option<u32>,
    ) -> Result<PublishedSurfaceFrameHostRgba8Image> {
        self.refuse_a_retired_frame_id(published_surface_id)?;

        // Resolving hands back the claim itself for a pooled backing — the
        // refcount clone the ring reads — and, for a texture backing, the
        // registration that keeps the texture alive but does not stop its
        // producer rewriting it (see the module docs). Held across the copy
        // below and dropped the moment that copy is done.
        //
        // Its one failure is "neither backing answered", which for a
        // caller naming a surface is an absence, not a device fault — both
        // misses travel inside so the answer still says which doors were
        // tried.
        let claimed_frame_backing = self
            .resolve_device_export_source(published_surface_id)
            .map_err(|no_backing_answered| {
                Error::NotFound(format!(
                    "surface '{published_surface_id}' names no frame this runtime holds: \
                     {no_backing_answered}"
                ))
            })?;

        let (source_surface_pixel_width, source_surface_pixel_height) =
            claimed_frame_backing_extent(published_surface_id, &claimed_frame_backing)?;
        let (image_pixel_width, image_pixel_height) = downscaled_image_extent_under_long_edge_cap(
            source_surface_pixel_width,
            source_surface_pixel_height,
            downscale_long_edge_pixel_cap,
        );

        let image = self.normalize_claimed_frame_into_an_exchange_image_texture(
            published_surface_id,
            &claimed_frame_backing,
            (source_surface_pixel_width, source_surface_pixel_height),
            (image_pixel_width, image_pixel_height),
        )?;
        // The claim ends here, and it is `normalize` above that earns it:
        // every arm of it copies into a texture this call allocated and
        // waits for that copy, so nothing below reads anything the producer
        // owns. That is the invariant to hold on to — an arm that handed
        // back the producer's own texture would move the copy off it to
        // after this line, where a ring rewrite lands in the answer.
        // Releasing here rather than after the readback makes the window
        // the conversion and the copy off the producer's slot, and nothing
        // else.
        drop(claimed_frame_backing);

        let rgba8_pixel_bytes = self.read_exchange_image_texture_into_host_bytes(&image)?;

        Ok(PublishedSurfaceFrameHostRgba8Image {
            rgba8_pixel_bytes,
            image_pixel_width,
            image_pixel_height,
            source_surface_pixel_width,
            source_surface_pixel_height,
        })
    }

    /// Bring whichever backing answered for the surface into one RGBA8
    /// texture at the image extent, ready for the readback.
    fn normalize_claimed_frame_into_an_exchange_image_texture(
        &self,
        published_surface_id: &str,
        claimed_frame_backing: &ResolvedBlitSource,
        (source_surface_pixel_width, source_surface_pixel_height): (u32, u32),
        (image_pixel_width, image_pixel_height): (u32, u32),
    ) -> Result<ExchangeImageTextureReadyForReadback> {
        match claimed_frame_backing {
            ResolvedBlitSource::PixelBuffer(pixel_buffer) => {
                let at_source_extent = self.copy_pooled_frame_into_an_exchange_image_texture(
                    published_surface_id,
                    pixel_buffer,
                    source_surface_pixel_width,
                    source_surface_pixel_height,
                )?;
                let downscale_applies = (image_pixel_width, image_pixel_height)
                    != (source_surface_pixel_width, source_surface_pixel_height);
                if !downscale_applies {
                    return Ok(at_source_extent);
                }
                self.blit_texture_into_an_exchange_image_texture(
                    &at_source_extent.texture,
                    at_source_extent.vulkan_layout(),
                    image_pixel_width,
                    image_pixel_height,
                )
            }
            ResolvedBlitSource::RegisteredTexture(registration) => {
                // Always through the blit, never straight out of the
                // producer's own texture — even when that texture is
                // already this format, extent and transfer usage. Handing
                // the producer's allocation on to the readback would put
                // the copy off it *after* this frame's backing is released,
                // and a producer's ring rewrites its slots in place: the
                // caller would get bytes from a later frame under the id it
                // asked for. The blit is what makes every later step read
                // storage the exchange owns.
                let composed = self.blit_texture_into_an_exchange_image_texture(
                    registration.texture(),
                    registration.current_layout(),
                    image_pixel_width,
                    image_pixel_height,
                )?;
                // The blit left the producer's texture sampled, and the
                // registration is what its next consumer barriers from.
                registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
                Ok(composed)
            }
        }
    }

    /// Normalize a pooled allocation's frame into a fresh RGBA8 texture,
    /// in the RHI: the engine's color converter for a camera's YUV frame,
    /// a buffer→image copy for one already published as RGBA.
    fn copy_pooled_frame_into_an_exchange_image_texture(
        &self,
        published_surface_id: &str,
        pixel_buffer: &PixelBuffer,
        source_surface_pixel_width: u32,
        source_surface_pixel_height: u32,
    ) -> Result<ExchangeImageTextureReadyForReadback> {
        let image_texture = self.create_exchange_image_texture(
            "surface-exchange-source",
            source_surface_pixel_width,
            source_surface_pixel_height,
            TextureUsages::STORAGE_BINDING
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
        )?;

        // Pool slots are allocated tightly packed for their format, so every
        // stride below is `width`-derived with no driver padding to honour.
        // Each format names its own layout rather than falling through a
        // default: handing one format's plane strides to another's shader
        // produces garbage pixels and no error anywhere.
        let source_pixel_format = pixel_buffer.format();
        match source_pixel_format {
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange => {
                self.convert_pooled_frame_into_texture(
                    pixel_buffer,
                    source_pixel_format,
                    SourceLayoutInfo::nv12_tight(
                        source_surface_pixel_width,
                        source_surface_pixel_height,
                    ),
                    &image_texture,
                )?;
            }
            PixelFormat::Yuyv422 => {
                self.convert_pooled_frame_into_texture(
                    pixel_buffer,
                    source_pixel_format,
                    SourceLayoutInfo::yuyv_tight(source_surface_pixel_width),
                    &image_texture,
                )?;
            }
            PixelFormat::Rgba32 => {
                self.copy_pooled_rgba8_frame_into_texture(
                    pixel_buffer,
                    &image_texture,
                    source_surface_pixel_width,
                    source_surface_pixel_height,
                )?;
            }
            unconvertible => {
                return Err(Error::NotSupported(format!(
                    "surface '{published_surface_id}' publishes {unconvertible:?} pixels, and \
                     the exchange converts only NV12, YUYV and RGBA today — every conversion it \
                     does runs in the RHI's color converter, so a new source format is a shader \
                     there, never a CPU walk here"
                )));
            }
        }
        Ok(ExchangeImageTextureReadyForReadback {
            texture: image_texture,
            layout_the_last_barrier_left_it_in: TextureSourceLayout::General,
        })
    }

    /// Run the RHI's color converter over a YUV pool allocation, into an
    /// RGBA8 texture this exchange owns.
    fn convert_pooled_frame_into_texture(
        &self,
        pixel_buffer: &PixelBuffer,
        source_pixel_format: PixelFormat,
        source_plane_layout: SourceLayoutInfo,
        image_texture: &Texture,
    ) -> Result<()> {
        // The converter writes through an `imageStore`, so the destination
        // has to be GENERAL before the dispatch — the kernel records no
        // layout transition of its own.
        self.transition_storage_image_to_general(image_texture)?;
        let color_converter = self.color_converter(source_pixel_format, PixelFormat::Rgba32)?;
        // No color description travels with a surface, so the defaults the
        // format itself implies are what the matrix and range come from —
        // the same resolve a camera runs before it has read a frame's
        // metadata.
        let resolved_color = resolve_color_defaults(
            None,
            None,
            None,
            None,
            pixel_format_color_kind(source_pixel_format),
        );
        color_converter.convert_buffer_to_image_pixel(
            pixel_buffer,
            source_plane_layout,
            image_texture,
            &resolved_color,
        )
    }

    /// Copy an already-RGBA pool allocation into `image_texture`, leaving
    /// it in `GENERAL` so both pooled arms end the same way.
    fn copy_pooled_rgba8_frame_into_texture(
        &self,
        pixel_buffer: &PixelBuffer,
        image_texture: &Texture,
        source_surface_pixel_width: u32,
        source_surface_pixel_height: u32,
    ) -> Result<()> {
        let mut recorder = self.create_command_recorder("surface_exchange_pooled_frame_copy")?;
        recorder.begin()?;
        // The producer's writes were made on an earlier submission;
        // submission order alone does not make them visible to this read.
        recorder.record_buffer_barrier(
            pixel_buffer,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::TRANSFER_READ,
        )?;
        recorder.record_image_barrier(
            image_texture,
            VulkanLayout::UNDEFINED,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            VulkanStage::NONE,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::NONE,
            VulkanAccess::TRANSFER_WRITE,
        )?;
        recorder.record_copy_buffer_to_image(
            pixel_buffer,
            image_texture,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            ImageCopyRegion::tightly_packed(
                source_surface_pixel_width,
                source_surface_pixel_height,
            ),
        )?;
        recorder.record_image_barrier(
            image_texture,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            VulkanLayout::GENERAL,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::MEMORY_READ,
        )?;
        recorder.submit_and_wait()
    }

    /// Draw `texture_to_sample_from` onto a fresh RGBA8 texture of the
    /// requested extent through the RHI's existing display blit — the one
    /// scaler the engine has, and the only place the downscale cap is spent.
    ///
    /// It samples rather than copies, so it asks nothing of its source but
    /// that it be sampleable: neither the source's format nor its transfer
    /// usage has to suit a readback.
    fn blit_texture_into_an_exchange_image_texture(
        &self,
        texture_to_sample_from: &Texture,
        layout_that_texture_is_currently_in: VulkanLayout,
        image_pixel_width: u32,
        image_pixel_height: u32,
    ) -> Result<ExchangeImageTextureReadyForReadback> {
        let image_texture = self.create_exchange_image_texture(
            "surface-exchange-image",
            image_pixel_width,
            image_pixel_height,
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC,
        )?;
        // `Fit` rather than `Stretch`: the destination extent preserves the
        // source aspect to within a rounded pixel, and letterboxing that
        // remainder is a sub-pixel bar, where stretching it is a distorted
        // picture.
        self.compose_texture_onto_offscreen_texture(
            &image_texture,
            texture_to_sample_from,
            layout_that_texture_is_currently_in,
            PresentScalingMode::Fit,
        )?;
        Ok(ExchangeImageTextureReadyForReadback {
            texture: image_texture,
            layout_the_last_barrier_left_it_in: TextureSourceLayout::ColorAttachment,
        })
    }

    /// Allocate one of the exchange's own textures, always at
    /// [`EXCHANGE_IMAGE_TEXTURE_FORMAT`]. Device-local and same-process: the
    /// exchange exports nothing, so it never spends the DMA-BUF budget a
    /// swapchain shares (`docs/learnings/nvidia-dma-buf-after-swapchain.md`).
    fn create_exchange_image_texture(
        &self,
        label: &str,
        pixel_width: u32,
        pixel_height: u32,
        usage: TextureUsages,
    ) -> Result<Texture> {
        self.device().create_texture_local(
            &TextureDescriptor::new(pixel_width, pixel_height, EXCHANGE_IMAGE_TEXTURE_FORMAT)
                .with_label(label)
                .with_usage(usage),
        )
    }

    /// Copy the exchange's own image texture to the host and own the bytes.
    fn read_exchange_image_texture_into_host_bytes(
        &self,
        image: &ExchangeImageTextureReadyForReadback,
    ) -> Result<Vec<u8>> {
        let readback = self.create_texture_readback(&TextureReadbackDescriptor {
            label: "surface-exchange-readback",
            format: image.texture.format(),
            width: image.texture.width(),
            height: image.texture.height(),
        })?;
        let ticket = readback.submit(&image.texture, image.layout_the_last_barrier_left_it_in)?;
        Ok(readback
            .wait_and_read(ticket, EXCHANGE_READBACK_WAIT_TIMEOUT_NANOSECONDS)?
            .to_vec())
    }
}

/// The pixel extent the claimed backing carries.
///
/// A zero extent means the backing resolved through a path that carries no
/// shape — a cross-process `lookup` import, which hands back planes and no
/// geometry. Refused by name rather than encoded as a zero-pixel image.
fn claimed_frame_backing_extent(
    published_surface_id: &str,
    claimed_frame_backing: &ResolvedBlitSource,
) -> Result<(u32, u32)> {
    let (pixel_width, pixel_height) = match claimed_frame_backing {
        ResolvedBlitSource::PixelBuffer(pixel_buffer) => (pixel_buffer.width, pixel_buffer.height),
        ResolvedBlitSource::RegisteredTexture(registration) => {
            let texture = registration.texture();
            (texture.width(), texture.height())
        }
    };
    if pixel_width == 0 || pixel_height == 0 {
        return Err(Error::NotSupported(format!(
            "surface '{published_surface_id}' resolves to a backing this process holds no pixel \
             extent for, so there is no image to hand back; exchange it from the runtime that \
             owns its pool"
        )));
    }
    Ok((pixel_width, pixel_height))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::rhi::TextureFormat;

    /// A frame small enough to fill by hand, and non-square so a downscale
    /// that ignored aspect shows up as a wrong extent rather than as the
    /// right one by luck.
    const FRAME_PIXEL_WIDTH: u32 = 96;
    const FRAME_PIXEL_HEIGHT: u32 = 48;
    /// The RGBA a pooled frame is published with — four distinct channels,
    /// so a swizzle is a failure rather than a coincidence.
    const PUBLISHED_RGBA8_PIXEL: [u8; 4] = [0x21, 0x43, 0x65, 0xFF];
    /// What a texture-backed producer renders.
    const KERNEL_OUTPUT_RGBA8_PIXEL: [u8; 4] = [0x9A, 0x2C, 0x5E, 0xFF];

    // A GPU-gated test that finds no device passes trivially, so the skip
    // has to reach the person reading the run — and stdout is the only
    // channel a test harness surfaces.
    #[allow(clippy::disallowed_macros)]
    fn gpu_context_or_skip() -> Option<GpuContext> {
        match GpuContext::init_for_platform() {
            Ok(gpu) => Some(gpu),
            Err(_) => {
                println!("Skipping - no GPU device available");
                None
            }
        }
    }

    /// Write `pattern`, repeated, over every byte of a pooled allocation —
    /// the producer's side of publishing a frame.
    fn fill_pooled_backing_with(pixel_buffer: &PixelBuffer, pattern: &[u8]) {
        let base_address = pixel_buffer.plane_base_address(0);
        assert!(
            !base_address.is_null(),
            "a pooled allocation must be host-mapped for this fixture to publish into it"
        );
        let byte_count = pixel_buffer.plane_size(0) as usize;
        let backing = unsafe { std::slice::from_raw_parts_mut(base_address, byte_count) };
        for (index, byte) in backing.iter_mut().enumerate() {
            *byte = pattern[index % pattern.len()];
        }
    }

    /// A YUYV frame whose top half is the limited-range black point and
    /// whose bottom half is the white point, both with neutral chroma —
    /// what a camera hands the engine, in the layout
    /// `color_convert_yuyv_buffer_to_rgba.comp` reads (`[Y0, U, Y1, V]`).
    fn fill_pooled_backing_with_a_two_tone_yuyv_frame(pixel_buffer: &PixelBuffer) {
        const LIMITED_RANGE_BLACK_LUMA: u8 = 0x10;
        const LIMITED_RANGE_WHITE_LUMA: u8 = 0xEB;
        const NEUTRAL_CHROMA: u8 = 0x80;

        let base_address = pixel_buffer.plane_base_address(0);
        assert!(
            !base_address.is_null(),
            "the YUYV allocation must be mapped"
        );
        let byte_count = pixel_buffer.plane_size(0) as usize;
        let backing = unsafe { std::slice::from_raw_parts_mut(base_address, byte_count) };
        let bytes_per_row = (FRAME_PIXEL_WIDTH * 2) as usize;
        for (row, row_bytes) in backing.chunks_mut(bytes_per_row).enumerate() {
            let luma = if (row as u32) < FRAME_PIXEL_HEIGHT / 2 {
                LIMITED_RANGE_BLACK_LUMA
            } else {
                LIMITED_RANGE_WHITE_LUMA
            };
            for macro_pixel in row_bytes.chunks_mut(4) {
                if macro_pixel.len() < 4 {
                    break;
                }
                macro_pixel.copy_from_slice(&[luma, NEUTRAL_CHROMA, luma, NEUTRAL_CHROMA]);
            }
        }
    }

    fn assert_every_pixel_is(rgba8_pixel_bytes: &[u8], expected: [u8; 4], subject: &str) {
        assert!(!rgba8_pixel_bytes.is_empty(), "{subject} came back empty");
        for (pixel_index, pixel) in rgba8_pixel_bytes.chunks_exact(4).enumerate() {
            assert_eq!(
                pixel, expected,
                "{subject}: pixel {pixel_index} is {pixel:02x?}, expected {expected:02x?}"
            );
        }
    }

    fn pixel_at(image: &PublishedSurfaceFrameHostRgba8Image, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * image.image_pixel_width + x) * 4) as usize;
        image.rgba8_pixel_bytes[offset..offset + 4]
            .try_into()
            .expect("four bytes per pixel")
    }

    /// The pooled arm end to end: what the exchange copies out is the frame
    /// the bag published, channel order intact.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_pooled_rgba_frame_exchanges_for_the_pixels_the_bag_published() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (published_frame_id, pooled_backing) = gpu
            .acquire_pixel_buffer(FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame's pooled backing");
        fill_pooled_backing_with(&pooled_backing, &PUBLISHED_RGBA8_PIXEL);

        let exchanged = gpu
            .copy_published_surface_frame_to_host_rgba8_image(&published_frame_id.to_string(), None)
            .expect("exchange the published frame");

        assert_eq!(
            (exchanged.image_pixel_width, exchanged.image_pixel_height),
            (FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT),
            "no cap means the exact source resolution"
        );
        assert_eq!(
            (
                exchanged.source_surface_pixel_width,
                exchanged.source_surface_pixel_height
            ),
            (FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT)
        );
        assert_every_pixel_is(
            &exchanged.rgba8_pixel_bytes,
            PUBLISHED_RGBA8_PIXEL,
            "the exchanged pooled frame",
        );
    }

    /// The texture arm: a kernel output has no pooled member, and its own
    /// usage flags need not suit a readback — the RHI's blit is what makes
    /// it readable, so this covers both the resolve and the conversion.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_texture_backed_frame_exchanges_for_the_pixels_its_producer_rendered() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (surface_id, kernel_output_texture) = gpu
            .acquire_output_texture(
                FRAME_PIXEL_WIDTH,
                FRAME_PIXEL_HEIGHT,
                TextureFormat::Rgba8Unorm,
            )
            .expect("acquire a kernel-output texture");
        let (_, upload_source) = gpu
            .acquire_pixel_buffer(FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the upload source");
        fill_pooled_backing_with(&upload_source, &KERNEL_OUTPUT_RGBA8_PIXEL);
        gpu.copy_pixel_buffer_to_texture(
            &upload_source,
            &kernel_output_texture,
            &surface_id,
            FRAME_PIXEL_WIDTH,
            FRAME_PIXEL_HEIGHT,
        )
        .expect("render the kernel's output");

        let exchanged = gpu
            .copy_published_surface_frame_to_host_rgba8_image(&surface_id, None)
            .expect("exchange the texture-backed surface");

        assert_eq!(
            (exchanged.image_pixel_width, exchanged.image_pixel_height),
            (FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT)
        );
        assert_every_pixel_is(
            &exchanged.rgba8_pixel_bytes,
            KERNEL_OUTPUT_RGBA8_PIXEL,
            "the exchanged texture-backed frame",
        );
    }

    /// A producer texture that is already the exchange's format, extent and
    /// transfer usage — everything a readback would need to copy it
    /// directly — still goes through the blit into exchange-owned storage.
    /// This is the shape a Python kernel output has (`acquire_texture` puts
    /// `COPY_SRC | COPY_DST` on every request), so it is the common case,
    /// and reading it directly would move the copy off the producer's
    /// allocation to after the frame's backing is released.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_transfer_readable_producer_texture_is_still_copied_into_exchange_storage() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let transfer_readable_texture = gpu
            .device()
            .create_texture_local(
                &TextureDescriptor::new(
                    FRAME_PIXEL_WIDTH,
                    FRAME_PIXEL_HEIGHT,
                    EXCHANGE_IMAGE_TEXTURE_FORMAT,
                )
                .with_label("producer-texture-a-readback-can-copy")
                .with_usage(
                    TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST
                        | TextureUsages::TEXTURE_BINDING
                        | TextureUsages::STORAGE_BINDING,
                ),
            )
            .expect("a transfer-readable producer texture");
        assert!(
            transfer_readable_texture.supports_transfer_read(),
            "the fixture must be a texture a readback could copy directly, or it proves \
             nothing about the arm that declines to"
        );

        let surface_id = format!("kernel-output-{}", uuid::Uuid::new_v4());
        gpu.register_texture(&surface_id, transfer_readable_texture.clone());
        let (_, upload_source) = gpu
            .acquire_pixel_buffer(FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the upload source");
        fill_pooled_backing_with(&upload_source, &KERNEL_OUTPUT_RGBA8_PIXEL);
        gpu.copy_pixel_buffer_to_texture(
            &upload_source,
            &transfer_readable_texture,
            &surface_id,
            FRAME_PIXEL_WIDTH,
            FRAME_PIXEL_HEIGHT,
        )
        .expect("render the kernel's output");

        let exchanged = gpu
            .copy_published_surface_frame_to_host_rgba8_image(&surface_id, None)
            .expect("exchange the transfer-readable texture");
        assert_every_pixel_is(
            &exchanged.rgba8_pixel_bytes,
            KERNEL_OUTPUT_RGBA8_PIXEL,
            "the transfer-readable producer texture",
        );
    }

    /// A camera publishes YUV, and converting it is the RHI's job. The
    /// assertion is that a conversion ran at all: a frame copied through
    /// byte-for-byte would read the packed `[Y, U, Y, V]` as RGBA and come
    /// back strongly green, not as the neutral grey ramp this frame
    /// encodes.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_yuyv_camera_frame_is_converted_in_the_rhi_rather_than_copied_through() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (published_frame_id, pooled_backing) = gpu
            .acquire_pixel_buffer(FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT, PixelFormat::Yuyv422)
            .expect("acquire a YUYV pooled backing");
        fill_pooled_backing_with_a_two_tone_yuyv_frame(&pooled_backing);

        let exchanged = gpu
            .copy_published_surface_frame_to_host_rgba8_image(&published_frame_id.to_string(), None)
            .expect("exchange the YUYV frame");

        let dark = pixel_at(&exchanged, 0, 0);
        let light = pixel_at(&exchanged, 0, FRAME_PIXEL_HEIGHT - 1);
        for (subject, pixel) in [("the black half", dark), ("the white half", light)] {
            let spread = pixel[..3].iter().max().unwrap() - pixel[..3].iter().min().unwrap();
            assert!(
                spread <= 8,
                "{subject} must decode to neutral grey, got {pixel:02x?}"
            );
            assert_eq!(pixel[3], 0xFF, "{subject} must be opaque");
        }
        assert!(
            dark[0] < 0x30,
            "the limited-range black point must decode near black, got {dark:02x?}"
        );
        assert!(
            light[0] > 0xC8,
            "the limited-range white point must decode near white, got {light:02x?}"
        );
    }

    /// The claim is bounded to the copy. In one address space the pool's
    /// accounting *is* the `PixelBuffer` refcount — the count
    /// `PixelBufferRingEntry::hand_off_if_unheld_in_process` reads before it
    /// rehands a slot — so a claim the exchange forgot to drop shows up
    /// here as a hold that never comes back.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn sequential_exchanges_of_one_frame_never_pin_more_than_one_hold() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (published_frame_id, pooled_backing) = gpu
            .acquire_pixel_buffer(FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame's pooled backing");
        fill_pooled_backing_with(&pooled_backing, &PUBLISHED_RGBA8_PIXEL);
        let holds_before_any_exchange = pooled_backing.strong_count();

        for exchange_number in 1..=4 {
            gpu.copy_published_surface_frame_to_host_rgba8_image(
                &published_frame_id.to_string(),
                None,
            )
            .expect("exchange the published frame");
            assert_eq!(
                pooled_backing.strong_count(),
                holds_before_any_exchange,
                "exchange {exchange_number} left a hold on the slot"
            );
        }
    }

    /// A retired `<slot>#<generation>` id names a frame the producer has
    /// already overwritten. Refused by name, before any bytes move — never
    /// answered with the slot's newer pixels.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_retired_frame_id_is_refused_at_the_exchange_naming_the_recycling() {
        // Its own extent, so this test walks a pool no other test has
        // advanced, and the ring is small enough that this many hand-offs
        // cycles it several times over.
        const RECYCLING_FRAME_PIXEL_WIDTH: u32 = 32;
        const RECYCLING_FRAME_PIXEL_HEIGHT: u32 = 32;
        const HAND_OFFS_THAT_CYCLE_THE_RING: usize = 16;

        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (retired_frame_id, pooled_backing) = gpu
            .acquire_pixel_buffer(
                RECYCLING_FRAME_PIXEL_WIDTH,
                RECYCLING_FRAME_PIXEL_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("acquire the frame that will be recycled");
        let retired_frame_id = retired_frame_id.to_string();
        // Nothing holds the slot, so the producer may have it back.
        drop(pooled_backing);
        for _ in 0..HAND_OFFS_THAT_CYCLE_THE_RING {
            let (_, handed_off) = gpu
                .acquire_pixel_buffer(
                    RECYCLING_FRAME_PIXEL_WIDTH,
                    RECYCLING_FRAME_PIXEL_HEIGHT,
                    PixelFormat::Rgba32,
                )
                .expect("the producer keeps acquiring");
            drop(handed_off);
        }

        // `let ... else` rather than `expect_err`: the success value holds
        // a whole frame, and `Debug`-formatting it into a panic message
        // would print megabytes of pixels.
        let Err(refusal) =
            gpu.copy_published_surface_frame_to_host_rgba8_image(&retired_frame_id, None)
        else {
            panic!("a retired frame id must not exchange for pixels");
        };
        assert!(
            matches!(refusal, Error::SurfaceFrameRecycled { .. }),
            "the refusal must name the recycling, got: {refusal}"
        );
        assert!(
            refusal.to_string().contains(&retired_frame_id),
            "the refusal must name the id asked for: {refusal}"
        );
    }

    /// The cap bounds the long edge and nothing else: the image shrinks,
    /// the aspect holds, and the surface's true extent is still reported.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_downscale_cap_bounds_the_long_edge_and_still_states_the_true_extent() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let (published_frame_id, pooled_backing) = gpu
            .acquire_pixel_buffer(FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame's pooled backing");
        fill_pooled_backing_with(&pooled_backing, &PUBLISHED_RGBA8_PIXEL);

        let exchanged = gpu
            .copy_published_surface_frame_to_host_rgba8_image(
                &published_frame_id.to_string(),
                Some(FRAME_PIXEL_WIDTH / 4),
            )
            .expect("exchange the published frame under a cap");

        assert_eq!(
            (exchanged.image_pixel_width, exchanged.image_pixel_height),
            (FRAME_PIXEL_WIDTH / 4, FRAME_PIXEL_HEIGHT / 4),
            "both axes take the cap's ratio"
        );
        assert_eq!(
            (
                exchanged.source_surface_pixel_width,
                exchanged.source_surface_pixel_height
            ),
            (FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT),
            "a downscaled image still reports the resolution the surface holds"
        );
        assert_eq!(
            exchanged.rgba8_pixel_bytes.len(),
            (FRAME_PIXEL_WIDTH / 4 * FRAME_PIXEL_HEIGHT / 4 * 4) as usize
        );
    }

    /// An id nothing published is an absence, not a device fault — the
    /// answer says so and names both doors it tried.
    /// GPU-gated: skips when no device is present.
    #[test]
    fn a_surface_id_no_backing_answers_for_is_reported_as_not_found() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let Err(refusal) =
            gpu.copy_published_surface_frame_to_host_rgba8_image("no-surface-of-this-name", None)
        else {
            panic!("an unknown surface has no pixels to hand back");
        };
        assert!(
            matches!(refusal, Error::NotFound(_)),
            "an unknown surface is an absence, got: {refusal}"
        );
        assert!(refusal.to_string().contains("no-surface-of-this-name"));
    }

    #[test]
    fn no_cap_and_a_cap_above_the_long_edge_both_keep_the_source_extent() {
        assert_eq!(
            downscaled_image_extent_under_long_edge_cap(1920, 1080, None),
            (1920, 1080)
        );
        assert_eq!(
            downscaled_image_extent_under_long_edge_cap(1920, 1080, Some(1920)),
            (1920, 1080)
        );
        assert_eq!(
            downscaled_image_extent_under_long_edge_cap(1920, 1080, Some(4096)),
            (1920, 1080),
            "a cap above the long edge never upscales"
        );
    }

    /// A caller spelling `0` is declining the dial, not asking for an
    /// empty image.
    #[test]
    fn a_zero_cap_reads_as_no_cap() {
        assert_eq!(
            downscaled_image_extent_under_long_edge_cap(1920, 1080, Some(0)),
            (1920, 1080)
        );
    }

    #[test]
    fn a_cap_below_the_long_edge_scales_both_axes_by_the_same_ratio() {
        assert_eq!(
            downscaled_image_extent_under_long_edge_cap(1920, 1080, Some(1568)),
            (1568, 882)
        );
        assert_eq!(
            downscaled_image_extent_under_long_edge_cap(1080, 1920, Some(1568)),
            (882, 1568),
            "the cap follows the long edge whichever axis it is"
        );
    }

    /// A cap far below the source must never produce a zero-pixel axis:
    /// a zero extent is not an image, and every downstream allocation
    /// would refuse it.
    #[test]
    fn an_extreme_cap_still_leaves_at_least_one_pixel_on_each_axis() {
        assert_eq!(
            downscaled_image_extent_under_long_edge_cap(4000, 3, Some(1)),
            (1, 1)
        );
    }
}
