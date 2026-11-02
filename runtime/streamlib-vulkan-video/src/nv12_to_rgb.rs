// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GPU NV12 → RGB(A) color-space converter using a Vulkan compute shader.
//!
//! The reverse of [`crate::rgb_to_nv12::RgbToNv12Converter`].
//!
//! Uses a `VkSamplerYcbcrConversion` combined image sampler baked into the
//! descriptor-set layout via `pImmutableSamplers` so the hardware handles
//! plane separation, chroma upsampling (4:2:0 → 4:4:4), and YCbCr→RGB
//! conversion automatically, with the matrix and range the decoded picture
//! signals.
//!
//! The converter creates an RGBA output image with `STORAGE` usage for compute
//! writes and `TRANSFER_SRC` for CPU readback, using `CONCURRENT` sharing
//! between the compute and decode queue families.
//!
//! The converter owns its compute pipeline, the YCbCr conversion and the
//! immutable sampler baked into its descriptor-set layout, the per-call NV12
//! sampled view, and its own command buffer and fence.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use crate::VideoError;
use crate::VulkanVideoDecodeContext;
use streamlib_consumer_rhi::h273_color_vui::H273ColorVui;
use streamlib_consumer_rhi::h273_color_vui::matrix;

/// Pre-compiled SPIR-V bytecode for the NV12→RGB compute shader.
const SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nv12_to_rgb.spv"));

/// Binding shape of `nv12_to_rgb.comp`:
/// - 0: COMBINED_IMAGE_SAMPLER — NV12 input via an immutable YCbCr sampler
///   chained at descriptor-set-layout creation time.
/// - 1: STORAGE_IMAGE — RGBA output (R8G8B8A8_UNORM).
const NV12_INPUT_BINDING: u32 = 0;
const RGBA_OUTPUT_BINDING: u32 = 1;

/// Horizontal siting the hardware reconstructs 4:2:0 chroma at.
///
/// Bitstream contract: H.264 §E.2.1 / H.265 §E.3.1 specify that an absent
/// `chroma_loc_info_present_flag` is inferred as `chroma_sample_loc_type` 0 —
/// chroma co-sited with the even luma column horizontally, midpoint vertically.
/// Type 0 is also the MPEG-2/H.264/H.265 default a third-party stream carries when
/// it signals nothing. `rgb_to_nv12.comp` downsamples to the same siting, and the
/// tree's own encoder never sets the flag, so both our streams and the common
/// third-party case are type 0.
const RECONSTRUCTED_X_CHROMA_OFFSET: vk::ChromaLocation = vk::ChromaLocation::COSITED_EVEN;

/// Vertical siting the hardware reconstructs 4:2:0 chroma at — `chroma_sample_loc_type`
/// 0 is midpoint on this axis, which is what `rgb_to_nv12.comp` downsamples to.
const RECONSTRUCTED_Y_CHROMA_OFFSET: vk::ChromaLocation = vk::ChromaLocation::MIDPOINT;

/// The sampler YCbCr model and range a decoded picture's matrix and range select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nv12SamplerYcbcrModelAndRange {
    /// The YCbCr → RGB matrix the sampler applies.
    pub ycbcr_model: vk::SamplerYcbcrModelConversion,
    /// The quantization range the sampler expands from.
    pub ycbcr_range: vk::SamplerYcbcrRange,
}

impl Nv12SamplerYcbcrModelAndRange {
    /// The model and range that convert a picture in `decoded_picture_color_vui`,
    /// refusing a matrix no sampler YCbCr model represents.
    pub fn for_decoded_picture_color_vui(
        decoded_picture_color_vui: &H273ColorVui,
    ) -> Result<Self, VideoError> {
        let ycbcr_model = match decoded_picture_color_vui.matrix_byte() {
            matrix::BT709 => vk::SamplerYcbcrModelConversion::YCBCR_709,
            matrix::BT470_BG | matrix::SMPTE170M => vk::SamplerYcbcrModelConversion::YCBCR_601,
            matrix::BT2020_NCL => vk::SamplerYcbcrModelConversion::YCBCR_2020,
            matrix_coefficients => {
                return Err(VideoError::MatrixCoefficientsNotConvertible(
                    matrix_coefficients,
                ));
            }
        };
        let ycbcr_range = if decoded_picture_color_vui.full_range == Some(true) {
            vk::SamplerYcbcrRange::ITU_FULL
        } else {
            vk::SamplerYcbcrRange::ITU_NARROW
        };
        Ok(Self {
            ycbcr_model,
            ycbcr_range,
        })
    }
}

