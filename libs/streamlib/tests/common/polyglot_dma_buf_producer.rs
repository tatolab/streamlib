// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared test helper for the polyglot DMA-BUF integration tests.
//!
//! Lives under `tests/common/` so cargo's integration-test harness does
//! *not* compile it as its own test binary. Each integration test pulls
//! it in with `#[path = "common/polyglot_dma_buf_producer.rs"]` + a `mod`
//! declaration.
//!
//! Produces a real Vulkan-exported DMA-BUF fd, filled with a caller-supplied
//! byte pattern. Replaces the `memfd` stand-in used before #420 shipped the
//! Vulkan import path in the polyglot consumers — NVIDIA's proprietary
//! driver rejects memfd fds as `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT`,
//! so the integration test now has to feed a genuine DMA-BUF through the
//! surface-share service to exercise the subprocess's import path.
//!
//! Allowed to call `vkGetMemoryFdKHR` (the export path) because this is
//! **host-side** test code — the subprocess-under-test never sees this
//! module and remains strictly import-only (allocation escalates to the
//! host via #325).

#![cfg(target_os = "linux")]
#![allow(dead_code)] // Each test binary uses only a subset of the helper.

use std::ffi::{c_char, CStr};
use std::os::unix::io::RawFd;

use vulkanalia::loader::{LibloadingLoader, LIBRARY};
use vulkanalia::prelude::v1_1::*;
use vulkanalia::vk::{self, KhrExternalMemoryFdExtensionDeviceCommands as _};

pub struct TestDmaBufProducer {
    _entry: vulkanalia::Entry,
    instance: vulkanalia::Instance,
    device: vulkanalia::Device,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
}

impl TestDmaBufProducer {
    pub fn try_new() -> Result<Self, String> {
        let loader = unsafe { LibloadingLoader::new(LIBRARY) }
            .map_err(|e| format!("load libvulkan: {e}"))?;
        let entry = unsafe { vulkanalia::Entry::new(loader) }
            .map_err(|e| format!("vulkan entry: {e}"))?;

        let app_info = vk::ApplicationInfo::builder()
            .application_name(b"streamlib-polyglot-integration-test\0")
            .application_version(vk::make_version(0, 1, 0))
            .engine_name(b"streamlib\0")
            .engine_version(vk::make_version(0, 1, 0))
            .api_version(vk::make_version(1, 1, 0))
            .build();
        let instance_info = vk::InstanceCreateInfo::builder()
            .application_info(&app_info)
            .build();
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|e| format!("create_instance: {e}"))?;

