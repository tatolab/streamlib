// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GPU RGB(A) → NV12 color-space converter using a Vulkan compute shader.
//!
//! Based on pyroenc's `rgb_to_yuv.comp` shader (MIT, Arntzen Software AS).
//! Uses BT.709 color matrix with TV range (16-235 luma, 16-240 chroma),
//! 6-tap left-sited chroma filter, and push descriptors for zero allocation.
//!
//! The converter creates an NV12 output image with flags suitable for both
//! compute shader writes (STORAGE_BIT) and video encode source
//! (VIDEO_ENCODE_SRC_BIT), using CONCURRENT sharing between compute and
//! encode queue families.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrPushDescriptorExtensionDeviceCommands;
use vulkanalia_vma::{self as vma, Alloc};

use crate::video_context::{VideoContext, VideoError};

/// Pre-compiled SPIR-V bytecode for the RGB→NV12 compute shader.
const SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rgb_to_nv12.spv"));

/// Push constants passed to the compute shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    resolution: [i32; 2],
}

/// GPU RGB(A) → NV12 converter.
///
/// Owns a compute pipeline, NV12 output image with per-plane views, and a
/// command buffer for recording conversion dispatches. The output image uses
/// `CONCURRENT` sharing between the compute and encode queue families so it
/// can be used as `VIDEO_ENCODE_SRC` without queue family ownership transfers.
pub struct RgbToNv12Converter {
    device: vulkanalia::Device,
    allocator: Arc<vma::Allocator>,

    // Pipeline objects
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    shader_module: vk::ShaderModule,

    // NV12 output image
    nv12_image: vk::Image,
    nv12_allocation: vma::Allocation,
    nv12_color_view: vk::ImageView,
    luma_view: vk::ImageView,
    chroma_view: vk::ImageView,

    // Command recording
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,

    // Queue
    compute_queue: vk::Queue,
    _compute_queue_family: u32,

