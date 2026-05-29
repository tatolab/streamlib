// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vertex buffer for graphics pipeline vertex input.
//!
//! Layout-stable `(handle, vtable, cached POD)` shape; see
//! [`StorageBuffer`](super::StorageBuffer) for the shared rationale.

#[cfg(target_os = "linux")]
use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

/// Vertex buffer for graphics pipeline vertex input.
///
/// Linux-only. Graphics kernels bind it via `set_vertex_buffer`,
/// which accepts `&impl VulkanVertexBindable`.
#[cfg(target_os = "linux")]
#[repr(C)]
pub struct VertexBuffer {
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
unsafe impl Send for VertexBuffer {}
#[cfg(target_os = "linux")]
unsafe impl Sync for VertexBuffer {}

#[cfg(target_os = "linux")]
impl VertexBuffer {
    /// Allocate a HOST_VISIBLE vertex buffer of the given byte size.
    /// Underlying `VkBuffer` carries `VERTEX_BUFFER | TRANSFER_SRC |
    /// TRANSFER_DST` usage.
    pub fn new_host_visible(
        device: &Arc<crate::vulkan::rhi::HostVulkanDevice>,
        byte_size: u64,
    ) -> crate::core::Result<Self> {
        let inner =
            crate::vulkan::rhi::HostVulkanBuffer::new_vertex_buffer_host_visible(
                device, byte_size,
            )?;
        Ok(Self::from_arc_into_raw(Arc::new(inner)))
    }

    /// Wrap a pre-allocated buffer that already has `VERTEX_BUFFER`
    /// usage. Callers are responsible for confirming the usage flag at
    /// allocation time; mismatched usage fails at descriptor write.
    pub fn from_host_vulkan_buffer(
        inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
    ) -> Self {
        Self::from_arc_into_raw(inner)
    }

    pub(crate) fn from_arc_into_raw(
        inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
    ) -> Self {
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
                "VertexBuffer::host_inner() reached from cdylib code; this method \
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
impl Clone for VertexBuffer {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle were paired at construction.
            unsafe {
                ((*self.vtable).clone_vertex_buffer)(self.handle);
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
impl Drop for VertexBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with `Arc::into_raw` in `from_arc_into_raw`.
            unsafe {
                ((*self.vtable).drop_vertex_buffer)(self.handle);
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for VertexBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexBuffer")
            .field("byte_size", &self.byte_size_cached)
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64", target_os = "linux"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vertex_buffer_layout() {
        // 32 bytes, same shape as StorageBuffer.
        assert_eq!(size_of::<VertexBuffer>(), 32);
        assert_eq!(align_of::<VertexBuffer>(), 8);
        assert_eq!(offset_of!(VertexBuffer, handle), 0);
        assert_eq!(offset_of!(VertexBuffer, vtable), 8);
        assert_eq!(offset_of!(VertexBuffer, byte_size_cached), 16);
        assert_eq!(offset_of!(VertexBuffer, mapped_ptr_cached), 24);
    }

    #[test]
    fn vertex_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<VertexBuffer>();
    }
}
