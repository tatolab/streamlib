use crate::{Result, StreamError};
use metal;
use metal::foreign_types::ForeignTypeRef;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDevice, MTLTexture};
use wgpu;
use wgpu::hal;

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
        let metal_crate_texture =
            unsafe { metal::TextureRef::from_ptr(metal_texture_ptr as *mut _) }.to_owned();

        let hal_texture = hal::metal::Device::texture_from_raw(
            metal_crate_texture,
            format,
            metal::MTLTextureType::D2,
            1, // array_layers
            1, // mip_levels
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
                .map(|hal_texture| hal_texture.raw_handle().to_owned())
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
    // Note: WgpuBridge tests have been removed because they relied on an old `new()` API
    // that no longer exists. The current API uses `from_shared_device()` which requires
    // a wgpu::Device and wgpu::Queue that are complex to set up in unit tests.
    //
    // The WgpuBridge functionality is tested through:
    // 1. Integration tests that use real GPU contexts
    // 2. Example applications (camera-display) that exercise the GPU bridge
}
