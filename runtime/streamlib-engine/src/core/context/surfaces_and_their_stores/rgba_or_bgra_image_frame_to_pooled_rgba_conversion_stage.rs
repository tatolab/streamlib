// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The engine's conversion of an 8-bit RGBA or BGRA Image frame into an `rgba8_unorm` one: the
//! colour converter's image-to-image kernel samples the frame into a scratch texture, which is
//! copied into a pixel buffer from the converting node's own output pool, in one submission.

use streamlib_ipc_types::ImageFrameDescriptorBody;

use crate::core::context::surfaces_and_their_stores::surface_backing_resolution::ResolvedSurfaceBacking;
use crate::core::context::surfaces_and_their_stores::texture_layouts_published_to_the_surface_store::publish_the_layouts_a_dispatch_left_its_bound_textures_in;
use crate::core::context::{
    GpuContext, GpuContextFullAccess, GpuContextLimitedAccess,
    NodeOutputPixelBufferPoolOfOneNativeNode,
};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, Texture, TextureDescriptor, TextureFormat, TextureUsages,
    VulkanLayout, texture_format_the_image_to_image_kernel_samples,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    ImageCopyRegion, ImageToImageColorConvertersWithTheirKernelsBuilt,
    LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN, RhiCommandRecorder,
    TextureTheImageToImageKernelSamples, VulkanAccess, VulkanStage,
    record_the_image_to_image_dispatch,
};

/// The pixels the kernel samples, as one conversion records them.
enum PixelsTheKernelSamples<'pixels> {
    /// The frame's own texture, in the layout its registration holds.
    FramesOwnTexture {
        texture: &'pixels Texture,
        known_layout: VulkanLayout,
    },
    /// The frame's tightly packed pixel buffer, copied first into a texture of the stage's own.
    PixelBufferCopiedIntoATexture {
        pixel_buffer: &'pixels PixelBuffer,
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
    image_to_image_color_converters: ImageToImageColorConvertersWithTheirKernelsBuilt,
    recorder: RhiCommandRecorder,
    texture_a_source_pixel_buffer_is_copied_into: Option<Texture>,
    rgba_texture_the_kernel_writes: Option<Texture>,
    node_output_pixel_buffer_pool: NodeOutputPixelBufferPoolOfOneNativeNode,
}

impl RgbaOrBgraImageFrameToPooledRgbaConversionStage {
    /// The stage of the node `node_run_id_of_the_owning_node`, publishing from an output pool of
    /// that node's own, with the kernel's pipelines built now.
    #[tracing::instrument(skip(full))]
    pub fn create(
        full: &GpuContextFullAccess,
        node_run_id_of_the_owning_node: &str,
    ) -> Result<Self> {
        let gpu = full.host_inner();
        Ok(Self {
            node_run_id_of_the_owning_node: node_run_id_of_the_owning_node.to_string(),
            image_to_image_color_converters:
                ImageToImageColorConvertersWithTheirKernelsBuilt::create(gpu)?,
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
    /// colour. A frame of a format outside
    /// [`TEXTURE_FORMATS_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_AS_STORED`](crate::core::rhi::TEXTURE_FORMATS_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_AS_STORED)
    /// is refused naming it. A scratch texture of a new shape is allocated through `gpu`'s
    /// escalate.
    #[tracing::instrument(
        skip(self, gpu),
        fields(
            node_run_id = %self.node_run_id_of_the_owning_node,
            width = image.width,
            height = image.height,
            format = image.format
        )
    )]
    pub fn convert_into_the_next_pooled_pixel_buffer(
        &mut self,
        gpu: &GpuContextLimitedAccess,
        image: &ImageFrameDescriptorBody,
    ) -> Result<ImageFrameDescriptorBody> {
        let allocate_a_scratch_texture = |descriptor: &TextureDescriptor<'_>| {
            gpu.escalate(|full| full.host_inner().device().create_texture_local(descriptor))
        };
        self.convert(gpu.host_inner(), image, &allocate_a_scratch_texture)
    }

