// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VideoDecoderSessionMethodsVTable` bodies + static +
//! accessor (M32 #1259; decoder fill-in #1377).
//!
//! The seven live per-session method slots drive a boxed
//! [`HostVideoDecoderSession`] (a [`SimpleDecoder`] plus its host-side
//! staged-frame buffer). The session Box is minted by the FullAccess
//! `create_decoder_session` slot (`gpu_context/full/reserved_m32.rs`)
//! and reclaimed by `drop_decoder_session`; the per-frame methods here
//! are Limited-only (no re-escalation) and take just the opaque `session`
//! handle. Every body runs under the [`run_host_extern_c`] panic net.
//!
//! Pull model for the variable frame count: `feed` decodes + stages
//! `0..N` frames host-side and reports `N`; the caller then pulls each
//! staged frame's meta + CPU pixel bytes (NV12 or RGBA) via
//! `drain_frame(index)`. Because the decode output path is already
//! engine-free — the decoder reads back CPU pixel bytes into a
//! [`SimpleDecodedFrame`] — `drain_frame` is a pure memcpy of those bytes,
//! reconstructing NO PluginAbiObject. The cached-POD `make_*_borrow`
//! hazard class therefore does not apply to this fill-in; the only
//! make-borrow case on the decoder surface is the still-reserved
//! `decode_into_ring` slot (zero-copy decode into a `TextureRing`), which
//! stays a typed NotYetProvided stub here.
//!
//! Linux-only: [`SimpleDecoder`] lives in the `#[cfg(target_os = "linux")]`
//! `vulkan::video` module. Off-Linux every method returns a typed
//! "not available on this platform" error (no session is ever minted
//! there — `create_decoder_session` refuses off-Linux).

use std::ffi::c_void;

use streamlib_plugin_abi::{
    H273ColorVuiRepr, VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION, VideoDecodedFrameRepr,
    VideoDecoderSessionMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided, write_err};

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::VideoDecoderSessionDescriptorRepr;

#[cfg(target_os = "linux")]
use crate::vulkan::video::decode::{
    DpbOutputMode, SimpleDecodedFrame, SimpleDecoder, SimpleDecoderConfig,
};

// ============================================================================
// Boxed host session (Linux-only)
// ============================================================================

/// The opaque handle behind the `create_decoder_session` slot: a
/// host-owned [`SimpleDecoder`] plus the frames it staged on the last
/// `feed` call, awaiting `drain_frame` pulls. Boxed and handed across the
/// plugin ABI as `Box::into_raw(..) as *const c_void`;
/// `drop_decoder_session` reclaims it (`Box::from_raw` + `SimpleDecoder`
/// Drop, which `wait_idle`s + tears down spec-ordered, host-side).
///
/// Single-owner stateful pipeline (`!Clone`): the parent vtable carries
/// only `drop_decoder_session`, no clone slot.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) struct HostVideoDecoderSession {
    decoder: SimpleDecoder,
    /// Frames staged by the most recent `feed`, pulled by
    /// `drain_frame(index)`. Each `feed` replaces the staging set;
    /// `drain_frame` never re-decodes (pure copy of CPU pixel bytes).
    staged_frames: Vec<SimpleDecodedFrame>,
}

#[cfg(target_os = "linux")]
impl HostVideoDecoderSession {
    /// Wrap a freshly-minted [`SimpleDecoder`] with an empty staging set.
    pub(in crate::core::plugin::host_services) fn new(decoder: SimpleDecoder) -> Self {
        Self {
            decoder,
            staged_frames: Vec::new(),
        }
    }
}

/// Decode the frozen [`VideoDecoderSessionDescriptorRepr`] into a
/// [`SimpleDecoderConfig`]. Rejects an unsupported/reserved codec or an
/// out-of-range DPB output-mode discriminant with a typed error (GPU-free
/// — runs before scope resolution in the create slot). Coded dimensions
/// auto-detect from the first SPS when `max_width` / `max_height` are `0`;
/// there is no bit-depth field (the decoder auto-detects P010/10-bit).
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) fn decoder_config_from_repr(
    repr: &VideoDecoderSessionDescriptorRepr,
) -> Result<SimpleDecoderConfig, String> {
    use super::shared::video_codec_repr::codec_from_repr;

    let codec = codec_from_repr(repr.codec)?;
    let output_mode = match repr.output_mode {
        0 => DpbOutputMode::Coincide,
        1 => DpbOutputMode::Distinct,
        other => return Err(format!("invalid DPB output mode discriminant {other}")),
    };
    Ok(SimpleDecoderConfig {
        codec,
        max_width: repr.max_width,
        max_height: repr.max_height,
        output_mode,
        rgba_output: repr.rgba_output != 0,
    })
}

