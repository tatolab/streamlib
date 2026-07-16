// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm handle for the host's hardware video encoder session (#1259
//! fill-in, #1376).
//!
//! Minted host-side through
//! [`crate::context::GpuContextFullAccess::create_encoder_session`]; the
//! opaque `handle` points at the host's `Box<HostVideoEncoderSession>`
//! (a `SimpleEncoder` + its staged-packet buffer). Per-frame driving
//! dispatches through the per-type
//! [`streamlib_plugin_abi::VideoEncoderSessionMethodsVTable`]; Drop
//! dispatches the parent
//! [`GpuContextFullAccessVTable`](streamlib_plugin_abi::GpuContextFullAccessVTable)'s
//! `drop_encoder_session` (`Box::from_raw` + drop host-side, keeping every
//! `vkDestroy*` / `wait_idle` inside the host build).
//!
//! **Single-owner stateful pipeline, deliberately NOT `Clone`** — the
//! backing `SimpleEncoder` owns exclusive Vulkan Video session / DPB /
//! command resources; the parent vtable carries only
//! `drop_encoder_session`, no clone slot.
//!
//! Pull model for the variable packet count: `submit_frame_nv12` /
//! `submit_texture` / `finish` stage `0..N` packets host-side and return
//! `N`; the caller pulls each via [`Self::drain_packet`].
//!
//! Unlike the engine's `TextureReadback` / `PresentTarget` PluginAbiObjects
//! this handle is never written across the ABI by value (the host writes
//! only the opaque `handle` + two aligned-extent out-params), so it has no
//! `#[repr(C)]` engine twin and needs no byte-layout regression test — the
//! plugin ABI surface is the frozen `VideoEncoderSessionMethodsVTable` +
//! the create/drop slots, both layout-locked in `streamlib-plugin-abi`.

use std::ffi::c_void;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{
    GpuContextFullAccessVTable, VideoEncodedPacketRepr, VideoEncoderSessionMethodsVTable,
    VideoFrameTypeRepr,
};

use streamlib_consumer_rhi::VulkanLayout;

use crate::rhi::Texture;

/// Frame type of a drained [`EncodedPacket`], decoded from the frozen
/// [`VideoFrameTypeRepr`] discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedFrameType {
    /// Instantaneous Decoder Refresh keyframe (clean decode entry point).
    Idr,
    /// Intra frame (non-IDR).
    I,
    /// Predictive frame.
    P,
    /// Bi-predictive frame.
    B,
}

impl EncodedFrameType {
    /// Decode the frozen `#[repr(u32)]` [`VideoFrameTypeRepr`] discriminant.
    fn from_repr(raw: u32) -> Self {
        match raw {
            x if x == VideoFrameTypeRepr::Idr as u32 => Self::Idr,
            x if x == VideoFrameTypeRepr::I as u32 => Self::I,
            x if x == VideoFrameTypeRepr::P as u32 => Self::P,
            x if x == VideoFrameTypeRepr::B as u32 => Self::B,
            _ => Self::P,
        }
    }
}

/// One encoded bitstream packet pulled via [`EncoderSession::drain_packet`].
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    /// Raw encoded bitstream bytes (H.264 / H.265 NAL units).
    pub data: Vec<u8>,
    /// The frame type that was encoded.
    pub frame_type: EncodedFrameType,
    /// `true` if this frame is an IDR keyframe.
    pub is_keyframe: bool,
    /// Presentation timestamp (frame index in input order).
    pub pts: u64,
    /// Monotonic timestamp in nanoseconds passed through from the submit
    /// call. `None` if the caller did not provide one.
    pub timestamp_ns: Option<i64>,
}

