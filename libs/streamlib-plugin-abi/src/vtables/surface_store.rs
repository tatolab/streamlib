// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SurfaceStoreVTable` — extern "C" dispatch for cross-process surface sharing.

use core::ffi::c_void;

/// Layout version of [`SurfaceStoreVTable`].
pub const SURFACE_STORE_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Dispatch table for the host's `SurfaceStore`. The cdylib obtains a
/// handle via [`GpuContextLimitedAccessVTable::surface_store`] and
/// reads the static vtable from [`HostServices::surface_store_vtable`].
///
/// Lives in its own vtable (not folded into
/// [`GpuContextLimitedAccessVTable`]) for two reasons:
/// 1. **Surface-area discipline** — `SurfaceStore`'s public method
///    surface is large (~10 methods, mixing cross-platform and
///    Linux-only operations) and conceptually distinct from the GPU
///    capability surface. Folding it into the parent vtable would
///    nearly double `GpuContextLimitedAccessVTable`'s size without
///    adding semantic clarity.
/// 2. **Separate-vtable-per-subsystem precedent** — `AudioClockVTable`
///    already lives outside `RuntimeContextVTable` at the
///    `HostServices` level (via
///    [`HostServices::audio_clock_vtable`]); the same shape
///    applies here.
///
/// # Handle lifetime
///
/// `clone_handle` / `drop_handle` mirror every other Arc-handle β-
/// reshape: `clone_handle(borrowed) -> owned` bumps the host's
/// `Arc<SurfaceStoreInner>` refcount; `drop_handle(owned)` releases.
/// The owned handle remains valid even after the originating
/// `RuntimeContext` is dropped — matches the existing
/// `SurfaceStore: Clone` contract.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. New methods append to the
/// end and bump [`SURFACE_STORE_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct SurfaceStoreVTable {
    /// Vtable layout version. Must equal
    /// [`SURFACE_STORE_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps following pointers naturally aligned;
    /// zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Handle lifetime
    // -------------------------------------------------------------------------

    /// Bump the refcount on a `SurfaceStore` handle.
    /// `Arc::increment_strong_count(handle as *const SurfaceStoreInner)`.
    pub clone_handle: unsafe extern "C" fn(handle: *const c_void),

    /// Decrement the refcount on a `SurfaceStore` handle. When the
    /// strong count reaches zero the underlying connection / cache
    /// state is dropped.
    pub drop_handle: unsafe extern "C" fn(handle: *const c_void),

    // -------------------------------------------------------------------------
    // Cross-platform method dispatch
    // -------------------------------------------------------------------------

    /// Connect to the surface-share service (XPC on macOS, Unix
    /// socket on Linux). On success returns 0; on failure writes a
    /// UTF-8 error into `err_buf` and returns non-zero.
    pub connect: unsafe extern "C" fn(
        handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Disconnect from the surface-share service.
    pub disconnect: unsafe extern "C" fn(
        handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check in a pixel buffer for cross-process sharing. The
    /// returned `surface_id` is written into `out_id_buf` (capped at
    /// `out_id_cap`); the actual length is stored in `*out_id_len`.
    /// Truncation returns the required length without writing.
    pub check_in: unsafe extern "C" fn(
        handle: *const c_void,
        pixel_buffer: *const c_void,
        out_id_buf: *mut u8,
        out_id_cap: usize,
        out_id_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Check out a surface by its `surface_id`. On success writes a
    /// `PixelBuffer` β-shape into `*out_pixel_buffer` and returns 0.
    pub check_out: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Register a pre-allocated buffer under the given pool id.
    pub register_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        pool_id_ptr: *const u8,
        pool_id_len: usize,
        pixel_buffer: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Look up a previously-registered buffer by its pool id. Writes
    /// a `PixelBuffer` β-shape into `*out_pixel_buffer` on success.
    pub lookup_buffer: unsafe extern "C" fn(
        handle: *const c_void,
        pool_id_ptr: *const u8,
        pool_id_len: usize,
        out_pixel_buffer: *mut c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Release a checked-out surface by its `surface_id`. Idempotent.
    pub release: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // Linux-only method dispatch (stub on other platforms)
    // -------------------------------------------------------------------------
    //
    // `register_texture` / `register_pixel_buffer_with_timeline` /
    // `lookup_texture` / `update_image_layout` are Linux-only on the
    // host side (they wrap DMA-BUF / OPAQUE_FD surface-share IPC).
    // Non-Linux hosts ship stubs that return non-zero with a clean
    // error message.

    /// Register a texture for cross-process sharing. `texture` is a
    /// `*const Texture` β-shape pointer; `timeline_handle` is an
    /// opaque `Arc<HostVulkanTimelineSemaphore>` pointer (null for
    /// "no timeline") — engine-only, cdylibs pass null. `layout_raw`
    /// is the i32 `VkImageLayout` enumerant.
    pub register_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        texture: *const c_void,
        timeline_handle: *const c_void,
        layout_raw: i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Register a pixel buffer with an optional timeline-semaphore
    /// sidecar. Same `timeline_handle` shape as
    /// [`Self::register_texture`].
    pub register_pixel_buffer_with_timeline: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        pixel_buffer: *const c_void,
        timeline_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Look up a registered texture by `surface_id`. Writes a
    /// `Texture` β-shape into `*out_texture` and the producer's
    /// last-published `VkImageLayout` (raw i32) into `*out_layout_raw`.
    pub lookup_texture: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        out_texture: *mut c_void,
        out_layout_raw: *mut i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Update the published `VkImageLayout` for an already-registered
    /// texture. Linux-only on the host side.
    pub update_image_layout: unsafe extern "C" fn(
        handle: *const c_void,
        id_ptr: *const u8,
        id_len: usize,
        layout_raw: i32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for SurfaceStoreVTable {}
unsafe impl Sync for SurfaceStoreVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn surface_store_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 13 fn
        // pointers (8 bytes each) = 4 + 4 + 104 = 112 bytes.
        assert_eq!(size_of::<SurfaceStoreVTable>(), 112);
        assert_eq!(align_of::<SurfaceStoreVTable>(), 8);
        assert_eq!(offset_of!(SurfaceStoreVTable, layout_version), 0);
        assert_eq!(offset_of!(SurfaceStoreVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(SurfaceStoreVTable, clone_handle), 8);
        assert_eq!(offset_of!(SurfaceStoreVTable, drop_handle), 16);
        assert_eq!(offset_of!(SurfaceStoreVTable, connect), 24);
        assert_eq!(offset_of!(SurfaceStoreVTable, disconnect), 32);
        assert_eq!(offset_of!(SurfaceStoreVTable, check_in), 40);
        assert_eq!(offset_of!(SurfaceStoreVTable, check_out), 48);
        assert_eq!(offset_of!(SurfaceStoreVTable, register_buffer), 56);
        assert_eq!(offset_of!(SurfaceStoreVTable, lookup_buffer), 64);
        assert_eq!(offset_of!(SurfaceStoreVTable, release), 72);
        assert_eq!(offset_of!(SurfaceStoreVTable, register_texture), 80);
        assert_eq!(
            offset_of!(SurfaceStoreVTable, register_pixel_buffer_with_timeline),
            88
        );
        assert_eq!(offset_of!(SurfaceStoreVTable, lookup_texture), 96);
        assert_eq!(offset_of!(SurfaceStoreVTable, update_image_layout), 104);
    }
}
