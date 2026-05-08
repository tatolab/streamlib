// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GPU blit operations with texture caching.

use super::RhiPixelBuffer;
use crate::core::Result;

/// Trait for GPU blit operations with texture caching.
pub trait RhiBlitter: Send + Sync {
    /// Copy pixels between same-format, same-size buffers.
    fn blit_copy(&self, src: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()>;

    /// Copy from raw IOSurface (platform-specific, unsafe).
    ///
    /// # Safety
    /// - `src` must be a valid IOSurfaceRef pointer
    /// - The IOSurface must remain valid for the duration of the blit
    unsafe fn blit_copy_iosurface_raw(
        &self,
        src: *const std::ffi::c_void,
        dest: &RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()>;

    /// Clear texture cache to free GPU memory.
    fn clear_cache(&self);
}
