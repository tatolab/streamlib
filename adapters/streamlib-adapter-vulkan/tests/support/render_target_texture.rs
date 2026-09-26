// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine-allocated image an in-process adapter test wraps, on each floor.

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::error::Error;
use streamlib::sdk::rhi::{Texture, TextureFormat};

/// A tiled, render-target-capable DMA-BUF image.
#[cfg(target_os = "linux")]
pub fn acquire_render_target_texture(
    gpu: &GpuContext,
    width: u32,
    height: u32,
    format: TextureFormat,
) -> Result<Texture, Error> {
    gpu.acquire_render_target_dma_buf_image(width, height, format)
}

/// An image over a private IOSurface, carrying the usage set the Linux
/// render-target allocation does.
#[cfg(target_os = "macos")]
pub fn acquire_render_target_texture(
    gpu: &GpuContext,
    width: u32,
    height: u32,
    format: TextureFormat,
) -> Result<Texture, Error> {
    use streamlib::sdk::rhi::{TextureDescriptor, TextureUsages};
    gpu.device().create_texture_iosurface_backed(
        &TextureDescriptor::new(width, height, format).with_usage(
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::STORAGE_BINDING,
        ),
    )
}
