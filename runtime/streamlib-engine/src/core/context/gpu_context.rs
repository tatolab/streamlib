// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::context::TextureRegistration;
use crate::core::rhi::{
    CommandBuffer, GpuDevice, PixelBuffer, PixelBufferDescriptor, PixelBufferPoolId, PixelFormat,
    RhiBlitter, RhiColorConverter, RhiCommandQueue, RhiPixelBufferPool, Texture, TextureDescriptor,
    TextureFormat, TextureUsages,
};
use crate::core::{Error, Result};
#[cfg(target_os = "linux")]
use crate::host_rhi::HostTextureExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
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
    fn blit_copy(&self, _src: &PixelBuffer, _dest: &PixelBuffer) -> Result<()> {
        Err(Error::NotSupported(
            "Blitter not supported on this platform".into(),
        ))
    }

    unsafe fn blit_copy_iosurface_raw(
        &self,
        _src: *const std::ffi::c_void,
        _dest: &PixelBuffer,
        _width: u32,
        _height: u32,
    ) -> Result<()> {
        Err(Error::NotSupported(
            "Blitter not supported on this platform".into(),
        ))
    }

    fn clear_cache(&self) {}
}

#[cfg(target_os = "linux")]
use super::compute_kernel_bridge::ComputeKernelBridge;
#[cfg(target_os = "linux")]
use super::cpu_readback_bridge::CpuReadbackBridge;
#[cfg(target_os = "linux")]
use super::graphics_kernel_bridge::GraphicsKernelBridge;
#[cfg(target_os = "linux")]
use super::ray_tracing_kernel_bridge::RayTracingKernelBridge;
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
    buffer: PixelBuffer,
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
    /// Global cache for UUID -> PixelBuffer lookups (includes buffers from all pools).
    /// Used by consumers (e.g., display processor) to resolve UUIDs received via IPC.
    buffer_cache: Mutex<HashMap<String, PixelBuffer>>,
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
    ) -> Result<(PixelBufferPoolId, PixelBuffer)> {
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
                inner: return Err(crate::core::Error::Configuration(
                    "PixelBufferPool creation via descriptor not yet implemented".into(),
                )),
                #[cfg(target_os = "linux")]
                inner: {
                    let vulkan_device = std::sync::Arc::clone(&self.device.inner);
                    let bytes_per_pixel = format.bits_per_pixel() / 8;
                    if bytes_per_pixel == 0 {
                        return Err(crate::core::Error::Configuration(format!(
                            "Cannot create pixel buffer pool: PixelFormat {:?} has 0 bits per pixel",
                            format
                        )));
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
            return Err(Error::Configuration("No buffers available in pool".into()));
        }

        // Ring buffer: try each buffer starting from next_index, skip if in use
        for _ in 0..buffer_count {
            let idx = ring_pool.next_index % buffer_count;
            ring_pool.next_index = (ring_pool.next_index + 1) % buffer_count;

            let entry = &ring_pool.buffers[idx];

            // Check if buffer is available (only our permanent references exist)
            // PixelBuffer holds an opaque handle to a host-side
            // Arc<PixelBufferRef>; strong_count > 2 means in use
            // (2 = one in ring pool buffers Vec + one in buffer_cache HashMap).
            if entry.buffer.strong_count() <= 2 {
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
        Err(Error::Configuration(
            "All pixel buffers are currently in use".into(),
        ))
    }

    /// Get a buffer by its UUID from local cache.
    fn get_from_cache(&self, pool_id: &str) -> Option<PixelBuffer> {
        self.buffer_cache.lock().unwrap().get(pool_id).cloned()
    }

    /// Add a buffer to the local cache.
    fn cache_buffer(&self, pool_id: &str, buffer: PixelBuffer) {
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

/// Read-once GPU capability snapshot returned by
/// [`GpuContext::gpu_capabilities`] /
/// [`GpuContextFullAccess::gpu_capabilities`].
///
/// Plain owned data — the cdylib bridge populates this from the
/// [`streamlib_plugin_abi::GpuCapabilitiesRepr`] plugin ABI struct (decoding
/// the fixed-size device_name byte buffer into an owned `String`).
/// In-process callers get it directly from the host-side getters.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct GpuCapabilitiesSnapshot {
    /// UTF-8 device name (vendor + model).
    pub device_name: String,
    /// Whether the GPU exposes `VK_KHR_external_memory_fd` +
    /// `VK_EXT_external_memory_dma_buf`.
    pub supports_external_memory: bool,
    /// Whether cross-device DMA-BUF probe is supported (false on
    /// NVIDIA Linux per the engine capability guard).
    pub supports_cross_device_dma_buf_probe: bool,
    /// Whether the GPU exposes `VK_KHR_ray_tracing_pipeline`.
    pub supports_ray_tracing_pipeline: bool,
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
    /// `resolve_texture_registration_by_surface_id` get the same lifecycle metadata
    /// adapter consumers do.
    texture_cache: Arc<Mutex<HashMap<String, TextureRegistration>>>,
    /// Cache of textures backing surface-share-registered pixel buffers
    /// (`escalate_acquire_pixel_buffer` flow). Refreshed on every resolve so
    /// rotating-pool producers don't render stale contents — kept separate
    /// from `texture_cache` so a same-process cache hit can't shortcut the
    /// refresh.
    buffer_texture_cache: Arc<Mutex<HashMap<String, Texture>>>,
    /// Engine-wide cache of `(src, dst)`-keyed color converters. Per-frame
    /// `ResolvedColorInfo` lives in push constants, so a single cached
    /// converter handles every variation of source color description.
    /// Construction is rare; conversion is hot — RwLock with double-check
    /// on miss matches that read/write skew.
    #[cfg(target_os = "linux")]
    color_converter_cache: Arc<
        RwLock<HashMap<(PixelFormat, PixelFormat), Arc<crate::core::rhi::RhiColorConverterInner>>>,
    >,
    /// Engine-tier publication slot for an in-process producer's timeline
    /// semaphore. The producer (today: the camera; in principle any in-tree
    /// video source) publishes a typed handle here so an in-process consumer
    /// (today: display) can `vkQueueSubmit2`-wait on it for GPU-GPU sync.
    /// The slot is single-publisher by construction; concurrent publishers
    /// would clobber. Promote to a registry keyed by producer id if/when a
    /// second concurrent publisher is filed.
    #[cfg(target_os = "linux")]
    video_source_timeline_semaphore:
        Arc<Mutex<Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>>>,
    /// Serializes [`GpuContextLimitedAccess::escalate`] scopes across
    /// threads (and across the in-process and vtable dispatch paths —
    /// engine-internal callers using `host_inner` direct dispatch and
    /// cdylib plugin callers using vtable plugin ABI dispatch serialize
    /// against each other) so concurrent GPU resource creation (video
    /// sessions, DPB images, swapchain) can't race on the device. The
    /// compiler acquires this during Phase 4 of spawn_processor and
    /// releases it after waiting for the device to go idle. Replaces
    /// the older `std::sync::Mutex<()>`-based `processor_setup_lock`
    /// — a [`Mutex`] guard can't cross thread boundaries (the cdylib
    /// plugin ABI escalate_begin / escalate_end pair may run on different
    /// threads), so this gate uses a flag + Condvar that enter and
    /// exit can hit independently.
    escalate_gate: Arc<super::escalate_gate::EscalateGate>,
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
    /// Host-side bridge for the graphics-kernel escalate ops
    /// (`register_graphics_kernel`, `run_graphics_draw`). Wired by
    /// application code that exposes the host's
    /// [`crate::vulkan::rhi::VulkanGraphicsKernel`] to subprocess customers;
    /// left unset on hosts that don't expose graphics dispatch (the
    /// escalate handler responds with an `Err` in that case).
    #[cfg(target_os = "linux")]
    graphics_kernel_bridge: Arc<Mutex<Option<Arc<dyn GraphicsKernelBridge>>>>,
    /// Host-side bridge for the ray-tracing-kernel escalate ops
    /// (`register_acceleration_structure_blas`,
    /// `register_acceleration_structure_tlas`, `register_ray_tracing_kernel`,
    /// `run_ray_tracing_kernel`). Wired by application code that exposes the
    /// host's [`crate::vulkan::rhi::VulkanRayTracingKernel`] +
    /// [`crate::vulkan::rhi::VulkanAccelerationStructure`] to subprocess
    /// customers; left unset on hosts that don't expose RT dispatch (the
    /// escalate handler responds with an `Err` in that case, as does any
    /// device that lacks the `VK_KHR_ray_tracing_pipeline` extension chain).
    #[cfg(target_os = "linux")]
    ray_tracing_kernel_bridge: Arc<Mutex<Option<Arc<dyn RayTracingKernelBridge>>>>,
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
            #[cfg(target_os = "linux")]
            color_converter_cache: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(target_os = "linux")]
            video_source_timeline_semaphore: Arc::new(Mutex::new(None)),
            escalate_gate: Arc::new(super::escalate_gate::EscalateGate::new()),
            #[cfg(target_os = "linux")]
            cpu_readback_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            compute_kernel_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            graphics_kernel_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            ray_tracing_kernel_bridge: Arc::new(Mutex::new(None)),
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
            #[cfg(target_os = "linux")]
            color_converter_cache: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(target_os = "linux")]
            video_source_timeline_semaphore: Arc::new(Mutex::new(None)),
            escalate_gate: Arc::new(super::escalate_gate::EscalateGate::new()),
            #[cfg(target_os = "linux")]
            cpu_readback_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            compute_kernel_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            graphics_kernel_bridge: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "linux")]
            ray_tracing_kernel_bridge: Arc::new(Mutex::new(None)),
        }
    }

    /// Borrow this context's escalate gate. The gate serializes
    /// [`GpuContextLimitedAccess::escalate`] scopes across both
    /// dispatch paths — engine-internal in-process escalate (the
    /// host-mode caller; uses
    /// [`super::escalate_gate::EscalateGate::enter_scoped`] for RAII
    /// release) and cdylib plugin-ABI-dispatched escalate (the plugin
    /// caller; uses bare `enter` / `exit` through
    /// [`super::escalate_scope_registry::begin_escalate_scope`] /
    /// [`super::escalate_scope_registry::end_escalate_scope`] because
    /// the plugin ABI precludes RAII across it).
    pub(crate) fn escalate_gate(&self) -> &super::escalate_gate::EscalateGate {
        &self.escalate_gate
    }

    /// Wrap this `GpuContext` in a [`GpuContextLimitedAccess`] view.
    ///
    /// Intended for callers that already hold the raw `GpuContext` — setup
    /// hooks ([`crate::core::runtime::Runner::install_setup_hook`]),
    /// runtime orchestrators, and crate-external integration tests — and
    /// need to invoke the typestate API surface (most notably
    /// [`GpuContextLimitedAccess::escalate`] for serialized elevation to
    /// [`GpuContextFullAccess`]).
    ///
    /// This does NOT weaken the capability moat: processor code never
    /// holds a raw `GpuContext` (the field is `pub(crate)` on
    /// `RuntimeContext`), so processors still reach the typestate
    /// surface only through their `RuntimeContextLimitedAccess` /
    /// `RuntimeContextFullAccess` borrows. The Limited view returned
    /// here exposes a strict subset of `GpuContext`'s public API and is
    /// safe to clone (it does not grant Full).
    pub fn limited_access(&self) -> GpuContextLimitedAccess {
        GpuContextLimitedAccess::new(self.clone())
    }

    /// Wait for the GPU device to become idle. On Vulkan backends this calls
    /// `vkDeviceWaitIdle`; on other backends this is a no-op.
    pub fn wait_device_idle(&self) -> Result<()> {
        #[cfg(any(
            feature = "backend-vulkan",
            all(target_os = "linux", not(feature = "backend-metal"))
        ))]
        {
            // `vkDeviceWaitIdle` is externally synchronized over every
            // `VkQueue` the device has — go through
            // `HostVulkanDevice::wait_idle()` so the queue mutexes are
            // taken and we don't race with active submits on other
            // threads.
            self.device.inner.wait_idle()?;
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
    ) -> Result<(PixelBufferPoolId, PixelBuffer)> {
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
    pub fn get_pixel_buffer(&self, pool_id: &PixelBufferPoolId) -> Result<PixelBuffer> {
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
            Error::Configuration("SurfaceStore not initialized. Call runtime.start() first.".into())
        })?;

        let buffer = store.lookup_buffer(pool_id.as_str())?;

        // Cache for future lookups
        self.pixel_buffer_pool_manager
            .cache_buffer(pool_id.as_str(), buffer.clone());

        Ok(buffer)
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        let pool_id = PixelBufferPoolId::from_str(surface_id);
        self.get_pixel_buffer(&pool_id)
    }

    /// Register a texture in the same-process texture cache.
    ///
    /// On Linux the texture is registered with `VulkanLayout::UNDEFINED`
    /// as its initial layout — callers that know the texture's actual
    /// post-allocation layout (e.g. camera ring textures left in
    /// `SHADER_READ_ONLY_OPTIMAL` after compute) should use
    /// [`Self::register_texture_with_layout`] instead so consumers
    /// reaching the texture via [`Self::resolve_texture_registration_by_surface_id`]
    /// can issue correct layout transitions.
    pub fn register_texture(&self, id: &str, texture: Texture) {
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
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        let registration = TextureRegistration::new(texture, initial_layout);
        let mut cache = self.texture_cache.lock().unwrap();
        cache.insert(id.to_string(), registration);
    }

    /// Remove a `surface_id` from the same-process texture cache.
    ///
    /// Idempotent — missing entries are a no-op. Producers that
    /// pre-register textures with a known lifetime (e.g.
    /// [`TextureRing`](crate::core::context::TextureRing)) call this on
    /// teardown so the cache doesn't outlive the underlying texture.
    pub fn unregister_texture(&self, id: &str) {
        let mut cache = self.texture_cache.lock().unwrap();
        cache.remove(id);
    }

    /// Refresh the registration's `current_layout` for a given
    /// `surface_id`. No-op if the surface_id isn't in the cache.
    /// Used by producers after a layout transition (e.g.
    /// [`TextureRing`](crate::core::context::TextureRing)'s per-frame
    /// copy ends in `SHADER_READ_ONLY_OPTIMAL`).
    #[cfg(target_os = "linux")]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        if let Some(reg) = self.texture_cache.lock().unwrap().get(id) {
            reg.update_layout(layout);
        }
    }

    /// Resolve a VideoFrame's full registration record (texture + layout).
    ///
    /// Same lookup path as [`Self::resolve_texture_by_surface_id`] but
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
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] texture_layout: Option<i32>,
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] width: u32,
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] height: u32,
    ) -> Result<TextureRegistration> {
        // Path 1: same-process texture cache (fastest)
        {
            let cache = self.texture_cache.lock().unwrap();
            if let Some(reg) = cache.get(surface_id) {
                return Ok(reg.clone());
            }
        }

        // Path 2: cross-process DMA-BUF VkImage import via surface-share service.
        // Synthesized registration is not cached — Path 2 reimports per-call by
        // design, and caching would defeat that.
        //
        // QFOT acquire step (#633): the consumer-side VkImage was just
        // created with `initialLayout = UNDEFINED`. The producer's
        // post-release `VkImageLayout` is sourced from (priority order):
        //   1. `VideoFrame.texture_layout` — per-frame override for
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
                if let Ok((texture, ipc_layout)) = store.lookup_texture(surface_id) {
                    let resolved_layout = texture_layout.map(VulkanLayout).unwrap_or(ipc_layout);
                    if resolved_layout != VulkanLayout::UNDEFINED {
                        if let Some(image) = texture.vulkan_inner().image() {
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
                    .and_then(|store| store.lookup_buffer(surface_id).ok())
            };
            if let Some(buffer) = buffer {
                let texture =
                    self.refresh_pixel_buffer_texture(surface_id, &buffer, width, height)?;
                // upload_buffer_to_image leaves the texture in
                // SHADER_READ_ONLY_OPTIMAL (see vulkan_device.rs:1851).
                return Ok(TextureRegistration::new(
                    texture,
                    VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                ));
            }
        }

        Err(Error::GpuError(format!(
            "No texture or pixel buffer found for surface_id '{}'",
            surface_id
        )))
    }

    /// Resolve a VideoFrame's texture — unified entry point for consumers
    /// that don't need layout metadata.
    ///
    /// Thin projection over [`Self::resolve_texture_registration_by_surface_id`].
    /// Layout-aware consumers (display, future encoders) should call
    /// `resolve_texture_registration_by_surface_id` directly so they can issue
    /// correct barriers.
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
        Ok(self
            .resolve_texture_registration_by_surface_id(surface_id, texture_layout, width, height)?
            .texture()
            .clone())
    }

    /// Acquire a new output texture with a UUID, register it in the cache.
    pub fn acquire_output_texture(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<(String, Texture)> {
        let desc = TextureDescriptor::new(width, height, format).with_usage(
            TextureUsages::STORAGE_BINDING
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST,
        );
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
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
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
            let image = texture
                .vulkan_inner()
                .image()
                .ok_or_else(|| Error::GpuError("Texture has no VkImage".into()))?;
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
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

        let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm).with_usage(
            TextureUsages::COPY_DST
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::STORAGE_BINDING,
        );
        // Same-process texture cache path — skip the DMA-BUF export pool so
        // repeated decode-output allocations don't exhaust NVIDIA's DMA-BUF
        // budget after the display swapchain is created
        // (docs/learnings/nvidia-dma-buf-after-swapchain.md).
        let texture = self.device.create_texture_local(&desc)?;

        unsafe {
            let image = texture
                .vulkan_inner()
                .image()
                .ok_or_else(|| crate::core::Error::GpuError("Texture has no VkImage".into()))?;
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

    /// Copy a host-visible pixel buffer's contents into an *already-allocated*
    /// device-local texture.
    ///
    /// Counterpart to [`Self::upload_pixel_buffer_as_texture`]: that one
    /// allocates a fresh texture per call (privileged), this one writes
    /// to a texture the caller already owns (sampbox-safe — no
    /// allocation, no descriptor / pipeline construction, just a
    /// `vkCmdCopyBufferToImage` queue submit). The shared command queue
    /// serializes the submit; layout transitions run UNDEFINED →
    /// TRANSFER_DST → SHADER_READ_ONLY_OPTIMAL via the existing
    /// `upload_buffer_to_image` path (content discard on the
    /// UNDEFINED transition is intended — the caller is about to
    /// overwrite the slot's contents anyway).
    ///
    /// When `surface_id` resolves to an entry in the texture cache
    /// (e.g. a ring slot pre-registered via
    /// [`crate::core::context::GpuContextFullAccess::create_texture_ring`])
    /// the registration's `current_layout` is refreshed to
    /// `SHADER_READ_ONLY_OPTIMAL` to match the post-upload state.
    #[cfg(target_os = "linux")]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        unsafe {
            let image = texture
                .vulkan_inner()
                .image()
                .ok_or_else(|| Error::GpuError("Texture has no VkImage".into()))?;
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                image,
                width,
                height,
            )?;
        }
        // Refresh the registration's layout (no-op for unregistered surface_ids).
        if let Some(reg) = self.texture_cache.lock().unwrap().get(surface_id) {
            reg.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        }
        Ok(())
    }

    /// Publish a producer's timeline semaphore for in-process GPU-GPU sync.
    /// The slot is shared across `GpuContext` clones; clone the input Arc
    /// so the producer drop doesn't strand the consumer mid-wait.
    #[cfg(target_os = "linux")]
    pub fn set_video_source_timeline_semaphore(
        &self,
        timeline: &Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) {
        *self.video_source_timeline_semaphore.lock().unwrap() = Some(Arc::clone(timeline));
    }

    /// Drop the published producer timeline. Called by the producer on
    /// teardown so the consumer can observe the absence and skip the wait.
    #[cfg(target_os = "linux")]
    pub fn clear_video_source_timeline_semaphore(&self) {
        *self.video_source_timeline_semaphore.lock().unwrap() = None;
    }

    /// Snapshot the currently-published producer timeline, if any.
    #[cfg(target_os = "linux")]
    pub fn video_source_timeline_semaphore(
        &self,
    ) -> Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        self.video_source_timeline_semaphore.lock().unwrap().clone()
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
    /// `Texture` carries the chosen modifier on its inner
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
    ) -> Result<Texture> {
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
                return Err(Error::GpuError(format!(
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
            return Err(Error::GpuError(format!(
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
        Ok(Texture::from_vulkan(texture))
    }

    /// Acquire a HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    ///
    /// Thin wrapper over
    /// [`crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible`].
    /// Unlike [`Self::acquire_pixel_buffer`], the returned buffer is
    /// **caller-owned-lifecycle, not pool-managed** — SSBOs are typically
    /// per-stage ring slots whose count is known at processor setup, so
    /// pool churn is the wrong shape. Callers retain the
    /// [`crate::core::rhi::StorageBuffer`] in their processor state and
    /// drop it when teardown runs.
    ///
    /// The buffer carries `STORAGE_BUFFER | TRANSFER_SRC | TRANSFER_DST`
    /// usage and DMA-BUF export flags; compute kernels bind it via
    /// [`crate::vulkan::rhi::VulkanComputeKernel::set_storage_buffer`]
    /// (which accepts any
    /// [`crate::vulkan::rhi::VulkanStorageBufferBinding`], including
    /// [`crate::core::rhi::StorageBuffer`]). `byte_size` must fit in
    /// `u32` (4 GB cap); larger SSBOs are not a current consumer need.
    #[cfg(target_os = "linux")]
    pub fn acquire_storage_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        tracing::debug!(
            rhi_op = "acquire_storage_buffer",
            byte_size,
            "GpuContext::acquire_storage_buffer"
        );
        let vulkan_device = &self.device.inner;
        let buffer = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
            vulkan_device,
            byte_size,
        )?;
        Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
            Arc::new(buffer),
        ))
    }

    /// Acquire a HOST_VISIBLE uniform buffer (UBO).
    ///
    /// Returns a [`crate::core::rhi::UniformBuffer`] — the type system
    /// enforces that this buffer can only be bound to a kernel's
    /// `set_uniform_buffer` slot (not storage / vertex / index).
    #[cfg(target_os = "linux")]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        tracing::debug!(
            rhi_op = "acquire_uniform_buffer",
            byte_size,
            "GpuContext::acquire_uniform_buffer"
        );
        let vulkan_device = &self.device.inner;
        crate::core::rhi::UniformBuffer::new_host_visible(vulkan_device, byte_size)
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    ///
    /// Returns a [`crate::core::rhi::VertexBuffer`] — only bindable to
    /// `set_vertex_buffer` slots.
    #[cfg(target_os = "linux")]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        tracing::debug!(
            rhi_op = "acquire_vertex_buffer",
            byte_size,
            "GpuContext::acquire_vertex_buffer"
        );
        let vulkan_device = &self.device.inner;
        crate::core::rhi::VertexBuffer::new_host_visible(vulkan_device, byte_size)
    }

    /// Acquire a HOST_VISIBLE index buffer.
    ///
    /// Returns a [`crate::core::rhi::IndexBuffer`] — only bindable to
    /// `set_index_buffer` slots.
    #[cfg(target_os = "linux")]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        tracing::debug!(
            rhi_op = "acquire_index_buffer",
            byte_size,
            "GpuContext::acquire_index_buffer"
        );
        let vulkan_device = &self.device.inner;
        crate::core::rhi::IndexBuffer::new_host_visible(vulkan_device, byte_size)
    }

    /// Acquire a cached `(src, dst)`-keyed color converter.
    ///
    /// First call for a given pair builds the converter (lazy kernel
    /// SPIR-V load + reflection); subsequent calls return the cached
    /// handle. Per-frame `ResolvedColorInfo` lives in push constants,
    /// so one cached converter handles every variation of source color
    /// description without invalidating.
    #[cfg(target_os = "linux")]
    pub fn color_converter(&self, src: PixelFormat, dst: PixelFormat) -> Result<RhiColorConverter> {
        // Fast path: read lock; cache stores Arc<Inner> so we can build
        // a fresh PluginAbiObject via from_arc_into_raw per request.
        {
            let cache = self.color_converter_cache.read().unwrap();
            if let Some(c) = cache.get(&(src, dst)) {
                return Ok(RhiColorConverter::from_arc_into_raw(Arc::clone(c)));
            }
        }
        // Slow path: build under write lock with double-check.
        let mut cache = self.color_converter_cache.write().unwrap();
        if let Some(c) = cache.get(&(src, dst)) {
            return Ok(RhiColorConverter::from_arc_into_raw(Arc::clone(c)));
        }
        let vulkan_device = &self.device.inner;
        let inner = crate::vulkan::rhi::VulkanColorConverter::new(vulkan_device, src, dst)?;
        let inner_arc = Arc::new(crate::core::rhi::RhiColorConverterInner { inner });
        cache.insert((src, dst), Arc::clone(&inner_arc));
        tracing::debug!(
            rhi_op = "color_converter",
            ?src,
            ?dst,
            "GpuContext::color_converter — converter constructed"
        );
        Ok(RhiColorConverter::from_arc_into_raw(inner_arc))
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
    ) -> Result<crate::vulkan::rhi::VulkanComputeKernel> {
        tracing::debug!(
            rhi_op = "create_compute_kernel",
            label = descriptor.label,
            bindings = descriptor.bindings.len(),
            push_constant_size = descriptor.push_constant_size,
            "GpuContext::create_compute_kernel"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanComputeKernel::new(vulkan_device, descriptor)
    }

    /// Build an engine-owned command-buffer recorder bound to the
    /// device's default queue.
    ///
    /// Wraps the long-lived command pool + reset-able primary command
    /// buffer + per-frame barrier/copy/dispatch recording + queue-mutex-
    /// guarded submit-with-timeline-signal shape that processors
    /// reinvented inline pre-#751. See
    /// [`RhiCommandRecorder`](crate::vulkan::rhi::RhiCommandRecorder)
    /// for the per-frame usage protocol.
    #[cfg(target_os = "linux")]
    pub fn create_command_recorder(
        &self,
        label: &str,
    ) -> Result<crate::vulkan::rhi::RhiCommandRecorder> {
        tracing::debug!(
            rhi_op = "create_command_recorder",
            label,
            "GpuContext::create_command_recorder"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::RhiCommandRecorder::new(vulkan_device, label)
    }

    /// Build a swapchain-backed [`PresentTarget`](crate::vulkan::rhi::PresentTarget)
    /// from a native `window` handle, at the requested initial extent +
    /// vsync preference. `color_traits` drives the `VkColorSpaceKHR`
    /// priority walk; `None` keeps the legacy SDR pick. The window handle
    /// must outlive the returned target (the host owns the `VkSurfaceKHR`
    /// from creation, never the window). Minting entry point behind the
    /// FullAccess `create_present_target` plugin-ABI slot — engine-free
    /// display processors reach it through the SDK `create_present_target`
    /// wrapper, never `VulkanPresentTarget::new` on a raw device.
    #[cfg(target_os = "linux")]
    pub fn create_present_target(
        &self,
        window: &(impl raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle),
        width: u32,
        height: u32,
        vsync: bool,
        color_traits: Option<&crate::core::color::ColorTraits>,
    ) -> Result<crate::vulkan::rhi::PresentTarget> {
        tracing::debug!(
            rhi_op = "create_present_target",
            width,
            height,
            vsync,
            "GpuContext::create_present_target"
        );
        let vulkan_device = &self.device.inner;
        let target = crate::vulkan::rhi::VulkanPresentTarget::new(
            vulkan_device,
            window,
            width,
            height,
            vsync,
            color_traits,
        )?;
        Ok(crate::vulkan::rhi::PresentTarget::from_target(target))
    }

    /// Create a Vulkan video session — the privileged
    /// `VkVideoSessionKHR` + bound device memory the codec layer
    /// uses for `vkCmdDecodeVideoKHR` / `vkCmdEncodeVideoKHR`.
    ///
    /// FullAccess-only: the session creation path goes through
    /// `vkCreateVideoSessionKHR` + `vkBindVideoSessionMemoryKHR`,
    /// both excluded from the consumer-rhi carve-out. Subprocess
    /// consumers that need codec output reach it through the normal
    /// `surface_id` contract — they import the codec's render
    /// target, not the session itself.
    #[cfg(target_os = "linux")]
    pub fn create_video_session(
        &self,
        descriptor: &crate::vulkan::rhi::VideoSessionDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::HostVulkanVideoSession>> {
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::HostVulkanVideoSession::new(vulkan_device, descriptor)
    }

    /// Create a Vulkan video session parameters object parented to
    /// `session`. Companion to [`Self::create_video_session`]; covers
    /// `vkCreateVideoSessionParametersKHR`'s codec-specific add-info
    /// chain (H.264 / H.265 SPS / PPS / VPS plus encoder quality-level).
    #[cfg(target_os = "linux")]
    pub fn create_video_session_parameters(
        &self,
        session: &Arc<crate::vulkan::rhi::HostVulkanVideoSession>,
        descriptor: &crate::vulkan::rhi::VideoSessionParametersDescriptor<'_>,
    ) -> Result<Arc<crate::vulkan::rhi::HostVulkanVideoSessionParameters>> {
        crate::vulkan::rhi::HostVulkanVideoSessionParameters::new(session, descriptor)
    }

    /// Allocate a video DPB (Decoded Picture Buffer) image bound to a
    /// codec profile. Backs
    /// [`GpuContextFullAccess::create_video_dpb_texture`]; the
    /// FullAccess wrapper enforces the privileged-scope invariants
    /// and dispatches here for the Boxed mode (subprocess
    /// `ScopeToken` mode errors out — codec packages live host-side).
    #[cfg(target_os = "linux")]
    pub fn create_video_dpb_texture(
        &self,
        descriptor: &crate::vulkan::rhi::VideoDpbTextureDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanTexture> {
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::HostVulkanTexture::new_video_dpb(vulkan_device, descriptor)
    }

    /// Allocate a video bitstream buffer bound to a codec profile.
    /// Backs [`GpuContextFullAccess::create_video_bitstream_buffer`].
    #[cfg(target_os = "linux")]
    pub fn create_video_bitstream_buffer(
        &self,
        descriptor: &crate::vulkan::rhi::VideoBitstreamBufferDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanBuffer> {
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::HostVulkanBuffer::new_video_bitstream(vulkan_device, descriptor)
    }

    /// Allocate a Vulkan query pool. Backs
    /// [`GpuContextFullAccess::create_query_pool`]. Generic over
    /// `VkQueryType` — services timestamp, occlusion, pipeline-statistics,
    /// and video-encode-feedback queries through one primitive.
    #[cfg(target_os = "linux")]
    pub fn create_query_pool(
        &self,
        descriptor: &crate::vulkan::rhi::QueryPoolDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanQueryPool> {
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::HostVulkanQueryPool::new(vulkan_device, descriptor)
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
    ) -> Result<crate::vulkan::rhi::VulkanGraphicsKernel> {
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
        crate::vulkan::rhi::VulkanGraphicsKernel::new(vulkan_device, descriptor)
    }

    /// Create a ray-tracing kernel from shader stages, shader-group
    /// layout, binding declaration, and push-constant range. Mirror of
    /// [`Self::create_compute_kernel`] / [`Self::create_graphics_kernel`]
    /// for `VkRayTracingPipelineKHR`-backed work.
    ///
    /// Validates every stage's SPIR-V against the declared bindings +
    /// push-constants at creation time, builds the pipeline, fetches
    /// shader-group handles, lays out the shader-binding table, and
    /// returns a kernel ready for `set_*` + `trace_rays` dispatch.
    /// Returns a clean error when the device lacks RT support.
    #[cfg(target_os = "linux")]
    pub fn create_ray_tracing_kernel(
        &self,
        descriptor: &crate::core::rhi::RayTracingKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanRayTracingKernel> {
        tracing::debug!(
            rhi_op = "create_ray_tracing_kernel",
            label = descriptor.label,
            stages = descriptor.stages.len(),
            groups = descriptor.groups.len(),
            bindings = descriptor.bindings.len(),
            push_constant_size = descriptor.push_constants.size,
            max_recursion_depth = descriptor.max_recursion_depth,
            "GpuContext::create_ray_tracing_kernel"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanRayTracingKernel::new(vulkan_device, descriptor)
    }

    /// Pre-allocate a ring of `count` non-exportable DEVICE_LOCAL
    /// textures and register each in the same-process texture cache.
    /// Mirror of [`GpuContextFullAccess::create_texture_ring`] at the
    /// inner-`GpuContext` level — the FullAccess wrapper delegates here
    /// after enforcing the privileged-scope invariants.
    #[cfg(target_os = "linux")]
    pub fn create_texture_ring(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
        usages: TextureUsages,
        count: usize,
    ) -> Result<crate::core::context::TextureRing> {
        use crate::core::context::{TextureRing, TextureRingInner, TextureRingSlot};

        if count == 0 {
            return Err(Error::GpuError(
                "create_texture_ring: count must be > 0".into(),
            ));
        }

        let mut slots = Vec::with_capacity(count);
        let mut upload_resources = Vec::with_capacity(count);
        for slot_index in 0..count {
            let desc = TextureDescriptor::new(width, height, format).with_usage(usages);
            let texture = self.device.create_texture_local(&desc)?;
            let surface_id = uuid::Uuid::new_v4().to_string();
            // Spec-correct initial layout for a freshly-allocated
            // VkImage that no one has touched yet (per
            // docs/architecture/texture-registration.md Producer
            // Rule 2). The per-frame
            // `TextureRing::copy_pixel_buffer_to_slot` runs the
            // amortized upload that transitions UNDEFINED →
            // SHADER_READ_ONLY_OPTIMAL and updates the registration
            // to match, so after the first per-frame copy the claim
            // and reality agree.
            self.register_texture_with_layout(
                &surface_id,
                texture.clone(),
                VulkanLayout::UNDEFINED,
            );
            slots.push(TextureRingSlot::new(
                texture,
                &surface_id,
                slot_index as u32,
            ));
            let res = crate::vulkan::rhi::HostVulkanUploadResources::new(&self.device.inner)?;
            upload_resources.push(res);
        }
        let inner_arc = TextureRingInner::from_slots(
            slots,
            upload_resources,
            width,
            height,
            format,
            self.clone(),
        );
        Ok(TextureRing::from_arc_into_raw(inner_arc))
    }

    /// Build a triangle-geometry bottom-level acceleration structure
    /// from CPU-side vertex + index data. Backs [`Self::create_ray_tracing_kernel`]
    /// — every TLAS instance references one of these BLAS handles.
    /// Returns a clean error when the device lacks RT support.
    #[cfg(target_os = "linux")]
    pub fn build_triangles_blas(
        &self,
        label: &str,
        vertices: &[f32],
        indices: &[u32],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        tracing::debug!(
            rhi_op = "build_triangles_blas",
            label,
            vertex_count = vertices.len() / 3,
            triangle_count = indices.len() / 3,
            "GpuContext::build_triangles_blas"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanAccelerationStructure::build_triangles_blas(
            vulkan_device,
            label,
            vertices,
            indices,
        )
    }

    /// Build a top-level acceleration structure from a list of TLAS
    /// instances. Each instance references a BLAS the TLAS keeps alive
    /// for its lifetime. Returns a clean error when the device lacks
    /// RT support.
    #[cfg(target_os = "linux")]
    pub fn build_tlas(
        &self,
        label: &str,
        instances: &[crate::vulkan::rhi::TlasInstanceDesc],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        tracing::debug!(
            rhi_op = "build_tlas",
            label,
            instance_count = instances.len(),
            "GpuContext::build_tlas"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanAccelerationStructure::build_tlas(vulkan_device, label, instances)
    }

    /// Whether the underlying GPU exposes the
    /// `VK_KHR_ray_tracing_pipeline` extension chain. RT-dependent
    /// consumers should check this before calling
    /// [`Self::create_ray_tracing_kernel`] /
    /// [`Self::build_triangles_blas`] / [`Self::build_tlas`].
    #[cfg(target_os = "linux")]
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        self.device.inner.supports_ray_tracing_pipeline()
    }

    /// Read-once GPU capability snapshot. Mirrors the underlying
    /// `HostVulkanDevice`'s capability getters into one struct so
    /// cdylib callers (camera processor, future plugins) can decide
    /// vendor-specific branching + DMA-BUF / external-memory paths
    /// at setup time without per-method vtable round-trips.
    #[cfg(target_os = "linux")]
    pub fn gpu_capabilities(&self) -> GpuCapabilitiesSnapshot {
        let dev = &self.device.inner;
        GpuCapabilitiesSnapshot {
            device_name: dev.name(),
            supports_external_memory: dev.supports_external_memory(),
            supports_cross_device_dma_buf_probe: dev.supports_cross_device_dma_buf_probe(),
            supports_ray_tracing_pipeline: dev.supports_ray_tracing_pipeline(),
        }
    }

    /// Construct a timeline semaphore against the host's vulkan device.
    /// Backs [`GpuContextFullAccess::create_timeline_semaphore`] which
    /// is the FullAccess-callable entry point.
    #[cfg(target_os = "linux")]
    pub fn create_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        let device = self.device.inner.device();
        let sem = crate::vulkan::rhi::HostVulkanTimelineSemaphore::new(device, initial_value)?;
        Ok(Arc::new(sem))
    }

    /// Construct an OPAQUE_FD-exportable timeline semaphore against the
    /// host's vulkan device. Backs
    /// [`GpuContextFullAccess::create_exportable_timeline_semaphore`]
    /// which is the FullAccess-callable entry point.
    ///
    /// Distinct from [`Self::create_timeline_semaphore`]: the returned
    /// semaphore is created with `VK_KHR_external_semaphore_fd` export
    /// support so its `export_opaque_fd` can hand a fresh OPAQUE_FD to a
    /// subprocess consumer (surface-share / CUDA cross-process sync).
    #[cfg(target_os = "linux")]
    pub fn create_exportable_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        let device = self.device.inner.device();
        let sem =
            crate::vulkan::rhi::HostVulkanTimelineSemaphore::new_exportable(device, initial_value)?;
        Ok(Arc::new(sem))
    }

    /// Import a DMA-BUF FD as a `StorageBuffer`. Camera V4L2 zero-copy
    /// path. **Consumes `fd` on success** (`vkImportMemoryFdInfoKHR`
    /// takes ownership); on failure caller retains fd and must close.
    #[cfg(target_os = "linux")]
    pub fn import_dma_buf_storage_buffer(
        &self,
        fd: std::os::unix::io::RawFd,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        let vulkan_device = &self.device.inner;
        let buf = crate::vulkan::rhi::HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(
            vulkan_device,
            fd,
            byte_size,
        )?;
        Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
            Arc::new(buf),
        ))
    }

    /// Allocate an OPAQUE_FD-exportable `VkBuffer` as a `StorageBuffer`.
    /// `device_local = true` picks the VRAM-resident CUDA-visible pool
    /// (`new_opaque_fd_export_device_local`); `false` picks the
    /// HOST_VISIBLE pool (`new_opaque_fd_export`). Backs
    /// [`GpuContextFullAccess::create_opaque_fd_export_buffer`], the
    /// cdylib-safe OPAQUE_FD/CUDA producer allocation (#1262).
    #[cfg(target_os = "linux")]
    pub fn create_opaque_fd_export_buffer(
        &self,
        byte_size: u64,
        device_local: bool,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        let vulkan_device = &self.device.inner;
        let buf = if device_local {
            crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export_device_local(
                vulkan_device,
                byte_size,
            )?
        } else {
            crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export(vulkan_device, byte_size)?
        };
        Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
            Arc::new(buf),
        ))
    }

    /// Export a fresh dup'd OPAQUE_FD from `buffer` plus its byte size
    /// and the exporting device's `VkPhysicalDeviceIDProperties::deviceUUID`.
    /// The fd ownership transfers to the caller; the 16-byte UUID is the
    /// entire CUDA device-binding contract on multi-GPU rigs (a cdylib
    /// CUDA adapter matches the CUDA device whose `cudaDeviceProp::uuid`
    /// equals this value, never a silent fall-through to CUDA device 0).
    /// Backs [`GpuContextFullAccess::export_storage_buffer_opaque_fd`]
    /// (#1262).
    #[cfg(target_os = "linux")]
    pub fn export_storage_buffer_opaque_fd(
        &self,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        let fd = buffer.host_inner().export_opaque_fd_memory()?;
        let size = buffer.byte_size();
        let uuid = self.device.inner.physical_device_uuid();
        Ok((fd, size, uuid))
    }

    /// Wrap an existing OPAQUE_FD `StorageBuffer` (flat `VkBuffer`) as a
    /// `PixelBuffer` sharing the same `Arc<HostVulkanBuffer>`, so the flat
    /// CUDA buffer can register through the existing
    /// `SurfaceStore::register_pixel_buffer_with_timeline` path. Backs
    /// [`GpuContextFullAccess::wrap_storage_buffer_as_pixel_buffer`]
    /// (#1262).
    #[cfg(target_os = "linux")]
    pub fn wrap_storage_buffer_as_pixel_buffer(
        &self,
        storage_buffer: &crate::core::rhi::StorageBuffer,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: crate::core::rhi::PixelFormat,
    ) -> Result<crate::core::rhi::PixelBuffer> {
        let inner = storage_buffer.host_inner_arc();
        Ok(crate::core::rhi::PixelBuffer::from_host_vulkan_buffer(
            inner,
            width,
            height,
            bytes_per_pixel,
            format,
        ))
    }

    /// Per-frame CUDA producer copy: in one host-device submission,
    /// optionally GPU-wait `consume_done` (`(timeline, wait_value)`),
    /// `vkCmdCopyImageToBuffer` from `source_texture` (currently in
    /// `source_layout`) into `dst`, then optionally signal `produce_done`
    /// (`(timeline, signal_value)`) on completion. When both timelines
    /// are `None` the submission blocks host-side via `submit_and_wait`.
    /// Backs
    /// [`GpuContextFullAccess::copy_texture_to_storage_buffer_and_signal`]
    /// (#1262).
    ///
    /// The source is copied directly from `source_layout` (the camera
    /// leaves ring textures in `GENERAL`, a legal copy-source layout), so
    /// no extra layout transition is recorded — the timeline signal's
    /// completion guarantee is what orders the buffer write ahead of the
    /// consumer's `acquire_read`.
    #[cfg(target_os = "linux")]
    pub fn copy_texture_to_storage_buffer_and_signal(
        &self,
        source_texture: &crate::core::rhi::Texture,
        source_layout: crate::core::rhi::VulkanLayout,
        dst: &crate::core::rhi::StorageBuffer,
        consume_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
        produce_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
    ) -> Result<()> {
        let vulkan_device = &self.device.inner;
        let mut recorder = crate::vulkan::rhi::RhiCommandRecorderInner::new(
            vulkan_device,
            "copy_texture_to_storage_buffer_and_signal",
        )?;
        recorder.begin()?;
        let region = crate::vulkan::rhi::ImageCopyRegion::tightly_packed(
            source_texture.width(),
            source_texture.height(),
        );
        recorder.record_copy_image_to_buffer(source_texture, source_layout, dst, region)?;
        match (consume_done, produce_done) {
            (None, None) => recorder.submit_and_wait(),
            (wait, signal) => recorder.submit_waiting_and_signaling_timeline(wait, signal),
        }
    }

    /// Transition `texture` into `VK_IMAGE_LAYOUT_GENERAL` via a
    /// one-shot command buffer + fence. Used as the prelude to binding
    /// a freshly-created storage image to a compute / RT kernel that
    /// will write into it via `imageStore`. The transition uses
    /// `UNDEFINED` as the source layout, so this is correct for
    /// just-allocated textures only — once the texture has content
    /// you'd otherwise lose, callers must use a barrier with the
    /// actual prior layout.
    ///
    /// Lives here (not on `HostVulkanTexture`) so example / processor
    /// code that needs a one-shot layout transition stays inside the
    /// RHI boundary instead of pulling vulkanalia directly. Mirrors
    /// the existing `acquire_*` shape on `GpuContext`.
    #[cfg(target_os = "linux")]
    pub fn transition_storage_image_to_general(
        &self,
        texture: &crate::core::rhi::Texture,
    ) -> Result<()> {
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::HostVulkanTexture::transition_to_general(
            vulkan_device,
            texture.vulkan_inner().image().ok_or_else(|| {
                crate::core::Error::GpuError(
                    "transition_storage_image_to_general: texture missing VkImage".to_string(),
                )
            })?,
        )
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
            Err(Error::GpuError(
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
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
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
        dest: &PixelBuffer,
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

    // =========================================================================
    // GraphicsKernelBridge — host-side dispatch for the graphics-kernel escalate ops
    // =========================================================================

    /// Register a [`GraphicsKernelBridge`] implementation. The escalate handler
    /// dispatches `register_graphics_kernel` and `run_graphics_draw` requests
    /// through this bridge; until it is set, those requests fail with an
    /// "unsupported" error response. Linux-only: graphics escalate uses the
    /// Linux-side `VulkanGraphicsKernel`.
    #[cfg(target_os = "linux")]
    pub fn set_graphics_kernel_bridge(&self, bridge: Arc<dyn GraphicsKernelBridge>) {
        *self.graphics_kernel_bridge.lock().unwrap() = Some(bridge);
    }

    /// Get the registered [`GraphicsKernelBridge`], if any.
    #[cfg(target_os = "linux")]
    pub fn graphics_kernel_bridge(&self) -> Option<Arc<dyn GraphicsKernelBridge>> {
        self.graphics_kernel_bridge.lock().unwrap().clone()
    }

    // =========================================================================
    // RayTracingKernelBridge — host-side dispatch for the RT-kernel escalate ops
    // =========================================================================

    /// Register a [`RayTracingKernelBridge`] implementation. The escalate
    /// handler dispatches `register_acceleration_structure_blas`,
    /// `register_acceleration_structure_tlas`, `register_ray_tracing_kernel`,
    /// and `run_ray_tracing_kernel` requests through this bridge; until it
    /// is set, those requests fail with an "unsupported" error response.
    /// Linux-only: RT escalate uses the Linux-side `VulkanRayTracingKernel`
    /// + `VulkanAccelerationStructure`.
    #[cfg(target_os = "linux")]
    pub fn set_ray_tracing_kernel_bridge(&self, bridge: Arc<dyn RayTracingKernelBridge>) {
        *self.ray_tracing_kernel_bridge.lock().unwrap() = Some(bridge);
    }

    /// Get the registered [`RayTracingKernelBridge`], if any.
    #[cfg(target_os = "linux")]
    pub fn ray_tracing_kernel_bridge(&self) -> Option<Arc<dyn RayTracingKernelBridge>> {
        self.ray_tracing_kernel_bridge.lock().unwrap().clone()
    }

    /// Check in a pixel buffer to the surface-share service, returning a surface ID.
    ///
    /// The surface ID can be shared with other processes (e.g., Python subprocesses)
    /// which can then call `check_out_surface` to get the same IOSurface.
    ///
    /// If this pixel buffer was already checked in, returns the existing ID.
    #[cfg(target_os = "macos")]
    pub fn check_in_surface(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        let store = self.surface_store.lock().unwrap();
        let store = store.as_ref().ok_or_else(|| {
            crate::core::Error::Configuration(
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
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        let store = self.surface_store.lock().unwrap();
        let store = store.as_ref().ok_or_else(|| {
            crate::core::Error::Configuration(
                "SurfaceStore not initialized. Call runtime.start() first.".into(),
            )
        })?;
        store.check_out(surface_id)
    }

    /// Check in a pixel buffer (non-macOS stub).
    #[cfg(not(target_os = "macos"))]
    pub fn check_in_surface(&self, _pixel_buffer: &PixelBuffer) -> Result<String> {
        Err(crate::core::Error::NotSupported(
            "Surface store is only supported on macOS".into(),
        ))
    }

    /// Check out a surface (non-macOS stub).
    #[cfg(not(target_os = "macos"))]
    pub fn check_out_surface(&self, _surface_id: &str) -> Result<PixelBuffer> {
        Err(crate::core::Error::NotSupported(
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
// Capability-typed wrappers
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
///
/// Restricted GPU capability shim with ABI-stable `(handle, vtable)`
/// shape. Both fields cross the plugin ABI unchanged:
///
/// - `handle`: opaque `*const c_void` pointing at a host-leaked
///   `Box<Arc<GpuContext>>`. Cdylib code passes this pointer to
///   [`GpuContextLimitedAccessVTable`] callbacks; the host's
///   callbacks (running in host-compiled code) cast it back to
///   `*const Arc<GpuContext>` and invoke real methods.
/// - `vtable`: pointer to the `&'static GpuContextLimitedAccessVTable`
///   installed by the host (resolved via
///   [`crate::core::plugin::host_services::host_gpu_context_limited_access_vtable`]).
#[repr(C)]
pub struct GpuContextLimitedAccess {
    /// Opaque host handle. Points at a `Box<Arc<GpuContext>>` allocated
    /// by host-compiled code; cdylib code treats it as opaque and
    /// passes it through to vtable callbacks unchanged.
    pub(crate) handle: *const std::ffi::c_void,
    /// Dispatch table. Set at construction from
    /// [`crate::core::plugin::host_services::host_gpu_context_limited_access_vtable`];
    /// host mode resolves to the `&HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`
    /// static, cdylib mode resolves to the host-installed pointer
    /// cached on [`HostServices::gpu_context_limited_access_vtable`].
    pub(crate) vtable: *const streamlib_plugin_abi::GpuContextLimitedAccessVTable,
}

// SAFETY: `handle` points at a host-owned `Box<Arc<GpuContext>>` that
// is `Send + Sync` (Arc carries atomic refcounts, GpuContext's
// fields are themselves Send + Sync via their Arc wrappers). The
// vtable pointer is `&'static` and pinned for the host's lifetime.
// Every method (engine and cdylib) reaches the GpuContext through
// the handle, gated on plugin mode by `host_inner()`'s `host_callbacks()`
// check.
unsafe impl Send for GpuContextLimitedAccess {}
unsafe impl Sync for GpuContextLimitedAccess {}

impl Clone for GpuContextLimitedAccess {
    /// plugin-ABI-safe Clone. Dispatches through
    /// [`GpuContextLimitedAccessVTable::clone_handle`] to bump the
    /// host's `Arc<GpuContext>` refcount.
    fn clone(&self) -> Self {
        let new_handle = if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle + vtable were paired at construction
            // and the host's `clone_handle` callback contractually
            // returns a fresh `Box::into_raw(Box::new(Arc::clone(...)))`-
            // shaped pointer the matching `drop_handle` releases.
            unsafe { ((*self.vtable).clone_handle)(self.handle) }
        } else {
            std::ptr::null()
        };
        Self {
            handle: new_handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for GpuContextLimitedAccess {
    /// Releases the host-owned handle via
    /// [`GpuContextLimitedAccessVTable::drop_handle`].
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle was produced by either `new()` (which
            // calls `Box::into_raw(Box::new(Arc::new(...)))`) or by
            // `clone_handle` (which produces the same shape). The
            // matching `drop_handle` callback runs
            // `Box::from_raw + drop` on the host side.
            unsafe { ((*self.vtable).drop_handle)(self.handle) };
        }
    }
}

/// Privileged GPU capability handed to `setup()` and inside
/// [`GpuContextLimitedAccess::escalate`] closures.
///
/// Exposes the full GPU API, including resource creation and
/// device-wide operations.
///
/// Deliberately **not** `Clone`. Processors only ever see a
/// `&GpuContextFullAccess` borrowed from a `RuntimeContextFullAccess`
/// wrapper for the duration of a single lifecycle call (setup /
/// teardown / start / stop / escalate closure). Removing `Clone` makes
/// "stash a FullAccess in a field" a compile error: nothing can
/// produce an owned value outside the runtime's construction path, so
/// the capability can never escape its call.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib::sdk::context::GpuContextFullAccess>();
/// ```
///
/// # In-process vs vtable dispatch (Phase C3)
///
/// Two construction paths populate this struct depending on which
/// side of the plugin ABI the caller lives on; the same surface
/// methods work from either:
///
/// - **In-process dispatch** ([`Self::new`], `pub(in
///   crate::core::context)` so only
///   [`GpuContextLimitedAccess::escalate`]'s engine-internal body
///   can construct it). `handle` is a host-allocated
///   `Box<Arc<GpuContext>>` and `handle_kind` is
///   [`HandleKind::Boxed`]. Every method routes through
///   [`Self::host_inner`] for direct dispatch — no plugin ABI hop, no
///   scope-registry lookup. Drop runs
///   [`std::boxed::Box::from_raw`] on the boxed Arc.
/// - **Vtable dispatch** ([`Self::from_scope_token`], reached from
///   the cdylib path of [`GpuContextLimitedAccess::escalate`]).
///   `handle` is an opaque scope token issued by the host's
///   [`GpuContextLimitedAccessVTable`]'s `escalate_begin` callback
///   and `handle_kind` is [`HandleKind::ScopeToken`]. Every method
///   routes through the vtable; the host validates the token against
///   [`super::escalate_scope_registry::with_scope`] before dispatch.
///   Drop is a no-op — cleanup runs in the matching `escalate_end`
///   callback the cdylib's wrapper invokes after the closure returns.
///
/// New methods that any cdylib code can reach MUST add a matching
/// vtable entry on [`GpuContextFullAccessVTable`] (otherwise the
/// vtable-dispatched path silently can't reach them).
/// Engine-internal methods that no cdylib path ever needs can be
/// host_inner-only.
///
/// [`GpuContextFullAccessVTable`]: streamlib_plugin_abi::GpuContextFullAccessVTable
/// [`GpuContextLimitedAccessVTable`]: streamlib_plugin_abi::GpuContextLimitedAccessVTable
#[repr(C)]
pub struct GpuContextFullAccess {
    pub(crate) handle: *const std::ffi::c_void,
    pub(crate) vtable: *const streamlib_plugin_abi::GpuContextFullAccessVTable,
    /// Discriminator for [`Drop`]:
    /// - [`HandleKind::Boxed`] (in-process dispatch shape): Drop
    ///   runs `Box::from_raw` on the boxed `Arc<GpuContext>`.
    /// - [`HandleKind::ScopeToken`] (vtable-dispatched shape): Drop
    ///   is a no-op; the cdylib's escalate wrapper handles cleanup
    ///   via the LimitedAccess vtable's `escalate_end` callback.
    ///
    /// Set by the constructor ([`Self::new`] vs
    /// [`Self::from_scope_token`]).
    pub(crate) handle_kind: HandleKind,
    /// Inherited LimitedAccess handle (scope-token mode only).
    ///
    /// Phase D — Option B (Limited-handle inheritance): the
    /// FullAccess wrappers for the LimitedAccess-mirror methods
    /// (`acquire_pixel_buffer`, `register_texture_with_layout`,
    /// `surface_store`, etc.) dispatch through the **inherited**
    /// LimitedAccess vtable using this handle, rather than through a
    /// parallel FullAccess vtable slot. Reuses the C1-proven
    /// LimitedAccess dispatch surface instead of mirroring it on
    /// FullAccess. The LimitedAccess that originated the escalate
    /// scope outlives the FullAccess (closure scope), so borrowing
    /// the handle without bumping its refcount is sound.
    ///
    /// `null` in [`HandleKind::Boxed`] (in-process) mode — engine
    /// callers use `host_inner()` directly and don't need this.
    pub(crate) inherited_lim_handle: *const std::ffi::c_void,
    /// Inherited LimitedAccess vtable pointer paired with
    /// [`Self::inherited_lim_handle`]. See its doc for the role.
    ///
    /// `null` in [`HandleKind::Boxed`] (in-process) mode.
    pub(crate) inherited_lim_vtable: *const streamlib_plugin_abi::GpuContextLimitedAccessVTable,
}

/// Discriminator for [`GpuContextFullAccess`]'s `handle` field. The
/// engine-internal in-process constructor sets [`Self::Boxed`]; the
/// cdylib vtable-dispatched constructor sets [`Self::ScopeToken`].
/// [`GpuContextFullAccess::drop`] dispatches on this kind.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandleKind {
    /// Handle is a host-allocated `Box<Arc<GpuContext>>` from
    /// [`GpuContextFullAccess::new`]. Methods on the cdylib-facing
    /// `GpuContextFullAccess` surface dispatch through `host_inner`
    /// directly (no plugin ABI hop). Used by engine-internal escalate
    /// scopes.
    Boxed = 0,
    /// Handle is an opaque scope token from the host's
    /// `GpuContextLimitedAccessVTable::escalate_begin` callback.
    /// Methods dispatch through the FullAccess vtable; the host
    /// validates the token against the scope registry before
    /// dispatch. Used by cdylib escalate scopes.
    ScopeToken = 1,
}

// SAFETY: same shape as `GpuContextLimitedAccess`. The handle points
// at a host-owned `Box<Arc<GpuContext>>` (in-process dispatch shape)
// or an opaque scope token (vtable-dispatched shape); both are
// `Send + Sync` (the scope token is a u64 reinterpreted as
// `*const c_void`; the boxed Arc<GpuContext> is Send + Sync). The
// vtable pointer is `&'static`. The inherited LimitedAccess fields
// either point at the same host-owned `Box<Arc<GpuContext>>` the
// originating LimitedAccess holds (scope-token mode) — borrowed for
// the closure's lifetime, no refcount bump — or are null (Boxed
// mode); both are `Send + Sync` for the same reason as `handle`.
unsafe impl Send for GpuContextFullAccess {}
unsafe impl Sync for GpuContextFullAccess {}

impl Drop for GpuContextFullAccess {
    /// Releases the handle.
    ///
    /// - [`HandleKind::Boxed`] (in-process dispatch shape): runs
    ///   `Box::from_raw` on the boxed `Arc<GpuContext>` directly,
    ///   without going through the vtable. No plugin ABI hop;
    ///   engine-internal cleanup.
    /// - [`HandleKind::ScopeToken`] (vtable-dispatched shape):
    ///   no-op. The cdylib's escalate wrapper that constructed this
    ///   `GpuContextFullAccess` calls `escalate_end` on the
    ///   LimitedAccess vtable after the closure returns, which
    ///   removes the scope from the registry and releases the
    ///   escalate gate. Doing it here too would double-release.
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        match self.handle_kind {
            HandleKind::Boxed => {
                // SAFETY: handle was produced by `Self::new` via
                // `Box::into_raw(Box::new(Arc::new(GpuContext)))`.
                // `Box::from_raw` releases the box; the resulting
                // Arc<GpuContext>'s Drop releases the per-scope clone.
                let _ = unsafe { Box::from_raw(self.handle as *mut std::sync::Arc<GpuContext>) };
            }
            HandleKind::ScopeToken => {
                // No-op — escalate_end is the authority. See doc.
            }
        }
    }
}

impl GpuContextLimitedAccess {
    /// Wrap a [`GpuContext`] as a limited-access capability.
    ///
    /// The handle is the sole owning reference to the
    /// `Arc<GpuContext>`; every engine method reaches it through
    /// [`Self::host_inner`] and every cdylib method dispatches
    /// through the vtable. Allocates a host-side
    /// `Box<Arc<GpuContext>>` as the opaque handle, then resolves
    /// the vtable through
    /// [`crate::core::plugin::host_services::host_gpu_context_limited_access_vtable`]
    /// (plugin-ABI-routed: host static in host mode, cdylib-installed
    /// pointer in cdylib mode).
    pub(crate) fn new(inner: GpuContext) -> Self {
        // Leak a fresh `Arc<GpuContext>` to back the opaque handle.
        // The handle is the sole owner; `host_inner()` derefs it for
        // engine callers, the vtable callbacks deref it for cdylib
        // callers.
        let arc: std::sync::Arc<GpuContext> = std::sync::Arc::new(inner);
        let boxed: Box<std::sync::Arc<GpuContext>> = Box::new(arc);
        let handle = Box::into_raw(boxed) as *const std::ffi::c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self { handle, vtable }
    }

    /// Engine-internal borrow of the host's [`GpuContext`] (read
    /// through the handle's `Box<Arc<GpuContext>>`).
    ///
    /// **Panics if called from cdylib code.** The `GpuContext` value
    /// itself is host-private; cdylib code that reads it would deref
    /// host-written bytes under cdylib's view of `GpuContext`'s
    /// layout, which is undefined behaviour under the deployment
    /// model the plugin ABI supports (different rustc minor versions
    /// + different dep graphs between host and cdylib). Cdylib code
    /// dispatches through the
    /// [`GpuContextLimitedAccessVTable`](streamlib_plugin_abi::GpuContextLimitedAccessVTable)
    /// instead — every cdylib-callable method on
    /// [`GpuContextLimitedAccess`] is wired through the vtable.
    ///
    /// The panic is caught by `run_host_extern_c` at the plugin ABI
    /// boundary (host extern "C" callbacks all route through
    /// `catch_unwind`), so a misconfigured cdylib path gets a clean
    /// "callback panicked" log entry instead of UB.
    pub(crate) fn host_inner(&self) -> &GpuContext {
        // `host_callbacks()` is `Some` in cdylib mode (set by
        // `install_host_services`) and `None` in host mode.
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextLimitedAccess::host_inner() reached from cdylib code; \
                 this method must dispatch through the GpuContextLimitedAccessVTable. \
                 The panic is caught by run_host_extern_c at the plugin ABI."
            );
        }
        // SAFETY: `self.handle` was produced by `Self::new` or
        // `host_gpu_lim_clone_handle` — both produce
        // `Box::into_raw(Box::new(Arc::new(GpuContext)))`. The
        // matching `host_gpu_lim_drop_handle` runs on Drop, so the
        // `Arc<GpuContext>` is alive for the duration of `&self`.
        // We deref the Box, then the Arc, to borrow the inner
        // `GpuContext`.
        unsafe {
            let arc = &*(self.handle as *const std::sync::Arc<GpuContext>);
            &**arc
        }
    }

    /// Produce a [`GpuContextFullAccess`] view of the same underlying context.
    ///
    /// In #323 this becomes private and only reachable through
    /// `escalate(|full| …)`; today it is `pub(crate)` so the runtime and
    /// processor setup paths can still reach the full surface without a
    /// compile-time barrier.
    pub(crate) fn to_full_access(&self) -> GpuContextFullAccess {
        GpuContextFullAccess::new(self.host_inner().clone())
    }

    /// Serialized escalation to full GPU capability. Hands the
    /// closure a [`GpuContextFullAccess`] scoped to its body, with
    /// the host's escalate gate held for the duration; after the
    /// closure returns the gate releases and the device waits idle.
    ///
    /// This is the single primitive for GPU resource-creation work
    /// outside `setup()` — used by the compiler to run each
    /// processor's setup() and by running processors that need to
    /// reconfigure (acquire a new video session, resize a swapchain,
    /// etc.).
    ///
    /// Mode-routed:
    /// - **In-process dispatch** (engine-internal callers): acquires
    ///   the gate directly, constructs [`GpuContextFullAccess::new`]
    ///   (Boxed), runs the closure with method dispatch via
    ///   [`GpuContextFullAccess::host_inner`] (no plugin ABI hop), then
    ///   waits device idle and releases the gate.
    /// - **Vtable dispatch** (cdylib callers): dispatches through
    ///   the [`GpuContextLimitedAccessVTable`]'s `escalate_begin` /
    ///   `escalate_end` callback pair. Constructs
    ///   [`GpuContextFullAccess::from_scope_token`] (ScopeToken) so
    ///   method dispatch crosses through the FullAccess vtable; the
    ///   host's `escalate_end` callback handles `wait_device_idle`
    ///   and releases the gate. A closure panic still unwinds; the
    ///   matching `escalate_end` fires through a guard so the gate
    ///   never leaks.
    ///
    /// The runtime mode is selected by `host_callbacks().is_some()`:
    /// true in cdylib code (callbacks installed by the host), false
    /// in engine-internal code.
    ///
    /// Closure failure returns the closure's error; on closure
    /// success a follow-up `wait_device_idle` error is propagated.
    ///
    /// [`GpuContextLimitedAccessVTable`]: streamlib_plugin_abi::GpuContextLimitedAccessVTable
    pub fn escalate<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            self.escalate_via_vtable(f)
        } else {
            self.escalate_in_process(f)
        }
    }

    /// Engine-internal escalate path. Direct in-process dispatch —
    /// no plugin ABI hop. See [`Self::escalate`] for the mode router.
    fn escalate_in_process<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        let inner = self.host_inner();
        let lock_start = std::time::Instant::now();
        let _gate_guard = inner.escalate_gate().enter_scoped();
        let mutex_wait_ns = lock_start.elapsed().as_nanos() as u64;

        let closure_start = std::time::Instant::now();
        let full = GpuContextFullAccess::new(inner.clone());
        let closure_result = f(&full);
        drop(full);
        let closure_duration_ns = closure_start.elapsed().as_nanos() as u64;

        let wait_start = std::time::Instant::now();
        let wait_result = inner.wait_device_idle();
        let wait_idle_ns = wait_start.elapsed().as_nanos() as u64;

        tracing::trace!(
            target: "streamlib::gpu_context::escalate",
            dispatch = "in_process",
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

    /// Cdylib escalate path. Dispatches through the LimitedAccess
    /// vtable's `escalate_begin` / `escalate_end` pair; constructs
    /// `GpuContextFullAccess::from_scope_token` so the closure's
    /// FullAccess method calls cross the plugin ABI through the
    /// FullAccess vtable. See [`Self::escalate`] for the mode router.
    fn escalate_via_vtable<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "escalate (vtable): GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        // SAFETY: vtable + handle were paired at construction; vtable
        // is `&'static`.
        let vt = unsafe { &*self.vtable };

        let lock_start = std::time::Instant::now();
        let mut scope_token: *const std::ffi::c_void = std::ptr::null();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let begin_rc = unsafe {
            (vt.escalate_begin)(
                self.handle,
                &mut scope_token,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        let mutex_wait_ns = lock_start.elapsed().as_nanos() as u64;
        if begin_rc != 0 {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            return Err(Error::GpuError(format!(
                "escalate (vtable): escalate_begin failed: {msg}"
            )));
        }

        let closure_start = std::time::Instant::now();
        // Pass through the originating LimitedAccess (handle, vtable)
        // so the FullAccess wrappers for the LimitedAccess-mirror
        // methods (Phase D Option B) can dispatch through the C1-
        // proven LimitedAccess vtable. The originating LimitedAccess
        // owns the handle for the lifetime of this scope (we hold
        // `&self` across the closure), so borrowing without bumping
        // the refcount is sound.
        let full = GpuContextFullAccess::from_scope_token(scope_token, self.handle, self.vtable);
        // catch_unwind so a closure panic still fires escalate_end —
        // otherwise the host's escalate gate would leak. We lean on
        // AssertUnwindSafe because `escalate`'s public signature
        // doesn't add an `UnwindSafe` bound on `F` (the in-process
        // path doesn't need it; only this path catches the unwind to
        // pair with `escalate_end`).
        let closure_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&full)));
        drop(full);
        let closure_duration_ns = closure_start.elapsed().as_nanos() as u64;

        let wait_start = std::time::Instant::now();
        let mut end_err_buf = [0u8; 512];
        let mut end_err_len: usize = 0;
        let end_rc = unsafe {
            (vt.escalate_end)(
                self.handle,
                scope_token,
                end_err_buf.as_mut_ptr(),
                end_err_buf.len(),
                &mut end_err_len as *mut usize,
            )
        };
        let wait_idle_ns = wait_start.elapsed().as_nanos() as u64;

        tracing::trace!(
            target: "streamlib::gpu_context::escalate",
            dispatch = "vtable",
            mutex_wait_ns,
            closure_duration_ns,
            wait_idle_ns,
            closure_ok = closure_result.is_ok(),
            "GpuContextLimitedAccess::escalate completed"
        );

        check_sustained_escalation_rate();

        let wait_result: Result<()> = if end_rc != 0 {
            let msg = String::from_utf8_lossy(&end_err_buf[..end_err_len.min(end_err_buf.len())])
                .into_owned();
            Err(Error::GpuError(format!(
                "escalate (vtable): escalate_end failed: {msg}"
            )))
        } else {
            Ok(())
        };

        match closure_result {
            Ok(Ok(value)) => match wait_result {
                Ok(()) => Ok(value),
                Err(e) => Err(e),
            },
            Ok(Err(e)) => Err(e),
            Err(panic) => std::panic::resume_unwind(panic),
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
    /// Back-room constructor. Wraps an in-process [`GpuContext`] as a
    /// full-access capability whose methods route through
    /// [`Self::host_inner`] for direct dispatch.
    ///
    /// Scope tightened to `pub(in crate::core::context)` so only
    /// [`GpuContextLimitedAccess::escalate`]'s host-mode body can
    /// construct one. Other engine code that wants FullAccess goes
    /// through `escalate(|full| ...)`; the privilege gate enforces
    /// serialization + `wait_device_idle`.
    pub(in crate::core::context) fn new(inner: GpuContext) -> Self {
        let arc: std::sync::Arc<GpuContext> = std::sync::Arc::new(inner);
        let boxed: Box<std::sync::Arc<GpuContext>> = Box::new(arc);
        let handle = Box::into_raw(boxed) as *const std::ffi::c_void;
        let vtable = crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
        Self {
            handle,
            vtable,
            handle_kind: HandleKind::Boxed,
            // In-process Boxed mode never consults the inherited
            // LimitedAccess vtable — `host_inner()` is the direct path.
            // Null sentinels match the field doc on the struct.
            inherited_lim_handle: std::ptr::null(),
            inherited_lim_vtable: std::ptr::null(),
        }
    }

    /// Lobby constructor. Wraps an opaque scope token (issued by the
    /// host's
    /// [`GpuContextLimitedAccessVTable`](streamlib_plugin_abi::GpuContextLimitedAccessVTable)'s
    /// `escalate_begin` callback) as a full-access capability whose
    /// methods route through the
    /// [`GpuContextFullAccessVTable`](streamlib_plugin_abi::GpuContextFullAccessVTable)
    /// for plugin ABI dispatch.
    ///
    /// Used by the cdylib-mode path of
    /// [`GpuContextLimitedAccess::escalate`]; the matching cleanup
    /// (release the escalate gate + `wait_device_idle`) runs inside
    /// `escalate_end` rather than [`Drop`], so the FullAccess's
    /// [`Drop`] short-circuits.
    ///
    /// `inherited_lim_handle` / `inherited_lim_vtable` are the
    /// originating `GpuContextLimitedAccess`'s handle + vtable. Phase
    /// D's Option-B dispatch shape for the LimitedAccess-mirror
    /// methods (e.g. [`Self::acquire_pixel_buffer`],
    /// [`Self::register_texture_with_layout`], [`Self::surface_store`])
    /// borrows these to dispatch through the C1-proven LimitedAccess
    /// vtable, avoiding ~20 duplicate slot entries on the FullAccess
    /// vtable. See the struct doc for the inheritance rationale.
    pub(in crate::core::context) fn from_scope_token(
        scope_token: *const std::ffi::c_void,
        inherited_lim_handle: *const std::ffi::c_void,
        inherited_lim_vtable: *const streamlib_plugin_abi::GpuContextLimitedAccessVTable,
    ) -> Self {
        let vtable = crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
        Self {
            handle: scope_token,
            vtable,
            handle_kind: HandleKind::ScopeToken,
            inherited_lim_handle,
            inherited_lim_vtable,
        }
    }

    /// Engine-internal borrow of the host's [`GpuContext`] (read
    /// through the handle's `Box<Arc<GpuContext>>`).
    ///
    /// **Panics if called from cdylib code.** Mirrors
    /// [`GpuContextLimitedAccess::host_inner`]'s contract: cdylib code
    /// must dispatch through the
    /// [`GpuContextFullAccessVTable`](streamlib_plugin_abi::GpuContextFullAccessVTable)
    /// instead. The panic is caught by `run_host_extern_c` at the plugin ABI
    /// boundary.
    pub(crate) fn host_inner(&self) -> &GpuContext {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::host_inner() reached from cdylib code; \
                 this method must dispatch through the GpuContextFullAccessVTable. \
                 The panic is caught by run_host_extern_c at the plugin ABI. \
                 \
                 Read docs/architecture/cdylib-reachability.md before workarounds — \
                 the right pattern depends on the lifecycle stage and the type \
                 shape. For setup()/teardown() bodies, ctx.gpu_full_access() now \
                 dispatches through the vtable (Pattern 1, #1072). For accessor \
                 returns, see PluginAbiObject Arc-transit slots (Pattern 2). For per- \
                 method binding work, see per-method vtable slots (Pattern 3). \
                 DO NOT call gpu_limited_access().escalate(...) from setup() / \
                 teardown() — the gate is already held and re-entry panics."
            );
        }
        // SAFETY: `self.handle` was produced by `Self::new`, which
        // calls `Box::into_raw(Box::new(Arc::new(GpuContext)))`. The
        // matching `host_gpu_full_drop_handle` runs on Drop, so the
        // `Arc<GpuContext>` is alive for the duration of `&self`.
        unsafe {
            let arc = &*(self.handle as *const std::sync::Arc<GpuContext>);
            &**arc
        }
    }

    /// Borrow the inner [`GpuContext`]. Crate-internal — call sites
    /// migrate to capability-typed methods as the privilege split
    /// lands.
    pub(crate) fn inner(&self) -> &GpuContext {
        self.host_inner()
    }

    /// Phase D Option B helper — construct a non-dropping view of
    /// the originating [`GpuContextLimitedAccess`] for cdylib
    /// dispatch through the inherited vtable.
    ///
    /// Used by the FullAccess mirror methods (`acquire_pixel_buffer`,
    /// `register_texture_with_layout`, `surface_store`, etc.) to
    /// dispatch through the C1-proven LimitedAccess vtable instead
    /// of mirroring those slots on the FullAccess vtable.
    ///
    /// **Must be wrapped in [`std::mem::ManuallyDrop`] at the call
    /// site** so the borrowed handle isn't double-released — the
    /// originating LimitedAccess outlives the FullAccess scope and
    /// owns the only Drop responsibility for the handle.
    pub(crate) fn inherited_limited_unchecked(
        &self,
    ) -> std::mem::ManuallyDrop<GpuContextLimitedAccess> {
        std::mem::ManuallyDrop::new(GpuContextLimitedAccess {
            handle: self.inherited_lim_handle,
            vtable: self.inherited_lim_vtable,
        })
    }

    /// Produce a [`GpuContextLimitedAccess`] view of the same
    /// underlying context.
    pub(crate) fn to_limited_access(&self) -> GpuContextLimitedAccess {
        GpuContextLimitedAccess::new(self.host_inner().clone())
    }
}

// -----------------------------------------------------------------------------
// Vtable-dispatch helper for the 4 acquire_*_buffer methods.
// Each Linux-only buffer type follows the same out-param + err_buf
// convention, so a single generic helper covers all 4 callsites
// without per-call boilerplate.
// -----------------------------------------------------------------------------

#[cfg(target_os = "linux")]
type AcquireBufferCallback = unsafe extern "C" fn(
    handle: *const std::ffi::c_void,
    byte_size: u64,
    out_buffer: *mut std::ffi::c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32;

#[cfg(target_os = "linux")]
fn acquire_buffer_via_vtable<B, F>(
    lim: &GpuContextLimitedAccess,
    byte_size: u64,
    label: &'static str,
    get_cb: F,
) -> Result<B>
where
    F: FnOnce(&streamlib_plugin_abi::GpuContextLimitedAccessVTable) -> AcquireBufferCallback,
{
    if lim.handle.is_null() || lim.vtable.is_null() {
        return Err(Error::GpuError(format!(
            "{label}: GpuContextLimitedAccess has null handle/vtable"
        )));
    }
    let mut out: std::mem::MaybeUninit<B> = std::mem::MaybeUninit::uninit();
    let mut err_buf = [0u8; 512];
    let mut err_len: usize = 0;
    // SAFETY: vtable + handle were paired at construction; `get_cb`
    // selects a fn pointer that adheres to the standard
    // `acquire_*_buffer` shape. `out` points at uninitialized stack
    // storage the host writes a valid `B` into on success.
    let cb: AcquireBufferCallback = unsafe { get_cb(&*lim.vtable) };
    let status = unsafe {
        cb(
            lim.handle,
            byte_size,
            out.as_mut_ptr() as *mut std::ffi::c_void,
            err_buf.as_mut_ptr(),
            err_buf.len(),
            &mut err_len as *mut usize,
        )
    };
    if status == 0 {
        // SAFETY: host signaled success and wrote a valid `B`.
        Ok(unsafe { out.assume_init() })
    } else {
        let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
        Err(Error::GpuError(msg))
    }
}

// -----------------------------------------------------------------------------
// Capability-split API surface.
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
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `acquire_pixel_buffer` callback. The tuple return is encoded
    /// via paired out-params: the pool id's string bytes land in a
    /// fixed-size stack buffer (1 KiB; UUID strings are well under
    /// 128 bytes), and the PluginAbiObject PixelBuffer goes into a
    /// MaybeUninit slot.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PixelBufferPoolId, PixelBuffer)> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "acquire_pixel_buffer: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut pool_id_buf = [0u8; 1024];
        let mut pool_id_len: usize = 0;
        let mut out_pb: std::mem::MaybeUninit<PixelBuffer> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.vtable).acquire_pixel_buffer)(
                self.handle,
                width,
                height,
                format as u32,
                pool_id_buf.as_mut_ptr(),
                pool_id_buf.len(),
                &mut pool_id_len as *mut usize,
                out_pb.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            let id_str =
                String::from_utf8_lossy(&pool_id_buf[..pool_id_len.min(pool_id_buf.len())])
                    .into_owned();
            let pool_id = PixelBufferPoolId::from_string(id_str);
            let pb = unsafe { out_pb.assume_init() };
            Ok((pool_id, pb))
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Acquire a HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    /// See [`GpuContext::acquire_storage_buffer`].
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `acquire_storage_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_storage_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        acquire_buffer_via_vtable(self, byte_size, "acquire_storage_buffer", |vt| {
            vt.acquire_storage_buffer
        })
    }

    /// Acquire a HOST_VISIBLE uniform buffer.
    /// See [`GpuContext::acquire_uniform_buffer`].
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `acquire_uniform_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        acquire_buffer_via_vtable(self, byte_size, "acquire_uniform_buffer", |vt| {
            vt.acquire_uniform_buffer
        })
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    /// See [`GpuContext::acquire_vertex_buffer`].
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `acquire_vertex_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        acquire_buffer_via_vtable(self, byte_size, "acquire_vertex_buffer", |vt| {
            vt.acquire_vertex_buffer
        })
    }

    /// Acquire a HOST_VISIBLE index buffer.
    /// See [`GpuContext::acquire_index_buffer`].
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `acquire_index_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        acquire_buffer_via_vtable(self, byte_size, "acquire_index_buffer", |vt| {
            vt.acquire_index_buffer
        })
    }

    /// Get a pixel buffer by its pool id (Split: local cache).
    ///
    /// Dispatches through the plugin ABI vtable's `get_pixel_buffer`
    /// callback.
    pub fn get_pixel_buffer(&self, pool_id: &PixelBufferPoolId) -> Result<PixelBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "get_pixel_buffer: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let id_str = pool_id.as_str();
        let mut out_pb: std::mem::MaybeUninit<PixelBuffer> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.vtable).get_pixel_buffer)(
                self.handle,
                id_str.as_ptr(),
                id_str.len(),
                out_pb.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_pb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `resolve_pixel_buffer_by_surface_id` callback.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "resolve_pixel_buffer_by_surface_id: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_pb: std::mem::MaybeUninit<PixelBuffer> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.vtable).resolve_pixel_buffer_by_surface_id)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                out_pb.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_pb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Register a texture in the same-process texture cache.
    ///
    /// Dispatches through the plugin ABI
    /// [`GpuContextLimitedAccessVTable::register_texture`](streamlib_plugin_abi::GpuContextLimitedAccessVTable::register_texture)
    /// callback. The host-side impl bumps the
    /// `Arc<TextureInner>` refcount before stashing a clone in the
    /// cache, so dropping the caller's `texture` here releases
    /// exactly the caller's owned ref.
    pub fn register_texture(&self, id: &str, texture: Texture) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable were paired at construction.
        unsafe {
            ((*self.vtable).register_texture)(
                self.handle,
                id.as_ptr(),
                id.len(),
                texture.handle,
                0, // VulkanLayout::UNDEFINED.0 == 0
            );
        }
        drop(texture);
    }

    /// Register a texture with a declared initial Vulkan image layout.
    /// See [`GpuContext::register_texture_with_layout`].
    ///
    /// Dispatches through the plugin ABI vtable's `register_texture`
    /// callback with the layout's `i32` enumerant.
    #[cfg(target_os = "linux")]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable were paired at construction.
        unsafe {
            ((*self.vtable).register_texture)(
                self.handle,
                id.as_ptr(),
                id.len(),
                texture.handle,
                initial_layout.0,
            );
        }
        drop(texture);
    }

    /// Update a registered texture's tracked layout after a transition.
    /// See [`GpuContext::update_texture_registration_layout`].
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `update_texture_registration_layout` callback.
    #[cfg(target_os = "linux")]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable were paired at construction.
        unsafe {
            ((*self.vtable).update_texture_registration_layout)(
                self.handle,
                id.as_ptr(),
                id.len(),
                layout.0,
            );
        }
    }

    /// Resolve a VideoFrame's full registration record (texture + layout).
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `resolve_texture_registration_by_surface_id` callback. Returns
    /// a β-reshaped [`TextureRegistration`] value (handle + vtable);
    /// Clone is cheap (refcount bump via vtable), Drop releases the
    /// host's `Arc<TextureRegistrationInner>` strong count.
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<TextureRegistration> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "resolve_texture_registration_by_surface_id: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_reg: std::mem::MaybeUninit<TextureRegistration> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let (has_layout, layout_raw) = match texture_layout {
            Some(v) => (1i32, v),
            None => (0i32, 0i32),
        };
        // SAFETY: handle + vtable were paired at construction.
        // `out_reg` is uninitialized stack storage; the host writes a
        // valid TextureRegistration on success (status == 0) and
        // nothing on failure.
        let status = unsafe {
            ((*self.vtable).resolve_texture_registration_by_surface_id)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                has_layout,
                layout_raw,
                width,
                height,
                out_reg.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid TextureRegistration.
            Ok(unsafe { out_reg.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Resolve a VideoFrame's texture (Split: cache hit).
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `resolve_texture_by_surface_id` callback.
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "resolve_texture_by_surface_id: GpuContextLimitedAccess has null handle/vtable"
                    .into(),
            ));
        }
        let mut out_texture: std::mem::MaybeUninit<Texture> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let (has_layout, layout_raw) = match texture_layout {
            Some(v) => (1i32, v),
            None => (0i32, 0i32),
        };
        // SAFETY: handle + vtable were paired at construction. `out_texture`
        // points at uninitialized stack storage that the host writes a
        // valid `Texture` into on success (return code 0). On failure the
        // host writes nothing into `out_texture` so we leave it
        // `MaybeUninit` and never assume_init it.
        let status = unsafe {
            ((*self.vtable).resolve_texture_by_surface_id)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                has_layout,
                layout_raw,
                width,
                height,
                out_texture.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid Texture.
            Ok(unsafe { out_texture.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// See [`GpuContext::set_video_source_timeline_semaphore`].
    ///
    /// Dispatches through the plugin ABI
    /// [`GpuContextLimitedAccessVTable::set_video_source_timeline_semaphore`](streamlib_plugin_abi::GpuContextLimitedAccessVTable::set_video_source_timeline_semaphore)
    /// callback. The cdylib passes
    /// `Arc::as_ptr(timeline) as *const c_void` — a **borrowed**
    /// pointer; the host's callback `Arc::increment_strong_count`s
    /// it, reconstitutes a temporary owned Arc via `Arc::from_raw`,
    /// calls the underlying `GpuContext::set_video_source_timeline_semaphore`
    /// (which itself clones into the published slot), and lets the
    /// temporary drop. Net effect: one fresh strong count moves into
    /// the slot; the caller's Arc is unchanged.
    ///
    /// **Arc-raw-pointer transit** — see
    /// [`GpuContextFullAccess::create_timeline_semaphore`]'s docs on
    /// the same rustc-version-coupling caveat. In-tree consumers
    /// (camera, display) ride this freely; cross-repo distribution
    /// awaits a PluginAbiObject lift of `HostVulkanTimelineSemaphore`.
    #[cfg(target_os = "linux")]
    pub fn set_video_source_timeline_semaphore(
        &self,
        timeline: &Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        let timeline_ptr = Arc::as_ptr(timeline) as *const std::ffi::c_void;
        // SAFETY: handle + vtable were paired at construction; the
        // host's callback contractually does the
        // increment-+-from_raw-+-clone-+-drop dance for the borrowed
        // Arc pointer.
        unsafe {
            ((*self.vtable).set_video_source_timeline_semaphore)(self.handle, timeline_ptr);
        }
    }

    /// See [`GpuContext::clear_video_source_timeline_semaphore`].
    ///
    /// Dispatches through the plugin ABI
    /// [`GpuContextLimitedAccessVTable::clear_video_source_timeline_semaphore`](streamlib_plugin_abi::GpuContextLimitedAccessVTable::clear_video_source_timeline_semaphore)
    /// callback. Pairs with
    /// [`Self::set_video_source_timeline_semaphore`].
    #[cfg(target_os = "linux")]
    pub fn clear_video_source_timeline_semaphore(&self) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable were paired at construction.
        unsafe {
            ((*self.vtable).clear_video_source_timeline_semaphore)(self.handle);
        }
    }

    /// See [`GpuContext::video_source_timeline_semaphore`].
    ///
    /// **Engine-only** — return type
    /// `Option<Arc<HostVulkanTimelineSemaphore>>` borrows into
    /// host-private state, so the borrow can't cross the plugin ABI
    /// boundary. Calling from a cdylib panics at the explicit guard
    /// below. **Cdylib callers** must use
    /// [`Self::host_video_source_timeline_arc`] instead — it returns
    /// the same `Option<Arc<...>>` but via the v14 vtable slot that
    /// transits the Arc as a raw pointer
    /// (`Arc::into_raw` / `Arc::from_raw`).
    #[cfg(target_os = "linux")]
    pub fn video_source_timeline_semaphore(
        &self,
    ) -> Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextLimitedAccess::video_source_timeline_semaphore() \
                 reached from cdylib code; use \
                 `host_video_source_timeline_arc()` instead. The legacy method \
                 returns `Option<Arc<HostVulkanTimelineSemaphore>>` from \
                 `host_inner()` which borrows host-private state and cannot \
                 cross the plugin ABI."
            );
        }
        self.host_inner().video_source_timeline_semaphore()
    }

    /// Cdylib-safe sibling of
    /// [`Self::video_source_timeline_semaphore`]. Dispatches through
    /// the v14
    /// [`GpuContextLimitedAccessVTable::host_video_source_timeline_arc`](streamlib_plugin_abi::GpuContextLimitedAccessVTable::host_video_source_timeline_arc)
    /// slot in cdylib mode; in host mode the same vtable dispatch
    /// resolves to the local `HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`
    /// static. Returns `None` when no producer has published a
    /// timeline yet (the slot is empty), when the slot was cleared
    /// via [`Self::clear_video_source_timeline_semaphore`], or when
    /// the handle/vtable is null.
    ///
    /// **Arc-raw-pointer transit** — same rustc-version coupling
    /// caveat as
    /// [`Self::set_video_source_timeline_semaphore`].
    /// `HostVulkanTimelineSemaphore` is not `#[repr(C)]`; in-tree
    /// workspace plugin cdylibs share the host's rustc + dep graph
    /// and ride this freely.
    #[cfg(target_os = "linux")]
    pub fn host_video_source_timeline_arc(
        &self,
    ) -> Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        if self.handle.is_null() || self.vtable.is_null() {
            return None;
        }
        // SAFETY: handle + vtable were paired at construction. The
        // host's callback either returns null (slot empty / null
        // handle) or `Arc::into_raw` on a freshly cloned
        // `Arc<HostVulkanTimelineSemaphore>` (fresh strong count
        // moves into the cdylib).
        let raw = unsafe { ((*self.vtable).host_video_source_timeline_arc)(self.handle) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: matched with the host's `Arc::into_raw` above.
        let arc =
            unsafe { Arc::from_raw(raw as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore) };
        Some(arc)
    }

    /// Acquire a pooled texture from a pre-reserved pool (Split: fast path).
    ///
    /// `VK_IMAGE_TILING_OPTIMAL`, in-process use only. For cross-process
    /// render targets, see [`GpuContextFullAccess::acquire_render_target_dma_buf_image`]
    /// (Linux) — Sandbox callers don't have a render-target alloc path
    /// because allocating a new RT-capable image is a privileged op
    /// that goes through escalate.
    ///
    /// Dispatches through the plugin ABI vtable's `acquire_texture`
    /// callback. The descriptor's `label` field is currently dropped
    /// on the wire (debugging-only, never load-bearing).
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "acquire_texture: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_pooled: std::mem::MaybeUninit<PooledTextureHandle> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable were paired at construction. `out_pooled`
        // points at uninitialized stack storage; the host writes a valid
        // PooledTextureHandle on success.
        let status = unsafe {
            ((*self.vtable).acquire_texture)(
                self.handle,
                desc.width,
                desc.height,
                desc.format as u32,
                desc.usage.bits(),
                out_pooled.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid PooledTextureHandle.
            Ok(unsafe { out_pooled.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Copy a host-visible pixel buffer's contents into a pre-allocated
    /// device-local texture (e.g. a [`TextureRing`](crate::core::context::TextureRing)
    /// slot the caller already owns).
    ///
    /// Sandbox-safe: no allocation, no descriptor / pipeline construction,
    /// just a `vkCmdCopyBufferToImage` queue submit on the shared queue.
    /// See [`GpuContext::copy_pixel_buffer_to_texture`] for the full
    /// contract.
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `copy_pixel_buffer_to_texture` callback.
    #[cfg(target_os = "linux")]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "copy_pixel_buffer_to_texture: GpuContextLimitedAccess has null handle/vtable"
                    .into(),
            ));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.vtable).copy_pixel_buffer_to_texture)(
                self.handle,
                pixel_buffer as *const PixelBuffer as *const std::ffi::c_void,
                texture as *const Texture as *const std::ffi::c_void,
                surface_id.as_ptr(),
                surface_id.len(),
                width,
                height,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// See [`GpuContext::unregister_texture`].
    ///
    /// Dispatches through the plugin ABI vtable's `unregister_texture`
    /// callback.
    pub fn unregister_texture(&self, id: &str) {
        if self.handle.is_null() || self.vtable.is_null() {
            return;
        }
        // SAFETY: handle + vtable were paired at construction.
        unsafe {
            ((*self.vtable).unregister_texture)(self.handle, id.as_ptr(), id.len());
        }
    }

    /// Get the shared command queue.
    ///
    /// Submitting recorded command buffers from `process()` is safe: the
    /// images/buffers a Sandbox caller can construct are pool-backed and
    /// pre-reserved. See design doc §8 Q5.
    ///
    /// Dispatches through the plugin ABI vtable's `command_queue`
    /// callback. Returns an owned [`RhiCommandQueue`] PluginAbiObject with the
    /// host's `Arc<RhiCommandQueueInner>` refcount bumped.
    pub fn command_queue(&self) -> RhiCommandQueue {
        if self.handle.is_null() || self.vtable.is_null() {
            // Construct a null-handle PluginAbiObject that's safe to Drop
            // (Drop short-circuits on null). Caller's subsequent
            // method calls on the queue will fail cleanly.
            return RhiCommandQueue {
                handle: std::ptr::null(),
                vtable: std::ptr::null(),
            };
        }
        let mut out_q: std::mem::MaybeUninit<RhiCommandQueue> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable were paired at construction. The
        // host writes a valid RhiCommandQueue into `out_q` on success.
        // On failure we still produce a null-handle PluginAbiObject so the
        // method's signature stays infallible.
        let status = unsafe {
            ((*self.vtable).command_queue)(
                self.handle,
                out_q.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            // SAFETY: host signaled success and wrote a valid value.
            unsafe { out_q.assume_init() }
        } else {
            RhiCommandQueue {
                handle: std::ptr::null(),
                vtable: std::ptr::null(),
            }
        }
    }

    /// Create a CPU-side command buffer from the shared queue.
    ///
    /// Dispatches through the plugin ABI vtable's
    /// `create_command_buffer` callback.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "create_command_buffer: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_cb: std::mem::MaybeUninit<CommandBuffer> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.vtable).create_command_buffer)(
                self.handle,
                out_cb.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_cb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Copy pixels between same-format, same-size buffers (Split: cache hit).
    ///
    /// Dispatches through the plugin ABI vtable's `blit_copy` callback.
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "blit_copy: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.vtable).blit_copy)(
                self.handle,
                src as *const PixelBuffer as *const std::ffi::c_void,
                dest as *const PixelBuffer as *const std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Copy from raw IOSurface to a pixel buffer (Split: cache hit).
    ///
    /// # Safety
    /// - `src` must be a valid IOSurfaceRef pointer
    /// - The IOSurface must remain valid for the duration of the blit
    ///
    /// Dispatches through the plugin ABI vtable's `blit_copy_iosurface`
    /// callback. macOS-only; non-macOS hosts return an error.
    #[cfg(target_os = "macos")]
    pub unsafe fn blit_copy_iosurface(
        &self,
        src: crate::apple::corevideo_ffi::IOSurfaceRef,
        dest: &PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "blit_copy_iosurface: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        // SAFETY: see the method-level safety note.
        let status = unsafe {
            ((*self.vtable).blit_copy_iosurface)(
                self.handle,
                src as *const std::ffi::c_void,
                dest as *const PixelBuffer as *const std::ffi::c_void,
                width,
                height,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Get the surface store, if initialized.
    ///
    /// Dispatches through the plugin ABI vtable's `surface_store`
    /// callback. Returns `Some(SurfaceStore)` (PluginAbiObject, refcount
    /// bumped) when the host has one, else `None`. The PluginAbiObject's
    /// own Clone/Drop dispatch through the
    /// [`streamlib_plugin_abi::SurfaceStoreVTable`] reached via
    /// [`HostServices::surface_store_vtable`].
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        if self.handle.is_null() || self.vtable.is_null() {
            return None;
        }
        let mut out_store: std::mem::MaybeUninit<SurfaceStore> = std::mem::MaybeUninit::uninit();
        // SAFETY: handle + vtable were paired at construction. The
        // callback always writes a SurfaceStore — either a real
        // PluginAbiObject (Some) or a null-handle PluginAbiObject (None sentinel).
        unsafe {
            ((*self.vtable).surface_store)(
                self.handle,
                out_store.as_mut_ptr() as *mut std::ffi::c_void,
            );
        }
        // SAFETY: the callback wrote either a real PluginAbiObject or a
        // null-handle PluginAbiObject; either way `out_store` is initialized.
        let store = unsafe { out_store.assume_init() };
        if store.is_none() {
            // Null-handle PluginAbiObject — Drop is a no-op (short-circuits
            // on null), so we can safely drop here without affecting
            // any Arc refcount.
            drop(store);
            None
        } else {
            Some(store)
        }
    }

    /// Check out a surface by ID (Split: cache hit).
    ///
    /// Dispatches through the plugin ABI vtable's `check_out_surface`
    /// callback.
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "check_out_surface: GpuContextLimitedAccess has null handle/vtable".into(),
            ));
        }
        let mut out_pb: std::mem::MaybeUninit<PixelBuffer> = std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 512];
        let mut err_len: usize = 0;
        let status = unsafe {
            ((*self.vtable).check_out_surface)(
                self.handle,
                surface_id.as_ptr(),
                surface_id.len(),
                out_pb.as_mut_ptr() as *mut std::ffi::c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(unsafe { out_pb.assume_init() })
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned();
            Err(Error::GpuError(msg))
        }
    }
}

