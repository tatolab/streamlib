// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer pool for efficient buffer allocation.

use super::{PixelFormat, RhiPixelBuffer};
use crate::core::Result;

/// Descriptor for creating pixel buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelBufferDescriptor {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Pixel format.
    pub format: PixelFormat,
}

impl PixelBufferDescriptor {
    /// Create a new descriptor.
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Self {
        Self {
            width,
            height,
            format,
        }
    }
}

/// Pool for reusable pixel buffers.
///
/// Wraps the platform's buffer pool (CVPixelBufferPool on macOS).
/// Buffers are automatically recycled when their refcount drops to zero.
pub struct RhiPixelBufferPool {
    #[cfg(target_os = "macos")]
    pub(crate) inner: crate::metal::rhi::pixel_buffer_pool::PixelBufferPoolMacOS,

    #[cfg(not(target_os = "macos"))]
    pub(crate) _marker: std::marker::PhantomData<()>,
}

impl RhiPixelBufferPool {
    /// Acquire a buffer from the pool.
    ///
    /// Returns a recycled buffer if available, or allocates a new one.
    pub fn acquire(&self) -> Result<RhiPixelBuffer> {
        #[cfg(target_os = "macos")]
        {
            self.inner.acquire()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(crate::core::StreamError::Configuration(
                "RhiPixelBufferPool not implemented for this platform".into(),
            ))
        }
    }
}

impl std::fmt::Debug for RhiPixelBufferPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiPixelBufferPool").finish()
    }
}