/// Push constants passed to the compute shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    resolution: [i32; 2],
}

/// The `nv12_to_rgb.comp` pipeline and the one descriptor set its dispatch
/// binds, rewritten before each dispatch: the converter waits on every
/// dispatch's fence before the next, so the set is never in flight twice.
struct Nv12ToRgbComputePipeline {
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
}

impl Nv12ToRgbComputePipeline {
    /// Build the pipeline with `ycbcr_sampler` baked into binding 0's layout.
    unsafe fn new(
        device: &vulkanalia::Device,
        ycbcr_sampler: vk::Sampler,
    ) -> Result<Self, VideoError> {
        unsafe {
            let immutable_samplers = [ycbcr_sampler];
            let layout_bindings = [
                vk::DescriptorSetLayoutBinding::builder()
                    .binding(NV12_INPUT_BINDING)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
                    .immutable_samplers(&immutable_samplers)
                    .build(),
                vk::DescriptorSetLayoutBinding::builder()
                    .binding(RGBA_OUTPUT_BINDING)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
                    .build(),
            ];
            let descriptor_set_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::builder().bindings(&layout_bindings),
                None,
            )?;
            let push_constant_ranges = [vk::PushConstantRange::builder()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .offset(0)
                .size(std::mem::size_of::<PushConstants>() as u32)
                .build()];
            let set_layouts = [descriptor_set_layout];
            let pipeline_layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::builder()
                    .set_layouts(&set_layouts)
                    .push_constant_ranges(&push_constant_ranges),
                None,
            )?;
            let spirv_words: Vec<u32> = SHADER_SPIRV
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
                .collect();
            let shader_module = device.create_shader_module(
                &vk::ShaderModuleCreateInfo::builder()
                    .code_size(SHADER_SPIRV.len())
                    .code(&spirv_words),
                None,
            )?;
            let stage = vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(shader_module)
                .name(b"main\0");
            let pipeline_info = vk::ComputePipelineCreateInfo::builder()
                .stage(stage)
                .layout(pipeline_layout);
            let pipeline_result =
                device.create_compute_pipelines(vk::PipelineCache::null(), &[pipeline_info], None);
            device.destroy_shader_module(shader_module, None);
            let pipeline = pipeline_result?.0[0];
            let pool_sizes = [
                vk::DescriptorPoolSize::builder()
                    .type_(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .build(),
                vk::DescriptorPoolSize::builder()
                    .type_(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .build(),
            ];
            let descriptor_pool = device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::builder()
                    .max_sets(1)
                    .pool_sizes(&pool_sizes),
                None,
            )?;
            let descriptor_set = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::builder()
                    .descriptor_pool(descriptor_pool)
                    .set_layouts(&set_layouts),
            )?[0];
            Ok(Self {
                descriptor_set_layout,
                pipeline_layout,
                pipeline,
                descriptor_pool,
                descriptor_set,
            })
        }
    }

    /// Point the set at this dispatch's views, then record the bind, push
    /// constants and dispatch into `command_buffer`.
    unsafe fn record(
        &self,
        device: &vulkanalia::Device,
        command_buffer: vk::CommandBuffer,
        nv12_sampled_view: vk::ImageView,
        rgba_storage_view: vk::ImageView,
        push_constants: PushConstants,
        group_count_x: u32,
        group_count_y: u32,
    ) {
        unsafe {
            let nv12_image_info = [vk::DescriptorImageInfo::builder()
                .image_view(nv12_sampled_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .build()];
            let rgba_image_info = [vk::DescriptorImageInfo::builder()
                .image_view(rgba_storage_view)
                .image_layout(vk::ImageLayout::GENERAL)
                .build()];
            let writes = [
                vk::WriteDescriptorSet::builder()
                    .dst_set(self.descriptor_set)
                    .dst_binding(NV12_INPUT_BINDING)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&nv12_image_info)
                    .build(),
                vk::WriteDescriptorSet::builder()
                    .dst_set(self.descriptor_set)
                    .dst_binding(RGBA_OUTPUT_BINDING)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(&rgba_image_info)
                    .build(),
            ];
            device.update_descriptor_sets(&writes, &[] as &[vk::CopyDescriptorSet]);
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );
            let push_constant_bytes = std::slice::from_raw_parts(
                (&push_constants as *const PushConstants).cast::<u8>(),
                std::mem::size_of::<PushConstants>(),
            );
            device.cmd_push_constants(
                command_buffer,
                self.pipeline_layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                push_constant_bytes,
            );
            device.cmd_dispatch(command_buffer, group_count_x, group_count_y, 1);
        }
    }

    unsafe fn destroy(&self, device: &vulkanalia::Device) {
        unsafe {
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        }
    }
}

