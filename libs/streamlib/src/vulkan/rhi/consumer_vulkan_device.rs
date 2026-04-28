// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side Vulkan device — sandboxed subset of the RHI for
//! subprocess (cdylib) use.
//!
//! The consumer-side device deliberately *does not* duplicate
//! [`crate::vulkan::rhi::VulkanDevice`]. Where the host device owns
//! the privileged state — VMA allocator + DMA-BUF export pools, DRM
//! modifier probe, transfer / encode / decode / compute queues, the
//! per-queue submit-mutex matrix, swapchain extensions — the consumer
//! device holds only what the carve-out in
//! `docs/architecture/subprocess-rhi-parity.md` permits:
//!
//! - DMA-BUF FD import (`vkAllocateMemory` with `VkImportMemoryFdInfoKHR`).
//! - Tiled-image import via `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`
//!   (the modifier comes pre-chosen from the host's descriptor).
//! - Layout transitions on imported handles, single-shot at the
//!   acquire/release boundary — submitted via the device's one queue.
//! - `vkMapMemory` / `vkUnmapMemory` on imported memory.
//! - `vkWaitSemaphores` / `vkSignalSemaphore` on imported timeline
//!   semaphores (lives on
//!   [`crate::vulkan::rhi::ConsumerVulkanTimelineSemaphore`]).
//!
//! Crucially, this struct is constructed via [`Self::new`], which spins
//! up its *own* `VkInstance` + `VkDevice`. It does **not** share the
//! host's logical device — that's load-bearing for the
//! capability-boundary story (the cdylib never holds a reference to
//! the host's `vulkanalia::Device`). The two devices target the same
//! physical GPU but operate on disjoint Vulkan state objects, which
//! also sidesteps the dual-`VkDevice` crash on NVIDIA Linux as long as
//! their queue submissions don't overlap (the carve-out guarantees the
//! consumer only submits at acquire/release boundaries while the host
//! is paused).

use std::ffi::{c_char, CStr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use vulkanalia::loader::{LibloadingLoader, LIBRARY};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Result, StreamError};

use super::{ConsumerMarker, VulkanRhiDevice};

/// Consumer-only Vulkan device — see module docs.
pub struct ConsumerVulkanDevice {
    entry: vulkanalia::Entry,
    instance: vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    #[allow(dead_code)]
    device_name: String,
    /// Live count of memory allocations made via [`Self::import_dma_buf_memory`]
    /// (raw `vkAllocateMemory`). Mirrors the host counterpart; surfaces leaks
    /// on drop via the warning emitted in [`Drop`].
    live_allocation_count: AtomicUsize,
    /// Per-queue mutex serializing `vkQueueSubmit2` calls — Vulkan requires
    /// external synchronization on the same `VkQueue` from multiple threads.
    /// Single mutex is enough since the consumer device holds only one
    /// queue.
    queue_mutex: Mutex<()>,
}

impl ConsumerVulkanDevice {
    /// Construct a fresh consumer-only Vulkan device targeting the
    /// system's preferred discrete GPU.
    ///
    /// Enables only the extensions / features the carve-out needs:
    /// `VK_KHR_external_memory{,_fd}`, `VK_EXT_external_memory_dma_buf`,
    /// `VK_EXT_image_drm_format_modifier`,
    /// `VK_KHR_external_semaphore_fd`, plus the Vulkan 1.3 sync-2 and
    /// timeline-semaphore features. Every requested device extension
    /// is *required* — failure surfaces as
    /// [`StreamError::GpuError`] rather than a silent capability
    /// downgrade, so cdylib code never tries to import a render-target
    /// modifier on a driver that doesn't expose the extension.
    pub fn new() -> Result<Self> {
        let loader = unsafe { LibloadingLoader::new(LIBRARY) }
            .map_err(|e| StreamError::GpuError(format!("Failed to load Vulkan library: {e}")))?;
        let entry = unsafe { vulkanalia::Entry::new(loader) }
            .map_err(|e| StreamError::GpuError(format!("Failed to load Vulkan entry: {e}")))?;

        // Instance — minimal: just enough to enumerate physical devices.
        // No surface extensions; the consumer doesn't render to a window.
        let app_info = vk::ApplicationInfo::builder()
            .application_name(b"StreamLibConsumer\0")
            .application_version(vk::make_version(0, 1, 0))
            .engine_name(b"StreamLib\0")
            .engine_version(vk::make_version(0, 1, 0))
            .api_version(vk::make_version(1, 4, 0))
            .build();

        let instance_info = vk::InstanceCreateInfo::builder()
            .application_info(&app_info)
            .build();

        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create Vulkan instance: {e}")))?;

        // Physical device — prefer DISCRETE_GPU, fall back to first available.
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| StreamError::GpuError(format!("Failed to enumerate physical devices: {e}")))?;
        if physical_devices.is_empty() {
            return Err(StreamError::GpuError("No Vulkan physical devices found".into()));
        }
        let physical_device = physical_devices
            .iter()
            .find(|&&pd| {
                let props = unsafe { instance.get_physical_device_properties(pd) };
                props.device_type == vk::PhysicalDeviceType::DISCRETE_GPU
            })
            .copied()
            .unwrap_or(physical_devices[0]);

