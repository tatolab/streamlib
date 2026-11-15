// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Importing host memory as a buffer through VK_EXT_external_memory_host.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Error, Result};

use crate::vulkan::rhi::HostVulkanDevice;

use super::{HostVulkanBuffer, HostVulkanBufferMemoryOrigin};

impl HostVulkanBuffer {
    /// Import a caller-owned host range as a `STORAGE_BUFFER` through
    /// `VK_EXT_external_memory_host`, so a compute pass reads or writes the
    /// range in place.
    ///
    /// `host_ptr` and `byte_len` must both be multiples of
    /// [`HostVulkanDevice::min_imported_host_pointer_alignment`], and the
    /// range must outlive the returned buffer — the driver pins it, it
    /// never copies it. Pass the range's owner as `host_range_owner` and the
    /// buffer keeps it alive itself, clones included; pass `None` only when
    /// the caller guarantees the range outlives the buffer. Refused by name
    /// when the extension is absent or the driver declines this range (a
    /// mapping of another driver's device memory is the case the caller has
    /// to expect).
    pub fn from_imported_host_pointer_as_storage_buffer(
        vulkan_device: &Arc<HostVulkanDevice>,
        host_ptr: *mut u8,
        byte_len: u64,
        host_range_owner: Option<Box<dyn Send + Sync>>,
    ) -> Result<Self> {
        Self::from_imported_host_range_as_buffer_of_size(
            vulkan_device,
            host_ptr,
            byte_len,
            byte_len,
            host_range_owner,
            "HostVulkanBuffer::from_imported_host_pointer_as_storage_buffer",
        )
    }

    /// As [`Self::from_imported_host_pointer_as_storage_buffer`], with the
    /// buffer spanning only the first `buffer_byte_len` bytes of the
    /// `byte_len` imported — for a range the import alignment rounds up past
    /// what the buffer's user addresses. Refusals name `constructor_label`,
    /// the public constructor the caller came through.
    pub(in crate::vulkan::rhi) fn from_imported_host_range_as_buffer_of_size(
        vulkan_device: &Arc<HostVulkanDevice>,
        host_ptr: *mut u8,
        byte_len: u64,
        buffer_byte_len: u64,
        host_range_owner: Option<Box<dyn Send + Sync>>,
        constructor_label: &'static str,
    ) -> Result<Self> {
        use vulkanalia::vk::ExtExternalMemoryHostExtensionDeviceCommands as _;

        if buffer_byte_len == 0 || buffer_byte_len > byte_len {
            return Err(Error::Configuration(format!(
                "{constructor_label}: a {buffer_byte_len}-byte buffer does not fit the {byte_len} bytes \
                 imported"
            )));
        }
        if !vulkan_device.supports_host_pointer_import() {
            return Err(Error::NotSupported(format!(
                "{constructor_label}: VK_EXT_external_memory_host is not enabled on this device"
            )));
        }
        if byte_len == 0 {
            return Err(Error::Configuration(format!(
                "{constructor_label}: byte_len must be > 0"
            )));
        }
        let alignment = vulkan_device.min_imported_host_pointer_alignment();
        if alignment == 0
            || !(host_ptr as u64).is_multiple_of(alignment)
            || !byte_len.is_multiple_of(alignment)
        {
            return Err(Error::Configuration(format!(
                "{constructor_label}: host range {host_ptr:p}+{byte_len} is not aligned to the \
                 driver's {alignment}-byte import alignment"
            )));
        }

        let device = vulkan_device.device();
        let handle_type = vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;

        let mut host_pointer_properties = vk::MemoryHostPointerPropertiesEXT::builder().build();
        unsafe {
            device.get_memory_host_pointer_properties_ext(
                handle_type,
                host_ptr.cast_const().cast(),
                &mut host_pointer_properties,
            )
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "{constructor_label}: the driver declined to import the host range {host_ptr:p}+{byte_len}: {e}"
            ))
        })?;

        let mut external_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(handle_type)
            .build();
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(buffer_byte_len)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info)
            .build();
        let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|e| {
            Error::GpuError(format!("{constructor_label}: vkCreateBuffer failed: {e}"))
        })?;

        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        if requirements.size > byte_len {
            unsafe { device.destroy_buffer(buffer, None) };
            return Err(Error::GpuError(format!(
                "{constructor_label}: the buffer needs {} bytes of memory but the host range is {byte_len}",
                requirements.size
            )));
        }
        let memory_type_bits =
            requirements.memory_type_bits & host_pointer_properties.memory_type_bits;
        let memory = vulkan_device
            .import_host_pointer_memory(host_ptr, byte_len, memory_type_bits)
            .inspect_err(|_| unsafe { device.destroy_buffer(buffer, None) })?;
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|e| {
            vulkan_device.free_imported_memory(memory);
            unsafe { device.destroy_buffer(buffer, None) };
            Error::GpuError(format!(
                "{constructor_label}: vkBindBufferMemory failed: {e}"
            ))
        })?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: None,
            imported_memory: Some(memory),
            memory_origin: HostVulkanBufferMemoryOrigin::ImportedFromHostPointer,
            imported_host_range_owner: host_range_owner,
            mapped_ptr: host_ptr,
            #[cfg(target_os = "linux")]
            extra_imported_planes: Vec::new(),
            #[cfg(target_os = "macos")]
            backing_iosurface: None,
            size: buffer_byte_len,
        })
    }
}
