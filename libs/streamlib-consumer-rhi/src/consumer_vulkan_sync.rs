// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side timeline semaphore — imports a host-allocated
//! exportable timeline semaphore via OPAQUE_FD and exposes the wait /
//! signal-from-host / counter-read operations.
//!
//! Mirrors [`crate::ConsumerVulkanTexture`] for sync primitives.
//! There is no `new` / `new_exportable` constructor: the consumer
//! never originates a timeline semaphore — it only imports one the
//! host already created.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrExternalSemaphoreFdExtensionDeviceCommands;

use crate::{ConsumerRhiError, ConsumerVulkanDevice, Result, VulkanTimelineSemaphoreLike};

/// Consumer-side timeline semaphore. See module docs.
pub struct ConsumerVulkanTimelineSemaphore {
    vulkan_device: Arc<ConsumerVulkanDevice>,
    semaphore: vk::Semaphore,
}

impl ConsumerVulkanTimelineSemaphore {
    /// Import a host-side exportable timeline semaphore via OPAQUE_FD.
    ///
    /// The consumer creates a fresh `VkSemaphore` against its own
    /// device, then `vkImportSemaphoreFdKHR` replaces the payload with
    /// the host's timeline state. After import, `wait` /
    /// `signal_host` / `current_value` operate against the same
    /// timeline as the host.
    ///
    /// fd ownership transfers to the Vulkan driver on success — caller
    /// must NOT close `fd` afterwards. On error the caller still owns
    /// it.
    pub fn from_imported_opaque_fd(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0)
            .build();
        let info = vk::SemaphoreCreateInfo::builder()
            .push_next(&mut type_info)
            .build();
        let semaphore = unsafe { device.create_semaphore(&info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanTimelineSemaphore: create_semaphore failed: {e}"
            ))
        })?;

        let import_info = vk::ImportSemaphoreFdInfoKHR::builder()
            .semaphore(semaphore)
            .flags(vk::SemaphoreImportFlags::empty())
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .fd(fd)
            .build();

        if let Err(e) = unsafe { device.import_semaphore_fd_khr(&import_info) } {
            unsafe { device.destroy_semaphore(semaphore, None) };
            return Err(ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanTimelineSemaphore: import_semaphore_fd_khr failed: {e}"
            )));
        }

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            semaphore,
        })
    }

    /// Block until the timeline counter has reached or surpassed
    /// `value`. `timeout_ns` of `u64::MAX` means "no timeout".
    pub fn wait(&self, value: u64, timeout_ns: u64) -> Result<()> {
        let semaphores = [self.semaphore];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::builder()
            .flags(vk::SemaphoreWaitFlags::empty())
            .semaphores(&semaphores)
            .values(&values)
            .build();
        unsafe { self.vulkan_device.device().wait_semaphores(&info, timeout_ns) }
            .map(|_| ())
            .map_err(|e| {
                ConsumerRhiError::Gpu(format!(
                    "wait_semaphores(value={value}, timeout_ns={timeout_ns}): {e}"
                ))
            })
    }

    /// Host-side signal: advance the counter to `value` directly from
    /// the CPU. `value` MUST be greater than the current counter.
    pub fn signal_host(&self, value: u64) -> Result<()> {
        let info = vk::SemaphoreSignalInfo::builder()
            .semaphore(self.semaphore)
            .value(value)
            .build();
        unsafe { self.vulkan_device.device().signal_semaphore(&info) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!("signal_semaphore(value={value}): {e}"))
        })
    }

    /// Read the timeline counter via `vkGetSemaphoreCounterValue`.
    pub fn current_value(&self) -> Result<u64> {
        unsafe { self.vulkan_device.device().get_semaphore_counter_value(self.semaphore) }.map_err(
            |e| ConsumerRhiError::Gpu(format!("get_semaphore_counter_value: {e}")),
        )
    }

    /// Raw `vk::Semaphore` handle for inclusion in queue submit infos.
    pub fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }
}

impl Drop for ConsumerVulkanTimelineSemaphore {
    fn drop(&mut self) {
        unsafe {
            self.vulkan_device
                .device()
                .destroy_semaphore(self.semaphore, None)
        };
    }
}

unsafe impl Send for ConsumerVulkanTimelineSemaphore {}
unsafe impl Sync for ConsumerVulkanTimelineSemaphore {}

impl VulkanTimelineSemaphoreLike for ConsumerVulkanTimelineSemaphore {
    fn wait(&self, value: u64, timeout_ns: u64) -> Result<()> {
        ConsumerVulkanTimelineSemaphore::wait(self, value, timeout_ns)
    }
    fn signal_host(&self, value: u64) -> Result<()> {
        ConsumerVulkanTimelineSemaphore::signal_host(self, value)
    }
}
