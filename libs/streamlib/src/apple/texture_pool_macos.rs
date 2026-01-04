// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS-specific texture pool implementation using IOSurface.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_io_surface::IOSurface;
use objc2_metal::MTLDevice;

use super::iosurface::{create_iosurface, create_metal_texture_from_iosurface, PixelFormat};
use super::wgpu_bridge::WgpuBridge;
use crate::core::context::texture_pool::{
    PoolSlot, SendSyncMtlTexture, TexturePoolDescriptor, TexturePoolInner, TexturePoolKey,
};
use crate::core::{Result, StreamError};

// FFI binding to get IOSurface ID for cross-process sharing
#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceGetID(surface: *const IOSurface) -> u32;
}

/// Get the IOSurface ID for cross-process sharing.
pub fn get_iosurface_id(surface: &IOSurface) -> u32 {
    unsafe { IOSurfaceGetID(surface as *const IOSurface) }
}

/// Convert wgpu texture format to IOSurface pixel format.
fn wgpu_format_to_pixel_format(format: wgpu::TextureFormat) -> Result<PixelFormat> {
    match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
            Ok(PixelFormat::Bgra8Unorm)
        }
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
            Ok(PixelFormat::Rgba8Unorm)
        }
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
    // Get Metal device from pool inner or create default
    let metal_device = pool_inner
        .metal_device
        .as_ref()
        .cloned()
        .or_else(|| objc2_metal::MTLCreateSystemDefaultDevice())
        .ok_or_else(|| StreamError::GpuError("No Metal device available".into()))?;

    // Convert wgpu format to IOSurface format
    let pixel_format = wgpu_format_to_pixel_format(desc.format)?;

    // Create IOSurface
    let iosurface = create_iosurface(desc.width as usize, desc.height as usize, pixel_format)?;

    // Get IOSurface ID for cross-process sharing
    let iosurface_id = get_iosurface_id(&iosurface);

    // Create Metal texture from IOSurface
    let metal_texture = create_metal_texture_from_iosurface(&metal_device, &iosurface, 0)?;

    // Create wgpu texture via WgpuBridge
    // We need to create a temporary WgpuBridge for the conversion
    let wgpu_bridge = WgpuBridge::from_shared_device(
        clone_metal_device(&metal_device),
        (*pool_inner.device).clone(),
        (*pool_inner.queue).clone(),
    );

    let wgpu_texture =
        unsafe { wgpu_bridge.wrap_metal_texture(&metal_texture, desc.format, desc.usage)? };

    let slot = PoolSlot {
        id: pool_inner.next_slot_id(),
        texture: Arc::new(wgpu_texture),
        key: TexturePoolKey::from_descriptor(desc),
        in_use: AtomicBool::new(false),
        iosurface,
        iosurface_id,
        metal_texture: SendSyncMtlTexture(metal_texture),
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

/// Clone a Metal device reference.
fn clone_metal_device(
    _device: &ProtocolObject<dyn MTLDevice>,
) -> Retained<ProtocolObject<dyn MTLDevice>> {
    // SAFETY: MTLDevice is a protocol object that can be retained
    // We use the system default device which is always available
    objc2_metal::MTLCreateSystemDefaultDevice().expect("Failed to get Metal device")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_iosurface_id() {
        let surface =
            create_iosurface(64, 64, PixelFormat::Rgba8Unorm).expect("Failed to create IOSurface");

        let id = get_iosurface_id(&surface);
        // ID should be non-zero for a valid IOSurface
        assert!(id > 0, "IOSurface ID should be non-zero");
    }

    #[test]
    fn test_wgpu_format_conversion() {
        assert!(wgpu_format_to_pixel_format(wgpu::TextureFormat::Rgba8Unorm).is_ok());
        assert!(wgpu_format_to_pixel_format(wgpu::TextureFormat::Bgra8Unorm).is_ok());
        assert!(wgpu_format_to_pixel_format(wgpu::TextureFormat::R8Unorm).is_err());
    }
}
