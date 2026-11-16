// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::context::{
    OneOffAcquisitionCountedForItsNode, OneOffAcquisitionShape,
    OneOffAcquisitionsCountedForOneNativeNode, TextureRegistration,
    acquire_a_one_off_counted_against_its_node,
};
use crate::core::media_clock::MediaClock;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::core::rhi::KernelShaderStageMask;
use crate::core::rhi::{
    CommandBuffer, GpuDevice, PixelBuffer, PixelFormat, PublishedPixelBufferFrameId, RhiBlitter,
    RhiColorConverter, RhiCommandQueue, Texture, TextureDescriptor, TextureFormat, TextureUsages,
    pool_slot_key_of_surface_id,
};
use crate::core::{Error, Result};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::host_rhi::HostTextureExt;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use streamlib_consumer_rhi::VulkanLayout;

/// No-op blitter for platforms without a native blitter.
struct NoOpBlitter;

impl RhiBlitter for NoOpBlitter {
    fn blit_copy(&self, _src: &PixelBuffer, _dest: &PixelBuffer) -> Result<()> {
        Err(Error::NotSupported(
            "Blitter not supported on this platform".into(),
        ))
    }

    fn clear_cache(&self) {}
}

use super::surfaces_and_their_stores::surface_store::SurfaceStore;
use super::texture_pools_and_rings::lease_aware_pool_slot_ring::LeaseAwarePoolMintedFrameGenerations;
use super::texture_pools_and_rings::shared_pool_entry_freeing::LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES;
use super::texture_pools_and_rings::texture_pool::{
    PooledTextureHandle, TexturePool, TexturePoolDescriptor,
};

mod compute_dispatch_batches;
mod counters_and_diagnostics;
#[cfg(target_os = "linux")]
mod dma_buf_opaque_fd_and_host_mapping_acquisition;
mod full_access;
#[cfg(target_os = "linux")]
mod full_access_dma_buf_and_opaque_fd_acquisition;
#[cfg(target_os = "macos")]
mod full_access_iosurface_acquisition;
#[cfg(target_os = "macos")]
mod iosurface_acquisition;
#[cfg(target_os = "macos")]
mod iosurface_check_in_and_out;
mod kernels;
mod limited_access;
mod pixel_buffer_pool_manager;
mod pixel_buffer_pool_slot;
mod pixel_buffers;
mod ray_tracing;
mod resource_acquisition;
#[cfg(not(target_os = "macos"))]
mod surface_check_in_and_out_refused_off_macos;
#[cfg(target_os = "linux")]
mod surface_export_staging;
mod surface_store_check_in_and_out;
mod texture_registration;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use compute_dispatch_batches::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding,
    SurfaceBoundKernelBindingResource,
};
pub use full_access::GpuContextFullAccess;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use kernels::{GraphicsKernelHeldForOneDraw, RayTracingKernelHeldForOneTrace};
pub use limited_access::GpuContextLimitedAccess;
pub(super) use pixel_buffer_pool_manager::PixelBufferPoolManager;
pub(super) use pixel_buffer_pool_slot::{
    PixelBufferPoolKey, PixelBufferPoolSlot, gpu_pool_slot_frame_of_the_pixel_buffer_slot,
    published_pixel_buffer_frame_id,
};
#[cfg(target_os = "linux")]
pub use surface_export_staging::{SurfaceExportStaging, SurfaceExportStagingResidency};

/// Read-once GPU capability snapshot returned by
/// [`GpuContext::gpu_capabilities`] /
/// [`GpuContextFullAccess::gpu_capabilities`].
///
/// Plain owned data, populated directly from the host-side getters.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone)]
pub struct GpuCapabilitiesSnapshot {
    /// UTF-8 device name (vendor + model).
    pub device_name: String,
    /// Whether the GPU exposes `VK_KHR_external_memory` +
    /// `VK_KHR_external_memory_fd`.
    pub supports_external_memory: bool,
    /// Whether the device is a CPU-type software rasterizer.
    pub device_is_software: bool,
    /// Whether frames can cross as DMA-BUF; never on a software device.
    pub supports_dma_buf_export: bool,
    /// Whether a timeline semaphore can be exported as an OPAQUE_FD.
    pub supports_semaphore_export: bool,
    /// Whether cross-device DMA-BUF probe is supported (false on
    /// NVIDIA Linux per the engine capability guard).
    pub supports_cross_device_dma_buf_probe: bool,
    /// Whether the GPU exposes `VK_KHR_ray_tracing_pipeline`.
    pub supports_ray_tracing_pipeline: bool,
}

