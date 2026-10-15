// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan device implementation for RHI.

use std::ffi::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk::{self, KhrSwapchainExtensionDeviceCommands};
use vulkanalia_vma as vma;

use crate::core::rhi::TextureDescriptor;
use crate::core::{Error, Result};

use streamlib_consumer_rhi::vulkan_extension_names_borrowed_from_properties;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::VulkanTextureLike;
#[cfg(target_os = "linux")]
use super::drm_modifier_probe::{self, DrmModifierTable};
use super::vulkan_device_selection::{
    VulkanDevicePassOverReason, VulkanPhysicalDeviceCandidate,
    build_on_the_first_candidate_that_accepts,
};
use super::vulkan_frame_allocation_device_memory_budget::{
    DeviceMemoryAllocationCountReading, DeviceMemoryBudgetAdmission, FrameAllocationBudgetReading,
    FrameAllocationDeviceMemoryBudgetGate, FrameAllocationDeviceMemoryReading,
    FrameDeviceMemoryPlacement, FrameDeviceMemoryRequest,
    device_memory_allocations_this_process_holds,
};
#[cfg(target_os = "linux")]
use super::vulkan_frame_allocation_device_memory_budget::{
    DeviceMemoryHeapBudgetReading, heap_budget_without_the_memory_budget_extension,
};
use super::vulkan_instance::HostVulkanInstance;
use super::vulkan_kernel_capability_refusal::{
    VulkanSubgroupOperationSupport, ray_tracing_tier_absent_refusal,
};
#[cfg(target_os = "macos")]
use super::vulkan_metal_working_set_budget::{
    MetalDeviceWhoseWorkingSetBoundsFrameAllocations, MetalWorkingSetOfTheDevice,
};
use super::vulkan_queue_submission_completion::{
    HostVulkanQueueSubmissionCompletionTimelines, HostVulkanQueueSubmissionsSnapshot,
    HostVulkanSubmissionQueue,
};
use super::vulkan_validation_messenger::VulkanValidationMessenger;
use super::{
    HostMarker, HostVulkanTexture, VulkanCommandQueue, VulkanRhiDevice,
    VulkanValidationMessageCounts,
};

/// Best-effort hint about which third-party GPU compute libraries are
/// **available to integrate against this device**. Probed once at
/// device construction; each field is `true` when the matching vendor
/// SDK can plausibly run on this physical device + host environment.
///
/// "Available" is a build-host hint, not a runtime guarantee: the
/// actual `Backend::new` step may still fail (CUDA context init
/// fails, driver mismatch, GPU lacks the right compute capability,
/// etc.). Backend-using libraries (e.g. `vulkan-jpeg`'s
/// `SimpleJpegDecoder`) treat the field as "try this backend first,
/// fall back if construction fails."
///
/// **When you add a second field to this struct**, read
/// `docs/architecture/third-party-gpu-backends.md` first. The single-
/// consumer JPEG-specific backend trait in `vulkan-jpeg` is the right
/// shape for one consumer — the second consumer is the trigger to
/// lift the trait to an engine-tier `ThirdPartyGpuBackend` primitive
/// next to this struct. Don't copy the JPEG-specific shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThirdPartyGpuCapabilities {
    /// NVIDIA `libnvjpeg` (CUDA-based JPEG decode). True when this
    /// physical device's PCI vendor matches NVIDIA and
    /// `libnvjpeg.so.12` is loadable via `dlopen`. The actual nvJPEG
    /// init (`nvjpegCreateSimple` + CUDA context creation) happens at
    /// backend construction time and may still fail for reasons not
    /// covered by this probe (e.g. CUDA driver missing, GPU lacks
    /// the required compute capability).
    pub nvjpeg: bool,
}

impl ThirdPartyGpuCapabilities {
    /// PCI vendor ID assigned by the Khronos Vulkan registry to NVIDIA.
    const PCI_VENDOR_NVIDIA: u32 = 0x10DE;

    /// Probe the host environment for available third-party GPU
    /// libraries. Cheap — `dlopen` + `dlclose` per probed lib, no
    /// vendor SDK init, no CUDA context creation. Safe to call from
    /// `HostVulkanDevice::new` before any consumer code runs.
    pub(crate) fn probe(vendor_id: u32) -> Self {
        Self {
            nvjpeg: vendor_id == Self::PCI_VENDOR_NVIDIA && probe_nvjpeg_loadable(),
        }
    }
}

/// Try to `dlopen` `libnvjpeg.so.12` and close it immediately. Returns
/// `true` when the dynamic linker can find the library by any of:
///
/// 1. The bare SONAME `libnvjpeg.so.12` — works when the toolkit's
///    `/etc/ld.so.conf.d/*cuda*.conf` entries have been picked up by a
///    recent `ldconfig` run.
/// 2. The canonical CUDA Toolkit install paths (12.x versioned
///    `/usr/local/cuda-12.{9,8,7,6,5,4,3,2,1,0}/targets/x86_64-linux/lib/`
///    and the unversioned `/usr/local/cuda/...` symlink). A fresh
///    `libnvjpeg-dev-12-*` apt install often leaves the ldconfig cache
///    stale until the next `ldconfig` run; falling back to explicit
///    paths means our capability probe doesn't depend on root-only
///    cache state.
/// 3. The Debian multiarch path `/usr/lib/x86_64-linux-gnu/` —
///    distro-packaged installs land here.
///
/// Probing the actual `nvjpegCreateSimple` symbol would require a CUDA
/// context (CUDA initializes lazily on first call), so the minimal probe
/// stops at the library-loadable level. Backend construction validates
/// the rest.
fn probe_nvjpeg_loadable() -> bool {
    // SAFETY: `libloading::Library::new` is safe to call against any
    // path — failure to find the lib returns `Err`, not undefined
    // behavior. The library is dropped immediately, calling `dlclose`.
    let candidates: &[&str] = &[
        "libnvjpeg.so.12",
        "/usr/local/cuda/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.9/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.8/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.7/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.6/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.5/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.4/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.3/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.2/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.1/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/local/cuda-12.0/targets/x86_64-linux/lib/libnvjpeg.so.12",
        "/usr/lib/x86_64-linux-gnu/libnvjpeg.so.12",
    ];
    for candidate in candidates {
        if unsafe { libloading::Library::new(candidate) }.is_ok() {
            return true;
        }
    }
    false
}

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
    #[allow(dead_code)]
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
    /// Best-effort hint about which third-party GPU compute libraries
    /// (nvJPEG, etc.) are available to integrate against this device.
    /// Probed at construction; see [`ThirdPartyGpuCapabilities`].
    third_party_gpu_capabilities: ThirdPartyGpuCapabilities,
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
    /// [`super::HostMappingWrittenByGpu`]: a caller-owned mapping — a
    /// V4L2 output buffer — becomes a `VkBuffer` the GPU writes directly.
    has_external_memory_host: bool,
    /// `VkPhysicalDeviceExternalMemoryHostPropertiesEXT::minImportedHostPointerAlignment`
    /// snapshotted at construction when [`Self::has_external_memory_host`]
    /// is true; a page on every driver seen so far. Zero when the
    /// extension is absent.
    min_imported_host_pointer_alignment: u64,
    /// Whether `VK_EXT_hdr_metadata` was enabled at device creation.
    /// Gates `vkSetHdrMetadataEXT`, the per-swapchain HDR static metadata
    /// (mastering display + content light) attachment that drives the
    /// driver's tone-mapping / color-volume signaling on transition into
    /// a PQ/HLG colorspace. Useful only alongside `VK_KHR_swapchain` and
    /// the instance-level `VK_EXT_swapchain_colorspace`.
    has_hdr_metadata: bool,
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
    supports_video_encode: bool,
    supports_video_decode: bool,
    video_encode_queue_family_index: Option<u32>,
    video_encode_queue: Option<vk::Queue>,
    video_decode_queue_family_index: Option<u32>,
    video_decode_queue: Option<vk::Queue>,
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
    /// [`HostVulkanTexture::new_render_target_dma_buf`].
    #[cfg(target_os = "linux")]
    drm_modifier_table: Arc<DrmModifierTable>,
    /// Live raw `vkAllocateMemory` allocations, which VMA never sees: DMA-BUF
    /// and host-pointer imports, and IOSurface-backed image memory.
    live_allocation_count: AtomicUsize,
    /// Every `vkQueueSubmit2` this device has made, across every queue.
    queue_submission_count: AtomicUsize,
    /// Fence waits made through [`Self::wait_for_fences_blocking_counted`]:
    /// every `RhiCommandRecorder` drain plus `VulkanComputeKernel::dispatch`.
    /// Not every `vkWaitForFences` in the engine, and not compute-only — the
    /// recorder is what present, staging and the tone mapper wait on too.
    recorder_and_compute_kernel_fence_wait_count: AtomicUsize,
    /// A completion timeline per queue, signalled by every
    /// [`Self::submit_to_queue`]. `None` only once `Drop` has destroyed them.
    queue_submission_completion_timelines: Option<HostVulkanQueueSubmissionCompletionTimelines>,
    /// Per-queue mutex for thread-safe queue submission (Vulkan spec requirement).
    graphics_queue_mutex: Mutex<()>,
    /// Per-queue mutex for the dedicated transfer queue.
    transfer_queue_mutex: Mutex<()>,
    /// Per-queue mutex for the video encode queue (if available).
    video_encode_queue_mutex: Mutex<()>,
    /// Per-queue mutex for the video decode queue (if available).
    video_decode_queue_mutex: Mutex<()>,
    /// Per-queue mutex for the dedicated compute queue (if available).
    compute_queue_mutex: Mutex<()>,
    /// Device-level mutex for the codec layer's resource creation: video
    /// sessions, query pools, DPB, bitstream and encode-staging allocations.
    device_mutex: Mutex<()>,
    /// One tiny allocation per export pool, kept for the device's life so
    /// NVIDIA's per-handle-type export state never sees "no live allocation
    /// of this handle type" once a swapchain exists — not even after the
    /// shared pools free every frame of that flavour. See
    /// `docs/learnings/nvidia-opaque-fd-after-swapchain.md`.
    #[cfg(target_os = "linux")]
    export_pool_sentinels: Vec<ExportPoolSentinel>,
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

/// A VMA export pool and the one memory type it allocates from.
#[cfg(target_os = "linux")]
pub(crate) struct VmaExportPoolPinnedToOneMemoryType {
    pub(crate) pool: vma::Pool,
    pub(crate) memory_type_index: u32,
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

#[cfg(target_os = "linux")]
impl<'a> VmaFrameAllocationSource<'a> {
    /// `export_pool` where the device created it, the default allocator otherwise.
    pub(crate) fn export_pool_or_the_default_allocator(
        export_pool: Option<&'a VmaExportPoolPinnedToOneMemoryType>,
    ) -> Self {
        export_pool.map_or(Self::DefaultAllocator, Self::ExportPool)
    }
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

/// Refuse, naming the image's format and usage, an image whose
/// `memory_type_bits` exclude the one memory type an export pool is pinned
/// to — the pool was probed with one image shape, and an image of another
/// may not bind its type.
#[cfg(target_os = "linux")]
fn refuse_an_image_the_export_pools_memory_type_cannot_bind(
    image_memory_type_bits: u32,
    export_pool_memory_type_index: u32,
    image_create_info: &vk::ImageCreateInfo,
    frame_allocation_site: &'static str,
) -> Result<()> {
    let export_pool_memory_type_bit = 1u32.checked_shl(export_pool_memory_type_index).unwrap_or(0);
    if image_memory_type_bits & export_pool_memory_type_bit != 0 {
        return Ok(());
    }
    Err(Error::GpuError(format!(
        "{frame_allocation_site}: a {:?} image with usage {:?} cannot bind memory type {} its \
         export pool is pinned to (the image's memory_type_bits are {:#x})",
        image_create_info.format,
        image_create_info.usage,
        export_pool_memory_type_index,
        image_memory_type_bits,
    )))
}

/// Internal anti-decay sentinel for an export-capable VMA pool.
///
/// Frees its allocation on drop, so it must drop before the pool it was
/// allocated from: VMA aborts when a pool is destroyed with a live
/// dedicated allocation. Holds the allocator rather than an
/// `Arc<HostVulkanDevice>` so it can be stored on the device itself
/// without creating a reference cycle.
#[cfg(target_os = "linux")]
struct ExportPoolSentinel {
    allocator: Arc<vma::Allocator>,
    resource: ExportPoolSentinelResource,
    allocation: vma::Allocation,
    /// Diagnostic label for the pool this sentinel covers.
    label: &'static str,
    /// Bytes the sentinel's buffer or image asked for; the driver may round
    /// the allocation up.
    size: vk::DeviceSize,
}

/// Width and height of every image-backed [`ExportPoolSentinel`].
#[cfg(target_os = "linux")]
const EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS: u32 = 8;

/// Bytes every [`ExportPoolSentinel`] asks for: an 8×8 four-byte-per-pixel
/// image's worth. Kept tiny on purpose: a consumer-sized OPAQUE_FD sentinel
/// deterministically blocked the consumer's own allocation on NVIDIA.
#[cfg(target_os = "linux")]
const EXPORT_POOL_SENTINEL_BYTES: vk::DeviceSize = (EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS
    as vk::DeviceSize)
    * (EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS as vk::DeviceSize)
    * 4;

/// How an image-backed [`ExportPoolSentinel`] is tiled — the same tiling the
/// pool's real frames use, so its memory-type bits admit the pool's type.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum ExportPoolSentinelImageTiling<'a> {
    /// `VK_IMAGE_TILING_OPTIMAL`, no DRM modifier.
    Optimal,
    /// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`, the driver picking one of
    /// these render-target-capable modifiers.
    DrmFormatModifierFromCandidates(&'a [u64]),
}

/// The Vulkan object an [`ExportPoolSentinel`]'s allocation is bound to.
#[cfg(target_os = "linux")]
enum ExportPoolSentinelResource {
    Buffer(vk::Buffer),
    Image(vk::Image),
}

#[cfg(target_os = "linux")]
impl Drop for ExportPoolSentinel {
    fn drop(&mut self) {
        match self.resource {
            ExportPoolSentinelResource::Buffer(buffer) => unsafe {
                self.allocator.destroy_buffer(buffer, self.allocation);
            },
            ExportPoolSentinelResource::Image(image) => unsafe {
                self.allocator.destroy_image(image, self.allocation);
            },
        }
    }
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

/// Whether `physical_device` can export a timeline semaphore as an OPAQUE_FD.
#[cfg(target_os = "linux")]
fn physical_device_exports_an_opaque_fd_timeline_semaphore(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> bool {
    let mut timeline_semaphore_type = vk::SemaphoreTypeCreateInfo::builder()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0)
        .build();
    let external_semaphore_info = vk::PhysicalDeviceExternalSemaphoreInfo::builder()
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
        .push_next(&mut timeline_semaphore_type)
        .build();
    let mut external_semaphore_properties = vk::ExternalSemaphoreProperties::default();
    unsafe {
        instance.get_physical_device_external_semaphore_properties(
            physical_device,
            &external_semaphore_info,
            &mut external_semaphore_properties,
        )
    };
    external_semaphore_properties
        .external_semaphore_features
        .contains(vk::ExternalSemaphoreFeatureFlags::EXPORTABLE)
}

impl HostVulkanDevice {
    /// Create the engine's Vulkan device on the first physical device it can
    /// build on, in the loader's order with CPU-type devices last, logging
    /// every device passed over and why.
    ///
    /// On macOS/iOS, this loads MoltenVK and enables VK_EXT_metal_objects
    /// for Metal interoperability.
    ///
    /// Returns the device wrapped in [`Arc`] because construction runs
    /// engine init-time invariants that need [`Arc<Self>`] to call back
    /// into the public RHI constructors — most importantly, every
    /// export-capable VMA pool (DMA-BUF + OPAQUE_FD) is pre-warmed so
    /// the underlying [`vk::DeviceMemory`] block exists before any
    /// swapchain is created. NVIDIA's Linux driver caps fresh
    /// exportable allocations after a swapchain has been bound (see
    /// [`docs/learnings/nvidia-dma-buf-after-swapchain.md`]
    /// and `nvidia-opaque-fd-after-swapchain.md`); pre-warming here
    /// removes that footgun for every consumer at the engine layer.
    pub fn new() -> Result<Arc<Self>> {
        let vulkan_instance = Arc::new(HostVulkanInstance::new()?);

        let physical_devices = unsafe { vulkan_instance.instance.enumerate_physical_devices() }
            .map_err(|e| Error::GpuError(format!("Failed to enumerate devices: {e}")))?;

        if physical_devices.is_empty() {
            return Err(Error::GpuError(
                "A Vulkan loader is present but reports no GPU. Check that your GPU vendor's \
                 Vulkan driver is installed and `vulkaninfo` lists a device."
                    .into(),
            ));
        }

        let candidates = physical_devices
            .into_iter()
            .map(|physical_device| {
                let properties = unsafe {
                    vulkan_instance
                        .instance
                        .get_physical_device_properties(physical_device)
                };
                VulkanPhysicalDeviceCandidate {
                    physical_device,
                    device_name: properties
                        .device_name
                        .as_cstr()
                        .to_string_lossy()
                        .into_owned(),
                    device_type: properties.device_type,
                    device_properties: properties,
                }
            })
            .collect();

        let selection = build_on_the_first_candidate_that_accepts(candidates, |candidate| {
            Self::build_on_physical_device(&vulkan_instance, candidate)
        })?;
        Ok(selection.log_the_chosen_device_beside_every_pass_over())
    }

    /// Build the logical device, its queues, allocator and export pools on
    /// one candidate. A refusal drops only what this candidate created; the
    /// instance stays for the next.
    fn build_on_physical_device(
        vulkan_instance: &Arc<HostVulkanInstance>,
        candidate: &VulkanPhysicalDeviceCandidate,
    ) -> std::result::Result<Arc<Self>, VulkanDevicePassOverReason> {
        let instance = &vulkan_instance.instance;
        let physical_device = candidate.physical_device;
        let device_is_software = candidate.device_type == vk::PhysicalDeviceType::CPU;

        let device_props = candidate.device_properties;
        let device_name = candidate.device_name.clone();
        let min_storage_buffer_offset_alignment =
            device_props.limits.min_storage_buffer_offset_alignment;
        let max_memory_allocation_count = device_props.limits.max_memory_allocation_count;

        // PCI vendor IDs assigned by Khronos Vulkan registry. NVIDIA's
        // proprietary Linux driver (and currently NVK on the same
        // hardware) report 0x10DE; failed cross-device DMA-BUF imports
        // perturb the engine's OPAQUE_FD allocation accounting on this
        // driver family per issue #638, so consumers must skip the probe.
        // Mesa drivers (iris, radeonsi) tolerate the failed import; the
        // probe runs as before. If a future NVIDIA driver release fixes
        // this, the blocklist is a one-line update.
        const PCI_VENDOR_NVIDIA: u32 = 0x10DE;
        let supports_cross_device_dma_buf_probe = device_props.vendor_id != PCI_VENDOR_NVIDIA;

        // Probe third-party GPU library availability (nvJPEG, future
        // siblings like AMD AMF / Intel MFX / OptiX denoiser). Cheap
        // dlopen-based hint; backend construction validates the rest.
        // See `docs/architecture/third-party-gpu-backends.md` and the
        // `ThirdPartyGpuCapabilities` struct doc for the "when to lift
        // to engine-tier" trigger as siblings arrive.
        let third_party_gpu_capabilities = ThirdPartyGpuCapabilities::probe(device_props.vendor_id);

        // 5b. Query VkPhysicalDeviceIDProperties::deviceUUID via
        //     vkGetPhysicalDeviceProperties2. CUDA-Vulkan interop matches
        //     this against `cudaDeviceProp::uuid` (16 bytes) to pick the
        //     right CUDA device; on multi-GPU rigs (Jetson + dGPU,
        //     prosumer workstations) using a mismatched device fails
        //     silently when CUDA imports the OPAQUE_FD memory.
        let mut id_props = vk::PhysicalDeviceIDProperties::default();
        let mut subgroup_props = vk::PhysicalDeviceSubgroupProperties::default();
        let mut driver_props = vk::PhysicalDeviceDriverProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut id_props)
            .push_next(&mut subgroup_props)
            .push_next(&mut driver_props)
            .build();
        unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
        let subgroup_operation_support = VulkanSubgroupOperationSupport {
            driver_name: driver_props
                .driver_name
                .as_cstr()
                .to_string_lossy()
                .into_owned(),
            supported_stages: subgroup_props.supported_stages,
            supported_operations: subgroup_props.supported_operations,
        };
        tracing::info!(
            driver = %subgroup_operation_support.driver_name,
            supported_stages = ?subgroup_operation_support.supported_stages,
            supported_operations = ?subgroup_operation_support.supported_operations,
            "Vulkan subgroup operation support"
        );
        // `id_props.device_uuid` is `ByteArray<16>` (a transparent newtype
        // around `[u8; 16]`); use the upstream `From<ByteArray<N>> for [u8; N]`
        // impl to land in plain-array shape.
        let physical_device_uuid: [u8; 16] = id_props.device_uuid.into();

        // 6. Find graphics queue family
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };

        let queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| props.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(idx, _)| idx as u32)
            .ok_or(VulkanDevicePassOverReason::NoGraphicsQueueFamily)?;

        // 6b. Find dedicated transfer queue family (TRANSFER-only, no GRAPHICS/COMPUTE).
        //     Dedicated transfer queues use independent DMA engines for parallel data movement.
        //     Falls back to graphics queue if no dedicated transfer queue is available.
        let transfer_queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| {
                let has_transfer = props.queue_flags.contains(vk::QueueFlags::TRANSFER);
                let no_graphics = !props.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                let no_compute = !props.queue_flags.contains(vk::QueueFlags::COMPUTE);
                has_transfer && no_graphics && no_compute
            })
            .map(|(idx, _)| idx as u32)
            .unwrap_or(queue_family_index);

        if transfer_queue_family_index != queue_family_index {
            tracing::info!(
                "Dedicated transfer queue family found: {} (graphics: {})",
                transfer_queue_family_index,
                queue_family_index
            );
        } else {
            tracing::info!("No dedicated transfer queue — using graphics queue for transfers");
        }