/// Hardware video encoder session (cdylib arm). Drive it per frame:
/// `submit_frame_nv12` / `submit_texture` → returns a staged packet count
/// → [`Self::drain_packet`] for each, then [`Self::finish`] to flush
/// trailing packets at end-of-stream.
pub struct EncoderSession {
    /// Opaque handle to the host's `Box<HostVideoEncoderSession>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin-ABI Drop dispatch (`drop_encoder_session`).
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin-ABI method dispatch.
    pub(crate) methods_vtable: *const VideoEncoderSessionMethodsVTable,
    /// Codec-aligned encode width (RGBA input to [`Self::submit_texture`]
    /// must be at least this wide). Cached from the mint out-param.
    pub(crate) aligned_width: u32,
    /// Codec-aligned encode height. Cached from the mint out-param.
    pub(crate) aligned_height: u32,
}

// SAFETY: `handle` points at a host-owned `Box<HostVideoEncoderSession>`
// whose inner `SimpleEncoder` is `Send`; the vtable pointers are `'static`
// host statics. NOT `Sync` (the stateful pipeline is single-threaded — the
// methods take `&mut self`).
unsafe impl Send for EncoderSession {}

impl EncoderSession {
    /// Codec-aligned `(width, height)` the encode session runs at — RGBA
    /// input passed to [`Self::submit_texture`] must be at least these
    /// dimensions. Cached POD (no plugin-ABI hop).
    pub fn aligned_extent(&self) -> (u32, u32) {
        (self.aligned_width, self.aligned_height)
    }

