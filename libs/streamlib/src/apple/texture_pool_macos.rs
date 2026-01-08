// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS-specific texture pool implementation using IOSurface.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use objc2_io_surface::IOSurface;

use super::iosurface::{create_iosurface, create_metal_texture_from_iosurface, PixelFormat};
use crate::apple::rhi::MetalTexture;
use crate::core::context::texture_pool::{
    PoolSlot, TexturePoolDescriptor, TexturePoolInner, TexturePoolKey,
};
use crate::core::rhi::{StreamTexture, TextureFormat};
use crate::core::{Result, StreamError};

// FFI binding to get IOSurface ID for cross-process sharing
#[allow(clashing_extern_declarations)]
#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceGetID(surface: *const IOSurface) -> u32;
}

/// Get the IOSurface ID for cross-process sharing.
pub fn get_iosurface_id(surface: &IOSurface) -> u32 {
    unsafe { IOSurfaceGetID(surface as *const IOSurface) }
}

/// Convert RHI texture format to IOSurface pixel format.
fn rhi_format_to_pixel_format(format: TextureFormat) -> Result<PixelFormat> {
    match format {
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => Ok(PixelFormat::Bgra32),
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => Ok(PixelFormat::Rgba32),
        _ => Err(StreamError::TextureError(format!(
            "Unsupported texture format for IOSurface: {:?}",
            format
        ))),
    }
}

/// Allocate an IOSurface-backed texture slot.
pub fn allocate_iosurface_slot(
    pool_inner: &TexturePoolInner,
    desc: &TexturePoolDescriptor,
) -> Result<Arc<PoolSlot>> {
    // Get Metal device from pool inner's GpuDevice
    let metal_device = pool_inner.device.as_metal_device();

    // Convert RHI format to IOSurface format
    let pixel_format = rhi_format_to_pixel_format(desc.format)?;

    // Create IOSurface
    let iosurface = create_iosurface(desc.width as usize, desc.height as usize, pixel_format)?;

    // Get IOSurface ID for cross-process sharing
    let iosurface_id = get_iosurface_id(&iosurface);

    // Create Metal texture from IOSurface
    let metal_texture_raw =
        create_metal_texture_from_iosurface(metal_device.device(), &iosurface, 0)?;

    // Wrap in MetalTexture with IOSurface backing
    let metal_texture = MetalTexture::with_iosurface(
        metal_texture_raw,
        iosurface,
        iosurface_id,
        desc.width,
        desc.height,
        desc.format,
    );

    // Wrap in StreamTexture
    let stream_texture = StreamTexture::from_metal(metal_texture);

    let slot = PoolSlot {
        id: pool_inner.next_slot_id(),
        texture: stream_texture,
        key: TexturePoolKey::from_descriptor(desc),
        in_use: AtomicBool::new(false),
    };

    tracing::debug!(
        "Allocated IOSurface-backed texture: {}x{} format={:?} id={}",
        desc.width,
        desc.height,
        desc.format,
        iosurface_id
    );

    Ok(Arc::new(slot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_iosurface_id() {
        let surface =
            create_iosurface(64, 64, PixelFormat::Rgba32).expect("Failed to create IOSurface");

        let id = get_iosurface_id(&surface);
        // ID should be non-zero for a valid IOSurface
        assert!(id > 0, "IOSurface ID should be non-zero");
    }

    #[test]
    fn test_rhi_format_conversion() {
        assert!(rhi_format_to_pixel_format(TextureFormat::Rgba8Unorm).is_ok());
        assert!(rhi_format_to_pixel_format(TextureFormat::Bgra8Unorm).is_ok());
        assert!(rhi_format_to_pixel_format(TextureFormat::Nv12).is_err());
    }
}
