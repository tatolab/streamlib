// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Vulkan device implementation for RHI.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};

use crate::vulkan::rhi::command_recording_and_sync::vulkan_queue_submission_completion::HostVulkanQueueSubmissionCompletionTimelines;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::device::drm_modifier_probe::DrmModifierTable;
use crate::vulkan::rhi::device::vulkan_device_selection::VulkanDevicePassOverReason;
use crate::vulkan::rhi::device::vulkan_instance::HostVulkanInstance;
use crate::vulkan::rhi::device::vulkan_validation_messenger::VulkanValidationMessenger;
use crate::vulkan::rhi::kernels::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::{
    FrameAllocationDeviceMemoryBudgetGate, FrameDeviceMemoryPlacement, FrameDeviceMemoryRequest,
};
#[cfg(target_os = "macos")]
use crate::vulkan::rhi::memory_and_buffers::vulkan_metal_working_set_budget::MetalWorkingSetOfTheDevice;
use crate::vulkan::rhi::{
    HostMarker, VulkanCommandQueue, VulkanRhiDevice, VulkanValidationMessageCounts,
};

mod device_capabilities;
mod device_creation;
#[cfg(target_os = "linux")]
mod dma_buf_and_opaque_fd_texture_creation;
#[cfg(target_os = "linux")]
mod drm_and_opaque_fd_physical_device_probes;
#[cfg(target_os = "linux")]
mod export_memory_pools;
#[cfg(target_os = "linux")]
mod export_pool_sentinels;
mod frame_allocation_within_the_device_memory_budget;
mod memory_import;
#[cfg(target_os = "macos")]
mod metal_objects_interop_and_iosurface_backed_images;
#[cfg(target_os = "macos")]
mod metal_working_set_readings;
#[cfg(target_os = "linux")]
mod opaque_fd_memory_import_and_dma_buf_fd_refusal;
mod ownership_transfer_and_uploads;
#[cfg(target_os = "linux")]
mod pipeline_prewarm;
mod queue_submission;
mod texture_creation;
#[cfg(target_os = "linux")]
mod vulkan_memory_budget_readings;

#[cfg(target_os = "linux")]
pub(crate) use export_memory_pools::{
    MappedOpaqueFdBufferHostAccessPattern, VmaExportPoolPinnedToOneMemoryType,
};
#[cfg(target_os = "linux")]
use export_pool_sentinels::ExportPoolSentinel;