        let device_props = unsafe { instance.get_physical_device_properties(physical_device) };
        let device_name =
            unsafe { CStr::from_ptr(device_props.device_name.as_ptr()) }.to_string_lossy().into_owned();

        // Pick any GRAPHICS-capable queue family (graphics implies transfer +
        // compute on every conformant Vulkan implementation, which covers
        // every layout transition + sync the carve-out needs).
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| props.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(idx, _)| idx as u32)
            .ok_or_else(|| StreamError::GpuError("No graphics queue family on consumer device".into()))?;

        // Required device extensions. All four are mandatory on the
        // carve-out path — refusing to construct the device on a driver
        // that doesn't expose them is the right shape (see module docs).
        let available_device_ext_names: Vec<&CStr> = unsafe {
            instance
                .enumerate_device_extension_properties(physical_device, None)
                .map_err(|e| StreamError::GpuError(format!("enumerate_device_extension_properties: {e}")))?
        }
        .iter()
        .map(|ext| unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) })
        .collect();

        const REQUIRED: &[&CStr] = &[
            c"VK_KHR_external_memory",
            c"VK_KHR_external_memory_fd",
            c"VK_EXT_external_memory_dma_buf",
            c"VK_EXT_image_drm_format_modifier",
            c"VK_KHR_external_semaphore_fd",
        ];
        let mut device_extensions: Vec<*const c_char> = Vec::with_capacity(REQUIRED.len());
        for ext in REQUIRED {
            if !available_device_ext_names.contains(ext) {
                return Err(StreamError::GpuError(format!(
                    "ConsumerVulkanDevice: required extension {} not available on this driver",
                    ext.to_string_lossy()
                )));
            }
            device_extensions.push(ext.as_ptr());
        }

        // Logical device. Sync2 + timeline semaphore are core in 1.3 but
        // still need their feature flags enabled.
        let queue_priorities = [1.0f32];
        let queue_create_info = vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)
            .build();
        let queue_create_infos = [queue_create_info];

        let mut sync2_features = vk::PhysicalDeviceSynchronization2Features::builder()
            .synchronization2(true)
            .build();
        let mut timeline_features = vk::PhysicalDeviceTimelineSemaphoreFeatures::builder()
            .timeline_semaphore(true)
            .build();
        let mut dynamic_rendering_features =
            vk::PhysicalDeviceDynamicRenderingFeatures::builder()
                .dynamic_rendering(true)
                .build();
        // samplerYcbcrConversion: required to import NV12 textures on the
        // consumer side (multi-plane sampler-ycbcr support is core 1.1
        // but gated by the feature flag).
        let mut vulkan_1_1_features = vk::PhysicalDeviceVulkan11Features::builder()
            .sampler_ycbcr_conversion(true)
            .build();

        let device_create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .push_next(&mut sync2_features)
            .push_next(&mut timeline_features)
            .push_next(&mut dynamic_rendering_features)
            .push_next(&mut vulkan_1_1_features)
            .build();

        let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create consumer logical device: {e}")))?;

        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        tracing::info!(
            "ConsumerVulkanDevice initialized: {} (queue family {}, {} memory types)",
            device_name,
            queue_family_index,
            memory_properties.memory_type_count
        );

        Ok(Self {
            entry,
            instance,
            physical_device,
            memory_properties,
            device,
            queue,
            queue_family_index,
            device_name,
            live_allocation_count: AtomicUsize::new(0),
            queue_mutex: Mutex::new(()),
        })
    }

    /// Device label from `VkPhysicalDeviceProperties::deviceName`.
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        &self.device_name
    }

    /// Loaded Vulkan entry points.
    #[allow(dead_code)]
    pub fn entry(&self) -> &vulkanalia::Entry {
        &self.entry
    }

    /// Vulkan instance owned by this consumer device.
    #[allow(dead_code)]
    pub fn instance(&self) -> &vulkanalia::Instance {
        &self.instance
    }

    /// Selected physical device.
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    /// Logical Vulkan device — exposed for the carve-out's import +
    /// bind paths (creating an image / buffer, querying memory
    /// requirements, building command buffers for layout transitions).
    pub fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    /// Default submit queue for layout transitions + sync work.
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }

    /// Queue family that owns [`Self::queue`].
    pub fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    /// Find the first memory type whose bit is set in `type_filter` and
    /// satisfies `required_properties`.
    fn find_memory_type(
        &self,
        type_filter: u32,
        required_properties: vk::MemoryPropertyFlags,
    ) -> Result<u32> {
        if required_properties.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
            // Prefer pure DEVICE_LOCAL (main VRAM heap) over BAR aperture
            // when DEVICE_LOCAL is requested.
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                    && !flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
                {
                    return Ok(i);
                }
            }
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                {
                    return Ok(i);
                }
            }
        }
        for i in 0..self.memory_properties.memory_type_count {
            let flags = self.memory_properties.memory_types[i as usize].property_flags;
            if (type_filter & (1 << i)) != 0 && flags.contains(required_properties) {
                return Ok(i);
            }
        }
        Err(StreamError::GpuError(format!(
            "ConsumerVulkanDevice: no suitable memory type (filter=0x{:x}, required={:?})",
            type_filter, required_properties
        )))
    }

    /// Import a DMA-BUF file descriptor as `VkDeviceMemory`. Pairs with
    /// the host's `export_dma_buf_fd`: the host allocates, exports,
    /// and registers in surface-share; the consumer looks up, imports,
    /// and binds.
    ///
    /// fd ownership transfers to the Vulkan driver on success — caller
    /// must NOT close `fd` afterwards. On error the caller still owns
    /// `fd`.
    pub fn import_dma_buf_memory(
        &self,
        fd: i32,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
        preferred_flags: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory> {
        let memory_type_index = self.find_memory_type(memory_type_bits, preferred_flags)?;

        let mut import_info = vk::ImportMemoryFdInfoKHR::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd)
            .build();

        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info)
            .build();

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            StreamError::GpuError(format!(
                "ConsumerVulkanDevice: import_dma_buf_memory failed: {e}"
            ))
        })?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            "ConsumerVulkanDevice: imported DMA-BUF ({} bytes, type={}, live={})",
            allocation_size, memory_type_index, count
        );

        Ok(memory)
    }

    /// Free imported memory. Pair with [`Self::import_dma_buf_memory`].
    /// Calling on memory not allocated through this method is undefined
    /// behavior at the Vulkan level.
    pub fn free_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.free_memory(memory, None) };
        self.live_allocation_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Map imported device memory for CPU access. The returned pointer
    /// is valid until [`Self::unmap_imported_memory`] is called.
    pub fn map_imported_memory(
        &self,
        memory: vk::DeviceMemory,
        size: vk::DeviceSize,
    ) -> Result<*mut u8> {
        let ptr = unsafe {
            self.device
                .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "ConsumerVulkanDevice: map_imported_memory failed: {e}"
            ))
        })?;
        Ok(ptr as *mut u8)
    }

    /// Unmap a previously-mapped imported memory region.
    pub fn unmap_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.unmap_memory(memory) };
    }

    /// Submit command buffers under the per-queue mutex.
    ///
    /// # Safety
    /// Standard `vkQueueSubmit2` preconditions apply (valid `submits`,
    /// valid optional `fence`, no concurrent native submits to `queue`
    /// outside this method).
    pub unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        let _lock = self.queue_mutex.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.queue_submit2(queue, submits, fence) }
            .map(|_| ())
            .map_err(|e| StreamError::GpuError(format!("queue_submit2 failed: {e}")))
    }

    /// Current number of live DMA-BUF imports through this device.
    /// Surfaced for tracing + leak detection on Drop.
    #[allow(dead_code)]
    pub fn live_import_allocation_count(&self) -> usize {
        self.live_allocation_count.load(Ordering::Relaxed)
    }
}

