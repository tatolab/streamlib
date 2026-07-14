// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's RHI [`StorageBuffer`] PluginAbiObject.
//!
//! Layout-stable `(handle, vtable, cached POD)` shape — Arc refcount
//! accounting runs in host-compiled code via the vtable's
//! `clone_storage_buffer` / `drop_storage_buffer` callbacks. The host
//! `HostVulkanBuffer` backing + the `from_arc_into_raw` / `host_inner`
//! constructors stay in the engine.

use std::ffi::c_void;

use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

/// Raw byte-shaped GPU storage buffer (SSBO).
///
/// Linux-only — SSBO allocation rides the Vulkan RHI path. Compute
/// kernels bind it via
/// [`crate::rhi::VulkanComputeKernel::set_storage_buffer_storage`].
#[repr(C)]
pub struct StorageBuffer {
    /// Opaque handle to the host's `Arc<HostVulkanBuffer>`.
    pub(crate) handle: *const c_void,
    /// Vtable for plugin ABI Clone/Drop dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
    /// Cached byte size (captured at construction host-side).
    pub(crate) byte_size_cached: u64,
    /// Cached persistently-mapped CPU pointer. Stable for the buffer's
    /// lifetime for HOST_VISIBLE allocations; null for DEVICE_LOCAL
    /// imports.
    pub(crate) mapped_ptr_cached: *mut u8,
}

// SAFETY: `handle` points at an `Arc<HostVulkanBuffer>` whose interior is
// Send+Sync. Refcount management crosses the plugin ABI through the
// vtable; the mapped pointer is null or a persistently-mapped pointer the
// host's VMA allocator guarantees stable for the buffer's lifetime.
unsafe impl Send for StorageBuffer {}
unsafe impl Sync for StorageBuffer {}

impl StorageBuffer {
    /// Total buffer size in bytes. Cached at construction; pure POD read.
    pub fn byte_size(&self) -> u64 {
        self.byte_size_cached
    }

    /// Persistently mapped CPU pointer for HOST_VISIBLE allocations.
    /// Cached at construction; pure POD read. Returns null for
    /// DEVICE_LOCAL imports.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr_cached
    }
}

impl Clone for StorageBuffer {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at construction; the vtable's
            // `clone_storage_buffer` contract is
            // `Arc::increment_strong_count` host-side.
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

impl Drop for StorageBuffer {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_storage_buffer` bumps.
            unsafe {
                ((*self.vtable).drop_storage_buffer)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for StorageBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageBuffer")
            .field("byte_size", &self.byte_size_cached)
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn storage_buffer_layout() {
        // Must match the engine's `core/rhi/storage_buffer.rs`:
        //   handle @ 0, vtable @ 8, byte_size_cached @ 16,
        //   mapped_ptr_cached @ 24. Total 32 bytes, align 8.
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
