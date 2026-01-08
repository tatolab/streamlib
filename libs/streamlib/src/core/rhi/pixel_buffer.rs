// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffer with cached dimensions.

use std::sync::Arc;

use super::{PixelFormat, RhiPixelBufferRef};

/// Pixel buffer with cached dimensions.
///
/// Wraps [`RhiPixelBufferRef`] in an Arc for cheap cloning. Clone only increments
/// the Arc refcount - it does NOT increment the platform buffer refcount (e.g.,
/// CVPixelBufferRetain on macOS). This is critical for avoiding memory leaks
/// when sharing buffers between Rust and Python.
///
/// The platform buffer is retained exactly once (when created) and released
/// exactly once (when the last Arc reference is dropped).
#[derive(Clone)]
pub struct RhiPixelBuffer {
    /// The underlying platform buffer reference, shared via Arc.
    /// Clone increments Arc refcount, NOT platform refcount.
    pub(crate) ref_: Arc<RhiPixelBufferRef>,
    /// Cached width (queried once at construction).
    pub width: u32,
    /// Cached height (queried once at construction).
    pub height: u32,
}

impl RhiPixelBuffer {
    /// Create from a platform buffer reference.
    ///
    /// Queries width and height from the platform once and caches them.
    /// The RhiPixelBufferRef is wrapped in Arc for cheap cloning.
    pub fn new(ref_: RhiPixelBufferRef) -> Self {
        let width = ref_.width();
        let height = ref_.height();
        Self {
            ref_: Arc::new(ref_),
            width,
            height,
        }
    }

    /// Query the pixel format from the platform.
    pub fn format(&self) -> PixelFormat {
        self.ref_.format()
    }

    /// Get the underlying buffer reference.
    pub fn buffer_ref(&self) -> &RhiPixelBufferRef {
        &self.ref_
    }

    /// Get the raw platform pointer (CVPixelBufferRef on macOS).
    #[cfg(target_os = "macos")]
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.ref_.as_ptr()
    }
}

impl std::fmt::Debug for RhiPixelBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiPixelBuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format())
            .finish()
    }
}

// Send + Sync inherited from Arc<RhiPixelBufferRef>
