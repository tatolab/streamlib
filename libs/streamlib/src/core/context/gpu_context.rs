// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::Videoframe;
use crate::core::context::TextureRegistration;
use crate::core::rhi::{
    CommandBuffer, GpuDevice, PixelBufferDescriptor, PixelBufferPoolId, PixelFormat, RhiBlitter,
    RhiCommandQueue, RhiPixelBuffer, RhiPixelBufferPool, StreamTexture, TextureDescriptor,
    TextureFormat, TextureUsages,
};
use crate::core::{Result, StreamError};
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::VulkanLayout;

/// Number of buffers to pre-allocate per pool.
const POOL_PRE_ALLOCATE_COUNT: usize = 4;

/// Maximum number of buffers per pool (expansion limit).
const POOL_MAX_BUFFER_COUNT: usize = 64;

/// Maximum number of entries in the buffer_cache before eviction.
const MAX_BUFFER_CACHE_SIZE: usize = 512;

/// No-op blitter for platforms without a native blitter.
#[cfg(not(target_os = "macos"))]
struct NoOpBlitter;

#[cfg(not(target_os = "macos"))]
impl RhiBlitter for NoOpBlitter {
    fn blit_copy(&self, _src: &RhiPixelBuffer, _dest: &RhiPixelBuffer) -> Result<()> {
        Err(StreamError::NotSupported(
            "Blitter not supported on this platform".into(),
        ))
    }

    unsafe fn blit_copy_iosurface_raw(
        &self,
        _src: *const std::ffi::c_void,
        _dest: &RhiPixelBuffer,
        _width: u32,
        _height: u32,
    ) -> Result<()> {
        Err(StreamError::NotSupported(
            "Blitter not supported on this platform".into(),
        ))
    }

    fn clear_cache(&self) {}
}

#[cfg(target_os = "linux")]
use super::compute_kernel_bridge::ComputeKernelBridge;
#[cfg(target_os = "linux")]
use super::cpu_readback_bridge::CpuReadbackBridge;
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
    /// Kept alive for ownership - buffers reference its backing storage.
    #[allow(dead_code)]
    pool: RhiPixelBufferPool,
    /// Permanently held buffers.
    buffers: Vec<PixelBufferRingEntry>,
    /// Next index in the ring to try.
    next_index: usize,
}

/// Shared pixel buffer pool manager.
///
/// Manages ring pools keyed by (width, height, format).
/// Pre-allocates buffers on pool creation and registers them with the surface-share service.
/// Buffers are held permanently for the runtime's lifetime.
struct PixelBufferPoolManager {
    pools: Mutex<HashMap<PixelBufferPoolKey, PixelBufferRingPool>>,
    /// Global cache for UUID -> RhiPixelBuffer lookups (includes buffers from all pools).
    /// Used by consumers (e.g., display processor) to resolve UUIDs received via IPC.
    buffer_cache: Mutex<HashMap<String, RhiPixelBuffer>>,
    /// GPU device reference for creating platform pixel buffer pools.
    #[allow(dead_code)]
    device: Arc<GpuDevice>,
}

impl PixelBufferPoolManager {
    fn new(device: Arc<GpuDevice>) -> Self {
        Self {
            pools: Mutex::new(HashMap::new()),
            buffer_cache: Mutex::new(HashMap::new()),
            device,
        }
    }

