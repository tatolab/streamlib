// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's [`PresentTarget`] PluginAbiObject (#1258).
//!
//! Layout-stable `#[repr(C)] { handle, vtable, methods_vtable,
//! color_format_raw, _padding }` (32 bytes). The opaque handle points at
//! the host's `Box<Mutex<VulkanPresentTarget>>`; Drop dispatches the
//! parent [`GpuContextFullAccessVTable`]'s `drop_present_target`
//! (`Box::from_raw` + drop host-side, keeping every `vkDestroy*` inside
//! the host build). Per-frame method dispatch (`begin_frame` / `end_frame`
//! / `recreate` / `set_hdr_metadata`) routes through the per-type
//! [`PresentTargetMethodsVTable`].
//!
//! **Single-owner; deliberately NOT `Clone`** — the backing
//! `VulkanPresentTarget` owns a `VkSurfaceKHR` + `VkSwapchainKHR` +
//! per-image semaphores; the parent vtable carries only
//! `drop_present_target`, no clone slot.
//!
//! The per-frame recorder handed back by [`Self::begin_frame`] is
//! **borrowed** — the present target owns it across the begin/end split.
//! It is wrapped in a drop-suppressing [`PresentTargetFrame`] so a plugin
//! author can never release it and double-free the present target's own
//! recorder (OQ-2: the capability is enforced by the type system, not by
//! a docs-only "do not drop" note).

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{
    ColorTraitsRepr, GpuContextFullAccessVTable, HdrStaticMetadataRepr, PresentFrameBeginRepr,
    PresentTargetMethodsVTable, RhiCommandRecorderMethodsVTable, SemaphoreSubmitInfoRepr,
};

use crate::rhi::RhiCommandRecorder;

/// Swapchain-backed present target, minted through the FullAccess
/// `create_present_target` slot. Drive it per frame with
/// [`Self::begin_frame`] → draw through the frame's borrowed recorder →
/// [`PresentTargetFrame::end`].
#[repr(C)]
pub struct PresentTarget {
    /// Opaque handle to the host's `Box<Mutex<VulkanPresentTarget>>`.
    pub(crate) handle: *const c_void,
    /// Parent vtable for plugin-ABI Drop dispatch (`drop_present_target`).
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin-ABI method dispatch.
    pub(crate) methods_vtable: *const PresentTargetMethodsVTable,
    /// Cached swapchain-image `TextureFormat` `#[repr(u32)]` discriminant —
    /// a zero-hop `&self` read, refreshed on [`Self::recreate`].
    pub(crate) color_format_raw: u32,
    /// Reserved padding (keeps the struct a multiple of 8; zero, never read).
    pub(crate) _padding: u32,
}

// SAFETY: same shape as the engine twin; `handle` points at the host's
// `Box<Mutex<VulkanPresentTarget>>` (Send + Sync), the vtable pointers are
// `'static` host statics.
unsafe impl Send for PresentTarget {}
unsafe impl Sync for PresentTarget {}

impl PresentTarget {
    /// Swapchain image color format `#[repr(u32)]` discriminant — a
    /// zero-hop cached-POD read (no plugin-ABI hop). Refreshed on
    /// [`Self::recreate`].
    pub fn color_format_raw(&self) -> u32 {
        self.color_format_raw
    }