        // 6c. Find video encode queue family (VIDEO_ENCODE_BIT_KHR).
        //     Dedicated encode queues have independent hardware encode engines.
        let video_encode_queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| props.queue_flags.contains(vk::QueueFlags::VIDEO_ENCODE_KHR))
            .map(|(idx, _)| idx as u32);

        if let Some(ve_family) = video_encode_queue_family_index {
            tracing::info!("Video encode queue family found: {}", ve_family);
        } else {
            tracing::info!("No video encode queue family available");
        }

        // 6d. Find video decode queue family (VIDEO_DECODE_BIT_KHR).
        let video_decode_queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| props.queue_flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR))
            .map(|(idx, _)| idx as u32);

        if let Some(vd_family) = video_decode_queue_family_index {
            tracing::info!("Video decode queue family found: {}", vd_family);
        } else {
            tracing::info!("No video decode queue family available");
        }

        // 6e. Find dedicated compute queue family (COMPUTE but not GRAPHICS).
        let compute_queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| {
                props.queue_flags.contains(vk::QueueFlags::COMPUTE)
                    && !props.queue_flags.contains(vk::QueueFlags::GRAPHICS)
            })
            .map(|(idx, _)| idx as u32);

        if let Some(cq_family) = compute_queue_family_index {
            tracing::info!("Dedicated compute queue family found: {}", cq_family);
        } else {
            tracing::info!("No dedicated compute queue family — using graphics queue for compute");
        }

        // 7. Create logical device with required extensions
        let queue_priorities = [1.0f32];
        let mut queue_create_infos = vec![
            vk::DeviceQueueCreateInfo::builder()
                .queue_family_index(queue_family_index)
                .queue_priorities(&queue_priorities)
                .build(),
        ];

        // Request separate video encode/decode queues if they're different families
        let mut requested_families = vec![queue_family_index];
        if transfer_queue_family_index != queue_family_index
            && !requested_families.contains(&transfer_queue_family_index)
        {
            requested_families.push(transfer_queue_family_index);
            queue_create_infos.push(
                vk::DeviceQueueCreateInfo::builder()
                    .queue_family_index(transfer_queue_family_index)
                    .queue_priorities(&queue_priorities)
                    .build(),
            );
        }
        if let Some(ve_family) = video_encode_queue_family_index {
            if !requested_families.contains(&ve_family) {
                requested_families.push(ve_family);
                queue_create_infos.push(
                    vk::DeviceQueueCreateInfo::builder()
                        .queue_family_index(ve_family)
                        .queue_priorities(&queue_priorities)
                        .build(),
                );
            }
        }
        if let Some(vd_family) = video_decode_queue_family_index {
            if !requested_families.contains(&vd_family) {
                requested_families.push(vd_family);
                queue_create_infos.push(
                    vk::DeviceQueueCreateInfo::builder()
                        .queue_family_index(vd_family)
                        .queue_priorities(&queue_priorities)
                        .build(),
                );
            }
        }
        if let Some(cq_family) = compute_queue_family_index {
            if !requested_families.contains(&cq_family) {
                requested_families.push(cq_family);
                queue_create_infos.push(
                    vk::DeviceQueueCreateInfo::builder()
                        .queue_family_index(cq_family)
                        .queue_priorities(&queue_priorities)
                        .build(),
                );
            }
        }

        // Device extensions
        let mut device_extensions: Vec<*const c_char> = Vec::new();

        let available_device_extension_properties =
            unsafe { instance.enumerate_device_extension_properties(physical_device, None) }
                .inspect_err(|e| {
                    tracing::warn!(
                        error = %e,
                        "vkEnumerateDeviceExtensionProperties failed; treating this device as \
                         exposing no optional extensions"
                    );
                })
                .unwrap_or_default();
        let available_device_ext_names =
            vulkan_extension_names_borrowed_from_properties(&available_device_extension_properties);

        let has_memory_budget = {
            let memory_budget_ext = c"VK_EXT_memory_budget";
            let advertised = available_device_ext_names.contains(&memory_budget_ext);
            if advertised {
                device_extensions.push(memory_budget_ext.as_ptr());
                tracing::info!("VK_EXT_memory_budget enabled");
            } else if cfg!(target_os = "macos") {
                tracing::info!(
                    "VK_EXT_memory_budget is not advertised; frame allocations are bounded by \
                     Metal's recommended working set, which the extension does not change"
                );
            } else {
                tracing::warn!(
                    "VK_EXT_memory_budget is not advertised, so the bound on frame allocations \
                     is an estimate: 80% of each heap, a heap that is not device-local also \
                     capped by the system's available memory, weighed against this runtime's \
                     own allocator usage rather than the driver's"
                );
            }
            advertised
        };

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let has_external_memory_host = {
            // A portability implementation that advertises the subset extension
            // requires it to be enabled (VUID-VkDeviceCreateInfo-pProperties-04451),
            // so this is probed rather than pushed blind. The subset MoltenVK
            // withholds is pointPolygons, samplerMipLodBias, tessellationIsolines
            // and tessellationPointMode; the engine uses no geometry or
            // tessellation shaders, so none of it is reachable from here.
            let portability_subset_ext = c"VK_KHR_portability_subset";
            if available_device_ext_names.contains(&portability_subset_ext) {
                device_extensions.push(portability_subset_ext.as_ptr());
                tracing::info!("VK_KHR_portability_subset enabled");
            }

            // Metal interop — importing an IOSurface-backed CVPixelBuffer as a
            // VkImage goes through this. Device-level, unlike the surface
            // extension requested on the instance.
            let metal_objects_ext = c"VK_EXT_metal_objects";
            if available_device_ext_names.contains(&metal_objects_ext) {
                device_extensions.push(metal_objects_ext.as_ptr());
                tracing::info!("VK_EXT_metal_objects enabled - Metal interop available");
            } else {
                tracing::warn!(
                    "VK_EXT_metal_objects not available - Metal interop will be limited"
                );
            }

            // Import an IOSurface's pages as a storage buffer, zero-copy.
            // MoltenVK 1.4.0 advertises it but refuses the spec's
            // `VkExternalMemoryBufferCreateInfo{HOST_ALLOCATION}`; 1.4.1
            // accepts it, so the refusal surfaces at import time.
            let external_memory_host_ext = c"VK_EXT_external_memory_host";
            let has_external_memory_host =
                available_device_ext_names.contains(&external_memory_host_ext);
            if has_external_memory_host {
                device_extensions.push(external_memory_host_ext.as_ptr());
                tracing::info!("VK_EXT_external_memory_host enabled");
            }
            has_external_memory_host
        };

        // On Linux, check for DMA-BUF external memory extensions
        #[cfg(target_os = "linux")]
        let mut has_acquire_unmodified = false;
        #[cfg(target_os = "linux")]
        let mut has_external_memory_host = false;
        #[cfg(target_os = "linux")]
        let mut has_external_memory_dma_buf = false;
        #[cfg(target_os = "linux")]
        let mut has_image_drm_format_modifier = false;
        #[cfg(target_os = "linux")]
        let mut has_external_semaphore_fd = false;
        #[cfg(target_os = "linux")]
        let has_external_memory = {
            let external_memory_ext = c"VK_KHR_external_memory";
            let external_memory_fd_ext = c"VK_KHR_external_memory_fd";
            let external_memory_dmabuf_ext = c"VK_EXT_external_memory_dma_buf";

            let has_external_memory = available_device_ext_names.contains(&external_memory_ext)
                && available_device_ext_names.contains(&external_memory_fd_ext);

            if has_external_memory {
                device_extensions.push(external_memory_ext.as_ptr());
                device_extensions.push(external_memory_fd_ext.as_ptr());

                if available_device_ext_names.contains(&external_memory_dmabuf_ext) {
                    device_extensions.push(external_memory_dmabuf_ext.as_ptr());
                    has_external_memory_dma_buf = true;
                    tracing::info!("VK_EXT_external_memory_dma_buf available");
                }

                let drm_format_modifier_ext = c"VK_EXT_image_drm_format_modifier";
                if available_device_ext_names.contains(&drm_format_modifier_ext) {
                    device_extensions.push(drm_format_modifier_ext.as_ptr());
                    has_image_drm_format_modifier = true;
                    tracing::info!("VK_EXT_image_drm_format_modifier available");
                }

                // Required for cross-process timeline-semaphore handoff via
                // sync-fd: the host exports with vkGetSemaphoreFdKHR and the
                // subprocess imports with vkImportSemaphoreFdKHR. Without
                // this extension, surface-adapter sync across the IPC seam
                // cannot land. VK_KHR_external_semaphore is core since 1.1
                // so only the fd-handle subset needs explicit opt-in.
                let external_semaphore_fd_ext = c"VK_KHR_external_semaphore_fd";
                if available_device_ext_names.contains(&external_semaphore_fd_ext) {
                    device_extensions.push(external_semaphore_fd_ext.as_ptr());
                    has_external_semaphore_fd = true;
                    tracing::info!("VK_KHR_external_semaphore_fd available");
                }

                // Optional extension for spec-correct cross-process
                // layout coordination (#633).
                // `VK_EXT_external_memory_acquire_unmodified` lets the
                // consumer's acquire barrier chain
                // `VkExternalMemoryAcquireUnmodifiedEXT` so producer-
                // side content survives the QFOT acquire. When absent,
                // `acquire_from_foreign` falls back to bridging
                // UNDEFINED → target (content discard permitted by
                // spec but preserved in practice on NVIDIA Linux
                // DMA-BUF — and structurally permanent on NVIDIA per
                // current driver roadmap). `VK_QUEUE_FAMILY_EXTERNAL`
                // (used for QFOT src/dst) is core Vulkan 1.1 and
                // doesn't need an optional probe.
                let acquire_unmodified_ext = c"VK_EXT_external_memory_acquire_unmodified";
                if available_device_ext_names.contains(&acquire_unmodified_ext) {
                    device_extensions.push(acquire_unmodified_ext.as_ptr());
                    has_acquire_unmodified = true;
                    tracing::info!("VK_EXT_external_memory_acquire_unmodified available");
                }

                // Optional: import a host-owned mapping as device memory, so a
                // compute pass can write another driver's buffer in place.
                let external_memory_host_ext = c"VK_EXT_external_memory_host";
                if available_device_ext_names.contains(&external_memory_host_ext) {
                    device_extensions.push(external_memory_host_ext.as_ptr());
                    has_external_memory_host = true;
                    tracing::info!("VK_EXT_external_memory_host available");
                }

                tracing::info!("Vulkan external memory extensions enabled");
            } else {
                tracing::info!("Vulkan external memory extensions not available");
            }

            has_external_memory
        };

        // Ray-tracing pipeline extension chain — gates
        // `VulkanRayTracingKernel` and `VulkanAccelerationStructure`.
        // All four extensions must be present together; partial
        // support is not a thing the engine exposes. The feature
        // structs are pushed into the device-create chain only when
        // every extension is available so devices without RT do not
        // fail at create time on a missing-feature error.
        #[cfg(target_os = "linux")]
        let has_ray_tracing_pipeline = {
            let accel_struct_ext = c"VK_KHR_acceleration_structure";
            let rt_pipeline_ext = c"VK_KHR_ray_tracing_pipeline";
            let deferred_host_ext = c"VK_KHR_deferred_host_operations";
            let pipeline_library_ext = c"VK_KHR_pipeline_library";
            let all_present = available_device_ext_names.contains(&accel_struct_ext)
                && available_device_ext_names.contains(&rt_pipeline_ext)
                && available_device_ext_names.contains(&deferred_host_ext)
                && available_device_ext_names.contains(&pipeline_library_ext);
            if all_present {
                device_extensions.push(accel_struct_ext.as_ptr());
                device_extensions.push(rt_pipeline_ext.as_ptr());
                device_extensions.push(deferred_host_ext.as_ptr());
                device_extensions.push(pipeline_library_ext.as_ptr());
                tracing::info!(
                    "Vulkan ray-tracing extensions enabled \
                     (acceleration_structure + ray_tracing_pipeline + \
                      deferred_host_operations + pipeline_library)"
                );
            } else {
                tracing::info!(
                    "Vulkan ray-tracing extensions not available \
                     (accel_struct={}, rt_pipeline={}, deferred_host={}, \
                      pipeline_library={})",
                    available_device_ext_names.contains(&accel_struct_ext),
                    available_device_ext_names.contains(&rt_pipeline_ext),
                    available_device_ext_names.contains(&deferred_host_ext),
                    available_device_ext_names.contains(&pipeline_library_ext),
                );
            }
            all_present
        };
        #[cfg(not(target_os = "linux"))]
        let has_ray_tracing_pipeline = false;

        // Enable VK_KHR_swapchain for windowed display rendering
        let has_hdr_metadata = {
            let swapchain_ext = c"VK_KHR_swapchain";
            if available_device_ext_names.contains(&swapchain_ext) {
                device_extensions.push(swapchain_ext.as_ptr());
                tracing::info!("VK_KHR_swapchain enabled");
            }

            // VK_KHR_dynamic_rendering is core since Vulkan 1.3 — no extension string needed.
            // Feature struct (PhysicalDeviceDynamicRenderingFeatures) is still enabled below.

            // VK_EXT_hdr_metadata — gates `vkSetHdrMetadataEXT`, which
            // attaches `VkHdrMetadataEXT` (mastering display + content
            // light) to a swapchain on transition into a PQ/HLG
            // colorspace. Only useful alongside `VK_KHR_swapchain` and
            // the instance-level `VK_EXT_swapchain_colorspace`.
            let hdr_metadata_ext = c"VK_EXT_hdr_metadata";
            let probe = available_device_ext_names.contains(&hdr_metadata_ext);
            if probe {
                device_extensions.push(hdr_metadata_ext.as_ptr());
                tracing::info!("VK_EXT_hdr_metadata enabled");
            }
            probe
        };

        // On Linux, check for Vulkan Video encode extensions
        // VK_KHR_synchronization2 is core since Vulkan 1.3 — no extension string needed.
        // Feature struct (PhysicalDeviceSynchronization2Features) is still enabled below.

        #[cfg(target_os = "linux")]
        let has_video_encode = {
            let has_video_queue = available_device_ext_names.contains(&c"VK_KHR_video_queue");
            let has_video_encode_queue =
                available_device_ext_names.contains(&c"VK_KHR_video_encode_queue");
            let has_video_encode_h264 =
                available_device_ext_names.contains(&c"VK_KHR_video_encode_h264");
            let has_video_encode_h265 =
                available_device_ext_names.contains(&c"VK_KHR_video_encode_h265");

            let all_present = has_video_queue
                && has_video_encode_queue
                && has_video_encode_h264
                && video_encode_queue_family_index.is_some();

            if all_present {
                device_extensions.push(c"VK_KHR_video_queue".as_ptr());
                device_extensions.push(c"VK_KHR_video_encode_queue".as_ptr());
                device_extensions.push(c"VK_KHR_video_encode_h264".as_ptr());
                if has_video_encode_h265 {
                    device_extensions.push(c"VK_KHR_video_encode_h265".as_ptr());
                    tracing::info!("Vulkan Video encode extensions enabled (H.264 + H.265)");
                } else {
                    tracing::info!("Vulkan Video encode extensions enabled (H.264 only)");
                }
            } else {
                tracing::info!(
                    "Vulkan Video encode not available (queue={}, encode_queue={}, h264={}, h265={}, queue_family={})",
                    has_video_queue,
                    has_video_encode_queue,
                    has_video_encode_h264,
                    has_video_encode_h265,
                    video_encode_queue_family_index.is_some()
                );
            }

            // Enable video_maintenance1 and push_descriptor if video encode is available
            // (required by the vulkan/video codec layer's encoder/decoder).
            if all_present {
                let video_maint1_ext = c"VK_KHR_video_maintenance1";
                if available_device_ext_names.contains(&video_maint1_ext) {
                    device_extensions.push(video_maint1_ext.as_ptr());
                }
                let push_desc_ext = c"VK_KHR_push_descriptor";
                if available_device_ext_names.contains(&push_desc_ext) {
                    device_extensions.push(push_desc_ext.as_ptr());
                }
            }

            all_present
        };

        // Check for Vulkan Video decode extensions
        #[cfg(target_os = "linux")]
        let has_video_decode = {
            let has_video_queue = available_device_ext_names.contains(&c"VK_KHR_video_queue");
            let has_video_decode_queue =
                available_device_ext_names.contains(&c"VK_KHR_video_decode_queue");
            let has_video_decode_h264 =
                available_device_ext_names.contains(&c"VK_KHR_video_decode_h264");
            let has_video_decode_h265 =
                available_device_ext_names.contains(&c"VK_KHR_video_decode_h265");

            let all_present = has_video_queue
                && has_video_decode_queue
                && has_video_decode_h264
                && video_decode_queue_family_index.is_some();

            if all_present {
                // VK_KHR_video_queue already enabled by encode block above (if present)
                if !has_video_encode {
                    device_extensions.push(c"VK_KHR_video_queue".as_ptr());
                }
                device_extensions.push(c"VK_KHR_video_decode_queue".as_ptr());
                device_extensions.push(c"VK_KHR_video_decode_h264".as_ptr());
                if has_video_decode_h265 {
                    device_extensions.push(c"VK_KHR_video_decode_h265".as_ptr());
                    tracing::info!("Vulkan Video decode extensions enabled (H.264 + H.265)");
                } else {
                    tracing::info!("Vulkan Video decode extensions enabled (H.264 only)");
                }
            } else {
                tracing::info!("Vulkan Video decode not available");
            }

            all_present
        };

        #[cfg(target_os = "linux")]
        let supports_external_memory = has_external_memory;
        #[cfg(not(target_os = "linux"))]
        let supports_external_memory = false;

        // DMA-BUF on a software device exists only where the process may open
        // `/dev/udmabuf`, a container's permissions; it is absent by rule.
        #[cfg(target_os = "linux")]
        let supports_dma_buf_export =
            has_external_memory_dma_buf && has_image_drm_format_modifier && !device_is_software;
        #[cfg(not(target_os = "linux"))]
        let supports_dma_buf_export = false;

        #[cfg(target_os = "linux")]
        let supports_semaphore_export = has_external_semaphore_fd
            && physical_device_exports_an_opaque_fd_timeline_semaphore(instance, physical_device);
        #[cfg(not(target_os = "linux"))]
        let supports_semaphore_export = false;

        #[cfg(not(target_os = "linux"))]
        let has_acquire_unmodified = false;
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        let has_external_memory_host = false;
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        let min_imported_host_pointer_alignment: u64 = 0;

        // Snapshot the RT pipeline properties (SBT alignment + handle
        // size + max recursion depth) before device creation. The query
        // is on the physical device; chained `VkPhysicalDeviceRayTracingPipelinePropertiesKHR`
        // is well-defined once the implementation advertises the extension.
        #[cfg(target_os = "linux")]
        let ray_tracing_properties = if has_ray_tracing_pipeline {
            let mut rt_props = vk::PhysicalDeviceRayTracingPipelinePropertiesKHR::builder().build();
            let mut props2 = vk::PhysicalDeviceProperties2::builder()
                .push_next(&mut rt_props)
                .build();
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            tracing::info!(
                "Ray-tracing pipeline properties: handle_size={}, handle_alignment={}, base_alignment={}, max_recursion_depth={}",
                rt_props.shader_group_handle_size,
                rt_props.shader_group_handle_alignment,
                rt_props.shader_group_base_alignment,
                rt_props.max_ray_recursion_depth,
            );
            Some(RayTracingPipelineProperties {
                shader_group_handle_size: rt_props.shader_group_handle_size,
                shader_group_handle_alignment: rt_props.shader_group_handle_alignment,
                shader_group_base_alignment: rt_props.shader_group_base_alignment,
                max_ray_recursion_depth: rt_props.max_ray_recursion_depth,
            })
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let ray_tracing_properties: Option<RayTracingPipelineProperties> = None;

        // The host-pointer import alignment is a physical-device property;
        // well-defined to chain once the extension is advertised.
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
        let min_imported_host_pointer_alignment: u64 = if has_external_memory_host {
            let mut host_props =
                vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::builder().build();
            let mut props2 = vk::PhysicalDeviceProperties2::builder()
                .push_next(&mut host_props)
                .build();
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            tracing::info!(
                min_imported_host_pointer_alignment =
                    host_props.min_imported_host_pointer_alignment,
                "VK_EXT_external_memory_host: host-pointer import alignment"
            );
            host_props.min_imported_host_pointer_alignment
        } else {
            0
        };

        #[cfg(target_os = "linux")]
        let supports_video_encode = has_video_encode;
        #[cfg(not(target_os = "linux"))]
        let supports_video_encode = false;

        #[cfg(target_os = "linux")]
        let supports_video_decode = has_video_decode;
        #[cfg(not(target_os = "linux"))]
        let supports_video_decode = false;

        // Dynamic rendering, timeline semaphores and synchronization2 are the
        // promoted-1.3 core the engine is written against; requesting them is
        // not optional on any platform. Synchronization2 is also a mandatory
        // dependency of VK_KHR_video_encode_queue. A device that refuses one
        // fails creation with FEATURE_NOT_PRESENT rather than being quietly
        // served a device the engine's call sites cannot use.
        let mut dynamic_rendering_features = vk::PhysicalDeviceDynamicRenderingFeatures::builder()
            .dynamic_rendering(true)
            .build();

        let mut timeline_semaphore_features =
            vk::PhysicalDeviceTimelineSemaphoreFeatures::builder()
                .timeline_semaphore(true)
                .build();

        let mut synchronization2_features = vk::PhysicalDeviceSynchronization2Features::builder()
            .synchronization2(true)
            .build();

        #[cfg(target_os = "linux")]
        let mut video_maintenance1_features =
            vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::builder()
                .video_maintenance1(true)
                .build();

        // samplerYcbcrConversion is required to create NV12 image views / samplers
        // with VK_KHR_sampler_ycbcr_conversion (core in 1.1) on the codec layer's path.
        // Without it, VUID-vkCreateSamplerYcbcrConversion-None-01648 fires every frame.
        let mut vulkan_1_1_features = vk::PhysicalDeviceVulkan11Features::builder()
            .sampler_ycbcr_conversion(true)
            .build();

        // Ray-tracing requires `bufferDeviceAddress` (core 1.2). Use the
        // standalone `VkPhysicalDeviceBufferDeviceAddressFeatures`
        // extension struct rather than `VkPhysicalDeviceVulkan12Features`
        // — Vulkan spec rejects mixing the 1.2 aggregate with the
        // standalone `*Features` structs it subsumes (timeline-semaphore
        // / synchronization2 are already pushed via their standalone
        // structs above; using the 1.2 aggregate here would trip
        // VUID-VkDeviceCreateInfo-pNext-02830).
        #[cfg(target_os = "linux")]
        let mut buffer_device_address_features =
            vk::PhysicalDeviceBufferDeviceAddressFeatures::builder()
                .buffer_device_address(true)
                .build();

        #[cfg(target_os = "linux")]
        let mut acceleration_structure_features =
            vk::PhysicalDeviceAccelerationStructureFeaturesKHR::builder()
                .acceleration_structure(true)
                .build();

        #[cfg(target_os = "linux")]
        let mut ray_tracing_pipeline_features =
            vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::builder()
                .ray_tracing_pipeline(true)
                .build();

        // shaderStorageImage{Read,Write}WithoutFormat (core 1.0) — the
        // tone-curve kernel's storage images are declared with SPIR-V
        // format `Unknown` so one pipeline serves BGRA8 / RGBA8 /
        // RGBA16F views without tripping the storage-image
        // format-match rule. Requested unconditionally: universal on
        // the platform floor (NVIDIA, radv, anv, llvmpipe all
        // advertise both), and a driver without them fails device
        // creation with FEATURE_NOT_PRESENT rather than reading
        // undefined values per dispatch. The shader-side capability
        // pairing is locked by `vulkan_tone_mapper::tests::`
        // `tone_curve_spirv_declares_the_without_format_capabilities`.
        let enabled_device_features = vk::PhysicalDeviceFeatures::builder()
            .shader_storage_image_read_without_format(true)
            .shader_storage_image_write_without_format(true)
            .build();

        let device_create_info = {
            #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
            let mut builder = vk::DeviceCreateInfo::builder()
                .queue_create_infos(&queue_create_infos)
                .enabled_extension_names(&device_extensions)
                .enabled_features(&enabled_device_features)
                .push_next(&mut dynamic_rendering_features)
                .push_next(&mut timeline_semaphore_features)
                .push_next(&mut synchronization2_features)
                .push_next(&mut vulkan_1_1_features);
            #[cfg(target_os = "linux")]
            if supports_video_encode || supports_video_decode {
                builder = builder.push_next(&mut video_maintenance1_features);
            }
            #[cfg(target_os = "linux")]
            if has_ray_tracing_pipeline {
                builder = builder
                    .push_next(&mut buffer_device_address_features)
                    .push_next(&mut acceleration_structure_features)
                    .push_next(&mut ray_tracing_pipeline_features);
            }
            builder.build()
        };

        let logical_device_destroyed_unless_taken = VulkanLogicalDeviceDestroyedUnlessTaken {
            device: unsafe { instance.create_device(physical_device, &device_create_info, None) }
                .map_err(|vulkan_error| {
                VulkanDevicePassOverReason::LogicalDeviceCreationRefused { vulkan_error }
            })?,
        };
        let device = logical_device_destroyed_unless_taken.device();

        #[cfg(target_os = "macos")]
        let metal_working_set_of_the_device = {
            let metal_device =
                match MetalDeviceWhoseWorkingSetBoundsFrameAllocations::exported_from_moltenvk(
                    device,
                ) {
                    Some(metal_device) => Some(metal_device),
                    None => {
                        tracing::warn!(
                            "MoltenVK exported no MTLDevice (VK_EXT_metal_objects absent or \
                             declined); frame allocations are bounded by the working set of \
                             the system's default Metal device instead"
                        );
                        MetalDeviceWhoseWorkingSetBoundsFrameAllocations::the_systems_default()
                    }
                }
                .ok_or_else(|| {
                    Error::GpuError(
                        "no Metal device could be read for the working set that bounds frame \
                         allocations: MoltenVK exported none and the system has no default"
                            .into(),
                    )
                })?;
            MetalWorkingSetOfTheDevice::new(metal_device)
        };

        // 8. Get the graphics queue
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        // 8a2. Get the transfer queue (may be same as graphics if no dedicated transfer family)
        let transfer_queue = unsafe { device.get_device_queue(transfer_queue_family_index, 0) };

        // 8b. Get the video encode queue (if available)
        let video_encode_queue = if supports_video_encode {
            video_encode_queue_family_index
                .map(|ve_family| unsafe { device.get_device_queue(ve_family, 0) })
        } else {
            None
        };

        // 8c. Get the video decode queue (if available)
        let video_decode_queue = if supports_video_decode {
            video_decode_queue_family_index
                .map(|vd_family| unsafe { device.get_device_queue(vd_family, 0) })
        } else {
            None
        };

        // 8d. Get the dedicated compute queue (if available)
        let compute_queue = compute_queue_family_index
            .map(|cq_family| unsafe { device.get_device_queue(cq_family, 0) });

        // 8e. One completion timeline per queue
        let queue_submission_completion_timelines =
            HostVulkanQueueSubmissionCompletionTimelines::new(device)?;

        // 9. Query memory properties (raw-allocation type choice, heap of a type)
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        // 10. Create VMA allocator for all GPU memory management
        //
        //     DMA-BUF export is NOT configured globally on VMA. Exportable allocations
        //     (pixel buffers, textures for IPC) go through dedicated VMA POOLS that
        //     carry VkExportMemoryAllocateInfo via pMemoryAllocateNext. This isolates
        //     export from the default pool — internal allocations (display textures,
        //     compute images) use the default pool which has NO export flags. NVIDIA
        //     rejects new DMA-BUF exportable DEVICE_LOCAL block allocations after
        //     swapchain creation, so keeping internal allocations export-free is the
        //     correct fix.
        let mut alloc_options = vma::AllocatorOptions::new(instance, device, physical_device);
        alloc_options.version = vulkanalia::Version::new(1, 4, 0);
        // BUFFER_DEVICE_ADDRESS — required so buffers created with
        // `VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT` (acceleration-
        // structure storage / scratch / vertex / index, the SBT) get
        // `VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT` on their device-memory
        // allocations. Only enabled when the RT extension chain is on,
        // since the underlying `bufferDeviceAddress` feature is gated
        // there. Non-RT devices keep the default flags so the
        // allocator's behavior is unchanged on hardware without RT.
        #[cfg(target_os = "linux")]
        if has_ray_tracing_pipeline {
            alloc_options.flags |= vma::AllocatorCreateFlags::BUFFER_DEVICE_ADDRESS;
        }
        if has_memory_budget {
            alloc_options.flags |= vma::AllocatorCreateFlags::EXT_MEMORY_BUDGET;
        }

        let allocator = Arc::new(
            unsafe { vma::Allocator::new(&alloc_options) }
                .map_err(|e| Error::GpuError(format!("Failed to create VMA allocator: {e}")))?,
        );

        // Run the EGL probe BEFORE creating pools — the tiled pool needs the
        // modifier list. Probe failure (no libEGL, no display, extension
        // missing) is non-fatal: the table stays empty and the tiled pool
        // is skipped, which makes render-target DMA-BUF allocations refuse
        // with an error rather than silently picking a sampler-only modifier.
        // See `docs/learnings/nvidia-egl-dmabuf-render-target.md`. The table
        // is the EGL default display's, not this device's, so a device that
        // exports no DMA-BUF takes none of it.
        #[cfg(target_os = "linux")]
        let drm_modifier_table = if supports_dma_buf_export {
            match drm_modifier_probe::default_display_drm_modifier_table_probed_once_per_process() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(
                        "EGL DRM modifier probe failed: {e} — render-target DMA-BUF \
                         pool will be unavailable; only sampler-only / linear DMA-BUF \
                         allocations will succeed"
                    );
                    Arc::new(DrmModifierTable::empty())
                }
            }
        } else {
            Arc::new(DrmModifierTable::empty())
        };

        // Build the OPAQUE_FD buffer pool — used by CUDA / OpenCL interop
        // (host allocates, exports OPAQUE_FD, CUDA `cudaImportExternalMemory`
        // imports it). Independent of the DMA-BUF pools below; needs its
        // own pool because VMA carries one chained `VkExportMemoryAllocateInfo`
        // per pool. Probe failure is non-fatal — callers refuse the
        // allocation if the pool isn't available.
        #[cfg(target_os = "linux")]
        let (opaque_fd_buffer_pool, opaque_fd_buffer_export_info) = if supports_external_memory {
            match Self::create_opaque_fd_buffer_pool(&allocator) {
                Ok((pool, export_info)) => (Some(pool), Some(export_info)),
                Err(e) => {
                    tracing::warn!(
                        "OPAQUE_FD buffer pool creation failed — CUDA/OpenCL interop will be unavailable: {e}"
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // Host-cached OPAQUE_FD buffer pool — the CPU-read sibling of the
        // pool above, used by the surface-export staging's HostVisible
        // residency. Absence is a real answer, not a failure: allocation
        // degrades to the write-combined pool, which the plan requires be
        // slower but never refused.
        #[cfg(target_os = "linux")]
        let (opaque_fd_buffer_pool_host_cached, opaque_fd_buffer_export_info_host_cached) =
            if supports_external_memory {
                match Self::create_opaque_fd_buffer_pool_host_cached(&allocator) {
                    Ok(Some((pool, export_info))) => (Some(pool), Some(export_info)),
                    Ok(None) => {
                        tracing::warn!(
                            "no cached exportable host-visible memory type on this device — \
                             CPU readback of a texture-backed surface keeps the \
                             write-combined staging and reads it far more slowly"
                        );
                        (None, None)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "host-cached OPAQUE_FD buffer pool creation failed — CPU readback \
                             staging falls back to the write-combined pool: {e}"
                        );
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

        // DEVICE_LOCAL OPAQUE_FD pool — the GPU-resident sibling. Pool
        // construction failure is non-fatal in the same way as the
        // HOST_VISIBLE pool: callers refuse the allocation when the pool
        // isn't available. Older drivers without external-memory support
        // skip this entirely.
        #[cfg(target_os = "linux")]
        let (opaque_fd_buffer_pool_device_local, opaque_fd_buffer_export_info_device_local) =
            if supports_external_memory {
                match Self::create_opaque_fd_buffer_pool_device_local(&allocator) {
                    Ok((pool, export_info)) => (Some(pool), Some(export_info)),
                    Err(e) => {
                        tracing::warn!(
                            "DEVICE_LOCAL OPAQUE_FD buffer pool creation failed — \
                             GPU-resident CUDA interop will be unavailable: {e}"
                        );
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

        // OPAQUE_FD image pool — DEVICE_LOCAL, OPTIMAL tiling.
        // Image-flavored sibling of the buffer pools above;
        // unblocks `cudaExternalMemoryGetMappedMipmappedArray` for the
        // tiled-image zero-copy Vulkan → CUDA path. Pool construction
        // failure is non-fatal: callers (`HostVulkanTexture::new_opaque_fd_export`)
        // refuse the allocation when the pool isn't available. Older
        // drivers without external-memory support skip this entirely.
        #[cfg(target_os = "linux")]
        let (opaque_fd_image_pool, opaque_fd_image_export_info) = if supports_external_memory {
            match Self::create_opaque_fd_image_pool(&allocator) {
                Ok((pool, export_info)) => (Some(pool), Some(export_info)),
                Err(e) => {
                    tracing::warn!(
                        "OPAQUE_FD image pool creation failed — CUDA `VkImage` interop \
                         (mipmapped-array import) will be unavailable: {e}"
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // Build DMA-BUF export pools only on a device that exports DMA-BUF.
        // The tiled pool is built only when the EGL probe found at least one
        // RT-capable modifier for BGRA — that's the probe shape.
        #[cfg(target_os = "linux")]
        let (
            dma_buf_buffer_pool,
            dma_buf_image_pool,
            dma_buf_image_pool_tiled,
            dma_buf_buffer_export_info,
            dma_buf_image_export_info,
            dma_buf_image_tiled_export_info,
        ) = if supports_dma_buf_export {
            // BGRA8_UNORM in Vulkan = ARGB8888 in DRM.
            let bgra_modifiers =
                drm_modifier_table.rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888);
            match Self::create_dma_buf_pools(&allocator, device, &memory_properties, bgra_modifiers)
            {
                Ok((bp, ip, tp, bi, ii, ti)) => (Some(bp), Some(ip), tp, Some(bi), Some(ii), ti),
                Err(e) => {
                    tracing::warn!(
                        "DMA-BUF export pools could not be created — falling back to \
                         default pool for exportable allocations (may fail on NVIDIA \
                         after swapchain creation): {e}"
                    );
                    (None, None, None, None, None, None)
                }
            }
        } else {
            (None, None, None, None, None, None)
        };

        tracing::info!(
            "Vulkan device initialized: {} (queue family {}, {} memory types, external_memory={}, cross_device_dma_buf_probe={}, acquire_unmodified={}, hdr_metadata={}, ray_tracing={}, vma=enabled, dma_buf_pools={})",
            device_name,
            queue_family_index,
            memory_properties.memory_type_count,
            supports_external_memory,
            supports_cross_device_dma_buf_probe,
            has_acquire_unmodified,
            has_hdr_metadata,
            has_ray_tracing_pipeline,
            {
                #[cfg(target_os = "linux")]
                {
                    dma_buf_buffer_pool.is_some()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    false
                }
            }
        );

        let frame_allocation_device_memory_budget_gate =
            FrameAllocationDeviceMemoryBudgetGate::new();
        let device = Self {
            vulkan_instance: Arc::clone(vulkan_instance),
            physical_device,
            memory_properties,
            max_memory_allocation_count,
            #[cfg(target_os = "linux")]
            has_memory_budget,
            frame_allocation_device_memory_budget_gate,
            #[cfg(target_os = "linux")]
            device_memory_budget_fresh_fetch_frame_index: std::sync::atomic::AtomicU32::new(0),
            #[cfg(target_os = "macos")]
            metal_working_set_of_the_device,
            device: logical_device_destroyed_unless_taken.take_the_logical_device(),
            queue,
            queue_family_index,
            transfer_queue_family_index,
            transfer_queue,
            device_name,
            supports_external_memory,
            device_is_software,
            supports_dma_buf_export,
            supports_semaphore_export,
            third_party_gpu_capabilities,
            supports_cross_device_dma_buf_probe,
            has_acquire_unmodified,
            has_external_memory_host,
            min_imported_host_pointer_alignment,
            has_hdr_metadata,
            has_ray_tracing_pipeline,
            ray_tracing_properties,
            subgroup_operation_support,
            supports_video_encode,
            supports_video_decode,
            video_encode_queue_family_index,
            video_encode_queue,
            video_decode_queue_family_index,
            video_decode_queue,
            compute_queue_family_index,
            compute_queue,
            allocator: Some(allocator),
            #[cfg(target_os = "linux")]
            dma_buf_buffer_pool,
            #[cfg(target_os = "linux")]
            dma_buf_image_pool,
            #[cfg(target_os = "linux")]
            dma_buf_image_pool_tiled,
            #[cfg(target_os = "linux")]
            _dma_buf_buffer_export_info: dma_buf_buffer_export_info,
            #[cfg(target_os = "linux")]
            _dma_buf_image_export_info: dma_buf_image_export_info,
            #[cfg(target_os = "linux")]
            _dma_buf_image_tiled_export_info: dma_buf_image_tiled_export_info,
            #[cfg(target_os = "linux")]
            opaque_fd_buffer_pool,
            #[cfg(target_os = "linux")]
            opaque_fd_buffer_pool_host_cached,
            #[cfg(target_os = "linux")]
            opaque_fd_buffer_pool_device_local,
            #[cfg(target_os = "linux")]
            opaque_fd_image_pool,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info: opaque_fd_buffer_export_info,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info_host_cached: opaque_fd_buffer_export_info_host_cached,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info_device_local: opaque_fd_buffer_export_info_device_local,
            #[cfg(target_os = "linux")]
            _opaque_fd_image_export_info: opaque_fd_image_export_info,
            physical_device_uuid,
            min_storage_buffer_offset_alignment,
            #[cfg(target_os = "linux")]
            drm_modifier_table,
            live_allocation_count: AtomicUsize::new(0),
            queue_submission_count: AtomicUsize::new(0),
            recorder_and_compute_kernel_fence_wait_count: AtomicUsize::new(0),
            queue_submission_completion_timelines: Some(queue_submission_completion_timelines),
            graphics_queue_mutex: Mutex::new(()),
            transfer_queue_mutex: Mutex::new(()),
            video_encode_queue_mutex: Mutex::new(()),
            video_decode_queue_mutex: Mutex::new(()),
            compute_queue_mutex: Mutex::new(()),
            device_mutex: Mutex::new(()),
            #[cfg(target_os = "linux")]
            export_pool_sentinels: Vec::new(),
        };

        // Wrapped in `Arc` before the pipeline-compiler pre-warm, which
        // builds its probe kernels through constructors taking
        // `&Arc<HostVulkanDevice>`.
        #[cfg(not(target_os = "linux"))]
        let device = Arc::new(device);

        #[cfg(target_os = "linux")]
        let device = {
            let mut device = Arc::new(device);
            let sentinels =
                Self::allocate_one_sentinel_per_export_pool(&device).map_err(|refusal| {
                    VulkanDevicePassOverReason::ExportPoolSentinelAllocationFailed {
                        refusal: refusal.to_string(),
                    }
                })?;
            // Sentinels hold the allocator, not `Arc<HostVulkanDevice>`
            // clones, so the Arc strong count is still 1 here and
            // `get_mut` succeeds.
            Arc::get_mut(&mut device)
                .expect(
                    "HostVulkanDevice has unique Arc ownership during construction; \
                     export sentinels must not hold Arc<HostVulkanDevice> clones",
                )
                .export_pool_sentinels = sentinels;
            // Provisional precaution: force the driver's shader-compiler init
            // now — single-threaded, at construction, before any plugin
            // processor setup runs. Each probe pipeline is built and dropped
            // purely for the compiler-init side effect. Compute + graphics
            // cover the two pipeline-creation paths the engine exercises today;
            // ray-tracing is a follow-up (it very likely shares the same
            // compiler `once`). See `prewarm_pipeline_compiler` for why this is
            // a precaution rather than a proven fix.
            Self::prewarm_pipeline_compiler(&device);
            Self::prewarm_graphics_pipeline(&device);
            device
        };

        Ok(device)
    }

    /// Build (and immediately drop) a trivial compute pipeline at device
    /// construction so the GPU driver's shader-compiler global init runs
    /// single-threaded in a controlled state.
    ///
    /// **Provisional precaution, not a proven fix.** On NVIDIA Linux,
    /// `libnvidia-gpucomp` does a lazy `pthread_once` init on the first
    /// `vkCreatePipelineLayout` in a process; the working hypothesis is
    /// that running it concurrently with other multi-plugin processor setup
    /// can corrupt the driver's heap. Forcing it here — before any plugin
    /// setup runs — closes that one window, mirroring how the VMA pool
    /// pre-warm above sidesteps NVIDIA's exportable-memory cap. It does NOT
    /// fix the broader concurrent pipeline-creation contention: only the
    /// FIRST init is serialized, while later concurrent `vkCreate*Pipelines`
    /// calls still race. The canonical fix is funneling every pipeline
    /// creation through one dedicated compile thread (the concurrent-setup
    /// race, tracked separately); this pre-warm is the cheap precaution kept
    /// until that lands. Its causal necessity is unproven — the crash that
    /// originally motivated it was later diagnosed as an IPC/wiring failure,
    /// and the residual driver race is latent (not reproduced outside a
    /// debugger). Unconditional on Linux today (only NVIDIA is suspected to
    /// need it; vendor-id gating is a tracked consideration). Non-fatal: a
    /// pre-warm failure only forgoes the precaution — the first real
    /// pipeline triggers the init instead.
    ///
    /// **Boxed-in — treat as a removal candidate, not load-bearing code.**
    /// To prove necessity: delete the two `prewarm_*` calls in `new()`, run
    /// a multi-plugin NVIDIA setup (the drone-racer host) across a
    /// dozen-plus cold starts, and watch for a `libnvidia-glcore` /
    /// `vkCreate*Pipelines` SIGSEGV during setup. No crash ⇒ dead weight:
    /// remove the calls and the `prewarm.comp` / `prewarm.{vert,frag}`
    /// shaders. The single-threaded pipeline-create funnel supersedes it
    /// outright; don't grow it (e.g. a ray-tracing probe) on speculation
    /// before necessity is shown.
    #[cfg(target_os = "linux")]
    fn prewarm_pipeline_compiler(device: &Arc<Self>) {
        const PREWARM_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/prewarm.spv"));
        // Kitchen-sink layout — NOT modeled on any kernel. The driver's
        // lazy compiler init fires on the first pipeline whose shader needs
        // real codegen; a no-op shader can be fast-pathed and skip it. So
        // the probe touches every compute descriptor kind (storage buffers,
        // uniform buffer, sampled texture, storage image) + the spec-min-max
        // 128-byte push block, with enough work that the driver can't
        // fast-path it — forcing the init regardless of which feature
        // engages codegen. Mirror this in prewarm.comp; don't trim to a
        // specific kernel's shape.
        let descriptor = crate::core::rhi::ComputeKernelDescriptor {
            entry_point: "main",
            label: "pipeline_compiler_prewarm",
            spv: PREWARM_SPV,
            bindings: &[
                crate::core::rhi::ComputeBindingSpec::storage_buffer(0),
                crate::core::rhi::ComputeBindingSpec::storage_buffer(1),
                crate::core::rhi::ComputeBindingSpec::uniform_buffer(2),
                crate::core::rhi::ComputeBindingSpec::sampled_texture(3),
                crate::core::rhi::ComputeBindingSpec::storage_image(4),
            ],
            push_constant_size: 128,
        };
        match crate::vulkan::rhi::VulkanComputeKernel::new(device, &descriptor) {
            // `_kernel` drops at the end of this arm — only the
            // compiler-init side effect persists process-globally.
            Ok(_kernel) => {
                tracing::info!("HostVulkanDevice pipeline-compiler pre-warmed");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "pipeline-compiler pre-warm failed (non-fatal — first real \
                     pipeline will trigger the driver's compiler init instead)"
                );
            }
        }
    }

    /// Build (and immediately drop) a trivial graphics pipeline at device
    /// construction so the driver's shader-compiler init for the
    /// `vkCreateGraphicsPipelines` path also runs single-threaded in a
    /// controlled state. Companion to [`Self::prewarm_pipeline_compiler`]
    /// (the compute path) — same provisional-precaution caveat; see it for
    /// why this is a precaution rather than a proven fix. Ray-tracing is a
    /// tracked follow-up. Trivial
    /// vertex + fragment stages, no vertex input, a dummy `Rgba8Unorm`
    /// color attachment via dynamic rendering, dynamic viewport/scissor —
    /// built and dropped purely for the compiler-init side effect.
    /// Non-fatal.
    #[cfg(target_os = "linux")]
    fn prewarm_graphics_pipeline(device: &Arc<Self>) {
        use crate::core::rhi::{
            AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState,
            GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor,
            GraphicsPipelineState, GraphicsPushConstants, GraphicsShaderStage,
            GraphicsShaderStageFlags, GraphicsStage, MultisampleState, PrimitiveTopology,
            RasterizationState, TextureFormat, VertexInputState,
        };
        const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/prewarm.vert.spv"));
        const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/prewarm.frag.spv"));
        const NO_BINDINGS: &[GraphicsBindingSpec] = &[];
        let stages = [
            GraphicsStage {
                stage: GraphicsShaderStage::Vertex,
                spv: VERT_SPV,
                entry_point: "main",
            },
            GraphicsStage {
                stage: GraphicsShaderStage::Fragment,
                spv: FRAG_SPV,
                entry_point: "main",
            },
        ];
        let descriptor = GraphicsKernelDescriptor {
            label: "pipeline_compiler_prewarm_graphics",
            stages: &stages,
            bindings: NO_BINDINGS,
            push_constants: GraphicsPushConstants {
                size: 4,
                stages: GraphicsShaderStageFlags::FRAGMENT,
            },
            pipeline_state: GraphicsPipelineState {
                topology: PrimitiveTopology::TriangleList,
                vertex_input: VertexInputState::None,
                rasterization: RasterizationState::default(),
                multisample: MultisampleState::default(),
                depth_stencil: DepthStencilState::Disabled,
                color_blend: ColorBlendState::Disabled {
                    color_write_mask: ColorWriteMask::RGBA,
                },
                attachment_formats: AttachmentFormats {
                    color: vec![TextureFormat::Rgba8Unorm],
                    depth: None,
                },
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            descriptor_sets_in_flight: 1,
        };
        match crate::vulkan::rhi::VulkanGraphicsKernel::new(device, &descriptor) {
            // `_kernel` drops here — only the compiler-init side effect persists.
            Ok(_kernel) => {
                tracing::info!("HostVulkanDevice graphics-pipeline compiler pre-warmed");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "graphics-pipeline pre-warm failed (non-fatal — first real \
                     graphics pipeline will trigger the compiler init instead)"
                );
            }
        }
    }

    /// Allocate one tiny sentinel out of every export pool the device
    /// created, **before any swapchain is bound**, for the device to hold
    /// for its life.
    ///
    /// On NVIDIA Linux a post-swapchain allocation of an exportable handle
    /// type succeeds only while a live allocation of that type anchors the
    /// driver's per-handle-type state, and nothing outside the engine is
    /// such an anchor: the shared pools free every frame of a flavour once
    /// it goes unused. The sentinels are that anchor, on every driver. They
    /// stay tiny because a consumer-sized OPAQUE_FD sentinel
    /// deterministically blocked the consumer's own allocation (#637).
    /// Pools the device did not create are skipped.
    #[cfg(target_os = "linux")]
    fn allocate_one_sentinel_per_export_pool(device: &Self) -> Result<Vec<ExportPoolSentinel>> {
        let bgra_sampled_copyable_image_usage = vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST;

        let mut sentinels: Vec<ExportPoolSentinel> = Vec::new();

        if let Some(pinned_pool) = device.dma_buf_buffer_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "dma_buf_host_visible",
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            )?);
        }

        if let Some(pinned_pool) = device.dma_buf_image_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_image_sentinel(
                device,
                pinned_pool,
                "dma_buf_image",
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                vk::Format::B8G8R8A8_UNORM,
                ExportPoolSentinelImageTiling::Optimal,
                bgra_sampled_copyable_image_usage,
            )?);
        }

        // The tiled pool exists only when this same modifier list is non-empty.
        if let Some(pinned_pool) = device.dma_buf_image_pool_tiled_pinned_to_one_memory_type() {
            let render_target_modifiers = device
                .drm_modifier_table()
                .rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888);
            if !render_target_modifiers.is_empty() {
                sentinels.push(make_export_pool_image_sentinel(
                    device,
                    pinned_pool,
                    "dma_buf_image_tiled",
                    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                    vk::Format::B8G8R8A8_UNORM,
                    ExportPoolSentinelImageTiling::DrmFormatModifierFromCandidates(
                        render_target_modifiers,
                    ),
                    bgra_sampled_copyable_image_usage | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                )?);
            }
        }

        if let Some(pinned_pool) = device.opaque_fd_buffer_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "opaque_fd_host_visible",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                MappedOpaqueFdBufferHostAccessPattern::SequentialWrite
                    .vma_allocation_create_flags(),
            )?);
        }

        // Its memory type differs from the write-combined pool's wherever the
        // device has a cached exportable type, so it is anchored separately.
        if let Some(pinned_pool) =
            device.opaque_fd_buffer_pool_host_cached_pinned_to_one_memory_type()
        {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "opaque_fd_host_cached",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                MappedOpaqueFdBufferHostAccessPattern::Random.vma_allocation_create_flags(),
            )?);
        }

        if let Some(pinned_pool) =
            device.opaque_fd_buffer_pool_device_local_pinned_to_one_memory_type()
        {
            sentinels.push(make_export_pool_buffer_sentinel(
                device,
                pinned_pool,
                "opaque_fd_device_local",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                vma::AllocationCreateFlags::empty(),
            )?);
        }

        // R8G8B8A8 is the CUDA-mappable layout the pool's real images use, so
        // the sentinel's memory-type bits match theirs. No in-tree consumer
        // allocates OPAQUE_FD images after a swapchain yet, so this anchor is
        // extrapolated from the buffer evidence rather than reproduced.
        if let Some(pinned_pool) = device.opaque_fd_image_pool_pinned_to_one_memory_type() {
            sentinels.push(make_export_pool_image_sentinel(
                device,
                pinned_pool,
                "opaque_fd_image",
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                vk::Format::R8G8B8A8_UNORM,
                ExportPoolSentinelImageTiling::Optimal,
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            )?);
        }

        for sentinel in &sentinels {
            tracing::info!(
                "HostVulkanDevice export pool sentinel retained: {} ({} bytes)",
                sentinel.label,
                sentinel.size,
            );
        }
        Ok(sentinels)
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

