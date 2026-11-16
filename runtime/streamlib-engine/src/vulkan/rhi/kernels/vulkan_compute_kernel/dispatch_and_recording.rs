// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::ComputeBindingKind;
use crate::core::{Error, Result};

use super::{BindingResource, PendingState, VulkanComputeKernelInner};

impl VulkanComputeKernelInner {
    /// Run the kernel: write all staged descriptors, record bind+push+dispatch,
    /// submit to the device queue, and wait on the kernel fence before returning.
    ///
    /// Every binding declared at construction must have been set since the last
    /// dispatch (unset bindings are an error — Vulkan's behavior in that case
    /// is undefined, so we refuse to dispatch).
    pub(crate) fn dispatch(
        &self,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        // Drain + validate up-front so concurrent set_* calls during the
        // dispatch don't leak into the next one.
        let pending = self.drain_and_validate_pending()?;

        // Wait for prior dispatch (if any) to drain so the command buffer +
        // descriptor set are safe to mutate.
        self.vulkan_device.wait_for_fences_blocking_counted(
            &[self.fence],
            format_args!(
                "Compute kernel '{}' draining its prior dispatch",
                self.label
            ),
        )?;
        unsafe {
            self.device
                .reset_fences(&[self.fence])
                .map_err(|e| Error::GpuError(format!("Failed to reset compute fence: {e}")))?;

            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| Error::GpuError(format!("Failed to reset command buffer: {e}")))?;

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(self.command_buffer, &begin_info)
                .map_err(|e| Error::GpuError(format!("Failed to begin command buffer: {e}")))?;
        }

