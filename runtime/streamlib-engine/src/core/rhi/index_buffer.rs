// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Index buffer for graphics pipeline indexed draws.
//!
//! `(handle, cached POD)` shape; see
//! [`StorageBuffer`](super::StorageBuffer) for the shared rationale.

#[cfg(target_os = "linux")]
use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

/// Index buffer for graphics pipeline indexed draws.
///
/// Linux-only. Graphics kernels bind it via `set_index_buffer`,
/// which accepts `&impl VulkanIndexBindable`. The caller separately
/// specifies the index element type (u16 / u32) at the binding
/// callsite via `IndexType`.
#[cfg(target_os = "linux")]
pub struct IndexBuffer {
    /// Opaque handle to the host's `Arc<HostVulkanBuffer>`.
    pub(crate) handle: *const c_void,
    /// Cached byte size.
    pub(crate) byte_size_cached: u64,
    /// Cached persistently-mapped CPU pointer.
    pub(crate) mapped_ptr_cached: *mut u8,
}

#[cfg(target_os = "linux")]
unsafe impl Send for IndexBuffer {}
#[cfg(target_os = "linux")]
unsafe impl Sync for IndexBuffer {}

#[cfg(target_os = "linux")]
impl IndexBuffer {
    /// Allocate a HOST_VISIBLE index buffer of the given byte size.
    /// Underlying `VkBuffer` carries `INDEX_BUFFER | TRANSFER_SRC |
    /// TRANSFER_DST` usage.
    pub fn new_host_visible(
        device: &Arc<crate::vulkan::rhi::HostVulkanDevice>,
        byte_size: u64,
    ) -> crate::core::Result<Self> {
        let inner =
            crate::vulkan::rhi::HostVulkanBuffer::new_index_buffer_host_visible(device, byte_size)?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    /// Wrap a pre-allocated buffer that already has `INDEX_BUFFER` usage.
    pub fn from_host_vulkan_buffer(inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>) -> Self {
        Self::from_arc_into_raw(inner)
    }

    pub(crate) fn from_arc_into_raw(inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>) -> Self {
        let byte_size = inner.size() as u64;
        let mapped_ptr = inner.mapped_ptr();
        let handle = Arc::into_raw(inner) as *const c_void;
        Self {
            handle,
            byte_size_cached: byte_size,
            mapped_ptr_cached: mapped_ptr,
        }
    }

    /// Engine-internal borrow of the host-owned `HostVulkanBuffer`.
    pub(crate) fn host_inner(&self) -> &crate::vulkan::rhi::HostVulkanBuffer {
        // SAFETY: see StorageBuffer::host_inner.
        unsafe { &*(self.handle as *const crate::vulkan::rhi::HostVulkanBuffer) }
    }

    /// Total buffer size in bytes.
    pub fn byte_size(&self) -> u64 {
        self.byte_size_cached
    }

    /// Persistently mapped CPU pointer for HOST_VISIBLE allocations.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr_cached
    }
}

#[cfg(target_os = "linux")]
impl Clone for IndexBuffer {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`
            // (see `from_arc_into_raw`); balanced by the Drop impl below.
            unsafe {
                Arc::increment_strong_count(
                    self.handle as *const crate::vulkan::rhi::HostVulkanBuffer,
                );
            }
        }
        Self {
            handle: self.handle,
            byte_size_cached: self.byte_size_cached,
            mapped_ptr_cached: self.mapped_ptr_cached,
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for IndexBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with `Arc::into_raw` in `from_arc_into_raw`
            // and any `Clone` increment.
            unsafe {
                Arc::decrement_strong_count(
                    self.handle as *const crate::vulkan::rhi::HostVulkanBuffer,
                );
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for IndexBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBuffer")
            .field("byte_size", &self.byte_size_cached)
            .finish()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn index_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<IndexBuffer>();
    }
}
