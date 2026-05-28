// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanComputeKernelMethodsVTable` — per-type vtable for compute-kernel method dispatch.

use core::ffi::c_void;

use crate::repr::ComputeBindingSpecRepr;

/// Layout version of [`crate::VulkanComputeKernelMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: appended `set_push_constants` / `dispatch` slots (primitive
///   arguments only).
/// - v3: appended typed binding-method slots
///   `set_storage_buffer_pixel` / `set_storage_buffer_storage` /
///   `set_uniform_buffer` / `set_sampled_texture` /
///   `set_storage_image`. Each carries the matching plugin-handle's
///   raw `Arc::into_raw` pointer; the host wrapper reconstructs the
///   borrow and forwards to the inner kernel. Buffer slots are typed
///   by Rust wrapper to mirror streamlib's typed-wrapper binding-site
///   contract.
///
/// - v4: appended the `bindings` introspection slot. Writes the
///   kernel's binding declarations into a caller-provided
///   `[ComputeBindingSpecRepr]` buffer and reports the actual count.
///   Status code 2 signals "buffer too small" with the required count
///   in `out_specs_len`; callers reallocate and retry. Replaces the
///   β-shape's bare `host_inner().bindings()` call which panicked
///   from cdylib code.
///
/// - v5: appended four raw-vulkanalia-handle slots —
///   `set_sampled_image_view`, `set_combined_image_sampler_view`,
///   `set_storage_image_view`, `record`. Each carries the raw
///   `vk::ImageView` / `vk::CommandBuffer` handle as a `u64`
///   (vulkanalia's handles are `#[repr(transparent)] pub struct
///   ImageView(u64)` / `CommandBuffer(usize)`, so the wire form is the
///   raw integer the host reconstructs via `Handle::from_raw`).
///   Replaces the β-shape's bare `host_inner().set_*_view(...)` /
///   `host_inner().record(...)` calls that engine SDK code
///   (`RgbToNv12Converter::convert`, `Nv12ToRgbConverter::convert`)
///   reaches per-frame from cdylib-resident processor bodies (#1073).
pub const VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION: u32 = 5;

