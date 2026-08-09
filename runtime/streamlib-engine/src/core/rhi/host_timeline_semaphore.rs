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
    /// `Arc::into_raw`, released by `Drop`.
    pub fn from_arc(inner: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>) -> Self {
        let handle = Arc::into_raw(inner) as *const c_void;
        Self { handle }
    }
}

#[cfg(target_os = "linux")]
impl Clone for HostTimelineSemaphore {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)`
            // (see `from_arc`); balanced by the Drop impl below.
            unsafe {
                Arc::increment_strong_count(self.handle as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore);
            }
        }
        Self {
            handle: self.handle,
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for HostTimelineSemaphore {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in `from_arc`
            // and any `Clone` increment.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore);
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
    

    #[test]
    fn host_timeline_semaphore_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HostTimelineSemaphore>();
    }
}
