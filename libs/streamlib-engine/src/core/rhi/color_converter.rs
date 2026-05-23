// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-owned color converter — `(src_format, dst_format)`-keyed
//! kernel that consumes [`ResolvedColorInfo`] as push-constant state.
//!
//! The converter unifies what used to be a constellation of hand-rolled
//! YUV→RGB shaders (`packages/camera/src/linux/shaders/{nv12,yuyv}_to_rgba.comp`,
//! `libs/streamlib-engine/src/vulkan/rhi/shaders/nv12_to_bgra.comp`) and
//! a `VkSamplerYcbcrConversion`-based path (`vulkan/video/nv12_to_rgb.rs`)
//! into one engine-owned primitive. Per-frame [`ResolvedColorInfo`]
//! changes cost one [`set_push_constants_value`] call rather than a
//! pipeline rebuild.

use crate::core::color::{
    yuv_to_rgb_matrix, ColorSpaceKind, ResolvedColorInfo, TransferId,
};
use crate::core::rhi::PixelFormat;

#[cfg(target_os = "linux")]
use crate::core::rhi::Texture;
#[cfg(target_os = "linux")]
use crate::core::Result;

/// Push-constants struct matching the converter shader's
/// `layout(push_constant, std430)` block.
///
/// std430 places `vec3` on 16-byte boundaries, so each row of the
/// matrix and each `vec3` is laid out as `[f32; 4]` with one slot
/// unused. Total size: 96 bytes — well under the spec minimum 128.
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
    /// Bit flags: bit 0 = apply transfer-function conversion;
    /// bit 1 = reserved for primaries matrix; bit 2 = reserved for
    /// tone mapping.
    pub flags: u32,
    /// Row stride in bytes for the primary plane (Y plane for NV12,
    /// the packed plane for YUYV). Set to `width` for tightly-packed
    /// data; honors V4L2 `bytesperline` padding for camera sources
    /// where vivid + some UVC drivers use 2×width strides.
    pub plane0_stride_bytes: u32,
    /// Row stride in bytes for the secondary plane (UV plane for
    /// NV12). Set to `width` for tightly-packed NV12; ignored for
    /// single-plane formats (YUYV).
    pub plane1_stride_bytes: u32,
    /// Byte offset where the secondary plane begins inside the
    /// source SSBO. For NV12, `plane0_stride_bytes * height`. Zero
    /// for single-plane formats (YUYV).
    pub plane1_offset_bytes: u32,
}

impl ColorConverterPushConstants {
    /// Bit 0 of `flags`: apply the transfer-function conversion path
    /// (`encoded → linear` then `linear → encoded`). Cleared when src
    /// and dst transfers match — passes encoded values through.
    pub const FLAG_APPLY_TRANSFER: u32 = 1 << 0;

    /// Build push-constants from a resolved color description.
    ///
    /// RGB vs YCbCr source-kind disambiguation lives upstream of this
    /// call in [`crate::core::color::resolve_color_defaults`] — RGB
    /// sources arrive with matrix collapsed to `Identity` and range
    /// resolved to `Full`, so the same decomposition path handles
    /// both without a separate kind parameter.
    ///
    /// `dst_transfer` is the output curve the shader encodes to. When
    /// it matches `info.transfer`, the transfer path is bypassed.
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
        // Always apply transfer when src/dst curves differ. For
        // matched curves leave it off — saves a pow() per channel.
        // Also skip when both ends are Linear (passthrough).
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
/// Honors V4L2 `bytesperline` padding so the shader walks the right
/// row strides whether the camera buffer is tightly packed
/// (`stride = width`) or has driver-side padding (vivid's 2×width
/// stride for NV12).
#[derive(Debug, Clone, Copy)]
pub struct SourceLayoutInfo {
    /// Y plane (NV12) or packed plane (YUYV) row stride in bytes.
    pub plane0_stride_bytes: u32,
    /// UV plane row stride in bytes for NV12; zero for YUYV.
    pub plane1_stride_bytes: u32,
    /// Offset of the UV plane from the start of the source SSBO,
    /// in bytes. Zero for YUYV (single plane).
    pub plane1_offset_bytes: u32,
}

impl SourceLayoutInfo {
    /// NV12 layout with the actual V4L2-reported strides and the Y
    /// plane size used to locate the UV plane. Pass
    /// `v4l2_pix_format.bytesperline` for the stride; UV plane stride
    /// matches Y stride by V4L2 convention.
    pub fn nv12(y_stride_bytes: u32, uv_stride_bytes: u32, uv_offset_bytes: u32) -> Self {
        Self {
            plane0_stride_bytes: y_stride_bytes,
            plane1_stride_bytes: uv_stride_bytes,
            plane1_offset_bytes: uv_offset_bytes,
        }
    }

