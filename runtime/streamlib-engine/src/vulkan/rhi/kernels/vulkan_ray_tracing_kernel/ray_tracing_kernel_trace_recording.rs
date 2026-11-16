// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Recording, submitting and waiting on a ray-tracing kernel's trace.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrRayTracingPipelineExtensionDeviceCommands as _;

use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;

use super::{PendingState, VulkanRayTracingKernelInner};

impl VulkanRayTracingKernelInner {
    /// Run the kernel: write all staged descriptors, record bind+push+
    /// trace_rays, submit to the device queue, and wait on the kernel
    /// fence before returning.
    pub(crate) fn trace_rays(&self, width: u32, height: u32, depth: u32) -> Result<()> {
        let pending = {
            let mut guard = self.pending.lock();
            PendingState {
                bindings: std::mem::take(&mut guard.bindings),
                push_constants: std::mem::take(&mut guard.push_constants),
            }
        };

        for spec in &self.bindings {
            if !pending.bindings.contains_key(&spec.binding) {
                return Err(Error::GpuError(format!(
                    "Ray-tracing kernel '{}': binding {} ({:?}) not set before trace_rays",
                    self.label, spec.binding, spec.kind
                )));
            }
        }
        if self.push_constant_size > 0 && pending.push_constants.is_empty() {
            return Err(Error::GpuError(format!(
                "Ray-tracing kernel '{}': push constants not set before trace_rays",
                self.label
            )));
        }

        unsafe {
            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "Ray-tracing kernel '{}': wait_for_fences failed: {e}",
                        self.label
                    ))
                })?;
            self.device.reset_fences(&[self.fence]).map_err(|e| {
                Error::GpuError(format!(
                    "Ray-tracing kernel '{}': reset_fences failed: {e}",
                    self.label
                ))
            })?;
        }

        self.flush_descriptor_writes(&pending)?;

        unsafe {
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    Error::GpuError(format!(
                        "Ray-tracing kernel '{}': reset_command_buffer failed: {e}",
                        self.label
                    ))
                })?;
            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "Ray-tracing kernel '{}': begin_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            self.device.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::RAY_TRACING_KHR,
                self.pipeline,
            );
            self.device.cmd_bind_descriptor_sets(
                self.command_buffer,
                vk::PipelineBindPoint::RAY_TRACING_KHR,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );
            if self.push_constant_size > 0 {
                self.device.cmd_push_constants(
                    self.command_buffer,
                    self.pipeline_layout,
                    self.push_constant_stages,
                    0,
                    &pending.push_constants,
                );
            }

            self.device.cmd_trace_rays_khr(
                self.command_buffer,
                &self.sbt.raygen_region,
                &self.sbt.miss_region,
                &self.sbt.hit_region,
                &self.sbt.callable_region,
                width,
                height,
                depth,
            );

            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "Ray-tracing kernel '{}': end_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();

            HostVulkanDevice::submit_to_queue(
                &self.vulkan_device,
                self.queue,
                &[submit],
                self.fence,
            )?;

            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map(|_| ())
                .map_err(|e| {
                    Error::GpuError(format!(
                        "Ray-tracing kernel '{}': wait_for_fences (post-submit) failed: {e}",
                        self.label
                    ))
                })?;
        }

        Ok(())
    }
}
