// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan synchronization primitives and Metal interop.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
#[cfg(target_os = "linux")]
use vulkanalia::vk::KhrExternalSemaphoreFdExtensionDeviceCommands;

use crate::core::{Result, StreamError};

/// Vulkan semaphore wrapper for synchronization.
///
/// Can be created standalone or imported from a Metal shared event
/// for cross-API synchronization.
#[allow(dead_code)]
pub struct VulkanSemaphore {
    device: vulkanalia::Device,
    semaphore: vk::Semaphore,
    /// Whether this was imported from Metal (affects cleanup)
    #[allow(dead_code)]
    imported_from_metal: bool,
}

#[allow(dead_code)]
impl VulkanSemaphore {
    /// Create a new Vulkan semaphore.
    pub fn new(device: &vulkanalia::Device) -> Result<Self> {
        let semaphore_info = vk::SemaphoreCreateInfo::builder().build();

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
        device: &vulkanalia::Device,
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
    device: vulkanalia::Device,
    fence: vk::Fence,
}

#[allow(dead_code)]
impl VulkanFence {
    /// Create a new Vulkan fence.
    ///
    /// # Arguments
    /// * `device` - The Vulkan device
    /// * `signaled` - Whether to create the fence in signaled state
    pub fn new(device: &vulkanalia::Device, signaled: bool) -> Result<Self> {
        let flags = if signaled {
            vk::FenceCreateFlags::SIGNALED
        } else {
            vk::FenceCreateFlags::empty()
        };

        let fence_info = vk::FenceCreateInfo::builder().flags(flags).build();

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
            .map(|_| ())
            .map_err(|e| StreamError::GpuError(format!("Failed to wait for fence: {e}")))
    }

    /// Reset the fence to unsignaled state.
    pub fn reset(&self) -> Result<()> {
        unsafe { self.device.reset_fences(&[self.fence]) }
            .map(|_| ())
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

/// Vulkan **timeline** semaphore wrapper.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[allow(dead_code)]
///
/// Timeline semaphores carry a monotonically-increasing 64-bit counter.
/// Submitters wait on a value and signal a higher value; the wait
/// completes when the counter has reached or surpassed the requested
/// value. This is the synchronization primitive used by surface
/// adapters: each per-surface acquire/release pair advances the counter.
///
/// Created with `VkSemaphoreTypeCreateInfo` chained into the standard
/// `VkSemaphoreCreateInfo`. Optionally created with
/// `VkExportSemaphoreCreateInfo` so [`Self::export_opaque_fd`] can hand a
/// file descriptor to a subprocess, which imports it via
/// [`Self::from_imported_opaque_fd`] into its own `VkDevice`. The two
/// processes then signal/wait the same timeline.
pub struct HostVulkanTimelineSemaphore {
    device: vulkanalia::Device,
    semaphore: vk::Semaphore,
    /// Whether the semaphore was created with VK_KHR_external_semaphore_fd
    /// export support — i.e. [`Self::export_opaque_fd`] is callable.
    exportable: bool,
}

#[cfg(target_os = "linux")]
impl HostVulkanTimelineSemaphore {
    /// Create an in-process timeline semaphore (no export).
    ///
    /// Pair with [`Self::wait`] / [`Self::signal_host`] / [`Self::signal_on_queue`]
    /// for single-process work. Use [`Self::new_exportable`] when the
    /// timeline must be shared with a subprocess via sync-fd.
    pub fn new(device: &vulkanalia::Device, initial_value: u64) -> Result<Self> {
        Self::create(device, initial_value, false)
    }

    /// Create an exportable timeline semaphore.
    ///
    /// `vkGetSemaphoreFdKHR` will hand a fresh OPAQUE_FD per
    /// [`Self::export_opaque_fd`] call; ownership transfers to the caller
    /// (close after use, or pass via SCM_RIGHTS).
    pub fn new_exportable(device: &vulkanalia::Device, initial_value: u64) -> Result<Self> {
        Self::create(device, initial_value, true)
    }

    fn create(
        device: &vulkanalia::Device,
        initial_value: u64,
        exportable: bool,
    ) -> Result<Self> {
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(initial_value)
            .build();

        let mut export_info = vk::ExportSemaphoreCreateInfo::builder()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .build();

        let info = if exportable {
            // Chain order: SemaphoreCreateInfo -> ExportSemaphoreCreateInfo -> SemaphoreTypeCreateInfo.
            // p_next is set manually to avoid moving the local `type_info`
            // into the builder's pNext (vulkanalia's builder takes &mut and
            // would borrow `type_info`).
            export_info.next = (&mut type_info as *mut _) as *mut std::ffi::c_void;
            vk::SemaphoreCreateInfo::builder()
                .push_next(&mut export_info)
                .build()
        } else {
            vk::SemaphoreCreateInfo::builder()
                .push_next(&mut type_info)
                .build()
        };

        let semaphore = unsafe { device.create_semaphore(&info, None) }.map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to create timeline semaphore (exportable={exportable}): {e}"
            ))
        })?;