    /// Tightly-packed NV12 (stride = width). Convenience for sources
    /// that don't report driver-side padding.
    pub fn nv12_tight(width: u32, height: u32) -> Self {
        Self::nv12(width, width, width * height)
    }

    /// YUYV layout — single packed plane, stride is the V4L2-reported
    /// `bytesperline` for the YUYV plane (must be a multiple of 4).
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
/// Must match the `layout(push_constant)` size in
/// `vulkan/rhi/shaders/color_convert_*.comp`.
pub const COLOR_CONVERTER_PUSH_CONSTANT_SIZE: u32 =
    std::mem::size_of::<ColorConverterPushConstants>() as u32;

/// Whether `format` denotes RGB-encoded pixel data (input is already
/// in RGB linear-or-encoded form and skips the YCbCr→RGB matrix step).
///
/// Exhaustive over [`PixelFormat`] — adding a new YUV variant without
/// extending this match is a compile-time error rather than a silent
/// route through the RGB code path.
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

/// Host-only rich data backing a [`RhiColorConverter`]. Cdylib code
/// never sees this type; it reaches the public surface through the
/// `(handle, vtable)` β-shape.
pub struct RhiColorConverterInner {
    #[cfg(target_os = "linux")]
    pub(crate) inner: crate::vulkan::rhi::VulkanColorConverter,

    #[cfg(target_os = "macos")]
    pub(crate) src_format: PixelFormat,
    #[cfg(target_os = "macos")]
    pub(crate) dst_format: PixelFormat,

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    _marker: std::marker::PhantomData<()>,
}

impl RhiColorConverterInner {
    /// Convert a [`crate::core::rhi::StorageBuffer`]-shape source into
    /// an RGBA storage image.
    #[cfg(target_os = "linux")]
    pub fn convert_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        self.inner.convert_buffer_to_image_storage(src, src_layout, dst, info)
    }

    /// [`crate::core::rhi::PixelBuffer`]-shape source variant.
    #[cfg(target_os = "linux")]
    pub fn convert_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        self.inner.convert_buffer_to_image_pixel(src, src_layout, dst, info)
    }

    /// Bind source / destination / push-constants on the buffer→image
    /// kernel and return it for recorder-driven dispatch.
    #[cfg(target_os = "linux")]
    pub fn prepare_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.inner
            .prepare_buffer_to_image_storage(src, src_layout, dst, info, dst_transfer)
    }

    /// [`crate::core::rhi::PixelBuffer`]-shape source variant of
    /// [`Self::prepare_buffer_to_image_storage`].
    #[cfg(target_os = "linux")]
    pub fn prepare_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.inner
            .prepare_buffer_to_image_pixel(src, src_layout, dst, info, dst_transfer)
    }

    /// macOS stub — Apple-platform color conversion lives in the
    /// follow-on Apple activation work; until then converter
    /// construction returns `NotSupported`, so this is unreachable.
    #[cfg(target_os = "macos")]
    pub fn convert_buffer_to_image<S, D>(
        &self,
        _src: &S,
        _dst: &D,
        _info: &ResolvedColorInfo,
    ) -> crate::core::Result<()> {
        Err(crate::core::Error::NotSupported(
            "color conversion not implemented on macOS".into(),
        ))
    }

    /// Source pixel format this converter accepts.
    pub fn src_format(&self) -> PixelFormat {
        #[cfg(target_os = "linux")]
        {
            self.inner.src_format()
        }
        #[cfg(target_os = "macos")]
        {
            self.src_format
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            PixelFormat::Unknown
        }
    }

    /// Destination pixel format this converter produces.
    pub fn dst_format(&self) -> PixelFormat {
        #[cfg(target_os = "linux")]
        {
            self.inner.dst_format()
        }
        #[cfg(target_os = "macos")]
        {
            self.dst_format
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            PixelFormat::Unknown
        }
    }
}

impl std::fmt::Debug for RhiColorConverterInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiColorConverterInner")
            .field("src", &self.src_format())
            .field("dst", &self.dst_format())
            .finish()
    }
}