        match Self::select_and_create(&instance) {
            Ok((device, physical_device)) => {
                let memory_properties =
                    unsafe { instance.get_physical_device_memory_properties(physical_device) };
                Ok(Self {
                    _entry: entry,
                    instance,
                    device,
                    memory_properties,
                })
            }
            Err(e) => {
                unsafe { instance.destroy_instance(None) };
                Err(e)
            }
        }
    }

    fn select_and_create(
        instance: &vulkanalia::Instance,
    ) -> Result<(vulkanalia::Device, vk::PhysicalDevice), String> {
        let physical_devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| format!("enumerate_physical_devices: {e}"))?;
        if physical_devices.is_empty() {
            return Err("no Vulkan-capable physical devices".into());
        }
        let physical_device = physical_devices
            .iter()
            .find(|&&pd| {
                let p = unsafe { instance.get_physical_device_properties(pd) };
                p.device_type == vk::PhysicalDeviceType::DISCRETE_GPU
            })
            .copied()
            .unwrap_or(physical_devices[0]);

        let available_ext =
            unsafe { instance.enumerate_device_extension_properties(physical_device, None) }
                .map_err(|e| format!("enumerate_device_extension_properties: {e}"))?;
        let available_names: Vec<&CStr> = available_ext
            .iter()
            .map(|e| unsafe { CStr::from_ptr(e.extension_name.as_ptr()) })
            .collect();
        let ext_external_memory = c"VK_KHR_external_memory";
        let ext_external_memory_fd = c"VK_KHR_external_memory_fd";
        let ext_dma_buf = c"VK_EXT_external_memory_dma_buf";
        for required in [ext_external_memory, ext_external_memory_fd, ext_dma_buf] {
            if !available_names.contains(&required) {
                return Err(format!(
                    "required device extension missing: {}",
                    required.to_string_lossy()
                ));
            }
        }

        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        if queue_families.is_empty() {
            return Err("physical device has no queue families".into());
        }
        let queue_family_index = 0u32;
        let queue_priorities = [1.0f32];
        let queue_create_infos = [vk::DeviceQueueCreateInfo::builder()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)
            .build()];
        let device_extensions: Vec<*const c_char> = vec![
            ext_external_memory.as_ptr(),
            ext_external_memory_fd.as_ptr(),
            ext_dma_buf.as_ptr(),
        ];
        let device_info = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .build();
        let device = unsafe { instance.create_device(physical_device, &device_info, None) }
            .map_err(|e| format!("create_device: {e}"))?;
        Ok((device, physical_device))
    }

    fn find_memory_type(
        &self,
        type_filter: u32,
        required_flags: vk::MemoryPropertyFlags,
    ) -> Option<u32> {
        for i in 0..self.memory_properties.memory_type_count {
            let type_supported = (type_filter & (1 << i)) != 0;
            let flags = self.memory_properties.memory_types[i as usize].property_flags;
            if type_supported && flags.contains(required_flags) {
                return Some(i);
            }
        }
        None
    }

    /// Allocate a HOST_VISIBLE DMA-BUF-exportable buffer, write `pattern`
    /// into it, and return the exported DMA-BUF fd. Caller owns the fd.
    pub fn produce(&self, pattern: &[u8]) -> Result<RawFd, String> {
        let size = pattern.len() as u64;
        let device_size = size as vk::DeviceSize;

        let mut external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(device_size)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_info)
            .build();
        let buffer = unsafe { self.device.create_buffer(&buffer_info, None) }
            .map_err(|e| format!("create_buffer: {e}"))?;

        let mem_req = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory_type_index = match self.find_memory_type(
            mem_req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) {
            Some(i) => i,
            None => {
                unsafe { self.device.destroy_buffer(buffer, None) };
                return Err("no HOST_VISIBLE|HOST_COHERENT memory type".into());
            }
        };
        let alloc_size = device_size.max(mem_req.size);

        let mut export_info = vk::ExportMemoryAllocateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(alloc_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut export_info)
            .build();
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(m) => m,
            Err(e) => {
                unsafe { self.device.destroy_buffer(buffer, None) };
                return Err(format!("allocate_memory: {e}"));
            }
        };
        if let Err(e) = unsafe { self.device.bind_buffer_memory(buffer, memory, 0) } {
            unsafe {
                self.device.free_memory(memory, None);
                self.device.destroy_buffer(buffer, None);
            }
            return Err(format!("bind_buffer_memory: {e}"));
        }
        let mapped_ptr = match unsafe {
            self.device
                .map_memory(memory, 0, alloc_size, vk::MemoryMapFlags::empty())
        } {
            Ok(p) => p as *mut u8,
            Err(e) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(format!("map_memory: {e}"));
            }
        };
        unsafe {
            std::ptr::copy_nonoverlapping(pattern.as_ptr(), mapped_ptr, pattern.len());
            self.device.unmap_memory(memory);
        }

        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .build();
        let fd_result = unsafe { self.device.get_memory_fd_khr(&get_fd_info) };
        unsafe {
            self.device.destroy_buffer(buffer, None);
            self.device.free_memory(memory, None);
        }
        fd_result.map_err(|e| format!("get_memory_fd_khr: {e}"))
    }
}

impl Drop for TestDmaBufProducer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
