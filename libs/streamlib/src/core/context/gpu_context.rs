// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Result, StreamError};
use std::sync::Arc;
use wgpu;

use super::texture_pool::{
    PooledTextureHandle, TexturePool, TexturePoolConfig, TexturePoolDescriptor,
};

#[derive(Clone)]
pub struct GpuContext {
    device: Arc<wgpu::Device>,

    queue: Arc<wgpu::Queue>,

    texture_pool: TexturePool,
}

impl GpuContext {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let texture_pool = TexturePool::new(Arc::clone(&device), Arc::clone(&queue));
        Self {
            device,
            queue,
            texture_pool,
        }
    }

    /// Create with custom texture pool configuration.
    pub fn with_texture_pool_config(
        device: wgpu::Device,
        queue: wgpu::Queue,
        pool_config: TexturePoolConfig,
    ) -> Self {
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let texture_pool =
            TexturePool::with_config(Arc::clone(&device), Arc::clone(&queue), pool_config);
        Self {
            device,
            queue,
            texture_pool,
        }
    }

    pub fn device(&self) -> &Arc<wgpu::Device> {
        &self.device
    }

    pub fn queue(&self) -> &Arc<wgpu::Queue> {
        &self.queue
    }

    pub fn device_and_queue(&self) -> (&Arc<wgpu::Device>, &Arc<wgpu::Queue>) {
        (&self.device, &self.queue)
    }

    /// Get the texture pool for acquiring pooled textures.
    pub fn texture_pool(&self) -> &TexturePool {
        &self.texture_pool
    }

    /// Acquire a texture from the pool.
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        self.texture_pool.acquire(desc)
    }

    pub async fn init_for_platform() -> Result<Self> {
        let backends = if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
            wgpu::Backends::METAL
        } else if cfg!(target_os = "linux") {
            wgpu::Backends::VULKAN
        } else if cfg!(target_os = "windows") {
            wgpu::Backends::DX12
        } else {
            return Err(StreamError::GpuError(
                "Unsupported platform for GPU initialization".into(),
            ));
        };

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| StreamError::GpuError(format!("Failed to find GPU adapter: {}", e)))?;

        tracing::info!(
            "GPU: Using adapter '{}' (backend: {:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("StreamLib GPU Context"),
                required_features: wgpu::Features::TIMESTAMP_QUERY
                    | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: Default::default(),
            })
            .await
            .map_err(|e| StreamError::GpuError(format!("Failed to create device: {}", e)))?;

        tracing::info!("GPU: Device and queue created successfully");
        tracing::info!("GPU: Device address: {:p}", &device);
        tracing::info!("GPU: Queue address: {:p}", &queue);

        Ok(Self::new(device, queue))
    }

    /// Synchronous GPU initialization using pollster to block on async operations.
    /// This should be called from the main thread before starting the runtime.
    pub fn init_for_platform_sync() -> Result<Self> {
        pollster::block_on(Self::init_for_platform())
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("device", &format!("{:p}", self.device.as_ref()))
            .field("queue", &format!("{:p}", self.queue.as_ref()))
            .finish()
    }
}
