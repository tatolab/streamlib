// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A caller-owned host range the GPU reads.
//!
//! The imported tier binds the range itself as a `VkBuffer` through
//! `VK_EXT_external_memory_host`, so a kernel reads the caller's pages in
//! place. The staged tier, taken when the extension is absent, the driver
//! declines the range, or the caller must not ask, gives the kernel
//! host-visible staging of the same length, filled with one `memcpy` before
//! each read.

use std::sync::Arc;

use crate::core::rhi::StorageBuffer;
use crate::core::{Error, Result};

use super::{HostVulkanBuffer, HostVulkanDevice};

/// Which of the two tiers a [`HostMappingReadByGpu`] took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMappingReadByGpuTier {
    /// The caller's range is the buffer; the kernel reads it in place.
    ImportedHostPointer,
    /// The kernel reads host-visible staging; `stage_from_host` copies once.
    HostVisibleStagingCopy,
}

impl HostMappingReadByGpuTier {
    /// The tier's name as a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImportedHostPointer => "imported_host_pointer",
            Self::HostVisibleStagingCopy => "host_visible_staging_copy",
        }
    }
}

/// A host range the host fills and a compute pass then reads.
///
/// Call [`Self::stage_from_host`] once the range holds the bytes to read,
/// then bind [`Self::storage_buffer`] as the kernel's input. The range must
/// outlive this value: on the imported tier the driver pins it, and on the
/// staged tier every stage reads from it.
pub struct HostMappingReadByGpu {
    storage_buffer: StorageBuffer,
    host_range_ptr: *const u8,
    readable_byte_len: usize,
    tier: HostMappingReadByGpuTier,
    fallback_reason: Option<String>,
}

// SAFETY: the raw pointer is a caller-owned mapping the caller keeps alive
// for this value's lifetime, only ever read through it.
unsafe impl Send for HostMappingReadByGpu {}

impl HostMappingReadByGpu {
    /// Import the first `readable_byte_len` bytes of the mapped range
    /// `host_range_ptr..+mapped_byte_len` for GPU reads, taking the imported
    /// tier when the device and driver allow it and the staged tier
    /// otherwise, with the reason kept for the caller's log line.
    ///
    /// The imported tier needs the pointer and `mapped_byte_len` aligned to
    /// the driver's import alignment, which a page-aligned `mmap` rounded up
    /// to that alignment satisfies.
    ///
    /// # Safety
    ///
    /// All `mapped_byte_len` bytes must stay mapped and readable until the
    /// returned value drops.
    pub unsafe fn import_for_gpu_reads(
        vulkan_device: &Arc<HostVulkanDevice>,
        host_range_ptr: *const u8,
        mapped_byte_len: usize,
        readable_byte_len: usize,
    ) -> Result<Self> {
        if host_range_ptr.is_null() || readable_byte_len == 0 || readable_byte_len > mapped_byte_len
        {
            return Err(Error::Configuration(format!(
                "HostMappingReadByGpu::import_for_gpu_reads: {readable_byte_len} readable bytes \
                 must be non-empty and fit the {mapped_byte_len} bytes mapped at a non-null \
                 pointer"
            )));
        }

        let import_refusal = if vulkan_device.supports_host_pointer_import() {
            match HostVulkanBuffer::from_imported_host_range_as_buffer_of_size(
                vulkan_device,
                host_range_ptr.cast_mut(),
                mapped_byte_len as u64,
                readable_byte_len as u64,
                None,
                "HostMappingReadByGpu::import_for_gpu_reads",
            ) {
                Ok(imported) => {
                    return Ok(Self {
                        storage_buffer: StorageBuffer::from_host_vulkan_buffer(Arc::new(imported)),
                        host_range_ptr,
                        readable_byte_len,
                        tier: HostMappingReadByGpuTier::ImportedHostPointer,
                        fallback_reason: None,
                    });
                }
                Err(refusal) => refusal.to_string(),
            }
        } else {
            "VK_EXT_external_memory_host is not enabled on this device".to_string()
        };

        // SAFETY: the caller upholds this function's contract, which covers
        // the readable bytes the staged tier copies from.
        unsafe {
            Self::stage_for_gpu_reads(
                vulkan_device,
                host_range_ptr,
                readable_byte_len,
                import_refusal,
            )
        }
    }

