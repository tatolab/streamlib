// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer pool for efficient buffer allocation.

use super::{PixelFormat, RhiPixelBuffer};
use crate::core::Result;

/// Platform-agnostic identifier for a pooled pixel buffer.
///
/// Uses UUID for global uniqueness across parallel runtimes.
/// Serializable as string for messagepack transport in frame payloads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PixelBufferPoolId(String);

impl PixelBufferPoolId {
    /// Generate a new unique ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create from an existing string (e.g., from IPC deserialization).
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Create from a string slice.
    pub fn from_str(s: &str) -> Self {
        Self(s.to_string())
    }

    /// Get the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PixelBufferPoolId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PixelBufferPoolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

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
    /// Returns (id, buffer) where id is the platform-agnostic identifier.
    /// Returns a recycled buffer if available, or allocates a new one.
    pub fn acquire(&self) -> Result<(PixelBufferPoolId, RhiPixelBuffer)> {
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