        Ok(Self {
            device: device.clone(),
            semaphore,
            exportable,
        })
    }

    /// Import a timeline semaphore from an OPAQUE_FD handed in by the
    /// host. Subprocess side of [`Self::export_opaque_fd`].
    ///
    /// `VK_SEMAPHORE_IMPORT_TEMPORARY_BIT` is NOT used: the imported
    /// semaphore takes permanent payload ownership, matching how DMA-BUF
    /// memory imports are bound for surface lifetime.
    ///
    /// On success the kernel fd ownership transfers to the Vulkan driver;
    /// the caller MUST NOT close `fd` afterwards. On error the caller
    /// retains ownership and is responsible for closing it.
    pub fn from_imported_opaque_fd(
        device: &vulkanalia::Device,
        fd: std::os::unix::io::RawFd,
    ) -> Result<Self> {
        // The semaphore must already exist before import. Create it as a
        // timeline semaphore with initial value 0; the import then
        // replaces the payload with the host's timeline state.
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0)
            .build();
        let info = vk::SemaphoreCreateInfo::builder()
            .push_next(&mut type_info)
            .build();
        let semaphore = unsafe { device.create_semaphore(&info, None) }.map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to create receiving timeline semaphore for import: {e}"
            ))
        })?;

        let import_info = vk::ImportSemaphoreFdInfoKHR::builder()
            .semaphore(semaphore)
            .flags(vk::SemaphoreImportFlags::empty())
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .fd(fd)
            .build();

        let import_result = unsafe { device.import_semaphore_fd_khr(&import_info) };
        if let Err(e) = import_result {
            unsafe { device.destroy_semaphore(semaphore, None) };
            return Err(StreamError::GpuError(format!(
                "vkImportSemaphoreFdKHR failed: {e}"
            )));
        }

        Ok(Self {
            device: device.clone(),
            semaphore,
            exportable: false,
        })
    }

    /// Export the semaphore as a fresh OPAQUE_FD suitable for SCM_RIGHTS
    /// passing to a subprocess. Each call returns a NEW fd; callers own
    /// the returned fd and must close it after use (or after the
    /// subprocess has imported its own copy).
    pub fn export_opaque_fd(&self) -> Result<std::os::unix::io::RawFd> {
        if !self.exportable {
            return Err(StreamError::GpuError(
                "HostVulkanTimelineSemaphore::export_opaque_fd: semaphore was not created with `new_exportable`".into(),
            ));
        }
        let info = vk::SemaphoreGetFdInfoKHR::builder()
            .semaphore(self.semaphore)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .build();
        let fd = unsafe { self.device.get_semaphore_fd_khr(&info) }.map_err(|e| {
            StreamError::GpuError(format!("vkGetSemaphoreFdKHR failed: {e}"))
        })?;
        Ok(fd)
    }

    /// Block until the timeline counter has reached or surpassed `value`.
    ///
    /// `timeout_ns` is the per-call timeout; pass `u64::MAX` for "no
    /// timeout". Returns `Ok(())` on success and
    /// [`StreamError::GpuError`] (containing the underlying VkResult) on
    /// timeout or driver failure.
    pub fn wait(&self, value: u64, timeout_ns: u64) -> Result<()> {
        let semaphores = [self.semaphore];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::builder()
            .flags(vk::SemaphoreWaitFlags::empty())
            .semaphores(&semaphores)
            .values(&values)
            .build();
        unsafe { self.device.wait_semaphores(&info, timeout_ns) }
            .map(|_| ())
            .map_err(|e| {
                StreamError::GpuError(format!(
                    "vkWaitSemaphores(value={value}, timeout_ns={timeout_ns}): {e}"
                ))
            })
    }

    /// Host-side signal: advance the counter to `value` directly from
    /// the CPU. Used when the producer has finished writing on the host
    /// side and wants to release the surface to the next consumer.
    ///
    /// `value` MUST be greater than the current counter — Vulkan
    /// disallows monotonic regressions on a timeline semaphore.
    pub fn signal_host(&self, value: u64) -> Result<()> {
        let info = vk::SemaphoreSignalInfo::builder()
            .semaphore(self.semaphore)
            .value(value)
            .build();
        unsafe { self.device.signal_semaphore(&info) }.map_err(|e| {
            StreamError::GpuError(format!("vkSignalSemaphore(value={value}): {e}"))
        })
    }

    /// Read the current timeline counter value via
    /// `vkGetSemaphoreCounterValue`. Used by tests and progress reporting.
    pub fn current_value(&self) -> Result<u64> {
        unsafe { self.device.get_semaphore_counter_value(self.semaphore) }.map_err(|e| {
            StreamError::GpuError(format!("vkGetSemaphoreCounterValue: {e}"))
        })
    }

    /// Raw `vk::Semaphore` handle for inclusion in queue submit infos.
    pub fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }

    /// Whether [`Self::export_opaque_fd`] can be called.
    pub fn is_exportable(&self) -> bool {
        self.exportable
    }
}