impl GpuContextFullAccess {
    /// Wait for the GPU device to become idle.
    ///
    /// Mode-routed; see [`Self::create_compute_kernel`] for the
    /// dispatch contract.
    pub fn wait_device_idle(&self) -> Result<()> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().wait_device_idle(),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "wait_device_idle: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).wait_device_idle)(
                        self.handle,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    Ok(())
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Acquire a pixel buffer from the shared pool.
    ///
    /// LimitedAccess mirror — cdylib dispatch inherits the C1-proven
    /// `acquire_pixel_buffer` slot via [`Self::inherited_limited_unchecked`].
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PixelBufferPoolId, PixelBuffer)> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .acquire_pixel_buffer(width, height, format),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .acquire_pixel_buffer(width, height, format),
        }
    }

    /// Acquire a HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    /// See [`GpuContext::acquire_storage_buffer`].
    #[cfg(target_os = "linux")]
    pub fn acquire_storage_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().acquire_storage_buffer(byte_size),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .acquire_storage_buffer(byte_size),
        }
    }

    /// Acquire a HOST_VISIBLE uniform buffer.
    #[cfg(target_os = "linux")]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().acquire_uniform_buffer(byte_size),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .acquire_uniform_buffer(byte_size),
        }
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    #[cfg(target_os = "linux")]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().acquire_vertex_buffer(byte_size),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .acquire_vertex_buffer(byte_size),
        }
    }

    /// Acquire a HOST_VISIBLE index buffer.
    #[cfg(target_os = "linux")]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().acquire_index_buffer(byte_size),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .acquire_index_buffer(byte_size),
        }
    }

    /// Allocate a render-target-capable DMA-BUF VkImage (privileged path —
    /// host-only adapter primitive, customers never see this directly).
    /// See [`GpuContext::acquire_render_target_dma_buf_image`].
    ///
    /// Mode-routed:
    /// - [`HandleKind::Boxed`] (in-process): direct dispatch via
    ///   [`Self::host_inner`].
    /// - [`HandleKind::ScopeToken`] (cdylib): dispatch through the
    ///   [`GpuContextFullAccessVTable`](streamlib_plugin_abi::GpuContextFullAccessVTable)'s
    ///   `acquire_render_target_dma_buf_image` slot, which validates the
    ///   scope token via [`super::escalate_scope_registry::with_scope`]
    ///   before calling the host's privileged surface path.
    #[cfg(target_os = "linux")]
    pub fn acquire_render_target_dma_buf_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<Texture> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .acquire_render_target_dma_buf_image(width, height, format),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "acquire_render_target_dma_buf_image: GpuContextFullAccess has null vtable"
                            .into(),
                    ));
                }
                let mut out_texture: std::mem::MaybeUninit<Texture> =
                    std::mem::MaybeUninit::uninit();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                // SAFETY: vtable + handle (scope_token) were paired at
                // construction; the host writes a valid Texture into
                // `out_texture` on success.
                let status = unsafe {
                    ((*self.vtable).acquire_render_target_dma_buf_image)(
                        self.handle,
                        width,
                        height,
                        format as u32,
                        out_texture.as_mut_ptr() as *mut std::ffi::c_void,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    // SAFETY: host signaled success and wrote a valid value.
                    Ok(unsafe { out_texture.assume_init() })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Get a pixel buffer by its pool id.
    pub fn get_pixel_buffer(&self, pool_id: &PixelBufferPoolId) -> Result<PixelBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().get_pixel_buffer(pool_id),
            HandleKind::ScopeToken => self.inherited_limited_unchecked().get_pixel_buffer(pool_id),
        }
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .resolve_pixel_buffer_by_surface_id(surface_id),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .resolve_pixel_buffer_by_surface_id(surface_id),
        }
    }

    /// Register a texture in the same-process texture cache.
    pub fn register_texture(&self, id: &str, texture: Texture) {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().register_texture(id, texture),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .register_texture(id, texture),
        }
    }

    /// Register a texture with a declared initial Vulkan image layout.
    /// See [`GpuContext::register_texture_with_layout`].
    #[cfg(target_os = "linux")]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        match self.handle_kind {
            HandleKind::Boxed => {
                self.host_inner()
                    .register_texture_with_layout(id, texture, initial_layout)
            }
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .register_texture_with_layout(id, texture, initial_layout),
        }
    }

    /// Update a registered texture's tracked layout after a transition.
    /// See [`GpuContext::update_texture_registration_layout`].
    #[cfg(target_os = "linux")]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .update_texture_registration_layout(id, layout),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .update_texture_registration_layout(id, layout),
        }
    }

    /// Resolve a VideoFrame's full registration record (texture + layout).
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<TextureRegistration> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .resolve_texture_registration_by_surface_id(
                    surface_id,
                    texture_layout,
                    width,
                    height,
                ),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .resolve_texture_registration_by_surface_id(
                    surface_id,
                    texture_layout,
                    width,
                    height,
                ),
        }
    }

    /// Resolve a VideoFrame's texture.
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().resolve_texture_by_surface_id(
                surface_id,
                texture_layout,
                width,
                height,
            ),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .resolve_texture_by_surface_id(surface_id, texture_layout, width, height),
        }
    }

    /// Acquire a new output texture with a UUID and register it in the cache.
    pub fn acquire_output_texture(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<(String, Texture)> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .acquire_output_texture(width, height, format),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "acquire_output_texture: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut id_buf = [0u8; 1024];
                let mut id_len: usize = 0;
                let mut out_texture: std::mem::MaybeUninit<Texture> =
                    std::mem::MaybeUninit::uninit();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).acquire_output_texture)(
                        self.handle,
                        width,
                        height,
                        format as u32,
                        id_buf.as_mut_ptr(),
                        id_buf.len(),
                        &mut id_len as *mut usize,
                        out_texture.as_mut_ptr() as *mut std::ffi::c_void,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    let id = match std::str::from_utf8(&id_buf[..id_len]) {
                        Ok(s) => s.to_string(),
                        Err(e) => {
                            return Err(Error::GpuError(format!(
                                "acquire_output_texture: surface id not UTF-8: {e}"
                            )));
                        }
                    };
                    // SAFETY: host signaled success and wrote a Texture.
                    let texture = unsafe { out_texture.assume_init() };
                    Ok((id, texture))
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Upload a pixel buffer's contents to a GPU texture and register it.
    #[cfg(target_os = "linux")]
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().upload_pixel_buffer_as_texture(
                surface_id,
                pixel_buffer,
                width,
                height,
            ),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "upload_pixel_buffer_as_texture: GpuContextFullAccess has null vtable"
                            .into(),
                    ));
                }
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).upload_pixel_buffer_as_texture)(
                        self.handle,
                        surface_id.as_ptr(),
                        surface_id.len(),
                        pixel_buffer as *const PixelBuffer as *const std::ffi::c_void,
                        width,
                        height,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    Ok(())
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Copy a host-visible pixel buffer's contents into an *already-allocated*
    /// device-local texture.
    ///
    /// See [`GpuContext::copy_pixel_buffer_to_texture`] for the
    /// underlying contract; the same primitive is exposed on
    /// [`GpuContextLimitedAccess`] for hot-path callers that already
    /// hold a texture (e.g. from a [`TextureRing`](crate::core::context::TextureRing)
    /// slot).
    #[cfg(target_os = "linux")]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().copy_pixel_buffer_to_texture(
                pixel_buffer,
                texture,
                surface_id,
                width,
                height,
            ),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .copy_pixel_buffer_to_texture(pixel_buffer, texture, surface_id, width, height),
        }
    }

    /// Pre-allocate a ring of `count` non-exportable DEVICE_LOCAL
    /// textures and register each in the same-process texture cache.
    ///
    /// The returned [`crate::core::context::TextureRing`] is the
    /// canonical engine helper for decode-output hot paths — replaces
    /// every per-frame `upload_pixel_buffer_as_texture` escalation
    /// with a one-shot setup-time allocation plus a sandbox-safe
    /// rotation in `process()`. See `docs/architecture/texture-ring.md`
    /// for the recipe and CLAUDE.md → "Texture rings — single
    /// canonical abstraction" for the engine-model context.
    ///
    /// `count` is rejected if zero; sizing to
    /// `MAX_FRAMES_IN_FLIGHT = 2`
    /// (`docs/learnings/vulkan-frames-in-flight.md`) is the standard
    /// for hot-path decoders.
    #[cfg(target_os = "linux")]
    pub fn create_texture_ring(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
        usages: TextureUsages,
        count: usize,
    ) -> Result<crate::core::context::TextureRing> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .create_texture_ring(width, height, format, usages, count),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_texture_ring: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_ring: *const std::ffi::c_void = std::ptr::null();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).create_texture_ring)(
                        self.handle,
                        width,
                        height,
                        format as u32,
                        usages.bits(),
                        count,
                        &mut out_ring,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_ring.is_null() {
                        return Err(Error::GpuError(
                            "create_texture_ring: host signaled success but out_ring is null"
                                .into(),
                        ));
                    }
                    // PluginAbiObject: bundle the raw handle
                    // (`Arc::into_raw(Arc<TextureRingInner>)`-shaped)
                    // with the host vtables + cached POD descriptors.
                    // The cached values come from the caller's own
                    // inputs (we know `width` / `height` / `format` /
                    // `count` — these are the args we just passed
                    // through the plugin ABI), avoiding an extra round-trip
                    // for the getters. Cross-rustc-version safe because
                    // cdylib never derefs the Inner layout.
                    let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                        .map(|c| c.texture_ring_methods_vtable)
                        .unwrap_or(std::ptr::null());
                    Ok(crate::core::context::TextureRing {
                        handle: out_ring,
                        vtable: self.vtable,
                        methods_vtable,
                        cached_len: count as u32,
                        cached_width: width,
                        cached_height: height,
                        cached_format: format as u32,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Create a single-in-flight GPU→CPU texture readback bound to a
    /// fixed format/extent and return it as the layout-stable
    /// [`crate::core::rhi::TextureReadback`] PluginAbiObject. The staging
    /// buffer + command resources + timeline semaphore are allocated
    /// once at construction and reused across every submit; for parallel
    /// readbacks, hold N handles. Planar `Nv12` is rejected (the readback
    /// staging model assumes a flat interleaved plane).
    #[cfg(target_os = "linux")]
    pub fn create_texture_readback(
        &self,
        label: &str,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<crate::core::rhi::TextureReadback> {
        match self.handle_kind {
            HandleKind::Boxed => {
                if matches!(format, TextureFormat::Nv12) {
                    return Err(Error::GpuError(
                        "create_texture_readback: planar Nv12 is not supported \
                         (readback assumes a flat interleaved plane)"
                            .into(),
                    ));
                }
                let descriptor = crate::core::rhi::TextureReadbackDescriptor {
                    label,
                    format,
                    width,
                    height,
                };
                let arc = self.host_inner().create_texture_readback(&descriptor)?;
                // Cached POD sourced from the primitive itself — never
                // recomputed here.
                let cached_handle_id = arc.handle_id();
                let cached_staging_size = arc.staging_size();
                // Box-shaped opaque handle: `Box<Arc<VulkanTextureReadback>>`.
                let handle = Box::into_raw(Box::new(arc)) as *const std::ffi::c_void;
                let vtable =
                    crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
                let methods_vtable =
                    crate::core::plugin::host_services::host_vulkan_texture_readback_methods_vtable(
                    );
                Ok(crate::core::rhi::TextureReadback {
                    handle,
                    vtable,
                    methods_vtable,
                    cached_handle_id,
                    cached_staging_size,
                    cached_width: width,
                    cached_height: height,
                    cached_format_raw: format as u32,
                    _reserved_padding: 0,
                })
            }
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_texture_readback: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_readback: *const std::ffi::c_void = std::ptr::null();
                let mut out_handle_id: u64 = 0;
                let mut out_staging_size: u64 = 0;
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                // SAFETY: vtable + handle (scope token) paired at construction.
                let status = unsafe {
                    ((*self.vtable).create_texture_readback)(
                        self.handle,
                        label.as_ptr(),
                        label.len(),
                        width,
                        height,
                        format as u32,
                        &mut out_readback,
                        &mut out_handle_id as *mut u64,
                        &mut out_staging_size as *mut u64,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status != 0 {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    return Err(Error::GpuError(msg));
                }
                if out_readback.is_null() {
                    return Err(Error::GpuError(
                        "create_texture_readback: host signaled success but out handle is null"
                            .into(),
                    ));
                }
                let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                    .map(|c| c.vulkan_texture_readback_methods_vtable)
                    .unwrap_or(std::ptr::null());
                Ok(crate::core::rhi::TextureReadback {
                    handle: out_readback,
                    vtable: self.vtable,
                    methods_vtable,
                    cached_handle_id: out_handle_id,
                    cached_staging_size: out_staging_size,
                    cached_width: width,
                    cached_height: height,
                    cached_format_raw: format as u32,
                    _reserved_padding: 0,
                })
            }
        }
    }

    /// See [`GpuContext::unregister_texture`].
    pub fn unregister_texture(&self, id: &str) {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().unregister_texture(id),
            HandleKind::ScopeToken => {
                self.inherited_limited_unchecked().unregister_texture(id);
            }
        }
    }

    /// See [`GpuContext::set_video_source_timeline_semaphore`].
    ///
    /// **Engine-only** — parameter is `&Arc<HostVulkanTimelineSemaphore>`
    /// (host-internal type from `crate::vulkan::rhi`). Calling from a
    /// cdylib panics inside [`Self::host_inner`]; the panic is caught by
    /// `run_host_extern_c` at the plugin ABI, so it surfaces as a
    /// clean "callback panicked" log rather than UB.
    #[cfg(target_os = "linux")]
    pub fn set_video_source_timeline_semaphore(
        &self,
        timeline: &Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::set_video_source_timeline_semaphore(): \
                 parameter `&Arc<HostVulkanTimelineSemaphore>` is host-internal \
                 (crate::vulkan::rhi) and cannot cross the plugin ABI; this \
                 method is engine-only and cdylib code must not call it."
            );
        }
        self.host_inner()
            .set_video_source_timeline_semaphore(timeline);
    }

    /// See [`GpuContext::clear_video_source_timeline_semaphore`].
    ///
    /// **Engine-only** — pairs with
    /// [`Self::set_video_source_timeline_semaphore`]; that method is
    /// engine-only, so this one is too. Calling from a cdylib panics
    /// at the explicit guard below.
    #[cfg(target_os = "linux")]
    pub fn clear_video_source_timeline_semaphore(&self) {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::clear_video_source_timeline_semaphore(): \
                 pairs with set_video_source_timeline_semaphore which takes a \
                 host-internal `&Arc<HostVulkanTimelineSemaphore>`; engine-only \
                 by inheritance — cdylib code must not call it."
            );
        }
        self.host_inner().clear_video_source_timeline_semaphore();
    }

    /// See [`GpuContext::video_source_timeline_semaphore`].
    ///
    /// **Engine-only** — return type is
    /// `Option<Arc<HostVulkanTimelineSemaphore>>` (host-internal type).
    /// Calling from a cdylib panics at the explicit guard below.
    #[cfg(target_os = "linux")]
    pub fn video_source_timeline_semaphore(
        &self,
    ) -> Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::video_source_timeline_semaphore(): \
                 return type `Option<Arc<HostVulkanTimelineSemaphore>>` is \
                 host-internal (crate::vulkan::rhi) and cannot cross the plugin ABI \
                 boundary; engine-only — cdylib code must not call it."
            );
        }
        self.host_inner().video_source_timeline_semaphore()
    }

    /// Get a reference to the RHI GPU device.
    ///
    /// **Engine-only** — returns `&Arc<GpuDevice>` which borrows into
    /// host-private state (the `Box<Arc<GpuContext>>` behind the
    /// handle). The borrow can't cross the plugin ABI; cdylib code
    /// that needs GPU device capabilities should use the higher-level
    /// FullAccess methods (kernel construction, buffer/texture
    /// allocation, etc.) which dispatch through the vtable. Calling
    /// from a cdylib panics at the explicit guard below.
    pub fn device(&self) -> &Arc<GpuDevice> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::device(): return type `&Arc<GpuDevice>` \
                 borrows into host-private state and cannot cross the plugin ABI \
                 boundary; engine-only. Cdylib code that needs GPU device \
                 capabilities must use higher-level FullAccess methods (kernel \
                 construction, buffer/texture allocation) which dispatch \
                 through the FullAccess vtable. To construct a host-flavor \
                 surface adapter from an in-process workspace plugin cdylib \
                 use `host_vulkan_device_arc()` instead."
            );
        }
        self.host_inner().device()
    }

    /// Clone the host's `Arc<HostVulkanDevice>` for in-process workspace
    /// plugin cdylibs that need to construct a host-flavor
    /// `XxxSurfaceAdapter<HostVulkanDevice>` (e.g. #1004 dlopen smoke
    /// fixtures for the surface adapters). Dispatches through the v9
    /// `host_vulkan_device_arc` FullAccess vtable slot in cdylib mode;
    /// in host mode reaches `host_inner().device().vulkan_device()`
    /// directly. The returned Arc's strong count is incremented; the
    /// caller's `Drop` decrements the host's count.
    ///
    /// **Rustc-version coupling.** `HostVulkanDevice` is not
    /// `#[repr(C)]` — the plugin ABI Arc transit is safe only when the
    /// cdylib shares the host's rustc version and the engine's dep
    /// graph (workspace plugin cdylibs do; subprocess cdylibs
    /// — `streamlib-python-native`, `streamlib-deno-native` — don't dep
    /// on `streamlib-engine` and can't import `HostVulkanDevice`, so
    /// they can't reach this method at all).
    #[cfg(target_os = "linux")]
    pub fn host_vulkan_device_arc(&self) -> Result<Arc<crate::vulkan::rhi::HostVulkanDevice>> {
        match self.handle_kind {
            HandleKind::Boxed => Ok(Arc::clone(
                crate::host_rhi::HostGpuDeviceExt::vulkan_device(
                    self.host_inner().device().as_ref(),
                ),
            )),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "host_vulkan_device_arc: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let raw = unsafe { ((*self.vtable).host_vulkan_device_arc)(self.handle) };
                if raw.is_null() {
                    return Err(Error::GpuError(
                        "host_vulkan_device_arc: host returned null pointer (likely \
                         null/stale scope token or host-side panic)"
                            .into(),
                    ));
                }
                // SAFETY: host's wrapper called `Arc::into_raw` on a freshly
                // cloned `Arc<HostVulkanDevice>` and the cdylib shares the
                // host's rustc version + dep graph (workspace plugin cdylib
                // contract documented on the method).
                let arc =
                    unsafe { Arc::from_raw(raw as *const crate::vulkan::rhi::HostVulkanDevice) };
                Ok(arc)
            }
        }
    }

    /// Get the texture pool for acquiring pooled textures.
    ///
    /// **Engine-only** — returns `&TexturePool` which borrows into
    /// host-private state. Cdylib code uses [`Self::acquire_texture`]
    /// instead. Calling from a cdylib panics at the explicit guard below.
    pub fn texture_pool(&self) -> &TexturePool {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::texture_pool(): return type \
                 `&TexturePool` borrows into host-private state and cannot \
                 cross the plugin ABI; engine-only. Cdylib code uses \
                 acquire_texture() which dispatches through the FullAccess \
                 vtable."
            );
        }
        self.host_inner().texture_pool()
    }

    /// Acquire a pooled texture for in-process GPU work
    /// (`VK_IMAGE_TILING_OPTIMAL`). For cross-process render targets the
    /// host adapter layer wants on Linux, see
    /// [`Self::acquire_render_target_dma_buf_image`].
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().acquire_texture(desc),
            HandleKind::ScopeToken => self.inherited_limited_unchecked().acquire_texture(desc),
        }
    }

    /// Get the shared command queue.
    ///
    /// Phase D adopts the owned PluginAbiObject return that matches
    /// [`GpuContextLimitedAccess::command_queue`] — borrowed
    /// references can't cross the plugin ABI, so a cdylib-callable
    /// `command_queue` must hand out a refcount-bumped owned
    /// [`RhiCommandQueue`] regardless of mode. The PluginAbiObject's Drop
    /// dispatches through the LimitedAccess vtable's
    /// `drop_rhi_command_queue` callback.
    pub fn command_queue(&self) -> RhiCommandQueue {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().command_queue().clone(),
            HandleKind::ScopeToken => self.inherited_limited_unchecked().command_queue(),
        }
    }

    /// Create a command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_command_buffer(),
            HandleKind::ScopeToken => self.inherited_limited_unchecked().create_command_buffer(),
        }
    }

    /// Acquire a cached `(src, dst)`-keyed color converter. See
    /// [`GpuContext::color_converter`](crate::core::context::GpuContext::color_converter)
    /// on the inner context for usage.
    #[cfg(target_os = "linux")]
    pub fn color_converter(&self, src: PixelFormat, dst: PixelFormat) -> Result<RhiColorConverter> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().color_converter(src, dst),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "color_converter: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_converter: *const std::ffi::c_void = std::ptr::null();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).color_converter)(
                        self.handle,
                        src as u32,
                        dst as u32,
                        &mut out_converter,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_converter.is_null() {
                        return Err(Error::GpuError(
                            "color_converter: host signaled success but out_converter is null"
                                .into(),
                        ));
                    }
                    // PluginAbiObject: bundle the raw handle with the parent
                    // vtable + per-type methods vtable (Phase E sub-
                    // lift slice A). The methods vtable comes from
                    // `host_callbacks()` — populated at plugin
                    // install time alongside the parent vtable.
                    let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                        .map(|c| c.rhi_color_converter_methods_vtable)
                        .unwrap_or(std::ptr::null());
                    Ok(RhiColorConverter {
                        handle: out_converter,
                        vtable: self.vtable,
                        methods_vtable,
                        cached_src_format_raw: src as u32,
                        cached_dst_format_raw: dst as u32,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Create a compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Mode-routed: in-process callers use [`Self::host_inner`]; cdylib
    /// callers dispatch through the FullAccess vtable's
    /// `create_compute_kernel` slot, which validates the scope token and
    /// runs the host's [`GpuContext::create_compute_kernel`]. The host
    /// returns the kernel as `Arc::into_raw`; this wrapper reconstructs
    /// it via `Arc::from_raw` under the rustc-version coupling contract
    /// (CLAUDE.md "Cross-cutting decisions") that keeps layouts byte-
    /// identical between host and cdylib.
    #[cfg(target_os = "linux")]
    pub fn create_compute_kernel(
        &self,
        descriptor: &crate::core::rhi::ComputeKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanComputeKernel> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_compute_kernel(descriptor),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_compute_kernel: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                // Stage the descriptor into its repr + backing
                // bindings_buf; the backing Vec must stay alive for the
                // vtable call because the repr's bindings_ptr borrows
                // into it.
                let (repr, _bindings_buf) =
                    crate::core::rhi::plugin_abi_bridge::stage_compute_kernel_descriptor(
                        descriptor,
                    );
                let mut out_kernel: *const std::ffi::c_void = std::ptr::null();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).create_compute_kernel)(
                        self.handle,
                        &repr,
                        &mut out_kernel,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_kernel.is_null() {
                        return Err(Error::GpuError(
                            "create_compute_kernel: host signaled success but out_kernel is null"
                                .into(),
                        ));
                    }
                    // PluginAbiObject: bundle the raw handle (an
                    // `Arc::into_raw(Arc<VulkanComputeKernelInner>)`
                    // pointer host-side, opaque to the cdylib) with
                    // the host's parent vtable + per-type methods
                    // vtable + cached `push_constant_size` POD
                    // (#907 PR 2/5). The cached value comes from the
                    // descriptor input the cdylib just handed across
                    // — we know it without needing a plugin ABI round-trip
                    // to read it back.
                    let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                        .map(|c| c.vulkan_compute_kernel_methods_vtable)
                        .unwrap_or(std::ptr::null());
                    Ok(crate::vulkan::rhi::VulkanComputeKernel {
                        handle: out_kernel,
                        vtable: self.vtable,
                        methods_vtable,
                        cached_push_constant_size: descriptor.push_constant_size,
                        _reserved_padding: 0,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Create a Vulkan video session — the privileged
    /// `VkVideoSessionKHR` + bound device memory the codec layer
    /// uses for `vkCmdDecodeVideoKHR` / `vkCmdEncodeVideoKHR`.
    ///
    /// FullAccess-only and host-only: subprocess cdylibs do not
    /// build their own codec layers — codec packages live inside
    /// the host engine. The `ScopeToken` branch returns an explicit
    /// error rather than silently falling through.
    #[cfg(target_os = "linux")]
    pub fn create_video_session(
        &self,
        descriptor: &crate::vulkan::rhi::VideoSessionDescriptor<'_>,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::HostVulkanVideoSession>> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_video_session(descriptor),
            HandleKind::ScopeToken => Err(Error::GpuError(
                "create_video_session: video session creation is host-only; \
                 subprocess customers consume codec output through the \
                 surface-share registry, not by constructing sessions"
                    .into(),
            )),
        }
    }

    /// Create Vulkan video session parameters parented to `session`.
    /// Companion to [`Self::create_video_session`]; same FullAccess +
    /// host-only privilege story.
    #[cfg(target_os = "linux")]
    pub fn create_video_session_parameters(
        &self,
        session: &std::sync::Arc<crate::vulkan::rhi::HostVulkanVideoSession>,
        descriptor: &crate::vulkan::rhi::VideoSessionParametersDescriptor<'_>,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::HostVulkanVideoSessionParameters>> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .create_video_session_parameters(session, descriptor),
            HandleKind::ScopeToken => Err(Error::GpuError(
                "create_video_session_parameters: video session parameter \
                 creation is host-only; subprocess customers consume codec \
                 output through the surface-share registry"
                    .into(),
            )),
        }
    }

    /// Allocate a video DPB (Decoded Picture Buffer) image bound to a
    /// codec profile — the engine-RHI primitive the codec layer uses
    /// for reference-picture and decode-target images.
    ///
    /// FullAccess-only and host-only: codec packages live inside the
    /// host engine, so subprocess cdylibs do not construct DPB images
    /// directly — they consume codec output through the surface-share
    /// registry. The `ScopeToken` branch returns an explicit error.
    #[cfg(target_os = "linux")]
    pub fn create_video_dpb_texture(
        &self,
        descriptor: &crate::vulkan::rhi::VideoDpbTextureDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanTexture> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_video_dpb_texture(descriptor),
            HandleKind::ScopeToken => Err(Error::GpuError(
                "create_video_dpb_texture: DPB image creation is host-only; \
                 subprocess customers consume codec output through the \
                 surface-share registry, not by constructing DPB images"
                    .into(),
            )),
        }
    }

    /// Allocate a video bitstream buffer bound to a codec profile —
    /// the HOST_VISIBLE engine-RHI primitive the codec layer uses for
    /// the encoder's output NAL bytes (and the decoder's input
    /// bytes). Same FullAccess + host-only privilege story as
    /// [`Self::create_video_dpb_texture`].
    #[cfg(target_os = "linux")]
    pub fn create_video_bitstream_buffer(
        &self,
        descriptor: &crate::vulkan::rhi::VideoBitstreamBufferDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_video_bitstream_buffer(descriptor),
            HandleKind::ScopeToken => Err(Error::GpuError(
                "create_video_bitstream_buffer: bitstream buffer creation \
                 is host-only; subprocess customers consume codec output \
                 through the surface-share registry, not by constructing \
                 bitstream buffers"
                    .into(),
            )),
        }
    }

    /// Allocate a Vulkan query pool — the generic engine-RHI primitive
    /// servicing every query class (timestamp, occlusion,
    /// pipeline-statistics, video-encode-feedback). FullAccess-only;
    /// subprocess cdylibs do not construct query pools — they consume
    /// codec results (when applicable) through the surface-share /
    /// escalate IPC channels, not by reaching into pool primitives.
    #[cfg(target_os = "linux")]
    pub fn create_query_pool(
        &self,
        descriptor: &crate::vulkan::rhi::QueryPoolDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanQueryPool> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_query_pool(descriptor),
            HandleKind::ScopeToken => Err(Error::GpuError(
                "create_query_pool: query pool creation is host-only; \
                 subprocess customers don't reach the GPU query API \
                 surface directly"
                    .into(),
            )),
        }
    }

    /// Build an engine-owned multi-step command-buffer recorder. See
    /// [`GpuContext::create_command_recorder`](crate::core::context::GpuContext::create_command_recorder)
    /// for the per-frame usage protocol.
    ///
    /// FullAccess-only because the recorder dispatches
    /// [`VulkanComputeKernel`](crate::vulkan::rhi::VulkanComputeKernel),
    /// which itself is FullAccess-only (privileged pipeline
    /// construction is excluded from the consumer-rhi carve-out). Subprocess
    /// consumers that need cross-process recording must escalate
    /// dispatch through the escalate IPC.
    #[cfg(target_os = "linux")]
    pub fn create_command_recorder(
        &self,
        label: &str,
    ) -> Result<crate::vulkan::rhi::RhiCommandRecorder> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_command_recorder(label),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_command_recorder: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_recorder: std::mem::MaybeUninit<
                    crate::vulkan::rhi::RhiCommandRecorder,
                > = std::mem::MaybeUninit::uninit();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).create_command_recorder)(
                        self.handle,
                        label.as_ptr(),
                        label.len(),
                        out_recorder.as_mut_ptr() as *mut std::ffi::c_void,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    // SAFETY: host signaled success and wrote the
                    // RhiCommandRecorder by value via std::ptr::write.
                    // Layout is byte-identical by `#[repr(C)]`
                    // invariant. The host's `from_inner` populated
                    // both `vtable` and `methods_vtable` (Phase E
                    // sub-lift slice B — #984) with host-static
                    // addresses; cdylib dispatch through those
                    // pointers resolves to host-resident functions
                    // in the shared process address space.
                    Ok(unsafe { out_recorder.assume_init() })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Create a graphics kernel from a multi-stage SPIR-V set, binding
    /// declaration, and fixed-function pipeline state.
    ///
    /// Mode-routed; see [`Self::create_compute_kernel`] for the
    /// dispatch contract.
    #[cfg(target_os = "linux")]
    pub fn create_graphics_kernel(
        &self,
        descriptor: &crate::core::rhi::GraphicsKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanGraphicsKernel> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_graphics_kernel(descriptor),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_graphics_kernel: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let (repr, _stage) =
                    crate::core::rhi::plugin_abi_bridge::stage_graphics_kernel_descriptor(
                        descriptor,
                    );
                let mut out_kernel: *const std::ffi::c_void = std::ptr::null();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).create_graphics_kernel)(
                        self.handle,
                        &repr,
                        &mut out_kernel,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_kernel.is_null() {
                        return Err(Error::GpuError(
                            "create_graphics_kernel: host signaled success but out_kernel is null"
                                .into(),
                        ));
                    }
                    // PluginAbiObject: see compute_kernel above. Cached PODs
                    // come from the caller's descriptor — we know
                    // them without a plugin ABI round-trip (#907 PR 3/5).
                    let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                        .map(|c| c.vulkan_graphics_kernel_methods_vtable)
                        .unwrap_or(std::ptr::null());
                    Ok(crate::vulkan::rhi::VulkanGraphicsKernel {
                        handle: out_kernel,
                        vtable: self.vtable,
                        methods_vtable,
                        cached_push_constant_size: descriptor.push_constants.size,
                        cached_descriptor_sets_in_flight: descriptor.descriptor_sets_in_flight,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Create a ray-tracing kernel from shader stages, shader-group
    /// layout, binding declaration, and push-constant range.
    ///
    /// Mode-routed; see [`Self::create_compute_kernel`] for the
    /// dispatch contract.
    #[cfg(target_os = "linux")]
    pub fn create_ray_tracing_kernel(
        &self,
        descriptor: &crate::core::rhi::RayTracingKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanRayTracingKernel> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_ray_tracing_kernel(descriptor),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_ray_tracing_kernel: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let (repr, _stage) =
                    crate::core::rhi::plugin_abi_bridge::stage_ray_tracing_kernel_descriptor(
                        descriptor,
                    );
                let mut out_kernel: *const std::ffi::c_void = std::ptr::null();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).create_ray_tracing_kernel)(
                        self.handle,
                        &repr,
                        &mut out_kernel,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_kernel.is_null() {
                        return Err(Error::GpuError(
                            "create_ray_tracing_kernel: host signaled success but out_kernel is null".into(),
                        ));
                    }
                    // PluginAbiObject: see compute_kernel above. Cached PODs
                    // come from the caller's descriptor (#907 PR 4/5).
                    let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                        .map(|c| c.vulkan_ray_tracing_kernel_methods_vtable)
                        .unwrap_or(std::ptr::null());
                    Ok(crate::vulkan::rhi::VulkanRayTracingKernel {
                        handle: out_kernel,
                        vtable: self.vtable,
                        methods_vtable,
                        cached_push_constant_size: descriptor.push_constants.size,
                        _reserved_padding: 0,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Build a triangle-geometry bottom-level acceleration structure
    /// from CPU-side vertex + index data.
    #[cfg(target_os = "linux")]
    pub fn build_triangles_blas(
        &self,
        label: &str,
        vertices: &[f32],
        indices: &[u32],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .build_triangles_blas(label, vertices, indices),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "build_triangles_blas: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_blas: *const std::ffi::c_void = std::ptr::null();
                let mut out_device_address: u64 = 0;
                let mut out_storage_size: u64 = 0;
                let mut out_kind: u32 = 0;
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).build_triangles_blas)(
                        self.handle,
                        label.as_ptr(),
                        label.len(),
                        vertices.as_ptr(),
                        vertices.len(),
                        indices.as_ptr(),
                        indices.len(),
                        &mut out_blas,
                        &mut out_device_address as *mut u64,
                        &mut out_storage_size as *mut u64,
                        &mut out_kind as *mut u32,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_blas.is_null() {
                        return Err(Error::GpuError(
                            "build_triangles_blas: host signaled success but out_blas is null"
                                .into(),
                        ));
                    }
                    // PluginAbiObject: bundle the raw handle (`Arc::into_raw(Arc<Inner>)`-shaped)
                    // with the host vtables. The cached POD descriptors
                    // (`device_address`, `storage_size`, `kind`) come
                    // from the host's PluginAbiObject post-mint (see
                    // `host_gpu_full_build_triangles_blas`); they are
                    // always real values, never placeholder zeros.
                    let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                        .map(|c| c.vulkan_acceleration_structure_methods_vtable)
                        .unwrap_or(std::ptr::null());
                    Ok(crate::vulkan::rhi::VulkanAccelerationStructure {
                        handle: out_blas,
                        vtable: self.vtable,
                        methods_vtable,
                        cached_kind: out_kind,
                        _reserved_padding: 0,
                        cached_device_address: out_device_address,
                        cached_storage_size: out_storage_size,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Build a top-level acceleration structure from BLAS instances.
    #[cfg(target_os = "linux")]
    pub fn build_tlas(
        &self,
        label: &str,
        instances: &[crate::vulkan::rhi::TlasInstanceDesc],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().build_tlas(label, instances),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "build_tlas: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_tlas: *const std::ffi::c_void = std::ptr::null();
                let mut out_device_address: u64 = 0;
                let mut out_storage_size: u64 = 0;
                let mut out_kind: u32 = 0;
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).build_tlas)(
                        self.handle,
                        label.as_ptr(),
                        label.len(),
                        instances.as_ptr() as *const std::ffi::c_void,
                        instances.len(),
                        &mut out_tlas,
                        &mut out_device_address as *mut u64,
                        &mut out_storage_size as *mut u64,
                        &mut out_kind as *mut u32,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_tlas.is_null() {
                        return Err(Error::GpuError(
                            "build_tlas: host signaled success but out_tlas is null".into(),
                        ));
                    }
                    // PluginAbiObject: see build_triangles_blas above. Cached
                    // PODs come from the host's PluginAbiObject post-mint via
                    // the v8 out-params; always real values.
                    let methods_vtable = crate::core::plugin::host_services::host_callbacks()
                        .map(|c| c.vulkan_acceleration_structure_methods_vtable)
                        .unwrap_or(std::ptr::null());
                    Ok(crate::vulkan::rhi::VulkanAccelerationStructure {
                        handle: out_tlas,
                        vtable: self.vtable,
                        methods_vtable,
                        cached_kind: out_kind,
                        _reserved_padding: 0,
                        cached_device_address: out_device_address,
                        cached_storage_size: out_storage_size,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Whether the underlying GPU exposes the
    /// `VK_KHR_ray_tracing_pipeline` extension chain.
    #[cfg(target_os = "linux")]
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().supports_ray_tracing_pipeline(),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return false;
                }
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let rc = unsafe {
                    ((*self.vtable).supports_ray_tracing_pipeline)(
                        self.handle,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                // 1 = true, 0 = false, -1 = error (treat as false).
                rc == 1
            }
        }
    }

    /// Import a DMA-BUF FD as a `StorageBuffer` (PluginAbiObject). Camera
    /// V4L2 zero-copy path. **Consumes `fd` on success** — on success
    /// the host's `vkImportMemoryFdInfoKHR` takes ownership of the
    /// kernel-side fd transfer; on failure the caller retains the fd
    /// and must close it.
    #[cfg(target_os = "linux")]
    pub fn import_dma_buf_storage_buffer(
        &self,
        fd: std::os::unix::io::RawFd,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .import_dma_buf_storage_buffer(fd, byte_size),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "import_dma_buf_storage_buffer: GpuContextFullAccess has null vtable"
                            .into(),
                    ));
                }
                let mut out_buffer: std::mem::MaybeUninit<crate::core::rhi::StorageBuffer> =
                    std::mem::MaybeUninit::uninit();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).import_dma_buf_storage_buffer)(
                        self.handle,
                        fd,
                        byte_size,
                        out_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    // SAFETY: host signaled success and wrote the
                    // StorageBuffer PluginAbiObject struct into the slot.
                    Ok(unsafe { out_buffer.assume_init() })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Allocate an OPAQUE_FD-exportable `VkBuffer` as a `StorageBuffer`
    /// (`device_local` picks VRAM-resident vs HOST_VISIBLE). The
    /// cdylib-safe OPAQUE_FD/CUDA producer allocation (#1262). Mode-routed:
    /// host-mode via `host_inner()`, cdylib-mode via the
    /// `create_opaque_fd_export_buffer` slot.
    #[cfg(target_os = "linux")]
    pub fn create_opaque_fd_export_buffer(
        &self,
        byte_size: u64,
        device_local: bool,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self
                .host_inner()
                .create_opaque_fd_export_buffer(byte_size, device_local),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_opaque_fd_export_buffer: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_buffer: std::mem::MaybeUninit<crate::core::rhi::StorageBuffer> =
                    std::mem::MaybeUninit::uninit();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).create_opaque_fd_export_buffer)(
                        self.handle,
                        byte_size,
                        u8::from(device_local),
                        out_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    // SAFETY: host signaled success and wrote the
                    // StorageBuffer PluginAbiObject into the slot.
                    Ok(unsafe { out_buffer.assume_init() })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Export a fresh dup'd OPAQUE_FD + byte size + exporting-device UUID
    /// from a `StorageBuffer`. The fd transfers to the caller. Mode-routed:
    /// host-mode via `host_inner()`, cdylib-mode via the
    /// `export_storage_buffer_opaque_fd` slot (decoding the
    /// [`OpaqueFdExportDescriptorRepr`](streamlib_plugin_abi::OpaqueFdExportDescriptorRepr)).
    #[cfg(target_os = "linux")]
    pub fn export_storage_buffer_opaque_fd(
        &self,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().export_storage_buffer_opaque_fd(buffer),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "export_storage_buffer_opaque_fd: GpuContextFullAccess has null vtable"
                            .into(),
                    ));
                }
                let mut descriptor = streamlib_plugin_abi::OpaqueFdExportDescriptorRepr::default();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).export_storage_buffer_opaque_fd)(
                        self.handle,
                        buffer as *const _ as *const std::ffi::c_void,
                        &mut descriptor,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    Ok((descriptor.fd, descriptor.size, descriptor.device_uuid))
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Wrap an OPAQUE_FD `StorageBuffer` as a `PixelBuffer` sharing the
    /// same allocation so it can register through the surface-store
    /// `register_pixel_buffer_with_timeline` path (#1262). Mode-routed:
    /// host-mode via `host_inner()`, cdylib-mode via the
    /// `wrap_storage_buffer_as_pixel_buffer` slot.
    #[cfg(target_os = "linux")]
    pub fn wrap_storage_buffer_as_pixel_buffer(
        &self,
        storage_buffer: &crate::core::rhi::StorageBuffer,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: crate::core::rhi::PixelFormat,
    ) -> Result<crate::core::rhi::PixelBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().wrap_storage_buffer_as_pixel_buffer(
                storage_buffer,
                width,
                height,
                bytes_per_pixel,
                format,
            ),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "wrap_storage_buffer_as_pixel_buffer: GpuContextFullAccess has null vtable"
                            .into(),
                    ));
                }
                let mut out_pixel_buffer: std::mem::MaybeUninit<crate::core::rhi::PixelBuffer> =
                    std::mem::MaybeUninit::uninit();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).wrap_storage_buffer_as_pixel_buffer)(
                        self.handle,
                        storage_buffer as *const _ as *const std::ffi::c_void,
                        width,
                        height,
                        bytes_per_pixel,
                        format as u32,
                        out_pixel_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    // SAFETY: host signaled success and wrote the PixelBuffer
                    // PluginAbiObject into the slot.
                    Ok(unsafe { out_pixel_buffer.assume_init() })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Per-frame CUDA producer copy: image→buffer in one host-device
    /// submission with optional `consume_done` wait + `produce_done`
    /// signal (#1262). Mode-routed: host-mode via `host_inner()`,
    /// cdylib-mode via the `copy_texture_to_storage_buffer_and_signal`
    /// slot (marshalling the texture PluginAbiObject handle + timeline
    /// inner-Arc pointers).
    #[cfg(target_os = "linux")]
    pub fn copy_texture_to_storage_buffer_and_signal(
        &self,
        source_texture: &crate::core::rhi::Texture,
        source_layout: crate::core::rhi::VulkanLayout,
        dst: &crate::core::rhi::StorageBuffer,
        consume_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
        produce_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
    ) -> Result<()> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().copy_texture_to_storage_buffer_and_signal(
                source_texture,
                source_layout,
                dst,
                consume_done,
                produce_done,
            ),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "copy_texture_to_storage_buffer_and_signal: GpuContextFullAccess has null vtable"
                            .into(),
                    ));
                }
                let (consume_handle, consume_value) = match consume_done {
                    Some((sem, value)) => {
                        (sem as *const _ as *const std::ffi::c_void, value)
                    }
                    None => (std::ptr::null(), 0),
                };
                let (produce_handle, produce_value) = match produce_done {
                    Some((sem, value)) => {
                        (sem as *const _ as *const std::ffi::c_void, value)
                    }
                    None => (std::ptr::null(), 0),
                };
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).copy_texture_to_storage_buffer_and_signal)(
                        self.handle,
                        source_texture.handle,
                        source_layout.0,
                        dst as *const _ as *const std::ffi::c_void,
                        consume_handle,
                        consume_value,
                        produce_handle,
                        produce_value,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    Ok(())
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Construct a timeline semaphore. Backs the camera processor's
    /// per-frame timeline that signals into downstream consumers
    /// (display, encoders). Mode-routed: host-mode dispatches through
    /// `host_inner()`; cdylib-mode dispatches through the vtable's
    /// `create_timeline_semaphore` slot and reconstructs the Arc via
    /// `Arc::from_raw`.
    ///
    /// **Note**: this slot transits `Arc<HostVulkanTimelineSemaphore>`
    /// via Arc-raw-pointer pattern — same hazard as the kernel paths
    /// pre-#917 Phase 8. Arc internals leak across the plugin ABI for now.
    /// In-tree consumers (camera, display) are built in the same
    /// workspace as engine; the layout matches by construction.
    /// Cross-repo plugin distribution will need a PluginAbiObject lift for
    /// `HostVulkanTimelineSemaphore` — tracked as a future follow-up.
    #[cfg(target_os = "linux")]
    pub fn create_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_timeline_semaphore(initial_value),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "create_timeline_semaphore: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_handle: *const std::ffi::c_void = std::ptr::null();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).create_timeline_semaphore)(
                        self.handle,
                        initial_value,
                        &mut out_handle,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_handle.is_null() {
                        return Err(Error::GpuError(
                            "create_timeline_semaphore: host signaled success but out_handle is null".into(),
                        ));
                    }
                    // SAFETY: host wrote
                    // `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)`.
                    let arc = unsafe {
                        Arc::from_raw(
                            out_handle as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore,
                        )
                    };
                    Ok(arc)
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Read-once GPU capability snapshot. Backs the camera processor's
    /// vendor-name / external-memory / cross-device-DMA-BUF-probe
    /// branching without exposing host-internal `HostVulkanDevice`
    /// across the plugin ABI. Mode-routed: host-mode dispatches through
    /// `host_inner()`; cdylib-mode reads a `#[repr(C)]`
    /// [`GpuCapabilitiesRepr`](streamlib_plugin_abi::GpuCapabilitiesRepr)
    /// via the vtable's `gpu_capabilities` slot and decodes the
    /// fixed-size device-name buffer into an owned `String`.
    #[cfg(target_os = "linux")]
    pub fn gpu_capabilities(&self) -> Result<GpuCapabilitiesSnapshot> {
        match self.handle_kind {
            HandleKind::Boxed => Ok(self.host_inner().gpu_capabilities()),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "gpu_capabilities: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                // Stack-allocate the plugin ABI repr; the host populates it
                // via *out_caps. We then decode into an owned snapshot.
                let mut out: std::mem::MaybeUninit<streamlib_plugin_abi::GpuCapabilitiesRepr> =
                    std::mem::MaybeUninit::uninit();
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).gpu_capabilities)(
                        self.handle,
                        out.as_mut_ptr(),
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    // SAFETY: host signaled success; the repr is now
                    // initialized.
                    let repr = unsafe { out.assume_init() };
                    let name_len = (repr.device_name_len as usize).min(repr.device_name.len());
                    let device_name =
                        String::from_utf8_lossy(&repr.device_name[..name_len]).into_owned();
                    Ok(GpuCapabilitiesSnapshot {
                        device_name,
                        supports_external_memory: repr.supports_external_memory != 0,
                        supports_cross_device_dma_buf_probe: repr
                            .supports_cross_device_dma_buf_probe
                            != 0,
                        supports_ray_tracing_pipeline: repr.supports_ray_tracing_pipeline != 0,
                    })
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Get the underlying Metal device (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_device(&self) -> &crate::metal::rhi::MetalDevice {
        self.host_inner().metal_device()
    }

    /// Create a texture cache for converting pixel buffers to texture views.
    #[cfg(target_os = "macos")]
    pub fn create_texture_cache(&self) -> Result<crate::core::rhi::RhiTextureCache> {
        self.host_inner().create_texture_cache()
    }

    /// Copy pixels between same-format, same-size buffers.
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().blit_copy(src, dest),
            HandleKind::ScopeToken => self.inherited_limited_unchecked().blit_copy(src, dest),
        }
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
        dest: &PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        unsafe {
            self.host_inner()
                .blit_copy_iosurface(src, dest, width, height)
        }
    }

    /// Clear the blitter's texture cache to free GPU memory.
    ///
    /// **Engine-only** — engine setup-time housekeeping; no cdylib
    /// path needs to invoke it. Calling from a cdylib panics at the
    /// explicit guard below.
    pub fn clear_blitter_cache(&self) {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::clear_blitter_cache(): engine setup-time \
                 housekeeping that operates on host-internal blitter cache; \
                 engine-only — cdylib code must not call it."
            );
        }
        self.host_inner().clear_blitter_cache();
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().surface_store(),
            HandleKind::ScopeToken => self.inherited_limited_unchecked().surface_store(),
        }
    }

    /// Check in a pixel buffer to the surface-share service.
    pub fn check_in_surface(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().check_in_surface(pixel_buffer),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "check_in_surface: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut id_buf = [0u8; 1024];
                let mut id_len: usize = 0;
                let mut err_buf = [0u8; 512];
                let mut err_len: usize = 0;
                let status = unsafe {
                    ((*self.vtable).check_in_surface)(
                        self.handle,
                        pixel_buffer as *const PixelBuffer as *const std::ffi::c_void,
                        id_buf.as_mut_ptr(),
                        id_buf.len(),
                        &mut id_len as *mut usize,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    match std::str::from_utf8(&id_buf[..id_len]) {
                        Ok(s) => Ok(s.to_string()),
                        Err(e) => Err(Error::GpuError(format!(
                            "check_in_surface: surface id not UTF-8: {e}"
                        ))),
                    }
                } else {
                    let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                        .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Check out a surface by ID.
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().check_out_surface(surface_id),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .check_out_surface(surface_id),
        }
    }

    /// Get the registered cpu-readback bridge, if any. Reachable only inside
    /// `escalate(|full| ...)` since it requires `FullAccess`.
    ///
    /// **Engine-only** — return type is `Option<Arc<dyn CpuReadbackBridge>>`,
    /// a trait object whose vtable layout is rustc-private (no `#[repr(C)]`
    /// shape that crosses the plugin ABI). The bridge is registered by
    /// host code via `set_cpu_readback_bridge` and read by host adapter
    /// machinery; cdylib code doesn't need to read it. Calling from a
    /// cdylib panics at the explicit guard below.
    #[cfg(target_os = "linux")]
    pub fn cpu_readback_bridge(&self) -> Option<Arc<dyn CpuReadbackBridge>> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::cpu_readback_bridge(): return type \
                 `Option<Arc<dyn CpuReadbackBridge>>` is a trait object whose \
                 vtable layout is rustc-private and cannot cross the plugin ABI \
                 boundary; engine-only — cdylib code must not call it."
            );
        }
        self.host_inner().cpu_readback_bridge()
    }

    /// Get the registered compute-kernel bridge, if any. Reachable only inside
    /// `escalate(|full| ...)` since it requires `FullAccess`.
    ///
    /// **Engine-only** — trait-object return; same rationale as
    /// [`Self::cpu_readback_bridge`].
    #[cfg(target_os = "linux")]
    pub fn compute_kernel_bridge(&self) -> Option<Arc<dyn ComputeKernelBridge>> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::compute_kernel_bridge(): return type \
                 `Option<Arc<dyn ComputeKernelBridge>>` is a trait object whose \
                 vtable layout is rustc-private and cannot cross the plugin ABI \
                 boundary; engine-only — cdylib code must not call it."
            );
        }
        self.host_inner().compute_kernel_bridge()
    }

    /// Get the registered graphics-kernel bridge, if any. Reachable only inside
    /// `escalate(|full| ...)` since it requires `FullAccess`.
    ///
    /// **Engine-only** — trait-object return; same rationale as
    /// [`Self::cpu_readback_bridge`].
    #[cfg(target_os = "linux")]
    pub fn graphics_kernel_bridge(&self) -> Option<Arc<dyn GraphicsKernelBridge>> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::graphics_kernel_bridge(): return type \
                 `Option<Arc<dyn GraphicsKernelBridge>>` is a trait object \
                 whose vtable layout is rustc-private and cannot cross the plugin ABI \
                 boundary; engine-only — cdylib code must not call it."
            );
        }
        self.host_inner().graphics_kernel_bridge()
    }

    /// Get the registered ray-tracing-kernel bridge, if any. Reachable only
    /// inside `escalate(|full| ...)` since it requires `FullAccess`.
    ///
    /// **Engine-only** — trait-object return; same rationale as
    /// [`Self::cpu_readback_bridge`].
    #[cfg(target_os = "linux")]
    pub fn ray_tracing_kernel_bridge(&self) -> Option<Arc<dyn RayTracingKernelBridge>> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::ray_tracing_kernel_bridge(): return type \
                 `Option<Arc<dyn RayTracingKernelBridge>>` is a trait object \
                 whose vtable layout is rustc-private and cannot cross the plugin ABI \
                 boundary; engine-only — cdylib code must not call it."
            );
        }
        self.host_inner().ray_tracing_kernel_bridge()
    }
}

