// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Recording the conversion of an RGBA or BGRA texture into an RGBA texture.

use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::rhi::{PixelFormat, TextureFormat, VulkanLayout};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{COLOR_CONVERTER_WORKGROUP_SIZE, VulkanAccess, VulkanStage};

use super::{
    RgbaOrBgraTextureSourceLookup, SurfaceColorConversionSession,
    resolve_color_conversion_destination,
};

impl SurfaceColorConversionSession {
    /// Whether `source_surface_id` is an RGBA or BGRA texture another process
    /// registered, imported and kept the first time a conversion names it. A
    /// source this session already reads as YCbCr, or a pooled frame, never
    /// asks the surface store.
    pub(super) fn rgba_or_bgra_texture_source_kept_for(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
    ) -> RgbaOrBgraTextureSourceLookup {
        if self
            .rgba_or_bgra_texture_sources_by_surface_id
            .contains_key(source_surface_id)
        {
            return RgbaOrBgraTextureSourceLookup::Kept;
        }
        let not_a_texture_source = RgbaOrBgraTextureSourceLookup::NotATextureSource {
            texture_lookup_refusal: None,
        };
        if self.sources_by_surface_id.contains_key(source_surface_id)
            || crate::core::rhi::PublishedPixelBufferFrameId::parse(source_surface_id).is_some()
        {
            return not_a_texture_source;
        }
        let Some(store) = gpu.surface_store() else {
            return not_a_texture_source;
        };
        let (texture, layout_the_producer_left_it_in) =
            match store.lookup_texture(source_surface_id) {
                Ok(looked_up) => looked_up,
                Err(texture_lookup_refusal) => {
                    return RgbaOrBgraTextureSourceLookup::NotATextureSource {
                        texture_lookup_refusal: Some(texture_lookup_refusal),
                    };
                }
            };
        if !matches!(
            texture.format(),
            TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm
        ) {
            return not_a_texture_source;
        }
        self.rgba_or_bgra_texture_sources_by_surface_id.insert(
            source_surface_id.to_string(),
            TextureRegistration::new(texture, layout_the_producer_left_it_in),
        );
        RgbaOrBgraTextureSourceLookup::Kept
    }

    /// Copy the RGBA or BGRA texture `source_surface_id` into the RGBA
    /// texture `destination_surface_id`, texel for texel, and return once the
    /// destination's next reader sees it — with the destination's
    /// registration, whose layout the conversion settled.
    ///
    /// The source is another process's image this one imported, so its
    /// contents reach this queue through no earlier submission: the barrier
    /// that makes them visible waits on nothing, and leaves the image in the
    /// layout the next conversion transitions it from.
    pub(super) fn convert_an_rgba_or_bgra_texture_into_an_rgba_texture(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<(String, TextureRegistration)> {
        const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout =
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL;
        let source = self
            .rgba_or_bgra_texture_sources_by_surface_id
            .get(source_surface_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "surface {source_surface_id} was resolved and is gone"
                ))
            })?;
        let source_texture = source.texture();
        let destination = resolve_color_conversion_destination(gpu, destination_surface_id)?;
        let destination_texture = destination.texture();
        let source_pixel_format = match source_texture.format() {
            TextureFormat::Bgra8Unorm => PixelFormat::Bgra32,
            _ => PixelFormat::Rgba32,
        };
        let color_converter = match self
            .color_converters_by_source_format
            .get(&source_pixel_format)
        {
            Some(color_converter) => color_converter,
            None => self
                .color_converters_by_source_format
                .entry(source_pixel_format)
                .or_insert(gpu.create_color_converter(source_pixel_format, PixelFormat::Rgba32)?),
        };
        let kernel =
            color_converter.prepare_image_to_image_storage(source_texture, destination_texture)?;
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
                VulkanStage::NONE,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::NONE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.record_image_barrier(
                destination_texture,
                destination.current_layout(),
                VulkanLayout::GENERAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE | VulkanAccess::MEMORY_READ,
                VulkanAccess::SHADER_WRITE,
            )?;
            recorder.record_dispatch(
                &kernel,
                destination_texture
                    .width()
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                destination_texture
                    .height()
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
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
        source.update_layout(LAYOUT_THE_CONVERSION_SAMPLES_IN);
        destination.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        recorder.wait_for_completion()?;
        Ok((destination_surface_id.to_string(), destination))
    }
}
