// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine-allocated render-target image an in-process adapter test wraps.

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::error::Error;
use streamlib::sdk::rhi::{Texture, TextureFormat};

/// A DMA-BUF image on Linux, an image over a private IOSurface on macOS.
pub fn acquire_render_target_texture(
    gpu: &GpuContext,
    width: u32,
    height: u32,
    format: TextureFormat,
) -> Result<Texture, Error> {
    #[cfg(target_os = "linux")]
    return gpu.acquire_render_target_dma_buf_image(width, height, format);
    #[cfg(target_os = "macos")]
    return gpu.acquire_render_target_iosurface_image(width, height, format);
}
