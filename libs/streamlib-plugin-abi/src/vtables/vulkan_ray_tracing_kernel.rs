// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanRayTracingKernelMethodsVTable` — per-type vtable for RT-kernel method dispatch.

use core::ffi::c_void;

use crate::repr::RayTracingBindingSpecRepr;

/// Layout version of [`crate::VulkanRayTracingKernelMethodsVTable`].
///
/// - v1: empty shell — pointer plumbing only.
/// - v2: appended typed binding-method slots
///   `set_acceleration_structure` / `set_storage_buffer_pixel` /
///   `set_storage_buffer_storage` / `set_uniform_buffer` /
///   `set_sampled_texture` / `set_storage_image` plus the
///   primitive-argument slots `set_push_constants` / `trace_rays`.
///   Each binding slot carries the matching plugin-handle's raw
///   `Arc::into_raw` pointer; the host wrapper reconstructs the
///   borrow and forwards to the inner kernel. Buffer slots are
///   typed by Rust wrapper to mirror streamlib's typed-wrapper
///   binding-site contract (same shape as the compute-kernel
///   methods vtable v3). Ray-tracing kernels are serial — like
///   compute, they own a single command buffer + fence and have no
///   `frame_index` argument on any slot.
///
/// - v3: appended the `bindings` introspection slot — same shape as
///   the compute-kernel v4 slot but writes `RayTracingBindingSpecRepr`.
pub const VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION: u32 = 3;

/// Per-type method-dispatch vtable for the `VulkanRayTracingKernel`
/// β-shape.
///
/// Mirrors the compute-kernel vtable shape (serial dispatch — one
/// command buffer + fence owned by the kernel, no `frame_index`
/// argument on any slot). The `bindings()` getter and
/// `set_push_constants_value::<T>` (generic) stay `host_inner`-routed
/// — `Vec<RayTracingBindingSpec>` isn't `#[repr(C)]` and the generic
/// reduces to `set_push_constants` for cdylib mode.
#[repr(C)]
pub struct VulkanRayTracingKernelMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Bind a top-level acceleration structure at `binding`. The
    /// slot must be declared as `AccelerationStructure` in the
    /// kernel's binding spec. `acceleration_structure_handle` is
    /// the raw `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`
    /// pointer the plugin handle carries. Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on declaration
    /// mismatch / null handle / wrong AS kind.
    pub set_acceleration_structure: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        binding: u32,
        acceleration_structure_handle: *const c_void,
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

    /// Upload push-constant bytes. `bytes_len` should match the
    /// kernel's declared `push_constants.size` (already cached on
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

    /// Dispatch the kernel: write all staged descriptors, record
    /// bind + push + `cmd_trace_rays_khr`, submit, wait on the
    /// kernel's fence before returning. Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure (missing
    /// binding, unset push-constants, GPU submission error, fence
    /// wait timeout, etc.).
    pub trace_rays: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        width: u32,
        height: u32,
        depth: u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// Read the kernel's binding declarations into `out_specs_buf`.
    /// Same shape as [`crate::VulkanComputeKernelMethodsVTable::bindings`];
    /// writes [`crate::RayTracingBindingSpecRepr`] entries. (Available since v3.)
    pub bindings: unsafe extern "C" fn(
        kernel_handle: *const c_void,
        out_specs_buf: *mut RayTracingBindingSpecRepr,
        out_specs_cap: usize,
        out_specs_len: *mut usize,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for VulkanRayTracingKernelMethodsVTable {}
unsafe impl Sync for VulkanRayTracingKernelMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn vulkan_ray_tracing_kernel_methods_vtable_layout() {
        // v3 (bindings introspection slot added):
        //   layout_version              @ 0   (4 bytes, u32)
        //   _reserved_padding           @ 4   (4 bytes, u32)
        //   set_acceleration_structure  @ 8   (8 bytes, fn pointer)
        //   set_storage_buffer_pixel    @ 16
        //   set_storage_buffer_storage  @ 24
        //   set_uniform_buffer          @ 32
        //   set_sampled_texture         @ 40
        //   set_storage_image           @ 48
        //   set_push_constants          @ 56
        //   trace_rays                  @ 64
        //   bindings                    @ 72
        // Total = 80 bytes, align = 8.
        assert_eq!(size_of::<VulkanRayTracingKernelMethodsVTable>(), 80);
        assert_eq!(align_of::<VulkanRayTracingKernelMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_acceleration_structure),
            8
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_storage_buffer_pixel),
            16
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_storage_buffer_storage),
            24
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_uniform_buffer),
            32
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_sampled_texture),
            40
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_storage_image),
            48
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, set_push_constants),
            56
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, trace_rays),
            64
        );
        assert_eq!(
            offset_of!(VulkanRayTracingKernelMethodsVTable, bindings),
            72
        );
    }
}
