//! Metal GPU texture implementation for streamlib-core::GpuTexture

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLTexture;
use streamlib_core::{GpuTexture, GpuTextureHandle, PixelFormat, Result, StreamError};

/// Create a GpuTexture from a Metal texture
///
/// This wraps a Metal texture (typically from IOSurface) into streamlib's
/// platform-agnostic GpuTexture type for use in portable pipelines.
///
/// # Arguments
/// * `texture` - The Metal texture to wrap (from IOSurface or other source)
///
/// # Returns
/// A GpuTexture that can be used across streamlib's portable APIs
pub fn gpu_texture_from_metal(
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
) -> Result<GpuTexture> {
    let width = texture.width() as u32;
    let height = texture.height() as u32;

    // Convert Metal pixel format to streamlib PixelFormat
    let format = metal_format_to_pixel_format(texture.pixelFormat())?;

    // Store the Metal texture as a raw pointer in the handle
    // We use Retained::into_raw to transfer ownership to GpuTexture
    let texture_ptr = Retained::into_raw(texture) as u64;

    Ok(GpuTexture {
        handle: GpuTextureHandle::Metal {
            texture: texture_ptr,
        },
        width,
        height,
        format,
    })
}

/// Extract a Metal texture from a GpuTexture
///
/// This retrieves the underlying Metal texture from a GpuTexture.
/// Useful when you need to pass the texture to Metal-specific APIs.
///
/// # Safety
/// The GpuTexture must have been created on macOS/iOS with Metal backend.
/// The returned texture borrows from the GpuTexture and must not outlive it.
pub unsafe fn metal_texture_from_gpu_texture(
    gpu_texture: &GpuTexture,
) -> Result<&ProtocolObject<dyn MTLTexture>> {
    match &gpu_texture.handle {
        GpuTextureHandle::Metal { texture } => {
            let ptr = *texture as *const ProtocolObject<dyn MTLTexture>;
            Ok(&*ptr)
        }
        #[allow(unreachable_patterns)]
        _ => Err(StreamError::GpuError(
            "GpuTexture is not a Metal texture".into(),
        )),
    }
}

/// Convert Metal pixel format to streamlib PixelFormat
fn metal_format_to_pixel_format(
    metal_format: objc2_metal::MTLPixelFormat,
) -> Result<PixelFormat> {
    use objc2_metal::MTLPixelFormat;

    match metal_format {
        MTLPixelFormat::BGRA8Unorm => Ok(PixelFormat::Bgra8Unorm),
        MTLPixelFormat::RGBA8Unorm => Ok(PixelFormat::Rgba8Unorm),
        MTLPixelFormat::R8Unorm => Ok(PixelFormat::R8Unorm),
        _ => Err(StreamError::NotSupported(format!(
            "Metal pixel format {:?} not supported",
            metal_format
        ))),
    }
}

/// Drop implementation for GpuTexture to properly release Metal resources
///
/// This needs to be called from streamlib-core's Drop implementation
pub fn drop_metal_texture(handle: &GpuTextureHandle) {
    let GpuTextureHandle::Metal { texture } = handle;
    unsafe {
        // Reconstruct the Retained to drop it properly
        let ptr = *texture as *mut ProtocolObject<dyn MTLTexture>;
        let _texture = Retained::from_raw(ptr);
        // _texture is dropped here, releasing the Metal texture
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iosurface::create_iosurface;
    use crate::metal::MetalDevice;
    use crate::iosurface::create_metal_texture_from_iosurface;

    #[test]
    fn test_gpu_texture_from_metal() {
        let device = MetalDevice::new().expect("Metal device");

        // Create IOSurface and Metal texture
        let surface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm)
            .expect("IOSurface");
        let metal_texture = create_metal_texture_from_iosurface(device.device(), &surface, 0)
            .expect("Metal texture");

        // Wrap in GpuTexture
        let gpu_texture = gpu_texture_from_metal(metal_texture)
            .expect("GpuTexture");

        assert_eq!(gpu_texture.width, 1920);
        assert_eq!(gpu_texture.height, 1080);
        assert_eq!(gpu_texture.format, PixelFormat::Bgra8Unorm);

        // Extract Metal texture back
        let extracted = unsafe { metal_texture_from_gpu_texture(&gpu_texture) }
            .expect("Extract Metal texture");

        assert_eq!(extracted.width(), 1920);
        assert_eq!(extracted.height(), 1080);
    }
}
