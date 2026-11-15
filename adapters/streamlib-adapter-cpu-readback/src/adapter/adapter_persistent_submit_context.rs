// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The command pool and fence the in-process copy trigger reuses on every submit.

use streamlib_surface_adapter::AdapterError;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

/// Persistent per-trigger / per-adapter command pool, command buffer,
/// and completion fence — replaces the create-and-destroy-per-submit
/// pattern that used to churn `vkCreateCommandPool` /
/// `vkDestroyCommandPool` once per copy. Same shape lives in
/// `streamlib-adapter-cuda::adapter::AdapterPersistentSubmitContext`
/// and `streamlib-adapter-vulkan::adapter::AdapterPersistentSubmitContext`;
/// fix ALL THREE if you change ANY (issue #620 + #640 AI Agent
/// Notes — `streamlib-surface-adapter` deliberately does not depend on
/// `vulkanalia`, so duplication is the project pattern here).
///
/// The fence is created signaled so the first submit doesn't block
/// waiting on a previous-submit completion. Subsequent submits wait
/// on the fence (instant if the prior submit has already drained,
/// which is the steady state for cpu-readback because the adapter
/// already CPU-waits on the timeline before the customer reads).
/// `vkResetCommandPool` is the cheap path per Vulkan spec — recycles
/// every command buffer's memory in one call.
pub(super) struct AdapterPersistentSubmitContext {
    pool: vk::CommandPool,
    pub(super) cmd: vk::CommandBuffer,
    pub(super) fence: vk::Fence,
}

impl AdapterPersistentSubmitContext {
    pub(super) fn new(device: &vulkanalia::Device, qf: u32) -> Result<Self, AdapterError> {
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(qf)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let pool = unsafe { device.create_command_pool(&pool_info, None) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("create_command_pool: {e}"),
            }
        })?;

        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cmd = match unsafe { device.allocate_command_buffers(&alloc_info) } {
            Ok(v) => v[0],
            Err(e) => {
                unsafe { device.destroy_command_pool(pool, None) };
                return Err(AdapterError::BackendRejected {
                    reason: format!("allocate_command_buffers: {e}"),
                });
            }
        };

        let fence_info = vk::FenceCreateInfo::builder()
            .flags(vk::FenceCreateFlags::SIGNALED)
            .build();
        let fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                unsafe { device.destroy_command_pool(pool, None) };
                return Err(AdapterError::BackendRejected {
                    reason: format!("create_fence: {e}"),
                });
            }
        };

        Ok(Self { pool, cmd, fence })
    }

    /// Wait for the previous submit's fence, reset it, then reset the
    /// command pool so the single command buffer is ready to be
    /// re-recorded. Steady-state cost is the wait, which is instant
    /// when the prior submit has already drained.
    pub(super) fn reset_for_recording(
        &self,
        device: &vulkanalia::Device,
    ) -> Result<(), AdapterError> {
        unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("wait_for_fences (persistent submit fence): {e}"),
            }
        })?;
        unsafe { device.reset_fences(&[self.fence]) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("reset_fences (persistent submit fence): {e}"),
            }
        })?;
        unsafe { device.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty()) }
            .map_err(|e| AdapterError::BackendRejected {
                reason: format!("reset_command_pool (persistent submit pool): {e}"),
            })?;
        Ok(())
    }

    /// Tear down the pool + fence. Caller must guarantee the fence is
    /// signaled (no GPU work pending) — `Drop` paths satisfy this by
    /// either waiting on the fence first or only destroying after a
    /// known-completed submit.
    pub(super) fn destroy(self, device: &vulkanalia::Device) {
        // Wait for any pending submit to drain so destruction is safe.
        let _ = unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) };
        unsafe {
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
    }
}
