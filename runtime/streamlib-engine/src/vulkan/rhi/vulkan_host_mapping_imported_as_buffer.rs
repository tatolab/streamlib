// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A caller-owned host range the GPU writes — one primitive, two tiers.
//!
//! The imported tier binds the range itself as a `VkBuffer` through
//! `VK_EXT_external_memory_host`, so the kernel's writes land in the
//! caller's pages. The staged tier, taken when the extension is absent or
//! the driver declines the range, gives the kernel host-cached staging of
//! the same length and lands it with one `memcpy`. Which tier a driver
//! takes is a fact about that driver, reported once through
//! [`HostMappingWrittenByGpu::tier`] and its reason.

use std::sync::Arc;

use crate::core::rhi::StorageBuffer;
use crate::core::{Error, Result};

use super::{HostVulkanBuffer, HostVulkanDevice, RhiCommandRecorder, VulkanAccess, VulkanStage};

/// Which of the two tiers a [`HostMappingWrittenByGpu`] took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMappingTier {
    /// The caller's range is the buffer; the kernel writes it in place.
    ImportedHostPointer,
    /// The kernel writes host-cached staging; `publish_to_host` copies once.
    HostCachedStagingCopy,
}

impl HostMappingTier {
    /// The tier's name as a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImportedHostPointer => "imported_host_pointer",
            Self::HostCachedStagingCopy => "host_cached_staging_copy",
        }
    }
}

/// A host range a compute pass writes and the host then reads.
///
/// Bind [`Self::storage_buffer`] as the kernel's output, record
/// [`Self::record_release_to_host`] after the dispatch, wait for the
/// submission, then [`Self::publish_to_host`]. The range must outlive this
/// value: on the imported tier the driver pins it, and on the staged tier
/// the publish writes into it.
pub struct HostMappingWrittenByGpu {
    storage_buffer: StorageBuffer,
    host_range_ptr: *mut u8,
    host_range_byte_len: usize,
    tier: HostMappingTier,
    fallback_reason: Option<String>,
    staging_is_host_cached: bool,
}

// SAFETY: the raw pointer is a caller-owned mapping the caller keeps alive
// for this value's lifetime; nothing here aliases it across threads beyond
// the one `memcpy` in `publish_to_host`, which the caller serialises.
unsafe impl Send for HostMappingWrittenByGpu {}
unsafe impl Sync for HostMappingWrittenByGpu {}

impl HostMappingWrittenByGpu {
    /// Import `host_range_ptr..+host_range_byte_len` for GPU writes, taking
    /// the imported tier when the device and driver allow it and the staged
    /// tier otherwise, with the reason kept for the caller's log line.
    ///
    /// `host_range_byte_len` must be > 0; the imported tier additionally
    /// needs the range aligned to the driver's import alignment, which a
    /// page-aligned `mmap` of a whole buffer satisfies.
    pub fn import_for_gpu_writes(
        vulkan_device: &Arc<HostVulkanDevice>,
        host_range_ptr: *mut u8,
        host_range_byte_len: usize,
    ) -> Result<Self> {
        if host_range_ptr.is_null() || host_range_byte_len == 0 {
            return Err(Error::Configuration(
                "HostMappingWrittenByGpu::import_for_gpu_writes: the host range must be \
                 non-null and non-empty"
                    .into(),
            ));
        }
        let byte_len = host_range_byte_len as u64;

        let import_refusal = if vulkan_device.supports_host_pointer_import() {
            match HostVulkanBuffer::from_imported_host_pointer_as_storage_buffer(
                vulkan_device,
                host_range_ptr,
                byte_len,
            ) {
                Ok(imported) => {
                    return Ok(Self {
                        storage_buffer: StorageBuffer::from_host_vulkan_buffer(Arc::new(imported)),
                        host_range_ptr,
                        host_range_byte_len,
                        tier: HostMappingTier::ImportedHostPointer,
                        fallback_reason: None,
                        staging_is_host_cached: true,
                    });
                }
                Err(refusal) => refusal.to_string(),
            }
        } else {
            "VK_EXT_external_memory_host is not enabled on this device".to_string()
        };

        let staging =
            HostVulkanBuffer::new_storage_buffer_host_cached_for_cpu_reads(vulkan_device, byte_len)?;
        let staging_is_host_cached = staging.vma_allocation_is_host_cached().unwrap_or(false);
        Ok(Self {
            storage_buffer: StorageBuffer::from_host_vulkan_buffer(Arc::new(staging)),
            host_range_ptr,
            host_range_byte_len,
            tier: HostMappingTier::HostCachedStagingCopy,
            fallback_reason: Some(import_refusal),
            staging_is_host_cached,
        })
    }

    /// The buffer a kernel binds as its storage-buffer output.
    pub fn storage_buffer(&self) -> &StorageBuffer {
        &self.storage_buffer
    }

    /// Which tier this mapping took.
    pub fn tier(&self) -> HostMappingTier {
        self.tier
    }

    /// Why the imported tier was not taken; `None` on the imported tier.
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    /// Whether the memory the kernel writes sits on a HOST_CACHED type —
    /// always on the imported tier (it is ordinary host RAM), and on the
    /// staged tier wherever the device offered one. False means the one
    /// copy reads write-combined memory, the slow path.
    pub fn gpu_written_memory_is_host_cached(&self) -> bool {
        self.staging_is_host_cached
    }

    /// Length of the host range in bytes.
    pub fn host_range_byte_len(&self) -> usize {
        self.host_range_byte_len
    }

