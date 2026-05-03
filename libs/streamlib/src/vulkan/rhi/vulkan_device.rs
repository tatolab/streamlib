// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan device implementation for RHI.

use std::ffi::{c_char, CStr};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

use vulkanalia::loader::{LibloadingLoader, LIBRARY};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk::{self, KhrSwapchainExtensionDeviceCommands};
use vulkanalia_vma as vma;
use vma::Alloc as _;

use crate::core::rhi::TextureDescriptor;
use crate::core::{Result, StreamError};

#[cfg(target_os = "linux")]
use super::drm_modifier_probe::{self, DrmModifierTable};
use super::{HostMarker, VulkanCommandQueue, VulkanRhiDevice, HostVulkanTexture};

/// Vulkan GPU device.
///
/// Wraps the Vulkan instance, physical device, and logical device.
/// On macOS/iOS, uses MoltenVK to provide Vulkan API on top of Metal.
pub struct HostVulkanDevice {
    entry: vulkanalia::Entry,
    instance: vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    /// Memory properties kept for DMA-BUF import path (raw vkAllocateMemory).
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    transfer_queue_family_index: u32,
    transfer_queue: vk::Queue,
    #[allow(dead_code)]
    device_name: String,
    supports_external_memory: bool,
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
    dma_buf_buffer_pool: Option<vma::Pool>,
    /// VMA pool for DMA-BUF exportable DEVICE_LOCAL images (textures for IPC).
    #[cfg(target_os = "linux")]
    dma_buf_image_pool: Option<vma::Pool>,
    /// VMA pool for DMA-BUF exportable images created with
    /// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` and a render-target-capable
    /// DRM modifier from [`Self::drm_modifier_table`]. Separate from
    /// [`Self::dma_buf_image_pool`] because the memory type index for tiled
    /// DRM-modifier images may differ from the OPTIMAL pool's, and because
    /// callers should never get a sampler-only LINEAR image when they asked
    /// for an RT-capable one.
    #[cfg(target_os = "linux")]
    dma_buf_image_pool_tiled: Option<vma::Pool>,
    /// VMA pool for OPAQUE_FD exportable HOST_VISIBLE buffers — the export
    /// flavor CUDA / OpenCL interop expects. Separate from
    /// [`Self::dma_buf_buffer_pool`] because `VkExportMemoryAllocateInfo`'s
    /// `handleTypes` differ (OPAQUE_FD vs DMA_BUF_EXT) and a single VMA pool
    /// can carry only one chained `pNext`.
    #[cfg(target_os = "linux")]
    opaque_fd_buffer_pool: Option<vma::Pool>,
    /// VMA pool for OPAQUE_FD exportable DEVICE_LOCAL buffers — the
    /// GPU-resident sibling of [`Self::opaque_fd_buffer_pool`]. CUDA imports
    /// the same OPAQUE_FD handle type, but the underlying memory lives in
    /// VRAM (~600 GB/s on RTX 3090) instead of pinned host (~16-32 GB/s
    /// PCIe), which matters for hot-path camera→inference flows. Separate
    /// pool because VMA pools are pinned to a single memory-type-index and
    /// DEVICE_LOCAL types differ from HOST_VISIBLE ones.
    #[cfg(target_os = "linux")]
    opaque_fd_buffer_pool_device_local: Option<vma::Pool>,
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
    /// Backing storage for the DEVICE_LOCAL OPAQUE_FD buffer pool's
    /// VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _opaque_fd_buffer_export_info_device_local: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// `VkPhysicalDeviceIDProperties::deviceUUID` queried at construction.
    /// Required by CUDA-Vulkan interop on multi-GPU rigs to match the
    /// CUDA device whose `cudaDeviceProp::uuid` equals this value;
    /// using a mismatched device silently fails on the import.
    physical_device_uuid: [u8; 16],
    /// Render-target-capable DRM modifiers per format from the EGL probe at
    /// device init. Empty when libEGL is unavailable or the probe failed.
    /// Callers consult this before requesting
    /// [`HostVulkanTexture::new_render_target_dma_buf`].
    #[cfg(target_os = "linux")]
    drm_modifier_table: Arc<DrmModifierTable>,
    /// Tracks DMA-BUF import-path allocations (raw vkAllocateMemory for import only).
    live_allocation_count: AtomicUsize,
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
    /// Device-level mutex for resource creation (video sessions, VMA allocations).
    device_mutex: Mutex<()>,
    /// Long-lived sentinels for the OPAQUE_FD export pools, kept alive
    /// for the device's lifetime so NVIDIA's per-handle-type kernel
    /// state can never observe "no live OPAQUE_FD allocation" between
    /// the engine pre-warm and the consumer's allocation. See
    /// [`docs/learnings/nvidia-opaque-fd-after-swapchain.md`] and
    /// issue #637 — the existing allocate-and-drop pre-warm was
    /// sufficient for DMA-BUF (the compositor's swapchain DMA-BUF
    /// imports keep the kernel state alive), but OPAQUE_FD has no
    /// equivalent live consumer between init and the camera→cuda
    /// processor's allocation, so the kernel state can decay and
    /// the post-swapchain allocation flakes intermittently.
    #[cfg(target_os = "linux")]
    opaque_fd_export_sentinels: Vec<ExportPoolSentinel>,
}

/// Internal anti-decay sentinel for an export-capable VMA pool.
///
/// Holds raw `vk::Buffer` + `vma::Allocation` (no `Arc<HostVulkanDevice>`
/// back-reference) so it can be stored on the device itself without
/// creating a reference cycle. Freed by [`HostVulkanDevice`]'s `Drop`
/// impl before the allocator is torn down.
#[cfg(target_os = "linux")]
struct ExportPoolSentinel {
    buffer: vk::Buffer,
    allocation: vma::Allocation,
    /// Diagnostic label for the pool this sentinel covers.
    label: &'static str,
    /// Allocation size in bytes (for the tracing log at init).
    size: vk::DeviceSize,
}

