// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VulkanTextureReadbackMethodsVTable` static + accessor
//! (M32 reservation, #1261).
//!
//! Every slot ships a typed NotYetProvided-style stub under the panic
//! net until the texture-readback fill-in (#1261) lands the real bodies.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION, VulkanTextureReadbackMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided};

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_submit(
    _readback_handle: *const c_void,
    _texture_handle: *const c_void,
    _source_layout_raw: i32,
    _out_ticket_handle_id: *mut u64,
    _out_ticket_counter: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_submit",
        || not_yet_provided("submit", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_try_read(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _out_ready: *mut u32,
    _out_bytes_ptr: *mut *const u8,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_try_read",
        || not_yet_provided("try_read", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_wait_and_read(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _timeout_ns: u64,
    _out_bytes_ptr: *mut *const u8,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_wait_and_read",
        || not_yet_provided("wait_and_read", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_try_read_copy(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _out_ready: *mut u32,
    _out_buf: *mut u8,
    _out_cap: usize,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_try_read_copy",
        || not_yet_provided("try_read_copy", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_wait_and_copy(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _timeout_ns: u64,
    _out_buf: *mut u8,
    _out_cap: usize,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_wait_and_copy",
        || not_yet_provided("wait_and_copy", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

/// Host-side `VulkanTextureReadbackMethodsVTable`, wired to the reserved
/// stubs. #1261 replaces the stub bodies with the real
/// `VulkanTextureReadback`-driving logic.
pub static HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE: VulkanTextureReadbackMethodsVTable =
    VulkanTextureReadbackMethodsVTable {
        layout_version: VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        submit: host_texture_readback_submit,
        try_read: host_texture_readback_try_read,
        wait_and_read: host_texture_readback_wait_and_read,
        try_read_copy: host_texture_readback_try_read_copy,
        wait_and_copy: host_texture_readback_wait_and_copy,
    };

/// Accessor for the host's static `VulkanTextureReadbackMethodsVTable`.
pub fn host_vulkan_texture_readback_methods_vtable() -> *const VulkanTextureReadbackMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.vulkan_texture_readback_methods_vtable.is_null() => {
            c.vulkan_texture_readback_methods_vtable
        }
        _ => &HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.layout_version,
            VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION
        );
    }

    #[test]
    fn submit_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut hid = 0u64;
        let mut counter = 0u64;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.submit)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut hid,
                &mut counter,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("submit: not yet provided"));
    }

    #[test]
    fn try_read_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut ready = 0u32;
        let mut bytes: *const u8 = std::ptr::null();
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.try_read)(
                std::ptr::null(),
                0,
                0,
                &mut ready,
                &mut bytes,
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("try_read: not yet provided"));
    }

    #[test]
    fn wait_and_read_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut bytes: *const u8 = std::ptr::null();
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.wait_and_read)(
                std::ptr::null(),
                0,
                0,
                u64::MAX,
                &mut bytes,
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("wait_and_read: not yet provided"));
    }

    #[test]
    fn try_read_copy_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut ready = 0u32;
        let mut out = [0u8; 8];
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.try_read_copy)(
                std::ptr::null(),
                0,
                0,
                &mut ready,
                out.as_mut_ptr(),
                out.len(),
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("try_read_copy: not yet provided"));
    }

    #[test]
    fn wait_and_copy_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 8];
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.wait_and_copy)(
                std::ptr::null(),
                0,
                0,
                u64::MAX,
                out.as_mut_ptr(),
                out.len(),
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("wait_and_copy: not yet provided"));
    }
}