    /// Acquire the next swapchain image and open the frame's borrowed
    /// recorder (already `begin()`'d + pre-barriered host-side). Returns
    /// `Ok(None)` when the swapchain is `OUT_OF_DATE_KHR` — drive
    /// [`Self::recreate`] and retry, do NOT call end. Otherwise returns a
    /// [`PresentTargetFrame`] the caller records draws into and then
    /// [`PresentTargetFrame::end`]s exactly once (even on a draw error).
    pub fn begin_frame(&mut self) -> Result<Option<PresentTargetFrame<'_>>> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "begin_frame: present target methods vtable is null".into(),
            ));
        }
        let mut frame_repr = PresentFrameBeginRepr::default();
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; handle paired at mint.
        let status = unsafe {
            ((*self.methods_vtable).begin_frame)(
                self.handle,
                &mut frame_repr,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        if frame_repr.acquired_ok == 0 {
            // OUT_OF_DATE_KHR: no frame stashed in flight; caller recreates.
            return Ok(None);
        }

        // Build the drop-suppressed borrowed recorder. The methods vtable is
        // the host's RhiCommandRecorder methods table (cdylib mode resolves
        // the host-installed pointer; host mode the local static). The
        // recorder is NON-OWNING — wrapped in `ManuallyDrop` so its Drop
        // (which would dispatch `drop_command_recorder`) never runs.
        let recorder_methods_vtable = crate::plugin::host_callbacks()
            .map(|c| c.rhi_command_recorder_methods_vtable)
            .unwrap_or(std::ptr::null::<RhiCommandRecorderMethodsVTable>());
        let recorder = ManuallyDrop::new(RhiCommandRecorder {
            handle: frame_repr.recorder_handle as *const c_void,
            vtable: self.vtable,
            methods_vtable: recorder_methods_vtable,
        });

        Ok(Some(PresentTargetFrame {
            image_raw: frame_repr.image_raw,
            image_view_raw: frame_repr.image_view_raw,
            frame_index: frame_repr.frame_index,
            extent: (frame_repr.extent_w, frame_repr.extent_h),
            color_format_raw: frame_repr.color_format_raw,
            recorder,
            present_target: self,
        }))
    }

    /// Recreate the swapchain at a new extent / color traits (`None` keeps
    /// the legacy SDR pick). Refreshes the cached [`Self::color_format_raw`]
    /// from the slot's `out_color_format_raw` — a recreate can flip SDR
    /// BGRA8 → HDR10 FP16, and the cached getter must track it immediately
    /// (the make-borrow staleness class,
    /// `docs/learnings/cdylib-make-borrow-cached-fields.md`).
    pub fn recreate(
        &mut self,
        width: u32,
        height: u32,
        color: Option<ColorTraitsRepr>,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "recreate: present target methods vtable is null".into(),
            ));
        }
        let color_ptr = color
            .as_ref()
            .map(|c| c as *const ColorTraitsRepr)
            .unwrap_or(std::ptr::null());
        let mut out_color_format_raw: u32 = self.color_format_raw;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; color_ptr is null or
        // borrows `color` for the call; out pointer is a local.
        let status = unsafe {
            ((*self.methods_vtable).recreate)(
                self.handle,
                width,
                height,
                color_ptr,
                &mut out_color_format_raw as *mut u32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        self.color_format_raw = out_color_format_raw;
        Ok(())
    }

    /// Push HDR static metadata (no-op host-side when the swapchain
    /// colorspace is not HDR-signaling). Dispatches the `set_hdr_metadata`
    /// slot.
    pub fn set_hdr_metadata(&mut self, metadata: &HdrStaticMetadataRepr) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "set_hdr_metadata: present target methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; metadata borrowed
        // for the call.
        let status = unsafe {
            ((*self.methods_vtable).set_hdr_metadata)(
                self.handle,
                metadata as *const HdrStaticMetadataRepr,
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
}

impl Drop for PresentTarget {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle is the host's `Box::into_raw(Box<Mutex<
            // VulkanPresentTarget>>)`; the vtable's `drop_present_target`
            // runs `Box::from_raw` + drop host-side.
            unsafe {
                ((*self.vtable).drop_present_target)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for PresentTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresentTarget")
            .field("color_format_raw", &self.color_format_raw)
            .finish()
    }
}

/// An acquired frame: the swapchain image + its **borrowed** per-frame
/// recorder. Record draws into [`Self::recorder`], then call [`Self::end`]
/// exactly once — the host always barriers + submits + presents on `end`
/// (even after a draw error) to keep the swapchain making forward
/// progress.
///
/// The recorder is drop-suppressed ([`ManuallyDrop`]): the present target
/// owns it across the begin/end split, so a plugin author can never
/// release it and double-free. Holding the frame borrows the present
/// target mutably, so a second `begin_frame` is a compile error until this
/// frame is `end`ed (or dropped).
pub struct PresentTargetFrame<'a> {
    /// Acquired swapchain `VkImage` (widened to `u64`).
    pub image_raw: u64,
    /// `VkImageView` for `cmd_begin_dynamic_rendering` (widened to `u64`).
    pub image_view_raw: u64,
    /// Frame-in-flight slot index (descriptor-ring slot for `set_*` / draws).
    pub frame_index: u32,
    /// Acquired swapchain extent `(width, height)`.
    pub extent: (u32, u32),
    /// Live swapchain `TextureFormat` discriminant (never a stale cache) for
    /// kernel attachment matching.
    pub color_format_raw: u32,
    /// Borrowed per-frame recorder — NON-OWNING (drop-suppressed).
    recorder: ManuallyDrop<RhiCommandRecorder>,
    present_target: &'a mut PresentTarget,
}

impl PresentTargetFrame<'_> {
    /// The frame's borrowed command recorder. Drive it with
    /// `cmd_begin_dynamic_rendering` → `record_draw` / `record_dispatch` →
    /// `cmd_end_dynamic_rendering`. The present target handles the pre/post
    /// swapchain barriers + submit + present internally.
    pub fn recorder(&mut self) -> &mut RhiCommandRecorder {
        &mut self.recorder
    }

    /// Complete the frame: post-draw barrier + submit (wait image-available
    /// + `extra_waits`; signal render-finished + frame-timeline) + present,
    /// all host-side. `extra_waits` fold any producer-finished timeline
    /// waits into the submit wait list (empty slice valid). Must be called
    /// exactly once per acquired frame — consuming the frame releases the
    /// present-target borrow so the next `begin_frame` can run.
    pub fn end(self, extra_waits: &[SemaphoreSubmitInfoRepr]) -> Result<()> {
        let vt = self.present_target.methods_vtable;
        if vt.is_null() {
            return Err(Error::GpuError(
                "end_frame: present target methods vtable is null".into(),
            ));
        }
        let present_handle = self.present_target.handle;
        // The recorder-identity check the host runs: hand back the exact
        // borrowed recorder from begin_frame.
        let recorder_handle = self.recorder.handle;
        let waits_ptr = if extra_waits.is_empty() {
            std::ptr::null()
        } else {
            extra_waits.as_ptr()
        };
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: vt non-null per the guard; handles paired at mint;
        // extra_waits outlives the call by the caller's borrow.
        let status = unsafe {
            ((*vt).end_frame)(
                present_handle,
                recorder_handle,
                waits_ptr,
                extra_waits.len(),
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        // The borrowed recorder must NOT drop (present target owns it):
        // `ManuallyDrop` guarantees its Drop never runs as `self` is
        // consumed here.
        if status != 0 {
            return Err(decode_err(&err_buf, err_len));
        }
        Ok(())
    }
}

/// Decode a `(status, err_buf)` plugin-ABI failure into a typed error.
fn decode_err(err_buf: &[u8], err_len: usize) -> Error {
    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
    Error::GpuError(msg)
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// Must match the engine's
    /// `vulkan/rhi/vulkan_present_target.rs::PresentTarget`:
    ///   handle @ 0, vtable @ 8, methods_vtable @ 16,
    ///   color_format_raw @ 24, _padding @ 28. Total 32 bytes, align 8.
    /// Both arms pin this — a drift on either side is silent cross-build
    /// corruption.
    #[test]
    fn present_target_twin_layout() {
        assert_eq!(size_of::<PresentTarget>(), 32);
        assert_eq!(align_of::<PresentTarget>(), 8);
        assert_eq!(offset_of!(PresentTarget, handle), 0);
        assert_eq!(offset_of!(PresentTarget, vtable), 8);
        assert_eq!(offset_of!(PresentTarget, methods_vtable), 16);
        assert_eq!(offset_of!(PresentTarget, color_format_raw), 24);
        assert_eq!(offset_of!(PresentTarget, _padding), 28);
    }

    #[test]
    fn present_target_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PresentTarget>();
    }

    /// `PresentTarget` is deliberately NOT `Clone` (single-owner Box).
    /// ```compile_fail
    /// fn assert_clone<T: Clone>() {}
    /// assert_clone::<streamlib_plugin_sdk::rhi::PresentTarget>();
    /// ```
    #[test]
    fn present_target_not_clone_doc() {}
}

/// Regression: `recreate` must refresh the cached `color_format_raw` from
/// the slot's `out_color_format_raw`. A recreate can flip SDR BGRA8 →
/// HDR10 FP16; without this refresh the cached-POD getter is stale until
/// the next `begin_frame` — the exact make-borrow staleness class
/// (`docs/learnings/cdylib-make-borrow-cached-fields.md`). Driven through
/// a fake methods vtable so it needs no GPU. Mentally reverting the
/// `self.color_format_raw = out_color_format_raw;` line in `recreate`
/// leaves the getter at the stale initial value and fails this test.
#[cfg(test)]
mod recreate_refresh_tests {
    use super::*;

    /// Sentinel the fake `recreate` slot writes into `out_color_format_raw`
    /// — distinct from the initial cached value so the refresh is observable.
    const RECREATED_FORMAT_RAW: u32 = 0xBEEF;

    unsafe extern "C" fn fake_begin_frame(
        _h: *const c_void,
        _o: *mut PresentFrameBeginRepr,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        0
    }

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn fake_end_frame(
        _h: *const c_void,
        _r: *const c_void,
        _wp: *const SemaphoreSubmitInfoRepr,
        _wc: usize,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        0
    }

    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn fake_recreate(
        _h: *const c_void,
        _w: u32,
        _ht: u32,
        _c: *const ColorTraitsRepr,
        out_color_format_raw: *mut u32,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        if !out_color_format_raw.is_null() {
            // SAFETY: caller-provided out-pointer (the wrapper's local).
            unsafe { *out_color_format_raw = RECREATED_FORMAT_RAW };
        }
        0
    }

    unsafe extern "C" fn fake_set_hdr_metadata(
        _h: *const c_void,
        _m: *const HdrStaticMetadataRepr,
        _eb: *mut u8,
        _ec: usize,
        _el: *mut usize,
    ) -> i32 {
        0
    }

    static FAKE_METHODS: PresentTargetMethodsVTable = PresentTargetMethodsVTable {
        layout_version: streamlib_plugin_abi::PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        begin_frame: fake_begin_frame,
        end_frame: fake_end_frame,
        recreate: fake_recreate,
        set_hdr_metadata: fake_set_hdr_metadata,
    };

    #[test]
    fn recreate_refreshes_cached_color_format_raw() {
        // Null parent vtable => Drop is a no-op (no real handle to reclaim);
        // a dummy non-null handle so recreate proceeds past the null guard.
        let mut target = PresentTarget {
            handle: 0x1 as *const c_void,
            vtable: std::ptr::null(),
            methods_vtable: &FAKE_METHODS,
            color_format_raw: 42,
            _padding: 0,
        };
        assert_eq!(target.color_format_raw(), 42);
        target
            .recreate(1920, 1080, None)
            .expect("fake recreate returns 0");
        assert_eq!(
            target.color_format_raw(),
            RECREATED_FORMAT_RAW,
            "recreate must refresh the cached color_format_raw from out_color_format_raw"
        );
    }
}
