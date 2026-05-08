// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal texture implementation for RHI.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_io_surface::IOSurface;
use objc2_metal::MTLTexture;

use crate::core::rhi::TextureFormat;

/// Metal texture wrapper.
///
/// Can be backed by a regular Metal texture or an IOSurface for cross-framework sharing.
pub struct MetalTexture {
    texture: SendSyncMtlTexture,
    iosurface: Option<Retained<IOSurface>>,
    iosurface_id: Option<u32>,
    width: u32,
    height: u32,
    format: TextureFormat,
}

/// Wrapper to make MTLTexture Send + Sync.
///
/// SAFETY: Metal textures are thread-safe for rendering operations.
/// The GPU manages synchronization internally.
pub struct SendSyncMtlTexture(pub Retained<ProtocolObject<dyn MTLTexture>>);

unsafe impl Send for SendSyncMtlTexture {}
unsafe impl Sync for SendSyncMtlTexture {}

impl MetalTexture {
    /// Create a new Metal texture wrapper.
    pub fn new(
        texture: Retained<ProtocolObject<dyn MTLTexture>>,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        Self {
            texture: SendSyncMtlTexture(texture),
            iosurface: None,
            iosurface_id: None,
            width,
            height,
            format,
        }
    }

    /// Create a Metal texture backed by an IOSurface.
    pub fn with_iosurface(
        texture: Retained<ProtocolObject<dyn MTLTexture>>,
        iosurface: Retained<IOSurface>,
        iosurface_id: u32,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        Self {
            texture: SendSyncMtlTexture(texture),
            iosurface: Some(iosurface),
            iosurface_id: Some(iosurface_id),
            width,
            height,
            format,
        }
    }

    /// Texture width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Texture height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Texture format.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Get the IOSurface ID for cross-framework sharing.
    pub fn iosurface_id(&self) -> Option<u32> {
        self.iosurface_id
    }

    /// Get the underlying IOSurface if this texture is IOSurface-backed.
    pub fn iosurface(&self) -> Option<&IOSurface> {
        self.iosurface.as_deref()
    }

    /// Get the underlying Metal texture.
    pub fn as_metal_texture(&self) -> &metal::TextureRef {
        use metal::foreign_types::ForeignTypeRef;
        // Get the Objective-C object pointer from the protocol object
        let obj_ptr =
            &*self.texture.0 as *const ProtocolObject<dyn MTLTexture> as *mut std::ffi::c_void;
        // SAFETY: The Retained keeps the texture alive for the lifetime of self
        unsafe { metal::TextureRef::from_ptr(obj_ptr as *mut _) }
    }

    /// Get the raw Metal texture protocol object.
    pub fn metal_texture(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.texture.0
    }
}

impl Clone for MetalTexture {
    fn clone(&self) -> Self {
        Self {
            texture: SendSyncMtlTexture(Retained::clone(&self.texture.0)),
            iosurface: self.iosurface.clone(),
            iosurface_id: self.iosurface_id,
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::TextureDescriptor;
    use crate::metal::MetalDevice;

    #[test]
    fn test_metal_texture_properties() {
        let device = MetalDevice::new().expect("Metal device");
        let desc = TextureDescriptor::new(128, 64, TextureFormat::Bgra8Unorm);
        let texture = device.create_texture(&desc).expect("texture");

        assert_eq!(texture.width(), 128);
        assert_eq!(texture.height(), 64);
        assert_eq!(texture.format(), TextureFormat::Bgra8Unorm);
        assert!(texture.iosurface_id().is_none());
    }
}
