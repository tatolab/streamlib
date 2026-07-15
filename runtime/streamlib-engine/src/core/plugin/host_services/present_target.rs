// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `PresentTargetMethodsVTable` static + accessor (M32
//! reservation, #1258).
//!
//! Every slot ships a typed NotYetProvided-style stub under the panic
//! net until the present-target fill-in (#1258) lands the real bodies.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    ColorTraitsRepr, HdrStaticMetadataRepr, PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION,
    PresentFrameBeginRepr, PresentTargetMethodsVTable, SemaphoreSubmitInfoRepr,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided};

unsafe extern "C" fn host_present_target_begin_frame(
    _present_handle: *const c_void,
    _out_frame: *mut PresentFrameBeginRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_begin_frame",
        || not_yet_provided("begin_frame", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_present_target_end_frame(
    _present_handle: *const c_void,
    _recorder_handle: *const c_void,
    _extra_waits_ptr: *const SemaphoreSubmitInfoRepr,
    _extra_waits_count: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_end_frame",
        || not_yet_provided("end_frame", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_present_target_recreate(
    _present_handle: *const c_void,
    _width: u32,
    _height: u32,
    _color: *const ColorTraitsRepr,
    _out_color_format_raw: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_recreate",
        || not_yet_provided("recreate", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_present_target_set_hdr_metadata(
    _present_handle: *const c_void,
    _metadata: *const HdrStaticMetadataRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_set_hdr_metadata",
        || not_yet_provided("set_hdr_metadata", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

/// Host-side `PresentTargetMethodsVTable`, wired to the reserved stubs.
/// #1258 replaces the stub bodies with the real present-target driving.
pub static HOST_PRESENT_TARGET_METHODS_VTABLE: PresentTargetMethodsVTable =
    PresentTargetMethodsVTable {
        layout_version: PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        begin_frame: host_present_target_begin_frame,
        end_frame: host_present_target_end_frame,
        recreate: host_present_target_recreate,
        set_hdr_metadata: host_present_target_set_hdr_metadata,
    };

/// Accessor for the host's static `PresentTargetMethodsVTable` — used by
/// the cdylib `PresentTarget` PluginAbiObject constructor (host mode
/// resolves the local static; cdylib mode resolves the host-installed
/// pointer cached on [`super::HostCallbacks`]).
pub fn host_present_target_methods_vtable() -> *const PresentTargetMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.present_target_methods_vtable.is_null() => c.present_target_methods_vtable,
        _ => &HOST_PRESENT_TARGET_METHODS_VTABLE,
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
            HOST_PRESENT_TARGET_METHODS_VTABLE.layout_version,
            PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION
        );
    }

    #[test]
    fn begin_frame_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut frame = PresentFrameBeginRepr::default();
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.begin_frame)(
                std::ptr::null(),
                &mut frame,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("begin_frame: not yet provided"));
    }

    #[test]
    fn end_frame_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.end_frame)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("end_frame: not yet provided"));
    }

    #[test]
    fn recreate_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut fmt: u32 = 0;
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.recreate)(
                std::ptr::null(),
                64,
                64,
                std::ptr::null(),
                &mut fmt,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("recreate: not yet provided"));
    }

    #[test]
    fn set_hdr_metadata_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.set_hdr_metadata)(
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("set_hdr_metadata: not yet provided"));
    }
}
