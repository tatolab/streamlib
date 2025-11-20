// TODO(@jonathan): IOSurface module has unused utilities (PixelFormat enum, create_iosurface(), pixel_format_to_iosurface())
// Review if these are needed for future texture format support or can be removed
#![allow(dead_code)]

//! # IOSurface ↔ Metal Texture Conversion Utilities
//!
//! This module provides utilities for working with IOSurface-backed Metal textures.
//!
//! ## Two Common Conversion Patterns
//!
//! ### Pattern 1: IOSurface → Metal Texture (READING)
//! **Use case**: Reading camera frames, capturing from CVPixelBuffers
//! **Function**: `create_metal_texture_from_iosurface()` (this file)
//! **Example**: CameraProcessor receives CVPixelBuffer → extracts IOSurface → creates Metal texture → wraps in wgpu texture
//!
//! ```rust,ignore
//! // In CameraProcessor (libs/streamlib/src/apple/processors/camera.rs)
//! let iosurface = CVPixelBufferGetIOSurface(pixel_buffer);
//! let metal_texture = create_metal_texture_from_iosurface(device, &iosurface, 0)?;
//! // Now you can wrap this in wgpu or use directly with Metal
//! ```
//!
//! ### Pattern 2: wgpu Texture → Metal Texture → IOSurface → CVPixelBuffer (WRITING)
//! **Use case**: Writing video frames to AVAssetWriter (MP4 files), exporting to Photos, creating thumbnails
//! **Technique**: Use Metal BLITTING, not direct IOSurface extraction
//! **Example**: See Mp4WriterProcessor::write_video_frame()
//!
//! **IMPORTANT**: You CANNOT directly get an IOSurface from a Metal texture created by wgpu.
//! The `metal` crate does NOT have a `texture.iosurface()` method, and there is no public
//! `MTLTextureGetIOSurface()` FFI function in the Metal framework.
//!
//! **Instead, use this pattern**:
//! ```rust,ignore
//! // In Mp4WriterProcessor (libs/streamlib/src/apple/processors/mp4_writer.rs:564-679)
//!
//! // 1. Get source Metal texture from wgpu (already IOSurface-backed internally)
//! let source_metal = wgpu_bridge.unwrap_to_metal_texture(&wgpu_texture)?;
//!
//! // 2. Create destination CVPixelBuffer (from pool or manually)
//! let pixel_buffer = CVPixelBufferPool::create_pixel_buffer(&pool)?;
//!
//! // 3. Extract IOSurface from the CVPixelBuffer
//! let iosurface = CVPixelBufferGetIOSurface(&pixel_buffer);
//!
//! // 4. Create Metal texture from that IOSurface (destination)
//! let dest_metal = create_metal_texture_from_iosurface(device, &iosurface, 0)?;
//!
//! // 5. Use Metal blit command to copy source → destination
//! let command_buffer = command_queue.new_command_buffer();
//! let blit_encoder = command_buffer.new_blit_command_encoder();
//! blit_encoder.copy_from_texture(&source_metal, 0, 0, origin, size,
//!                                  dest_metal_ref, 0, 0, origin);
//! blit_encoder.end_encoding();
//! command_buffer.commit();
//! command_buffer.wait_until_completed();
//!
//! // 6. Now pixel_buffer contains the GPU texture data and can be used with AVAssetWriter
//! pixel_buffer_adaptor.appendPixelBuffer_withPresentationTime(&pixel_buffer, time);
//! ```
//!
//! **Why blitting instead of direct IOSurface access?**
//! - wgpu creates IOSurface-backed textures internally, but doesn't expose the IOSurface pointer
//! - The Metal framework's IOSurface APIs are private/undocumented for texture → IOSurface direction
//! - Blitting is the official, supported way to copy GPU texture data
//! - This approach works with AVAssetWriter's pixel buffer pool and is what Apple's APIs expect
//!
//! **See also**:
//! - DisplayProcessor (libs/streamlib/src/apple/processors/display.rs:164-244) for similar blitting to CAMetalDrawable
//! - WgpuBridge::unwrap_to_metal_texture() (libs/streamlib/src/apple/wgpu_bridge.rs) for wgpu → Metal conversion

use crate::core::{Result, StreamError};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_io_surface::IOSurface;
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTexture, MTLTextureDescriptor, MTLTextureUsage};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8Unorm,
    Rgba8Unorm,
}

/// Creates a Metal texture from an IOSurface.
///
/// This is used for READING from IOSurface-backed buffers (e.g., camera frames).
/// For WRITING to IOSurface/CVPixelBuffer (e.g., MP4 export), see the module-level
/// documentation for the blitting pattern.
pub fn create_metal_texture_from_iosurface(
    device: &ProtocolObject<dyn MTLDevice>,
    iosurface: &IOSurface,
    plane: usize,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
    let width = iosurface.width();
    let height = iosurface.height();
    let pixel_format = iosurface.pixelFormat();

    let metal_format = iosurface_format_to_metal(pixel_format)?;

    let descriptor = MTLTextureDescriptor::new();
    unsafe {
        descriptor.setWidth(width as usize);
        descriptor.setHeight(height as usize);
        descriptor.setPixelFormat(metal_format);
        descriptor.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
    }

    use objc2_io_surface::IOSurfaceRef;
    let iosurface_ptr: *const IOSurfaceRef = iosurface as *const IOSurface as *const IOSurfaceRef;

    let texture: Option<Retained<ProtocolObject<dyn MTLTexture>>> = unsafe {
        msg_send![
            device,
            newTextureWithDescriptor: &*descriptor,
            iosurface: iosurface_ptr,
            plane: plane
        ]
    };

    texture.ok_or_else(|| {
        StreamError::TextureError(format!(
            "Failed to create Metal texture from IOSurface (width={}, height={}, format={})",
            width, height, pixel_format
        ))
    })
}

