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
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
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

use super::lease_aware_pool_slot_ring::LeaseAwarePoolMintedFrameGenerations;
use super::shared_pool_entry_freeing::LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES;
use super::surface_store::SurfaceStore;
use super::texture_pool::{PooledTextureHandle, TexturePool, TexturePoolDescriptor};

mod compute_dispatch_batches;
mod counters_and_diagnostics;
mod kernels;
mod pixel_buffer_pool_manager;
mod pixel_buffer_pool_slot;
mod pixel_buffers;
mod ray_tracing;
mod resource_acquisition;
mod surface_store_check_in_and_out;
mod texture_registration;

pub(super) use pixel_buffer_pool_manager::PixelBufferPoolManager;
pub(super) use pixel_buffer_pool_slot::{
    PixelBufferPoolKey, PixelBufferPoolSlot, gpu_pool_slot_frame_of_the_pixel_buffer_slot,
    published_pixel_buffer_frame_id,
};

/// Cache key for a compute kernel built from a pre-compiled blob.
///
/// The blob fixes stage, entry point and target environment on its own, so the
/// bytes plus the declared push-constant size are the whole key. A Slang source
/// contract keys on the compiler's version too, because then the engine is what
/// turns source into bytes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn compute_kernel_cache_key(spv: &[u8], push_constant_size: u32, entry_point: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(spv);
    hasher.update(push_constant_size.to_le_bytes());
    // Two pipelines built from one module against different entry points are
    // different kernels, so the id has to tell them apart. The length prefix is
    // defensive: the name is last in the digest today, so nothing can run into
    // it yet, but a field appended below would merge with it silently.
    hasher.update((entry_point.len() as u64).to_le_bytes());
    hasher.update(entry_point.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Digest a variable-length part of a cache key.
///
/// The length prefix is what keeps two different splits of the same
/// concatenated bytes from hashing the same.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn digest_length_prefixed(hasher: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest as _;
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Cache key for a graphics kernel.
///
/// Everything that changes the pipeline the driver builds is in the digest.
/// The fixed-function state goes in through its `Debug` rendering because that
/// is total: a field added to `GraphicsPipelineState` joins the key without
/// anyone remembering to add it, which a hand-enumerated digest cannot promise.
/// The key never leaves this process, so its stability across builds buys
/// nothing that would justify the alternative.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn graphics_kernel_cache_key(
    stages: &[crate::core::rhi::GraphicsStage<'_>],
    push_constants: crate::core::rhi::GraphicsPushConstants,
    pipeline_state: &crate::core::rhi::GraphicsPipelineState,
    descriptor_sets_in_flight: u32,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update((stages.len() as u64).to_le_bytes());
    for stage in stages {
        hasher.update((stage.stage as u32).to_le_bytes());
        digest_length_prefixed(&mut hasher, stage.spv);
        digest_length_prefixed(&mut hasher, stage.entry_point.as_bytes());
    }
    hasher.update(push_constants.size.to_le_bytes());
    hasher.update(push_constants.stages.bits().to_le_bytes());
    hasher.update(descriptor_sets_in_flight.to_le_bytes());
    digest_length_prefixed(&mut hasher, format!("{pipeline_state:?}").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Cache key for a ray-tracing kernel.
///
/// Same shape as the graphics key; the shader-group layout and the recursion
/// depth take the place of the fixed-function state, since those are what the
/// driver builds the pipeline and its binding table from.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ray_tracing_kernel_cache_key(
    stages: &[crate::core::rhi::RayTracingStage<'_>],
    groups: &[crate::core::rhi::RayTracingShaderGroup],
    push_constants: crate::core::rhi::RayTracingPushConstants,
    max_recursion_depth: u32,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update((stages.len() as u64).to_le_bytes());
    for stage in stages {
        hasher.update((stage.stage as u32).to_le_bytes());
        digest_length_prefixed(&mut hasher, stage.spv);
        digest_length_prefixed(&mut hasher, stage.entry_point.as_bytes());
    }
    hasher.update(push_constants.size.to_le_bytes());
    hasher.update(push_constants.stages.bits().to_le_bytes());
    hasher.update(max_recursion_depth.to_le_bytes());
    digest_length_prefixed(&mut hasher, format!("{groups:?}").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Settle a caller's push-constant declaration against what the shaders
/// reflect, for a kernel kind whose push-constant range carries a stage mask.
///
/// The size must agree outright. The stage mask follows the same rule a
/// binding's does — a declaration may widen visibility past what the shaders
/// read, never narrow it below — and an empty mask asserts nothing, so the
/// reflected one stands.
///
/// Generic over the mask rather than written once per kernel kind, for the same
/// reason the shared binding reconciliation is: graphics and ray tracing differ
/// only in which unrelated `u32` newtype they name, and
/// [`KernelShaderStageMask`] is the seam that already spans both.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn reconciled_push_constant_stages<Stages: KernelShaderStageMask>(
    kernel_kind_label: &str,
    declared_size: u32,
    declared_stages: Stages,
    reflected_size: u32,
    reflected_stages: Stages,
) -> Result<Stages> {
    if declared_size != reflected_size {
        return Err(Error::GpuError(format!(
            "{kernel_kind_label} kernel declares {declared_size} push-constant bytes but its \
             shaders reflect {reflected_size}"
        )));
    }
    if declared_stages.names_no_stage() {
        return Ok(reflected_stages);
    }
    if !declared_stages.contains_every_stage_in(reflected_stages) {
        return Err(Error::GpuError(format!(
            "{kernel_kind_label} kernel declares its push constants for {} but its shaders also \
             read them from {}",
            crate::core::rhi::quote_shader_stage_names(&declared_stages.named_stages()),
            crate::core::rhi::quote_shader_stage_names(
                &reflected_stages.stages_missing_from(declared_stages)
            )
        )));
    }
    Ok(declared_stages)
}

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

/// A cached graphics kernel with the context's draw recorder locked: its staged
/// bindings are this holder's until it drops.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct GraphicsKernelHeldForOneDraw<'context> {
    kernel: Arc<crate::vulkan::rhi::VulkanGraphicsKernel>,
    input_transition_recorder_slot:
        parking_lot::MutexGuard<'context, Option<crate::vulkan::rhi::RhiCommandRecorder>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl GraphicsKernelHeldForOneDraw<'_> {
    /// The kernel whose bindings this draw stages, borrowed no longer than
    /// the lock is held.
    pub fn kernel(&self) -> &crate::vulkan::rhi::VulkanGraphicsKernel {
        &self.kernel
    }

    /// The kernel, with the draw family's input-transition recorder — `None`
    /// until first built — borrowed alongside it.
    pub(crate) fn kernel_and_input_transition_recorder_slot(
        &mut self,
    ) -> (
        &crate::vulkan::rhi::VulkanGraphicsKernel,
        &mut Option<crate::vulkan::rhi::RhiCommandRecorder>,
    ) {
        (&self.kernel, &mut self.input_transition_recorder_slot)
    }
}

/// A cached ray-tracing kernel with the context's trace recorder locked: its
/// staged bindings are this holder's until it drops.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct RayTracingKernelHeldForOneTrace<'context> {
    kernel: Arc<crate::vulkan::rhi::VulkanRayTracingKernel>,
    input_transition_recorder_slot:
        parking_lot::MutexGuard<'context, Option<crate::vulkan::rhi::RhiCommandRecorder>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl RayTracingKernelHeldForOneTrace<'_> {
    /// The kernel whose bindings this trace stages, borrowed no longer than
    /// the lock is held.
    pub fn kernel(&self) -> &crate::vulkan::rhi::VulkanRayTracingKernel {
        &self.kernel
    }

    /// The kernel, with the trace family's input-transition recorder — `None`
    /// until first built — borrowed alongside it.
    pub(crate) fn kernel_and_input_transition_recorder_slot(
        &mut self,
    ) -> (
        &crate::vulkan::rhi::VulkanRayTracingKernel,
        &mut Option<crate::vulkan::rhi::RhiCommandRecorder>,
    ) {
        (&self.kernel, &mut self.input_transition_recorder_slot)
    }
}

/// One dispatch inside a batched recording, with its bindings already resolved
/// to registrations.
///
/// Resolution happens before the recording opens, on purpose: a name the shader
/// does not declare, or a surface this graph cannot resolve, is a caller
/// mistake, and refusing it while a command buffer is open would mean unwinding
/// one.
/// Owned rather than borrowing: a kernel and a registration are both opaque
/// `Arc` handles whose clone is a refcount bump, and a cloned registration
/// shares the very layout cell `update_layout` writes — so owning them costs a
/// few increments and spares the caller a second set of vectors to borrow from.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct BatchedComputeKernelDispatch {
    /// The kernel to bind and dispatch. No kernel may appear twice in one
    /// batch — see [`GpuContext::dispatch_compute_kernel_batch`].
    pub kernel: Arc<crate::vulkan::rhi::VulkanComputeKernel>,
    /// Every binding the kernel declares. Bindings do not persist on a kernel,
    /// so this is complete for each dispatch.
    pub bindings: Vec<BatchedComputeKernelDispatchBinding>,
    /// Push-constant payload for this dispatch alone — recorded into the
    /// command buffer, so consecutive dispatches of different kernels keep
    /// their own.
    pub push_constants: Vec<u8>,
    /// `vkCmdDispatch` groupCountX.
    pub group_count_x: u32,
    /// `vkCmdDispatch` groupCountY.
    pub group_count_y: u32,
    /// `vkCmdDispatch` groupCountZ.
    pub group_count_z: u32,
}

/// One resolved binding of a [`BatchedComputeKernelDispatch`].
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct BatchedComputeKernelDispatchBinding {
    /// Descriptor binding number the kernel declares this resource at.
    pub binding: u32,
    /// The surface bound there, as the kind the shader declares it.
    pub surface_bound_resource: SurfaceBoundKernelBindingResource,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl BatchedComputeKernelDispatchBinding {
    /// Stage this binding on `kernel`, as the kind the shader declares it.
    ///
    /// The one place a binding kind becomes a descriptor write: every
    /// dispatch, batched or alone, records its writes through here.
    pub fn write_into_kernel(
        &self,
        kernel: &crate::vulkan::rhi::VulkanComputeKernel,
    ) -> Result<()> {
        match &self.surface_bound_resource {
            SurfaceBoundKernelBindingResource::StorageImage(registration) => {
                kernel.set_storage_image(self.binding, registration.texture())
            }
            SurfaceBoundKernelBindingResource::SampledTexture(registration) => {
                kernel.set_sampled_texture(self.binding, registration.texture())
            }
            SurfaceBoundKernelBindingResource::StorageBuffer(buffer) => {
                kernel.set_storage_buffer_storage(self.binding, buffer)
            }
        }
    }
}

/// The resource a surface id resolved to for one kernel binding, carried as
/// the kind the shader declares it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone)]
pub enum SurfaceBoundKernelBindingResource {
    /// A texture written through `imageStore`, and the layout it is tracked in.
    StorageImage(TextureRegistration),
    /// A texture read through a combined sampler, and the layout it is tracked in.
    SampledTexture(TextureRegistration),
    /// A tensor storage buffer, read and written as an SSBO.
    StorageBuffer(crate::core::rhi::StorageBuffer),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl SurfaceBoundKernelBindingResource {
    /// The layout a storage image's descriptor requires.
    pub const STORAGE_IMAGE_REQUIRED_LAYOUT: VulkanLayout = VulkanLayout::GENERAL;
    /// The layout a combined image sampler's descriptor requires.
    pub const SAMPLED_TEXTURE_REQUIRED_LAYOUT: VulkanLayout =
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL;

    /// The binding kind this resource is bound as.
    pub fn kind(&self) -> crate::core::rhi::SurfaceBoundKernelBindingKind {
        use crate::core::rhi::SurfaceBoundKernelBindingKind;
        match self {
            Self::StorageImage(_) => SurfaceBoundKernelBindingKind::StorageImage,
            Self::SampledTexture(_) => SurfaceBoundKernelBindingKind::SampledTexture,
            Self::StorageBuffer(_) => SurfaceBoundKernelBindingKind::StorageBuffer,
        }
    }

    /// The bound texture and the image layout its descriptor requires, or
    /// `None` for a storage buffer, which has no layout.
    pub fn texture_and_required_layout(&self) -> Option<(&TextureRegistration, VulkanLayout)> {
        match self {
            Self::StorageImage(registration) => {
                Some((registration, Self::STORAGE_IMAGE_REQUIRED_LAYOUT))
            }
            Self::SampledTexture(registration) => {
                Some((registration, Self::SAMPLED_TEXTURE_REQUIRED_LAYOUT))
            }
            Self::StorageBuffer(_) => None,
        }
    }

    /// The image a texture binding names; `None` for a storage buffer, or a
    /// texture that carries no image.
    pub(crate) fn bound_image(&self) -> Option<vulkanalia::vk::Image> {
        self.texture_and_required_layout()
            .and_then(|(registration, _)| registration.texture().vulkan_inner().image())
    }

    /// The bound storage buffer, or `None` for a texture.
    pub fn storage_buffer(&self) -> Option<&crate::core::rhi::StorageBuffer> {
        match self {
            Self::StorageBuffer(buffer) => Some(buffer),
            Self::StorageImage(_) | Self::SampledTexture(_) => None,
        }
    }
}

/// Where a dispatch's binding sits in `recording`, for a refusal to name.
///
/// A recording of one dispatch is the single-dispatch escalate op riding the
/// batch machinery; its caller wrote no batch, so the location names the
/// binding alone rather than a "dispatch 0" that exists only host-side.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn binding_location_in_this_recording(
    recording: &[BatchedComputeKernelDispatch],
    dispatch_index: usize,
    binding: u32,
) -> String {
    if recording.len() == 1 {
        format!("binding {binding}")
    } else {
        format!("dispatch {dispatch_index} of this batch, binding {binding}")
    }
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

    /// Construct an OPAQUE_FD-exportable timeline semaphore against the
    /// host's vulkan device. Backs
    /// [`GpuContextFullAccess::create_exportable_timeline_semaphore`]
    /// which is the FullAccess-callable entry point.
    ///
    /// The returned semaphore is created with
    /// `VK_KHR_external_semaphore_fd` export support so its
    /// `export_opaque_fd` can hand a fresh OPAQUE_FD to a subprocess
    /// consumer (surface-share / CUDA cross-process sync).
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
/// Restricted GPU capability shim: an opaque `*const c_void` handle
/// pointing at a host-leaked `Box<Arc<GpuContext>>`.
#[repr(C)]
pub struct GpuContextLimitedAccess {
    /// Opaque host handle. Points at a `Box<Arc<GpuContext>>`.
    pub(crate) handle: *const std::ffi::c_void,
    /// The node this capability acquires one-offs for; `None` for the shared
    /// context, which counts none.
    one_off_acquisitions_of_the_node: Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
}

// SAFETY: `handle` points at a host-owned `Box<Arc<GpuContext>>` that
// is `Send + Sync` (Arc carries atomic refcounts, GpuContext's
// fields are themselves Send + Sync via their Arc wrappers). The
// SAFETY: `handle` points at a `Box<Arc<GpuContext>>` whose interior is
// Send + Sync; every method reaches the GpuContext through the handle.
unsafe impl Send for GpuContextLimitedAccess {}
unsafe impl Sync for GpuContextLimitedAccess {}

impl Clone for GpuContextLimitedAccess {
    /// Bumps the host's `Arc<GpuContext>` refcount.
    fn clone(&self) -> Self {
        let new_handle = if !self.handle.is_null() {
            // SAFETY: `handle` is `Box::into_raw(Box<Arc<GpuContext>>)`;
            // clone the Arc, re-box, and leak — released by the matching Drop.
            unsafe {
                let arc = &*(self.handle as *const std::sync::Arc<GpuContext>);
                Box::into_raw(Box::new(std::sync::Arc::clone(arc))) as *const std::ffi::c_void
            }
        } else {
            std::ptr::null()
        };
        Self {
            handle: new_handle,
            one_off_acquisitions_of_the_node: self.one_off_acquisitions_of_the_node.clone(),
        }
    }
}

impl Drop for GpuContextLimitedAccess {
    /// Releases the host-owned handle.
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Box::into_raw(Box<Arc<GpuContext>>)`,
            // from `new()` or `Clone`; reclaim it.
            unsafe {
                drop(Box::from_raw(
                    self.handle as *mut std::sync::Arc<GpuContext>,
                ));
            }
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
/// Constructed only by `Self::counting_one_offs_against_the_node` (`pub(in crate::core::context)`,
/// so only [`GpuContextLimitedAccess::escalate`]'s engine-internal
/// body can construct it). `handle` is a host-allocated
/// `Box<Arc<GpuContext>>`; every method routes through
/// `Self::host_inner`, and Drop runs [`std::boxed::Box::from_raw`]
/// on the boxed Arc.
pub struct GpuContextFullAccess {
    pub(crate) handle: *const std::ffi::c_void,
    /// The node this capability acquires one-offs for; `None` for the shared
    /// context, which counts none.
    one_off_acquisitions_of_the_node: Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
}

unsafe impl Send for GpuContextFullAccess {}
unsafe impl Sync for GpuContextFullAccess {}

impl Drop for GpuContextFullAccess {
    /// Releases the handle: runs `Box::from_raw` on the boxed
    /// `Arc<GpuContext>`.
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: handle was produced by `Self::counting_one_offs_against_the_node` via
        // `Box::into_raw(Box::new(Arc::new(GpuContext)))`.
        // `Box::from_raw` releases the box; the resulting
        // Arc<GpuContext>'s Drop releases the per-scope clone.
        let _ = unsafe { Box::from_raw(self.handle as *mut std::sync::Arc<GpuContext>) };
    }
}

impl GpuContextLimitedAccess {
    /// Wrap a [`GpuContext`] as a limited-access capability.
    ///
    /// The handle is the sole owning reference to the
    /// `Arc<GpuContext>`; every method reaches it through
    /// [`Self::host_inner`]. Allocates a host-side
    /// `Box<Arc<GpuContext>>` as the opaque handle.
    pub(crate) fn new(inner: GpuContext) -> Self {
        Self::counting_one_offs_against_the_node(inner, None)
    }

    /// Wrap a [`GpuContext`] as a limited-access capability whose one-off
    /// acquisitions count against `one_off_acquisitions_of_the_node`.
    pub(crate) fn counting_one_offs_against_the_node(
        inner: GpuContext,
        one_off_acquisitions_of_the_node: Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    ) -> Self {
        // Leak a fresh `Arc<GpuContext>` to back the opaque handle.
        // The handle is the sole owner; `host_inner()` derefs it.
        let arc: std::sync::Arc<GpuContext> = std::sync::Arc::new(inner);
        let boxed: Box<std::sync::Arc<GpuContext>> = Box::new(arc);
        let handle = Box::into_raw(boxed) as *const std::ffi::c_void;
        Self {
            handle,
            one_off_acquisitions_of_the_node,
        }
    }

    /// Engine-internal borrow of the host's [`GpuContext`] (read
    /// through the handle's `Box<Arc<GpuContext>>`).
    ///
    pub(crate) fn host_inner(&self) -> &GpuContext {
        // SAFETY: `self.handle` was produced by `Self::new` or
        // `host_gpu_lim_clone_handle` — both produce
        // `Box::into_raw(Box::new(Arc::new(GpuContext)))`. The
        // matching `host_gpu_lim_drop_handle` runs on Drop, so the
        // `Arc<GpuContext>` is alive for the duration of `&self`.
        // We deref the Box, then the Arc, to borrow the inner
        // `GpuContext`.
        unsafe { &*(self.handle as *const std::sync::Arc<GpuContext>) }
    }

    /// Produce a [`GpuContextFullAccess`] view of the same underlying context.
    #[cfg(test)]
    pub(crate) fn to_full_access(&self) -> GpuContextFullAccess {
        GpuContextFullAccess::counting_one_offs_against_the_node(
            self.host_inner().clone(),
            self.one_off_acquisitions_of_the_node.clone(),
        )
    }

    /// Serialized escalation to full GPU capability. Hands the
    /// closure a [`GpuContextFullAccess`] scoped to its body, with
    /// the host's escalate gate held for the duration; after the
    /// closure returns the gate releases and the device waits idle.
    ///
    /// This is the single primitive for GPU resource-creation work
    /// outside `setup()` — used by the compiler to run each
    /// processor's setup() and by running processors that need to
    /// reconfigure (acquire a new video session, etc.).
    ///
    /// Acquires the gate, constructs a `GpuContextFullAccess`, runs
    /// the closure, then waits device idle and releases the gate. A
    /// closure panic still unwinds; the release fires through a guard
    /// so the gate never leaks.
    ///
    /// Closure failure returns the closure's error; on closure
    /// success a follow-up `wait_device_idle` error is propagated.
    pub fn escalate<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        self.escalate_in_process(f)
    }

    /// Engine-internal escalate path.
    fn escalate_in_process<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        let inner = self.host_inner();
        let lock_start = std::time::Instant::now();
        let _gate_guard = inner.escalate_gate().enter_scoped();
        let mutex_wait_ns = lock_start.elapsed().as_nanos() as u64;

        let closure_start = std::time::Instant::now();
        let full = GpuContextFullAccess::counting_one_offs_against_the_node(
            inner.clone(),
            self.one_off_acquisitions_of_the_node.clone(),
        );
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
}

// Thread-local escalation rate tracker. Each processor runs `process()` on a
// dedicated worker thread, so per-thread counters approximate per-processor
// escalation rates. Sustained rate above the threshold fires `tracing::warn!`.
std::thread_local! {
    static ESCALATION_TIMESTAMPS_NS: std::cell::RefCell<std::collections::VecDeque<u64>> =
        std::cell::RefCell::new(std::collections::VecDeque::with_capacity(16));
    static ESCALATION_LAST_WARN_NS: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

const ESCALATION_RATE_WARN_THRESHOLD_PER_SEC: usize = 1;
const ESCALATION_RATE_WINDOW_NS: u64 = 1_000_000_000;
const ESCALATION_WARN_DEBOUNCE_NS: u64 = 5_000_000_000;

fn check_sustained_escalation_rate() {
    let now_ns = MediaClock::now().as_nanos() as u64;
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
        && last_warn.is_none_or(|at_ns| now_ns.saturating_sub(at_ns) >= ESCALATION_WARN_DEBOUNCE_NS)
    {
        ESCALATION_LAST_WARN_NS.with(|c| c.set(Some(now_ns)));
        let thread = std::thread::current();
        tracing::warn!(
            thread = thread.name().unwrap_or("<unnamed>"),
            escalations_last_second = count,
            "sustained GpuContextLimitedAccess::escalate rate on this thread — \
             processor likely needs more pre-reservation in setup()"
        );
    }
}

impl GpuContextFullAccess {
    /// Back-room constructor. Wraps an in-process [`GpuContext`] as a
    /// full-access capability whose methods route through
    /// [`Self::host_inner`] for direct dispatch, and whose one-off
    /// acquisitions count against `one_off_acquisitions_of_the_node`.
    ///
    /// Scope tightened to `pub(in crate::core::context)` so only
    /// [`GpuContextLimitedAccess::escalate`]'s host-mode body can
    /// construct one. Other engine code that wants FullAccess goes
    /// through `escalate(|full| ...)`; the privilege gate enforces
    /// serialization + `wait_device_idle`.
    pub(in crate::core::context) fn counting_one_offs_against_the_node(
        inner: GpuContext,
        one_off_acquisitions_of_the_node: Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    ) -> Self {
        let arc: std::sync::Arc<GpuContext> = std::sync::Arc::new(inner);
        let boxed: Box<std::sync::Arc<GpuContext>> = Box::new(arc);
        let handle = Box::into_raw(boxed) as *const std::ffi::c_void;
        Self {
            handle,
            one_off_acquisitions_of_the_node,
        }
    }

    /// Engine-internal borrow of the host's [`GpuContext`] (read
    /// through the handle's `Box<Arc<GpuContext>>`).
    pub(crate) fn host_inner(&self) -> &GpuContext {
        // SAFETY: `self.handle` was produced by `Self::counting_one_offs_against_the_node`, which
        // calls `Box::into_raw(Box::new(Arc::new(GpuContext)))`. The
        // matching Drop reclaims it, so the `Arc<GpuContext>` is alive
        // for the duration of `&self`.
        unsafe { &*(self.handle as *const std::sync::Arc<GpuContext>) }
    }
}

// -----------------------------------------------------------------------------
// Capability-split API surface.
//
// `GpuContextLimitedAccess` exposes the Sandbox surface only: pool acquires
// (pre-reserved), texture sampling, writes to mapped pixel buffers, read-only
// queries, and the shared command queue. Methods that allocate new GPU
// memory, create sessions/descriptors, or hand out raw device
// handles live exclusively on [`GpuContextFullAccess`] and are reachable from
// `process()` only via [`GpuContextLimitedAccess::escalate`].
// -----------------------------------------------------------------------------

impl GpuContextLimitedAccess {
    /// Acquire a pixel buffer from a pre-reserved pool (Split: fast path).
    ///
    /// The expected steady-state is a ring-slot hit. Callers should pre-reserve
    /// the pool in `setup()` by calling `acquire_pixel_buffer` on the
    /// [`GpuContextFullAccess`] with the target `(width, height, format)`.
    /// When every slot of the shape is held — by consumers, or lent to native
    /// nodes' output pools — the pool grows; past the device's memory budget
    /// that growth is refused by name, and nothing waits on another holder.
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(
        PublishedPixelBufferFrameId,
        OneOffAcquisitionCountedForItsNode<PixelBuffer>,
    )> {
        acquire_pixel_buffer_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            width,
            height,
            format,
        )
    }

    /// Acquire a caller-held storage buffer sized for `layout`.
    /// See [`GpuContext::acquire_storage_buffer`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_storage_buffer(
        &self,
        layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<OneOffAcquisitionCountedForItsNode<crate::core::rhi::StorageBuffer>> {
        acquire_storage_buffer_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            layout,
        )
    }

    /// Acquire a HOST_VISIBLE uniform buffer.
    /// See [`GpuContext::acquire_uniform_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        self.host_inner().acquire_uniform_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    /// See [`GpuContext::acquire_vertex_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        self.host_inner().acquire_vertex_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE index buffer.
    /// See [`GpuContext::acquire_index_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        self.host_inner().acquire_index_buffer(byte_size)
    }

    /// Get a pixel buffer by its published surface id (Split: local cache).
    pub fn get_pixel_buffer(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().get_pixel_buffer(surface_id)
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner()
            .resolve_pixel_buffer_by_surface_id(surface_id)
    }

    /// Register a texture in the same-process texture cache.
    ///
    /// The host-side impl bumps the
    /// `Arc<TextureInner>` refcount before stashing a clone in the
    /// cache, so dropping the caller's `texture` here releases
    /// exactly the caller's owned ref.
    pub fn register_texture(&self, id: &str, texture: Texture) {
        self.host_inner().register_texture(id, texture)
    }

    /// Register a texture with a declared initial Vulkan image layout.
    /// See [`GpuContext::register_texture_with_layout`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        self.host_inner()
            .register_texture_with_layout(id, texture, initial_layout)
    }

    /// Update a registered texture's tracked layout after a transition.
    /// See [`GpuContext::update_texture_registration_layout`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        self.host_inner()
            .update_texture_registration_layout(id, layout)
    }

    /// Resolve a VideoFrame's full registration record (texture + layout).
    ///
    /// Returns a [`TextureRegistration`] handle;
    /// Clone is cheap (refcount bump via vtable), Drop releases the
    /// host's `Arc<TextureRegistrationInner>` strong count.
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<TextureRegistration> {
        self.host_inner()
            .resolve_texture_registration_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Resolve a VideoFrame's texture (Split: cache hit).
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
        self.host_inner()
            .resolve_texture_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Acquire a pooled texture from a pre-reserved pool (Split: fast path).
    ///
    /// `VK_IMAGE_TILING_OPTIMAL`, in-process use only. For cross-process
    /// render targets, see [`GpuContextFullAccess::acquire_render_target_dma_buf_image`]
    /// (Linux) — Sandbox callers don't have a render-target alloc path
    /// because allocating a new RT-capable image is a privileged op
    /// that goes through escalate.
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_texture(
        &self,
        desc: &TexturePoolDescriptor,
    ) -> Result<OneOffAcquisitionCountedForItsNode<PooledTextureHandle>> {
        acquire_texture_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            desc,
        )
    }

    /// Copy a host-visible pixel buffer's contents into a pre-allocated
    /// device-local texture (e.g. a [`TextureRing`](crate::core::context::TextureRing)
    /// slot the caller already owns).
    ///
    /// Sandbox-safe: no allocation, no descriptor / pipeline construction,
    /// just a `vkCmdCopyBufferToImage` queue submit on the shared queue.
    /// See [`GpuContext::copy_pixel_buffer_to_texture`] for the full
    /// contract.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.host_inner().copy_pixel_buffer_to_texture(
            pixel_buffer,
            texture,
            surface_id,
            width,
            height,
        )
    }

    /// See [`GpuContext::unregister_texture`].
    pub fn unregister_texture(&self, id: &str) {
        self.host_inner().unregister_texture(id)
    }

    /// Drop a torn-down lease-aware pool slot from the generation index.
    pub(crate) fn forget_lease_aware_pool_slot(&self, pool_slot_key: &str) {
        self.host_inner()
            .lease_aware_pool_minted_frame_generations
            .forget_slot(pool_slot_key)
    }

    /// Get the shared command queue.
    ///
    /// Submitting recorded command buffers from `process()` is safe: the
    /// images/buffers a Sandbox caller can construct are pool-backed and
    /// pre-reserved. See design doc §8 Q5.
    ///
    /// Returns an owned [`RhiCommandQueue`] handle with the host's
    /// `Arc<RhiCommandQueueInner>` refcount bumped.
    pub fn command_queue(&self) -> RhiCommandQueue {
        self.host_inner().command_queue().clone()
    }

    /// Create a CPU-side command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        self.host_inner().create_command_buffer()
    }

    /// Copy pixels between same-format, same-size buffers (Split: cache hit).
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        self.host_inner().blit_copy(src, dest)
    }

    /// Get the surface store, if initialized.
    ///
    /// Returns `Some(SurfaceStore)` (refcount bumped) when the host has
    /// one, else `None`. The handle's own Clone/Drop manage the inner
    /// `Arc<SurfaceStoreInner>` strong count.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.host_inner().surface_store()
    }

    /// Check out a surface by ID (Split: cache hit).
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().check_out_surface(surface_id)
    }
}

