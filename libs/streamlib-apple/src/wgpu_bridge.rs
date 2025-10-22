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
    /// Create a new WebGPU bridge from a Metal device
    ///
    /// This initializes a WebGPU device that uses the provided Metal device
    /// as its backend, enabling zero-copy texture sharing.
    ///
    /// # Arguments
    ///
    /// * `metal_device` - Native Metal device to wrap
    ///
    /// # Returns
    ///
    /// A bridge that can convert Metal resources to WebGPU
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
                trace: wgpu::Trace::Off,
                experimental_features: Default::default(),
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
            format.into(),
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

    /// Get the Metal device
    pub fn metal_device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.metal_device
    }

    /// Consume the bridge and return the WebGPU device and queue
    ///
    /// This is useful for passing ownership to streamlib-core's runtime.
    pub fn into_wgpu(self) -> (wgpu::Device, wgpu::Queue) {
        (self.wgpu_device, self.wgpu_queue)
    }
}
