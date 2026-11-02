// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's colour converter run on surfaces named by id: a YCbCr pixel
//! buffer — a camera's imported DMA-BUF, a decoder's output — converted into
//! an RGBA texture a stream publishes.

use std::collections::HashMap;

use crate::core::color::{ColorSpaceKind, H273ColorVui};
use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, PixelPlanesWithinOneAllocation, RhiColorConverter, TextureFormat,
    VulkanLayout,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    COLOR_CONVERTER_WORKGROUP_SIZE, RhiCommandRecorder, VulkanAccess, VulkanStage,
};

/// A conversion source resolved once and read on every later conversion: the
/// imported allocation, what its registration says its pixels are, and the
/// layout the converter reads them through.
struct SurfaceColorConversionSource {
    pixel_buffer: PixelBuffer,
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    planes: PixelPlanesWithinOneAllocation,
}

/// One caller's conversions: the sources it has named, a converter per source
/// format, and the recorder they dispatch on.
///
/// Owned by one helper's escalate registry, so the converters' staged bindings
/// are never shared with another node's conversions, and what a helper
/// resolved goes away with it.
#[derive(Default)]
pub(crate) struct SurfaceColorConversionSession {
    sources_by_surface_id: HashMap<String, SurfaceColorConversionSource>,
    color_converters_by_source_format: HashMap<PixelFormat, RhiColorConverter>,
    conversion_recorder: Option<RhiCommandRecorder>,
}

impl SurfaceColorConversionSession {
    /// Convert `source_surface_id`'s YCbCr pixels, described by
    /// `source_color`, into the RGBA texture `destination_surface_id`, keeping
    /// the source's transfer curve, and return once the destination's next
    /// reader sees them — with the registration whose layout the conversion
    /// settled, under the id it resolved under, for the caller to publish.
    pub(crate) fn convert_surface_into(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
        source_color: &H273ColorVui,
    ) -> Result<(String, TextureRegistration)> {
        if !self.sources_by_surface_id.contains_key(source_surface_id) {
            let resolved = resolve_color_conversion_source(gpu, source_surface_id)?;
            self.sources_by_surface_id
                .insert(source_surface_id.to_string(), resolved);
        }
        let source = &self.sources_by_surface_id[source_surface_id];
        let destination = resolve_color_conversion_destination(gpu, destination_surface_id)?;
        let destination_texture = destination.texture();
        if (destination_texture.width(), destination_texture.height())
            != (source.width, source.height)
        {
            return Err(Error::GpuError(format!(
                "surface {source_surface_id} is {}x{} and destination {destination_surface_id} \
                 is {}x{}; the conversion scales nothing",
                source.width,
                source.height,
                destination_texture.width(),
                destination_texture.height()
            )));
        }

        let source_layout = source
            .planes
            .color_converter_source_layout(source.pixel_format)?;
        let resolved_color = source_color.resolve_defaults(ColorSpaceKind::Yuv);
        let color_converter = match self
            .color_converters_by_source_format
            .get(&source.pixel_format)
        {
            Some(color_converter) => color_converter,
            None => self
                .color_converters_by_source_format
                .entry(source.pixel_format)
                .or_insert(gpu.create_color_converter(source.pixel_format, PixelFormat::Rgba32)?),
        };
        let kernel = color_converter.prepare_buffer_to_image_pixel(
            &source.pixel_buffer,
            source_layout,
            destination_texture,
            &resolved_color,
            resolved_color.transfer,
        )?;

        let recorder = match self.conversion_recorder.as_mut() {
            Some(recorder) => recorder,
            None => self
                .conversion_recorder
                .insert(gpu.create_command_recorder("surface_color_conversion")?),
        };
        recorder.begin()?;
        let recorded = (|| -> Result<()> {
            recorder.record_image_barrier(
                destination_texture,
                destination.current_layout(),
                VulkanLayout::GENERAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE | VulkanAccess::MEMORY_READ,
                VulkanAccess::SHADER_WRITE,
            )?;
            // A foreign writer's stores reach the GPU through no earlier
            // submission, so the read is made available from nothing; a buffer
            // the GPU or the CPU filled is ordered by the wide barrier every
            // read of an unknown producer takes.
            if source.planes.written_by_a_foreign_device() {
                recorder.record_buffer_barrier(
                    &source.pixel_buffer,
                    VulkanStage::NONE,
                    VulkanStage::COMPUTE_SHADER,
                    VulkanAccess::NONE,
                    VulkanAccess::SHADER_READ,
                )?;
            } else {
                recorder.record_buffer_barrier(
                    &source.pixel_buffer,
                    VulkanStage::ALL_COMMANDS,
                    VulkanStage::COMPUTE_SHADER,
                    VulkanAccess::MEMORY_WRITE,
                    VulkanAccess::SHADER_READ,
                )?;
            }
            recorder.record_dispatch(
                &kernel,
                source.width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                source.height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                1,
            )?;
            recorder.record_image_barrier(
                destination_texture,
                VulkanLayout::GENERAL,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::SHADER_WRITE,
                VulkanAccess::MEMORY_READ,
            )
        })();
        if let Err(record_failure) = recorded {
            recorder.abort_recording();
            return Err(record_failure);
        }
        recorder.submit()?;
        destination.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        recorder.wait_for_completion()?;
        Ok((destination_surface_id.to_string(), destination))
    }

