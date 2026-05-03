// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side Vulkan device — sandboxed subset of the RHI for
//! subprocess (cdylib) use.
//!
//! The consumer-side device deliberately *does not* duplicate
//! `streamlib::vulkan::rhi::HostVulkanDevice`. Where the host device
//! owns the privileged state — VMA allocator + DMA-BUF export pools,
//! DRM modifier probe, transfer / encode / decode / compute queues,
//! the per-queue submit-mutex matrix, swapchain extensions — the
//! consumer device holds only what the carve-out in
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
//!   [`crate::ConsumerVulkanTimelineSemaphore`]).
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

use crate::{ConsumerMarker, ConsumerRhiError, Result, VulkanRhiDevice};

/// Single-COLOR-aspect / single-mip / single-layer subresource range —
/// every surface-adapter-managed image fits this shape today.
fn default_color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

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
    /// Whether `VK_EXT_external_memory_acquire_unmodified` was enabled
    /// at device creation. Lets the consumer's acquire barrier chain
    /// `VkExternalMemoryAcquireUnmodifiedEXT` so the producer's
    /// post-release contents survive the QFOT acquire (instead of
    /// being discarded by the spec's UNDEFINED equivalence). When
    /// false, fall back to UNDEFINED → target on acquire — content
    /// preservation is then driver-empirical, not spec-guaranteed.
    /// `VK_QUEUE_FAMILY_EXTERNAL` (the queue family index used for the
    /// QFOT release/acquire src/dst) is core Vulkan 1.1 and always
    /// available; only this acquire-side extension is the meaningful
    /// gate.
    has_acquire_unmodified: bool,
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
    /// [`ConsumerRhiError::Gpu`] rather than a silent capability
    /// downgrade, so cdylib code never tries to import a render-target
    /// modifier on a driver that doesn't expose the extension.
    pub fn new() -> Result<Self> {
        let loader = unsafe { LibloadingLoader::new(LIBRARY) }
            .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to load Vulkan library: {e}")))?;
        let entry = unsafe { vulkanalia::Entry::new(loader) }
            .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to load Vulkan entry: {e}")))?;

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
            .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to create Vulkan instance: {e}")))?;

        // Physical device — prefer DISCRETE_GPU, fall back to first available.
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to enumerate physical devices: {e}")))?;
        if physical_devices.is_empty() {
            return Err(ConsumerRhiError::Gpu("No Vulkan physical devices found".into()));
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
            .ok_or_else(|| ConsumerRhiError::Gpu("No graphics queue family on consumer device".into()))?;

        // Required device extensions. All four are mandatory on the
        // carve-out path — refusing to construct the device on a driver
        // that doesn't expose them is the right shape (see module docs).
        let available_device_ext_names: Vec<&CStr> = unsafe {
            instance
                .enumerate_device_extension_properties(physical_device, None)
                .map_err(|e| ConsumerRhiError::Gpu(format!("enumerate_device_extension_properties: {e}")))?
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
        let mut device_extensions: Vec<*const c_char> = Vec::with_capacity(REQUIRED.len() + 2);
        for ext in REQUIRED {
            if !available_device_ext_names.contains(ext) {
                return Err(ConsumerRhiError::Gpu(format!(
                    "ConsumerVulkanDevice: required extension {} not available on this driver",
                    ext.to_string_lossy()
                )));
            }
            device_extensions.push(ext.as_ptr());
        }

        // Optional extension for spec-correct cross-process layout
        // coordination (#633). `VK_EXT_external_memory_acquire_unmodified`
        // lets the consumer-side acquire barrier declare the import is
        // unmodified so contents survive the transfer
        // (spec-permits-discard otherwise). When absent, the helpers
        // fall back to bridging UNDEFINED → target (content
        // preservation then relies on driver-empirical DMA-BUF kernel
        // semantics, which hold on NVIDIA Linux). The queue family
        // index `VK_QUEUE_FAMILY_EXTERNAL` used in the QFOT
        // release/acquire is core Vulkan 1.1 and doesn't need an
        // optional probe.
        let acquire_unmodified_ext = c"VK_EXT_external_memory_acquire_unmodified";
        let has_acquire_unmodified = available_device_ext_names.contains(&acquire_unmodified_ext);
        if has_acquire_unmodified {
            device_extensions.push(acquire_unmodified_ext.as_ptr());
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
            .map_err(|e| ConsumerRhiError::Gpu(format!("Failed to create consumer logical device: {e}")))?;

        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        tracing::info!(
            "ConsumerVulkanDevice initialized: {} (queue family {}, {} memory types, acquire_unmodified={})",
            device_name,
            queue_family_index,
            memory_properties.memory_type_count,
            has_acquire_unmodified,
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
            has_acquire_unmodified,
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
        Err(ConsumerRhiError::Gpu(format!(
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
            ConsumerRhiError::Gpu(format!(
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

    /// Import an OPAQUE_FD file descriptor as `VkDeviceMemory`. Pairs with
    /// the host's `export_opaque_fd_memory`: the host allocates, exports,
    /// and registers in surface-share with `handle_type="opaque_fd"`; the
    /// consumer looks up, imports, and binds.
    ///
    /// Use this for cross-process Vulkan memory sharing where the importer
    /// is also Vulkan-aware (CUDA via UUID-matched device, OpenCL, peer
    /// VkInstance) and tile-aware DRM-modifier negotiation isn't needed.
    /// For DMA-BUF FDs (EGL, V4L2, multi-plane Vulkan importers) use
    /// [`Self::import_dma_buf_memory`].
    ///
    /// fd ownership transfers to the Vulkan driver on success — caller
    /// must NOT close `fd` afterwards. On error the caller still owns
    /// `fd`. Pairs with [`Self::free_imported_memory`].
    #[tracing::instrument(level = "trace", skip(self), fields(fd, allocation_size))]
    pub fn import_opaque_fd_memory(
        &self,
        fd: i32,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
        preferred_flags: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory> {
        let memory_type_index = self.find_memory_type(memory_type_bits, preferred_flags)?;

        let mut import_info = vk::ImportMemoryFdInfoKHR::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .fd(fd)
            .build();

        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info)
            .build();

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: import_opaque_fd_memory failed: {e}"
            ))
        })?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            "ConsumerVulkanDevice: imported OPAQUE_FD ({} bytes, type={}, live={})",
            allocation_size, memory_type_index, count
        );

        Ok(memory)
    }

    /// Free imported memory. Pair with [`Self::import_dma_buf_memory`] or
    /// [`Self::import_opaque_fd_memory`]. Calling on memory not allocated
    /// through one of those methods is undefined behavior at the Vulkan
    /// level.
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
            ConsumerRhiError::Gpu(format!(
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
            .map_err(|e| ConsumerRhiError::Gpu(format!("queue_submit2 failed: {e}")))
    }

    /// Whether this device supports content-preserving QFOT for
    /// cross-process layout coordination per #633: the optional
    /// `VK_EXT_external_memory_acquire_unmodified` extension was
    /// enabled at construction, letting the consumer-side acquire
    /// barrier chain `VkExternalMemoryAcquireUnmodifiedEXT` so the
    /// producer's contents survive the transfer.
    ///
    /// `VK_QUEUE_FAMILY_EXTERNAL` (the queue family index used for
    /// QFOT src/dst across this carve-out) is core Vulkan 1.1 and is
    /// always available; it is NOT a meaningful capability gate.
    /// Only the acquire-unmodified extension is.
    ///
    /// When `false`, [`Self::release_to_foreign`] and
    /// [`Self::acquire_from_foreign`] fall back to a regular
    /// same-family layout transition; content preservation across
    /// the import boundary then relies on driver-empirical DMA-BUF
    /// kernel semantics rather than the Vulkan spec. To the best of
    /// our current knowledge this fallback is structurally permanent
    /// on NVIDIA Linux (the extension isn't on NVIDIA's roadmap as of
    /// 2026-05-03); on Mesa drivers QFOT is the eventual landing
    /// point.
    pub fn supports_qfot_acquire_unmodified(&self) -> bool {
        self.has_acquire_unmodified
    }

    /// Producer-side QFOT release barrier — declares the surface's
    /// post-write `VkImageLayout` and transfers ownership to
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) so a foreign
    /// device (host or peer subprocess) can subsequently acquire it.
    /// Pair with [`Self::acquire_from_foreign`] on the consumer side.
    ///
    /// One-shot synchronous submit on the device's queue: the
    /// transition is the producer's last GPU work for this surface
    /// before publishing, so blocking on the fence is the simple
    /// correct shape (and matches the cross-process IPC granularity).
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `false`,
    /// falls back to a same-family layout transition — the IPC
    /// `update_image_layout` publish still happens at the call site,
    /// but content preservation across the consumer's import is
    /// driver-empirical.
    pub fn release_to_foreign(
        &self,
        image: vk::Image,
        src_layout: vk::ImageLayout,
        dst_layout: vk::ImageLayout,
    ) -> Result<()> {
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 (promoted from
        // VK_KHR_external_memory) — always available. The Khronos
        // proposal for `VK_EXT_external_memory_acquire_unmodified`
        // explicitly permits either EXTERNAL or FOREIGN_EXT as the
        // src/dst; for cross-process Vulkan-to-Vulkan handoff,
        // EXTERNAL is the idiomatic choice (FOREIGN_EXT is for
        // non-Vulkan foreign owners — OpenGL adapters in this codebase
        // don't issue Vulkan barriers from GL writes anyway).
        let barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::NONE)
            .dst_access_mask(vk::AccessFlags2::empty())
            .old_layout(src_layout)
            .new_layout(dst_layout)
            .src_queue_family_index(self.queue_family_index)
            .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
            .image(image)
            .subresource_range(default_color_subresource_range())
            .build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Consumer-side QFOT acquire barrier — receives ownership from
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) and transitions
    /// the imported `VkImage`'s tracker into `target` so subsequent
    /// consumer barriers (`oldLayout = target → next`) are
    /// validation-clean per VUID-VkImageMemoryBarrier-oldLayout-01197.
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `true`, chains
    /// `VkExternalMemoryAcquireUnmodifiedEXT { acquireUnmodifiedMemory =
    /// VK_TRUE }` so the producer's content survives the transfer; the
    /// `oldLayout` is `target` (consistent with the post-release layout
    /// the producer published). When `false`, falls back to bridging
    /// `UNDEFINED → target` — content discard permitted by spec, but in
    /// practice DMA-BUF kernel-side memory contents are preserved on
    /// every modern Linux Vulkan driver. No-op when `target ==
    /// UNDEFINED` (nothing to transition into).
    ///
    /// One-shot synchronous submit; the fence wait is the simple correct
    /// shape because the next consumer GPU work assumes the new layout
    /// is visible.
    pub fn acquire_from_foreign(
        &self,
        image: vk::Image,
        target: vk::ImageLayout,
    ) -> Result<()> {
        if target == vk::ImageLayout::UNDEFINED {
            return Ok(());
        }
        // QFOT path requires only `VK_EXT_external_memory_acquire_unmodified`;
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 and always
        // available. When the optional extension is absent, fall back
        // to bridging UNDEFINED → target as a same-family transition.
        let use_qfot = self.has_acquire_unmodified;
        let (src_qf, src_layout) = if use_qfot {
            (vk::QUEUE_FAMILY_EXTERNAL, target)
        } else {
            (self.queue_family_index, vk::ImageLayout::UNDEFINED)
        };
        // The chained `VkExternalMemoryAcquireUnmodifiedEXT` lives on the
        // stack here — must outlive the call to `submit_one_shot_image_barriers`,
        // since the built barrier holds a raw pointer to it via pNext.
        let mut acquire_unmodified = vk::ExternalMemoryAcquireUnmodifiedEXT::builder()
            .acquire_unmodified_memory(true)
            .build();
        let mut barrier_builder = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(
                vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE,
            )
            .old_layout(src_layout)
            .new_layout(target)
            .src_queue_family_index(src_qf)
            .dst_queue_family_index(self.queue_family_index)
            .image(image)
            .subresource_range(default_color_subresource_range());
        if use_qfot {
            barrier_builder = barrier_builder.push_next(&mut acquire_unmodified);
        }
        let barrier = barrier_builder.build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Internal: one-shot pipeline barrier on the device queue. Caller
    /// passes already-built `vk::ImageMemoryBarrier2` values (including
    /// any chained pNext structs they need to keep alive); this helper
    /// handles command-pool/buffer/fence creation, submit, wait, and
    /// teardown.
    fn submit_one_shot_image_barriers(
        &self,
        barriers: &[vk::ImageMemoryBarrier2],
    ) -> Result<()> {
        let device = &self.device;

        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(self.queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("qfot cmd pool: {e}")))?;

        let cb = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| {
            unsafe { device.destroy_command_pool(pool, None) };
            ConsumerRhiError::Gpu(format!("qfot cmd buf: {e}"))
        })?[0];

        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .map_err(|e| {
                unsafe { device.destroy_command_pool(pool, None) };
                ConsumerRhiError::Gpu(format!("qfot fence: {e}"))
            })?;

        unsafe {
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("begin qfot cb: {e}")))?;

        let dep = vk::DependencyInfo::builder().image_memory_barriers(barriers);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep) };

        unsafe { device.end_command_buffer(cb) }
            .map_err(|e| ConsumerRhiError::Gpu(format!("end qfot cb: {e}")))?;

        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe { self.submit_to_queue(self.queue, &[submit], fence) }?;
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| ConsumerRhiError::Gpu(format!("qfot wait: {e}")))?;

        unsafe { device.destroy_fence(fence, None) };
        unsafe { device.destroy_command_pool(pool, None) };

        Ok(())
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

    fn instance(&self) -> &vulkanalia::Instance {
        &self.instance
    }

    fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

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

    fn supports_qfot_acquire_unmodified(&self) -> bool {
        ConsumerVulkanDevice::supports_qfot_acquire_unmodified(self)
    }

    fn release_to_foreign(
        &self,
        image: vk::Image,
        src_layout: vk::ImageLayout,
        dst_layout: vk::ImageLayout,
    ) -> Result<()> {
        ConsumerVulkanDevice::release_to_foreign(self, image, src_layout, dst_layout)
    }

    fn acquire_from_foreign(
        &self,
        image: vk::Image,
        target: vk::ImageLayout,
    ) -> Result<()> {
        ConsumerVulkanDevice::acquire_from_foreign(self, image, target)
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

    /// `supports_qfot_acquire_unmodified` should equal the
    /// `has_acquire_unmodified` extension flag collected at
    /// construction (#633). The trait-method route must agree with
    /// the inherent method.
    #[test]
    fn supports_qfot_acquire_unmodified_consistent_across_inherent_and_trait() {
        let device = match try_create() {
            Some(d) => d,
            None => return,
        };
        let inherent = ConsumerVulkanDevice::supports_qfot_acquire_unmodified(&device);
        let via_trait =
            <ConsumerVulkanDevice as VulkanRhiDevice>::supports_qfot_acquire_unmodified(&device);
        assert_eq!(inherent, via_trait, "trait and inherent reports must agree");
        assert_eq!(
            inherent, device.has_acquire_unmodified,
            "supports_qfot_acquire_unmodified must equal has_acquire_unmodified \
             (VK_QUEUE_FAMILY_EXTERNAL is core 1.1 and always available)"
        );
    }

    /// QFOT acquire is a no-op when target is UNDEFINED — verifies
    /// the early-return short-circuit at the top of
    /// [`ConsumerVulkanDevice::acquire_from_foreign`]. **Honest
    /// scope**: this asserts only that the call returns `Ok`, not
    /// that no GPU work was issued. Reverting the early return
    /// makes the helper attempt a barrier with `image = vk::Image::null()`;
    /// validation layers reject that
    /// (`VUID-VkImageMemoryBarrier2-image-parameter`), but raw
    /// drivers may tolerate it silently. Real GPU-correctness for
    /// QFOT comes from E2E scenarios (polyglot examples that hit
    /// Path 2 with a real imported image, run with
    /// `VK_LOADER_LAYERS_ENABLE=*validation*`).
    #[test]
    fn acquire_from_foreign_undefined_target_is_noop() {
        let device = match try_create() {
            Some(d) => d,
            None => return,
        };
        // A null VkImage is fine here because the no-op short-circuits
        // before any handle is dereferenced.
        let result =
            device.acquire_from_foreign(vk::Image::null(), vk::ImageLayout::UNDEFINED);
        assert!(
            result.is_ok(),
            "acquire_from_foreign with target=UNDEFINED must short-circuit Ok"
        );
    }
}
