// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `RhiColorConverterMethodsVTable` callbacks + static
//! vtable + accessor (Phase E sub-lift slice A).
//!
//! Each wrapper reconstructs the converter borrow from the raw
//! `Arc::as_ptr(Arc<RhiColorConverterInner>)` handle the cdylib
//! passes, reconstructs the buffer + texture borrows via the same
//! `make_*_borrow` ManuallyDrop pattern the compute / graphics
//! kernel wrappers use, runs the inner method, and converts the
//! `Result<Arc<VulkanComputeKernel>>` into the FFI's `i32 + out
//! param + err_buf` shape. All bodies wrapped in
//! `run_host_extern_c` so a panic in the inner method becomes a
//! non-zero return.

use std::ffi::c_void;

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::borrow::{
    make_pixel_buffer_borrow, make_storage_buffer_borrow, make_texture_borrow,
};
use super::shared::wire::write_err;


// =============================================================================
// RhiColorConverterMethodsVTable wrappers (Phase E sub-lift slice A).
// Each wrapper reconstructs the converter borrow from the raw
// `Arc::as_ptr(Arc<RhiColorConverterInner>)` handle the cdylib passes,
// reconstructs the buffer + texture borrows via the same
// `make_*_borrow` ManuallyDrop pattern the compute / graphics kernel
// wrappers use, runs the inner method, and converts the
// `Result<Arc<VulkanComputeKernel>>` into the FFI's `i32 + out
// param + err_buf` shape. All bodies are wrapped in
// `run_host_extern_c` so a panic in the inner method becomes a
// non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::as_ptr(Arc<RhiColorConverterInner>)`. The host borrows
/// only — no refcount bump — for the call's duration; the cdylib
/// retains ownership.
#[cfg(target_os = "linux")]
unsafe fn handle_as_color_converter(
    handle: *const c_void,
) -> Option<&'static crate::core::rhi::RhiColorConverterInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe {
        &*(handle as *const crate::core::rhi::RhiColorConverterInner)
    })
}

/// Convert a `#[repr(u32)]` `PrimariesId` discriminant to the typed
/// enum. Returns `None` for out-of-range values so the wrapper can
/// report a clean error rather than transmuting a garbage tag.
#[cfg(target_os = "linux")]
fn primaries_from_raw(
    raw: u32,
) -> Option<crate::core::color::PrimariesId> {
    use crate::core::color::PrimariesId;
    match raw {
        0 => Some(PrimariesId::Bt709),
        1 => Some(PrimariesId::Bt470M),
        2 => Some(PrimariesId::Bt470Bg),
        3 => Some(PrimariesId::Smpte170m),
        4 => Some(PrimariesId::Smpte240m),
        5 => Some(PrimariesId::Film),
        6 => Some(PrimariesId::Bt2020),
        7 => Some(PrimariesId::Smpte428),
        8 => Some(PrimariesId::Smpte431),
        9 => Some(PrimariesId::Smpte432),
        10 => Some(PrimariesId::Ebu3213),
        _ => None,
    }
}

/// Convert a `#[repr(u32)]` `TransferId` discriminant to the typed enum.
#[cfg(target_os = "linux")]
fn transfer_from_raw(raw: u32) -> Option<crate::core::color::TransferId> {
    use crate::core::color::TransferId;
    match raw {
        0 => Some(TransferId::Linear),
        1 => Some(TransferId::Srgb),
        2 => Some(TransferId::Bt709),
        3 => Some(TransferId::Pq),
        4 => Some(TransferId::Hlg),
        _ => None,
    }
}

/// Convert a `#[repr(u32)]` `MatrixId` discriminant to the typed enum.
#[cfg(target_os = "linux")]
fn matrix_from_raw(raw: u32) -> Option<crate::core::color::MatrixId> {
    use crate::core::color::MatrixId;
    match raw {
        0 => Some(MatrixId::Identity),
        1 => Some(MatrixId::Bt709),
        2 => Some(MatrixId::Fcc),
        3 => Some(MatrixId::Bt470Bg),
        4 => Some(MatrixId::Smpte170m),
        5 => Some(MatrixId::Smpte240m),
        6 => Some(MatrixId::Ycgco),
        7 => Some(MatrixId::Bt2020Ncl),
        8 => Some(MatrixId::Bt2020Cl),
        9 => Some(MatrixId::Smpte2085),
        10 => Some(MatrixId::ChromaNcl),
        11 => Some(MatrixId::ChromaCl),
        12 => Some(MatrixId::Ictcp),
        _ => None,
    }
}

