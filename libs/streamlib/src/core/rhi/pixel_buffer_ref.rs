// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform pixel buffer reference.

use super::PixelFormat;

/// Lightweight reference to a platform pixel buffer (8 bytes).
///
/// This is a thin wrapper around the platform's native pixel buffer type:
/// - macOS/iOS: CVPixelBufferRef
/// - Windows: ID3D11Texture2D* (future)
/// - Linux: DMA-BUF fd (future)
///
/// Clone increments the platform refcount, Drop decrements it.
/// No image data is ever copied - this is just a pointer.
pub struct RhiPixelBufferRef {
    #[cfg(target_os = "macos")]
    pub(crate) inner: std::ptr::NonNull<std::ffi::c_void>,

    #[cfg(not(target_os = "macos"))]
    pub(crate) _marker: std::marker::PhantomData<()>,
}

impl RhiPixelBufferRef {
    /// Query the pixel format from the platform.
    pub fn format(&self) -> PixelFormat {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::format_impl(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            PixelFormat::Unknown
        }
    }

    /// Query the width from the platform.
    pub fn width(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::width_impl(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Query the height from the platform.
    pub fn height(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::height_impl(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Get the raw platform pointer (CVPixelBufferRef on macOS).
    #[cfg(target_os = "macos")]
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.inner.as_ptr()
    }
}

impl Clone for RhiPixelBufferRef {
    fn clone(&self) -> Self {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::clone_impl(self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                _marker: std::marker::PhantomData,
            }
        }
    }
}

impl Drop for RhiPixelBufferRef {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            crate::metal::rhi::pixel_buffer_ref::drop_impl(self);
        }
    }
}

// Safety: CVPixelBufferRef is thread-safe (reference counted with atomic ops)
unsafe impl Send for RhiPixelBufferRef {}
unsafe impl Sync for RhiPixelBufferRef {}

impl std::fmt::Debug for RhiPixelBufferRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiPixelBufferRef")
            .field("format", &self.format())
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}
