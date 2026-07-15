// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VideoEncoderSessionMethodsVTable` static + accessor (M32
//! reservation, #1259).
//!
//! Every slot ships a typed NotYetProvided-style stub under the panic
//! net until the hardware-video fill-in (#1259) lands the real bodies.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION, VideoEncodedPacketRepr,
    VideoEncoderSessionMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided};

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_video_encoder_header(
    _session: *const c_void,
    _out_buf: *mut u8,
    _out_cap: usize,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_header",
        || not_yet_provided("header", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_encoder_force_idr(
    _session: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_force_idr",
        || not_yet_provided("force_idr", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_video_encoder_submit_frame_nv12(
    _session: *const c_void,
    _nv12_ptr: *const u8,
    _nv12_len: usize,
    _has_timestamp: u8,
    _timestamp_ns: i64,
    _out_packet_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_submit_frame_nv12",
        || not_yet_provided("submit_frame_nv12", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_video_encoder_submit_texture(
    _session: *const c_void,
    _texture_handle: *const c_void,
    _input_layout: i32,
    _has_timestamp: u8,
    _timestamp_ns: i64,
    _out_packet_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_submit_texture",
        || not_yet_provided("submit_texture", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_video_encoder_drain_packet(
    _session: *const c_void,
    _index: u32,
    _out_meta: *mut VideoEncodedPacketRepr,
    _out_data_buf: *mut u8,
    _out_data_cap: usize,
    _out_data_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_drain_packet",
        || not_yet_provided("drain_packet", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_encoder_finish(
    _session: *const c_void,
    _out_packet_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_finish",
        || not_yet_provided("finish", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

/// Host-side `VideoEncoderSessionMethodsVTable`, wired to the reserved
/// stubs. #1259 replaces the stub bodies with the real
/// `SimpleEncoder`-driving logic.
pub static HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE: VideoEncoderSessionMethodsVTable =
    VideoEncoderSessionMethodsVTable {
        layout_version: VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        header: host_video_encoder_header,
        force_idr: host_video_encoder_force_idr,
        submit_frame_nv12: host_video_encoder_submit_frame_nv12,
        submit_texture: host_video_encoder_submit_texture,
        drain_packet: host_video_encoder_drain_packet,
        finish: host_video_encoder_finish,
    };

/// Accessor for the host's static `VideoEncoderSessionMethodsVTable`.
pub fn host_video_encoder_session_methods_vtable() -> *const VideoEncoderSessionMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.video_encoder_session_methods_vtable.is_null() => {
            c.video_encoder_session_methods_vtable
        }
        _ => &HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE,
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
            HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.layout_version,
            VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION
        );
    }

    #[test]
    fn header_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 8];
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.header)(
                std::ptr::null(),
                out.as_mut_ptr(),
                out.len(),
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("header: not yet provided"));
    }

    #[test]
    fn force_idr_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.force_idr)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("force_idr: not yet provided"));
    }

    #[test]
    fn submit_frame_nv12_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_frame_nv12)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("submit_frame_nv12: not yet provided"));
    }

    #[test]
    fn submit_texture_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_texture)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("submit_texture: not yet provided"));
    }

    #[test]
    fn drain_packet_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut meta = VideoEncodedPacketRepr::default();
        let mut data = [0u8; 8];
        let mut data_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.drain_packet)(
                std::ptr::null(),
                0,
                &mut meta,
                data.as_mut_ptr(),
                data.len(),
                &mut data_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("drain_packet: not yet provided"));
    }

    #[test]
    fn finish_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.finish)(
                std::ptr::null(),
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("finish: not yet provided"));
    }
}
