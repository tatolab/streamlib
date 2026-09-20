// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GPU blit operations with texture caching.

use super::PixelBuffer;
use crate::core::Result;

/// Trait for GPU blit operations with texture caching.
pub trait RhiBlitter: Send + Sync {
    /// Copy pixels between same-format, same-size buffers.
    fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()>;

    /// Clear texture cache to free GPU memory.
    fn clear_cache(&self);
}