    // Host-side queue submission gateway.
    submitter: Arc<dyn crate::rhi::RhiQueueSubmitter>,

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
    pub unsafe fn new(
        ctx: &Arc<VideoContext>,
        width: u32,
        height: u32,
        compute_queue_family: u32,
        compute_queue: vk::Queue,
        encode_queue_family: u32,
        codec_flag: vk::VideoCodecOperationFlagsKHR,
        submitter: Arc<dyn crate::rhi::RhiQueueSubmitter>,
    ) -> Result<Self, VideoError> {
        let device = ctx.device().clone();
        let allocator = ctx.allocator().clone();
        // --- 1. Create shader module ---
        let spirv_words = Self::spirv_to_words(SHADER_SPIRV);
        let shader_module = device.create_shader_module(
            &vk::ShaderModuleCreateInfo::builder()
                .code_size(SHADER_SPIRV.len())
                .code(&spirv_words),
            None,
        )?;

        // --- 2. Descriptor set layout (push descriptors) ---
        let bindings = [
            // binding 0: sampled input image (texture2D)
            vk::DescriptorSetLayoutBinding::builder()
                .binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build(),
            // binding 1: luma output (image2D, R8)
            vk::DescriptorSetLayoutBinding::builder()
                .binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build(),
            // binding 2: chroma output (image2D, RG8)
            vk::DescriptorSetLayoutBinding::builder()
                .binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build(),
        ];

        let descriptor_set_layout = device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::builder()
                .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR)
                .bindings(&bindings),
            None,
        )?;
        // --- 3. Pipeline layout (push constants + push descriptors) ---
        let push_range = vk::PushConstantRange::builder()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(std::mem::size_of::<PushConstants>() as u32);

        let pipeline_layout = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::builder()
                .set_layouts(std::slice::from_ref(&descriptor_set_layout))
                .push_constant_ranges(std::slice::from_ref(&push_range)),
            None,
        )?;

        // --- 4. Compute pipeline ---
        let stage = vk::PipelineShaderStageCreateInfo::builder()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(shader_module)
            .name(b"main\0");

        let pipeline_info = vk::ComputePipelineCreateInfo::builder()
            .stage(stage)
            .layout(pipeline_layout);

        let (pipelines, _) = device
            .create_compute_pipelines(
                vk::PipelineCache::null(),
                std::slice::from_ref(&pipeline_info),
                None,
            )
            .map_err(|e| VideoError::Vulkan(vk::Result::from(e)))?;
        let pipeline = pipelines[0];

        // --- 5. NV12 output image ---
        // Build the video profile for the profile list (required for
        // VIDEO_ENCODE_SRC usage).
        let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder()
            .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
        let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder()
            .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);

        let mut profile_info = vk::VideoProfileInfoKHR::builder()
            .video_codec_operation(codec_flag)
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8);

        if codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            profile_info = profile_info.push_next(&mut h264_profile);
        } else {
            profile_info = profile_info.push_next(&mut h265_profile);
        }

        let profile_list =
            vk::VideoProfileListInfoKHR::builder().profiles(std::slice::from_ref(&profile_info));

        let queue_families = [compute_queue_family, encode_queue_family];
        let concurrent = compute_queue_family != encode_queue_family;

        let mut nv12_image_info = vk::ImageCreateInfo::builder()
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
            .flags(
                vk::ImageCreateFlags::MUTABLE_FORMAT
                    | vk::ImageCreateFlags::EXTENDED_USAGE
                    | vk::ImageCreateFlags::VIDEO_PROFILE_INDEPENDENT_KHR,
            )
            .usage(
                vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);

        if concurrent {
            nv12_image_info = nv12_image_info
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&queue_families);
        } else {
            nv12_image_info = nv12_image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
        }

        // Chain video profile list for VIDEO_ENCODE_SRC compatibility.
        nv12_image_info.next =
            &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

        let alloc_options = vma::AllocationOptions {
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let (nv12_image, nv12_allocation) =
            allocator.create_image(nv12_image_info, &alloc_options)?;

        // --- 6. Image views ---

        // COLOR view for vkCmdEncodeVideoKHR (combined planes).
        let mut color_view_ycbcr_info = vk::SamplerYcbcrConversionInfo::builder()
            .conversion(ctx.nv12_ycbcr_conversion());
        let nv12_color_view = device.create_image_view(
            &vk::ImageViewCreateInfo::builder()
                .image(nv12_image)
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

        // Per-plane views restricted to STORAGE usage for compute shader writes.
        let mut luma_usage = vk::ImageViewUsageCreateInfo::builder()
            .usage(vk::ImageUsageFlags::STORAGE);
        let luma_view = device.create_image_view(
            &vk::ImageViewCreateInfo::builder()
                .image(nv12_image)
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

        let mut chroma_usage = vk::ImageViewUsageCreateInfo::builder()
            .usage(vk::ImageUsageFlags::STORAGE);
        let chroma_view = device.create_image_view(
            &vk::ImageViewCreateInfo::builder()
                .image(nv12_image)
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

        // --- 7. Command pool / buffer / fence ---
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
            descriptor_set_layout,
            pipeline_layout,
            pipeline,
            shader_module,
            nv12_image,
            nv12_allocation,
            nv12_color_view,
            luma_view,
            chroma_view,
            command_pool,
            command_buffer,
            fence,
            compute_queue,
            _compute_queue_family: compute_queue_family,
            submitter,
            width,
            height,
        })
    }

    /// Convert an RGBA VkImage to NV12.
    ///
    /// The input image must be in `SHADER_READ_ONLY_OPTIMAL` layout.
    /// After this call, the NV12 output image is in `VIDEO_ENCODE_SRC_KHR`
    /// layout and ready for the encoder.
    ///
    /// Returns `(nv12_image, nv12_color_view)` for the caller to pass to
    /// `Encoder::encode_frame()`.
    pub unsafe fn convert(
        &mut self,
        rgba_image_view: vk::ImageView,
    ) -> Result<(vk::Image, vk::ImageView), VideoError> {
        let cb = self.command_buffer;
        let no_mem_barriers: &[vk::MemoryBarrier] = &[];
        let no_buf_barriers: &[vk::BufferMemoryBarrier] = &[];

        self.device
            .reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
        self.device.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;

        // --- Barrier: NV12 UNDEFINED → GENERAL (for compute writes) ---
        let barrier_to_general = vk::ImageMemoryBarrier::builder()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.nv12_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_access_mask(vk::AccessFlags::SHADER_WRITE);

        self.device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            no_mem_barriers,
            no_buf_barriers,
            &[barrier_to_general],
        );

        // --- Bind compute pipeline ---
        self.device
            .cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.pipeline);

        // --- Push descriptors ---
        let input_image_info = vk::DescriptorImageInfo::builder()
            .image_view(rgba_image_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);

        let luma_image_info = vk::DescriptorImageInfo::builder()
            .image_view(self.luma_view)
            .image_layout(vk::ImageLayout::GENERAL);

        let chroma_image_info = vk::DescriptorImageInfo::builder()
            .image_view(self.chroma_view)
            .image_layout(vk::ImageLayout::GENERAL);

        let writes = [
            vk::WriteDescriptorSet::builder()
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(std::slice::from_ref(&input_image_info))
                .build(),
            vk::WriteDescriptorSet::builder()
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(std::slice::from_ref(&luma_image_info))
                .build(),
            vk::WriteDescriptorSet::builder()
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(std::slice::from_ref(&chroma_image_info))
                .build(),
        ];

        self.device.cmd_push_descriptor_set_khr(
            cb,
            vk::PipelineBindPoint::COMPUTE,
            self.pipeline_layout,
            0,
            &writes,
        );

        // --- Push constants ---
        let push = PushConstants {
            resolution: [self.width as i32, self.height as i32],
        };
        self.device.cmd_push_constants(
            cb,
            self.pipeline_layout,
            vk::ShaderStageFlags::COMPUTE,
            0,
            std::slice::from_raw_parts(
                &push as *const PushConstants as *const u8,
                std::mem::size_of::<PushConstants>(),
            ),
        );

        // --- Dispatch ---
        // Each thread handles a 2x2 luma block, so we need
        // (width/2 + 7) / 8 x (height/2 + 7) / 8 workgroups.
        let group_x = (self.width / 2 + 7) / 8;
        let group_y = (self.height / 2 + 7) / 8;
        self.device.cmd_dispatch(cb, group_x, group_y, 1);

        // --- Barrier: NV12 GENERAL → VIDEO_ENCODE_SRC ---
        let barrier_to_encode = vk::ImageMemoryBarrier::builder()
            .src_access_mask(vk::AccessFlags::SHADER_WRITE)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.nv12_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });

        self.device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            no_mem_barriers,
            no_buf_barriers,
            &[barrier_to_encode],
        );

        self.device.end_command_buffer(cb)?;

        // --- Submit and wait ---
        let submit = vk::SubmitInfo::builder()
            .command_buffers(std::slice::from_ref(&cb))
            .build();

        self.device.reset_fences(&[self.fence])?;
        self.submitter
            .submit_to_queue_legacy(self.compute_queue, &[submit], self.fence)?;
        self.device
            .wait_for_fences(&[self.fence], true, u64::MAX)?;

        Ok((self.nv12_image, self.nv12_color_view))
    }

    /// Returns the NV12 output image handle.
    pub fn nv12_image(&self) -> vk::Image {
        self.nv12_image
    }

    /// Returns the NV12 COLOR image view (combined planes, for encoder).
    pub fn nv12_color_view(&self) -> vk::ImageView {
        self.nv12_color_view
    }

    /// Convert SPIR-V byte slice to u32 word slice.
    fn spirv_to_words(bytes: &[u8]) -> Vec<u32> {
        assert!(bytes.len() % 4 == 0, "SPIR-V size must be multiple of 4");
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}

impl Drop for RgbToNv12Converter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();

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
            if self.nv12_color_view != vk::ImageView::null() {
                self.device.destroy_image_view(self.nv12_color_view, None);
            }
            if self.nv12_image != vk::Image::null() {
                self.allocator
                    .destroy_image(self.nv12_image, self.nv12_allocation);
            }

            if self.pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(self.pipeline, None);
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.pipeline_layout, None);
            }
            if self.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                self.device
                    .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            }
            if self.shader_module != vk::ShaderModule::null() {
                self.device
                    .destroy_shader_module(self.shader_module, None);
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
    fn test_spirv_to_words() {
        let bytes = [0x03, 0x02, 0x23, 0x07, 0x00, 0x00, 0x01, 0x00];
        let words = RgbToNv12Converter::spirv_to_words(&bytes);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0], 0x07230203); // SPIR-V magic number
    }

    #[test]
    fn test_push_constants_size() {
        assert_eq!(std::mem::size_of::<PushConstants>(), 8);
    }

    #[test]
    fn test_spirv_embedded() {
        assert!(!SHADER_SPIRV.is_empty(), "SPIR-V bytecode must not be empty");
        assert!(
            SHADER_SPIRV.len() % 4 == 0,
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