/// `DMA_BUF_MAGIC` from the kernel's `include/uapi/linux/magic.h`: the
/// `f_type` `fstatfs` reports for a DMA-BUF, which lives on the dmabuf
/// pseudo-filesystem from Linux 5.3 on.
#[cfg(target_os = "linux")]
const DMA_BUF_FILE_SYSTEM_MAGIC: u64 = 0x444d_4142;

/// Hand `candidate_fd` back when it names a DMA-BUF; close it and refuse
/// the import otherwise.
///
/// Whether a driver took the fd after a failed `vkAllocateMemory` depends
/// on why it refused: NVIDIA 595.84 closes a DMA-BUF it cannot import but
/// leaves a memfd open behind `ERROR_OUT_OF_DEVICE_MEMORY`. Only a refusal
/// before the call leaves an owner who knows the fd is still open.
#[cfg(target_os = "linux")]
fn refuse_a_file_descriptor_that_is_not_a_dma_buf(
    candidate_fd: std::os::fd::OwnedFd,
) -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd as _;

    let mut file_system_status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `candidate_fd` is open for the call and the pointer names a
    // writable `statfs`.
    if unsafe { libc::fstatfs(candidate_fd.as_raw_fd(), file_system_status.as_mut_ptr()) } != 0 {
        return Err(Error::GpuError(format!(
            "DMA-BUF import: fstatfs on fd {} failed: {}",
            candidate_fd.as_raw_fd(),
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: `fstatfs` returned 0 above, so it filled every field.
    let file_system_magic = unsafe { file_system_status.assume_init() }.f_type as u64;
    if file_system_magic != DMA_BUF_FILE_SYSTEM_MAGIC {
        return Err(Error::NotSupported(format!(
            "DMA-BUF import: fd {} is not a DMA-BUF (its file system magic is \
             {file_system_magic:#x}, a DMA-BUF's is {DMA_BUF_FILE_SYSTEM_MAGIC:#x})",
            candidate_fd.as_raw_fd()
        )));
    }
    Ok(candidate_fd)
}

/// How a mapped OPAQUE_FD buffer allocation is reached from the host.
///
/// One choice rather than two flags: VMA asserts when both HOST_ACCESS
/// bits are set on one allocation. A pool and every allocation it serves
/// must carry the same one, or the probe's `memoryTypeBits` stop matching
/// the real buffer's and the bind trips
/// VUID-vkBindBufferMemory-memory-01035.
#[cfg(target_os = "linux")]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum MappedOpaqueFdBufferHostAccessPattern {
    /// Producer-write memory — the shared OPAQUE_FD pool's hint.
    SequentialWrite,
    /// CPU-read memory; steers VMA to prefer a HOST_CACHED type.
    Random,
}

#[cfg(target_os = "linux")]
impl MappedOpaqueFdBufferHostAccessPattern {
    /// `MAPPED` plus this pattern's single HOST_ACCESS bit.
    pub(crate) fn vma_allocation_create_flags(self) -> vma::AllocationCreateFlags {
        vma::AllocationCreateFlags::MAPPED
            | match self {
                Self::SequentialWrite => vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                Self::Random => vma::AllocationCreateFlags::HOST_ACCESS_RANDOM,
            }
    }
}

/// Allocate one [`EXPORT_POOL_SENTINEL_BYTES`] buffer exportable as
/// `handle_type` out of `pinned_pool`, which must be one of `device`'s pools.
///
/// Bypasses [`super::HostVulkanBuffer`], whose `Arc<HostVulkanDevice>` would
/// be a reference cycle once the sentinel is stored on the device itself.
/// `required_flags` and `allocation_create_flags` (on top of
/// `DEDICATED_MEMORY`) must be the pool's real frames', or the sentinel's
/// memory-type bits can miss the type the pool is pinned to.
#[cfg(target_os = "linux")]
fn make_export_pool_buffer_sentinel(
    device: &HostVulkanDevice,
    pinned_pool: &VmaExportPoolPinnedToOneMemoryType,
    label: &'static str,
    handle_type: vk::ExternalMemoryHandleTypeFlags,
    required_flags: vk::MemoryPropertyFlags,
    allocation_create_flags: vma::AllocationCreateFlags,
) -> Result<ExportPoolSentinel> {
    let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(handle_type)
        .build();

    let buffer_info = vk::BufferCreateInfo::builder()
        .size(EXPORT_POOL_SENTINEL_BYTES)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_buffer_info);

    let alloc_opts = vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY | allocation_create_flags,
        required_flags,
        ..Default::default()
    };

    let (buffer, allocation) = unsafe { pinned_pool.pool.create_buffer(buffer_info, &alloc_opts) }
        .map_err(|e| {
            Error::GpuError(format!(
                "export pool '{label}' sentinel buffer allocation failed \
                 (size={EXPORT_POOL_SENTINEL_BYTES}): {e}"
            ))
        })?;

    Ok(ExportPoolSentinel {
        allocator: Arc::clone(device.allocator()),
        resource: ExportPoolSentinelResource::Buffer(buffer),
        allocation,
        label,
        size: EXPORT_POOL_SENTINEL_BYTES,
    })
}

