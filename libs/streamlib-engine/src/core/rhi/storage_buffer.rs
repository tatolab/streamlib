// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Raw byte-shaped GPU storage buffer (SSBO).
//!
//! Layout-stable `(handle, vtable, cached POD)` shape — Arc refcount
//! accounting runs in host-compiled code via the vtable's
//! `clone_storage_buffer` / `drop_storage_buffer` callbacks.
//!
//! Sibling of [`PixelBuffer`](super::PixelBuffer) for callers that
//! have raw bytes rather than formatted pixel data — V4L2-shape capture
//! frames pre-conversion, audio→GPU compute inputs, ML tensor uploads.
//! Exposes byte size and a mapped pointer only; no pixel-shaped
//! getters that would be meaningless on an SSBO.

#[cfg(target_os = "linux")]
use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

/// Raw byte-shaped GPU storage buffer (SSBO).
///
/// Linux-only — SSBO allocation rides the Vulkan RHI path. Compute
/// kernels bind it via
/// [`crate::vulkan::rhi::VulkanComputeKernel::set_storage_buffer`].
///
/// Layout-stable: every field is either a primitive or an opaque
/// pointer. Engine-internal callers reach the underlying
/// `Arc<HostVulkanBuffer>` via [`Self::host_inner`]; cdylib callers
/// route Clone/Drop through the vtable.
#[cfg(target_os = "linux")]
#[repr(C)]
pub struct StorageBuffer {
    /// Opaque handle to the host's `Arc<HostVulkanBuffer>` (produced
    /// by `Arc::into_raw`).
    pub(crate) handle: *const c_void,
    /// Vtable for cross-DSO Clone/Drop dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
    /// Cached byte size (captured at construction).
    pub(crate) byte_size_cached: u64,
    /// Cached persistently-mapped CPU pointer. Stable for the
    /// buffer's lifetime for HOST_VISIBLE allocations; null for
    /// DEVICE_LOCAL imports (DMA-BUF without HOST_VISIBLE).
    pub(crate) mapped_ptr_cached: *mut u8,
}

// SAFETY: `handle` points at an `Arc<HostVulkanBuffer>` whose interior
// is Send+Sync (the underlying VMA allocation is). Refcount management
// crosses the cdylib boundary through the vtable but Arc bookkeeping
// runs in host-compiled code regardless. `mapped_ptr_cached` is either
// null or a persistently-mapped pointer the host's VMA allocator
// guarantees stable for the buffer's lifetime.
#[cfg(target_os = "linux")]
unsafe impl Send for StorageBuffer {}
#[cfg(target_os = "linux")]
unsafe impl Sync for StorageBuffer {}

#[cfg(target_os = "linux")]
impl StorageBuffer {
    /// Wrap an externally-allocated `Arc<HostVulkanBuffer>` as a
    /// `StorageBuffer`. The inner buffer must have been allocated via
    /// one of the SSBO constructors
    /// ([`crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible`]
    /// or
    /// [`crate::vulkan::rhi::HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer`]).
    pub fn from_host_vulkan_buffer(
        inner: Arc<crate::vulkan::rhi::HostVulkanBuffer>,
    ) -> Self {
        Self::from_arc_into_raw(inner)
    }

    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw`, capture POD fields, resolve the host-mode
    /// vtable.
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
    ///
    /// **Panics if called from cdylib code** for the same reason
    /// [`super::Texture::host_inner`] does.
    pub(crate) fn host_inner(&self) -> &crate::vulkan::rhi::HostVulkanBuffer {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "StorageBuffer::host_inner() reached from cdylib code; this method \
                 must dispatch through the GpuContextLimitedAccessVTable. The panic \
                 is caught by run_host_extern_c at the FFI boundary."
            );
        }
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`
        // (see `from_arc_into_raw`). The leaked strong count keeps the
        // `HostVulkanBuffer` alive at least until `Drop` runs.
        unsafe { &*(self.handle as *const crate::vulkan::rhi::HostVulkanBuffer) }
    }

    /// Total buffer size in bytes. Cached at construction; pure POD
    /// read with no cross-DSO dispatch.
    pub fn byte_size(&self) -> u64 {
        self.byte_size_cached
    }

    /// Persistently mapped CPU pointer for HOST_VISIBLE allocations.
    /// Cached at construction; pure POD read with no cross-DSO
    /// dispatch. Returns null for DEVICE_LOCAL imports (DMA-BUF
    /// without HOST_VISIBLE).
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr_cached
    }
}

#[cfg(target_os = "linux")]
impl Clone for StorageBuffer {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle were paired at construction; the
            // vtable's `clone_storage_buffer` contract is
            // `Arc::increment_strong_count(handle)` on the host side.
            unsafe {
                ((*self.vtable).clone_storage_buffer)(self.handle);
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
impl Drop for StorageBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `clone_storage_buffer` bumps.
            unsafe {
                ((*self.vtable).drop_storage_buffer)(self.handle);
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for StorageBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageBuffer")
            .field("byte_size", &self.byte_size_cached)
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64", target_os = "linux"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn storage_buffer_layout() {
        // Pin the byte-level shape. Fields:
        //   handle              : *const c_void → offset 0,  size 8
        //   vtable              : *const VTable → offset 8,  size 8
        //   byte_size_cached    : u64           → offset 16, size 8
        //   mapped_ptr_cached   : *mut u8       → offset 24, size 8
        // Total: 32 bytes, 8-byte alignment.
        assert_eq!(size_of::<StorageBuffer>(), 32);
        assert_eq!(align_of::<StorageBuffer>(), 8);
        assert_eq!(offset_of!(StorageBuffer, handle), 0);
        assert_eq!(offset_of!(StorageBuffer, vtable), 8);
        assert_eq!(offset_of!(StorageBuffer, byte_size_cached), 16);
        assert_eq!(offset_of!(StorageBuffer, mapped_ptr_cached), 24);
    }

    #[test]
    fn storage_buffer_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StorageBuffer>();
    }
}
