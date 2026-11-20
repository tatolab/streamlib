// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The engine's conversion of an 8-bit RGBA or BGRA Image frame into an `rgba8_unorm` one: the
//! colour converter's image-to-image kernel samples the frame into a scratch texture, which is
//! copied into a pixel buffer from the converting node's own output pool, in one submission.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use streamlib_ipc_types::ImageFrameDescriptorBody;

use crate::core::compiler::compiler_ops::publish_the_layouts_a_dispatch_left_its_bound_textures_in;
use crate::core::context::surfaces_and_their_stores::surface_backing_resolution::ResolvedSurfaceBacking;
use crate::core::context::{
    GpuContext, GpuContextFullAccess, GpuContextLimitedAccess,
    NodeOutputPixelBufferPoolOfOneNativeNode,
};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, RhiColorConverter, Texture, TextureDescriptor, TextureFormat,
    TextureUsages, VulkanLayout,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    COLOR_CONVERTER_WORKGROUP_SIZE, ImageCopyRegion, RhiCommandRecorder, VulkanAccess,
    VulkanComputeKernel, VulkanStage,
};

/// The texture formats the image-to-image kernel samples as they are stored: 8-bit RGBA and
/// BGRA. An sRGB format is not among them, since sampling one decodes it.
pub const TEXTURE_FORMATS_CONVERTED_INTO_RGBA8_UNORM: [TextureFormat; 2] =
    [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm];

/// The pixels the kernel samples, as one conversion records them.
enum PixelsTheKernelSamples<'pixels> {
    /// The frame's own texture, in the layout it was left in.
    FramesOwnTexture {
        texture: &'pixels Texture,
        known_layout: VulkanLayout,
    },
    /// The frame's pixel buffer, copied first into a texture of the stage's own.
    PixelBufferCopiedIntoATexture {
        pixel_buffer: &'pixels PixelBuffer,
        buffer_row_length_in_pixels: u32,
        texture: &'pixels Texture,
    },
}

/// One native node's conversion of 8-bit RGBA or BGRA Image frames into `rgba8_unorm` ones,
/// written into pixel buffers from the node's own output pool.
///
/// Owned by the node, which is the only one that converts through it, and each conversion
/// returns once the GPU has written its frame.
pub struct RgbaOrBgraImageFrameToPooledRgbaConversionStage {
    node_run_id_of_the_owning_node: String,
    color_converters_by_source_format: HashMap<PixelFormat, RhiColorConverter>,
    recorder: RhiCommandRecorder,
    texture_a_source_pixel_buffer_is_copied_into: Option<Texture>,
    rgba_texture_the_kernel_writes: Option<Texture>,
    node_output_pixel_buffer_pool: NodeOutputPixelBufferPoolOfOneNativeNode,
}

impl RgbaOrBgraImageFrameToPooledRgbaConversionStage {
    /// The stage of the node `node_run_id_of_the_owning_node`, publishing from an output pool of
    /// that node's own.
    pub fn create(
        full: &GpuContextFullAccess,
        node_run_id_of_the_owning_node: &str,
    ) -> Result<Self> {
        Self::create_on(full.host_inner(), node_run_id_of_the_owning_node)
    }

    pub(crate) fn create_on(
        gpu: &GpuContext,
        node_run_id_of_the_owning_node: &str,
    ) -> Result<Self> {
        Ok(Self {
            node_run_id_of_the_owning_node: node_run_id_of_the_owning_node.to_string(),
            color_converters_by_source_format: HashMap::new(),
            recorder: gpu.create_command_recorder("rgba_or_bgra_image_frame_to_rgba")?,
            texture_a_source_pixel_buffer_is_copied_into: None,
            rgba_texture_the_kernel_writes: None,
            node_output_pixel_buffer_pool: gpu
                .create_node_output_pixel_buffer_pool_for_a_native_node(
                    node_run_id_of_the_owning_node,
                ),
        })
    }

    /// Convert the Image frame `image` names into the next pixel buffer of the node's output
    /// pool, answering the `rgba8_unorm` Image frame that names it, of `image`'s extent and
    /// colour. A frame of a format outside [`TEXTURE_FORMATS_CONVERTED_INTO_RGBA8_UNORM`] is
    /// refused naming it.
    pub fn convert_into_the_next_pooled_pixel_buffer(
        &mut self,
        gpu: &GpuContextLimitedAccess,
        image: &ImageFrameDescriptorBody,
    ) -> Result<ImageFrameDescriptorBody> {
        self.convert_on(gpu.host_inner(), image)
    }