impl std::fmt::Debug for GpuContextLimitedAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The underlying GpuContext is host-private and not safely
        // formattable from cdylib code; print the (handle, vtable)
        // shape instead.
        f.debug_struct("GpuContextLimitedAccess")
            .field("handle", &self.handle)
            .field("vtable", &self.vtable)
            .finish()
    }
}

impl std::fmt::Debug for GpuContextFullAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContextFullAccess")
            .field("handle", &self.handle)
            .field("vtable", &self.vtable)
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
        let texture = gpu
            .device()
            .create_texture(&desc)
            .expect("texture creation failed");
        let surface_id = "test-surface-001";

        gpu.register_texture(surface_id, texture.clone());

        let resolved = gpu
            .resolve_texture_by_surface_id(surface_id, None, 640, 480)
            .expect("texture cache miss");
        assert_eq!(resolved.width(), 640);
        assert_eq!(resolved.height(), 480);

        println!("Texture cache: register + resolve OK");
    }

    /// #1262 OPAQUE_FD/CUDA producer surface — positive mint/export/wrap
    /// path plus the zeroed-cached-fields regression.
    ///
    /// Mental-revert: if `create_opaque_fd_export_buffer` built the
    /// `StorageBuffer` with a zeroed `byte_size_cached` (the silent
    /// all-zero borrow hazard — `docs/learnings/cdylib-make-borrow-cached-fields.md`),
    /// the `byte_size() == BYTES` assertion below fails immediately —
    /// with no panic, no export error, just a wrong cached POD. GPU-gated:
    /// skips when no device is present (CI is GPU-free).
    #[test]
    #[cfg(target_os = "linux")]
    fn opaque_fd_export_buffer_mint_export_wrap_and_non_zero_cache() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        const W: u32 = 32;
        const H: u32 = 32;
        const BPP: u32 = 4;
        const BYTES: u64 = (W as u64) * (H as u64) * (BPP as u64);

        // HOST_VISIBLE OPAQUE_FD flavor is broadly available; the
        // DEVICE_LOCAL CUDA flavor rides the same code path with a
        // different pool. Skip if the OPAQUE_FD pool is unavailable on
        // this driver rather than failing the suite.
        let storage = match gpu.create_opaque_fd_export_buffer(BYTES, false) {
            Ok(s) => s,
            Err(e) => {
                println!("Skipping - OPAQUE_FD export pool unavailable: {e}");
                return;
            }
        };

        // Zeroed-cached-fields regression: the cached byte size must be
        // the real allocation size, never 0.
        assert_eq!(
            storage.byte_size(),
            BYTES,
            "StorageBuffer.byte_size_cached must carry the real allocation size (zeroed-cache regression)"
        );
        assert!(
            !storage.mapped_ptr().is_null(),
            "HOST_VISIBLE OPAQUE_FD buffer must expose a persistent mapping"
        );

        // Export → fresh dup'd fd + size + device UUID.
        let (fd, size, uuid) = gpu
            .export_storage_buffer_opaque_fd(&storage)
            .expect("export_storage_buffer_opaque_fd failed");
        assert!(fd >= 0, "exported OPAQUE_FD must be non-negative, got {fd}");
        assert_eq!(size, BYTES, "exported size must equal the allocation size");
        assert!(
            uuid.iter().any(|b| *b != 0),
            "device UUID must not be all-zero — CUDA device binding depends on it, got {uuid:02x?}"
        );
        // The caller owns the dup'd fd; close it so the test leaks nothing.
        unsafe { libc::close(fd) };

        // Wrap → PixelBuffer sharing the same allocation, with the
        // caller's pixel-shape metadata cached.
        let pixel_buffer = gpu
            .wrap_storage_buffer_as_pixel_buffer(&storage, W, H, BPP, crate::core::rhi::PixelFormat::Bgra32)
            .expect("wrap_storage_buffer_as_pixel_buffer failed");
        assert_eq!(pixel_buffer.width, W);
        assert_eq!(pixel_buffer.height, H);

        println!("OPAQUE_FD mint/export/wrap OK — byte_size={BYTES} uuid={uuid:02x?}");
    }

    /// #1262 followup #1 — the ONLY positive coverage of the batch's
    /// riskiest FullAccess slot, `copy_texture_to_storage_buffer_and_signal`.
    ///
    /// Drives the REAL host vtable body
    /// (`HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE`), not the plain
    /// `GpuContext` method — that body reconstructs a borrowed source
    /// `Texture` from a raw inner-Arc handle via
    /// `Arc::increment_strong_count` + `Texture::from_arc_into_raw`,
    /// records the image->buffer copy, and host-blocks on the
    /// null-timeline submit path. Two locks:
    ///   (a) the destination OPAQUE_FD `StorageBuffer` bytes equal the
    ///       known source contents after the copy;
    ///   (b) the source texture's inner-Arc strong count is identical
    ///       before and after the call — the
    ///       `increment_strong_count`/`from_arc_into_raw` borrow must be
    ///       balanced by exactly one `Texture::Drop`.
    ///
    /// Mental-revert: if the host body leaked one strong count (dropped
    /// the balancing `Texture::Drop`, or double-incremented), the
    /// strong-count equality assertion fails — a use-after-free / leak
    /// this test catches. GPU-gated: skips cleanly with no device (CI is
    /// GPU-free), so it does not run in the sandbox — it is a
    /// /verify-live regression.
    #[test]
    #[cfg(target_os = "linux")]
    fn copy_texture_to_storage_buffer_and_signal_positive_and_refcount_balance() {
        use std::ffi::c_void;
        use std::sync::Arc;

        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => Arc::new(g),
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        const W: u32 = 32;
        const H: u32 = 32;
        const BPP: u32 = 4;
        const BYTES: u64 = (W as u64) * (H as u64) * (BPP as u64);

        // HOST_VISIBLE OPAQUE_FD staging + destination buffers. Skip the
        // whole test if the pool is unavailable on this driver rather
        // than failing the suite (mirrors the sibling OPAQUE_FD
        // regression's skip-clean shape).
        let staging = match gpu.create_opaque_fd_export_buffer(BYTES, false) {
            Ok(s) => s,
            Err(e) => {
                println!("Skipping - OPAQUE_FD export pool unavailable: {e}");
                return;
            }
        };
        let dst = match gpu.create_opaque_fd_export_buffer(BYTES, false) {
            Ok(s) => s,
            Err(e) => {
                println!("Skipping - OPAQUE_FD export pool unavailable: {e}");
                return;
            }
        };

        // Known source pattern written into the host-visible staging map.
        let staging_ptr = staging.mapped_ptr();
        assert!(
            !staging_ptr.is_null(),
            "HOST_VISIBLE staging buffer must expose a mapping"
        );
        for i in 0..BYTES as usize {
            unsafe { *staging_ptr.add(i) = (i % 251) as u8 };
        }

        // Source texture, filled from the staging buffer and left in
        // GENERAL — a legal copy-source layout (the landed slot copies
        // directly from whatever `source_layout` the caller passes,
        // recording no transition).
        let desc = TextureDescriptor::new(W, H, TextureFormat::Rgba8Unorm).with_usage(
            TextureUsages::COPY_DST | TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING,
        );
        let source_texture = match gpu.device().create_texture_local(&desc) {
            Ok(t) => t,
            Err(e) => {
                println!("Skipping - source texture allocation failed: {e}");
                return;
            }
        };

        {
            use crate::vulkan::rhi::{VulkanAccess, VulkanStage};
            let mut fill = crate::vulkan::rhi::RhiCommandRecorderInner::new(
                &gpu.device().inner,
                "copy_slot_regression_fill_source",
            )
            .expect("fill recorder");
            fill.begin().expect("fill begin");
            fill.record_image_barrier(
                &source_texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanStage::TOP_OF_PIPE,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::NONE,
                VulkanAccess::TRANSFER_WRITE,
            )
            .expect("barrier UNDEFINED -> TRANSFER_DST");
            fill.record_copy_buffer_to_image(
                &staging,
                &source_texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                crate::vulkan::rhi::ImageCopyRegion::tightly_packed(W, H),
            )
            .expect("copy staging -> source image");
            fill.record_image_barrier(
                &source_texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanLayout::GENERAL,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::TRANSFER_READ,
            )
            .expect("barrier TRANSFER_DST -> GENERAL");
            fill.submit_and_wait().expect("fill submit_and_wait");
        }

        // Measure the source texture's inner-Arc strong count without
        // disturbing it (bump, reconstruct, read, drop — net zero). The
        // absolute value is immaterial; only before == after is asserted.
        let strong_count = |texture: &crate::core::rhi::Texture| -> usize {
            let ptr = texture.handle as *const crate::core::rhi::texture::TextureInner;
            unsafe {
                Arc::increment_strong_count(ptr);
                let arc = Arc::from_raw(ptr);
                let count = Arc::strong_count(&arc);
                drop(arc);
                count
            }
        };
        let count_before = strong_count(&source_texture);

        // Mint a full escalate scope token bound to this context, then
        // drive the REAL host vtable body so the dangerous
        // Arc-reconstruction path actually runs (the plain `GpuContext`
        // method borrows `&Texture` directly and would not exercise it).
        let token =
            crate::core::context::escalate_scope_registry::begin_escalate_scope(gpu.clone());

        let mut err_buf = vec![0u8; 512];
        let mut err_len: usize = 0;
        let rc = unsafe {
            (crate::core::plugin::host_services::HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE
                .copy_texture_to_storage_buffer_and_signal)(
                token as *const c_void,
                source_texture.handle,
                VulkanLayout::GENERAL.0,
                &dst as *const crate::core::rhi::StorageBuffer as *const c_void,
                std::ptr::null(), // consume_done: none -> host-blocking submit_and_wait
                0,
                std::ptr::null(), // produce_done: none
                0,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len,
            )
        };

        let count_after = strong_count(&source_texture);
        crate::core::context::escalate_scope_registry::end_escalate_scope(token);

        let err = String::from_utf8_lossy(&err_buf[..err_len]).into_owned();
        assert_eq!(
            rc, 0,
            "copy_texture_to_storage_buffer_and_signal returned error: {err}"
        );

        // (b) THE crucial lock: host-side Arc reconstruction refcount
        // balance. increment_strong_count + from_arc_into_raw must be
        // balanced by exactly one Texture::Drop.
        assert_eq!(
            count_before, count_after,
            "host-side Arc reconstruction (increment_strong_count + from_arc_into_raw) must be \
             balanced by exactly one Texture::Drop; a leak or double-decrement is a \
             use-after-free / leak bug"
        );

        // (a) destination bytes equal the known source contents.
        let dst_ptr = dst.mapped_ptr();
        assert!(
            !dst_ptr.is_null(),
            "HOST_VISIBLE destination buffer must expose a mapping"
        );
        for i in 0..BYTES as usize {
            let got = unsafe { *dst_ptr.add(i) };
            let want = (i % 251) as u8;
            assert_eq!(got, want, "destination byte {i} mismatch: got {got}, want {want}");
        }

        println!(
            "copy_texture_to_storage_buffer_and_signal OK — {BYTES} bytes copied, \
             strong_count balanced ({count_before} -> {count_after})"
        );
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

        let registration = gpu
            .resolve_texture_registration_by_surface_id(surface_id, None, 640, 480)
            .expect("registration cache miss");
        assert_eq!(
            registration.current_layout(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            "declared initial layout should be visible to consumers"
        );

        // Update flow — consumer barriers transition + advance layout.
        registration.update_layout(VulkanLayout::TRANSFER_SRC_OPTIMAL);
        let registration2 = gpu
            .resolve_texture_registration_by_surface_id(surface_id, None, 640, 480)
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
        let registration3 = gpu
            .resolve_texture_registration_by_surface_id(
                "test-surface-default-layout",
                None,
                640,
                480,
            )
            .expect("default-layout resolve");
        assert_eq!(
            registration3.current_layout(),
            VulkanLayout::UNDEFINED,
            "register_texture without explicit layout defaults to UNDEFINED"
        );

        println!("register_texture_with_layout + resolve_texture_registration_by_surface_id: OK");
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
        assert!(
            gpu.resolve_texture_by_surface_id("nonexistent-surface", None, 640, 480)
                .is_err()
        );

        // Timeline semaphore publication slot is shared across Clones.
        #[cfg(target_os = "linux")]
        {
            use crate::host_rhi::HostGpuDeviceExt;
            assert!(gpu.video_source_timeline_semaphore().is_none());

            let vk_device = gpu.device().vulkan_device();
            let timeline = Arc::new(
                crate::vulkan::rhi::HostVulkanTimelineSemaphore::new(vk_device.device(), 0)
                    .expect("create timeline semaphore"),
            );

            gpu.set_video_source_timeline_semaphore(&timeline);
            let snapshot1 = gpu.video_source_timeline_semaphore().expect("set");
            assert!(Arc::ptr_eq(&snapshot1, &timeline));

            let gpu2 = gpu.clone();
            let snapshot2 = gpu2.video_source_timeline_semaphore().expect("shared");
            assert!(Arc::ptr_eq(&snapshot2, &timeline));

            gpu2.clear_video_source_timeline_semaphore();
            assert!(gpu.video_source_timeline_semaphore().is_none());
        }

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

        // Limited-access delegates to the same underlying context.
        let limited = GpuContextLimitedAccess::new(gpu.clone());
        let full = limited.to_full_access();

        #[cfg(target_os = "linux")]
        {
            use crate::host_rhi::HostGpuDeviceExt;
            let vk_device = gpu.device().vulkan_device();
            let timeline_a = Arc::new(
                crate::vulkan::rhi::HostVulkanTimelineSemaphore::new(vk_device.device(), 0)
                    .expect("create timeline a"),
            );
            let timeline_b = Arc::new(
                crate::vulkan::rhi::HostVulkanTimelineSemaphore::new(vk_device.device(), 0)
                    .expect("create timeline b"),
            );

            limited.set_video_source_timeline_semaphore(&timeline_a);
            assert!(Arc::ptr_eq(
                &gpu.video_source_timeline_semaphore().expect("via gpu"),
                &timeline_a,
            ));
            assert!(Arc::ptr_eq(
                &limited
                    .video_source_timeline_semaphore()
                    .expect("via limited"),
                &timeline_a,
            ));

            // The full-access view shares the same publication slot.
            assert!(Arc::ptr_eq(
                &full.video_source_timeline_semaphore().expect("via full"),
                &timeline_a,
            ));
            full.set_video_source_timeline_semaphore(&timeline_b);
            assert!(Arc::ptr_eq(
                &limited
                    .video_source_timeline_semaphore()
                    .expect("limited sees b"),
                &timeline_b,
            ));

            // Conversion full -> limited round-trips.
            let limited2 = full.to_limited_access();
            assert!(Arc::ptr_eq(
                &limited2
                    .video_source_timeline_semaphore()
                    .expect("limited2 sees b"),
                &timeline_b,
            ));
            full.clear_video_source_timeline_semaphore();
        }

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
        let result: Result<()> =
            limited.escalate(|_full| Err(Error::Runtime("synthetic failure".to_string())));
        match result {
            Err(Error::Runtime(msg)) if msg == "synthetic failure" => {}
            other => panic!("expected synthetic Runtime error, got {other:?}"),
        }

        // Mutex must be released after the error — a second escalation should proceed.
        let after: Result<u32> = limited.escalate(|_full| Ok(7));
        assert_eq!(after.expect("escalate after error"), 7);

        println!("escalate propagates closure error + releases lock: OK");
    }

    /// In-process escalate panic recovery (#1006 scenario 2).
    ///
    /// `escalate_gate.enter_scoped()` is an RAII guard whose Drop runs
    /// even when the closure panics — `catch_unwind` at the test
    /// boundary catches the panic, and the subsequent `escalate` call
    /// must proceed (proves the gate was released by the Drop on the
    /// panicking closure's stack frame).
    ///
    /// Mental revert: changing `enter_scoped()`'s Drop to a no-op (or
    /// switching back to a manual lock/unlock pair without the RAII
    /// release) would leave the gate held forever; the post-panic
    /// `escalate` call would block until test timeout.
    #[test]
    fn test_escalate_releases_gate_on_panic() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        let limited = GpuContextLimitedAccess::new(gpu);

        // Inside `catch_unwind`, intentionally panic inside an
        // escalate closure.
        let unwind_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = limited.escalate(|_full| -> Result<()> {
                panic!("synthetic in-process escalate panic");
            });
        }));
        assert!(
            unwind_result.is_err(),
            "the catch_unwind block must observe the panic"
        );

        // The next escalate must succeed — proves the gate released
        // even though the closure unwound.
        let after: Result<u32> = limited.escalate(|_full| Ok(11));
        assert_eq!(
            after.expect("escalate after panic must succeed"),
            11,
            "escalate gate must release on panic via Drop"
        );

        println!("escalate releases gate on panic via RAII Drop: OK");
    }

    /// LimitedAccess + FullAccess interleaving (#1006 scenario 5).
    ///
    /// LimitedAccess ops route through the shared command-queue
    /// mutex; FullAccess escalates through the separate
    /// `EscalateGate`. Concurrent callers — thread A holds an
    /// escalate mid-closure, thread B issues an `acquire_pixel_buffer`
    /// Limited call — must both complete without deadlock.
    /// Documented model: the two locks are independent and Limited
    /// observes no partial-Full state.
    ///
    /// Mental revert: collapsing both locks into one would let
    /// thread B block on the escalate gate; the assertion that
    /// Limited completes before the escalate releases would fail.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — needs GPU + #[serial] discipline; \
                  exercises Limited+Full lock interleaving"
    )]
    #[test]
    fn test_limited_and_full_interleave_without_deadlock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        let limited = GpuContextLimitedAccess::new(gpu);
        let limited_a = limited.clone();
        let limited_b = limited.clone();

        let escalate_in_progress = Arc::new(AtomicBool::new(false));
        let escalate_in_progress_a = Arc::clone(&escalate_in_progress);
        let limited_observed_during_escalate = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&limited_observed_during_escalate);

        let thread_a = std::thread::spawn(move || {
            limited_a
                .escalate(|_full| -> Result<()> {
                    escalate_in_progress_a.store(true, Ordering::SeqCst);
                    // Hold the gate for ~200ms; thread B must complete
                    // its Limited acquire in this window.
                    std::thread::sleep(Duration::from_millis(200));
                    escalate_in_progress_a.store(false, Ordering::SeqCst);
                    Ok(())
                })
                .expect("escalate must succeed")
        });

        // Wait until thread A has entered the escalate closure.
        let start = std::time::Instant::now();
        while !escalate_in_progress.load(Ordering::SeqCst) {
            if start.elapsed() > Duration::from_secs(2) {
                panic!("thread A never entered the escalate closure within 2s");
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        // Thread B issues a Limited acquire while the escalate is
        // mid-closure. This must complete BEFORE thread A releases
        // the gate — proves the two locks are independent.
        let thread_b = std::thread::spawn(move || {
            use crate::core::rhi::PixelFormat;
            let result = limited_b.acquire_pixel_buffer(16, 16, PixelFormat::Rgba32);
            if result.is_ok() {
                observed.store(true, Ordering::SeqCst);
            }
            result.map(|_| ())
        });

        thread_b.join().expect("thread B panicked").ok();

        // Thread B's Limited op completed; thread A's escalate may
        // still be in flight. If interleaving works, observed=true.
        assert!(
            limited_observed_during_escalate.load(Ordering::SeqCst),
            "thread B's Limited acquire failed — independent locks regression?"
        );

        thread_a.join().expect("thread A panicked");

        println!("Limited + Full interleave without deadlock: OK");
    }

    /// Kernel drop past `escalate_end` (#1006 scenario 6).
    ///
    /// A kernel constructed inside `escalate(|full| ...)` and returned
    /// out of the closure must Drop cleanly after the scope ends. The
    /// kernel PluginAbiObject's Drop dispatches through its own per-vtable
    /// `drop_compute_kernel` callback — independent of any active
    /// escalate scope (the scope token only validates FullAccess CALL
    /// dispatch; drop is a refcount decrement on an opaque handle).
    ///
    /// Mental revert: wiring the drop to require a live escalate
    /// scope would crash here because the scope is closed before the
    /// drop runs.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — kernel construction needs GPU"
    )]
    #[test]
    fn test_kernel_drops_cleanly_after_escalate_end() {
        use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor};

        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        let limited = GpuContextLimitedAccess::new(gpu);

        // Trivial compute SPIR-V (just an entry point — the test
        // doesn't dispatch; it only verifies kernel construction +
        // drop semantics across the escalate boundary). If the
        // workspace's existing test fixtures already include a tiny
        // valid SPIR-V, prefer that; otherwise this test is gated by
        // the hardware feature flag and the engine's own
        // `create_compute_kernel` validates SPIR-V at construction.
        let trivial_spv: &[u8] = include_bytes!(concat!(
            env!("OUT_DIR"),
            "/color_convert_nv12_buffer_to_rgba.spv"
        ));
        let bindings: &[ComputeBindingSpec] = &[
            ComputeBindingSpec::storage_buffer(0),
            ComputeBindingSpec::storage_image(1),
        ];

        let kernel_arc = limited
            .escalate(|full| {
                full.create_compute_kernel(&ComputeKernelDescriptor {
                    label: "drop_post_escalate_smoke",
                    spv: trivial_spv,
                    bindings,
                    push_constant_size: 96,
                })
            })
            .expect("escalate must succeed");

        // Scope ended. Drop the kernel; this dispatches through
        // `drop_compute_kernel` on the parent FullAccess vtable. If
        // the drop path required a live scope, it would crash here.
        drop(kernel_arc);

        // A subsequent escalate must succeed too — proves the drop
        // didn't leave any locks held.
        let after: Result<u32> = limited.escalate(|_full| Ok(13));
        assert_eq!(after.expect("escalate after kernel drop must succeed"), 13,);

        println!("kernel drops cleanly after escalate_end: OK");
    }

    /// `GpuContextLimitedAccess::acquire_storage_buffer` reaches the
    /// shared inner context, allocates a HOST_VISIBLE storage buffer
    /// with the requested byte size, and hands back a `StorageBuffer`
    /// with a non-null mapped pointer. This exercises Sandbox-side
    /// reachability — the path subprocess Vulkan code rides after the
    /// camera carve-out (#673) lands. Returning `StorageBuffer` (not
    /// `PixelBuffer`) means consumers never see synthetic pixel
    /// dimensions on SSBOs.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn acquire_storage_buffer_via_limited_access() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        let limited = GpuContextLimitedAccess::new(gpu.clone());
        let byte_size: u64 = 1024 * 64;

        let buffer: crate::core::rhi::StorageBuffer = limited
            .acquire_storage_buffer(byte_size)
            .expect("Sandbox-side acquire_storage_buffer should succeed");

        // Public StorageBuffer surface: byte_size, mapped_ptr only —
        // no width/height/format getters to confuse SSBO consumers.
        assert_eq!(buffer.byte_size(), byte_size);
        assert!(
            !buffer.mapped_ptr().is_null(),
            "Sandbox-acquired SSBO must expose a non-null mapped pointer"
        );

        // FullAccess mirror also reaches the same inner context.
        let full = limited.to_full_access();
        let buffer2 = full
            .acquire_storage_buffer(byte_size)
            .expect("FullAccess mirror should succeed");
        assert_eq!(buffer2.byte_size(), byte_size);

        println!(
            "GpuContextLimitedAccess::acquire_storage_buffer: {} bytes; FullAccess mirror also OK",
            byte_size
        );
    }
}
