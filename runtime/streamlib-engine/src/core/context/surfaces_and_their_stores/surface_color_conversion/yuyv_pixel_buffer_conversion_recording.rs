// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Recording the conversion of an RGBA texture into a `yuyv` pixel buffer (Linux).

use crate::core::color::{ColorSpaceKind, H273ColorVui};
use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::rhi::{PixelFormat, VulkanLayout};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{COLOR_CONVERTER_WORKGROUP_SIZE, VulkanAccess, VulkanStage};

use super::{SurfaceColorConversionSession, a_pooled_ycbcr_frame_held_in_this_process};

impl SurfaceColorConversionSession {
    /// Whether `destination_surface_id` is a `yuyv` pixel buffer, resolved and
    /// kept the first time a conversion names it. A source this session
    /// already reads as YCbCr, or a pooled frame, never asks the surface
    /// store.
    #[cfg(target_os = "linux")]
    pub(super) fn converts_into_a_yuyv_pixel_buffer(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<bool> {
        if self
            .yuyv_pixel_buffer_destinations_by_surface_id
            .contains_key(destination_surface_id)
        {
            return Ok(true);
        }
        if self.sources_by_surface_id.contains_key(source_surface_id)
            || self
                .rgba_or_bgra_texture_sources_by_surface_id
                .contains_key(source_surface_id)
            || a_pooled_ycbcr_frame_held_in_this_process(gpu, source_surface_id)
        {
            return Ok(false);
        }
        let Ok(pixel_buffer) = gpu.resolve_pixel_buffer_by_surface_id(destination_surface_id)
        else {
            return Ok(false);
        };
        if pixel_buffer.format() != PixelFormat::Yuyv422 {
            return Ok(false);
        }
        self.yuyv_pixel_buffer_destinations_by_surface_id
            .insert(destination_surface_id.to_string(), pixel_buffer);
        Ok(true)
    }

    /// Convert the RGBA texture `source_surface_id` into the `yuyv` pixel
    /// buffer `destination_surface_id`, tightly packed, in
    /// `ycbcr_side_color`'s matrix and range, and return once the buffer's
    /// CPU readers see it — with the source's registration, whose layout the
    /// conversion settled.
    #[cfg(target_os = "linux")]
    pub(super) fn convert_an_rgba_texture_into_a_yuyv_pixel_buffer(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
        ycbcr_side_color: &H273ColorVui,
    ) -> Result<(String, TextureRegistration)> {
        const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout =
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL;
        let destination = self
            .yuyv_pixel_buffer_destinations_by_surface_id
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
                "texture {source_surface_id} is {}x{} and pixel buffer {destination_surface_id} \
                 is {}x{}; the conversion scales nothing",
                source_texture.width(),
                source_texture.height(),
                destination.width,
                destination.height
            )));
        }
        let resolved_color = crate::core::color::ResolveH273ColorVuiDefaults::resolve_defaults(
            ycbcr_side_color,
            ColorSpaceKind::Yuv,
        );
        let color_converter = match self.rgba_to_yuyv_color_converter.as_ref() {
            Some(color_converter) => color_converter,
            None => self
                .rgba_to_yuyv_color_converter
                .insert(gpu.create_color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)?),
        };
        let kernel = color_converter.prepare_image_to_yuyv_pixel_buffer(
            source_texture,
            destination,
            destination.width * 2,
            &resolved_color,
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
                source_texture,
                source.current_layout(),
                LAYOUT_THE_CONVERSION_SAMPLES_IN,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.record_dispatch(
                &kernel,
                destination
                    .width
                    .div_ceil(2)
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                destination.height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                1,
            )?;
            recorder.record_buffer_barrier(
                destination,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::HOST,
                VulkanAccess::SHADER_WRITE,
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
