// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan synchronization primitives and Metal interop.

use ash::vk;

use crate::core::{Result, StreamError};

/// Vulkan semaphore wrapper for synchronization.
///
/// Can be created standalone or imported from a Metal shared event
/// for cross-API synchronization.
#[allow(dead_code)]
pub struct VulkanSemaphore {
    device: ash::Device,
    semaphore: vk::Semaphore,
    /// Whether this was imported from Metal (affects cleanup)
    #[allow(dead_code)]
    imported_from_metal: bool,
}

#[allow(dead_code)]
impl VulkanSemaphore {
    /// Create a new Vulkan semaphore.
    pub fn new(device: &ash::Device) -> Result<Self> {
        let semaphore_info = vk::SemaphoreCreateInfo::default();

        let semaphore = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create semaphore: {e}")))?;

        Ok(Self {
            device: device.clone(),
            semaphore,
            imported_from_metal: false,
        })
    }

    /// Import a Vulkan semaphore from a Metal shared event.
    ///
    /// This enables cross-API synchronization: Metal can signal the event,
    /// and Vulkan can wait on the semaphore (or vice versa).
    ///
    /// # Arguments
    /// * `device` - The Vulkan device
    /// * `mtl_shared_event` - Raw pointer to MTLSharedEvent (id<MTLSharedEvent>)
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn from_metal_shared_event(
        device: &ash::Device,
        mtl_shared_event: *const std::ffi::c_void,
    ) -> Result<Self> {
        if mtl_shared_event.is_null() {
            return Err(StreamError::GpuError(
                "Cannot import null MTLSharedEvent".into(),
            ));
        }

        // Create import info for Metal shared event
        let import_info = vk::ImportMetalSharedEventInfoEXT {
            mtl_shared_event: mtl_shared_event as vk::MTLSharedEvent_id,
            ..Default::default()
        };

        // Create semaphore with import info in pNext chain
        let semaphore_info = vk::SemaphoreCreateInfo {
            p_next: &import_info as *const _ as *const _,
            ..Default::default()
        };

        let semaphore = unsafe { device.create_semaphore(&semaphore_info, None) }.map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to create semaphore from MTLSharedEvent: {e}"
            ))
        })?;

        tracing::debug!("Imported MTLSharedEvent as Vulkan semaphore");

        Ok(Self {
            device: device.clone(),
            semaphore,
            imported_from_metal: true,
        })
    }

    /// Get the underlying Vulkan semaphore handle.
    pub fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }
}

impl Drop for VulkanSemaphore {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_semaphore(self.semaphore, None);
        }
    }
}

// VulkanSemaphore is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for VulkanSemaphore {}
unsafe impl Sync for VulkanSemaphore {}

/// Vulkan fence wrapper for CPU-GPU synchronization.
#[allow(dead_code)]
pub struct VulkanFence {
    device: ash::Device,
    fence: vk::Fence,
}

#[allow(dead_code)]
impl VulkanFence {
    /// Create a new Vulkan fence.
    ///
    /// # Arguments
    /// * `device` - The Vulkan device
    /// * `signaled` - Whether to create the fence in signaled state
    pub fn new(device: &ash::Device, signaled: bool) -> Result<Self> {
        let flags = if signaled {
            vk::FenceCreateFlags::SIGNALED
        } else {
            vk::FenceCreateFlags::empty()
        };

        let fence_info = vk::FenceCreateInfo::default().flags(flags);

        let fence = unsafe { device.create_fence(&fence_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create fence: {e}")))?;

        Ok(Self {
            device: device.clone(),
            fence,
        })
    }

    /// Wait for the fence to be signaled.
    ///
    /// # Arguments
    /// * `timeout_ns` - Timeout in nanoseconds (u64::MAX for no timeout)
    pub fn wait(&self, timeout_ns: u64) -> Result<()> {
        unsafe { self.device.wait_for_fences(&[self.fence], true, timeout_ns) }
            .map_err(|e| StreamError::GpuError(format!("Failed to wait for fence: {e}")))
    }

    /// Reset the fence to unsignaled state.
    pub fn reset(&self) -> Result<()> {
        unsafe { self.device.reset_fences(&[self.fence]) }
            .map_err(|e| StreamError::GpuError(format!("Failed to reset fence: {e}")))
    }

    /// Get the underlying Vulkan fence handle.
    pub fn fence(&self) -> vk::Fence {
        self.fence
    }
}

impl Drop for VulkanFence {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_fence(self.fence, None);
        }
    }
}

// VulkanFence is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for VulkanFence {}
unsafe impl Sync for VulkanFence {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::VulkanDevice;

    #[test]
    fn test_semaphore_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };

        let semaphore = VulkanSemaphore::new(device.device());
        assert!(semaphore.is_ok(), "Semaphore creation should succeed");
        println!("Vulkan semaphore created successfully");
    }

    #[test]
    fn test_fence_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };

        // Test unsignaled fence
        let fence = VulkanFence::new(device.device(), false);
        assert!(fence.is_ok(), "Fence creation should succeed");

        // Test signaled fence
        let signaled_fence = VulkanFence::new(device.device(), true);
        assert!(
            signaled_fence.is_ok(),
            "Signaled fence creation should succeed"
        );

        // Wait on signaled fence should return immediately
        let fence = signaled_fence.unwrap();
        let result = fence.wait(0);
        assert!(result.is_ok(), "Wait on signaled fence should succeed");

        println!("Vulkan fence tests passed");
    }
}