/// GPU NV12 → RGBA converter.
///
/// Owns a compute kernel with an immutable YCbCr sampler, an RGBA output
/// image, and a command buffer for recording conversion dispatches. The
/// output image uses `CONCURRENT` sharing between compute and decode queue
/// families so the NV12 DPB image can be sampled directly without queue
/// family ownership transfers.
pub struct Nv12ToRgbConverter {
    device: vulkanalia::Device,
    allocator: Arc<vma::Allocator>,

    // YCbCr conversion objects — `sampler` is baked into the kernel's
    // descriptor-set layout via `pImmutableSamplers` and must outlive the
    // kernel.
    ycbcr_model_and_range: Nv12SamplerYcbcrModelAndRange,
    ycbcr_conversion: vk::SamplerYcbcrConversion,
    sampler: vk::Sampler,

    compute_pipeline: Nv12ToRgbComputePipeline,

    // RGBA output image
    rgba_image: vk::Image,
    rgba_allocation: vma::Allocation,
    rgba_view: vk::ImageView,

    // Command recording (separate from the kernel's own internal command
    // buffer so the converter can wrap `kernel.record(cb, ...)` with its
    // own barriers).
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,

    // Queue
    compute_queue: vk::Queue,
    _compute_queue_family: u32,

    // Host-side queue submission gateway.
    codec_device: Arc<dyn crate::VulkanVideoCodecDevice>,

    // Dimensions
    width: u32,
    height: u32,
}

