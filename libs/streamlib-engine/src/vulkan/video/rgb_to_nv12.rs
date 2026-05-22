// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
//! Compute dispatch is owned by [`VulkanComputeKernel`]; the converter
//! holds the per-plane reinterpreted-format storage views the kernel
//! writes through plus its own command-buffer / fence for recording the
//! surrounding barriers + copy.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor};
use crate::vulkan::rhi::VulkanComputeKernel;
use crate::vulkan::video::video_context::{VideoContext, VideoError};

/// Pre-compiled SPIR-V bytecode for the RGB→NV12 compute shader.
const SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rgb_to_nv12.spv"));

/// Binding shape of `rgb_to_nv12.comp`:
/// - 0: SAMPLED_IMAGE — input RGBA texture (texture2D, texelFetch).
/// - 1: STORAGE_IMAGE — luma output plane (R8, reinterpreted view of NV12 plane 0).
/// - 2: STORAGE_IMAGE — chroma output plane (RG8, reinterpreted view of NV12 plane 1).
const BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::sampled_image(0),
    ComputeBindingSpec::storage_image(1),
    ComputeBindingSpec::storage_image(2),
];

/// Push constants passed to the compute shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    resolution: [i32; 2],
}

/// GPU RGB(A) → NV12 converter.
///
/// Owns a compute kernel plus two NV12 images (compute-output + encode-src)
/// joined by a per-plane vkCmdCopyImage. The encode-src image uses
/// `CONCURRENT` sharing between the compute and encode queue families so it
/// can be used as `VIDEO_ENCODE_SRC` without queue family ownership transfers.
pub struct RgbToNv12Converter {
    device: vulkanalia::Device,
    allocator: Arc<vma::Allocator>,

    // Engine-managed compute pipeline (descriptor set / pipeline layout /
    // pipeline / pipeline cache / SPIR-V reflection all live inside).
    kernel: VulkanComputeKernel,

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

    // Command recording (separate from the kernel's own internal command
    // buffer so the converter can wrap `kernel.record(cb, ...)` with its
    // own barriers + multi-plane copy).
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,

    // Queue
    compute_queue: vk::Queue,
    _compute_queue_family: u32,