/// Vulkan GPU device.
///
/// Wraps the Vulkan instance, physical device, and logical device.
/// On macOS/iOS, uses MoltenVK to provide Vulkan API on top of Metal.
pub struct HostVulkanDevice {
    /// Shared with device selection, so a candidate refused after its
    /// logical device was created drops without destroying the instance.
    vulkan_instance: Arc<HostVulkanInstance>,
    physical_device: vk::PhysicalDevice,
    /// Memory types and heaps: the raw `vkAllocateMemory` paths pick a type
    /// from them, and the frame-allocation budget maps a type to its heap.
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    /// `VkPhysicalDeviceLimits::maxMemoryAllocationCount`: how many
    /// `VkDeviceMemory` objects the device lets this process hold at once.
    max_memory_allocation_count: u32,
    /// Whether `VK_EXT_memory_budget` was enabled: the driver reports each
    /// heap's budget and usage, rather than the allocator estimating them.
    #[cfg(target_os = "linux")]
    has_memory_budget: bool,
    /// Weighs every frame allocation against the device-memory bound and
    /// makes it, one at a time. A lock of its own, never [`Self::device_mutex`].
    frame_allocation_device_memory_budget_gate: FrameAllocationDeviceMemoryBudgetGate,
    /// The frame index handed to `vmaSetCurrentFrameIndex`, which is what
    /// makes VMA fetch a fresh budget from the driver.
    #[cfg(target_os = "linux")]
    device_memory_budget_fresh_fetch_frame_index: std::sync::atomic::AtomicU32,
    /// The Metal device MoltenVK drives, whose recommended working set bounds
    /// frame allocations on macOS.
    #[cfg(target_os = "macos")]
    metal_working_set_of_the_device: MetalWorkingSetOfTheDevice,
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    transfer_queue_family_index: u32,
    transfer_queue: vk::Queue,
    device_name: String,
    supports_external_memory: bool,
    /// Whether the device is `VK_PHYSICAL_DEVICE_TYPE_CPU` — a software
    /// rasterizer such as lavapipe.
    device_is_software: bool,
    /// Whether frames can cross as DMA-BUF: `VK_EXT_external_memory_dma_buf`
    /// and `VK_EXT_image_drm_format_modifier`, never on a software device.
    supports_dma_buf_export: bool,
    /// Whether a timeline semaphore can be exported as an OPAQUE_FD.
    supports_semaphore_export: bool,
    /// Whether the driver tolerates a failed cross-device DMA-BUF import
    /// (raw `vkAllocateMemory` chained with `VkImportMemoryFdInfoKHR`)
    /// without perturbing per-handle-type accounting for other export
    /// handle types — notably OPAQUE_FD. False on NVIDIA Linux per #638.
    supports_cross_device_dma_buf_probe: bool,
    /// Whether `VK_EXT_external_memory_acquire_unmodified` was enabled
    /// at device creation. Lets the host's acquire barrier chain
    /// `VkExternalMemoryAcquireUnmodifiedEXT` so producer-side content
    /// survives the QFOT acquire (#633). When unavailable, the QFOT
    /// helpers fall back to bridging UNDEFINED → target —
    /// structurally permanent on NVIDIA Linux (extension not on
    /// NVIDIA's roadmap as of 2026-05-03; Mesa is the eventual
    /// landing point). `VK_QUEUE_FAMILY_EXTERNAL` (the queue family
    /// index used for QFOT src/dst) is core Vulkan 1.1 and is always
    /// available; this acquire-side extension is the only meaningful
    /// gate.
    has_acquire_unmodified: bool,
    /// Whether `VK_EXT_external_memory_host` was enabled at device
    /// creation. Gates the host-pointer import tier of
    /// [`crate::vulkan::rhi::HostMappingReadByGpu`]: a caller-owned mapping becomes a
    /// `VkBuffer` the GPU reads in place.
    has_external_memory_host: bool,
    /// `VkPhysicalDeviceExternalMemoryHostPropertiesEXT::minImportedHostPointerAlignment`
    /// snapshotted at construction when [`Self::has_external_memory_host`]
    /// is true; a page on every driver seen so far. Zero when the
    /// extension is absent.
    min_imported_host_pointer_alignment: u64,
    /// Whether the `VK_KHR_ray_tracing_pipeline` extension chain
    /// (acceleration_structure + ray_tracing_pipeline +
    /// deferred_host_operations + pipeline_library) was enabled at
    /// device creation, plus the core 1.2 `bufferDeviceAddress`
    /// feature it depends on. Gates [`VulkanRayTracingKernel`]
    /// construction; absent on devices without RT support and on
    /// non-Linux platforms.
    has_ray_tracing_pipeline: bool,
    /// `VkPhysicalDeviceRayTracingPipelinePropertiesKHR` snapshot
    /// queried at construction when [`Self::has_ray_tracing_pipeline`]
    /// is true. Carries the SBT alignment + handle-size constants the
    /// kernel needs at every dispatch — caching on the device avoids
    /// a re-query per kernel.
    ray_tracing_properties: Option<RayTracingPipelineProperties>,
    /// The subgroup operations and stages the driver serves, checked against
    /// each kernel's shaders at construction.
    subgroup_operation_support: VulkanSubgroupOperationSupport,
    /// The most samples an anisotropic kernel sampler may take, 1 when the
    /// device serves no anisotropic filtering.
    sampler_anisotropy_limit: u32,
    compute_queue_family_index: Option<u32>,
    compute_queue: Option<vk::Queue>,
    /// VMA allocator for all GPU memory allocation. Option for controlled drop order.
    allocator: Option<Arc<vma::Allocator>>,
    /// VMA pool for DMA-BUF exportable HOST_VISIBLE buffers (pixel buffers for IPC).
    /// Created when external memory is supported. Carries VkExportMemoryAllocateInfo
    /// via pMemoryAllocateNext, isolated from the default pool so non-export
    /// allocations don't carry export flags (which NVIDIA rejects after swapchain
    /// creation when set globally on the allocator).
    #[cfg(target_os = "linux")]
    dma_buf_buffer_pool: Option<VmaExportPoolPinnedToOneMemoryType>,
    /// VMA pool for DMA-BUF exportable DEVICE_LOCAL images (textures for IPC).
    #[cfg(target_os = "linux")]
    dma_buf_image_pool: Option<VmaExportPoolPinnedToOneMemoryType>,
    /// VMA pool for DMA-BUF exportable images created with
    /// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` and a render-target-capable
    /// DRM modifier from [`Self::drm_modifier_table`]. Separate from
    /// [`Self::dma_buf_image_pool`] because the memory type index for tiled
    /// DRM-modifier images may differ from the OPTIMAL pool's, and because
    /// callers should never get a sampler-only LINEAR image when they asked
    /// for an RT-capable one.
    #[cfg(target_os = "linux")]
    dma_buf_image_pool_tiled: Option<VmaExportPoolPinnedToOneMemoryType>,
    /// VMA pool for OPAQUE_FD exportable HOST_VISIBLE buffers — the export
    /// flavor CUDA / OpenCL interop expects. Separate from
    /// [`Self::dma_buf_buffer_pool`] because `VkExportMemoryAllocateInfo`'s
    /// `handleTypes` differ (OPAQUE_FD vs DMA_BUF_EXT) and a single VMA pool
    /// can carry only one chained `pNext`.
    #[cfg(target_os = "linux")]
    opaque_fd_buffer_pool: Option<VmaExportPoolPinnedToOneMemoryType>,
    /// VMA pool for OPAQUE_FD exportable HOST_VISIBLE buffers on a
    /// HOST_CACHED memory type — the CPU-read sibling of
    /// [`Self::opaque_fd_buffer_pool`]. `None` on a device with no cached
    /// exportable type; see
    /// [`Self::create_opaque_fd_buffer_pool_host_cached`].
    #[cfg(target_os = "linux")]
    opaque_fd_buffer_pool_host_cached: Option<VmaExportPoolPinnedToOneMemoryType>,
    /// VMA pool for OPAQUE_FD exportable DEVICE_LOCAL buffers — the
    /// GPU-resident sibling of [`Self::opaque_fd_buffer_pool`]. CUDA imports
    /// the same OPAQUE_FD handle type, but the underlying memory lives in
    /// VRAM (~600 GB/s on RTX 3090) instead of pinned host (~16-32 GB/s
    /// PCIe), which matters for hot-path camera→inference flows. Separate
    /// pool because VMA pools are pinned to a single memory-type-index and
    /// DEVICE_LOCAL types differ from HOST_VISIBLE ones.
    #[cfg(target_os = "linux")]
    opaque_fd_buffer_pool_device_local: Option<VmaExportPoolPinnedToOneMemoryType>,
    /// VMA pool for OPAQUE_FD exportable DEVICE_LOCAL **images** —
    /// `VK_IMAGE_TILING_OPTIMAL`, no DRM modifier. CUDA's
    /// `cudaExternalMemoryGetMappedMipmappedArray` consumes the
    /// underlying memory by OPAQUE_FD and treats it as a tiled mipmapped
    /// array; LINEAR returns `CUDA_ERROR_INVALID_VALUE` and OPAQUE_FD
    /// is not compatible with `DRM_FORMAT_MODIFIER_EXT` on NVIDIA, so
    /// OPTIMAL is the only valid combination. Separate from the
    /// buffer-flavored OPAQUE_FD pools because a VMA pool can chain
    /// only one `VkExportMemoryAllocateInfo` per pool — same reason
    /// the DMA-BUF image pool is split from the DMA-BUF buffer pool.
    #[cfg(target_os = "linux")]
    opaque_fd_image_pool: Option<VmaExportPoolPinnedToOneMemoryType>,
    /// Backing storage for the buffer pool's VkExportMemoryAllocateInfo. VMA stores
    /// a raw pointer to this struct via pMemoryAllocateNext, so we must keep it
    /// alive for the pool's entire lifetime.
    #[cfg(target_os = "linux")]
    _dma_buf_buffer_export_info: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Backing storage for the image pool's VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _dma_buf_image_export_info: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Backing storage for the tiled image pool's VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _dma_buf_image_tiled_export_info: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Backing storage for the OPAQUE_FD buffer pool's VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _opaque_fd_buffer_export_info: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Backing storage for the host-cached OPAQUE_FD buffer pool's
    /// VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _opaque_fd_buffer_export_info_host_cached: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Backing storage for the DEVICE_LOCAL OPAQUE_FD buffer pool's
    /// VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _opaque_fd_buffer_export_info_device_local: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Backing storage for the OPAQUE_FD image pool's
    /// VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _opaque_fd_image_export_info: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// `VkPhysicalDeviceIDProperties::deviceUUID` queried at construction.
    /// Required by CUDA-Vulkan interop on multi-GPU rigs to match the
    /// CUDA device whose `cudaDeviceProp::uuid` equals this value;
    /// using a mismatched device silently fails on the import.
    physical_device_uuid: [u8; 16],
    /// `VkPhysicalDeviceLimits::minStorageBufferOffsetAlignment`: every
    /// storage-buffer descriptor's offset must be a multiple of it.
    min_storage_buffer_offset_alignment: u64,
    /// Render-target-capable DRM modifiers per format from the EGL probe at
    /// device init. Empty when libEGL is unavailable or the probe failed.
    /// Callers consult this before requesting
    /// [`HostVulkanTexture::new_render_target_dma_buf`](crate::vulkan::rhi::HostVulkanTexture::new_render_target_dma_buf).
    #[cfg(target_os = "linux")]
    drm_modifier_table: Arc<DrmModifierTable>,
    /// Live raw `vkAllocateMemory` allocations, which VMA never sees: DMA-BUF
    /// and host-pointer imports, and IOSurface-backed image memory.
    live_allocation_count: AtomicUsize,
    /// Every `vkQueueSubmit2` this device has made, across every queue.
    queue_submission_count: AtomicUsize,
    /// Waits made through [`Self::wait_for_fences_blocking_counted`] and
    /// [`Self::wait_for_queue_submissions_blocking_counted`]: every
    /// `RhiCommandRecorder` drain, `VulkanComputeKernel::dispatch`, and every
    /// compute dispatch's wait on its own submission. Not every host wait in
    /// the engine, and not compute-only — the recorder is what present,
    /// staging and the tone mapper wait on too.
    counted_host_waits_on_gpu_work_count: AtomicUsize,
    /// A completion timeline per queue, signalled by every
    /// [`Self::submit_to_queue`]. `None` only once `Drop` has destroyed them.
    queue_submission_completion_timelines: Option<HostVulkanQueueSubmissionCompletionTimelines>,
    /// Per-queue mutex for thread-safe queue submission (Vulkan spec requirement).
    graphics_queue_mutex: Mutex<()>,
    /// Per-queue mutex for the dedicated transfer queue.
    transfer_queue_mutex: Mutex<()>,
    /// Per-queue mutex for the dedicated compute queue (if available).
    compute_queue_mutex: Mutex<()>,
    /// One tiny allocation per export pool, kept for the device's life so
    /// NVIDIA's per-handle-type export state never sees "no live allocation
    /// of this handle type" once a swapchain exists — not even after the
    /// shared pools free every frame of that flavour. See
    /// NVIDIA's OPAQUE_FD export cap after a swapchain.
    #[cfg(target_os = "linux")]
    export_pool_sentinels: Vec<ExportPoolSentinel>,
}