/// Allocate one 8×8 four-byte-per-pixel image exportable as `handle_type`
/// out of `pinned_pool`, which must be one of `device`'s pools.
///
/// The image-backed counterpart of [`make_export_pool_buffer_sentinel`].
/// `format`, `tiling` and `usage` must be the pool's real frames', or the
/// sentinel's memory-type bits can miss the type the pool is pinned to.
#[cfg(target_os = "linux")]
fn make_export_pool_image_sentinel(
    device: &HostVulkanDevice,
    pinned_pool: &VmaExportPoolPinnedToOneMemoryType,
    label: &'static str,
    handle_type: vk::ExternalMemoryHandleTypeFlags,
    format: vk::Format,
    tiling: ExportPoolSentinelImageTiling<'_>,
    usage: vk::ImageUsageFlags,
) -> Result<ExportPoolSentinel> {
    let mut external_image_info = vk::ExternalMemoryImageCreateInfo::builder()
        .handle_types(handle_type)
        .build();
    let drm_format_modifier_candidates: &[u64] = match tiling {
        ExportPoolSentinelImageTiling::Optimal => &[],
        ExportPoolSentinelImageTiling::DrmFormatModifierFromCandidates(candidates) => candidates,
    };
    let mut drm_format_modifier_list_info = vk::ImageDrmFormatModifierListCreateInfoEXT::builder()
        .drm_format_modifiers(drm_format_modifier_candidates)
        .build();

    let image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS,
            height: EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external_image_info);
    let image_info = match tiling {
        ExportPoolSentinelImageTiling::Optimal => image_info.tiling(vk::ImageTiling::OPTIMAL),
        ExportPoolSentinelImageTiling::DrmFormatModifierFromCandidates(_) => image_info
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .push_next(&mut drm_format_modifier_list_info),
    };

    let alloc_opts = vma::AllocationOptions {
        flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ..Default::default()
    };

    let (image, allocation) = unsafe { pinned_pool.pool.create_image(image_info, &alloc_opts) }
        .map_err(|e| {
            Error::GpuError(format!(
                "export pool '{label}' sentinel image allocation failed \
                 ({EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS}x\
                 {EXPORT_POOL_SENTINEL_IMAGE_EXTENT_IN_PIXELS} {format:?}): {e}"
            ))
        })?;

    Ok(ExportPoolSentinel {
        allocator: Arc::clone(device.allocator()),
        resource: ExportPoolSentinelResource::Image(image),
        allocation,
        label,
        size: EXPORT_POOL_SENTINEL_BYTES,
    })
}

/// Pick a DEVICE_LOCAL memory type for a tiled DMA-BUF VkImage.
///
/// VMA's `find_memory_type_index_for_image_info` cannot probe images with
/// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (it asserts on the missing
/// plane aspect for multi-plane modifiers), so we create a real probe
/// image, query its memory requirements, and find a matching DEVICE_LOCAL
/// type ourselves. The probe image is destroyed before the function returns.
#[cfg(target_os = "linux")]
fn probe_tiled_dma_buf_memory_type(
    device: &vulkanalia::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    tiled_modifiers: &[u64],
) -> Result<u32> {
    let mut probe_modifier_info = vk::ImageDrmFormatModifierListCreateInfoEXT::builder()
        .drm_format_modifiers(tiled_modifiers);
    let mut probe_external_info = vk::ExternalMemoryImageCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let probe_image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width: 64,
            height: 64,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut probe_modifier_info)
        .push_next(&mut probe_external_info);

    let probe_image = unsafe { device.create_image(&probe_image_info, None) }
        .map_err(|e| Error::GpuError(format!("tiled-DMA-BUF probe vkCreateImage failed: {e}")))?;

    let mem_reqs = unsafe { device.get_image_memory_requirements(probe_image) };
    unsafe { device.destroy_image(probe_image, None) };

    // First DEVICE_LOCAL type that satisfies memory_type_bits, preferring
    // one without HOST_VISIBLE (main VRAM heap) on discrete GPUs.
    for pass in 0..2 {
        for i in 0..memory_properties.memory_type_count {
            if (mem_reqs.memory_type_bits & (1 << i)) == 0 {
                continue;
            }
            let flags = memory_properties.memory_types[i as usize].property_flags;
            if !flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
                continue;
            }
            if pass == 0 && flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
                continue;
            }
            return Ok(i);
        }
    }
    Err(Error::GpuError(format!(
        "no DEVICE_LOCAL memory type accepts tiled DMA-BUF probe image (memory_type_bits=0x{:x})",
        mem_reqs.memory_type_bits
    )))
}

impl HostVulkanDevice {
    /// Build VMA pools dedicated to DMA-BUF exportable allocations.
    ///
    /// Each pool is pinned to a memory type that supports the relevant property
    /// flags and carries VkExportMemoryAllocateInfo::DMA_BUF_EXT via
    /// pMemoryAllocateNext. The export info structs are heap-boxed and returned
    /// alongside the pools — the caller must keep them alive for the pool's
    /// lifetime (VMA stores raw pointers to them).
    ///
    /// The third pool (tiled) is conditional on `tiled_modifiers` being
    /// non-empty — we need at least one render-target-capable DRM modifier to
    /// create the probe image. When the EGL probe returned no modifiers (no
    /// libEGL, no display, extension missing) the tiled pool is `None` and
    /// callers fall back to refusing render-target allocations.
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    fn create_dma_buf_pools(
        allocator: &Arc<vma::Allocator>,
        device: &vulkanalia::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        tiled_modifiers: &[u64],
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        VmaExportPoolPinnedToOneMemoryType,
        Option<VmaExportPoolPinnedToOneMemoryType>,
        Box<vk::ExportMemoryAllocateInfo>,
        Box<vk::ExportMemoryAllocateInfo>,
        Option<Box<vk::ExportMemoryAllocateInfo>>,
    )> {
        // ── Find memory type for HOST_VISIBLE DMA-BUF exportable buffers ──
        // The probe must mirror the real buffer create info used by
        // `HostVulkanBuffer::new`, including the `ExternalMemoryBufferCreateInfo`
        // pNext chain — DMA-BUF external buffers have a narrower
        // `memoryTypeBits` than plain buffers, and omitting the chain lets VMA
        // pick a memory type the real buffer won't accept at bind time
        // (VUID-vkBindBufferMemory-memory-01035).
        let mut probe_buffer_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_buffer_external_info);
        let probe_buffer_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        let buffer_mem_type_idx = unsafe {
            allocator
                .find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_buffer_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for DMA-BUF buffer pool: {e}")))?;

        // ── Find memory type for DEVICE_LOCAL DMA-BUF exportable images ──
        // Same rationale: the real image (`HostVulkanTexture::new`) carries
        // `ExternalMemoryImageCreateInfo::DMA_BUF_EXT` which can narrow
        // `memoryTypeBits`.
        let mut probe_image_external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut probe_image_external_info);
        let probe_image_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let image_mem_type_idx = unsafe {
            allocator
                .find_memory_type_index_for_image_info(probe_image_info, &probe_image_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for DMA-BUF image pool: {e}")))?;

        // ── Box the VkExportMemoryAllocateInfo structs (need stable pointers) ──
        let mut buffer_export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build(),
        );
        let mut image_export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build(),
        );

        // ── Create the pools ──────────────────────────────────────────────────
        // VMA's pMemoryAllocateNext stores a raw pointer to the export info.
        // Box gives a stable heap address — we keep the Boxes alive by returning
        // them alongside the pools. Drop order (handled by HostVulkanDevice::drop):
        // pool → Box, so the pointer is valid for the pool's entire lifetime.
        let mut buffer_pool_options = vma::PoolOptions::default();
        buffer_pool_options = buffer_pool_options.push_next(buffer_export_info.as_mut());
        buffer_pool_options.memory_type_index = buffer_mem_type_idx;
        let buffer_pool = allocator
            .create_pool(&buffer_pool_options)
            .map_err(|e| Error::GpuError(format!("create DMA-BUF buffer pool: {e}")))?;

        let mut image_pool_options = vma::PoolOptions::default();
        image_pool_options = image_pool_options.push_next(image_export_info.as_mut());
        image_pool_options.memory_type_index = image_mem_type_idx;
        let image_pool = allocator
            .create_pool(&image_pool_options)
            .map_err(|e| Error::GpuError(format!("create DMA-BUF image pool: {e}")))?;

