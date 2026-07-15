// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VideoDecoderSessionMethodsVTable` static + accessor (M32
//! reservation, #1259).
//!
//! Every slot ships a typed NotYetProvided-style stub under the panic
//! net until the hardware-video fill-in (#1259) lands the real bodies —
//! including the reserved `decode_into_ring` (zero-copy GPU output).

use std::ffi::c_void;

use streamlib_plugin_abi::{
    H273ColorVuiRepr, VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION, VideoDecodedFrameRepr,
    VideoDecoderSessionMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided};

unsafe extern "C" fn host_video_decoder_feed(
    _session: *const c_void,
    _bitstream_ptr: *const u8,
    _bitstream_len: usize,
    _out_frame_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_feed",
        || not_yet_provided("feed", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_video_decoder_drain_frame(
    _session: *const c_void,
    _index: u32,
    _out_meta: *mut VideoDecodedFrameRepr,
    _out_data_buf: *mut u8,
    _out_data_cap: usize,
    _out_data_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_drain_frame",
        || not_yet_provided("drain_frame", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_decoder_feed_discontinuity(
    _session: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_feed_discontinuity",
        || not_yet_provided("feed_discontinuity", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_decoder_reset(
    _session: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_reset",
        || not_yet_provided("reset", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_decoder_dimensions(
    _session: *const c_void,
    _out_width: *mut u32,
    _out_height: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_dimensions",
        || not_yet_provided("dimensions", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_decoder_decode_count(
    _session: *const c_void,
    _out_count: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_decode_count",
        || not_yet_provided("decode_count", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_decoder_current_color_vui(
    _session: *const c_void,
    _out_vui: *mut H273ColorVuiRepr,
    _out_present: *mut u8,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_current_color_vui",
        || not_yet_provided("current_color_vui", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

unsafe extern "C" fn host_video_decoder_decode_into_ring(
    _session: *const c_void,
    _ring_handle: *const c_void,
    _out_frame_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_decode_into_ring",
        || not_yet_provided("decode_into_ring", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

/// Host-side `VideoDecoderSessionMethodsVTable`, wired to the reserved
/// stubs. #1259 replaces the stub bodies with the real
/// `SimpleDecoder`-driving logic.
pub static HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE: VideoDecoderSessionMethodsVTable =
    VideoDecoderSessionMethodsVTable {
        layout_version: VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        feed: host_video_decoder_feed,
        drain_frame: host_video_decoder_drain_frame,
        feed_discontinuity: host_video_decoder_feed_discontinuity,
        reset: host_video_decoder_reset,
        dimensions: host_video_decoder_dimensions,
        decode_count: host_video_decoder_decode_count,
        current_color_vui: host_video_decoder_current_color_vui,
        decode_into_ring: host_video_decoder_decode_into_ring,
    };

/// Accessor for the host's static `VideoDecoderSessionMethodsVTable`.
pub fn host_video_decoder_session_methods_vtable() -> *const VideoDecoderSessionMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.video_decoder_session_methods_vtable.is_null() => {
            c.video_decoder_session_methods_vtable
        }
        _ => &HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE,
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
            HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.layout_version,
            VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION
        );
    }

    #[test]
    fn feed_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.feed)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("feed: not yet provided"));
    }

    #[test]
    fn drain_frame_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut meta = VideoDecodedFrameRepr::default();
        let mut data = [0u8; 8];
        let mut data_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.drain_frame)(
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
        assert!(err_buf_as_str(&buf, len).contains("drain_frame: not yet provided"));
    }

    #[test]
    fn dimensions_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut w = 0u32;
        let mut h = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.dimensions)(
                std::ptr::null(),
                &mut w,
                &mut h,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("dimensions: not yet provided"));
    }

    #[test]
    fn current_color_vui_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut vui = H273ColorVuiRepr::default();
        let mut present = 0u8;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.current_color_vui)(
                std::ptr::null(),
                &mut vui,
                &mut present,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("current_color_vui: not yet provided"));
    }

    #[test]
    fn decode_into_ring_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.decode_into_ring)(
                std::ptr::null(),
                std::ptr::null(),
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("decode_into_ring: not yet provided"));
    }
}