/// The queue families the engine device is created with: graphics, a
/// transfer-only family when the device has one, and a compute family without
/// graphics when it has one. Video queues belong to the codec modules' own
/// devices, so a family that codes video is never chosen for transfer or compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostVulkanQueueFamilySelection {
    graphics_queue_family_index: u32,
    transfer_queue_family_index: u32,
    compute_queue_family_index: Option<u32>,
}

impl HostVulkanQueueFamilySelection {
    fn from_queue_families(
        queue_families: &[vk::QueueFamilyProperties],
    ) -> std::result::Result<Self, VulkanDevicePassOverReason> {
        let index_of_first_family_where = |accepts: &dyn Fn(vk::QueueFlags) -> bool| {
            queue_families
                .iter()
                .position(|family| accepts(family.queue_flags))
                .map(|index| index as u32)
        };
        let video_queue_flags = vk::QueueFlags::VIDEO_DECODE_KHR | vk::QueueFlags::VIDEO_ENCODE_KHR;
        let graphics_queue_family_index =
            index_of_first_family_where(&|flags| flags.contains(vk::QueueFlags::GRAPHICS))
                .ok_or(VulkanDevicePassOverReason::NoGraphicsQueueFamily)?;
        let transfer_queue_family_index = index_of_first_family_where(&|flags| {
            flags.contains(vk::QueueFlags::TRANSFER)
                && !flags.intersects(
                    vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | video_queue_flags,
                )
        })
        .unwrap_or(graphics_queue_family_index);
        let compute_queue_family_index = index_of_first_family_where(&|flags| {
            flags.contains(vk::QueueFlags::COMPUTE)
                && !flags.intersects(vk::QueueFlags::GRAPHICS | video_queue_flags)
        });
        Ok(Self {
            graphics_queue_family_index,
            transfer_queue_family_index,
            compute_queue_family_index,
        })
    }