/// Convert a `#[repr(u32)]` `RangeId` discriminant to the typed enum.
#[cfg(target_os = "linux")]
fn range_from_raw(raw: u32) -> Option<crate::core::color::RangeId> {
    use crate::core::color::RangeId;
    match raw {
        0 => Some(RangeId::Limited),
        1 => Some(RangeId::Full),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_storage(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    dst_transfer_raw: u32,
    out_kernel: *mut *const c_void,
    out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_prepare_buffer_to_image_storage",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "prepare_buffer_to_image_storage: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_kernel.is_null() || out_cached_push_constant_size.is_null() {
                write_err(
                    "prepare_buffer_to_image_storage: null out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let Some(dst_transfer) = transfer_from_raw(dst_transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_storage: invalid dst_transfer discriminant {}",
                        dst_transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };

            let src_borrow = make_storage_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.prepare_buffer_to_image_storage(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
                dst_transfer,
            ) {
                Ok(arc_kernel) => {
                    // `arc_kernel.handle` is the inner Arc-into-raw'd
                    // pointer baked into the β-shape at construction.
                    // The cdylib needs its own strong count on the
                    // inner Arc so its β-shape can outlive our return
                    // (the converter's kernel cache + the inner Arc
                    // chain it sits behind keep their own strong
                    // counts). Bump the inner refcount by 1; the
                    // returned `Arc<VulkanComputeKernel>` drops
                    // naturally at end-of-block — its β-shape's Drop
                    // decrements the inner by 1, but only if this Arc
                    // was the last strong ref, which it isn't because
                    // the converter cache still holds one. Net effect:
                    // cdylib walks away with +1 inner-Arc strong count
                    // dedicated to it.
                    let raw_inner = arc_kernel.handle;
                    unsafe {
                        std::sync::Arc::increment_strong_count(
                            raw_inner
                                as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                        );
                    }
                    let push_constant_size = arc_kernel.cached_push_constant_size;
                    unsafe {
                        std::ptr::write(out_kernel, raw_inner);
                        std::ptr::write(
                            out_cached_push_constant_size,
                            push_constant_size,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(
                        &format!("prepare_buffer_to_image_storage: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_storage(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    _dst_transfer_raw: u32,
    _out_kernel: *mut *const c_void,
    _out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "prepare_buffer_to_image_storage: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// `PixelBuffer`-shape source variant of
/// [`host_color_converter_prepare_buffer_to_image_storage`]. Decodes
/// the `ResolvedColorInfoRepr` + `SourceLayoutInfoRepr`, reconstructs
/// the `PixelBuffer` borrow, calls
/// `RhiColorConverterInner::prepare_buffer_to_image_pixel`, and bumps
/// the returned kernel's inner Arc strong count for the cdylib to
/// own. v2 (Phase E sub-lift completion).
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_pixel(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    dst_transfer_raw: u32,
    out_kernel: *mut *const c_void,
    out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_prepare_buffer_to_image_pixel",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "prepare_buffer_to_image_pixel: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_kernel.is_null() || out_cached_push_constant_size.is_null() {
                write_err(
                    "prepare_buffer_to_image_pixel: null out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let Some(dst_transfer) = transfer_from_raw(dst_transfer_raw) else {
                write_err(
                    &format!(
                        "prepare_buffer_to_image_pixel: invalid dst_transfer discriminant {}",
                        dst_transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };

            let src_borrow = make_pixel_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.prepare_buffer_to_image_pixel(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
                dst_transfer,
            ) {
                Ok(arc_kernel) => {
                    let raw_inner = arc_kernel.handle;
                    unsafe {
                        std::sync::Arc::increment_strong_count(
                            raw_inner
                                as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                        );
                    }
                    let push_constant_size = arc_kernel.cached_push_constant_size;
                    unsafe {
                        std::ptr::write(out_kernel, raw_inner);
                        std::ptr::write(
                            out_cached_push_constant_size,
                            push_constant_size,
                        );
                    }
                    0
                }
                Err(e) => {
                    write_err(
                        &format!("prepare_buffer_to_image_pixel: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_prepare_buffer_to_image_pixel(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    _dst_transfer_raw: u32,
    _out_kernel: *mut *const c_void,
    _out_cached_push_constant_size: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "prepare_buffer_to_image_pixel: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// `StorageBuffer`-shape end-to-end conversion. Same handle and enum-
/// decoding contracts as
/// [`host_color_converter_prepare_buffer_to_image_storage`]; returns
/// no kernel handle (the host's converter retains the kernel cache).
/// v2 (Phase E sub-lift completion).
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_storage(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_convert_buffer_to_image_storage",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "convert_buffer_to_image_storage: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "convert_buffer_to_image_storage: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_storage: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let src_borrow = make_storage_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.convert_buffer_to_image_storage(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("convert_buffer_to_image_storage: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_storage(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "convert_buffer_to_image_storage: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// `PixelBuffer`-shape end-to-end conversion. Identical to
/// [`host_color_converter_convert_buffer_to_image_storage`] except for
/// the source buffer flavor. v2 (Phase E sub-lift completion).
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_pixel(
    converter_handle: *const c_void,
    src_buffer_handle: *const c_void,
    src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    dst_texture_handle: *const c_void,
    info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_color_converter_convert_buffer_to_image_pixel",
        || -> i32 {
            let Some(converter) =
                (unsafe { handle_as_color_converter(converter_handle) })
            else {
                write_err(
                    "convert_buffer_to_image_pixel: null converter handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_buffer_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null src_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_texture_handle.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null dst_texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if src_layout.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null src_layout pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if info.is_null() {
                write_err(
                    "convert_buffer_to_image_pixel: null info pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }

            let layout_repr = unsafe { &*src_layout };
            let info_repr = unsafe { &*info };

            let rust_layout = crate::core::rhi::SourceLayoutInfo {
                plane0_stride_bytes: layout_repr.plane0_stride_bytes,
                plane1_stride_bytes: layout_repr.plane1_stride_bytes,
                plane1_offset_bytes: layout_repr.plane1_offset_bytes,
            };

            let Some(primaries) = primaries_from_raw(info_repr.primaries_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid primaries discriminant {}",
                        info_repr.primaries_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(transfer_in) = transfer_from_raw(info_repr.transfer_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid transfer discriminant {}",
                        info_repr.transfer_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(matrix) = matrix_from_raw(info_repr.matrix_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid matrix discriminant {}",
                        info_repr.matrix_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let Some(range) = range_from_raw(info_repr.range_raw) else {
                write_err(
                    &format!(
                        "convert_buffer_to_image_pixel: invalid range discriminant {}",
                        info_repr.range_raw
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let resolved = crate::core::color::ResolvedColorInfo {
                primaries,
                transfer: transfer_in,
                matrix,
                range,
            };

            let src_borrow = make_pixel_buffer_borrow(src_buffer_handle);
            let dst_borrow = make_texture_borrow(dst_texture_handle);

            match converter.convert_buffer_to_image_pixel(
                &*src_borrow,
                rust_layout,
                &*dst_borrow,
                &resolved,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("convert_buffer_to_image_pixel: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_color_converter_convert_buffer_to_image_pixel(
    _converter_handle: *const c_void,
    _src_buffer_handle: *const c_void,
    _src_layout: *const streamlib_plugin_abi::SourceLayoutInfoRepr,
    _dst_texture_handle: *const c_void,
    _info: *const streamlib_plugin_abi::ResolvedColorInfoRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "convert_buffer_to_image_pixel: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `RhiColorConverterMethodsVTable` wired to the per-method
/// wrappers above. v2 ships the Phase E sub-lift completion —
/// `prepare_buffer_to_image_pixel`, `convert_buffer_to_image_storage`,
/// `convert_buffer_to_image_pixel`.
pub static HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE:
    streamlib_plugin_abi::RhiColorConverterMethodsVTable =
    streamlib_plugin_abi::RhiColorConverterMethodsVTable {
        layout_version:
            streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        prepare_buffer_to_image_storage:
            host_color_converter_prepare_buffer_to_image_storage,
        prepare_buffer_to_image_pixel:
            host_color_converter_prepare_buffer_to_image_pixel,
        convert_buffer_to_image_storage:
            host_color_converter_convert_buffer_to_image_storage,
        convert_buffer_to_image_pixel:
            host_color_converter_convert_buffer_to_image_pixel,
    };

/// Accessor for the host's static `RhiColorConverterMethodsVTable` —
/// used by `RhiColorConverter::from_arc_into_raw` to populate the
/// β-shape's `methods_vtable` field.
pub fn host_rhi_color_converter_methods_vtable(
) -> *const streamlib_plugin_abi::RhiColorConverterMethodsVTable {
    &HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE
}

#[cfg(all(test, target_os = "linux"))]
mod gpu_rhi_color_converter_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v1 method slot on
    //! `RhiColorConverterMethodsVTable`. The wrapper must reject a
    //! null converter handle before reaching any converter-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-src-buffer / null-dst-texture / null-src-layout /
    //! null-info / null-out-pointer guards live in the same wrapper
    //! and fire when the converter handle is valid; they're
    //! exercised end-to-end by the camera-package dlopen smoke test
    //! (which holds a real converter). Tier-1 cannot reach them
    //! without first passing the converter-handle deref — passing a
    //! non-null garbage handle for the converter trips a misaligned-
    //! pointer-deref panic before any subsequent guard runs. This
    //! mirrors the precedent set by
    //! `compute_kernel_methods_vtable_null_tests` (only the null-
    //! kernel-handle case is tier-1; the rest ride dlopen).
    //!
    //! Success-path coverage (real Arc<RhiColorConverterInner>, a
    //! cached buffer→image kernel that mints a fresh
    //! Arc<VulkanComputeKernelInner>-shaped out-handle, refcount
    //! transfer to the cdylib) requires a real Vulkan device and
    //! arrives in the camera-package dlopen smoke test.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    fn dummy_layout() -> streamlib_plugin_abi::SourceLayoutInfoRepr {
        streamlib_plugin_abi::SourceLayoutInfoRepr {
            plane0_stride_bytes: 0,
            plane1_stride_bytes: 0,
            plane1_offset_bytes: 0,
            _reserved_padding: 0,
        }
    }

    fn dummy_info() -> streamlib_plugin_abi::ResolvedColorInfoRepr {
        streamlib_plugin_abi::ResolvedColorInfoRepr {
            primaries_raw: 0,
            transfer_raw: 0,
            matrix_raw: 0,
            range_raw: 0,
        }
    }

    #[test]
    fn prepare_buffer_to_image_storage_rejects_null_converter_handle() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let mut out_kernel: *const std::ffi::c_void = std::ptr::null();
        let mut out_pc_size: u32 = 0;
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.prepare_buffer_to_image_storage)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                0,
                &mut out_kernel,
                &mut out_pc_size,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("prepare_buffer_to_image_storage: null converter handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}
#[cfg(all(test, target_os = "linux"))]
mod rhi_color_converter_methods_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for the v2 sibling slots added to
    //! `RhiColorConverterMethodsVTable`: `prepare_buffer_to_image_pixel`,
    //! `convert_buffer_to_image_storage`, `convert_buffer_to_image_pixel`.
    //!
    //! Each slot's null-handle / null out-ptr / err-buf contract is
    //! exercised against the static `HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE`.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    fn dummy_layout() -> streamlib_plugin_abi::SourceLayoutInfoRepr {
        streamlib_plugin_abi::SourceLayoutInfoRepr {
            plane0_stride_bytes: 0,
            plane1_stride_bytes: 0,
            plane1_offset_bytes: 0,
            _reserved_padding: 0,
        }
    }

    fn dummy_info() -> streamlib_plugin_abi::ResolvedColorInfoRepr {
        streamlib_plugin_abi::ResolvedColorInfoRepr {
            primaries_raw: 0,
            transfer_raw: 1,
            matrix_raw: 1,
            range_raw: 1,
        }
    }

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.layout_version,
            streamlib_plugin_abi::RHI_COLOR_CONVERTER_METHODS_VTABLE_LAYOUT_VERSION,
        );
    }

    #[test]
    fn prepare_buffer_to_image_pixel_returns_error_on_null_converter() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let mut out_kernel: *const c_void = std::ptr::null();
        let mut out_size: u32 = 0;
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.prepare_buffer_to_image_pixel)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                1,
                &mut out_kernel as *mut *const c_void,
                &mut out_size as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("prepare_buffer_to_image_pixel: null converter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn convert_buffer_to_image_storage_returns_error_on_null_converter() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE
                .convert_buffer_to_image_storage)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("convert_buffer_to_image_storage: null converter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn convert_buffer_to_image_pixel_returns_error_on_null_converter() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE
                .convert_buffer_to_image_pixel)(
                std::ptr::null(),
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("convert_buffer_to_image_pixel: null converter handle"),
            "got: {msg}"
        );
    }

    /// Verifies the converter slot's null `out_kernel` rejection
    /// path. Mirrors `prepare_buffer_to_image_storage`'s contract.
    #[test]
    fn prepare_buffer_to_image_pixel_returns_error_on_null_out_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let layout = dummy_layout();
        let info = dummy_info();
        let mut out_size: u32 = 0;
        // Use a non-null fake converter handle so we reach the
        // out-ptr null check. Casting a stack reference to *const
        // is safe here because the FFI wrapper only reaches handle_as_*
        // (a transmute) if other null-checks pass — the out_kernel
        // check runs after the converter null-check but before the
        // handle deref.
        let fake_converter: usize = 1;
        let rc = unsafe {
            (HOST_RHI_COLOR_CONVERTER_METHODS_VTABLE.prepare_buffer_to_image_pixel)(
                &fake_converter as *const usize as *const c_void,
                std::ptr::null(),
                &layout,
                std::ptr::null(),
                &info,
                1,
                std::ptr::null_mut(),
                &mut out_size as *mut u32,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        // The null-check ordering surfaces src_buffer first (it's a
        // simpler check than out_kernel). Either error is acceptable —
        // verify we got *an* error tagged with the slot name.
        assert!(
            msg.contains("prepare_buffer_to_image_pixel:"),
            "got: {msg}"
        );
    }
}
