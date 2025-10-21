//! IOSurface zero-copy texture sharing
//!
//! Provides zero-copy GPU texture sharing between processes and frameworks.
//! Works on both macOS and iOS.
//!
//! IOSurface allows us to share GPU memory between different frameworks
//! (AVFoundation, Metal, Core Image) and even across processes - all with
//! ZERO copies. This is the foundation of streamlib's performance.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::msg_send;
use objc2_io_surface::{IOSurface, IOSurfaceRef};
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTexture, MTLTextureDescriptor, MTLTextureUsage};
use streamlib_core::{PixelFormat, Result, StreamError};

/// Creates a Metal texture from an IOSurface (zero-copy)
///
/// This is the critical function that enables zero-copy GPU operations.
/// The Metal texture directly references the IOSurface's GPU memory.
///
/// # Arguments
/// * `device` - The Metal device to create the texture on
/// * `iosurface` - The IOSurface containing the pixel data
/// * `plane` - The plane index (0 for most formats, 1+ for planar formats like NV12)
///
/// # Returns
/// A Metal texture that shares memory with the IOSurface
pub fn create_metal_texture_from_iosurface(
    device: &ProtocolObject<dyn MTLDevice>,
    iosurface: &IOSurface,
    plane: usize,
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>> {
    // Get IOSurface properties
    let width = iosurface.width();
    let height = iosurface.height();
    let pixel_format = iosurface.pixelFormat();

    // Convert IOSurface pixel format to Metal pixel format
    let metal_format = iosurface_format_to_metal(pixel_format)?;

    // Create texture descriptor
    let descriptor = MTLTextureDescriptor::new();
    unsafe {
        descriptor.setWidth(width as usize);
        descriptor.setHeight(height as usize);
        descriptor.setPixelFormat(metal_format);
        descriptor.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
    }

    // Create Metal texture from IOSurface (ZERO-COPY!)
    // This is the magic call that shares GPU memory
    //
    // Unfortunately, objc2-metal doesn't expose newTextureWithDescriptor:iosurface:plane: yet,
    // so we need to call it directly using msg_send
    //
    // Metal expects an IOSurfaceRef (CF type), not the Objective-C IOSurface object
    // IOSurface is toll-free bridged, so we can cast the pointer
    let iosurface_ref: *const IOSurfaceRef = iosurface as *const _ as *const IOSurfaceRef;

    let texture: Option<Retained<ProtocolObject<dyn MTLTexture>>> = unsafe {
        msg_send![
            device,
            newTextureWithDescriptor: &*descriptor,
            iosurface: iosurface_ref,
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

/// Creates an IOSurface with the specified dimensions and pixel format
///
/// This creates a new IOSurface that can be shared across frameworks.
/// Useful when you need to create a destination surface for rendering.
pub fn create_iosurface(
    width: usize,
    height: usize,
    pixel_format: PixelFormat,
) -> Result<Retained<IOSurface>> {
    use objc2_foundation::{ns_string, NSNumber, NSString};
    use objc2::runtime::AnyObject;

    // Convert streamlib pixel format to IOSurface format
    let ios_format = pixel_format_to_iosurface(pixel_format)?;

    // Calculate bytes per row (width * bytes per pixel, aligned to 64 bytes)
    let bytes_per_element = match pixel_format {
        PixelFormat::Rgba8Unorm => 4,
        PixelFormat::Bgra8Unorm => 4,
        _ => {
            return Err(StreamError::NotSupported(format!(
                "Pixel format {:?} not yet supported",
                pixel_format
            )))
        }
    };
    let bytes_per_row = (width * bytes_per_element).div_ceil(64) * 64; // Align to 64 bytes

    // Create values as NSNumber
    let val_width = NSNumber::new_usize(width);
    let val_height = NSNumber::new_usize(height);
    let val_pixel_format = NSNumber::new_u32(ios_format);
    let val_bytes_per_element = NSNumber::new_usize(bytes_per_element);
    let val_bytes_per_row = NSNumber::new_usize(bytes_per_row);

    // Create properties dictionary
    // IOSurface expects specific keys with NSNumber values
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

    // Create IOSurface with properties
    // Use msg_send directly with raw pointers since IOSurface::alloc isn't exposed
    use objc2::ClassType;
    use objc2::runtime::AnyClass;

    let cls: &AnyClass = IOSurface::class();
    let allocated_ptr: *mut IOSurface = unsafe {
        msg_send![cls, alloc]
    };

    let surface_ptr: *mut IOSurface = unsafe {
        msg_send![allocated_ptr, initWithProperties: &*properties]
    };

    // Convert to Retained (from_raw returns Option<Retained<T>>)
    let surface = unsafe {
        Retained::from_raw(surface_ptr)
    }.ok_or_else(|| {
        StreamError::TextureError(format!(
            "Failed to create IOSurface with dimensions {}x{}, format={:?}",
            width, height, pixel_format
        ))
    })?;

    // Verify dimensions match what we requested
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

/// Convert IOSurface pixel format (FourCC) to Metal pixel format
fn iosurface_format_to_metal(ios_format: u32) -> Result<MTLPixelFormat> {
    // IOSurface uses FourCC codes
    // See: https://developer.apple.com/documentation/corevideo/1563591-pixel_format_identifiers
    match ios_format {
        0x42475241 => Ok(MTLPixelFormat::BGRA8Unorm), // 'BGRA' - most common on macOS
        0x52474241 => Ok(MTLPixelFormat::RGBA8Unorm), // 'RGBA'
        _ => Err(StreamError::NotSupported(format!(
            "IOSurface pixel format 0x{:08X} not supported",
            ios_format
        ))),
    }
}

/// Convert streamlib pixel format to IOSurface FourCC format
fn pixel_format_to_iosurface(format: PixelFormat) -> Result<u32> {
    match format {
        PixelFormat::Bgra8Unorm => Ok(0x42475241), // 'BGRA'
        PixelFormat::Rgba8Unorm => Ok(0x52474241), // 'RGBA'
        _ => Err(StreamError::NotSupported(format!(
            "Pixel format {:?} not supported for IOSurface",
            format
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iosurface_creation() {
        // Create an IOSurface
        let surface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm);
        assert!(surface.is_ok());

        let surface = surface.unwrap();
        assert_eq!(surface.width(), 1920);
        assert_eq!(surface.height(), 1080);
    }

    #[test]
    fn test_metal_texture_from_iosurface() {
        use objc2_metal::MTLCreateSystemDefaultDevice;

        // Create Metal device
        let device = MTLCreateSystemDefaultDevice().expect("No Metal device available");

        // Create IOSurface
        let surface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        // Create Metal texture from IOSurface (zero-copy!)
        let texture = create_metal_texture_from_iosurface(&device, &surface, 0);
        assert!(texture.is_ok());

        let texture = texture.unwrap();
        assert_eq!(texture.width(), 1920);
        assert_eq!(texture.height(), 1080);
        assert_eq!(texture.pixelFormat(), MTLPixelFormat::BGRA8Unorm);
    }

    #[test]
    fn test_format_conversions() {
        // IOSurface to Metal
        assert_eq!(
            iosurface_format_to_metal(0x42475241).unwrap(),
            MTLPixelFormat::BGRA8Unorm
        );
        assert_eq!(
            iosurface_format_to_metal(0x52474241).unwrap(),
            MTLPixelFormat::RGBA8Unorm
        );

        // StreamLib to IOSurface
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
