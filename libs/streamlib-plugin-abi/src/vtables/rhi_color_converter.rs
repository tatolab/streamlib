// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `RhiColorConverterMethodsVTable` — per-type method dispatch for `RhiColorConverter`.

use core::ffi::c_void;

use crate::repr::{ResolvedColorInfoRepr, SourceLayoutInfoRepr};

/// Layout version of [`crate::RhiColorConverterMethodsVTable`].
///
/// - v1: ships the `prepare_buffer_to_image_storage` slot — the
///   minimum surface a cdylib camera processor needs to dispatch
///   YCbCr→RGBA conversion through the host's cached buffer→image
///   kernel without panicking at the PluginAbiObject's host-mode-only
///   `host_inner()` access. Out-params return an opaque
///   `Arc<VulkanComputeKernelInner>`-shaped handle plus the kernel's
///   `push_constant_size` POD so the cdylib can reconstruct a
///   `VulkanComputeKernel` PluginAbiObject via the parent FullAccess vtable's
///   per-type methods vtable lookup.
///
/// - v2: appends three sibling slots completing the Phase E sub-lift —
///   `prepare_buffer_to_image_pixel` for the `PixelBuffer`-shape
///   source variant, plus `convert_buffer_to_image_storage` and
///   `convert_buffer_to_image_pixel` for callers that don't drive
///   dispatch through a recorder. Before v2 these methods bare-
///   called `host_inner()` and panicked from cdylib code.
pub const RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION: u32 = 2;

/// Per-type method-dispatch vtable for the `RhiColorConverter`
/// PluginAbiObject (Phase E sub-lift slice A).
///
/// `RhiColorConverter` keeps `clone_color_converter` /
/// `drop_color_converter` dispatch on the parent
/// [`crate::GpuContextFullAccessVTable`]; this vtable carries the
/// `prepare_buffer_to_image_storage` slot so cdylib camera processors
/// can prepare the host's cached buffer→image kernel without tripping
/// the PluginAbiObject's host-mode-only `host_inner()` access. The slot
/// returns an opaque `Arc<VulkanComputeKernelInner>`-shaped handle
/// plus the kernel's `push_constant_size`; the cdylib reconstructs a
/// `VulkanComputeKernel` PluginAbiObject via the host_callbacks() per-type
/// methods vtable lookup.
#[repr(C)]
pub struct RhiColorConverterMethodsVTable {
    /// Vtable layout version. Must equal
    /// [`RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION`].
    pub layout_version: u32,

    /// Reserved padding (keeps the following pointer naturally
    /// aligned on 32-bit hosts; zero today, never read).
    pub _reserved_padding: u32,

