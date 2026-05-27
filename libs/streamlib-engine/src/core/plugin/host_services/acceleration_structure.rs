// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VulkanAccelerationStructureMethodsVTable` callbacks +
//! static vtable + accessor (issue #955).
//!
//! The per-type vtable currently carries one method slot (`label`).
//! POD getters (`device_address`, `storage_size`, `kind`) are
//! served from cached fields populated at β-shape construction
//! time and never round-trip through the vtable.

use std::ffi::c_void;

use super::run_host_extern_c;
use super::shared::wire::{slice_from_raw, write_err};



// ---------------------------------------------------------------------------
// VulkanAccelerationStructureMethodsVTable wrappers (issue #955)
//
// The per-type vtable currently carries one method slot — `label` —
// because:
//   * POD getters (`device_address`, `storage_size`, `kind`) are
//     populated at mint time via the v8 build_triangles_blas /
//     build_tlas out-params; the β-shape's cached fields are always
//     real values, no vtable dispatch needed.
//   * `vk_handle` stays host-only (vulkanalia handle layout couples
//     to vulkanalia minor version; no in-tree cdylib consumer reads
//     it — every binding goes through the ray-tracing kernel's
//     host-side `set_acceleration_structure` slot which dereferences
//     the AS host-side).
//
// `label` uses the same caller-provided byte-buffer out-param shape
// as `TextureRingSlot.surface_id` from #947 — labels longer than
// `out_buf_cap` are silently truncated (fine for diagnostic strings).
// ---------------------------------------------------------------------------

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`. The
/// leaked strong count keeps the AS alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_acceleration_structure(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanAccelerationStructureInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe {
        &*(handle as *const crate::vulkan::rhi::VulkanAccelerationStructureInner)
    })
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_vulkan_acceleration_structure_label(
    as_handle: *const c_void,
    out_buf: *mut u8,
    out_buf_cap: usize,
    out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_vulkan_acceleration_structure_label",
        || -> i32 {
            let Some(as_inner) =
                (unsafe { handle_as_acceleration_structure(as_handle) })
            else {
                write_err(
                    "label: null acceleration_structure handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buf.is_null() || out_len.is_null() {
                write_err(
                    "label: null out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_bytes = as_inner.label().as_bytes();
            // Silent truncation at buffer cap — labels are diagnostic
            // strings, not load-bearing data; an over-large label
            // just shows the prefix in logs.
            let copy_len = label_bytes.len().min(out_buf_cap);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    label_bytes.as_ptr(),
                    out_buf,
                    copy_len,
                );
                std::ptr::write(out_len, copy_len);
            }
            0
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_vulkan_acceleration_structure_label(
    _as_handle: *const c_void,
    _out_buf: *mut u8,
    _out_buf_cap: usize,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "label: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `VulkanAccelerationStructureMethodsVTable` wired to the
/// per-method wrappers above (issue #907 PR 5/5 shell + #955 method
/// dispatch).
pub static HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable =
    streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable {
        layout_version:
            streamlib_plugin_abi::VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        label: host_vulkan_acceleration_structure_label,
    };

/// Accessor for the host's static
/// `VulkanAccelerationStructureMethodsVTable` — used by
/// `VulkanAccelerationStructure::from_arc_into_raw` to populate the
/// β-shape's `methods_vtable` field.
pub fn host_vulkan_acceleration_structure_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanAccelerationStructureMethodsVTable {
    &HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE
}
#[cfg(all(test, target_os = "linux"))]
mod acceleration_structure_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 `label` method slot on
    //! `VulkanAccelerationStructureMethodsVTable` (issue #955). The
    //! wrapper must reject a null AS handle before reaching any
    //! Inner state so cdylib callers get a clean error return
    //! instead of UB.
    //!
    //! End-to-end coverage (real AS + label round-trip) is locked
    //! by the dlopen integration test for the cross-rustc fixture,
    //! which builds a real BLAS and asserts the round-tripped label
    //! matches the one passed at build time.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn label_rejects_null_acceleration_structure_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_buf = [0u8; 64];
        let mut out_len: usize = 0;
        let rc = unsafe {
            (HOST_VULKAN_ACCELERATION_STRUCTURE_METHODS_VTABLE.label)(
                std::ptr::null(),
                out_buf.as_mut_ptr(),
                out_buf.len(),
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("label: null acceleration_structure handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}
