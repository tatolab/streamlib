// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's one surface-to-surface copy: any backing pair, same format
//! and extent, no conversion.

use crate::core::context::surface_backing_resolution::ResolvedSurfaceBacking;
use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::error::{Error, Result};
use crate::host_rhi::{VulkanAccess, VulkanStage};
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

/// Record the copy the two backings need, answering each texture whose
/// layout the recording settles, with the layout it lands in.
fn record_surface_to_surface_copy(
    recorder: &mut RhiCommandRecorder,
    source: &SurfaceToSurfaceCopyEndpoint<'_>,
    destination: &SurfaceToSurfaceCopyEndpoint<'_>,
) -> Result<Vec<(TextureRegistration, VulkanLayout)>> {
    let (pixel_width, pixel_height) = source.pixel_extent;
    let whole_image = ImageCopyRegion::tightly_packed(pixel_width, pixel_height);
    match (&source.backing, &destination.backing) {
        (
            ResolvedSurfaceBacking::PixelBuffer(source_buffer),
            ResolvedSurfaceBacking::PixelBuffer(destination_buffer),
        ) => {
            recorder.record_buffer_barrier(
                source_buffer,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::TRANSFER_READ,
            )?;
            recorder.record_copy_buffer_to_buffer(
                source_buffer,
                destination_buffer,
                source_buffer.plane_size(0),
            )?;
            recorder.record_buffer_barrier(
                destination_buffer,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::MEMORY_READ,
            )?;
            Ok(Vec::new())
        }
        (
            ResolvedSurfaceBacking::PixelBuffer(source_buffer),
            ResolvedSurfaceBacking::RegisteredTexture(destination_registration),
        ) => {
            let destination_texture = destination_registration.texture();
            let destination_known_layout = destination_registration.current_layout();
            let destination_restore_layout = restore_layout_for(destination_known_layout);
            recorder.record_buffer_barrier(
                source_buffer,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::TRANSFER_READ,
            )?;
            recorder.record_image_barrier(
                destination_texture,
                destination_known_layout,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
                VulkanAccess::TRANSFER_WRITE,
            )?;
            recorder.record_copy_buffer_to_image(
                source_buffer,
                destination_texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                whole_image,
            )?;
            recorder.record_image_barrier(
                destination_texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                destination_restore_layout,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
            )?;
            Ok(vec![(
                destination_registration.clone(),
                destination_restore_layout,
            )])
        }
        (
            ResolvedSurfaceBacking::RegisteredTexture(source_registration),
            ResolvedSurfaceBacking::PixelBuffer(destination_buffer),
        ) => {
            let source_texture = source_registration.texture();
            let source_known_layout = refuse_a_source_never_written(
                source.surface_id,
                source_registration.current_layout(),
            )?;
            if !source_texture.supports_transfer_read() {
                return Err(Error::GpuError(format!(
                    "surface {}'s texture was allocated without \"copy_src\" usage, so no copy \
                     may read it",
                    source.surface_id
                )));
            }
            recorder.record_image_barrier(
                source_texture,
                source_known_layout,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::TRANSFER_READ,
            )?;
            recorder.record_copy_image_to_buffer(
                source_texture,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                destination_buffer,
                whole_image,
            )?;
            recorder.record_image_barrier(
                source_texture,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                source_known_layout,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::TRANSFER_READ,
                VulkanAccess::MEMORY_READ,
            )?;
            recorder.record_buffer_barrier(
                destination_buffer,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::MEMORY_READ,
            )?;
            Ok(Vec::new())
        }
        (
            ResolvedSurfaceBacking::RegisteredTexture(source_registration),
            ResolvedSurfaceBacking::RegisteredTexture(destination_registration),
        ) => {
            let source_known_layout = refuse_a_source_never_written(
                source.surface_id,
                source_registration.current_layout(),
            )?;
            let destination_restore_layout = recorder.record_copy_image_to_image(
                source_registration.texture(),
                source_known_layout,
                destination_registration.texture(),
                destination_registration.current_layout(),
            )?;
            Ok(vec![(
                destination_registration.clone(),
                destination_restore_layout,
            )])
        }
    }
}

/// UNDEFINED is never a barrier target, so an image nothing has written yet
/// comes to rest in GENERAL.
fn restore_layout_for(known_layout: VulkanLayout) -> VulkanLayout {
    if known_layout == VulkanLayout::UNDEFINED {
        VulkanLayout::GENERAL
    } else {
        known_layout
    }
}

fn refuse_a_source_never_written(
    surface_id: &str,
    known_layout: VulkanLayout,
) -> Result<VulkanLayout> {
    if known_layout == VulkanLayout::UNDEFINED {
        return Err(Error::GpuError(format!(
            "surface {surface_id} has never been written, so it holds no pixels to copy"
        )));
    }
    Ok(known_layout)
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
    /// returning once the copy has retired, with each destination texture
    /// and the surface id it resolved under so the caller can publish the
    /// layout it settled in.
    pub(crate) fn copy_surface_to_surface(
        &self,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<Vec<(String, TextureRegistration)>> {
        // Both backings stay resolved until the wait returns: nothing else
        // ties the registration check to the submit, and these are what keep
        // the two allocations alive while the GPU reads and writes them.
        let source = SurfaceToSurfaceCopyEndpoint::resolve(self, source_surface_id)?;
        let destination = SurfaceToSurfaceCopyEndpoint::resolve(self, destination_surface_id)?;
        if !self.resolved_backing_takes_a_write_back(destination_surface_id, &destination.backing) {
            return Err(destination_takes_no_write_back(destination_surface_id));
        }
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
        for (registration, settled_layout) in &settled {
            registration.update_layout(*settled_layout);
        }
        recorder.wait_for_completion()?;
        drop(surface_to_surface_copy_recorder_slot);

        Ok(settled
            .into_iter()
            .map(|(registration, _)| (destination_surface_id.to_string(), registration))
            .collect())
    }
}

impl crate::core::context::GpuContextLimitedAccess {
    /// See [`GpuContext::copy_surface_to_surface`].
    pub(crate) fn copy_surface_to_surface(
        &self,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<Vec<(String, TextureRegistration)>> {
        self.host_inner()
            .copy_surface_to_surface(source_surface_id, destination_surface_id)
    }
}
