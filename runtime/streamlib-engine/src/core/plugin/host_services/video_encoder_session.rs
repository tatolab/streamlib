// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VideoEncoderSessionMethodsVTable` bodies + static +
//! accessor (M32 #1259; encoder fill-in #1376).
//!
//! The six per-session method slots drive a boxed
//! [`HostVideoEncoderSession`] (a [`SimpleEncoder`] plus its host-side
//! staged-packet buffer). The session Box is minted by the FullAccess
//! `create_encoder_session` slot (`gpu_context/full/reserved_m32.rs`)
//! and reclaimed by `drop_encoder_session`; the per-frame methods here
//! are Limited-only (no re-escalation) and take just the opaque `session`
//! handle. Every body runs under the [`run_host_extern_c`] panic net.
//!
//! Pull model for the variable packet count: `submit_frame_nv12` /
//! `submit_texture` / `finish` encode + stage `0..N` packets host-side
//! and report `N`; the caller then pulls each staged packet's meta +
//! bitstream via `drain_packet(index)`.
//!
//! Linux-only: [`SimpleEncoder`] lives in the `#[cfg(target_os = "linux")]`
//! `vulkan::video` module. Off-Linux every method returns a typed
//! "not available on this platform" error (no session is ever minted
//! there — `create_encoder_session` refuses off-Linux).

use std::ffi::c_void;

use streamlib_plugin_abi::{
    VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION, VideoEncodedPacketRepr,
    VideoEncoderSessionMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::write_err;

#[cfg(target_os = "linux")]
use streamlib_plugin_abi::{VideoEncoderSessionDescriptorRepr, VideoFrameTypeRepr};

#[cfg(target_os = "linux")]
use crate::vulkan::video::encode::{EncodePacket, FrameType, SimpleEncoder, SimpleEncoderConfig};

// ============================================================================
// Boxed host session (Linux-only)
// ============================================================================

/// The opaque handle behind the `create_encoder_session` slot: a
/// host-owned [`SimpleEncoder`] plus the packets it staged on the last
/// `submit_*` / `finish` call, awaiting `drain_packet` pulls. Boxed and
/// handed across the plugin ABI as `Box::into_raw(..) as *const c_void`;
/// `drop_encoder_session` reclaims it (`Box::from_raw` + `SimpleEncoder`
/// Drop, which `wait_idle`s + tears down spec-ordered, host-side).
///
/// Single-owner stateful pipeline (`!Clone`): the parent vtable carries
/// only `drop_encoder_session`, no clone slot.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) struct HostVideoEncoderSession {
    encoder: SimpleEncoder,
    /// Packets staged by the most recent `submit_*` / `finish`, pulled by
    /// `drain_packet(index)`. Each `submit_*` / `finish` replaces the
    /// staging set; `drain_packet` never re-encodes (pure copy).
    staged_packets: Vec<EncodePacket>,
}

#[cfg(target_os = "linux")]
impl HostVideoEncoderSession {
    /// Wrap a freshly-minted [`SimpleEncoder`] with an empty staging set.
    pub(in crate::core::plugin::host_services) fn new(encoder: SimpleEncoder) -> Self {
        Self {
            encoder,
            staged_packets: Vec::new(),
        }
    }
}

/// Decode the frozen [`VideoEncoderSessionDescriptorRepr`] into a
/// [`SimpleEncoderConfig`]. Rejects an unsupported/reserved codec or
/// preset discriminant with a typed error (GPU-free — runs before scope
/// resolution in the create slot). The reserved descriptor band
/// (`max_bitrate_bps`, `rate_control_mode`, `luma_bit_depth`,
/// `chroma_subsampling`) is carried internally by `EncodeConfig`'s
/// defaults and not read here.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) fn encoder_config_from_repr(
    repr: &VideoEncoderSessionDescriptorRepr,
) -> Result<SimpleEncoderConfig, String> {
    use super::shared::video_codec_repr::{codec_from_repr, h273_color_vui_from_repr, preset_from_repr};

    let codec = codec_from_repr(repr.codec)?;
    let preset = preset_from_repr(repr.preset)?;

    let color_vui = h273_color_vui_from_repr(&repr.color_vui);
    // An all-None VUI is equivalent to "no colour_description block"; keep
    // it `None` so the encoder emits no VUI rather than an empty one.
    let color_vui = color_vui
        .is_video_signal_type_block_needed()
        .then_some(color_vui);

    Ok(SimpleEncoderConfig {
        width: repr.width,
        height: repr.height,
        fps: repr.fps,
        codec,
        preset,
        qp: (repr.has_qp != 0).then_some(repr.qp),
        bitrate_bps: (repr.has_bitrate != 0).then_some(repr.bitrate_bps),
        streaming: repr.streaming != 0,
        idr_interval_secs: repr.idr_interval_secs,
        prepend_header_to_idr: (repr.prepend_header_present != 0).then_some(repr.prepend_header != 0),
        effort_level: (repr.has_effort_level != 0).then_some(repr.effort_level),
        color_vui,
    })
}