    /// Each chosen family once, graphics first: one `VkDeviceQueueCreateInfo` each.
    fn distinct_queue_family_indices(&self) -> Vec<u32> {
        let mut distinct_queue_family_indices = vec![self.graphics_queue_family_index];
        for family_index in [
            Some(self.transfer_queue_family_index),
            self.compute_queue_family_index,
        ]
        .into_iter()
        .flatten()
        {
            if !distinct_queue_family_indices.contains(&family_index) {
                distinct_queue_family_indices.push(family_index);
            }
        }
        distinct_queue_family_indices
    }
}

/// Single-COLOR-aspect / single-mip / single-layer subresource range —
/// every host-side surface-adapter-managed image fits this shape today.
fn host_default_color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// Snapshot of `VkPhysicalDeviceRayTracingPipelinePropertiesKHR` at
/// device construction. Only the fields the kernel actually uses are
/// kept — the upstream struct carries dispatch-time limits the engine
/// doesn't surface today (e.g. `maxRayHitAttributeSize`).
#[derive(Debug, Clone, Copy)]
pub struct RayTracingPipelineProperties {
    /// Size in bytes of an opaque shader group handle. Each SBT entry
    /// starts with one of these handles followed by optional shader
    /// record data.
    pub shader_group_handle_size: u32,
    /// Required alignment of every SBT region's stride. Per Khronos
    /// spec, `stride` passed to `vkCmdTraceRaysKHR` must be a multiple
    /// of this value.
    pub shader_group_handle_alignment: u32,
    /// Required alignment of the start address of each SBT region.
    /// The buffer device address passed in
    /// `VkStridedDeviceAddressRegionKHR::deviceAddress` must be a
    /// multiple of this value.
    pub shader_group_base_alignment: u32,
    /// Maximum recursion depth a ray-tracing pipeline can declare.
    /// Pipelines requesting a higher value fail at creation.
    pub max_ray_recursion_depth: u32,
}

