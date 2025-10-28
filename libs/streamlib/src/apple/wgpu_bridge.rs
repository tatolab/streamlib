//! Metal ↔ WebGPU bridge for streamlib-apple
//!
//! This module provides zero-copy bridging between native Metal textures
//! and WebGPU (wgpu) textures, allowing platform-agnostic GPU code to
//! work with Apple's Metal API underneath.

use crate::{Result, StreamError};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDevice, MTLTexture};
use wgpu;
use wgpu::hal;
use metal;
use metal::foreign_types::ForeignTypeRef;

/// Bridge for converting between Metal and WebGPU resources
///
/// This is streamlib-apple's core wrapper that enables the WebGPU-first
/// architecture while using Metal natively underneath.
pub struct WgpuBridge {
    /// Native Metal device
    metal_device: Retained<ProtocolObject<dyn MTLDevice>>,

    /// WebGPU device (wraps Metal)
    wgpu_device: wgpu::Device,

    /// WebGPU queue
    wgpu_queue: wgpu::Queue,
}

impl WgpuBridge {
    /// Create a new WebGPU bridge from existing WebGPU device and queue (recommended)
    ///
    /// This creates a bridge using a shared WebGPU device provided by the runtime,
    /// ensuring all processors use the same GPU context for zero-copy texture sharing.
    ///
    /// # Arguments
    ///
    /// * `metal_device` - Native Metal device
    /// * `wgpu_device` - Shared WebGPU device from runtime
    /// * `wgpu_queue` - Shared WebGPU queue from runtime
    ///
    /// # Returns
    ///
    /// A bridge that can convert Metal resources to WebGPU using the shared device
    pub fn from_shared_device(
        metal_device: Retained<ProtocolObject<dyn MTLDevice>>,
        wgpu_device: wgpu::Device,
        wgpu_queue: wgpu::Queue,
    ) -> Self {
        Self {
            metal_device,
            wgpu_device,
            wgpu_queue,
        }
    }

