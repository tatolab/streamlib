// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's RHI color-converter surface:
//! [`RhiColorConverter`] PluginAbiObject plus the pure-data
//! [`ColorConverterPushConstants`] / [`SourceLayoutInfo`] helpers a
//! Vulkan-compute consumer builds its color push constants from.
//!
//! [`RhiColorConverter`] is layout-stable
//! `#[repr(C)] { handle, vtable, methods_vtable, cached POD }`; lifecycle
//! dispatches through the parent [`GpuContextFullAccessVTable`]'s
//! `clone_color_converter` / `drop_color_converter`, and method calls
//! dispatch through the per-type
//! [`streamlib_plugin_abi::RhiColorConverterMethodsVTable`].
//!
//! The host `RhiColorConverterInner` backing stays in the engine.

use streamlib_consumer_rhi::PixelFormat;
use streamlib_error::{Error, Result};
use streamlib_plugin_abi::{
    GpuContextFullAccessVTable, ResolvedColorInfoRepr, RhiColorConverterMethodsVTable,
    SourceLayoutInfoRepr,
};

use crate::color::{yuv_to_rgb_matrix, ColorSpaceKind, ResolvedColorInfo, TransferId};
use crate::rhi::{StorageBuffer, Texture, VulkanComputeKernel};

/// Push-constants struct matching the converter shader's
/// `layout(push_constant, std430)` block. Total size: 96 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ColorConverterPushConstants {
    /// Row 0 of the YCbCr→RGB matrix, `(r·y, r·cb, r·cr, _pad)`.
    pub matrix_row0: [f32; 4],
    /// Row 1 of the YCbCr→RGB matrix, `(g·y, g·cb, g·cr, _pad)`.
    pub matrix_row1: [f32; 4],
    /// Row 2 of the YCbCr→RGB matrix, `(b·y, b·cb, b·cr, _pad)`.
    pub matrix_row2: [f32; 4],
    /// Byte-domain offset subtracted from raw YCbCr before the matrix
    /// multiply, `(y_offset, cb_offset, cr_offset, _pad)`.
    pub range_offset: [f32; 4],
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Source transfer characteristic, encoded as [`TransferId`].
    pub transfer_in: u32,
    /// Destination transfer characteristic, encoded as [`TransferId`].
    pub transfer_out: u32,
    /// Bit flags: bit 0 = apply transfer-function conversion.
    pub flags: u32,
    /// Row stride in bytes for the primary plane.
    pub plane0_stride_bytes: u32,
    /// Row stride in bytes for the secondary plane (UV plane for NV12).
    pub plane1_stride_bytes: u32,
    /// Byte offset where the secondary plane begins inside the source.
    pub plane1_offset_bytes: u32,
}

impl ColorConverterPushConstants {
    /// Bit 0 of `flags`: apply the transfer-function conversion path.
    pub const FLAG_APPLY_TRANSFER: u32 = 1 << 0;

    /// Build push-constants from a resolved color description.
    ///
    /// `dst_transfer` is the output curve the shader encodes to. When it
    /// matches `info.transfer`, the transfer path is bypassed.
    pub fn from_resolved(
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
        width: u32,
        height: u32,
        layout: SourceLayoutInfo,
    ) -> Self {
        let decomposition = yuv_to_rgb_matrix(info.matrix, info.range);
        let m = decomposition.matrix_row_major;
        let off = decomposition.offset;

        let transfer_in = info.transfer as u32;
        let transfer_out = dst_transfer as u32;
        let mut flags = 0u32;
        if transfer_in != transfer_out {
            flags |= Self::FLAG_APPLY_TRANSFER;
        }

        Self {
            matrix_row0: [m[0], m[1], m[2], 0.0],
            matrix_row1: [m[3], m[4], m[5], 0.0],
            matrix_row2: [m[6], m[7], m[8], 0.0],
            range_offset: [off[0], off[1], off[2], 0.0],
            width,
            height,
            transfer_in,
            transfer_out,
            flags,
            plane0_stride_bytes: layout.plane0_stride_bytes,
            plane1_stride_bytes: layout.plane1_stride_bytes,
            plane1_offset_bytes: layout.plane1_offset_bytes,
        }
    }
}