/// The allocator a frame's VMA allocation comes from. It alone decides the
/// memory type, and through it the heap, whose budget weighs the frame.
#[derive(Clone, Copy)]
pub(crate) enum VmaFrameAllocationSource<#[cfg(target_os = "linux")] 'a> {
    /// An export pool, which allocates from the one memory type it is pinned to.
    #[cfg(target_os = "linux")]
    ExportPool(&'a VmaExportPoolPinnedToOneMemoryType),
    /// The device's default allocator, which picks a memory type per create info.
    DefaultAllocator,
}

/// The request a frame of `memory_requirements` makes on the memory type its
/// allocator chose, or the refusal when no memory type satisfies it.
fn frame_device_memory_request_on_a_vulkan_memory_type(
    memory_requirements: vk::MemoryRequirements,
    memory_type_index_or_refusal: vulkanalia::VkResult<u32>,
    frame_allocation_site: &'static str,
) -> Result<FrameDeviceMemoryRequest> {
    let memory_type_index = memory_type_index_or_refusal.map_err(|e| {
        Error::GpuError(format!(
            "{frame_allocation_site}: no memory type in memory_type_bits {:#x} satisfies the \
             allocation: {e}",
            memory_requirements.memory_type_bits
        ))
    })?;
    Ok(FrameDeviceMemoryRequest {
        requested_bytes: memory_requirements.size,
        placement: FrameDeviceMemoryPlacement::VulkanMemoryType { memory_type_index },
        frame_allocation_site,
    })
}

