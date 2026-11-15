// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Error, Result};

use super::{HostVulkanDevice, memory_type_index_is_host_cached};

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
pub(super) fn refuse_a_file_descriptor_that_is_not_a_dma_buf(
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
}
