// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the colour converter's image→image kernel converts, 8-bit RGBA and BGRA textures sampled
//! as stored into `rgba8_unorm`, and the one recording of its dispatch.

use std::sync::Arc;

use crate::core::context::GpuContext;
use crate::core::rhi::{PixelFormat, RhiColorConverter, Texture, TextureFormat, VulkanLayout};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    COLOR_CONVERTER_WORKGROUP_SIZE, RhiCommandRecorder, VulkanAccess, VulkanComputeKernel,
    VulkanStage,
};

/// The texture formats the image→image kernel samples as they are stored: 8-bit RGBA and BGRA.
/// No sRGB format is among them, since sampling one decodes it.
pub const TEXTURE_FORMATS_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_AS_STORED: [TextureFormat; 2] =
    [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm];

/// The layout the image→image kernel samples its source in.
pub(crate) const LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN: VulkanLayout =
    VulkanLayout::SHADER_READ_ONLY_OPTIMAL;

/// Whether the image→image kernel samples `format` as it is stored.
pub fn the_image_to_image_kernel_samples_as_stored(format: TextureFormat) -> bool {
    TEXTURE_FORMATS_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_AS_STORED.contains(&format)
}

/// The format `format_wire_value` names, refused naming it unless the image→image kernel samples
/// it as it is stored.
pub fn texture_format_the_image_to_image_kernel_samples(
    format_wire_value: u32,
) -> Result<TextureFormat> {
    let format = TextureFormat::from_repr_value(format_wire_value);
    format
        .filter(|format| the_image_to_image_kernel_samples_as_stored(*format))
        .ok_or_else(|| {
            refusal_of_a_format_the_kernel_does_not_sample_as_stored(&format.map_or_else(
                || format!("an unknown texture format ({format_wire_value})"),
                |format| format.wire_name().to_string(),
            ))
        })
}

fn refusal_of_a_format_the_kernel_does_not_sample_as_stored(named_format: &str) -> Error {
    let formats_it_samples: Vec<&str> = TEXTURE_FORMATS_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_AS_STORED
        .iter()
        .map(|format| format.wire_name())
        .collect();
    Error::NotSupported(format!(
        "a texture of {named_format} is not converted into rgba8_unorm; only {} are",
        formats_it_samples.join(" and ")
    ))
}

/// One colour converter for each format the image→image kernel samples as stored, each with its
/// kernel's pipeline already built.
pub(crate) struct ImageToImageColorConvertersWithTheirKernelsBuilt {
    color_converter_sampling_rgba8_unorm: RhiColorConverter,
    color_converter_sampling_bgra8_unorm: RhiColorConverter,
}

impl ImageToImageColorConvertersWithTheirKernelsBuilt {
    /// Both converters, their pipelines built now. It creates pipelines, so it runs only where
    /// full GPU access is held.
    pub(crate) fn create(gpu: &GpuContext) -> Result<Self> {
        let color_converter_sampling_rgba8_unorm =
            gpu.create_color_converter(PixelFormat::Rgba32, PixelFormat::Rgba32)?;
        color_converter_sampling_rgba8_unorm.build_the_image_to_image_kernel()?;
        let color_converter_sampling_bgra8_unorm =
            gpu.create_color_converter(PixelFormat::Bgra32, PixelFormat::Rgba32)?;
        color_converter_sampling_bgra8_unorm.build_the_image_to_image_kernel()?;
        Ok(Self {
            color_converter_sampling_rgba8_unorm,
            color_converter_sampling_bgra8_unorm,
        })
    }

    /// The kernel with `source` bound to be sampled and `destination` to be written, refused
    /// unless the kernel samples `source`'s format as stored.
    pub(crate) fn prepare_dispatch(
        &self,
        source: &Texture,
        destination: &Texture,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let color_converter = match source.format() {
            TextureFormat::Rgba8Unorm => &self.color_converter_sampling_rgba8_unorm,
            TextureFormat::Bgra8Unorm => &self.color_converter_sampling_bgra8_unorm,
            other => {
                return Err(refusal_of_a_format_the_kernel_does_not_sample_as_stored(
                    other.wire_name(),
                ));
            }
        };
        color_converter.prepare_image_to_image_storage(source, destination)
    }
}

/// The texture one image→image dispatch samples: the layout it is in, and the stage and access
/// of the last write the dispatch waits on.
pub(crate) struct TextureTheImageToImageKernelSamples<'texture> {
    pub(crate) texture: &'texture Texture,
    pub(crate) layout: VulkanLayout,
    pub(crate) last_written_in_stage: VulkanStage,
    pub(crate) last_written_with_access: VulkanAccess,
}

/// Record the barriers into the kernel's layouts and its dispatch over `destination`, which it
/// leaves in GENERAL, its source in [`LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN`].
pub(crate) fn record_the_image_to_image_dispatch(
    recorder: &mut RhiCommandRecorder,
    kernel: &VulkanComputeKernel,
    source: &TextureTheImageToImageKernelSamples<'_>,
    destination: &Texture,
    destination_layout: VulkanLayout,
) -> Result<()> {
    recorder.record_image_barrier(
        source.texture,
        source.layout,
        LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN,
        source.last_written_in_stage,
        VulkanStage::COMPUTE_SHADER,
        source.last_written_with_access,
        VulkanAccess::SHADER_SAMPLED_READ,
    )?;
    recorder.record_image_barrier(
        destination,
        destination_layout,
        VulkanLayout::GENERAL,
        VulkanStage::ALL_COMMANDS,
        VulkanStage::COMPUTE_SHADER,
        VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        VulkanAccess::SHADER_WRITE,
    )?;
    recorder.record_dispatch(
        kernel,
        destination.width().div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
        destination
            .height()
            .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
        1,
    )
}
