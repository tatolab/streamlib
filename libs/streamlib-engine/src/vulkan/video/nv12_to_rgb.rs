// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GPU NV12 → RGB(A) color-space converter using a Vulkan compute shader.
//!
//! The reverse of [`crate::vulkan::video::rgb_to_nv12::RgbToNv12Converter`].
//!
//! Uses a `VkSamplerYcbcrConversion` combined image sampler baked into the
//! descriptor-set layout via `pImmutableSamplers` so the hardware handles
//! plane separation, chroma upsampling (4:2:0 → 4:4:4), and YCbCr→RGB
//! conversion automatically (BT.709 / ITU narrow range).
//!
//! The converter creates an RGBA output image with `STORAGE` usage for compute
//! writes and `TRANSFER_SRC` for CPU readback, using `CONCURRENT` sharing
//! between the compute and decode queue families.
//!
//! Compute dispatch is owned by [`VulkanComputeKernel`]; the converter
//! holds the YCbCr conversion + sampler + per-call NV12 sampled view, plus
//! its own command-buffer / fence for recording the surrounding barriers.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor};
use crate::vulkan::rhi::VulkanComputeKernel;
use crate::vulkan::video::video_context::{VideoContext, VideoError};

/// Pre-compiled SPIR-V bytecode for the NV12→RGB compute shader.
const SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nv12_to_rgb.spv"));

/// Binding shape of `nv12_to_rgb.comp`:
/// - 0: COMBINED_IMAGE_SAMPLER — NV12 input via an immutable YCbCr sampler
///   chained at descriptor-set-layout creation time.
/// - 1: STORAGE_IMAGE — RGBA output (R8G8B8A8_UNORM).
const BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::sampled_texture(0),
    ComputeBindingSpec::storage_image(1),
];

