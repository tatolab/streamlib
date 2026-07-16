// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side wire envelope for the exportable-timeline PluginAbiObject
//! (#1260).
//!
//! Layout-stable `(handle, methods)` shape — the exportable timeline is
//! an Arc-refcounted PluginAbiObject minted by the FullAccess vtable's
//! `create_exportable_timeline_semaphore` slot and handed to a cdylib.
//! The cdylib holds `(handle, methods)` opaquely and dispatches
//! clone/drop/wait/signal/current_value/export_opaque_fd through the
//! per-type [`HostTimelineSemaphoreMethodsVTable`]; refcount accounting
//! runs in host-compiled code (`Arc::increment/decrement_strong_count`
//! against [`crate::vulkan::rhi::HostVulkanTimelineSemaphore`]).
//!
//! Sibling shape of [`super::StorageBuffer`] / [`super::RhiColorConverter`]:
//! every field is an opaque pointer, the byte layout is pinned by a
//! regression test, and the SDK carries a byte-identical twin
//! (`streamlib_plugin_sdk::rhi::HostTimelineSemaphore`). The exportable
//! timeline is self-contained — clone/drop live on its own methods vtable
//! (SurfaceStore-style), so the envelope needs only one vtable pointer,
//! not a parent-vtable-plus-methods-vtable pair.

#[cfg(target_os = "linux")]
use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::HostTimelineSemaphoreMethodsVTable;

/// Host-side wire envelope for an OPAQUE_FD-exportable timeline
/// semaphore crossing the plugin ABI.
///
/// Linux-only — exportable timeline construction rides the Vulkan RHI
/// path. Minted by
/// [`crate::core::context::GpuContext::create_exportable_timeline_semaphore`]
/// via the FullAccess `create_exportable_timeline_semaphore` slot;
/// resolved consumer-side by the SDK twin's wait/signal/current_value/
/// export_opaque_fd methods.
///
/// Layout-stable: `handle` is `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)`
/// (the same inner pointer the SurfaceStore `register_texture` slot
/// derefs for its `produce_done` / `consume_done` sidecars); `methods`
/// points at the host-static [`HostTimelineSemaphoreMethodsVTable`].
#[cfg(target_os = "linux")]
#[repr(C)]
pub struct HostTimelineSemaphore {
    /// Opaque handle to the host's `Arc<HostVulkanTimelineSemaphore>`
    /// (produced by `Arc::into_raw`).
    pub(crate) handle: *const c_void,
    /// Per-type vtable for plugin ABI clone/drop + method dispatch.
    /// Self-contained (clone/drop live here, not on a parent vtable).
    pub(crate) methods: *const HostTimelineSemaphoreMethodsVTable,
}

// SAFETY: `handle` points at an `Arc<HostVulkanTimelineSemaphore>` whose
// interior (a `vulkanalia::Device` clone + `vk::Semaphore`) is Send+Sync.
// Refcount bookkeeping crosses the cdylib boundary through the methods
// vtable but always runs in host-compiled code.
#[cfg(target_os = "linux")]
unsafe impl Send for HostTimelineSemaphore {}
#[cfg(target_os = "linux")]
unsafe impl Sync for HostTimelineSemaphore {}

#[cfg(target_os = "linux")]
impl HostTimelineSemaphore {
    /// Mint the wire envelope from an owned
    /// `Arc<HostVulkanTimelineSemaphore>`. Leaks one strong count via
    /// `Arc::into_raw` (released by the cdylib's `drop_handle`) and pins
    /// the host-static methods vtable pointer.
    pub fn from_arc(inner: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        let methods = crate::core::plugin::host_services::host_timeline_semaphore_methods_vtable();
        Self { handle, methods }
    }
}

#[cfg(target_os = "linux")]
impl Clone for HostTimelineSemaphore {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.methods.is_null() {
            // SAFETY: handle + methods paired at mint time; the vtable's
            // `clone_handle` contract is `Arc::increment_strong_count`.
            unsafe {
                ((*self.methods).clone_handle)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            methods: self.methods,
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for HostTimelineSemaphore {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.methods.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in `from_arc` and
            // any `clone_handle` bumps.
            unsafe {
                ((*self.methods).drop_handle)(self.handle);
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for HostTimelineSemaphore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostTimelineSemaphore").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64", target_os = "linux"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn host_timeline_semaphore_layout() {
        // Pin the byte-level shape. Must match the SDK twin
        // `streamlib_plugin_sdk::rhi::HostTimelineSemaphore`:
        //   handle  : *const c_void  → offset 0, size 8
        //   methods : *const VTable  → offset 8, size 8
        // Total: 16 bytes, 8-byte alignment.
        assert_eq!(size_of::<HostTimelineSemaphore>(), 16);
        assert_eq!(align_of::<HostTimelineSemaphore>(), 8);
        assert_eq!(offset_of!(HostTimelineSemaphore, handle), 0);
        assert_eq!(offset_of!(HostTimelineSemaphore, methods), 8);
    }

    #[test]
    fn host_timeline_semaphore_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HostTimelineSemaphore>();
    }
}
