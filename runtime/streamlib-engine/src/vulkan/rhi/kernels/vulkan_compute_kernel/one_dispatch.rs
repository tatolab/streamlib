// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One dispatch's own bindings, staged apart from every other dispatch on the
//! same kernel.

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::Result;
use crate::core::rhi::Texture;

use super::{PendingState, VulkanComputeKernelInner};

/// The bindings and push constants of one dispatch, staged apart from the
/// kernel's own, so threads sharing a kernel never bind into each other's
/// dispatch. Taken with [`super::VulkanComputeKernel::begin_dispatch`].
///
/// The kernel runs [`COMPUTE_DISPATCH_SLOTS`](super::COMPUTE_DISPATCH_SLOTS)
/// dispatches at once; a dispatch past that waits for a slot to finish.
pub struct ComputeKernelDispatch {
    kernel: Arc<VulkanComputeKernelInner>,
    staged: PendingState,
}

impl ComputeKernelDispatch {
    pub(super) fn of(kernel: Arc<VulkanComputeKernelInner>) -> Self {
        Self {
            kernel,
            staged: PendingState {
                bindings: HashMap::new(),
                push_constants: Vec::new(),
            },
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
}
