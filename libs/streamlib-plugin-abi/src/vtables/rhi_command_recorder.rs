// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `RhiCommandRecorderMethodsVTable` — per-type method dispatch for `RhiCommandRecorder`.

use core::ffi::c_void;

use crate::repr::{DrawCallRepr, DrawIndexedCallRepr, ImageCopyRegionRepr, SemaphoreSubmitInfoRepr};

/// Layout version of [`crate::RhiCommandRecorderMethodsVTable`].
///
/// - v1: ships six method slots a cdylib camera processor needs
///   to drive the host-owned `RhiCommandRecorder` per frame —
///   `begin`, `record_image_barrier`, `record_buffer_barrier`,
///   `record_dispatch`, `record_copy_image_to_buffer`,
///   `submit_signaling_timeline`. Without these the PluginAbiObject's
///   `host_inner()` / `host_inner_mut()` panic-guards fire from
///   cdylib code on every per-frame call.
/// - v2: appends two PixelBuffer-flavored sibling slots —
///   `record_pixel_buffer_barrier` and
///   `record_copy_image_to_pixel_buffer` — so cdylibs can barrier
///   and copy-image-to into a `PixelBuffer` destination. The v1
///   StorageBuffer-flavored slots are unchanged; the new slots
///   are appended at the end of the struct. This is the
///   "sibling-slot per buffer flavor" pattern documented on
///   `RhiCommandRecorderMethodsVTable` and already used by
///   `VulkanGraphicsKernelMethodsVTable`'s
///   `set_storage_buffer_pixel` / `set_storage_buffer_storage`
///   pair.
/// - v3: #1066 — appends five slots needed for cdylib display
///   processors to drive the swapchain render path through the
///   recorder rather than reaching for `command_buffer_raw()` /
///   `vulkan_device_ref()` / `host_inner_mut()` directly:
///   `record_swapchain_image_barrier` (raw `VkImage` layout
///   transitions on swapchain images — distinct from the v1
///   `record_image_barrier` which takes a `Texture` PluginAbiObject),
///   `cmd_begin_dynamic_rendering` and `cmd_end_dynamic_rendering`
///   (dynamic-rendering pass bracketing against a caller-owned
///   `VkImageView` — no `VkRenderPass` / `VkFramebuffer` needed),
///   `submit_with_semaphores` (general queue submit with
///   variable-length wait/signal semaphore lists — sibling of v1
///   `submit_signaling_timeline` but accepts arbitrary
///   wait/signal sets), and `record_draw` (sibling of v1
///   `record_dispatch` but binds a `VulkanGraphicsKernel`'s
///   graphics pipeline + records `vkCmdDraw`). These slots all
///   take raw `VkImage` / `VkImageView` / `VkSemaphore` handles as
///   `u64` integers; the host materializes the typed `vk::*`
///   wrappers internally.
/// - v4: #1066 — appends `record_draw_indexed` (sibling of v3
///   `record_draw` for `vkCmdDrawIndexed`). Added eagerly alongside
///   `record_draw` rather than waiting for a consumer because the
///   marginal cost of mirroring the non-indexed slot is trivial and
///   the asymmetry would force the next "first indexed-draw cdylib
///   consumer" to re-derive the same engine work mid-task. Same
///   wire shape as `record_draw` but takes a `DrawIndexedCallRepr`.
/// - v5: appends `submit` and `submit_and_wait` (siblings of v1
///   `submit_signaling_timeline`). `RhiToneMapper::apply_with_layouts`
///   creates its own recorder + calls `submit_and_wait()`; when
///   engine SDK code that reaches the tone-mapper is compiled into
///   a cdylib (the per-input working-space conversion path in
///   graphics-kernel wrappers is the first in-tree consumer), the
///   bare-submit calls trip the recorder's `host_inner_mut()`
///   panic guard. Same wire shape as `submit_signaling_timeline`
///   minus the timeline parameters.
pub const RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION: u32 = 5;

