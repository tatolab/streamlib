// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm handle for the host's hardware video decoder session (#1259
//! fill-in, #1377).
//!
//! Minted host-side through
//! [`crate::context::GpuContextFullAccess::create_decoder_session`]; the
//! opaque `handle` points at the host's `Box<HostVideoDecoderSession>`
//! (a `SimpleDecoder` + its staged-frame buffer). Per-frame driving
//! dispatches through the per-type
//! [`streamlib_plugin_abi::VideoDecoderSessionMethodsVTable`]; Drop
//! dispatches the parent
//! [`GpuContextFullAccessVTable`](streamlib_plugin_abi::GpuContextFullAccessVTable)'s
//! `drop_decoder_session` (`Box::from_raw` + drop host-side, keeping every
//! `vkDestroy*` / `wait_idle` inside the host build).
//!
//! **Single-owner stateful pipeline, deliberately NOT `Clone`** — the
//! backing `SimpleDecoder` owns exclusive Vulkan Video session / DPB /
//! command resources; the parent vtable carries only
//! `drop_decoder_session`, no clone slot.
//!
//! Pull model for the variable frame count: [`Self::feed`] decodes + stages
//! `0..N` frames host-side and returns `N`; the caller pulls each via
//! [`Self::drain_frame`]. Decoded frames are CPU pixel bytes (NV12 or RGBA
//! per the session's `rgba_output` config), copied out by value — no host
//! texture handle crosses the ABI in v1 (zero-copy decode into a
//! `TextureRing` is the reserved `decode_into_ring` follow-up).
//!
//! Like the encoder twin this handle is never written across the ABI by
//! value (the host writes only the opaque `handle`), so it has no
//! `#[repr(C)]` engine twin and needs no byte-layout regression test — the
//! plugin ABI surface is the frozen `VideoDecoderSessionMethodsVTable` +
//! the create/drop slots, both layout-locked in `streamlib-plugin-abi`.

use std::ffi::c_void;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{
    GpuContextFullAccessVTable, H273ColorVuiRepr, VideoDecodedFrameRepr,
    VideoDecoderSessionMethodsVTable,
};

/// H.273 colorimetry / VUI parsed from the active SPS, decoded from the
/// flattened [`H273ColorVuiRepr`]. Each axis is `Some(byte)` only when the
/// bitstream carried it; the codec-processor seam translates these H.273
/// byte values to its domain color info.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodedColorVui {
    /// ColourPrimaries — ITU-T H.273 §8.1.
    pub primaries: Option<u8>,
    /// TransferCharacteristics — ITU-T H.273 §8.2.
    pub transfer: Option<u8>,
    /// MatrixCoefficients — ITU-T H.273 §8.3.
    pub matrix: Option<u8>,
    /// `true` = full range (PC), `false` = limited range (TV).
    pub full_range: Option<bool>,
}

impl DecodedColorVui {
    /// Decode the flattened `(value, present)` axes of the frozen
    /// [`H273ColorVuiRepr`] into `Option` fields.
    fn from_repr(repr: &H273ColorVuiRepr) -> Self {
        Self {
            primaries: (repr.primaries_present != 0).then_some(repr.primaries),
            transfer: (repr.transfer_present != 0).then_some(repr.transfer),
            matrix: (repr.matrix_present != 0).then_some(repr.matrix),
            full_range: (repr.full_range_present != 0).then_some(repr.full_range != 0),
        }
    }
}

/// One decoded video frame pulled via [`DecoderSession::drain_frame`],
/// carrying the CPU pixel bytes plus the frozen
/// [`VideoDecodedFrameRepr`] metadata.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// Raw pixel bytes: RGBA (`width*height*4`) when [`Self::is_rgba`],
    /// else NV12 (`width*height*3/2`).
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Picture Order Count (display ordering).
    pub picture_order_count: i32,
    /// Decode order index (submission order).
    pub decode_order: u64,
    /// `true` when [`Self::data`] holds RGBA pixels, `false` for NV12.
    pub is_rgba: bool,
}

