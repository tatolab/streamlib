// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One dispatch's own bindings, staged apart from every other dispatch on the
//! same kernel.

use crate::core::Result;
use crate::core::rhi::Texture;
use crate::vulkan::rhi::SampledImageReadLayout;
use vulkanalia::vk;

use super::{PendingState, VulkanComputeKernelInner};

/// The bindings and push constants of one dispatch, staged apart from the
/// kernel's own, so threads sharing a kernel never bind into each other's
/// dispatch. Taken with [`super::VulkanComputeKernel::begin_dispatch`].
///
/// The kernel runs [`COMPUTE_DISPATCH_SLOTS`](super::COMPUTE_DISPATCH_SLOTS)
/// dispatches at once; a dispatch past that waits for a slot to finish.
#[must_use = "a dispatch runs nothing until it is dispatched or recorded"]
pub struct ComputeKernelDispatch<'kernel> {
    kernel: &'kernel VulkanComputeKernelInner,
    staged: PendingState,
}

impl<'kernel> ComputeKernelDispatch<'kernel> {
    pub(super) fn of(kernel: &'kernel VulkanComputeKernelInner) -> Self {
        Self {
            kernel,
            staged: PendingState::default(),
        }
    }

    /// Bind a [`crate::core::rhi::PixelBuffer`] as a storage buffer.
    pub fn set_storage_buffer_pixel(
        &mut self,
        binding: u32,
        buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<()> {
        self.kernel
            .stage_storage_buffer_from_byte_offset(&mut self.staged, binding, buffer, 0)
    }

    /// Bind a raw-bytes [`crate::core::rhi::StorageBuffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_storage_buffer_storage(
        &mut self,
        binding: u32,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<()> {
        self.kernel
            .stage_storage_buffer_from_byte_offset(&mut self.staged, binding, buffer, 0)
    }

    /// Bind a [`crate::core::rhi::UniformBuffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn set_uniform_buffer(
        &mut self,
        binding: u32,
        buffer: &crate::core::rhi::UniformBuffer,
    ) -> Result<()> {
        self.kernel
            .stage_uniform_buffer(&mut self.staged, binding, buffer)
    }

    pub fn set_sampled_texture(&mut self, binding: u32, texture: &Texture) -> Result<()> {
        self.kernel
            .stage_sampled_texture(&mut self.staged, binding, texture)
    }

    pub fn set_storage_image(&mut self, binding: u32, texture: &Texture) -> Result<()> {
        self.kernel
            .stage_storage_image(&mut self.staged, binding, texture)
    }

    /// Bind a raw `vk::ImageView` to a sampled-image slot, in `layout` at
    /// dispatch time.
    pub fn set_sampled_image_view(
        &mut self,
        binding: u32,
        view: vk::ImageView,
        layout: SampledImageReadLayout,
    ) -> Result<()> {
        self.kernel
            .stage_sampled_image_view(&mut self.staged, binding, view, layout)
    }

    /// Bind a raw `vk::ImageView` to a storage-image slot, in `GENERAL` at
    /// dispatch time.
    pub fn set_storage_image_view(&mut self, binding: u32, view: vk::ImageView) -> Result<()> {
        self.kernel
            .stage_storage_image_view(&mut self.staged, binding, view)
    }

    pub fn set_push_constants(&mut self, bytes: &[u8]) -> Result<()> {
        self.kernel.stage_push_constants(&mut self.staged, bytes)
    }

    pub fn set_push_constants_value<T: Copy>(&mut self, value: &T) -> Result<()> {
        self.kernel
            .stage_push_constants_value(&mut self.staged, value)
    }

    /// Run the dispatch on a free slot and wait for it.
    pub fn dispatch(
        self,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        self.kernel
            .dispatch_staged(self.staged, group_count_x, group_count_y, group_count_z)
    }

    /// Record the dispatch into `command_buffer` through the kernel's batch
    /// descriptor set `batch_descriptor_set`, without submitting.
    ///
    /// The caller keeps that set to this recording until its submission has
    /// retired: Vulkan disallows rewriting a descriptor set still in use.
    pub(crate) fn record_into_batch_descriptor_set(
        self,
        command_buffer: vk::CommandBuffer,
        batch_descriptor_set: usize,
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<()> {
        self.kernel.record_staged_into_batch_descriptor_set(
            command_buffer,
            batch_descriptor_set,
            &self.staged,
            group_count_x,
            group_count_y,
            group_count_z,
        )
    }
}
