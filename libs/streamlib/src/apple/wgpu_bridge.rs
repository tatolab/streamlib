
use crate::{Result, StreamError};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDevice, MTLTexture};
use wgpu;
use wgpu::hal;
use metal;
use metal::foreign_types::ForeignTypeRef;

pub struct WgpuBridge {
    metal_device: Retained<ProtocolObject<dyn MTLDevice>>,

    wgpu_device: wgpu::Device,

    wgpu_queue: wgpu::Queue,
}

impl WgpuBridge {
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

    pub unsafe fn wrap_metal_texture(
        &self,
        metal_texture: &ProtocolObject<dyn MTLTexture>,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Result<wgpu::Texture> {
        let width = metal_texture.width();
        let height = metal_texture.height();

        let metal_texture_ptr = metal_texture as *const _ as *mut std::ffi::c_void;
        let metal_crate_texture = unsafe {
            metal::TextureRef::from_ptr(metal_texture_ptr as *mut _)
        }.to_owned();

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

    pub fn wgpu_device(&self) -> &wgpu::Device {
        &self.wgpu_device
    }

    pub fn wgpu_queue(&self) -> &wgpu::Queue {
        &self.wgpu_queue
    }

    pub fn wgpu(&self) -> (&wgpu::Device, &wgpu::Queue) {
        (&self.wgpu_device, &self.wgpu_queue)
    }

    pub fn metal_device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.metal_device
    }

    pub unsafe fn unwrap_to_metal_texture(
        &self,
        wgpu_texture: &wgpu::Texture,
    ) -> Result<metal::Texture> {
        let metal_texture = wgpu_texture.as_hal::<hal::api::Metal, _, _>(|hal_texture_opt| {
            hal_texture_opt
                .map(|hal_texture| {
                    hal_texture.raw_handle().to_owned()
                })
                .ok_or_else(|| {
                    StreamError::GpuError("Failed to get HAL texture from WebGPU texture".into())
                })
        })?;

        Ok(metal_texture)
    }

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
        use objc2_metal::MTLCreateSystemDefaultDevice;
        let metal_device = MTLCreateSystemDefaultDevice()
            .expect("No Metal device available");

        let bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone()).await
        }).expect("Failed to create WgpuBridge");

        let iosurface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        let metal_texture = create_metal_texture_from_iosurface(&metal_device, &iosurface, 0)
            .expect("Failed to create Metal texture");

        let wgpu_texture = unsafe {
            bridge.wrap_metal_texture(
                &metal_texture,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            )
        }.expect("Failed to wrap Metal texture");

        assert_eq!(wgpu_texture.width(), 1920);
        assert_eq!(wgpu_texture.height(), 1080);
        assert_eq!(wgpu_texture.format(), wgpu::TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn test_unwrap_to_metal_texture() {
        use objc2_metal::MTLCreateSystemDefaultDevice;
        let metal_device = MTLCreateSystemDefaultDevice()
            .expect("No Metal device available");

        let bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone()).await
        }).expect("Failed to create WgpuBridge");

        let iosurface = create_iosurface(1920, 1080, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        let metal_texture = create_metal_texture_from_iosurface(&metal_device, &iosurface, 0)
            .expect("Failed to create Metal texture");

        let original_width = metal_texture.width();
        let original_height = metal_texture.height();

        let wgpu_texture = unsafe {
            bridge.wrap_metal_texture(
                &metal_texture,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            )
        }.expect("Failed to wrap Metal texture");

        let unwrapped_metal = unsafe {
            bridge.unwrap_to_metal_texture(&wgpu_texture)
        }.expect("Failed to unwrap to Metal texture");

        assert_eq!(unwrapped_metal.width(), original_width as u64);
        assert_eq!(unwrapped_metal.height(), original_height as u64);
        assert_eq!(unwrapped_metal.pixel_format(), metal::MTLPixelFormat::BGRA8Unorm);
    }

    #[test]
    fn test_round_trip_conversion() {
        use objc2_metal::MTLCreateSystemDefaultDevice;
        let metal_device = MTLCreateSystemDefaultDevice()
            .expect("No Metal device available");

        let bridge = pollster::block_on(async {
            WgpuBridge::new(metal_device.clone()).await
        }).expect("Failed to create WgpuBridge");

        let iosurface = create_iosurface(640, 480, PixelFormat::Bgra8Unorm)
            .expect("Failed to create IOSurface");

        let original_metal = create_metal_texture_from_iosurface(&metal_device, &iosurface, 0)
            .expect("Failed to create Metal texture");

        let wgpu_tex = unsafe {
            bridge.wrap_metal_texture(
                &original_metal,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            )
        }.expect("Failed to wrap");

        let final_metal = unsafe {
            bridge.unwrap_to_metal_texture(&wgpu_tex)
        }.expect("Failed to unwrap");

        assert_eq!(final_metal.width(), 640);
        assert_eq!(final_metal.height(), 480);
        assert_eq!(original_metal.width() as u64, final_metal.width());
        assert_eq!(original_metal.height() as u64, final_metal.height());
    }
}
