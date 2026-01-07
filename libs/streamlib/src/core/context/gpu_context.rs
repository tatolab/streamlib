// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::rhi::{
    GpuDevice, PixelBufferDescriptor, PixelFormat, RhiPixelBuffer, RhiPixelBufferPool,
};
use crate::core::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::texture_pool::{
    PooledTextureHandle, TexturePool, TexturePoolConfig, TexturePoolDescriptor,
};

/// Key for caching pixel buffer pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PixelBufferPoolKey {
    width: u32,
    height: u32,
    format: PixelFormat,
}

/// Shared pixel buffer pool manager.
struct PixelBufferPoolManager {
    pools: Mutex<HashMap<PixelBufferPoolKey, RhiPixelBufferPool>>,
}

impl PixelBufferPoolManager {
    fn new() -> Self {
        Self {
            pools: Mutex::new(HashMap::new()),
        }
    }

    fn acquire(&self, width: u32, height: u32, format: PixelFormat) -> Result<RhiPixelBuffer> {
        let key = PixelBufferPoolKey {
            width,
            height,
            format,
        };
        let mut pools = self.pools.lock().unwrap();

        let pool = if let Some(existing) = pools.get(&key) {
            tracing::trace!(
                "PixelBufferPoolManager: reusing cached pool for {}x{} {:?}",
                width,
                height,
                format
            );
            existing
        } else {
            tracing::info!(
                "PixelBufferPoolManager: creating new pool for {}x{} {:?}",
                width,
                height,
                format
            );
            let desc = PixelBufferDescriptor::new(width, height, format);
            let new_pool = RhiPixelBufferPool::new_with_descriptor(&desc)?;
            pools.insert(key, new_pool);
            pools.get(&key).unwrap()
        };

        pool.acquire()
    }
}

#[derive(Clone)]
pub struct GpuContext {
    device: Arc<GpuDevice>,
    texture_pool: TexturePool,
    pixel_buffer_pool_manager: Arc<PixelBufferPoolManager>,
}

impl GpuContext {
    /// Create a new GPU context with an RHI device.
    pub fn new(device: GpuDevice) -> Self {
        let device = Arc::new(device);
        let texture_pool = TexturePool::new(Arc::clone(&device));
        Self {
            device,
            texture_pool,
            pixel_buffer_pool_manager: Arc::new(PixelBufferPoolManager::new()),
        }
    }

    /// Create with custom texture pool configuration.
    pub fn with_texture_pool_config(device: GpuDevice, pool_config: TexturePoolConfig) -> Self {
        let device = Arc::new(device);
        let texture_pool = TexturePool::with_config(Arc::clone(&device), pool_config);
        Self {
            device,
            texture_pool,
            pixel_buffer_pool_manager: Arc::new(PixelBufferPoolManager::new()),
        }
    }

    /// Acquire a pixel buffer from the shared pool.
    ///
    /// Pools are cached by (width, height, format) - the first call creates the pool,
    /// subsequent calls reuse it. This avoids repeated CVPixelBufferPoolCreate calls.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<RhiPixelBuffer> {
        self.pixel_buffer_pool_manager
            .acquire(width, height, format)
    }

    /// Get a reference to the RHI GPU device.
    pub fn device(&self) -> &Arc<GpuDevice> {
        &self.device
    }

    /// Get the texture pool for acquiring pooled textures.
    pub fn texture_pool(&self) -> &TexturePool {
        &self.texture_pool
    }

    /// Acquire a texture from the pool.
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        self.texture_pool.acquire(desc)
    }

    /// Initialize GPU context for the current platform.
    pub fn init_for_platform() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let device = GpuDevice::new()?;
            tracing::info!("GPU: Using Metal device");
            Ok(Self::new(device))
        }

        #[cfg(target_os = "linux")]
        {
            let device = GpuDevice::new()?;
            tracing::info!("GPU: Using Vulkan device");
            Ok(Self::new(device))
        }

        #[cfg(target_os = "windows")]
        {
            let device = GpuDevice::new()?;
            tracing::info!("GPU: Using DX12 device");
            Ok(Self::new(device))
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(StreamError::GpuError(
                "Unsupported platform for GPU initialization".into(),
            ))
        }
    }

    /// Synchronous alias for init_for_platform (no async needed with native RHI).
    pub fn init_for_platform_sync() -> Result<Self> {
        Self::init_for_platform()
    }

    /// Get the underlying Metal device (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_device(&self) -> &crate::apple::rhi::MetalDevice {
        self.device.as_metal_device()
    }

    /// Create a texture cache for converting pixel buffers to texture views.
    #[cfg(target_os = "macos")]
    pub fn create_texture_cache(&self) -> Result<crate::core::rhi::RhiTextureCache> {
        use metal::foreign_types::ForeignTypeRef;
        let device_ptr = self.metal_device().device() as *const _ as *mut std::ffi::c_void;
        let metal_device_ref = unsafe { metal::DeviceRef::from_ptr(device_ptr as *mut _) };
        crate::core::rhi::RhiTextureCache::new_metal(metal_device_ref)
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("device", &self.device)
            .finish()
    }
}