        // ── Tiled (DRM_FORMAT_MODIFIER) image pool ────────────────────────
        // Only created when the EGL probe returned at least one
        // render-target-capable modifier. VMA's
        // `find_memory_type_index_for_image_info` asserts on
        // `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (it can't infer the plane
        // aspect for multi-plane modifiers), so we probe via raw
        // `vkCreateImage` + `vkGetImageMemoryRequirements` and pick a
        // DEVICE_LOCAL type ourselves. The probe image is destroyed before
        // the pool is created — VMA only needs the memory type index.
        let (tiled_pool, tiled_export_info) = if tiled_modifiers.is_empty() {
            (None, None)
        } else {
            match probe_tiled_dma_buf_memory_type(device, memory_properties, tiled_modifiers) {
                Ok(tiled_mem_type_idx) => {
                    let mut tiled_export_info = Box::new(
                        vk::ExportMemoryAllocateInfo::builder()
                            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                            .build(),
                    );
                    let mut tiled_pool_options = vma::PoolOptions::default();
                    tiled_pool_options = tiled_pool_options.push_next(tiled_export_info.as_mut());
                    tiled_pool_options.memory_type_index = tiled_mem_type_idx;
                    match allocator.create_pool(&tiled_pool_options) {
                        Ok(tiled_pool) => {
                            tracing::info!(
                                "DMA-BUF VMA tiled image pool created — mem_type={}, modifiers_probed={}",
                                tiled_mem_type_idx,
                                tiled_modifiers.len(),
                            );
                            (
                                Some(VmaExportPoolPinnedToOneMemoryType {
                                    pool: tiled_pool,
                                    memory_type_index: tiled_mem_type_idx,
                                }),
                                Some(tiled_export_info),
                            )
                        }
                        Err(e) => {
                            tracing::warn!(
                                "DMA-BUF VMA tiled image pool creation failed (mem_type={}): {e} — render-target DMA-BUF allocations will be unavailable",
                                tiled_mem_type_idx,
                            );
                            (None, None)
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Tiled DMA-BUF memory-type probe failed: {e} — render-target DMA-BUF allocations will be unavailable",
                    );
                    (None, None)
                }
            }
        };

        tracing::info!(
            "DMA-BUF VMA pools created — buffer mem_type={}, image mem_type={}, tiled={}",
            buffer_mem_type_idx,
            image_mem_type_idx,
            tiled_pool.is_some(),
        );

        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool: buffer_pool,
                memory_type_index: buffer_mem_type_idx,
            },
            VmaExportPoolPinnedToOneMemoryType {
                pool: image_pool,
                memory_type_index: image_mem_type_idx,
            },
            tiled_pool,
            buffer_export_info,
            image_export_info,
            tiled_export_info,
        ))
    }

    /// Probe the memory type index an OPAQUE_FD-exportable HOST_VISIBLE
    /// buffer pool must pin itself to under `host_access_pattern`.
    ///
    /// The probe shape mirrors `HostVulkanBuffer`'s real allocations —
    /// HOST_VISIBLE | HOST_COHERENT, TRANSFER_SRC | TRANSFER_DST |
    /// STORAGE_BUFFER, chained `VkExternalMemoryBufferCreateInfo::OPAQUE_FD`.
    /// The probe's `memoryTypeBits` must match the real buffer's or the
    /// bind fails with VUID-vkBindBufferMemory-memory-01035.
    #[cfg(target_os = "linux")]
    fn probe_opaque_fd_host_visible_buffer_memory_type_index(
        allocator: &Arc<vma::Allocator>,
        host_access_pattern: MappedOpaqueFdBufferHostAccessPattern,
    ) -> Result<u32> {
        let mut probe_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | host_access_pattern.vma_allocation_create_flags(),
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        unsafe {
            allocator.find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for OPAQUE_FD buffer pool: {e}")))
    }

    /// Pin a VMA pool chaining `VkExportMemoryAllocateInfo::OPAQUE_FD` to
    /// `memory_type_index`.
    ///
    /// The export info `Box` is returned alongside the pool — callers
    /// must keep it alive for the pool's entire lifetime (VMA stores
    /// raw pointers via `pMemoryAllocateNext`).
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
        allocator: &Arc<vma::Allocator>,
        memory_type_index: u32,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = memory_type_index;
        let pool = allocator
            .create_pool(&pool_options)
            .map_err(|e| Error::GpuError(format!("create OPAQUE_FD buffer pool: {e}")))?;
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index,
            },
            export_info,
        ))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers.
    ///
    /// Used by CUDA / OpenCL interop: the host allocates from this pool,
    /// `vkGetMemoryFdKHR` with `OPAQUE_FD` produces a kernel fd, and the
    /// remote API (`cudaImportExternalMemory` etc.) imports the same
    /// underlying memory by fd. Sibling of [`Self::create_dma_buf_pools`];
    /// kept separate because a VMA pool can chain only one
    /// `VkExportMemoryAllocateInfo` per pool, and OPAQUE_FD vs DMA_BUF_EXT
    /// are mutually-exclusive handle types.
    ///
    /// `HOST_ACCESS_SEQUENTIAL_WRITE` is the right hint for this pool's
    /// producer-write consumers; the CPU-read sibling is
    /// [`Self::create_opaque_fd_buffer_pool_host_cached`].
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_buffer_pool(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let memory_type_index = Self::probe_opaque_fd_host_visible_buffer_memory_type_index(
            allocator,
            MappedOpaqueFdBufferHostAccessPattern::SequentialWrite,
        )?;
        let (pinned_pool, export_info) =
            Self::create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
                allocator,
                memory_type_index,
            )?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool created — mem_type={}",
            memory_type_index
        );
        Ok((pinned_pool, export_info))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE
    /// buffers on a HOST_CACHED memory type — the CPU-read sibling of
    /// [`Self::create_opaque_fd_buffer_pool`], whose write-combined memory
    /// the CPU reads roughly an order of magnitude slower.
    ///
    /// `Ok(None)` is the device having no cached exportable type, not a
    /// failure: `HOST_ACCESS_RANDOM` makes VMA *prefer* HOST_CACHED and
    /// `vk_mem_alloc.h` is explicit that it "cannot require it", so the
    /// probe succeeds on a cache-less device and hands back an uncached
    /// index. Reading the probed index's flags is therefore the detection,
    /// not a probe error.
    ///
    /// HOST_COHERENT stays required: the child-side OPAQUE_FD import binds
    /// the memory as HOST_VISIBLE | HOST_COHERENT and no flush or
    /// invalidate exists anywhere on this path.
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_buffer_pool_host_cached(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<
        Option<(
            VmaExportPoolPinnedToOneMemoryType,
            Box<vk::ExportMemoryAllocateInfo>,
        )>,
    > {
        let memory_type_index = Self::probe_opaque_fd_host_visible_buffer_memory_type_index(
            allocator,
            MappedOpaqueFdBufferHostAccessPattern::Random,
        )?;
        if !memory_type_index_is_host_cached(allocator.get_memory_properties(), memory_type_index) {
            return Ok(None);
        }
        let (pinned_pool, export_info) =
            Self::create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
                allocator,
                memory_type_index,
            )?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool (HOST_CACHED) created — mem_type={}",
            memory_type_index
        );
        Ok(Some((pinned_pool, export_info)))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL
    /// buffers (GPU-resident sibling of [`Self::create_opaque_fd_buffer_pool`]).
    ///
    /// Same `VkExportMemoryAllocateInfo::handleTypes = OPAQUE_FD`, but the
    /// memory-type probe excludes HOST_VISIBLE so VMA picks a true VRAM
    /// type instead of the BAR aperture or pinned-host fallback. Used by
    /// hot-path camera→cuda flows where pinned-host PCIe bandwidth is a
    /// real bottleneck (~16-32 GB/s vs ~600 GB/s on VRAM, RTX 3090).
    /// Pool storage is single-memory-type-index by VMA contract — that's
    /// why this is a separate pool from the HOST_VISIBLE one.
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_buffer_pool_device_local(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut probe_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let mem_type_idx = unsafe {
            allocator.find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_alloc_opts)
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "find memory type for DEVICE_LOCAL OPAQUE_FD buffer pool: {e}"
            ))
        })?;

        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = mem_type_idx;
        let pool = allocator.create_pool(&pool_options).map_err(|e| {
            Error::GpuError(format!("create DEVICE_LOCAL OPAQUE_FD buffer pool: {e}"))
        })?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool (DEVICE_LOCAL) created — mem_type={}",
            mem_type_idx
        );
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index: mem_type_idx,
            },
            export_info,
        ))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL
    /// **images** — image-flavored sibling of
    /// [`Self::create_opaque_fd_buffer_pool_device_local`].
    ///
    /// Probe shape matches [`super::HostVulkanTexture::new_opaque_fd_export`]
    /// exactly so the pool's `memoryTypeBits` matches the real
    /// allocations' bind requirements (VUID-vkBindImageMemory-memory-01047):
    ///
    /// - `vk::Format::R8G8B8A8_UNORM` — the canonical CUDA-mappable
    ///   8-bit RGBA format. Other CUDA-mappable variants (`R16G16B16A16_SFLOAT`,
    ///   `R32G32B32A32_SFLOAT`) share the same DEVICE_LOCAL memory-type
    ///   bits on every Vulkan driver shipping today.
    /// - `vk::ImageTiling::OPTIMAL` — LINEAR returns
    ///   `CUDA_ERROR_INVALID_VALUE` from `cuExternalMemoryGetMappedMipmappedArray`;
    ///   no DRM modifier (OPAQUE_FD + `DRM_FORMAT_MODIFIER_EXT` is not
    ///   a valid combination on NVIDIA).
    /// - `VkExternalMemoryImageCreateInfo::OPAQUE_FD` — the export handle
    ///   type that pairs with `cudaImportExternalMemory(OPAQUE_FD)` on
    ///   the consumer side.
    /// - `DEDICATED_MEMORY` allocation flag — CUDA's
    ///   `cudaExternalMemoryGetMappedMipmappedArray` imports the whole
    ///   `VkDeviceMemory` block.
    ///
    /// VMA's `find_memory_type_index_for_image_info` works for OPTIMAL
    /// tiling (unlike `DRM_FORMAT_MODIFIER_EXT`, which requires the
    /// raw `vkCreateImage` + `vkGetImageMemoryRequirements` probe used
    /// in `probe_tiled_dma_buf_memory_type`).
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_image_pool(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut probe_external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let mem_type_idx = unsafe {
            allocator.find_memory_type_index_for_image_info(probe_image_info, &probe_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for OPAQUE_FD image pool: {e}")))?;

        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = mem_type_idx;
        let pool = allocator
            .create_pool(&pool_options)
            .map_err(|e| Error::GpuError(format!("create OPAQUE_FD image pool: {e}")))?;

        tracing::info!(
            "OPAQUE_FD VMA image pool created — mem_type={}",
            mem_type_idx
        );
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index: mem_type_idx,
            },
            export_info,
        ))
    }

    /// Find a memory type that satisfies both the type filter and required properties.
    ///
    /// Used internally for DMA-BUF import (raw `vkAllocateMemory` path).
    /// VMA handles memory type selection for all non-import allocations.
    fn find_memory_type(
        &self,
        type_filter: u32,
        required_properties: vk::MemoryPropertyFlags,
    ) -> Result<u32> {
        // When DEVICE_LOCAL is requested, prefer types NOT also HOST_VISIBLE
        // (main VRAM heap) over types that are HOST_VISIBLE (BAR aperture).
        if required_properties.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
            // Pass 1: DEVICE_LOCAL without HOST_VISIBLE (main VRAM)
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                    && !flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
                {
                    return Ok(i);
                }
            }
            // Pass 2: any DEVICE_LOCAL (including BAR aperture)
            for i in 0..self.memory_properties.memory_type_count {
                let flags = self.memory_properties.memory_types[i as usize].property_flags;
                if (type_filter & (1 << i)) != 0
                    && flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                {
                    return Ok(i);
                }
            }
        }

        // Standard path: find first matching type
        for i in 0..self.memory_properties.memory_type_count {
            let type_supported = (type_filter & (1 << i)) != 0;
            let properties_supported = self.memory_properties.memory_types[i as usize]
                .property_flags
                .contains(required_properties);
            if type_supported && properties_supported {
                return Ok(i);
            }
        }
        Err(Error::GpuError(format!(
            "No suitable memory type found (filter: 0x{:x}, required: {:?})",
            type_filter, required_properties
        )))
    }

    /// Create a texture on this device.
    pub fn create_texture(self: &Arc<Self>, desc: &TextureDescriptor) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new(self, desc)
    }

    /// Create a non-exportable device-local texture for same-process consumers.
    ///
    /// Unlike [`create_texture`], this skips the DMA-BUF export pool and
    /// allocates from the default VMA pool. Use this for textures that never
    /// cross process boundaries — NVIDIA Linux caps DMA-BUF exportable
    /// allocations after swapchain creation, so minimizing exportable
    /// allocations is important (see `docs/learnings/nvidia-dma-buf-after-swapchain.md`).
    pub fn create_texture_local(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new_device_local(self, desc)
    }

    /// Create an OPAQUE_FD-exportable texture whose memory a foreign device
    /// (a helper child's Vulkan import, CUDA) can adopt whole.
    ///
    /// Thin wrapper over [`HostVulkanTexture::new_opaque_fd_export`]; that
    /// constructor's usage and format rules apply.
    #[cfg(target_os = "linux")]
    pub fn create_texture_opaque_fd_export(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new_opaque_fd_export(self, desc)
    }

    /// Create a texture over a fresh private IOSurface — see
    /// [`HostVulkanTexture::new_iosurface_backed`].
    #[cfg(target_os = "macos")]
    pub fn create_texture_iosurface_backed(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new_iosurface_backed(self, desc)
    }

    /// Create a DMA-BUF-exportable texture with an explicit DRM format
    /// modifier, importable by a foreign process as the same tiled image.
    ///
    /// The modifier candidates come from this device's EGL probe
    /// (`external_only=FALSE` — render-target-capable). Errors when the
    /// format has no DRM FOURCC or the probe advertised no RT-capable
    /// modifier for it — there is no fallback to LINEAR (sampler-only on
    /// NVIDIA, see `docs/learnings/nvidia-egl-dmabuf-render-target.md`).
    #[cfg(target_os = "linux")]
    pub fn create_texture_render_target_dma_buf(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        let Some(fourcc) = drm_modifier_probe::fourcc::drm_fourcc_for_texture_format(desc.format)
        else {
            return Err(Error::GpuError(format!(
                "create_texture_render_target_dma_buf: format {:?} has no DRM FOURCC \
                 mapping; it cannot cross a process boundary as a DMA-BUF image",
                desc.format
            )));
        };
        let modifiers: Vec<u64> = self.drm_modifier_table().rt_modifiers(fourcc).to_vec();
        if modifiers.is_empty() {
            return Err(Error::GpuError(format!(
                "create_texture_render_target_dma_buf: no RT-capable DRM modifier for \
                 {:?} (fourcc=0x{fourcc:08x}); EGL probe returned an empty list",
                desc.format
            )));
        }
        HostVulkanTexture::new_render_target_dma_buf(self, desc, &modifiers)
    }

    /// Create a VulkanCommandQueue wrapper for the shared command queue.
    pub fn create_command_queue_wrapper(self: &Arc<Self>) -> VulkanCommandQueue {
        VulkanCommandQueue::new(Arc::clone(self), self.queue, self.queue_family_index)
    }

    /// Get the device name.
    #[allow(dead_code)]
    pub fn name(&self) -> String {
        self.device_name.clone()
    }

    /// Get the Vulkan entry point loader.
    pub fn entry(&self) -> &vulkanalia::Entry {
        &self.vulkan_instance.entry
    }

    /// Get the Vulkan instance.
    #[allow(dead_code)]
    pub fn instance(&self) -> &vulkanalia::Instance {
        &self.vulkan_instance.instance
    }

    /// Get the Vulkan physical device.
    #[allow(dead_code)]
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    /// Get the Vulkan logical device.
    pub fn device(&self) -> &vulkanalia::Device {
        &self.device
    }

    /// Get the graphics queue.
    #[allow(dead_code)]
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }

    /// Get the graphics queue family index.
    #[allow(dead_code)]
    pub fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    /// Get the dedicated transfer queue family index (falls back to graphics queue).
    #[allow(dead_code)]
    pub fn transfer_queue_family_index(&self) -> u32 {
        self.transfer_queue_family_index
    }

    /// Get the transfer queue handle.
    pub fn transfer_queue(&self) -> vk::Queue {
        self.transfer_queue
    }

    /// Whether `VK_KHR_external_memory` and `VK_KHR_external_memory_fd` are
    /// enabled.
    pub fn supports_external_memory(&self) -> bool {
        self.supports_external_memory
    }

    /// Whether the device is a CPU-type software rasterizer.
    pub fn device_is_software(&self) -> bool {
        self.device_is_software
    }

    /// Whether frames can cross as DMA-BUF; never on a software device.
    pub fn supports_dma_buf_export(&self) -> bool {
        self.supports_dma_buf_export
    }

    /// Whether a timeline semaphore can be exported as an OPAQUE_FD.
    pub fn supports_semaphore_export(&self) -> bool {
        self.supports_semaphore_export
    }

    /// Whether the driver tolerates a failed cross-device DMA-BUF import
    /// attempt without perturbing per-handle-type kernel accounting for
    /// other export handle types (notably OPAQUE_FD).
    ///
    /// Cross-device meaning: the source DMA-BUF was allocated by a
    /// different DRM device (e.g. UVC camera kernel driver) than this
    /// `VkDevice`. Same-device DMA-BUF imports (e.g. swapchain images
    /// from the compositor) are unaffected and always safe.
    ///
    /// Returns `false` on NVIDIA Linux today: failed cross-device DMA-BUF
    /// imports flake the engine's OPAQUE_FD allocation budget for the
    /// device's lifetime, even with the engine-side OPAQUE_FD sentinels
    /// from #637 in place. Returns `true` on Mesa drivers (Intel iris,
    /// AMD radeonsi) where the probe runs without observable side
    /// effects.
    pub fn supports_cross_device_dma_buf_probe(&self) -> bool {
        self.supports_cross_device_dma_buf_probe
    }

    /// Best-effort hint about which third-party GPU compute libraries
    /// (nvJPEG today; future siblings as the workspace grows) are
    /// available to integrate against this device. Probed once at
    /// construction; cheap to clone.
    ///
    /// Backend-using libraries (e.g. `vulkan-jpeg`'s `SimpleJpegDecoder`)
    /// consult this to decide which backend to attempt first. A `true`
    /// field is a *hint*, not a guarantee — the matching backend's
    /// `new()` may still fail for reasons not covered by the probe
    /// (CUDA context init, GPU compute-capability mismatch, etc.).
    /// Callers handle that by falling back to a lower-tier backend.
    ///
    /// See [`ThirdPartyGpuCapabilities`] and
    /// `docs/architecture/third-party-gpu-backends.md` for the
    /// integration shape and the trigger that lifts the JPEG-specific
    /// backend trait to an engine-tier primitive as siblings arrive.
    pub fn third_party_gpu_capabilities(&self) -> ThirdPartyGpuCapabilities {
        self.third_party_gpu_capabilities
    }

    /// Whether Vulkan Video encode extensions are available.
    #[allow(dead_code)]
    pub fn supports_video_encode(&self) -> bool {
        self.supports_video_encode
    }

    /// Get the video encode queue family index (if available).
    #[allow(dead_code)]
    pub fn video_encode_queue_family_index(&self) -> Option<u32> {
        self.video_encode_queue_family_index
    }

    /// Get the video encode queue (if available).
    #[allow(dead_code)]
    pub fn video_encode_queue(&self) -> Option<vk::Queue> {
        self.video_encode_queue
    }

    /// Whether this device supports Vulkan Video decode.
    #[allow(dead_code)]
    pub fn supports_video_decode(&self) -> bool {
        self.supports_video_decode
    }

    /// Get the video decode queue family index (if available).
    #[allow(dead_code)]
    pub fn video_decode_queue_family_index(&self) -> Option<u32> {
        self.video_decode_queue_family_index
    }

    /// Get the video decode queue (if available).
    #[allow(dead_code)]
    pub fn video_decode_queue(&self) -> Option<vk::Queue> {
        self.video_decode_queue
    }

    /// Get the dedicated compute queue family index (if available).
    #[allow(dead_code)]
    pub fn compute_queue_family_index(&self) -> Option<u32> {
        self.compute_queue_family_index
    }

    /// Get the dedicated compute queue (if available).
    #[allow(dead_code)]
    pub fn compute_queue(&self) -> Option<vk::Queue> {
        self.compute_queue
    }

    // ---- Thread-safe queue submission ----
    //
    // Vulkan requires external synchronization for vkQueueSubmit on the same
    // VkQueue from multiple threads. NVIDIA's driver also has internal
    // thread-safety issues during concurrent device-level operations.
    // These methods acquire per-queue mutexes before submitting.

    /// Which of this device's queues a handle is; `None` for a handle this
    /// device never handed out. Aliased handles resolve to the first match.
    fn submission_queue_for(&self, queue: vk::Queue) -> Option<HostVulkanSubmissionQueue> {
        if queue == self.queue {
            Some(HostVulkanSubmissionQueue::Graphics)
        } else if queue == self.transfer_queue {
            Some(HostVulkanSubmissionQueue::Transfer)
        } else if self.video_encode_queue == Some(queue) {
            Some(HostVulkanSubmissionQueue::VideoEncode)
        } else if self.video_decode_queue == Some(queue) {
            Some(HostVulkanSubmissionQueue::VideoDecode)
        } else if self.compute_queue == Some(queue) {
            Some(HostVulkanSubmissionQueue::Compute)
        } else {
            None
        }
    }

    /// Look up the mutex that guards a given queue handle.
    fn mutex_for_queue(&self, queue: vk::Queue) -> &Mutex<()> {
        match self.submission_queue_for(queue) {
            Some(HostVulkanSubmissionQueue::Graphics) | None => &self.graphics_queue_mutex,
            Some(HostVulkanSubmissionQueue::Transfer) => &self.transfer_queue_mutex,
            Some(HostVulkanSubmissionQueue::VideoEncode) => &self.video_encode_queue_mutex,
            Some(HostVulkanSubmissionQueue::VideoDecode) => &self.video_decode_queue_mutex,
            Some(HostVulkanSubmissionQueue::Compute) => &self.compute_queue_mutex,
        }
    }

    /// Submit command buffers to a queue with per-queue mutex
    /// synchronization, signalling that queue's completion timeline after
    /// the last submit. A queue this device did not hand out is refused:
    /// no completion timeline would record its work, so the shared pools
    /// could free memory it still names.
    pub unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        let Some(submission_queue) = self.submission_queue_for(queue) else {
            return Err(Error::GpuError(format!(
                "queue submission to {queue:?}, a queue this device did not hand out"
            )));
        };
        let Some(completion_timelines) = self.queue_submission_completion_timelines.as_ref() else {
            return Err(Error::GpuError(
                "queue submission to a device whose queue completion timelines are already \
                 destroyed"
                    .into(),
            ));
        };
        let _lock = self
            .mutex_for_queue(queue)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe {
            completion_timelines.submit_signaling_completion_timeline(
                submission_queue,
                queue,
                submits,
                fence,
            )
        }?;
        self.queue_submission_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// The latest submission made so far on every queue of this device.
    pub(crate) fn snapshot_of_queue_submissions_made_so_far(
        &self,
    ) -> HostVulkanQueueSubmissionsSnapshot {
        self.queue_submission_completion_timelines
            .as_ref()
            .map(HostVulkanQueueSubmissionCompletionTimelines::snapshot_of_submissions_made_so_far)
            .unwrap_or_default()
    }

    /// Whether every queue submission the snapshot names has completed,
    /// read without blocking.
    pub(crate) fn every_queue_submission_in_snapshot_has_completed(
        &self,
        snapshot: &HostVulkanQueueSubmissionsSnapshot,
    ) -> Result<bool> {
        match self.queue_submission_completion_timelines.as_ref() {
            Some(completion_timelines) => {
                completion_timelines.every_submission_in_snapshot_has_completed(snapshot)
            }
            None => Ok(true),
        }
    }

    /// Block until every queue submission the snapshot names has completed
    /// or `timeout` passes; `Ok(false)` on timeout.
    pub(crate) fn wait_for_every_queue_submission_in_snapshot(
        &self,
        snapshot: &HostVulkanQueueSubmissionsSnapshot,
        timeout: std::time::Duration,
    ) -> Result<bool> {
        match self.queue_submission_completion_timelines.as_ref() {
            Some(completion_timelines) => {
                completion_timelines.wait_for_every_submission_in_snapshot(snapshot, timeout)
            }
            None => Ok(true),
        }
    }

    /// Block until `fences` signal, counting the stall.
    ///
    /// The counted seam for work that trades submissions for stalls: batching N
    /// dispatches into one recording is a claim about how many times the caller
    /// blocks, and this is where that is observable rather than inferred from
    /// elapsed time. Routed through by every
    /// [`RhiCommandRecorder`](super::RhiCommandRecorder) drain and by
    /// [`VulkanComputeKernel::dispatch`](super::VulkanComputeKernel::dispatch);
    /// other engine fence waits call `vkWaitForFences` directly and are not
    /// counted, so a reading is only about a batch when nothing else on this
    /// device is recording.
    ///
    /// `waited_by` takes `format_args!` so the caller's label is rendered only
    /// when the wait actually fails — this sits on the per-frame path.
    pub fn wait_for_fences_blocking_counted(
        &self,
        fences: &[vk::Fence],
        waited_by: std::fmt::Arguments<'_>,
    ) -> Result<()> {
        self.recorder_and_compute_kernel_fence_wait_count
            .fetch_add(1, Ordering::Relaxed);
        unsafe { self.device.wait_for_fences(fences, true, u64::MAX) }
            .map(|_| ())
            .map_err(|e| Error::GpuError(format!("{waited_by}: wait_for_fences: {e}")))
    }

    /// Every `vkQueueSubmit2` this device has made.
    pub fn queue_submission_count(&self) -> usize {
        self.queue_submission_count.load(Ordering::Relaxed)
    }

    /// Fence waits made through [`Self::wait_for_fences_blocking_counted`].
    pub fn recorder_and_compute_kernel_fence_wait_count(&self) -> usize {
        self.recorder_and_compute_kernel_fence_wait_count
            .load(Ordering::Relaxed)
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

    /// Present to a queue with per-queue mutex synchronization.
    pub unsafe fn present_to_queue(
        &self,
        queue: vk::Queue,
        present_info: &vk::PresentInfoKHR,
    ) -> std::result::Result<vk::SuccessCode, vk::ErrorCode> {
        let _lock = self
            .mutex_for_queue(queue)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.queue_present_khr(queue, present_info) }
    }

    /// Acquire the device-level mutex for resource creation operations.
    pub fn lock_device(&self) -> std::sync::MutexGuard<'_, ()> {
        self.device_mutex.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `vkDeviceWaitIdle` with the threading discipline the Vulkan spec
    /// requires. Per the spec, `vkDeviceWaitIdle` is externally
    /// synchronized over **every `VkQueue` the device owns** — calling
    /// it concurrently with `vkQueueSubmit2` / `vkQueuePresentKHR` on
    /// any of those queues trips
    /// `UNASSIGNED-Threading-MultipleThreads-Write`. Acquires all
    /// per-queue mutexes (in a fixed lock order so there's no deadlock
    /// against [`Self::submit_to_queue`] / [`Self::present_to_queue`],
    /// each of which takes only a single queue mutex) and then the
    /// device mutex before calling through.
    ///
    /// Every in-tree consumer should reach `vkDeviceWaitIdle` through
    /// this helper, not raw `self.device().device_wait_idle()`.
    pub fn wait_idle(&self) -> Result<()> {
        // Fixed lock order: graphics → transfer → compute → video-encode →
        // video-decode → device. `submit_to_queue` / `present_to_queue`
        // only ever take ONE queue mutex, so any order here is deadlock-
        // safe against them; this order matches struct-field declaration
        // for readability.
        let _g = self
            .graphics_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _t = self
            .transfer_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _c = self
            .compute_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ve = self
            .video_encode_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _vd = self
            .video_decode_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _dev = self.device_mutex.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.device_wait_idle() }
            .map_err(|e| Error::GpuError(format!("device_wait_idle failed: {e}")))
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
    /// Copy a host-visible VkBuffer to a device-local VkImage (RGBA upload).
    ///
    /// Transitions the image UNDEFINED → TRANSFER_DST → the destination's
    /// usage-legal terminal layout, which it returns for the caller to
    /// publish.
    ///
    /// Creates and destroys a transient command pool + command buffer + fence
    /// per call — fine for one-shot uses (texture init from a fixture), wrong
    /// for the per-frame hot path. Hot-path callers should use
    /// [`HostVulkanUploadResources`] + [`Self::upload_buffer_to_image_amortized`]
    /// instead.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub unsafe fn upload_buffer_to_image(
        &self,
        src_buffer: vk::Buffer,
        dst_texture: &HostVulkanTexture,
        width: u32,
        height: u32,
    ) -> crate::core::Result<PixelBufferUploadFinalTextureLayout> {
        use crate::core::Error;

        let device = self.device();
        let qf = self.queue_family_index;

        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| Error::GpuError(format!("upload cmd pool: {e}")))?;

        let cb = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| Error::GpuError(format!("upload cmd buf: {e}")))?[0];

        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .map_err(|e| Error::GpuError(format!("upload fence: {e}")))?;

        let final_layout = unsafe {
            self.record_and_submit_buffer_to_image(
                cb,
                fence,
                src_buffer,
                dst_texture,
                width,
                height,
            )
        };

        unsafe { device.destroy_fence(fence, None) };
        unsafe { device.destroy_command_pool(pool, None) };

        final_layout
    }

    /// Amortized hot-path variant of [`Self::upload_buffer_to_image`].
    ///
    /// Reuses caller-provided pool / command buffer / fence — no
    /// `vkCreateCommandPool` / `vkAllocateCommandBuffers` / `vkCreateFence`
    /// per call. The cb is reset (its pool must have
    /// `RESET_COMMAND_BUFFER_BIT`) and the fence is reset before recording.
    /// Submits to the shared queue and blocks on the fence (same sync
    /// model as the per-call variant; only the resource churn is removed).
    ///
    /// Used by [`crate::core::context::TextureRing`]'s per-slot upload
    /// path — see `docs/architecture/texture-ring.md`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub unsafe fn upload_buffer_to_image_amortized(
        &self,
        cb: vk::CommandBuffer,
        fence: vk::Fence,
        src_buffer: vk::Buffer,
        dst_texture: &HostVulkanTexture,
        width: u32,
        height: u32,
    ) -> crate::core::Result<PixelBufferUploadFinalTextureLayout> {
        use crate::core::Error;

        let device = self.device();
        // Fence may be signaled (from prior submit on this slot) or
        // unsignaled (first use). Reset unconditionally.
        unsafe { device.reset_fences(&[fence]) }
            .map_err(|e| Error::GpuError(format!("amortized upload reset fence: {e}")))?;
        // Cb may be in executable (recorded but not submitted), pending
        // (submitted, before fence wait), or initial state. We just
        // fenced-waited (or first use), so it's safe to reset.
        unsafe { device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()) }
            .map_err(|e| Error::GpuError(format!("amortized upload reset cb: {e}")))?;

        unsafe {
            self.record_and_submit_buffer_to_image(
                cb,
                fence,
                src_buffer,
                dst_texture,
                width,
                height,
            )
        }
    }

    /// Shared by [`Self::upload_buffer_to_image`] and
    /// [`Self::upload_buffer_to_image_amortized`]: records the
    /// UNDEFINED → TRANSFER_DST → copy → usage-legal terminal layout
    /// sequence into `cb`, submits to the shared queue
    /// (mutex-protected), and waits on `fence`. Caller owns cb + fence
    /// lifecycle.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    unsafe fn record_and_submit_buffer_to_image(
        &self,
        cb: vk::CommandBuffer,
        fence: vk::Fence,
        src_buffer: vk::Buffer,
        dst_texture: &HostVulkanTexture,
        width: u32,
        height: u32,
    ) -> crate::core::Result<PixelBufferUploadFinalTextureLayout> {
        use crate::core::Error;

        let device = self.device();
        let queue = self.queue;

        let dst_image = dst_texture.image().ok_or_else(|| {
            Error::GpuError("buffer-to-image upload destination has no VkImage".into())
        })?;
        // Chosen here, at the barrier that records it, rather than by
        // the caller that publishes it.
        let final_texture_layout = dst_texture.terminal_layout_for_shader_read_access();

        unsafe {
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| Error::GpuError(format!("begin cb: {e}")))?;

        // Barrier: UNDEFINED → TRANSFER_DST
        let barrier_to_dst = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let barriers_to_dst = [barrier_to_dst];
        let dep_to_dst = vk::DependencyInfo::builder().image_memory_barriers(&barriers_to_dst);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep_to_dst) };

        // Copy buffer → image
        let region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_offset: vk::Offset3D::default(),
            image_extent: vk::Extent3D {
                width,
                height,
                depth: 1,
            },
        };
        unsafe {
            device.cmd_copy_buffer_to_image(
                cb,
                src_buffer,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            )
        };

        // Barrier: TRANSFER_DST → the destination's usage-legal terminal
        // layout. The destination scope covers every command stage
        // because a GENERAL-terminal image's next consumer is a storage
        // descriptor, a colour attachment or a transfer read, none of
        // which a sampled-read scope would cover.
        let barrier_to_read = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(final_texture_layout.as_vk())
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(dst_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let barriers_to_read = [barrier_to_read];
        let dep_to_read = vk::DependencyInfo::builder().image_memory_barriers(&barriers_to_read);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep_to_read) };

        unsafe { device.end_command_buffer(cb) }
            .map_err(|e| Error::GpuError(format!("end cb: {e}")))?;

        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe { self.submit_to_queue(queue, &[submit], fence) }?;
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| Error::GpuError(format!("wait: {e}")))?;

        Ok(PixelBufferUploadFinalTextureLayout {
            final_texture_layout,
        })
    }

    /// Whether this device supports content-preserving QFOT for
    /// cross-process layout coordination per #633: the optional
    /// `VK_EXT_external_memory_acquire_unmodified` extension was
    /// enabled at construction.
    ///
    /// `VK_QUEUE_FAMILY_EXTERNAL` (the queue family index used for
    /// QFOT src/dst) is core Vulkan 1.1 and is always available; only
    /// the acquire-unmodified extension is the meaningful gate.
    /// Structurally permanent `false` on NVIDIA Linux per current
    /// driver roadmap (extension not on NVIDIA's published support
    /// list as of 2026-05-03); Mesa is the eventual landing point.
    pub fn supports_qfot_acquire_unmodified(&self) -> bool {
        self.has_acquire_unmodified
    }

    /// Whether `VK_EXT_external_memory_host` was enabled at device
    /// construction — the imported tier of
    /// [`super::HostMappingWrittenByGpu`] is reachable only then.
    pub fn supports_host_pointer_import(&self) -> bool {
        self.has_external_memory_host
    }

    /// The alignment every imported host range must start and end on
    /// (`minImportedHostPointerAlignment`); zero when
    /// [`Self::supports_host_pointer_import`] is false.
    pub fn min_imported_host_pointer_alignment(&self) -> u64 {
        self.min_imported_host_pointer_alignment
    }

    /// Whether `VK_EXT_hdr_metadata` was enabled at device construction.
    /// Gates `vkSetHdrMetadataEXT` — the per-swapchain HDR static metadata
    /// (mastering display + content light) attachment that signals
    /// driver-side tone-mapping / color-volume on transition into a
    /// PQ/HLG colorspace. Useful only alongside `VK_KHR_swapchain` and
    /// the instance-level `VK_EXT_swapchain_colorspace`.
    pub fn supports_hdr_metadata(&self) -> bool {
        self.has_hdr_metadata
    }

    /// Whether the `VK_KHR_ray_tracing_pipeline` extension chain
    /// (acceleration_structure + ray_tracing_pipeline +
    /// deferred_host_operations + pipeline_library) was enabled at
    /// device creation. Gates [`VulkanRayTracingKernel`] and
    /// [`VulkanAccelerationStructure`] construction.
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        self.has_ray_tracing_pipeline
    }

    /// `VkPhysicalDeviceRayTracingPipelinePropertiesKHR` snapshot
    /// queried at device construction. `Some(_)` whenever
    /// [`Self::supports_ray_tracing_pipeline`] is true.
    pub fn ray_tracing_pipeline_properties(&self) -> Option<RayTracingPipelineProperties> {
        self.ray_tracing_properties
    }

    /// Refuse a ray-tracing constructor on a device without the ray-tracing
    /// tier, naming the tier.
    pub(crate) fn refuse_without_the_ray_tracing_tier(
        &self,
        constructor_description: &str,
    ) -> crate::core::Result<()> {
        if self.has_ray_tracing_pipeline {
            return Ok(());
        }
        Err(ray_tracing_tier_absent_refusal(
            constructor_description,
            &self.device_name,
        ))
    }

    /// The subgroup operations and stages this device's driver serves,
    /// checked against each kernel stage before anything is built.
    pub(crate) fn subgroup_operation_support(&self) -> &VulkanSubgroupOperationSupport {
        &self.subgroup_operation_support
    }

    /// Producer-side QFOT release barrier — declares the surface's
    /// post-write `VkImageLayout` and transfers ownership to
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) so a foreign
    /// device (host or peer subprocess) can subsequently acquire it.
    /// Pair with [`Self::acquire_from_foreign`] on the consumer side.
    ///
    /// One-shot synchronous submit on the host graphics queue.
    /// Caller is responsible for publishing the post-release layout
    /// to the surface-share daemon via
    /// `SurfaceStore::update_image_layout`.
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `false`,
    /// falls back to a same-family layout transition; content
    /// preservation across the consumer's import is then driver-
    /// empirical on DMA-BUF.
    pub fn release_to_foreign(
        &self,
        image: vk::Image,
        src_layout: vk::ImageLayout,
        dst_layout: vk::ImageLayout,
    ) -> Result<()> {
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 (promoted from
        // VK_KHR_external_memory) — always available. The Khronos
        // proposal for `VK_EXT_external_memory_acquire_unmodified`
        // permits either EXTERNAL or FOREIGN_EXT as the src/dst; for
        // cross-process Vulkan-to-Vulkan handoff EXTERNAL is the
        // idiomatic choice (FOREIGN_EXT is for non-Vulkan foreign
        // owners, and the OpenGL adapter doesn't issue Vulkan
        // barriers from GL writes anyway).
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
            .subresource_range(host_default_color_subresource_range())
            .build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Consumer-side QFOT acquire barrier — receives ownership from
    /// [`vk::QUEUE_FAMILY_EXTERNAL`] (core Vulkan 1.1) and transitions
    /// the imported `VkImage`'s tracker into `target` so subsequent
    /// host-side consumer barriers (`oldLayout = target → next`) are
    /// validation-clean per VUID-VkImageMemoryBarrier-oldLayout-01197.
    ///
    /// When [`Self::supports_qfot_acquire_unmodified`] is `true`,
    /// chains `VkExternalMemoryAcquireUnmodifiedEXT { acquireUnmodifiedMemory =
    /// VK_TRUE }` so the producer's content survives the transfer per
    /// Vulkan spec. When `false`, falls back to bridging
    /// `UNDEFINED → target` — content discard permitted by spec, but
    /// in practice DMA-BUF kernel-side memory contents are preserved
    /// on every modern Linux Vulkan driver (NVIDIA confirmed; Mesa
    /// iris/radeonsi follow the same convention).
    ///
    /// No-op when `target == UNDEFINED` (nothing to transition into).
    /// Path 2 of
    /// [`crate::core::context::GpuContext::resolve_texture_registration_by_surface_id`]
    /// calls this to align the consumer-side VkImage tracker with the
    /// surface-share IPC's `current_image_layout` (#633).
    pub fn acquire_from_foreign(&self, image: vk::Image, target: vk::ImageLayout) -> Result<()> {
        if target == vk::ImageLayout::UNDEFINED {
            return Ok(());
        }
        // QFOT path requires only `VK_EXT_external_memory_acquire_unmodified`;
        // `VK_QUEUE_FAMILY_EXTERNAL` is core Vulkan 1.1 and always
        // available. When the optional extension is absent (NVIDIA
        // Linux today and for the foreseeable future), fall back to
        // bridging UNDEFINED → target as a same-family transition.
        let use_qfot = self.has_acquire_unmodified;
        let (src_qf, src_layout) = if use_qfot {
            (vk::QUEUE_FAMILY_EXTERNAL, target)
        } else {
            (self.queue_family_index, vk::ImageLayout::UNDEFINED)
        };
        // The chained `VkExternalMemoryAcquireUnmodifiedEXT` lives on
        // the stack here — must outlive the call to
        // `submit_one_shot_image_barriers` because the built barrier
        // holds a raw pointer to it via pNext.
        let mut acquire_unmodified = vk::ExternalMemoryAcquireUnmodifiedEXT::builder()
            .acquire_unmodified_memory(true)
            .build();
        let mut barrier_builder = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(src_layout)
            .new_layout(target)
            .src_queue_family_index(src_qf)
            .dst_queue_family_index(self.queue_family_index)
            .image(image)
            .subresource_range(host_default_color_subresource_range());
        if use_qfot {
            barrier_builder = barrier_builder.push_next(&mut acquire_unmodified);
        }
        let barrier = barrier_builder.build();
        self.submit_one_shot_image_barriers(&[barrier])
    }

    /// Internal: one-shot pipeline barrier on the host graphics queue.
    /// Caller passes already-built `vk::ImageMemoryBarrier2` values
    /// (including any chained pNext structs they keep alive on their
    /// own stack); this helper handles command-pool/buffer/fence
    /// creation, submit, wait, and teardown.
    fn submit_one_shot_image_barriers(&self, barriers: &[vk::ImageMemoryBarrier2]) -> Result<()> {
        use crate::core::Error;

        let device = self.device();
        let queue = self.queue;
        let qf = self.queue_family_index;

        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|e| Error::GpuError(format!("qfot cmd pool: {e}")))?;

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
            Error::GpuError(format!("qfot cmd buf: {e}"))
        })?[0];

        let fence =
            unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| {
                unsafe { device.destroy_command_pool(pool, None) };
                Error::GpuError(format!("qfot fence: {e}"))
            })?;

        unsafe {
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .map_err(|e| Error::GpuError(format!("begin qfot cb: {e}")))?;

        let dep = vk::DependencyInfo::builder().image_memory_barriers(barriers);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep) };

        unsafe { device.end_command_buffer(cb) }
            .map_err(|e| Error::GpuError(format!("end qfot cb: {e}")))?;

        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe { self.submit_to_queue(queue, &[submit], fence) }?;
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| Error::GpuError(format!("qfot wait: {e}")))?;

        unsafe { device.destroy_fence(fence, None) };
        unsafe { device.destroy_command_pool(pool, None) };

        Ok(())
    }

    /// Get the VMA allocator for GPU memory management.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        self.allocator
            .as_ref()
            .expect("VMA allocator not initialized")
    }

    /// Import external memory from a DMA-BUF file descriptor.
    ///
    /// Uses raw `vkAllocateMemory` with `VkImportMemoryFdInfoKHR` since VMA
    /// does not support importing external memory from file descriptors.
    /// All non-import allocations go through VMA.
    ///
    /// Takes the fd by value: it is the driver's from `vkAllocateMemory`
    /// on, whatever that call returns — the spec transfers ownership on
    /// success, and the NVIDIA driver also closes a DMA-BUF on a failed
    /// import, so a close after the call lands on whatever the kernel has
    /// since handed that number to. Every exit before that call closes it
    /// here, and a fd that is not a DMA-BUF never reaches the call.
    pub fn import_dma_buf_memory(
        &self,
        dma_buf_fd: std::os::fd::OwnedFd,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
        preferred_flags: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory> {
        #[cfg(target_os = "linux")]
        let dma_buf_fd = refuse_a_file_descriptor_that_is_not_a_dma_buf(dma_buf_fd)?;
        let memory_type_index = self.find_memory_type(memory_type_bits, preferred_flags)?;

        let (memory, count) = self
            .allocate_device_memory_importing_the_fd(
                dma_buf_fd,
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                allocation_size,
                memory_type_index,
            )
            .map_err(|e| Error::GpuError(format!("Failed to import DMA-BUF memory: {e}")))?;
        tracing::debug!(
            "HostVulkanDevice: DMA-BUF memory imported ({} bytes, type={}, live={})",
            allocation_size,
            memory_type_index,
            count
        );

        Ok(memory)
    }

    /// The `vkAllocateMemory` import both fd import paths share, after each
    /// has made its own refusals. The fd is the driver's from the call on,
    /// whatever it returns; a success counts one more live allocation, and
    /// the new count comes back with the memory.
    fn allocate_device_memory_importing_the_fd(
        &self,
        imported_fd: std::os::fd::OwnedFd,
        handle_type: vk::ExternalMemoryHandleTypeFlags,
        allocation_size: vk::DeviceSize,
        memory_type_index: u32,
    ) -> std::result::Result<(vk::DeviceMemory, usize), vk::ErrorCode> {
        use std::os::fd::IntoRawFd as _;

        let mut import_info = vk::ImportMemoryFdInfoKHR::builder()
            .handle_type(handle_type)
            .fd(imported_fd.into_raw_fd())
            .build();
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info)
            .build();

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }?;
        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        Ok((memory, count))
    }

    /// Import an OPAQUE_FD as device memory at the memory type index the
    /// exporter allocated from — OPAQUE_FD has no fd-properties query, so
    /// the exporter's stated index is the only conforming one.
    ///
    /// Takes the fd by value. Every exit before `vkAllocateMemory` closes
    /// it; from that call on it is the driver's whatever the call returns.
    /// The spec leaves a failed import's fd with the caller, but a driver
    /// that closes it anyway — as NVIDIA does a DMA-BUF — would make a close
    /// here land on whatever the kernel has since handed that number to, so
    /// a failed import leaks the fd rather than risk that.
    #[cfg(target_os = "linux")]
    pub fn import_opaque_fd_memory_at_stated_memory_type_index(
        &self,
        opaque_fd: std::os::fd::OwnedFd,
        allocation_size: vk::DeviceSize,
        stated_memory_type_index: u32,
    ) -> Result<vk::DeviceMemory> {
        if !self.supports_external_memory {
            return Err(Error::NotSupported(format!(
                "OPAQUE_FD import: {} did not enable VK_KHR_external_memory_fd",
                self.device_name
            )));
        }
        if stated_memory_type_index >= self.memory_properties.memory_type_count {
            return Err(Error::Configuration(format!(
                "OPAQUE_FD import: the stated memory type index {stated_memory_type_index} is not \
                 one of the {} memory types {} has",
                self.memory_properties.memory_type_count, self.device_name
            )));
        }

        let (memory, count) = self
            .allocate_device_memory_importing_the_fd(
                opaque_fd,
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                allocation_size,
                stated_memory_type_index,
            )
            .map_err(|e| {
                Error::GpuError(format!(
                    "OPAQUE_FD import of {allocation_size} bytes at memory type \
                     {stated_memory_type_index} on {} failed: {e}",
                    self.device_name
                ))
            })?;
        tracing::debug!(
            allocation_size,
            stated_memory_type_index,
            live = count,
            "HostVulkanDevice: OPAQUE_FD memory imported"
        );
        Ok(memory)
    }

    /// The property flags of the memory type at `memory_type_index`, or
    /// `None` past the device's memory type count.
    #[cfg(target_os = "linux")]
    pub(crate) fn property_flags_of_memory_type_index(
        &self,
        memory_type_index: u32,
    ) -> Option<vk::MemoryPropertyFlags> {
        (memory_type_index < self.memory_properties.memory_type_count)
            .then(|| self.memory_properties.memory_types[memory_type_index as usize].property_flags)
    }

    /// Import a caller-owned host range as device memory through
    /// `VK_EXT_external_memory_host`, raw `vkAllocateMemory` like the
    /// DMA-BUF import above. `memory_type_bits` is the intersection of the
    /// buffer's requirements and what `vkGetMemoryHostPointerPropertiesEXT`
    /// reported for the range; a HOST_CACHED type wins when one qualifies,
    /// since the caller is going to read the range back on the CPU.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn import_host_pointer_memory(
        &self,
        host_ptr: *mut u8,
        byte_len: vk::DeviceSize,
        memory_type_bits: u32,
    ) -> Result<vk::DeviceMemory> {
        let host_visible =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let memory_type_index = self
            .find_memory_type(
                memory_type_bits,
                host_visible | vk::MemoryPropertyFlags::HOST_CACHED,
            )
            .or_else(|_| self.find_memory_type(memory_type_bits, host_visible))
            .map_err(|_| {
                Error::GpuError(format!(
                    "import_host_pointer_memory: no host-visible memory type accepts the host \
                     range (memory_type_bits = {memory_type_bits:#x})"
                ))
            })?;

        let mut import_info = vk::ImportMemoryHostPointerInfoEXT::builder()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT)
            .build();
        import_info.host_pointer = host_ptr.cast();
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(byte_len)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info)
            .build();

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "import_host_pointer_memory: the driver refused to import the host range \
                 {host_ptr:p}+{byte_len}: {e}"
            ))
        })?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            byte_len,
            memory_type_index,
            host_cached =
                memory_type_index_is_host_cached(&self.memory_properties, memory_type_index),
            live = count,
            "HostVulkanDevice: host pointer imported as device memory"
        );
        Ok(memory)
    }

    /// Get the VMA pool for DMA-BUF exportable HOST_VISIBLE buffers.
    /// Returns None on a device that exports no DMA-BUF.
    #[cfg(target_os = "linux")]
    pub fn dma_buf_buffer_pool(&self) -> Option<&vma::Pool> {
        self.dma_buf_buffer_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// Get the VMA pool for DMA-BUF exportable DEVICE_LOCAL images.
    /// Returns None on a device that exports no DMA-BUF.
    #[cfg(target_os = "linux")]
    pub fn dma_buf_image_pool(&self) -> Option<&vma::Pool> {
        self.dma_buf_image_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// Get the VMA pool for DMA-BUF exportable images created with
    /// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`.
    ///
    /// Returns `None` when the EGL probe didn't find any RT-capable modifier
    /// (no libEGL, no display, extension missing, or driver doesn't expose
    /// `external_only=FALSE` modifiers for BGRA). Callers must refuse the
    /// allocation in that case rather than fall back to LINEAR — LINEAR is
    /// sampler-only on NVIDIA.
    #[cfg(target_os = "linux")]
    pub fn dma_buf_image_pool_tiled(&self) -> Option<&vma::Pool> {
        self.dma_buf_image_pool_tiled
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers.
    ///
    /// Returns `None` when external memory isn't supported on this device,
    /// or when the pool's memory-type probe failed at construction.
    /// Used by CUDA / OpenCL interop — see
    /// [`super::HostVulkanBuffer::new_opaque_fd_export`].
    #[cfg(target_os = "linux")]
    pub fn opaque_fd_buffer_pool(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers on
    /// a HOST_CACHED memory type — the CPU-read sibling of
    /// [`Self::opaque_fd_buffer_pool`].
    ///
    /// `HOST_CACHED` is a VMA *preference*, never a requirement, so this
    /// is `None` on a device whose probe came back uncached — as well as
    /// when external memory is unsupported.
    ///
    /// `None` is not a refusal:
    /// [`super::HostVulkanBuffer::new_opaque_fd_export_host_cached`]
    /// degrades to [`Self::opaque_fd_buffer_pool`].
    #[cfg(target_os = "linux")]
    pub fn opaque_fd_buffer_pool_host_cached(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool_host_cached
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL buffers
    /// (GPU-resident sibling of [`Self::opaque_fd_buffer_pool`]).
    ///
    /// Returns `None` when external memory isn't supported or the
    /// DEVICE_LOCAL probe failed at construction. Used by GPU-resident
    /// CUDA interop paths — see
    /// [`super::HostVulkanBuffer::new_opaque_fd_export_device_local`].
    #[cfg(target_os = "linux")]
    pub fn opaque_fd_buffer_pool_device_local(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool_device_local
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL **images**
    /// (image-flavored sibling of [`Self::opaque_fd_buffer_pool_device_local`]).
    /// `VK_IMAGE_TILING_OPTIMAL`, no DRM modifier.
    ///
    /// Returns `None` when external memory isn't supported or the
    /// image-side probe failed at construction. Used by CUDA's
    /// `cudaExternalMemoryGetMappedMipmappedArray` import path — see
    /// [`super::HostVulkanTexture::new_opaque_fd_export`].
    #[cfg(target_os = "linux")]
    pub fn opaque_fd_image_pool(&self) -> Option<&vma::Pool> {
        self.opaque_fd_image_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// [`Self::dma_buf_buffer_pool`] with the memory type it is pinned to.
    #[cfg(target_os = "linux")]
    pub(crate) fn dma_buf_buffer_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.dma_buf_buffer_pool.as_ref()
    }

    /// [`Self::dma_buf_image_pool`] with the memory type it is pinned to.
    #[cfg(target_os = "linux")]
    pub(crate) fn dma_buf_image_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.dma_buf_image_pool.as_ref()
    }

    /// [`Self::dma_buf_image_pool_tiled`] with the memory type it is pinned to.
    #[cfg(target_os = "linux")]
    pub(crate) fn dma_buf_image_pool_tiled_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.dma_buf_image_pool_tiled.as_ref()
    }

    /// [`Self::opaque_fd_buffer_pool`] with the memory type it is pinned to.
    #[cfg(target_os = "linux")]
    pub(crate) fn opaque_fd_buffer_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_buffer_pool.as_ref()
    }

    /// [`Self::opaque_fd_buffer_pool_host_cached`] with the memory type it is pinned to.
    #[cfg(target_os = "linux")]
    pub(crate) fn opaque_fd_buffer_pool_host_cached_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_buffer_pool_host_cached.as_ref()
    }

    /// [`Self::opaque_fd_buffer_pool_device_local`] with the memory type it is pinned to.
    #[cfg(target_os = "linux")]
    pub(crate) fn opaque_fd_buffer_pool_device_local_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_buffer_pool_device_local.as_ref()
    }

    /// [`Self::opaque_fd_image_pool`] with the memory type it is pinned to.
    #[cfg(target_os = "linux")]
    pub(crate) fn opaque_fd_image_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_image_pool.as_ref()
    }

    /// Allocate as `admission` says: a frame through the device-memory budget
    /// gate, refused by name past its budget or near the allocation-count
    /// limit; an exempt engine allocation straight through, never weighed.
    pub(crate) fn allocate_as_the_device_memory_budget_admits<Allocated>(
        &self,
        admission: DeviceMemoryBudgetAdmission,
        frame_device_memory_request: impl FnOnce() -> Result<FrameDeviceMemoryRequest>,
        allocate: impl FnOnce() -> Result<Allocated>,
    ) -> Result<Allocated> {
        match admission {
            DeviceMemoryBudgetAdmission::EngineAllocationTheDeviceMemoryBudgetNeverRefuses => {
                allocate()
            }
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget => {
                let request = frame_device_memory_request()?;
                self.frame_allocation_device_memory_budget_gate
                    .allocate_a_frame_within_the_device_memory_budget(
                        &request,
                        || self.read_the_frame_allocation_device_memory_fetched_now(&request),
                        allocate,
                    )
            }
        }
    }

    /// Create a frame image from `allocation_source`, weighed as `admission`
    /// says against the heap that same source allocates it from.
    pub(crate) fn create_frame_image_within_the_device_memory_budget(
        &self,
        admission: DeviceMemoryBudgetAdmission,
        allocation_source: VmaFrameAllocationSource,
        image_create_info: &vk::ImageCreateInfo,
        allocation_options: &vma::AllocationOptions,
        frame_allocation_site: &'static str,
        describe_the_allocators_refusal: impl FnOnce(vk::ErrorCode) -> Error,
    ) -> Result<(vk::Image, vma::Allocation)> {
        // SAFETY (every unsafe block below): the memory-type queries only read
        // the allocator's types, and the create info and options are the
        // caller's, valid on this device.
        match allocation_source {
            #[cfg(target_os = "linux")]
            VmaFrameAllocationSource::ExportPool(export_pool) => {
                let memory_requirements =
                    self.memory_requirements_of_an_image_create_info(image_create_info)?;
                refuse_an_image_the_export_pools_memory_type_cannot_bind(
                    memory_requirements.memory_type_bits,
                    export_pool.memory_type_index,
                    image_create_info,
                    frame_allocation_site,
                )?;
                self.allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            memory_requirements,
                            Ok(export_pool.memory_type_index),
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            export_pool
                                .pool
                                .create_image(*image_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                )
            }
            VmaFrameAllocationSource::DefaultAllocator => self
                .allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        let memory_requirements =
                            self.memory_requirements_of_an_image_create_info(image_create_info)?;
                        // VMA cannot infer a DRM-modifier image's usage for an
                        // `Auto` query, so the type is chosen on the options'
                        // flags alone, which every frame constructor states.
                        let memory_type_index_or_refusal = if image_create_info.tiling
                            == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT
                        {
                            unsafe {
                                self.allocator().find_memory_type_index(
                                    memory_requirements.memory_type_bits,
                                    &vma::AllocationOptions {
                                        usage: vma::MemoryUsage::Unknown,
                                        ..*allocation_options
                                    },
                                )
                            }
                        } else {
                            unsafe {
                                self.allocator().find_memory_type_index_for_image_info(
                                    *image_create_info,
                                    allocation_options,
                                )
                            }
                        };
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            memory_requirements,
                            memory_type_index_or_refusal,
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            self.allocator()
                                .create_image(*image_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                ),
        }
    }

    /// Create a frame buffer from `allocation_source`, weighed as `admission`
    /// says against the heap that same source allocates it from.
    pub(crate) fn create_frame_buffer_within_the_device_memory_budget(
        &self,
        admission: DeviceMemoryBudgetAdmission,
        allocation_source: VmaFrameAllocationSource,
        buffer_create_info: &vk::BufferCreateInfo,
        allocation_options: &vma::AllocationOptions,
        frame_allocation_site: &'static str,
        describe_the_allocators_refusal: impl FnOnce(vk::ErrorCode) -> Error,
    ) -> Result<(vk::Buffer, vma::Allocation)> {
        // SAFETY (every unsafe block below): the memory-type query only reads
        // the allocator's types, and the create info and options are the
        // caller's, valid on this device.
        match allocation_source {
            #[cfg(target_os = "linux")]
            VmaFrameAllocationSource::ExportPool(export_pool) => self
                .allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            self.memory_requirements_of_a_buffer_create_info(buffer_create_info),
                            Ok(export_pool.memory_type_index),
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            export_pool
                                .pool
                                .create_buffer(*buffer_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                ),
            VmaFrameAllocationSource::DefaultAllocator => self
                .allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            self.memory_requirements_of_a_buffer_create_info(buffer_create_info),
                            unsafe {
                                self.allocator().find_memory_type_index_for_buffer_info(
                                    *buffer_create_info,
                                    allocation_options,
                                )
                            },
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            self.allocator()
                                .create_buffer(*buffer_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                ),
        }
    }

    /// Memory requirements of an image not yet created. A DRM-modifier image
    /// takes a probe image: `vkGetDeviceImageMemoryRequirements` needs a
    /// `planeAspect` naming a memory plane of the modifier the driver picks
    /// only at creation (VUID-VkDeviceImageMemoryRequirements-pCreateInfo-06420).
    fn memory_requirements_of_an_image_create_info(
        &self,
        image_create_info: &vk::ImageCreateInfo,
    ) -> Result<vk::MemoryRequirements> {
        if image_create_info.tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
            // SAFETY: the create info is the one the caller is about to
            // allocate with; the probe image is destroyed before returning.
            let probe_image = unsafe { self.device.create_image(image_create_info, None) }
                .map_err(|e| {
                    Error::GpuError(format!(
                        "vkCreateImage for the memory-requirements probe of a DRM-modifier \
                         image failed: {e}"
                    ))
                })?;
            let memory_requirements =
                unsafe { self.device.get_image_memory_requirements(probe_image) };
            unsafe { self.device.destroy_image(probe_image, None) };
            return Ok(memory_requirements);
        }
        let image_memory_requirements_info = vk::DeviceImageMemoryRequirements::builder()
            .create_info(image_create_info)
            .build();
        let mut memory_requirements = vk::MemoryRequirements2::default();
        // SAFETY: a query on a valid create info; nothing is created.
        unsafe {
            self.device.get_device_image_memory_requirements(
                &image_memory_requirements_info,
                &mut memory_requirements,
            )
        };
        Ok(memory_requirements.memory_requirements)
    }

    /// Memory requirements of a buffer not yet created.
    fn memory_requirements_of_a_buffer_create_info(
        &self,
        buffer_create_info: &vk::BufferCreateInfo,
    ) -> vk::MemoryRequirements {
        let buffer_memory_requirements_info = vk::DeviceBufferMemoryRequirements::builder()
            .create_info(buffer_create_info)
            .build();
        let mut memory_requirements = vk::MemoryRequirements2::default();
        // SAFETY: a query on a valid create info; nothing is created.
        unsafe {
            self.device.get_device_buffer_memory_requirements(
                &buffer_memory_requirements_info,
                &mut memory_requirements,
            )
        };
        memory_requirements.memory_requirements
    }

    /// The heap a memory type allocates from; refused by name past the
    /// device's memory type count.
    #[cfg(target_os = "linux")]
    pub(crate) fn device_memory_heap_of_memory_type(&self, memory_type_index: u32) -> Result<u32> {
        if memory_type_index >= self.memory_properties.memory_type_count {
            return Err(Error::GpuError(format!(
                "memory type {memory_type_index} is past this device's {} memory types",
                self.memory_properties.memory_type_count
            )));
        }
        Ok(self.memory_properties.memory_types[memory_type_index as usize].heap_index)
    }

    /// Every heap's budget, usage and allocator holdings, with the budget
    /// fetched from the driver for this call: VMA otherwise serves a budget
    /// it refreshes only every 30 of its own operations.
    #[cfg(target_os = "linux")]
    pub(crate) fn read_every_heaps_device_memory_budget_fetched_from_the_driver_now(
        &self,
    ) -> Result<Vec<DeviceMemoryHeapBudgetReading>> {
        use crate::core::error::{DeviceMemoryHeapBudgetSource, DeviceMemoryHeapLocality};

        let allocator = self.allocator();
        let fresh_fetch_frame_index = self
            .device_memory_budget_fresh_fetch_frame_index
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        // SAFETY: the frame index is opaque to VMA; with EXT_MEMORY_BUDGET
        // enabled, setting it fetches every heap's budget from the driver.
        unsafe { allocator.set_current_frame_index(fresh_fetch_frame_index) };
        let heap_budgets = allocator
            .get_heap_budgets()
            .map_err(|e| Error::GpuError(format!("vmaGetHeapBudgets failed: {e}")))?;
        let heap_locality_of = |heap_index: usize| {
            if self.memory_properties.memory_heaps[heap_index]
                .flags
                .contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
            {
                DeviceMemoryHeapLocality::DeviceLocal
            } else {
                DeviceMemoryHeapLocality::NotDeviceLocal
            }
        };
        let system_memory_available_bytes = (!self.has_memory_budget
            && (0..heap_budgets.len())
                .any(|heap_index| heap_locality_of(heap_index) == DeviceMemoryHeapLocality::NotDeviceLocal))
        .then(|| {
            crate::linux::system_memory_available_from_proc_meminfo::read_system_memory_available_bytes_from_proc_meminfo()
                .inspect_err(|e| {
                    tracing::debug!(
                        error = %e,
                        "the system's available memory is unreadable; a host heap's budget stays \
                         the allocator's estimate"
                    );
                })
                .ok()
        })
        .flatten();
        Ok(heap_budgets
            .iter()
            .enumerate()
            .map(|(heap_index, heap_budget)| {
                let heap_locality = heap_locality_of(heap_index);
                let (heap_budget_bytes, heap_budget_source) = if self.has_memory_budget {
                    (
                        heap_budget.budget,
                        DeviceMemoryHeapBudgetSource::ReportedByTheDriver,
                    )
                } else {
                    heap_budget_without_the_memory_budget_extension(
                        heap_budget.budget,
                        heap_budget.usage,
                        heap_locality,
                        system_memory_available_bytes,
                    )
                };
                DeviceMemoryHeapBudgetReading {
                    heap_index: heap_index as u32,
                    heap_locality,
                    heap_budget_bytes,
                    heap_usage_bytes: heap_budget.usage,
                    bytes_this_runtimes_allocator_pools_hold_on_the_heap: heap_budget
                        .statistics
                        .blockBytes,
                    device_memory_blocks_this_runtimes_allocator_holds_on_the_heap: heap_budget
                        .statistics
                        .blockCount,
                    heap_budget_source,
                }
            })
            .collect())
    }

    /// The device memory one frame allocation is decided on, read for it now.
    #[cfg(target_os = "linux")]
    fn read_the_frame_allocation_device_memory_fetched_now(
        &self,
        request: &FrameDeviceMemoryRequest,
    ) -> Result<FrameAllocationDeviceMemoryReading> {
        let FrameDeviceMemoryPlacement::VulkanMemoryType { memory_type_index } = request.placement;
        let heap_index = self.device_memory_heap_of_memory_type(memory_type_index)?;
        let every_heaps_reading =
            self.read_every_heaps_device_memory_budget_fetched_from_the_driver_now()?;
        let allocation_count = DeviceMemoryAllocationCountReading {
            live_device_memory_allocation_count: device_memory_allocations_this_process_holds(
                every_heaps_reading.iter().map(|heap| {
                    heap.device_memory_blocks_this_runtimes_allocator_holds_on_the_heap
                }),
                self.live_allocation_count.load(Ordering::Relaxed),
            ),
            max_memory_allocation_count: self.max_memory_allocation_count,
        };
        let heap_reading = every_heaps_reading
            .into_iter()
            .nth(heap_index as usize)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "the allocator reported no budget for heap {heap_index} of memory type \
                     {memory_type_index}"
                ))
            })?;
        Ok(FrameAllocationDeviceMemoryReading {
            budget: FrameAllocationBudgetReading::VulkanMemoryHeap(heap_reading),
            allocation_count,
        })
    }

    /// The device memory one frame allocation is decided on, read for it now:
    /// Metal's working set read live, whatever the frame's placement.
    #[cfg(target_os = "macos")]
    fn read_the_frame_allocation_device_memory_fetched_now(
        &self,
        _request: &FrameDeviceMemoryRequest,
    ) -> Result<FrameAllocationDeviceMemoryReading> {
        let heap_budgets = self
            .allocator()
            .get_heap_budgets()
            .map_err(|e| Error::GpuError(format!("vmaGetHeapBudgets failed: {e}")))?;
        Ok(FrameAllocationDeviceMemoryReading {
            budget: FrameAllocationBudgetReading::MetalWorkingSet(
                self.metal_working_set_of_the_device.reading(),
            ),
            allocation_count: DeviceMemoryAllocationCountReading {
                live_device_memory_allocation_count: device_memory_allocations_this_process_holds(
                    heap_budgets
                        .iter()
                        .map(|heap_budget| heap_budget.statistics.blockCount),
                    self.live_allocation_count.load(Ordering::Relaxed),
                ),
                max_memory_allocation_count: self.max_memory_allocation_count,
            },
        })
    }

    /// The Metal working set frame allocations are weighed against.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_working_set_of_the_device(&self) -> &MetalWorkingSetOfTheDevice {
        &self.metal_working_set_of_the_device
    }

    /// From now on, weigh every frame allocation on this device against a
    /// budget lowered to its usage, so the next one is refused.
    #[cfg(test)]
    pub(crate) fn leave_no_room_in_any_frame_allocation_budget_for_this_test(&self) {
        self.frame_allocation_device_memory_budget_gate
            .leave_no_room_in_any_budget_for_this_test();
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

    /// Render-target-capable DRM format modifiers, by DRM FOURCC, from the
    /// EGL probe at device init. Empty when the probe failed. Callers
    /// pass [`DrmModifierTable::rt_modifiers`] into
    /// [`HostVulkanTexture::new_render_target_dma_buf`].
    #[cfg(target_os = "linux")]
    pub fn drm_modifier_table(&self) -> &Arc<DrmModifierTable> {
        &self.drm_modifier_table
    }

    /// Whether this device can allocate a cross-process-importable DMA-BUF
    /// image for `format`: the format has a DRM FOURCC and the EGL probe
    /// advertised at least one RT-capable modifier for it.
    #[cfg(target_os = "linux")]
    pub fn has_render_target_modifier_for_texture_format(
        &self,
        format: crate::core::rhi::TextureFormat,
    ) -> bool {
        drm_modifier_probe::fourcc::drm_fourcc_for_texture_format(format)
            .is_some_and(|fourcc| self.drm_modifier_table.has_rt_modifier(fourcc))
    }

    /// Whether `VK_EXT_metal_objects` was enabled — what an image over an
    /// IOSurface and a timeline's Metal shared event both need.
    #[cfg(target_os = "macos")]
    pub fn supports_metal_objects_interop(&self) -> bool {
        self.device
            .extensions()
            .contains(&vk::EXT_METAL_OBJECTS_EXTENSION.name)
    }

    /// Memory for an image created over an IOSurface, raw `vkAllocateMemory`
    /// like the imports above, on the type
    /// [`streamlib_consumer_rhi::device_local_memory_type_that_is_not_host_visible`]
    /// picks.
    #[cfg(target_os = "macos")]
    pub fn allocate_device_local_memory_for_an_iosurface_backed_image(
        &self,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
    ) -> Result<vk::DeviceMemory> {
        let memory_type_index =
            streamlib_consumer_rhi::device_local_memory_type_that_is_not_host_visible(
                &self.memory_properties,
                memory_type_bits,
            )
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "allocate_device_local_memory_for_an_iosurface_backed_image: no device-local \
                 memory type without host visibility is in the image's memory_type_bits \
                 ({memory_type_bits:#x})"
                ))
            })?;
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .build();
        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "allocate_device_local_memory_for_an_iosurface_backed_image: the driver refused \
                 {allocation_size} bytes on memory type {memory_type_index}: {e}"
            ))
        })?;
        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            allocation_size,
            memory_type_index,
            live = count,
            "HostVulkanDevice: memory bound for an IOSurface-backed image"
        );
        Ok(memory)
    }

    /// Free device memory allocated via raw vkAllocateMemory (import path only).
    ///
    /// VMA-managed allocations are freed via [`vma::Allocator::destroy_image`] or
    /// [`vma::Allocator::destroy_buffer`] — do not call this for VMA allocations.
    pub fn free_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.free_memory(memory, None) };
        self.live_allocation_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Map imported device memory for CPU access (DMA-BUF and OPAQUE_FD imports).
    pub fn map_imported_memory(
        &self,
        memory: vk::DeviceMemory,
        size: vk::DeviceSize,
    ) -> Result<*mut u8> {
        let ptr = unsafe {
            self.device
                .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| Error::GpuError(format!("Failed to map device memory: {e}")))?;
        Ok(ptr as *mut u8)
    }

    /// Unmap imported device memory.
    pub fn unmap_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.unmap_memory(memory) };
    }

    /// Live raw `vkAllocateMemory` allocations: DMA-BUF, OPAQUE_FD and
    /// host-pointer imports, and IOSurface-backed image memory.
    pub fn live_import_allocation_count(&self) -> usize {
        self.live_allocation_count.load(Ordering::Relaxed)
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
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::vulkan::rhi::dma_buf_export_test_device_fixture::a_device_that_exports_dma_buf_or_skip;

    #[cfg(target_os = "linux")]
    #[test]
    fn an_image_that_cannot_bind_the_export_pools_memory_type_is_refused_naming_its_shape() {
        let image_create_info = vk::ImageCreateInfo::builder()
            .format(vk::Format::B8G8R8A8_UNORM)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .build();
        const EXPORT_POOL_MEMORY_TYPE_INDEX: u32 = 2;

        let refusal = refuse_an_image_the_export_pools_memory_type_cannot_bind(
            0b1011,
            EXPORT_POOL_MEMORY_TYPE_INDEX,
            &image_create_info,
            "an allocation site under test",
        )
        .expect_err("memory_type_bits without bit 2 cannot bind type 2")
        .to_string();
        assert!(
            refusal.contains("an allocation site under test"),
            "{refusal}"
        );
        assert!(refusal.contains("B8G8R8A8_UNORM"), "{refusal}");
        assert!(refusal.contains("COLOR_ATTACHMENT"), "{refusal}");
        assert!(refusal.contains("memory type 2"), "{refusal}");

        refuse_an_image_the_export_pools_memory_type_cannot_bind(
            0b0100,
            EXPORT_POOL_MEMORY_TYPE_INDEX,
            &image_create_info,
            "an allocation site under test",
        )
        .expect("memory_type_bits with bit 2 bind type 2");
    }

    #[cfg(target_os = "linux")]
    fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
        let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
            return None;
        }
        Some(unsafe { file_status.assume_init() }.st_ino as u64)
    }

    /// The one exit of the import that is ours to close: no memory type
    /// matches, so the fd never reaches `vkAllocateMemory` and nobody but
    /// this function ever held it.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_memory_type_refusal_before_the_import_closes_the_fd_it_was_handed() {
        use std::os::fd::{FromRawFd as _, OwnedFd};

        let Some(device) = a_device_that_exports_dma_buf_or_skip(
            "a_memory_type_refusal_before_the_import_closes_the_fd_it_was_handed",
        ) else {
            return;
        };
        let source =
            crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(&device, 4096)
                .expect("source buffer allocation failed");
        let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");
        let inode = inode_of(fd).expect("an exported DMA-BUF must stat");

        let refusal = device
            .import_dma_buf_memory(
                unsafe { OwnedFd::from_raw_fd(fd) },
                4096,
                0,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .expect_err("no memory type matches an empty type filter");
        assert!(
            refusal.to_string().contains("No suitable memory type"),
            "the refusal must be the memory-type search, not the driver: {refusal}"
        );
        assert_ne!(
            inode_of(fd),
            Some(inode),
            "the refused import left the DMA-BUF fd open — no owner remains to close it"
        );
    }

    /// A memfd and a pipe are refused and closed without a device. A kept
    /// second descriptor of each file holds its inode, so a closed number
    /// that a parallel test reopens can never stat as the same file.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_descriptor_that_is_not_a_dma_buf_is_refused_and_closed() {
        use std::os::fd::{FromRawFd as _, OwnedFd};

        let memfd = unsafe { libc::memfd_create(c"not-a-dma-buf".as_ptr(), 0) };
        assert!(
            memfd >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        let memfd_duplicate = unsafe { libc::fcntl(memfd, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(
            memfd_duplicate >= 0,
            "dup: {}",
            std::io::Error::last_os_error()
        );
        let _memfd_kept_open = unsafe { OwnedFd::from_raw_fd(memfd_duplicate) };

        let mut pipe_ends = [0 as std::os::unix::io::RawFd; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe_ends.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let _pipe_write_end_kept_open = unsafe { OwnedFd::from_raw_fd(pipe_ends[1]) };

        for (candidate_fd, descriptor_kind) in [(memfd, "memfd"), (pipe_ends[0], "pipe")] {
            let inode = inode_of(candidate_fd).expect("a fresh fd must stat");
            let refusal = refuse_a_file_descriptor_that_is_not_a_dma_buf(unsafe {
                OwnedFd::from_raw_fd(candidate_fd)
            })
            .expect_err("only a DMA-BUF passes");
            assert!(
                refusal.to_string().contains("is not a DMA-BUF"),
                "the {descriptor_kind} refusal must say why: {refusal}"
            );
            assert_ne!(
                inode_of(candidate_fd),
                Some(inode),
                "the refused {descriptor_kind} fd was left open — no owner remains to close it"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn an_exported_dma_buf_passes_the_dma_buf_check_as_the_same_fd() {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

        let Some(device) = a_device_that_exports_dma_buf_or_skip(
            "an_exported_dma_buf_passes_the_dma_buf_check_as_the_same_fd",
        ) else {
            return;
        };
        let source =
            crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(&device, 4096)
                .expect("source buffer allocation failed");
        let fd = source.export_dma_buf_fd().expect("DMA-BUF export failed");

        let fd_that_passed_the_dma_buf_check =
            refuse_a_file_descriptor_that_is_not_a_dma_buf(unsafe { OwnedFd::from_raw_fd(fd) })
                .expect("an exported DMA-BUF must pass");
        assert_eq!(fd_that_passed_the_dma_buf_check.as_raw_fd(), fd);
    }

    /// Build a `VkPhysicalDeviceMemoryProperties` whose first
    /// `property_flags.len()` types carry the given flags.
    #[cfg(target_os = "linux")]
    fn memory_properties_with_types(
        property_flags: &[vk::MemoryPropertyFlags],
    ) -> vk::PhysicalDeviceMemoryProperties {
        let mut memory_properties = vk::PhysicalDeviceMemoryProperties {
            memory_type_count: property_flags.len() as u32,
            ..Default::default()
        };
        for (memory_type, flags) in memory_properties
            .memory_types
            .iter_mut()
            .zip(property_flags.iter())
        {
            memory_type.property_flags = *flags;
        }
        memory_properties
    }

    /// The host-cached pool's presence is decided by reading the probed
    /// index's flags, never by the probe failing: VMA prefers HOST_CACHED
    /// under `HOST_ACCESS_RANDOM` but cannot require it, so a cache-less
    /// device answers a *successful* probe with an uncached index.
    ///
    /// Mental-revert: return `true` unconditionally and a cache-less
    /// device pins its readback pool to write-combined memory while
    /// reporting it cached. Verified.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_device_whose_probed_type_is_not_host_cached_gets_no_host_cached_pool() {
        let uncached =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let cached = uncached | vk::MemoryPropertyFlags::HOST_CACHED;

        let cache_less_device =
            memory_properties_with_types(&[vk::MemoryPropertyFlags::DEVICE_LOCAL, uncached]);
        assert!(
            !memory_type_index_is_host_cached(&cache_less_device, 1),
            "a HOST_VISIBLE | HOST_COHERENT type without HOST_CACHED must not pass"
        );

        let cached_device = memory_properties_with_types(&[
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
            uncached,
            cached,
        ]);
        assert!(
            memory_type_index_is_host_cached(&cached_device, 2),
            "the cached exportable type must pass"
        );
        assert!(
            !memory_type_index_is_host_cached(&cached_device, 1),
            "the flags are read at the probed index, not searched for"
        );
    }

    /// An index past `memoryTypeCount` names no memory type, so it is
    /// not cached — the trailing entries of the fixed 32-slot array are
    /// zeroed padding, never allocatable types.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_memory_type_index_past_the_device_count_is_not_host_cached() {
        let memory_properties =
            memory_properties_with_types(&[vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT
                | vk::MemoryPropertyFlags::HOST_CACHED]);
        assert!(memory_type_index_is_host_cached(&memory_properties, 0));
        assert!(!memory_type_index_is_host_cached(&memory_properties, 1));
        assert!(!memory_type_index_is_host_cached(&memory_properties, 31));
        assert!(!memory_type_index_is_host_cached(
            &memory_properties,
            u32::MAX
        ));
    }

    /// Try to create a HostVulkanDevice; return None if GPU/Vulkan is unavailable (CI).
    fn try_create_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(e) => {
                println!("Skipping test — Vulkan not available: {e}");
                None
            }
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_device_creation() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        assert!(!device.name().is_empty(), "Device name should not be empty");
        println!("Vulkan device created: {}", device.name());
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_queue_family_discovery() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        println!(
            "Graphics queue family: {}, transfer: {}, video_encode: {:?}",
            device.queue_family_index(),
            device.transfer_queue_family_index(),
            device.video_encode_queue_family_index(),
        );
    }

    /// Issue #638 — `supports_cross_device_dma_buf_probe()` must
    /// return `false` on NVIDIA and `true` elsewhere. Locks the
    /// vendor-id check; mentally-revertible (delete the
    /// `vendor_id != 0x10DE` check in `new()` and this test fails on
    /// NVIDIA hardware). Skips when no Vulkan device is available.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn supports_cross_device_dma_buf_probe_matches_vendor_blocklist() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        let name = device.name();
        let is_nvidia = name.contains("NVIDIA") || name.contains("nvidia");
        let supports = device.supports_cross_device_dma_buf_probe();

        if is_nvidia {
            assert!(
                !supports,
                "expected supports_cross_device_dma_buf_probe()=false on NVIDIA ({name}), got true"
            );
        } else {
            assert!(
                supports,
                "expected supports_cross_device_dma_buf_probe()=true on non-NVIDIA ({name}), got false"
            );
        }
    }

    /// Non-NVIDIA vendor IDs must report `nvjpeg = false` regardless
    /// of whether `libnvjpeg.so.12` happens to be on the host. Locks
    /// the vendor-id gate inside [`ThirdPartyGpuCapabilities::probe`]
    /// — mentally revert that gate (`vendor_id == PCI_VENDOR_NVIDIA &&`
    /// → `true &&`) and this test fails on any host where libnvjpeg
    /// IS present (true on this development host). Cheap, no Vulkan
    /// device required.
    #[test]
    fn third_party_gpu_capabilities_non_nvidia_vendor_disables_nvjpeg() {
        // PCI vendor IDs from the Khronos Vulkan registry. Intel, AMD,
        // ARM Mali, Qualcomm Adreno — none are NVIDIA.
        for non_nvidia_vendor in [0x8086_u32, 0x1002, 0x13B5, 0x5143] {
            let caps = ThirdPartyGpuCapabilities::probe(non_nvidia_vendor);
            assert!(
                !caps.nvjpeg,
                "non-NVIDIA vendor 0x{:04X} must report nvjpeg=false; \
                 got nvjpeg=true (vendor-id gate broken)",
                non_nvidia_vendor,
            );
        }
    }

    /// On NVIDIA-vendor hosts, `nvjpeg` reflects whether `libnvjpeg.so.12`
    /// is dlopen-able — runtime-detection shape, not a build-time
    /// feature gate. The probe is deterministic for a given host, so
    /// the field's value must equal the independent dlopen check.
    /// Cheap, no Vulkan device required.
    #[test]
    fn third_party_gpu_capabilities_nvidia_nvjpeg_tracks_library_loadability() {
        const PCI_VENDOR_NVIDIA: u32 = 0x10DE;
        let caps = ThirdPartyGpuCapabilities::probe(PCI_VENDOR_NVIDIA);
        let independent_check = probe_nvjpeg_loadable();
        assert_eq!(
            caps.nvjpeg, independent_check,
            "ThirdPartyGpuCapabilities::probe must report nvjpeg=true \
             iff libnvjpeg.so.12 is dlopen-able; got caps.nvjpeg={} \
             but independent dlopen returns {}",
            caps.nvjpeg, independent_check,
        );
    }

    /// `Default` must produce an all-`false` capabilities struct.
    /// Lets callers spell "no third-party libs available" without
    /// touching every field — important for future siblings that
    /// extend the struct (the `Default` derive auto-tracks new
    /// fields). Cheap, no Vulkan device required.
    #[test]
    fn third_party_gpu_capabilities_default_is_all_false() {
        let caps = ThirdPartyGpuCapabilities::default();
        assert!(!caps.nvjpeg, "Default::nvjpeg must be false");
    }

    /// Locks the canonical inherent `HostVulkanDevice::submit_to_queue`
    /// signature the codec layer reaches directly. If a future refactor
    /// reintroduces a wrapper trait method returning a different
    /// `Result` type, this signature binding fails to compile.
    #[test]
    fn submit_to_queue_inherent_signature_is_streamlib_result() {
        let _f: unsafe fn(
            &HostVulkanDevice,
            vk::Queue,
            &[vk::SubmitInfo2],
            vk::Fence,
        ) -> Result<()> = HostVulkanDevice::submit_to_queue;
    }

    /// Issue #633 — `supports_qfot_acquire_unmodified` must equal the
    /// `has_acquire_unmodified` extension probe from `new()`.
    /// Trait-method route must agree with the inherent method.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn supports_qfot_acquire_unmodified_consistent_across_inherent_and_trait() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };
        let inherent = HostVulkanDevice::supports_qfot_acquire_unmodified(&device);
        let via_trait =
            <HostVulkanDevice as streamlib_consumer_rhi::VulkanRhiDevice>::supports_qfot_acquire_unmodified(&device);
        assert_eq!(inherent, via_trait, "trait and inherent reports must agree");
        assert_eq!(
            inherent, device.has_acquire_unmodified,
            "supports_qfot_acquire_unmodified must equal has_acquire_unmodified \
             (VK_QUEUE_FAMILY_EXTERNAL is core 1.1 and always available)"
        );
    }

    /// Issue #633 — QFOT acquire is a no-op when target is UNDEFINED.
    /// **Honest scope**: this asserts only that the call returns
    /// `Ok`, not that no GPU work was issued. Reverting the early
    /// return makes the helper attempt a barrier with
    /// `image = vk::Image::null()`; validation layers reject that
    /// (`VUID-VkImageMemoryBarrier2-image-parameter`), but raw
    /// drivers may tolerate it silently. Real GPU-correctness for
    /// QFOT comes from E2E scenarios run with
    /// `VK_LOADER_LAYERS_ENABLE=*validation*`.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn host_acquire_from_foreign_undefined_target_is_noop() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };
        let result = device.acquire_from_foreign(vk::Image::null(), vk::ImageLayout::UNDEFINED);
        assert!(
            result.is_ok(),
            "acquire_from_foreign with target=UNDEFINED must short-circuit Ok"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_vma_allocator_created() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        // Verify VMA allocator is accessible
        let _ = device.allocator();
        println!("VMA allocator created successfully");
    }

    /// Every export pool the device created holds a tiny sentinel for the
    /// device's life — the anchor NVIDIA's per-handle-type export state needs
    /// after a swapchain exists, which the shared pools freeing their frames
    /// would otherwise take away. Locks it at the data-structure level, since
    /// the driver failure itself needs a live swapchain and real camera to
    /// reproduce.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn every_export_pool_the_device_created_keeps_a_tiny_sentinel_for_its_life() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        // Built from the pools the device actually created, so the assertion
        // holds on any driver; empty only when no export pool exists.
        let mut expected_labels: Vec<&'static str> = Vec::new();
        if device.dma_buf_buffer_pool().is_some() {
            expected_labels.push("dma_buf_host_visible");
        }
        if device.dma_buf_image_pool().is_some() {
            expected_labels.push("dma_buf_image");
        }
        if device.dma_buf_image_pool_tiled().is_some() {
            expected_labels.push("dma_buf_image_tiled");
        }
        if device.opaque_fd_buffer_pool().is_some() {
            expected_labels.push("opaque_fd_host_visible");
        }
        if device.opaque_fd_buffer_pool_host_cached().is_some() {
            expected_labels.push("opaque_fd_host_cached");
        }
        if device.opaque_fd_buffer_pool_device_local().is_some() {
            expected_labels.push("opaque_fd_device_local");
        }
        if device.opaque_fd_image_pool().is_some() {
            expected_labels.push("opaque_fd_image");
        }
        if expected_labels.is_empty() {
            tracing::warn!("Skipping — no export pools on this driver, so no sentinels expected");
            return;
        }

        let actual_labels: Vec<&'static str> = device
            .export_pool_sentinels
            .iter()
            .map(|sentinel| sentinel.label)
            .collect();
        assert_eq!(
            actual_labels, expected_labels,
            "every export pool the device created must hold a sentinel for the device's life"
        );
        if let Some(validation_message_counts) = device.validation_layer_message_counts() {
            assert_eq!(
                validation_message_counts.error_count, 0,
                "allocating the export pool sentinels raised validation errors"
            );
        }

        // A consumer-sized sentinel deterministically blocked the consumer's
        // own allocation on NVIDIA (#637).
        let largest_acceptable_sentinel_bytes: vk::DeviceSize = 64 * 1024;
        for sentinel in device.export_pool_sentinels.iter() {
            let label = sentinel.label;
            assert!(
                sentinel.size <= largest_acceptable_sentinel_bytes,
                "export pool sentinel '{label}' must stay tiny (got {} bytes, at most {})",
                sentinel.size,
                largest_acceptable_sentinel_bytes,
            );
            let live_allocation_bytes = device
                .allocator()
                .get_allocation_info(sentinel.allocation)
                .size;
            assert!(
                live_allocation_bytes > 0,
                "export pool sentinel '{label}' must hold a live allocation"
            );
            match sentinel.resource {
                ExportPoolSentinelResource::Buffer(buffer) => {
                    assert_ne!(
                        buffer,
                        vk::Buffer::null(),
                        "sentinel '{label}' has null VkBuffer"
                    );
                }
                ExportPoolSentinelResource::Image(image) => {
                    assert_ne!(
                        image,
                        vk::Image::null(),
                        "sentinel '{label}' has null VkImage"
                    );
                }
            }
        }
    }

    /// Sentinel allocation returns early when a later sentinel fails,
    /// dropping the sentinels it already made. Each must free its own allocation, or the
    /// device's pool teardown aborts inside VMA with `Unfreed dedicated
    /// allocations found!` (#2247).
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn an_export_pool_sentinel_dropped_before_the_device_frees_its_pool_allocation() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };
        let Some(pinned_pool) = device.opaque_fd_buffer_pool_pinned_to_one_memory_type() else {
            tracing::warn!("Skipping — no OPAQUE_FD buffer pool on this driver");
            return;
        };
        let live_allocations_in_pool = || {
            pinned_pool
                .pool
                .calculate_statistics()
                .expect("pool statistics")
                .statistics
                .allocationCount
        };
        let allocations_before_the_sentinel = live_allocations_in_pool();

        let sentinel = make_export_pool_buffer_sentinel(
            &device,
            pinned_pool,
            "opaque_fd_host_visible",
            vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            MappedOpaqueFdBufferHostAccessPattern::SequentialWrite.vma_allocation_create_flags(),
        )
        .expect("a tiny OPAQUE_FD sentinel allocates");
        assert_eq!(
            live_allocations_in_pool(),
            allocations_before_the_sentinel + 1,
            "the sentinel allocates out of the pool it pins"
        );

        drop(sentinel);
        assert_eq!(
            live_allocations_in_pool(),
            allocations_before_the_sentinel,
            "a sentinel dropped outside the device's teardown must free its allocation"
        );
    }

    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_dma_buf_import_round_trip() {
        use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands;

        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        // The export pool is what carries `VkExportMemoryAllocateInfo` onto
        // the allocation, which `vkGetMemoryFdKHR` below requires. Allocating
        // straight from `allocator()` — the default pool — yields memory that
        // cannot legally be exported.
        let Some(export_pool) = device.dma_buf_buffer_pool() else {
            println!("Skipping test — DMA-BUF buffer export pool unavailable on this driver");
            return;
        };

        let buffer_size: vk::DeviceSize = 4096;

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(buffer_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        // No `required_flags`: the pool's memory type was fixed at
        // `create_pool` time, and VMA ignores memory-type selection once
        // `pool` is set.
        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };

        let (buffer, allocation) = unsafe { export_pool.create_buffer(buffer_info, &alloc_opts) }
            .expect("create exportable buffer from the DMA-BUF export pool");

        // Get allocation info to access the underlying DeviceMemory for export
        let alloc_info = device.allocator().get_allocation_info(allocation);
        let memory = alloc_info.deviceMemory;

        // Export DMA-BUF fd via vulkanalia extension trait
        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let fd =
            unsafe { device.device().get_memory_fd_khr(&get_fd_info) }.expect("export DMA-BUF fd");

        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
        println!("Exported DMA-BUF fd: {fd}");

        // Import the fd back
        let mem_reqs = unsafe { device.device().get_buffer_memory_requirements(buffer) };
        let _imported = device
            .import_dma_buf_memory(
                unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) },
                mem_reqs.size.max(buffer_size),
                mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .expect("import DMA-BUF memory");

        println!("DMA-BUF import round-trip passed");

        // Cleanup
        device.free_imported_memory(_imported);
        unsafe { device.allocator().destroy_buffer(buffer, allocation) };
    }

    /// The device this milestone exists to bring up. Unlike [`try_create_device`]
    /// this refuses to pass by skipping: under `hardware-tests` the rig is
    /// asserted to exist, and a MoltenVK that cannot produce a device is the
    /// failure the test is for.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_device_comes_up_on_this_hosts_driver_and_names_itself() {
        let device = HostVulkanDevice::new()
            .expect("the rig must produce a Vulkan device — on Apple that is MoltenVK");

        let device_name = device.name();
        assert!(
            !device_name.trim().is_empty(),
            "a device that cannot name itself is not a device the engine can report on"
        );
        println!("Vulkan device: {device_name}");
    }

    /// Ray tracing is an absent tier on MoltenVK, not a failure: construction
    /// refuses by name rather than reaching an entry point that never loaded.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn ray_tracing_on_a_device_without_it_refuses_rather_than_panicking() {
        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        if device.supports_ray_tracing_pipeline() {
            println!("skipping — this device serves ray tracing, so there is no tier to refuse");
            return;
        }

        let unit_triangle_vertices = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let unit_triangle_indices = [0u32, 1, 2];
        let refusal = crate::vulkan::rhi::VulkanAccelerationStructure::build_triangles_blas(
            &device,
            "absent-tier/blas",
            &unit_triangle_vertices,
            &unit_triangle_indices,
        )
        .expect_err("a device without ray tracing must refuse to build an acceleration structure");
        assert!(
            matches!(refusal, Error::GpuError(_)),
            "the refusal must be the engine's typed GPU error, got {refusal:?}"
        );
    }

    /// Past a budget with no room left, a frame texture is refused by name
    /// before anything is allocated, while a uniform buffer — engine memory
    /// the budget never weighs — still allocates.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[allow(clippy::disallowed_macros)]
    fn a_frame_texture_past_a_budget_with_no_room_left_is_refused_while_an_engine_buffer_still_allocates()
     {
        use crate::core::rhi::{TextureFormat, TextureUsages};

        let Some(device) = try_create_device() else {
            return;
        };
        let device_memory_blocks_the_allocator_holds = || -> u64 {
            device
                .allocator()
                .get_heap_budgets()
                .expect("the allocator reports every heap")
                .iter()
                .map(|heap_budget| u64::from(heap_budget.statistics.blockCount))
                .sum()
        };
        let frame_descriptor = TextureDescriptor::new(256, 256, TextureFormat::Bgra8Unorm)
            .with_usage(
                TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
            );

        device.leave_no_room_in_any_frame_allocation_budget_for_this_test();
        let blocks_before_the_refusal = device_memory_blocks_the_allocator_holds();
        let refusal = device
            .create_texture(&frame_descriptor)
            .err()
            .expect("a frame past a budget with no room left is refused");
        println!("refused: {refusal}");

        #[cfg(target_os = "linux")]
        match &refusal {
            Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
                heap_index,
                frame_allocation_site,
                ..
            } => {
                assert_eq!(frame_allocation_site, "HostVulkanTexture::new");
                if let Some(dma_buf_image_pool) =
                    device.dma_buf_image_pool_pinned_to_one_memory_type()
                {
                    assert_eq!(
                        *heap_index,
                        device
                            .device_memory_heap_of_memory_type(dma_buf_image_pool.memory_type_index)
                            .expect("the pool's pinned type is one of the device's"),
                        "the refusal names the heap the DMA-BUF image pool's pinned memory type \
                         allocates from: {refusal}"
                    );
                }
            }
            other => panic!("expected the heap-budget refusal, got {other:?}"),
        }
        #[cfg(target_os = "macos")]
        assert!(
            matches!(
                refusal,
                Error::FrameAllocationRefusedPastTheMetalWorkingSetBudget { .. }
            ),
            "expected the Metal working-set refusal, got {refusal:?}"
        );
        assert_eq!(
            device_memory_blocks_the_allocator_holds(),
            blocks_before_the_refusal,
            "a refused frame allocates no device memory"
        );

        crate::vulkan::rhi::HostVulkanBuffer::new_uniform_buffer_host_visible(&device, 256)
            .expect("a uniform buffer the budget never weighs still allocates past it");
    }

    /// Every VMA frame constructor, past a budget with no room left, is
    /// refused naming the heap that the same constructor's allocation lands on.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[allow(clippy::disallowed_macros)]
    fn every_vma_frame_constructors_refusal_names_the_heap_its_allocation_lands_on() {
        use crate::core::rhi::{TextureFormat, TextureUsages};
        use crate::vulkan::rhi::HostVulkanBuffer;
        use crate::vulkan::rhi::drm_modifier_probe::fourcc;

        type MemoryTypeOfOneFreshFrameAllocation<'a> = Box<dyn Fn() -> Result<u32> + 'a>;
        fn memory_type_of_the_texture(texture: HostVulkanTexture) -> u32 {
            texture
                .vma_allocation_memory_type_index()
                .expect("a VMA-allocated texture has a memory type")
        }
        fn memory_type_of_the_buffer(buffer: HostVulkanBuffer) -> u32 {
            buffer
                .vma_allocation_memory_type_index()
                .expect("a VMA-allocated buffer has a memory type")
        }

        let Some(device) = try_create_device() else {
            return;
        };
        const FRAME_EDGE_PIXELS: u32 = 256;
        let frame_byte_len = u64::from(FRAME_EDGE_PIXELS * FRAME_EDGE_PIXELS * 4);
        let sampled_frame_descriptor = TextureDescriptor::new(
            FRAME_EDGE_PIXELS,
            FRAME_EDGE_PIXELS,
            TextureFormat::Bgra8Unorm,
        )
        .with_usage(
            TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
        );
        let render_target_frame_descriptor = TextureDescriptor::new(
            FRAME_EDGE_PIXELS,
            FRAME_EDGE_PIXELS,
            TextureFormat::Bgra8Unorm,
        )
        .with_usage(
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC,
        );
        let cuda_mappable_frame_descriptor = TextureDescriptor::new(
            FRAME_EDGE_PIXELS,
            FRAME_EDGE_PIXELS,
            TextureFormat::Rgba8Unorm,
        );
        let render_target_modifiers = device
            .drm_modifier_table()
            .rt_modifiers(fourcc::DRM_FORMAT_ARGB8888);
        let host_cached_opaque_fd_buffer_site = if device
            .opaque_fd_buffer_pool_host_cached_pinned_to_one_memory_type()
            .is_some()
        {
            "HostVulkanBuffer::new_opaque_fd_export_host_cached"
        } else {
            "HostVulkanBuffer::new_opaque_fd_export"
        };
        let frame_constructors_by_site: Vec<(&str, MemoryTypeOfOneFreshFrameAllocation<'_>)> = vec![
            (
                "HostVulkanTexture::new",
                Box::new(|| {
                    HostVulkanTexture::new(&device, &sampled_frame_descriptor)
                        .map(memory_type_of_the_texture)
                }),
            ),
            (
                "HostVulkanTexture::new_device_local",
                Box::new(|| {
                    HostVulkanTexture::new_device_local(&device, &sampled_frame_descriptor)
                        .map(memory_type_of_the_texture)
                }),
            ),
            (
                "HostVulkanTexture::new_render_target_dma_buf",
                Box::new(|| {
                    HostVulkanTexture::new_render_target_dma_buf(
                        &device,
                        &render_target_frame_descriptor,
                        render_target_modifiers,
                    )
                    .map(memory_type_of_the_texture)
                }),
            ),
            (
                "HostVulkanTexture::new_opaque_fd_export",
                Box::new(|| {
                    HostVulkanTexture::new_opaque_fd_export(
                        &device,
                        &cuda_mappable_frame_descriptor,
                    )
                    .map(memory_type_of_the_texture)
                }),
            ),
            (
                "HostVulkanBuffer::new",
                Box::new(|| {
                    HostVulkanBuffer::new(&device, frame_byte_len).map(memory_type_of_the_buffer)
                }),
            ),
            (
                "HostVulkanBuffer::new_storage_buffer_host_cached_for_cpu_reads",
                Box::new(|| {
                    HostVulkanBuffer::new_storage_buffer_host_cached_for_cpu_reads(
                        &device,
                        frame_byte_len,
                    )
                    .map(memory_type_of_the_buffer)
                }),
            ),
            (
                "HostVulkanBuffer::new_opaque_fd_export",
                Box::new(|| {
                    HostVulkanBuffer::new_opaque_fd_export(&device, frame_byte_len)
                        .map(memory_type_of_the_buffer)
                }),
            ),
            (
                host_cached_opaque_fd_buffer_site,
                Box::new(|| {
                    HostVulkanBuffer::new_opaque_fd_export_host_cached(&device, frame_byte_len)
                        .map(memory_type_of_the_buffer)
                }),
            ),
            (
                "HostVulkanBuffer::new_opaque_fd_export_device_local",
                Box::new(|| {
                    HostVulkanBuffer::new_opaque_fd_export_device_local(&device, frame_byte_len)
                        .map(memory_type_of_the_buffer)
                }),
            ),
        ];

        let heap_each_frame_constructors_allocation_lands_on: Vec<_> = frame_constructors_by_site
            .iter()
            .filter_map(
                |(frame_allocation_site, allocate_one_frame)| match allocate_one_frame() {
                    Ok(memory_type_index) => Some((
                        *frame_allocation_site,
                        allocate_one_frame,
                        device
                            .device_memory_heap_of_memory_type(memory_type_index)
                            .expect("the allocation's memory type is one of the device's"),
                    )),
                    Err(e) => {
                        println!(
                            "skipping {frame_allocation_site}: it does not allocate here: {e}"
                        );
                        None
                    }
                },
            )
            .collect();
        assert!(
            !heap_each_frame_constructors_allocation_lands_on.is_empty(),
            "a device that allocates no frame through any constructor has nothing to weigh"
        );

        device.leave_no_room_in_any_frame_allocation_budget_for_this_test();
        for (expected_frame_allocation_site, allocate_one_frame, heap_the_allocation_lands_on) in
            heap_each_frame_constructors_allocation_lands_on
        {
            match allocate_one_frame() {
                Err(Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
                    heap_index,
                    frame_allocation_site,
                    ..
                }) => {
                    assert_eq!(frame_allocation_site, expected_frame_allocation_site);
                    assert_eq!(
                        heap_index, heap_the_allocation_lands_on,
                        "{expected_frame_allocation_site} is refused naming heap {heap_index}, \
                         but its allocation lands on heap {heap_the_allocation_lands_on}"
                    );
                    println!(
                        "{expected_frame_allocation_site}: refused naming heap {heap_index}, the \
                         heap its allocation lands on"
                    );
                }
                Ok(_) => panic!(
                    "{expected_frame_allocation_site} allocated past a budget with no room left"
                ),
                Err(other) => panic!(
                    "{expected_frame_allocation_site}: expected the heap-budget refusal, got \
                     {other:?}"
                ),
            }
        }
    }

    /// The rig's evidence that GPU memory another process holds reaches this
    /// runtime as a smaller heap budget: a child holds a CUDA allocation and
    /// the largest device-local heap's driver-reported budget falls by it.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[allow(clippy::disallowed_macros)]
    fn the_heap_budget_the_device_reads_falls_while_another_process_holds_a_large_cuda_allocation()
    {
        use std::io::{BufRead as _, BufReader};
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        use std::time::Duration;

        use crate::core::error::DeviceMemoryHeapLocality;

        const PYTHON_WITH_CUDA_TORCH_VARIABLE: &str = "STREAMLIB_RIG_PYTHON_WITH_CUDA_TORCH";
        const LARGEST_CHILD_CUDA_ALLOCATION_BYTES: u64 = 4 << 30;
        const SMALLEST_CHILD_CUDA_ALLOCATION_WORTH_MEASURING_BYTES: u64 = 256 << 20;
        const CHILD_HOLDS_ITS_ALLOCATION_WITHIN: Duration = Duration::from_secs(120);

        let Ok(python_with_cuda_torch) = std::env::var(PYTHON_WITH_CUDA_TORCH_VARIABLE) else {
            println!(
                "Skipping - {PYTHON_WITH_CUDA_TORCH_VARIABLE} names no python whose torch has CUDA"
            );
            return;
        };
        let Some(device) = try_create_device() else {
            return;
        };
        if !device.has_memory_budget {
            println!(
                "Skipping - VK_EXT_memory_budget is not advertised, so no driver-reported \
                 budget exists to fall"
            );
            return;
        }
        let largest_device_local_heap =
            |every_heaps_reading: &[DeviceMemoryHeapBudgetReading]| {
                every_heaps_reading
                    .iter()
                    .filter(|heap| heap.heap_locality == DeviceMemoryHeapLocality::DeviceLocal)
                    .max_by_key(|heap| heap.heap_budget_bytes)
                    .cloned()
                    .expect("a GPU has a device-local heap")
            };

        let heap_before = largest_device_local_heap(
            &device
                .read_every_heaps_device_memory_budget_fetched_from_the_driver_now()
                .expect("every heap's budget reads before the child allocates"),
        );
        let heap_room_before = heap_before
            .heap_budget_bytes
            .saturating_sub(heap_before.heap_usage_bytes);
        let child_cuda_allocation_bytes =
            (heap_room_before / 4).min(LARGEST_CHILD_CUDA_ALLOCATION_BYTES);
        if child_cuda_allocation_bytes < SMALLEST_CHILD_CUDA_ALLOCATION_WORTH_MEASURING_BYTES {
            println!(
                "Skipping - heap {} has {heap_room_before} bytes of room, too little to measure a \
                 child's allocation against",
                heap_before.heap_index
            );
            return;
        }

        let child_script = format!(
            "import sys, torch\n\
             print('torch', torch.__version__, 'cuda', torch.cuda.is_available(), flush=True)\n\
             if not torch.cuda.is_available():\n    sys.exit(3)\n\
             held = torch.empty({child_cuda_allocation_bytes}, dtype=torch.uint8, device='cuda')\n\
             torch.cuda.synchronize()\n\
             print('held', flush=True)\n\
             sys.stdin.read()\n"
        );
        let mut child = Command::new(&python_with_cuda_torch)
            .arg("-c")
            .arg(&child_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the python named by the variable starts");
        let child_stdout = child.stdout.take().expect("the child's stdout is piped");
        let (child_line_tx, child_line_rx) = mpsc::channel::<String>();
        let child_stdout_reader = std::thread::spawn(move || {
            for line in BufReader::new(child_stdout)
                .lines()
                .map_while(|line| line.ok())
            {
                if child_line_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut child_holds_its_cuda_allocation = false;
        while let Ok(line) = child_line_rx.recv_timeout(CHILD_HOLDS_ITS_ALLOCATION_WITHIN) {
            println!("child: {line}");
            if line == "held" {
                child_holds_its_cuda_allocation = true;
                break;
            }
        }
        if !child_holds_its_cuda_allocation {
            let _ = child.kill();
            let child_status = child.wait();
            let _ = child_stdout_reader.join();
            println!(
                "Skipping - the child exited or stalled before holding a \
                 {child_cuda_allocation_bytes}-byte CUDA allocation ({child_status:?}); a +cpu \
                 torch exits 3"
            );
            return;
        }

        let every_heaps_reading_while_the_child_holds = device
            .read_every_heaps_device_memory_budget_fetched_from_the_driver_now()
            .expect("every heap's budget reads while the child holds its allocation");
        drop(child.stdin.take());
        let child_status = child.wait().expect("the child exits once its stdin closes");
        let _ = child_stdout_reader.join();
        let heap_while_the_child_holds = every_heaps_reading_while_the_child_holds
            .into_iter()
            .nth(heap_before.heap_index as usize)
            .expect("the same heap reads again");

        println!("child CUDA allocation: {child_cuda_allocation_bytes} bytes ({child_status})");
        println!("heap before the child allocated: {heap_before:?}");
        println!("heap while the child held it:    {heap_while_the_child_holds:?}");
        let heap_budget_fall = heap_before
            .heap_budget_bytes
            .saturating_sub(heap_while_the_child_holds.heap_budget_bytes);
        assert!(
            heap_budget_fall >= child_cuda_allocation_bytes / 4 * 3,
            "heap {}'s budget fell by {heap_budget_fall} bytes while another process held \
             {child_cuda_allocation_bytes} bytes of CUDA memory",
            heap_before.heap_index
        );
    }

    /// Set in the child process a device-selection test re-runs in; names the
    /// device the child must build on.
    #[cfg(target_os = "linux")]
    const EXPECTED_SELECTED_VULKAN_DEVICE_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_EXPECTED_SELECTED_VULKAN_DEVICE";

    /// The device a device-selection test's child process must build on.
    #[cfg(target_os = "linux")]
    #[derive(Clone, Copy)]
    enum ExpectedSelectedVulkanDevice {
        /// The hardware device, though the loader lists lavapipe first.
        HardwareListedAfterLavapipe,
        /// Lavapipe, the only device visible.
        LavapipeAlone,
    }

    #[cfg(target_os = "linux")]
    impl ExpectedSelectedVulkanDevice {
        fn as_environment_value(self) -> &'static str {
            match self {
                Self::HardwareListedAfterLavapipe => "hardware-listed-after-lavapipe",
                Self::LavapipeAlone => "lavapipe-alone",
            }
        }

        fn from_this_process_environment() -> Option<Self> {
            let value = std::env::var_os(EXPECTED_SELECTED_VULKAN_DEVICE_ENVIRONMENT_VARIABLE)?;
            [Self::HardwareListedAfterLavapipe, Self::LavapipeAlone]
                .into_iter()
                .find(|expected| value == expected.as_environment_value())
                .or_else(|| panic!("no expected device is named {value:?}"))
        }
    }

    /// The ICD manifest whose file name starts with `manifest_file_name_prefix`,
    /// from the loader variables this process carries, else the loader's
    /// system search directories.
    #[cfg(target_os = "linux")]
    fn an_icd_manifest_named(manifest_file_name_prefix: &str) -> Option<std::path::PathBuf> {
        let named_by_a_loader_variable = ["VK_ADD_DRIVER_FILES", "VK_DRIVER_FILES"]
            .into_iter()
            .filter_map(std::env::var_os)
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>());
        let in_a_system_search_directory = [
            "/etc/vulkan/icd.d",
            "/usr/local/share/vulkan/icd.d",
            "/usr/share/vulkan/icd.d",
        ]
        .into_iter()
        .filter_map(|directory| std::fs::read_dir(directory).ok())
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()));
        named_by_a_loader_variable
            .chain(in_a_system_search_directory)
            .find(|manifest| {
                manifest
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| file_name.starts_with(manifest_file_name_prefix))
                    && manifest.is_file()
            })
    }

    /// In the child: build the device under the `VK_DRIVER_FILES` the parent
    /// set and hold it to what the parent expects of it.
    #[cfg(target_os = "linux")]
    fn the_device_this_process_builds_on_is_the_expected_one(
        expected_device: ExpectedSelectedVulkanDevice,
    ) {
        if let ExpectedSelectedVulkanDevice::HardwareListedAfterLavapipe = expected_device {
            let vulkan_instance = HostVulkanInstance::new().expect("the instance creates");
            let first_listed_device = unsafe {
                let physical_devices = vulkan_instance
                    .instance
                    .enumerate_physical_devices()
                    .expect("the loader enumerates");
                vulkan_instance
                    .instance
                    .get_physical_device_properties(physical_devices[0])
            };
            assert_eq!(
                first_listed_device.device_type,
                vk::PhysicalDeviceType::CPU,
                "the loader must list lavapipe first for this test to prove the engine's own \
                 CPU-type-last order"
            );
        }

        let device = HostVulkanDevice::new().expect("a device builds under these driver files");
        let device_name = device.name();
        match expected_device {
            ExpectedSelectedVulkanDevice::HardwareListedAfterLavapipe => {
                assert!(
                    !device.device_is_software(),
                    "the engine built on {device_name}, a software device, with a hardware \
                     device visible"
                );
                assert!(
                    device.supports_semaphore_export(),
                    "{device_name} exports a timeline semaphore"
                );
                assert!(
                    device.supports_dma_buf_export(),
                    "{device_name} exports DMA-BUF"
                );
            }
            ExpectedSelectedVulkanDevice::LavapipeAlone => {
                assert!(
                    device.device_is_software(),
                    "{device_name} must report itself software"
                );
                assert!(
                    device_name.contains("llvmpipe"),
                    "the only visible device is lavapipe, got {device_name}"
                );
                assert!(
                    !device.supports_semaphore_export(),
                    "lavapipe exports no semaphore"
                );
                assert!(
                    !device.supports_dma_buf_export(),
                    "a software device exports no DMA-BUF, whatever /dev/udmabuf permits"
                );
                assert!(
                    device
                        .drm_modifier_table()
                        .rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888)
                        .is_empty(),
                    "a device that exports no DMA-BUF takes no EGL modifier table"
                );
            }
        }
    }

    /// Re-run `test_path` in a child process that sees only
    /// `vulkan_driver_files`, in the order the loader enumerates them, and
    /// expects `expected_device`.
    #[cfg(target_os = "linux")]
    fn rerun_expecting_the_selected_device(
        test_path: &str,
        vulkan_driver_files: &[std::path::PathBuf],
        expected_device: ExpectedSelectedVulkanDevice,
    ) {
        let vulkan_driver_files =
            std::env::join_paths(vulkan_driver_files).expect("ICD manifest paths join");
        // Both switches off the reordering a loader and Mesa's implicit
        // device-select layer apply, so the engine sees the drivers in the
        // order the loader enumerates them: the reverse of the list.
        let child = crate::core::test_support::rerun_this_test_in_a_child_process_with_environment(
            test_path,
            &[
                ("VK_DRIVER_FILES", vulkan_driver_files.as_os_str()),
                ("VK_LOADER_DISABLE_SELECT", std::ffi::OsStr::new("1")),
                ("NODEVICE_SELECT", std::ffi::OsStr::new("1")),
                (
                    EXPECTED_SELECTED_VULKAN_DEVICE_ENVIRONMENT_VARIABLE,
                    std::ffi::OsStr::new(expected_device.as_environment_value()),
                ),
            ],
        );
        assert!(
            child.status.success(),
            "the child process built on the wrong device or refused: {}\n{}\n{}",
            child.status,
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr),
        );
    }

    /// The loader lists lavapipe first, its own sorting switched off; the
    /// engine still builds on the hardware device, which exports DMA-BUF and
    /// a timeline semaphore.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn with_lavapipe_listed_before_the_nvidia_driver_the_engine_builds_on_the_nvidia_device() {
        if let Some(expected_device) = ExpectedSelectedVulkanDevice::from_this_process_environment()
        {
            the_device_this_process_builds_on_is_the_expected_one(expected_device);
            return;
        }
        let lavapipe_manifest =
            an_icd_manifest_named("lvp_icd").expect("the rig carries Mesa's lavapipe");
        let nvidia_manifest =
            an_icd_manifest_named("nvidia_icd").expect("the rig carries the NVIDIA driver");

        rerun_expecting_the_selected_device(
            "vulkan::rhi::vulkan_device::tests::with_lavapipe_listed_before_the_nvidia_driver_the_engine_builds_on_the_nvidia_device",
            &[nvidia_manifest, lavapipe_manifest],
            ExpectedSelectedVulkanDevice::HardwareListedAfterLavapipe,
        );
    }

    /// With lavapipe the only driver the engine builds on it, and the
    /// snapshot says software, no DMA-BUF and no semaphore export.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn with_only_lavapipe_visible_the_engine_builds_on_a_software_device_that_exports_neither_dma_buf_nor_a_semaphore()
     {
        if let Some(expected_device) = ExpectedSelectedVulkanDevice::from_this_process_environment()
        {
            the_device_this_process_builds_on_is_the_expected_one(expected_device);
            return;
        }
        let lavapipe_manifest =
            an_icd_manifest_named("lvp_icd").expect("the rig carries Mesa's lavapipe");

        rerun_expecting_the_selected_device(
            "vulkan::rhi::vulkan_device::tests::with_only_lavapipe_visible_the_engine_builds_on_a_software_device_that_exports_neither_dma_buf_nor_a_semaphore",
            &[lavapipe_manifest],
            ExpectedSelectedVulkanDevice::LavapipeAlone,
        );
    }
}
