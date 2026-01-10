// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS RhiPixelBufferRef implementation.

use std::ptr::NonNull;

use crate::apple::corevideo_ffi::{
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferRef, CVPixelBufferRelease, CVPixelBufferRetain,
};
use crate::core::rhi::{PixelFormat, RhiPixelBufferRef};

impl RhiPixelBufferRef {
    /// Create from a raw CVPixelBufferRef.
    ///
    /// # Safety
    /// The caller must ensure the CVPixelBufferRef is valid.
    /// This function retains the buffer (increments refcount).
    pub unsafe fn from_cv_pixel_buffer(cv_buffer: CVPixelBufferRef) -> Option<Self> {
        if cv_buffer.is_null() {
            return None;
        }
        // Retain the buffer
        CVPixelBufferRetain(cv_buffer);
        Some(Self {
            inner: NonNull::new_unchecked(cv_buffer),
        })
    }

    /// Create from a raw CVPixelBufferRef without retaining.
    ///
    /// # Safety
    /// The caller must ensure the CVPixelBufferRef is valid and already retained.
    /// Use this when you're taking ownership of a buffer that was already retained
    /// (e.g., from AVFoundation callback).
    pub unsafe fn from_cv_pixel_buffer_no_retain(cv_buffer: CVPixelBufferRef) -> Option<Self> {
        if cv_buffer.is_null() {
            return None;
        }
        Some(Self {
            inner: NonNull::new_unchecked(cv_buffer),
        })
    }

    /// Get the raw CVPixelBufferRef.
    pub fn cv_pixel_buffer(&self) -> CVPixelBufferRef {
        self.inner.as_ptr()
    }
}

/// Query pixel format from CVPixelBuffer.
pub(crate) fn format_impl(buffer_ref: &RhiPixelBufferRef) -> PixelFormat {
    let cv_format = unsafe { CVPixelBufferGetPixelFormatType(buffer_ref.inner.as_ptr()) };
    PixelFormat::from_cv_pixel_format_type(cv_format)
}

/// Query width from CVPixelBuffer.
pub(crate) fn width_impl(buffer_ref: &RhiPixelBufferRef) -> u32 {
    unsafe { CVPixelBufferGetWidth(buffer_ref.inner.as_ptr()) as u32 }
}

/// Query height from CVPixelBuffer.
pub(crate) fn height_impl(buffer_ref: &RhiPixelBufferRef) -> u32 {
    unsafe { CVPixelBufferGetHeight(buffer_ref.inner.as_ptr()) as u32 }
}

/// Clone implementation - retains the CVPixelBuffer.
pub(crate) fn clone_impl(buffer_ref: &RhiPixelBufferRef) -> RhiPixelBufferRef {
    unsafe {
        CVPixelBufferRetain(buffer_ref.inner.as_ptr());
        RhiPixelBufferRef {
            inner: buffer_ref.inner,
        }
    }
}

/// Drop implementation - releases the CVPixelBuffer.
pub(crate) fn drop_impl(buffer_ref: &mut RhiPixelBufferRef) {
    unsafe {
        CVPixelBufferRelease(buffer_ref.inner.as_ptr());
    }
}
