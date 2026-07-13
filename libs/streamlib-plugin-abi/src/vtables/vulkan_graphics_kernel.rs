// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanGraphicsKernelMethodsVTable` — per-type vtable for graphics-kernel method dispatch.

use core::ffi::c_void;

use crate::repr::{DrawCallRepr, DrawIndexedCallRepr, GraphicsBindingSpecRepr, OffscreenDrawRepr};

/// Layout version of [`crate::VulkanGraphicsKernelMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: appended typed binding-method slots
///   `set_storage_buffer_pixel` / `set_storage_buffer_storage` /
///   `set_uniform_buffer` / `set_sampled_texture` /
///   `set_storage_image` / `set_vertex_buffer` / `set_index_buffer`
///   plus the primitive-argument slots `set_push_constants` /
///   `offscreen_render`. Each binding slot carries the matching
///   plugin-handle's raw `Arc::into_raw` pointer; the host wrapper
///   reconstructs the borrow and forwards to the inner kernel.
///   Buffer slots are typed by Rust wrapper to mirror streamlib's
///   typed-wrapper binding-site contract (same shape as the
///   compute-kernel methods vtable v3).
///
/// - v3: appended the `bindings` introspection slot — same shape as
///   the compute-kernel v4 slot but writes `GraphicsBindingSpecRepr`.
///
/// - v4: appended the raw-vulkanalia-handle slots
///   `cmd_bind_and_draw` / `cmd_bind_and_draw_indexed` so cdylib
///   graphics consumers that mint their own `vk::CommandBuffer` can
///   record bind + push + draw without tripping the kernel's
///   `host_inner()` panic guard. Same shape as the
///   `VulkanComputeKernelMethodsVTable` v5 `record` slot —
///   `command_buffer_handle: u64` wraps `vk::CommandBuffer`'s
///   `repr(transparent)` `usize` payload (lossless on 64-bit Linux,
///   the only platform that ships); the draw call travels as the
///   existing [`crate::DrawCallRepr`] / [`crate::DrawIndexedCallRepr`] shapes
///   already wired for `offscreen_render`.
pub const VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION: u32 = 4;