    /// Cached SPS/PPS (H.264) or VPS/SPS/PPS (H.265) header bytes. Two-call
    /// sizing under the hood (a too-small probe grows + retries).
    pub fn header(&self) -> Result<Vec<u8>> {
        let vt = self.require_methods_vtable("header")?;
        let mut out = vec![0u8; 256];
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; handle paired with it at mint;
        // `out` owns `out.len()` writable bytes.
        let status = unsafe {
            ((*vt).header)(
                self.handle,
                out.as_mut_ptr(),
                out.len(),
                &mut out_len as *mut usize,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 2 {
            out = vec![0u8; out_len];
            // SAFETY: same contract; buffer now sized to the host-reported length.
            let retry = unsafe {
                ((*vt).header)(
                    self.handle,
                    out.as_mut_ptr(),
                    out.len(),
                    &mut out_len as *mut usize,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            };
            if retry != 0 {
                return Err(decode_err(&err_buf, err_len));
            }
        } else if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        out.truncate(out_len);
        Ok(out)
    }

    /// Force the next submitted frame to encode as an IDR keyframe.
    pub fn force_idr(&mut self) -> Result<()> {
        let vt = self.require_methods_vtable("force_idr")?;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; handle paired at mint.
        let status = unsafe {
            ((*vt).force_idr)(
                self.handle,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok(())
    }

    /// Submit a raw NV12 frame (`width*height*3/2` bytes) for encoding,
    /// returning the number of packets staged (pull each via
    /// [`Self::drain_packet`]).
    pub fn submit_frame_nv12(
        &mut self,
        nv12: &[u8],
        timestamp_ns: Option<i64>,
    ) -> Result<u32> {
        let vt = self.require_methods_vtable("submit_frame_nv12")?;
        let (has_timestamp, timestamp) = timestamp_split(timestamp_ns);
        let mut out_count: u32 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; `(nv12.as_ptr(), nv12.len())`
        // valid for the call; out-param is an owned local.
        let status = unsafe {
            ((*vt).submit_frame_nv12)(
                self.handle,
                nv12.as_ptr(),
                nv12.len(),
                has_timestamp,
                timestamp,
                &mut out_count as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok(out_count)
    }

    /// Encode a GPU-resident RGBA [`Texture`] directly (RGB→NV12 on the
    /// GPU, then hardware encode). `input_layout` is the layout the texture
    /// is currently in — the host requires
    /// [`VulkanLayout::SHADER_READ_ONLY_OPTIMAL`]. Returns the number of
    /// packets staged.
    pub fn submit_texture(
        &mut self,
        texture: &Texture,
        input_layout: VulkanLayout,
        timestamp_ns: Option<i64>,
    ) -> Result<u32> {
        let vt = self.require_methods_vtable("submit_texture")?;
        let (has_timestamp, timestamp) = timestamp_split(timestamp_ns);
        let mut out_count: u32 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; `texture.handle` is the
        // borrowed Texture PluginAbiObject handle (host resolves the
        // encode-src view via make-borrow); out-param is an owned local.
        let status = unsafe {
            ((*vt).submit_texture)(
                self.handle,
                texture.handle,
                input_layout.0,
                has_timestamp,
                timestamp,
                &mut out_count as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok(out_count)
    }

    /// Pull staged packet `[index]` (`index` in `[0, count)` from the most
    /// recent submit / finish). Copies the bitstream into an owned
    /// [`EncodedPacket`] via two-call sizing (a too-small probe grows +
    /// retries). An out-of-range index is a typed error.
    pub fn drain_packet(&self, index: u32) -> Result<EncodedPacket> {
        let vt = self.require_methods_vtable("drain_packet")?;
        let mut meta = VideoEncodedPacketRepr::default();
        let mut data = vec![0u8; 64 * 1024];
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; `meta` + `data` + `out_len`
        // are owned locals the host writes into.
        let status = unsafe {
            ((*vt).drain_packet)(
                self.handle,
                index,
                &mut meta as *mut VideoEncodedPacketRepr,
                data.as_mut_ptr(),
                data.len(),
                &mut out_len as *mut usize,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 2 {
            data = vec![0u8; out_len];
            // SAFETY: same contract; buffer now sized to the host-reported
            // `bitstream_size`. Meta was already written on the probe call.
            let retry = unsafe {
                ((*vt).drain_packet)(
                    self.handle,
                    index,
                    &mut meta as *mut VideoEncodedPacketRepr,
                    data.as_mut_ptr(),
                    data.len(),
                    &mut out_len as *mut usize,
                    err_buf.as_mut_ptr(),
                    err_buf.len(),
                    &mut err_len as *mut usize,
                )
            };
            if retry != 0 {
                return Err(decode_err(&err_buf, err_len));
            }
        } else if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        data.truncate(out_len);
        Ok(EncodedPacket {
            data,
            frame_type: EncodedFrameType::from_repr(meta.frame_type),
            is_keyframe: meta.is_keyframe != 0,
            pts: meta.pts,
            timestamp_ns: (meta.has_timestamp != 0).then_some(meta.timestamp_ns),
        })
    }

    /// Flush the reorder buffer / end-of-stream, staging trailing packets;
    /// returns the number staged (pull each via [`Self::drain_packet`]).
    pub fn finish(&mut self) -> Result<u32> {
        let vt = self.require_methods_vtable("finish")?;
        let mut out_count: u32 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; out-param is an owned local.
        let status = unsafe {
            ((*vt).finish)(
                self.handle,
                &mut out_count as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok(out_count)
    }

    /// Non-null methods-vtable guard shared by every method dispatch.
    fn require_methods_vtable(
        &self,
        op: &str,
    ) -> Result<*const VideoEncoderSessionMethodsVTable> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(format!(
                "{op}: encoder session methods vtable is null"
            )));
        }
        Ok(self.methods_vtable)
    }
}

impl Drop for EncoderSession {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle is the host's
            // `Box::into_raw(Box<HostVideoEncoderSession>)`; the vtable's
            // `drop_encoder_session` runs `Box::from_raw` + drop host-side
            // (`!Clone`, so a single drop reclaims it).
            unsafe {
                ((*self.vtable).drop_encoder_session)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for EncoderSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncoderSession")
            .field("aligned_width", &self.aligned_width)
            .field("aligned_height", &self.aligned_height)
            .finish()
    }
}

/// Split `Option<i64>` into the `(has_timestamp, timestamp_ns)` pair the
/// submit slots take (`Option` cannot cross the plugin ABI).
fn timestamp_split(timestamp_ns: Option<i64>) -> (u8, i64) {
    match timestamp_ns {
        Some(ts) => (1, ts),
        None => (0, 0),
    }
}

/// Decode a `(status, err_buf)` plugin-ABI failure into a typed error.
fn decode_err(err_buf: &[u8], err_len: usize) -> Error {
    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
    Error::GpuError(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_frame_type_round_trips_every_repr() {
        // Mental-revert: renumber a `VideoFrameTypeRepr` discriminant and
        // this decode returns the wrong variant.
        for (repr, ty) in [
            (VideoFrameTypeRepr::Idr, EncodedFrameType::Idr),
            (VideoFrameTypeRepr::I, EncodedFrameType::I),
            (VideoFrameTypeRepr::P, EncodedFrameType::P),
            (VideoFrameTypeRepr::B, EncodedFrameType::B),
        ] {
            assert_eq!(EncodedFrameType::from_repr(repr as u32), ty);
        }
    }

    #[test]
    fn timestamp_split_maps_option() {
        assert_eq!(timestamp_split(None), (0, 0));
        assert_eq!(timestamp_split(Some(1_234)), (1, 1_234));
    }

    #[test]
    fn null_methods_vtable_is_typed_error_not_panic() {
        // A session whose methods vtable never got installed must return a
        // typed error from every method, never dispatch through null.
        let session = EncoderSession {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            methods_vtable: std::ptr::null(),
            aligned_width: 320,
            aligned_height: 240,
        };
        assert!(session.header().is_err());
        assert!(session.drain_packet(0).is_err());
        assert_eq!(session.aligned_extent(), (320, 240));
        // Null handle + null vtable => Drop is a no-op (guarded).
    }

    // GPU-free drive through a fake methods vtable: locks the SDK-side
    // two-call sizing (`header` / `drain_packet`) + `VideoEncodedPacketRepr`
    // decode without a device. Mental-revert: dropping the `status == 2`
    // retry branch truncates the header / bitstream to the probe buffer.
    const FAKE_HEADER: &[u8] = b"\x00\x00\x00\x01SPSPPS-header-bytes";
    const FAKE_BITSTREAM: &[u8] = b"\x00\x00\x00\x01encoded-nal-unit-payload";

    unsafe extern "C" fn fake_header(
        _s: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        let required = FAKE_HEADER.len();
        unsafe { *out_len = required };
        if required > out_cap {
            return 2;
        }
        unsafe { std::ptr::copy_nonoverlapping(FAKE_HEADER.as_ptr(), out_buf, required) };
        0
    }

    unsafe extern "C" fn fake_force_idr(
        _s: *const c_void,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        0
    }

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn fake_submit_frame_nv12(
        _s: *const c_void,
        _p: *const u8,
        _l: usize,
        _ht: u8,
        _ts: i64,
        out_count: *mut u32,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        unsafe { *out_count = 1 };
        0
    }

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn fake_submit_texture(
        _s: *const c_void,
        _th: *const c_void,
        _il: i32,
        _ht: u8,
        _ts: i64,
        out_count: *mut u32,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        unsafe { *out_count = 1 };
        0
    }

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn fake_drain_packet(
        _s: *const c_void,
        index: u32,
        out_meta: *mut VideoEncodedPacketRepr,
        out_data_buf: *mut u8,
        out_data_cap: usize,
        out_data_len: *mut usize,
        eb: *mut u8,
        ec: usize,
        el: *mut usize,
    ) -> i32 {
        if index != 0 {
            let msg = b"drain_packet: index out of range";
            let n = msg.len().min(ec);
            unsafe {
                std::ptr::copy_nonoverlapping(msg.as_ptr(), eb, n);
                *el = n;
            }
            return 1;
        }
        let required = FAKE_BITSTREAM.len();
        let meta = VideoEncodedPacketRepr {
            frame_type: VideoFrameTypeRepr::Idr as u32,
            is_keyframe: 1,
            has_timestamp: 1,
            _pad0: [0; 2],
            pts: 7,
            timestamp_ns: 42,
            bitstream_size: required as u32,
            _reserved: 0,
        };
        unsafe {
            std::ptr::write(out_meta, meta);
            *out_data_len = required;
        }
        if required > out_data_cap {
            return 2;
        }
        unsafe { std::ptr::copy_nonoverlapping(FAKE_BITSTREAM.as_ptr(), out_data_buf, required) };
        0
    }

    unsafe extern "C" fn fake_finish(
        _s: *const c_void,
        out_count: *mut u32,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        unsafe { *out_count = 0 };
        0
    }

    static FAKE_METHODS: VideoEncoderSessionMethodsVTable = VideoEncoderSessionMethodsVTable {
        layout_version: streamlib_plugin_abi::VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        header: fake_header,
        force_idr: fake_force_idr,
        submit_frame_nv12: fake_submit_frame_nv12,
        submit_texture: fake_submit_texture,
        drain_packet: fake_drain_packet,
        finish: fake_finish,
    };

    fn fake_session() -> EncoderSession {
        EncoderSession {
            // Non-null dummy handle so method guards proceed; the fake
            // methods never dereference it. Null parent vtable => Drop no-op.
            handle: 0x1 as *const c_void,
            vtable: std::ptr::null(),
            methods_vtable: &FAKE_METHODS,
            aligned_width: 320,
            aligned_height: 240,
        }
    }

    #[test]
    fn header_returns_full_bytes() {
        let session = fake_session();
        // FAKE_HEADER (22 bytes) fits the initial 256-byte probe, so this
        // locks the one-call decode; the grow branch is locked separately
        // by `header_two_call_grows_when_probe_too_small`.
        assert_eq!(session.header().unwrap(), FAKE_HEADER);
    }

    #[test]
    fn drain_packet_decodes_meta_and_bitstream() {
        let mut session = fake_session();
        let count = session.submit_frame_nv12(&[0u8; 16], Some(99)).unwrap();
        assert_eq!(count, 1);
        let packet = session.drain_packet(0).unwrap();
        assert_eq!(packet.data, FAKE_BITSTREAM);
        assert_eq!(packet.frame_type, EncodedFrameType::Idr);
        assert!(packet.is_keyframe);
        assert_eq!(packet.pts, 7);
        assert_eq!(packet.timestamp_ns, Some(42));
    }

    // Dedicated fake whose header exceeds the SDK's 256-byte initial probe,
    // forcing the `status == 2` grow-and-retry branch to run for real.
    static FAKE_LARGE_HEADER: [u8; 300] = [0xAB; 300];

    unsafe extern "C" fn fake_header_large(
        _s: *const c_void,
        out_buf: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        let required = FAKE_LARGE_HEADER.len();
        unsafe { *out_len = required };
        if required > out_cap {
            return 2;
        }
        unsafe { std::ptr::copy_nonoverlapping(FAKE_LARGE_HEADER.as_ptr(), out_buf, required) };
        0
    }

    static FAKE_METHODS_LARGE_HEADER: VideoEncoderSessionMethodsVTable =
        VideoEncoderSessionMethodsVTable {
            layout_version:
                streamlib_plugin_abi::VIDEO_ENCODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
            _reserved_padding: 0,
            header: fake_header_large,
            force_idr: fake_force_idr,
            submit_frame_nv12: fake_submit_frame_nv12,
            submit_texture: fake_submit_texture,
            drain_packet: fake_drain_packet,
            finish: fake_finish,
        };

    #[test]
    fn header_two_call_grows_when_probe_too_small() {
        let session = EncoderSession {
            handle: 0x1 as *const c_void,
            vtable: std::ptr::null(),
            methods_vtable: &FAKE_METHODS_LARGE_HEADER,
            aligned_width: 320,
            aligned_height: 240,
        };
        // Initial 256-byte probe < 300 => status 2 => grow to 300 => retry.
        // Mental-revert: dropping the retry branch truncates to 256 bytes.
        assert_eq!(session.header().unwrap(), FAKE_LARGE_HEADER.to_vec());
    }

    #[test]
    fn drain_packet_out_of_range_is_typed_error() {
        let session = fake_session();
        let err = session.drain_packet(5).unwrap_err();
        assert!(format!("{err}").contains("out of range"), "got: {err}");
    }
}