impl HostVulkanDevice {
    /// Create a new Vulkan device.
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
        // 1. Load Vulkan entry points via libloading
        let loader = unsafe { LibloadingLoader::new(LIBRARY) }.map_err(|e| {
            StreamError::GpuError(format!("Failed to load Vulkan library: {e}"))
        })?;
        let entry = unsafe { vulkanalia::Entry::new(loader) }.map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to load Vulkan. On macOS, ensure MoltenVK is installed: {e}"
            ))
        })?;

        // 2. Enumerate available instance extensions
        let available_extensions = unsafe { entry.enumerate_instance_extension_properties(None) }
            .map_err(|e| {
                StreamError::GpuError(format!("Failed to enumerate extensions: {e}"))
            })?;

        let available_ext_names: Vec<&CStr> = available_extensions
            .iter()
            .map(|ext| unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) })
            .collect();

        // 3. Build extension list
        let mut instance_extensions: Vec<*const c_char> = Vec::new();

        // On macOS/iOS, we need portability enumeration for MoltenVK
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            // VK_KHR_portability_enumeration is required for MoltenVK
            let portability_enum = c"VK_KHR_portability_enumeration";
            if available_ext_names.contains(&portability_enum) {
                instance_extensions.push(portability_enum.as_ptr());
            }

            // VK_EXT_metal_objects for Metal interop
            let metal_objects = c"VK_EXT_metal_objects";
            if available_ext_names.contains(&metal_objects) {
                instance_extensions.push(metal_objects.as_ptr());
                tracing::info!("VK_EXT_metal_objects available - Metal interop enabled");
            } else {
                tracing::warn!(
                    "VK_EXT_metal_objects not available - Metal interop will be limited"
                );
            }
        }

        // On Linux, enable surface extensions for windowed display (Vulkan WSI)
        #[cfg(target_os = "linux")]
        {
            let surface_ext = c"VK_KHR_surface";
            if available_ext_names.contains(&surface_ext) {
                instance_extensions.push(surface_ext.as_ptr());
                tracing::info!("VK_KHR_surface enabled");
            }

            // Enable all available platform surface extensions
            let wayland_ext = c"VK_KHR_wayland_surface";
            if available_ext_names.contains(&wayland_ext) {
                instance_extensions.push(wayland_ext.as_ptr());
                tracing::info!("VK_KHR_wayland_surface available");
            }
            let xcb_ext = c"VK_KHR_xcb_surface";
            if available_ext_names.contains(&xcb_ext) {
                instance_extensions.push(xcb_ext.as_ptr());
                tracing::info!("VK_KHR_xcb_surface available");
            }
            let xlib_ext = c"VK_KHR_xlib_surface";
            if available_ext_names.contains(&xlib_ext) {
                instance_extensions.push(xlib_ext.as_ptr());
                tracing::info!("VK_KHR_xlib_surface available");
            }

            // VK_EXT_headless_surface — enables creating a Vulkan surface without
            // a window. Used by unit tests to exercise the swapchain code path
            // without requiring a display server.
            let headless_ext = c"VK_EXT_headless_surface";
            if available_ext_names.contains(&headless_ext) {
                instance_extensions.push(headless_ext.as_ptr());
                tracing::info!("VK_EXT_headless_surface available");
            }
        }

        // 4. Create Vulkan instance at API version 1.4
        let app_info = vk::ApplicationInfo::builder()
            .application_name(b"StreamLib\0")
            .application_version(vk::make_version(0, 1, 0))
            .engine_name(b"StreamLib\0")
            .engine_version(vk::make_version(0, 1, 0))
            .api_version(vk::make_version(1, 4, 0))
            .build();

        let mut instance_create_flags = vk::InstanceCreateFlags::empty();

        // On macOS/iOS, enable portability enumeration flag
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            instance_create_flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }

        let instance_info = vk::InstanceCreateInfo::builder()
            .application_info(&app_info)
            .enabled_extension_names(&instance_extensions)
            .flags(instance_create_flags)
            .build();

        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create Vulkan instance: {e}")))?;

        // 5. Select physical device
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| StreamError::GpuError(format!("Failed to enumerate devices: {e}")))?;

        if physical_devices.is_empty() {
            return Err(StreamError::GpuError("No Vulkan devices found".into()));
        }

        // Prefer discrete GPU, fall back to first available
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
            unsafe { CStr::from_ptr(device_props.device_name.as_ptr()) }.to_string_lossy();

        // 5b. Query VkPhysicalDeviceIDProperties::deviceUUID via
        //     vkGetPhysicalDeviceProperties2. CUDA-Vulkan interop matches
        //     this against `cudaDeviceProp::uuid` (16 bytes) to pick the
        //     right CUDA device; on multi-GPU rigs (Jetson + dGPU,
        //     prosumer workstations) using a mismatched device fails
        //     silently when CUDA imports the OPAQUE_FD memory.
        let mut id_props = vk::PhysicalDeviceIDProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut id_props)
            .build();
        unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
        // `id_props.device_uuid` is `ByteArray<16>` (a transparent newtype
        // around `[u8; 16]`); use the upstream `From<ByteArray<N>> for [u8; N]`
        // impl to land in plain-array shape.
        let physical_device_uuid: [u8; 16] = id_props.device_uuid.into();

        let device_type_str = match device_props.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => "Discrete GPU",
            vk::PhysicalDeviceType::INTEGRATED_GPU => "Integrated GPU",
            vk::PhysicalDeviceType::VIRTUAL_GPU => "Virtual GPU",
            vk::PhysicalDeviceType::CPU => "CPU",
            _ => "Other",
        };
        tracing::info!(
            "Selected Vulkan device: {} (type: {})",
            device_name,
            device_type_str
        );

        // 6. Find graphics queue family
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };

        let queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| props.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(idx, _)| idx as u32)
            .ok_or_else(|| StreamError::GpuError("No graphics queue family found".into()))?;

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
            .find(|(_, props)| {
                props
                    .queue_flags
                    .contains(vk::QueueFlags::VIDEO_ENCODE_KHR)
            })
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
            .find(|(_, props)| {
                props
                    .queue_flags
                    .contains(vk::QueueFlags::VIDEO_DECODE_KHR)
            })
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
        let mut queue_create_infos = vec![vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)
            .build()];

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

        // On macOS/iOS, we need portability subset
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            device_extensions.push(c"VK_KHR_portability_subset".as_ptr());
        }

        // On Linux, enumerate device extensions once and enable what's available
        #[cfg(target_os = "linux")]
        let available_device_ext_names: Vec<&CStr> = {
            let available_device_extensions =
                unsafe { instance.enumerate_device_extension_properties(physical_device, None) }
                    .unwrap_or_default();
            available_device_extensions
                .iter()
                .map(|ext| unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) })
                .collect()
        };

        // On Linux, check for DMA-BUF external memory extensions
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
                    tracing::info!("VK_EXT_external_memory_dma_buf available");
                }

                let drm_format_modifier_ext = c"VK_EXT_image_drm_format_modifier";
                if available_device_ext_names.contains(&drm_format_modifier_ext) {
                    device_extensions.push(drm_format_modifier_ext.as_ptr());
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
                    tracing::info!("VK_KHR_external_semaphore_fd available");
                }

                tracing::info!("Vulkan external memory extensions enabled");
            } else {
                tracing::info!("Vulkan external memory extensions not available");
            }

            has_external_memory
        };

        // On Linux, enable VK_KHR_swapchain for windowed display rendering
        #[cfg(target_os = "linux")]
        {
            let swapchain_ext = c"VK_KHR_swapchain";
            if available_device_ext_names.contains(&swapchain_ext) {
                device_extensions.push(swapchain_ext.as_ptr());
                tracing::info!("VK_KHR_swapchain enabled");
            }

            // VK_KHR_dynamic_rendering is core since Vulkan 1.3 — no extension string needed.
            // Feature struct (PhysicalDeviceDynamicRenderingFeatures) is still enabled below.
        }

        // On Linux, check for Vulkan Video encode extensions
        // VK_KHR_synchronization2 is core since Vulkan 1.3 — no extension string needed.
        // Feature struct (PhysicalDeviceSynchronization2Features) is still enabled below.

        #[cfg(target_os = "linux")]
        let has_video_encode = {
            let has_video_queue =
                available_device_ext_names.contains(&c"VK_KHR_video_queue");
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
            // (required by vulkan-video crate's encoder/decoder).
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
            let has_video_queue =
                available_device_ext_names.contains(&c"VK_KHR_video_queue");
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

        #[cfg(target_os = "linux")]
        let supports_video_encode = has_video_encode;
        #[cfg(not(target_os = "linux"))]
        let supports_video_encode = false;

        #[cfg(target_os = "linux")]
        let supports_video_decode = has_video_decode;
        #[cfg(not(target_os = "linux"))]
        let supports_video_decode = false;

        // Enable dynamic rendering, timeline semaphore, and synchronization2 features on Linux.
        // Synchronization2 is a mandatory dependency of VK_KHR_video_encode_queue.
        #[cfg(target_os = "linux")]
        let mut dynamic_rendering_features =
            vk::PhysicalDeviceDynamicRenderingFeatures::builder().dynamic_rendering(true).build();

        #[cfg(target_os = "linux")]
        let mut timeline_semaphore_features =
            vk::PhysicalDeviceTimelineSemaphoreFeatures::builder().timeline_semaphore(true).build();

        #[cfg(target_os = "linux")]
        let mut synchronization2_features =
            vk::PhysicalDeviceSynchronization2Features::builder().synchronization2(true).build();

        #[cfg(target_os = "linux")]
        let mut video_maintenance1_features =
            vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::builder().video_maintenance1(true).build();

        // samplerYcbcrConversion is required to create NV12 image views / samplers
        // with VK_KHR_sampler_ycbcr_conversion (core in 1.1) on the vulkan-video path.
        // Without it, VUID-vkCreateSamplerYcbcrConversion-None-01648 fires every frame.
        #[cfg(target_os = "linux")]
        let mut vulkan_1_1_features = vk::PhysicalDeviceVulkan11Features::builder()
            .sampler_ycbcr_conversion(true)
            .build();

        #[cfg(target_os = "linux")]
        let device_create_info = {
            let mut builder = vk::DeviceCreateInfo::builder()
                .queue_create_infos(&queue_create_infos)
                .enabled_extension_names(&device_extensions)
                .push_next(&mut dynamic_rendering_features)
                .push_next(&mut timeline_semaphore_features)
                .push_next(&mut synchronization2_features)
                .push_next(&mut vulkan_1_1_features);
            if supports_video_encode || supports_video_decode {
                builder = builder.push_next(&mut video_maintenance1_features);
            }
            builder.build()
        };

        #[cfg(not(target_os = "linux"))]
        let device_create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .build();

        let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create logical device: {e}")))?;

        // 8. Get the graphics queue
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        // 8a2. Get the transfer queue (may be same as graphics if no dedicated transfer family)
        let transfer_queue = unsafe { device.get_device_queue(transfer_queue_family_index, 0) };

        // 8b. Get the video encode queue (if available)
        let video_encode_queue = if supports_video_encode {
            video_encode_queue_family_index.map(|ve_family| unsafe {
                device.get_device_queue(ve_family, 0)
            })
        } else {
            None
        };

        // 8c. Get the video decode queue (if available)
        let video_decode_queue = if supports_video_decode {
            video_decode_queue_family_index.map(|vd_family| unsafe {
                device.get_device_queue(vd_family, 0)
            })
        } else {
            None
        };

        // 8d. Get the dedicated compute queue (if available)
        let compute_queue = compute_queue_family_index.map(|cq_family| unsafe {
            device.get_device_queue(cq_family, 0)
        });

        // 9. Query memory properties (kept for DMA-BUF import path)
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
        let mut alloc_options = vma::AllocatorOptions::new(&instance, &device, physical_device);
        alloc_options.version = vulkanalia::Version::new(1, 4, 0);

        let allocator = Arc::new(
            unsafe { vma::Allocator::new(&alloc_options) }
                .map_err(|e| StreamError::GpuError(format!("Failed to create VMA allocator: {e}")))?,
        );

        // Run the EGL probe BEFORE creating pools — the tiled pool needs the
        // modifier list. Probe failure (no libEGL, no display, extension
        // missing) is non-fatal: the table stays empty and the tiled pool
        // is skipped, which makes render-target DMA-BUF allocations refuse
        // with an error rather than silently picking a sampler-only modifier.
        // See `docs/learnings/nvidia-egl-dmabuf-render-target.md`.
        #[cfg(target_os = "linux")]
        let drm_modifier_table = match drm_modifier_probe::probe_default_display() {
            Ok(t) => Arc::new(t),
            Err(e) => {
                tracing::warn!(
                    "EGL DRM modifier probe failed: {e} — render-target DMA-BUF \
                     pool will be unavailable; only sampler-only / linear DMA-BUF \
                     allocations will succeed"
                );
                Arc::new(DrmModifierTable::empty())
            }
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

        // Build DMA-BUF export pools on Linux when external memory is supported.
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
        ) = if supports_external_memory {
            // BGRA8_UNORM in Vulkan = ARGB8888 in DRM.
            let bgra_modifiers = drm_modifier_table
                .rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888);
            match Self::create_dma_buf_pools(
                &allocator,
                &device,
                &memory_properties,
                bgra_modifiers,
            ) {
                Ok((bp, ip, tp, bi, ii, ti)) => {
                    (Some(bp), Some(ip), tp, Some(bi), Some(ii), ti)
                }
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
            "Vulkan device initialized: {} (queue family {}, {} memory types, external_memory={}, vma=enabled, dma_buf_pools={})",
            device_name,
            queue_family_index,
            memory_properties.memory_type_count,
            supports_external_memory,
            {
                #[cfg(target_os = "linux")]
                { dma_buf_buffer_pool.is_some() }
                #[cfg(not(target_os = "linux"))]
                { false }
            }
        );

        let device = Self {
            entry,
            instance,
            physical_device,
            memory_properties,
            device,
            queue,
            queue_family_index,
            transfer_queue_family_index,
            transfer_queue,
            device_name: device_name.into_owned(),
            supports_external_memory,
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
            opaque_fd_buffer_pool_device_local,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info: opaque_fd_buffer_export_info,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info_device_local:
                opaque_fd_buffer_export_info_device_local,
            physical_device_uuid,
            #[cfg(target_os = "linux")]
            drm_modifier_table,
            live_allocation_count: AtomicUsize::new(0),
            graphics_queue_mutex: Mutex::new(()),
            transfer_queue_mutex: Mutex::new(()),
            video_encode_queue_mutex: Mutex::new(()),
            video_decode_queue_mutex: Mutex::new(()),
            compute_queue_mutex: Mutex::new(()),
            device_mutex: Mutex::new(()),
            #[cfg(target_os = "linux")]
            opaque_fd_export_sentinels: Vec::new(),
        };

        // Wrap in `Arc` before pre-warming so the export-pool probes
        // can call back through the public RHI constructors (which
        // take `&Arc<HostVulkanDevice>`).
        #[cfg(not(target_os = "linux"))]
        let device = Arc::new(device);

        #[cfg(target_os = "linux")]
        let device = {
            let mut device = Arc::new(device);
            let sentinels = Self::prewarm_export_pools(&device)?;
            // Sentinels hold only raw `vk::Buffer` + `vma::Allocation`,
            // not `Arc<HostVulkanDevice>` clones, so the Arc strong count
            // is still 1 here and `get_mut` succeeds. The other prewarm
            // probes inside `prewarm_export_pools` are dropped before the
            // function returns, so they don't bump the count either.
            Arc::get_mut(&mut device)
                .expect(
                    "HostVulkanDevice has unique Arc ownership during construction; \
                     export sentinels must not hold Arc<HostVulkanDevice> clones",
                )
                .opaque_fd_export_sentinels = sentinels;
            device
        };

        Ok(device)
    }

    /// Run every export-capable VMA pool through one allocation **before
    /// any swapchain is bound** so subsequent post-swapchain allocations
    /// through the same pool don't hit NVIDIA's exportable-memory cap.
    ///
    /// DMA-BUF probes are allocate-and-drop: the compositor's swapchain
    /// imports keep a live DMA-BUF allocation in the kernel for the
    /// process's lifetime, so the per-handle-type kernel state cannot
    /// decay between init and a consumer's allocation.
    ///
    /// OPAQUE_FD probes are **retained as long-lived sentinels** on the
    /// device (returned from this function and stashed by the caller in
    /// [`Self::opaque_fd_export_sentinels`]). There is no compositor-
    /// equivalent live OPAQUE_FD allocation in the kernel, so dropping
    /// the probe leaves the per-handle-type state vulnerable to
    /// reclaim/decay; subsequent post-swapchain allocations then flake
    /// intermittently. Sentinels are intentionally tiny (8×8×4 = 256
    /// bytes): empirical E2E on Cam Link 4K showed that a same-size
    /// (1920×1080×4 ≈ 8 MiB) sentinel *deterministically* blocked the
    /// consumer's post-swapchain allocation, indicating NVIDIA tracks
    /// a cumulative byte budget for OPAQUE_FD and the sentinel must
    /// not compete with consumer-class allocations. Issue #637.
    ///
    /// Pools that weren't created (external memory unsupported, EGL
    /// probe failed, etc.) are skipped — the engine never produces a
    /// device that exposes a pool but won't pre-warm it.
    ///
    /// **Verification:** `examples/camera-python-display` against a
    /// real UVC camera (Cam Link 4K) reproduces the post-swapchain
    /// failure intermittently when the OPAQUE_FD sentinels are
    /// removed; with the sentinels retained the failure does not
    /// reproduce. See `docs/learnings/nvidia-opaque-fd-after-swapchain.md`
    /// for the run-and-revert protocol.
    #[cfg(target_os = "linux")]
    fn prewarm_export_pools(device: &Arc<Self>) -> Result<Vec<ExportPoolSentinel>> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
        use streamlib_consumer_rhi::PixelFormat;
        use super::{HostVulkanPixelBuffer, HostVulkanTexture};

        const PROBE_W: u32 = 8;
        const PROBE_H: u32 = 8;
        const PROBE_BPP: u32 = 4;

        let mut sentinels: Vec<ExportPoolSentinel> = Vec::new();

        // 1. DMA-BUF buffer pool — HOST_VISIBLE exportable buffer.
        //    Allocate-and-drop: compositor swapchain DMA-BUF imports
        //    keep the kernel state alive for the process's lifetime.
        if device.dma_buf_buffer_pool().is_some() {
            let probe = HostVulkanPixelBuffer::new(
                device, PROBE_W, PROBE_H, PROBE_BPP, PixelFormat::Bgra32,
            )
            .map_err(|e| {
                StreamError::GpuError(format!(
                    "DMA-BUF buffer pool pre-warm failed: {e}"
                ))
            })?;
            drop(probe);
        }

        // 2. DMA-BUF image pool (linear, OPTIMAL tiling). Allocate-and-drop.
        if device.dma_buf_image_pool().is_some() {
            let desc = TextureDescriptor::new(PROBE_W, PROBE_H, TextureFormat::Bgra8Unorm)
                .with_usage(
                    TextureUsages::TEXTURE_BINDING
                        | TextureUsages::COPY_SRC
                        | TextureUsages::COPY_DST,
                );
            let probe = HostVulkanTexture::new(device, &desc).map_err(|e| {
                StreamError::GpuError(format!(
                    "DMA-BUF image pool pre-warm failed: {e}"
                ))
            })?;
            drop(probe);
        }

        // 3. Tiled DMA-BUF image pool (DRM modifier). Only when the
        //    EGL probe found at least one render-target-capable
        //    modifier for ARGB8888 — that's the same gating
        //    `create_dma_buf_pools` uses for the tiled pool itself.
        //    Allocate-and-drop.
        if device.dma_buf_image_pool_tiled().is_some() {
            let bgra_modifiers = device
                .drm_modifier_table()
                .rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888);
            if !bgra_modifiers.is_empty() {
                let desc =
                    TextureDescriptor::new(PROBE_W, PROBE_H, TextureFormat::Bgra8Unorm)
                        .with_usage(
                            TextureUsages::RENDER_ATTACHMENT
                                | TextureUsages::TEXTURE_BINDING
                                | TextureUsages::COPY_SRC
                                | TextureUsages::COPY_DST,
                        );
                let probe = HostVulkanTexture::new_render_target_dma_buf(
                    device,
                    &desc,
                    bgra_modifiers,
                )
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "tiled DMA-BUF image pool pre-warm failed: {e}"
                    ))
                })?;
                drop(probe);
            }
        }

        // 4. OPAQUE_FD HOST_VISIBLE buffer pool — retained sentinel.
        //    Small (8×8×4) because no large-allocation HOST_VISIBLE
        //    OPAQUE_FD consumer exists today; the sentinel exists to
        //    pin the per-handle-type kernel state.
        if let Some(pool) = device.opaque_fd_buffer_pool() {
            let sentinel = make_opaque_fd_buffer_sentinel(
                pool,
                "opaque_fd_host_visible",
                (PROBE_W as vk::DeviceSize)
                    * (PROBE_H as vk::DeviceSize)
                    * (PROBE_BPP as vk::DeviceSize),
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                /* mapped */ true,
            )?;
            sentinels.push(sentinel);
        }

        // 5. OPAQUE_FD DEVICE_LOCAL buffer pool — retained sentinel.
        //    Small (8×8×4) so it pins the per-handle-type kernel state
        //    without competing with consumer allocations for any
        //    NVIDIA-side total-byte cap on OPAQUE_FD allocations.
        //    Empirically (#637 E2E run on Cam Link 4K), a sentinel
        //    sized at the consumer's resolution (1920×1080×4 ≈ 8 MiB)
        //    *deterministically* blocked the consumer's same-size
        //    allocation post-swapchain, indicating NVIDIA tracks a
        //    cumulative byte budget — so the sentinel must be tiny.
        if let Some(pool) = device.opaque_fd_buffer_pool_device_local() {
            let sentinel = make_opaque_fd_buffer_sentinel(
                pool,
                "opaque_fd_device_local",
                (PROBE_W as vk::DeviceSize)
                    * (PROBE_H as vk::DeviceSize)
                    * (PROBE_BPP as vk::DeviceSize),
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                /* mapped */ false,
            )?;
            sentinels.push(sentinel);
        }

        for s in &sentinels {
            tracing::info!(
                "HostVulkanDevice export pool sentinel retained: {} ({} bytes)",
                s.label,
                s.size,
            );
        }
        tracing::info!("HostVulkanDevice export pools pre-warmed");
        Ok(sentinels)
    }

}

