// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::rhi::{
    CommandBuffer, GpuDevice, PixelBufferDescriptor, PixelBufferPoolId, PixelFormat,
    RhiCommandQueue, RhiPixelBuffer, RhiPixelBufferPool,
};
use crate::core::{Result, StreamError};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Number of buffers to pre-allocate per pool.
const POOL_PRE_ALLOCATE_COUNT: usize = 8;

use super::surface_store::SurfaceStore;
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

/// A single entry in the ring pool.
struct PixelBufferRingEntry {
    pool_id: PixelBufferPoolId,
    buffer: RhiPixelBuffer,
}

/// Ring pool of permanently held pixel buffers for a given (width, height, format).
///
/// Buffers are pre-allocated at pool creation and held for the runtime's lifetime.
/// `acquire()` cycles through buffers, skipping any currently in use.
struct PixelBufferRingPool {
    /// The underlying CVPixelBufferPool (used only for initial allocation).
    pool: RhiPixelBufferPool,
    /// Permanently held buffers.
    buffers: Vec<PixelBufferRingEntry>,
    /// Next index in the ring to try.
    next_index: usize,
}

/// Shared pixel buffer pool manager.
///
/// Manages ring pools keyed by (width, height, format).
/// Pre-allocates buffers on pool creation and registers them with the broker.
/// Buffers are held permanently for the runtime's lifetime.
struct PixelBufferPoolManager {
    pools: Mutex<HashMap<PixelBufferPoolKey, PixelBufferRingPool>>,
    /// Global cache for UUID -> RhiPixelBuffer lookups (includes buffers from all pools).
    /// Used by consumers (e.g., display processor) to resolve UUIDs received via IPC.
    buffer_cache: Mutex<HashMap<String, RhiPixelBuffer>>,
}

