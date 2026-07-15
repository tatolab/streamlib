// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `HostTimelineSemaphoreMethodsVTable` — per-`HostTimelineSemaphore`
//! extern "C" dispatch for the exportable-timeline surface (#1260).

use core::ffi::c_void;

/// Layout version of [`crate::HostTimelineSemaphoreMethodsVTable`].
///
/// - v1: initial shape — `clone_handle`, `drop_handle`, `wait`,
///   `signal`, `current_value`, `export_opaque_fd`. Self-contained
///   (clone/drop live on this methods vtable, SurfaceStore-style), so
///   `GpuContextFullAccessVTable` grows only the single
///   `create_exportable_timeline_semaphore` mint slot and the
///   `HostTimelineSemaphore` PluginAbiObject needs only one vtable
///   pointer.
pub const HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Per-`HostTimelineSemaphore` method-dispatch table. The exportable
/// timeline is an Arc-shaped, refcounted PluginAbiObject: `clone_handle`
/// / `drop_handle` run `Arc::increment/decrement_strong_count` against
/// the host-internal `HostVulkanTimelineSemaphore` in host-compiled
/// code; the cdylib holds `(handle, methods)` opaquely and never reads
/// the inner's `repr(Rust)` layout.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump
/// [`HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct HostTimelineSemaphoreMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`HOST_TIMELINE_SEMAPHORE_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (zero today, never read).
    pub _reserved_padding: u32,

    /// `Arc::increment_strong_count(handle as *const HostVulkanTimelineSemaphore)`.
    pub clone_handle: unsafe extern "C" fn(handle: *const c_void),

    /// `Arc::decrement_strong_count` — releases the semaphore at zero.
    pub drop_handle: unsafe extern "C" fn(handle: *const c_void),

    /// Block until the timeline reaches `value`; `timeout_ns == u64::MAX`
    /// for no timeout. `0` = reached; non-zero = timeout / driver error.
    pub wait: unsafe extern "C" fn(
        handle: *const c_void,
        value: u64,
        timeout_ns: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// CPU-side signal advancing the counter to `value`
    /// (single-writer-per-edge; `value` strictly increasing by
    /// contract).
    pub signal: unsafe extern "C" fn(
        handle: *const c_void,
        value: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// `vkGetSemaphoreCounterValue` into `*out_value`.
    pub current_value: unsafe extern "C" fn(
        handle: *const c_void,
        out_value: *mut u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Fresh `OPAQUE_FD` via `vkGetSemaphoreFdKHR` into `*out_fd`; caller
    /// owns the returned fd. On any non-zero return `*out_fd` is written
    /// `-1` (preventing a double-close). Linux-only.
    pub export_opaque_fd: unsafe extern "C" fn(
        handle: *const c_void,
        out_fd: *mut i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for HostTimelineSemaphoreMethodsVTable {}
unsafe impl Sync for HostTimelineSemaphoreMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn host_timeline_semaphore_methods_vtable_layout() {
        // 8-byte header + 6 fn pointers = 8 + 48 = 56 bytes, align 8.
        assert_eq!(size_of::<HostTimelineSemaphoreMethodsVTable>(), 56);
        assert_eq!(align_of::<HostTimelineSemaphoreMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(HostTimelineSemaphoreMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(HostTimelineSemaphoreMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(HostTimelineSemaphoreMethodsVTable, clone_handle),
            8
        );
        assert_eq!(
            offset_of!(HostTimelineSemaphoreMethodsVTable, drop_handle),
            16
        );
        assert_eq!(offset_of!(HostTimelineSemaphoreMethodsVTable, wait), 24);
        assert_eq!(offset_of!(HostTimelineSemaphoreMethodsVTable, signal), 32);
        assert_eq!(
            offset_of!(HostTimelineSemaphoreMethodsVTable, current_value),
            40
        );
        assert_eq!(
            offset_of!(HostTimelineSemaphoreMethodsVTable, export_opaque_fd),
            48
        );
    }
}
