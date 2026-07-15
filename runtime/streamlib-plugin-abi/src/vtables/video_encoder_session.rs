// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VideoEncoderSessionMethodsVTable` — per-encoder-session extern "C"
//! dispatch for the hardware video encode surface (#1259).

use core::ffi::c_void;

use crate::repr::VideoEncodedPacketRepr;

/// Layout version of [`crate::VideoEncoderSessionMethodsVTable`].
///
/// - v1: initial shape — `header`, `force_idr`, `submit_frame_nv12`,
///   `submit_texture`, `drain_packet`, `finish`. Drop-only (`!Clone`):
///   the parent [`crate::GpuContextFullAccessVTable`] carries
///   `drop_encoder_session`, no clone slot (the Box-shaped
///   `SimpleEncoder` session is a single-owner stateful pipeline).
pub const VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Per-encoder-session method-dispatch table. Every slot takes only the
/// opaque `session` handle (`Box::into_raw(Box<SimpleEncoder>)`) — the
/// session was minted under an escalate scope; per-frame driving is
/// Limited-only (no re-escalation). Pull model for variable packet
/// count: `submit_*` / `finish` stage `0..N` packets host-side and
/// report `N`; the caller then pulls each via `drain_packet(index)`.
///
/// # Return-code convention
///
/// `0` = success; non-zero = error with a UTF-8 message in `err_buf`;
/// `2` = buffer-too-small (a distinguished retry signal with the
/// required length in the relevant `out_*_len`). Any null handle / null
/// out-param is a typed error, never a segfault.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump
/// [`VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct VideoEncoderSessionMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (zero today, never read).
    pub _reserved_padding: u32,

    /// Cached SPS/PPS (H.264) or VPS/SPS/PPS (H.265) bytes. Two-call
    /// sizing (`status 2` = `out_buf` too small, required length in
    /// `out_len`, retry).
    pub header: unsafe extern "C" fn(
        session: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Force the next submitted frame to encode as an IDR keyframe.
    pub force_idr: unsafe extern "C" fn(
        session: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// NV12 CPU path (expects `width*height*3/2` bytes). Stages `0..N`
    /// packets, writes `N` into `*out_packet_count`.
    pub submit_frame_nv12: unsafe extern "C" fn(
        session: *const c_void,
        nv12_ptr: *const u8,
        nv12_len: usize,
        has_timestamp: u8,
        timestamp_ns: i64,
        out_packet_count: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// GPU-resident encode path. `texture_handle` is a `Texture`
    /// PluginAbiObject handle; the host resolves the encode-src view
    /// host-side. `input_layout` is the raw `VkImageLayout` the session
    /// transitions from (SDK twin types it as `VulkanLayout`). Stages
    /// `0..N` packets, writes `N`.
    pub submit_texture: unsafe extern "C" fn(
        session: *const c_void,
        texture_handle: *const c_void,
        input_layout: i32,
        has_timestamp: u8,
        timestamp_ns: i64,
        out_packet_count: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Pull staged packet `[index]`, index in `[0, count)`. Writes meta
    /// plus bitstream bytes. Pure copy, no re-encode (safe two-call
    /// sizing; `status 2` = `out_data_buf` too small). Out-of-range
    /// `index` is caller misuse → typed error.
    pub drain_packet: unsafe extern "C" fn(
        session: *const c_void,
        index: u32,
        out_meta: *mut VideoEncodedPacketRepr,
        out_data_buf: *mut u8,
        out_data_cap: usize,
        out_data_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Flush the B-frame reorder buffer / end-of-stream, stage trailing
    /// packets, write `N`; caller pulls via `drain_packet`.
    pub finish: unsafe extern "C" fn(
        session: *const c_void,
        out_packet_count: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VideoEncoderSessionMethodsVTable {}
unsafe impl Sync for VideoEncoderSessionMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn video_encoder_session_methods_vtable_layout() {
        // 8-byte header + 6 fn pointers = 8 + 48 = 56 bytes, align 8.
        assert_eq!(size_of::<VideoEncoderSessionMethodsVTable>(), 56);
        assert_eq!(align_of::<VideoEncoderSessionMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VideoEncoderSessionMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(offset_of!(VideoEncoderSessionMethodsVTable, header), 8);
        assert_eq!(offset_of!(VideoEncoderSessionMethodsVTable, force_idr), 16);
        assert_eq!(
            offset_of!(VideoEncoderSessionMethodsVTable, submit_frame_nv12),
            24
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionMethodsVTable, submit_texture),
            32
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionMethodsVTable, drain_packet),
            40
        );
        assert_eq!(offset_of!(VideoEncoderSessionMethodsVTable, finish), 48);
    }
}
