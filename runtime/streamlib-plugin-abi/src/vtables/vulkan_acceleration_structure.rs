// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanAccelerationStructureMethodsVTable` — per-type vtable for AS method dispatch.

use core::ffi::c_void;

/// Layout version of [`crate::VulkanAccelerationStructureMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only (issue #907 Phase E
///   PR 5/5).
/// - v2: appended `label` slot returning the AS's human-readable
///   label via a caller-provided byte buffer (same shape as
///   `TextureRingSlot.surface_id` from #947 — `String` layout
///   isn't cdylib-safe). `vk_handle` stays host-only — the
///   `vk::AccelerationStructureKHR` is a vulkanalia handle that
///   can't safely cross the plugin ABI without vulkanalia
///   version coupling, and no in-tree cdylib consumer reads it.
///   The POD getters (`device_address`, `storage_size`, `kind`)
///   are populated at mint time via the v8
///   [`crate::GpuContextFullAccessVTable::build_triangles_blas`] /
///   `build_tlas` out-params and don't need vtable slots.
pub const VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Per-type method-dispatch vtable for the
/// `VulkanAccelerationStructure` PluginAbiObject (issue #907 Phase E PR 5/5
/// + #955 method dispatch).
///
/// Mirrors the kernel methods-vtable shape. POD getters
/// (`device_address`, `kind`, `storage_size`) are populated at mint
/// time via the v8 [`crate::GpuContextFullAccessVTable::build_triangles_blas`]
/// / `build_tlas` out-params and don't need vtable slots — the
/// cached values on the PluginAbiObject struct are always real, never
/// placeholder zeros.
///
/// The single vtable slot is `label`, which uses the same byte-
/// buffer out-param shape as `TextureRingSlot.surface_id` from #947.
/// `vk_handle` stays host-only — the
/// `vk::AccelerationStructureKHR` is a vulkanalia handle whose
/// layout couples to the vulkanalia minor version, and there is no
/// in-tree cdylib consumer that reads it (every binding into a
/// ray-tracing kernel goes through the host-side
/// `set_acceleration_structure` slot, which dereferences the AS on
/// the host side and reads `vk_handle` there).
#[repr(C)]
pub struct VulkanAccelerationStructureMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Read the AS's human-readable label into a caller-provided
    /// byte buffer. `out_buf` / `out_buf_cap` describe the buffer;
    /// `*out_len` receives the number of bytes written (≤ `out_buf_cap`
    /// — labels longer than the buffer are silently truncated, which
    /// is fine for diagnostic strings). Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure (null AS
    /// handle, null out-buffer pointer, etc.).
    pub label: unsafe extern "C" fn(
        as_handle: *const c_void,
        out_buf: *mut u8,
        out_buf_cap: usize,
        out_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanAccelerationStructureMethodsVTable {}
unsafe impl Sync for VulkanAccelerationStructureMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_acceleration_structure_methods_vtable_layout() {
        // v2 (`label` slot added — #955):
        //   layout_version       @ 0 (4 bytes, u32)
        //   _reserved_padding    @ 4 (4 bytes, u32)
        //   label                @ 8 (8 bytes, fn pointer)
        // Total = 16 bytes, align = 8.
        assert_eq!(size_of::<VulkanAccelerationStructureMethodsVTable>(), 16);
        assert_eq!(align_of::<VulkanAccelerationStructureMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanAccelerationStructureMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanAccelerationStructureMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanAccelerationStructureMethodsVTable, label),
            8
        );
    }
}