impl PixelBufferPoolManager {
    fn new() -> Self {
        Self {
            pools: Mutex::new(HashMap::new()),
            buffer_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Acquire a buffer from the pool.
    ///
    /// If this is a new pool, pre-allocates POOL_PRE_ALLOCATE_COUNT buffers
    /// and registers them with the broker (if surface_store is available).
    /// Returns the next available buffer from the ring, skipping any in use.
    fn acquire(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
        surface_store: Option<&SurfaceStore>,
    ) -> Result<(PixelBufferPoolId, RhiPixelBuffer)> {
        let key = PixelBufferPoolKey {
            width,
            height,
            format,
        };
        let mut pools = self.pools.lock().unwrap();

        // Create new ring pool if needed
        if !pools.contains_key(&key) {
            tracing::info!(
                "PixelBufferPoolManager: creating new pool for {}x{} {:?}",
                width,
                height,
                format
            );
            let desc = PixelBufferDescriptor::new(width, height, format);
            let underlying_pool = RhiPixelBufferPool::new_with_descriptor(&desc)?;

            // Pre-allocate all buffers at once (hold them simultaneously)
            let mut buffers = Vec::with_capacity(POOL_PRE_ALLOCATE_COUNT);
            let mut registered_count = 0;

            tracing::info!(
                "PixelBufferPoolManager: pre-allocating {} buffers for {}x{} {:?}",
                POOL_PRE_ALLOCATE_COUNT,
                width,
                height,
                format
            );

            for i in 0..POOL_PRE_ALLOCATE_COUNT {
                match underlying_pool.acquire() {
                    Ok((pool_id, buffer)) => {
                        tracing::debug!(
                            "PixelBufferPoolManager: pre-allocated buffer {} with id={}",
                            i,
                            pool_id
                        );

                        // Register with broker if available
                        if let Some(store) = surface_store {
                            if let Err(e) = store.register_buffer(pool_id.as_str(), &buffer) {
                                tracing::warn!(
                                    "PixelBufferPoolManager: failed to register buffer {}: {}",
                                    pool_id,
                                    e
                                );
                            } else {
                                tracing::debug!(
                                    "PixelBufferPoolManager: registered buffer {} with broker",
                                    pool_id
                                );
                                registered_count += 1;
                            }
                        }

                        // Add to global cache for UUID lookups
                        self.buffer_cache
                            .lock()
                            .unwrap()
                            .insert(pool_id.as_str().to_string(), buffer.clone());

                        // Store permanently in ring pool
                        buffers.push(PixelBufferRingEntry { pool_id, buffer });
                    }
                    Err(e) => {
                        tracing::warn!(
                            "PixelBufferPoolManager: failed to pre-allocate buffer {}: {}",
                            i,
                            e
                        );
                        break;
                    }
                }
            }

            tracing::info!(
                "PixelBufferPoolManager: pre-allocated {} buffers, registered {} with broker",
                buffers.len(),
                registered_count
            );

            let ring_pool = PixelBufferRingPool {
                pool: underlying_pool,
                buffers,
                next_index: 0,
            };
            pools.insert(key, ring_pool);
        }

        // Get the ring pool and find next available buffer
        let ring_pool = pools.get_mut(&key).unwrap();
        let buffer_count = ring_pool.buffers.len();

        if buffer_count == 0 {
            return Err(StreamError::Configuration(
                "No buffers available in pool".into(),
            ));
        }

        // Ring buffer: try each buffer starting from next_index, skip if in use
        for _ in 0..buffer_count {
            let idx = ring_pool.next_index % buffer_count;
            ring_pool.next_index = (ring_pool.next_index + 1) % buffer_count;

            let entry = &ring_pool.buffers[idx];

            // Check if buffer is available (only our permanent references exist)
            // RhiPixelBuffer wraps Arc<RhiPixelBufferRef>, so strong_count > 2 means in use
            // (2 = one in ring pool buffers Vec + one in buffer_cache HashMap)
            if Arc::strong_count(&entry.buffer.ref_) <= 2 {
                tracing::trace!(
                    "PixelBufferPoolManager: acquired buffer {} (idx {})",
                    entry.pool_id,
                    idx
                );
                return Ok((entry.pool_id.clone(), entry.buffer.clone()));
            }
        }

        // All buffers in use - this shouldn't happen with 8 buffers in normal use
        tracing::warn!(
            "PixelBufferPoolManager: all {} buffers in use for {}x{} {:?}",
            buffer_count,
            width,
            height,
            format
        );
        Err(StreamError::Configuration(
            "All pixel buffers are currently in use".into(),
        ))
    }

    /// Get a buffer by its UUID from local cache.
    fn get_from_cache(&self, pool_id: &str) -> Option<RhiPixelBuffer> {
        self.buffer_cache.lock().unwrap().get(pool_id).cloned()
    }

    /// Add a buffer to the local cache.
    fn cache_buffer(&self, pool_id: &str, buffer: RhiPixelBuffer) {
        self.buffer_cache
            .lock()
            .unwrap()
            .insert(pool_id.to_string(), buffer);
    }
}

#[derive(Clone)]
pub struct GpuContext {
    device: Arc<GpuDevice>,
    texture_pool: TexturePool,
    pixel_buffer_pool_manager: Arc<PixelBufferPoolManager>,
    /// Surface store for cross-process GPU surface sharing (macOS only).
    /// Set during runtime.start(), None before that.
    surface_store: Arc<Mutex<Option<SurfaceStore>>>,
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
            surface_store: Arc::new(Mutex::new(None)),
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
            surface_store: Arc::new(Mutex::new(None)),
        }
    }

