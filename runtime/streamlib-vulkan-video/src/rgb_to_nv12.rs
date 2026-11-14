// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! GPU RGB(A) → NV12 color-space converter using a Vulkan compute shader.
//!
//! Based on pyroenc's `rgb_to_yuv.comp` shader (MIT, Arntzen Software AS).
//! Uses BT.709 color matrix with TV range (16-235 luma, 16-240 chroma),
//! 6-tap left-sited chroma filter.
//!
//! Two NV12 images are owned: a STORAGE compute-output image (no video
//! profile chained) and a VIDEO_ENCODE_SRC_KHR image (profile chained). A
//! per-plane vkCmdCopyImage moves data from the first to the second every
//! frame. The split is required because no NVIDIA encode profile reports
//! `STORAGE` as a supported usage via vkGetPhysicalDeviceVideoFormatPropertiesKHR
//! (VUID-VkImageCreateInfo-pNext-06811).
//!
//! The converter owns its compute pipeline, the per-plane
//! reinterpreted-format storage views the shader writes through, and its own
//! command buffer / fence for recording the surrounding barriers + copy.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use streamlib_consumer_rhi::SampledImageReadLayout;

use crate::{VideoError, VulkanVideoCodecDevice, VulkanVideoEncodeContext};

/// An RGBA source published in a layout it is not sampled in, which the
/// converter barriers out of before its dispatch reads it.
#[derive(Clone, Copy, Debug)]
pub struct RgbaSourceImageBarrierIntoItsSampledLayout {
    /// The source image the sampled view is a view of.
    pub rgba_image: vk::Image,
    /// The layout the source was published in.
    pub published_layout: vk::ImageLayout,
}

const SINGLE_MIP_SINGLE_LAYER_COLOR_SUBRESOURCE_RANGE: vk::ImageSubresourceRange =
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };

/// Pre-compiled SPIR-V bytecode for the RGB→NV12 compute shader.
const SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rgb_to_nv12.spv"));

/// Binding shape of `rgb_to_nv12.comp`:
/// - 0: SAMPLED_IMAGE — input RGBA texture (texture2D, texelFetch).
/// - 1: STORAGE_IMAGE — luma output plane (R8, reinterpreted view of NV12 plane 0).
/// - 2: STORAGE_IMAGE — chroma output plane (RG8, reinterpreted view of NV12 plane 1).
const BINDINGS: [(u32, vk::DescriptorType); 3] = [
    (0, vk::DescriptorType::SAMPLED_IMAGE),
    (1, vk::DescriptorType::STORAGE_IMAGE),
    (2, vk::DescriptorType::STORAGE_IMAGE),
];

/// Push constants passed to the compute shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    resolution: [i32; 2],
}

/// The `rgb_to_nv12.comp` pipeline and the one descriptor set its dispatch
/// binds, rewritten before each dispatch: the converter waits on every
/// dispatch's fence before the next, so the set is never in flight twice.
struct RgbToNv12ComputePipeline {
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
}

