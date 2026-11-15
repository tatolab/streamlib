// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Error, Result};

#[cfg(target_os = "linux")]
use super::opaque_fd_memory_import_and_dma_buf_fd_refusal::refuse_a_file_descriptor_that_is_not_a_dma_buf;
use super::{HostVulkanDevice, memory_type_index_is_host_cached};

impl HostVulkanDevice {
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
    pub(super) fn allocate_device_memory_importing_the_fd(
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

    /// Free device memory allocated via raw vkAllocateMemory (import path only).
    ///
    /// VMA-managed allocations are freed via [`vma::Allocator::destroy_image`](vulkanalia_vma::Allocator::destroy_image) or
    /// [`vma::Allocator::destroy_buffer`](vulkanalia_vma::Allocator::destroy_buffer) — do not call this for VMA allocations.
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

    /// Find a memory type that satisfies both the type filter and required properties.
    ///
    /// Used internally for DMA-BUF import (raw `vkAllocateMemory` path).
    /// VMA handles memory type selection for all non-import allocations.
    pub(super) fn find_memory_type(
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
}