/// Per-type method-dispatch vtable for the `RhiCommandRecorder`
/// PluginAbiObject (Phase E sub-lift slice B — #984).
///
/// `RhiCommandRecorder` keeps `clone_command_recorder` /
/// `drop_command_recorder` dispatch on the parent
/// [`crate::GpuContextFullAccessVTable`]; this vtable carries per-method
/// slots for the six camera-hot-path methods cdylib code needs to
/// dispatch through (`begin`, `record_image_barrier`,
/// `record_buffer_barrier`, `record_dispatch`,
/// `record_copy_image_to_buffer`, `submit_signaling_timeline`).
/// Without these the PluginAbiObject's `host_inner_mut()` / `host_inner()`
/// panic-guards fire from cdylib code on every per-frame call.
///
/// v3 (#1066) appends `record_swapchain_image_barrier`,
/// `cmd_begin_dynamic_rendering`, `cmd_end_dynamic_rendering`,
/// `submit_with_semaphores`, and `record_draw` for cdylib display
/// processors. v4 (same milestone) appends `record_draw_indexed`
/// eagerly alongside `record_draw` — the marginal cost of mirroring
/// the non-indexed slot is trivial and the asymmetry would force
/// the next indexed-draw cdylib consumer to re-derive the engine
/// work mid-task. The remaining `RhiCommandRecorder` methods
/// (`record_copy_buffer_to_image`, `submit`, `submit_and_wait`)
/// keep their cdylib-mode panic in place — they don't sit on any
/// cdylib hot path today and a follow-up slice lifts them when a
/// consumer arrives.
///
/// **Buffer-flavor coverage today:** the v1 `record_buffer_barrier`
/// and `record_copy_image_to_buffer` slots accept a
/// `StorageBuffer`-shaped handle. v2 added `record_pixel_buffer_barrier`
/// and `record_copy_image_to_pixel_buffer` sibling slots — the
/// camera's per-frame path copies the compute output into a pooled
/// `PixelBuffer` and barriers it through `HOST_READ`. Future
/// consumers needing uniform / vertex / index buffer barriers add
/// further sibling slots rather than discriminating on these (same
/// pattern as `VulkanGraphicsKernelMethodsVTable`'s
/// `set_storage_buffer_pixel` / `set_storage_buffer_storage`).
#[repr(C)]
pub struct RhiCommandRecorderMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Begin a new recording. `recorder_handle` is the
    /// `Box::into_raw(Box<RhiCommandRecorderInner>)` pointer from
    /// the PluginAbiObject's `handle` field. Returns 0 on success; non-zero
    /// with UTF-8 message in `err_buf` on failure. Linux-only on the
    /// host side; non-Linux stubs return non-zero.
    pub begin: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record an image layout transition. Layout / stage / access
    /// enumerants travel as their raw integer types: `VulkanLayout`
    /// is `i32` (matches the `VkImageLayout` enumerant);
    /// `VulkanStage` and `VulkanAccess` are `i64` (the
    /// `VK_PIPELINE_STAGE_2_*` and `VK_ACCESS_2_*` bitmasks are
    /// 64-bit).
    ///
    /// - `recorder_handle` is the
    ///   `Box::into_raw(Box<RhiCommandRecorderInner>)` pointer.
    /// - `texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the
    ///   `Texture` PluginAbiObject's `handle` field (borrowed).
    pub record_image_barrier: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        texture_handle: *const c_void,
        from_layout_raw: i32,
        to_layout_raw: i32,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record a buffer memory barrier covering the whole buffer.
    /// Today the camera only uses storage-buffer barriers; this
    /// slot accepts a `StorageBuffer`-shaped handle. The host
    /// reconstructs the typed borrow via
    /// `make_storage_buffer_borrow`. Future consumers needing
    /// uniform / vertex / index buffer barriers add sibling slots,
    /// not a discriminator on this one.
    ///
    /// - `storage_buffer_handle` is
    ///   `Arc::into_raw(Arc<HostVulkanBufferInner>)`-shaped from
    ///   the `StorageBuffer` PluginAbiObject's `handle` field (borrowed).
    pub record_buffer_barrier: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        storage_buffer_handle: *const c_void,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record a compute dispatch via
    /// `VulkanComputeKernel::record`. `kernel_handle` is the
    /// `Arc::into_raw(Arc<VulkanComputeKernelInner>)` pointer from
    /// the kernel PluginAbiObject's `handle` field (borrowed).
    pub record_dispatch: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        kernel_handle: *const c_void,
        group_x: u32,
        group_y: u32,
        group_z: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record `vkCmdCopyImageToBuffer`. Storage-buffer-shape
    /// destination only today; mirrors the
    /// `record_buffer_barrier` constraint.
    ///
    /// - `src_texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the source
    ///   `Texture` PluginAbiObject's `handle` field (borrowed).
    /// - `dst_storage_buffer_handle` is
    ///   `Arc::into_raw(Arc<HostVulkanBufferInner>)`-shaped from the
    ///   destination `StorageBuffer` PluginAbiObject's `handle` field
    ///   (borrowed).
    /// - `region` points at an [`crate::ImageCopyRegionRepr`] the host
    ///   reads once at call time.
    pub record_copy_image_to_buffer: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        src_texture_handle: *const c_void,
        src_layout_raw: i32,
        dst_storage_buffer_handle: *const c_void,
        region: *const ImageCopyRegionRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End recording and submit, signaling `timeline` at
    /// `signal_value` on completion. `timeline_handle` is a borrow
    /// of `&HostVulkanTimelineSemaphore` (`self as *const Self`
    /// shape — same pattern as the v13 `wait_timeline_semaphore`
    /// slot on `GpuContextLimitedAccessVTable`). The host does not
    /// bump the timeline's refcount.
    pub submit_signaling_timeline: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        timeline_handle: *const c_void,
        signal_value: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// v2 sibling of `record_buffer_barrier` for `PixelBuffer`-shaped
    /// destinations. The camera's per-frame path uses this after the
    /// `vkCmdCopyImageToBuffer` to barrier the pooled pixel buffer
    /// from `TRANSFER_WRITE` to `HOST_READ` so the IPC consumer can
    /// map it.
    ///
    /// - `pixel_buffer_handle` is
    ///   `Arc::into_raw(Arc<PixelBufferRef>)`-shaped from the
    ///   `PixelBuffer` PluginAbiObject's `handle` field (borrowed).
    pub record_pixel_buffer_barrier: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        pixel_buffer_handle: *const c_void,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// v2 sibling of `record_copy_image_to_buffer` for
    /// `PixelBuffer`-shaped destinations. The camera's per-frame
    /// path copies the compute output (a host-allocated
    /// `Texture` ring slot) into a pooled `PixelBuffer` for
    /// cross-process IPC.
    ///
    /// - `src_texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the source
    ///   `Texture` PluginAbiObject's `handle` field (borrowed).
    /// - `dst_pixel_buffer_handle` is
    ///   `Arc::into_raw(Arc<PixelBufferRef>)`-shaped from the
    ///   destination `PixelBuffer` PluginAbiObject's `handle` field
    ///   (borrowed).
    /// - `region` points at an [`crate::ImageCopyRegionRepr`] the host
    ///   reads once at call time.
    pub record_copy_image_to_pixel_buffer: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        src_texture_handle: *const c_void,
        src_layout_raw: i32,
        dst_pixel_buffer_handle: *const c_void,
        region: *const ImageCopyRegionRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    // -------------------------------------------------------------------------
    // v3 entries (#1066) — swapchain render-path slots.
    // -------------------------------------------------------------------------

    /// Record a layout transition on a raw `VkImage` handle —
    /// distinct from v1 [`Self::record_image_barrier`] which takes
    /// a `Texture` PluginAbiObject. Used by `VulkanPresentTarget` to
    /// transition swapchain images (UNDEFINED →
    /// COLOR_ATTACHMENT_OPTIMAL and COLOR_ATTACHMENT_OPTIMAL →
    /// PRESENT_SRC_KHR) between cdylib-driven render-frame
    /// iterations. The image is COLOR-aspect, single mip, single
    /// array layer, QUEUE_FAMILY_IGNORED on both sides.
    ///
    /// - `image_raw` is the raw `VkImage` handle (`u64`).
    /// - Layout / stage / access enumerants follow v1
    ///   `record_image_barrier`'s integer-typed wire format.
    pub record_swapchain_image_barrier: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        image_raw: u64,
        from_layout_raw: i32,
        to_layout_raw: i32,
        from_stage_raw: i64,
        to_stage_raw: i64,
        from_access_raw: i64,
        to_access_raw: i64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Begin a dynamic-rendering pass against a caller-owned
    /// `VkImageView`. Used by `VulkanPresentTarget::PresentFrame`
    /// to open the swapchain-image render pass. CLEAR load op is
    /// selected by `has_clear_color = 1`; LOAD by
    /// `has_clear_color = 0`.
    ///
    /// - `image_view_raw` is the raw `VkImageView` handle (`u64`).
    /// - `extent_w` / `extent_h` are the render-area extents in
    ///   pixels.
    /// - `clear_rgba` is the CLEAR color (ignored when
    ///   `has_clear_color == 0`).
    pub cmd_begin_dynamic_rendering: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        image_view_raw: u64,
        extent_w: u32,
        extent_h: u32,
        has_clear_color: u32,
        clear_r: f32,
        clear_g: f32,
        clear_b: f32,
        clear_a: f32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Close the dynamic-rendering pass opened by
    /// [`Self::cmd_begin_dynamic_rendering`].
    pub cmd_end_dynamic_rendering: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End recording and submit with arbitrary wait + signal
    /// semaphore lists. Sibling of v1
    /// [`Self::submit_signaling_timeline`] — that slot only
    /// covered the single-timeline-signal case the camera
    /// processor needed; this one covers the general case
    /// `VulkanPresentTarget::render_frame` uses (wait on
    /// image-available binary + caller-added timeline waits;
    /// signal render-finished binary + frame-timeline).
    ///
    /// - `waits_ptr` / `waits_count` and `signals_ptr` /
    ///   `signals_count` describe two arrays of
    ///   [`crate::SemaphoreSubmitInfoRepr`]. Empty arrays are valid
    ///   (`*_ptr` may be null when `*_count == 0`).
    pub submit_with_semaphores: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        waits_ptr: *const SemaphoreSubmitInfoRepr,
        waits_count: usize,
        signals_ptr: *const SemaphoreSubmitInfoRepr,
        signals_count: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record a non-indexed draw via
    /// `VulkanGraphicsKernel::cmd_bind_and_draw`. Sibling of v1
    /// [`Self::record_dispatch`] (compute) for graphics pipelines.
    /// Bindings + push constants for `frame_index` must have been
    /// staged via the kernel's `set_*` methods before this call.
    ///
    /// - `kernel_handle` is the
    ///   `Arc::into_raw(Arc<VulkanGraphicsKernelInner>)` pointer
    ///   from the kernel PluginAbiObject's `handle` field (borrowed).
    /// - `draw` points at a [`crate::DrawCallRepr`] the host reads once
    ///   at call time.
    pub record_draw: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        kernel_handle: *const c_void,
        frame_index: u32,
        draw: *const DrawCallRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Indexed-draw sibling of [`Self::record_draw`]. Caller must
    /// have bound an index buffer for `frame_index` via the kernel's
    /// `set_index_buffer` before this call. Same wire convention as
    /// `record_draw`; `draw` points at a [`crate::DrawIndexedCallRepr`].
    pub record_draw_indexed: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        kernel_handle: *const c_void,
        frame_index: u32,
        draw: *const DrawIndexedCallRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End recording and submit without semaphore signaling. Sibling
    /// of v1 [`Self::submit_signaling_timeline`] — that slot covered
    /// the timeline-signal case the camera processor needed; this
    /// covers the bare-submit case `RhiToneMapper::apply_with_layouts`
    /// uses for its private recorder. (Available since v5.)
    pub submit: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End recording, submit, and block until the GPU completes.
    /// Sibling of [`Self::submit`] — caller-side `vkWaitForFences`
    /// after submit. Used by
    /// `RhiToneMapper::apply_with_layouts` for its self-contained
    /// dispatch-then-wait flow. (Available since v5.)
    pub submit_and_wait: unsafe extern "C" fn(
        recorder_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for RhiCommandRecorderMethodsVTable {}
unsafe impl Sync for RhiCommandRecorderMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn rhi_command_recorder_methods_vtable_layout() {
        // v2 (v1 unchanged through @48, sibling slots appended):
        //   layout_version                       @ 0   (4 bytes, u32)
        //   _reserved_padding                    @ 4   (4 bytes, u32)
        //   begin                                @ 8   (8 bytes, fn pointer)
        //   record_image_barrier                 @ 16  (8 bytes, fn pointer)
        //   record_buffer_barrier                @ 24  (8 bytes, fn pointer)
        //   record_dispatch                      @ 32  (8 bytes, fn pointer)
        //   record_copy_image_to_buffer          @ 40  (8 bytes, fn pointer)
        //   submit_signaling_timeline            @ 48  (8 bytes, fn pointer)
        //   record_pixel_buffer_barrier          @ 56  (8 bytes, fn pointer, v2)
        //   record_copy_image_to_pixel_buffer    @ 64  (8 bytes, fn pointer, v2)
        // Total = 72 bytes, align = 8.
        // layout_version (u32) + _reserved_padding (u32) + 16 fn
        // pointers (8 bytes each) = 4 + 4 + 128 = 136 bytes, align = 8.
        assert_eq!(size_of::<RhiCommandRecorderMethodsVTable>(), 136);
        assert_eq!(align_of::<RhiCommandRecorderMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, begin),
            8
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_image_barrier),
            16
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_buffer_barrier),
            24
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_dispatch),
            32
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                record_copy_image_to_buffer
            ),
            40
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                submit_signaling_timeline
            ),
            48
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                record_pixel_buffer_barrier
            ),
            56
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                record_copy_image_to_pixel_buffer
            ),
            64
        );
        // v3 entries (#1066).
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                record_swapchain_image_barrier
            ),
            72
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                cmd_begin_dynamic_rendering
            ),
            80
        );
        assert_eq!(
            offset_of!(
                RhiCommandRecorderMethodsVTable,
                cmd_end_dynamic_rendering
            ),
            88
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, submit_with_semaphores),
            96
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_draw),
            104
        );
        // v4 entry (#1066).
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, record_draw_indexed),
            112
        );
        // v5 entries.
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, submit),
            120
        );
        assert_eq!(
            offset_of!(RhiCommandRecorderMethodsVTable, submit_and_wait),
            128
        );
    }
}
