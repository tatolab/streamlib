// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal device implementation for RHI.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice};

use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
use crate::core::{Result, StreamError};

use super::{MetalCommandQueue, MetalTexture};

/// Metal GPU device.
pub struct MetalDevice {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

impl MetalDevice {
    /// Create a new Metal device.
    pub fn new() -> Result<Self> {
        let device = MTLCreateSystemDefaultDevice().ok_or_else(|| {
            StreamError::GpuError(
                "No Metal device available on this system. Metal requires macOS 10.11+ or iOS 8+."
                    .into(),
            )
        })?;

        let command_queue = device
            .newCommandQueue()
            .ok_or_else(|| StreamError::GpuError("Failed to create Metal command queue".into()))?;

        Ok(Self {
            device,
            command_queue,
        })
    }

    /// Create a texture on this device.
    pub fn create_texture(&self, desc: &TextureDescriptor) -> Result<MetalTexture> {
        use objc2_metal::MTLTextureDescriptor;

        let metal_format = texture_format_to_metal(desc.format);
        let metal_usage = texture_usages_to_metal(desc.usage);

        let texture_desc = MTLTextureDescriptor::new();
        unsafe {
            texture_desc.setWidth(desc.width as usize);
            texture_desc.setHeight(desc.height as usize);
            texture_desc.setPixelFormat(metal_format);
            texture_desc.setUsage(metal_usage);
        }

        let texture = self
            .device
            .newTextureWithDescriptor(&texture_desc)
            .ok_or_else(|| {
                StreamError::TextureError(format!(
                    "Failed to create Metal texture {}x{} format={:?}",
                    desc.width, desc.height, desc.format
                ))
            })?;

        Ok(MetalTexture::new(
            texture,
            desc.width,
            desc.height,
            desc.format,
        ))
    }

    /// Get a reference to the raw Metal device.
    pub fn device_ref(&self) -> &metal::DeviceRef {
        use metal::foreign_types::ForeignTypeRef;
        // Get the Objective-C object pointer from the protocol object
        let obj_ptr =
            &*self.device as *const ProtocolObject<dyn MTLDevice> as *mut std::ffi::c_void;
        // SAFETY: The Retained keeps the device alive for the lifetime of self
        unsafe { metal::DeviceRef::from_ptr(obj_ptr as *mut _) }
    }

    /// Get the raw Metal device protocol object.
    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    /// Clone the Metal device handle.
    pub fn clone_device(&self) -> Retained<ProtocolObject<dyn MTLDevice>> {
        Retained::clone(&self.device)
    }

    /// Get the command queue.
    pub fn command_queue(&self) -> &ProtocolObject<dyn MTLCommandQueue> {
        &self.command_queue
    }

    /// Clone the command queue handle.
    pub fn clone_command_queue(&self) -> Retained<ProtocolObject<dyn MTLCommandQueue>> {
        Retained::clone(&self.command_queue)
    }

    /// Create a MetalCommandQueue wrapper for the shared command queue.
    pub fn create_command_queue_wrapper(&self) -> MetalCommandQueue {
        MetalCommandQueue::new(self.clone_command_queue())
    }

    /// Get the device name.
    pub fn name(&self) -> String {
        self.device.name().to_string()
    }
}

/// Convert RHI TextureFormat to Metal pixel format.
fn texture_format_to_metal(format: TextureFormat) -> objc2_metal::MTLPixelFormat {
    use objc2_metal::MTLPixelFormat;
    match format {
        TextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        TextureFormat::Rgba8UnormSrgb => MTLPixelFormat::RGBA8Unorm_sRGB,
        TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        TextureFormat::Bgra8UnormSrgb => MTLPixelFormat::BGRA8Unorm_sRGB,
        TextureFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
        TextureFormat::Rgba32Float => MTLPixelFormat::RGBA32Float,
        TextureFormat::Nv12 => MTLPixelFormat::Invalid, // NV12 requires special handling
    }
}

/// Convert RHI TextureUsages to Metal texture usage.
fn texture_usages_to_metal(usages: TextureUsages) -> objc2_metal::MTLTextureUsage {
    use objc2_metal::MTLTextureUsage;

    let mut metal_usage = MTLTextureUsage::empty();

    if usages.contains(TextureUsages::TEXTURE_BINDING) {
        metal_usage |= MTLTextureUsage::ShaderRead;
    }
    if usages.contains(TextureUsages::STORAGE_BINDING) {
        metal_usage |= MTLTextureUsage::ShaderWrite;
    }
    if usages.contains(TextureUsages::RENDER_ATTACHMENT) {
        metal_usage |= MTLTextureUsage::RenderTarget;
    }

    metal_usage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metal_device_creation() {
        let device = MetalDevice::new();
        assert!(device.is_ok(), "Metal should be available on macOS/iOS");
    }

    #[test]
    fn test_metal_device_name() {
        let device = MetalDevice::new().expect("Metal device");
        let name = device.name();
        assert!(!name.is_empty(), "Metal device should have a name");
    }

    #[test]
    fn test_texture_creation() {
        let device = MetalDevice::new().expect("Metal device");
        let desc = TextureDescriptor::new(64, 64, TextureFormat::Rgba8Unorm);
        let texture = device.create_texture(&desc);
        assert!(texture.is_ok(), "Should create texture");

        let texture = texture.unwrap();
        assert_eq!(texture.width(), 64);
        assert_eq!(texture.height(), 64);
        assert_eq!(texture.format(), TextureFormat::Rgba8Unorm);
    }
}