    /// Record the barrier that makes the kernel's writes available to the
    /// host stage, after the dispatch and before the submit.
    pub fn record_release_to_host(&self, recorder: &mut RhiCommandRecorder) -> Result<()> {
        recorder.record_buffer_barrier(
            &self.storage_buffer,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::HOST,
            VulkanAccess::SHADER_WRITE,
            VulkanAccess::HOST_READ,
        )
    }

    /// Land the kernel's output in the host range. Call once the
    /// submission that wrote the buffer has been waited for. On the
    /// imported tier the writes are already there; on the staged tier this
    /// is the one copy.
    pub fn publish_to_host(&self) {
        if self.tier == HostMappingTier::ImportedHostPointer {
            return;
        }
        let staging_ptr = self.storage_buffer.mapped_ptr();
        // SAFETY: both ranges are `host_range_byte_len` long — the staging
        // buffer was allocated at exactly that size and the host range is
        // the caller's, kept alive for this value's lifetime — and they
        // never overlap: one is a VMA allocation, the other a foreign
        // mapping.
        unsafe {
            std::ptr::copy_nonoverlapping(
                staging_ptr.cast_const(),
                self.host_range_ptr,
                self.host_range_byte_len,
            );
        }
    }
}

impl std::fmt::Debug for HostMappingWrittenByGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostMappingWrittenByGpu")
            .field("tier", &self.tier)
            .field("host_range_byte_len", &self.host_range_byte_len)
            .field("fallback_reason", &self.fallback_reason)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page-aligned anonymous mapping of `byte_len` bytes, released on drop.
    struct PageAlignedHostRange {
        ptr: *mut u8,
        byte_len: usize,
    }

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

        fn as_slice(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.ptr, self.byte_len) }
        }
    }

    impl Drop for PageAlignedHostRange {
        fn drop(&mut self) {
            unsafe { libc::munmap(self.ptr.cast(), self.byte_len) };
        }
    }

    fn device_or_skip() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(device) => Some(device),
            Err(e) => {
                tracing::warn!("skipping — no Vulkan device: {e}");
                None
            }
        }
    }

    /// Write the first and last byte of every page of `mapping`'s buffer to
    /// `value` on the GPU, release to host, and wait.
    fn fill_on_gpu(device: &Arc<HostVulkanDevice>, mapping: &HostMappingWrittenByGpu, value: u8) {
        let word = u32::from_le_bytes([value; 4]);
        let mut recorder = RhiCommandRecorder::new(device, "host_mapping_fill").expect("recorder");
        recorder.begin().expect("begin");
        recorder
            .record_fill_buffer(mapping.storage_buffer(), word)
            .expect("fill");
        mapping
            .record_release_to_host(&mut recorder)
            .expect("release barrier");
        recorder.submit_and_wait().expect("submit");
        mapping.publish_to_host();
    }

    /// On a device advertising the extension, an ordinary page-aligned host
    /// range imports, and a GPU fill lands in the caller's pages with no
    /// copy: the tier says so and the bytes prove it.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — needs a Vulkan device; see docs/testing-hardware.md"
    )]
    #[test]
    fn a_host_mapping_takes_the_imported_tier_when_the_device_allows_it() {
        let Some(device) = device_or_skip() else {
            return;
        };
        if !device.supports_host_pointer_import() {
            tracing::warn!("skipping — VK_EXT_external_memory_host absent on this device");
            return;
        }
        let range = PageAlignedHostRange::new(4096 * 4);
        let mapping = HostMappingWrittenByGpu::import_for_gpu_writes(&device, range.ptr, range.byte_len)
            .expect("import");
        assert_eq!(mapping.tier(), HostMappingTier::ImportedHostPointer);
        assert!(mapping.fallback_reason().is_none());

        fill_on_gpu(&device, &mapping, 0x5A);
        assert!(
            range.as_slice().iter().all(|&b| b == 0x5A),
            "the kernel's writes must land in the caller's pages"
        );
        drop(mapping);
    }

    /// A range the driver cannot import — here one that breaks the
    /// alignment rule — falls back to host-cached staging, says why, and
    /// still lands the bytes through the one copy.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — needs a Vulkan device; see docs/testing-hardware.md"
    )]
    #[test]
    fn a_refused_import_falls_back_to_host_cached_staging_and_says_why() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let range = PageAlignedHostRange::new(4096 * 2);
        // Offset by one word: the pointer is no longer import-aligned, so
        // the imported tier is refused on every driver that has it and the
        // staged tier is what remains.
        let misaligned_ptr = unsafe { range.ptr.add(4) };
        let misaligned_len = range.byte_len - 4;
        let mapping =
            HostMappingWrittenByGpu::import_for_gpu_writes(&device, misaligned_ptr, misaligned_len)
                .expect("the staged tier never refuses");
        assert_eq!(mapping.tier(), HostMappingTier::HostCachedStagingCopy);
        let reason = mapping.fallback_reason().expect("a fallback carries its reason");
        assert!(
            reason.contains("align") || reason.contains("not enabled"),
            "reason names the refusal: {reason}"
        );

        fill_on_gpu(&device, &mapping, 0xA5);
        let written = &range.as_slice()[4..];
        assert!(
            written.iter().all(|&b| b == 0xA5),
            "publish_to_host lands the staging copy in the range"
        );
        assert_eq!(range.as_slice()[0], 0, "bytes before the range are untouched");
    }
}