    /// Bind source / destination / push-constants on the converter's
    /// buffer→image kernel and return a fresh
    /// `Arc<VulkanComputeKernelInner>`-shaped opaque handle the cdylib
    /// wraps into its `VulkanComputeKernel` PluginAbiObject via the
    /// `host_callbacks().vulkan_compute_kernel_methods_vtable` lookup.
    ///
    /// - `converter_handle` is
    ///   `Arc::as_ptr(Arc<RhiColorConverterInner>)`-shaped (borrowed;
    ///   the host does not bump the converter's refcount).
    /// - `src_buffer_handle` is
    ///   `Arc::into_raw(Arc<HostVulkanBufferInner>)`-shaped from the
    ///   `StorageBuffer` PluginAbiObject's `handle` field (borrowed; the
    ///   cdylib retains ownership).
    /// - `dst_texture_handle` is
    ///   `Arc::into_raw(Arc<TextureInner>)`-shaped from the `Texture`
    ///   PluginAbiObject's `handle` field (borrowed; the cdylib retains
    ///   ownership).
    /// - `dst_transfer_raw` is the `#[repr(u32)]` discriminant of
    ///   `streamlib::core::color::TransferId`.
    ///
    /// On success writes a bumped
    /// `Arc::into_raw(Arc<VulkanComputeKernelInner>)`-shaped pointer
    /// into `*out_kernel` (the cdylib owns the bumped strong count
    /// and releases it via the parent vtable's
    /// `drop_compute_kernel`) and the kernel's `push_constant_size`
    /// into `*out_cached_push_constant_size`. Returns 0 on success;
    /// non-zero with UTF-8 message in `err_buf` on failure. Linux-only
    /// on the host side; non-Linux stubs return non-zero.
    pub prepare_buffer_to_image_storage: unsafe extern "C" fn(
        converter_handle: *const c_void,
        src_buffer_handle: *const c_void,
        src_layout: *const SourceLayoutInfoRepr,
        dst_texture_handle: *const c_void,
        info: *const ResolvedColorInfoRepr,
        dst_transfer_raw: u32,
        out_kernel: *mut *const c_void,
        out_cached_push_constant_size: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// `PixelBuffer`-shape source variant of
    /// [`Self::prepare_buffer_to_image_storage`]. Identical contract;
    /// `src_buffer_handle` is `Arc::into_raw(Arc<HostVulkanBufferInner>)`-
    /// shaped from a `PixelBuffer` PluginAbiObject's `handle` field (borrowed;
    /// the cdylib retains ownership). v2 (Phase E sub-lift completion).
    pub prepare_buffer_to_image_pixel: unsafe extern "C" fn(
        converter_handle: *const c_void,
        src_buffer_handle: *const c_void,
        src_layout: *const SourceLayoutInfoRepr,
        dst_texture_handle: *const c_void,
        info: *const ResolvedColorInfoRepr,
        dst_transfer_raw: u32,
        out_kernel: *mut *const c_void,
        out_cached_push_constant_size: *mut u32,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// End-to-end `StorageBuffer`→texture conversion: prepare the
    /// kernel and dispatch via the converter's own command buffer +
    /// fence + queue submit. Use when there's no surrounding
    /// `RhiCommandRecorder` scope. Same handle and enum-decoding
    /// contracts as
    /// [`Self::prepare_buffer_to_image_storage`]; no kernel handle is
    /// returned (the converter retains the cached kernel host-side).
    /// Returns 0 on success; non-zero with UTF-8 message in `err_buf`
    /// on failure. Linux-only; non-Linux stubs return non-zero.
    /// v2 (Phase E sub-lift completion).
    pub convert_buffer_to_image_storage: unsafe extern "C" fn(
        converter_handle: *const c_void,
        src_buffer_handle: *const c_void,
        src_layout: *const SourceLayoutInfoRepr,
        dst_texture_handle: *const c_void,
        info: *const ResolvedColorInfoRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,

    /// `PixelBuffer`-shape source variant of
    /// [`Self::convert_buffer_to_image_storage`]. Identical contract.
    /// v2 (Phase E sub-lift completion).
    pub convert_buffer_to_image_pixel: unsafe extern "C" fn(
        converter_handle: *const c_void,
        src_buffer_handle: *const c_void,
        src_layout: *const SourceLayoutInfoRepr,
        dst_texture_handle: *const c_void,
        info: *const ResolvedColorInfoRepr,
        err_buf: *mut u8,
        err_buf_cap: usize,
        err_len: *mut usize,
    ) -> i32,
}

unsafe impl Send for RhiColorConverterMethodsVTable {}
unsafe impl Sync for RhiColorConverterMethodsVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn rhi_color_converter_methods_vtable_layout() {
        // v2:
        //   layout_version                    @ 0   (4 bytes, u32)
        //   _reserved_padding                 @ 4   (4 bytes, u32)
        //   prepare_buffer_to_image_storage   @ 8   (8 bytes, fn pointer)
        //   prepare_buffer_to_image_pixel     @ 16
        //   convert_buffer_to_image_storage   @ 24
        //   convert_buffer_to_image_pixel     @ 32
        // Total = 40 bytes, align = 8.
        assert_eq!(size_of::<RhiColorConverterMethodsVTable>(), 40);
        assert_eq!(align_of::<RhiColorConverterMethodsVTable>(), 8);
        assert_eq!(
            offset_of!(RhiColorConverterMethodsVTable, layout_version),
            0
        );
        assert_eq!(
            offset_of!(RhiColorConverterMethodsVTable, _reserved_padding),
            4
        );
        assert_eq!(
            offset_of!(
                RhiColorConverterMethodsVTable,
                prepare_buffer_to_image_storage
            ),
            8
        );
        assert_eq!(
            offset_of!(
                RhiColorConverterMethodsVTable,
                prepare_buffer_to_image_pixel
            ),
            16
        );
        assert_eq!(
            offset_of!(
                RhiColorConverterMethodsVTable,
                convert_buffer_to_image_storage
            ),
            24
        );
        assert_eq!(
            offset_of!(
                RhiColorConverterMethodsVTable,
                convert_buffer_to_image_pixel
            ),
            32
        );
    }
}