/// Source-buffer layout description — plane strides and offsets.
#[derive(Debug, Clone, Copy)]
pub struct SourceLayoutInfo {
    /// Y plane (NV12) or packed plane (YUYV) row stride in bytes.
    pub plane0_stride_bytes: u32,
    /// UV plane row stride in bytes for NV12; zero for YUYV.
    pub plane1_stride_bytes: u32,
    /// Offset of the UV plane from the start of the source SSBO, in bytes.
    pub plane1_offset_bytes: u32,
}

impl SourceLayoutInfo {
    /// NV12 layout with the actual V4L2-reported strides and the Y plane
    /// size used to locate the UV plane.
    pub fn nv12(y_stride_bytes: u32, uv_stride_bytes: u32, uv_offset_bytes: u32) -> Self {
        Self {
            plane0_stride_bytes: y_stride_bytes,
            plane1_stride_bytes: uv_stride_bytes,
            plane1_offset_bytes: uv_offset_bytes,
        }
    }

    /// Tightly-packed NV12 (stride = width).
    pub fn nv12_tight(width: u32, height: u32) -> Self {
        Self::nv12(width, width, width * height)
    }

    /// YUYV layout — single packed plane.
    pub fn yuyv(packed_stride_bytes: u32) -> Self {
        Self {
            plane0_stride_bytes: packed_stride_bytes,
            plane1_stride_bytes: 0,
            plane1_offset_bytes: 0,
        }
    }

    /// Tightly-packed YUYV (stride = `2 * width`).
    pub fn yuyv_tight(width: u32) -> Self {
        Self::yuyv(width * 2)
    }
}

/// Byte size of the push-constants block sent to the converter kernel.
pub const COLOR_CONVERTER_PUSH_CONSTANT_SIZE: u32 =
    std::mem::size_of::<ColorConverterPushConstants>() as u32;

/// Whether `format` denotes RGB-encoded pixel data (input skips the
/// YCbCr→RGB matrix step).
pub fn pixel_format_color_kind(format: PixelFormat) -> ColorSpaceKind {
    match format {
        PixelFormat::Rgba32
        | PixelFormat::Bgra32
        | PixelFormat::Argb32
        | PixelFormat::Rgba64
        | PixelFormat::Gray8
        | PixelFormat::Unknown => ColorSpaceKind::Rgb,
        PixelFormat::Nv12VideoRange
        | PixelFormat::Nv12FullRange
        | PixelFormat::Uyvy422
        | PixelFormat::Yuyv422 => ColorSpaceKind::Yuv,
    }
}

// =============================================================================
// RhiColorConverter PluginAbiObject
// =============================================================================

/// Stateless color converter — a `(src, dst)`-keyed handle that converts
/// pixel buffers / textures of the source format into the destination
/// format, with per-frame [`ResolvedColorInfo`] driving the math.
#[repr(C)]
pub struct RhiColorConverter {
    /// Opaque handle to the host's `Arc<RhiColorConverterInner>`.
    pub(crate) handle: *const std::ffi::c_void,
    /// Parent vtable for plugin ABI Clone/Drop dispatch.
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Per-type vtable for plugin ABI method dispatch.
    pub(crate) methods_vtable: *const RhiColorConverterMethodsVTable,
    /// Cached `#[repr(u32)]` `PixelFormat` discriminant for the source
    /// format.
    pub(crate) cached_src_format_raw: u32,
    /// Cached `#[repr(u32)]` `PixelFormat` discriminant for the
    /// destination format.
    pub(crate) cached_dst_format_raw: u32,
}

// SAFETY: handle points at an `Arc<RhiColorConverterInner>`; the inner's
// internal converter is Send+Sync (queue submits serialize via host queue
// mutex).
unsafe impl Send for RhiColorConverter {}
unsafe impl Sync for RhiColorConverter {}