/// A logical device destroyed on drop unless the built device takes it, so an
/// early return while a candidate is being built leaks no `VkDevice`.
struct VulkanLogicalDeviceDestroyedUnlessTaken {
    device: vulkanalia::Device,
}

impl VulkanLogicalDeviceDestroyedUnlessTaken {
    fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    /// Hand the device on without destroying it.
    fn take_the_logical_device(self) -> vulkanalia::Device {
        let guard_that_never_drops = std::mem::ManuallyDrop::new(self);
        // SAFETY: the guard is never dropped and `device` is its only field,
        // so the device is moved out exactly once.
        unsafe { std::ptr::read(&guard_that_never_drops.device) }
    }
}

impl Drop for VulkanLogicalDeviceDestroyedUnlessTaken {
    fn drop(&mut self) {
        // SAFETY: every object created on the device was a local of the
        // refused build and has already dropped.
        unsafe { self.device.destroy_device(None) };
    }
}

/// Whether the memory type at `memory_type_index` carries HOST_CACHED.
///
/// [`HostVulkanDevice::create_opaque_fd_buffer_pool_host_cached`] turns
/// on this alone. Indices at or past `memoryTypeCount` are not real
/// memory types and answer `false`.
pub(crate) fn memory_type_index_is_host_cached(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    memory_type_index: u32,
) -> bool {
    if memory_type_index >= memory_properties.memory_type_count {
        return false;
    }
    memory_properties.memory_types[memory_type_index as usize]
        .property_flags
        .contains(vk::MemoryPropertyFlags::HOST_CACHED)
}

impl HostVulkanDevice {
    /// Create a VulkanCommandQueue wrapper for the shared command queue.
    pub fn create_command_queue_wrapper(self: &Arc<Self>) -> VulkanCommandQueue {
        VulkanCommandQueue::new(Arc::clone(self), self.queue, self.queue_family_index)
    }

    /// Get the device name.
    pub fn name(&self) -> String {
        self.device_name.clone()
    }

    /// Get the Vulkan entry point loader.
    pub fn entry(&self) -> &vulkanalia::Entry {
        &self.vulkan_instance.entry
    }

    /// Get the Vulkan instance.
    pub fn instance(&self) -> &vulkanalia::Instance {
        &self.vulkan_instance.instance
    }

    /// Get the Vulkan physical device.
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    /// Get the Vulkan logical device.
    pub fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    /// Get the graphics queue.
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }

    /// Get the transfer queue handle.
    pub fn transfer_queue(&self) -> vk::Queue {
        self.transfer_queue
    }

    /// Get the dedicated compute queue (if available).
    pub fn compute_queue(&self) -> Option<vk::Queue> {
        self.compute_queue
    }

    /// Khronos-validation-layer findings this device has seen, or `None`
    /// when no messenger is installed — validation off, layer absent, or
    /// `VK_EXT_debug_utils` unavailable. A test asserting zero findings
    /// must treat `None` as "not measured", never as zero.
    pub fn validation_layer_message_counts(&self) -> Option<VulkanValidationMessageCounts> {
        self.vulkan_instance
            .validation_messenger
            .as_ref()
            .and_then(VulkanValidationMessenger::counts)
    }
}

impl VulkanRhiDevice for HostVulkanDevice {
    type Privilege = HostMarker;

    fn instance(&self) -> &vulkanalia::Instance {
        &self.vulkan_instance.instance
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
    ) -> streamlib_consumer_rhi::Result<()> {
        unsafe { HostVulkanDevice::submit_to_queue(self, queue, submits, fence) }
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }

    fn supports_qfot_acquire_unmodified(&self) -> bool {
        HostVulkanDevice::supports_qfot_acquire_unmodified(self)
    }

    fn release_to_foreign(
        &self,
        image: vk::Image,
        src_layout: vk::ImageLayout,
        dst_layout: vk::ImageLayout,
    ) -> streamlib_consumer_rhi::Result<()> {
        HostVulkanDevice::release_to_foreign(self, image, src_layout, dst_layout)
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }

