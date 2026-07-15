// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Minimal `extern "C"` shims into CoreVideo + IOSurface frameworks.
//!
//! The screen-capture processor's ScreenCaptureKit callback receives
//! `CVPixelBuffer` handles and walks them down to the backing
//! `IOSurface` so the engine's `GpuContext::blit_copy_iosurface` path
//! can stage the frame into a pooled `PixelBuffer`. These declarations
//! are everything the processor uses — duplicated here rather than
//! shared via SDK pollution.

use std::ffi::c_void;

/// Opaque CoreVideo pixel-buffer reference. Use the FFI functions
/// below to query dimensions and obtain the backing IOSurface.
pub type CVPixelBufferRef = *mut c_void;

/// Opaque IOSurface reference. The kernel-side ID returned by
/// [`IOSurfaceGetID`] is what crosses process boundaries.
pub type IOSurfaceRef = *const c_void;

/// IOSurface ID — the cross-process key the surface-share service
/// stores against a `PixelBuffer`.
pub type IOSurfaceID = u32;

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    /// Width of the `CVPixelBuffer` in pixels.
    pub fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    /// Height of the `CVPixelBuffer` in pixels.
    pub fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    /// Backing IOSurface; null if the pixel buffer isn't IOSurface-backed.
    pub fn CVPixelBufferGetIOSurface(pixel_buffer: CVPixelBufferRef) -> *const c_void;
}

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    /// Process-unique ID for the surface; used to look it up in another
    /// process via the surface-share service.
    pub fn IOSurfaceGetID(buffer: *const c_void) -> IOSurfaceID;
}
