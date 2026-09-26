// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's one surface-to-surface copy: any backing pair, same format
//! and extent, no conversion.

use crate::core::context::surface_backing_resolution::ResolvedSurfaceBacking;
use crate::core::context::texture_registration::TextureLayoutSettledByThisCopy;
use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::error::{Error, Result};
use crate::vulkan::rhi::{ImageCopyRegion, RhiCommandRecorder};
use streamlib_consumer_rhi::{PixelFormat, VulkanLayout};

/// One side of a copy, resolved and measured.
struct SurfaceToSurfaceCopyEndpoint<'a> {
    surface_id: &'a str,
    backing: ResolvedSurfaceBacking,
    pixel_format: PixelFormat,
    pixel_extent: (u32, u32),
}

impl<'a> SurfaceToSurfaceCopyEndpoint<'a> {
    fn resolve_source(gpu: &GpuContext, surface_id: &'a str) -> Result<Self> {
        let source = Self::resolve(gpu, surface_id)?;
        if let ResolvedSurfaceBacking::RegisteredTexture(registration) = &source.backing {
            if registration.current_layout() == VulkanLayout::UNDEFINED {
                return Err(Error::GpuError(format!(
                    "surface {surface_id} has never been written, so it holds no pixels to copy"
                )));
            }
            if !registration.texture().supports_transfer_read() {
                return Err(Error::GpuError(format!(
                    "surface {surface_id}'s texture was allocated without \"copy_src\" usage, so \
                     no copy may read it"
                )));
            }
        }
        Ok(source)
    }

    fn resolve_destination(gpu: &GpuContext, surface_id: &'a str) -> Result<Self> {
        let destination = Self::resolve(gpu, surface_id)?;
        if !gpu.resolved_backing_takes_a_write_back(surface_id, &destination.backing) {
            return Err(destination_takes_no_write_back(surface_id));
        }
        Ok(destination)
    }

    fn resolve(gpu: &GpuContext, surface_id: &'a str) -> Result<Self> {
        gpu.refuse_a_retired_frame_id(surface_id)?;
        let backing = gpu.resolve_device_export_source(surface_id)?;
        let (pixel_format, bytes_per_pixel) = backing
            .one_plane_pixel_format_and_bytes_per_pixel()
            .map_err(|refusal| {
                Error::GpuError(format!("surface {surface_id} cannot be copied: {refusal}"))
            })?;
        let pixel_extent = backing.pixel_extent(surface_id)?;
        if let ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) = &backing {
            let tightly_packed_byte_size =
                u64::from(pixel_extent.0) * u64::from(pixel_extent.1) * u64::from(bytes_per_pixel);
            if pixel_buffer.plane_size(0) != tightly_packed_byte_size {
                return Err(Error::GpuError(format!(
                    "surface {surface_id} is a {}x{} {pixel_format:?} buffer of {} bytes, not the \
                     {tightly_packed_byte_size} its rows would fill tightly packed; a copy reads \
                     rows as tightly packed and would shear the image",
                    pixel_extent.0,
                    pixel_extent.1,
                    pixel_buffer.plane_size(0),
                )));
            }
        }
        Ok(Self {
            surface_id,
            backing,
            pixel_format,
            pixel_extent,
        })
    }
}

/// Refuse a pair that resolves to one allocation — a frame id and its pool
/// slot are two spellings of one buffer, and a copy onto itself overlaps
/// its own source.
fn refuse_a_pair_that_is_one_allocation(
    source: &SurfaceToSurfaceCopyEndpoint<'_>,
    destination: &SurfaceToSurfaceCopyEndpoint<'_>,
) -> Result<()> {
    use crate::host_rhi::HostTextureExt as _;
    use crate::vulkan::rhi::VulkanBufferLike as _;

    let one_allocation = match (&source.backing, &destination.backing) {
        (
            ResolvedSurfaceBacking::PixelBuffer(source_buffer),
            ResolvedSurfaceBacking::PixelBuffer(destination_buffer),
        ) => source_buffer.vk_buffer() == destination_buffer.vk_buffer(),
        (
            ResolvedSurfaceBacking::RegisteredTexture(source_registration),
            ResolvedSurfaceBacking::RegisteredTexture(destination_registration),
        ) => source_registration
            .texture()
            .vulkan_inner()
            .image()
            .is_some_and(|image| {
                Some(image) == destination_registration.texture().vulkan_inner().image()
            }),
        _ => false,
    };
    if one_allocation {
        return Err(Error::GpuError(format!(
            "surfaces {} and {} are one allocation; a copy onto itself would read the pixels \
             it is overwriting",
            source.surface_id, destination.surface_id
        )));
    }
    Ok(())
}