/// Which allocation a storage buffer takes, derived from where it goes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageBufferAllocationFlavour {
    /// HOST_VISIBLE and mapped: the Rust caller that acquired it keeps it.
    CallerHeldHostVisible,
    /// It crosses to a helper process and from there to the floor's array
    /// library: DEVICE_LOCAL OPAQUE_FD on Linux, which CUDA imports and
    /// DMA-BUF it cannot; a byte-shaped private IOSurface on macOS, which a
    /// helper imports and hands torch-MPS or MLX as its `MTLBuffer`.
    CrossesToAHelperProcess,
}

/// The refusal of a surface-share call made through a GPU context no surface
/// store is connected to.
const NO_SURFACE_STORE_IS_CONNECTED_TO_THIS_GPU_CONTEXT: &str = "no surface store is connected to this GPU context: a stream's view of the engine's GPU \
     context holds one from the stream's start until its stop, so start the stream first";

#[derive(Clone)]
pub struct GpuContext {
    device: Arc<GpuDevice>,
    texture_pool: TexturePool,
    pixel_buffer_pool_manager: Arc<PixelBufferPoolManager>,
    /// The generation every lease-aware ring this context owns most recently
    /// minted per slot — pixel-buffer pools and processor output pools alike.
    lease_aware_pool_minted_frame_generations: Arc<LeaseAwarePoolMintedFrameGenerations>,
    /// Surface store for cross-process GPU surface sharing. Each loaded
    /// stream's view holds a slot of its own, set as the stream starts and
    /// cleared as it stops; the engine's own context holds the engine's.
    surface_store: Arc<Mutex<Option<SurfaceStore>>>,
    /// The engine's own surface store, owned by the runtime id for the
    /// engine's life and shared by every view: the pixel-buffer pools and the
    /// export stagings — engine-wide state every stream reads — register
    /// through it, so a stream's stop never takes their registrations.
    engines_own_surface_store: Arc<std::sync::OnceLock<SurfaceStore>>,
    /// Where kernels this view builds keep their on-disk pipeline caches: the
    /// stream's, under its project. `None` on the engine's own context.
    pipeline_cache_directory_of_its_stream: Option<Arc<std::path::Path>>,
    /// The loaded stream this view serves, which every shared pool entry it
    /// acquires is attributed to. `None` on the engine's own context.
    loaded_stream_this_view_serves: Option<crate::core::runtime::LoadedStreamTag>,
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
    /// Export stagings keyed by (surface_id, residency) — sibling of
    /// `texture_cache`, but spanning registration replacements: a
    /// rotating producer re-registers per frame, and the staging must
    /// survive that while its blit source is re-resolved per refill.
    /// Nested by residency because one surface can be exported to a GPU
    /// consumer and a CPU consumer at once, as two allocations — and
    /// because eviction takes the surface's whole inner map at once.
    /// Dropped with the context; evicted by `unregister_texture`.
    #[cfg(target_os = "linux")]
    pub(crate) surface_export_stagings: Arc<
        parking_lot::Mutex<
            HashMap<
                String,
                HashMap<super::SurfaceExportStagingResidency, Arc<super::SurfaceExportStaging>>,
            >,
        >,
    >,
    /// Cache of textures backing surface-share-registered pixel buffers
    /// (`escalate_acquire_pixel_buffer` flow). Refreshed on every resolve so
    /// rotating-pool producers don't render stale contents — kept separate
    /// from `texture_cache` so a same-process cache hit can't shortcut the
    /// refresh.
    buffer_texture_cache: Arc<Mutex<HashMap<String, Texture>>>,
    /// Tensor storage buffers a helper process acquired, keyed by pool slot —
    /// the parent-wide map a dispatch in any helper binds another helper's
    /// tensor surface from.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    storage_buffer_registrations: Arc<Mutex<HashMap<String, crate::core::rhi::StorageBuffer>>>,
    /// Engine-wide cache of `(src, dst)`-keyed color converters. Per-frame
    /// `ResolvedColorInfo` lives in push constants, so a single cached
    /// converter handles every variation of source color description.
    /// Construction is rare; conversion is hot — RwLock with double-check
    /// on miss matches that read/write skew.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    color_converter_cache: Arc<
        RwLock<HashMap<(PixelFormat, PixelFormat), Arc<crate::core::rhi::RhiColorConverterInner>>>,
    >,
    /// Offscreen compositors keyed by attachment format — the graphics twin of
    /// `color_converter_cache`, and cached for the same reason: building one
    /// compiles two SPIR-V modules and a whole graphics pipeline, which is
    /// not per-call work.
    ///
    /// Each entry is behind its own lock because the draw, not just the
    /// build, is the shared resource: `compose_to_offscreen_texture` stages
    /// one descriptor-ring slot and then submits, so two concurrent draws
    /// through one compositor would overwrite each other's bindings.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    offscreen_compositor_cache: Arc<
        parking_lot::Mutex<
            HashMap<
                TextureFormat,
                Arc<parking_lot::Mutex<crate::vulkan::rhi::VulkanOffscreenCompositor>>,
            >,
        >,
    >,
    /// Serializes [`GpuContextLimitedAccess::escalate`] scopes across
    /// threads so concurrent GPU resource creation (video
    /// sessions, DPB images) can't race on the device. The
    /// compiler acquires this during Phase 4 of spawn_processor and
    /// releases it after waiting for the device to go idle. A flag +
    /// Condvar rather than a `Mutex<()>` guard, so enter and exit can
    /// run on different threads.
    escalate_gate: Arc<super::escalate_gate::EscalateGate>,
    /// Compute kernels built for the `register_compute_kernel` escalate op,
    /// keyed so that re-creating an identical kernel is free of compilation.
    /// Compute dispatch is a capability every caller reaches, so this is
    /// always present — there is nothing to install and no absent case.
    ///
    /// Entries live for this context's lifetime: bounded by distinct SPIR-V
    /// blobs, never evicted, so a kernel survives its registering helper.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    compute_kernel_cache: Arc<Mutex<HashMap<String, Arc<crate::vulkan::rhi::VulkanComputeKernel>>>>,
    /// The engine's Slang compiler and the SPIR-V it has already produced.
    ///
    /// Slang text is the kernel source contract, so compilation happens here at
    /// kernel construction rather than in a build step the author has to own.
    /// Sits in front of `compute_kernel_cache`: this one spares the
    /// compilation, that one spares the pipeline.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    slang_shader_source_compiler: Arc<crate::core::rhi::SlangShaderSourceCompiler>,
    /// The recorders no compute dispatch is using, each built on first need
    /// and kept for this context's lifetime.
    ///
    /// A dispatch takes one for itself and returns it once its submission has
    /// retired, so dispatches waiting on their own work at once each hold a
    /// recorder, and the pool grows to as many as ever wait together. Reusing
    /// them spares the command pool, command buffer and fence a recording
    /// would otherwise allocate per frame.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    idle_compute_dispatch_recorders:
        Arc<parking_lot::Mutex<Vec<crate::vulkan::rhi::RhiCommandRecorder>>>,
    /// The latest queue submission that dispatched each compute kernel, keyed
    /// by the kernel's handle.
    ///
    /// Held by a dispatch from its first recorded command through publishing
    /// the layouts its bindings land in, and released before it waits on its
    /// work: so recordings see each other's layouts in submission order, while
    /// one stream's dispatch never waits for another's GPU work to finish. A
    /// kernel owns one descriptor set, which its next dispatch rewrites only
    /// once the submission named here has retired.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    latest_submission_per_compute_kernel: Arc<
        parking_lot::Mutex<HashMap<usize, crate::vulkan::rhi::HostVulkanQueueSubmissionsSnapshot>>,
    >,
    /// The recorder every surface-to-surface copy reuses, built on first use;
    /// its lock is what serializes two copies.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) surface_to_surface_copy_recorder:
        Arc<parking_lot::Mutex<Option<crate::vulkan::rhi::RhiCommandRecorder>>>,
    /// Graphics kernels built for the `register_graphics_kernel` escalate op,
    /// keyed the same way `compute_kernel_cache` is and with the same
    /// lifetime.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    graphics_kernel_cache:
        Arc<Mutex<HashMap<String, Arc<crate::vulkan::rhi::VulkanGraphicsKernel>>>>,
    /// Ray-tracing kernels built for the `register_ray_tracing_kernel`
    /// escalate op, keyed the same way `compute_kernel_cache` is and with the
    /// same lifetime.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ray_tracing_kernel_cache:
        Arc<Mutex<HashMap<String, Arc<crate::vulkan::rhi::VulkanRayTracingKernel>>>>,
    /// The recorder every escalate draw's input transition reuses, built on
    /// first use so a draw creates no command pool or fence per frame.
    ///
    /// Its lock is also what keeps two draws apart: a graphics kernel stages
    /// its bindings on itself, and the cache hands one kernel to every helper
    /// registering the same shaders, so a draw holds it from its first staged
    /// binding through its fence wait.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    graphics_draw_input_transition_recorder:
        Arc<parking_lot::Mutex<Option<crate::vulkan::rhi::RhiCommandRecorder>>>,
    /// The ray-tracing twin of `graphics_draw_input_transition_recorder`, held
    /// by a trace for the same two reasons.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ray_tracing_trace_input_transition_recorder:
        Arc<parking_lot::Mutex<Option<crate::vulkan::rhi::RhiCommandRecorder>>>,
    /// Acceleration structures built for the
    /// `register_acceleration_structure_blas` / `_tlas` escalate ops.
    ///
    /// A registry, not a cache: two builds of the same geometry are two
    /// structures under two ids, because an acceleration structure holds
    /// device memory proportional to its mesh and deduplicating them by
    /// content would retain every mesh any helper ever built.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    acceleration_structure_registry:
        Arc<Mutex<HashMap<String, Arc<crate::vulkan::rhi::VulkanAccelerationStructure>>>>,
}