    // Host-side queue submission gateway.
    submitter: Arc<dyn crate::vulkan::video::rhi::RhiQueueSubmitter>,

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
        submitter: Arc<dyn crate::vulkan::video::rhi::RhiQueueSubmitter>,
    ) -> Result<Self, VideoError> { unsafe {
        let device = ctx.device().clone();
        let allocator = ctx.allocator().clone();

        // --- 1. Compute kernel via the engine RHI ---
        // Descriptor-set layout, pipeline layout, compute pipeline, descriptor
        // pool, command buffer + fence (for `dispatch`) all live inside the
        // kernel. The converter uses `kernel.record(...)` instead of
        // `kernel.dispatch(...)` so the surrounding barriers + multi-plane
        // copy land in the converter's own command buffer in one submit.
        let kernel = VulkanComputeKernel::new(
            ctx.host_device(),
            &ComputeKernelDescriptor {
                label: "rgb_to_nv12",
                spv: SHADER_SPIRV,
                bindings: BINDINGS,
                push_constant_size: std::mem::size_of::<PushConstants>() as u32,
            },
        )?;

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
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .flags(
                vk::ImageCreateFlags::MUTABLE_FORMAT | vk::ImageCreateFlags::EXTENDED_USAGE,
            )
            .usage(
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
            )
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

        let profile_list =
            vk::VideoProfileListInfoKHR::builder().profiles(std::slice::from_ref(&profile_info));

        let mut encode_nv12_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(vk::Extent3D { width, height, depth: 1 })
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
            encode_nv12_image_info = encode_nv12_image_info.sharing_mode(vk::SharingMode::EXCLUSIVE);
        }
        encode_nv12_image_info.next =
            &*profile_list as *const vk::VideoProfileListInfoKHR as *const std::ffi::c_void;

        let (encode_nv12_image, encode_nv12_allocation) =
            allocator.create_image(encode_nv12_image_info, &alloc_options)?;

        // --- 3. Image views ---

        // COLOR view of the encode-src image for vkCmdEncodeVideoKHR.
        let mut color_view_ycbcr_info = vk::SamplerYcbcrConversionInfo::builder()
            .conversion(ctx.nv12_ycbcr_conversion());
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
        let mut luma_usage = vk::ImageViewUsageCreateInfo::builder()
            .usage(vk::ImageUsageFlags::STORAGE);
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

        let mut chroma_usage = vk::ImageViewUsageCreateInfo::builder()
            .usage(vk::ImageUsageFlags::STORAGE);
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
            kernel,
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
            submitter,
            width,
            height,
        })
    }}

    /// Convert an RGBA VkImage to NV12.
    ///
    /// The input image must be in `SHADER_READ_ONLY_OPTIMAL` layout.
    /// After this call, the encode-src NV12 image is in `VIDEO_ENCODE_SRC_KHR`
    /// layout and ready for the encoder.
    ///
    /// Returns `(encode_nv12_image, encode_nv12_color_view)` for the caller to
    /// pass to `Encoder::encode_frame()`.
    pub unsafe fn convert(
        &mut self,
        rgba_image_view: vk::ImageView,
    ) -> Result<(vk::Image, vk::ImageView), VideoError> { unsafe {
        let cb = self.command_buffer;

        // --- Stage descriptor writes for this dispatch ---
        // The kernel drains its pending state on `record(...)`; setting
        // before recording is the contract.
        self.kernel.set_sampled_image_view(0, rgba_image_view)?;
        self.kernel.set_storage_image_view(1, self.luma_view)?;
        self.kernel.set_storage_image_view(2, self.chroma_view)?;
        self.kernel.set_push_constants_value(&PushConstants {
            resolution: [self.width as i32, self.height as i32],
        })?;

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

        let pre_barriers = [barrier_compute_to_general];
        let pre_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&pre_barriers);
        self.device.cmd_pipeline_barrier2(cb, &pre_dep);

        // --- Bind compute pipeline + push descriptors + push constants + dispatch ---
        // Each thread handles a 2x2 luma block, so we need
        // (width/2 + 7) / 8 x (height/2 + 7) / 8 workgroups.
        let group_x = (self.width / 2 + 7) / 8;
        let group_y = (self.height / 2 + 7) / 8;
        self.kernel
            .record(cb, group_x, group_y, 1)
            .map_err(|e| {
                VideoError::Other(format!("rgb_to_nv12 kernel record failed: {e}"))
            })?;

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
        let copy_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&copy_barriers);
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
            .extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 })
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
            .extent(vk::Extent3D { width: self.width / 2, height: self.height / 2, depth: 1 })
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
        let post_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&post_barriers);
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
        self.submitter
            .submit_to_queue(self.compute_queue, &[submit], self.fence)?;
        self.device
            .wait_for_fences(&[self.fence], true, u64::MAX)?;

        Ok((self.encode_nv12_image, self.encode_nv12_color_view))
    }}

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
            if self.encode_nv12_color_view != vk::ImageView::null() {
                self.device.destroy_image_view(self.encode_nv12_color_view, None);
            }
            if self.encode_nv12_image != vk::Image::null() {
                self.allocator
                    .destroy_image(self.encode_nv12_image, self.encode_nv12_allocation);
            }
            if self.compute_nv12_image != vk::Image::null() {
                self.allocator
                    .destroy_image(self.compute_nv12_image, self.compute_nv12_allocation);
            }
            // The compute kernel (descriptor layout / pipeline / pipeline
            // layout / shader module / descriptor pool / command pool /
            // fence) is torn down by its own Drop when `self.kernel` is
            // dropped after this function returns.
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

    #[test]
    fn bindings_match_shader_declaration() {
        // Lock the binding shape against the shader: any drift between
        // BINDINGS and rgb_to_nv12.comp will fail SPIR-V reflection at
        // kernel construction time (rejecting silently-wrong dispatches),
        // but locking it here at the data level catches regressions
        // without needing a GPU.
        use crate::core::rhi::ComputeBindingKind;
        assert_eq!(BINDINGS.len(), 3);
        assert_eq!(BINDINGS[0].binding, 0);
        assert_eq!(BINDINGS[0].kind, ComputeBindingKind::SampledImage);
        assert_eq!(BINDINGS[1].binding, 1);
        assert_eq!(BINDINGS[1].kind, ComputeBindingKind::StorageImage);
        assert_eq!(BINDINGS[2].binding, 2);
        assert_eq!(BINDINGS[2].kind, ComputeBindingKind::StorageImage);
    }
}