#[cfg(target_os = "linux")]
impl Drop for HostVulkanTimelineSemaphore {
    fn drop(&mut self) {
        unsafe { self.device.destroy_semaphore(self.semaphore, None) };
    }
}

#[cfg(target_os = "linux")]
unsafe impl Send for HostVulkanTimelineSemaphore {}
#[cfg(target_os = "linux")]
unsafe impl Sync for HostVulkanTimelineSemaphore {}

#[cfg(target_os = "linux")]
impl super::VulkanTimelineSemaphoreLike for HostVulkanTimelineSemaphore {
    fn wait(&self, value: u64, timeout_ns: u64) -> streamlib_consumer_rhi::Result<()> {
        HostVulkanTimelineSemaphore::wait(self, value, timeout_ns)
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }
    fn signal_host(&self, value: u64) -> streamlib_consumer_rhi::Result<()> {
        HostVulkanTimelineSemaphore::signal_host(self, value)
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    #[test]
    fn test_semaphore_creation() {
        let device = match HostVulkanDevice::new() {
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

    #[cfg(target_os = "linux")]
    #[test]
    fn timeline_semaphore_host_signal_advances_counter() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };
        let sem = HostVulkanTimelineSemaphore::new(device.device(), 0)
            .expect("create timeline semaphore");
        assert_eq!(sem.current_value().unwrap(), 0);
        sem.signal_host(7).expect("host signal");
        assert_eq!(sem.current_value().unwrap(), 7);
        // wait on a value already reached returns immediately.
        sem.wait(7, 0).expect("wait on already-reached value");
    }

    /// `new_exportable` plus `export_opaque_fd` returns a valid kernel
    /// fd. Sufficient to confirm `VK_KHR_external_semaphore_fd` is wired.
    /// Cross-process import is exercised by the surface-adapter
    /// integration tests in `streamlib-adapter-vulkan`.
    #[cfg(target_os = "linux")]
    #[test]
    fn timeline_semaphore_exports_valid_opaque_fd() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };
        let sem = match HostVulkanTimelineSemaphore::new_exportable(device.device(), 0) {
            Ok(s) => s,
            Err(_) => {
                println!("Skipping — VK_KHR_external_semaphore_fd unavailable on this driver");
                return;
            }
        };
        let fd = sem.export_opaque_fd().expect("export_opaque_fd");
        assert!(fd >= 0, "exported sync fd should be a valid kernel fd");
        unsafe { libc::close(fd) };
    }

    #[test]
    fn test_fence_creation() {
        let device = match HostVulkanDevice::new() {
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
