// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Index buffer for graphics pipeline indexed draws.
//!
//! Layout-stable `(handle, vtable, cached POD)` shape; see
//! [`StorageBuffer`](super::StorageBuffer) for the shared rationale.

#[cfg(target_os = "linux")]
use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

/// Index buffer for graphics pipeline indexed draws.
///
/// Linux-only. Graphics kernels bind it via `set_index_buffer`,
/// which accepts `&impl VulkanIndexBindable`. The caller separately
/// specifies the index element type (u16 / u32) at the binding
/// callsite via `IndexType`.
#[cfg(target_os = "linux")]
#[repr(C)]
pub struct IndexBuffer {
    /// Opaque handle to the host's `Arc<HostVulkanBuffer>`.
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI Clone/Drop dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
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
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self {
            handle,
            vtable,
            byte_size_cached: byte_size,
            mapped_ptr_cached: mapped_ptr,
        }
    }

    /// Engine-internal borrow of the host-owned `HostVulkanBuffer`.
    /// **Panics if called from cdylib code.**
    pub(crate) fn host_inner(&self) -> &crate::vulkan::rhi::HostVulkanBuffer {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "IndexBuffer::host_inner() reached from cdylib code; this method \
                 must dispatch through the GpuContextLimitedAccessVTable."
            );
        }
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
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle were paired at construction.
            unsafe {
                ((*self.vtable).clone_index_buffer)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            byte_size_cached: self.byte_size_cached,
            mapped_ptr_cached: self.mapped_ptr_cached,
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for IndexBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with `Arc::into_raw` in `from_arc_into_raw`.
            unsafe {
                ((*self.vtable).drop_index_buffer)(self.handle);
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

#[cfg(all(test, target_pointer_width = "64", target_os = "linux"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn index_buffer_layout() {
        // 32 bytes, same shape as StorageBuffer.
        assert_eq!(size_of::<IndexBuffer>(), 32);
        assert_eq!(align_of::<IndexBuffer>(), 8);
        assert_eq!(offset_of!(IndexBuffer, handle), 0);
        assert_eq!(offset_of!(IndexBuffer, vtable), 8);
        assert_eq!(offset_of!(IndexBuffer, byte_size_cached), 16);
        assert_eq!(offset_of!(IndexBuffer, mapped_ptr_cached), 24);
    }

    #[test]
    fn index_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<IndexBuffer>();
    }
}
