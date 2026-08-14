// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer pool for efficient buffer allocation.

use super::{PixelBuffer, PixelFormat};
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

    #[cfg(target_os = "linux")]
    pub(crate) inner: crate::vulkan::rhi::VulkanPixelBufferPool,

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) _marker: std::marker::PhantomData<()>,
}

impl RhiPixelBufferPool {
    /// Acquire a buffer from the pool.
    ///
    /// Returns (id, buffer) where id is the platform-agnostic identifier.
    /// Returns a recycled buffer if available, or allocates a new one.
    pub fn acquire(&self) -> Result<(PixelBufferPoolId, PixelBuffer)> {
        #[cfg(target_os = "macos")]
        {
            self.inner.acquire()
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.acquire()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(crate::core::Error::Configuration(
                "RhiPixelBufferPool not implemented for this platform".into(),
            ))
        }
    }

    /// Grow the pool by one buffer and hand it out.
    ///
    /// What a manager calls when every existing slot is held — by an
    /// in-process reader or by a cross-process checkout lease — and the
    /// producer still needs somewhere to write. What comes back must be a
    /// slot no caller has seen before: the manager appends it to its ring,
    /// so a recycled buffer would enter the ring twice under one id.
    ///
    /// On macOS this refuses rather than growing: `CVPixelBufferPool` may
    /// recycle, and `PixelBufferPoolMacOS::acquire` deliberately returns the
    /// existing id for a recycled IOSurface — exactly the duplicate the ring
    /// must never hold. Growth there needs a dedicated fresh-allocation path.
    pub fn allocate_additional_buffer(&mut self) -> Result<(PixelBufferPoolId, PixelBuffer)> {
        #[cfg(target_os = "macos")]
        {
            Err(crate::core::Error::NotSupported(
                "macOS pool growth needs a fresh-allocation path: CVPixelBufferPool recycling \
                 would re-enter the ring under an existing id"
                    .into(),
            ))
        }
        #[cfg(target_os = "linux")]
        {
            self.inner.allocate_additional_buffer()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(crate::core::Error::Configuration(
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