pub fn create_iosurface(
    width: usize,
    height: usize,
    pixel_format: PixelFormat,
) -> Result<Retained<IOSurface>> {
    use objc2::runtime::AnyObject;
    use objc2_foundation::{ns_string, NSNumber, NSString};

    let ios_format = pixel_format_to_iosurface(pixel_format)?;

    let bytes_per_element = match pixel_format {
        PixelFormat::Rgba8Unorm | PixelFormat::Bgra8Unorm => 4,
    };
    let bytes_per_row = (width * bytes_per_element).div_ceil(64) * 64; // Align to 64 bytes

    let val_width = NSNumber::new_usize(width);
    let val_height = NSNumber::new_usize(height);
    let val_pixel_format = NSNumber::new_u32(ios_format);
    let val_bytes_per_element = NSNumber::new_usize(bytes_per_element);
    let val_bytes_per_row = NSNumber::new_usize(bytes_per_row);

    use objc2_foundation::NSDictionary;

    let keys: Vec<&NSString> = vec![
        ns_string!("IOSurfaceWidth"),
        ns_string!("IOSurfaceHeight"),
        ns_string!("IOSurfacePixelFormat"),
        ns_string!("IOSurfaceBytesPerElement"),
        ns_string!("IOSurfaceBytesPerRow"),
    ];

    let values: Vec<&AnyObject> = vec![
        (&*val_width as &NSNumber).as_super(),
        (&*val_height as &NSNumber).as_super(),
        (&*val_pixel_format as &NSNumber).as_super(),
        (&*val_bytes_per_element as &NSNumber).as_super(),
        (&*val_bytes_per_row as &NSNumber).as_super(),
    ];

    let properties = NSDictionary::from_slices(&keys, &values);

    use objc2::runtime::AnyClass;
    use objc2::ClassType;

    let cls: &AnyClass = IOSurface::class();
    let allocated_ptr: *mut IOSurface = unsafe { msg_send![cls, alloc] };

    let surface_ptr: *mut IOSurface =
        unsafe { msg_send![allocated_ptr, initWithProperties: &*properties] };

    let surface = unsafe { Retained::from_raw(surface_ptr) }.ok_or_else(|| {
        StreamError::TextureError(format!(
            "Failed to create IOSurface with dimensions {}x{}, format={:?}",
            width, height, pixel_format
        ))
    })?;

    let actual_width = surface.width() as usize;
    let actual_height = surface.height() as usize;

    if actual_width != width || actual_height != height {
        return Err(StreamError::TextureError(format!(
            "IOSurface created with wrong dimensions: expected {}x{}, got {}x{}",
            width, height, actual_width, actual_height
        )));
    }

    Ok(surface)
}

fn iosurface_format_to_metal(ios_format: u32) -> Result<MTLPixelFormat> {
    match ios_format {
        0x42475241 => Ok(MTLPixelFormat::BGRA8Unorm), // 'BGRA' - most common on macOS
        0x52474241 => Ok(MTLPixelFormat::RGBA8Unorm), // 'RGBA'
        _ => Err(StreamError::NotSupported(format!(
            "IOSurface pixel format 0x{:08X} not supported",
            ios_format
        ))),
    }
}

fn pixel_format_to_iosurface(format: PixelFormat) -> Result<u32> {
    match format {
        PixelFormat::Bgra8Unorm => Ok(0x42475241), // 'BGRA'
        PixelFormat::Rgba8Unorm => Ok(0x52474241), // 'RGBA'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iosurface_creation() {
        let surface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm);
        assert!(surface.is_ok());

        let surface = surface.unwrap();
        assert_eq!(surface.width(), 1920);
        assert_eq!(surface.height(), 1080);
    }

    #[test]
    fn test_metal_texture_from_iosurface() {
        use objc2_metal::MTLCreateSystemDefaultDevice;

        let device = MTLCreateSystemDefaultDevice().expect("No Metal device available");

        let surface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        let texture = create_metal_texture_from_iosurface(&device, &surface, 0);
        assert!(texture.is_ok());

        let texture = texture.unwrap();
        assert_eq!(texture.width(), 1920);
        assert_eq!(texture.height(), 1080);
        assert_eq!(texture.pixelFormat(), MTLPixelFormat::BGRA8Unorm);
    }

    #[test]
    fn test_format_conversions() {
        assert_eq!(
            iosurface_format_to_metal(0x42475241).unwrap(),
            MTLPixelFormat::BGRA8Unorm
        );
        assert_eq!(
            iosurface_format_to_metal(0x52474241).unwrap(),
            MTLPixelFormat::RGBA8Unorm
        );

        assert_eq!(
            pixel_format_to_iosurface(PixelFormat::Bgra8Unorm).unwrap(),
            0x42475241
        );
        assert_eq!(
            pixel_format_to_iosurface(PixelFormat::Rgba8Unorm).unwrap(),
            0x52474241
        );
    }
}