impl RgbToNv12ComputePipeline {
    unsafe fn new(device: &vulkanalia::Device) -> Result<Self, VideoError> {
        unsafe {
            let layout_bindings = BINDINGS.map(|(binding, descriptor_type)| {
                vk::DescriptorSetLayoutBinding::builder()
                    .binding(binding)
                    .descriptor_type(descriptor_type)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
                    .build()
            });
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
            let (spirv_word_bytes, _) = SHADER_SPIRV.as_chunks::<4>();
            let spirv_words: Vec<u32> = spirv_word_bytes
                .iter()
                .map(|word| u32::from_le_bytes(*word))
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
                    .type_(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .build(),
                vk::DescriptorPoolSize::builder()
                    .type_(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(2)
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

    /// Point the set at this dispatch's images, then record the bind, push
    /// constants and dispatch into `command_buffer`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn record(
        &self,
        device: &vulkanalia::Device,
        command_buffer: vk::CommandBuffer,
        rgba_image_view: vk::ImageView,
        rgba_image_layout: SampledImageReadLayout,
        luma_view: vk::ImageView,
        chroma_view: vk::ImageView,
        push_constants: PushConstants,
        group_count_x: u32,
        group_count_y: u32,
    ) {
        unsafe {
            let rgba_image_info = [vk::DescriptorImageInfo::builder()
                .image_view(rgba_image_view)
                .image_layout(rgba_image_layout.as_vk())
                .build()];
            let luma_image_info = [vk::DescriptorImageInfo::builder()
                .image_view(luma_view)
                .image_layout(vk::ImageLayout::GENERAL)
                .build()];
            let chroma_image_info = [vk::DescriptorImageInfo::builder()
                .image_view(chroma_view)
                .image_layout(vk::ImageLayout::GENERAL)
                .build()];
            let writes = [
                vk::WriteDescriptorSet::builder()
                    .dst_set(self.descriptor_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&rgba_image_info)
                    .build(),
                vk::WriteDescriptorSet::builder()
                    .dst_set(self.descriptor_set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(&luma_image_info)
                    .build(),
                vk::WriteDescriptorSet::builder()
                    .dst_set(self.descriptor_set)
                    .dst_binding(2)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(&chroma_image_info)
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

/// GPU RGB(A) → NV12 converter.
///
/// Owns a compute pipeline plus two NV12 images (compute-output + encode-src)
/// joined by a per-plane vkCmdCopyImage. The encode-src image uses
/// `CONCURRENT` sharing between the compute and encode queue families so it
/// can be used as `VIDEO_ENCODE_SRC` without queue family ownership transfers.
pub struct RgbToNv12Converter {
    device: vulkanalia::Device,
    allocator: Arc<vma::Allocator>,

    compute_pipeline: RgbToNv12ComputePipeline,

    // Compute-output NV12 image — STORAGE + TRANSFER_SRC, no video profile.
    compute_nv12_image: vk::Image,
    compute_nv12_allocation: vma::Allocation,
    luma_view: vk::ImageView,
    chroma_view: vk::ImageView,

    // Encode-src NV12 image — VIDEO_ENCODE_SRC_KHR + TRANSFER_DST + SAMPLED,
    // with the encode video profile list chained at create time.
    encode_nv12_image: vk::Image,
    encode_nv12_allocation: vma::Allocation,
    encode_nv12_color_view: vk::ImageView,

    // Command recording: the dispatch, its barriers and the multi-plane copy
    // land in one submit.
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,

    // Queue
    compute_queue: vk::Queue,
    _compute_queue_family: u32,

    // The device the converter submits through.
    codec_device: Arc<dyn VulkanVideoCodecDevice>,

    // Dimensions
    width: u32,
    height: u32,
}

impl RgbToNv12Converter {
    /// Create a new RGB→NV12 converter.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Shared Vulkan device context (provides device + VMA allocator)
    /// * `width` - Image width in pixels (must be even)
    /// * `height` - Image height in pixels (must be even)
    /// * `compute_queue_family` - Queue family index with COMPUTE support
    /// * `compute_queue` - Queue from the compute family
    /// * `encode_queue_family` - Queue family index for video encode (for CONCURRENT sharing)
    /// * `codec_flag` - Codec operation flag for the video profile
    ///
    /// # Safety
    ///
    /// `compute_queue` must be a queue of `compute_queue_family` on `ctx`'s device,
    /// and `encode_queue_family` must be a queue family of that device.
    pub unsafe fn new(
        ctx: &Arc<VulkanVideoEncodeContext>,
        width: u32,
        height: u32,
        compute_queue_family: u32,
        compute_queue: vk::Queue,
        encode_queue_family: u32,
        codec_flag: vk::VideoCodecOperationFlagsKHR,
    ) -> Result<Self, VideoError> {
        unsafe {
            let device = ctx.device().clone();
            let allocator = ctx.allocator().clone();
            let codec_device = Arc::clone(ctx.codec_device());
            let compute_pipeline = RgbToNv12ComputePipeline::new(&device)?;

            // --- 2a. Compute-output NV12 image (STORAGE | TRANSFER_SRC) ---
            // No video profile chained here — STORAGE on a VIDEO_ENCODE_SRC image is
            // not reported as a supported video format by NVIDIA encode profiles
            // (VUID-VkImageCreateInfo-pNext-06811). Compute writes go here, then a
            // per-plane vkCmdCopyImage moves the result to the encode-src image.
            let queue_families = [compute_queue_family, encode_queue_family];
            let concurrent = compute_queue_family != encode_queue_family;

            let compute_nv12_image_info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::_2D)
                .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .flags(vk::ImageCreateFlags::MUTABLE_FORMAT | vk::ImageCreateFlags::EXTENDED_USAGE)
                .usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);

            let alloc_options = vma::AllocationOptions {
                required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                ..Default::default()
            };
            let (compute_nv12_image, compute_nv12_allocation) =
                allocator.create_image(compute_nv12_image_info, &alloc_options)?;

            // --- 2b. Encode-src NV12 image (VIDEO_ENCODE_SRC | TRANSFER_DST | SAMPLED) ---
            // Profile list MUST match the video session profile exactly, including
            // the encode_usage pNext chain. Without this, the validation layer
            // reports VUID-vkCmdEncodeVideoKHR-pEncodeInfo-08206. Keep every field
            // here in sync with `encode/session.rs` and `encode/staging.rs`.
            let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder()
                .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
            let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder()
                .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);
            let mut encode_usage = vk::VideoEncodeUsageInfoKHR::builder()
                .tuning_mode(vk::VideoEncodeTuningModeKHR::LOW_LATENCY);

            let mut profile_info = vk::VideoProfileInfoKHR::builder()
                .video_codec_operation(codec_flag)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
                .push_next(&mut encode_usage);

            if codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
                profile_info = profile_info.push_next(&mut h264_profile);
            } else {
                profile_info = profile_info.push_next(&mut h265_profile);
            }

            let profile_list = vk::VideoProfileListInfoKHR::builder()
                .profiles(std::slice::from_ref(&profile_info));

            let mut encode_nv12_image_info = vk::ImageCreateInfo::builder()
                .image_type(vk::ImageType::_2D)
                .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
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
                    vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR
                        | vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::SAMPLED,
                )
                .initial_layout(vk::ImageLayout::UNDEFINED);

            if concurrent {
                encode_nv12_image_info = encode_nv12_image_info
                    .sharing_mode(vk::SharingMode::CONCURRENT)
                    .queue_family_indices(&queue_families);
            } else {
                encode_nv12_image_info =
                    encode_nv12_image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
            }
            encode_nv12_image_info.next =
                &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

            let (encode_nv12_image, encode_nv12_allocation) =
                allocator.create_image(encode_nv12_image_info, &alloc_options)?;

            // --- 3. Image views ---

            // COLOR view of the encode-src image for vkCmdEncodeVideoKHR.
            let mut color_view_ycbcr_info =
                vk::SamplerYcbcrConversionInfo::builder().conversion(ctx.nv12_ycbcr_conversion());
            let encode_nv12_color_view = device.create_image_view(
                &vk::ImageViewCreateInfo::builder()
                    .image(encode_nv12_image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .push_next(&mut color_view_ycbcr_info),
                None,
            )?;

            // Per-plane STORAGE views of the compute-output image for the shader.
            let mut luma_usage =
                vk::ImageViewUsageCreateInfo::builder().usage(vk::ImageUsageFlags::STORAGE);
            let luma_view = device.create_image_view(
                &vk::ImageViewCreateInfo::builder()
                    .image(compute_nv12_image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::R8_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::PLANE_0,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .push_next(&mut luma_usage),
                None,
            )?;

            let mut chroma_usage =
                vk::ImageViewUsageCreateInfo::builder().usage(vk::ImageUsageFlags::STORAGE);
            let chroma_view = device.create_image_view(
                &vk::ImageViewCreateInfo::builder()
                    .image(compute_nv12_image)
                    .view_type(vk::ImageViewType::_2D)
                    .format(vk::Format::R8G8_UNORM)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::PLANE_1,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .push_next(&mut chroma_usage),
                None,
            )?;

            // --- 4. Command pool / buffer / fence ---
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
                compute_pipeline,
                compute_nv12_image,
                compute_nv12_allocation,
                luma_view,
                chroma_view,
                encode_nv12_image,
                encode_nv12_allocation,
                encode_nv12_color_view,
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

    /// Convert an RGBA VkImage to NV12.
    ///
    /// The input image is sampled in `rgba_image_layout`, barriered into it
    /// ahead of the dispatch when `rgba_source_barrier` is given.
    /// After this call, the encode-src NV12 image is in `VIDEO_ENCODE_SRC_KHR`
    /// layout and ready for the encoder.
    ///
    /// `source_width` / `source_height` are the real extent of the image
    /// behind `rgba_image_view`: the shader clamps every fetch to it, so a
    /// source smaller than the converter's codec-aligned extent yields
    /// edge-replicated padding rows/columns — the region the SPS conformance
    /// crop hides — instead of out-of-bounds fetches of undefined pixels.
    ///
    /// Returns `(encode_nv12_image, encode_nv12_color_view)` for the caller to
    /// pass to `Encoder::encode_frame()`.
    ///
    /// # Safety
    ///
    /// `rgba_image_view` must be a live view on the converter's device over an
    /// image of `source_width` × `source_height`.
    pub unsafe fn convert(
        &mut self,
        rgba_image_view: vk::ImageView,
        rgba_image_layout: SampledImageReadLayout,
        rgba_source_barrier: Option<RgbaSourceImageBarrierIntoItsSampledLayout>,
        source_width: u32,
        source_height: u32,
    ) -> Result<(vk::Image, vk::ImageView), VideoError> {
        unsafe {
            let cb = self.command_buffer;

            self.device
                .reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;

            // --- Barrier: compute_nv12 UNDEFINED → GENERAL (for compute writes) ---
            let barrier_compute_to_general = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.compute_nv12_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            // A pool slot can be handed back while an earlier reader's submit
            // is still in flight, so the source's barrier waits on every prior
            // stage and write.
            let source_barrier = rgba_source_barrier.map(|source| {
                vk::ImageMemoryBarrier2::builder()
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                    .old_layout(source.published_layout)
                    .new_layout(rgba_image_layout.as_vk())
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(source.rgba_image)
                    .subresource_range(SINGLE_MIP_SINGLE_LAYER_COLOR_SUBRESOURCE_RANGE)
                    .build()
            });
            let barriers_before_the_dispatch = [
                barrier_compute_to_general.build(),
                source_barrier.unwrap_or_default(),
            ];
            let pre_barriers =
                &barriers_before_the_dispatch[..1 + source_barrier.is_some() as usize];
            let pre_dep = vk::DependencyInfo::builder().image_memory_barriers(pre_barriers);
            self.device.cmd_pipeline_barrier2(cb, &pre_dep);

            // --- Bind compute pipeline + push descriptors + push constants + dispatch ---
            // Each thread handles a 2x2 luma block, so we need
            // (width/2 + 7) / 8 x (height/2 + 7) / 8 workgroups.
            let group_x = (self.width / 2).div_ceil(8);
            let group_y = (self.height / 2).div_ceil(8);
            self.compute_pipeline.record(
                &self.device,
                cb,
                rgba_image_view,
                rgba_image_layout,
                self.luma_view,
                self.chroma_view,
                PushConstants {
                    resolution: [source_width as i32, source_height as i32],
                },
                group_x,
                group_y,
            );

            // --- Barriers: compute_nv12 → TRANSFER_SRC, encode_nv12 → TRANSFER_DST ---
            let barrier_compute_to_transfer = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.compute_nv12_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let barrier_encode_to_transfer = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::NONE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.encode_nv12_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let copy_barriers = [barrier_compute_to_transfer, barrier_encode_to_transfer];
            let copy_dep = vk::DependencyInfo::builder().image_memory_barriers(&copy_barriers);
            self.device.cmd_pipeline_barrier2(cb, &copy_dep);

            // --- Per-plane vkCmdCopyImage ---
            // Multi-planar copies must specify each plane separately
            // (VUID-vkCmdCopyImage-srcImage-01558). PLANE_0 is full-res luma,
            // PLANE_1 is half-res interleaved chroma.
            let plane_0_region = vk::ImageCopy::builder()
                .src_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_0,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .src_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .dst_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_0,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .dst_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .extent(vk::Extent3D {
                    width: self.width,
                    height: self.height,
                    depth: 1,
                })
                .build();
            let plane_1_region = vk::ImageCopy::builder()
                .src_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_1,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .src_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .dst_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_1,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .dst_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .extent(vk::Extent3D {
                    width: self.width / 2,
                    height: self.height / 2,
                    depth: 1,
                })
                .build();
            let regions = [plane_0_region, plane_1_region];
            self.device.cmd_copy_image(
                cb,
                self.compute_nv12_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.encode_nv12_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );

            // --- Barrier: encode_nv12 TRANSFER_DST → VIDEO_ENCODE_SRC ---
            let barrier_encode_to_src = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::NONE)
                .dst_access_mask(vk::AccessFlags2::empty())
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.encode_nv12_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            let post_barriers = [barrier_encode_to_src];
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
                .submit_to_queue(self.compute_queue, &[submit], self.fence)?;
            self.device.wait_for_fences(&[self.fence], true, u64::MAX)?;

            Ok((self.encode_nv12_image, self.encode_nv12_color_view))
        }
    }

    /// Returns the encode-src NV12 image handle (the one bound to the encoder).
    pub fn nv12_image(&self) -> vk::Image {
        self.encode_nv12_image
    }

    /// Returns the encode-src NV12 COLOR image view (combined planes, for encoder).
    pub fn nv12_color_view(&self) -> vk::ImageView {
        self.encode_nv12_color_view
    }
}

impl Drop for RgbToNv12Converter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.codec_device.wait_idle();

            self.compute_pipeline.destroy(&self.device);

            if self.fence != vk::Fence::null() {
                self.device.destroy_fence(self.fence, None);
            }
            if self.command_pool != vk::CommandPool::null() {
                self.device.destroy_command_pool(self.command_pool, None);
            }

            if self.chroma_view != vk::ImageView::null() {
                self.device.destroy_image_view(self.chroma_view, None);
            }
            if self.luma_view != vk::ImageView::null() {
                self.device.destroy_image_view(self.luma_view, None);
            }
            if self.encode_nv12_color_view != vk::ImageView::null() {
                self.device
                    .destroy_image_view(self.encode_nv12_color_view, None);
            }
            if self.encode_nv12_image != vk::Image::null() {
                self.allocator
                    .destroy_image(self.encode_nv12_image, self.encode_nv12_allocation);
            }
            if self.compute_nv12_image != vk::Image::null() {
                self.allocator
                    .destroy_image(self.compute_nv12_image, self.compute_nv12_allocation);
            }
        }
    }
}

// SAFETY: Vulkan handles are only accessed through &mut self methods.
unsafe impl Send for RgbToNv12Converter {}

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
            SHADER_SPIRV.len().is_multiple_of(4),
            "SPIR-V size must be multiple of 4"
        );
        // Verify magic number
        let magic = u32::from_le_bytes([
            SHADER_SPIRV[0],
            SHADER_SPIRV[1],
            SHADER_SPIRV[2],
            SHADER_SPIRV[3],
        ]);
        assert_eq!(magic, 0x07230203, "Invalid SPIR-V magic number");
    }
}