    pub(crate) fn convert_on(
        &mut self,
        gpu: &GpuContext,
        image: &ImageFrameDescriptorBody,
    ) -> Result<ImageFrameDescriptorBody> {
        let source_format = texture_format_the_kernel_samples(image.format)?;
        let (width, height) = (image.width, image.height);
        let source_surface_id = gpu.published_frame_id_of_the_gpu_pool_slot_frame(&image.slot)?;
        let _claim_until_the_conversion_has_run =
            gpu.claim_surface_against_producer_reuse(&source_surface_id)?;
        let source_backing = gpu.resolve_device_export_source(&source_surface_id)?;
        let rgba_texture_the_kernel_writes = texture_of_the_shape(
            gpu,
            &mut self.rgba_texture_the_kernel_writes,
            width,
            height,
            TextureFormat::Rgba8Unorm,
            TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
        )?;
        let pixels_the_kernel_samples = match &source_backing {
            ResolvedSurfaceBacking::RegisteredTexture(registration) => {
                let texture = registration.texture();
                if texture.format() != source_format {
                    return Err(Error::GpuError(format!(
                        "surface {source_surface_id} is a {} texture, and its Image frame states \
                         {}",
                        texture.format().wire_name(),
                        source_format.wire_name()
                    )));
                }
                PixelsTheKernelSamples::FramesOwnTexture {
                    texture,
                    known_layout: registration.current_layout(),
                }
            }
            ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) => {
                let buffer_row_length_in_pixels = buffer_row_length_in_pixels_of(
                    &source_surface_id,
                    pixel_buffer,
                    source_format,
                    width,
                    height,
                )?;
                PixelsTheKernelSamples::PixelBufferCopiedIntoATexture {
                    pixel_buffer,
                    buffer_row_length_in_pixels,
                    texture: texture_of_the_shape(
                        gpu,
                        &mut self.texture_a_source_pixel_buffer_is_copied_into,
                        width,
                        height,
                        source_format,
                        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                    )?,
                }
            }
        };
        let source_texture = match &pixels_the_kernel_samples {
            PixelsTheKernelSamples::FramesOwnTexture { texture, .. }
            | PixelsTheKernelSamples::PixelBufferCopiedIntoATexture { texture, .. } => *texture,
        };
        let kernel = color_converter_kept_for(
            gpu,
            &mut self.color_converters_by_source_format,
            source_format,
        )?
        .prepare_image_to_image_storage(source_texture, rgba_texture_the_kernel_writes)?;
        let output_frame = self
            .node_output_pixel_buffer_pool
            .acquire_the_next_output_pixel_buffer(width, height, PixelFormat::Rgba32)?;
        let output_slot = output_frame.gpu_pool_slot_frame.ok_or_else(|| {
            Error::GpuError(format!(
                "node {}: output pixel buffer {} leases no claim-table row, so no reader could \
                 claim it",
                self.node_run_id_of_the_owning_node, output_frame.published_frame_id
            ))
        })?;

        self.recorder.begin()?;
        if let Err(record_failure) = record_one_conversion(
            &mut self.recorder,
            &pixels_the_kernel_samples,
            &kernel,
            rgba_texture_the_kernel_writes,
            &output_frame.pixel_buffer,
            (width, height),
        ) {
            self.recorder.abort_recording();
            return Err(record_failure);
        }
        self.recorder.submit()?;
        // Between the submit and the wait: once the queue holds the recording the transition
        // belongs to the GPU, and a failed wait must not leave the tracked layout behind it.
        if let ResolvedSurfaceBacking::RegisteredTexture(registration) = &source_backing {
            registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        }
        self.recorder.wait_for_completion()?;
        if let ResolvedSurfaceBacking::RegisteredTexture(registration) = source_backing {
            publish_the_layouts_a_dispatch_left_its_bound_textures_in(
                gpu,
                &[(source_surface_id, registration)],
            );
        }
        Ok(ImageFrameDescriptorBody {
            slot: output_slot,
            width,
            height,
            format: TextureFormat::Rgba8Unorm as u32,
            colour: image.colour,
        })
    }
}

/// The colour converter `kept` holds for `source_format`, else a new one kept there.
fn color_converter_kept_for<'kept>(
    gpu: &GpuContext,
    kept: &'kept mut HashMap<PixelFormat, RhiColorConverter>,
    source_format: TextureFormat,
) -> Result<&'kept RhiColorConverter> {
    let source_pixel_format = match source_format {
        TextureFormat::Bgra8Unorm => PixelFormat::Bgra32,
        _ => PixelFormat::Rgba32,
    };
    Ok(match kept.entry(source_pixel_format) {
        Entry::Occupied(kept_converter) => kept_converter.into_mut(),
        Entry::Vacant(no_converter_yet) => no_converter_yet
            .insert(gpu.create_color_converter(source_pixel_format, PixelFormat::Rgba32)?),
    })
}

/// The format `format_wire_value` names, refused unless the kernel samples it as it is stored.
pub(super) fn texture_format_the_kernel_samples(format_wire_value: u32) -> Result<TextureFormat> {
    let format = TextureFormat::from_repr_value(format_wire_value);
    format
        .filter(|format| TEXTURE_FORMATS_CONVERTED_INTO_RGBA8_UNORM.contains(format))
        .ok_or_else(|| {
            let named_format = format.map_or_else(
                || format!("an unknown texture format ({format_wire_value})"),
                |format| format.wire_name().to_string(),
            );
            Error::PortError(format!(
                "an Image frame of {named_format} is not converted into rgba8_unorm; only \
                 rgba8_unorm and bgra8_unorm are"
            ))
        })
}

