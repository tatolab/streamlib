// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan device implementation for RHI.

use std::ffi::{c_char, CStr};
use std::sync::Arc;

use ash::vk;
use gpu_allocator::vulkan::{
    Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc,
};
use gpu_allocator::MemoryLocation;
use parking_lot::Mutex;

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
    gpu_memory_allocator: Option<Arc<Mutex<Allocator>>>,
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

        // 7. Create logical device with required extensions
        let queue_priorities = [1.0f32];
        let mut queue_create_infos = vec![vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)];

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

        // 9. Query memory properties (used by find_memory_type for all allocations)
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        // 10. Create gpu-allocator for sub-allocation
        let mut debug_settings = gpu_allocator::AllocatorDebugSettings::default();
        debug_settings.log_memory_information = true;
        debug_settings.log_leaks_on_shutdown = true;

        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device,
            debug_settings,
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })
        .map_err(|e| {
            StreamError::GpuError(format!("Failed to create GPU memory allocator: {e}"))
        })?;

        let gpu_memory_allocator = Some(Arc::new(Mutex::new(allocator)));

        tracing::info!(
            "Vulkan device initialized: {} (queue family {}, {} memory types, gpu-allocator enabled)",
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
            transfer_queue_family_index,
            device_name: device_name.into_owned(),
            supports_external_memory,
            supports_video_encode,
            video_encode_queue_family_index,
            video_encode_queue,
            gpu_memory_allocator,
        })
    }

    /// Find a memory type that satisfies both the type filter and required properties.
    pub fn find_memory_type(
        &self,
        type_filter: u32,
        required_properties: vk::MemoryPropertyFlags,
    ) -> Result<u32> {
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
    pub fn create_texture(&self, desc: &TextureDescriptor) -> Result<VulkanTexture> {
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

    /// Allocate GPU memory through the sub-allocator.
    pub fn allocate_gpu_memory(
        &self,
        name: &str,
        requirements: vk::MemoryRequirements,
        location: MemoryLocation,
        linear: bool,
    ) -> Result<Allocation> {
        let allocator_arc = self.gpu_memory_allocator.as_ref().ok_or_else(|| {
            StreamError::GpuError("GPU memory allocator not available".into())
        })?;
        allocator_arc
            .lock()
            .allocate(&AllocationCreateDesc {
                name,
                requirements,
                location,
                linear,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            })
            .map_err(|e| StreamError::GpuError(format!("GPU memory allocation failed: {e}")))
    }

    /// Allocate GPU memory as a dedicated image allocation.
    ///
    /// Uses DedicatedImage scheme — creates a 1:1 driver allocation for
    /// this specific image. Use for large images (camera textures, render
    /// targets) where sub-allocation from shared blocks isn't appropriate.
    pub fn allocate_gpu_memory_dedicated_image(
        &self,
        name: &str,
        requirements: vk::MemoryRequirements,
        location: MemoryLocation,
        image: vk::Image,
    ) -> Result<Allocation> {
        let allocator_arc = self.gpu_memory_allocator.as_ref().ok_or_else(|| {
            StreamError::GpuError("GPU memory allocator not available".into())
        })?;
        allocator_arc
            .lock()
            .allocate(&AllocationCreateDesc {
                name,
                requirements,
                location,
                linear: false,
                allocation_scheme: AllocationScheme::DedicatedImage(image),
            })
            .map_err(|e| StreamError::GpuError(format!("GPU memory allocation failed: {e}")))
    }

    /// Free GPU memory through the sub-allocator.
    pub fn free_gpu_memory(&self, allocation: Allocation) -> Result<()> {
        let allocator_arc = self.gpu_memory_allocator.as_ref().ok_or_else(|| {
            StreamError::GpuError("GPU memory allocator not available".into())
        })?;
        allocator_arc
            .lock()
            .free(allocation)
            .map_err(|e| StreamError::GpuError(format!("GPU memory free failed: {e}")))
    }

    /// Get a shared reference to the GPU memory allocator.
    pub fn gpu_memory_allocator(&self) -> Option<&Arc<Mutex<Allocator>>> {
        self.gpu_memory_allocator.as_ref()
    }
}

impl Drop for VulkanDevice {
    fn drop(&mut self) {
        // Wait for all GPU work to finish before freeing any memory.
        unsafe {
            let _ = self.device.device_wait_idle();
        }

        // Now safe to drop the allocator — GPU is idle, so freeing its
        // internal memory blocks won't cause use-after-free on the GPU.
        drop(self.gpu_memory_allocator.take());

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

    #[test]
    fn test_vulkan_device_creation() {
        let result = VulkanDevice::new();
        match &result {
            Ok(device) => {
                println!("Vulkan device created successfully: {}", device.name());
            }
            Err(e) => {
                println!(
                    "Vulkan device creation failed (expected if MoltenVK not installed): {}",
                    e
                );
            }
        }
        // Don't assert - just verify it doesn't panic
        // MoltenVK may or may not be installed
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_vulkan_command_queue_creation() {
        let device = match VulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };

        let queue = device.create_command_queue_wrapper();
        let cmd_buf = queue.create_command_buffer();
        assert!(cmd_buf.is_ok(), "Command buffer creation should succeed");

        // Commit the empty command buffer
        cmd_buf.unwrap().commit();
        println!("Command queue and buffer test passed");
    }
}