/// Refuse a pair whose pixels differ in format or extent — the copy
/// converts and scales nothing.
///
/// Format identity is the one-buffer pixel shape the engine's export doors
/// present, so an `rgba` pixel buffer and an `rgba8_unorm` texture are one
/// format. Two textures compare their own formats as well: a buffer carries
/// no colour encoding, but a texture does, and copying unorm bytes into an
/// sRGB image changes what they mean.
fn refuse_a_pair_whose_pixels_differ(
    source: &SurfaceToSurfaceCopyEndpoint<'_>,
    destination: &SurfaceToSurfaceCopyEndpoint<'_>,
) -> Result<()> {
    if source.pixel_format != destination.pixel_format {
        return Err(Error::GpuError(format!(
            "format mismatch: surface {} is {:?} and surface {} is {:?}; the copy converts no \
             format",
            source.surface_id,
            source.pixel_format,
            destination.surface_id,
            destination.pixel_format
        )));
    }
    if let (
        ResolvedSurfaceBacking::RegisteredTexture(source_registration),
        ResolvedSurfaceBacking::RegisteredTexture(destination_registration),
    ) = (&source.backing, &destination.backing)
    {
        let (source_format, destination_format) = (
            source_registration.texture().format(),
            destination_registration.texture().format(),
        );
        if source_format != destination_format {
            return Err(Error::GpuError(format!(
                "format mismatch: surface {} is {} and surface {} is {}; the copy converts no \
                 format",
                source.surface_id,
                source_format.wire_name(),
                destination.surface_id,
                destination_format.wire_name()
            )));
        }
    }
    if source.pixel_extent != destination.pixel_extent {
        return Err(Error::GpuError(format!(
            "extent mismatch: surface {} is {}x{} and surface {} is {}x{}; the copy scales \
             nothing",
            source.surface_id,
            source.pixel_extent.0,
            source.pixel_extent.1,
            destination.surface_id,
            destination.pixel_extent.0,
            destination.pixel_extent.1
        )));
    }
    Ok(())
}

/// Record the copy the two backings need, answering the destination
/// texture's settled layout when there is one — a source always comes back
/// to the layout it was in.
fn record_surface_to_surface_copy(
    recorder: &mut RhiCommandRecorder,
    source: &SurfaceToSurfaceCopyEndpoint<'_>,
    destination: &SurfaceToSurfaceCopyEndpoint<'_>,
) -> Result<Option<TextureLayoutSettledByThisCopy>> {
    let (pixel_width, pixel_height) = source.pixel_extent;
    let whole_image = ImageCopyRegion::tightly_packed(pixel_width, pixel_height);
    match (&source.backing, &destination.backing) {
        (
            ResolvedSurfaceBacking::PixelBuffer(source_buffer),
            ResolvedSurfaceBacking::PixelBuffer(destination_buffer),
        ) => {
            recorder.record_buffer_barrier_before_a_transfer_read(source_buffer)?;
            recorder.record_copy_buffer_to_buffer(
                source_buffer,
                destination_buffer,
                source_buffer.plane_size(0),
            )?;
            recorder.record_buffer_barrier_publishing_a_transfer_write(destination_buffer)?;
            Ok(None)
        }
        (
            ResolvedSurfaceBacking::PixelBuffer(source_buffer),
            ResolvedSurfaceBacking::RegisteredTexture(destination_registration),
        ) => {
            let destination_texture = destination_registration.texture();
            recorder.record_buffer_barrier_before_a_transfer_read(source_buffer)?;
            let settled_layout = recorder.record_image_write_as_transfer_destination(
                destination_texture,
                destination_registration.current_layout(),
                |recorder| {
                    recorder.record_copy_buffer_to_image(
                        source_buffer,
                        destination_texture,
                        VulkanLayout::TRANSFER_DST_OPTIMAL,
                        whole_image,
                    )
                },
            )?;
            Ok(Some(TextureLayoutSettledByThisCopy {
                registration: destination_registration.clone(),
                settled_layout,
            }))
        }
        (
            ResolvedSurfaceBacking::RegisteredTexture(source_registration),
            ResolvedSurfaceBacking::PixelBuffer(destination_buffer),
        ) => {
            let source_texture = source_registration.texture();
            recorder.record_image_read_as_transfer_source(
                source_texture,
                source_registration.current_layout(),
                |recorder| {
                    recorder.record_copy_image_to_buffer(
                        source_texture,
                        VulkanLayout::TRANSFER_SRC_OPTIMAL,
                        destination_buffer,
                        whole_image,
                    )
                },
            )?;
            recorder.record_buffer_barrier_publishing_a_transfer_write(destination_buffer)?;
            Ok(None)
        }
        (
            ResolvedSurfaceBacking::RegisteredTexture(source_registration),
            ResolvedSurfaceBacking::RegisteredTexture(destination_registration),
        ) => {
            let settled_layout = recorder.record_copy_image_to_image(
                source_registration.texture(),
                source_registration.current_layout(),
                destination_registration.texture(),
                destination_registration.current_layout(),
            )?;
            Ok(Some(TextureLayoutSettledByThisCopy {
                registration: destination_registration.clone(),
                settled_layout,
            }))
        }
    }
}