    /// Give the first `readable_byte_len` bytes at `host_range_ptr` to GPU
    /// reads through host-visible staging, without asking the driver to
    /// import the range, keeping `why_the_range_is_not_imported` for the
    /// caller's log line.
    ///
    /// For a range the driver must not be asked to import: NVIDIA answers a
    /// host-pointer import of a foreign DMA-BUF's mapping with
    /// `ERROR_OUT_OF_DEVICE_MEMORY` and then fails every later allocation on
    /// the device the same way.
    ///
    /// # Safety
    ///
    /// All `readable_byte_len` bytes must stay mapped and readable until the
    /// returned value drops.
    pub unsafe fn stage_for_gpu_reads(
        vulkan_device: &Arc<HostVulkanDevice>,
        host_range_ptr: *const u8,
        readable_byte_len: usize,
        why_the_range_is_not_imported: String,
    ) -> Result<Self> {
        if host_range_ptr.is_null() || readable_byte_len == 0 {
            return Err(Error::Configuration(format!(
                "HostMappingReadByGpu::stage_for_gpu_reads: {readable_byte_len} readable bytes \
                 must be non-empty at a non-null pointer"
            )));
        }
        let staging = HostVulkanBuffer::new_storage_buffer_host_visible(
            vulkan_device,
            readable_byte_len as u64,
        )?;
        Ok(Self {
            storage_buffer: StorageBuffer::from_host_vulkan_buffer(Arc::new(staging)),
            host_range_ptr,
            readable_byte_len,
            tier: HostMappingReadByGpuTier::HostVisibleStagingCopy,
            fallback_reason: Some(why_the_range_is_not_imported),
        })
    }

    /// The buffer a kernel binds as its storage-buffer input.
    pub fn storage_buffer(&self) -> &StorageBuffer {
        &self.storage_buffer
    }

    /// Which tier this mapping took.
    pub fn tier(&self) -> HostMappingReadByGpuTier {
        self.tier
    }

    /// Why the imported tier was not taken; `None` on the imported tier.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    /// Make the range's current bytes the ones the next submitted read
    /// sees. On the imported tier the kernel reads the range itself; on the
    /// staged tier this is the one copy, and the caller must not submit a
    /// read of the previous stage that is still in flight.
    pub fn stage_from_host(&mut self) {
        if self.tier == HostMappingReadByGpuTier::ImportedHostPointer {
            return;
        }
        let staging_ptr = self.storage_buffer.mapped_ptr();
        // SAFETY: both ranges are `readable_byte_len` long — the staging
        // buffer was allocated at exactly that size and the host range is the
        // caller's, kept mapped for this value's lifetime — and they never
        // overlap: one is a VMA allocation, the other a foreign mapping.
        unsafe {
            std::ptr::copy_nonoverlapping(self.host_range_ptr, staging_ptr, self.readable_byte_len);
        }
    }
}

impl std::fmt::Debug for HostMappingReadByGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostMappingReadByGpu")
            .field("tier", &self.tier)
            .field("readable_byte_len", &self.readable_byte_len)
            .field("fallback_reason", &self.fallback_reason)
            .finish()
    }
}

/// A page-aligned anonymous mapping of `byte_len` bytes, released on drop —
/// the stand-in for a caller-owned host range in this module's device tests.
#[cfg(test)]
struct PageAlignedHostRange {
    ptr: *mut u8,
    byte_len: usize,
}

#[cfg(test)]
impl PageAlignedHostRange {
    fn new(byte_len: usize) -> Self {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                byte_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");
        Self {
            ptr: ptr.cast(),
            byte_len,
        }
    }
}

#[cfg(test)]
impl Drop for PageAlignedHostRange {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.byte_len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::{RhiCommandRecorder, VulkanAccess, VulkanStage};

