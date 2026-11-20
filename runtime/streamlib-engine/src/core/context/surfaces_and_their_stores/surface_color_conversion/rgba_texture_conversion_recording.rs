// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Recording the conversion of an RGBA or BGRA texture into an RGBA texture.

use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::rhi::{VulkanLayout, the_image_to_image_kernel_samples_as_stored};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    ImageToImageColorConvertersWithTheirKernelsBuilt, LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN,
    TextureTheImageToImageKernelSamples, VulkanAccess, VulkanStage,
    record_the_image_to_image_dispatch,
};

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
        if !the_image_to_image_kernel_samples_as_stored(texture.format()) {
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
        let image_to_image_color_converters = match self.image_to_image_color_converters.as_ref() {
            Some(image_to_image_color_converters) => image_to_image_color_converters,
            None => self.image_to_image_color_converters.insert(
                ImageToImageColorConvertersWithTheirKernelsBuilt::create(gpu)?,
            ),
        };
        let kernel = image_to_image_color_converters
            .prepare_dispatch(source_texture, destination_texture)?;
        let recorder = match self.conversion_recorder.as_mut() {
            Some(recorder) => recorder,
            None => self
                .conversion_recorder
                .insert(gpu.create_command_recorder("surface_color_conversion")?),
        };
        recorder.begin()?;
        let recorded = (|| -> Result<()> {
            record_the_image_to_image_dispatch(
                recorder,
                &kernel,
                &TextureTheImageToImageKernelSamples {
                    texture: source_texture,
                    layout: source.current_layout(),
                    last_written_in_stage: VulkanStage::NONE,
                    last_written_with_access: VulkanAccess::NONE,
                },
                destination_texture,
                destination.current_layout(),
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
        source.update_layout(LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN);
        destination.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        recorder.wait_for_completion()?;
        Ok((destination_surface_id.to_string(), destination))
    }
}