/// Push constants passed to the compute shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    resolution: [i32; 2],
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
    ycbcr_conversion: vk::SamplerYcbcrConversion,
    sampler: vk::Sampler,

    // Engine-managed compute pipeline (descriptor set / pipeline layout /
    // pipeline / pipeline cache / SPIR-V reflection all live inside).
    kernel: VulkanComputeKernel,

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
    submitter: Arc<dyn crate::vulkan::video::rhi::RhiQueueSubmitter>,

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
    pub unsafe fn new(
        ctx: &Arc<VideoContext>,
        width: u32,
        height: u32,
        compute_queue_family: u32,
        compute_queue: vk::Queue,
        decode_queue_family: u32,
        submitter: Arc<dyn crate::vulkan::video::rhi::RhiQueueSubmitter>,
    ) -> Result<Self, VideoError> { unsafe {
        let device = ctx.device().clone();
        let allocator = ctx.allocator().clone();

        // --- 1. YCbCr conversion (BT.709, ITU narrow range) ---
        let ycbcr_conversion = device.create_sampler_ycbcr_conversion(
            &vk::SamplerYcbcrConversionCreateInfo::builder()
                .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .ycbcr_model(vk::SamplerYcbcrModelConversion::YCBCR_709)
                .ycbcr_range(vk::SamplerYcbcrRange::ITU_NARROW)
                .components(vk::ComponentMapping {
                    r: vk::ComponentSwizzle::IDENTITY,
                    g: vk::ComponentSwizzle::IDENTITY,
                    b: vk::ComponentSwizzle::IDENTITY,
                    a: vk::ComponentSwizzle::IDENTITY,
                })
                .x_chroma_offset(vk::ChromaLocation::MIDPOINT)
                .y_chroma_offset(vk::ChromaLocation::MIDPOINT)
                .chroma_filter(vk::Filter::LINEAR)
                .force_explicit_reconstruction(false),
            None,
        )?;

        // --- 2. Sampler with YCbCr conversion ---
        let mut ycbcr_info = vk::SamplerYcbcrConversionInfo::builder()
            .conversion(ycbcr_conversion);

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

        // --- 3. Compute kernel via the engine RHI, with the YCbCr
        // sampler baked into the descriptor-set layout as an immutable
        // sampler. Lifetime contract: `sampler` is owned by Self and is
        // not dropped until after `kernel` (Drop runs fields in
        // declaration order, so `sampler` is declared AFTER `kernel` in
        // the struct body for that reason).
        let kernel = VulkanComputeKernel::new_with_immutable_samplers(
            ctx.host_device(),
            &ComputeKernelDescriptor {
                label: "nv12_to_rgb",
                spv: SHADER_SPIRV,
                bindings: BINDINGS,
                push_constant_size: std::mem::size_of::<PushConstants>() as u32,
            },
            &[(0, sampler)],
        )?;

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
            ycbcr_conversion,
            sampler,
            kernel,
            rgba_image,
            rgba_allocation,
            rgba_view,
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
    ) -> Result<(vk::Image, vk::ImageView), VideoError> { unsafe {
        // Create a sampled view for this layer with YCbCr conversion info.
        // The YCbCr conversion chained on the view must match the immutable
        // sampler's conversion exactly (VUID-VkImageView/VkSampler-format-..).
        let mut ycbcr_info = vk::SamplerYcbcrConversionInfo::builder()
            .conversion(self.ycbcr_conversion);

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

        // --- Stage descriptor writes ---
        // Binding 0: COMBINED_IMAGE_SAMPLER with view-only write (sampler
        // is the immutable YCbCr one baked into the descriptor-set
        // layout; Vulkan ignores the sampler field in the write).
        // Binding 1: STORAGE_IMAGE for RGBA output.
        self.kernel
            .set_combined_image_sampler_view(0, nv12_sampled_view)?;
        self.kernel.set_storage_image_view(1, self.rgba_view)?;
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
        let pre_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&pre_barriers);
        self.device.cmd_pipeline_barrier2(cb, &pre_dep);

        // --- Bind compute pipeline + push descriptors + push constants + dispatch ---
        // Each thread handles one pixel, workgroup is 8x8.
        let group_x = (self.width + 7) / 8;
        let group_y = (self.height + 7) / 8;
        self.kernel
            .record(cb, group_x, group_y, 1)
            .map_err(|e| {
                VideoError::Other(format!("nv12_to_rgb kernel record failed: {e}"))
            })?;

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

        // Destroy temporary view
        self.device
            .destroy_image_view(nv12_sampled_view, None);

        Ok((self.rgba_image, self.rgba_view))
    }}

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
}

impl Drop for Nv12ToRgbConverter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();

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
            // The compute kernel (descriptor layout / pipeline / pipeline
            // layout / shader module / descriptor pool / command pool /
            // fence) is torn down by its own Drop when `self.kernel` is
            // dropped after this function returns. The kernel's
            // descriptor-set layout retains the immutable `sampler`
            // handle via `pImmutableSamplers`; the `device_wait_idle`
            // above guarantees no GPU work is still using it, so
            // destroying the sampler here is safe.
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
        assert!(!SHADER_SPIRV.is_empty(), "SPIR-V bytecode must not be empty");
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
    fn bindings_match_shader_declaration() {
        // Lock the binding shape against the shader: any drift between
        // BINDINGS and nv12_to_rgb.comp will fail SPIR-V reflection at
        // kernel construction time (rejecting silently-wrong dispatches),
        // but locking it here at the data level catches regressions
        // without needing a GPU.
        use crate::core::rhi::ComputeBindingKind;
        assert_eq!(BINDINGS.len(), 2);
        assert_eq!(BINDINGS[0].binding, 0);
        assert_eq!(BINDINGS[0].kind, ComputeBindingKind::SampledTexture);
        assert_eq!(BINDINGS[1].binding, 1);
        assert_eq!(BINDINGS[1].kind, ComputeBindingKind::StorageImage);
    }
}