/// Reconstruct a `&mut HostVideoEncoderSession` from the opaque handle.
/// Returns `None` on a null handle (the caller writes a typed
/// null-handle error). Single-owner contract: the session Box is `!Clone`
/// and driven from one thread, so the `&mut` is never aliased.
///
/// # Safety
///
/// `session` must be a live `Box::into_raw(Box<HostVideoEncoderSession>)`
/// handle (or null) minted by `create_encoder_session` and not yet
/// dropped.
#[cfg(target_os = "linux")]
unsafe fn session_mut<'a>(session: *const c_void) -> Option<&'a mut HostVideoEncoderSession> {
    if session.is_null() {
        return None;
    }
    // SAFETY: per the fn contract `session` is a live boxed session
    // handle; the caller guarantees single-owner (no aliasing) access.
    Some(unsafe { &mut *(session as *mut HostVideoEncoderSession) })
}

/// Map an engine [`FrameType`] to the frozen [`VideoFrameTypeRepr`]
/// discriminant for `drain_packet`'s `out_meta`.
#[cfg(target_os = "linux")]
fn frame_type_to_repr(frame_type: FrameType) -> u32 {
    match frame_type {
        FrameType::Idr => VideoFrameTypeRepr::Idr as u32,
        FrameType::I => VideoFrameTypeRepr::I as u32,
        FrameType::P => VideoFrameTypeRepr::P as u32,
        FrameType::B => VideoFrameTypeRepr::B as u32,
    }
}

