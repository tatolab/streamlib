// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The adapter's persistent command pool, command buffer and completion fence.

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

use streamlib_surface_adapter::AdapterError;
#[cfg(target_os = "linux")]
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

/// Persistent per-adapter command pool, command buffer, and
/// completion fence — replaces the create-and-destroy-per-submit
/// pattern that used to churn `vkCreateCommandPool` /
/// `vkDestroyCommandPool` on every host-pipeline copy. Same shape
/// lives in
/// `streamlib-adapter-cpu-readback::adapter::AdapterPersistentSubmitContext`
/// and `streamlib-adapter-vulkan::adapter::AdapterPersistentSubmitContext`;
/// fix ALL THREE if you change ANY: `streamlib-surface-adapter` deliberately does not depend on
/// `vulkanalia`, so duplication is the project pattern here).
///
/// The fence is created signaled so the first submit doesn't block.
/// Subsequent submits wait on it (instant when the prior submit has
/// already drained, which is the steady state). `vkResetCommandPool`
/// is the cheap path per Vulkan spec — recycles every command
/// buffer's memory in one call.
#[cfg(target_os = "linux")]
pub(super) struct AdapterPersistentSubmitContext {
    pool: vk::CommandPool,
    pub(super) cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Whether the fence is signaled or a queued batch will signal it.
    /// False from the reset until a submit carrying the fence queues, so a
    /// recording or submit that failed in between leaves nothing to wait
    /// for instead of a fence nothing will ever signal.
    the_fence_will_signal: AtomicBool,
}

#[cfg(target_os = "linux")]
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

        Ok(Self {
            pool,
            cmd,
            fence,
            the_fence_will_signal: AtomicBool::new(true),
        })
    }

    /// Wait for the previous submit's fence, reset it, then reset the
    /// command pool so the single command buffer is ready to be
    /// re-recorded. Steady-state cost is the wait, which is instant
    /// when the prior submit has already drained; after a recording or
    /// submit that failed, nothing is queued and nothing is waited for.
    pub(super) fn reset_for_recording(
        &self,
        device: &vulkanalia::Device,
    ) -> Result<(), AdapterError> {
        if self.the_fence_will_signal.load(Ordering::Relaxed) {
            unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) }.map_err(|e| {
                AdapterError::BackendRejected {
                    reason: format!("wait_for_fences (persistent submit fence): {e}"),
                }
            })?;
        }
        unsafe { device.reset_fences(&[self.fence]) }.map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("reset_fences (persistent submit fence): {e}"),
            }
        })?;
        self.the_fence_will_signal.store(false, Ordering::Relaxed);
        unsafe { device.reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty()) }
            .map_err(|e| AdapterError::BackendRejected {
                reason: format!("reset_command_pool (persistent submit pool): {e}"),
            })?;
        Ok(())
    }

    /// Run `submit` with the fence as its completion signal, and once it
    /// has queued, have the next reset and the teardown wait for it.
    pub(super) fn submit_carrying_the_fence<E>(
        &self,
        submit: impl FnOnce(vk::Fence) -> Result<(), E>,
    ) -> Result<(), E> {
        submit(self.fence)?;
        self.the_fence_will_signal.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Tear down the pool and fence, first waiting for a queued submit to
    /// drain.
    pub(super) fn destroy(self, device: &vulkanalia::Device) {
        if self.the_fence_will_signal.load(Ordering::Relaxed) {
            let _ = unsafe { device.wait_for_fences(&[self.fence], true, u64::MAX) };
        }
        unsafe {
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
    }
}