impl RhiColorConverter {
    /// Convert a [`StorageBuffer`]-shape source into an RGBA storage
    /// image. Dispatches through the per-type methods vtable's
    /// `convert_buffer_to_image_storage` slot.
    pub fn convert_buffer_to_image_storage(
        &self,
        src: &StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "convert_buffer_to_image_storage: color converter methods vtable is null".into(),
            ));
        }
        let layout_repr = source_layout_repr(src_layout);
        let info_repr = resolved_color_info_repr(info);
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; src/dst handles
        // are the borrowed `Arc::into_raw` pointers the host reconstructs.
        let status = unsafe {
            ((*self.methods_vtable).convert_buffer_to_image_storage)(
                self.handle,
                src.handle,
                &layout_repr,
                dst.handle,
                &info_repr,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_result(status, &err_buf, err_len)
    }

    /// Bind source / destination / push-constants on the buffer→image
    /// kernel and return it for recorder-driven dispatch. Dispatches
    /// through the per-type methods vtable's
    /// `prepare_buffer_to_image_storage` slot; the cdylib reconstructs a
    /// [`VulkanComputeKernel`] PluginAbiObject from the host-returned
    /// handle plus the per-type vtables sourced from `host_callbacks()`.
    pub fn prepare_buffer_to_image_storage(
        &self,
        src: &StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<VulkanComputeKernel> {
        if self.methods_vtable.is_null() {
            return Err(Error::GpuError(
                "prepare_buffer_to_image_storage: color converter methods vtable is null".into(),
            ));
        }
        // The cdylib needs the parent FullAccess vtable and the per-type
        // VulkanComputeKernel methods vtable to assemble its own
        // PluginAbiObject from the host-returned inner handle.
        let callbacks = crate::plugin::host_callbacks().ok_or_else(|| {
            Error::GpuError(
                "prepare_buffer_to_image_storage: host callbacks not installed".into(),
            )
        })?;
        let parent_vtable = callbacks.gpu_context_full_access_vtable;
        let kernel_methods_vtable = callbacks.vulkan_compute_kernel_methods_vtable;
        if parent_vtable.is_null() {
            return Err(Error::GpuError(
                "prepare_buffer_to_image_storage: GpuContextFullAccess vtable is null".into(),
            ));
        }

        let layout_repr = source_layout_repr(src_layout);
        let info_repr = resolved_color_info_repr(info);

        let mut out_kernel: *const std::ffi::c_void = std::ptr::null();
        let mut out_cached_push_constant_size: u32 = 0;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods_vtable non-null per the guard; src/dst handles
        // are the borrowed `Arc::into_raw` pointers the host reconstructs.
        let status = unsafe {
            ((*self.methods_vtable).prepare_buffer_to_image_storage)(
                self.handle,
                src.handle,
                &layout_repr,
                dst.handle,
                &info_repr,
                dst_transfer as u32,
                &mut out_kernel,
                &mut out_cached_push_constant_size,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(msg));
        }
        if out_kernel.is_null() {
            return Err(Error::GpuError(
                "prepare_buffer_to_image_storage: host signaled success but out_kernel is null"
                    .into(),
            ));
        }
        // PluginAbiObject: the host bumped the inner Arc strong count
        // before returning; the kernel's Drop releases it via
        // `drop_compute_kernel`.
        Ok(VulkanComputeKernel {
            handle: out_kernel,
            vtable: parent_vtable,
            methods_vtable: kernel_methods_vtable,
            cached_push_constant_size: out_cached_push_constant_size,
            _reserved_padding: 0,
        })
    }

    /// Source pixel format this converter accepts. Cached POD — no plugin
    /// ABI hop.
    pub fn src_format(&self) -> PixelFormat {
        pixel_format_from_raw(self.cached_src_format_raw)
    }

    /// Destination pixel format this converter produces. Cached POD.
    pub fn dst_format(&self) -> PixelFormat {
        pixel_format_from_raw(self.cached_dst_format_raw)
    }
}

fn source_layout_repr(src_layout: SourceLayoutInfo) -> SourceLayoutInfoRepr {
    SourceLayoutInfoRepr {
        plane0_stride_bytes: src_layout.plane0_stride_bytes,
        plane1_stride_bytes: src_layout.plane1_stride_bytes,
        plane1_offset_bytes: src_layout.plane1_offset_bytes,
        _reserved_padding: 0,
    }
}

fn resolved_color_info_repr(info: &ResolvedColorInfo) -> ResolvedColorInfoRepr {
    ResolvedColorInfoRepr {
        primaries_raw: info.primaries as u32,
        transfer_raw: info.transfer as u32,
        matrix_raw: info.matrix as u32,
        range_raw: info.range as u32,
    }
}

/// Decode a `#[repr(u32)]` `PixelFormat` discriminant. Unknown values map
/// to [`PixelFormat::Unknown`].
fn pixel_format_from_raw(raw: u32) -> PixelFormat {
    match raw {
        x if x == PixelFormat::Bgra32 as u32 => PixelFormat::Bgra32,
        x if x == PixelFormat::Rgba32 as u32 => PixelFormat::Rgba32,
        x if x == PixelFormat::Argb32 as u32 => PixelFormat::Argb32,
        x if x == PixelFormat::Rgba64 as u32 => PixelFormat::Rgba64,
        x if x == PixelFormat::Nv12VideoRange as u32 => PixelFormat::Nv12VideoRange,
        x if x == PixelFormat::Nv12FullRange as u32 => PixelFormat::Nv12FullRange,
        x if x == PixelFormat::Uyvy422 as u32 => PixelFormat::Uyvy422,
        x if x == PixelFormat::Yuyv422 as u32 => PixelFormat::Yuyv422,
        x if x == PixelFormat::Gray8 as u32 => PixelFormat::Gray8,
        _ => PixelFormat::Unknown,
    }
}

/// Decode a `(status, err_buf)` plugin-ABI return into `Result<()>`.
fn status_to_result(status: i32, err_buf: &[u8], err_len: usize) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
        Err(Error::GpuError(msg))
    }
}

