// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanTextureReadbackMethodsVTable` — per-`TextureReadback`
//! extern "C" dispatch for the GPU texture-readback surface (#1261).

use core::ffi::c_void;

/// Layout version of [`crate::VulkanTextureReadbackMethodsVTable`].
///
/// - v1: initial shape — `submit`, `try_read`, `wait_and_read`,
///   `try_read_copy`, `wait_and_copy`. Drop-only (`!Clone`): the parent
///   [`crate::GpuContextFullAccessVTable`] carries
///   `drop_texture_readback`, no clone slot (the Box-shaped
///   `VulkanTextureReadback` owns exclusive single-in-flight
///   resources). The borrow slots (`try_read` / `wait_and_read`) hand
///   back a raw borrow into host persistent-mapped staging; the copy
///   slots (`try_read_copy` / `wait_and_copy`) copy into a caller
///   buffer for plugins that must outlive the handle.
pub const VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Per-`TextureReadback` method-dispatch table, keyed by the readback
/// handle (`Box::into_raw(Box<Arc<VulkanTextureReadback>>)`), not the
/// gpu scope token — the primitive clones its own `Arc<HostVulkanDevice>`
/// at construction. A `ReadbackTicket` crosses as two bare `u64`
/// (`handle_id`, `counter`), preserving the engine's
/// `ForeignTicket` / `StaleTicket` identity checks.
///
/// # Return-code convention
///
/// `0` = success; non-zero = error (`InFlight` / `DescriptorMismatch` /
/// `ForeignTicket` / `StaleTicket` / `WaitTimeout` / …). Null handle /
/// null out-param is a typed error.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump
/// [`VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct VulkanTextureReadbackMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (zero today, never read).
    pub _reserved_padding: u32,

    /// Schedule a GPU→CPU copy of `texture_handle` (a borrowed `Texture`
    /// PluginAbiObject `handle` field — the make-borrow convention) at
    /// `source_layout_raw` (raw `VkImageLayout`). Issues a ticket
    /// (`out_ticket_handle_id` / `out_ticket_counter`). Non-zero on
    /// `InFlight` / `DescriptorMismatch` / …
    pub submit: unsafe extern "C" fn(
        readback_handle: *const c_void,
        texture_handle: *const c_void,
        source_layout_raw: i32,
        out_ticket_handle_id: *mut u64,
        out_ticket_counter: *mut u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking poll. `out_ready = 1` when the copy is complete;
    /// `out_bytes_ptr` / `out_len` then borrow the host persistent-mapped
    /// staging (row stride = `width × bytes_per_pixel`, no padding). The
    /// borrow is valid only until the next `submit` on the same handle.
    pub try_read: unsafe extern "C" fn(
        readback_handle: *const c_void,
        ticket_handle_id: u64,
        ticket_counter: u64,
        out_ready: *mut u32,
        out_bytes_ptr: *mut *const u8,
        out_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking await on the handle timeline (`timeout_ns == u64::MAX` =
    /// no timeout), then the same borrow contract as `try_read`.
    pub wait_and_read: unsafe extern "C" fn(
        readback_handle: *const c_void,
        ticket_handle_id: u64,
        ticket_counter: u64,
        timeout_ns: u64,
        out_bytes_ptr: *mut *const u8,
        out_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Non-blocking poll that COPIES into the caller's `out_buf` when
    /// ready (for plugins that must outlive the handle). `out_ready = 1`
    /// when copied; `out_len` records the byte count. `status 2` =
    /// `out_buf` too small (required length in `out_len`).
    pub try_read_copy: unsafe extern "C" fn(
        readback_handle: *const c_void,
        ticket_handle_id: u64,
        ticket_counter: u64,
        out_ready: *mut u32,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Blocking await, then COPY into the caller's `out_buf`. `status 2`
    /// = `out_buf` too small.
    pub wait_and_copy: unsafe extern "C" fn(
        readback_handle: *const c_void,
        ticket_handle_id: u64,
        ticket_counter: u64,
        timeout_ns: u64,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanTextureReadbackMethodsVTable {}
unsafe impl Sync for VulkanTextureReadbackMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_texture_readback_methods_vtable_layout() {
        // 8-byte header + 5 fn pointers = 8 + 40 = 48 bytes, align 8.
        assert_eq!(size_of::<VulkanTextureReadbackMethodsVTable>(), 48);
        assert_eq!(align_of::<VulkanTextureReadbackMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanTextureReadbackMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanTextureReadbackMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(offset_of!(VulkanTextureReadbackMethodsVTable, submit), 8);
        assert_eq!(offset_of!(VulkanTextureReadbackMethodsVTable, try_read), 16);
        assert_eq!(
            offset_of!(VulkanTextureReadbackMethodsVTable, wait_and_read),
            24
        );
        assert_eq!(
            offset_of!(VulkanTextureReadbackMethodsVTable, try_read_copy),
            32
        );
        assert_eq!(
            offset_of!(VulkanTextureReadbackMethodsVTable, wait_and_copy),
            40
        );
    }
}