    /// Create a new WebGPU bridge from a Metal device (legacy - creates its own device)
    ///
    /// **⚠️ DEPRECATED**: This method creates a new WebGPU device, which prevents
    /// texture sharing between processors. Use `from_shared_device()` instead with
    /// a device from streamlib-core's runtime.
    ///
    /// # Arguments
    ///
    /// * `metal_device` - Native Metal device to wrap
    ///
    /// # Returns
    ///
    /// A bridge that can convert Metal resources to WebGPU
    #[deprecated(note = "Use from_shared_device() instead to share GPU context with runtime")]
    pub async fn new(metal_device: Retained<ProtocolObject<dyn MTLDevice>>) -> Result<Self> {
        // Create wgpu instance
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });

        // Request adapter (Metal backend)
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| StreamError::GpuError(format!("Failed to find Metal adapter: {}", e)))?;

        // Request device and queue
        let (wgpu_device, wgpu_queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("StreamLib Metal Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: Default::default(),  // wgpu 25 uses path-based tracing
            })
            .await
            .map_err(|e| StreamError::GpuError(format!("Failed to create device: {}", e)))?;

        Ok(Self {
            metal_device,
            wgpu_device,
            wgpu_queue,
        })
    }

    /// Wrap a Metal texture as a WebGPU texture (zero-copy)
    ///
    /// This is the core bridging function that allows Metal textures to be
    /// used in WebGPU code without copying any data.
    ///
    /// # Arguments
    ///
    /// * `metal_texture` - Native Metal texture to wrap
    /// * `format` - WebGPU texture format
    /// * `usage` - WebGPU texture usage flags
    ///
    /// # Returns
    ///
    /// A WebGPU texture that references the same GPU memory as the Metal texture
    ///
    /// # Safety
    ///
    /// The Metal texture must remain valid for the lifetime of the WebGPU texture.
    pub unsafe fn wrap_metal_texture(
        &self,
        metal_texture: &ProtocolObject<dyn MTLTexture>,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Result<wgpu::Texture> {
        let width = metal_texture.width();
        let height = metal_texture.height();

        // Convert objc2_metal texture to metal crate texture
        let metal_texture_ptr = metal_texture as *const _ as *mut std::ffi::c_void;
        let metal_crate_texture = unsafe {
            metal::TextureRef::from_ptr(metal_texture_ptr as *mut _)
        }.to_owned();

        // Create wgpu-hal Metal texture from raw Metal texture
        let hal_texture = hal::metal::Device::texture_from_raw(
            metal_crate_texture,
            format,
            metal::MTLTextureType::D2,
            1,  // array_layers
            1,  // mip_levels
            hal::CopyExtent {
                width: width as u32,
                height: height as u32,
                depth: 1,
            },
        );

        // Wrap hal texture as wgpu::Texture
        let wgpu_texture = self.wgpu_device.create_texture_from_hal::<hal::api::Metal>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("Metal Bridge Texture"),
                size: wgpu::Extent3d {
                    width: width as u32,
                    height: height as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            },
        );

        Ok(wgpu_texture)
    }

    /// Get the WebGPU device
    pub fn wgpu_device(&self) -> &wgpu::Device {
        &self.wgpu_device
    }

    /// Get the WebGPU queue
    pub fn wgpu_queue(&self) -> &wgpu::Queue {
        &self.wgpu_queue
    }

    /// Get both WebGPU device and queue as a tuple
    pub fn wgpu(&self) -> (&wgpu::Device, &wgpu::Queue) {
        (&self.wgpu_device, &self.wgpu_queue)
    }

    /// Get the Metal device
    pub fn metal_device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.metal_device
    }

    /// Unwrap a WebGPU texture to get the underlying Metal texture (zero-copy)
    ///
    /// This is the reverse operation of `wrap_metal_texture`, allowing us to
    /// extract the native Metal texture from a WebGPU texture for use with
    /// Metal-specific APIs like blit encoders.
    ///
    /// # Arguments
    ///
    /// * `wgpu_texture` - WebGPU texture to unwrap
    ///
    /// # Returns
    ///
    /// The underlying Metal texture reference from the `metal` crate
    ///
    /// # Safety
    ///
    /// The returned Metal texture is only valid as long as the WebGPU texture exists.
    pub unsafe fn unwrap_to_metal_texture(
        &self,
        wgpu_texture: &wgpu::Texture,
    ) -> Result<metal::Texture> {
        // Get the HAL texture from the WebGPU texture
        // In wgpu 25, as_hal uses a callback pattern: as_hal<A, F, R>(callback: F)
        // where F: FnOnce(Option<&A::Texture>) -> R
        let metal_texture = wgpu_texture.as_hal::<hal::api::Metal, _, _>(|hal_texture_opt| {
            hal_texture_opt
                .map(|hal_texture| {
                    // Get the raw Metal texture from HAL texture using raw_handle()
                    hal_texture.raw_handle().to_owned()
                })
                .ok_or_else(|| {
                    StreamError::GpuError("Failed to get HAL texture from WebGPU texture".into())
                })
        })?;

        Ok(metal_texture)
    }

    /// Consume the bridge and return the WebGPU device and queue
    ///
    /// This is useful for passing ownership to streamlib-core's runtime.
    pub fn into_wgpu(self) -> (wgpu::Device, wgpu::Queue) {
        (self.wgpu_device, self.wgpu_queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::iosurface::{create_iosurface, PixelFormat, create_metal_texture_from_iosurface};

    #[test]
    fn test_wrap_metal_texture() {
        // Create Metal device
        use objc2_metal::MTLCreateSystemDefaultDevice;
        let metal_device = MTLCreateSystemDefaultDevice()
            .expect("No Metal device available");

        // Create WgpuBridge
        let bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone()).await
        }).expect("Failed to create WgpuBridge");

        // Create an IOSurface and Metal texture
        let iosurface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        let metal_texture = create_metal_texture_from_iosurface(&metal_device, &iosurface, 0)
            .expect("Failed to create Metal texture");

        // Wrap Metal texture as WebGPU texture
        let wgpu_texture = unsafe {
            bridge.wrap_metal_texture(
                &metal_texture,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            )
        }.expect("Failed to wrap Metal texture");

        // Verify texture properties
        assert_eq!(wgpu_texture.width(), 1920);
        assert_eq!(wgpu_texture.height(), 1080);
        assert_eq!(wgpu_texture.format(), wgpu::TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn test_unwrap_to_metal_texture() {
        // Create Metal device
        use objc2_metal::MTLCreateSystemDefaultDevice;
        let metal_device = MTLCreateSystemDefaultDevice()
            .expect("No Metal device available");

        // Create WgpuBridge
        let bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone()).await
        }).expect("Failed to create WgpuBridge");

        // Create an IOSurface and Metal texture
        let iosurface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        let metal_texture = create_metal_texture_from_iosurface(&metal_device, &iosurface, 0)
            .expect("Failed to create Metal texture");

        let original_width = metal_texture.width();
        let original_height = metal_texture.height();

        // Wrap Metal → WebGPU
        let wgpu_texture = unsafe {
            bridge.wrap_metal_texture(
                &metal_texture,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            )
        }.expect("Failed to wrap Metal texture");

        // Unwrap WebGPU → Metal (round-trip!)
        let unwrapped_metal = unsafe {
            bridge.unwrap_to_metal_texture(&wgpu_texture)
        }.expect("Failed to unwrap to Metal texture");

        // Verify the unwrapped texture has the same properties
        assert_eq!(unwrapped_metal.width(), original_width as u64);
        assert_eq!(unwrapped_metal.height(), original_height as u64);
        assert_eq!(unwrapped_metal.pixel_format(), metal::MTLPixelFormat::BGRA8Unorm);
    }

    #[test]
    fn test_round_trip_conversion() {
        // This test verifies that Metal → WebGPU → Metal preserves the texture
        use objc2_metal::MTLCreateSystemDefaultDevice;
        let metal_device = MTLCreateSystemDefaultDevice()
            .expect("No Metal device available");

        let bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone()).await
        }).expect("Failed to create WgpuBridge");

        // Create test texture via IOSurface
        let iosurface = create_iosurface(640, 480, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        let original_metal = create_metal_texture_from_iosurface(&metal_device, &iosurface, 0)
            .expect("Failed to create Metal texture");

        // Metal → WebGPU
        let wgpu_tex = unsafe {
            bridge.wrap_metal_texture(
                &original_metal,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            )
        }.expect("Failed to wrap");

        // WebGPU → Metal
        let final_metal = unsafe {
            bridge.unwrap_to_metal_texture(&wgpu_tex)
        }.expect("Failed to unwrap");

        // Verify dimensions match
        assert_eq!(final_metal.width(), 640);
        assert_eq!(final_metal.height(), 480);
        assert_eq!(original_metal.width() as u64, final_metal.width());
        assert_eq!(original_metal.height() as u64, final_metal.height());
    }
}