impl GpuContext {
    /// Create a new GPU context with an RHI device.
    pub fn new(device: GpuDevice) -> Self {
        let device = Arc::new(device);
        let engines_own_surface_store = Arc::<std::sync::OnceLock<SurfaceStore>>::default();
        let texture_pool = TexturePool::new_seeing_the_engines_surface_store(
            Arc::clone(&device),
            Arc::clone(&engines_own_surface_store),
        );
        let blitter = Self::create_blitter(&device);
        let lease_aware_pool_minted_frame_generations =
            Arc::new(LeaseAwarePoolMintedFrameGenerations::default());
        let buffer_texture_cache = Arc::new(Mutex::new(HashMap::new()));
        Self {
            pixel_buffer_pool_manager: Arc::new(PixelBufferPoolManager::new(
                Arc::clone(&device),
                Arc::clone(&lease_aware_pool_minted_frame_generations),
                Arc::clone(&engines_own_surface_store),
                Arc::clone(&buffer_texture_cache),
            )),
            lease_aware_pool_minted_frame_generations,
            device,
            texture_pool,
            surface_store: Arc::new(Mutex::new(None)),
            engines_own_surface_store,
            pipeline_cache_directory_of_its_stream: None,
            loaded_stream_this_view_serves: None,
            blitter,
            texture_cache: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(target_os = "linux")]
            surface_export_stagings: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            buffer_texture_cache,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            storage_buffer_registrations: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            color_converter_cache: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            offscreen_compositor_cache: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            escalate_gate: Arc::new(super::escalate_gate::EscalateGate::new()),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            compute_kernel_cache: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            slang_shader_source_compiler: Arc::new(
                crate::core::rhi::SlangShaderSourceCompiler::new(),
            ),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            idle_compute_dispatch_recorders: Arc::new(parking_lot::Mutex::new(Vec::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            latest_submission_per_compute_kernel: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            surface_to_surface_copy_recorder: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            graphics_kernel_cache: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            ray_tracing_kernel_cache: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            graphics_draw_input_transition_recorder: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            ray_tracing_trace_input_transition_recorder: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            acceleration_structure_registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Borrow this context's escalate gate. The gate serializes
    /// [`GpuContextLimitedAccess::escalate`] scopes
    /// ([`super::escalate_gate::EscalateGate::enter_scoped`] gives
    /// RAII release).
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn create_blitter(_device: &Arc<GpuDevice>) -> Arc<dyn RhiBlitter> {
        Arc::new(NoOpBlitter)
    }

    /// Get a reference to the RHI GPU device.
    pub fn device(&self) -> &Arc<GpuDevice> {
        &self.device
    }

    /// Get the texture pool for acquiring pooled textures.
    pub fn texture_pool(&self) -> &TexturePool {
        &self.texture_pool
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

    /// Build an engine-owned command-buffer recorder bound to the
    /// device's default queue.
    ///
    /// Wraps the long-lived command pool + reset-able primary command
    /// buffer + per-frame barrier/copy/dispatch recording + queue-mutex-
    /// guarded submit-with-timeline-signal shape that processors
    /// reinvented inline pre-#751. See
    /// [`RhiCommandRecorder`](crate::vulkan::rhi::RhiCommandRecorder)
    /// for the per-frame usage protocol.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// A timeline semaphore for this process alone — signaled by a recorder
    /// submit, waited on the host — that is never exported. Backs
    /// [`GpuContextFullAccess::create_timeline_semaphore`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tracing::instrument(
        level = "debug",
        skip(self),
        fields(rhi_op = "create_timeline_semaphore")
    )]
    pub fn create_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        let device = self.device.inner.device();
        let semaphore =
            crate::vulkan::rhi::HostVulkanTimelineSemaphore::new(device, initial_value)?;
        Ok(Arc::new(semaphore))
    }

    /// Initialize GPU context for the current platform.
    pub fn init_for_platform() -> Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    // =========================================================================
    // Surface Store (Cross-Process GPU Surface Sharing)
    // =========================================================================

    /// This context's view for one loaded stream: it shares the device and
    /// every pool, cache and gate, owns a surface-store slot holding
    /// `surface_store` and the stream's pipeline-cache directory, and
    /// attributes the shared pool entries it acquires to `loaded_stream`.
    pub(crate) fn view_for_one_loaded_stream(
        &self,
        loaded_stream: crate::core::runtime::LoadedStreamTag,
        surface_store: Option<SurfaceStore>,
        pipeline_cache_directory_of_the_stream: &std::path::Path,
    ) -> Self {
        Self {
            surface_store: Arc::new(Mutex::new(surface_store)),
            pipeline_cache_directory_of_its_stream: Some(Arc::from(
                pipeline_cache_directory_of_the_stream,
            )),
            loaded_stream_this_view_serves: Some(loaded_stream),
            ..self.clone()
        }
    }

    /// Where kernels this context builds keep their pipeline caches, when it is
    /// a stream's view.
    pub fn pipeline_cache_directory_of_its_stream(&self) -> Option<&std::path::Path> {
        self.pipeline_cache_directory_of_its_stream.as_deref()
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("device", &self.device)
            .finish()
    }
}

/// A kernel's output texture, registered in the texture cache under its id
/// until this drops.
pub struct OutputTextureRegisteredUntilDropped {
    texture: Texture,
    registered_under_id: String,
    gpu_the_texture_is_registered_with: GpuContext,
}

impl std::ops::Deref for OutputTextureRegisteredUntilDropped {
    type Target = Texture;

