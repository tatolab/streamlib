// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VideoDecoderSessionMethodsVTable` — per-decoder-session extern "C"
//! dispatch for the hardware video decode surface (#1259).

use core::ffi::c_void;

use crate::repr::{H273ColorVuiRepr, VideoDecodedFrameRepr};

/// Layout version of [`crate::VideoDecoderSessionMethodsVTable`].
///
/// - v1: initial shape — `feed`, `drain_frame`, `feed_discontinuity`,
///   `reset`, `dimensions`, `decode_count`, `current_color_vui`, plus
///   the reserved `decode_into_ring` (zero-copy decode into a
///   `TextureRing` slot; body returns a typed NotYetProvided-style
///   error until landed). Drop-only (`!Clone`): the parent
///   [`crate::GpuContextFullAccessVTable`] carries `drop_decoder_session`,
///   no clone slot (the Box-shaped `SimpleDecoder` session is a
///   single-owner stateful pipeline).
pub const VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Per-decoder-session method-dispatch table. Every slot takes only the
/// opaque `session` handle (`Box::into_raw(Box<SimpleDecoder>)`). Same
/// pull model as the encoder: `feed` stages `0..N` decoded frames and
/// reports `N`; the caller pulls each via `drain_frame(index)`.
///
/// # Return-code convention
///
/// `0` = success; non-zero = error; `2` = buffer-too-small (retry with
/// the required length in `out_data_len`). Null handle / null out-param
/// is a typed error.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump
/// [`VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct VideoDecoderSessionMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (zero today, never read).
    pub _reserved_padding: u32,

    /// Feed Annex-B bytes, stage `0..N` decoded frames, write `N`.
    pub feed: unsafe extern "C" fn(
        session: *const c_void,
        bitstream_ptr: *const u8,
        bitstream_len: usize,
        out_frame_count: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Pull decoded frame `[index]`. Writes meta + pixel bytes (NV12
    /// `W*H*3/2` or RGBA `W*H*4` per `out_meta.is_rgba`). `status 2` =
    /// `out_data_buf` too small. A ring-resident frame (once
    /// `decode_into_ring` lands) delivers meta with `pixel_size = 0` +
    /// `ring_slot_index` set, no pixel bytes.
    pub drain_frame: unsafe extern "C" fn(
        session: *const c_void,
        index: u32,
        out_meta: *mut VideoDecodedFrameRepr,
        out_data_buf: *mut u8,
        out_data_cap: usize,
        out_data_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Signal a decode discontinuity (seek / gap).
    pub feed_discontinuity: unsafe extern "C" fn(
        session: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Reset the decoder session state.
    pub reset: unsafe extern "C" fn(
        session: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// SPS-detected coded dimensions (valid after the first frame is
    /// produced).
    pub dimensions: unsafe extern "C" fn(
        session: *const c_void,
        out_width: *mut u32,
        out_height: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Total frames decoded so far.
    pub decode_count: unsafe extern "C" fn(
        session: *const c_void,
        out_count: *mut u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Parsed SPS VUI. `out_present` distinguishes no-VUI-yet from
    /// all-Unspecified.
    pub current_color_vui: unsafe extern "C" fn(
        session: *const c_void,
        out_vui: *mut H273ColorVuiRepr,
        out_present: *mut u8,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// RESERVED — zero-copy decode into an existing Arc-shaped
    /// `TextureRing` PluginAbiObject slot (no CPU readback). Body returns
    /// a typed NotYetProvided-style error under the panic net until the
    /// surface fill-in lands.
    pub decode_into_ring: unsafe extern "C" fn(
        session: *const c_void,
        ring_handle: *const c_void,
        out_frame_count: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VideoDecoderSessionMethodsVTable {}
unsafe impl Sync for VideoDecoderSessionMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn video_decoder_session_methods_vtable_layout() {
        // 8-byte header + 8 fn pointers = 8 + 64 = 72 bytes, align 8.
        assert_eq!(size_of::<VideoDecoderSessionMethodsVTable>(), 72);
        assert_eq!(align_of::<VideoDecoderSessionMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VideoDecoderSessionMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VideoDecoderSessionMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(offset_of!(VideoDecoderSessionMethodsVTable, feed), 8);
        assert_eq!(offset_of!(VideoDecoderSessionMethodsVTable, drain_frame), 16);
        assert_eq!(
            offset_of!(VideoDecoderSessionMethodsVTable, feed_discontinuity),
            24
        );
        assert_eq!(offset_of!(VideoDecoderSessionMethodsVTable, reset), 32);
        assert_eq!(offset_of!(VideoDecoderSessionMethodsVTable, dimensions), 40);
        assert_eq!(
            offset_of!(VideoDecoderSessionMethodsVTable, decode_count),
            48
        );
        assert_eq!(
            offset_of!(VideoDecoderSessionMethodsVTable, current_color_vui),
            56
        );
        assert_eq!(
            offset_of!(VideoDecoderSessionMethodsVTable, decode_into_ring),
            64
        );
    }
}