// =============================================================================
// β-shape implementation
// =============================================================================

/// Stateless color converter — a `(src, dst)`-keyed handle that knows
/// how to convert pixel buffers / textures of the source format into
/// the destination format, with per-frame [`ResolvedColorInfo`] driving
/// the math via push constants.
///
/// Layout-stable: `#[repr(C)] (handle, vtable)`. Cdylibs can hold,
/// refcount, and drop without sharing rustc-version or dep-graph with
/// the host. The opaque handle points at an `Arc<RhiColorConverterInner>`;
/// lifecycle dispatches through the host-installed FullAccess vtable's
/// `clone_color_converter` / `drop_color_converter` callbacks.
#[repr(C)]
pub struct RhiColorConverter {
    /// Opaque handle to the host's `Arc<RhiColorConverterInner>`.
    pub(crate) handle: *const std::ffi::c_void,
    /// Vtable for cross-DSO Clone/Drop dispatch.
    pub(crate) vtable: *const streamlib_plugin_abi::GpuContextFullAccessVTable,
}

// SAFETY: handle points at an Arc<RhiColorConverterInner>; the Inner's
// internal VulkanColorConverter is Send+Sync (queue submits serialize
// via host queue mutex).
unsafe impl Send for RhiColorConverter {}
unsafe impl Sync for RhiColorConverter {}

impl RhiColorConverter {
    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw`, resolve the host-mode FullAccess vtable, and
    /// assemble the cross-DSO shape.
    pub(crate) fn from_arc_into_raw(arc: std::sync::Arc<RhiColorConverterInner>) -> Self {
        let handle = std::sync::Arc::into_raw(arc) as *const std::ffi::c_void;
        let vtable =
            crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
        Self { handle, vtable }
    }

    /// Engine-internal borrow of the host-owned `RhiColorConverterInner`.
    /// **Panics if called from cdylib code.**
    pub(crate) fn host_inner(&self) -> &RhiColorConverterInner {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "RhiColorConverter::host_inner() reached from cdylib code; this method \
                 must dispatch through the GpuContextFullAccessVTable."
            );
        }
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<RhiColorConverterInner>)`.
        unsafe { &*(self.handle as *const RhiColorConverterInner) }
    }

    /// Convert a [`crate::core::rhi::StorageBuffer`]-shape source into
    /// an RGBA storage image. Host-mode only until Phase E (#907) lifts
    /// method dispatch to the vtable.
    #[cfg(target_os = "linux")]
    pub fn convert_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        self.host_inner()
            .convert_buffer_to_image_storage(src, src_layout, dst, info)
    }

    /// [`crate::core::rhi::PixelBuffer`]-shape source variant.
    #[cfg(target_os = "linux")]
    pub fn convert_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        self.host_inner()
            .convert_buffer_to_image_pixel(src, src_layout, dst, info)
    }

    /// Bind source / destination / push-constants on the buffer→image
    /// kernel and return it for recorder-driven dispatch.
    #[cfg(target_os = "linux")]
    pub fn prepare_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.host_inner()
            .prepare_buffer_to_image_storage(src, src_layout, dst, info, dst_transfer)
    }

    /// [`crate::core::rhi::PixelBuffer`]-shape source variant of
    /// [`Self::prepare_buffer_to_image_storage`].
    #[cfg(target_os = "linux")]
    pub fn prepare_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.host_inner()
            .prepare_buffer_to_image_pixel(src, src_layout, dst, info, dst_transfer)
    }

    /// macOS stub.
    #[cfg(target_os = "macos")]
    pub fn convert_buffer_to_image<S, D>(
        &self,
        _src: &S,
        _dst: &D,
        _info: &ResolvedColorInfo,
    ) -> crate::core::Result<()> {
        Err(crate::core::Error::NotSupported(
            "color conversion not implemented on macOS".into(),
        ))
    }

    /// Source pixel format this converter accepts.
    pub fn src_format(&self) -> PixelFormat {
        self.host_inner().src_format()
    }

    /// Destination pixel format this converter produces.
    pub fn dst_format(&self) -> PixelFormat {
        self.host_inner().dst_format()
    }
}