impl Nv12ToRgbConverter {
    /// Create a new NV12→RGB converter.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Shared Vulkan device context (provides device + VMA allocator)
    /// * `width` - Image width in pixels
    /// * `height` - Image height in pixels
    /// * `compute_queue_family` - Queue family index with COMPUTE support
    /// * `compute_queue` - Queue from the compute family
    /// * `decode_queue_family` - Queue family index for video decode (for CONCURRENT sharing)
    /// * `ycbcr_model_and_range` - The conversion baked into the immutable sampler
    pub unsafe fn new(
        ctx: &Arc<VulkanVideoDecodeContext>,
        width: u32,
        height: u32,
        compute_queue_family: u32,
        compute_queue: vk::Queue,
        decode_queue_family: u32,
        ycbcr_model_and_range: Nv12SamplerYcbcrModelAndRange,
    ) -> Result<Self, VideoError> {
        unsafe {
            let device = ctx.device().clone();
            let allocator = ctx.allocator().clone();
            let codec_device = ctx.codec_device().clone();

            // --- 1. YCbCr conversion ---
            // VUID-VkSamplerYcbcrConversionCreateInfo-xChromaOffset-01651 and -01652: a
            // chroma location is legal only when the format advertises its matching
            // feature bit. This conversion sites the two axes differently, so it needs
            // both COSITED_CHROMA_SAMPLES and MIDPOINT_CHROMA_SAMPLES, and each is a
            // per-device format property rather than something the format guarantees.
            // Refuse by name rather than create a conversion the spec forbids.
            let nv12_format_features = ctx
                .instance()
                .get_physical_device_format_properties(
                    ctx.physical_device(),
                    vk::Format::G8_B8R8_2PLANE_420_UNORM,
                )
                .optimal_tiling_features;
            let required_chroma_feature = |location: vk::ChromaLocation| match location {
                vk::ChromaLocation::COSITED_EVEN => vk::FormatFeatureFlags::COSITED_CHROMA_SAMPLES,
                _ => vk::FormatFeatureFlags::MIDPOINT_CHROMA_SAMPLES,
            };
            for offset in [RECONSTRUCTED_X_CHROMA_OFFSET, RECONSTRUCTED_Y_CHROMA_OFFSET] {
                let feature = required_chroma_feature(offset);
                if !nv12_format_features.contains(feature) {
                    tracing::error!(
                        "G8_B8R8_2PLANE_420_UNORM does not advertise {feature:?}, so chroma \
                         cannot be reconstructed at {offset:?} — the decoded picture would be \
                         sited against its own bitstream"
                    );
                    return Err(VideoError::FormatNotSupported(
                        vk::Format::G8_B8R8_2PLANE_420_UNORM,
                    ));
                }
            }

            let ycbcr_conversion = device.create_sampler_ycbcr_conversion(
                &vk::SamplerYcbcrConversionCreateInfo::builder()
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .ycbcr_model(ycbcr_model_and_range.ycbcr_model)
                    .ycbcr_range(ycbcr_model_and_range.ycbcr_range)
                    .components(vk::ComponentMapping {
                        r: vk::ComponentSwizzle::IDENTITY,
                        g: vk::ComponentSwizzle::IDENTITY,
                        b: vk::ComponentSwizzle::IDENTITY,
                        a: vk::ComponentSwizzle::IDENTITY,
                    })
                    .x_chroma_offset(RECONSTRUCTED_X_CHROMA_OFFSET)
                    .y_chroma_offset(RECONSTRUCTED_Y_CHROMA_OFFSET)
                    .chroma_filter(vk::Filter::LINEAR)
                    .force_explicit_reconstruction(false),
                None,
            )?;

            // --- 2. Sampler with YCbCr conversion ---
            let mut ycbcr_info =
                vk::SamplerYcbcrConversionInfo::builder().conversion(ycbcr_conversion);

            let sampler = device.create_sampler(
                &vk::SamplerCreateInfo::builder()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .push_next(&mut ycbcr_info),
                None,
            )?;

            // --- 3. Compute pipeline, with the YCbCr sampler baked into
            // its descriptor-set layout as an immutable sampler. `Drop`
            // destroys the pipeline before the sampler.
            let compute_pipeline = Nv12ToRgbComputePipeline::new(&device, sampler)?;

            // --- 4. RGBA output image ---
            let queue_families = [compute_queue_family, decode_queue_family];
            let concurrent = compute_queue_family != decode_queue_family;

            let mut rgba_image_info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(
                    vk::ImageUsageFlags::STORAGE
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::SAMPLED,
                )
                .initial_layout(vk::ImageLayout::UNDEFINED);

            if concurrent {
                rgba_image_info = rgba_image_info
                    .sharing_mode(vk::SharingMode::CONCURRENT)
                    .queue_family_indices(&queue_families);
            } else {
                rgba_image_info = rgba_image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
            }

            let alloc_options = vma::AllocationOptions {
                required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                ..Default::default()
            };
            let (rgba_image, rgba_allocation) =
                allocator.create_image(rgba_image_info, &alloc_options)?;

            // --- 5. RGBA image view ---
            let rgba_view = device.create_image_view(
                &vk::ImageViewCreateInfo::builder()
                    .image(rgba_image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }),
                None,
            )?;

            // --- 6. Command pool / buffer / fence ---
            let command_pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(compute_queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;

            let command_buffer = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];

            let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;

            Ok(Self {
                device,
                allocator,
                ycbcr_model_and_range,
                ycbcr_conversion,
                sampler,
                compute_pipeline,
                rgba_image,
                rgba_allocation,
                rgba_view,
                command_pool,
                command_buffer,
                fence,
                compute_queue,
                _compute_queue_family: compute_queue_family,
                codec_device,
                width,
                height,
            })
        }
    }

    /// Convert an NV12 DPB image layer to RGBA.
    ///
    /// Creates a temporary sampled view with YCbCr conversion for the
    /// specified array layer, dispatches the compute shader, and leaves
    /// the RGBA output image in `TRANSFER_SRC_OPTIMAL` layout for readback.
    ///
    /// The input NV12 image must be in a layout readable by the compute
    /// shader (e.g. `GENERAL` or `VIDEO_DECODE_DPB_KHR` — NVIDIA drivers
    /// accept DPB layout for sampled reads).
    ///
    /// Returns the RGBA image and view.
    pub unsafe fn convert(
        &mut self,
        nv12_image: vk::Image,
        array_layer: u32,
        src_layout: vk::ImageLayout,
    ) -> Result<(vk::Image, vk::ImageView), VideoError> {
        unsafe {
            // Create a sampled view for this layer with YCbCr conversion info.
            // The YCbCr conversion chained on the view must match the immutable
            // sampler's conversion exactly (VUID-VkImageView/VkSampler-format-..).
            let mut ycbcr_info =
                vk::SamplerYcbcrConversionInfo::builder().conversion(self.ycbcr_conversion);

            let nv12_sampled_view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::builder()
                    .image(nv12_image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: array_layer,
                        layer_count: 1,
                    })
                    .push_next(&mut ycbcr_info),
                None,
            )?;

            let cb = self.command_buffer;

            self.device
                .reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;

            // --- Barrier: NV12 source → SHADER_READ_ONLY_OPTIMAL ---
            let barrier_nv12 = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                .old_layout(src_layout)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(nv12_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: array_layer,
                    layer_count: 1,
                });

            // --- Barrier: RGBA output UNDEFINED → GENERAL (for compute writes) ---
            let barrier_rgba = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.rgba_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            let pre_barriers = [barrier_nv12, barrier_rgba];
            let pre_dep = vk::DependencyInfo::builder().image_memory_barriers(&pre_barriers);
            self.device.cmd_pipeline_barrier2(cb, &pre_dep);

            // --- Bind compute pipeline + push descriptors + push constants + dispatch ---
            // Each thread handles one pixel, workgroup is 8x8.
            let group_x = (self.width + 7) / 8;
            let group_y = (self.height + 7) / 8;
            self.compute_pipeline.record(
                &self.device,
                cb,
                nv12_sampled_view,
                self.rgba_view,
                PushConstants {
                    resolution: [self.width as i32, self.height as i32],
                },
                group_x,
                group_y,
            );

            // --- Barrier: RGBA GENERAL → TRANSFER_SRC_OPTIMAL (for readback) ---
            let barrier_to_transfer = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.rgba_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            let post_barriers = [barrier_to_transfer];
            let post_dep = vk::DependencyInfo::builder().image_memory_barriers(&post_barriers);
            self.device.cmd_pipeline_barrier2(cb, &post_dep);

            self.device.end_command_buffer(cb)?;

            // --- Submit and wait ---
            let cb_submit = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(cb)
                .build();
            let cb_submits = [cb_submit];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cb_submits)
                .build();

            self.device.reset_fences(&[self.fence])?;
            self.codec_device
                .submit_to_queue(self.compute_queue, &[submit], self.fence)
                .map_err(VideoError::from)?;
            self.device.wait_for_fences(&[self.fence], true, u64::MAX)?;

            // Destroy temporary view
            self.device.destroy_image_view(nv12_sampled_view, None);

            Ok((self.rgba_image, self.rgba_view))
        }
    }

    /// Returns the RGBA output image handle.
    pub fn rgba_image(&self) -> vk::Image {
        self.rgba_image
    }

    /// Returns the RGBA image view.
    pub fn rgba_view(&self) -> vk::ImageView {
        self.rgba_view
    }

    /// Returns the output dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The conversion this converter's immutable sampler applies.
    pub fn ycbcr_model_and_range(&self) -> Nv12SamplerYcbcrModelAndRange {
        self.ycbcr_model_and_range
    }
}