    /// Acquire a buffer from the pool.
    ///
    /// If this is a new pool, pre-allocates POOL_PRE_ALLOCATE_COUNT buffers
    /// and registers them with the surface-share service (if surface_store is available).
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
        if let std::collections::hash_map::Entry::Vacant(entry) = pools.entry(key) {
            tracing::info!(
                "PixelBufferPoolManager: creating new pool for {}x{} {:?}",
                width,
                height,
                format
            );
            let desc = PixelBufferDescriptor::new(width, height, format);
            let _ = desc;
            let underlying_pool = RhiPixelBufferPool {
                #[cfg(target_os = "macos")]
                inner: return Err(crate::core::StreamError::Configuration(
                    "PixelBufferPool creation via descriptor not yet implemented".into(),
                )),
                #[cfg(target_os = "linux")]
                inner: {
                    let vulkan_device = std::sync::Arc::clone(&self.device.inner);
                    let bytes_per_pixel = format.bits_per_pixel() / 8;
                    if bytes_per_pixel == 0 {
                        return Err(crate::core::StreamError::Configuration(
                            format!("Cannot create pixel buffer pool: PixelFormat {:?} has 0 bits per pixel", format),
                        ));
                    }
                    crate::vulkan::rhi::VulkanPixelBufferPool::new(
                        vulkan_device,
                        width,
                        height,
                        bytes_per_pixel,
                        format,
                        POOL_PRE_ALLOCATE_COUNT,
                    )?
                },
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                _marker: std::marker::PhantomData,
            };

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

                        // Register with the surface-share service if available
                        if let Some(store) = surface_store {
                            if let Err(e) = store.register_buffer(pool_id.as_str(), &buffer) {
                                tracing::warn!(
                                    "PixelBufferPoolManager: failed to register buffer {}: {}",
                                    pool_id,
                                    e
                                );
                            } else {
                                tracing::debug!(
                                    "PixelBufferPoolManager: registered buffer {} with the surface-share service",
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
                "PixelBufferPoolManager: pre-allocated {} buffers, registered {} with the surface-share service",
                buffers.len(),
                registered_count
            );

            let ring_pool = PixelBufferRingPool {
                pool: underlying_pool,
                buffers,
                next_index: 0,
            };
            entry.insert(ring_pool);
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

        // All buffers in use - try to expand the pool up to POOL_MAX_BUFFER_COUNT
        if buffer_count < POOL_MAX_BUFFER_COUNT {
            let expand_count = (POOL_MAX_BUFFER_COUNT - buffer_count).min(4);
            tracing::warn!(
                "PixelBufferPoolManager: all {} buffers in use for {}x{} {:?}, expanding by {}",
                buffer_count,
                width,
                height,
                format,
                expand_count
            );

            let surface_store_guard = self.buffer_cache.lock().unwrap();
            drop(surface_store_guard);

            let mut newly_added = 0;
            for _ in 0..expand_count {
                match ring_pool.pool.acquire() {
                    Ok((pool_id, buffer)) => {
                        // Register with the surface-share service if available
                        if let Some(store) = surface_store {
                            if let Err(e) = store.register_buffer(pool_id.as_str(), &buffer) {
                                tracing::warn!(
                                    "PixelBufferPoolManager: failed to register expanded buffer {}: {}",
                                    pool_id,
                                    e
                                );
                            }
                        }

                        // Add to global cache
                        self.buffer_cache
                            .lock()
                            .unwrap()
                            .insert(pool_id.as_str().to_string(), buffer.clone());

                        ring_pool
                            .buffers
                            .push(PixelBufferRingEntry { pool_id, buffer });
                        newly_added += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "PixelBufferPoolManager: failed to allocate expansion buffer: {}",
                            e
                        );
                        break;
                    }
                }
            }

            if newly_added > 0 {
                tracing::info!(
                    "PixelBufferPoolManager: expanded pool to {} buffers for {}x{} {:?}",
                    ring_pool.buffers.len(),
                    width,
                    height,
                    format
                );

                // Return the first newly added buffer (it's definitely not in use)
                let idx = ring_pool.buffers.len() - newly_added;
                let entry = &ring_pool.buffers[idx];
                return Ok((entry.pool_id.clone(), entry.buffer.clone()));
            }
        }

        tracing::error!(
            "PixelBufferPoolManager: all {} buffers in use for {}x{} {:?} (max {})",
            buffer_count,
            width,
            height,
            format,
            POOL_MAX_BUFFER_COUNT
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
        let mut cache = self.buffer_cache.lock().unwrap();
        cache.insert(pool_id.to_string(), buffer);
        if cache.len() > MAX_BUFFER_CACHE_SIZE {
            tracing::warn!(
                "PixelBufferPoolManager: buffer_cache exceeded {} entries ({}), clearing",
                MAX_BUFFER_CACHE_SIZE,
                cache.len()
            );
            cache.clear();
        }
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
    /// GPU blitter for efficient buffer-to-buffer copies with texture caching.
    blitter: Arc<dyn RhiBlitter>,
    /// Same-process texture cache — maps surface_id to a registration
    /// record carrying the texture plus per-surface lifecycle metadata
    /// (e.g. last-known Vulkan image layout). Mirrors the per-surface
    /// state pattern used by `streamlib-adapter-vulkan::SurfaceState`,
    /// lifted to engine-wide scope so consumers reaching textures via
    /// `resolve_videoframe_registration` get the same lifecycle metadata
    /// adapter consumers do.
    texture_cache: Arc<Mutex<HashMap<String, Arc<TextureRegistration>>>>,
    /// Cache of textures backing surface-share-registered pixel buffers
    /// (`escalate_acquire_pixel_buffer` flow). Refreshed on every resolve so
    /// rotating-pool producers don't render stale contents — kept separate
    /// from `texture_cache` so a same-process cache hit can't shortcut the
    /// refresh.
    buffer_texture_cache: Arc<Mutex<HashMap<String, StreamTexture>>>,
    /// Raw handle for the camera's timeline semaphore (same-process GPU-GPU sync).
    /// Stored as u64 for platform-agnostic GpuContext (0 = not set).
    camera_timeline_semaphore_handle: Arc<AtomicU64>,
    /// Serializes processor setup() across threads so concurrent GPU resource
    /// creation (video sessions, DPB images, swapchain) can't race on the
    /// device. The compiler acquires this during Phase 4 of spawn_processor
    /// and releases it after waiting for the device to go idle.
    processor_setup_lock: Arc<Mutex<()>>,
    /// Host-side bridge for the cpu-readback escalate op. Set by application
    /// code that wires a `CpuReadbackSurfaceAdapter` into the runtime; left
    /// unset on hosts that don't expose cpu-readback to subprocess customers
    /// (the escalate handler responds with an `Err` in that case).
    #[cfg(target_os = "linux")]
    cpu_readback_bridge: Arc<Mutex<Option<Arc<dyn CpuReadbackBridge>>>>,
    /// Host-side bridge for the compute-kernel escalate ops
    /// (`register_compute_kernel`, `run_compute_kernel`). Wired by
    /// application code that exposes the host's
    /// [`crate::vulkan::rhi::VulkanComputeKernel`] to subprocess customers;
    /// left unset on hosts that don't expose compute dispatch (the escalate
    /// handler responds with an `Err` in that case).
    #[cfg(target_os = "linux")]
    compute_kernel_bridge: Arc<Mutex<Option<Arc<dyn ComputeKernelBridge>>>>,
}

impl GpuContext {
    /// Create a new GPU context with an RHI device.
    pub fn new(device: GpuDevice) -> Self {
        let device = Arc::new(device);
        let texture_pool = TexturePool::new(Arc::clone(&device));
        let blitter = Self::create_blitter(&device);
        Self {
            pixel_buffer_pool_manager: Arc::new(PixelBufferPoolManager::new(Arc::clone(&device))),
            device,
            texture_pool,
            surface_store: Arc::new(Mutex::new(None)),
            blitter,
            texture_cache: Arc::new(Mutex::new(HashMap::new())),
            buffer_texture_cache: Arc::new(Mutex::new(HashMap::new())),
            camera_timeline_semaphore_handle: Arc::new(AtomicU64::new(0)),
            processor_setup_lock: Arc::new(Mutex::new(())),
            #[cfg(target_os = "linux")]
            cpu_readback_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            compute_kernel_bridge: Arc::new(Mutex::new(None)),
        }
    }

    /// Create with custom texture pool configuration.
    pub fn with_texture_pool_config(device: GpuDevice, pool_config: TexturePoolConfig) -> Self {
        let device = Arc::new(device);
        let texture_pool = TexturePool::with_config(Arc::clone(&device), pool_config);
        let blitter = Self::create_blitter(&device);
        Self {
            pixel_buffer_pool_manager: Arc::new(PixelBufferPoolManager::new(Arc::clone(&device))),
            device,
            texture_pool,
            surface_store: Arc::new(Mutex::new(None)),
            blitter,
            texture_cache: Arc::new(Mutex::new(HashMap::new())),
            buffer_texture_cache: Arc::new(Mutex::new(HashMap::new())),
            camera_timeline_semaphore_handle: Arc::new(AtomicU64::new(0)),
            processor_setup_lock: Arc::new(Mutex::new(())),
            #[cfg(target_os = "linux")]
            cpu_readback_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            compute_kernel_bridge: Arc::new(Mutex::new(None)),
        }
    }

    /// Acquire the processor-setup mutex. The compiler wraps each processor's
    /// `setup()` call with this lock and a subsequent wait-for-idle so
    /// concurrent setups can't race on GPU resource creation.
    pub fn lock_processor_setup(&self) -> std::sync::MutexGuard<'_, ()> {
        self.processor_setup_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Wait for the GPU device to become idle. On Vulkan backends this calls
    /// `vkDeviceWaitIdle`; on other backends this is a no-op.
    pub fn wait_device_idle(&self) -> Result<()> {
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
        {
            use vulkanalia::vk::DeviceV1_0;
            unsafe { self.device.inner.device().device_wait_idle() }.map_err(|e| {
                StreamError::GpuError(format!("device_wait_idle failed: {e}"))
            })?;
        }
        Ok(())
    }

    /// Create platform-specific blitter.
    #[cfg(target_os = "macos")]
    fn create_blitter(device: &Arc<GpuDevice>) -> Arc<dyn RhiBlitter> {
        let command_queue = device.command_queue().clone();
        Arc::new(crate::metal::rhi::MetalBlitter::new(command_queue))
    }

    #[cfg(target_os = "linux")]
    fn create_blitter(device: &Arc<GpuDevice>) -> Arc<dyn RhiBlitter> {
        let vulkan_device = &device.inner;
        match crate::vulkan::rhi::VulkanBlitter::new(
            vulkan_device,
            vulkan_device.queue(),
            vulkan_device.queue_family_index(),
        ) {
            Ok(blitter) => Arc::new(blitter),
            Err(e) => {
                tracing::warn!(
                    "Failed to create VulkanBlitter: {}, falling back to no-op",
                    e
                );
                Arc::new(NoOpBlitter)
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn create_blitter(_device: &Arc<GpuDevice>) -> Arc<dyn RhiBlitter> {
        Arc::new(NoOpBlitter)
    }

    /// Acquire a pixel buffer from the shared pool.
    ///
    /// Pools are cached by (width, height, format) - the first call creates the pool
    /// and pre-allocates buffers, subsequent calls reuse it. Returns (id, buffer) where
    /// id can be used with `get_pixel_buffer()` to retrieve the same buffer.
    ///
    /// If SurfaceStore is initialized, pre-allocated buffers are registered with the surface-share service.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PixelBufferPoolId, RhiPixelBuffer)> {
        tracing::debug!(
            rhi_op = "acquire_pixel_buffer",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_pixel_buffer"
        );
        let surface_store = self.surface_store.lock().unwrap();
        self.pixel_buffer_pool_manager
            .acquire(width, height, format, surface_store.as_ref())
    }

    /// Get a pixel buffer by its UUID.
    ///
    /// First checks local cache, then falls back to surface-share service lookup for cross-process sharing.
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

        // Cache miss - try surface-share service lookup
        tracing::debug!(
            "GpuContext::get_pixel_buffer: cache miss for '{}', trying surface-share service",
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

    /// Resolve a Videoframe's buffer from its surface_id.
    pub fn resolve_videoframe_buffer(&self, frame: &Videoframe) -> Result<RhiPixelBuffer> {
        let pool_id = PixelBufferPoolId::from_str(&frame.surface_id);
        self.get_pixel_buffer(&pool_id)
    }

    /// Register a texture in the same-process texture cache.
    ///
    /// On Linux the texture is registered with `VulkanLayout::UNDEFINED`
    /// as its initial layout — callers that know the texture's actual
    /// post-allocation layout (e.g. camera ring textures left in
    /// `SHADER_READ_ONLY_OPTIMAL` after compute) should use
    /// [`Self::register_texture_with_layout`] instead so consumers
    /// reaching the texture via [`Self::resolve_videoframe_registration`]
    /// can issue correct layout transitions.
    pub fn register_texture(&self, id: &str, texture: StreamTexture) {
        #[cfg(target_os = "linux")]
        let registration = TextureRegistration::new(texture, VulkanLayout::UNDEFINED);
        #[cfg(not(target_os = "linux"))]
        let registration = TextureRegistration::new(texture);
        let mut cache = self.texture_cache.lock().unwrap();
        cache.insert(id.to_string(), registration);
    }

    /// Register a texture with a declared initial Vulkan image layout.
    ///
    /// Producers call this when they know the layout the texture is in
    /// at the moment it becomes visible to consumers — e.g. camera
    /// processors that finish their compute pipeline with a transition
    /// to `SHADER_READ_ONLY_OPTIMAL` (so the next display frame's
    /// barrier source layout is correct), or adapter setup hooks that
    /// pre-allocate a render target the adapter writes to without
    /// transitioning the Vulkan layout (declare `UNDEFINED`).
    #[cfg(target_os = "linux")]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: StreamTexture,
        initial_layout: VulkanLayout,
    ) {
        let registration = TextureRegistration::new(texture, initial_layout);
        let mut cache = self.texture_cache.lock().unwrap();
        cache.insert(id.to_string(), registration);
    }

    /// Resolve a Videoframe's full registration record (texture + layout).
    ///
    /// Same lookup path as [`Self::resolve_videoframe_texture`] but
    /// returns the registration so consumers can read `current_layout`
    /// for barrier-source correctness.
    ///
    /// Path 2 (cross-process DMA-BUF VkImage import) reads the
    /// producer's last-published `VkImageLayout` from the surface-share
    /// IPC (#633). The consumer feeds this into the source layout of
    /// its first QFOT acquire barrier. Surfaces registered without a
    /// declared layout default to `UNDEFINED` (back-compat —
    /// content-discard permitted on the consumer's first transition).
    ///
    /// Path 3 (cross-process pixel buffer fallback) leaves the host-
    /// owned texture in `SHADER_READ_ONLY_OPTIMAL` after the upload
    /// pipeline runs (`upload_buffer_to_image` ends in that layout —
    /// see `vulkan_device.rs`); the registration declares it.
    pub fn resolve_videoframe_registration(
        &self,
        frame: &Videoframe,
    ) -> Result<Arc<TextureRegistration>> {
        // Path 1: same-process texture cache (fastest)
        {
            let cache = self.texture_cache.lock().unwrap();
            if let Some(reg) = cache.get(&frame.surface_id) {
                return Ok(Arc::clone(reg));
            }
        }

        // Path 2: cross-process DMA-BUF VkImage import via surface-share service.
        // Synthesized registration is not cached — Path 2 reimports per-call by
        // design, and caching would defeat that.
        //
        // QFOT acquire step (#633): the consumer-side VkImage was just
        // created with `initialLayout = UNDEFINED`. The producer's
        // post-release `VkImageLayout` is sourced from (priority order):
        //   1. `Videoframe.texture_layout` — per-frame override for
        //      producers that vary layout per frame.
        //   2. The surface-share IPC's per-surface `current_image_layout`
        //      — published at registration via `register_texture` and
        //      refreshed via `update_image_layout` after each producer
        //      release.
        // When the resolved layout is non-UNDEFINED, run a one-shot
        // QFOT acquire on the host queue. `acquire_from_foreign` uses
        // `VK_QUEUE_FAMILY_EXTERNAL` (core Vulkan 1.1, always
        // available) for the src family, and chains
        // `VkExternalMemoryAcquireUnmodifiedEXT` so producer-side
        // content survives the transfer when the optional
        // `VK_EXT_external_memory_acquire_unmodified` extension is
        // enabled. When that extension is missing (NVIDIA Linux
        // today and per the current driver roadmap), the helper falls
        // back to a bridging UNDEFINED → resolved_layout transition
        // (content-discard permitted by spec but preserved in
        // practice on every modern Linux Vulkan driver). Either way
        // the consumer-side tracker ends up at the resolved layout so
        // subsequent consumer barriers (`oldLayout = resolved →
        // target`) are validation-clean per
        // VUID-VkImageMemoryBarrier-oldLayout-01197.
        #[cfg(target_os = "linux")]
        {
            let surface_store = self.surface_store.lock().unwrap();
            if let Some(store) = surface_store.as_ref() {
                if let Ok((texture, ipc_layout)) = store.lookup_texture(&frame.surface_id) {
                    let resolved_layout = frame
                        .texture_layout
                        .map(VulkanLayout)
                        .unwrap_or(ipc_layout);
                    if resolved_layout != VulkanLayout::UNDEFINED {
                        if let Some(image) = texture.inner.image() {
                            self.device
                                .inner
                                .acquire_from_foreign(image, resolved_layout.as_vk())?;
                        }
                    }
                    return Ok(TextureRegistration::new(texture, resolved_layout));
                }
            }
        }

        // Path 3: cross-process pixel buffer fallback — refresh a private
        // host-owned texture from the latest buffer contents. The cache is
        // separate from `texture_cache` because rotating-pool producers reuse
        // surface_ids across cycles and a cache hit on stale contents would
        // silently render the previous frame.
        #[cfg(target_os = "linux")]
        {
            let buffer = {
                let surface_store = self.surface_store.lock().unwrap();
                surface_store
                    .as_ref()
                    .and_then(|store| store.lookup_buffer(&frame.surface_id).ok())
            };
            if let Some(buffer) = buffer {
                let texture = self.refresh_pixel_buffer_texture(
                    &frame.surface_id,
                    &buffer,
                    frame.width,
                    frame.height,
                )?;
                // upload_buffer_to_image leaves the texture in
                // SHADER_READ_ONLY_OPTIMAL (see vulkan_device.rs:1851).
                return Ok(TextureRegistration::new(
                    texture,
                    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                ));
            }
        }

        Err(StreamError::GpuError(format!(
            "No texture or pixel buffer found for surface_id '{}'",
            frame.surface_id
        )))
    }

    /// Resolve a Videoframe's texture — unified entry point for consumers
    /// that don't need layout metadata.
    ///
    /// Thin projection over [`Self::resolve_videoframe_registration`].
    /// Layout-aware consumers (display, future encoders) should call
    /// `resolve_videoframe_registration` directly so they can issue
    /// correct barriers.
    pub fn resolve_videoframe_texture(&self, frame: &Videoframe) -> Result<StreamTexture> {
        Ok(self.resolve_videoframe_registration(frame)?.texture().clone())
    }

    /// Acquire a new output texture with a UUID, register it in the cache.
    pub fn acquire_output_texture(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<(String, StreamTexture)> {
        let desc = TextureDescriptor::new(width, height, format)
            .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST);
        let texture = self.device.create_texture(&desc)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.register_texture(&id, texture.clone());
        Ok((id, texture))
    }

    /// Refresh a private texture from a host-visible pixel buffer for cross-
    /// process producers that registered a buffer (not a texture). The texture
    /// is created on first call and reused for subsequent calls under the same
    /// `surface_id`; contents are re-uploaded every time so rotating-pool
    /// producers see fresh frames.
    #[cfg(target_os = "linux")]
    fn refresh_pixel_buffer_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<StreamTexture> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

        // Get-or-create the cached texture for this surface_id.
        let texture = {
            let mut cache = self.buffer_texture_cache.lock().unwrap();
            if let Some(existing) = cache.get(surface_id) {
                if existing.width() == width && existing.height() == height {
                    existing.clone()
                } else {
                    cache.remove(surface_id);
                    let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
                        .with_usage(
                            TextureUsages::COPY_DST
                                | TextureUsages::TEXTURE_BINDING
                                | TextureUsages::STORAGE_BINDING,
                        );
                    let new_texture = self.device.create_texture_local(&desc)?;
                    cache.insert(surface_id.to_string(), new_texture.clone());
                    new_texture
                }
            } else {
                let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
                    .with_usage(
                        TextureUsages::COPY_DST
                            | TextureUsages::TEXTURE_BINDING
                            | TextureUsages::STORAGE_BINDING,
                    );
                let new_texture = self.device.create_texture_local(&desc)?;
                cache.insert(surface_id.to_string(), new_texture.clone());
                new_texture
            }
        };

        unsafe {
            let image = texture.inner.image().ok_or_else(|| {
                StreamError::GpuError("Texture has no VkImage".into())
            })?;
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                image,
                width,
                height,
            )?;
        }
        Ok(texture)
    }

    /// Upload a pixel buffer's contents to a GPU texture and register it in the texture cache.
    ///
    /// Copies the host-visible pixel buffer data to a device-local texture via
    /// vkCmdCopyBufferToImage, then registers the texture so display/encoder
    /// consumers can resolve it by surface_id.
    #[cfg(target_os = "linux")]
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

        let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING);
        // Same-process texture cache path — skip the DMA-BUF export pool so
        // repeated decode-output allocations don't exhaust NVIDIA's DMA-BUF
        // budget after the display swapchain is created
        // (docs/learnings/nvidia-dma-buf-after-swapchain.md).
        let texture = self.device.create_texture_local(&desc)?;

        unsafe {
            let image = texture.inner.image().ok_or_else(|| {
                crate::core::StreamError::GpuError("Texture has no VkImage".into())
            })?;
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                image,
                width,
                height,
            )?;
        }

        // upload_buffer_to_image leaves the image in SHADER_READ_ONLY_OPTIMAL
        // (see vulkan_device.rs:1851).
        self.register_texture_with_layout(
            surface_id,
            texture,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        Ok(())
    }

    /// Set the camera's timeline semaphore handle for same-process GPU-GPU sync.
    pub fn set_camera_timeline_semaphore(&self, raw_handle: u64) {
        self.camera_timeline_semaphore_handle
            .store(raw_handle, std::sync::atomic::Ordering::Release);
    }

    /// Get the camera's timeline semaphore handle (0 = not set).
    pub fn camera_timeline_semaphore(&self) -> u64 {
        self.camera_timeline_semaphore_handle
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Get a reference to the RHI GPU device.
    pub fn device(&self) -> &Arc<GpuDevice> {
        &self.device
    }

    /// Get the texture pool for acquiring pooled textures.
    pub fn texture_pool(&self) -> &TexturePool {
        &self.texture_pool
    }

    /// Acquire a pooled texture for in-process GPU work.
    ///
    /// Uses `VK_IMAGE_TILING_OPTIMAL` and is **not** safe to share with
    /// another process as a render target on NVIDIA Linux — the resulting
    /// DMA-BUF (if exported) is sampler-only there. For cross-process
    /// surfaces a consumer adapter will render INTO, use
    /// [`Self::acquire_render_target_dma_buf_image`] (Linux) instead.
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        tracing::debug!(
            rhi_op = "acquire_texture",
            width = desc.width,
            height = desc.height,
            format = ?desc.format,
            "GpuContext::acquire_texture"
        );
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

    /// Allocate a render-target-capable DMA-BUF VkImage backed by the device's
    /// tiled-modifier VMA pool.
    ///
    /// The driver picks one of the EGL-advertised render-target modifiers
    /// from [`HostVulkanDevice::drm_modifier_table`]. The resulting
    /// `StreamTexture` carries the chosen modifier on its inner
    /// [`HostVulkanTexture`] (see [`HostVulkanTexture::chosen_drm_format_modifier`]),
    /// ready to be carried in a `SurfaceTransportHandle` when the host
    /// surface-share service registers the surface.
    ///
    /// Errors when the EGL probe didn't find an RT-capable modifier for
    /// `format` — there is no silent fallback to LINEAR (sampler-only on
    /// NVIDIA — see `docs/learnings/nvidia-egl-dmabuf-render-target.md`).
    ///
    /// Picking the right acquire method:
    /// - **In-process texture for sampling/compute**: use
    ///   [`Self::acquire_texture`] (`VK_IMAGE_TILING_OPTIMAL`, no
    ///   DMA-BUF export pressure).
    /// - **CPU-readable buffer (mmap/PNG sample/MMAP fallback)**: use
    ///   [`Self::acquire_pixel_buffer`] (`VkBuffer`, linear).
    /// - **Cross-process surface a consumer adapter renders into**:
    ///   this method (tiled DRM modifier, DMA-BUF exportable, FBO-completable
    ///   on the consumer side).
    #[cfg(target_os = "linux")]
    pub fn acquire_render_target_dma_buf_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<StreamTexture> {
        use crate::vulkan::rhi::drm_modifier_probe::fourcc;

        tracing::debug!(
            rhi_op = "acquire_render_target_dma_buf_image",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_render_target_dma_buf_image"
        );

        let fourcc = match format {
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => {
                fourcc::DRM_FORMAT_ARGB8888
            }
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => {
                fourcc::DRM_FORMAT_ABGR8888
            }
            TextureFormat::Nv12 => fourcc::DRM_FORMAT_NV12,
            other => {
                return Err(StreamError::GpuError(format!(
                    "acquire_render_target_dma_buf_image: format {other:?} has no DRM FOURCC mapping"
                )));
            }
        };

        let vulkan_device = &self.device.inner;
        let modifiers: Vec<u64> = vulkan_device
            .drm_modifier_table()
            .rt_modifiers(fourcc)
            .to_vec();

        if modifiers.is_empty() {
            return Err(StreamError::GpuError(format!(
                "acquire_render_target_dma_buf_image: no RT-capable DRM modifier for {format:?} (fourcc=0x{fourcc:08x}); EGL probe returned empty list"
            )));
        }

        let desc = TextureDescriptor::new(width, height, format).with_usage(
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC
                // COPY_DST is required by Skia's `check_image_info`
                // gate (`GrVkGpu.cpp:1298-1302`): Skia mandates both
                // `VK_IMAGE_USAGE_TRANSFER_SRC_BIT` and
                // `VK_IMAGE_USAGE_TRANSFER_DST_BIT` on every
                // externally-allocated image it wraps as a Surface or
                // Image — without TRANSFER_DST, both
                // `wrap_backend_render_target` and `borrow_texture_from`
                // silently return `None`. The bit is also additive
                // for OpenGL / Vulkan compute / cpu-readback adapters,
                // so it lives at the canonical render-target
                // allocation point rather than per-adapter.
                | TextureUsages::COPY_DST
                // STORAGE_BINDING is on by default so subprocess Vulkan
                // adapters can bind the imported VkImage as a storage
                // image for compute writes (#531). Render-target +
                // sample-only adapters (OpenGL fragment shader, Skia)
                // still work — STORAGE is additive and tiled modifiers
                // for these formats reliably support it on every driver
                // streamlib runs on.
                | TextureUsages::STORAGE_BINDING,
        );
        let texture = crate::vulkan::rhi::HostVulkanTexture::new_render_target_dma_buf(
            vulkan_device,
            &desc,
            &modifiers,
        )?;
        Ok(StreamTexture::from_vulkan(texture))
    }

    /// Create a compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Reflects the SPIR-V at creation time and validates that the declared
    /// bindings match the shader; mismatches are reported with a clear error
    /// message rather than producing undefined GPU behavior at first dispatch.
    /// Returned kernel is held and dispatched via its own `set_*` / `dispatch`
    /// methods — one kernel handle per processor pipeline stage is the expected
    /// usage.
    #[cfg(target_os = "linux")]
    pub fn create_compute_kernel(
        &self,
        descriptor: &crate::core::rhi::ComputeKernelDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        tracing::debug!(
            rhi_op = "create_compute_kernel",
            label = descriptor.label,
            bindings = descriptor.bindings.len(),
            push_constant_size = descriptor.push_constant_size,
            "GpuContext::create_compute_kernel"
        );
        let vulkan_device = &self.device.inner;
        let kernel = crate::vulkan::rhi::VulkanComputeKernel::new(vulkan_device, descriptor)?;
        Ok(Arc::new(kernel))
    }

    /// Create a graphics kernel from a multi-stage SPIR-V set + binding
    /// declaration + pipeline state. Graphics counterpart to
    /// [`Self::create_compute_kernel`].
    ///
    /// Reflects every stage's SPIR-V at creation time and validates that
    /// the declared bindings + push constants + stage visibility match the
    /// shaders; mismatches surface as a clear error rather than at first
    /// draw.
    #[cfg(target_os = "linux")]
    pub fn create_graphics_kernel(
        &self,
        descriptor: &crate::core::rhi::GraphicsKernelDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::VulkanGraphicsKernel>> {
        tracing::debug!(
            rhi_op = "create_graphics_kernel",
            label = descriptor.label,
            stages = descriptor.stages.len(),
            bindings = descriptor.bindings.len(),
            push_constant_size = descriptor.push_constants.size,
            descriptor_sets_in_flight = descriptor.descriptor_sets_in_flight,
            "GpuContext::create_graphics_kernel"
        );
        let vulkan_device = &self.device.inner;
        let kernel = crate::vulkan::rhi::VulkanGraphicsKernel::new(vulkan_device, descriptor)?;
        Ok(Arc::new(kernel))
    }

    /// Create a host-side texture-readback handle bound to a fixed
    /// format/extent. The staging buffer + command resources + timeline
    /// semaphore are allocated once at construction and reused across
    /// every submit. Single-in-flight per handle (mirroring
    /// [`crate::vulkan::rhi::VulkanComputeKernel`]); for parallel
    /// readbacks, hold N handles.
    #[cfg(target_os = "linux")]
    pub fn create_texture_readback(
        &self,
        descriptor: &crate::core::rhi::TextureReadbackDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::VulkanTextureReadback>> {
        tracing::debug!(
            rhi_op = "create_texture_readback",
            label = descriptor.label,
            format = ?descriptor.format,
            width = descriptor.width,
            height = descriptor.height,
            bytes = descriptor.staging_size(),
            "GpuContext::create_texture_readback"
        );
        let vulkan_device = &self.device.inner;
        let handle = crate::vulkan::rhi::VulkanTextureReadback::new_into_stream_error(
            vulkan_device,
            descriptor,
        )?;
        Ok(Arc::new(handle))
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
    // GPU Blit Operations
    // =========================================================================

    /// Copy pixels between same-format, same-size buffers.
    ///
    /// Uses GPU blit with texture caching for efficient repeated copies.
    pub fn blit_copy(&self, src: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        self.blitter.blit_copy(src, dest)
    }

    /// Copy from raw IOSurface to a pixel buffer.
    ///
    /// # Safety
    /// - `src` must be a valid IOSurfaceRef pointer
    /// - The IOSurface must remain valid for the duration of the blit
    #[cfg(target_os = "macos")]
    pub unsafe fn blit_copy_iosurface(
        &self,
        src: crate::apple::corevideo_ffi::IOSurfaceRef,
        dest: &RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.blitter
            .blit_copy_iosurface_raw(src, dest, width, height)
    }

    /// Clear the blitter's texture cache to free GPU memory.
    pub fn clear_blitter_cache(&self) {
        self.blitter.clear_cache();
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

    // =========================================================================
    // CpuReadbackBridge — host-side dispatch for the cpu-readback escalate op
    // =========================================================================

    /// Register a [`CpuReadbackBridge`] implementation. The escalate handler
    /// dispatches `acquire_cpu_readback` requests through this bridge; until
    /// it is set, those requests fail with an "unsupported" error response.
    /// Linux-only: the cpu-readback adapter is Linux-only.
    #[cfg(target_os = "linux")]
    pub fn set_cpu_readback_bridge(&self, bridge: Arc<dyn CpuReadbackBridge>) {
        *self.cpu_readback_bridge.lock().unwrap() = Some(bridge);
    }

    /// Get the registered [`CpuReadbackBridge`], if any.
    #[cfg(target_os = "linux")]
    pub fn cpu_readback_bridge(&self) -> Option<Arc<dyn CpuReadbackBridge>> {
        self.cpu_readback_bridge.lock().unwrap().clone()
    }

    // =========================================================================
    // ComputeKernelBridge — host-side dispatch for the compute-kernel escalate ops
    // =========================================================================

    /// Register a [`ComputeKernelBridge`] implementation. The escalate handler
    /// dispatches `register_compute_kernel` and `run_compute_kernel` requests
    /// through this bridge; until it is set, those requests fail with an
    /// "unsupported" error response. Linux-only: compute escalate uses the
    /// Linux-side `VulkanComputeKernel`.
    #[cfg(target_os = "linux")]
    pub fn set_compute_kernel_bridge(&self, bridge: Arc<dyn ComputeKernelBridge>) {
        *self.compute_kernel_bridge.lock().unwrap() = Some(bridge);
    }

    /// Get the registered [`ComputeKernelBridge`], if any.
    #[cfg(target_os = "linux")]
    pub fn compute_kernel_bridge(&self) -> Option<Arc<dyn ComputeKernelBridge>> {
        self.compute_kernel_bridge.lock().unwrap().clone()
    }

    /// Check in a pixel buffer to the surface-share service, returning a surface ID.
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
    /// Returns from local cache if available, otherwise fetches from the surface-share service.
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

// =============================================================================
// Capability-typed wrappers — see docs/design/gpu-capability-sandbox.md
// =============================================================================
//
// `GpuContextLimitedAccess` is the capability handed to `process()` — at runtime
// it is meant to expose only cheap, pool-backed, non-allocating operations.
// `GpuContextFullAccess` is handed to `setup()` and inside
// `limited.escalate(|full| …)` closures — it exposes the full API, including
// GPU memory allocation.
//
// In this task (#321) both types are thin newtype wrappers around `GpuContext`
// and expose the **same** full API. This is a pure compile-time addition with
// no behavior change. The API surface split and the `escalate()` primitive
// land in #323/#324.

/// Restricted GPU capability handed to `process()`.
///
/// In the final design this type exposes only cheap, pool-backed, non-allocating
/// operations; heavier work must go through [`GpuContextLimitedAccess::escalate`].
/// In #321 it delegates every method to the inner [`GpuContext`] — no surface
/// is hidden yet.
#[derive(Clone)]
pub struct GpuContextLimitedAccess {
    inner: GpuContext,
}

/// Privileged GPU capability handed to `setup()` and inside
/// [`GpuContextLimitedAccess::escalate`] closures.
///
/// Exposes the full GPU API, including resource creation and device-wide
/// operations. In #321 this is the same surface as [`GpuContextLimitedAccess`];
/// the split lands in #324.
///
/// Deliberately **not** `Clone`. Processors only ever see a `&GpuContextFullAccess`
/// borrowed from a `RuntimeContextFullAccess` wrapper for the duration of a
/// single lifecycle call (setup / teardown / start / stop / escalate closure).
/// Removing `Clone` makes "stash a FullAccess in a field" a compile error:
/// nothing can produce an owned value outside the runtime's construction
/// path, so the capability can never escape its call.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib::core::GpuContextFullAccess>();
/// ```
pub struct GpuContextFullAccess {
    inner: GpuContext,
}

impl GpuContextLimitedAccess {
    /// Wrap a [`GpuContext`] as a limited-access capability.
    pub(crate) fn new(inner: GpuContext) -> Self {
        Self { inner }
    }

    /// Borrow the inner [`GpuContext`]. Crate-internal — call sites migrate
    /// to capability-typed methods as #322 lands.
    pub(crate) fn inner(&self) -> &GpuContext {
        &self.inner
    }

    /// Produce a [`GpuContextFullAccess`] view of the same underlying context.
    ///
    /// In #323 this becomes private and only reachable through
    /// `escalate(|full| …)`; today it is `pub(crate)` so the runtime and
    /// processor setup paths can still reach the full surface without a
    /// compile-time barrier.
    pub(crate) fn to_full_access(&self) -> GpuContextFullAccess {
        GpuContextFullAccess {
            inner: self.inner.clone(),
        }
    }

    /// Serialized escalation to full GPU capability. Acquires the
    /// processor-setup mutex, hands the closure a [`GpuContextFullAccess`]
    /// scoped to its body, then waits for the device to go idle before
    /// releasing the lock.
    ///
    /// This is the single primitive for GPU resource-creation work outside
    /// `setup()` — used by the compiler to run each processor's setup()
    /// and by running processors that need to reconfigure (acquire a new
    /// video session, resize a swapchain, etc.).
    ///
    /// The `device_wait_idle` fires exactly once per escalation (after the
    /// closure returns). On closure failure its error is returned; a
    /// follow-up `wait_device_idle` failure is returned only when the
    /// closure succeeded.
    pub fn escalate<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        let lock_start = std::time::Instant::now();
        let _setup_guard = self.inner.lock_processor_setup();
        let mutex_wait_ns = lock_start.elapsed().as_nanos() as u64;

        let closure_start = std::time::Instant::now();
        let full = GpuContextFullAccess::new(self.inner.clone());
        let closure_result = f(&full);
        drop(full);
        let closure_duration_ns = closure_start.elapsed().as_nanos() as u64;

        let wait_start = std::time::Instant::now();
        let wait_result = self.inner.wait_device_idle();
        let wait_idle_ns = wait_start.elapsed().as_nanos() as u64;

        tracing::trace!(
            target: "streamlib::gpu_context::escalate",
            mutex_wait_ns,
            closure_duration_ns,
            wait_idle_ns,
            closure_ok = closure_result.is_ok(),
            "GpuContextLimitedAccess::escalate completed"
        );

        check_sustained_escalation_rate();

        match (closure_result, wait_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(e),
        }
    }
}

// Thread-local escalation rate tracker. Each processor runs `process()` on a
// dedicated worker thread, so per-thread counters approximate per-processor
// escalation rates. Sustained rate above the threshold fires `tracing::warn!`.
std::thread_local! {
    static ESCALATION_TIMESTAMPS_NS: std::cell::RefCell<std::collections::VecDeque<u64>> =
        std::cell::RefCell::new(std::collections::VecDeque::with_capacity(16));
    static ESCALATION_LAST_WARN_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

const ESCALATION_RATE_WARN_THRESHOLD_PER_SEC: usize = 1;
const ESCALATION_RATE_WINDOW_NS: u64 = 1_000_000_000;
const ESCALATION_WARN_DEBOUNCE_NS: u64 = 5_000_000_000;

fn check_sustained_escalation_rate() {
    let now_ns = escalation_monotonic_ns();
    let cutoff = now_ns.saturating_sub(ESCALATION_RATE_WINDOW_NS);

    let (count, last_warn) = ESCALATION_TIMESTAMPS_NS.with(|buf| {
        let mut buf = buf.borrow_mut();
        while buf.front().is_some_and(|&ts| ts < cutoff) {
            buf.pop_front();
        }
        buf.push_back(now_ns);
        let count = buf.len();
        let last_warn = ESCALATION_LAST_WARN_NS.with(|c| c.get());
        (count, last_warn)
    });

    if count > ESCALATION_RATE_WARN_THRESHOLD_PER_SEC
        && now_ns.saturating_sub(last_warn) >= ESCALATION_WARN_DEBOUNCE_NS
    {
        ESCALATION_LAST_WARN_NS.with(|c| c.set(now_ns));
        let thread = std::thread::current();
        tracing::warn!(
            thread = thread.name().unwrap_or("<unnamed>"),
            escalations_last_second = count,
            "sustained GpuContextLimitedAccess::escalate rate on this thread — \
             processor likely needs more pre-reservation in setup()"
        );
    }
}

fn escalation_monotonic_ns() -> u64 {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(std::time::Instant::now);
    origin.elapsed().as_nanos() as u64
}

impl GpuContextFullAccess {
    /// Wrap a [`GpuContext`] as a full-access capability.
    pub(crate) fn new(inner: GpuContext) -> Self {
        Self { inner }
    }

    /// Borrow the inner [`GpuContext`]. Crate-internal — call sites migrate
    /// to capability-typed methods as #322 lands.
    pub(crate) fn inner(&self) -> &GpuContext {
        &self.inner
    }

    /// Produce a [`GpuContextLimitedAccess`] view of the same underlying context.
    pub(crate) fn to_limited_access(&self) -> GpuContextLimitedAccess {
        GpuContextLimitedAccess {
            inner: self.inner.clone(),
        }
    }
}

// -----------------------------------------------------------------------------
// Capability-split API surface (per the design doc in
// `docs/design/gpu-capability-sandbox.md` §1).
//
// `GpuContextLimitedAccess` exposes the Sandbox surface only: pool acquires
// (pre-reserved), texture sampling, writes to mapped pixel buffers, read-only
// queries, and the shared command queue. Methods that allocate new GPU
// memory, create sessions/swapchains/descriptors, or hand out raw device
// handles live exclusively on [`GpuContextFullAccess`] and are reachable from
// `process()` only via [`GpuContextLimitedAccess::escalate`].
// -----------------------------------------------------------------------------

impl GpuContextLimitedAccess {
    /// Acquire a pixel buffer from a pre-reserved pool (Split: fast path).
    ///
    /// The expected steady-state is a ring-slot hit. Callers should pre-reserve
    /// the pool in `setup()` by calling `acquire_pixel_buffer` on the
    /// [`GpuContextFullAccess`] with the target `(width, height, format)`.
    /// If the pool has to grow to serve this call, the growth path internally
    /// allocates — nonzero sustained rates will fire the escalation-rate
    /// warning, indicating a pre-reservation gap.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PixelBufferPoolId, RhiPixelBuffer)> {
        self.inner.acquire_pixel_buffer(width, height, format)
    }

    /// Get a pixel buffer by its pool id (Split: local cache).
    pub fn get_pixel_buffer(&self, pool_id: &PixelBufferPoolId) -> Result<RhiPixelBuffer> {
        self.inner.get_pixel_buffer(pool_id)
    }

    /// Resolve a [`Videoframe`]'s buffer from its surface_id.
    pub fn resolve_videoframe_buffer(&self, frame: &Videoframe) -> Result<RhiPixelBuffer> {
        self.inner.resolve_videoframe_buffer(frame)
    }

    /// Register a texture in the same-process texture cache.
    pub fn register_texture(&self, id: &str, texture: StreamTexture) {
        self.inner.register_texture(id, texture);
    }

    /// Register a texture with a declared initial Vulkan image layout.
    /// See [`GpuContext::register_texture_with_layout`].
    #[cfg(target_os = "linux")]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: StreamTexture,
        initial_layout: VulkanLayout,
    ) {
        self.inner
            .register_texture_with_layout(id, texture, initial_layout);
    }

    /// Resolve a [`Videoframe`]'s full registration record (texture + layout).
    pub fn resolve_videoframe_registration(
        &self,
        frame: &Videoframe,
    ) -> Result<Arc<TextureRegistration>> {
        self.inner.resolve_videoframe_registration(frame)
    }

    /// Resolve a [`Videoframe`]'s texture (Split: cache hit).
    pub fn resolve_videoframe_texture(&self, frame: &Videoframe) -> Result<StreamTexture> {
        self.inner.resolve_videoframe_texture(frame)
    }

    /// Set the camera's timeline semaphore handle for same-process GPU-GPU sync.
    pub fn set_camera_timeline_semaphore(&self, raw_handle: u64) {
        self.inner.set_camera_timeline_semaphore(raw_handle);
    }

    /// Get the camera's timeline semaphore handle (0 = not set).
    pub fn camera_timeline_semaphore(&self) -> u64 {
        self.inner.camera_timeline_semaphore()
    }

    /// Acquire a pooled texture from a pre-reserved pool (Split: fast path).
    ///
    /// `VK_IMAGE_TILING_OPTIMAL`, in-process use only. For cross-process
    /// render targets, see [`GpuContextFullAccess::acquire_render_target_dma_buf_image`]
    /// (Linux) — Sandbox callers don't have a render-target alloc path
    /// because allocating a new RT-capable image is a privileged op
    /// that goes through escalate.
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        self.inner.acquire_texture(desc)
    }

    /// Get the shared command queue.
    ///
    /// Submitting recorded command buffers from `process()` is safe: the
    /// images/buffers a Sandbox caller can construct are pool-backed and
    /// pre-reserved. See design doc §8 Q5.
    pub fn command_queue(&self) -> &RhiCommandQueue {
        self.inner.command_queue()
    }

    /// Create a CPU-side command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        self.inner.create_command_buffer()
    }

    /// Copy pixels between same-format, same-size buffers (Split: cache hit).
    pub fn blit_copy(&self, src: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        self.inner.blit_copy(src, dest)
    }

    /// Copy from raw IOSurface to a pixel buffer (Split: cache hit).
    ///
    /// # Safety
    /// - `src` must be a valid IOSurfaceRef pointer
    /// - The IOSurface must remain valid for the duration of the blit
    #[cfg(target_os = "macos")]
    pub unsafe fn blit_copy_iosurface(
        &self,
        src: crate::apple::corevideo_ffi::IOSurfaceRef,
        dest: &RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        unsafe { self.inner.blit_copy_iosurface(src, dest, width, height) }
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.inner.surface_store()
    }

    /// Check out a surface by ID (Split: cache hit).
    pub fn check_out_surface(&self, surface_id: &str) -> Result<RhiPixelBuffer> {
        self.inner.check_out_surface(surface_id)
    }
}

impl GpuContextFullAccess {
    /// Acquire the processor-setup mutex.
    pub fn lock_processor_setup(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner.lock_processor_setup()
    }

    /// Wait for the GPU device to become idle.
    pub fn wait_device_idle(&self) -> Result<()> {
        self.inner.wait_device_idle()
    }

    /// Acquire a pixel buffer from the shared pool.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PixelBufferPoolId, RhiPixelBuffer)> {
        self.inner.acquire_pixel_buffer(width, height, format)
    }

    /// Allocate a render-target-capable DMA-BUF VkImage (privileged path —
    /// host-only adapter primitive, customers never see this directly).
    /// See [`GpuContext::acquire_render_target_dma_buf_image`].
    #[cfg(target_os = "linux")]
    pub fn acquire_render_target_dma_buf_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<StreamTexture> {
        self.inner
            .acquire_render_target_dma_buf_image(width, height, format)
    }

    /// Get a pixel buffer by its pool id.
    pub fn get_pixel_buffer(&self, pool_id: &PixelBufferPoolId) -> Result<RhiPixelBuffer> {
        self.inner.get_pixel_buffer(pool_id)
    }

    /// Resolve a [`Videoframe`]'s buffer from its surface_id.
    pub fn resolve_videoframe_buffer(&self, frame: &Videoframe) -> Result<RhiPixelBuffer> {
        self.inner.resolve_videoframe_buffer(frame)
    }

    /// Register a texture in the same-process texture cache.
    pub fn register_texture(&self, id: &str, texture: StreamTexture) {
        self.inner.register_texture(id, texture);
    }

    /// Register a texture with a declared initial Vulkan image layout.
    /// See [`GpuContext::register_texture_with_layout`].
    #[cfg(target_os = "linux")]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: StreamTexture,
        initial_layout: VulkanLayout,
    ) {
        self.inner
            .register_texture_with_layout(id, texture, initial_layout);
    }

    /// Resolve a [`Videoframe`]'s full registration record (texture + layout).
    pub fn resolve_videoframe_registration(
        &self,
        frame: &Videoframe,
    ) -> Result<Arc<TextureRegistration>> {
        self.inner.resolve_videoframe_registration(frame)
    }

    /// Resolve a [`Videoframe`]'s texture.
    pub fn resolve_videoframe_texture(&self, frame: &Videoframe) -> Result<StreamTexture> {
        self.inner.resolve_videoframe_texture(frame)
    }

    /// Acquire a new output texture with a UUID and register it in the cache.
    pub fn acquire_output_texture(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<(String, StreamTexture)> {
        self.inner.acquire_output_texture(width, height, format)
    }

    /// Upload a pixel buffer's contents to a GPU texture and register it.
    #[cfg(target_os = "linux")]
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.inner
            .upload_pixel_buffer_as_texture(surface_id, pixel_buffer, width, height)
    }

    /// Set the camera's timeline semaphore handle for same-process GPU-GPU sync.
    pub fn set_camera_timeline_semaphore(&self, raw_handle: u64) {
        self.inner.set_camera_timeline_semaphore(raw_handle);
    }

    /// Get the camera's timeline semaphore handle (0 = not set).
    pub fn camera_timeline_semaphore(&self) -> u64 {
        self.inner.camera_timeline_semaphore()
    }

    /// Get a reference to the RHI GPU device.
    pub fn device(&self) -> &Arc<GpuDevice> {
        self.inner.device()
    }

    /// Get the texture pool for acquiring pooled textures.
    pub fn texture_pool(&self) -> &TexturePool {
        self.inner.texture_pool()
    }

    /// Acquire a pooled texture for in-process GPU work
    /// (`VK_IMAGE_TILING_OPTIMAL`). For cross-process render targets the
    /// host adapter layer wants on Linux, see
    /// [`Self::acquire_render_target_dma_buf_image`].
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        self.inner.acquire_texture(desc)
    }