// ============================================================================
// Method slot bodies
// ============================================================================

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_video_encoder_header(
    session: *const c_void,
    out_buf: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_header",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_len.is_null() {
                    write_err("header: null out_len pointer", err_buf, err_buf_cap, err_len);
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err("header: null session handle", err_buf, err_buf_cap, err_len);
                    return 1;
                };
                let header = session.encoder.header();
                let required = header.len();
                // SAFETY: out_len non-null per the guard.
                unsafe { *out_len = required };
                if required > out_cap {
                    // Two-call sizing: caller retries with a `required`-byte buffer.
                    return 2;
                }
                if required > 0 {
                    if out_buf.is_null() {
                        write_err("header: null out_buf pointer", err_buf, err_buf_cap, err_len);
                        return 1;
                    }
                    // SAFETY: out_cap >= required > 0 and out_buf non-null.
                    unsafe { std::ptr::copy_nonoverlapping(header.as_ptr(), out_buf, required) };
                }
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, out_buf, out_cap, out_len);
                write_err(
                    "header: not available on this platform",
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

unsafe extern "C" fn host_video_encoder_force_idr(
    session: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_force_idr",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err("force_idr: null session handle", err_buf, err_buf_cap, err_len);
                    return 1;
                };
                session.encoder.force_idr();
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = session;
                write_err(
                    "force_idr: not available on this platform",
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
unsafe extern "C" fn host_video_encoder_submit_frame_nv12(
    session: *const c_void,
    nv12_ptr: *const u8,
    nv12_len: usize,
    has_timestamp: u8,
    timestamp_ns: i64,
    out_packet_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_submit_frame_nv12",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_packet_count.is_null() {
                    write_err(
                        "submit_frame_nv12: null out_packet_count pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err(
                        "submit_frame_nv12: null session handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                // SAFETY: caller-supplied `(nv12_ptr, nv12_len)` byte slice,
                // valid for the dispatch.
                let nv12 = unsafe { super::shared::wire::slice_from_raw(nv12_ptr, nv12_len) };
                let timestamp = (has_timestamp != 0).then_some(timestamp_ns);
                match session.encoder.submit_frame(nv12, timestamp) {
                    Ok(packets) => {
                        let count = packets.len() as u32;
                        session.staged_packets = packets;
                        // SAFETY: out_packet_count non-null per the guard.
                        unsafe { *out_packet_count = count };
                        0
                    }
                    Err(e) => {
                        write_err(
                            &format!("submit_frame_nv12: {e}"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        1
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, nv12_ptr, nv12_len, has_timestamp, timestamp_ns, out_packet_count);
                write_err(
                    "submit_frame_nv12: not available on this platform",
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
unsafe extern "C" fn host_video_encoder_submit_texture(
    session: *const c_void,
    texture_handle: *const c_void,
    input_layout: i32,
    has_timestamp: u8,
    timestamp_ns: i64,
    out_packet_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_submit_texture",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_packet_count.is_null() {
                    write_err(
                        "submit_texture: null out_packet_count pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                if texture_handle.is_null() {
                    write_err(
                        "submit_texture: null texture_handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // The RGB->NV12 converter (`encode_image`) requires the
                // source in SHADER_READ_ONLY_OPTIMAL. Reject any other
                // input layout with a typed error rather than silently
                // encoding from a wrong layout (transitioning from an
                // arbitrary layout is a follow-up on the encode path).
                if input_layout != streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0 {
                    write_err(
                        &format!(
                            "submit_texture: input_layout {input_layout} unsupported \
                             (encoder requires SHADER_READ_ONLY_OPTIMAL = {})",
                            streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner. Held
                // across the borrow below (no aliasing — one thread).
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err(
                        "submit_texture: null session handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                // Reconstruct a borrowed source Texture from the inner-Arc
                // handle and resolve its encode-src image view host-side.
                // `make_texture_borrow` runs the two-step dance so the
                // borrow's cached POD (width/height/format) mirror the real
                // inner — never a zeroed borrow (the
                // cdylib-make-borrow-cached-fields hazard class, #988).
                use crate::host_rhi::HostTextureExt;
                let texture = super::shared::borrow::make_texture_borrow(texture_handle);
                // Aligned-extent precondition. The RGB->NV12 converter
                // samples the source view over the encoder's codec-aligned
                // extent (`rgb_to_nv12` in encode/staging.rs); `encode_image`
                // receives only a bare `vk::ImageView` and cannot self-
                // validate. A source smaller than the aligned extent would
                // silently encode clamped / garbage NV12 with no downstream
                // complaint. Reject undersize with a typed error here (this
                // ABI enforcement point also covers the in-process
                // `encode_image` path) — mirroring the SDK contract that
                // RGBA input "must be at least these dimensions".
                let (aligned_width, aligned_height) = session.encoder.aligned_extent();
                if texture.width() < aligned_width || texture.height() < aligned_height {
                    write_err(
                        &format!(
                            "submit_texture: source {}x{} smaller than required aligned \
                             extent {}x{}",
                            texture.width(),
                            texture.height(),
                            aligned_width,
                            aligned_height
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                let image_view = match texture.vulkan_inner().image_view() {
                    Ok(view) => view,
                    Err(e) => {
                        write_err(
                            &format!("submit_texture: resolve image view: {e}"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                };
                let timestamp = (has_timestamp != 0).then_some(timestamp_ns);
                match session.encoder.encode_image(image_view, timestamp) {
                    Ok(packets) => {
                        let count = packets.len() as u32;
                        session.staged_packets = packets;
                        // SAFETY: out_packet_count non-null per the guard.
                        unsafe { *out_packet_count = count };
                        0
                    }
                    Err(e) => {
                        write_err(
                            &format!("submit_texture: {e}"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        1
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (
                    session,
                    texture_handle,
                    input_layout,
                    has_timestamp,
                    timestamp_ns,
                    out_packet_count,
                );
                write_err(
                    "submit_texture: not available on this platform",
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
unsafe extern "C" fn host_video_encoder_drain_packet(
    session: *const c_void,
    index: u32,
    out_meta: *mut VideoEncodedPacketRepr,
    out_data_buf: *mut u8,
    out_data_cap: usize,
    out_data_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_drain_packet",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_meta.is_null() || out_data_len.is_null() {
                    write_err(
                        "drain_packet: null out_meta / out_data_len pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err("drain_packet: null session handle", err_buf, err_buf_cap, err_len);
                    return 1;
                };
                let staged = session.staged_packets.len();
                let Some(packet) = session.staged_packets.get(index as usize) else {
                    write_err(
                        &format!(
                            "drain_packet: index {index} out of range (staged {staged})"
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                };
                let bitstream = &packet.data;
                let required = bitstream.len();
                let meta = VideoEncodedPacketRepr {
                    frame_type: frame_type_to_repr(packet.frame_type),
                    is_keyframe: u8::from(packet.is_keyframe),
                    has_timestamp: u8::from(packet.timestamp_ns.is_some()),
                    _pad0: [0; 2],
                    pts: packet.pts,
                    timestamp_ns: packet.timestamp_ns.unwrap_or(0),
                    bitstream_size: required as u32,
                    _reserved: 0,
                };
                // SAFETY: out_meta / out_data_len non-null per the guard.
                // Meta is written before the size check so a meta-only
                // probe (out_data_cap == 0) still reads `bitstream_size`.
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
                            "drain_packet: null out_data_buf pointer",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    // SAFETY: out_data_cap >= required > 0 and out_data_buf non-null.
                    unsafe {
                        std::ptr::copy_nonoverlapping(bitstream.as_ptr(), out_data_buf, required)
                    };
                }
                0
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, index, out_meta, out_data_buf, out_data_cap, out_data_len);
                write_err(
                    "drain_packet: not available on this platform",
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

unsafe extern "C" fn host_video_encoder_finish(
    session: *const c_void,
    out_packet_count: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_video_encoder_finish",
        || -> i32 {
            #[cfg(target_os = "linux")]
            {
                if out_packet_count.is_null() {
                    write_err(
                        "finish: null out_packet_count pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                // SAFETY: caller-owned session handle; single-owner.
                let Some(session) = (unsafe { session_mut(session) }) else {
                    write_err("finish: null session handle", err_buf, err_buf_cap, err_len);
                    return 1;
                };
                match session.encoder.finish() {
                    Ok(packets) => {
                        let count = packets.len() as u32;
                        session.staged_packets = packets;
                        // SAFETY: out_packet_count non-null per the guard.
                        unsafe { *out_packet_count = count };
                        0
                    }
                    Err(e) => {
                        write_err(&format!("finish: {e}"), err_buf, err_buf_cap, err_len);
                        1
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (session, out_packet_count);
                write_err(
                    "finish: not available on this platform",
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

/// Host-side `VideoEncoderSessionMethodsVTable`, wired to the real
/// [`HostVideoEncoderSession`]-driving bodies (#1376).
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
    //! Tier-1 wire-format tests for the encoder methods vtable.
    //!
    //! GPU-free: the null-handle / null-out-param / null-input guards
    //! fire before any session deref or device work. Mental-revert:
    //! dropping a guard turns the matching assertion into a UB deref
    //! (SIGSEGV) — e.g. removing the `out_packet_count.is_null()` check in
    //! `submit_frame_nv12` UB-writes a `u32` through a null pointer. The
    //! GPU-gated positive round-trip lives in
    //! `encoder_session_gpu_gated_tests`.

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
    fn header_rejects_null_out_len() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 8];
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.header)(
                std::ptr::null(),
                out.as_mut_ptr(),
                out.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("header: null out_len"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn header_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("header: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn force_idr_rejects_null_session() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.force_idr)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("force_idr: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn submit_frame_nv12_rejects_null_out_count() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_frame_nv12)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("submit_frame_nv12: null out_packet_count"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn submit_frame_nv12_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("submit_frame_nv12: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn submit_texture_rejects_null_out_count() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_texture)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("submit_texture: null out_packet_count"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn submit_texture_rejects_null_texture_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        // Non-null out_count so the null-texture-handle guard (not the
        // out-count guard) is what fires; null session never deref'd
        // because texture_handle is checked first.
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("submit_texture: null texture_handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn submit_texture_rejects_non_shader_read_only_layout() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        // Non-null out_count + non-null dummy texture_handle so the layout
        // guard fires before the session deref (the dummy handle is never
        // dereferenced — the layout check precedes `session_mut`).
        let dummy_texture = 0x1usize as *const c_void;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_texture)(
                std::ptr::null(),
                dummy_texture,
                streamlib_consumer_rhi::VulkanLayout::GENERAL.0, // wrong layout
                0,
                0,
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("submit_texture: input_layout"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn submit_texture_rejects_null_session_after_pointer_guards() {
        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        // All pointer + layout guards pass (non-null out_count, non-null
        // dummy texture, correct layout) so the null-session guard is what
        // fires — the dummy texture is never dereferenced because
        // `session_mut(null)` short-circuits before `make_texture_borrow`.
        let dummy_texture = 0x1usize as *const c_void;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_texture)(
                std::ptr::null(),
                dummy_texture,
                streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0,
                0,
                0,
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("submit_texture: null session handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn drain_packet_rejects_null_out_meta() {
        let (mut buf, mut len) = make_err_buf();
        let mut data = [0u8; 8];
        let mut data_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.drain_packet)(
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
        assert!(err_buf_as_str(&buf, len).contains("drain_packet: null out_meta"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn drain_packet_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("drain_packet: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn finish_rejects_null_out_count() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.finish)(
                std::ptr::null(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("finish: null out_packet_count"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }

    #[test]
    fn finish_rejects_null_session() {
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
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("finish: null session handle"));
        #[cfg(not(target_os = "linux"))]
        assert!(err_buf_as_str(&buf, len).contains("not available on this platform"));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod encoder_session_gpu_gated_tests {
    //! GPU-gated host-side E2E for the encoder session — the encode half
    //! of `vulkan-video-roundtrip` driven through the new
    //! `GpuContext::create_encoder_session` primitive + the methods
    //! vtable. Skips cleanly when no Vulkan-Video encode device is present
    //! (per `project_ci_strategy_no_gpu`). Local-only.

    use super::*;
    use crate::vulkan::video::encode::{Codec, Preset, SimpleEncoderConfig};

    fn try_encoder_session() -> Option<Box<HostVideoEncoderSession>> {
        let gpu = crate::core::context::GpuContext::init_for_platform().ok()?;
        let config = SimpleEncoderConfig {
            width: 320,
            height: 240,
            fps: 30,
            codec: Codec::H264,
            preset: Preset::Medium,
            ..Default::default()
        };
        // NV12 path only — skip the GPU-input pre-alloc.
        let encoder = gpu.create_encoder_session(config, false).ok()?;
        Some(Box::new(HostVideoEncoderSession::new(encoder)))
    }

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    #[test]
    fn nv12_encode_drain_finish_round_trip_through_vtable() {
        let Some(session) = try_encoder_session() else {
            return;
        };
        let handle = Box::into_raw(session) as *const c_void;
        // Reclaim through the drop slot at the end (single-owner Box).
        struct DropGuard(*const c_void);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = Box::from_raw(self.0 as *mut HostVideoEncoderSession);
                }
            }
        }
        let _guard = DropGuard(handle);

        // header() must return non-empty SPS/PPS.
        let (mut buf, mut len) = make_err_buf();
        let mut header = [0u8; 512];
        let mut header_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.header)(
                handle,
                header.as_mut_ptr(),
                header.len(),
                &mut header_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0, "header rc");
        assert!(header_len > 0, "SPS/PPS header must be non-empty");

        // Drain every packet staged by a submit / finish call via two-call
        // sizing (meta-only probe → status 2 with the size → fill). Returns
        // the total drained bitstream byte count. The pull protocol: each
        // submit / finish REPLACES the staging set, so all `count` packets
        // must be drained before the next submit / finish call.
        let drain_all = |handle: *const c_void, count: u32| -> usize {
            let mut buf = [0u8; 256];
            let mut len = 0usize;
            let mut total_bytes = 0usize;
            for index in 0..count {
                let mut meta = VideoEncodedPacketRepr::default();
                let mut data_len = 0usize;
                // Meta-only probe (zero-cap) → status 2 with the size (or 0
                // for an empty packet).
                let rc = unsafe {
                    (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.drain_packet)(
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
                assert_eq!(meta.bitstream_size as usize, data_len);
                let mut out = vec![0u8; data_len];
                let mut out_len = 0usize;
                let rc = unsafe {
                    (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.drain_packet)(
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
                total_bytes += out_len;
            }
            total_bytes
        };

        // Submit one gray NV12 frame, then drain its packets BEFORE finish
        // (finish replaces the staging set).
        let nv12 = vec![0x80u8; 320 * 240 * 3 / 2];
        let mut count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_frame_nv12)(
                handle,
                nv12.as_ptr(),
                nv12.len(),
                1,
                0,
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0, "submit rc: {}", {
            std::str::from_utf8(&buf[..len]).unwrap_or("")
        });
        let submit_bytes = drain_all(handle, count);

        // Flush trailing packets, then drain them.
        let mut trailing = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.finish)(
                handle,
                &mut trailing,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 0, "finish rc");
        let finish_bytes = drain_all(handle, trailing);

        // At least one encoded packet (submit or finish) must carry a
        // non-empty bitstream — a real IDR frame was produced.
        assert!(
            submit_bytes + finish_bytes > 0,
            "encode produced no bitstream bytes"
        );

        // Out-of-range drain index is caller misuse → typed error.
        let mut meta = VideoEncodedPacketRepr::default();
        let mut data_len = 0usize;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.drain_packet)(
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

    /// Cached-POD borrow regression for the `submit_texture` call site
    /// (the `cdylib-make-borrow-cached-fields` hazard class, #988).
    /// `host_video_encoder_submit_texture` reconstructs the source Texture
    /// via `make_texture_borrow` and resolves the encode-src `image_view`
    /// off its inner. Lock that the borrow mirrors the real dimensions
    /// (never a zeroed borrow) AND that the encode-src view resolves —
    /// mental-revert: reverting `make_texture_borrow` to `width_cached: 0`
    /// / a null-inner borrow fails the dimension asserts / the image-view
    /// resolution here.
    #[test]
    fn submit_texture_make_borrow_reads_real_dims_and_view() {
        use crate::host_rhi::HostTextureExt;
        let Some(device) = crate::vulkan::rhi::HostVulkanDevice::new().ok() else {
            return;
        };
        let desc = crate::core::rhi::TextureDescriptor::new(
            320,
            240,
            crate::core::rhi::TextureFormat::Rgba8Unorm,
        );
        let Ok(host_texture) = crate::vulkan::rhi::HostVulkanTexture::new(&device, &desc) else {
            return;
        };
        let texture = crate::core::rhi::Texture::from_vulkan(host_texture);
        let borrow = super::super::shared::borrow::make_texture_borrow(texture.handle);
        assert_eq!(borrow.width(), 320, "borrow width must mirror the inner, not 0");
        assert_eq!(
            borrow.height(),
            240,
            "borrow height must mirror the inner, not 0"
        );
        assert!(
            borrow.vulkan_inner().image_view().is_ok(),
            "encode-src image view must resolve from the reconstructed borrow"
        );
    }

    /// Aligned-extent precondition on the `submit_texture` slot: a source
    /// texture smaller than the encoder's codec-aligned extent must return
    /// a typed error, NOT silently encode clamped / garbage NV12. The
    /// texture is minted on the encoder's own `HostVulkanDevice` (no second
    /// VkDevice) at a size well below any realistic aligned extent for the
    /// 320x240 session, then submitted through the vtable slot with a valid
    /// `SHADER_READ_ONLY_OPTIMAL` input layout so the layout guard passes
    /// and the dimension guard is what fires. Mental-revert: dropping the
    /// aligned-extent guard lets the call reach `encode_image`, which either
    /// returns rc 0 with garbage packets or a differently-worded error —
    /// either way the `rc == 1` + "smaller than required aligned extent"
    /// assertions below fail.
    #[test]
    fn submit_texture_rejects_source_smaller_than_aligned_extent() {
        use crate::host_rhi::HostTextureExt;
        let Some(session) = try_encoder_session() else {
            return;
        };
        let (aligned_width, aligned_height) = session.encoder.aligned_extent();
        // 16x16 is guaranteed strictly smaller than the aligned extent of a
        // 320x240 encode session (aligned width/height >= config dims).
        let desc = crate::core::rhi::TextureDescriptor::new(
            16,
            16,
            crate::core::rhi::TextureFormat::Rgba8Unorm,
        );
        let host_texture =
            match crate::vulkan::rhi::HostVulkanTexture::new(&session.encoder.host_device, &desc) {
                Ok(t) => t,
                Err(_) => return,
            };
        let texture = crate::core::rhi::Texture::from_vulkan(host_texture);
        let texture_handle = texture.handle;

        let handle = Box::into_raw(session) as *const c_void;
        struct DropGuard(*const c_void);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = Box::from_raw(self.0 as *mut HostVideoEncoderSession);
                }
            }
        }
        let _guard = DropGuard(handle);

        let (mut buf, mut len) = make_err_buf();
        let mut count = 0u32;
        let rc = unsafe {
            (HOST_VIDEO_ENCODER_SESSION_METHODS_VTABLE.submit_texture)(
                handle,
                texture_handle,
                streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL.0,
                0,
                0,
                &mut count,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(
            rc, 1,
            "undersize source must be a typed error, not a silent encode"
        );
        assert!(
            std::str::from_utf8(&buf[..len])
                .unwrap_or("")
                .contains("smaller than required aligned extent"),
            "got: {} (aligned {aligned_width}x{aligned_height})",
            std::str::from_utf8(&buf[..len]).unwrap_or("")
        );
    }
}