/// Per-type method-dispatch vtable for the `VulkanComputeKernel`
/// β-shape (issue #907 Phase E + #949 method-dispatch first slice).
///
/// `VulkanComputeKernel` keeps `clone_*` / `drop_*` dispatch on the
/// parent [`crate::GpuContextFullAccessVTable`] (PR #918's Phase D shape);
/// this vtable carries per-method slots for everything the plugin
/// handle exposes that cdylib code needs to dispatch through.
///
/// **Binding-method shape:** typed-by-input-wrapper (one slot per
/// kernel-method × buffer-or-texture wrapper). This mirrors the
/// production cross-DSO pattern used by Dawn / WebGPU (`WGPUBuffer`
/// + per-binding-kind method) and Unreal RHI (typed
/// `SetShaderResourceViewParameter` methods) while honoring
/// streamlib's existing typed-wrapper allocation layer (separate
/// `PixelBuffer` / `StorageBuffer` / `UniformBuffer` Rust types).
/// The longer-term option of collapsing typed wrappers into one
/// `Buffer` + flags primitive is tracked separately in the
/// **RHI Buffer Model Alignment** milestone and would simplify this
/// vtable further; until then, per-type slots are the right shape.
///
/// **Coverage today** (v5): all v3 binding-method slots
/// (`set_push_constants`, `dispatch`, `set_storage_buffer_pixel`,
/// `set_storage_buffer_storage`, `set_uniform_buffer`,
/// `set_sampled_texture`, `set_storage_image`), v4's `bindings`
/// introspection slot, plus v5's four raw-vulkanalia-handle slots
/// (`set_sampled_image_view`, `set_combined_image_sampler_view`,
/// `set_storage_image_view`, `record`). The image-view-by-handle
/// trio carries `vk::ImageView` as `u64`; the `record` slot carries
/// `vk::CommandBuffer` as `u64`. Vulkanalia handles are
/// `#[repr(transparent)]` wrappers over their raw `u64`/`usize`
/// integer, so the wire form is the raw integer and the host
/// reconstructs via `Handle::from_raw` before forwarding.
#[repr(C)]
pub struct VulkanComputeKernelMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Upload push-constant bytes. `bytes_len` should match the
    /// kernel's declared `push_constant_size` (already cached on
    /// the plugin handle). Returns 0 on success; non-zero with UTF-8
    /// message in `err_buf` on failure.
    pub set_push_constants: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        bytes_ptr: *const u8,
        bytes_len: usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Dispatch the kernel with the given workgroup counts. Returns
    /// 0 on success; non-zero with UTF-8 message in `err_buf` on
    /// failure (GPU submission error, fence wait timeout, etc.).
    pub dispatch: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        group_x: u32,
        group_y: u32,
        group_z: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a [`PixelBuffer`](struct@crate)-shaped storage buffer
    /// (SSBO) at `binding`. `pixel_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<PixelBufferRef>)` pointer the plugin
    /// handle carries; the host wrapper reconstructs the borrow and
    /// forwards. Returns 0 on success; non-zero with UTF-8 message
    /// in `err_buf` on declaration mismatch / unset binding / null
    /// handle.
    pub set_storage_buffer_pixel: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        pixel_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw-bytes-shaped storage buffer (SSBO) at `binding`.
    /// `storage_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer the plugin
    /// handle carries.
    pub set_storage_buffer_storage: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        storage_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a uniform buffer (UBO) at `binding`.
    /// `uniform_buffer_handle` is the raw
    /// `Arc::into_raw(Arc<HostVulkanBuffer>)` pointer the plugin
    /// handle carries.
    pub set_uniform_buffer: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        uniform_buffer_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a sampled texture at `binding` using the kernel's
    /// default linear-clamp sampler. `texture_handle` is the raw
    /// `Arc::into_raw(Arc<TextureInner>)` pointer the plugin
    /// handle carries.
    pub set_sampled_texture: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a storage image at `binding`. Caller guarantees the
    /// underlying texture's `STORAGE_BINDING` usage was declared at
    /// creation time.
    pub set_storage_image: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        texture_handle: *const c_void,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Read the kernel's binding declarations into `out_specs_buf`.
    /// On success, writes the actual count into `out_specs_len` and
    /// returns 0. If `out_specs_cap` is smaller than the actual count,
    /// writes nothing into `out_specs_buf`, still writes the actual
    /// count into `out_specs_len`, and returns 2 — the caller
    /// reallocates with the now-known size and calls again. Returns 1
    /// with UTF-8 message in `err_buf` for null-handle / null-out-ptr
    /// / panic; v4 (introspection). (Available since v4.)
    pub bindings: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        out_specs_buf: *mut ComputeBindingSpecRepr,
        out_specs_cap: usize,
        out_specs_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw `vk::ImageView` to a sampled-image binding.
    /// `image_view_handle` is the raw `u64` from
    /// `vk::ImageView::as_raw()`; the host wrapper reconstructs the
    /// vulkanalia handle via `vk::ImageView::from_raw` before
    /// forwarding. Returns 0 on success; non-zero with UTF-8 message
    /// in `err_buf` on declaration mismatch / null handle / unset
    /// binding. v5.
    pub set_sampled_image_view: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        image_view_handle: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw `vk::ImageView` to a sampled-texture binding that
    /// was declared with an immutable sampler (combined-image-sampler
    /// shape, view-only write). v5. Same `u64`-handle wire shape as
    /// [`Self::set_sampled_image_view`].
    pub set_combined_image_sampler_view: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        image_view_handle: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Bind a raw `vk::ImageView` to a storage-image binding. v5.
    /// Same `u64`-handle wire shape as [`Self::set_sampled_image_view`].
    pub set_storage_image_view: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        image_view_handle: u64,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Record bind + push-constants + dispatch into a caller-owned
    /// command buffer. `command_buffer_handle` is the raw `u64` from
    /// `vk::CommandBuffer::as_raw() as u64` (vulkanalia stores the
    /// dispatchable handle as `usize`; on every supported target
    /// `usize == u64`). The host wrapper reconstructs the vulkanalia
    /// handle via `vk::CommandBuffer::from_raw(handle as usize)`
    /// before forwarding. Returns 0 on success; non-zero with UTF-8
    /// message in `err_buf` for null kernel handle / dispatch error.
    /// v5.
    pub record: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        command_buffer_handle: u64,
        group_x: u32,
        group_y: u32,
        group_z: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanComputeKernelMethodsVTable {}
unsafe impl Sync for VulkanComputeKernelMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_compute_kernel_methods_vtable_layout() {
        // v5 (raw-vulkanalia-handle slots appended for #1073):
        //   layout_version                    @ 0   (4 bytes, u32)
        //   _reserved_padding                 @ 4   (4 bytes, u32)
        //   set_push_constants                @ 8   (8 bytes, fn pointer)
        //   dispatch                          @ 16
        //   set_storage_buffer_pixel          @ 24
        //   set_storage_buffer_storage        @ 32
        //   set_uniform_buffer                @ 40
        //   set_sampled_texture               @ 48
        //   set_storage_image                 @ 56
        //   bindings                          @ 64
        //   set_sampled_image_view            @ 72   (v5)
        //   set_combined_image_sampler_view   @ 80   (v5)
        //   set_storage_image_view            @ 88   (v5)
        //   record                            @ 96   (v5)
        // Total = 104 bytes, align = 8.
        assert_eq!(size_of::<VulkanComputeKernelMethodsVTable>(), 104);
        assert_eq!(align_of::<VulkanComputeKernelMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_push_constants),
            8
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, dispatch),
            16
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_storage_buffer_pixel),
            24
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_storage_buffer_storage),
            32
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_uniform_buffer),
            40
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_sampled_texture),
            48
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_storage_image),
            56
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, bindings),
            64
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_sampled_image_view),
            72
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_combined_image_sampler_view),
            80
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, set_storage_image_view),
            88
        );
        assert_eq!(
            offset_of!(VulkanComputeKernelMethodsVTable, record),
            96
        );
    }
}
