// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Recording the conversion of an RGBA texture into an adopted NV12 IOSurface (macOS).

use crate::core::color::{ColorSpaceKind, H273ColorVui};
use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::rhi::{PixelFormat, RhiColorConverter, VulkanLayout};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{VulkanAccess, VulkanStage};

use super::{
    AdoptedNv12IOSurfaceConversionDestination, SurfaceColorConversionSession,
    a_pooled_ycbcr_frame_held_in_this_process,
};

impl SurfaceColorConversionSession {
    /// Whether `destination_surface_id` is an adopted NV12 IOSurface, resolved
    /// and kept the first time a conversion names it. A source this session
    /// already reads as YCbCr, or a pooled YCbCr frame, never asks the surface
    /// store.
    #[cfg(target_os = "macos")]
    pub(super) fn converts_into_an_adopted_nv12_iosurface(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<bool> {
        if self
            .nv12_iosurface_destinations_by_surface_id
            .contains_key(destination_surface_id)
        {
            return Ok(true);
        }
        if self.sources_by_surface_id.contains_key(source_surface_id)
            || a_pooled_ycbcr_frame_held_in_this_process(gpu, source_surface_id)
        {
            return Ok(false);
        }
        let Some(store) = gpu.surface_store() else {
            return Ok(false);
        };
        let Ok(registration) =
            store.lookup_iosurface_registration_without_importing_it(destination_surface_id)
        else {
            return Ok(false);
        };
        if !matches!(
            registration.pixel_format,
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange
        ) {
            return Ok(false);
        }
        let imported_iosurface = gpu.import_iosurface_as_storage_buffer(&registration.iosurface)?;
        self.nv12_iosurface_destinations_by_surface_id.insert(
            destination_surface_id.to_string(),
            AdoptedNv12IOSurfaceConversionDestination {
                imported_iosurface,
                pixel_format: registration.pixel_format,
                width: registration.width,
                height: registration.height,
            },
        );
        Ok(true)
    }

    /// Convert the RGBA texture `source_surface_id` — or an RGBA pixel buffer,
    /// refreshed into a texture first — into the adopted NV12 IOSurface
    /// `destination_surface_id` in `ycbcr_side_color`'s matrix, and
    /// return once the surface's CPU and media-engine readers see it — with
    /// the source's registration, whose layout the conversion settled.
    #[cfg(target_os = "macos")]
    pub(super) fn convert_an_rgba_texture_into_an_adopted_nv12_iosurface(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
        ycbcr_side_color: &H273ColorVui,
    ) -> Result<(String, TextureRegistration)> {
        const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout =
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL;
        let destination = self
            .nv12_iosurface_destinations_by_surface_id
            .get(destination_surface_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "surface {destination_surface_id} was resolved and is gone"
                ))
            })?;
        let source = gpu.resolve_texture_registration_by_surface_id(
            source_surface_id,
            None,
            destination.width,
            destination.height,
        )?;
        let source_texture = source.texture();
        if (source_texture.width(), source_texture.height())
            != (destination.width, destination.height)
        {
            return Err(Error::GpuError(format!(
                "texture {source_surface_id} is {}x{} and IOSurface {destination_surface_id} is \
                 {}x{}; the conversion scales nothing",
                source_texture.width(),
                source_texture.height(),
                destination.width,
                destination.height
            )));
        }
        let destination_is_full_range = destination.pixel_format == PixelFormat::Nv12FullRange;
        if let Some(stated_full_range) = ycbcr_side_color.full_range
            && stated_full_range != destination_is_full_range
        {
            return Err(Error::GpuError(format!(
                "IOSurface {destination_surface_id} is {:?} and the conversion was asked for {} \
                 range",
                destination.pixel_format,
                if stated_full_range { "full" } else { "limited" }
            )));
        }
        let resolved_color = crate::core::color::ResolveH273ColorVuiDefaults::resolve_defaults(
            &H273ColorVui {
                full_range: Some(destination_is_full_range),
                ..*ycbcr_side_color
            },
            ColorSpaceKind::Yuv,
        );
        let color_converter = match self
            .rgba_to_nv12_color_converters_by_destination_format
            .get(&destination.pixel_format)
        {
            Some(color_converter) => color_converter,
            None => self
                .rgba_to_nv12_color_converters_by_destination_format
                .entry(destination.pixel_format)
                .or_insert(
                    gpu.create_color_converter(PixelFormat::Rgba32, destination.pixel_format)?,
                ),
        };
        let destination_storage_buffer = destination.imported_iosurface.storage_buffer();
        let kernel = color_converter.prepare_image_to_nv12_buffer(
            source_texture,
            destination_storage_buffer,
            destination.imported_iosurface.nv12_source_layout()?,
            &resolved_color,
        )?;
        let (dispatch_group_x, dispatch_group_y) =
            RhiColorConverter::image_to_nv12_buffer_dispatch_group_counts(
                source_texture.width(),
                source_texture.height(),
            );
        let recorder = match self.conversion_recorder.as_mut() {
            Some(recorder) => recorder,
            None => self
                .conversion_recorder
                .insert(gpu.create_command_recorder("surface_color_conversion")?),
        };
        recorder.begin()?;
        let recorded = (|| -> Result<()> {
            recorder.record_image_barrier(
                source_texture,
                source.current_layout(),
                LAYOUT_THE_CONVERSION_SAMPLES_IN,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.record_dispatch(&kernel, dispatch_group_x, dispatch_group_y, 1)?;
            recorder.record_buffer_barrier(
                destination_storage_buffer,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::HOST,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::HOST_READ,
            )
        })();
        if let Err(record_failure) = recorded {
            recorder.abort_recording();
            return Err(record_failure);
        }
        recorder.submit()?;
        source.update_layout(LAYOUT_THE_CONVERSION_SAMPLES_IN);
        recorder.wait_for_completion()?;
        Ok((source_surface_id.to_string(), source))
    }
}