/// The texture `kept` holds when it is of this shape, else a new one of it kept there.
fn texture_of_the_shape<'kept>(
    gpu: &GpuContext,
    kept: &'kept mut Option<Texture>,
    width: u32,
    height: u32,
    format: TextureFormat,
    usage: TextureUsages,
) -> Result<&'kept Texture> {
    let of_the_shape = |texture: &Texture| {
        (texture.width(), texture.height(), texture.format()) == (width, height, format)
    };
    if !kept.as_ref().is_some_and(of_the_shape) {
        // Read and written only inside this stage's own submissions, so it is local, never
        // exported.
        *kept = Some(gpu.device().create_texture_local(
            &TextureDescriptor::new(width, height, format).with_usage(usage),
        )?);
    }
    kept.as_ref()
        .ok_or_else(|| Error::GpuError("a texture was made and is gone".into()))
}

/// How many pixels one row of `pixel_buffer` spans, refused unless it holds a `width` by
/// `height` frame of `format`.
fn buffer_row_length_in_pixels_of(
    surface_id: &str,
    pixel_buffer: &PixelBuffer,
    format: TextureFormat,
    width: u32,
    height: u32,
) -> Result<u32> {
    if Some(pixel_buffer.format()) != format.host_view_pixel_format() {
        return Err(Error::GpuError(format!(
            "surface {surface_id} is a {:?} pixel buffer, and its Image frame states {}",
            pixel_buffer.format(),
            format.wire_name()
        )));
    }
    let bytes_per_pixel = u64::from(format.bytes_per_pixel());
    let row_length_in_pixels =
        pixel_buffer.plane_size(0) / (u64::from(height.max(1)) * bytes_per_pixel);
    match u32::try_from(row_length_in_pixels) {
        Ok(row_length_in_pixels) if row_length_in_pixels >= width => Ok(row_length_in_pixels),
        _ => Err(Error::GpuError(format!(
            "surface {surface_id} is a pixel buffer of {} bytes, which holds no {width}x{height} \
             frame of {}",
            pixel_buffer.plane_size(0),
            format.wire_name()
        ))),
    }
}

/// Record the copy of a pixel-buffer source into its texture, the kernel's dispatch, and the
/// copy of what it wrote into `output_pixel_buffer`.
fn record_one_conversion(
    recorder: &mut RhiCommandRecorder,
    pixels_the_kernel_samples: &PixelsTheKernelSamples<'_>,
    kernel: &VulkanComputeKernel,
    rgba_texture_the_kernel_writes: &Texture,
    output_pixel_buffer: &PixelBuffer,
    (width, height): (u32, u32),
) -> Result<()> {
    let (source_texture, source_layout) = match pixels_the_kernel_samples {
        PixelsTheKernelSamples::FramesOwnTexture {
            texture,
            known_layout,
        } => (*texture, *known_layout),
        PixelsTheKernelSamples::PixelBufferCopiedIntoATexture {
            pixel_buffer,
            buffer_row_length_in_pixels,
            texture,
        } => {
            recorder.record_buffer_barrier_before_a_transfer_read(*pixel_buffer)?;
            let resting_layout = recorder.record_image_write_as_transfer_destination(
                texture,
                VulkanLayout::UNDEFINED,
                |recorder| {
                    recorder.record_copy_buffer_to_image(
                        *pixel_buffer,
                        texture,
                        VulkanLayout::TRANSFER_DST_OPTIMAL,
                        ImageCopyRegion {
                            buffer_row_length: *buffer_row_length_in_pixels,
                            ..ImageCopyRegion::tightly_packed(width, height)
                        },
                    )
                },
            )?;
            (*texture, resting_layout)
        }
    };
    recorder.record_image_barrier(
        source_texture,
        source_layout,
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        VulkanStage::ALL_COMMANDS,
        VulkanStage::COMPUTE_SHADER,
        VulkanAccess::MEMORY_WRITE,
        VulkanAccess::SHADER_SAMPLED_READ,
    )?;
    recorder.record_image_barrier(
        rgba_texture_the_kernel_writes,
        VulkanLayout::UNDEFINED,
        VulkanLayout::GENERAL,
        VulkanStage::ALL_COMMANDS,
        VulkanStage::COMPUTE_SHADER,
        VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        VulkanAccess::SHADER_WRITE,
    )?;
    recorder.record_dispatch(
        kernel,
        width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
        height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
        1,
    )?;
    recorder.record_image_read_as_transfer_source(
        rgba_texture_the_kernel_writes,
        VulkanLayout::GENERAL,
        |recorder| {
            recorder.record_copy_image_to_buffer(
                rgba_texture_the_kernel_writes,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                output_pixel_buffer,
                ImageCopyRegion::tightly_packed(width, height),
            )
        },
    )?;
    recorder.record_buffer_barrier_publishing_a_transfer_write(output_pixel_buffer)
}