impl Clone for RhiColorConverter {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle paired at mint time; the vtable's
            // `clone_color_converter` contract is
            // `Arc::increment_strong_count` host-side.
            unsafe {
                ((*self.vtable).clone_color_converter)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
            methods_vtable: self.methods_vtable,
            cached_src_format_raw: self.cached_src_format_raw,
            cached_dst_format_raw: self.cached_dst_format_raw,
        }
    }
}

impl Drop for RhiColorConverter {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_color_converter` bumps.
            unsafe {
                ((*self.vtable).drop_color_converter)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for RhiColorConverter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiColorConverter").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn rhi_color_converter_layout() {
        // Must match the engine's
        // `core/rhi/color_converter.rs::RhiColorConverter`:
        //   handle @ 0, vtable @ 8, methods_vtable @ 16,
        //   cached_src_format_raw @ 24, cached_dst_format_raw @ 28.
        // Total 32 bytes, align 8.
        assert_eq!(size_of::<RhiColorConverter>(), 32);
        assert_eq!(align_of::<RhiColorConverter>(), 8);
        assert_eq!(offset_of!(RhiColorConverter, handle), 0);
        assert_eq!(offset_of!(RhiColorConverter, vtable), 8);
        assert_eq!(offset_of!(RhiColorConverter, methods_vtable), 16);
        assert_eq!(offset_of!(RhiColorConverter, cached_src_format_raw), 24);
        assert_eq!(offset_of!(RhiColorConverter, cached_dst_format_raw), 28);
    }

    #[test]
    fn push_constants_size_is_96_bytes() {
        assert_eq!(std::mem::size_of::<ColorConverterPushConstants>(), 96);
        assert_eq!(COLOR_CONVERTER_PUSH_CONSTANT_SIZE, 96);
    }

    #[test]
    fn rhi_color_converter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RhiColorConverter>();
    }
}
