// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `RuntimeContextVTable` — per-instance accessors for the RuntimeContext shim.

use core::ffi::c_void;

/// Layout version of [`crate::RuntimeContextVTable`]. Pinned at offset 0;
/// newer fields append to the end and bump this constant.
pub const RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Dispatch table the cdylib's `RuntimeContext{Full,Limited}Access`
/// shim uses to read host-owned runtime context state. Every accessor
/// on the shim's public API routes through this table — no Rust
/// trait-object / shared-struct-layout crossing at the cdylib
/// boundary.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0. Older vtables loaded into
/// newer hosts are rejected cleanly. New fields go at the **end** and
/// bump [`RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION`].
///
/// # Opaque-handle returns
///
/// `gpu_full_access` / `gpu_limited_access` return `*const c_void`
/// opaque handles paired with [`crate::GpuContextLimitedAccessVTable`] for
/// method dispatch.
///
/// `audio_clock_handle` and `runtime_ops_handle` return opaque per-
/// instance handles paired with the static vtables on [`crate::HostServices`]
/// ([`crate::HostServices::audio_clock_vtable`],
/// [`crate::HostServices::runtime_ops_vtable`]).
#[repr(C)]
pub struct RuntimeContextVTable {
    /// Vtable layout version. Must equal
    /// [`RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    // -------------------------------------------------------------------------
    // Identifier accessors (owned-return; cdylib does not retain a borrow)
    // -------------------------------------------------------------------------

    /// Copy the runtime id as UTF-8 bytes into `out_buf`. Returns the
    /// required length; `*out_len` receives the actually-written
    /// count (`min(required, out_buf_cap)`). Truncation is benign;
    /// the caller resizes and retries when `required > out_buf_cap`.
    pub runtime_id_copy: unsafe extern "C" fn(
        ctx: *const c_void,
        out_buf: *mut u8,
        out_buf_cap: usize,
        out_len: *mut usize,
    ) -> usize,

    /// Copy the processor id as UTF-8 bytes into `out_buf`. Returns
    /// `-1` when the processor id is `None` (shared/global ctx); for
    /// `Some`, returns the required length and writes `*out_len` like
    /// [`Self::runtime_id_copy`].
    pub processor_id_copy: unsafe extern "C" fn(
        ctx: *const c_void,
        out_buf: *mut u8,
        out_buf_cap: usize,
        out_len: *mut usize,
    ) -> isize,

    // -------------------------------------------------------------------------
    // Lifecycle flags
    // -------------------------------------------------------------------------

    pub is_paused: unsafe extern "C" fn(ctx: *const c_void) -> bool,
    pub should_process: unsafe extern "C" fn(ctx: *const c_void) -> bool,

    // -------------------------------------------------------------------------
    // GPU context handles
    // -------------------------------------------------------------------------

    /// Returns an opaque handle to the privileged [`GpuContextFullAccess`].
    /// Pointer is valid for the lifetime of the surrounding
    /// `RuntimeContextFullAccess` shim. Paired with the methods
    /// reached via [`crate::HostServices::gpu_context_limited_access_vtable`]
    /// for the limited-access surface (FullAccess is engine-only
    /// today; cross-DSO FullAccess wiring is future-phase work).
    pub gpu_full_access: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,

    /// Returns an opaque handle to the restricted [`GpuContextLimitedAccess`].
    /// Paired with [`crate::HostServices::gpu_context_limited_access_vtable`]
    /// for method dispatch.
    pub gpu_limited_access: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,

    // -------------------------------------------------------------------------
    // Host-owned services (handles; static vtables live on HostServices)
    // -------------------------------------------------------------------------

    /// Opaque handle to the runtime's audio clock. Pair with
    /// [`crate::HostServices::audio_clock_vtable`] to call methods on it.
    /// The handle remains valid for the lifetime of the runtime.
    pub audio_clock_handle: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,

    /// Opaque handle to the runtime's graph-mutation operations.
    /// Pair with [`crate::HostServices::runtime_ops_vtable`] to invoke
    /// methods. The handle remains valid for the lifetime of the
    /// runtime.
    pub runtime_ops_handle: unsafe extern "C" fn(ctx: *const c_void) -> *const c_void,
}

// Safety: every field is a primitive or a fn pointer. The vtable's
// `&'static` storage on the host side outlives the cdylib's process
// lifetime via the `LOADED_PLUGIN_LIBRARIES` pinning shape.
unsafe impl Send for RuntimeContextVTable {}
unsafe impl Sync for RuntimeContextVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn runtime_context_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 8 fn pointers (8 bytes each)
        // = 4 + 4 + 8*8 = 72 bytes
        assert_eq!(size_of::<RuntimeContextVTable>(), 72);
        assert_eq!(align_of::<RuntimeContextVTable>(), 8);
        assert_eq!(offset_of!(RuntimeContextVTable, layout_version), 0);
        assert_eq!(offset_of!(RuntimeContextVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(RuntimeContextVTable, runtime_id_copy), 8);
        assert_eq!(offset_of!(RuntimeContextVTable, processor_id_copy), 16);
        assert_eq!(offset_of!(RuntimeContextVTable, is_paused), 24);
        assert_eq!(offset_of!(RuntimeContextVTable, should_process), 32);
        assert_eq!(offset_of!(RuntimeContextVTable, gpu_full_access), 40);
        assert_eq!(offset_of!(RuntimeContextVTable, gpu_limited_access), 48);
        assert_eq!(offset_of!(RuntimeContextVTable, audio_clock_handle), 56);
        assert_eq!(offset_of!(RuntimeContextVTable, runtime_ops_handle), 64);
    }
}
