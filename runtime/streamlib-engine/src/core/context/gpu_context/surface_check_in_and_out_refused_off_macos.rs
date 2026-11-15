// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Check in a pixel buffer (non-macOS stub).
    pub fn check_in_surface(&self, _pixel_buffer: &PixelBuffer) -> Result<String> {
        Err(crate::core::Error::NotSupported(
            "Surface store is only supported on macOS".into(),
        ))
    }

    /// Check out a surface (non-macOS stub).
    pub fn check_out_surface(&self, _surface_id: &str) -> Result<PixelBuffer> {
        Err(crate::core::Error::NotSupported(
            "Surface store is only supported on macOS".into(),
        ))
    }
}
