//! Metal texture utilities for WebGPU bridge
//!
//! This module provides helper functions for creating and managing
//! Metal textures that will be bridged to WebGPU via WgpuBridge.
//!
//! In the WebGPU-first architecture, streamlib-apple is a driver layer
//! that exposes native Metal textures, which are then converted to
//! WebGPU textures by WgpuBridge. From that point on, everything in
//! streamlib-core speaks only WebGPU.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTexture, MTLTextureDescriptor, MTLTextureUsage};
use objc2_metal::MTLTextureType;
use crate::core::{Result, StreamError};

/// Create a Metal texture with the specified dimensions and format
///
/// This is a convenience function for creating Metal textures that will
/// later be bridged to WebGPU via `WgpuBridge::wrap_metal_texture()`.
///
/// # Arguments
/// * `device` - The Metal device to create the texture on
/// * `width` - Texture width in pixels
/// * `height` - Texture height in pixels
/// * `format` - Metal pixel format
/// * `usage` - Metal texture usage flags
///
/// # Returns
/// A Metal texture ready to be bridged to WebGPU
pub fn create_metal_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    width: usize,
    height: usize,
    format: MTLPixelFormat,
    usage: MTLTextureUsage,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
    let descriptor = MTLTextureDescriptor::new();

    unsafe {
        descriptor.setTextureType(MTLTextureType::Type2D);
        descriptor.setWidth(width);
        descriptor.setHeight(height);
        descriptor.setPixelFormat(format);
        descriptor.setUsage(usage);
    }

    let texture = device.newTextureWithDescriptor(&descriptor)
        .ok_or_else(|| StreamError::TextureError(
            format!("Failed to create Metal texture ({}x{}, format={:?})", width, height, format)
        ))?;

    Ok(texture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_metal::MTLCreateSystemDefaultDevice;

    #[test]
    fn test_create_metal_texture() {
        let device = MTLCreateSystemDefaultDevice().expect("No Metal device available");

        let texture = create_metal_texture(
            &device,
            1920,
            1080,
            MTLPixelFormat::BGRA8Unorm,
            MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget,
        );

        assert!(texture.is_ok());
        let texture = texture.unwrap();
        assert_eq!(texture.width(), 1920);
        assert_eq!(texture.height(), 1080);
        assert_eq!(texture.pixelFormat(), MTLPixelFormat::BGRA8Unorm);
    }
}