    fn device_or_skip() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(device) => Some(device),
            Err(e) => {
                tracing::warn!("skipping — no Vulkan device: {e}");
                None
            }
        }
    }

    /// Read the mapping's buffer back on the CPU through a GPU copy into a
    /// host-visible buffer.
    fn read_back_through_the_gpu(
        device: &Arc<HostVulkanDevice>,
        mapping: &HostMappingReadByGpu,
    ) -> Vec<u8> {
        let byte_len = mapping.readable_byte_len;
        let destination =
            HostVulkanBuffer::new_storage_buffer_host_visible(device, byte_len as u64)
                .expect("read-back buffer");
        let mut recorder =
            RhiCommandRecorder::new(device, "host_mapping_read_back").expect("recorder");
        recorder.begin().expect("begin");
        recorder
            .record_buffer_barrier(
                mapping.storage_buffer(),
                VulkanStage::HOST,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::HOST_WRITE,
                VulkanAccess::MEMORY_READ,
            )
            .expect("host-write barrier");
        recorder
            .record_copy_buffer_to_buffer(mapping.storage_buffer(), &destination, byte_len as u64)
            .expect("copy");
        recorder
            .record_buffer_barrier(
                &destination,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::HOST,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::HOST_READ,
            )
            .expect("host-read barrier");
        recorder.submit_and_wait().expect("submit");
        unsafe { std::slice::from_raw_parts(destination.mapped_ptr(), byte_len) }.to_vec()
    }

    /// On a device advertising the extension, a page-aligned host range
    /// imports, and the GPU reads the caller's pages with no copy: the tier
    /// says so and a second fill reaches the GPU without a stage.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — needs a Vulkan device"
    )]
    #[test]
    fn a_host_mapping_read_by_the_gpu_takes_the_imported_tier_when_the_device_allows_it() {
        let Some(device) = device_or_skip() else {
            return;
        };
        if !device.supports_host_pointer_import() {
            tracing::warn!("skipping — VK_EXT_external_memory_host absent on this device");
            return;
        }
        let range = PageAlignedHostRange::new(4096 * 4);
        let mapping = unsafe {
            HostMappingReadByGpu::import_for_gpu_reads(
                &device,
                range.ptr,
                range.byte_len,
                range.byte_len - 100,
            )
        }
        .expect("import");
        assert_eq!(
            mapping.tier(),
            HostMappingReadByGpuTier::ImportedHostPointer
        );
        assert!(mapping.fallback_reason().is_none());

        unsafe { std::ptr::write_bytes(range.ptr, 0x3C, range.byte_len) };
        assert!(
            read_back_through_the_gpu(&device, &mapping)
                .iter()
                .all(|&b| b == 0x3C),
            "the GPU reads the caller's pages in place"
        );
    }

    /// A range the driver cannot import — here one that breaks the alignment
    /// rule — falls back to host-visible staging, says why, and reaches the
    /// GPU through the one copy each stage makes.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — needs a Vulkan device"
    )]
    #[test]
    fn a_refused_import_for_reads_falls_back_to_staging_and_says_why() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let range = PageAlignedHostRange::new(4096 * 2);
        let misaligned_ptr = unsafe { range.ptr.add(4) };
        let misaligned_len = range.byte_len - 4;
        let mut mapping = unsafe {
            HostMappingReadByGpu::import_for_gpu_reads(
                &device,
                misaligned_ptr,
                misaligned_len,
                misaligned_len,
            )
        }
        .expect("the staged tier never refuses");
        assert_eq!(
            mapping.tier(),
            HostMappingReadByGpuTier::HostVisibleStagingCopy
        );
        let reason = mapping
            .fallback_reason()
            .expect("a fallback carries its reason");
        assert!(
            reason.contains("align") || reason.contains("not enabled"),
            "reason names the refusal: {reason}"
        );

        unsafe { std::ptr::write_bytes(misaligned_ptr, 0xC3, misaligned_len) };
        mapping.stage_from_host();
        assert!(
            read_back_through_the_gpu(&device, &mapping)
                .iter()
                .all(|&b| b == 0xC3),
            "stage_from_host lands the range in the staging the GPU reads"
        );
    }

    /// The staged constructor never asks the driver to import the range,
    /// keeps the caller's reason, and reaches the GPU through each stage.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — needs a Vulkan device"
    )]
    #[test]
    fn a_range_staged_without_an_import_reaches_the_gpu_through_each_stage() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let range = PageAlignedHostRange::new(4096 * 2);
        let mut mapping = unsafe {
            HostMappingReadByGpu::stage_for_gpu_reads(
                &device,
                range.ptr,
                range.byte_len,
                "the caller never imports this range".to_string(),
            )
        }
        .expect("stage");
        assert_eq!(
            mapping.tier(),
            HostMappingReadByGpuTier::HostVisibleStagingCopy
        );
        assert_eq!(
            mapping.fallback_reason(),
            Some("the caller never imports this range")
        );

        unsafe { std::ptr::write_bytes(range.ptr, 0x5A, range.byte_len) };
        mapping.stage_from_host();
        assert!(
            read_back_through_the_gpu(&device, &mapping)
                .iter()
                .all(|&b| b == 0x5A),
            "stage_from_host lands the range in the staging the GPU reads"
        );
    }

    #[test]
    fn an_empty_or_overlong_readable_range_is_refused() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let range = PageAlignedHostRange::new(4096);
        let overlong =
            unsafe { HostMappingReadByGpu::import_for_gpu_reads(&device, range.ptr, 4096, 4097) };
        assert!(overlong.is_err());
        let empty =
            unsafe { HostMappingReadByGpu::import_for_gpu_reads(&device, range.ptr, 4096, 0) };
        assert!(empty.is_err());
    }
}
