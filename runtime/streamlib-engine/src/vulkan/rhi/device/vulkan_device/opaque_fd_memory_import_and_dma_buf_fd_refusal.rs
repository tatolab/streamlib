// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vulkanalia::vk;

use crate::core::{Error, Result};

use super::HostVulkanDevice;

/// `DMA_BUF_MAGIC` from the kernel's `include/uapi/linux/magic.h`: the
/// `f_type` `fstatfs` reports for a DMA-BUF, which lives on the dmabuf
/// pseudo-filesystem from Linux 5.3 on.
const DMA_BUF_FILE_SYSTEM_MAGIC: u64 = 0x444d_4142;

/// Hand `candidate_fd` back when it names a DMA-BUF; close it and refuse
/// the import otherwise.
///
/// Whether a driver took the fd after a failed `vkAllocateMemory` depends
/// on why it refused: NVIDIA 595.84 closes a DMA-BUF it cannot import but
/// leaves a memfd open behind `ERROR_OUT_OF_DEVICE_MEMORY`. Only a refusal
/// before the call leaves an owner who knows the fd is still open.
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
}
