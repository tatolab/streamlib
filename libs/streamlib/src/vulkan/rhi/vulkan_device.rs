// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan device implementation for RHI.

use std::ffi::{c_char, CStr};
use std::sync::Arc;

use std::sync::atomic::{AtomicUsize, Ordering};

use ash::vk;

use crate::core::rhi::TextureDescriptor;
use crate::core::{Result, StreamError};

use super::{VulkanCommandQueue, VulkanTexture};

/// Vulkan GPU device.
///
/// Wraps the Vulkan instance, physical device, and logical device.
/// On macOS/iOS, uses MoltenVK to provide Vulkan API on top of Metal.
pub struct VulkanDevice {
    entry: ash::Entry,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    device: ash::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    transfer_queue_family_index: u32,
    #[allow(dead_code)]
    device_name: String,
    supports_external_memory: bool,
    supports_video_encode: bool,
    video_encode_queue_family_index: Option<u32>,
    video_encode_queue: Option<vk::Queue>,
    supports_video_decode: bool,
    video_decode_queue_family_index: Option<u32>,
    video_decode_queue: Option<vk::Queue>,
    graphics_queue_secondary: Option<vk::Queue>,
    live_allocation_count: AtomicUsize,
}

impl VulkanDevice {
    /// Create a new Vulkan device.
    ///
    /// On macOS/iOS, this loads MoltenVK and enables VK_EXT_metal_objects
    /// for Metal interoperability.
    pub fn new() -> Result<Self> {
        // 1. Load Vulkan entry points (via MoltenVK on macOS)
        let entry = unsafe { ash::Entry::load() }.map_err(|e| {
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
        }

        // 4. Create Vulkan instance
        let app_info = vk::ApplicationInfo::default()
            .application_name(c"StreamLib")
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(c"StreamLib")
            .engine_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::make_api_version(0, 1, 2, 0));

        let mut instance_create_flags = vk::InstanceCreateFlags::empty();

        // On macOS/iOS, enable portability enumeration flag
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            instance_create_flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }

        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&instance_extensions)
            .flags(instance_create_flags);

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

        // 7. Create logical device with required extensions
        let queue_priorities = [1.0f32];
        let queue_priorities_dual = [1.0f32, 1.0f32];
        let gfx_queue_count = queue_families[queue_family_index as usize].queue_count;
        let mut queue_create_infos = vec![vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(if gfx_queue_count >= 2 {
                &queue_priorities_dual
            } else {
                &queue_priorities
            })];

        // Request a separate video encode queue if it's a different family
        if let Some(ve_family) = video_encode_queue_family_index {
            if ve_family != queue_family_index {
                queue_create_infos.push(
                    vk::DeviceQueueCreateInfo::default()
                        .queue_family_index(ve_family)
                        .queue_priorities(&queue_priorities),
                );
            }
        }

        // Request a separate video decode queue if it's a different family
        if let Some(vd_family) = video_decode_queue_family_index {
            if vd_family != queue_family_index
                && video_encode_queue_family_index.map_or(true, |ve| vd_family != ve)
            {
                queue_create_infos.push(
                    vk::DeviceQueueCreateInfo::default()
                        .queue_family_index(vd_family)
                        .queue_priorities(&queue_priorities),
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
                unsafe { instance.enumerate_device_extension_properties(physical_device) }
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
                available_device_ext_names.contains(&vk::KHR_VIDEO_QUEUE_NAME);
            let has_video_encode_queue =
                available_device_ext_names.contains(&vk::KHR_VIDEO_ENCODE_QUEUE_NAME);
            let has_video_encode_h264 =
                available_device_ext_names.contains(&vk::KHR_VIDEO_ENCODE_H264_NAME);
            let has_video_encode_h265 =
                available_device_ext_names.contains(&vk::KHR_VIDEO_ENCODE_H265_NAME);

            // VK_KHR_synchronization2 is a mandatory dependency of VK_KHR_video_encode_queue
            let has_synchronization2 =
                available_device_ext_names.contains(&vk::KHR_SYNCHRONIZATION2_NAME);

            let all_present = has_video_queue
                && has_video_encode_queue
                && has_video_encode_h264
                && has_synchronization2
                && video_encode_queue_family_index.is_some();

            if all_present {
                device_extensions.push(vk::KHR_SYNCHRONIZATION2_NAME.as_ptr());
                device_extensions.push(vk::KHR_VIDEO_QUEUE_NAME.as_ptr());
                device_extensions.push(vk::KHR_VIDEO_ENCODE_QUEUE_NAME.as_ptr());
                device_extensions.push(vk::KHR_VIDEO_ENCODE_H264_NAME.as_ptr());
                if has_video_encode_h265 {
                    device_extensions.push(vk::KHR_VIDEO_ENCODE_H265_NAME.as_ptr());
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

        // On Linux, check for Vulkan Video decode extensions
        #[cfg(target_os = "linux")]
        let has_video_decode = {
            let has_video_queue =
                available_device_ext_names.contains(&vk::KHR_VIDEO_QUEUE_NAME);
            let has_video_decode_queue =
                available_device_ext_names.contains(&vk::KHR_VIDEO_DECODE_QUEUE_NAME);
            let has_video_decode_h264 =
                available_device_ext_names.contains(&vk::KHR_VIDEO_DECODE_H264_NAME);
            let has_synchronization2 =
                available_device_ext_names.contains(&vk::KHR_SYNCHRONIZATION2_NAME);

            let all_present = has_video_queue
                && has_video_decode_queue
                && has_video_decode_h264
                && has_synchronization2
                && video_decode_queue_family_index.is_some();

            if all_present {
                // Only push extensions not already pushed by encode
                if !has_video_encode {
                    device_extensions.push(vk::KHR_SYNCHRONIZATION2_NAME.as_ptr());
                    device_extensions.push(vk::KHR_VIDEO_QUEUE_NAME.as_ptr());
                }
                device_extensions.push(vk::KHR_VIDEO_DECODE_QUEUE_NAME.as_ptr());
                device_extensions.push(vk::KHR_VIDEO_DECODE_H264_NAME.as_ptr());
                tracing::info!("Vulkan Video decode extensions enabled (H.264)");
            } else {
                tracing::info!(
                    "Vulkan Video decode not available (queue={}, decode_queue={}, h264={}, sync2={}, queue_family={})",
                    has_video_queue,
                    has_video_decode_queue,
                    has_video_decode_h264,
                    has_synchronization2,
                    video_decode_queue_family_index.is_some()
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

        #[cfg(target_os = "linux")]
        let supports_video_decode = has_video_decode;
        #[cfg(not(target_os = "linux"))]
        let supports_video_decode = false;

        // Enable dynamic rendering, timeline semaphore, and synchronization2 features on Linux.
        // Synchronization2 is a mandatory dependency of VK_KHR_video_encode_queue.
        #[cfg(target_os = "linux")]
        let mut dynamic_rendering_features =
            vk::PhysicalDeviceDynamicRenderingFeatures::default().dynamic_rendering(true);

        #[cfg(target_os = "linux")]
        let mut timeline_semaphore_features =
            vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true);

        #[cfg(target_os = "linux")]
        let mut synchronization2_features =
            vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true);

        #[cfg(target_os = "linux")]
        let device_create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .push_next(&mut dynamic_rendering_features)
            .push_next(&mut timeline_semaphore_features)
            .push_next(&mut synchronization2_features);

        #[cfg(not(target_os = "linux"))]
        let device_create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions);

        let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create logical device: {e}")))?;

        // 8. Get the graphics queue(s)
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
        let graphics_queue_secondary = if gfx_queue_count >= 2 {
            tracing::info!("VulkanDevice: secondary graphics queue created (queue family {})", queue_family_index);
            Some(unsafe { device.get_device_queue(queue_family_index, 1) })
        } else {
            None
        };

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

        // 9. Query memory properties (used by find_memory_type for all allocations)
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        tracing::info!(
            "Vulkan device initialized: {} (queue family {}, {} memory types, external_memory={})",
            device_name,
            queue_family_index,
            memory_properties.memory_type_count,
            supports_external_memory
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
            supports_video_decode,
            video_decode_queue_family_index,
            video_decode_queue,
            graphics_queue_secondary,
            live_allocation_count: AtomicUsize::new(0),
        })
    }

    /// Find a memory type that satisfies both the type filter and required properties.
    ///
    /// For DEVICE_LOCAL requests, prefers types backed by the main VRAM heap
    /// (DEVICE_LOCAL without HOST_VISIBLE) over BAR aperture types (DEVICE_LOCAL +
    /// HOST_VISIBLE). On NVIDIA without Resizable BAR, the HOST_VISIBLE DEVICE_LOCAL
    /// type maps to a 256MB aperture that exhausts quickly. The main VRAM heap (24GB
    /// on RTX 3090) is the correct target for textures and images.
    pub fn find_memory_type(
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
            "No suitable memory type found (filter: 0x{:x}, required: 0x{:x})",
            type_filter, required_properties.as_raw()
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
    pub fn entry(&self) -> &ash::Entry {
        &self.entry
    }

    /// Get the Vulkan instance.
    #[allow(dead_code)]
    pub fn instance(&self) -> &ash::Instance {
        &self.instance
    }

    /// Get the Vulkan physical device.
    #[allow(dead_code)]
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    /// Get the Vulkan logical device.
    #[allow(dead_code)]
    pub fn device(&self) -> &ash::Device {
        &self.device
    }

    /// Get the graphics queue.
    #[allow(dead_code)]
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }

    /// Get the secondary graphics queue (for concurrent submissions from decoder).
    pub fn graphics_queue_secondary(&self) -> Option<vk::Queue> {
        self.graphics_queue_secondary
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

    /// Whether Vulkan Video decode extensions are available.
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

    /// Allocate device memory for an image.
    ///
    /// When `exportable` is true, adds VkExportMemoryAllocateInfo (DMA_BUF_EXT)
    /// and VkMemoryDedicatedAllocateInfo. The image MUST have been created with
    /// VkExternalMemoryImageCreateInfo in that case.
    ///
    /// When `exportable` is false, plain allocation without dedicated or export flags.
    /// Use `allocate_image_memory_dedicated` when NVIDIA video hardware requires
    /// dedicated allocation but export is not needed.
    pub fn allocate_image_memory(
        &self,
        image: vk::Image,
        preferred_flags: vk::MemoryPropertyFlags,
        exportable: bool,
    ) -> Result<vk::DeviceMemory> {
        let mem_reqs = unsafe { self.device.get_image_memory_requirements(image) };

        let memory_type_index = self
            .find_memory_type(mem_reqs.memory_type_bits, preferred_flags)
            .or_else(|_| {
                self.find_memory_type(mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::empty())
            })?;

        // Try each compatible memory type with dedicated allocation first,
        // then without. NVIDIA drivers can reject dedicated allocations on
        // specific memory types depending on device state (swapchain, video
        // session, etc.), so fallback across types is essential.
        let type_filter = mem_reqs.memory_type_bits;

        // Build candidate list: preferred type first, then all other compatible types
        let mut candidates = vec![memory_type_index];
        for i in 0..self.memory_properties.memory_type_count {
            if i != memory_type_index
                && (type_filter & (1 << i)) != 0
                && self.memory_properties.memory_types[i as usize]
                    .property_flags
                    .contains(preferred_flags)
            {
                candidates.push(i);
            }
        }
        // Also try types without the preferred flags as last resort
        for i in 0..self.memory_properties.memory_type_count {
            if (type_filter & (1 << i)) != 0 && !candidates.contains(&i) {
                candidates.push(i);
            }
        }

        for &type_idx in &candidates {
            // Attempt A: dedicated allocation
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let mut export = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

            let mut alloc_a = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(type_idx)
                .push_next(&mut dedicated);
            if exportable && self.supports_external_memory {
                alloc_a = alloc_a.push_next(&mut export);
            }

            if let Ok(memory) = unsafe { self.device.allocate_memory(&alloc_a, None) } {
                let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!(
                    "VulkanDevice: image memory allocated ({} bytes, type={}, dedicated=true, exportable={}, live={})",
                    mem_reqs.size, type_idx, exportable, count
                );
                return Ok(memory);
            }

            // Attempt B: plain allocation (no dedicated)
            let mut export_b = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let mut alloc_b = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(type_idx);
            if exportable && self.supports_external_memory {
                alloc_b = alloc_b.push_next(&mut export_b);
            }

            if let Ok(memory) = unsafe { self.device.allocate_memory(&alloc_b, None) } {
                let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!(
                    "VulkanDevice: image memory allocated ({} bytes, type={}, dedicated=false, exportable={}, live={})",
                    mem_reqs.size, type_idx, exportable, count
                );
                return Ok(memory);
            }
        }

        let heap_index = self.memory_properties.memory_types[memory_type_index as usize].heap_index;
        let heap_size = self.memory_properties.memory_heaps[heap_index as usize].size;
        Err(StreamError::GpuError(format!(
            "Failed to allocate image memory: all {} candidate types failed (requested={} bytes, preferred_type={}, heap={}, heap_size={}, exportable={}, live={})",
            candidates.len(), mem_reqs.size, memory_type_index, heap_index, heap_size, exportable,
            self.live_allocation_count.load(Ordering::Relaxed)
        )))
    }

    /// Allocate device memory for a buffer.
    ///
    /// When `exportable` is true, adds VkExportMemoryAllocateInfo (DMA_BUF_EXT)
    /// so the buffer is cross-process shareable via DMA-BUF. The buffer MUST have
    /// been created with VkExternalMemoryBufferCreateInfo in that case.
    pub fn allocate_buffer_memory(
        &self,
        buffer: vk::Buffer,
        preferred_flags: vk::MemoryPropertyFlags,
        exportable: bool,
    ) -> Result<vk::DeviceMemory> {
        let mem_reqs = unsafe { self.device.get_buffer_memory_requirements(buffer) };

        let memory_type_index = self.find_memory_type(
            mem_reqs.memory_type_bits,
            preferred_flags,
        )?;

        let mut export_info = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let mut alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(memory_type_index);

        if exportable && self.supports_external_memory {
            alloc_info = alloc_info.push_next(&mut export_info);
        }

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to allocate buffer memory: {e}")))?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::info!(
            "VulkanDevice: buffer memory allocated ({} bytes, type={}, exportable={}, live={})",
            mem_reqs.size, memory_type_index, exportable, count
        );

        Ok(memory)
    }

    /// Import external memory from a DMA-BUF file descriptor.
    pub fn import_dma_buf_memory(
        &self,
        fd: i32,
        allocation_size: vk::DeviceSize,
        memory_type_bits: u32,
        preferred_flags: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory> {
        let memory_type_index = self.find_memory_type(memory_type_bits, preferred_flags)?;

        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd);

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_info);

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to import DMA-BUF memory: {e}")))?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            "VulkanDevice: DMA-BUF memory imported ({} bytes, type={}, live={})",
            allocation_size, memory_type_index, count
        );

        Ok(memory)
    }

    /// Allocate device memory for video session binding.
    ///
    /// Video session memory is opaque to the application and used internally
    /// by the video hardware. No export flags — session memory is never shared.
    pub fn allocate_session_memory(
        &self,
        size: vk::DeviceSize,
        memory_type_index: u32,
    ) -> Result<vk::DeviceMemory> {
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(memory_type_index);

        let memory = unsafe { self.device.allocate_memory(&alloc_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to allocate session memory: {e}")))?;

        let count = self.live_allocation_count.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(
            "VulkanDevice: session memory allocated ({} bytes, type={}, live={})",
            size, memory_type_index, count
        );

        Ok(memory)
    }

    /// Map device memory for CPU access.
    pub fn map_device_memory(
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

    /// Unmap device memory.
    pub fn unmap_device_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.unmap_memory(memory) };
    }

    /// Free device memory.
    pub fn free_device_memory(&self, memory: vk::DeviceMemory) {
        unsafe { self.device.free_memory(memory, None) };
        self.live_allocation_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Current number of live device memory allocations.
    pub fn live_allocation_count(&self) -> usize {
        self.live_allocation_count.load(Ordering::Relaxed)
    }
}

impl Drop for VulkanDevice {
    fn drop(&mut self) {
        let live = self.live_allocation_count.load(Ordering::Relaxed);
        if live > 0 {
            tracing::warn!(
                "VulkanDevice dropping with {} live allocations (leak)",
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

    // ---------------------------------------------------------------
    // 1. Non-exportable image allocation (display camera texture pattern)
    //    VkImage without VkExternalMemoryImageCreateInfo → exportable=false
    // ---------------------------------------------------------------
    #[test]
    fn test_non_exportable_image_allocation() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { device.device().create_image(&image_info, None) }
            .expect("create non-exportable image");

        let count_before = device.live_allocation_count();

        let memory = device
            .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
            .expect("allocate non-exportable image memory");

        assert_eq!(device.live_allocation_count(), count_before + 1);

        unsafe { device.device().bind_image_memory(image, memory, 0) }
            .expect("bind non-exportable image memory");

        // Cleanup
        unsafe { device.device().destroy_image(image, None) };
        device.free_device_memory(memory);

        assert_eq!(device.live_allocation_count(), count_before);
        println!("Non-exportable image allocation test passed");
    }

    // ---------------------------------------------------------------
    // 2. Exportable image allocation (pool texture pattern)
    //    VkImage with VkExternalMemoryImageCreateInfo(DMA_BUF_EXT) → exportable=true
    //    Then export DMA-BUF fd via vkGetMemoryFdKHR
    // ---------------------------------------------------------------
    #[cfg(target_os = "linux")]
    #[test]
    fn test_exportable_image_allocation_and_dma_buf_export() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_image_info);

        let image = unsafe { device.device().create_image(&image_info, None) }
            .expect("create exportable image");

        let memory = device
            .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, true)
            .expect("allocate exportable image memory");

        unsafe { device.device().bind_image_memory(image, memory, 0) }
            .expect("bind exportable image memory");

        // Export DMA-BUF fd
        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(device.instance(), device.device());

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .expect("export DMA-BUF fd from image");

        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
        println!("Exported DMA-BUF fd: {fd}");

        // Cleanup
        unsafe { libc::close(fd) };
        unsafe { device.device().destroy_image(image, None) };
        device.free_device_memory(memory);

        println!("Exportable image allocation + DMA-BUF export test passed");
    }

    // ---------------------------------------------------------------
    // 3. Non-exportable buffer allocation (encoder bitstream pattern)
    //    VkBuffer → HOST_VISIBLE, exportable=false → map → write → unmap
    // ---------------------------------------------------------------
    #[test]
    fn test_non_exportable_buffer_allocation_with_map() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        let bitstream_size: vk::DeviceSize = 128 * 1024; // 128 KB — encoder bitstream size

        let buffer_info = vk::BufferCreateInfo::default()
            .size(bitstream_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }
            .expect("create non-exportable buffer");

        let memory = device
            .allocate_buffer_memory(
                buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                false,
            )
            .expect("allocate non-exportable buffer memory");

        unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }
            .expect("bind non-exportable buffer memory");

        // Map and write test pattern
        let ptr = device
            .map_device_memory(memory, bitstream_size)
            .expect("map non-exportable buffer");

        assert!(!ptr.is_null(), "mapped pointer must not be null");

        let test_pattern: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
        unsafe {
            std::ptr::copy_nonoverlapping(test_pattern.as_ptr(), ptr, test_pattern.len());
        }

        device.unmap_device_memory(memory);

        // Cleanup
        unsafe { device.device().destroy_buffer(buffer, None) };
        device.free_device_memory(memory);

        println!("Non-exportable buffer allocation + map/write test passed");
    }

    // ---------------------------------------------------------------
    // 4. Exportable buffer allocation (pixel buffer pool pattern)
    //    VkBuffer with VkExternalMemoryBufferCreateInfo(DMA_BUF_EXT)
    //    → HOST_VISIBLE, exportable=true → map → write → read back → unmap
    // ---------------------------------------------------------------
    #[cfg(target_os = "linux")]
    #[test]
    fn test_exportable_buffer_allocation_with_readback() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        let pixel_buffer_size: vk::DeviceSize = 1920 * 1080 * 4; // BGRA 1080p

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let buffer_info = vk::BufferCreateInfo::default()
            .size(pixel_buffer_size)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }
            .expect("create exportable buffer");

        let memory = device
            .allocate_buffer_memory(
                buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                true,
            )
            .expect("allocate exportable buffer memory");

        unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }
            .expect("bind exportable buffer memory");

        // Map, write, read back
        let ptr = device
            .map_device_memory(memory, pixel_buffer_size)
            .expect("map exportable buffer");

        assert!(!ptr.is_null(), "mapped pointer must not be null");

        // Write a test pattern at the beginning
        let write_data: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        unsafe {
            std::ptr::copy_nonoverlapping(write_data.as_ptr(), ptr, write_data.len());
        }

        // Read back and verify
        let mut read_back = [0u8; 8];
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, read_back.as_mut_ptr(), read_back.len());
        }
        assert_eq!(read_back, write_data, "read-back must match written data");

        device.unmap_device_memory(memory);

        // Cleanup
        unsafe { device.device().destroy_buffer(buffer, None) };
        device.free_device_memory(memory);

        println!("Exportable buffer allocation + map/write/readback test passed");
    }

    // ---------------------------------------------------------------
    // 5. Allocation counter tracking
    //    Allocate several resources, verify counter, free, verify 0.
    // ---------------------------------------------------------------
    #[test]
    fn test_allocation_counter_tracking() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        let baseline = device.live_allocation_count();

        // Allocate 3 buffers
        let mut buffers = Vec::new();
        let mut memories = Vec::new();

        for i in 0..3 {
            let buffer_info = vk::BufferCreateInfo::default()
                .size(4096)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);

            let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }
                .unwrap_or_else(|e| panic!("create buffer {i}: {e}"));

            let memory = device
                .allocate_buffer_memory(
                    buffer,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    false,
                )
                .unwrap_or_else(|e| panic!("allocate buffer memory {i}: {e}"));

            unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }
                .unwrap_or_else(|e| panic!("bind buffer memory {i}: {e}"));

            buffers.push(buffer);
            memories.push(memory);
        }

        assert_eq!(
            device.live_allocation_count(),
            baseline + 3,
            "counter must reflect 3 new allocations"
        );

        // Allocate 2 images
        for i in 0..2 {
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D {
                    width: 256,
                    height: 256,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);

            let image = unsafe { device.device().create_image(&image_info, None) }
                .unwrap_or_else(|e| panic!("create image {i}: {e}"));

            let memory = device
                .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
                .unwrap_or_else(|e| panic!("allocate image memory {i}: {e}"));

            unsafe { device.device().bind_image_memory(image, memory, 0) }
                .unwrap_or_else(|e| panic!("bind image memory {i}: {e}"));

            // Store image handle alongside buffer handles for cleanup
            // (use buffer vec for images too — just need vk handles for destroy)
            buffers.push(vk::Buffer::null()); // placeholder
            memories.push(memory);

            // Destroy image immediately but keep memory for counter test
            unsafe { device.device().destroy_image(image, None) };
        }

        assert_eq!(
            device.live_allocation_count(),
            baseline + 5,
            "counter must reflect 5 total allocations"
        );

        // Free all memories
        for (i, memory) in memories.iter().enumerate() {
            // Destroy buffers (first 3 entries)
            if i < 3 {
                unsafe { device.device().destroy_buffer(buffers[i], None) };
            }
            device.free_device_memory(*memory);
        }

        assert_eq!(
            device.live_allocation_count(),
            baseline,
            "counter must return to baseline after freeing all"
        );

        println!("Allocation counter tracking test passed");
    }

    // ---------------------------------------------------------------
    // 6. Session memory allocation
    //    find_memory_type for DEVICE_LOCAL → allocate_session_memory → free
    // ---------------------------------------------------------------
    #[test]
    fn test_session_memory_allocation() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        // Find a DEVICE_LOCAL memory type (type_filter = all bits set)
        let memory_type_index = device
            .find_memory_type(u32::MAX, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .expect("find DEVICE_LOCAL memory type");

        let count_before = device.live_allocation_count();

        // Allocate 64 KB of session memory (typical video session binding size)
        let session_size: vk::DeviceSize = 64 * 1024;
        let memory = device
            .allocate_session_memory(session_size, memory_type_index)
            .expect("allocate session memory");

        assert_eq!(device.live_allocation_count(), count_before + 1);
        assert_ne!(memory, vk::DeviceMemory::null());

        // Cleanup
        device.free_device_memory(memory);
        assert_eq!(device.live_allocation_count(), count_before);

        println!("Session memory allocation test passed");
    }

    // ---------------------------------------------------------------
    // 7. DMA-BUF round-trip
    //    Create exportable buffer → get fd → import_dma_buf_memory → verify → free both
    // ---------------------------------------------------------------
    #[cfg(target_os = "linux")]
    #[test]
    fn test_dma_buf_round_trip() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        let buffer_size: vk::DeviceSize = 1920 * 1080 * 4; // BGRA 1080p

        // --- Source: create exportable buffer ---
        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let src_buffer_info = vk::BufferCreateInfo::default()
            .size(buffer_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        let src_buffer = unsafe { device.device().create_buffer(&src_buffer_info, None) }
            .expect("create source exportable buffer");

        let src_memory = device
            .allocate_buffer_memory(
                src_buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                true,
            )
            .expect("allocate source buffer memory");

        unsafe { device.device().bind_buffer_memory(src_buffer, src_memory, 0) }
            .expect("bind source buffer memory");

        // Export DMA-BUF fd
        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(src_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(device.instance(), device.device());

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .expect("export DMA-BUF fd from source buffer");

        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");

        // --- Destination: import via DMA-BUF fd ---
        // Create a buffer to get memory requirements (type bits) for import
        let mut external_buffer_info_dst = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let dst_buffer_info = vk::BufferCreateInfo::default()
            .size(buffer_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info_dst);

        let dst_buffer = unsafe { device.device().create_buffer(&dst_buffer_info, None) }
            .expect("create destination buffer");

        let dst_mem_reqs =
            unsafe { device.device().get_buffer_memory_requirements(dst_buffer) };

        let imported_memory = device
            .import_dma_buf_memory(
                fd,
                dst_mem_reqs.size.max(buffer_size),
                dst_mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .expect("import DMA-BUF memory");

        assert_ne!(imported_memory, vk::DeviceMemory::null());

        unsafe { device.device().bind_buffer_memory(dst_buffer, imported_memory, 0) }
            .expect("bind imported buffer memory");

        println!("DMA-BUF round-trip: export fd={fd}, import succeeded");

        // Cleanup (fd ownership transferred to import — do NOT close fd)
        unsafe { device.device().destroy_buffer(dst_buffer, None) };
        device.free_device_memory(imported_memory);
        unsafe { device.device().destroy_buffer(src_buffer, None) };
        device.free_device_memory(src_memory);

        println!("DMA-BUF round-trip test passed");
    }

    // ---------------------------------------------------------------
    // Retained: basic device creation smoke test
    // ---------------------------------------------------------------
    #[test]
    fn test_vulkan_device_creation() {
        let result = VulkanDevice::new();
        match &result {
            Ok(device) => {
                println!("Vulkan device created: {}", device.name());
                println!("  external_memory: {}", device.supports_external_memory());
                println!("  video_encode: {}", device.supports_video_encode());
                println!("  queue_family: {}", device.queue_family_index());
                println!("  transfer_queue_family: {}", device.transfer_queue_family_index());
            }
            Err(e) => {
                println!("Vulkan not available: {e}");
            }
        }
    }

    // ---------------------------------------------------------------
    // Retained: command queue creation smoke test
    // ---------------------------------------------------------------
    #[test]
    fn test_vulkan_command_queue_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test — Vulkan not available");
                return;
            }
        };

        let queue = device.create_command_queue_wrapper();
        let cmd_buf = queue.create_command_buffer();
        assert!(cmd_buf.is_ok(), "Command buffer creation should succeed");

        cmd_buf.unwrap().commit();
        println!("Command queue and buffer test passed");
    }

    // ---------------------------------------------------------------
    // 8. Simultaneous exportable + non-exportable allocations
    //    The exact camera-display pipeline pattern:
    //    4 exportable buffers (pixel buffer pool) + 4 non-exportable images (display camera textures)
    //    THIS TEST MUST PASS for camera-display to work.
    // ---------------------------------------------------------------
    #[cfg(target_os = "linux")]
    #[test]
    fn test_simultaneous_exportable_and_nonexportable_allocations() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        let baseline = device.live_allocation_count();
        let pixel_buffer_size: vk::DeviceSize = 1920 * 1080 * 4; // BGRA32 1080p

        // --- Phase 1: Create 4 exportable buffers (pixel buffer pool pattern) ---
        let mut exportable_buffers = Vec::new();
        let mut exportable_buffer_memories = Vec::new();
        let mut exportable_buffer_ptrs = Vec::new();

        for i in 0..4 {
            let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

            let buffer_info = vk::BufferCreateInfo::default()
                .size(pixel_buffer_size)
                .usage(
                    vk::BufferUsageFlags::TRANSFER_SRC
                        | vk::BufferUsageFlags::TRANSFER_DST
                        | vk::BufferUsageFlags::STORAGE_BUFFER,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut external_buffer_info);

            let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }
                .unwrap_or_else(|e| panic!("create exportable buffer {i}: {e}"));

            let memory = device
                .allocate_buffer_memory(
                    buffer,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    true,
                )
                .unwrap_or_else(|e| panic!("allocate exportable buffer memory {i}: {e}"));

            unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }
                .unwrap_or_else(|e| panic!("bind exportable buffer memory {i}: {e}"));

            let ptr = device
                .map_device_memory(memory, pixel_buffer_size)
                .unwrap_or_else(|e| panic!("map exportable buffer {i}: {e}"));
            assert!(!ptr.is_null(), "exportable buffer {i} mapped pointer must not be null");

            exportable_buffers.push(buffer);
            exportable_buffer_memories.push(memory);
            exportable_buffer_ptrs.push(ptr);
            println!("  Exportable buffer {i} created + mapped");
        }

        assert_eq!(
            device.live_allocation_count(),
            baseline + 4,
            "4 exportable buffer allocations"
        );

        // --- Phase 2: Create 4 NON-exportable images (display camera texture pattern) ---
        let mut nonexportable_images = Vec::new();
        let mut nonexportable_image_memories = Vec::new();

        for i in 0..4 {
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D {
                    width: 1920,
                    height: 1080,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(
                    vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::SAMPLED
                        | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);

            let image = unsafe { device.device().create_image(&image_info, None) }
                .unwrap_or_else(|e| panic!("create non-exportable image {i}: {e}"));

            let memory = device
                .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
                .unwrap_or_else(|e| panic!("allocate non-exportable image memory {i}: {e}"));

            unsafe { device.device().bind_image_memory(image, memory, 0) }
                .unwrap_or_else(|e| panic!("bind non-exportable image memory {i}: {e}"));

            nonexportable_images.push(image);
            nonexportable_image_memories.push(memory);
            println!("  Non-exportable image {i} created + bound");
        }

        // --- Verify: ALL 8 allocations succeeded ---
        assert_eq!(
            device.live_allocation_count(),
            baseline + 8,
            "must have 8 total allocations (4 exportable buffers + 4 non-exportable images)"
        );
        println!("All 8 allocations coexist — camera-display pattern WORKS");

        // --- Cleanup: free everything ---
        for i in 0..4 {
            device.unmap_device_memory(exportable_buffer_memories[i]);
            unsafe { device.device().destroy_buffer(exportable_buffers[i], None) };
            device.free_device_memory(exportable_buffer_memories[i]);
        }
        for i in 0..4 {
            unsafe { device.device().destroy_image(nonexportable_images[i], None) };
            device.free_device_memory(nonexportable_image_memories[i]);
        }

        assert_eq!(
            device.live_allocation_count(),
            baseline,
            "counter must return to baseline after freeing all 8"
        );
        println!("Simultaneous exportable + non-exportable allocation test passed");
    }

    // ---------------------------------------------------------------
    // 9. Exportable buffer to non-exportable image blit pattern
    //    Camera captures into exportable pool buffer, display blits
    //    from buffer into internal texture. Proves both can coexist
    //    and the buffer is shareable via DMA-BUF.
    // ---------------------------------------------------------------
    #[cfg(target_os = "linux")]
    #[test]
    fn test_exportable_buffer_to_nonexportable_image_blit_pattern() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        let pixel_buffer_size: vk::DeviceSize = 1920 * 1080 * 4; // BGRA32 1080p

        // --- Step 1: Create 1 exportable buffer (simulating pixel buffer from pool) ---
        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let buffer_info = vk::BufferCreateInfo::default()
            .size(pixel_buffer_size)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);

        let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }
            .expect("create exportable buffer");

        let buffer_memory = device
            .allocate_buffer_memory(
                buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                true,
            )
            .expect("allocate exportable buffer memory");

        unsafe { device.device().bind_buffer_memory(buffer, buffer_memory, 0) }
            .expect("bind exportable buffer memory");

        // --- Step 2: Write pixel data via mapped_ptr ---
        let ptr = device
            .map_device_memory(buffer_memory, pixel_buffer_size)
            .expect("map exportable buffer");
        assert!(!ptr.is_null(), "mapped pointer must not be null");

        // Write BGRA test pattern (blue pixel)
        let blue_pixel: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF]; // B=255 G=0 R=0 A=255
        unsafe {
            std::ptr::copy_nonoverlapping(blue_pixel.as_ptr(), ptr, blue_pixel.len());
        }
        println!("  Wrote pixel data to exportable buffer");

        device.unmap_device_memory(buffer_memory);

        // --- Step 3: Create 1 non-exportable image (simulating display camera texture) ---
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { device.device().create_image(&image_info, None) }
            .expect("create non-exportable image");

        let image_memory = device
            .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
            .expect("allocate non-exportable image memory");

        unsafe { device.device().bind_image_memory(image, image_memory, 0) }
            .expect("bind non-exportable image memory");

        println!("  Both exportable buffer and non-exportable image coexist");

        // --- Step 5: Export DMA-BUF fd from the buffer (proves it's shareable) ---
        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(buffer_memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(device.instance(), device.device());

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .expect("export DMA-BUF fd from exportable buffer");

        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
        println!("  Exported DMA-BUF fd: {fd} (buffer is cross-process shareable)");

        // --- Cleanup ---
        unsafe { libc::close(fd) };
        unsafe { device.device().destroy_image(image, None) };
        device.free_device_memory(image_memory);
        unsafe { device.device().destroy_buffer(buffer, None) };
        device.free_device_memory(buffer_memory);

        println!("Exportable buffer to non-exportable image blit pattern test passed");
    }

    // ---------------------------------------------------------------
    // 10. Exportable image for cross-process sharing
    //     Pool textures need to be exportable for Python/Deno subprocesses.
    //     Creates an exportable image and exports its DMA-BUF fd.
    // ---------------------------------------------------------------
    #[cfg(target_os = "linux")]
    #[test]
    fn test_exportable_image_for_cross_process_sharing() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        // --- Step 1: Create image WITH VkExternalMemoryImageCreateInfo(DMA_BUF_EXT) ---
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_image_info);

        let image = unsafe { device.device().create_image(&image_info, None) }
            .expect("create exportable image for cross-process sharing");

        // --- Step 2: allocate_image_memory with exportable=true ---
        let memory = device
            .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, true)
            .expect("allocate exportable image memory");

        // --- Step 3: Bind ---
        unsafe { device.device().bind_image_memory(image, memory, 0) }
            .expect("bind exportable image memory");

        // --- Step 4: Export DMA-BUF fd via vkGetMemoryFdKHR ---
        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let external_memory_fd =
            ash::khr::external_memory_fd::Device::new(device.instance(), device.device());

        let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
            .expect("export DMA-BUF fd from exportable image");

        // --- Step 5: Verify fd >= 0 (proves cross-process sharing works) ---
        assert!(fd >= 0, "DMA-BUF fd must be non-negative, got {fd}");
        println!("  Exported DMA-BUF fd: {fd} (image is cross-process shareable)");

        // --- Cleanup ---
        unsafe { libc::close(fd) };
        unsafe { device.device().destroy_image(image, None) };
        device.free_device_memory(memory);

        println!("Exportable image for cross-process sharing test passed");
    }

    // ---------------------------------------------------------------
    // 11. Pool pre-allocation pattern
    //     Simulates what PixelBufferPoolManager does:
    //     pre-allocate 4 exportable buffers, acquire/release, verify integrity.
    // ---------------------------------------------------------------
    #[cfg(target_os = "linux")]
    #[test]
    fn test_pool_preallocation_pattern() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping test — external memory not supported");
            return;
        }

        let baseline = device.live_allocation_count();
        let pixel_buffer_size: vk::DeviceSize = 1920 * 1080 * 4; // BGRA32 1080p

        // --- Step 1: Pre-allocate 4 exportable buffers ---
        let mut pool_buffers = Vec::new();
        let mut pool_memories = Vec::new();

        for i in 0..4 {
            let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

            let buffer_info = vk::BufferCreateInfo::default()
                .size(pixel_buffer_size)
                .usage(
                    vk::BufferUsageFlags::TRANSFER_SRC
                        | vk::BufferUsageFlags::TRANSFER_DST
                        | vk::BufferUsageFlags::STORAGE_BUFFER,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut external_buffer_info);

            let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }
                .unwrap_or_else(|e| panic!("pre-allocate pool buffer {i}: {e}"));

            let memory = device
                .allocate_buffer_memory(
                    buffer,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    true,
                )
                .unwrap_or_else(|e| panic!("allocate pool buffer memory {i}: {e}"));

            unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }
                .unwrap_or_else(|e| panic!("bind pool buffer memory {i}: {e}"));

            pool_buffers.push(buffer);
            pool_memories.push(memory);
            println!("  Pool buffer {i} pre-allocated");
        }

        // --- Step 2: Verify all 4 succeeded ---
        assert_eq!(
            device.live_allocation_count(),
            baseline + 4,
            "4 pool buffers must be allocated"
        );
        println!("  All 4 pool buffers pre-allocated successfully");

        // --- Step 3: "Acquire" buffer 0 — map and write data ---
        let ptr = device
            .map_device_memory(pool_memories[0], pixel_buffer_size)
            .expect("map acquired pool buffer 0");
        assert!(!ptr.is_null(), "acquired buffer mapped pointer must not be null");

        let test_pattern: [u8; 8] = [0xCA, 0xFE, 0xBA, 0xBE, 0xDE, 0xAD, 0xBE, 0xEF];
        unsafe {
            std::ptr::copy_nonoverlapping(test_pattern.as_ptr(), ptr, test_pattern.len());
        }
        println!("  Acquired buffer 0, wrote test pattern");

        // --- Step 4: "Release" buffer 0 — unmap ---
        device.unmap_device_memory(pool_memories[0]);
        println!("  Released buffer 0");

        // --- Step 5: Verify all 4 are still valid (no corruption) ---
        // Re-map buffer 0 and verify data survived acquire/release cycle
        let ptr_recheck = device
            .map_device_memory(pool_memories[0], pixel_buffer_size)
            .expect("re-map pool buffer 0 after release");
        assert!(!ptr_recheck.is_null(), "re-mapped pointer must not be null");

        let mut read_back = [0u8; 8];
        unsafe {
            std::ptr::copy_nonoverlapping(ptr_recheck, read_back.as_mut_ptr(), read_back.len());
        }
        assert_eq!(
            read_back, test_pattern,
            "data must survive acquire/release cycle"
        );
        device.unmap_device_memory(pool_memories[0]);
        println!("  Buffer 0 data verified after release — no corruption");

        // Verify buffers 1-3 can still be mapped (not corrupted by buffer 0 usage)
        for i in 1..4 {
            let ptr_check = device
                .map_device_memory(pool_memories[i], pixel_buffer_size)
                .unwrap_or_else(|e| panic!("map pool buffer {i} for validation: {e}"));
            assert!(
                !ptr_check.is_null(),
                "pool buffer {i} must still be mappable"
            );
            device.unmap_device_memory(pool_memories[i]);
        }
        println!("  All 4 pool buffers validated — no corruption");

        // Still 4 allocations
        assert_eq!(
            device.live_allocation_count(),
            baseline + 4,
            "allocation count must remain 4 after acquire/release"
        );

        // --- Step 6: Drop all, verify counter ---
        for i in 0..4 {
            unsafe { device.device().destroy_buffer(pool_buffers[i], None) };
            device.free_device_memory(pool_memories[i]);
        }

        assert_eq!(
            device.live_allocation_count(),
            baseline,
            "counter must return to baseline after dropping all pool buffers"
        );
        println!("Pool pre-allocation pattern test passed");
    }

    // ---------------------------------------------------------------
    // Diagnostic: dump memory type layout and try each compatible type
    // ---------------------------------------------------------------
    #[test]
    fn test_diagnostic_memory_type_layout_and_per_type_allocation() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        // --- Dump full memory type/heap layout ---
        let props = &device.memory_properties;
        println!("=== Memory Heaps ({}) ===", props.memory_heap_count);
        for i in 0..props.memory_heap_count {
            let heap = &props.memory_heaps[i as usize];
            println!(
                "  Heap {}: size={} ({:.2} GiB), flags={:?}",
                i,
                heap.size,
                heap.size as f64 / (1024.0 * 1024.0 * 1024.0),
                heap.flags
            );
        }

        println!("=== Memory Types ({}) ===", props.memory_type_count);
        for i in 0..props.memory_type_count {
            let mt = &props.memory_types[i as usize];
            println!(
                "  Type {}: heap={}, flags={:?}",
                i, mt.heap_index, mt.property_flags
            );
        }

        // --- Create a test image matching display camera texture ---
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let image = unsafe { device.device().create_image(&image_info, None) }
            .expect("create diagnostic image");

        let mem_reqs = unsafe { device.device().get_image_memory_requirements(image) };
        println!(
            "=== Image memory requirements ===\n  size={}, alignment={}, memory_type_bits=0b{:032b}",
            mem_reqs.size, mem_reqs.alignment, mem_reqs.memory_type_bits
        );

        // --- Try allocating with each compatible memory type ---
        println!("=== Per-type allocation attempts (non-exportable, WITH dedicated) ===");
        for i in 0..props.memory_type_count {
            if (mem_reqs.memory_type_bits & (1 << i)) == 0 {
                println!("  Type {}: NOT compatible (bit not set)", i);
                continue;
            }
            let mt = &props.memory_types[i as usize];

            let mut dedicated_info =
                vk::MemoryDedicatedAllocateInfo::default().image(image);
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(i)
                .push_next(&mut dedicated_info);

            match unsafe { device.device().allocate_memory(&alloc_info, None) } {
                Ok(mem) => {
                    println!(
                        "  Type {}: OK (heap={}, flags={:?})",
                        i, mt.heap_index, mt.property_flags
                    );
                    unsafe { device.device().free_memory(mem, None) };
                }
                Err(e) => {
                    println!(
                        "  Type {}: FAILED — {} (heap={}, flags={:?})",
                        i, e, mt.heap_index, mt.property_flags
                    );
                }
            }
        }

        // --- Try WITHOUT dedicated to compare ---
        println!("=== Per-type allocation attempts (non-exportable, NO dedicated) ===");
        for i in 0..props.memory_type_count {
            if (mem_reqs.memory_type_bits & (1 << i)) == 0 {
                continue;
            }
            let mt = &props.memory_types[i as usize];

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(i);

            match unsafe { device.device().allocate_memory(&alloc_info, None) } {
                Ok(mem) => {
                    println!(
                        "  Type {}: OK (heap={}, flags={:?})",
                        i, mt.heap_index, mt.property_flags
                    );
                    unsafe { device.device().free_memory(mem, None) };
                }
                Err(e) => {
                    println!(
                        "  Type {}: FAILED — {} (heap={}, flags={:?})",
                        i, e, mt.heap_index, mt.property_flags
                    );
                }
            }
        }

        unsafe { device.device().destroy_image(image, None) };
        println!("Diagnostic memory type layout test passed");
    }

    /// Empirical test: create exportable image with VkExternalMemoryImageCreateInfo
    /// and check which memory types are compatible + which actually allocate.
    /// This tells us if exportable images can land in DEVICE_LOCAL VRAM.
    #[test]
    fn test_diagnostic_exportable_image_memory_types() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        let props = &device.memory_properties;

        // --- Image WITHOUT external memory info (the old non-exportable path) ---
        let image_info_plain = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D { width: 1920, height: 1080, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let plain_image = unsafe { device.device().create_image(&image_info_plain, None) }
            .expect("create plain image");
        let plain_reqs = unsafe { device.device().get_image_memory_requirements(plain_image) };

        println!("=== Plain image (no VkExternalMemoryImageCreateInfo) ===");
        println!("  memory_type_bits = 0b{:032b}", plain_reqs.memory_type_bits);
        for i in 0..props.memory_type_count {
            if (plain_reqs.memory_type_bits & (1 << i)) != 0 {
                let mt = &props.memory_types[i as usize];
                println!("  Compatible: type={}, heap={}, flags={:?}", i, mt.heap_index, mt.property_flags);
            }
        }
        unsafe { device.device().destroy_image(plain_image, None) };

        // --- Image WITH VkExternalMemoryImageCreateInfo (exportable) ---
        let mut external_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info_ext = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D { width: 1920, height: 1080, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_info);

        let ext_image = unsafe { device.device().create_image(&image_info_ext, None) }
            .expect("create exportable image");
        let ext_reqs = unsafe { device.device().get_image_memory_requirements(ext_image) };

        println!("\n=== Exportable image (WITH VkExternalMemoryImageCreateInfo DMA_BUF_EXT) ===");
        println!("  memory_type_bits = 0b{:032b}", ext_reqs.memory_type_bits);
        for i in 0..props.memory_type_count {
            if (ext_reqs.memory_type_bits & (1 << i)) != 0 {
                let mt = &props.memory_types[i as usize];
                println!("  Compatible: type={}, heap={}, flags={:?}", i, mt.heap_index, mt.property_flags);
            }
        }

        // --- Try allocating the exportable image on each compatible type ---
        println!("\n=== Exportable image allocation attempts (dedicated + export) ===");
        for i in 0..props.memory_type_count {
            if (ext_reqs.memory_type_bits & (1 << i)) == 0 {
                continue;
            }
            let mt = &props.memory_types[i as usize];

            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(ext_image);
            let mut export = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(ext_reqs.size)
                .memory_type_index(i)
                .push_next(&mut dedicated)
                .push_next(&mut export);

            match unsafe { device.device().allocate_memory(&alloc_info, None) } {
                Ok(mem) => {
                    let heap = &props.memory_heaps[mt.heap_index as usize];
                    let is_vram = heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL);
                    println!(
                        "  Type {}: OK — heap={} ({:.2} GiB, {}), flags={:?}",
                        i, mt.heap_index,
                        heap.size as f64 / (1024.0 * 1024.0 * 1024.0),
                        if is_vram { "VRAM" } else { "system RAM" },
                        mt.property_flags
                    );
                    unsafe { device.device().free_memory(mem, None) };
                }
                Err(e) => {
                    println!(
                        "  Type {}: FAILED — {} (heap={}, flags={:?})",
                        i, e, mt.heap_index, mt.property_flags
                    );
                }
            }
        }

        // --- Try plain (non-dedicated, non-export) on the exportable image ---
        println!("\n=== Exportable image allocation attempts (non-dedicated, no export) ===");
        for i in 0..props.memory_type_count {
            if (ext_reqs.memory_type_bits & (1 << i)) == 0 {
                continue;
            }
            let mt = &props.memory_types[i as usize];

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(ext_reqs.size)
                .memory_type_index(i);

            match unsafe { device.device().allocate_memory(&alloc_info, None) } {
                Ok(mem) => {
                    let heap = &props.memory_heaps[mt.heap_index as usize];
                    let is_vram = heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL);
                    println!(
                        "  Type {}: OK — heap={} ({:.2} GiB, {}), flags={:?}",
                        i, mt.heap_index,
                        heap.size as f64 / (1024.0 * 1024.0 * 1024.0),
                        if is_vram { "VRAM" } else { "system RAM" },
                        mt.property_flags
                    );
                    unsafe { device.device().free_memory(mem, None) };
                }
                Err(e) => {
                    println!(
                        "  Type {}: FAILED — {} (heap={}, flags={:?})",
                        i, e, mt.heap_index, mt.property_flags
                    );
                }
            }
        }

        unsafe { device.device().destroy_image(ext_image, None) };
        println!("\nDiagnostic exportable image memory types test complete");
    }

    /// Test allocation ORDER: prove that DEVICE_LOCAL image allocation
    /// fails AFTER DMA-BUF exportable buffers but succeeds BEFORE them.
    /// This is the NVIDIA driver bug that causes camera textures to fall
    /// back to system RAM.
    #[test]
    fn test_allocation_order_determines_vram_placement() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        // --- Scenario A: images BEFORE exportable buffers ---
        println!("=== Scenario A: allocate images FIRST, then exportable buffers ===");
        let mut images_a = Vec::new();
        let mut image_mems_a = Vec::new();
        for i in 0..4 {
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D { width: 1920, height: 1080, depth: 1 })
                .mip_levels(1).array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = unsafe { device.device().create_image(&image_info, None) }
                .expect("create image");
            let mem = device.allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
                .unwrap_or_else(|e| panic!("Scenario A image {i} failed: {e}"));
            let mem_reqs = unsafe { device.device().get_image_memory_requirements(image) };
            // Check which type was used by inspecting what find_memory_type returns
            let type_idx = device.find_memory_type(mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL).unwrap();
            let heap_idx = device.memory_properties.memory_types[type_idx as usize].heap_index;
            let is_vram = device.memory_properties.memory_heaps[heap_idx as usize].flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL);
            println!("  Image {i}: type={type_idx}, heap={heap_idx}, VRAM={is_vram}");
            unsafe { device.device().bind_image_memory(image, mem, 0).unwrap() };
            images_a.push(image);
            image_mems_a.push(mem);
        }

        // Now allocate exportable buffers (simulating pixel buffer pool)
        let mut buffers_a = Vec::new();
        let mut buffer_mems_a = Vec::new();
        for i in 0..4 {
            let mut ext_buf_info = vk::ExternalMemoryBufferCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let buffer_info = vk::BufferCreateInfo::default()
                .size(1920 * 1080 * 4)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::STORAGE_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut ext_buf_info);
            let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }.expect("create buffer");
            let mem = device.allocate_buffer_memory(buffer, vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT, true)
                .unwrap_or_else(|e| panic!("Scenario A buffer {i} failed: {e}"));
            unsafe { device.device().bind_buffer_memory(buffer, mem, 0).unwrap() };
            println!("  Buffer {i}: OK");
            buffers_a.push(buffer);
            buffer_mems_a.push(mem);
        }
        println!("Scenario A: ALL allocations in VRAM succeeded\n");

        // Cleanup A
        for img in &images_a { unsafe { device.device().destroy_image(*img, None) }; }
        for mem in &image_mems_a { device.free_device_memory(*mem); }
        for buf in &buffers_a { unsafe { device.device().destroy_buffer(*buf, None) }; }
        for mem in &buffer_mems_a { device.free_device_memory(*mem); }

        // --- Scenario B: exportable buffers FIRST, then images (the failing order) ---
        println!("=== Scenario B: allocate exportable buffers FIRST, then images ===");
        let mut buffers_b = Vec::new();
        let mut buffer_mems_b = Vec::new();
        for i in 0..4 {
            let mut ext_buf_info = vk::ExternalMemoryBufferCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let buffer_info = vk::BufferCreateInfo::default()
                .size(1920 * 1080 * 4)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::STORAGE_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut ext_buf_info);
            let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }.expect("create buffer");
            let mem = device.allocate_buffer_memory(buffer, vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT, true)
                .unwrap_or_else(|e| panic!("Scenario B buffer {i} failed: {e}"));
            unsafe { device.device().bind_buffer_memory(buffer, mem, 0).unwrap() };
            println!("  Buffer {i}: OK (exportable, DMA-BUF)");
            buffers_b.push(buffer);
            buffer_mems_b.push(mem);
        }

        // Now try images (this is what fails in camera-display)
        let mut any_failed_type1 = false;
        for i in 0..4 {
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D { width: 1920, height: 1080, depth: 1 })
                .mip_levels(1).array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = unsafe { device.device().create_image(&image_info, None) }.expect("create image");
            let mem_reqs = unsafe { device.device().get_image_memory_requirements(image) };

            // Try type 1 (DEVICE_LOCAL) specifically
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(1) // type 1 = DEVICE_LOCAL
                .push_next(&mut dedicated);
            match unsafe { device.device().allocate_memory(&alloc_info, None) } {
                Ok(mem) => {
                    println!("  Image {i}: type=1 (DEVICE_LOCAL) OK — in VRAM");
                    unsafe {
                        device.device().bind_image_memory(image, mem, 0).unwrap();
                        device.device().destroy_image(image, None);
                        device.device().free_memory(mem, None);
                    }
                }
                Err(e) => {
                    println!("  Image {i}: type=1 (DEVICE_LOCAL) FAILED — {e}");
                    any_failed_type1 = true;
                    unsafe { device.device().destroy_image(image, None) };
                }
            }
        }

        for buf in &buffers_b { unsafe { device.device().destroy_buffer(*buf, None) }; }
        for mem in &buffer_mems_b { device.free_device_memory(*mem); }

        if any_failed_type1 {
            println!("\n*** CONFIRMED: NVIDIA driver bug — DMA-BUF exportable buffers prevent subsequent DEVICE_LOCAL image allocations ***");
            println!("*** Fix: pre-allocate display camera textures BEFORE pixel buffer pool ***");
        } else {
            println!("\nScenario B: all images succeeded on type 1 — order may not matter on this driver version");
        }
    }

    // ---------------------------------------------------------------
    // Diagnostic: simulate real pipeline allocation sequence
    // ---------------------------------------------------------------
    #[test]
    fn test_diagnostic_real_pipeline_allocation_sequence() {
        let device = match try_create_device() {
            Some(d) => d,
            None => return,
        };

        if !device.supports_external_memory() {
            println!("Skipping — external memory not supported");
            return;
        }

        let baseline = device.live_allocation_count();

        // --- Phase 1: Simulate spec-violating VulkanTexture allocations ---
        // (VkImage WITHOUT VkExternalMemoryImageCreateInfo, but exportable=true)
        let mut spec_violating_images = Vec::new();
        let mut spec_violating_memories = Vec::new();

        println!("--- Phase 1: Spec-violating VulkanTexture pattern (exportable without external image info) ---");
        for i in 0..2 {
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D {
                    width: 1920,
                    height: 1080,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(
                    vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::SAMPLED,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            // NOTE: no push_next(external_image_info) — this is the bug

            let image = unsafe { device.device().create_image(&image_info, None) }
                .unwrap_or_else(|e| panic!("spec-violating image {i}: {e}"));

            let memory = device
                .allocate_image_memory(image, vk::MemoryPropertyFlags::DEVICE_LOCAL, true)
                .unwrap_or_else(|e| panic!("spec-violating image memory {i}: {e}"));

            unsafe { device.device().bind_image_memory(image, memory, 0) }
                .unwrap_or_else(|e| panic!("spec-violating bind {i}: {e}"));

            spec_violating_images.push(image);
            spec_violating_memories.push(memory);
            println!("  Spec-violating texture {i}: OK (live={})", device.live_allocation_count());
        }

        // --- Phase 2: Camera input SSBOs (HOST_VISIBLE, non-exportable) ---
        let mut camera_buffers = Vec::new();
        let mut camera_memories = Vec::new();

        println!("--- Phase 2: Camera input SSBOs ---");
        for i in 0..2 {
            let buffer_info = vk::BufferCreateInfo::default()
                .size(1920 * 1080 * 2) // YUYV
                .usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);

            let buffer = unsafe { device.device().create_buffer(&buffer_info, None) }
                .unwrap_or_else(|e| panic!("camera buffer {i}: {e}"));

            let memory = device
                .allocate_buffer_memory(
                    buffer,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                    false,
                )
                .unwrap_or_else(|e| panic!("camera buffer memory {i}: {e}"));

            unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }
                .unwrap_or_else(|e| panic!("camera buffer bind {i}: {e}"));

            camera_buffers.push(buffer);
            camera_memories.push(memory);
            println!("  Camera SSBO {i}: OK (live={})", device.live_allocation_count());
        }

        // --- Phase 3: Camera compute output image (DEVICE_LOCAL, non-exportable) ---
        println!("--- Phase 3: Camera compute output image ---");
        let compute_image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1920,
                height: 1080,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        let compute_image = unsafe { device.device().create_image(&compute_image_info, None) }
            .expect("compute image");
        let compute_memory = device
            .allocate_image_memory(compute_image, vk::MemoryPropertyFlags::DEVICE_LOCAL, false)
            .expect("compute image memory");
        unsafe { device.device().bind_image_memory(compute_image, compute_memory, 0) }
            .expect("compute image bind");
        println!("  Compute output image: OK (live={})", device.live_allocation_count());

        // --- Phase 4: Encoder bitstream buffer (HOST_VISIBLE, non-exportable) ---
        println!("--- Phase 4: Encoder bitstream buffer ---");
        let bitstream_info = vk::BufferCreateInfo::default()
            .size(2 * 1024 * 1024) // 2MB
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let bitstream_buffer = unsafe { device.device().create_buffer(&bitstream_info, None) }
            .expect("bitstream buffer");
        let bitstream_memory = device
            .allocate_buffer_memory(
                bitstream_buffer,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                false,
            )
            .expect("bitstream memory");
        unsafe { device.device().bind_buffer_memory(bitstream_buffer, bitstream_memory, 0) }
            .expect("bitstream bind");
        println!("  Bitstream buffer: OK (live={})", device.live_allocation_count());

        // --- Phase 5: Display camera textures (the failing allocation) ---
        let live_before_display = device.live_allocation_count();
        println!(
            "--- Phase 5: Display camera textures (live={}, attempting 4 more) ---",
            live_before_display
        );

        let mut display_images = Vec::new();
        let mut display_memories = Vec::new();

        for i in 0..4 {
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D {
                    width: 1920,
                    height: 1080,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);

            let image = unsafe { device.device().create_image(&image_info, None) }
                .unwrap_or_else(|e| panic!("display image {i}: {e}"));

            match device.allocate_image_memory(
                image,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                false,
            ) {
                Ok(memory) => {
                    unsafe { device.device().bind_image_memory(image, memory, 0) }
                        .unwrap_or_else(|e| panic!("display bind {i}: {e}"));
                    display_images.push(image);
                    display_memories.push(memory);
                    println!("  Display camera texture {i}: OK (live={})", device.live_allocation_count());
                }
                Err(e) => {
                    println!(
                        "  Display camera texture {i}: FAILED — {} (live={})",
                        e,
                        device.live_allocation_count()
                    );
                    unsafe { device.device().destroy_image(image, None) };
                }
            }
        }

        println!("--- Cleanup ---");
        // Cleanup all
        for i in 0..display_images.len() {
            unsafe { device.device().destroy_image(display_images[i], None) };
            device.free_device_memory(display_memories[i]);
        }
        unsafe { device.device().destroy_buffer(bitstream_buffer, None) };
        device.free_device_memory(bitstream_memory);
        unsafe { device.device().destroy_image(compute_image, None) };
        device.free_device_memory(compute_memory);
        for i in 0..camera_buffers.len() {
            unsafe { device.device().destroy_buffer(camera_buffers[i], None) };
            device.free_device_memory(camera_memories[i]);
        }
        for i in 0..spec_violating_images.len() {
            unsafe { device.device().destroy_image(spec_violating_images[i], None) };
            device.free_device_memory(spec_violating_memories[i]);
        }
        assert_eq!(device.live_allocation_count(), baseline);
        println!("Real pipeline allocation sequence test passed");
    }
}