/// Per-type method-dispatch vtable for the `VulkanGraphicsKernel`
/// PluginAbiObject (issue #907 Phase E PR 3/5 + #951 method-dispatch slice).
///
/// `VulkanGraphicsKernel` keeps `clone_*` / `drop_*` dispatch on the
/// parent [`crate::GpuContextFullAccessVTable`] (PR #918's Phase D shape);
/// this vtable carries per-method slots for the plugin handle's
/// binding + draw surface that cdylib code needs to dispatch through.
///
/// **Binding-method shape:** typed-by-input-wrapper (one slot per
/// kernel-method × buffer-or-texture wrapper). Mirrors the
/// `VulkanComputeKernelMethodsVTable` v3 shape and the production
/// plugin ABI patterns in Dawn / WebGPU + Unreal RHI.
///
/// **Coverage today** (v4):
/// - Binding slots: `set_storage_buffer_pixel`,
///   `set_storage_buffer_storage`, `set_uniform_buffer`,
///   `set_sampled_texture`, `set_storage_image`,
///   `set_vertex_buffer`, `set_index_buffer`.
/// - Primitive-argument slots: `set_push_constants`,
///   `offscreen_render`.
/// - Introspection slots: `bindings` (v3).
/// - Raw-vulkanalia-handle slots: `cmd_bind_and_draw`,
///   `cmd_bind_and_draw_indexed` (v4). Mirror the
///   `VulkanComputeKernelMethodsVTable` v5 `record` slot — they
///   accept a `command_buffer_handle: u64` carrying
///   `vk::CommandBuffer`'s `repr(transparent)` `usize` payload from
///   cdylib graphics consumers that mint and manage their own
///   command buffer (the example
///   `camera-python-display-effects` kernel wrappers, plus future
///   pre-RDG cdylib pass authors). The host wrapper reconstructs
///   the handle via `vk::CommandBuffer::from_raw` before dispatch.
///
/// **Engine-only methods** (NOT on this vtable): `bindings()` /
/// `pipeline_state()` accessors return host-internal types and
/// stay `host_inner`-routed for that reason.
#[repr(C)]
pub struct VulkanGraphicsKernelMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Bind a [`PixelBuffer`](struct@crate)-shaped storage buffer
    /// (SSBO) at `(frame_index, binding)`. `pixel_buffer_handle` is
    /// the raw `Arc::into_raw(Arc<PixelBufferRef>)` pointer the
    /// plugin handle carries. Returns 0 on success; non-zero with
    /// UTF-8 message in `err_buf` on failure.
    pub set_storage_buffer_pixel: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        pixel_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw-bytes storage buffer at `(frame_index, binding)`.
    /// `storage_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    pub set_storage_buffer_storage: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        storage_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a uniform buffer (UBO) at `(frame_index, binding)`.
    /// `uniform_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    pub set_uniform_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        uniform_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a sampled texture at `(frame_index, binding)` using the
    /// kernel's default linear-clamp sampler.
    pub set_sampled_texture: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a storage image at `(frame_index, binding)`. Caller
    /// guarantees the underlying texture's `STORAGE_BINDING` usage
    /// was declared at creation time.
    pub set_storage_image: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a vertex buffer at `(frame_index, binding)`. `binding`
    /// must match a `VertexInputBinding` declared in the pipeline's
    /// vertex input state. `vertex_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    pub set_vertex_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        binding: u32,
        vertex_buffer_handle: *const c_void,
        offset: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind an index buffer at `frame_index`. `index_buffer_handle`
    /// is the raw `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer.
    /// `index_type` is the [`crate::IndexTypeRepr`] discriminant.
    pub set_index_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        index_buffer_handle: *const c_void,
        offset: u64,
        index_type: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Stage push-constant bytes for `frame_index`. `bytes_len`
    /// should match the kernel's declared `push_constants.size`
    /// (already cached on the plugin handle). Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure.
    pub set_push_constants: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        bytes_ptr: *const u8,
        bytes_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Render into one or more offscreen color attachments using the
    /// kernel's owned command buffer + fence. Convenience for
    /// one-shot renderers (tests, smoke harnesses).
    ///
    /// Color targets travel as parallel arrays of the same length
    /// `target_count`:
    /// - `color_texture_handles`: `Arc::into_raw(Arc<TextureInner>)`
    ///   pointers, one per attachment.
    /// - `color_clear_present`: `1` per attachment that wants a
    ///   CLEAR load_op; `0` for LOAD.
    /// - `color_clear_values`: RGBA float clear color per
    ///   attachment; read only when the matching present flag is `1`.
    ///
    /// `draw` is the [`crate::OffscreenDrawRepr`] tagged union (only the
    /// `kind`-matched payload is read on the host side).
    pub offscreen_render: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        frame_index: u32,
        color_texture_handles: *const *const c_void,
        color_clear_present: *const u32,
        color_clear_values: *const [f32; 4],
        target_count: usize,
        extent_width: u32,
        extent_height: u32,
        draw: *const OffscreenDrawRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Read the kernel's binding declarations into `out_specs_buf`.
    /// Same shape as [`crate::VulkanComputeKernelMethodsVTable::bindings`];
    /// writes [`crate::GraphicsBindingSpecRepr`] entries. (Available since v3.)
    pub bindings: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        out_specs_buf: *mut GraphicsBindingSpecRepr,
        out_specs_cap: usize,
        out_specs_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record bind + push + draw into a caller-owned command buffer.
    /// `command_buffer_handle` carries `vk::CommandBuffer`'s
    /// `repr(transparent)` `usize` payload as a `u64` (lossless on
    /// 64-bit Linux). `draw` is the [`crate::DrawCallRepr`] mirror of
    /// `streamlib::core::rhi::DrawCall` already wired for
    /// `offscreen_render`. Returns 0 on success; non-zero with UTF-8
    /// message in `err_buf` on failure. (Available since v4.)
    pub cmd_bind_and_draw: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        command_buffer_handle: u64,
        frame_index: u32,
        draw: *const DrawCallRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Indexed variant of [`Self::cmd_bind_and_draw`]. Caller must
    /// have set an index buffer at `frame_index` via
    /// [`Self::set_index_buffer`]. (Available since v4.)
    pub cmd_bind_and_draw_indexed: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        command_buffer_handle: u64,
        frame_index: u32,
        draw: *const DrawIndexedCallRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanGraphicsKernelMethodsVTable {}
unsafe impl Sync for VulkanGraphicsKernelMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_graphics_kernel_methods_vtable_layout() {
        // v3 (bindings introspection slot added):
        //   layout_version              @ 0   (4 bytes, u32)
        //   _reserved_padding           @ 4   (4 bytes, u32)
        //   set_storage_buffer_pixel    @ 8   (8 bytes, fn pointer)
        //   set_storage_buffer_storage  @ 16
        //   set_uniform_buffer          @ 24
        //   set_sampled_texture         @ 32
        //   set_storage_image           @ 40
        //   set_vertex_buffer           @ 48
        //   set_index_buffer            @ 56
        //   set_push_constants          @ 64
        //   offscreen_render            @ 72
        //   bindings                    @ 80
        // Total = 88 bytes, align = 8.
        assert_eq!(align_of::<VulkanGraphicsKernelMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_storage_buffer_pixel),
            8
        );
        assert_eq!(
            offset_of!(
                VulkanGraphicsKernelMethodsVTable,
                set_storage_buffer_storage
            ),
            16
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_uniform_buffer),
            24
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_sampled_texture),
            32
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_storage_image),
            40
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_vertex_buffer),
            48
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_index_buffer),
            56
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, set_push_constants),
            64
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, offscreen_render),
            72
        );
        assert_eq!(offset_of!(VulkanGraphicsKernelMethodsVTable, bindings), 80);
        // v4 appended slots:
        //   cmd_bind_and_draw           @ 88
        //   cmd_bind_and_draw_indexed   @ 96
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, cmd_bind_and_draw),
            88
        );
        assert_eq!(
            offset_of!(VulkanGraphicsKernelMethodsVTable, cmd_bind_and_draw_indexed),
            96
        );
        // v4: 12 fn pointers @ 8 bytes each + 2 u32 = 104 bytes.
        assert_eq!(size_of::<VulkanGraphicsKernelMethodsVTable>(), 104);
    }
}