    /// Forget what this session resolved for `surface_id`, answering whether
    /// it held anything.
    pub(crate) fn forget_source(&mut self, surface_id: &str) -> bool {
        self.sources_by_surface_id.remove(surface_id).is_some()
    }
}

/// The pixel buffer `surface_id` names, with its format, extent and layout:
/// this process's own pooled buffer when it holds one, else the surface-share
/// registration's.
fn resolve_color_conversion_source(
    gpu: &GpuContext,
    surface_id: &str,
) -> Result<SurfaceColorConversionSource> {
    gpu.refuse_a_retired_frame_id(surface_id)?;
    if let Some(pixel_buffer) = gpu.pooled_backing_held_in_this_process(surface_id)
        && pixel_buffer.width != 0
    {
        let pixel_format = pixel_buffer.format();
        let (width, height) = (pixel_buffer.width, pixel_buffer.height);
        let tight = PixelPlanesWithinOneAllocation::tightly_packed(pixel_format, width, height)?;
        let planes = PixelPlanesWithinOneAllocation::new(
            pixel_format,
            width,
            height,
            pixel_buffer.plane_size(0),
            tight.plane_offsets_bytes().to_vec(),
            tight.plane_strides_bytes().to_vec(),
            false,
        )?;
        return Ok(SurfaceColorConversionSource {
            pixel_buffer,
            pixel_format,
            width,
            height,
            planes,
        });
    }
    resolve_color_conversion_source_through_the_surface_store(gpu, surface_id)
}

#[cfg(target_os = "linux")]
fn resolve_color_conversion_source_through_the_surface_store(
    gpu: &GpuContext,
    surface_id: &str,
) -> Result<SurfaceColorConversionSource> {
    let store = gpu.surface_store().ok_or_else(|| {
        Error::Configuration(format!(
            "surface {surface_id} is not a pixel buffer this process holds, and no surface-share \
             service is connected to look it up"
        ))
    })?;
    let looked_up = store.lookup_pixel_buffer_with_its_plane_layout(surface_id)?;
    Ok(SurfaceColorConversionSource {
        pixel_buffer: looked_up.pixel_buffer,
        pixel_format: looked_up.pixel_format,
        width: looked_up.width,
        height: looked_up.height,
        planes: looked_up.planes,
    })
}

#[cfg(target_os = "macos")]
fn resolve_color_conversion_source_through_the_surface_store(
    _gpu: &GpuContext,
    surface_id: &str,
) -> Result<SurfaceColorConversionSource> {
    Err(Error::NotSupported(format!(
        "surface {surface_id} is not a pixel buffer this process holds; a cross-process \
         conversion source is a Linux DMA-BUF import today"
    )))
}

/// The texture `surface_id` names, refused unless the converter can write it:
/// an `rgba8_unorm` texture allocated with `storage_binding` usage.
fn resolve_color_conversion_destination(
    gpu: &GpuContext,
    surface_id: &str,
) -> Result<TextureRegistration> {
    gpu.refuse_a_retired_frame_id(surface_id)?;
    let registration = gpu.resolve_texture_registration_by_surface_id(surface_id, None, 0, 0)?;
    let texture = registration.texture();
    if texture.format() != TextureFormat::Rgba8Unorm {
        return Err(Error::GpuError(format!(
            "surface {surface_id} is a {:?} texture; the conversion writes rgba8_unorm",
            texture.format()
        )));
    }
    if !texture.supports_storage_write() {
        return Err(Error::GpuError(format!(
            "surface {surface_id}'s texture was allocated without \"storage_binding\" usage, so \
             the conversion cannot write it"
        )));
    }
    Ok(registration)
}