fn destination_takes_no_write_back(surface_id: &str) -> Error {
    Error::GpuError(format!(
        "surface {surface_id} cannot take a write-back: it is a pool member its producer still \
         owns, or a texture allocated without \"copy_dst\" usage, so nothing copied into it \
         would publish"
    ))
}

impl GpuContext {
    /// Copy `source_surface_id`'s pixels into `destination_surface_id`,
    /// returning once the copy has retired, with the destination texture
    /// whose layout it settled, if any, under the surface id it resolved
    /// under, for the caller to publish.
    pub(crate) fn copy_surface_to_surface(
        &self,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<Option<(String, TextureRegistration)>> {
        // Both backings stay resolved until the wait returns: nothing else
        // ties the registration check to the submit, and these are what keep
        // the two allocations alive while the GPU reads and writes them.
        let source = SurfaceToSurfaceCopyEndpoint::resolve_source(self, source_surface_id)?;
        let destination =
            SurfaceToSurfaceCopyEndpoint::resolve_destination(self, destination_surface_id)?;
        refuse_a_pair_that_is_one_allocation(&source, &destination)?;
        refuse_a_pair_whose_pixels_differ(&source, &destination)?;

        let mut surface_to_surface_copy_recorder_slot =
            self.surface_to_surface_copy_recorder.lock();
        let recorder = match surface_to_surface_copy_recorder_slot.as_mut() {
            Some(recorder) => recorder,
            None => surface_to_surface_copy_recorder_slot
                .insert(self.create_command_recorder("surface_to_surface_copy")?),
        };
        recorder.begin()?;
        let settled = match record_surface_to_surface_copy(recorder, &source, &destination) {
            Ok(settled) => settled,
            Err(record_failure) => {
                recorder.abort_recording();
                return Err(record_failure);
            }
        };
        recorder.submit()?;
        // Between the submit and the wait: once the queue holds the
        // recording the transitions belong to the GPU, and a failed wait must
        // not leave the tracked layouts describing a state it has left.
        if let Some(settled) = &settled {
            settled.registration.update_layout(settled.settled_layout);
        }
        recorder.wait_for_completion()?;
        drop(surface_to_surface_copy_recorder_slot);

        Ok(settled.map(|settled| (destination_surface_id.to_string(), settled.registration)))
    }
}

impl crate::core::context::GpuContextLimitedAccess {
    /// See [`GpuContext::copy_surface_to_surface`].
    pub(crate) fn copy_surface_to_surface(
        &self,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<Option<(String, TextureRegistration)>> {
        self.host_inner()
            .copy_surface_to_surface(source_surface_id, destination_surface_id)
    }
}