    fn acquire_from_foreign(
        &self,
        image: vk::Image,
        target: vk::ImageLayout,
    ) -> streamlib_consumer_rhi::Result<()> {
        HostVulkanDevice::acquire_from_foreign(self, image, target)
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }
}

/// Terminal layout a buffer-to-image upload left its destination in —
/// `SHADER_READ_ONLY_OPTIMAL` for a sampled-capable image, `GENERAL`
/// otherwise. Callers holding a
/// [`crate::core::context::TextureRegistration`] write it back via
/// `update_layout`; callers minting one pass it as the initial layout.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the terminal layout must reach the destination's registration — discarding it leaves a registration claiming a layout the image is not in, and the next consumer barriers out of a layout that was never recorded"]
pub struct PixelBufferUploadFinalTextureLayout {
    /// Layout the destination texture was left in.
    pub final_texture_layout: streamlib_consumer_rhi::VulkanLayout,
}

impl HostVulkanDevice {
    /// Get the VMA allocator for GPU memory management.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        self.allocator
            .as_ref()
            .expect("VMA allocator not initialized")
    }

    /// `VkPhysicalDeviceIDProperties::deviceUUID` queried at construction.
    ///
    /// 16-byte big-endian UUID identifying this physical device.
    /// CUDA-Vulkan interop matches this against `cudaDeviceProp::uuid`
    /// to pick the correct CUDA device on multi-GPU rigs; using a
    /// mismatched device fails silently when CUDA imports an
    /// OPAQUE_FD memory or semaphore exported from this device.
    pub fn physical_device_uuid(&self) -> [u8; 16] {
        self.physical_device_uuid
    }

    /// `VkPhysicalDeviceLimits::minStorageBufferOffsetAlignment` — the
    /// multiple every storage-buffer descriptor offset must be.
    pub fn min_storage_buffer_offset_alignment(&self) -> u64 {
        self.min_storage_buffer_offset_alignment
    }
}

impl Drop for HostVulkanDevice {
    fn drop(&mut self) {
        let live = self.live_allocation_count.load(Ordering::Relaxed);
        if live > 0 {
            tracing::warn!(
                "HostVulkanDevice dropping with {} live import allocations (leak)",
                live
            );
        }

        // Queue-mutex-guarded wait (drains every queue before teardown);
        // uncontended at Drop but keeps the single discipline everywhere.
        let _ = self.wait_idle();
        drop(self.queue_submission_completion_timelines.take());

        // Critical drop order:
        //  0. Export pool sentinels — freed before any pool is
        //     destroyed (vmaDestroyPool aborts on a live dedicated
        //     allocation) and release their Arc<Allocator> refs.
        //  1. DMA-BUF + OPAQUE_FD pools — release Arc<Allocator> refs and call vmaDestroyPool
        //  2. Allocator — call vmaDestroyAllocator (only after all Arc refs gone)
        //  3. Export info Boxes — VMA no longer references them after pool destruction
        //  4. Device — the instance goes with the last `HostVulkanInstance` holder
        #[cfg(target_os = "linux")]
        {
            self.export_pool_sentinels.clear();
            drop(self.dma_buf_buffer_pool.take());
            drop(self.dma_buf_image_pool.take());
            drop(self.dma_buf_image_pool_tiled.take());
            drop(self.opaque_fd_buffer_pool.take());
            drop(self.opaque_fd_buffer_pool_host_cached.take());
            drop(self.opaque_fd_buffer_pool_device_local.take());
            drop(self.opaque_fd_image_pool.take());
        }

        drop(self.allocator.take());

        #[cfg(target_os = "linux")]
        {
            drop(self._dma_buf_buffer_export_info.take());
            drop(self._dma_buf_image_export_info.take());
            drop(self._dma_buf_image_tiled_export_info.take());
            drop(self._opaque_fd_buffer_export_info.take());
            drop(self._opaque_fd_buffer_export_info_host_cached.take());
            drop(self._opaque_fd_buffer_export_info_device_local.take());
            drop(self._opaque_fd_image_export_info.take());
        }

        unsafe { self.device.destroy_device(None) };
    }
}

// HostVulkanDevice is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for HostVulkanDevice {}
unsafe impl Sync for HostVulkanDevice {}

#[cfg(test)]
mod tests;