    /// Acquire a pixel buffer from the shared pool.
    ///
    /// Pools are cached by (width, height, format) - the first call creates the pool
    /// and pre-allocates buffers, subsequent calls reuse it. Returns (id, buffer) where
    /// id can be used with `get_pixel_buffer()` to retrieve the same buffer.
    ///
    /// If SurfaceStore is initialized, pre-allocated buffers are registered with the broker.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PixelBufferPoolId, RhiPixelBuffer)> {
        let surface_store = self.surface_store.lock().unwrap();
        self.pixel_buffer_pool_manager
            .acquire(width, height, format, surface_store.as_ref())
    }

    /// Get a pixel buffer by its UUID.
    ///
    /// First checks local cache, then falls back to broker lookup for cross-process sharing.
    /// Returns the buffer if found, or an error if not found anywhere.
    pub fn get_pixel_buffer(&self, pool_id: &PixelBufferPoolId) -> Result<RhiPixelBuffer> {
        // Check local cache first
        if let Some(buffer) = self
            .pixel_buffer_pool_manager
            .get_from_cache(pool_id.as_str())
        {
            tracing::trace!("GpuContext::get_pixel_buffer: cache hit for '{}'", pool_id);
            return Ok(buffer);
        }

        // Cache miss - try broker lookup
        tracing::debug!(
            "GpuContext::get_pixel_buffer: cache miss for '{}', trying broker",
            pool_id
        );

        let surface_store = self.surface_store.lock().unwrap();
        let store = surface_store.as_ref().ok_or_else(|| {
            StreamError::Configuration(
                "SurfaceStore not initialized. Call runtime.start() first.".into(),
            )
        })?;

        let buffer = store.lookup_buffer(pool_id.as_str())?;

        // Cache for future lookups
        self.pixel_buffer_pool_manager
            .cache_buffer(pool_id.as_str(), buffer.clone());

        Ok(buffer)
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

    /// Get the shared command queue.
    ///
    /// All processors should use this shared queue rather than creating their own.
    pub fn command_queue(&self) -> &RhiCommandQueue {
        self.device.command_queue()
    }

    /// Create a command buffer from the shared queue.
    ///
    /// Command buffers are single-use: create, record commands, commit.
    /// This is the recommended way to submit GPU work in processors.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        self.command_queue().create_command_buffer()
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
    pub fn metal_device(&self) -> &crate::metal::rhi::MetalDevice {
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

    // =========================================================================
    // Surface Store (Cross-Process GPU Surface Sharing)
    // =========================================================================

    /// Set the surface store for cross-process GPU surface sharing.
    ///
    /// Called internally during runtime.start() to enable check_in/check_out.
    pub(crate) fn set_surface_store(&self, store: SurfaceStore) {
        *self.surface_store.lock().unwrap() = Some(store);
    }

    /// Clear the surface store.
    ///
    /// Called internally during runtime.stop().
    pub(crate) fn clear_surface_store(&self) {
        *self.surface_store.lock().unwrap() = None;
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.surface_store.lock().unwrap().clone()
    }

    /// Check in a pixel buffer to the broker, returning a surface ID.
    ///
    /// The surface ID can be shared with other processes (e.g., Python subprocesses)
    /// which can then call `check_out_surface` to get the same IOSurface.
    ///
    /// If this pixel buffer was already checked in, returns the existing ID.
    #[cfg(target_os = "macos")]
    pub fn check_in_surface(&self, pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        let store = self.surface_store.lock().unwrap();
        let store = store.as_ref().ok_or_else(|| {
            crate::core::StreamError::Configuration(
                "SurfaceStore not initialized. Call runtime.start() first.".into(),
            )
        })?;
        store.check_in(pixel_buffer)
    }

    /// Check out a surface by ID, returning the pixel buffer.
    ///
    /// Returns from local cache if available, otherwise fetches from broker.
    /// The first checkout for a given ID incurs XPC overhead (~100-200µs),
    /// subsequent checkouts are cache hits (~10-50ns).
    #[cfg(target_os = "macos")]
    pub fn check_out_surface(&self, surface_id: &str) -> Result<RhiPixelBuffer> {
        let store = self.surface_store.lock().unwrap();
        let store = store.as_ref().ok_or_else(|| {
            crate::core::StreamError::Configuration(
                "SurfaceStore not initialized. Call runtime.start() first.".into(),
            )
        })?;
        store.check_out(surface_id)
    }

    /// Check in a pixel buffer (non-macOS stub).
    #[cfg(not(target_os = "macos"))]
    pub fn check_in_surface(&self, _pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        Err(crate::core::StreamError::NotSupported(
            "Surface store is only supported on macOS".into(),
        ))
    }

    /// Check out a surface (non-macOS stub).
    #[cfg(not(target_os = "macos"))]
    pub fn check_out_surface(&self, _surface_id: &str) -> Result<RhiPixelBuffer> {
        Err(crate::core::StreamError::NotSupported(
            "Surface store is only supported on macOS".into(),
        ))
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("device", &self.device)
            .finish()
    }
}
