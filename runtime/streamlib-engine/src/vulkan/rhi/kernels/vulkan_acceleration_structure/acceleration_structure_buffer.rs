// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The device-local buffer with a device address that acceleration-structure builds need.

use std::sync::Arc;

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};

use super::HostVulkanDevice;

/// Owning DEVICE_LOCAL buffer with a pre-queried device address. Internal
/// to the AS module — the engine's public buffer types
/// (`HostVulkanBuffer`) target HOST_VISIBLE / OPAQUE_FD-export use cases
/// and don't carry a `BUFFER_DEVICE_ADDRESS` flag, which AS builds need.
pub(super) struct AsBuffer {
    vulkan_device: Arc<HostVulkanDevice>,
    pub(super) buffer: vk::Buffer,
    allocation: Option<vma::Allocation>,
    pub(super) device_address: u64,
}

impl AsBuffer {
    pub(super) fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        label: &str,
    ) -> Result<Self> {
        debug_assert!(
            size > 0,
            "AsBuffer::new called with zero size for '{label}'"
        );
        let allocator = vulkan_device.allocator();
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let alloc_opts = vma::AllocationOptions {
            usage: vma::MemoryUsage::AutoPreferDevice,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let (buffer, allocation) = unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "AS buffer '{label}': vmaCreateBuffer (size={size}) failed: {e}"
                ))
            })?;

        let device_address = if usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS) {
            let info = vk::BufferDeviceAddressInfo::builder()
                .buffer(buffer)
                .build();
            unsafe { vulkan_device.device().get_buffer_device_address(&info) }
        } else {
            0
        };

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            device_address,
        })
    }

    /// HOST_VISIBLE + HOST_COHERENT + MAPPED variant — used for AS build
    /// inputs (vertex / index / instance buffers) so the data can be
    /// memcpy'd directly to GPU-visible memory without a staging copy.
    /// Avoids the cross-submit memory-visibility class of bug that an
    /// upload-then-build pattern with separate `vkQueueSubmit` calls is
    /// vulnerable to. NVIDIA / AMD / Intel all expose enough HOST_VISIBLE
    /// memory types that this works for AS-build inputs in practice.
    pub(super) fn new_host_visible(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        label: &str,
    ) -> Result<Self> {
        debug_assert!(
            size > 0,
            "AsBuffer::new_host_visible called with zero size for '{label}'"
        );
        let allocator = vulkan_device.allocator();
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let alloc_opts = vma::AllocationOptions {
            usage: vma::MemoryUsage::AutoPreferHost,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            ..Default::default()
        };
        let (buffer, allocation) = unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }
            .map_err(|e| {
                Error::GpuError(format!(
                    "AS buffer (host-visible) '{label}': vmaCreateBuffer (size={size}) failed: {e}"
                ))
            })?;

        let device_address = if usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS) {
            let info = vk::BufferDeviceAddressInfo::builder()
                .buffer(buffer)
                .build();
            unsafe { vulkan_device.device().get_buffer_device_address(&info) }
        } else {
            0
        };

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            device_address,
        })
    }

    /// `pMappedData` pointer for a HOST_VISIBLE / MAPPED-flag allocation,
    /// or null if the allocation isn't host-mapped.
    pub(super) fn mapped_ptr(&self) -> *mut u8 {
        if let Some(allocation) = self.allocation {
            let info = self
                .vulkan_device
                .allocator()
                .get_allocation_info(allocation);
            info.pMappedData as *mut u8
        } else {
            std::ptr::null_mut()
        }
    }

    /// Consume the AsBuffer and return its parts without freeing — the
    /// caller takes ownership of the `vk::Buffer` + `vma::Allocation` and
    /// is responsible for `vmaDestroyBuffer` on drop.
    pub(super) fn into_parts(mut self) -> (vk::Buffer, vma::Allocation, u64) {
        let buffer = self.buffer;
        let device_address = self.device_address;
        let allocation = self
            .allocation
            .take()
            .expect("AsBuffer always carries an allocation until consumed");
        (buffer, allocation, device_address)
    }
}

impl Drop for AsBuffer {
    fn drop(&mut self) {
        if let Some(allocation) = self.allocation.take() {
            unsafe {
                self.vulkan_device
                    .allocator()
                    .destroy_buffer(self.buffer, allocation);
            }
        }
    }
}
