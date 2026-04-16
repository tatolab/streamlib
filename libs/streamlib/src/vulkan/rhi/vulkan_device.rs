// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan device implementation for RHI.

use std::ffi::{c_char, CStr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vulkanalia::loader::{LibloadingLoader, LIBRARY};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;
use vma::Alloc as _;

use crate::core::rhi::TextureDescriptor;
use crate::core::{Result, StreamError};

use super::{VulkanCommandQueue, VulkanTexture};

/// Vulkan GPU device.
///
/// Wraps the Vulkan instance, physical device, and logical device.
/// On macOS/iOS, uses MoltenVK to provide Vulkan API on top of Metal.
pub struct VulkanDevice {
    entry: vulkanalia::Entry,
    instance: vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    /// Memory properties kept for DMA-BUF import path (raw vkAllocateMemory).
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    device: vulkanalia::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    transfer_queue_family_index: u32,
    #[allow(dead_code)]
    device_name: String,
    supports_external_memory: bool,
    supports_video_encode: bool,
    video_encode_queue_family_index: Option<u32>,
    video_encode_queue: Option<vk::Queue>,
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
    /// Backing storage for the buffer pool's VkExportMemoryAllocateInfo. VMA stores
    /// a raw pointer to this struct via pMemoryAllocateNext, so we must keep it
    /// alive for the pool's entire lifetime.
    #[cfg(target_os = "linux")]
    _dma_buf_buffer_export_info: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Backing storage for the image pool's VkExportMemoryAllocateInfo.
    #[cfg(target_os = "linux")]
    _dma_buf_image_export_info: Option<Box<vk::ExportMemoryAllocateInfo>>,
    /// Tracks DMA-BUF import-path allocations (raw vkAllocateMemory for import only).
    live_allocation_count: AtomicUsize,
}

impl VulkanDevice {
    /// Create a new Vulkan device.
    ///
    /// On macOS/iOS, this loads MoltenVK and enables VK_EXT_metal_objects
    /// for Metal interoperability.
    pub fn new() -> Result<Self> {
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

        // 7. Create logical device with required extensions
        let queue_priorities = [1.0f32];
        let mut queue_create_infos = vec![vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)
            .build()];

        // Request a separate video encode queue if it's a different family
        if let Some(ve_family) = video_encode_queue_family_index {
            if ve_family != queue_family_index {
                queue_create_infos.push(
                    vk::DeviceQueueCreateInfo::builder()
                        .queue_family_index(ve_family)
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

            // VK_KHR_dynamic_rendering — renderpass-free graphics pipelines
            let dynamic_rendering_ext = c"VK_KHR_dynamic_rendering";
            if available_device_ext_names.contains(&dynamic_rendering_ext) {
                device_extensions.push(dynamic_rendering_ext.as_ptr());
                tracing::info!("VK_KHR_dynamic_rendering enabled");
            }
        }

        // On Linux, check for Vulkan Video encode extensions
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

            // VK_KHR_synchronization2 is a mandatory dependency of VK_KHR_video_encode_queue
            let has_synchronization2 =
                available_device_ext_names.contains(&c"VK_KHR_synchronization2");

            let all_present = has_video_queue
                && has_video_encode_queue
                && has_video_encode_h264
                && has_synchronization2
                && video_encode_queue_family_index.is_some();

            if all_present {
                device_extensions.push(c"VK_KHR_synchronization2".as_ptr());
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
                    "Vulkan Video encode not available (queue={}, encode_queue={}, h264={}, h265={}, sync2={}, queue_family={})",
                    has_video_queue,
                    has_video_encode_queue,
                    has_video_encode_h264,
                    has_video_encode_h265,
                    has_synchronization2,
                    video_encode_queue_family_index.is_some()
                );
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
        let device_create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .push_next(&mut dynamic_rendering_features)
            .push_next(&mut timeline_semaphore_features)
            .push_next(&mut synchronization2_features)
            .build();

        #[cfg(not(target_os = "linux"))]
        let device_create_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .build();

        let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create logical device: {e}")))?;

        // 8. Get the graphics queue
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        // 8b. Get the video encode queue (if available)
        let video_encode_queue = if supports_video_encode {
            video_encode_queue_family_index.map(|ve_family| unsafe {
                device.get_device_queue(ve_family, 0)
            })
        } else {
            None
        };

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

        // Build DMA-BUF export pools on Linux when external memory is supported.
        #[cfg(target_os = "linux")]
        let (
            dma_buf_buffer_pool,
            dma_buf_image_pool,
            dma_buf_buffer_export_info,
            dma_buf_image_export_info,
        ) = if supports_external_memory {
            match Self::create_dma_buf_pools(&allocator) {
                Ok((bp, ip, bi, ii)) => (Some(bp), Some(ip), Some(bi), Some(ii)),
                Err(e) => {
                    tracing::warn!(
                        "DMA-BUF export pools could not be created — falling back to \
                         default pool for exportable allocations (may fail on NVIDIA \
                         after swapchain creation): {e}"
                    );
                    (None, None, None, None)
                }
            }
        } else {
            (None, None, None, None)
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

        Ok(Self {
            entry,
            instance,
            physical_device,
            memory_properties,
            device,
            queue,
            queue_family_index,
            transfer_queue_family_index,
            device_name: device_name.into_owned(),
            supports_external_memory,
            supports_video_encode,
            video_encode_queue_family_index,
            video_encode_queue,
            allocator: Some(allocator),
            #[cfg(target_os = "linux")]
            dma_buf_buffer_pool,
            #[cfg(target_os = "linux")]
            dma_buf_image_pool,
            #[cfg(target_os = "linux")]
            _dma_buf_buffer_export_info: dma_buf_buffer_export_info,
            #[cfg(target_os = "linux")]
            _dma_buf_image_export_info: dma_buf_image_export_info,
            live_allocation_count: AtomicUsize::new(0),
        })
    }

    /// Build VMA pools dedicated to DMA-BUF exportable allocations.
    ///
    /// Each pool is pinned to a memory type that supports the relevant property
    /// flags and carries VkExportMemoryAllocateInfo::DMA_BUF_EXT via
    /// pMemoryAllocateNext. The export info structs are heap-boxed and returned
    /// alongside the pools — the caller must keep them alive for the pool's
    /// lifetime (VMA stores raw pointers to them).
    #[cfg(target_os = "linux")]
    fn create_dma_buf_pools(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        vma::Pool,
        vma::Pool,
        Box<vk::ExportMemoryAllocateInfo>,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        // ── Find memory type for HOST_VISIBLE DMA-BUF exportable buffers ──
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
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
            .initial_layout(vk::ImageLayout::UNDEFINED);
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
        // them alongside the pools. Drop order (handled by VulkanDevice::drop):
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

        tracing::info!(
            "DMA-BUF VMA pools created — buffer mem_type={}, image mem_type={}",
            buffer_mem_type_idx,
            image_mem_type_idx
        );

        Ok((buffer_pool, image_pool, buffer_export_info, image_export_info))
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
    pub fn create_texture(self: &Arc<Self>, desc: &TextureDescriptor) -> Result<VulkanTexture> {
        VulkanTexture::new(self, desc)
    }

    /// Create a VulkanCommandQueue wrapper for the shared command queue.
    pub fn create_command_queue_wrapper(&self) -> VulkanCommandQueue {
        VulkanCommandQueue::new(self.device.clone(), self.queue, self.queue_family_index)
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
            "VulkanDevice: DMA-BUF memory imported ({} bytes, type={}, live={})",
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

impl Drop for VulkanDevice {
    fn drop(&mut self) {
        let live = self.live_allocation_count.load(Ordering::Relaxed);
        if live > 0 {
            tracing::warn!(
                "VulkanDevice dropping with {} live import allocations (leak)",
                live
            );
        }

        unsafe {
            let _ = self.device.device_wait_idle();
        }

        // Critical drop order:
        //  1. DMA-BUF pools — release Arc<Allocator> refs and call vmaDestroyPool
        //  2. Allocator — call vmaDestroyAllocator (only after all Arc refs gone)
        //  3. Export info Boxes — VMA no longer references them after pool destruction
        //  4. Device + instance — Vulkan handles
        #[cfg(target_os = "linux")]
        {
            drop(self.dma_buf_buffer_pool.take());
            drop(self.dma_buf_image_pool.take());
        }

        drop(self.allocator.take());

        #[cfg(target_os = "linux")]
        {
            drop(self._dma_buf_buffer_export_info.take());
            drop(self._dma_buf_image_export_info.take());
        }

        unsafe {
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

// VulkanDevice is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for VulkanDevice {}
unsafe impl Sync for VulkanDevice {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Try to create a VulkanDevice; return None if GPU/Vulkan is unavailable (CI).
    fn try_create_device() -> Option<Arc<VulkanDevice>> {
        match VulkanDevice::new() {
            Ok(d) => Some(Arc::new(d)),
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