impl Drop for Nv12ToRgbConverter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.codec_device.wait_idle();

            if self.fence != vk::Fence::null() {
                self.device.destroy_fence(self.fence, None);
            }
            if self.command_pool != vk::CommandPool::null() {
                self.device.destroy_command_pool(self.command_pool, None);
            }

            if self.rgba_view != vk::ImageView::null() {
                self.device.destroy_image_view(self.rgba_view, None);
            }
            if self.rgba_image != vk::Image::null() {
                self.allocator
                    .destroy_image(self.rgba_image, self.rgba_allocation);
            }
            self.compute_pipeline.destroy(&self.device);
            if self.sampler != vk::Sampler::null() {
                self.device.destroy_sampler(self.sampler, None);
            }
            if self.ycbcr_conversion != vk::SamplerYcbcrConversion::null() {
                self.device
                    .destroy_sampler_ycbcr_conversion(self.ycbcr_conversion, None);
            }
        }
    }
}

// SAFETY: Vulkan handles are only accessed through &mut self methods.
unsafe impl Send for Nv12ToRgbConverter {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_constants_size() {
        assert_eq!(std::mem::size_of::<PushConstants>(), 8);
    }

    #[test]
    fn test_spirv_embedded() {
        assert!(
            !SHADER_SPIRV.is_empty(),
            "SPIR-V bytecode must not be empty"
        );
        assert!(
            SHADER_SPIRV.len() % 4 == 0,
            "SPIR-V size must be multiple of 4"
        );
        let magic = u32::from_le_bytes([
            SHADER_SPIRV[0],
            SHADER_SPIRV[1],
            SHADER_SPIRV[2],
            SHADER_SPIRV[3],
        ]);
        assert_eq!(magic, 0x07230203, "Invalid SPIR-V magic number");
    }

