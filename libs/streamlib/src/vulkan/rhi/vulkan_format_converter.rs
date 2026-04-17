// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{PixelFormat, RhiPixelBuffer};
use crate::core::{Result, StreamError};

/// Vulkan format converter for pixel buffer format conversion via GPU compute.
pub struct VulkanFormatConverter {
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    command_pool: vk::CommandPool,
    source_bytes_per_pixel: u32,
    dest_bytes_per_pixel: u32,
    nv12_to_bgra_pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    nv12_to_bgra_shader_module: vk::ShaderModule,
    compute_command_buffer: vk::CommandBuffer,
    compute_fence: vk::Fence,
}

impl VulkanFormatConverter {
    /// Create a new format converter with GPU compute pipelines.
    pub fn new(
        device: &vulkanalia::Device,
        queue: vk::Queue,
        queue_family_index: u32,
        source_bytes_per_pixel: u32,
        dest_bytes_per_pixel: u32,
    ) -> Result<Self> {
        // Command pool
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .build();

        let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {e}")))?;

        // Load SPIR-V shader (inline conversion — no ash::util dependency)
        let nv12_to_bgra_bytes = include_bytes!("shaders/nv12_to_bgra.spv");
        let nv12_to_bgra_spirv: Vec<u32> = nv12_to_bgra_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let nv12_to_bgra_module_info =
            vk::ShaderModuleCreateInfo::builder().code(&nv12_to_bgra_spirv).build();
        let nv12_to_bgra_shader_module =
            unsafe { device.create_shader_module(&nv12_to_bgra_module_info, None) }.map_err(
                |e| {
                    unsafe { device.destroy_command_pool(command_pool, None) };
                    StreamError::GpuError(format!(
                        "Failed to create nv12_to_bgra shader module: {e}"
                    ))
                },
            )?;

        // Descriptor set layout: binding 0 = input SSBO, binding 1 = output SSBO
        let bindings = [
            vk::DescriptorSetLayoutBinding::builder()
                .binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build(),
            vk::DescriptorSetLayoutBinding::builder()
                .binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .build(),
        ];

        let descriptor_set_layout_info =
            vk::DescriptorSetLayoutCreateInfo::builder().bindings(&bindings).build();

        let descriptor_set_layout =
            unsafe { device.create_descriptor_set_layout(&descriptor_set_layout_info, None) }
                .map_err(|e| {
                    unsafe {
                        device.destroy_shader_module(nv12_to_bgra_shader_module, None);
                        device.destroy_command_pool(command_pool, None);
                    }
                    StreamError::GpuError(format!(
                        "Failed to create descriptor set layout: {e}"
                    ))
                })?;

        // Push constant range: width (u32) + height (u32) + flags (u32) = 12 bytes
        let push_constant_range = vk::PushConstantRange::builder()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(12)
            .build();

        let set_layouts = [descriptor_set_layout];
        let push_constant_ranges = [push_constant_range];
        let pipeline_layout_info = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push_constant_ranges)
            .build();

        let pipeline_layout =
            unsafe { device.create_pipeline_layout(&pipeline_layout_info, None) }.map_err(|e| {
                unsafe {
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(nv12_to_bgra_shader_module, None);
                    device.destroy_command_pool(command_pool, None);
                }
                StreamError::GpuError(format!("Failed to create pipeline layout: {e}"))
            })?;

        // Create NV12→BGRA compute pipeline
        let nv12_to_bgra_stage = vk::PipelineShaderStageCreateInfo::builder()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(nv12_to_bgra_shader_module)
            .name(b"main\0")
            .build();

        let nv12_to_bgra_pipeline_info = vk::ComputePipelineCreateInfo::builder()
            .stage(nv12_to_bgra_stage)
            .layout(pipeline_layout)
            .build();