    /// Get the shared command queue.
    pub fn command_queue(&self) -> &RhiCommandQueue {
        self.inner.command_queue()
    }

    /// Create a command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        self.inner.create_command_buffer()
    }

    /// Create a compute kernel from a SPIR-V shader and a binding declaration.
    #[cfg(target_os = "linux")]
    pub fn create_compute_kernel(
        &self,
        descriptor: &crate::core::rhi::ComputeKernelDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.inner.create_compute_kernel(descriptor)
    }

    /// Create a graphics kernel from a multi-stage SPIR-V set, binding
    /// declaration, and fixed-function pipeline state.
    #[cfg(target_os = "linux")]
    pub fn create_graphics_kernel(
        &self,
        descriptor: &crate::core::rhi::GraphicsKernelDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::VulkanGraphicsKernel>> {
        self.inner.create_graphics_kernel(descriptor)
    }

    /// Get the underlying Metal device (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_device(&self) -> &crate::metal::rhi::MetalDevice {
        self.inner.metal_device()
    }

    /// Create a texture cache for converting pixel buffers to texture views.
    #[cfg(target_os = "macos")]
    pub fn create_texture_cache(&self) -> Result<crate::core::rhi::RhiTextureCache> {
        self.inner.create_texture_cache()
    }

    /// Copy pixels between same-format, same-size buffers.
    pub fn blit_copy(&self, src: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        self.inner.blit_copy(src, dest)
    }

    /// Copy from raw IOSurface to a pixel buffer.
    ///
    /// # Safety
    /// - `src` must be a valid IOSurfaceRef pointer
    /// - The IOSurface must remain valid for the duration of the blit
    #[cfg(target_os = "macos")]
    pub unsafe fn blit_copy_iosurface(
        &self,
        src: crate::apple::corevideo_ffi::IOSurfaceRef,
        dest: &RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        unsafe { self.inner.blit_copy_iosurface(src, dest, width, height) }
    }

    /// Clear the blitter's texture cache to free GPU memory.
    pub fn clear_blitter_cache(&self) {
        self.inner.clear_blitter_cache();
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.inner.surface_store()
    }

    /// Check in a pixel buffer to the surface-share service.
    pub fn check_in_surface(&self, pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        self.inner.check_in_surface(pixel_buffer)
    }

    /// Check out a surface by ID.
    pub fn check_out_surface(&self, surface_id: &str) -> Result<RhiPixelBuffer> {
        self.inner.check_out_surface(surface_id)
    }

    /// Get the registered cpu-readback bridge, if any. Reachable only inside
    /// `escalate(|full| ...)` since it requires `FullAccess`.
    #[cfg(target_os = "linux")]
    pub fn cpu_readback_bridge(&self) -> Option<Arc<dyn CpuReadbackBridge>> {
        self.inner.cpu_readback_bridge()
    }

    /// Get the registered compute-kernel bridge, if any. Reachable only inside
    /// `escalate(|full| ...)` since it requires `FullAccess`.
    #[cfg(target_os = "linux")]
    pub fn compute_kernel_bridge(&self) -> Option<Arc<dyn ComputeKernelBridge>> {
        self.inner.compute_kernel_bridge()
    }
}