        self.record_inner(
            self.command_buffer,
            &pending,
            group_count_x,
            group_count_y,
            group_count_z,
        )?;

        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| Error::GpuError(format!("Failed to end command buffer: {e}")))?;

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();

            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], self.fence)?;
        }

        self.vulkan_device.wait_for_fences_blocking_counted(
            &[self.fence],
            format_args!("Compute kernel '{}' awaiting its dispatch", self.label),
        )
    }

    /// Record bind + push-constants + dispatch into a caller-owned
    /// command buffer (already in the `Recording` state via
    /// `vkBeginCommandBuffer`). Does **not** submit or wait; the caller
    /// is responsible for surrounding `end`/`submit`/sync.
    ///
    /// Drains the kernel's pending `set_*` state on entry, same as
    /// [`Self::dispatch`]. Validates every declared binding has been
    /// set; mismatches surface as [`Error::GpuError`] before any GPU
    /// recording happens.
    ///
    /// **Caller contract:** no concurrent `record` or `dispatch` on
    /// this kernel may be in flight — the kernel's descriptor set is
    /// shared across calls and Vulkan disallows updating an in-use
    /// descriptor set. For per-frame use, the recorder's own
    /// timeline-semaphore wait between frames satisfies this.
    pub(crate) fn record(
        &self,
        command_buffer: vk::CommandBuffer,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        let pending = self.drain_and_validate_pending()?;
        self.record_inner(
            command_buffer,
            &pending,
            group_count_x,
            group_count_y,
            group_count_z,
        )
    }

    fn drain_and_validate_pending(&self) -> Result<PendingState> {
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
                    "Compute kernel '{}': binding {} ({:?}) not set before dispatch",
                    self.label, spec.binding, spec.kind
                )));
            }
        }
        if self.push_constant_size > 0 && pending.push_constants.is_empty() {
            return Err(Error::GpuError(format!(
                "Compute kernel '{}': push constants not set before dispatch",
                self.label
            )));
        }

        Ok(pending)
    }

    fn record_inner(
        &self,
        command_buffer: vk::CommandBuffer,
        pending: &PendingState,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        self.flush_descriptor_writes(pending)?;

        unsafe {
            self.device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );

            self.device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );

            if self.push_constant_size > 0 {
                self.device.cmd_push_constants(
                    command_buffer,
                    self.pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    &pending.push_constants,
                );
            }

            self.device
                .cmd_dispatch(command_buffer, group_count_x, group_count_y, group_count_z);
        }

        Ok(())
    }

    fn flush_descriptor_writes(&self, pending: &PendingState) -> Result<()> {
        let mut buffer_infos: Vec<vk::DescriptorBufferInfo> =
            Vec::with_capacity(self.bindings.len());
        let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::with_capacity(self.bindings.len());
        let mut writes: Vec<vk::WriteDescriptorSet> = Vec::with_capacity(self.bindings.len());

        // Build the *_info vectors first so all slot pointers are stable, then
        // build writes that reference them by index.
        struct Slot {
            binding: u32,
            ty: vk::DescriptorType,
            buffer_idx: Option<usize>,
            image_idx: Option<usize>,
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(self.bindings.len());

        for spec in &self.bindings {
            let res = pending.bindings.get(&spec.binding).expect("checked above");
            match (spec.kind, res) {
                (
                    ComputeBindingKind::StorageBuffer,
                    BindingResource::Buffer {
                        buffer,
                        offset,
                        range,
                    },
                ) => {
                    let idx = buffer_infos.len();
                    buffer_infos.push(
                        vk::DescriptorBufferInfo::builder()
                            .buffer(*buffer)
                            .offset(*offset)
                            .range(*range)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::STORAGE_BUFFER,
                        buffer_idx: Some(idx),
                        image_idx: None,
                    });
                }
                (
                    ComputeBindingKind::UniformBuffer,
                    BindingResource::Buffer {
                        buffer,
                        offset,
                        range,
                    },
                ) => {
                    let idx = buffer_infos.len();
                    buffer_infos.push(
                        vk::DescriptorBufferInfo::builder()
                            .buffer(*buffer)
                            .offset(*offset)
                            .range(*range)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::UNIFORM_BUFFER,
                        buffer_idx: Some(idx),
                        image_idx: None,
                    });
                }
                (
                    ComputeBindingKind::SampledTexture,
                    BindingResource::SampledImage { view, sampler },
                ) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .sampler(*sampler)
                            .image_view(*view)
                            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                        buffer_idx: None,
                        image_idx: Some(idx),
                    });
                }
                (
                    ComputeBindingKind::SampledImage,
                    BindingResource::SampledImageOnly { view, layout },
                ) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .image_view(*view)
                            .image_layout(layout.as_vk())
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::SAMPLED_IMAGE,
                        buffer_idx: None,
                        image_idx: Some(idx),
                    });
                }
                (ComputeBindingKind::StorageImage, BindingResource::StorageImage { view }) => {
                    let idx = image_infos.len();
                    image_infos.push(
                        vk::DescriptorImageInfo::builder()
                            .image_view(*view)
                            .image_layout(vk::ImageLayout::GENERAL)
                            .build(),
                    );
                    slots.push(Slot {
                        binding: spec.binding,
                        ty: vk::DescriptorType::STORAGE_IMAGE,
                        buffer_idx: None,
                        image_idx: Some(idx),
                    });
                }
                _ => {
                    return Err(Error::GpuError(format!(
                        "Compute kernel '{}': binding {} kind/resource mismatch (declared {:?})",
                        self.label, spec.binding, spec.kind
                    )));
                }
            }
        }

        for slot in &slots {
            let mut write = vk::WriteDescriptorSet::builder()
                .dst_set(self.descriptor_set)
                .dst_binding(slot.binding)
                .descriptor_type(slot.ty);
            if let Some(i) = slot.buffer_idx {
                write = write.buffer_info(std::slice::from_ref(&buffer_infos[i]));
            }
            if let Some(i) = slot.image_idx {
                write = write.image_info(std::slice::from_ref(&image_infos[i]));
            }
            writes.push(write.build());
        }

        unsafe {
            self.device
                .update_descriptor_sets(&writes, &[] as &[vk::CopyDescriptorSet]);
        }
        Ok(())
    }
}