/// Reconstruct a `&mut HostVideoDecoderSession` from the opaque handle.
/// Returns `None` on a null handle (the caller writes a typed null-handle
/// error). Single-owner contract: the session Box is `!Clone` and driven
/// from one thread, so the `&mut` is never aliased.
///
/// # Safety
///
/// `session` must be a live `Box::into_raw(Box<HostVideoDecoderSession>)`
/// handle (or null) minted by `create_decoder_session` and not yet
/// dropped.
#[cfg(target_os = "linux")]
unsafe fn session_mut<'a>(session: *const c_void) -> Option<&'a mut HostVideoDecoderSession> {
    if session.is_null() {
        return None;
    }
    // SAFETY: per the fn contract `session` is a live boxed session
    // handle; the caller guarantees single-owner (no aliasing) access.
    Some(unsafe { &mut *(session as *mut HostVideoDecoderSession) })
}

// ============================================================================
// Method slot bodies
// ============================================================================

unsafe extern "C" fn host_video_decoder_feed(
    session: *const c_void,
    bitstream_ptr: *const u8,
    bitstream_len: usize,
    out_frame_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_feed",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_frame_count.is_null() {
                    write_err(
                        "feed: null out_frame_count pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err("feed: null session handle", err_buf, err_buf_cap, err_len);
                    return 1;
                };
                // SAFETY: caller-supplied `(bitstream_ptr, bitstream_len)`
                // byte slice, valid for the dispatch.
                let bitstream =
                    unsafe { super::shared::wire::slice_from_raw(bitstream_ptr, bitstream_len) };
                match session.decoder.feed(bitstream) {
                    Ok(frames) => {
                        let count = frames.len() as u32;
                        session.staged_frames = frames;
                        // SAFETY: out_frame_count non-null per the guard.
                        unsafe { *out_frame_count = count };
                        0
                    }
                    Err(e) => {
                        write_err(&format!("feed: {e}"), err_buf, err_buf_cap, err_len);
                        1
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, bitstream_ptr, bitstream_len, out_frame_count);
                write_err(
                    "feed: not available on this platform",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_video_decoder_drain_frame(
    session: *const c_void,
    index: u32,
    out_meta: *mut VideoDecodedFrameRepr,
    out_data_buf: *mut u8,
    out_data_cap: usize,
    out_data_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_drain_frame",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_meta.is_null() || out_data_len.is_null() {
                    write_err(
                        "drain_frame: null out_meta / out_data_len pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err(
                        "drain_frame: null session handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                let staged = session.staged_frames.len();
                let Some(frame) = session.staged_frames.get(index as usize) else {
                    write_err(
                        &format!("drain_frame: index {index} out of range (staged {staged})"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                let pixels = &frame.data;
                let required = pixels.len();
                let meta = VideoDecodedFrameRepr {
                    width: frame.width,
                    height: frame.height,
                    picture_order_count: frame.picture_order_count,
                    pixel_size: required as u32,
                    decode_order: frame.decode_order,
                    is_rgba: u8::from(frame.is_rgba),
                    _pad: [0; 3],
                    // RESERVED: CPU-readback path delivers pixel bytes
                    // inline, never a ring slot. Zero until the zero-copy
                    // `decode_into_ring` slot lands.
                    ring_slot_index: 0,
                };
                // SAFETY: out_meta / out_data_len non-null per the guard.
                // Meta is written before the size check so a meta-only probe
                // (out_data_cap == 0) still reads `pixel_size`.
                unsafe {
                    std::ptr::write(out_meta, meta);
                    *out_data_len = required;
                }
                if required > out_data_cap {
                    // Two-call sizing: caller retries with a `required`-byte buffer.
                    return 2;
                }
                if required > 0 {
                    if out_data_buf.is_null() {
                        write_err(
                            "drain_frame: null out_data_buf pointer",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    // SAFETY: out_data_cap >= required > 0 and out_data_buf non-null.
                    unsafe {
                        std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_data_buf, required)
                    };
                }
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, index, out_meta, out_data_buf, out_data_cap, out_data_len);
                write_err(
                    "drain_frame: not available on this platform",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

unsafe extern "C" fn host_video_decoder_feed_discontinuity(
    session: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_feed_discontinuity",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err(
                        "feed_discontinuity: null session handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                session.decoder.feed_discontinuity();
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = session;
                write_err(
                    "feed_discontinuity: not available on this platform",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

unsafe extern "C" fn host_video_decoder_reset(
    session: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_reset",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err("reset: null session handle", err_buf, err_buf_cap, err_len);
                    return 1;
                };
                session.decoder.reset();
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = session;
                write_err(
                    "reset: not available on this platform",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

unsafe extern "C" fn host_video_decoder_dimensions(
    session: *const c_void,
    out_width: *mut u32,
    out_height: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_dimensions",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_width.is_null() || out_height.is_null() {
                    write_err(
                        "dimensions: null out_width / out_height pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err(
                        "dimensions: null session handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                let (width, height) = session.decoder.dimensions();
                // SAFETY: out_width / out_height non-null per the guard.
                unsafe {
                    *out_width = width;
                    *out_height = height;
                }
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, out_width, out_height);
                write_err(
                    "dimensions: not available on this platform",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

unsafe extern "C" fn host_video_decoder_decode_count(
    session: *const c_void,
    out_count: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_decode_count",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_count.is_null() {
                    write_err(
                        "decode_count: null out_count pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err(
                        "decode_count: null session handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                // SAFETY: out_count non-null per the guard.
                unsafe { *out_count = session.decoder.decode_count() };
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, out_count);
                write_err(
                    "decode_count: not available on this platform",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

unsafe extern "C" fn host_video_decoder_current_color_vui(
    session: *const c_void,
    out_vui: *mut H273ColorVuiRepr,
    out_present: *mut u8,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_decoder_current_color_vui",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_vui.is_null() || out_present.is_null() {
                    write_err(
                        "current_color_vui: null out_vui / out_present pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err(
                        "current_color_vui: null session handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                // `out_present` distinguishes "no SPS VUI parsed yet"
                // (present = 0, repr zeroed) from a real all-axes-absent VUI.
                match session.decoder.current_color_vui() {
                    Some(vui) => {
                        let repr = super::shared::video_codec_repr::h273_color_vui_to_repr(&vui);
                        // SAFETY: out_vui / out_present non-null per the guard.
                        unsafe {
                            std::ptr::write(out_vui, repr);
                            *out_present = 1;
                        }
                    }
                    None => {
                        // SAFETY: out_vui / out_present non-null per the guard.
                        unsafe {
                            std::ptr::write(out_vui, H273ColorVuiRepr::default());
                            *out_present = 0;
                        }
                    }
                }
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, out_vui, out_present);
                write_err(
                    "current_color_vui: not available on this platform",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

/// RESERVED — zero-copy decode into a `TextureRing` PluginAbiObject slot.
/// This is the ONLY make-borrow case on the decoder surface (a decoded
/// frame would be written into a ring-slot texture whose cached POD must
/// be reconstructed via the two-step borrow dance). Deliberately out of
/// the v1 fill-in (#1377); returns a typed NotYetProvided error under the
/// panic net until the zero-copy follow-up lands, leaving the slot pinned
/// at offset 64 without a layout bump.
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

/// Host-side `VideoDecoderSessionMethodsVTable`, wired to the real
/// [`HostVideoDecoderSession`]-driving bodies (#1377). `decode_into_ring`
/// stays the reserved NotYetProvided stub.
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
    //! Tier-1 wire-format tests for the decoder methods vtable.
    //!
    //! GPU-free: the null-handle / null-out-param guards fire before any
    //! session deref or device work. Mental-revert: dropping a guard turns
    //! the matching assertion into a UB deref (SIGSEGV) — e.g. removing the
    //! `out_frame_count.is_null()` check in `feed` UB-writes a `u32`
    //! through a null pointer. `decode_into_ring` stays the reserved
    //! NotYetProvided stub. The GPU-gated positive round-trip lives in
    //! `decoder_session_gpu_gated_tests`.

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
    fn feed_rejects_null_out_count() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.feed)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("feed: null out_frame_count"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn feed_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("feed: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn drain_frame_rejects_null_out_meta() {
        let (mut buf, mut len) = make_err_buf();
        let mut data = [0u8; 8];
        let mut data_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.drain_frame)(
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                data.as_mut_ptr(),
                data.len(),
                &mut data_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("drain_frame: null out_meta"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn drain_frame_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("drain_frame: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn feed_discontinuity_rejects_null_session() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.feed_discontinuity)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("feed_discontinuity: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn reset_rejects_null_session() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.reset)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("reset: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn dimensions_rejects_null_out_params() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.dimensions)(
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("dimensions: null out_width / out_height"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn dimensions_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("dimensions: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn decode_count_rejects_null_out_count() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.decode_count)(
                std::ptr::null(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("decode_count: null out_count"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn decode_count_rejects_null_session() {
        let (mut buf, mut len) = make_err_buf();
        let mut c = 0u64;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.decode_count)(
                std::ptr::null(),
                &mut c,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("decode_count: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn current_color_vui_rejects_null_out_params() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.current_color_vui)(
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(
            err_buf_as_str(&buf, len).contains("current_color_vui: null out_vui / out_present")
        );
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn current_color_vui_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("current_color_vui: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    /// `decode_into_ring` stays the reserved NotYetProvided stub on every
    /// platform (zero-copy decode is the deliberately-deferred follow-up).
    /// Mental-revert: replacing the stub body with `unimplemented!()`
    /// aborts the process instead of returning the typed refusal.
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

    /// Invalid DPB output-mode discriminant is a typed error (GPU-free,
    /// decoded before scope resolution in the create slot). Mental-revert:
    /// widening the match to a silent default swallows the bad discriminant.
    #[cfg(target_os = "linux")]
    #[test]
    fn decoder_config_rejects_invalid_output_mode() {
        let repr = VideoDecoderSessionDescriptorRepr {
            codec: streamlib_plugin_abi::VideoCodecRepr::H264 as u32,
            output_mode: 99,
            ..Default::default()
        };
        let err = decoder_config_from_repr(&repr).expect_err("out-of-range output_mode");
        assert!(
            err.contains("invalid DPB output mode discriminant 99"),
            "got: {err}"
        );
    }

    /// A valid descriptor decodes into the matching config (codec, dims,
    /// output mode, RGBA flag all carried through).
    #[cfg(target_os = "linux")]
    #[test]
    fn decoder_config_decodes_valid_descriptor() {
        let repr = VideoDecoderSessionDescriptorRepr {
            codec: streamlib_plugin_abi::VideoCodecRepr::H265 as u32,
            max_width: 1920,
            max_height: 1080,
            output_mode: 1,
            rgba_output: 1,
            _pad: [0; 3],
        };
        let config = decoder_config_from_repr(&repr).expect("valid descriptor");
        assert_eq!(config.codec, crate::vulkan::video::encode::Codec::H265);
        assert_eq!(config.max_width, 1920);
        assert_eq!(config.max_height, 1080);
        assert_eq!(config.output_mode, DpbOutputMode::Distinct);
        assert!(config.rgba_output);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod decoder_session_gpu_gated_tests {
    //! GPU-gated host-side E2E for the decoder session — the decode half
    //! of `vulkan-video-roundtrip` driven through the new
    //! `GpuContext::create_decoder_session` primitive + the methods
    //! vtable. A short H.264 bitstream is minted in-process by the encoder
    //! sibling (both queues required) and fed through the decoder vtable;
    //! the full separate-build round-trip + PSNR lands with #1265. Skips
    //! cleanly when no Vulkan-Video decode (or encode) device is present
    //! (per `project_ci_strategy_no_gpu`). Local-only.
    //!
    //! Note on the cached-POD borrow hazard class (#988): `drain_frame` is
    //! a pure memcpy of CPU pixel bytes and reconstructs NO PluginAbiObject
    //! borrow, so there is no `make_*_borrow` cached-field regression to
    //! lock in v1. The only make-borrow case on this surface is the
    //! reserved `decode_into_ring` TextureRing path, deferred out of this
    //! fill-in — recorded here explicitly, not silently skipped.

    use super::*;

    use crate::core::context::GpuContext;
    use crate::vulkan::video::decode::SimpleDecoderConfig;
    use crate::vulkan::video::encode::{Codec, Preset, SimpleEncoderConfig};

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    /// Mint a short Annex-B H.264 bitstream (SPS/PPS header + a few gray
    /// NV12 frames + flush) via the encoder primitive on the same device.
    /// Returns `None` when the device lacks a video encode queue.
    fn encode_h264_bitstream(gpu: &GpuContext, width: u32, height: u32) -> Option<Vec<u8>> {
        let config = SimpleEncoderConfig {
            width,
            height,
            fps: 30,
            codec: Codec::H264,
            preset: Preset::Medium,
            ..Default::default()
        };
        let mut encoder = gpu.create_encoder_session(config, false).ok()?;
        let mut stream = encoder.header().to_vec();
        let nv12 = vec![0x80u8; (width * height * 3 / 2) as usize];
        for _ in 0..4 {
            for packet in encoder.submit_frame(&nv12, None).ok()? {
                stream.extend_from_slice(&packet.data);
            }
        }
        for packet in encoder.finish().ok()? {
            stream.extend_from_slice(&packet.data);
        }
        Some(stream)
    }

    #[test]
    fn h264_decode_half_round_trip_through_vtable() {
        let Some(gpu) = GpuContext::init_for_platform().ok() else {
            return;
        };
        let (width, height) = (320u32, 240u32);
        let Some(bitstream) = encode_h264_bitstream(&gpu, width, height) else {
            return;
        };

        let config = SimpleDecoderConfig {
            codec: Codec::H264,
            rgba_output: true,
            ..Default::default()
        };
        let Some(decoder) = gpu.create_decoder_session(config).ok() else {
            return;
        };
        let handle =
            Box::into_raw(Box::new(HostVideoDecoderSession::new(decoder))) as *const c_void;
        struct DropGuard(*const c_void);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = Box::from_raw(self.0 as *mut HostVideoDecoderSession);
                }
            }
        }
        let _guard = DropGuard(handle);

        // Feed the whole Annex-B stream; stages 0..N decoded frames.
        let (mut buf, mut len) = make_err_buf();
        let mut frame_count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.feed)(
                handle,
                bitstream.as_ptr(),
                bitstream.len(),
                &mut frame_count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0, "feed rc: {}", {
            std::str::from_utf8(&buf[..len]).unwrap_or("")
        });
        assert!(frame_count > 0, "decode produced no frames");

        // SPS-detected dimensions must match the encoded stream (proves the
        // real decode path parsed the SPS, not a zeroed default).
        let mut w = 0u32;
        let mut h = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.dimensions)(
                handle,
                &mut w,
                &mut h,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0, "dimensions rc");
        assert_eq!((w, h), (width, height), "SPS dimensions mismatch");

        // Drain every staged frame via two-call sizing (meta-only probe →
        // status 2 with the size → fill). RGBA output => W*H*4 bytes.
        for index in 0..frame_count {
            let mut meta = VideoDecodedFrameRepr::default();
            let mut data_len = 0usize;
            let rc = unsafe {
                (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.drain_frame)(
                    handle,
                    index,
                    &mut meta,
                    std::ptr::null_mut(),
                    0,
                    &mut data_len,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut len,
                )
            };
            assert!(rc == 0 || rc == 2, "drain probe rc {rc}");
            assert_eq!(meta.pixel_size as usize, data_len);
            let mut out = vec![0u8; data_len];
            let mut out_len = 0usize;
            let rc = unsafe {
                (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.drain_frame)(
                    handle,
                    index,
                    &mut meta,
                    out.as_mut_ptr(),
                    out.len(),
                    &mut out_len,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut len,
                )
            };
            assert_eq!(rc, 0, "drain fill rc");
            assert_eq!(out_len, data_len);
            let expected = if meta.is_rgba != 0 {
                (meta.width * meta.height * 4) as usize
            } else {
                (meta.width * meta.height * 3 / 2) as usize
            };
            assert_eq!(out_len, expected, "decoded pixel byte count mismatch");
        }

        // decode_count reports at least the frames we drained.
        let mut decode_count = 0u64;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.decode_count)(
                handle,
                &mut decode_count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0, "decode_count rc");
        assert!(decode_count >= frame_count as u64);

        // Out-of-range drain index is caller misuse → typed error.
        let mut meta = VideoDecodedFrameRepr::default();
        let mut data_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_DECODER_SESSION_METHODS_VTABLE.drain_frame)(
                handle,
                9999,
                &mut meta,
                std::ptr::null_mut(),
                0,
                &mut data_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1, "out-of-range index must be a typed error");
        assert!(
            std::str::from_utf8(&buf[..len])
                .unwrap_or("")
                .contains("out of range")
        );
    }
}
