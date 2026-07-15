// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `PresentTargetMethodsVTable` — per-`PresentTarget` extern "C"
//! dispatch for the present-target surface (#1258).

use core::ffi::c_void;

use crate::repr::{
    ColorTraitsRepr, HdrStaticMetadataRepr, PresentFrameBeginRepr, SemaphoreSubmitInfoRepr,
};

/// Layout version of [`crate::PresentTargetMethodsVTable`].
///
/// - v1: initial shape — `begin_frame`, `end_frame`, `recreate`,
///   `set_hdr_metadata`. Drop-only (`!Clone`): the parent
///   [`crate::GpuContextFullAccessVTable`] carries `drop_present_target`,
///   no clone slot (the Box-shaped `PresentTarget` is single-owner).
pub const PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION: u32 = 1;

/// Per-`PresentTarget` method-dispatch table. Every slot takes only the
/// opaque `present_handle` (`Box::into_raw(Box<VulkanPresentTarget>)`) —
/// the target was minted through the FullAccess `create_present_target`
/// slot and is driven Limited per frame. All swapchain-image acquire /
/// per-image render-finished-semaphore keying (VUID-03868) stays host-
/// side and opaque to the caller across the `begin_frame` / `end_frame`
/// split.
///
/// # Layout discipline
///
/// `layout_version` is pinned at offset 0 forever. New methods append
/// to the end and bump [`PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION`].
#[repr(C)]
pub struct PresentTargetMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally aligned
    /// on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Acquire the next swapchain image, begin + pre-barrier the
    /// internal per-frame recorder, and populate `*out_frame`. On
    /// `OUT_OF_DATE_KHR` returns 0 with `acquired_ok = 0` and
    /// `recorder_handle = 0` (the caller drives `recreate` and does NOT
    /// call `end_frame`). Blocks on timeline slot-reuse wait + acquire.
    pub begin_frame: unsafe extern "C" fn(
        present_handle: *const c_void,
        out_frame: *mut PresentFrameBeginRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Run the post-draw barrier
    /// (`COLOR_ATTACHMENT_OPTIMAL → PRESENT_SRC_KHR`),
    /// `submit_with_semaphores` (image-available wait + render-finished /
    /// frame-timeline signal, plus any `extra_waits`), and
    /// `vkQueuePresentKHR` — all host-internal. `recorder_handle` is the
    /// borrowed recorder from `begin_frame`, checked for identity.
    /// `extra_waits_ptr` / `extra_waits_count` (null / 0 valid) fold
    /// producer-finished timeline waits into the submit wait list. After
    /// `begin_frame` returns `acquired_ok = 1`, `end_frame` MUST be
    /// called exactly once (even on caller draw error).
    pub end_frame: unsafe extern "C" fn(
        present_handle: *const c_void,
        recorder_handle: *const c_void,
        extra_waits_ptr: *const SemaphoreSubmitInfoRepr,
        extra_waits_count: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Recreate the swapchain at the new extent / color traits (null
    /// `color` keeps the legacy SDR pick). `recreate` can flip the
    /// swapchain format (SDR BGRA8 → HDR10); the live format is written
    /// into `*out_color_format_raw` so the cdylib's cached-POD getter
    /// refreshes without waiting for the next `begin_frame`.
    pub recreate: unsafe extern "C" fn(
        present_handle: *const c_void,
        width: u32,
        height: u32,
        color: *const ColorTraitsRepr,
        out_color_format_raw: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Set HDR static metadata (no-op host-side when the colorspace is
    /// not HDR-signaling).
    pub set_hdr_metadata: unsafe extern "C" fn(
        present_handle: *const c_void,
        metadata: *const HdrStaticMetadataRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for PresentTargetMethodsVTable {}
unsafe impl Sync for PresentTargetMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn present_target_methods_vtable_layout() {
        // layout_version (u32) + _reserved_padding (u32) + 4 fn pointers
        // (8 bytes each) = 4 + 4 + 32 = 40 bytes, align 8.
        assert_eq!(size_of::<PresentTargetMethodsVTable>(), 40);
        assert_eq!(align_of::<PresentTargetMethodsVTable>(), 8);
        assert_eq!(offset_of!(PresentTargetMethodsVTable, layout_version), 0);
        assert_eq!(offset_of!(PresentTargetMethodsVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(PresentTargetMethodsVTable, begin_frame), 8);
        assert_eq!(offset_of!(PresentTargetMethodsVTable, end_frame), 16);
        assert_eq!(offset_of!(PresentTargetMethodsVTable, recreate), 24);
        assert_eq!(offset_of!(PresentTargetMethodsVTable, set_hdr_metadata), 32);
    }
}
