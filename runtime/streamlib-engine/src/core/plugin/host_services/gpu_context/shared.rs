// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-section helpers used by every host-side gpu_context vtable
//! callback.
//!
//! - [`handle_as_gpu_context`] borrows the `&Arc<GpuContext>` from the
//!   Boxed-Arc host handle that `GpuContextLimitedAccess::new` and the
//!   `clone_handle` callback both produce.
//! - [`pixel_format_from_raw`] reverses the `#[repr(u32)]` discriminant
//!   the cdylib hands across the FFI back into the typed
//!   `streamlib_consumer_rhi::PixelFormat` variant.

use std::ffi::c_void;
use std::sync::Arc;

/// Borrow a `&Arc<GpuContext>` from a `*const Arc<GpuContext>`-shaped
/// host handle. Caller must guarantee `handle` came from
/// [`crate::core::context::GpuContextLimitedAccess::new`] or
/// `host_gpu_lim_clone_handle`; both produce
/// `Box::into_raw(Box::new(Arc::new(...))) as *const c_void`.
pub(in crate::core::plugin::host_services) unsafe fn handle_as_gpu_context(
    handle: *const c_void,
) -> Option<&'static Arc<crate::core::context::GpuContext>> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: caller-supplied contract; the Box keeps the Arc alive
    // for the duration of the dispatch through the vtable.
    unsafe { Some(&*(handle as *const Arc<crate::core::context::GpuContext>)) }
}

/// Reverse-map the `#[repr(u32)]` discriminant the cdylib hands across
/// the FFI back into [`streamlib_consumer_rhi::PixelFormat`]. Mirror of
/// `PixelBuffer::format`'s forward mapping; unknown values return
/// `None` and the caller surfaces an error.
#[inline]
pub(in crate::core::plugin::host_services) fn pixel_format_from_raw(
    raw: u32,
) -> Option<streamlib_consumer_rhi::PixelFormat> {
    use streamlib_consumer_rhi::PixelFormat;
    match raw {
        0x42475241 => Some(PixelFormat::Bgra32),
        0x52474241 => Some(PixelFormat::Rgba32),
        0x00000020 => Some(PixelFormat::Argb32),
        0x52476841 => Some(PixelFormat::Rgba64),
        0x34323076 => Some(PixelFormat::Nv12VideoRange),
        0x34323066 => Some(PixelFormat::Nv12FullRange),
        0x32767579 => Some(PixelFormat::Uyvy422),
        0x79757673 => Some(PixelFormat::Yuyv422),
        0x4C303038 => Some(PixelFormat::Gray8),
        0x00000000 => Some(PixelFormat::Unknown),
        _ => None,
    }
}