/// Hardware video decoder session (cdylib arm). Drive it per bitstream
/// chunk: [`Self::feed`] the Annex-B bytes → returns a staged frame count →
/// [`Self::drain_frame`] for each. Query [`Self::dimensions`] /
/// [`Self::decode_count`] / [`Self::current_color_vui`]; signal a seek /
/// gap with [`Self::feed_discontinuity`] or a full [`Self::reset`].
pub struct DecoderSession {
    /// Opaque handle to the host's `Box<HostVideoDecoderSession>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin-ABI Drop dispatch (`drop_decoder_session`).
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin-ABI method dispatch.
    pub(crate) methods_vtable: *const VideoDecoderSessionMethodsVTable,
}

// SAFETY: `handle` points at a host-owned `Box<HostVideoDecoderSession>`
// whose inner `SimpleDecoder` is `Send`; the vtable pointers are `'static`
// host statics. NOT `Sync` (the stateful pipeline is single-threaded — the
// methods take `&mut self`).
unsafe impl Send for DecoderSession {}

impl DecoderSession {
    /// Feed Annex-B bitstream bytes (partial or full NAL units), decoding
    /// + staging `0..N` frames host-side; returns `N` (pull each via
    /// [`Self::drain_frame`]). Each `feed` replaces the staging set.
    pub fn feed(&mut self, bitstream: &[u8]) -> Result<u32> {
        let vt = self.require_methods_vtable("feed")?;
        let mut out_count: u32 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; `(bitstream.as_ptr(),
        // bitstream.len())` valid for the call; out-param is an owned local.
        let status = unsafe {
            ((*vt).feed)(
                self.handle,
                bitstream.as_ptr(),
                bitstream.len(),
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

    /// Pull staged frame `[index]` (`index` in `[0, count)` from the most
    /// recent [`Self::feed`]). Copies the CPU pixel bytes into an owned
    /// [`DecodedFrame`] via two-call sizing (a too-small probe grows +
    /// retries). An out-of-range index is a typed error.
    pub fn drain_frame(&self, index: u32) -> Result<DecodedFrame> {
        let vt = self.require_methods_vtable("drain_frame")?;
        let mut meta = VideoDecodedFrameRepr::default();
        let mut data = vec![0u8; 256 * 1024];
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; `meta` + `data` + `out_len`
        // are owned locals the host writes into.
        let status = unsafe {
            ((*vt).drain_frame)(
                self.handle,
                index,
                &mut meta as *mut VideoDecodedFrameRepr,
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
            // `pixel_size`. Meta was already written on the probe call.
            let retry = unsafe {
                ((*vt).drain_frame)(
                    self.handle,
                    index,
                    &mut meta as *mut VideoDecodedFrameRepr,
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
        Ok(DecodedFrame {
            data,
            width: meta.width,
            height: meta.height,
            picture_order_count: meta.picture_order_count,
            decode_order: meta.decode_order,
            is_rgba: meta.is_rgba != 0,
        })
    }

    /// Signal a decode discontinuity (seek / gap): resets parser state and
    /// waits for the next IDR before decoding.
    pub fn feed_discontinuity(&mut self) -> Result<()> {
        let vt = self.require_methods_vtable("feed_discontinuity")?;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; handle paired at mint.
        let status = unsafe {
            ((*vt).feed_discontinuity)(
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

    /// Full reset: re-initialize the decoder session on the next SPS.
    pub fn reset(&mut self) -> Result<()> {
        let vt = self.require_methods_vtable("reset")?;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; handle paired at mint.
        let status = unsafe {
            ((*vt).reset)(
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

    /// SPS-detected coded `(width, height)` (valid after the first frame is
    /// produced; `(0, 0)` before the first SPS is parsed).
    pub fn dimensions(&self) -> Result<(u32, u32)> {
        let vt = self.require_methods_vtable("dimensions")?;
        let mut width: u32 = 0;
        let mut height: u32 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; out-params are owned locals.
        let status = unsafe {
            ((*vt).dimensions)(
                self.handle,
                &mut width as *mut u32,
                &mut height as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok((width, height))
    }

    /// Total frames decoded so far by this session.
    pub fn decode_count(&self) -> Result<u64> {
        let vt = self.require_methods_vtable("decode_count")?;
        let mut count: u64 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; out-param is an owned local.
        let status = unsafe {
            ((*vt).decode_count)(
                self.handle,
                &mut count as *mut u64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok(count)
    }

    /// Parsed SPS VUI colorimetry, or `None` when no SPS VUI has been
    /// parsed yet (the host's `out_present` byte distinguishes "no VUI yet"
    /// from an all-axes-absent VUI).
    pub fn current_color_vui(&self) -> Result<Option<DecodedColorVui>> {
        let vt = self.require_methods_vtable("current_color_vui")?;
        let mut vui = H273ColorVuiRepr::default();
        let mut present: u8 = 0;
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; out-params are owned locals.
        let status = unsafe {
            ((*vt).current_color_vui)(
                self.handle,
                &mut vui as *mut H273ColorVuiRepr,
                &mut present as *mut u8,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok((present != 0).then(|| DecodedColorVui::from_repr(&vui)))
    }

    /// Non-null methods-vtable guard shared by every method dispatch.
    fn require_methods_vtable(
        &self,
        op: &str,
    ) -> Result<*const VideoDecoderSessionMethodsVTable> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(format!(
                "{op}: decoder session methods vtable is null"
            )));
        }
        Ok(self.methods_vtable)
    }
}

impl Drop for DecoderSession {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle is the host's
            // `Box::into_raw(Box<HostVideoDecoderSession>)`; the vtable's
            // `drop_decoder_session` runs `Box::from_raw` + drop host-side
            // (`!Clone`, so a single drop reclaims it).
            unsafe {
                ((*self.vtable).drop_decoder_session)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for DecoderSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecoderSession").finish_non_exhaustive()
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
    fn color_vui_present_byte_gates_option() {
        // present == 0 => None regardless of a stale value byte
        // (mental-revert: dropping the `present != 0` guard leaks Some(42)).
        let repr = H273ColorVuiRepr {
            primaries: 42,
            primaries_present: 0,
            ..Default::default()
        };
        assert_eq!(DecodedColorVui::from_repr(&repr).primaries, None);

        // present == 1 => Some(value).
        let repr = H273ColorVuiRepr {
            primaries: 9,
            primaries_present: 1,
            full_range: 1,
            full_range_present: 1,
            ..Default::default()
        };
        let vui = DecodedColorVui::from_repr(&repr);
        assert_eq!(vui.primaries, Some(9));
        assert_eq!(vui.full_range, Some(true));
    }

    #[test]
    fn null_methods_vtable_is_typed_error_not_panic() {
        // A session whose methods vtable never got installed must return a
        // typed error from every method, never dispatch through null.
        let session = DecoderSession {
            handle: std::ptr::null(),
            vtable: std::ptr::null(),
            methods_vtable: std::ptr::null(),
        };
        assert!(session.dimensions().is_err());
        assert!(session.decode_count().is_err());
        assert!(session.drain_frame(0).is_err());
        assert!(session.current_color_vui().is_err());
        // Null handle + null vtable => Drop is a no-op (guarded).
    }

    // GPU-free drive through a fake methods vtable: locks the SDK-side
    // two-call sizing (`drain_frame`) + `VideoDecodedFrameRepr` decode
    // without a device. Mental-revert: dropping the `status == 2` retry
    // branch truncates the decoded frame to the probe buffer.
    const FAKE_WIDTH: u32 = 4;
    const FAKE_HEIGHT: u32 = 4;
    // RGBA 4x4 => 64 bytes; well under the SDK's 256 KiB initial probe.
    fn fake_rgba() -> Vec<u8> {
        (0..(FAKE_WIDTH * FAKE_HEIGHT * 4) as u8).collect()
    }

    unsafe extern "C" fn fake_feed(
        _s: *const c_void,
        _p: *const u8,
        _l: usize,
        out_count: *mut u32,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        unsafe { *out_count = 1 };
        0
    }

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn fake_drain_frame(
        _s: *const c_void,
        index: u32,
        out_meta: *mut VideoDecodedFrameRepr,
        out_data_buf: *mut u8,
        out_data_cap: usize,
        out_data_len: *mut usize,
        eb: *mut u8,
        ec: usize,
        el: *mut usize,
    ) -> i32 {
        if index != 0 {
            let msg = b"drain_frame: index out of range";
            let n = msg.len().min(ec);
            unsafe {
                std::ptr::copy_nonoverlapping(msg.as_ptr(), eb, n);
                *el = n;
            }
            return 1;
        }
        let pixels = fake_rgba();
        let required = pixels.len();
        let meta = VideoDecodedFrameRepr {
            width: FAKE_WIDTH,
            height: FAKE_HEIGHT,
            picture_order_count: 7,
            pixel_size: required as u32,
            decode_order: 3,
            is_rgba: 1,
            _pad: [0; 3],
            ring_slot_index: 0,
        };
        unsafe {
            std::ptr::write(out_meta, meta);
            *out_data_len = required;
        }
        if required > out_data_cap {
            return 2;
        }
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_data_buf, required) };
        0
    }

    unsafe extern "C" fn fake_feed_discontinuity(
        _s: *const c_void,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn fake_reset(
        _s: *const c_void,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn fake_dimensions(
        _s: *const c_void,
        out_width: *mut u32,
        out_height: *mut u32,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        unsafe {
            *out_width = FAKE_WIDTH;
            *out_height = FAKE_HEIGHT;
        }
        0
    }

    unsafe extern "C" fn fake_decode_count(
        _s: *const c_void,
        out_count: *mut u64,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        unsafe { *out_count = 5 };
        0
    }

    unsafe extern "C" fn fake_current_color_vui(
        _s: *const c_void,
        out_vui: *mut H273ColorVuiRepr,
        out_present: *mut u8,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        let repr = H273ColorVuiRepr {
            primaries: 9,
            primaries_present: 1,
            transfer: 16,
            transfer_present: 1,
            matrix: 9,
            matrix_present: 1,
            full_range: 1,
            full_range_present: 1,
        };
        unsafe {
            std::ptr::write(out_vui, repr);
            *out_present = 1;
        }
        0
    }

    unsafe extern "C" fn fake_decode_into_ring(
        _s: *const c_void,
        _rh: *const c_void,
        _oc: *mut u32,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        -100
    }

    static FAKE_METHODS: VideoDecoderSessionMethodsVTable = VideoDecoderSessionMethodsVTable {
        layout_version: streamlib_plugin_abi::VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        feed: fake_feed,
        drain_frame: fake_drain_frame,
        feed_discontinuity: fake_feed_discontinuity,
        reset: fake_reset,
        dimensions: fake_dimensions,
        decode_count: fake_decode_count,
        current_color_vui: fake_current_color_vui,
        decode_into_ring: fake_decode_into_ring,
    };

    fn fake_session() -> DecoderSession {
        DecoderSession {
            // Non-null dummy handle so method guards proceed; the fake
            // methods never dereference it. Null parent vtable => Drop no-op.
            handle: 0x1 as *const c_void,
            vtable: std::ptr::null(),
            methods_vtable: &FAKE_METHODS,
        }
    }

    #[test]
    fn feed_then_drain_decodes_meta_and_pixels() {
        let mut session = fake_session();
        let count = session.feed(&[0u8; 16]).unwrap();
        assert_eq!(count, 1);
        let frame = session.drain_frame(0).unwrap();
        assert_eq!(frame.data, fake_rgba());
        assert_eq!(frame.width, FAKE_WIDTH);
        assert_eq!(frame.height, FAKE_HEIGHT);
        assert_eq!(frame.picture_order_count, 7);
        assert_eq!(frame.decode_order, 3);
        assert!(frame.is_rgba);
        assert_eq!(frame.data.len(), (FAKE_WIDTH * FAKE_HEIGHT * 4) as usize);
    }

    #[test]
    fn drain_frame_out_of_range_is_typed_error() {
        let session = fake_session();
        let err = session.drain_frame(5).unwrap_err();
        assert!(format!("{err}").contains("out of range"), "got: {err}");
    }

    #[test]
    fn dimensions_and_decode_count_and_vui_round_trip() {
        let session = fake_session();
        assert_eq!(session.dimensions().unwrap(), (FAKE_WIDTH, FAKE_HEIGHT));
        assert_eq!(session.decode_count().unwrap(), 5);
        let vui = session.current_color_vui().unwrap().expect("present");
        assert_eq!(vui.primaries, Some(9));
        assert_eq!(vui.transfer, Some(16));
        assert_eq!(vui.matrix, Some(9));
        assert_eq!(vui.full_range, Some(true));
    }

    // Dedicated fake whose decoded frame exceeds the SDK's 256 KiB initial
    // probe, forcing the `status == 2` grow-and-retry branch to run.
    const FAKE_LARGE_WIDTH: u32 = 512;
    const FAKE_LARGE_HEIGHT: u32 = 512; // 512*512*4 = 1 MiB > 256 KiB probe
    fn fake_large_rgba() -> Vec<u8> {
        vec![0xCD; (FAKE_LARGE_WIDTH * FAKE_LARGE_HEIGHT * 4) as usize]
    }

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn fake_drain_frame_large(
        _s: *const c_void,
        _index: u32,
        out_meta: *mut VideoDecodedFrameRepr,
        out_data_buf: *mut u8,
        out_data_cap: usize,
        out_data_len: *mut usize,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        let pixels = fake_large_rgba();
        let required = pixels.len();
        let meta = VideoDecodedFrameRepr {
            width: FAKE_LARGE_WIDTH,
            height: FAKE_LARGE_HEIGHT,
            picture_order_count: 0,
            pixel_size: required as u32,
            decode_order: 0,
            is_rgba: 1,
            _pad: [0; 3],
            ring_slot_index: 0,
        };
        unsafe {
            std::ptr::write(out_meta, meta);
            *out_data_len = required;
        }
        if required > out_data_cap {
            return 2;
        }
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_data_buf, required) };
        0
    }

    static FAKE_METHODS_LARGE_FRAME: VideoDecoderSessionMethodsVTable =
        VideoDecoderSessionMethodsVTable {
            layout_version:
                streamlib_plugin_abi::VIDEO_DECODER_SESSION_METHODS_VTABLE_LAYOUT_VERSION,
            _reserved_padding: 0,
            feed: fake_feed,
            drain_frame: fake_drain_frame_large,
            feed_discontinuity: fake_feed_discontinuity,
            reset: fake_reset,
            dimensions: fake_dimensions,
            decode_count: fake_decode_count,
            current_color_vui: fake_current_color_vui,
            decode_into_ring: fake_decode_into_ring,
        };

    #[test]
    fn drain_frame_two_call_grows_when_probe_too_small() {
        let session = DecoderSession {
            handle: 0x1 as *const c_void,
            vtable: std::ptr::null(),
            methods_vtable: &FAKE_METHODS_LARGE_FRAME,
        };
        // Initial 256 KiB probe < 1 MiB => status 2 => grow => retry.
        // Mental-revert: dropping the retry branch truncates to 256 KiB.
        let frame = session.drain_frame(0).unwrap();
        assert_eq!(frame.data, fake_large_rgba());
        assert_eq!(
            frame.data.len(),
            (FAKE_LARGE_WIDTH * FAKE_LARGE_HEIGHT * 4) as usize
        );
    }

    #[test]
    fn feed_discontinuity_and_reset_dispatch_ok() {
        let mut session = fake_session();
        assert!(session.feed_discontinuity().is_ok());
        assert!(session.reset().is_ok());
    }
}