        let nv12_to_bgra_pipeline = unsafe {
            device.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[nv12_to_bgra_pipeline_info],
                None,
            )
        }
        .map_err(|e| {
            unsafe {
                device.destroy_pipeline_layout(pipeline_layout, None);
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.destroy_shader_module(nv12_to_bgra_shader_module, None);
                device.destroy_command_pool(command_pool, None);
            }
            StreamError::GpuError(format!("Failed to create nv12_to_bgra pipeline: {e}"))
        })?
        .0[0];

        // Descriptor pool (1 set, 2 storage buffers)
        let pool_sizes = [vk::DescriptorPoolSize::builder()
            .type_(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(2)
            .build()];

        let descriptor_pool_info = vk::DescriptorPoolCreateInfo::builder()
            .max_sets(1)
            .pool_sizes(&pool_sizes)
            .build();

        let descriptor_pool =
            unsafe { device.create_descriptor_pool(&descriptor_pool_info, None) }.map_err(|e| {
                unsafe {
                    device.destroy_pipeline(nv12_to_bgra_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(nv12_to_bgra_shader_module, None);
                    device.destroy_command_pool(command_pool, None);
                }
                StreamError::GpuError(format!("Failed to create descriptor pool: {e}"))
            })?;

        // Allocate descriptor set
        let alloc_info = vk::DescriptorSetAllocateInfo::builder()
            .descriptor_pool(descriptor_pool)
            .set_layouts(&set_layouts)
            .build();

        let descriptor_set = unsafe { device.allocate_descriptor_sets(&alloc_info) }
            .map_err(|e| {
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(nv12_to_bgra_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(nv12_to_bgra_shader_module, None);
                    device.destroy_command_pool(command_pool, None);
                }
                StreamError::GpuError(format!("Failed to allocate descriptor set: {e}"))
            })?[0];

        // Command buffer
        let cmd_alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();

        let compute_command_buffer =
            unsafe { device.allocate_command_buffers(&cmd_alloc_info) }.map_err(|e| {
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(nv12_to_bgra_pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                    device.destroy_shader_module(nv12_to_bgra_shader_module, None);
                    device.destroy_command_pool(command_pool, None);
                }
                StreamError::GpuError(format!("Failed to allocate compute command buffer: {e}"))
            })?[0];

        // Fence (pre-signaled so the first convert() can wait+reset without hanging)
        let fence_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED)
            .build();
        let compute_fence = unsafe { device.create_fence(&fence_info, None) }.map_err(|e| {
            unsafe {
                device.destroy_descriptor_pool(descriptor_pool, None);
                device.destroy_pipeline(nv12_to_bgra_pipeline, None);
                device.destroy_pipeline_layout(pipeline_layout, None);
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.destroy_shader_module(nv12_to_bgra_shader_module, None);
                device.destroy_command_pool(command_pool, None);
            }
            StreamError::GpuError(format!("Failed to create compute fence: {e}"))
        })?;

        Ok(Self {
            device: device.clone(),
            queue,
            queue_family_index,
            command_pool,
            source_bytes_per_pixel,
            dest_bytes_per_pixel,
            nv12_to_bgra_pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            nv12_to_bgra_shader_module,
            compute_command_buffer,
            compute_fence,
        })
    }

    /// Convert pixel data from source buffer to destination buffer via GPU compute.
    ///
    /// Supports NV12 → RGBA/BGRA conversion for decoded video display.
    pub fn convert(&self, source: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        let src_ref = source.buffer_ref();
        let dst_ref = dest.buffer_ref();
        let src_vk = &src_ref.inner;
        let dst_vk = &dst_ref.inner;

        let width = source.width;
        let height = source.height;
        let src_format = src_vk.format();
        let dst_format = dst_vk.format();

        if width != dest.width || height != dest.height {
            return Err(StreamError::GpuError(
                "Source and destination buffers must have the same dimensions".into(),
            ));
        }

        // NV12 → RGBA/BGRA
        let (pipeline, flags) = match (src_format, dst_format) {
            (
                PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange,
                PixelFormat::Rgba32 | PixelFormat::Bgra32,
            ) => {
                let is_bgra = matches!(dst_format, PixelFormat::Bgra32);
                let full_range = matches!(src_format, PixelFormat::Nv12FullRange);
                let flags = (is_bgra as u32) | ((full_range as u32) << 1);
                (self.nv12_to_bgra_pipeline, flags)
            }
            _ => {
                return Err(StreamError::NotSupported(format!(
                    "Unsupported format conversion: {:?} → {:?}",
                    src_format, dst_format
                )));
            }
        };

        let src_buffer = src_vk.buffer();
        let dst_buffer = dst_vk.buffer();
        let src_size = src_vk.size();
        let dst_size = dst_vk.size();

        // Wait for previous compute dispatch to finish before re-recording the command buffer.
        // This is essential — without it, we'd reset a command buffer that's still executing.
        unsafe {
            self.device
                .wait_for_fences(&[self.compute_fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to wait for compute fence: {e}"))
                })?;
            self.device
                .reset_fences(&[self.compute_fence])
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset compute fence: {e}"))
                })?;
        }

        // Update descriptor set to bind source and destination buffers
        let src_buffer_info = vk::DescriptorBufferInfo::builder()
            .buffer(src_buffer)
            .offset(0)
            .range(src_size)
            .build();
        let src_buffer_infos = [src_buffer_info];

        let dst_buffer_info = vk::DescriptorBufferInfo::builder()
            .buffer(dst_buffer)
            .offset(0)
            .range(dst_size)
            .build();
        let dst_buffer_infos = [dst_buffer_info];

        let descriptor_writes = [
            vk::WriteDescriptorSet::builder()
                .dst_set(self.descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&src_buffer_infos)
                .build(),
            vk::WriteDescriptorSet::builder()
                .dst_set(self.descriptor_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&dst_buffer_infos)
                .build(),
        ];

        unsafe {
            self.device
                .update_descriptor_sets(&descriptor_writes, &[] as &[vk::CopyDescriptorSet]);
        }

        // Record command buffer
        unsafe {
            self.device
                .reset_command_buffer(
                    self.compute_command_buffer,
                    vk::CommandBufferResetFlags::empty(),
                )
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to reset command buffer: {e}"))
                })?;

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();

            self.device
                .begin_command_buffer(self.compute_command_buffer, &begin_info)
                .map(|_| ())
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to begin command buffer: {e}"))
                })?;

            // Bind pipeline and descriptor set
            self.device.cmd_bind_pipeline(
                self.compute_command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                pipeline,
            );

            self.device.cmd_bind_descriptor_sets(
                self.compute_command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );

            // Push constants: width, height, flags
            let push_data = [width, height, flags];
            let push_bytes: &[u8] = std::slice::from_raw_parts(
                push_data.as_ptr() as *const u8,
                std::mem::size_of_val(&push_data),
            );
            self.device.cmd_push_constants(
                self.compute_command_buffer,
                self.pipeline_layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                push_bytes,
            );

            // NV12→BGRA shader processes 1 pixel per thread, 16×16 workgroups
            let dispatch_x = (width + 15) / 16;
            let dispatch_y = (height + 15) / 16;
            self.device
                .cmd_dispatch(self.compute_command_buffer, dispatch_x, dispatch_y, 1);

            self.device
                .end_command_buffer(self.compute_command_buffer)
                .map(|_| ())
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to end command buffer: {e}"))
                })?;

            // Submit and wait for completion
            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.compute_command_buffer)
                .build();
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&[cmd_info])
                .build();

            self.device
                .queue_submit2(self.queue, &[submit], self.compute_fence)
                .map(|_| ())
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to submit compute dispatch: {e}"))
                })?;

            // Wait for the compute dispatch to complete, ensuring the output
            // buffer data is visible before the caller submits dependent work.
            self.device
                .wait_for_fences(&[self.compute_fence], true, u64::MAX)
                .map(|_| ())
                .map_err(|e| {
                    StreamError::GpuError(format!("Failed to wait for compute fence: {e}"))
                })?;
        }

        Ok(())
    }

    /// Source format bytes per pixel.
    pub fn source_bytes_per_pixel(&self) -> u32 {
        self.source_bytes_per_pixel
    }

    /// Destination format bytes per pixel.
    pub fn dest_bytes_per_pixel(&self) -> u32 {
        self.dest_bytes_per_pixel
    }
}

impl Drop for VulkanFormatConverter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_fence(self.compute_fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_pipeline(self.nv12_to_bgra_pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            self.device
                .destroy_shader_module(self.nv12_to_bgra_shader_module, None);
        }
    }
}

// Safety: Vulkan handles are thread-safe
unsafe impl Send for VulkanFormatConverter {}
unsafe impl Sync for VulkanFormatConverter {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::VulkanDevice;

    #[test]
    fn test_new_creates_compute_pipeline_successfully() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        // NV12 source (1.5 bytes/pixel average) → BGRA32 destination (4 bytes/pixel).
        // This exercises the full vulkanalia builder chain: shader module creation,
        // descriptor set layout, pipeline layout, compute pipeline, and command pool —
        // validating the ash → vulkanalia migration of the most complex RHI file.
        let result = VulkanFormatConverter::new(
            device.device(),
            device.queue(),
            device.queue_family_index(),
            2, // source: NV12 packed as 2 bytes/pixel for dispatch sizing
            4, // dest: BGRA32
        );

        assert!(
            result.is_ok(),
            "VulkanFormatConverter::new must succeed: {:?}",
            result.err()
        );
    }
}