    fn convert(
        &mut self,
        gpu: &GpuContext,
        image: &ImageFrameDescriptorBody,
        allocate_a_scratch_texture: &dyn Fn(&TextureDescriptor<'_>) -> Result<Texture>,
    ) -> Result<ImageFrameDescriptorBody> {
        let (width, height) = (image.width, image.height);
        if width == 0 || height == 0 {
            return Err(Error::TextureError(format!(
                "an Image frame of {width}x{height} has no pixels to convert"
            )));
        }
        let source_format = texture_format_the_image_to_image_kernel_samples(image.format)?;
        let source_surface_id = gpu.published_frame_id_of_the_gpu_pool_slot_frame(&image.slot)?;
        gpu.refuse_a_retired_frame_id(&source_surface_id)?;
        let _claim_until_the_conversion_has_run =
            gpu.claim_surface_against_producer_reuse(&source_surface_id)?;
        let source_backing = gpu.resolve_device_export_source(&source_surface_id)?;
        match &source_backing {
            ResolvedSurfaceBacking::RegisteredTexture(registration) => {
                refuse_a_texture_the_kernel_cannot_sample_as_the_frame(
                    &source_surface_id,
                    registration.texture(),
                    registration.current_layout(),
                    source_format,
                    (width, height),
                )?
            }
            ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) => {
                refuse_a_pixel_buffer_that_holds_no_tightly_packed_frame(
                    &source_surface_id,
                    pixel_buffer,
                    source_format,
                    (width, height),
                )?
            }
        }

        let rgba_texture_the_kernel_writes = texture_of_the_shape(
            &mut self.rgba_texture_the_kernel_writes,
            TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC),
            allocate_a_scratch_texture,
        )?;
        let pixels_the_kernel_samples = match &source_backing {
            // The registration, not the frame's `image_layout`, holds the layout its texture is
            // in: every engine transition of the texture updates it.
            ResolvedSurfaceBacking::RegisteredTexture(registration) => {
                PixelsTheKernelSamples::FramesOwnTexture {
                    texture: registration.texture(),
                    known_layout: registration.current_layout(),
                }
            }
            ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) => {
                PixelsTheKernelSamples::PixelBufferCopiedIntoATexture {
                    pixel_buffer,
                    texture: texture_of_the_shape(
                        &mut self.texture_a_source_pixel_buffer_is_copied_into,
                        TextureDescriptor::new(width, height, source_format)
                            .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
                        allocate_a_scratch_texture,
                    )?,
                }
            }
        };
        let source_texture = match &pixels_the_kernel_samples {
            PixelsTheKernelSamples::FramesOwnTexture { texture, .. }
            | PixelsTheKernelSamples::PixelBufferCopiedIntoATexture { texture, .. } => *texture,
        };
        let kernel = self
            .image_to_image_color_converters
            .prepare_dispatch(source_texture, rgba_texture_the_kernel_writes)?;
        let output_frame = self
            .node_output_pixel_buffer_pool
            .acquire_the_next_output_pixel_buffer(width, height, PixelFormat::Rgba32)?;
        let output_slot = output_frame.gpu_pool_slot_frame.ok_or_else(|| {
            Error::BufferError(format!(
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
        // belongs to the GPU, and a failed wait must not leave the tracked or the published
        // layout behind it.
        if let ResolvedSurfaceBacking::RegisteredTexture(registration) = source_backing {
            registration.update_layout(LAYOUT_THE_IMAGE_TO_IMAGE_KERNEL_SAMPLES_IN);
            publish_the_layouts_a_dispatch_left_its_bound_textures_in(
                gpu,
                &[(source_surface_id, registration)],
            );
        }
        self.recorder.wait_for_completion()?;
        Ok(ImageFrameDescriptorBody {
            slot: output_slot,
            width,
            height,
            format: TextureFormat::Rgba8Unorm as u32,
            colour: image.colour,
        })
    }
}

/// Refuse a frame's texture the kernel cannot sample as the frame its Image frame states: never
/// written, of another format or extent, or allocated without sampled usage.
fn refuse_a_texture_the_kernel_cannot_sample_as_the_frame(
    surface_id: &str,
    texture: &Texture,
    known_layout: VulkanLayout,
    format_the_frame_states: TextureFormat,
    (width, height): (u32, u32),
) -> Result<()> {
    if known_layout == VulkanLayout::UNDEFINED {
        return Err(Error::TextureError(format!(
            "surface {surface_id} has never been written, so it holds no pixels to convert"
        )));
    }
    if texture.format() != format_the_frame_states {
        return Err(Error::TextureError(format!(
            "surface {surface_id} is a {} texture, and its Image frame states {}",
            texture.format().wire_name(),
            format_the_frame_states.wire_name()
        )));
    }
    if (texture.width(), texture.height()) != (width, height) {
        return Err(Error::TextureError(format!(
            "surface {surface_id} is a {}x{} texture, and its Image frame states {width}x{height}",
            texture.width(),
            texture.height()
        )));
    }
    if !texture.supports_sampled_read() {
        return Err(Error::TextureError(format!(
            "surface {surface_id}'s texture was allocated without \"texture_binding\" usage, so \
             no kernel may sample it"
        )));
    }
    Ok(())
}

/// Refuse a frame's pixel buffer that does not hold its `width` by `height` frame of `format`
/// tightly packed: the copy into a texture reads its rows tightly packed, and would shear any
/// other.
fn refuse_a_pixel_buffer_that_holds_no_tightly_packed_frame(
    surface_id: &str,
    pixel_buffer: &PixelBuffer,
    format: TextureFormat,
    (width, height): (u32, u32),
) -> Result<()> {
    if Some(pixel_buffer.format()) != format.host_view_pixel_format() {
        return Err(Error::BufferError(format!(
            "surface {surface_id} is a {:?} pixel buffer, and its Image frame states {}",
            pixel_buffer.format(),
            format.wire_name()
        )));
    }
    let tightly_packed_byte_size =
        u64::from(width) * u64::from(height) * u64::from(format.bytes_per_pixel());
    if pixel_buffer.plane_size(0) != tightly_packed_byte_size {
        return Err(Error::BufferError(format!(
            "surface {surface_id} is a pixel buffer of {} bytes, not the \
             {tightly_packed_byte_size} a tightly packed {width}x{height} frame of {} fills",
            pixel_buffer.plane_size(0),
            format.wire_name()
        )));
    }
    Ok(())
}

/// The texture `kept` holds when it is of `descriptor`'s shape, else one `allocate` makes of it,
/// kept there.
fn texture_of_the_shape<'kept>(
    kept: &'kept mut Option<Texture>,
    descriptor: TextureDescriptor<'_>,
    allocate: &dyn Fn(&TextureDescriptor<'_>) -> Result<Texture>,
) -> Result<&'kept Texture> {
    let of_the_shape = |texture: &Texture| {
        (texture.width(), texture.height(), texture.format())
            == (descriptor.width, descriptor.height, descriptor.format)
    };
    let texture = match kept.take() {
        Some(kept_texture) if of_the_shape(&kept_texture) => kept_texture,
        // Read and written only inside this stage's own submissions, so it is local, never
        // exported.
        _ => allocate(&descriptor)?,
    };
    Ok(kept.insert(texture))
}

/// Record the copy of a pixel-buffer source into its texture, the kernel's dispatch, and the
/// copy of what it wrote into `output_pixel_buffer`.
fn record_one_conversion(
    recorder: &mut RhiCommandRecorder,
    pixels_the_kernel_samples: &PixelsTheKernelSamples<'_>,
    kernel: &crate::vulkan::rhi::VulkanComputeKernel,
    rgba_texture_the_kernel_writes: &Texture,
    output_pixel_buffer: &PixelBuffer,
    (width, height): (u32, u32),
) -> Result<()> {
    let texture_the_kernel_samples = match pixels_the_kernel_samples {
        PixelsTheKernelSamples::FramesOwnTexture {
            texture,
            known_layout,
        } => TextureTheImageToImageKernelSamples {
            texture,
            layout: *known_layout,
            last_written_in_stage: VulkanStage::ALL_COMMANDS,
            last_written_with_access: VulkanAccess::MEMORY_WRITE,
        },
        PixelsTheKernelSamples::PixelBufferCopiedIntoATexture {
            pixel_buffer,
            texture,
        } => {
            recorder.record_buffer_barrier_before_a_transfer_read(*pixel_buffer)?;
            recorder.record_image_barrier(
                texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::MEMORY_READ,
                VulkanAccess::TRANSFER_WRITE,
            )?;
            recorder.record_copy_buffer_to_image(
                *pixel_buffer,
                texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                ImageCopyRegion::tightly_packed(width, height),
            )?;
            TextureTheImageToImageKernelSamples {
                texture,
                layout: VulkanLayout::TRANSFER_DST_OPTIMAL,
                last_written_in_stage: VulkanStage::ALL_TRANSFER,
                last_written_with_access: VulkanAccess::TRANSFER_WRITE,
            }
        }
    };
    record_the_image_to_image_dispatch(
        recorder,
        kernel,
        &texture_the_kernel_samples,
        rgba_texture_the_kernel_writes,
        VulkanLayout::UNDEFINED,
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
