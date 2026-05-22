// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::context::TextureRegistration;
use crate::core::rhi::{
    CommandBuffer, GpuDevice, PixelBufferDescriptor, PixelBufferPoolId, PixelFormat, RhiBlitter,
    RhiColorConverter, RhiCommandQueue, PixelBuffer, RhiPixelBufferPool, Texture, TextureDescriptor,
    TextureFormat, TextureUsages,
};
use crate::core::{Result, Error};
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
use super::graphics_kernel_bridge::GraphicsKernelBridge;
#[cfg(target_os = "linux")]
use super::ray_tracing_kernel_bridge::RayTracingKernelBridge;
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
                        return Err(crate::core::Error::Configuration(
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
            return Err(Error::Configuration(
                "No buffers available in pool".into(),
            ));
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
/// [`streamlib_plugin_abi::GpuCapabilitiesRepr`] FFI struct (decoding
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
    color_converter_cache:
        Arc<RwLock<HashMap<(PixelFormat, PixelFormat), Arc<crate::core::rhi::RhiColorConverterInner>>>>,
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
    /// cdylib plugin callers using vtable FFI dispatch serialize
    /// against each other) so concurrent GPU resource creation (video
    /// sessions, DPB images, swapchain) can't race on the device. The
    /// compiler acquires this during Phase 4 of spawn_processor and
    /// releases it after waiting for the device to go idle. Replaces
    /// the older `std::sync::Mutex<()>`-based `processor_setup_lock`
    /// — a [`Mutex`] guard can't cross thread boundaries (the cdylib
    /// FFI escalate_begin / escalate_end pair may run on different
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
    /// release) and cdylib FFI-dispatched escalate (the plugin
    /// caller; uses bare `enter` / `exit` through
    /// [`super::escalate_scope_registry::begin_escalate_scope`] /
    /// [`super::escalate_scope_registry::end_escalate_scope`] because
    /// the FFI boundary precludes RAII across it).
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
            Error::Configuration(
                "SurfaceStore not initialized. Call runtime.start() first.".into(),
            )
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
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
        texture_layout: Option<i32>,
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
        width: u32,
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
        height: u32,
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
                    let resolved_layout = texture_layout
                        .map(VulkanLayout)
                        .unwrap_or(ipc_layout);
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
                let texture = self.refresh_pixel_buffer_texture(
                    surface_id,
                    &buffer,
                    width,
                    height,
                )?;
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
            let image = texture.vulkan_inner().image().ok_or_else(|| {
                Error::GpuError("Texture has no VkImage".into())
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
        pixel_buffer: &crate::core::rhi::PixelBuffer,
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
            let image = texture.vulkan_inner().image().ok_or_else(|| {
                crate::core::Error::GpuError("Texture has no VkImage".into())
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
            let image = texture.vulkan_inner().image().ok_or_else(|| {
                Error::GpuError("Texture has no VkImage".into())
            })?;
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
        Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(Arc::new(buffer)))
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
    pub fn acquire_vertex_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::VertexBuffer> {
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
    pub fn acquire_index_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::IndexBuffer> {
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
    pub fn color_converter(
        &self,
        src: PixelFormat,
        dst: PixelFormat,
    ) -> Result<RhiColorConverter> {
        // Fast path: read lock; cache stores Arc<Inner> so we can build
        // a fresh β-shape via from_arc_into_raw per request.
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
            slots.push(TextureRingSlot {
                surface_id,
                texture,
                slot_index,
            });
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
        crate::vulkan::rhi::VulkanAccelerationStructure::build_tlas(
            vulkan_device,
            label,
            instances,
        )
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
        let sem = crate::vulkan::rhi::HostVulkanTimelineSemaphore::new(
            device,
            initial_value,
        )?;
        Ok(Arc::new(sem))
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
            texture
                .vulkan_inner()
                .image()
                .ok_or_else(|| {
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
///
/// Restricted GPU capability shim with ABI-stable `(handle, vtable)`
/// shape. Both fields cross the cdylib DSO boundary unchanged:
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
    pub(crate) vtable:
        *const streamlib_plugin_abi::GpuContextLimitedAccessVTable,
}

// SAFETY: `handle` points at a host-owned `Box<Arc<GpuContext>>` that
// is `Send + Sync` (Arc carries atomic refcounts, GpuContext's
// fields are themselves Send + Sync via their Arc wrappers). The
// vtable pointer is `&'static` and pinned for the host's lifetime.
// Every method (engine and cdylib) reaches the GpuContext through
// the handle, gated on DSO mode by `host_inner()`'s `host_callbacks()`
// check.
unsafe impl Send for GpuContextLimitedAccess {}
unsafe impl Sync for GpuContextLimitedAccess {}

impl Clone for GpuContextLimitedAccess {
    /// Cross-DSO-safe Clone. Dispatches through
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
/// side of the FFI boundary the caller lives on; the same surface
/// methods work from either:
///
/// - **In-process dispatch** ([`Self::new`], `pub(in
///   crate::core::context)` so only
///   [`GpuContextLimitedAccess::escalate`]'s engine-internal body
///   can construct it). `handle` is a host-allocated
///   `Box<Arc<GpuContext>>` and `handle_kind` is
///   [`HandleKind::Boxed`]. Every method routes through
///   [`Self::host_inner`] for direct dispatch — no FFI hop, no
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
    pub(crate) vtable:
        *const streamlib_plugin_abi::GpuContextFullAccessVTable,
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
    pub(crate) inherited_lim_vtable:
        *const streamlib_plugin_abi::GpuContextLimitedAccessVTable,
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
    /// directly (no FFI hop). Used by engine-internal escalate
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
    ///   without going through the vtable. No FFI hop;
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
                let _ = unsafe {
                    Box::from_raw(
                        self.handle as *mut std::sync::Arc<GpuContext>,
                    )
                };
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
    /// (DSO-routed: host static in host mode, cdylib-installed
    /// pointer in cdylib mode).
    pub(crate) fn new(inner: GpuContext) -> Self {
        // Leak a fresh `Arc<GpuContext>` to back the opaque handle.
        // The handle is the sole owner; `host_inner()` derefs it for
        // engine callers, the vtable callbacks deref it for cdylib
        // callers.
        let arc: std::sync::Arc<GpuContext> = std::sync::Arc::new(inner);
        let boxed: Box<std::sync::Arc<GpuContext>> = Box::new(arc);
        let handle = Box::into_raw(boxed) as *const std::ffi::c_void;
        let vtable =
            crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
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
    /// The panic is caught by `run_host_extern_c` at the FFI
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
                 The panic is caught by run_host_extern_c at the FFI boundary."
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
    ///   [`GpuContextFullAccess::host_inner`] (no FFI hop), then
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
    /// no FFI hop. See [`Self::escalate`] for the mode router.
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
    /// FullAccess method calls cross the FFI boundary through the
    /// FullAccess vtable. See [`Self::escalate`] for the mode router.
    fn escalate_via_vtable<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        if self.handle.is_null() || self.vtable.is_null() {
            return Err(Error::GpuError(
                "escalate (vtable): GpuContextLimitedAccess has null handle/vtable"
                    .into(),
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
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
        let full = GpuContextFullAccess::from_scope_token(
            scope_token,
            self.handle,
            self.vtable,
        );
        // catch_unwind so a closure panic still fires escalate_end —
        // otherwise the host's escalate gate would leak. We lean on
        // AssertUnwindSafe because `escalate`'s public signature
        // doesn't add an `UnwindSafe` bound on `F` (the in-process
        // path doesn't need it; only this path catches the unwind to
        // pair with `escalate_end`).
        let closure_result = std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| f(&full)),
        );
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
            let msg = String::from_utf8_lossy(
                &end_err_buf[..end_err_len.min(end_err_buf.len())],
            )
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
        let vtable =
            crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
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
    /// for cross-DSO dispatch.
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
        inherited_lim_vtable:
            *const streamlib_plugin_abi::GpuContextLimitedAccessVTable,
    ) -> Self {
        let vtable =
            crate::core::plugin::host_services::host_gpu_context_full_access_vtable();
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
    /// instead. The panic is caught by `run_host_extern_c` at the FFI
    /// boundary.
    pub(crate) fn host_inner(&self) -> &GpuContext {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::host_inner() reached from cdylib code; \
                 this method must dispatch through the GpuContextFullAccessVTable. \
                 The panic is caught by run_host_extern_c at the FFI boundary."
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
    F: FnOnce(
        &streamlib_plugin_abi::GpuContextLimitedAccessVTable,
    ) -> AcquireBufferCallback,
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
        let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
            .into_owned();
        Err(Error::GpuError(msg))
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
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `acquire_pixel_buffer` callback. The tuple return is encoded
    /// via paired out-params: the pool id's string bytes land in a
    /// fixed-size stack buffer (1 KiB; UUID strings are well under
    /// 128 bytes), and the β-shape PixelBuffer goes into a
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
            let id_str = String::from_utf8_lossy(
                &pool_id_buf[..pool_id_len.min(pool_id_buf.len())],
            )
            .into_owned();
            let pool_id = PixelBufferPoolId::from_string(id_str);
            let pb = unsafe { out_pb.assume_init() };
            Ok((pool_id, pb))
        } else {
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Acquire a HOST_VISIBLE storage buffer for CPU→GPU SSBO upload.
    /// See [`GpuContext::acquire_storage_buffer`].
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `acquire_storage_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_storage_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        acquire_buffer_via_vtable(
            self,
            byte_size,
            "acquire_storage_buffer",
            |vt| vt.acquire_storage_buffer,
        )
    }

    /// Acquire a HOST_VISIBLE uniform buffer.
    /// See [`GpuContext::acquire_uniform_buffer`].
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `acquire_uniform_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        acquire_buffer_via_vtable(
            self,
            byte_size,
            "acquire_uniform_buffer",
            |vt| vt.acquire_uniform_buffer,
        )
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    /// See [`GpuContext::acquire_vertex_buffer`].
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `acquire_vertex_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_vertex_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::VertexBuffer> {
        acquire_buffer_via_vtable(
            self,
            byte_size,
            "acquire_vertex_buffer",
            |vt| vt.acquire_vertex_buffer,
        )
    }

    /// Acquire a HOST_VISIBLE index buffer.
    /// See [`GpuContext::acquire_index_buffer`].
    ///
    /// Dispatches through the cross-DSO vtable's
    /// `acquire_index_buffer` callback.
    #[cfg(target_os = "linux")]
    pub fn acquire_index_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::IndexBuffer> {
        acquire_buffer_via_vtable(
            self,
            byte_size,
            "acquire_index_buffer",
            |vt| vt.acquire_index_buffer,
        )
    }

    /// Get a pixel buffer by its pool id (Split: local cache).
    ///
    /// Dispatches through the cross-DSO vtable's `get_pixel_buffer`
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    ///
    /// Dispatches through the cross-DSO vtable's
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Register a texture in the same-process texture cache.
    ///
    /// Dispatches through the cross-DSO
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
    /// Dispatches through the cross-DSO vtable's `register_texture`
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
    /// Dispatches through the cross-DSO vtable's
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
    /// Dispatches through the cross-DSO vtable's
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Resolve a VideoFrame's texture (Split: cache hit).
    ///
    /// Dispatches through the cross-DSO vtable's
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
                "resolve_texture_by_surface_id: GpuContextLimitedAccess has null handle/vtable".into(),
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// See [`GpuContext::set_video_source_timeline_semaphore`].
    ///
    /// Engine-only call surface (the
    /// `&Arc<HostVulkanTimelineSemaphore>` parameter is host-internal
    /// — cdylib code cannot legitimately construct one), so this
    /// method reaches the inner GpuContext through `host_inner()`
    /// rather than the vtable. The `host_inner()` cdylib-mode panic
    /// guard short-circuits any misconfigured cdylib path.
    #[cfg(target_os = "linux")]
    pub fn set_video_source_timeline_semaphore(
        &self,
        timeline: &Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) {
        self.host_inner()
            .set_video_source_timeline_semaphore(timeline);
    }

    /// See [`GpuContext::clear_video_source_timeline_semaphore`].
    /// Engine-only call surface — see
    /// [`Self::set_video_source_timeline_semaphore`].
    #[cfg(target_os = "linux")]
    pub fn clear_video_source_timeline_semaphore(&self) {
        self.host_inner().clear_video_source_timeline_semaphore();
    }

    /// See [`GpuContext::video_source_timeline_semaphore`].
    /// Engine-only call surface — see
    /// [`Self::set_video_source_timeline_semaphore`].
    #[cfg(target_os = "linux")]
    pub fn video_source_timeline_semaphore(
        &self,
    ) -> Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        self.host_inner().video_source_timeline_semaphore()
    }

    /// Acquire a pooled texture from a pre-reserved pool (Split: fast path).
    ///
    /// `VK_IMAGE_TILING_OPTIMAL`, in-process use only. For cross-process
    /// render targets, see [`GpuContextFullAccess::acquire_render_target_dma_buf_image`]
    /// (Linux) — Sandbox callers don't have a render-target alloc path
    /// because allocating a new RT-capable image is a privileged op
    /// that goes through escalate.
    ///
    /// Dispatches through the cross-DSO vtable's `acquire_texture`
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
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
    /// Dispatches through the cross-DSO vtable's
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
                "copy_pixel_buffer_to_texture: GpuContextLimitedAccess has null handle/vtable".into(),
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// See [`GpuContext::unregister_texture`].
    ///
    /// Dispatches through the cross-DSO vtable's `unregister_texture`
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
    /// Dispatches through the cross-DSO vtable's `command_queue`
    /// callback. Returns an owned [`RhiCommandQueue`] β-shape with the
    /// host's `Arc<RhiCommandQueueInner>` refcount bumped.
    pub fn command_queue(&self) -> RhiCommandQueue {
        if self.handle.is_null() || self.vtable.is_null() {
            // Construct a null-handle β-shape that's safe to Drop
            // (Drop short-circuits on null). Caller's subsequent
            // method calls on the queue will fail cleanly.
            return RhiCommandQueue {
                handle: std::ptr::null(),
                vtable: std::ptr::null(),
            };
        }
        let mut out_q: std::mem::MaybeUninit<RhiCommandQueue> =
            std::mem::MaybeUninit::uninit();
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: handle + vtable were paired at construction. The
        // host writes a valid RhiCommandQueue into `out_q` on success.
        // On failure we still produce a null-handle β-shape so the
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
    /// Dispatches through the cross-DSO vtable's
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Copy pixels between same-format, same-size buffers (Split: cache hit).
    ///
    /// Dispatches through the cross-DSO vtable's `blit_copy` callback.
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Copy from raw IOSurface to a pixel buffer (Split: cache hit).
    ///
    /// # Safety
    /// - `src` must be a valid IOSurfaceRef pointer
    /// - The IOSurface must remain valid for the duration of the blit
    ///
    /// Dispatches through the cross-DSO vtable's `blit_copy_iosurface`
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
            Err(Error::GpuError(msg))
        }
    }

    /// Get the surface store, if initialized.
    ///
    /// Dispatches through the cross-DSO vtable's `surface_store`
    /// callback. Returns `Some(SurfaceStore)` (β-shape, refcount
    /// bumped) when the host has one, else `None`. The β-shape's
    /// own Clone/Drop dispatch through the
    /// [`streamlib_plugin_abi::SurfaceStoreVTable`] reached via
    /// [`HostServices::surface_store_vtable`].
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        if self.handle.is_null() || self.vtable.is_null() {
            return None;
        }
        let mut out_store: std::mem::MaybeUninit<SurfaceStore> =
            std::mem::MaybeUninit::uninit();
        // SAFETY: handle + vtable were paired at construction. The
        // callback always writes a SurfaceStore — either a real
        // β-shape (Some) or a null-handle β-shape (None sentinel).
        unsafe {
            ((*self.vtable).surface_store)(
                self.handle,
                out_store.as_mut_ptr() as *mut std::ffi::c_void,
            );
        }
        // SAFETY: the callback wrote either a real β-shape or a
        // null-handle β-shape; either way `out_store` is initialized.
        let store = unsafe { out_store.assume_init() };
        if store.is_none() {
            // Null-handle β-shape — Drop is a no-op (short-circuits
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
    /// Dispatches through the cross-DSO vtable's `check_out_surface`
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
            let msg = String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())])
                .into_owned();
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
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
            HandleKind::Boxed => {
                self.host_inner().acquire_pixel_buffer(width, height, format)
            }
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
    pub fn acquire_vertex_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::VertexBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().acquire_vertex_buffer(byte_size),
            HandleKind::ScopeToken => self
                .inherited_limited_unchecked()
                .acquire_vertex_buffer(byte_size),
        }
    }

    /// Acquire a HOST_VISIBLE index buffer.
    #[cfg(target_os = "linux")]
    pub fn acquire_index_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::IndexBuffer> {
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
                        "acquire_render_target_dma_buf_image: GpuContextFullAccess has null vtable".into(),
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
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
            HandleKind::ScopeToken => {
                self.inherited_limited_unchecked().get_pixel_buffer(pool_id)
            }
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
            HandleKind::Boxed => self
                .host_inner()
                .register_texture_with_layout(id, texture, initial_layout),
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
            HandleKind::Boxed => self.host_inner().resolve_texture_registration_by_surface_id(
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
                .resolve_texture_by_surface_id(
                    surface_id,
                    texture_layout,
                    width,
                    height,
                ),
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
            HandleKind::Boxed => {
                self.host_inner().acquire_output_texture(width, height, format)
            }
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
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
            HandleKind::Boxed => self
                .host_inner()
                .upload_pixel_buffer_as_texture(surface_id, pixel_buffer, width, height),
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "upload_pixel_buffer_as_texture: GpuContextFullAccess has null vtable".into(),
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
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
                .copy_pixel_buffer_to_texture(
                    pixel_buffer,
                    texture,
                    surface_id,
                    width,
                    height,
                ),
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
                    // β-shape: bundle the raw handle
                    // (`Arc::into_raw(Arc<TextureRingInner>)`-shaped) with
                    // the host vtable. Cross-rustc-version safe because
                    // cdylib never derefs the Inner layout.
                    Ok(crate::core::context::TextureRing {
                        handle: out_ring,
                        vtable: self.vtable,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
                    .into_owned();
                    Err(Error::GpuError(msg))
                }
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
    /// `run_host_extern_c` at the FFI boundary, so it surfaces as a
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
                 (crate::vulkan::rhi) and cannot cross the FFI boundary; this \
                 method is engine-only and cdylib code must not call it."
            );
        }
        self.host_inner().set_video_source_timeline_semaphore(timeline);
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
                 host-internal (crate::vulkan::rhi) and cannot cross the FFI \
                 boundary; engine-only — cdylib code must not call it."
            );
        }
        self.host_inner().video_source_timeline_semaphore()
    }

    /// Get a reference to the RHI GPU device.
    ///
    /// **Engine-only** — returns `&Arc<GpuDevice>` which borrows into
    /// host-private state (the `Box<Arc<GpuContext>>` behind the
    /// handle). The borrow can't cross the FFI boundary; cdylib code
    /// that needs GPU device capabilities should use the higher-level
    /// FullAccess methods (kernel construction, buffer/texture
    /// allocation, etc.) which dispatch through the vtable. Calling
    /// from a cdylib panics at the explicit guard below.
    pub fn device(&self) -> &Arc<GpuDevice> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::device(): return type `&Arc<GpuDevice>` \
                 borrows into host-private state and cannot cross the FFI \
                 boundary; engine-only. Cdylib code that needs GPU device \
                 capabilities must use higher-level FullAccess methods (kernel \
                 construction, buffer/texture allocation) which dispatch \
                 through the FullAccess vtable."
            );
        }
        self.host_inner().device()
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
                 cross the FFI boundary; engine-only. Cdylib code uses \
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
            HandleKind::ScopeToken => {
                self.inherited_limited_unchecked().acquire_texture(desc)
            }
        }
    }

    /// Get the shared command queue.
    ///
    /// Phase D adopts the owned β-shape return that matches
    /// [`GpuContextLimitedAccess::command_queue`] — borrowed
    /// references can't cross the FFI boundary, so a cdylib-callable
    /// `command_queue` must hand out a refcount-bumped owned
    /// [`RhiCommandQueue`] regardless of mode. The β-shape's Drop
    /// dispatches through the LimitedAccess vtable's
    /// `drop_rhi_command_queue` callback.
    pub fn command_queue(&self) -> RhiCommandQueue {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().command_queue().clone(),
            HandleKind::ScopeToken => {
                self.inherited_limited_unchecked().command_queue()
            }
        }
    }

    /// Create a command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        match self.handle_kind {
            HandleKind::Boxed => self.host_inner().create_command_buffer(),
            HandleKind::ScopeToken => {
                self.inherited_limited_unchecked().create_command_buffer()
            }
        }
    }

    /// Acquire a cached `(src, dst)`-keyed color converter. See
    /// [`GpuContext::color_converter`](crate::core::context::GpuContext::color_converter)
    /// on the inner context for usage.
    #[cfg(target_os = "linux")]
    pub fn color_converter(
        &self,
        src: PixelFormat,
        dst: PixelFormat,
    ) -> Result<RhiColorConverter> {
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
                            "color_converter: host signaled success but out_converter is null".into(),
                        ));
                    }
                    // β-shape: bundle the raw handle with the host vtable.
                    Ok(RhiColorConverter {
                        handle: out_converter,
                        vtable: self.vtable,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
                            "create_compute_kernel: host signaled success but out_kernel is null".into(),
                        ));
                    }
                    // β-shape: bundle the raw handle (an
                    // `Arc::into_raw(Arc<VulkanComputeKernelInner>)`
                    // pointer host-side, opaque to the cdylib) with the
                    // host vtable. Lifecycle dispatches through the
                    // vtable's clone/drop slots without ever crossing
                    // the host's `Arc<X>` allocation-header layout.
                    Ok(crate::vulkan::rhi::VulkanComputeKernel {
                        handle: out_kernel,
                        vtable: self.vtable,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
                    .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
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
                    // Layout is byte-identical under rustc-version
                    // coupling (CLAUDE.md).
                    Ok(unsafe { out_recorder.assume_init() })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
                            "create_graphics_kernel: host signaled success but out_kernel is null".into(),
                        ));
                    }
                    // β-shape: see compute_kernel above.
                    Ok(crate::vulkan::rhi::VulkanGraphicsKernel {
                        handle: out_kernel,
                        vtable: self.vtable,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
                    // β-shape: see compute_kernel above.
                    Ok(crate::vulkan::rhi::VulkanRayTracingKernel {
                        handle: out_kernel,
                        vtable: self.vtable,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
            HandleKind::Boxed => {
                self.host_inner().build_triangles_blas(label, vertices, indices)
            }
            HandleKind::ScopeToken => {
                if self.vtable.is_null() {
                    return Err(Error::GpuError(
                        "build_triangles_blas: GpuContextFullAccess has null vtable".into(),
                    ));
                }
                let mut out_blas: *const std::ffi::c_void = std::ptr::null();
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
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if status == 0 {
                    if out_blas.is_null() {
                        return Err(Error::GpuError(
                            "build_triangles_blas: host signaled success but out_blas is null".into(),
                        ));
                    }
                    // β-shape: bundle the raw handle (`Arc::into_raw(Arc<Inner>)`-shaped)
                    // with the host vtable. Cross-rustc-version safe because no Inner
                    // layout knowledge is required cdylib-side.
                    Ok(crate::vulkan::rhi::VulkanAccelerationStructure {
                        handle: out_blas,
                        vtable: self.vtable,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
                    Ok(crate::vulkan::rhi::VulkanAccelerationStructure {
                        handle: out_tlas,
                        vtable: self.vtable,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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

    /// Construct a timeline semaphore. Backs the camera processor's
    /// per-frame timeline that signals into downstream consumers
    /// (display, encoders). Mode-routed: host-mode dispatches through
    /// `host_inner()`; cdylib-mode dispatches through the vtable's
    /// `create_timeline_semaphore` slot and reconstructs the Arc via
    /// `Arc::from_raw`.
    ///
    /// **Note**: this slot transits `Arc<HostVulkanTimelineSemaphore>`
    /// via Arc-raw-pointer pattern — same hazard as the kernel paths
    /// pre-#917 Phase 8. Arc internals leak across DSO for now.
    /// In-tree consumers (camera, display) are built in the same
    /// workspace as engine; the layout matches by construction.
    /// Cross-repo plugin distribution will need a β-shape lift for
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
                            out_handle
                                as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore,
                        )
                    };
                    Ok(arc)
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
                    .into_owned();
                    Err(Error::GpuError(msg))
                }
            }
        }
    }

    /// Read-once GPU capability snapshot. Backs the camera processor's
    /// vendor-name / external-memory / cross-device-DMA-BUF-probe
    /// branching without exposing host-internal `HostVulkanDevice`
    /// across the FFI. Mode-routed: host-mode dispatches through
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
                // Stack-allocate the FFI repr; the host populates it
                // via *out_caps. We then decode into an owned snapshot.
                let mut out: std::mem::MaybeUninit<
                    streamlib_plugin_abi::GpuCapabilitiesRepr,
                > = std::mem::MaybeUninit::uninit();
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
                    let name_len = (repr.device_name_len as usize)
                        .min(repr.device_name.len());
                    let device_name =
                        String::from_utf8_lossy(&repr.device_name[..name_len])
                            .into_owned();
                    Ok(GpuCapabilitiesSnapshot {
                        device_name,
                        supports_external_memory: repr.supports_external_memory != 0,
                        supports_cross_device_dma_buf_probe: repr
                            .supports_cross_device_dma_buf_probe
                            != 0,
                        supports_ray_tracing_pipeline: repr
                            .supports_ray_tracing_pipeline
                            != 0,
                    })
                } else {
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
            HandleKind::ScopeToken => {
                self.inherited_limited_unchecked().blit_copy(src, dest)
            }
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
        unsafe { self.host_inner().blit_copy_iosurface(src, dest, width, height) }
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
            HandleKind::ScopeToken => {
                self.inherited_limited_unchecked().surface_store()
            }
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
                    let msg = String::from_utf8_lossy(
                        &err_buf[..err_len.min(err_buf.len())],
                    )
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
    /// shape that crosses the FFI boundary). The bridge is registered by
    /// host code via `set_cpu_readback_bridge` and read by host adapter
    /// machinery; cdylib code doesn't need to read it. Calling from a
    /// cdylib panics at the explicit guard below.
    #[cfg(target_os = "linux")]
    pub fn cpu_readback_bridge(&self) -> Option<Arc<dyn CpuReadbackBridge>> {
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "GpuContextFullAccess::cpu_readback_bridge(): return type \
                 `Option<Arc<dyn CpuReadbackBridge>>` is a trait object whose \
                 vtable layout is rustc-private and cannot cross the FFI \
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
                 vtable layout is rustc-private and cannot cross the FFI \
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
                 whose vtable layout is rustc-private and cannot cross the FFI \
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
                 whose vtable layout is rustc-private and cannot cross the FFI \
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
        let texture = gpu.device().create_texture(&desc).expect("texture creation failed");
        let surface_id = "test-surface-001";

        gpu.register_texture(surface_id, texture.clone());

        let resolved = gpu
            .resolve_texture_by_surface_id(surface_id, None, 640, 480)
            .expect("texture cache miss");
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
        assert!(gpu
            .resolve_texture_by_surface_id("nonexistent-surface", None, 640, 480)
            .is_err());

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
                &limited.video_source_timeline_semaphore().expect("via limited"),
                &timeline_a,
            ));

            // The full-access view shares the same publication slot.
            assert!(Arc::ptr_eq(
                &full.video_source_timeline_semaphore().expect("via full"),
                &timeline_a,
            ));
            full.set_video_source_timeline_semaphore(&timeline_b);
            assert!(Arc::ptr_eq(
                &limited.video_source_timeline_semaphore().expect("limited sees b"),
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
        let result: Result<()> = limited.escalate(|_full| {
            Err(Error::Runtime("synthetic failure".to_string()))
        });
        match result {
            Err(Error::Runtime(msg)) if msg == "synthetic failure" => {}
            other => panic!("expected synthetic Runtime error, got {other:?}"),
        }

        // Mutex must be released after the error — a second escalation should proceed.
        let after: Result<u32> = limited.escalate(|_full| Ok(7));
        assert_eq!(after.expect("escalate after error"), 7);

        println!("escalate propagates closure error + releases lock: OK");
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
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
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