impl Clone for RhiColorConverter {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: vtable + handle were paired at construction; the
            // vtable's `clone_color_converter` contract is
            // `Arc::increment_strong_count(handle)` host-side.
            unsafe {
                ((*self.vtable).clone_color_converter)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for RhiColorConverter {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the `Arc::into_raw` in
            // `from_arc_into_raw` and any `clone_color_converter` bumps.
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

/// Unused on macOS today; kept here so the symbol stays referenced
/// when the cdylib builds against `target_os = "macos"`.
#[cfg(target_os = "macos")]
impl RhiColorConverterInner {
    #[allow(dead_code)]
    pub(crate) fn new_macos_stub(src: PixelFormat, dst: PixelFormat) -> Result<Self, crate::core::Error> {
        let _ = (src, dst);
        Err(crate::core::Error::NotSupported(
            "RhiColorConverter not yet implemented on macOS".into(),
        ))
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn rhi_color_converter_layout() {
        assert_eq!(size_of::<RhiColorConverter>(), 16);
        assert_eq!(align_of::<RhiColorConverter>(), 8);
        assert_eq!(offset_of!(RhiColorConverter, handle), 0);
        assert_eq!(offset_of!(RhiColorConverter, vtable), 8);
    }

    #[test]
    fn rhi_color_converter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RhiColorConverter>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::color::{MatrixId, PrimariesId, RangeId};

    /// Push-constants size locks the cross-language contract with the
    /// shader. If the struct changes, the shader's
    /// `layout(push_constant)` size must change in lock-step.
    #[test]
    fn push_constants_size_is_96_bytes() {
        assert_eq!(
            std::mem::size_of::<ColorConverterPushConstants>(),
            96,
            "ColorConverterPushConstants layout drifted — update the shader's \
             push_constant block before regenerating SPIR-V"
        );
    }

    /// BT.709 limited resolves to the canonical first row.
    /// Mentally revert the `range_offset` assignment in `from_resolved`
    /// (e.g. swap `off[0]` and `0.0`) and this test fails: the offset
    /// becomes `[0, 128, 128]` for what should be `[16, 128, 128]`,
    /// breaking limited-range conversion.
    #[test]
    fn from_resolved_bt709_limited_populates_canonical_values() {
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Bt709,
            matrix: MatrixId::Bt709,
            range: RangeId::Limited,
        };
        let pc = ColorConverterPushConstants::from_resolved(
            &info,
            TransferId::Bt709,
            1920,
            1080,
            SourceLayoutInfo::nv12_tight(1920, 1080),
        );
        assert!((pc.matrix_row0[0] - 1.164).abs() < 5e-3);
        assert_eq!(pc.range_offset, [16.0, 128.0, 128.0, 0.0]);
        assert_eq!(pc.width, 1920);
        assert_eq!(pc.height, 1080);
        // Transfer matches → bypass flag.
        assert_eq!(pc.flags & ColorConverterPushConstants::FLAG_APPLY_TRANSFER, 0);
    }

    /// BT.601 full-range resolves to canonical webcam matrix and zero
    /// Y-offset.
    #[test]
    fn from_resolved_bt601_full_populates_canonical_values() {
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Srgb,
            matrix: MatrixId::Smpte170m,
            range: RangeId::Full,
        };
        let pc = ColorConverterPushConstants::from_resolved(
            &info,
            TransferId::Srgb,
            640,
            480,
            SourceLayoutInfo::nv12_tight(640, 480),
        );
        assert!((pc.matrix_row0[0] - 1.0).abs() < 1e-4);
        assert!((pc.matrix_row0[2] - 1.402).abs() < 1e-4);
        assert_eq!(pc.range_offset, [0.0, 128.0, 128.0, 0.0]);
    }

    /// PQ source + sRGB destination forces the transfer-conversion
    /// path on (bit 0 of `flags`).
    #[test]
    fn mismatched_transfer_sets_apply_flag() {
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt2020,
            transfer: TransferId::Pq,
            matrix: MatrixId::Bt2020Ncl,
            range: RangeId::Limited,
        };
        let pc = ColorConverterPushConstants::from_resolved(
            &info,
            TransferId::Srgb,
            1920,
            1080,
            SourceLayoutInfo::nv12_tight(1920, 1080),
        );
        assert_ne!(pc.flags & ColorConverterPushConstants::FLAG_APPLY_TRANSFER, 0);
        assert_eq!(pc.transfer_in, TransferId::Pq as u32);
        assert_eq!(pc.transfer_out, TransferId::Srgb as u32);
    }
}