impl VulkanRhiDevice for ConsumerVulkanDevice {
    type Privilege = ConsumerMarker;

    fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    fn queue(&self) -> vk::Queue {
        self.queue
    }

    fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        unsafe { ConsumerVulkanDevice::submit_to_queue(self, queue, submits, fence) }
    }
}

impl Drop for ConsumerVulkanDevice {
    fn drop(&mut self) {
        let live = self.live_allocation_count.load(Ordering::Relaxed);
        if live > 0 {
            tracing::warn!(
                "ConsumerVulkanDevice dropping with {} live DMA-BUF imports (leak)",
                live
            );
        }
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

// Vulkan handles are thread-safe behind external synchronization which
// the per-queue mutex provides for submits.
unsafe impl Send for ConsumerVulkanDevice {}
unsafe impl Sync for ConsumerVulkanDevice {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Try to construct a ConsumerVulkanDevice; return None if Vulkan
    /// is unavailable in this environment (CI sandboxes, no GPU).
    fn try_create() -> Option<Arc<ConsumerVulkanDevice>> {
        match ConsumerVulkanDevice::new() {
            Ok(d) => Some(Arc::new(d)),
            Err(e) => {
                println!("Skipping test — ConsumerVulkanDevice unavailable: {e}");
                None
            }
        }
    }

    #[test]
    fn consumer_device_constructs() {
        let device = match try_create() {
            Some(d) => d,
            None => return,
        };
        assert!(!device.name().is_empty());
        // Sanity: the trait bound is wired to ConsumerMarker, not Host.
        fn assert_consumer<D: VulkanRhiDevice<Privilege = ConsumerMarker>>(_: &D) {}
        assert_consumer(&*device);
    }

    #[test]
    fn consumer_device_exposes_queue() {
        let device = match try_create() {
            Some(d) => d,
            None => return,
        };
        let _q = device.queue();
        let _qfi = device.queue_family_index();
        // Trait method also returns the same queue.
        let _q_via_trait = <ConsumerVulkanDevice as VulkanRhiDevice>::queue(&device);
    }
}