    fn deref(&self) -> &Texture {
        &self.texture
    }
}

impl Drop for OutputTextureRegisteredUntilDropped {
    fn drop(&mut self) {
        self.gpu_the_texture_is_registered_with
            .unregister_texture(&self.registered_under_id);
    }
}

impl std::fmt::Debug for OutputTextureRegisteredUntilDropped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputTextureRegisteredUntilDropped")
            .field("texture", &self.texture)
            .field("registered_under_id", &self.registered_under_id)
            .finish()
    }
}

/// Acquire a pixel buffer from `gpu`, counted against `one_offs_of_the_node`.
fn acquire_pixel_buffer_counted_against_the_node(
    gpu: &GpuContext,
    one_offs_of_the_node: Option<&Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<(
    PublishedPixelBufferFrameId,
    OneOffAcquisitionCountedForItsNode<PixelBuffer>,
)> {
    acquire_a_one_off_counted_against_its_node(
        one_offs_of_the_node,
        OneOffAcquisitionShape::PixelBuffer {
            width,
            height,
            format,
        },
        || gpu.acquire_pixel_buffer(width, height, format),
    )
    .map(OneOffAcquisitionCountedForItsNode::split_off_the_id_it_was_acquired_under)
}

/// Acquire a storage buffer from `gpu`, counted against `one_offs_of_the_node`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn acquire_storage_buffer_counted_against_the_node(
    gpu: &GpuContext,
    one_offs_of_the_node: Option<&Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    layout: &crate::core::rhi::TensorStorageBufferLayout,
) -> Result<OneOffAcquisitionCountedForItsNode<crate::core::rhi::StorageBuffer>> {
    acquire_a_one_off_counted_against_its_node(
        one_offs_of_the_node,
        OneOffAcquisitionShape::StorageBuffer {
            tensor_layout: layout.clone(),
        },
        || gpu.acquire_storage_buffer(layout),
    )
}

/// Acquire a pooled texture from `gpu`, counted against `one_offs_of_the_node`.
fn acquire_texture_counted_against_the_node(
    gpu: &GpuContext,
    one_offs_of_the_node: Option<&Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    desc: &TexturePoolDescriptor,
) -> Result<OneOffAcquisitionCountedForItsNode<PooledTextureHandle>> {
    acquire_a_one_off_counted_against_its_node(
        one_offs_of_the_node,
        OneOffAcquisitionShape::of_texture_pool_descriptor(desc),
        || gpu.acquire_texture(desc),
    )
}

/// The usage set of a kernel's output texture.
fn output_texture_usages() -> TextureUsages {
    TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST
}

/// The usage set of a render-target image a consumer adapter wraps, on every
/// platform.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn render_target_texture_usages() -> TextureUsages {
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
            | TextureUsages::STORAGE_BINDING
}

#[cfg(test)]
mod tests;