/// Allocate one OPAQUE_FD-exportable buffer through the given VMA
/// pool and wrap it in an [`ExportPoolSentinel`]. Used by
/// [`HostVulkanDevice::prewarm_export_pools`] to pin the kernel-side
/// state for the OPAQUE_FD handle type for the device's lifetime.
///
/// Bypasses [`super::HostVulkanPixelBuffer`] deliberately: that wrapper
/// holds an `Arc<HostVulkanDevice>` for cleanup, which would create a
/// reference cycle when stored as a field on the device itself. The
/// sentinel holds only raw `vk::Buffer` + `vma::Allocation`; cleanup
/// runs in the device's `Drop` impl using the still-live allocator.
#[cfg(target_os = "linux")]
fn make_opaque_fd_buffer_sentinel(
    pool: &vma::Pool,
    label: &'static str,
    size: vk::DeviceSize,
    required_flags: vk::MemoryPropertyFlags,
    mapped: bool,
) -> Result<ExportPoolSentinel> {
    let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
        .build();

    let buffer_info = vk::BufferCreateInfo::builder()
        .size(size)
        .usage(
            vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_buffer_info);

    let mut flags = vma::AllocationCreateFlags::DEDICATED_MEMORY;
    if mapped {
        flags |= vma::AllocationCreateFlags::MAPPED
            | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE;
    }
    let alloc_opts = vma::AllocationOptions {
        flags,
        required_flags,
        ..Default::default()
    };

    let (buffer, allocation) = unsafe { pool.create_buffer(buffer_info, &alloc_opts) }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "OPAQUE_FD export pool '{label}' sentinel allocation failed \
                 (size={size}): {e}"
            ))
        })?;

    Ok(ExportPoolSentinel {
        buffer,
        allocation,
        label,
        size,
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
        .extent(vk::Extent3D { width: 64, height: 64, depth: 1 })
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
        .map_err(|e| {
            StreamError::GpuError(format!(
                "tiled-DMA-BUF probe vkCreateImage failed: {e}"
            ))
        })?;

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
    Err(StreamError::GpuError(format!(
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
        vma::Pool,
        vma::Pool,
        Option<vma::Pool>,
        Box<vk::ExportMemoryAllocateInfo>,
        Box<vk::ExportMemoryAllocateInfo>,
        Option<Box<vk::ExportMemoryAllocateInfo>>,
    )> {
        // ── Find memory type for HOST_VISIBLE DMA-BUF exportable buffers ──
        // The probe must mirror the real buffer create info used by
        // `HostVulkanPixelBuffer::new`, including the `ExternalMemoryBufferCreateInfo`
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
            allocator.find_memory_type_index_for_buffer_info(
                probe_buffer_info,
                &probe_buffer_alloc_opts,
            )
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "find memory type for DMA-BUF buffer pool: {e}"
            ))
        })?;

        // ── Find memory type for DEVICE_LOCAL DMA-BUF exportable images ──
        // Same rationale: the real image (`HostVulkanTexture::new`) carries
        // `ExternalMemoryImageCreateInfo::DMA_BUF_EXT` which can narrow
        // `memoryTypeBits`.
        let mut probe_image_external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D { width: 64, height: 64, depth: 1 })
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
            allocator.find_memory_type_index_for_image_info(
                probe_image_info,
                &probe_image_alloc_opts,
            )
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "find memory type for DMA-BUF image pool: {e}"
            ))
        })?;

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
            .map_err(|e| StreamError::GpuError(format!("create DMA-BUF buffer pool: {e}")))?;

        let mut image_pool_options = vma::PoolOptions::default();
        image_pool_options = image_pool_options.push_next(image_export_info.as_mut());
        image_pool_options.memory_type_index = image_mem_type_idx;
        let image_pool = allocator
            .create_pool(&image_pool_options)
            .map_err(|e| StreamError::GpuError(format!("create DMA-BUF image pool: {e}")))?;

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
                            (Some(tiled_pool), Some(tiled_export_info))
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
            buffer_pool,
            image_pool,
            tiled_pool,
            buffer_export_info,
            image_export_info,
            tiled_export_info,
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
    /// The export info `Box` is returned alongside the pool — callers
    /// must keep it alive for the pool's entire lifetime (VMA stores
    /// raw pointers via `pMemoryAllocateNext`).
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_buffer_pool(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(vma::Pool, Box<vk::ExportMemoryAllocateInfo>)> {
        // Probe with the same shape `HostVulkanPixelBuffer::new_opaque_fd_export`
        // will use: HOST_VISIBLE | HOST_COHERENT, TRANSFER_SRC | TRANSFER_DST,
        // chained `VkExternalMemoryBufferCreateInfo::OPAQUE_FD`. The probe's
        // memoryTypeBits must match the real buffer's or the bind fails
        // with VUID-vkBindBufferMemory-memory-01035.
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
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        let mem_type_idx = unsafe {
            allocator.find_memory_type_index_for_buffer_info(
                probe_buffer_info,
                &probe_alloc_opts,
            )
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "find memory type for OPAQUE_FD buffer pool: {e}"
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
        let pool = allocator
            .create_pool(&pool_options)
            .map_err(|e| StreamError::GpuError(format!("create OPAQUE_FD buffer pool: {e}")))?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool created — mem_type={}",
            mem_type_idx
        );
        Ok((pool, export_info))
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
    ) -> Result<(vma::Pool, Box<vk::ExportMemoryAllocateInfo>)> {
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
            allocator.find_memory_type_index_for_buffer_info(
                probe_buffer_info,
                &probe_alloc_opts,
            )
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
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
            StreamError::GpuError(format!(
                "create DEVICE_LOCAL OPAQUE_FD buffer pool: {e}"
            ))
        })?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool (DEVICE_LOCAL) created — mem_type={}",
            mem_type_idx
        );
        Ok((pool, export_info))
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
        Err(StreamError::GpuError(format!(
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
        &self.entry
    }

    /// Get the Vulkan instance.
    #[allow(dead_code)]
    pub fn instance(&self) -> &vulkanalia::Instance {
        &self.instance
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

    /// Whether DMA-BUF external memory extensions are available.
    pub fn supports_external_memory(&self) -> bool {
        self.supports_external_memory
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

    /// Look up the mutex that guards a given queue handle.
    fn mutex_for_queue(&self, queue: vk::Queue) -> &Mutex<()> {
        if queue == self.queue {
            &self.graphics_queue_mutex
        } else if queue == self.transfer_queue {
            &self.transfer_queue_mutex
        } else if self.video_encode_queue == Some(queue) {
            &self.video_encode_queue_mutex
        } else if self.video_decode_queue == Some(queue) {
            &self.video_decode_queue_mutex
        } else if self.compute_queue == Some(queue) {
            &self.compute_queue_mutex
        } else {
            // Unknown queue — fall back to graphics mutex as safety net
            &self.graphics_queue_mutex
        }
    }

    /// Submit command buffers to a queue with per-queue mutex synchronization.
    pub unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        let _lock = self.mutex_for_queue(queue).lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.queue_submit2(queue, submits, fence) }
            .map(|_| ())
            .map_err(|e| StreamError::GpuError(format!("queue_submit2 failed: {e}")))
    }

    /// Present to a queue with per-queue mutex synchronization.
    pub unsafe fn present_to_queue(
        &self,
        queue: vk::Queue,
        present_info: &vk::PresentInfoKHR,
    ) -> std::result::Result<vk::SuccessCode, vk::ErrorCode> {
        let _lock = self.mutex_for_queue(queue).lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.queue_present_khr(queue, present_info) }
    }

    /// Acquire the device-level mutex for resource creation operations.
    pub fn lock_device(&self) -> std::sync::MutexGuard<'_, ()> {
        self.device_mutex.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl vulkan_video::RhiQueueSubmitter for HostVulkanDevice {
    unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> vulkanalia::VkResult<()> {
        let _lock = self.mutex_for_queue(queue).lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.queue_submit2(queue, submits, fence) }.map(|_| ())
    }

    fn with_device_resource_lock(&self, f: &mut dyn FnMut()) {
        let _guard = self.lock_device();
        f();
    }
}