impl std::fmt::Debug for GpuContextLimitedAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContextLimitedAccess")
            .field("inner", &self.inner)
            .finish()
    }
}

impl std::fmt::Debug for GpuContextFullAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContextFullAccess")
            .field("inner", &self.inner)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_texture_cache_register_and_resolve() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        // Create a texture and register it
        let desc = TextureDescriptor::new(640, 480, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING);
        let texture = gpu.device().create_texture(&desc).expect("texture creation failed");
        let surface_id = "test-surface-001";

        gpu.register_texture(surface_id, texture.clone());

        // Resolve via Videoframe
        let frame = crate::_generated_::Videoframe {
            surface_id: surface_id.to_string(),
            width: 640,
            height: 480,
            timestamp_ns: "0".to_string(),
            frame_index: "1".to_string(),
            fps: None,
            texture_layout: None,
        };

        let resolved = gpu.resolve_videoframe_texture(&frame).expect("texture cache miss");
        assert_eq!(resolved.width(), 640);
        assert_eq!(resolved.height(), 480);

        println!("Texture cache: register + resolve OK");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_register_texture_with_layout_round_trip() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        let desc = TextureDescriptor::new(640, 480, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING);
        let texture = gpu
            .device()
            .create_texture(&desc)
            .expect("texture creation failed");
        let surface_id = "test-surface-with-layout";

        gpu.register_texture_with_layout(
            surface_id,
            texture,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        let frame = crate::_generated_::Videoframe {
            surface_id: surface_id.to_string(),
            width: 640,
            height: 480,
            timestamp_ns: "0".to_string(),
            frame_index: "1".to_string(),
            fps: None,
            texture_layout: None,
        };

        let registration = gpu
            .resolve_videoframe_registration(&frame)
            .expect("registration cache miss");
        assert_eq!(
            registration.current_layout(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            "declared initial layout should be visible to consumers"
        );

        // Update flow — consumer barriers transition + advance layout.
        registration.update_layout(VulkanLayout::TRANSFER_SRC_OPTIMAL);
        let registration2 = gpu
            .resolve_videoframe_registration(&frame)
            .expect("second resolve");
        assert_eq!(
            registration2.current_layout(),
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            "later resolves see the updated layout (Arc share)"
        );

        // Default register_texture path declares UNDEFINED.
        let texture2 = gpu
            .device()
            .create_texture(&desc)
            .expect("second texture creation failed");
        gpu.register_texture("test-surface-default-layout", texture2);
        let frame2 = crate::_generated_::Videoframe {
            surface_id: "test-surface-default-layout".to_string(),
            ..frame
        };
        let registration3 = gpu
            .resolve_videoframe_registration(&frame2)
            .expect("default-layout resolve");
        assert_eq!(
            registration3.current_layout(),
            VulkanLayout::UNDEFINED,
            "register_texture without explicit layout defaults to UNDEFINED"
        );

        println!("register_texture_with_layout + resolve_videoframe_registration: OK");
    }

    /// Issue #633 — `Videoframe.texture_layout` is an optional i32
    /// field that producers may set to override the per-surface
    /// `current_image_layout` from surface-share IPC. Lock the
    /// serialization shape: present when set, absent when None
    /// (skip-serializing-if=Option::is_none keeps backward compat with
    /// older consumers). Mentally revert the
    /// `skip_serializing_if = "Option::is_none"` in the generated
    /// `com_tatolab_videoframe.rs` and the `field_absent_when_none`
    /// case starts emitting `"texture_layout":null`.
    #[test]
    fn videoframe_texture_layout_serialization_round_trip() {
        let with_layout = crate::_generated_::Videoframe {
            surface_id: "s".to_string(),
            width: 8,
            height: 8,
            timestamp_ns: "0".to_string(),
            frame_index: "1".to_string(),
            fps: None,
            // SHADER_READ_ONLY_OPTIMAL = 5 per Vulkan spec.
            texture_layout: Some(5),
        };
        let json = serde_json::to_value(&with_layout).expect("serialize");
        assert_eq!(
            json.get("texture_layout").and_then(|v| v.as_i64()),
            Some(5),
            "set texture_layout must round-trip"
        );

        let absent = crate::_generated_::Videoframe {
            surface_id: "s".to_string(),
            width: 8,
            height: 8,
            timestamp_ns: "0".to_string(),
            frame_index: "1".to_string(),
            fps: None,
            texture_layout: None,
        };
        let json_absent = serde_json::to_value(&absent).expect("serialize");
        assert!(
            json_absent.get("texture_layout").is_none(),
            "None texture_layout must be absent from the wire (back-compat with pre-#633 consumers)"
        );

        // Round-trip through a JSON string: deserialize must recover
        // the field both directions (set → Some, absent → None).
        let serialized = serde_json::to_string(&with_layout).unwrap();
        let parsed: crate::_generated_::Videoframe = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.texture_layout, Some(5));
        let serialized_absent = serde_json::to_string(&absent).unwrap();
        let parsed_absent: crate::_generated_::Videoframe =
            serde_json::from_str(&serialized_absent).unwrap();
        assert_eq!(parsed_absent.texture_layout, None);
    }

    #[test]
    fn test_texture_cache_miss_and_timeline_semaphore() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        // Cache miss returns error (no texture registered, no surface-share service)
        let frame = crate::_generated_::Videoframe {
            surface_id: "nonexistent-surface".to_string(),
            width: 640,
            height: 480,
            timestamp_ns: "0".to_string(),
            frame_index: "1".to_string(),
            fps: None,
            texture_layout: None,
        };
        assert!(gpu.resolve_videoframe_texture(&frame).is_err());

        // Timeline semaphore sharing via Clone
        assert_eq!(gpu.camera_timeline_semaphore(), 0);
        gpu.set_camera_timeline_semaphore(0xDEAD_BEEF);
        assert_eq!(gpu.camera_timeline_semaphore(), 0xDEAD_BEEF);

        let gpu2 = gpu.clone();
        assert_eq!(gpu2.camera_timeline_semaphore(), 0xDEAD_BEEF);
        gpu2.set_camera_timeline_semaphore(0xCAFE_BABE);
        assert_eq!(gpu.camera_timeline_semaphore(), 0xCAFE_BABE);

        println!("Texture cache miss + timeline semaphore sharing: OK");
    }

    #[test]
    fn test_capability_newtypes_delegate_and_convert() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        // Limited-access delegates to the same underlying context (shared semaphore state).
        let limited = GpuContextLimitedAccess::new(gpu.clone());
        limited.set_camera_timeline_semaphore(0xA11CE);
        assert_eq!(gpu.camera_timeline_semaphore(), 0xA11CE);
        assert_eq!(limited.camera_timeline_semaphore(), 0xA11CE);

        // Conversion limited -> full shares the same context.
        let full = limited.to_full_access();
        assert_eq!(full.camera_timeline_semaphore(), 0xA11CE);
        full.set_camera_timeline_semaphore(0xB0B);
        assert_eq!(limited.camera_timeline_semaphore(), 0xB0B);

        // Conversion full -> limited round-trips.
        let limited2 = full.to_limited_access();
        assert_eq!(limited2.camera_timeline_semaphore(), 0xB0B);

        // Delegated accessor reaches the same RHI device. `device()` is
        // FullAccess-only after #324; Sandbox reaches the same underlying
        // context through `to_full_access()` (crate-internal) or
        // `escalate()` for user code.
        let device_ptr_gpu = Arc::as_ptr(gpu.device());
        let device_ptr_full = Arc::as_ptr(full.device());
        assert_eq!(device_ptr_gpu, device_ptr_full);

        println!("GpuContextLimitedAccess + GpuContextFullAccess delegation: OK");
    }

    #[test]
    fn test_escalate_serializes_concurrent_callers() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;

        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        const THREADS: usize = 8;
        let in_closure = Arc::new(AtomicBool::new(false));
        let overlap_count = Arc::new(AtomicUsize::new(0));
        let completed_count = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let gpu = gpu.clone();
                let in_closure = Arc::clone(&in_closure);
                let overlap_count = Arc::clone(&overlap_count);
                let completed_count = Arc::clone(&completed_count);
                std::thread::spawn(move || {
                    let limited = GpuContextLimitedAccess::new(gpu);
                    limited
                        .escalate(|_full| {
                            if in_closure.swap(true, Ordering::SeqCst) {
                                overlap_count.fetch_add(1, Ordering::SeqCst);
                            }
                            std::thread::sleep(Duration::from_millis(10));
                            in_closure.store(false, Ordering::SeqCst);
                            completed_count.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        })
                        .expect("escalate closure should succeed");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }

        assert_eq!(
            overlap_count.load(Ordering::SeqCst),
            0,
            "escalate closures overlapped — setup mutex not held"
        );
        assert_eq!(completed_count.load(Ordering::SeqCst), THREADS);

        println!("escalate serializes concurrent callers: OK");
    }

    #[test]
    fn test_escalate_propagates_closure_error() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        let limited = GpuContextLimitedAccess::new(gpu);
        let result: Result<()> = limited.escalate(|_full| {
            Err(StreamError::Runtime("synthetic failure".to_string()))
        });
        match result {
            Err(StreamError::Runtime(msg)) if msg == "synthetic failure" => {}
            other => panic!("expected synthetic Runtime error, got {other:?}"),
        }

        // Mutex must be released after the error — a second escalation should proceed.
        let after: Result<u32> = limited.escalate(|_full| Ok(7));
        assert_eq!(after.expect("escalate after error"), 7);

        println!("escalate propagates closure error + releases lock: OK");
    }
}