impl GpuContextFullAccess {
    /// Construct an OPAQUE_FD-exportable timeline semaphore — the
    /// FullAccess-callable entry point over
    /// [`GpuContext::create_exportable_timeline_semaphore`].
    #[cfg(target_os = "linux")]
    pub fn create_exportable_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        self.host_inner()
            .create_exportable_timeline_semaphore(initial_value)
    }

    /// A non-exportable timeline semaphore for host-side waits on this
    /// process's own submits. See [`GpuContext::create_timeline_semaphore`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        self.host_inner().create_timeline_semaphore(initial_value)
    }

    /// Build a [`crate::vulkan::rhi::VulkanOffscreenCompositor`] for
    /// `attachment_format`. In-process (Boxed) only.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_offscreen_compositor(
        &self,
        attachment_format: crate::core::rhi::TextureFormat,
    ) -> Result<crate::vulkan::rhi::VulkanOffscreenCompositor> {
        self.host_inner()
            .create_offscreen_compositor(attachment_format)
    }

    /// Wait for the GPU device to become idle.
    ///
    pub fn wait_device_idle(&self) -> Result<()> {
        self.host_inner().wait_device_idle()
    }

    /// An empty output pool the native node `processor_id_of_the_owning_node`
    /// publishes its per-frame pixel buffers from. See
    /// [`super::ProcessorOutputPixelBufferPoolOfOneNativeNode`].
    pub fn create_processor_output_pixel_buffer_pool_for_a_native_node(
        &self,
        processor_id_of_the_owning_node: &str,
    ) -> super::ProcessorOutputPixelBufferPoolOfOneNativeNode {
        self.host_inner()
            .create_processor_output_pixel_buffer_pool_for_a_native_node(
                processor_id_of_the_owning_node,
            )
    }

    /// Acquire a pixel buffer from the shared pool. LimitedAccess
    /// mirror.
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(
        PublishedPixelBufferFrameId,
        OneOffAcquisitionCountedForItsNode<PixelBuffer>,
    )> {
        acquire_pixel_buffer_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            width,
            height,
            format,
        )
    }

    /// Acquire a caller-held storage buffer sized for `layout`.
    /// See [`GpuContext::acquire_storage_buffer`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_storage_buffer(
        &self,
        layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<OneOffAcquisitionCountedForItsNode<crate::core::rhi::StorageBuffer>> {
        acquire_storage_buffer_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            layout,
        )
    }

    /// Acquire a HOST_VISIBLE uniform buffer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        self.host_inner().acquire_uniform_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        self.host_inner().acquire_vertex_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE index buffer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        self.host_inner().acquire_index_buffer(byte_size)
    }

    /// Allocate a render-target-capable DMA-BUF VkImage (privileged path —
    /// host-only adapter primitive, customers never see this directly).
    /// See [`GpuContext::acquire_render_target_dma_buf_image`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    #[cfg(target_os = "linux")]
    pub fn acquire_render_target_dma_buf_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<OneOffAcquisitionCountedForItsNode<Texture>> {
        acquire_a_one_off_counted_against_its_node(
            self.one_off_acquisitions_of_the_node.as_ref(),
            OneOffAcquisitionShape::RenderTargetImage {
                width,
                height,
                format,
            },
            || {
                self.host_inner()
                    .acquire_render_target_dma_buf_image(width, height, format)
            },
        )
    }

    /// Allocate a render-target-capable VkImage over a private IOSurface
    /// (privileged path). See
    /// [`GpuContext::acquire_render_target_iosurface_image`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    #[cfg(target_os = "macos")]
    pub fn acquire_render_target_iosurface_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<OneOffAcquisitionCountedForItsNode<Texture>> {
        acquire_a_one_off_counted_against_its_node(
            self.one_off_acquisitions_of_the_node.as_ref(),
            OneOffAcquisitionShape::RenderTargetImage {
                width,
                height,
                format,
            },
            || {
                self.host_inner()
                    .acquire_render_target_iosurface_image(width, height, format)
            },
        )
    }

    /// Get a pixel buffer by its published surface id.
    pub fn get_pixel_buffer(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().get_pixel_buffer(surface_id)
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner()
            .resolve_pixel_buffer_by_surface_id(surface_id)
    }

    /// Register a texture in the same-process texture cache.
    pub fn register_texture(&self, id: &str, texture: Texture) {
        self.host_inner().register_texture(id, texture)
    }

    /// Register a texture with a declared initial Vulkan image layout.
    /// See [`GpuContext::register_texture_with_layout`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        self.host_inner()
            .register_texture_with_layout(id, texture, initial_layout)
    }

    /// Update a registered texture's tracked layout after a transition.
    /// See [`GpuContext::update_texture_registration_layout`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        self.host_inner()
            .update_texture_registration_layout(id, layout)
    }

    /// Resolve a VideoFrame's full registration record (texture + layout).
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<TextureRegistration> {
        self.host_inner()
            .resolve_texture_registration_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Resolve a VideoFrame's texture.
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
        self.host_inner()
            .resolve_texture_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Acquire a new output texture registered in the cache under a fresh
    /// UUID until the returned handle drops.
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_output_texture(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<(
        String,
        OneOffAcquisitionCountedForItsNode<OutputTextureRegisteredUntilDropped>,
    )> {
        let gpu = self.host_inner();
        acquire_a_one_off_counted_against_its_node(
            self.one_off_acquisitions_of_the_node.as_ref(),
            OneOffAcquisitionShape::Texture {
                width,
                height,
                format,
                usage: output_texture_usages(),
            },
            || {
                let (registered_under_id, texture) =
                    gpu.acquire_output_texture(width, height, format)?;
                Ok((
                    registered_under_id.clone(),
                    OutputTextureRegisteredUntilDropped {
                        texture,
                        registered_under_id,
                        gpu_the_texture_is_registered_with: gpu.clone(),
                    },
                ))
            },
        )
        .map(OneOffAcquisitionCountedForItsNode::split_off_the_id_it_was_acquired_under)
    }

    /// Upload a pixel buffer's contents to a GPU texture and register it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.host_inner()
            .upload_pixel_buffer_as_texture(surface_id, pixel_buffer, width, height)
    }

    /// Copy a host-visible pixel buffer's contents into an *already-allocated*
    /// device-local texture.
    ///
    /// See [`GpuContext::copy_pixel_buffer_to_texture`] for the
    /// underlying contract; the same primitive is exposed on
    /// [`GpuContextLimitedAccess`] for hot-path callers that already
    /// hold a texture (e.g. from a [`TextureRing`](crate::core::context::TextureRing)
    /// slot).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.host_inner().copy_pixel_buffer_to_texture(
            pixel_buffer,
            texture,
            surface_id,
            width,
            height,
        )
    }

    /// Pre-allocate a ring of `count` non-exportable DEVICE_LOCAL
    /// textures and register each in the same-process texture cache.
    ///
    /// The returned [`crate::core::context::TextureRing`] is the
    /// canonical engine helper for decode-output hot paths — replaces
    /// every per-frame `upload_pixel_buffer_as_texture` escalation
    /// with a one-shot setup-time allocation plus a sandbox-safe
    /// rotation in `process()`.
    /// for the recipe and CLAUDE.md → "Texture rings — single
    /// canonical abstraction" for the engine-model context.
    ///
    /// `count` is rejected if zero; sizing to
    /// `MAX_FRAMES_IN_FLIGHT = 2`
    /// is the standard
    /// for hot-path decoders.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_texture_ring(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
        usages: TextureUsages,
        count: usize,
    ) -> Result<crate::core::context::TextureRing> {
        self.host_inner()
            .create_texture_ring(width, height, format, usages, count)
    }

    /// Create a single-in-flight GPU→CPU texture readback bound to a
    /// fixed format/extent and return it as the layout-stable
    /// [`crate::core::rhi::TextureReadback`] handle. The staging
    /// buffer + command resources + timeline semaphore are allocated
    /// once at construction and reused across every submit; for parallel
    /// readbacks, hold N handles. Planar `Nv12` is rejected (the readback
    /// staging model assumes a flat interleaved plane).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_texture_readback(
        &self,
        label: &str,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<crate::core::rhi::TextureReadback> {
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
        Ok(crate::core::rhi::TextureReadback {
            handle,
            cached_handle_id,
            cached_staging_size,
            cached_width: width,
            cached_height: height,
            cached_format_raw: format as u32,
            _reserved_padding: 0,
        })
    }

    /// See [`GpuContext::unregister_texture`].
    pub fn unregister_texture(&self, id: &str) {
        self.host_inner().unregister_texture(id)
    }

    /// Get a reference to the RHI GPU device.
    ///
    /// **Engine-only** — returns `&Arc<GpuDevice>` which borrows into
    /// host-private state (the `Box<Arc<GpuContext>>` behind the
    /// handle). Consumers use the higher-level FullAccess methods
    /// (kernel construction, buffer/texture allocation, etc.) instead.
    pub fn device(&self) -> &Arc<GpuDevice> {
        self.host_inner().device()
    }

    /// Clone the host's `Arc<HostVulkanDevice>`. Engine-internal
    /// accessor for in-process RHI helpers (subprocess
    /// escalate handle assignment). Consumer GPU code builds
    /// through the FullAccess primitives, never the raw device.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn host_vulkan_device_arc(&self) -> Result<Arc<crate::vulkan::rhi::HostVulkanDevice>> {
        Ok(Arc::clone(
            crate::host_rhi::HostGpuDeviceExt::vulkan_device(self.host_inner().device().as_ref()),
        ))
    }

    /// Get the texture pool for acquiring pooled textures.
    ///
    /// **Engine-only** — returns `&TexturePool` which borrows into
    /// host-private state. Consumers use [`Self::acquire_texture`]
    /// instead.
    pub fn texture_pool(&self) -> &TexturePool {
        self.host_inner().texture_pool()
    }

    /// Acquire a pooled texture for in-process GPU work
    /// (`VK_IMAGE_TILING_OPTIMAL`). For cross-process render targets the
    /// host adapter layer wants on Linux, see
    /// [`Self::acquire_render_target_dma_buf_image`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_texture(
        &self,
        desc: &TexturePoolDescriptor,
    ) -> Result<OneOffAcquisitionCountedForItsNode<PooledTextureHandle>> {
        acquire_texture_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            desc,
        )
    }

    /// Get the shared command queue.
    ///
    /// Hands out a refcount-bumped owned [`RhiCommandQueue`] handle; its
    /// Drop decrements the inner Arc's strong count.
    pub fn command_queue(&self) -> RhiCommandQueue {
        self.host_inner().command_queue().clone()
    }

    /// Create a command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        self.host_inner().create_command_buffer()
    }

    /// Acquire a cached `(src, dst)`-keyed color converter. See
    /// [`GpuContext::color_converter`](crate::core::context::GpuContext::color_converter)
    /// on the inner context for usage.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn color_converter(&self, src: PixelFormat, dst: PixelFormat) -> Result<RhiColorConverter> {
        self.host_inner().color_converter(src, dst)
    }

    /// A color converter of the caller's own. See
    /// [`GpuContext::create_color_converter`](crate::core::context::GpuContext::create_color_converter).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_color_converter(
        &self,
        src: PixelFormat,
        dst: PixelFormat,
    ) -> Result<RhiColorConverter> {
        self.host_inner().create_color_converter(src, dst)
    }

    /// Create a compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Runs the host's [`GpuContext::create_compute_kernel`], which
    /// returns the kernel as `Arc::into_raw`; this wrapper reconstructs
    /// it via `Arc::from_raw`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_compute_kernel(
        &self,
        descriptor: &crate::core::rhi::ComputeKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanComputeKernel> {
        self.host_inner().create_compute_kernel(descriptor)
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_command_recorder(
        &self,
        label: &str,
    ) -> Result<crate::vulkan::rhi::RhiCommandRecorder> {
        self.host_inner().create_command_recorder(label)
    }

    /// Import an IOSurface's memory as a storage buffer, zero-copy. See
    /// [`GpuContext::import_iosurface_as_storage_buffer`].
    ///
    /// FullAccess-only: every import creates a buffer and imports memory.
    #[cfg(target_os = "macos")]
    pub fn import_iosurface_as_storage_buffer(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> Result<crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer> {
        self.host_inner()
            .import_iosurface_as_storage_buffer(iosurface)
    }

    /// Create a graphics kernel from a multi-stage SPIR-V set, binding
    /// declaration, and fixed-function pipeline state.
    ///
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_graphics_kernel(
        &self,
        descriptor: &crate::core::rhi::GraphicsKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanGraphicsKernel> {
        self.host_inner().create_graphics_kernel(descriptor)
    }

    /// Create a ray-tracing kernel from shader stages, shader-group
    /// layout, binding declaration, and push-constant range.
    ///
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_ray_tracing_kernel(
        &self,
        descriptor: &crate::core::rhi::RayTracingKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanRayTracingKernel> {
        self.host_inner().create_ray_tracing_kernel(descriptor)
    }

    /// Build a triangle-geometry bottom-level acceleration structure
    /// from CPU-side vertex + index data.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn build_triangles_blas(
        &self,
        label: &str,
        vertices: &[f32],
        indices: &[u32],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        self.host_inner()
            .build_triangles_blas(label, vertices, indices)
    }

    /// Build a top-level acceleration structure from BLAS instances.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn build_tlas(
        &self,
        label: &str,
        instances: &[crate::vulkan::rhi::TlasInstanceDesc],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        self.host_inner().build_tlas(label, instances)
    }

    /// Whether the underlying GPU exposes the
    /// `VK_KHR_ray_tracing_pipeline` extension chain.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        self.host_inner().supports_ray_tracing_pipeline()
    }

    /// Import a DMA-BUF FD as a `StorageBuffer` handle. Camera V4L2
    /// zero-copy path. Consumes `dma_buf_fd` whatever the outcome: the
    /// driver owns it behind a successful import and a failure closes it.
    #[cfg(target_os = "linux")]
    pub fn import_dma_buf_storage_buffer(
        &self,
        dma_buf_fd: std::os::fd::OwnedFd,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.host_inner()
            .import_dma_buf_storage_buffer(dma_buf_fd, byte_size)
    }

    /// Export a fresh dup'd DMA-BUF FD + byte size for a `PixelBuffer`.
    /// The fd transfers to the caller.
    ///
    /// A helper process reaching
    /// for a DMA-BUF handle goes through surface-share, which already
    /// carries the fd over `SCM_RIGHTS`.
    #[cfg(target_os = "linux")]
    pub fn export_pixel_buffer_dma_buf_fd(
        &self,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64)> {
        self.host_inner()
            .export_pixel_buffer_dma_buf_fd(pixel_buffer)
    }

    /// Allocate an OPAQUE_FD-exportable `VkBuffer` as a `StorageBuffer`
    /// (`device_local` picks VRAM-resident vs HOST_VISIBLE). The
    /// OPAQUE_FD/CUDA producer allocation (#1262).
    #[cfg(target_os = "linux")]
    pub fn create_opaque_fd_export_buffer(
        &self,
        byte_size: u64,
        device_local: bool,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.host_inner()
            .create_opaque_fd_export_buffer(byte_size, device_local)
    }

    /// Export a fresh dup'd OPAQUE_FD + byte size + exporting-device UUID
    /// from a `StorageBuffer`. The fd transfers to the caller.
    #[cfg(target_os = "linux")]
    pub fn export_storage_buffer_opaque_fd(
        &self,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        self.host_inner().export_storage_buffer_opaque_fd(buffer)
    }

    /// Wrap an OPAQUE_FD `StorageBuffer` as a `PixelBuffer` sharing the
    /// same allocation so it can register through the surface-store
    /// `register_pixel_buffer_with_timeline` path (#1262).
    #[cfg(target_os = "linux")]
    pub fn wrap_storage_buffer_as_pixel_buffer(
        &self,
        storage_buffer: &crate::core::rhi::StorageBuffer,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: crate::core::rhi::PixelFormat,
    ) -> Result<crate::core::rhi::PixelBuffer> {
        self.host_inner().wrap_storage_buffer_as_pixel_buffer(
            storage_buffer,
            width,
            height,
            bytes_per_pixel,
            format,
        )
    }

    /// Per-frame CUDA producer copy: image→buffer in one host-device
    /// submission with optional `consume_done` wait + `produce_done`
    /// signal (#1262).
    #[cfg(target_os = "linux")]
    pub fn copy_texture_to_storage_buffer_and_signal(
        &self,
        source_texture: &crate::core::rhi::Texture,
        source_layout: crate::core::rhi::VulkanLayout,
        dst: &crate::core::rhi::StorageBuffer,
        consume_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
        produce_done: Option<(&crate::vulkan::rhi::HostVulkanTimelineSemaphore, u64)>,
    ) -> Result<()> {
        self.host_inner().copy_texture_to_storage_buffer_and_signal(
            source_texture,
            source_layout,
            dst,
            consume_done,
            produce_done,
        )
    }

    /// Read-once GPU capability snapshot. Backs the camera processor's
    /// vendor-name / external-memory / cross-device-DMA-BUF-probe
    /// branching without exposing host-internal `HostVulkanDevice`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn gpu_capabilities(&self) -> Result<GpuCapabilitiesSnapshot> {
        Ok(self.host_inner().gpu_capabilities())
    }

    /// Copy pixels between same-format, same-size buffers.
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        self.host_inner().blit_copy(src, dest)
    }

    /// Clear the blitter's texture cache to free GPU memory.
    ///
    /// **Engine-only** — engine setup-time housekeeping.
    pub fn clear_blitter_cache(&self) {
        self.host_inner().clear_blitter_cache();
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.host_inner().surface_store()
    }

    /// Check in a pixel buffer to the surface-share service.
    pub fn check_in_surface(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        self.host_inner().check_in_surface(pixel_buffer)
    }

    /// Check out a surface by ID.
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().check_out_surface(surface_id)
    }

    /// Build a compute kernel, reusing an identical one this context already
    /// built. Reachable only inside `escalate(|full| ...)` since it requires
    /// `FullAccess`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_compute_kernel(
        &self,
        spv: &[u8],
        push_constant_size: u32,
        declared_bindings: &[crate::core::rhi::ComputeBindingDeclaration],
        entry_point: &str,
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanComputeKernel>)> {
        self.host_inner().create_or_reuse_compute_kernel(
            spv,
            push_constant_size,
            declared_bindings,
            entry_point,
        )
    }

    /// Look up a compute kernel a prior `create_or_reuse_compute_kernel`
    /// returned.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compute_kernel_by_id(
        &self,
        kernel_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.host_inner().compute_kernel_by_id(kernel_id)
    }

    /// [`GpuContext::dispatch_compute_kernel_batch`] — N dispatches, one
    /// submission, one fence wait.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn dispatch_compute_kernel_batch(
        &self,
        batch: &[BatchedComputeKernelDispatch],
    ) -> Result<()> {
        self.host_inner().dispatch_compute_kernel_batch(batch)
    }

    /// Runs the host's [`GpuContext::create_or_reuse_graphics_kernel`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_graphics_kernel(
        &self,
        label: &str,
        stages: &[crate::core::rhi::GraphicsStage<'_>],
        declared_push_constants: crate::core::rhi::GraphicsPushConstants,
        pipeline_state: &crate::core::rhi::GraphicsPipelineState,
        descriptor_sets_in_flight: u32,
        declared_bindings: &[crate::core::rhi::GraphicsBindingDeclaration],
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanGraphicsKernel>)> {
        self.host_inner().create_or_reuse_graphics_kernel(
            label,
            stages,
            declared_push_constants,
            pipeline_state,
            descriptor_sets_in_flight,
            declared_bindings,
        )
    }

    /// [`GpuContext::graphics_kernel_by_id_held_for_one_draw`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn graphics_kernel_by_id_held_for_one_draw(
        &self,
        kernel_id: &str,
    ) -> Option<GraphicsKernelHeldForOneDraw<'_>> {
        self.host_inner()
            .graphics_kernel_by_id_held_for_one_draw(kernel_id)
    }

    /// Runs the host's [`GpuContext::create_or_reuse_ray_tracing_kernel`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_ray_tracing_kernel(
        &self,
        label: &str,
        stages: &[crate::core::rhi::RayTracingStage<'_>],
        groups: &[crate::core::rhi::RayTracingShaderGroup],
        declared_push_constants: crate::core::rhi::RayTracingPushConstants,
        max_recursion_depth: u32,
        declared_bindings: &[crate::core::rhi::RayTracingBindingDeclaration],
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanRayTracingKernel>)> {
        self.host_inner().create_or_reuse_ray_tracing_kernel(
            label,
            stages,
            groups,
            declared_push_constants,
            max_recursion_depth,
            declared_bindings,
        )
    }

    /// [`GpuContext::ray_tracing_kernel_by_id_held_for_one_trace`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn ray_tracing_kernel_by_id_held_for_one_trace(
        &self,
        kernel_id: &str,
    ) -> Option<RayTracingKernelHeldForOneTrace<'_>> {
        self.host_inner()
            .ray_tracing_kernel_by_id_held_for_one_trace(kernel_id)
    }

    /// Runs the host's [`GpuContext::register_acceleration_structure`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_acceleration_structure(
        &self,
        acceleration_structure: crate::vulkan::rhi::VulkanAccelerationStructure,
    ) -> String {
        self.host_inner()
            .register_acceleration_structure(acceleration_structure)
    }

    /// Look up an acceleration structure a prior
    /// `register_acceleration_structure` returned the id of.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acceleration_structure_by_id(
        &self,
        acceleration_structure_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanAccelerationStructure>> {
        self.host_inner()
            .acceleration_structure_by_id(acceleration_structure_id)
    }

    /// Runs the host's [`GpuContext::release_acceleration_structure`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn release_acceleration_structure(&self, acceleration_structure_id: &str) -> bool {
        self.host_inner()
            .release_acceleration_structure(acceleration_structure_id)
    }
}

impl std::fmt::Debug for GpuContextLimitedAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContextLimitedAccess")
            .field("handle", &self.handle)
            .finish()
    }
}

impl std::fmt::Debug for GpuContextFullAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContextFullAccess")
            .field("handle", &self.handle)
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