    #[test]
    fn chroma_is_reconstructed_at_the_siting_the_encoders_bitstream_implies() {
        use crate::encode::SPS_VUI_CHROMA_SAMPLE_LOC_TYPE;

        // Anchored on the value the SPS builder writes. `EncoderConfig` carries a
        // `chroma_sample_loc_type` too, but nothing on the bitstream path reads it,
        // so asserting against that field would lock nothing.
        //
        // Figure E-1: type 0 sites chroma on the even luma column horizontally and at
        // the midpoint vertically; type 1 is midpoint on both axes.
        assert_eq!(
            SPS_VUI_CHROMA_SAMPLE_LOC_TYPE, 0,
            "the SPS VUI carries type 0, and an absent present flag has decoders infer \
             the same; reconstructing anywhere else sites chroma against our own stream"
        );
        assert_eq!(
            RECONSTRUCTED_X_CHROMA_OFFSET,
            vk::ChromaLocation::COSITED_EVEN,
            "type 0 is left-sited horizontally"
        );
        assert_eq!(
            RECONSTRUCTED_Y_CHROMA_OFFSET,
            vk::ChromaLocation::MIDPOINT,
            "type 0 is midpoint vertically"
        );
    }

    fn color_vui_with_matrix_and_range(matrix_coefficients: u8, full_range: bool) -> H273ColorVui {
        H273ColorVui {
            matrix: Some(matrix_coefficients),
            full_range: Some(full_range),
            ..Default::default()
        }
    }

    #[test]
    fn each_convertible_matrix_selects_its_sampler_model() {
        for (matrix_coefficients, ycbcr_model) in [
            (matrix::BT709, vk::SamplerYcbcrModelConversion::YCBCR_709),
            (matrix::BT470_BG, vk::SamplerYcbcrModelConversion::YCBCR_601),
            (
                matrix::SMPTE170M,
                vk::SamplerYcbcrModelConversion::YCBCR_601,
            ),
            (
                matrix::BT2020_NCL,
                vk::SamplerYcbcrModelConversion::YCBCR_2020,
            ),
        ] {
            let selected = Nv12SamplerYcbcrModelAndRange::for_decoded_picture_color_vui(
                &color_vui_with_matrix_and_range(matrix_coefficients, false),
            )
            .expect("a convertible matrix");
            assert_eq!(
                selected.ycbcr_model, ycbcr_model,
                "matrix {matrix_coefficients}"
            );
        }
    }

    #[test]
    fn the_signalled_range_selects_the_sampler_range() {
        let full_range = Nv12SamplerYcbcrModelAndRange::for_decoded_picture_color_vui(
            &color_vui_with_matrix_and_range(matrix::BT709, true),
        )
        .expect("BT.709 converts");
        let limited_range = Nv12SamplerYcbcrModelAndRange::for_decoded_picture_color_vui(
            &color_vui_with_matrix_and_range(matrix::BT709, false),
        )
        .expect("BT.709 converts");
        assert_eq!(full_range.ycbcr_range, vk::SamplerYcbcrRange::ITU_FULL);
        assert_eq!(limited_range.ycbcr_range, vk::SamplerYcbcrRange::ITU_NARROW);
    }

    #[test]
    fn a_matrix_no_sampler_model_represents_is_refused_by_its_value() {
        for matrix_coefficients in [
            matrix::IDENTITY,
            matrix::UNSPECIFIED,
            matrix::FCC,
            matrix::SMPTE240M,
            matrix::YCGCO,
            matrix::BT2020_CL,
            matrix::SMPTE2085,
            matrix::CHROMA_NCL,
            matrix::CHROMA_CL,
            matrix::ICTCP,
            200,
        ] {
            let refusal = Nv12SamplerYcbcrModelAndRange::for_decoded_picture_color_vui(
                &color_vui_with_matrix_and_range(matrix_coefficients, false),
            )
            .expect_err("no sampler model represents this matrix");
            assert!(
                matches!(
                    refusal,
                    VideoError::MatrixCoefficientsNotConvertible(refused) if refused == matrix_coefficients
                ),
                "matrix {matrix_coefficients} refused as {refusal}"
            );
        }
    }
}