impl VulkanRhiDevice for HostVulkanDevice {
    type Privilege = HostMarker;

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
    ) -> streamlib_consumer_rhi::Result<()> {
        unsafe { HostVulkanDevice::submit_to_queue(self, queue, submits, fence) }
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }
}

impl HostVulkanDevice {
    /// Copy a host-visible VkBuffer to a device-local VkImage (RGBA upload).
    ///
    /// Transitions the image UNDEFINED → TRANSFER_DST → SHADER_READ_ONLY.
    pub unsafe fn upload_buffer_to_image(
        &self,
        src_buffer: vk::Buffer,
        dst_image: vk::Image,
        width: u32,
        height: u32,
    ) -> crate::core::Result<()> {
        use crate::core::StreamError;

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
        }.map_err(|e| StreamError::GpuError(format!("upload cmd pool: {e}")))?;

        let cb = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }.map_err(|e| StreamError::GpuError(format!("upload cmd buf: {e}")))?[0];

        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .map_err(|e| StreamError::GpuError(format!("upload fence: {e}")))?;

        unsafe {
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }.map_err(|e| StreamError::GpuError(format!("begin cb: {e}")))?;

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
        let dep_to_dst = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers_to_dst);
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
            image_extent: vk::Extent3D { width, height, depth: 1 },
        };
        unsafe {
            device.cmd_copy_buffer_to_image(
                cb, src_buffer, dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region],
            )
        };

        // Barrier: TRANSFER_DST → SHADER_READ_ONLY
        let barrier_to_read = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
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
        let dep_to_read = vk::DependencyInfo::builder()
            .image_memory_barriers(&barriers_to_read);
        unsafe { device.cmd_pipeline_barrier2(cb, &dep_to_read) };

        unsafe { device.end_command_buffer(cb) }
            .map_err(|e| StreamError::GpuError(format!("end cb: {e}")))?;

        let cb_submit = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cb)
            .build();
        let cb_submits = [cb_submit];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cb_submits)
            .build();
        unsafe { self.submit_to_queue(queue, &[submit], fence) }?;
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| StreamError::GpuError(format!("wait: {e}")))?;

        unsafe { device.destroy_fence(fence, None) };
        unsafe { device.destroy_command_pool(pool, None) };

        Ok(())
    }

    /// Get the VMA allocator for GPU memory management.
    pub fn allocator(&self) -> &Arc<vma::Allocator> {
        self.allocator.as_ref().expect("VMA allocator not initialized")
    }

    /// Import external memory from a DMA-BUF file descriptor.
    ///
    /// Uses raw `vkAllocateMemory` with `VkImportMemoryFdInfoKHR` since VMA
    /// does not support importing external memory from file descriptors.
    /// All non-import allocations go through VMA.
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

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to import DMA-BUF memory: {e}")))?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            "HostVulkanDevice: DMA-BUF memory imported ({} bytes, type={}, live={})",
            allocation_size, memory_type_index, count
        );

        Ok(memory)
    }

    /// Get the VMA pool for DMA-BUF exportable HOST_VISIBLE buffers.
    /// Returns None if external memory is not supported on this device.
    #[cfg(target_os = "linux")]
    pub fn dma_buf_buffer_pool(&self) -> Option<&vma::Pool> {
        self.dma_buf_buffer_pool.as_ref()
    }

    /// Get the VMA pool for DMA-BUF exportable DEVICE_LOCAL images.
    /// Returns None if external memory is not supported on this device.
    #[cfg(target_os = "linux")]
    pub fn dma_buf_image_pool(&self) -> Option<&vma::Pool> {
        self.dma_buf_image_pool.as_ref()
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
        self.dma_buf_image_pool_tiled.as_ref()
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers.
    ///
    /// Returns `None` when external memory isn't supported on this device,
    /// or when the pool's memory-type probe failed at construction.
    /// Used by CUDA / OpenCL interop — see
    /// [`super::HostVulkanPixelBuffer::new_opaque_fd_export`].
    #[cfg(target_os = "linux")]
    pub fn opaque_fd_buffer_pool(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool.as_ref()
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL buffers
    /// (GPU-resident sibling of [`Self::opaque_fd_buffer_pool`]).
    ///
    /// Returns `None` when external memory isn't supported or the
    /// DEVICE_LOCAL probe failed at construction. Used by GPU-resident
    /// CUDA interop paths — see
    /// [`super::HostVulkanPixelBuffer::new_opaque_fd_export_device_local`].
    #[cfg(target_os = "linux")]
    pub fn opaque_fd_buffer_pool_device_local(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool_device_local.as_ref()
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

    /// Render-target-capable DRM format modifiers, by DRM FOURCC, from the
    /// EGL probe at device init. Empty when the probe failed. Callers
    /// pass [`DrmModifierTable::rt_modifiers`] into
    /// [`HostVulkanTexture::new_render_target_dma_buf`].
    #[cfg(target_os = "linux")]
    pub fn drm_modifier_table(&self) -> &Arc<DrmModifierTable> {
        &self.drm_modifier_table
    }

    /// Free device memory allocated via raw vkAllocateMemory (import path only).
    ///
    /// VMA-managed allocations are freed via [`vma::Allocator::destroy_image`] or
    /// [`vma::Allocator::destroy_buffer`] — do not call this for VMA allocations.
    pub fn free_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.free_memory(memory, None) };
        self.live_allocation_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Map imported device memory for CPU access (DMA-BUF import path only).
    pub fn map_imported_memory(
        &self,
        memory: vk::DeviceMemory,
        size: vk::DeviceSize,
    ) -> Result<*mut u8> {
        let ptr = unsafe {
            self.device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
        }
        .map_err(|e| StreamError::GpuError(format!("Failed to map device memory: {e}")))?;
        Ok(ptr as *mut u8)
    }

    /// Unmap imported device memory.
    pub fn unmap_imported_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.unmap_memory(memory) };
    }

    /// Current number of live DMA-BUF import-path allocations.
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

        unsafe {
            let _ = self.device.device_wait_idle();
        }

        // Critical drop order:
        //  0. OPAQUE_FD export sentinels — free via the still-live
        //     allocator before any pool is destroyed (vmaDestroyPool
        //     refuses to run while live allocations exist).
        //  1. DMA-BUF + OPAQUE_FD pools — release Arc<Allocator> refs and call vmaDestroyPool
        //  2. Allocator — call vmaDestroyAllocator (only after all Arc refs gone)
        //  3. Export info Boxes — VMA no longer references them after pool destruction
        //  4. Device + instance — Vulkan handles
        #[cfg(target_os = "linux")]
        {
            if let Some(allocator) = self.allocator.as_ref() {
                for sentinel in self.opaque_fd_export_sentinels.drain(..) {
                    unsafe {
                        allocator.destroy_buffer(sentinel.buffer, sentinel.allocation);
                    }
                }
            }
            drop(self.dma_buf_buffer_pool.take());
            drop(self.dma_buf_image_pool.take());
            drop(self.dma_buf_image_pool_tiled.take());
            drop(self.opaque_fd_buffer_pool.take());
            drop(self.opaque_fd_buffer_pool_device_local.take());
        }

        drop(self.allocator.take());

        #[cfg(target_os = "linux")]
        {
            drop(self._dma_buf_buffer_export_info.take());
            drop(self._dma_buf_image_export_info.take());
            drop(self._dma_buf_image_tiled_export_info.take());
            drop(self._opaque_fd_buffer_export_info.take());
            drop(self._opaque_fd_buffer_export_info_device_local.take());
        }

        unsafe {
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

// HostVulkanDevice is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for HostVulkanDevice {}
unsafe impl Sync for HostVulkanDevice {}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_device_creation() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        assert!(!device.name().is_empty(), "Device name should not be empty");
        println!("Vulkan device created: {}", device.name());
    }

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

    /// Issue #637 — the OPAQUE_FD export sentinels must be retained
    /// on the device after construction. Drop-and-free flakes
    /// intermittently on NVIDIA Linux because no compositor-side
    /// live OPAQUE_FD allocation exists to keep the per-handle-type
    /// kernel state alive (unlike DMA-BUF, which the swapchain
    /// imports). This test asserts the sentinels are present and
    /// cover both OPAQUE_FD pools when the driver supports them. If
    /// `prewarm_export_pools` reverts to dropping its OPAQUE_FD
    /// probes, this test fails — locking the regression at the data-
    /// structure level so CI catches it without needing the manual
    /// Cam Link 4K reproducer.
    #[cfg(target_os = "linux")]
    #[test]
    fn opaque_fd_export_sentinels_retained_for_each_supported_pool() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        // Build the expected set from which OPAQUE_FD pools the device
        // actually constructed; we can't assert a hardcoded count
        // without overfitting to one driver. The empty set is only
        // legitimate when neither pool was created (no external memory
        // support); every other shape is a real assertion.
        let mut expected_labels: Vec<&'static str> = Vec::new();
        if device.opaque_fd_buffer_pool().is_some() {
            expected_labels.push("opaque_fd_host_visible");
        }
        if device.opaque_fd_buffer_pool_device_local().is_some() {
            expected_labels.push("opaque_fd_device_local");
        }
        if expected_labels.is_empty() {
            println!(
                "Skipping — no OPAQUE_FD pools on this driver, so no sentinels expected"
            );
            return;
        }

        let actual_labels: Vec<&'static str> = device
            .opaque_fd_export_sentinels
            .iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(
            actual_labels, expected_labels,
            "every constructed OPAQUE_FD pool must have a retained sentinel; \
             missing sentinels would let NVIDIA's kernel-side state decay \
             between init and the consumer's allocation (issue #637)"
        );

        // The DEVICE_LOCAL sentinel must be SMALL — sentinels at
        // the consumer's full size deterministically block the
        // consumer's allocation post-swapchain (empirically: #637
        // E2E on Cam Link 4K), pointing at a cumulative byte budget
        // on NVIDIA's side. The sentinel exists only to pin the
        // per-handle-type kernel state, so it must not compete with
        // consumer-class allocations. Reverting to a realistic-size
        // sentinel here would re-introduce that regression.
        if let Some(s) = device
            .opaque_fd_export_sentinels
            .iter()
            .find(|s| s.label == "opaque_fd_device_local")
        {
            let max_acceptable: vk::DeviceSize = 64 * 1024;
            assert!(
                s.size <= max_acceptable,
                "DEVICE_LOCAL OPAQUE_FD sentinel must be small (≤ 64 KiB) to \
                 avoid competing with consumer allocations on NVIDIA's \
                 cumulative OPAQUE_FD byte budget (got {} bytes, max {})",
                s.size,
                max_acceptable,
            );
        }

        // The buffer + allocation handles must be non-null (a sentinel
        // that never actually allocated would not pin any kernel state).
        for s in device.opaque_fd_export_sentinels.iter() {
            assert_ne!(
                s.buffer,
                vk::Buffer::null(),
                "sentinel '{}' has null VkBuffer",
                s.label,
            );
        }
    }

    #[cfg(target_os = "linux")]
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

        let buffer_size: vk::DeviceSize = 4096;

        // Create exportable buffer via VMA
        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(buffer_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };

        let (buffer, allocation) = unsafe {
            device.allocator().create_buffer(buffer_info, &alloc_opts)
        }
        .expect("create exportable buffer via VMA");

        // Get allocation info to access the underlying DeviceMemory for export
        let alloc_info = device.allocator().get_allocation_info(allocation);
        let memory = alloc_info.deviceMemory;

        // Export DMA-BUF fd via vulkanalia extension trait
        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();

        let fd = unsafe { device.device().get_memory_fd_khr(&get_fd_info) }
            .expect("export DMA-BUF fd");

        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
        println!("Exported DMA-BUF fd: {fd}");

        // Import the fd back
        let mem_reqs = unsafe { device.device().get_buffer_memory_requirements(buffer) };
        let _imported = device.import_dma_buf_memory(
            fd,
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

    #[test]
    fn test_physical_device_supports_vulkan_1_4() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        let props = unsafe {
            device
                .instance()
                .get_physical_device_properties(device.physical_device())
        };

        // Vulkan version is packed: (variant<<29) | (major<<22) | (minor<<12) | patch
        let major = props.api_version >> 22;
        let minor = (props.api_version >> 12) & 0x3ff;

        assert!(
            major > 1 || (major == 1 && minor >= 4),
            "Physical device must support Vulkan 1.4 for this codebase, got {major}.{minor}"
        );

        println!(
            "Physical device Vulkan version: {}.{}.{}",
            major,
            minor,
            props.api_version & 0xfff
        );
    }
}
