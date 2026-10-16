// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::context::{
    OneOffAcquisitionCountedForItsNode, OneOffAcquisitionShape,
    OneOffAcquisitionsCountedForOneNativeNode, TextureRegistration,
    acquire_a_one_off_counted_against_its_node,
};
use crate::core::media_clock::MediaClock;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::core::rhi::KernelShaderStageMask;
use crate::core::rhi::{
    CommandBuffer, GpuDevice, PixelBuffer, PixelBufferPoolSlotId, PixelFormat,
    PublishedPixelBufferFrameId, RhiBlitter, RhiColorConverter, RhiCommandQueue,
    RhiPixelBufferPool, Texture, TextureDescriptor, TextureFormat, TextureUsages,
    pool_slot_key_of_surface_id,
};
use crate::core::{Error, Result};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::host_rhi::HostTextureExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use streamlib_consumer_rhi::VulkanLayout;

/// Number of buffers to pre-allocate per pool.
const POOL_PRE_ALLOCATE_COUNT: usize = 4;

/// How many buffers a shape's shared ring adds when every buffer it holds is held.
const PIXEL_BUFFERS_A_SHARED_RING_ADDS_WHEN_EVERY_ONE_IS_HELD: usize = 4;

/// The most surface-store lookups the buffer cache keeps before it evicts them.
const MAX_BUFFER_CACHE_SIZE: usize = 512;

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

use super::lease_aware_pool_slot_ring::{
    LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot, LeaseAwarePoolSlotResource,
    LeaseAwarePoolSlotRing, PoolSlotReuse,
};
use super::shared_pool_entry_freeing::{
    LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK,
    LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES,
    SharedPoolEntriesAwaitingDestruction, SharedPoolEntryFreeingRecord, SharedPoolEntryFreeingScan,
    SharedPoolEntryIdleWindow, SharedPoolEntryObservationCursor, SharedPoolEntryTakenOutOfItsPool,
};
use super::surface_store::SurfaceStore;
use super::texture_pool::{PooledTextureHandle, TexturePool, TexturePoolDescriptor};
use crate::core::runtime::LoadedStreamTag;

/// The shape a shared pixel-buffer ring holds buffers of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PixelBufferPoolKey {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) format: PixelFormat,
}

/// One pixel-buffer pool slot's resource, as the lease-aware ring sees it.
pub(super) struct PixelBufferPoolSlot {
    pub(super) buffer: PixelBuffer,
    pub(super) freeing_record: SharedPoolEntryFreeingRecord,
}

impl PixelBufferPoolSlot {
    fn allocated_for(buffer: PixelBuffer, acquiring_stream: Option<LoadedStreamTag>) -> Self {
        Self {
            buffer,
            freeing_record: SharedPoolEntryFreeingRecord::handed_out_to(acquiring_stream),
        }
    }
}

impl LeaseAwarePoolSlotResource for PixelBufferPoolSlot {
    /// `PixelBuffer` holds an opaque handle to a host-side `Arc`; the
    /// baseline is 2 — one share in the ring that holds the slot, the shared
    /// ring or the native node output pool it is lent to, and one under the
    /// current published id in `buffer_cache` (1 before the slot ever
    /// publishes, when no consumer can hold it either) — so anything above
    /// that is a live reader.
    fn is_held_in_this_process(&self) -> bool {
        self.buffer.strong_count() > 2
    }

    /// Whether the kernel reports the slot's IOSurface in use by any
    /// process: a Mach port to it still in flight, or a use count a helper
    /// holds. Kernel-truthful, and cleared by the kernel when that process
    /// dies, so it holds a slot even where no checkout lease was taken.
    #[cfg(target_os = "macos")]
    fn is_in_use_per_the_platform(&self) -> bool {
        self.buffer
            .buffer_ref()
            .inner
            .backing_iosurface()
            .is_some_and(objc2_io_surface::IOSurfaceRef::is_in_use)
    }
}

/// The published frame id a pixel-buffer slot's most recent hand-off minted.
pub(super) fn published_pixel_buffer_frame_id(
    slot: &LeaseAwarePoolSlot<PixelBufferPoolSlot>,
) -> PublishedPixelBufferFrameId {
    PublishedPixelBufferFrameId::new(
        PixelBufferPoolSlotId::from_str(slot.pool_slot_key()),
        slot.published_frame_generation(),
    )
}

/// Ring pool of pixel buffers for a given (width, height, format).
///
/// Buffers are pre-allocated at pool creation. `acquire()` cycles through
/// buffers, skipping any currently in use, and grows when every one is held;
/// no count caps it. A slot is freed once unheld and idle, or once the stream
/// that last acquired it stops. A slot may be lent to a native node's output
/// pool, which publishes from it under its own count; at that pool's teardown
/// an unheld slot is freed and a held one comes back, generation kept.
struct PixelBufferRingPool {
    /// The allocator the ring's buffers came from, holding its own share of
    /// each until the slot is freed.
    pool: RhiPixelBufferPool,
    ring: LeaseAwarePoolSlotRing<PixelBufferPoolSlot>,
    /// Slots out on loan to native nodes' output pools; the ring is dropped
    /// only once it is empty and none is out.
    slots_lent_to_native_node_output_pools: usize,
}

/// The pixel buffers this process resolves by surface id with no surface-share
/// round trip: each pool slot's current published frame, and the surface-store
/// lookups [`GpuContext::get_pixel_buffer`] caches under ids that name no
/// published frame.
#[derive(Default)]
struct PixelBufferCacheBySurfaceId {
    buffers_by_surface_id: HashMap<String, PixelBuffer>,
    /// How many entries are surface-store lookups: the only ones
    /// [`MAX_BUFFER_CACHE_SIZE`] counts and evicts.
    surface_store_lookup_count: usize,
}

/// Shared pixel buffer pool manager.
///
/// Manages ring pools keyed by (width, height, format).
/// Pre-allocates buffers on pool creation and registers them with the surface-share service.
/// Each buffer lives in a shared ring, or lent from it to a native node's
/// output pool, until it is freed.
pub(super) struct PixelBufferPoolManager {
    pools: Mutex<HashMap<PixelBufferPoolKey, PixelBufferRingPool>>,
    /// Global cache for UUID -> PixelBuffer lookups (includes buffers from all pools).
    /// Used by consumers (e.g., display processor) to resolve UUIDs received via IPC.
    buffer_cache: Mutex<PixelBufferCacheBySurfaceId>,
    /// The generation every lease-aware ring most recently minted per slot,
    /// shared with the [`GpuContext`] that owns this manager.
    minted_frame_generations: Arc<LeaseAwarePoolMintedFrameGenerations>,
    /// The engine's own surface store, once installed: every slot registers
    /// through it, and its lease table is where a helper's checkout shows.
    engines_own_surface_store: Arc<std::sync::OnceLock<SurfaceStore>>,
    /// The [`GpuContext`]'s textures synthesized from slots' pixel buffers,
    /// keyed by slot; a freed slot's entry goes with it.
    buffer_texture_cache: Arc<Mutex<HashMap<String, Texture>>>,
    idle_window: SharedPoolEntryIdleWindow,
    observation_cursor: SharedPoolEntryObservationCursor,
    slots_awaiting_destruction:
        SharedPoolEntriesAwaitingDestruction<PixelBufferPoolSlotTakenOutOfItsRing>,
    device: Arc<GpuDevice>,
}

/// Cache key for a compute kernel built from a pre-compiled blob.
///
/// The blob fixes stage, entry point and target environment on its own, so the
/// bytes plus the declared push-constant size are the whole key. A GLSL source
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

impl PixelBufferPoolManager {
    fn new(
        device: Arc<GpuDevice>,
        minted_frame_generations: Arc<LeaseAwarePoolMintedFrameGenerations>,
        engines_own_surface_store: Arc<std::sync::OnceLock<SurfaceStore>>,
        buffer_texture_cache: Arc<Mutex<HashMap<String, Texture>>>,
    ) -> Self {
        Self {
            pools: Mutex::new(HashMap::new()),
            buffer_cache: Mutex::new(PixelBufferCacheBySurfaceId::default()),
            minted_frame_generations,
            engines_own_surface_store,
            buffer_texture_cache,
            idle_window: SharedPoolEntryIdleWindow::default(),
            observation_cursor: SharedPoolEntryObservationCursor::default(),
            slots_awaiting_destruction: SharedPoolEntriesAwaitingDestruction::new(
                "pixel-buffer pool",
                Arc::clone(&device),
            ),
            device,
        }
    }

    /// The shared rings, locked; a panic under an earlier hold leaves them
    /// usable rather than poisoning every later acquire.
    fn lock_the_shared_rings(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<PixelBufferPoolKey, PixelBufferRingPool>> {
        self.pools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Acquire a buffer of the shape from its shared ring, created on first
    /// use with POOL_PRE_ALLOCATE_COUNT buffers registered with the
    /// surface-share service.
    ///
    /// Returns the next available buffer from the ring, skipping any in use,
    /// and grows the ring when every buffer is held; past the device's memory
    /// budget that growth is refused by name and nothing waits.
    /// The id names the frame this hand-off publishes: the slot's next
    /// acquisition retires it, so a consumer that outwaits the pool resolves
    /// an error, never another frame's pixels. The slot is attributed to
    /// `acquiring_stream`, and at most one slot due to be freed is freed.
    fn acquire(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        let mut pools = self.lock_the_shared_rings();
        let handed_off =
            self.hand_off_a_slot_of_the_shape(&mut pools, width, height, format, acquiring_stream);
        let slot_due_to_be_freed = self.take_out_at_most_one_slot_due_to_be_freed(&mut pools);
        drop(pools);
        self.park_the_slots_taken_out_and_destroy_at_most_one_ready(slot_due_to_be_freed);
        handed_off
    }

    fn hand_off_a_slot_of_the_shape(
        &self,
        pools: &mut HashMap<PixelBufferPoolKey, PixelBufferRingPool>,
        width: u32,
        height: u32,
        format: PixelFormat,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        let shape = PixelBufferPoolKey {
            width,
            height,
            format,
        };
        let surface_store = self.engines_own_surface_store.get();
        let ring_pool = self.ring_pool_for_shape(pools, shape, surface_store, acquiring_stream)?;

        // A slot is free only when nobody holds it in this address space and
        // nobody holds it out of one. The first is an Arc refcount, the second
        // a checkout lease — see
        // `docs/decisions/surface-id-lifetime-contract.md` — and on macOS also
        // the kernel's own in-use answer for the slot's IOSurface.
        let check_out_leases = surface_store.and_then(SurfaceStore::check_out_lease_registry);
        if let Some(slot_index) = ring_pool
            .ring
            .hand_off_a_reusable_slot(check_out_leases, &self.minted_frame_generations)
        {
            let handed_off_slot = ring_pool.ring.slot_mut(slot_index);
            handed_off_slot
                .resource_mut()
                .freeing_record
                .note_handed_out_again_to(acquiring_stream);
            let (published, handed_off_buffer) =
                self.publish_the_handed_off_frame_in_the_buffer_cache(handed_off_slot);
            tracing::trace!(
                "PixelBufferPoolManager: acquired buffer {} (idx {})",
                published,
                slot_index
            );
            return Ok((published, handed_off_buffer));
        }

        let slot_count_before_growing = ring_pool.ring.slot_count();
        let handed_off_slot_index =
            Self::allocate_and_register_one_more_slot(ring_pool, surface_store, acquiring_stream)?;
        for _ in 1..PIXEL_BUFFERS_A_SHARED_RING_ADDS_WHEN_EVERY_ONE_IS_HELD {
            if let Err(refusal) = Self::allocate_and_register_one_more_slot(
                ring_pool,
                surface_store,
                acquiring_stream,
            ) {
                tracing::debug!(
                    "PixelBufferPoolManager: the {}x{} {:?} ring grew by fewer buffers than it \
                     asked for: {}",
                    width,
                    height,
                    format,
                    refusal
                );
                break;
            }
        }
        tracing::info!(
            "PixelBufferPoolManager: no free buffer in the {}x{} {:?} shared ring; grew it from \
             {} to {} buffers",
            width,
            height,
            format,
            slot_count_before_growing,
            ring_pool.ring.slot_count()
        );

        // Hand off the first buffer added — a fresh allocation no caller has
        // seen.
        ring_pool.ring.hand_off_fresh_slot(
            handed_off_slot_index,
            check_out_leases,
            &self.minted_frame_generations,
        );
        Ok(self.publish_the_handed_off_frame_in_the_buffer_cache(
            ring_pool.ring.slot(handed_off_slot_index),
        ))
    }

    /// Take one slot of `shape` out of its shared ring for a native node's
    /// output pool running in `acquiring_stream` — an unheld slot, or a fresh
    /// one when every slot is held — minted, published and filed in the
    /// buffer cache under one hold of the pools; past the device's memory
    /// budget the allocation is refused by name. Frees at most one slot due
    /// to be freed.
    pub(super) fn lend_a_slot_to_a_native_node_output_pool(
        &self,
        shape: PixelBufferPoolKey,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<LeaseAwarePoolSlot<PixelBufferPoolSlot>> {
        let mut pools = self.lock_the_shared_rings();
        let lent_slot = self.lend_a_slot_of_the_shape(&mut pools, shape, acquiring_stream);
        let slot_due_to_be_freed = self.take_out_at_most_one_slot_due_to_be_freed(&mut pools);
        drop(pools);
        self.park_the_slots_taken_out_and_destroy_at_most_one_ready(slot_due_to_be_freed);
        lent_slot
    }

    fn lend_a_slot_of_the_shape(
        &self,
        pools: &mut HashMap<PixelBufferPoolKey, PixelBufferRingPool>,
        shape: PixelBufferPoolKey,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<LeaseAwarePoolSlot<PixelBufferPoolSlot>> {
        let surface_store = self.engines_own_surface_store.get();
        let ring_pool = self.ring_pool_for_shape(pools, shape, surface_store, acquiring_stream)?;
        let check_out_leases = surface_store.and_then(SurfaceStore::check_out_lease_registry);
        let lent_slot_index = match ring_pool
            .ring
            .hand_off_a_reusable_slot(check_out_leases, &self.minted_frame_generations)
        {
            Some(reused_slot_index) => reused_slot_index,
            None => {
                let fresh_slot_index = Self::allocate_and_register_one_more_slot(
                    ring_pool,
                    surface_store,
                    acquiring_stream,
                )?;
                ring_pool.ring.hand_off_fresh_slot(
                    fresh_slot_index,
                    check_out_leases,
                    &self.minted_frame_generations,
                );
                fresh_slot_index
            }
        };
        let lent_slot = ring_pool.ring.slot_mut(lent_slot_index);
        lent_slot
            .resource_mut()
            .freeing_record
            .note_handed_out_again_to(acquiring_stream);
        self.publish_the_handed_off_frame_in_the_buffer_cache(lent_slot);
        ring_pool.slots_lent_to_native_node_output_pools += 1;
        Ok(ring_pool.ring.take_out_the_slot_at(lent_slot_index))
    }

    /// Hand slots a native node's output pool borrowed back to their shape's
    /// shared ring, generations kept, so each one's next hand-off there
    /// retires the last frame the node published from it.
    pub(super) fn take_back_slots_lent_to_a_native_node_output_pool(
        &self,
        shape: PixelBufferPoolKey,
        lent_slots: impl IntoIterator<Item = LeaseAwarePoolSlot<PixelBufferPoolSlot>>,
    ) {
        let mut pools = self.lock_the_shared_rings();
        let Some(ring_pool) = pools.get_mut(&shape) else {
            warn_of_no_shared_ring_to_take_back_slots_lent_to_a_native_node(
                shape,
                lent_slots.into_iter().count(),
            );
            return;
        };
        for lent_slot in lent_slots {
            ring_pool.note_a_lent_slot_came_back(shape);
            ring_pool
                .ring
                .push_a_slot_keeping_its_published_frame_generation(lent_slot);
        }
    }

    /// The slots a torn-down native node's output pool borrowed: each one
    /// nothing holds is freed, and each one still held goes back to its
    /// shape's shared ring, generation kept, to be freed as soon as it is
    /// unheld. Answers how many were destroyed; a freed slot whose GPU work
    /// has not completed is destroyed by a later acquire or release.
    pub(super) fn free_or_take_back_the_slots_of_a_torn_down_native_node_output_pool(
        &self,
        shape: PixelBufferPoolKey,
        lent_slots: impl IntoIterator<Item = LeaseAwarePoolSlot<PixelBufferPoolSlot>>,
    ) -> usize {
        let scan = SharedPoolEntryFreeingScan::starting_now(&self.idle_window);
        let mut pools = self.lock_the_shared_rings();
        let Some(ring_pool) = pools.get_mut(&shape) else {
            warn_of_no_shared_ring_to_take_back_slots_lent_to_a_native_node(
                shape,
                lent_slots.into_iter().count(),
            );
            return 0;
        };
        let mut cross_process_holders =
            PoolSlotReuse::over_the_engines_own_surface_store(&self.engines_own_surface_store);
        let mut slots_taken_out = Vec::new();
        for mut lent_slot in lent_slots {
            ring_pool.note_a_lent_slot_came_back(shape);
            let is_unheld = lent_slot.is_unheld(&cross_process_holders);
            let freeing_record = &mut lent_slot.resource_mut().freeing_record;
            freeing_record.flag_freed_as_soon_as_unheld();
            if scan.observe(freeing_record, is_unheld) {
                cross_process_holders.retire_every_frame_a_slot_being_freed_published(
                    &lent_slot,
                    &self.minted_frame_generations,
                );
                slots_taken_out.push(self.forget_the_slot_in_its_rings_allocator_and_every_cache(
                    &mut ring_pool.pool,
                    lent_slot,
                ));
            } else {
                ring_pool
                    .ring
                    .push_a_slot_keeping_its_published_frame_generation(lent_slot);
            }
        }
        drop(cross_process_holders);
        Self::drop_the_ring_of_the_shape_once_it_is_empty(&mut pools, shape);
        drop(pools);
        let slots_taken_out =
            self.release_the_surface_share_registrations_of_the_slots_taken_out(slots_taken_out);
        self.slots_awaiting_destruction
            .park_and_destroy_every_one_ready(slots_taken_out)
    }

    /// Free at most one slot due to be freed — idle past the window, or
    /// flagged — for a producer whose acquire did not reach the shared rings.
    pub(super) fn free_at_most_one_slot_due_to_be_freed(&self) {
        let mut pools = self.lock_the_shared_rings();
        let slot_due_to_be_freed = self.take_out_at_most_one_slot_due_to_be_freed(&mut pools);
        drop(pools);
        self.park_the_slots_taken_out_and_destroy_at_most_one_ready(slot_due_to_be_freed);
    }

    /// Observe the few slots of the shared rings this call's window covers,
    /// held or unheld, and take out and retire at most one that is due to be
    /// freed: this runs on a producer's acquire.
    fn take_out_at_most_one_slot_due_to_be_freed(
        &self,
        pools: &mut HashMap<PixelBufferPoolKey, PixelBufferRingPool>,
    ) -> Option<PixelBufferPoolSlotTakenOutOfItsRing> {
        let scan = SharedPoolEntryFreeingScan::starting_now(&self.idle_window);
        let mut cross_process_holders =
            PoolSlotReuse::over_the_engines_own_surface_store(&self.engines_own_surface_store);
        let window = self.observation_cursor.next_window_over(
            pools
                .values()
                .map(|ring_pool| ring_pool.ring.slot_count())
                .sum(),
        );
        let mut position_of_the_rings_first_slot = 0;
        let mut slot_due_to_be_freed = None;
        let mut shape_of_the_slot_due_to_be_freed = None;
        for (shape, ring_pool) in pools.iter_mut() {
            let slot_count = ring_pool.ring.slot_count();
            let slot_indices_to_observe =
                window.indices_within_the_group(position_of_the_rings_first_slot, slot_count);
            position_of_the_rings_first_slot += slot_count;
            let mut a_slot_is_chosen = slot_due_to_be_freed.is_some();
            let mut slots_taken_out = ring_pool.ring.take_out_and_retire_every_unheld_slot_chosen(
                slot_indices_to_observe,
                &mut cross_process_holders,
                &self.minted_frame_generations,
                |slot, is_unheld| {
                    let is_due_to_be_freed =
                        scan.observe(&mut slot.resource_mut().freeing_record, is_unheld);
                    let is_chosen = is_due_to_be_freed && !a_slot_is_chosen;
                    a_slot_is_chosen |= is_chosen;
                    is_chosen
                },
            );
            if let Some(slot_taken_out) = slots_taken_out.pop() {
                slot_due_to_be_freed =
                    Some(self.forget_the_slot_in_its_rings_allocator_and_every_cache(
                        &mut ring_pool.pool,
                        slot_taken_out,
                    ));
                shape_of_the_slot_due_to_be_freed = Some(*shape);
            }
        }
        drop(cross_process_holders);
        if let Some(shape) = shape_of_the_slot_due_to_be_freed {
            Self::drop_the_ring_of_the_shape_once_it_is_empty(pools, shape);
        }
        slot_due_to_be_freed
    }

    /// Flag every slot of the shared rings `stream` last acquired to be freed
    /// as soon as it is unheld, answering their pool slot keys — what a
    /// helper's checkout lease on any of them names.
    pub(super) fn flag_every_slot_the_stream_last_acquired(
        &self,
        stream: LoadedStreamTag,
    ) -> Vec<String> {
        let mut pools = self.lock_the_shared_rings();
        let mut pool_slot_keys_of_the_flagged_slots = Vec::new();
        for ring_pool in pools.values_mut() {
            for slot in ring_pool.ring.slots_mut() {
                let freeing_record = &mut slot.resource_mut().freeing_record;
                if freeing_record.was_last_acquired_by(stream) {
                    freeing_record.flag_freed_as_soon_as_unheld();
                    pool_slot_keys_of_the_flagged_slots.push(slot.pool_slot_key().to_string());
                }
            }
        }
        pool_slot_keys_of_the_flagged_slots
    }

    /// Flag the slot whose buffer is `buffer` to be freed as soon as it is
    /// unheld — for a one-off whose helper was torn down.
    pub(super) fn flag_the_slot_of_the_buffer_freed_as_soon_as_unheld(&self, buffer: &PixelBuffer) {
        let mut pools = self.lock_the_shared_rings();
        if let Some(slot) = pools
            .values_mut()
            .flat_map(|ring_pool| ring_pool.ring.slots_mut())
            .find(|slot| std::ptr::eq(slot.resource().buffer.buffer_ref(), buffer.buffer_ref()))
        {
            slot.resource_mut()
                .freeing_record
                .flag_freed_as_soon_as_unheld();
        }
    }

    /// Free every unheld slot due to be freed — flagged, or idle past the
    /// window — waiting at most
    /// [`LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK`] for
    /// the GPU work naming the slots awaiting destruction; answers how many
    /// were destroyed.
    pub(super) fn free_every_unheld_slot_due_to_be_freed_waiting_for_its_gpu_work(&self) -> usize {
        let scan = SharedPoolEntryFreeingScan::starting_now(&self.idle_window);
        let mut pools = self.lock_the_shared_rings();
        let mut cross_process_holders =
            PoolSlotReuse::over_the_engines_own_surface_store(&self.engines_own_surface_store);
        let mut slots_taken_out = Vec::new();
        for ring_pool in pools.values_mut() {
            let every_slot_index = 0..ring_pool.ring.slot_count();
            let slots_of_the_ring_taken_out =
                ring_pool.ring.take_out_and_retire_every_unheld_slot_chosen(
                    every_slot_index,
                    &mut cross_process_holders,
                    &self.minted_frame_generations,
                    |slot, is_unheld| {
                        scan.observe(&mut slot.resource_mut().freeing_record, is_unheld)
                    },
                );
            for slot_taken_out in slots_of_the_ring_taken_out {
                slots_taken_out.push(self.forget_the_slot_in_its_rings_allocator_and_every_cache(
                    &mut ring_pool.pool,
                    slot_taken_out,
                ));
            }
        }
        drop(cross_process_holders);
        pools.retain(|_, ring_pool| !ring_pool.is_empty_with_no_slot_out_on_loan());
        drop(pools);
        let slots_taken_out =
            self.release_the_surface_share_registrations_of_the_slots_taken_out(slots_taken_out);
        self.slots_awaiting_destruction
            .park_and_destroy_every_one_ready_waiting_at_most(
                slots_taken_out,
                LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK,
            )
    }

    /// Let go of the shares of a slot taken out of its ring and retired that
    /// its ring's allocator and the buffer cache hold, and take its canvas
    /// out of the texture cache, under the hold of the pools that took it
    /// out: no new holder can reach it after this.
    fn forget_the_slot_in_its_rings_allocator_and_every_cache(
        &self,
        rings_allocator: &mut RhiPixelBufferPool,
        slot_taken_out: LeaseAwarePoolSlot<PixelBufferPoolSlot>,
    ) -> PixelBufferPoolSlotTakenOutOfItsRing {
        let pool_slot_key = slot_taken_out.pool_slot_key();
        if !rings_allocator
            .forget_the_buffer_of_pool_slot(&PixelBufferPoolSlotId::from_str(pool_slot_key))
        {
            tracing::warn!(
                pool_slot = pool_slot_key,
                "PixelBufferPoolManager: the ring's allocator held no buffer for a slot being freed"
            );
        }
        if slot_taken_out.published_frame_generation() > 0 {
            self.buffer_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .buffers_by_surface_id
                .remove(&slot_taken_out.currently_published_frame_id());
        }
        let canvas_texture_synthesized_from_the_slot = self
            .buffer_texture_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(pool_slot_key);
        PixelBufferPoolSlotTakenOutOfItsRing {
            slot: slot_taken_out,
            canvas_texture_synthesized_from_the_slot,
        }
    }

    /// Release the surface-share registration of each slot taken out — a
    /// round trip to the service, so outside the hold of the pools.
    fn release_the_surface_share_registrations_of_the_slots_taken_out(
        &self,
        slots_taken_out: impl IntoIterator<Item = PixelBufferPoolSlotTakenOutOfItsRing>,
    ) -> Vec<PixelBufferPoolSlotTakenOutOfItsRing> {
        let slots_taken_out: Vec<_> = slots_taken_out.into_iter().collect();
        if let Some(store) = self.engines_own_surface_store.get() {
            for slot_taken_out in &slots_taken_out {
                let pool_slot_key = slot_taken_out.slot.pool_slot_key();
                if let Err(unreleased) = store.release(pool_slot_key) {
                    tracing::warn!(
                        "PixelBufferPoolManager: could not release freed slot {} from the \
                         surface-share service: {}",
                        pool_slot_key,
                        unreleased
                    );
                }
            }
        }
        slots_taken_out
    }

    /// Release the registrations of the slots taken out, then destroy at
    /// most one slot that may be destroyed: this runs on a producer's acquire,
    /// which pays one surface-share round trip per slot taken out and never a
    /// GPU wait.
    fn park_the_slots_taken_out_and_destroy_at_most_one_ready(
        &self,
        slots_taken_out: impl IntoIterator<Item = PixelBufferPoolSlotTakenOutOfItsRing>,
    ) {
        self.slots_awaiting_destruction
            .park_and_destroy_at_most_one_ready(
                self.release_the_surface_share_registrations_of_the_slots_taken_out(
                    slots_taken_out,
                ),
            );
    }

    fn drop_the_ring_of_the_shape_once_it_is_empty(
        pools: &mut HashMap<PixelBufferPoolKey, PixelBufferRingPool>,
        shape: PixelBufferPoolKey,
    ) {
        if pools
            .get(&shape)
            .is_some_and(PixelBufferRingPool::is_empty_with_no_slot_out_on_loan)
        {
            pools.remove(&shape);
        }
    }

    /// File the frame a hand-off just minted on `slot` in the buffer cache,
    /// answering its published id and a share of its buffer.
    pub(super) fn publish_the_handed_off_frame_in_the_buffer_cache(
        &self,
        slot: &LeaseAwarePoolSlot<PixelBufferPoolSlot>,
    ) -> (PublishedPixelBufferFrameId, PixelBuffer) {
        let handed_off_buffer = slot.resource().buffer.clone();
        self.retire_previous_frame_in_cache(slot, &handed_off_buffer);
        (published_pixel_buffer_frame_id(slot), handed_off_buffer)
    }

    /// The generation every lease-aware ring most recently minted per slot.
    pub(super) fn minted_frame_generations(&self) -> &LeaseAwarePoolMintedFrameGenerations {
        &self.minted_frame_generations
    }

    /// The shared ring of `shape`, created on its first acquire with its
    /// first buffers pre-allocated, registered with the surface-share
    /// service, and attributed to `acquiring_stream`.
    fn ring_pool_for_shape<'pools>(
        &self,
        pools: &'pools mut HashMap<PixelBufferPoolKey, PixelBufferRingPool>,
        shape: PixelBufferPoolKey,
        surface_store: Option<&SurfaceStore>,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<&'pools mut PixelBufferRingPool> {
        let entry = match pools.entry(shape) {
            std::collections::hash_map::Entry::Occupied(entry) => return Ok(entry.into_mut()),
            std::collections::hash_map::Entry::Vacant(entry) => entry,
        };
        let PixelBufferPoolKey {
            width,
            height,
            format,
        } = shape;
        tracing::debug!(
            "PixelBufferPoolManager: creating new pool for {}x{} {:?}",
            width,
            height,
            format
        );
        let underlying_pool = RhiPixelBufferPool {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            _marker: std::marker::PhantomData,
        };

        // Pre-allocate all buffers at once (hold them simultaneously)
        let mut ring = LeaseAwarePoolSlotRing::default();
        let mut registered_count = 0;

        tracing::debug!(
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

                    // The global cache gets an entry per *published
                    // frame*, at hand-off — a slot that has published
                    // nothing has no id anybody could resolve.
                    ring.push_fresh_slot(
                        pool_id.as_str().to_string(),
                        PixelBufferPoolSlot::allocated_for(buffer, acquiring_stream),
                    );
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
            ring.slot_count(),
            registered_count
        );

        Ok(entry.insert(PixelBufferRingPool {
            pool: underlying_pool,
            ring,
            slots_lent_to_native_node_output_pools: 0,
        }))
    }

    /// Allocate one more buffer into `ring_pool` for `acquiring_stream`,
    /// registered with the surface-share service, answering its slot index;
    /// an allocation the device's memory budget refuses is answered as that
    /// refusal.
    fn allocate_and_register_one_more_slot(
        ring_pool: &mut PixelBufferRingPool,
        surface_store: Option<&SurfaceStore>,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<usize> {
        let (pool_id, buffer) = ring_pool.pool.allocate_additional_buffer()?;
        if let Some(store) = surface_store
            && let Err(e) = store.register_buffer(pool_id.as_str(), &buffer)
        {
            tracing::warn!(
                "PixelBufferPoolManager: failed to register expanded buffer {}: {}",
                pool_id,
                e
            );
        }
        Ok(ring_pool.ring.push_fresh_slot(
            pool_id.as_str().to_string(),
            PixelBufferPoolSlot::allocated_for(buffer, acquiring_stream),
        ))
    }

    /// Swap the slot's `buffer_cache` entry to the id just minted: the
    /// retired id stops resolving in-process — absence *is* the loud
    /// failure here — and the cache keeps exactly one share per slot, which
    /// [`PixelBufferPoolSlot`]'s in-process-hold baseline counts on. Call
    /// after the mint: the slot's current id is the one this publishes.
    fn retire_previous_frame_in_cache(
        &self,
        slot: &LeaseAwarePoolSlot<PixelBufferPoolSlot>,
        buffer: &PixelBuffer,
    ) {
        let mut cache = self.buffer_cache.lock().unwrap();
        if let Some(previously_published_frame_id) = slot.previously_published_frame_id() {
            cache
                .buffers_by_surface_id
                .remove(&previously_published_frame_id);
        }
        cache
            .buffers_by_surface_id
            .insert(slot.currently_published_frame_id(), buffer.clone());
    }

    /// Whether any published frame of `pool_slot_key` resolves here — false
    /// once the slot was freed.
    fn publishes_a_frame_of_the_slot(&self, pool_slot_key: &str) -> bool {
        self.buffer_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .buffers_by_surface_id
            .keys()
            .any(|cached_surface_id| {
                PublishedPixelBufferFrameId::parse(cached_surface_id).is_some()
                    && pool_slot_key_of_surface_id(cached_surface_id) == pool_slot_key
            })
    }

    /// Get a buffer by its UUID from local cache.
    fn get_from_cache(&self, pool_id: &str) -> Option<PixelBuffer> {
        self.buffer_cache
            .lock()
            .unwrap()
            .buffers_by_surface_id
            .get(pool_id)
            .cloned()
    }

    /// Cache a buffer the surface store resolved under `surface_id`. An id
    /// naming a published frame is never cached here: its entry is the
    /// pool's, retired on recycle, and one minted here would outlive the
    /// frame and serve the recycled slot.
    fn cache_pixel_buffer_looked_up_through_the_surface_store(
        &self,
        surface_id: &str,
        buffer: PixelBuffer,
    ) {
        if PublishedPixelBufferFrameId::parse(surface_id).is_some() {
            return;
        }
        let mut cache = self.buffer_cache.lock().unwrap();
        if cache
            .buffers_by_surface_id
            .insert(surface_id.to_string(), buffer)
            .is_none()
        {
            cache.surface_store_lookup_count += 1;
        }
        if cache.surface_store_lookup_count > MAX_BUFFER_CACHE_SIZE {
            tracing::warn!(
                "PixelBufferPoolManager: buffer_cache holds {} surface-store lookups, past {}; \
                 evicting every lookup and keeping each slot's published frame",
                cache.surface_store_lookup_count,
                MAX_BUFFER_CACHE_SIZE
            );
            cache.buffers_by_surface_id.retain(|cached_surface_id, _| {
                PublishedPixelBufferFrameId::parse(cached_surface_id).is_some()
            });
            cache.surface_store_lookup_count = 0;
        }
    }

    /// Forget the buffer cached under a surface-store id once that id is
    /// released. A published frame's entry stays: on macOS a one-off's id is
    /// its slot's published frame id, and that entry is the pool's to retire.
    fn forget_pixel_buffer_looked_up_through_the_surface_store(&self, surface_id: &str) {
        if PublishedPixelBufferFrameId::parse(surface_id).is_some() {
            return;
        }
        let mut cache = self.buffer_cache.lock().unwrap();
        if cache.buffers_by_surface_id.remove(surface_id).is_some() {
            cache.surface_store_lookup_count = cache.surface_store_lookup_count.saturating_sub(1);
        }
    }
}

impl PixelBufferRingPool {
    fn is_empty_with_no_slot_out_on_loan(&self) -> bool {
        self.ring.slot_count() == 0 && self.slots_lent_to_native_node_output_pools == 0
    }

    /// Count one slot lent to a native node's output pool as back from loan.
    fn note_a_lent_slot_came_back(&mut self, shape: PixelBufferPoolKey) {
        match self.slots_lent_to_native_node_output_pools.checked_sub(1) {
            Some(slots_still_lent) => {
                self.slots_lent_to_native_node_output_pools = slots_still_lent
            }
            None => tracing::warn!(
                "PixelBufferPoolManager: a slot came back to the {}x{} {:?} ring that counted \
                 none out on loan; its loan count is wrong",
                shape.width,
                shape.height,
                shape.format
            ),
        }
    }
}

/// A pixel-buffer pool slot taken out of every ring, retired, and evicted
/// from every cache that resolves it, awaiting destruction.
struct PixelBufferPoolSlotTakenOutOfItsRing {
    slot: LeaseAwarePoolSlot<PixelBufferPoolSlot>,
    /// The texture synthesized from the slot's buffer, taken out of the
    /// texture cache with it.
    canvas_texture_synthesized_from_the_slot: Option<Texture>,
}

impl SharedPoolEntryTakenOutOfItsPool for PixelBufferPoolSlotTakenOutOfItsRing {
    fn is_referenced_beyond_the_taken_out_entry(&self) -> bool {
        self.slot.resource().buffer.strong_count() > 1
            || self
                .canvas_texture_synthesized_from_the_slot
                .as_ref()
                .is_some_and(|canvas_texture| canvas_texture.strong_count() > 1)
    }

    fn note_its_destruction(&self) {
        let buffer = &self.slot.resource().buffer;
        tracing::debug!(
            pool_slot = self.slot.pool_slot_key(),
            width = buffer.width,
            height = buffer.height,
            format = ?buffer.format(),
            "freed a pixel-buffer pool slot"
        );
    }
}

fn warn_of_no_shared_ring_to_take_back_slots_lent_to_a_native_node(
    shape: PixelBufferPoolKey,
    lent_slot_count: usize,
) {
    tracing::warn!(
        "PixelBufferPoolManager: no shared {}x{} {:?} ring to take back {} slots a native node's \
         output pool borrowed; no pool hands them out again, and each one's last published frame \
         stays in the buffer cache, still resolvable",
        shape.width,
        shape.height,
        shape.format,
        lent_slot_count
    );
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
    /// Whether a window of this platform's window systems can get a surface;
    /// never on the bundled software driver, which carries no window-system code.
    pub supports_window_system_surface: bool,
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
    /// Present compositors keyed by attachment format — the graphics twin of
    /// `color_converter_cache`, and cached for the same reason: building one
    /// compiles two SPIR-V modules and a whole graphics pipeline, which is
    /// not per-call work.
    ///
    /// Each entry is behind its own lock because the draw, not just the
    /// build, is the shared resource: `compose_to_offscreen_texture` stages
    /// one descriptor-ring slot and then submits, so two concurrent draws
    /// through one compositor would overwrite each other's bindings.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    present_compositor_cache: Arc<
        parking_lot::Mutex<
            HashMap<
                TextureFormat,
                Arc<parking_lot::Mutex<crate::vulkan::rhi::VulkanPresentCompositor>>,
            >,
        >,
    >,
    /// Serializes [`GpuContextLimitedAccess::escalate`] scopes across
    /// threads so concurrent GPU resource creation (video
    /// sessions, DPB images, swapchain) can't race on the device. The
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
    /// The engine's GLSL compiler and the SPIR-V it has already produced.
    ///
    /// GLSL text is the kernel source contract, so compilation happens here at
    /// kernel construction rather than in a build step the author has to own.
    /// Sits in front of `compute_kernel_cache`: this one spares the
    /// compilation, that one spares the pipeline.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    glsl_shader_source_compiler: Arc<crate::core::rhi::GlslShaderSourceToSpirvCompiler>,
    /// The recorder every compute dispatch — batched or a recording of one —
    /// records into, built on first use and kept for this context's lifetime.
    ///
    /// One recorder, not one per recording: its command pool, primary command
    /// buffer and completion fence are exactly what a recording reuses, and
    /// allocating them per frame would spend the submission the batch op
    /// exists to save. Serial use is what the recorder requires, and this lock
    /// is what gives it: a dispatch holds it from recording through the fence
    /// wait, and nothing else serializes two dispatches.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    batched_compute_dispatch_recorder:
        Arc<parking_lot::Mutex<Option<crate::vulkan::rhi::RhiCommandRecorder>>>,
    /// The recorder every surface-to-surface copy reuses, for the reason
    /// `batched_compute_dispatch_recorder` is one recorder; its lock is what
    /// serializes two copies.
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
            present_compositor_cache: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            escalate_gate: Arc::new(super::escalate_gate::EscalateGate::new()),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            compute_kernel_cache: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            glsl_shader_source_compiler: Arc::new(
                crate::core::rhi::GlslShaderSourceToSpirvCompiler::new(),
            ),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            batched_compute_dispatch_recorder: Arc::new(parking_lot::Mutex::new(None)),
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

    /// Acquire a pixel buffer from the shared pool.
    ///
    /// Pools are cached by (width, height, format) - the first call creates the pool
    /// and pre-allocates buffers, subsequent calls reuse it. Returns (id, buffer) where
    /// the id names this acquisition's frame and can be used with
    /// `get_pixel_buffer()` until the slot's next acquisition retires it.
    ///
    /// If SurfaceStore is initialized, pre-allocated buffers are registered with the surface-share service.
    ///
    /// Never waits on another holder; past the device's memory budget the
    /// allocation is refused by name. The slot is attributed to the stream
    /// this view serves, and at most one slot due to be freed is freed.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        tracing::debug!(
            rhi_op = "acquire_pixel_buffer",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_pixel_buffer"
        );
        self.pixel_buffer_pool_manager.acquire(
            width,
            height,
            format,
            self.loaded_stream_this_view_serves,
        )
    }

    /// An empty output pool for the native node `processor_id_of_the_owning_node`,
    /// which publishes its per-frame pixel buffers from slots it borrows from
    /// the shared pixel-buffer pool; allocates nothing until its first frame.
    pub(crate) fn create_processor_output_pixel_buffer_pool_for_a_native_node(
        &self,
        processor_id_of_the_owning_node: &str,
    ) -> super::ProcessorOutputPixelBufferPoolOfOneNativeNode {
        super::ProcessorOutputPixelBufferPoolOfOneNativeNode::new(
            processor_id_of_the_owning_node,
            self.loaded_stream_this_view_serves,
            Arc::clone(&self.pixel_buffer_pool_manager),
            Arc::clone(&self.engines_own_surface_store),
        )
    }

    /// Get a pixel buffer by its published surface id.
    ///
    /// First checks local cache, then falls back to surface-share service
    /// lookup for cross-process sharing. Either path fails for a retired
    /// published frame id: the pool evicts on recycle and the service
    /// refuses, so "found" always means "still that frame".
    pub fn get_pixel_buffer(&self, surface_id: &str) -> Result<PixelBuffer> {
        // Check local cache first
        if let Some(buffer) = self.pixel_buffer_pool_manager.get_from_cache(surface_id) {
            tracing::trace!(
                "GpuContext::get_pixel_buffer: cache hit for '{}'",
                surface_id
            );
            return Ok(buffer);
        }

        // Cache miss - try surface-share service lookup
        tracing::debug!(
            "GpuContext::get_pixel_buffer: cache miss for '{}', trying surface-share service",
            surface_id
        );

        let surface_store = self.surface_store.lock().unwrap();
        let store = surface_store.as_ref().ok_or_else(|| {
            Error::Configuration(NO_SURFACE_STORE_IS_CONNECTED_TO_THIS_GPU_CONTEXT.into())
        })?;

        let buffer = store.lookup_buffer(surface_id)?;
        self.pixel_buffer_pool_manager
            .cache_pixel_buffer_looked_up_through_the_surface_store(surface_id, buffer.clone());

        Ok(buffer)
    }

    /// Forget the pixel buffer [`Self::get_pixel_buffer`] cached under a
    /// surface-store id, once that id is released; a published frame's entry
    /// stays, the pool's to retire.
    pub(crate) fn forget_pixel_buffer_looked_up_through_the_surface_store(&self, surface_id: &str) {
        self.pixel_buffer_pool_manager
            .forget_pixel_buffer_looked_up_through_the_surface_store(surface_id);
    }

    /// The published frame ids this process's pixel-buffer pools hold in
    /// their cache, one per slot that has published.
    #[cfg(test)]
    pub(crate) fn published_pixel_buffer_frame_ids_cached_in_this_process(&self) -> Vec<String> {
        self.pixel_buffer_pool_manager
            .buffer_cache
            .lock()
            .unwrap()
            .buffers_by_surface_id
            .keys()
            .filter(|surface_id| PublishedPixelBufferFrameId::parse(surface_id).is_some())
            .cloned()
            .collect()
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.get_pixel_buffer(surface_id)
    }

    /// The generation every lease-aware ring this context owns most recently
    /// minted per slot.
    pub(crate) fn lease_aware_pool_minted_frame_generations(
        &self,
    ) -> &LeaseAwarePoolMintedFrameGenerations {
        &self.lease_aware_pool_minted_frame_generations
    }

    /// Refuse a published frame id whose slot has been recycled since.
    ///
    /// An id with no generation suffix passes. A slot one of this context's
    /// own lease-aware rings minted answers from their generation index —
    /// present with or without a lease registry, so the refusal holds in a
    /// context no service was wired into. Anything else falls to the
    /// registry, which fails closed on slots nobody published.
    pub(crate) fn refuse_a_retired_frame_id(&self, surface_id: &str) -> Result<()> {
        let Some((pool_slot, published_generation)) =
            crate::core::rhi::split_pool_slot_and_frame_generation(surface_id)
        else {
            return Ok(());
        };
        if let Some(answer) = self
            .lease_aware_pool_minted_frame_generations
            .refusal_of_a_retired_frame_id(surface_id, pool_slot, published_generation)
        {
            return answer;
        }
        let surface_store = self.surface_store.lock().unwrap();
        match surface_store
            .as_ref()
            .and_then(SurfaceStore::check_out_leases)
        {
            Some(leases) => leases.refuse_a_retired_frame_id(surface_id),
            None => Ok(()),
        }
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let registration = TextureRegistration::new(texture, VulkanLayout::UNDEFINED);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let registration = TextureRegistration::new(texture);
        let mut cache = self.texture_cache.lock().unwrap();
        cache.insert(pool_slot_key_of_surface_id(id).to_string(), registration);
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        let registration = TextureRegistration::new(texture, initial_layout);
        let mut cache = self.texture_cache.lock().unwrap();
        // Keyed by the slot so a producer re-registering per published
        // frame replaces one entry instead of growing one per frame; the
        // published id's generation is checked at resolve time instead.
        cache.insert(pool_slot_key_of_surface_id(id).to_string(), registration);
    }

    /// Remove a `surface_id` from the same-process texture cache.
    ///
    /// Idempotent — missing entries are a no-op. Producers that
    /// pre-register textures with a known lifetime (e.g.
    /// [`TextureRing`](crate::core::context::TextureRing)) call this on
    /// teardown so the cache doesn't outlive the underlying texture.
    pub fn unregister_texture(&self, id: &str) {
        // The guard is scoped, not held across the eviction below: that call
        // takes the staging map and then the surface-share socket, so holding
        // this one through it would couple three locks into an order nothing
        // else needs. The two removals were never atomic together anyway.
        self.texture_cache
            .lock()
            .unwrap()
            .remove(pool_slot_key_of_surface_id(id));
        #[cfg(target_os = "linux")]
        self.evict_surface_export_stagings(id);
    }

    /// Register a tensor storage buffer under `id` in the parent-wide map.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn register_storage_buffer_in_the_parent_wide_map(
        &self,
        id: &str,
        tensor_storage_buffer: crate::core::rhi::StorageBuffer,
    ) {
        self.storage_buffer_registrations.lock().unwrap().insert(
            pool_slot_key_of_surface_id(id).to_string(),
            tensor_storage_buffer,
        );
    }

    /// Remove `id` from the parent-wide storage buffer map. Idempotent.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn unregister_storage_buffer_from_the_parent_wide_map(&self, id: &str) {
        self.storage_buffer_registrations
            .lock()
            .unwrap()
            .remove(pool_slot_key_of_surface_id(id));
    }

    /// The tensor storage buffer the parent-wide map holds under
    /// `surface_id`, refusing a published frame id whose slot has been
    /// recycled since.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(
        &self,
        surface_id: &str,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.refuse_a_retired_frame_id(surface_id)?;
        self.storage_buffer_registrations
            .lock()
            .unwrap()
            .get(pool_slot_key_of_surface_id(surface_id))
            .cloned()
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "no tensor storage buffer is registered under surface id {surface_id:?}"
                ))
            })
    }

    /// The texture a producer registered of its own under `surface_id`
    /// in this process, if any.
    ///
    /// Deliberately not [`Self::resolve_texture_registration_by_surface_id`]:
    /// that call's Path 3 synthesizes a texture *from* the pooled
    /// backing, so a surface with no producer texture at all would still
    /// answer yes. Only a real registration distinguishes a frame its
    /// producer still owns from one whose only backing is its pool
    /// member. Same-process only — a producer that registered a texture
    /// with the surface-share service alone is invisible here, which no
    /// producer in this tree is: cross-process registrations mint their
    /// own handle id rather than a pool id.
    pub(crate) fn producer_registered_texture_for_surface_id(
        &self,
        surface_id: &str,
    ) -> Option<TextureRegistration> {
        self.texture_cache
            .lock()
            .unwrap()
            .get(pool_slot_key_of_surface_id(surface_id))
            .cloned()
    }

    /// The pooled allocation this process holds for `surface_id`, if any
    /// — the pool's own cache, with no surface-share round trip.
    ///
    /// For a published frame id this answers only while the frame is
    /// current: the pool owns the entry and evicts it on recycle. Ids with
    /// no generation ([`Self::get_pixel_buffer`] caches their successful
    /// cross-process lookups here) answer from the second resolution on.
    pub(crate) fn pooled_backing_held_in_this_process(
        &self,
        surface_id: &str,
    ) -> Option<PixelBuffer> {
        self.pixel_buffer_pool_manager.get_from_cache(surface_id)
    }

    /// Refresh the registration's `current_layout` for a given
    /// `surface_id`. No-op if the surface_id isn't in the cache.
    /// Used by producers after a layout transition (e.g.
    /// [`TextureRing`](crate::core::context::TextureRing)'s per-frame
    /// copy ends in `SHADER_READ_ONLY_OPTIMAL`).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        if let Some(reg) = self
            .texture_cache
            .lock()
            .unwrap()
            .get(pool_slot_key_of_surface_id(id))
        {
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
    /// Path 3 (pixel buffer fallback) declares whatever
    /// terminal layout the upload reports leaving the host-owned
    /// texture in — `SHADER_READ_ONLY_OPTIMAL` for the sampled-capable
    /// texture that path allocates.
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "macos")),
            allow(unused_variables)
        )]
        texture_layout: Option<i32>,
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "macos")),
            allow(unused_variables)
        )]
        width: u32,
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "macos")),
            allow(unused_variables)
        )]
        height: u32,
    ) -> Result<TextureRegistration> {
        // A retired published frame id resolves to an error, not to the
        // slot's current pixels — every path below serves per-slot backings,
        // so the frame-vs-slot distinction lives here.
        self.refuse_a_retired_frame_id(surface_id)?;

        // Path 1: same-process texture cache (fastest)
        {
            let cache = self.texture_cache.lock().unwrap();
            if let Some(reg) = cache.get(pool_slot_key_of_surface_id(surface_id)) {
                return Ok(reg.clone());
            }
        }

        // Path 2: cross-process VkImage import via the surface-share service —
        // a DMA-BUF on Linux, an IOSurface on macOS.
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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

        // Path 3: pixel buffer fallback — refresh a private host-owned
        // texture from the latest buffer contents. The cache is separate
        // from `texture_cache` because a pool slot serves a new frame every
        // cycle and a cache hit on stale contents would silently render the
        // previous one.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // Same-process pool first: a producer that published only a
            // pixel buffer (no texture registration) resolves through the
            // pool's local cache without any socket round-trip — the
            // cross-process store can't serve OPAQUE_FD-backed buffers to a
            // host-side consumer at all.
            let buffer = self.pixel_buffer_pool_manager.get_from_cache(surface_id);
            #[cfg(target_os = "linux")]
            let buffer = buffer.or_else(|| {
                let surface_store = self.surface_store.lock().unwrap();
                surface_store
                    .as_ref()
                    .and_then(|store| store.lookup_buffer(surface_id).ok())
            });
            if let Some(buffer) = buffer {
                return self.refresh_pixel_buffer_texture(surface_id, &buffer, width, height);
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
        let desc =
            TextureDescriptor::new(width, height, format).with_usage(output_texture_usages());
        let texture = self.device.create_texture(&desc)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.register_texture(&id, texture.clone());
        Ok((id, texture))
    }

    /// Refresh a private texture from a host-visible pixel buffer for cross-
    /// process producers that registered a buffer (not a texture). The texture
    /// is created on first call and reused for subsequent calls under the same
    /// `surface_id`; contents are re-uploaded every time so rotating-pool
    /// producers see fresh frames. The returned registration declares the
    /// terminal layout the upload reports leaving the texture in.
    /// Drop the canvas just cached for `slot_key` when the slot it was drawn
    /// for was freed meanwhile: freeing evicts the slot's published id before
    /// its canvas, so a canvas cached after that eviction would outlive the
    /// slot for the runtime's life.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn evict_a_canvas_cached_after_its_slot_was_freed(
        &self,
        surface_id: &str,
        slot_key: &str,
        cached_canvas: &Texture,
    ) {
        if PublishedPixelBufferFrameId::parse(surface_id).is_none()
            || self
                .pixel_buffer_pool_manager
                .publishes_a_frame_of_the_slot(slot_key)
        {
            return;
        }
        let mut cache = self
            .buffer_texture_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.get(slot_key).is_some_and(|still_cached| {
            Arc::ptr_eq(still_cached.vulkan_inner(), cached_canvas.vulkan_inner())
        }) {
            cache.remove(slot_key);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn refresh_pixel_buffer_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<TextureRegistration> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

        // Refused before touching the cache: a zero extent cannot describe a
        // texture, and callers that pass one (a kernel-dispatch binding
        // resolving a surface id) are saying a buffer-backed surface is not
        // acceptable to them at all. Evicting the slot's cached canvas on the
        // way to an invalid create would break the caller that *can* use it.
        if width == 0 || height == 0 {
            return Err(Error::GpuError(format!(
                "surface {surface_id:?} is buffer-backed, and this caller cannot synthesize a \
                 texture from a pixel buffer"
            )));
        }

        // Get-or-create the cached texture for this surface's slot: the
        // texture is a reusable canvas for the slot, and the per-frame
        // identity check happened before the buffer was resolved.
        let slot_key = pool_slot_key_of_surface_id(surface_id);
        let texture = {
            let mut cache = self.buffer_texture_cache.lock().unwrap();
            if let Some(existing) = cache.get(slot_key) {
                if existing.width() == width && existing.height() == height {
                    existing.clone()
                } else {
                    cache.remove(slot_key);
                    let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
                        .with_usage(
                            TextureUsages::COPY_DST
                                | TextureUsages::TEXTURE_BINDING
                                | TextureUsages::STORAGE_BINDING,
                        );
                    let new_texture = self.device.create_texture_local(&desc)?;
                    cache.insert(slot_key.to_string(), new_texture.clone());
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
                cache.insert(slot_key.to_string(), new_texture.clone());
                new_texture
            }
        };
        self.evict_a_canvas_cached_after_its_slot_was_freed(surface_id, slot_key, &texture);

        let upload = unsafe {
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                texture.vulkan_inner(),
                width,
                height,
            )
        }?;
        Ok(TextureRegistration::new(
            texture,
            upload.final_texture_layout,
        ))
    }

    /// Upload a pixel buffer's contents to a GPU texture and register it in the texture cache.
    ///
    /// Copies the host-visible pixel buffer data to a device-local texture via
    /// vkCmdCopyBufferToImage, then registers the texture so display/encoder
    /// consumers can resolve it by surface_id.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

        // Defense-in-depth for the FullAccess (privileged) tier — #1388 is
        // the first SDK/plugin exposure of this slot. The copy region is
        // tightly packed (buffer_row_length = 0 in
        // record_and_submit_buffer_to_image) and the destination format is
        // hardcoded Rgba8Unorm (4 bytes/pixel), so vkCmdCopyBufferToImage
        // reads exactly width * height * 4 bytes from the source
        // HOST_VISIBLE buffer. A buggy privileged plugin passing oversized
        // dimensions (or a smaller / non-RGBA8 source) would drive a
        // GPU-side out-of-bounds read of the staging buffer —
        // VK_ERROR_DEVICE_LOST (driver corruption) with no error and no
        // panic. Validate the required byte size against the source
        // buffer's actual allocation and return a typed error before submit.
        let required_byte_size = (width as u64)
            .checked_mul(height as u64)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                crate::core::Error::GpuError(format!(
                    "upload_pixel_buffer_as_texture: {width}x{height} Rgba8Unorm copy region \
                     byte size overflows u64"
                ))
            })?;
        let source_byte_size = pixel_buffer.buffer_ref().inner.size();
        if required_byte_size > source_byte_size {
            return Err(crate::core::Error::GpuError(format!(
                "upload_pixel_buffer_as_texture: {width}x{height} Rgba8Unorm copy region requires \
                 {required_byte_size} bytes but the source pixel buffer holds only \
                 {source_byte_size} bytes"
            )));
        }

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

        let upload = unsafe {
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                texture.vulkan_inner(),
                width,
                height,
            )
        }?;

        self.register_texture_with_layout(surface_id, texture, upload.final_texture_layout);
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
    /// TRANSFER_DST → the destination's usage-legal terminal layout via
    /// the existing `upload_buffer_to_image` path (content discard on
    /// the UNDEFINED transition is intended — the caller is about to
    /// overwrite the slot's contents anyway).
    ///
    /// When `surface_id` resolves to an entry in the texture cache
    /// (e.g. a ring slot pre-registered via
    /// [`crate::core::context::GpuContextFullAccess::create_texture_ring`])
    /// the registration's `current_layout` is refreshed to that same
    /// terminal layout, so a consumer barriers out of the layout the
    /// image is actually in.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let upload = unsafe {
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                texture.vulkan_inner(),
                width,
                height,
            )
        }?;
        // Refresh the registration's layout (no-op for unregistered surface_ids).
        if let Some(reg) = self
            .texture_cache
            .lock()
            .unwrap()
            .get(pool_slot_key_of_surface_id(surface_id))
        {
            reg.update_layout(upload.final_texture_layout);
        }
        Ok(())
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
    ///
    /// Never waits on another holder; past the device's memory budget the
    /// allocation is refused by name.
    pub fn acquire_texture(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        tracing::debug!(
            rhi_op = "acquire_texture",
            width = desc.width,
            height = desc.height,
            format = ?desc.format,
            "GpuContext::acquire_texture"
        );
        self.texture_pool
            .acquire(desc, self.loaded_stream_this_view_serves)
    }

    /// Free every unheld shared pool entry `stream` last acquired, and flag
    /// the ones still held to be freed as soon as they are unheld. Run as the
    /// stream stops, once its processors are gone and its surface store
    /// closed: it waits, bounded, for the checkout leases its helpers held on
    /// those entries to clear, then for the GPU work naming them.
    pub(crate) fn free_every_unheld_shared_pool_entry_the_stream_last_acquired(
        &self,
        stream: crate::core::runtime::LoadedStreamTag,
    ) {
        let mut surface_ids_naming_the_streams_entries = self
            .texture_pool
            .flag_every_slot_the_stream_last_acquired(stream);
        surface_ids_naming_the_streams_entries.extend(
            self.pixel_buffer_pool_manager
                .flag_every_slot_the_stream_last_acquired(stream),
        );
        if let Some(check_out_leases) = SurfaceStore::check_out_lease_registry_of_the_engines_own(
            &self.engines_own_surface_store,
        ) {
            match check_out_leases.wait_until_no_check_out_lease_names_any_of(
                &surface_ids_naming_the_streams_entries,
                LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES,
            ) {
                Ok(true) => {}
                still_leased_or_unreadable => tracing::debug!(
                    stream = %stream,
                    "a checkout lease on a stopped stream's shared pool entries outlasted {:?} \
                     ({still_leased_or_unreadable:?}); those entries are freed once it is \
                     released",
                    LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES
                ),
            }
        }
        let destroyed_texture_pool_slot_count = self
            .texture_pool
            .free_every_unheld_slot_due_to_be_freed_waiting_for_its_gpu_work();
        let destroyed_pixel_buffer_pool_slot_count = self
            .pixel_buffer_pool_manager
            .free_every_unheld_slot_due_to_be_freed_waiting_for_its_gpu_work();
        tracing::debug!(
            stream = %stream,
            destroyed_texture_pool_slot_count,
            destroyed_pixel_buffer_pool_slot_count,
            "freed the shared pool entries a stopped stream last acquired"
        );
    }

    /// Free the shared pool entry behind a helper's one-off pixel buffer as
    /// soon as nothing holds it, without waiting out the idle window.
    pub(crate) fn flag_the_pixel_buffer_pool_slot_of_the_buffer_freed_as_soon_as_unheld(
        &self,
        buffer: &PixelBuffer,
    ) {
        self.pixel_buffer_pool_manager
            .flag_the_slot_of_the_buffer_freed_as_soon_as_unheld(buffer);
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
        tracing::debug!(
            rhi_op = "acquire_render_target_dma_buf_image",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_render_target_dma_buf_image"
        );

        let desc = TextureDescriptor::new(width, height, format)
            .with_usage(render_target_texture_usages());
        self.device.create_texture_render_target_dma_buf(&desc)
    }

    /// Allocate a render-target-capable VkImage over a fresh private
    /// IOSurface — the macOS peer of
    /// [`Self::acquire_render_target_dma_buf_image`], with the same usage
    /// set. Single-plane formats only.
    #[cfg(target_os = "macos")]
    pub fn acquire_render_target_iosurface_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<Texture> {
        tracing::debug!(
            rhi_op = "acquire_render_target_iosurface_image",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_render_target_iosurface_image"
        );
        let desc = TextureDescriptor::new(width, height, format)
            .with_usage(render_target_texture_usages());
        self.device.create_texture_iosurface_backed(&desc)
    }

    /// Acquire a caller-held storage buffer sized for `layout`: HOST_VISIBLE,
    /// persistently mapped, never pooled — the caller keeps the
    /// [`crate::core::rhi::StorageBuffer`] in its processor state and drops it
    /// at teardown. A byte-shaped SSBO is a one-dimensional `uint8` layout.
    ///
    /// Usage is `STORAGE_BUFFER | TRANSFER_SRC | TRANSFER_DST`, plus DMA-BUF
    /// export flags on Linux; compute kernels bind it via
    /// [`crate::vulkan::rhi::VulkanComputeKernel::set_storage_buffer_storage`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_storage_buffer(
        &self,
        layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.acquire_storage_buffer_of_flavour(
            layout,
            StorageBufferAllocationFlavour::CallerHeldHostVisible,
        )
    }

    /// Acquire a storage buffer for `layout` in the flavour its destination
    /// needs — derived by the engine per acquisition, never a caller's dial.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn acquire_storage_buffer_of_flavour(
        &self,
        layout: &crate::core::rhi::TensorStorageBufferLayout,
        flavour: StorageBufferAllocationFlavour,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        let byte_size = layout.byte_size();
        tracing::debug!(
            rhi_op = "acquire_storage_buffer",
            byte_size,
            ?flavour,
            "GpuContext::acquire_storage_buffer"
        );
        match flavour {
            StorageBufferAllocationFlavour::CallerHeldHostVisible => {
                let buffer = crate::vulkan::rhi::HostVulkanBuffer::new_storage_buffer_host_visible(
                    &self.device.inner,
                    byte_size,
                )?;
                Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
                    Arc::new(buffer),
                ))
            }
            #[cfg(target_os = "linux")]
            StorageBufferAllocationFlavour::CrossesToAHelperProcess => {
                self.create_opaque_fd_export_buffer(byte_size, true)
            }
            #[cfg(target_os = "macos")]
            StorageBufferAllocationFlavour::CrossesToAHelperProcess => {
                let buffer =
                    crate::vulkan::rhi::HostVulkanBuffer::new_iosurface_backed_storage_buffer(
                        &self.device.inner,
                        byte_size,
                    )?;
                Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
                    Arc::new(buffer),
                ))
            }
        }
    }

    /// Acquire a HOST_VISIBLE uniform buffer (UBO).
    ///
    /// Returns a [`crate::core::rhi::UniformBuffer`] — the type system
    /// enforces that this buffer can only be bound to a kernel's
    /// `set_uniform_buffer` slot (not storage / vertex / index).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    ///
    /// The cached converter's kernel — and the bindings staged on it
    /// between `prepare_*` and the dispatch — is one object shared by every
    /// holder of the handle, so two processors driving the same format pair
    /// from their own threads race it. A processor that records the
    /// dispatch itself takes [`Self::create_color_converter`] instead.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn color_converter(&self, src: PixelFormat, dst: PixelFormat) -> Result<RhiColorConverter> {
        // Fast path: read lock; cache stores Arc<Inner> so we can build
        // a fresh handle via from_arc_into_raw per request.
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

    /// A color converter of the caller's own — the same `(src, dst)` kernel
    /// the cached `color_converter` would hand out, built fresh and never
    /// placed in the cache, so a processor recording its dispatch from its
    /// own thread shares no pending state with any other.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_color_converter(
        &self,
        src: PixelFormat,
        dst: PixelFormat,
    ) -> Result<RhiColorConverter> {
        let vulkan_device = &self.device.inner;
        let inner = crate::vulkan::rhi::VulkanColorConverter::new(vulkan_device, src, dst)?;
        tracing::debug!(
            rhi_op = "create_color_converter",
            ?src,
            ?dst,
            "GpuContext::create_color_converter — owned converter constructed"
        );
        Ok(RhiColorConverter::from_arc_into_raw(Arc::new(
            crate::core::rhi::RhiColorConverterInner { inner },
        )))
    }

    /// Create a compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Reflects the SPIR-V at creation time and validates that the declared
    /// bindings match the shader; mismatches are reported with a clear error
    /// message rather than producing undefined GPU behavior at first dispatch.
    /// Returned kernel is held and dispatched via its own `set_*` / `dispatch`
    /// methods — one kernel handle per processor pipeline stage is the expected
    /// usage.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        crate::vulkan::rhi::VulkanComputeKernel::new_caching_its_pipeline_in(
            vulkan_device,
            descriptor,
            self.pipeline_cache_directory_of_its_stream(),
        )
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

    /// Import an IOSurface's memory as a storage buffer, zero-copy, retaining
    /// the surface for the buffer's life. Refused, naming the reason, when
    /// the device cannot import host memory, the surface's base address is
    /// off the import alignment, or the driver declines (MoltenVK before
    /// 1.4.1 does). Backs
    /// [`GpuContextFullAccess::import_iosurface_as_storage_buffer`].
    #[cfg(target_os = "macos")]
    #[tracing::instrument(
        level = "debug",
        skip(self, iosurface),
        fields(rhi_op = "import_iosurface_as_storage_buffer")
    )]
    pub fn import_iosurface_as_storage_buffer(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> Result<crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer> {
        crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer::import(&self.device.inner, iosurface)
    }

    /// Import a caller-owned host range for GPU writes — the loopback
    /// device's mapped output buffer — taking the imported tier when the
    /// driver allows it and host-cached staging otherwise. See
    /// [`HostMappingWrittenByGpu`](crate::vulkan::rhi::HostMappingWrittenByGpu)
    /// for the per-frame protocol.
    ///
    /// # Safety
    ///
    /// The range must stay mapped, writable and unaliased until the returned
    /// value drops.
    #[cfg(target_os = "linux")]
    pub unsafe fn import_host_mapping_for_gpu_writes(
        &self,
        host_range_ptr: *mut u8,
        host_range_byte_len: usize,
    ) -> Result<crate::vulkan::rhi::HostMappingWrittenByGpu> {
        tracing::debug!(
            rhi_op = "import_host_mapping_for_gpu_writes",
            host_range_byte_len,
            "GpuContext::import_host_mapping_for_gpu_writes"
        );
        // SAFETY: the caller upholds this method's own contract.
        unsafe {
            crate::vulkan::rhi::HostMappingWrittenByGpu::import_for_gpu_writes(
                &self.device.inner,
                host_range_ptr,
                host_range_byte_len,
            )
        }
    }

    /// Build a swapchain-backed [`PresentTarget`](crate::vulkan::rhi::PresentTarget)
    /// on a window's `surface_source`, at the requested initial extent +
    /// vsync preference. `color_traits` drives the `VkColorSpaceKHR`
    /// priority walk; `None` keeps the legacy SDR pick. The window must
    /// outlive the returned target (the host owns the `VkSurfaceKHR`
    /// from creation, never the window). Display processors reach this
    /// through the SDK `create_present_target` wrapper, never
    /// `VulkanPresentTarget::new` on a raw device.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_present_target(
        &self,
        surface_source: crate::vulkan::rhi::PresentSurfaceSource<'_>,
        width: u32,
        height: u32,
        vsync: bool,
        color_traits: Option<&crate::core::color::ColorTraits>,
    ) -> Result<crate::vulkan::rhi::PresentTarget> {
        let target =
            self.create_vulkan_present_target(surface_source, width, height, vsync, color_traits)?;
        Ok(crate::vulkan::rhi::PresentTarget::from_target(target))
    }

    /// The host-side flavor of [`Self::create_present_target`]: mints the
    /// raw [`crate::vulkan::rhi::VulkanPresentTarget`] without the ABI-safe
    /// wrapper. In-process consumers (via
    /// [`GpuContextFullAccess::create_present_target`]) drive it directly.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_vulkan_present_target(
        &self,
        surface_source: crate::vulkan::rhi::PresentSurfaceSource<'_>,
        width: u32,
        height: u32,
        vsync: bool,
        color_traits: Option<&crate::core::color::ColorTraits>,
    ) -> Result<crate::vulkan::rhi::VulkanPresentTarget> {
        tracing::debug!(
            rhi_op = "create_present_target",
            width,
            height,
            vsync,
            "GpuContext::create_present_target"
        );
        crate::vulkan::rhi::VulkanPresentTarget::new(
            &self.device.inner,
            surface_source,
            width,
            height,
            vsync,
            color_traits,
        )
    }

    /// Build a [`crate::vulkan::rhi::VulkanPresentCompositor`] for
    /// `attachment_format`.
    ///
    /// A caller that owns its compositor across frames — a window, whose
    /// swapchain can flip the attachment format under it — builds one here.
    /// A caller that just wants one draw uses
    /// [`Self::compose_texture_onto_offscreen_texture`] instead, which shares
    /// a cached compositor rather than compiling a pipeline per call.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_present_compositor(
        &self,
        attachment_format: crate::core::rhi::TextureFormat,
    ) -> Result<crate::vulkan::rhi::VulkanPresentCompositor> {
        tracing::debug!(
            rhi_op = "create_present_compositor",
            ?attachment_format,
            "GpuContext::create_present_compositor"
        );
        crate::vulkan::rhi::VulkanPresentCompositor::new(&self.device.inner, attachment_format)
    }

    /// Draw `source` onto `destination` — scaled, aspect-managed, no window —
    /// through a compositor cached on this context for `destination`'s format.
    ///
    /// Submits and waits: `destination` comes back in
    /// `COLOR_ATTACHMENT_OPTIMAL` and `source` in `SHADER_READ_ONLY_OPTIMAL`.
    /// Concurrent callers serialize on the cached compositor's lock, which is
    /// also what makes one descriptor-ring slot enough.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compose_texture_onto_offscreen_texture(
        &self,
        destination: &Texture,
        source: &Texture,
        source_current_layout: VulkanLayout,
        scaling: crate::vulkan::rhi::PresentScalingMode,
    ) -> Result<()> {
        let attachment_format = destination.format();
        let compositor = {
            let mut cache = self.present_compositor_cache.lock();
            match cache.get(&attachment_format) {
                Some(cached) => Arc::clone(cached),
                None => {
                    let built = Arc::new(parking_lot::Mutex::new(
                        self.create_present_compositor(attachment_format)?,
                    ));
                    cache.insert(attachment_format, Arc::clone(&built));
                    built
                }
            }
        };
        // One slot, always, because the lock is held across stage and submit.
        const ONLY_DESCRIPTOR_RING_SLOT: u32 = 0;
        compositor.lock().compose_to_offscreen_texture(
            ONLY_DESCRIPTOR_RING_SLOT,
            destination,
            source,
            source_current_layout,
            scaling,
        )
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        crate::vulkan::rhi::VulkanGraphicsKernel::new_caching_its_pipeline_in(
            vulkan_device,
            descriptor,
            self.pipeline_cache_directory_of_its_stream(),
        )
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        self.device.inner.supports_ray_tracing_pipeline()
    }

    /// Read-once GPU capability snapshot. Mirrors the underlying
    /// `HostVulkanDevice`'s capability getters into one struct so
    /// callers (camera processor, adapters) can decide
    /// vendor-specific branching + DMA-BUF / external-memory paths
    /// at setup time.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn gpu_capabilities(&self) -> GpuCapabilitiesSnapshot {
        let dev = &self.device.inner;
        GpuCapabilitiesSnapshot {
            device_name: dev.name(),
            supports_external_memory: dev.supports_external_memory(),
            device_is_software: dev.device_is_software(),
            supports_dma_buf_export: dev.supports_dma_buf_export(),
            supports_semaphore_export: dev.supports_semaphore_export(),
            supports_cross_device_dma_buf_probe: dev.supports_cross_device_dma_buf_probe(),
            supports_ray_tracing_pipeline: dev.supports_ray_tracing_pipeline(),
            supports_window_system_surface: dev.supports_window_system_surface(),
        }
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

    /// Import a DMA-BUF FD as a `StorageBuffer`. Camera V4L2 zero-copy
    /// path. Consumes `dma_buf_fd` whatever the outcome: the driver owns
    /// it behind a successful import and a failure closes it.
    #[cfg(target_os = "linux")]
    pub fn import_dma_buf_storage_buffer(
        &self,
        dma_buf_fd: std::os::fd::OwnedFd,
        byte_size: u64,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        let vulkan_device = &self.device.inner;
        let buf = crate::vulkan::rhi::HostVulkanBuffer::from_dma_buf_fd_as_storage_buffer(
            vulkan_device,
            dma_buf_fd,
            byte_size,
        )?;
        Ok(crate::core::rhi::StorageBuffer::from_host_vulkan_buffer(
            Arc::new(buf),
        ))
    }

    /// Export a fresh dup'd DMA-BUF FD for `pixel_buffer`, plus its byte
    /// size. Ownership of the fd transfers to the caller.
    ///
    /// The counterpart of [`Self::import_dma_buf_storage_buffer`], and the
    /// export half of the handle-shaped native-interop surface: external
    /// code that speaks DMA-BUF (EGL, a V4L2 output device, another
    /// process) receives the frame in its own dialect without the engine
    /// exposing any Vulkan.
    ///
    /// Only a DMA-BUF-flavoured allocation can answer — pixel-buffer pool
    /// buffers on a device that exports DMA-BUF. An OPAQUE_FD allocation,
    /// which is what a pool buffer is on a device that does not, has no
    /// DMA-BUF export path, so this is refused by name rather than returning
    /// a handle the importer cannot use.
    #[cfg(target_os = "linux")]
    pub fn export_pixel_buffer_dma_buf_fd(
        &self,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64)> {
        use crate::host_rhi::HostPixelBufferRefExt as _;
        let host_buffer = pixel_buffer.buffer_ref().vulkan_inner();
        let fd = host_buffer.export_dma_buf_fd()?;
        Ok((fd, host_buffer.size()))
    }

    /// Allocate an OPAQUE_FD-exportable `VkBuffer` as a `StorageBuffer`.
    /// `device_local = true` picks the VRAM-resident CUDA-visible pool
    /// (`new_opaque_fd_export_device_local`); `false` picks the
    /// HOST_VISIBLE pool (`new_opaque_fd_export`). Backs
    /// [`GpuContextFullAccess::create_opaque_fd_export_buffer`], the
    /// OPAQUE_FD/CUDA producer allocation (#1262).
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
    /// entire CUDA device-binding contract on multi-GPU rigs (a
    /// CUDA adapter matches the CUDA device whose `cudaDeviceProp::uuid`
    /// equals this value, never a silent fall-through to CUDA device 0).
    /// Backs [`GpuContextFullAccess::export_storage_buffer_opaque_fd`]
    /// (#1262).
    #[cfg(target_os = "linux")]
    pub fn export_storage_buffer_opaque_fd(
        &self,
        buffer: &crate::core::rhi::StorageBuffer,
    ) -> Result<(std::os::unix::io::RawFd, u64, [u8; 16])> {
        let host_inner = buffer.host_inner();
        let fd = host_inner.export_opaque_fd_memory()?;
        let size = buffer.byte_size();
        // The UUID is the entire CUDA device-binding contract: it MUST
        // name the device that actually owns the exported memory, not the
        // `GpuContext`'s own device. Under the single-GPU invariant these
        // are the same, but sourcing from the buffer's owning
        // `HostVulkanDevice` keeps the binding correct if a `StorageBuffer`
        // ever originates from a different device (never a silent
        // mismatch).
        let uuid = host_inner.vulkan_device().physical_device_uuid();
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
        // Every branch must leave the shared-buffer write with a defined
        // completion contract before returning — the single-writer
        // producer copy is unsound otherwise (a consumer could read a
        // half-written buffer). Two contracts are valid: a `produce_done`
        // GPU signal that the consumer waits on (the async per-frame
        // path), or a host-side fence drain. Handle all four
        // (consume, produce) combinations explicitly so none returns
        // without one.
        match (consume_done, produce_done) {
            // No timelines: bare submit + host-side fence drain. The copy
            // is complete before return.
            (None, None) => recorder.submit_and_wait(),
            // A `produce_done` signal is present (with or without a
            // `consume_done` wait): the GPU-side signal IS the completion
            // contract — the consumer waits on `produce_done` before its
            // read — so the producer need not host-block. Covers
            // (None, Some) and (Some, Some).
            (consume, produce @ Some(_)) => {
                recorder.submit_waiting_and_signaling_timeline(consume, produce)
            }
            // A `consume_done` wait but no `produce_done` signal: the copy
            // GPU-waits on `consume_done`, but with no signal there is no
            // GPU-side ordering for the consumer. Host-block on the
            // recorder's completion fence so the shared-buffer write is
            // guaranteed done before return, keeping the single-writer
            // contract sound.
            (consume @ Some(_), None) => {
                recorder.submit_waiting_and_signaling_timeline(consume, None)?;
                recorder.wait_for_completion()
            }
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// Mint a hardware video [`SimpleEncoder`](crate::vulkan::video::encode::SimpleEncoder)
    /// on this context's host device, built directly from the host-owned
    /// `Arc<HostVulkanDevice>` (`self.device.inner`).
    ///
    /// Eagerly runs [`SimpleEncoder::prepare_gpu_encode_resources`] so the
    /// first submitted frame doesn't pay the RGB→NV12 converter allocation
    /// latency.
    #[cfg(target_os = "linux")]
    #[tracing::instrument(skip(self, config), fields(rhi_op = "create_encoder_session"))]
    pub(crate) fn create_encoder_session(
        &self,
        config: crate::vulkan::video::encode::SimpleEncoderConfig,
    ) -> Result<crate::vulkan::video::encode::SimpleEncoder> {
        let host_device = Arc::clone(&self.device.inner);
        let mut encoder =
            crate::vulkan::video::encode::SimpleEncoder::from_host_device(host_device, config)
                .map_err(|e| Error::GpuError(format!("create_encoder_session: {e}")))?;
        encoder.prepare_gpu_encode_resources().map_err(|e| {
            Error::GpuError(format!(
                "create_encoder_session: prepare GPU encode resources: {e}"
            ))
        })?;
        Ok(encoder)
    }

    /// Mint a hardware video [`SimpleDecoder`](crate::vulkan::video::decode::SimpleDecoder)
    /// on this context's host device, built directly from the host-owned
    /// `Arc<HostVulkanDevice>` (`self.device.inner`).
    ///
    /// Coded dimensions are auto-detected from the first SPS (query via
    /// [`SimpleDecoder::dimensions`](crate::vulkan::video::decode::SimpleDecoder::dimensions)
    /// after the first `feed`); the config's `max_width` / `max_height` of
    /// `0` request that auto-detection.
    #[cfg(target_os = "linux")]
    #[tracing::instrument(skip(self, config), fields(rhi_op = "create_decoder_session"))]
    pub(crate) fn create_decoder_session(
        &self,
        config: crate::vulkan::video::decode::SimpleDecoderConfig,
    ) -> Result<crate::vulkan::video::decode::SimpleDecoder> {
        let host_device = Arc::clone(&self.device.inner);
        crate::vulkan::video::decode::SimpleDecoder::from_host_device(host_device, config)
            .map_err(|e| Error::GpuError(format!("create_decoder_session: {e}")))
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
    // GPU Blit Operations
    // =========================================================================

    /// Copy pixels between same-format, same-size buffers.
    ///
    /// Uses GPU blit with texture caching for efficient repeated copies.
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        self.blitter.blit_copy(src, dest)
    }

    /// Clear the blitter's texture cache to free GPU memory.
    pub fn clear_blitter_cache(&self) {
        self.blitter.clear_cache();
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

    /// Install the engine's own surface store on this context — the engine's
    /// — as both its surface store and the one the shared pools and export
    /// stagings register through. Installed once; a second is refused.
    pub(crate) fn install_the_engines_surface_store(&self, store: SurfaceStore) -> Result<()> {
        self.engines_own_surface_store
            .set(store.clone())
            .map_err(|_| {
                Error::GpuError(
                    "the engine's own surface store is installed once, and it already was".into(),
                )
            })?;
        *self
            .surface_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store);
        Ok(())
    }

    /// The engine's own surface store, `None` until the engine installs it.
    pub(crate) fn engines_own_surface_store(&self) -> Option<&SurfaceStore> {
        self.engines_own_surface_store.get()
    }

    /// Clear this view's surface store, closing its connection to the
    /// surface-sharing service. Called as the stream whose view this is stops.
    pub(crate) fn clear_surface_store(&self) {
        // A poisoned slot is still cleared: a stream's stop must not panic on
        // a lock another thread's panic left behind.
        *self
            .surface_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.surface_store.lock().unwrap().clone()
    }

    // =========================================================================
    // Compute kernels for the escalate ops — always present, never installed
    // =========================================================================

    /// Compile GLSL kernel source to SPIR-V, reusing what an identical earlier
    /// request compiled.
    ///
    /// `label` is what the compiler's diagnostics name the source as, so it is
    /// the prefix an author sees on a syntax error.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compile_glsl_shader_source_to_spirv(
        &self,
        source: &str,
        stage: crate::core::rhi::GlslCompilationTargetStage,
        entry_point: &str,
        label: &str,
    ) -> Result<Arc<[u8]>> {
        self.glsl_shader_source_compiler
            .compile_or_reuse(source, stage, entry_point, label)
    }

    /// How many times this context has actually run the GLSL compiler.
    ///
    /// What a cache-hit assertion counts; elapsed time cannot stand in for it,
    /// since re-creating a kernel is free of compilation while still
    /// allocating handles.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[must_use]
    pub fn glsl_shader_compiler_invocation_count(&self) -> u64 {
        self.glsl_shader_source_compiler.invocation_count()
    }

    /// How many queue submissions this context's device has made.
    ///
    /// What a batching assertion counts. Elapsed time cannot stand in for it:
    /// a batch is faster than N dispatches for reasons a loaded machine can
    /// hide, while the submission count says outright whether the work went
    /// out as one command buffer or N.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[must_use]
    pub fn queue_submission_count(&self) -> usize {
        self.device.inner.queue_submission_count()
    }

    /// How many times a command recorder or a compute kernel has blocked on a
    /// fence — the stalls a batch exists to collapse.
    ///
    /// Recorder-wide, not compute-only: present, staging and the tone mapper
    /// drain recorders too, so a reading is only about a batch when nothing
    /// else on this device is recording. See [`Self::queue_submission_count`]
    /// on why this is counted, not timed.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[must_use]
    pub fn recorder_and_compute_kernel_fence_wait_count(&self) -> usize {
        self.device
            .inner
            .recorder_and_compute_kernel_fence_wait_count()
    }

    /// Build a compute kernel for a caller that named it by SPIR-V, reusing an
    /// identical one if this context already built it.
    ///
    /// The cache key covers everything that changes the compiled output. For a
    /// pre-compiled blob that is the blob itself plus the declared
    /// push-constant size — the bytes already fix stage, entry point and target
    /// environment. A GLSL source contract adds the compiler's own version to
    /// the key, because then the engine is what produces the bytes.
    ///
    /// Returns the cache key as the kernel id: a caller re-registering the same
    /// kernel gets the same id back, and pays no compilation for it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_compute_kernel(
        &self,
        spv: &[u8],
        push_constant_size: u32,
        declared_bindings: &[crate::core::rhi::ComputeBindingDeclaration],
        entry_point: &str,
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanComputeKernel>)> {
        let kernel_id = compute_kernel_cache_key(spv, push_constant_size, entry_point);
        let cached_kernel = self
            .compute_kernel_cache
            .lock()
            .unwrap()
            .get(&kernel_id)
            .map(Arc::clone);
        if let Some(cached) = cached_kernel {
            // The declaration is checked on the hit path too: the cache key
            // covers the blob, not the caller's assertion, and a wrong
            // assertion must refuse identically whether or not somebody
            // registered this blob first.
            crate::core::rhi::reconcile_compute_binding_declarations(
                declared_bindings,
                &cached.bindings(),
            )?;
            tracing::debug!(
                rhi_op = "create_or_reuse_compute_kernel",
                kernel_id,
                "GpuContext::create_or_reuse_compute_kernel — cache hit"
            );
            return Ok((kernel_id, Arc::clone(&cached)));
        }

        // Reflection is the source of truth for the binding shape; a caller's
        // declaration is checked against it rather than replacing it.
        let (reflected, reflected_push_size) = crate::core::rhi::derive_bindings_from_spirv(spv)?;
        crate::core::rhi::reconcile_compute_binding_declarations(declared_bindings, &reflected)?;
        if reflected_push_size != push_constant_size {
            return Err(Error::GpuError(format!(
                "compute kernel declares {push_constant_size} push-constant bytes but its \
                 SPIR-V reflects {reflected_push_size}"
            )));
        }

        let kernel = Arc::new(self.create_compute_kernel(
            &crate::core::rhi::ComputeKernelDescriptor {
                entry_point,
                label: "escalate-compute-kernel",
                spv,
                bindings: &reflected,
                push_constant_size,
            },
        )?);

        Ok((
            kernel_id.clone(),
            Arc::clone(
                self.compute_kernel_cache
                    .lock()
                    .unwrap()
                    .entry(kernel_id)
                    .or_insert(kernel),
            ),
        ))
    }

    /// Look up a compute kernel a prior `create_or_reuse_compute_kernel`
    /// returned.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compute_kernel_by_id(
        &self,
        kernel_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.compute_kernel_cache
            .lock()
            .unwrap()
            .get(kernel_id)
            .map(Arc::clone)
    }

    /// Build a graphics kernel for the `register_graphics_kernel` escalate op,
    /// or hand back one an identical earlier registration already built.
    ///
    /// The twin of [`Self::create_or_reuse_compute_kernel`], and the same
    /// contract: reflection is the source of truth for the binding shape, the
    /// caller's declaration is checked against it by name rather than
    /// replacing it, and the cache key is what the caller gets back as the
    /// kernel id.
    ///
    /// `label` names the pipeline in RHI errors and validation-layer messages,
    /// and is deliberately outside the cache key — two registrations differing
    /// only in label are one pipeline, and the first one's label is the one the
    /// driver keeps.
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
        use crate::core::rhi::GraphicsShaderStageFlags;

        // Both stages are always present — `VulkanGraphicsKernel::new` refuses
        // anything else — so every graphics kernel is built from both.
        let stages_the_kernel_was_built_from = GraphicsShaderStageFlags::VERTEX_FRAGMENT;
        let kernel_id = graphics_kernel_cache_key(
            stages,
            declared_push_constants,
            pipeline_state,
            descriptor_sets_in_flight,
        );
        let cached_kernel = self
            .graphics_kernel_cache
            .lock()
            .unwrap()
            .get(&kernel_id)
            .map(Arc::clone);
        if let Some(cached) = cached_kernel {
            // The declaration is checked on the hit path too: the cache key
            // covers the shaders and the pipeline, not the caller's assertion,
            // and a wrong assertion must refuse identically whether or not
            // somebody registered this kernel first.
            crate::core::rhi::reconcile_graphics_binding_declarations(
                declared_bindings,
                &cached.bindings(),
                stages_the_kernel_was_built_from,
            )?;
            tracing::debug!(
                rhi_op = "create_or_reuse_graphics_kernel",
                kernel_id,
                "GpuContext::create_or_reuse_graphics_kernel — cache hit"
            );
            return Ok((kernel_id, Arc::clone(&cached)));
        }

        let (reflected, reflected_push_constants) =
            crate::core::rhi::derive_bindings_from_spirv_multistage(stages)?;
        crate::core::rhi::reconcile_graphics_binding_declarations(
            declared_bindings,
            &reflected,
            stages_the_kernel_was_built_from,
        )?;
        let push_constants = crate::core::rhi::GraphicsPushConstants {
            size: reflected_push_constants.size,
            stages: reconciled_push_constant_stages(
                "graphics",
                declared_push_constants.size,
                declared_push_constants.stages,
                reflected_push_constants.size,
                reflected_push_constants.stages,
            )?,
        };

        let kernel = Arc::new(self.create_graphics_kernel(
            &crate::core::rhi::GraphicsKernelDescriptor {
                label,
                stages,
                bindings: &reflected,
                push_constants,
                pipeline_state: pipeline_state.clone(),
                descriptor_sets_in_flight,
            },
        )?);

        Ok((
            kernel_id.clone(),
            Arc::clone(
                self.graphics_kernel_cache
                    .lock()
                    .unwrap()
                    .entry(kernel_id)
                    .or_insert(kernel),
            ),
        ))
    }

    /// Look up a graphics kernel a prior `create_or_reuse_graphics_kernel`
    /// returned, holding the draw recorder for as long as the caller keeps it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn graphics_kernel_by_id_held_for_one_draw(
        &self,
        kernel_id: &str,
    ) -> Option<GraphicsKernelHeldForOneDraw<'_>> {
        let input_transition_recorder_slot = self.graphics_draw_input_transition_recorder.lock();
        let kernel = self
            .graphics_kernel_cache
            .lock()
            .unwrap()
            .get(kernel_id)
            .map(Arc::clone)?;
        Some(GraphicsKernelHeldForOneDraw {
            kernel,
            input_transition_recorder_slot,
        })
    }

    /// Build a ray-tracing kernel for the `register_ray_tracing_kernel`
    /// escalate op, or hand back one an identical earlier registration already
    /// built.
    ///
    /// The twin of [`Self::create_or_reuse_compute_kernel`]. Unlike graphics,
    /// a ray-tracing kernel's stage set varies per kernel, so the stages it was
    /// actually built from are what a declaration naming a stage is checked
    /// against.
    ///
    /// `label` names the pipeline in RHI errors and validation-layer messages,
    /// and is deliberately outside the cache key — two registrations differing
    /// only in label are one pipeline, and the first one's label is the one the
    /// driver keeps.
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
        let stages_the_kernel_was_built_from =
            crate::core::rhi::ray_tracing_stages_covered_by(stages);
        let kernel_id = ray_tracing_kernel_cache_key(
            stages,
            groups,
            declared_push_constants,
            max_recursion_depth,
        );
        let cached_kernel = self
            .ray_tracing_kernel_cache
            .lock()
            .unwrap()
            .get(&kernel_id)
            .map(Arc::clone);
        if let Some(cached) = cached_kernel {
            crate::core::rhi::reconcile_ray_tracing_binding_declarations(
                declared_bindings,
                &cached.bindings(),
                stages_the_kernel_was_built_from,
            )?;
            tracing::debug!(
                rhi_op = "create_or_reuse_ray_tracing_kernel",
                kernel_id,
                "GpuContext::create_or_reuse_ray_tracing_kernel — cache hit"
            );
            return Ok((kernel_id, Arc::clone(&cached)));
        }

        let (reflected, reflected_push_constants) =
            crate::core::rhi::derive_ray_tracing_bindings_from_spirv_multistage(stages)?;
        crate::core::rhi::reconcile_ray_tracing_binding_declarations(
            declared_bindings,
            &reflected,
            stages_the_kernel_was_built_from,
        )?;
        let push_constants = crate::core::rhi::RayTracingPushConstants {
            size: reflected_push_constants.size,
            stages: reconciled_push_constant_stages(
                "ray-tracing",
                declared_push_constants.size,
                declared_push_constants.stages,
                reflected_push_constants.size,
                reflected_push_constants.stages,
            )?,
        };

        let kernel = Arc::new(self.create_ray_tracing_kernel(
            &crate::core::rhi::RayTracingKernelDescriptor {
                label,
                stages,
                groups,
                bindings: &reflected,
                push_constants,
                max_recursion_depth,
            },
        )?);

        Ok((
            kernel_id.clone(),
            Arc::clone(
                self.ray_tracing_kernel_cache
                    .lock()
                    .unwrap()
                    .entry(kernel_id)
                    .or_insert(kernel),
            ),
        ))
    }

    /// Look up a ray-tracing kernel a prior
    /// `create_or_reuse_ray_tracing_kernel` returned, holding the trace recorder
    /// for as long as the caller keeps it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn ray_tracing_kernel_by_id_held_for_one_trace(
        &self,
        kernel_id: &str,
    ) -> Option<RayTracingKernelHeldForOneTrace<'_>> {
        let input_transition_recorder_slot =
            self.ray_tracing_trace_input_transition_recorder.lock();
        let kernel = self
            .ray_tracing_kernel_cache
            .lock()
            .unwrap()
            .get(kernel_id)
            .map(Arc::clone)?;
        Some(RayTracingKernelHeldForOneTrace {
            kernel,
            input_transition_recorder_slot,
        })
    }

    /// Take ownership of a freshly built acceleration structure and return the
    /// id a later trace names it by.
    ///
    /// Every call mints a fresh id: an acceleration structure holds device
    /// memory proportional to its mesh, so unlike a kernel it is registered
    /// rather than deduplicated by content.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_acceleration_structure(
        &self,
        acceleration_structure: crate::vulkan::rhi::VulkanAccelerationStructure,
    ) -> String {
        let acceleration_structure_id = uuid::Uuid::new_v4().to_string();
        self.acceleration_structure_registry.lock().unwrap().insert(
            acceleration_structure_id.clone(),
            Arc::new(acceleration_structure),
        );
        acceleration_structure_id
    }

    /// Look up an acceleration structure a prior
    /// `register_acceleration_structure` returned the id of.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acceleration_structure_by_id(
        &self,
        acceleration_structure_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanAccelerationStructure>> {
        self.acceleration_structure_registry
            .lock()
            .unwrap()
            .get(acceleration_structure_id)
            .map(Arc::clone)
    }

    /// Drop the registry's strong reference to an acceleration structure,
    /// answering whether an entry was there to drop.
    ///
    /// This is what a Rust caller's `VulkanAccelerationStructure` going out of
    /// scope does. The device memory returns once the last reference does — a
    /// TLAS holds its own reference to every BLAS it instances, so releasing a
    /// BLAS a scene still uses frees nothing until the scene goes too.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn release_acceleration_structure(&self, acceleration_structure_id: &str) -> bool {
        self.acceleration_structure_registry
            .lock()
            .unwrap()
            .remove(acceleration_structure_id)
            .is_some()
    }

    /// Record every dispatch in `batch` into one command buffer, submit once,
    /// and return when that submission has retired.
    ///
    /// The cost this exists to avoid is per-dispatch: N dispatches recorded
    /// separately cost N submissions and N fence waits — the single-dispatch
    /// escalate op runs as a recording of one through this very method.
    /// Batched, N passes cost one submission and one wait, and a caller that
    /// leaves this call has its writes visible, exactly as after a single
    /// dispatch.
    ///
    /// Each dispatch's bindings are barriered into the layout its descriptor
    /// requires. Those barriers are also the write-then-read edge between
    /// consecutive passes, so pass N+1 observes pass N's stores. An image's
    /// first barrier in the recording takes a wide source scope — whatever
    /// wrote it before the batch is not the batch's to know — and narrows to
    /// compute-to-compute once this recording is the writer.
    ///
    /// One kernel may appear only once. A kernel owns a single descriptor set,
    /// so a second bind would retarget the dispatch already recorded against it
    /// — silently, since nothing has executed yet. Refused by name rather than
    /// dispatched wrong.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn dispatch_compute_kernel_batch(
        &self,
        batch: &[BatchedComputeKernelDispatch],
    ) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        for (index, dispatch) in batch.iter().enumerate() {
            if let Some(earlier) = batch[..index]
                .iter()
                .position(|prior| prior.kernel.handle == dispatch.kernel.handle)
            {
                return Err(Error::GpuError(format!(
                    "dispatch {index} of this batch names the kernel dispatch {earlier} already \
                     names; a kernel owns one descriptor set, so binding it twice in one \
                     recording would give dispatch {earlier} this dispatch's bindings. Build a \
                     second kernel, or run them as separate batches"
                )));
            }
        }

        let mut batched_compute_dispatch_recorder_slot =
            self.batched_compute_dispatch_recorder.lock();
        let recorder = match batched_compute_dispatch_recorder_slot.as_mut() {
            Some(recorder) => recorder,
            None => batched_compute_dispatch_recorder_slot
                .insert(self.create_command_recorder("batched_compute_dispatch")?),
        };

        // From here on the recorder is open, and every early return has to
        // close it or the next batch's begin() refuses.
        recorder.begin()?;
        let layout_each_image_landed_in = match Self::record_compute_kernel_batch(recorder, batch) {
            Ok(landed_in) => landed_in,
            Err(e) => {
                recorder.abort_recording();
                return Err(e);
            }
        };
        recorder.submit()?;

        // Published between the submit and the wait, not after both: once the
        // queue has taken the recording the transitions belong to the GPU, so a
        // wait that fails must not leave the tracked layouts describing a state
        // the hardware has already left. A batch that failed while recording
        // never reaches here, and its textures are still as they arrived.
        //
        // Per binding, not per image: a cross-process resolve synthesizes a
        // fresh registration per call, so two slots naming one image hold two
        // layout cells — and every cell must learn the layout the recording
        // left the image in, or the stale one is what gets published.
        for binding in batch.iter().flat_map(|dispatch| &dispatch.bindings) {
            let Some((registration, _)) =
                binding.surface_bound_resource.texture_and_required_layout()
            else {
                continue;
            };
            let Some(image) = registration.texture().vulkan_inner().image() else {
                continue;
            };
            if let Some(layout_landed_in) = layout_each_image_landed_in.get(&image) {
                registration.update_layout(*layout_landed_in);
            }
        }
        recorder.wait_for_completion()?;

        tracing::debug!(
            rhi_op = "dispatch_compute_kernel_batch",
            dispatches = batch.len(),
            "GpuContext::dispatch_compute_kernel_batch — one submission, one wait"
        );
        Ok(())
    }

    /// The recording half of [`Self::dispatch_compute_kernel_batch`], split out
    /// so every failure path funnels through one `abort_recording`.
    ///
    /// Returns the layout each bound texture ends the recording in, for the
    /// caller to publish once the submission has retired.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn record_compute_kernel_batch(
        recorder: &mut crate::vulkan::rhi::RhiCommandRecorder,
        batch: &[BatchedComputeKernelDispatch],
    ) -> Result<HashMap<vulkanalia::vk::Image, VulkanLayout>> {
        use crate::vulkan::rhi::{VulkanAccess, VulkanBufferLike as _, VulkanStage};

        // The layout each image is in *as the recording proceeds*. Tracking it
        // here rather than re-reading the registration is what makes a chain
        // work: pass 1 leaves its output in GENERAL, and barriering pass 2's
        // read from the pre-batch layout — typically UNDEFINED for a freshly
        // acquired texture — would discard exactly the writes pass 2 is there
        // to read.
        //
        // Keyed on the image, which is what a layout belongs to, and not on
        // the registration: a cross-process import synthesizes a fresh
        // registration per resolve, so two passes over one imported surface
        // would otherwise track two independent layouts for one image.
        let mut layout_during_recording: HashMap<vulkanalia::vk::Image, VulkanLayout> =
            HashMap::new();
        // A buffer has no layout to track, only whether this recording has
        // touched it yet — which decides the barrier's source scope as it
        // does for an image.
        let mut buffers_touched_in_this_recording: std::collections::HashSet<
            vulkanalia::vk::Buffer,
        > = std::collections::HashSet::new();

        // The source scope narrows only once the batch is the writer.
        // On a resource's first touch the previous writer is whatever
        // the graph did — a transfer upload, a camera, another node,
        // CUDA — so the wide scope every other entry-from-an-unknown-
        // producer barrier in the engine uses is the only correct one.
        // From the second touch on, this recording wrote it, and
        // compute-to-compute is exactly the dependency to name.
        let barrier_source_stage_and_access_for_touch = |first_touch_in_this_recording: bool| {
            if first_touch_in_this_recording {
                (VulkanStage::ALL_COMMANDS, VulkanAccess::MEMORY_WRITE)
            } else {
                (VulkanStage::COMPUTE_SHADER, VulkanAccess::SHADER_WRITE)
            }
        };

        for (dispatch_index, dispatch) in batch.iter().enumerate() {
            for binding in &dispatch.bindings {
                if let Some(buffer) = binding.surface_bound_resource.storage_buffer() {
                    let (from_stage, from_access) = barrier_source_stage_and_access_for_touch(
                        buffers_touched_in_this_recording.insert(buffer.vk_buffer()),
                    );
                    // Recorded on every touch: it carries the previous pass's
                    // stores to this pass's loads.
                    recorder.record_buffer_barrier(
                        buffer,
                        from_stage,
                        VulkanStage::COMPUTE_SHADER,
                        from_access,
                        VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
                    )?;
                    continue;
                }
                let Some((registration, required_layout)) =
                    binding.surface_bound_resource.texture_and_required_layout()
                else {
                    continue;
                };
                let image = registration
                    .texture()
                    .vulkan_inner()
                    .image()
                    .ok_or_else(|| {
                        Error::GpuError(format!(
                            "{} names a texture with no image, which a descriptor cannot be \
                             written from",
                            binding_location_in_this_recording(
                                batch,
                                dispatch_index,
                                binding.binding
                            )
                        ))
                    })?;
                let first_touch_in_this_recording = !layout_during_recording.contains_key(&image);
                let layout_so_far = *layout_during_recording
                    .entry(image)
                    .or_insert_with(|| registration.current_layout());
                let (from_stage, from_access) =
                    barrier_source_stage_and_access_for_touch(first_touch_in_this_recording);
                // Recorded even when the layout already matches: the barrier is
                // carrying the previous pass's stores to this pass's reads, and
                // a same-layout transition is exactly that memory dependency.
                recorder.record_image_barrier(
                    registration.texture(),
                    layout_so_far,
                    required_layout,
                    from_stage,
                    VulkanStage::COMPUTE_SHADER,
                    from_access,
                    VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
                )?;
                layout_during_recording.insert(image, required_layout);
            }

            for binding in &dispatch.bindings {
                binding.write_into_kernel(&dispatch.kernel)?;
            }
            // A kernel that declares push constants must be given them even
            // when the payload is empty, so `set_push_constants` produces the
            // size mismatch rather than the dispatch running against whatever
            // the kernel's staged buffer last held.
            if dispatch.kernel.push_constant_size() > 0 || !dispatch.push_constants.is_empty() {
                dispatch
                    .kernel
                    .set_push_constants(&dispatch.push_constants)?;
            }

            recorder.record_dispatch(
                &dispatch.kernel,
                dispatch.group_count_x,
                dispatch.group_count_y,
                dispatch.group_count_z,
            )?;
        }
        Ok(layout_during_recording)
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
                NO_SURFACE_STORE_IS_CONNECTED_TO_THIS_GPU_CONTEXT.into(),
            )
        })?;
        store.check_in(pixel_buffer)
    }

    /// Check out a surface by ID, returning the pixel buffer.
    ///
    /// Returns from local cache if available, otherwise looks the surface up
    /// over the surface-share service's Mach channel and imports its
    /// IOSurface, zero-copy.
    #[cfg(target_os = "macos")]
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        let store = self.surface_store.lock().unwrap();
        let store = store.as_ref().ok_or_else(|| {
            crate::core::Error::Configuration(
                NO_SURFACE_STORE_IS_CONNECTED_TO_THIS_GPU_CONTEXT.into(),
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
    /// reconfigure (acquire a new video session, resize a swapchain,
    /// etc.).
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
        unsafe {
            let arc = &*(self.handle as *const std::sync::Arc<GpuContext>);
            &**arc
        }
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

    /// Build a swapchain-backed [`crate::vulkan::rhi::VulkanPresentTarget`]
    /// on a window's surface source.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_present_target(
        &self,
        surface_source: crate::vulkan::rhi::PresentSurfaceSource<'_>,
        width: u32,
        height: u32,
        vsync: bool,
        color_traits: Option<&crate::core::color::ColorTraits>,
    ) -> Result<crate::vulkan::rhi::VulkanPresentTarget> {
        self.host_inner().create_vulkan_present_target(
            surface_source,
            width,
            height,
            vsync,
            color_traits,
        )
    }

    /// Build a [`crate::vulkan::rhi::VulkanPresentCompositor`] for
    /// `attachment_format` (typically the present target's
    /// [`color_format`](crate::vulkan::rhi::VulkanPresentTarget::color_format)).
    /// In-process (Boxed) only.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_present_compositor(
        &self,
        attachment_format: crate::core::rhi::TextureFormat,
    ) -> Result<crate::vulkan::rhi::VulkanPresentCompositor> {
        self.host_inner()
            .create_present_compositor(attachment_format)
    }

    /// Mint a Vulkan Video encoder on the host device — the FullAccess mirror
    /// of [`GpuContext::create_encoder_session`] the video codec seam's Vulkan
    /// Video arm opens an encode session with.
    #[cfg(target_os = "linux")]
    pub(crate) fn create_encoder_session(
        &self,
        config: crate::vulkan::video::encode::SimpleEncoderConfig,
    ) -> Result<crate::vulkan::video::encode::SimpleEncoder> {
        self.host_inner().create_encoder_session(config)
    }

    /// Mint a Vulkan Video decoder on the host device — the FullAccess mirror
    /// of [`GpuContext::create_decoder_session`] the video codec seam's Vulkan
    /// Video arm opens a decode session with.
    #[cfg(target_os = "linux")]
    pub(crate) fn create_decoder_session(
        &self,
        config: crate::vulkan::video::decode::SimpleDecoderConfig,
    ) -> Result<crate::vulkan::video::decode::SimpleDecoder> {
        self.host_inner().create_decoder_session(config)
    }

    /// Wait for the GPU device to become idle.
    ///
    pub fn wait_device_idle(&self) -> Result<()> {
        self.host_inner().wait_device_idle()
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

    /// Register a PipeWire `Video/Source` node with `media.role = Camera`, so
    /// a portal-based application can select this graph as a camera.
    ///
    /// The door built-ins reach the PipeWire arm through: a virtual camera
    /// needs the engine's DMA-BUF textures to offer, so the node is minted
    /// where those are, and no built-in holds raw PipeWire.
    #[cfg(target_os = "linux")]
    pub fn open_pipewire_camera_node(
        &self,
        camera_name: &str,
    ) -> Result<crate::linux::pipewire_video_source::PipeWireCameraNode> {
        tracing::debug!(
            rhi_op = "open_pipewire_camera_node",
            camera_name,
            "GpuContextFullAccess::open_pipewire_camera_node"
        );
        crate::linux::pipewire_video_source::PipeWireCameraNode::open(self, camera_name)
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
    /// rotation in `process()`. See `docs/architecture/texture-ring.md`
    /// for the recipe and CLAUDE.md → "Texture rings — single
    /// canonical abstraction" for the engine-model context.
    ///
    /// `count` is rejected if zero; sizing to
    /// `MAX_FRAMES_IN_FLIGHT = 2`
    /// (`docs/learnings/vulkan-frames-in-flight.md`) is the standard
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

    /// Create a Vulkan video session — the privileged
    /// `VkVideoSessionKHR` + bound device memory the codec layer
    /// uses for `vkCmdDecodeVideoKHR` / `vkCmdEncodeVideoKHR`.
    ///
    /// FullAccess-only and host-only: helper processes do not
    /// build their own codec layers — codecs live inside
    /// the host engine.
    #[cfg(target_os = "linux")]
    pub fn create_video_session(
        &self,
        descriptor: &crate::vulkan::rhi::VideoSessionDescriptor<'_>,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::HostVulkanVideoSession>> {
        self.host_inner().create_video_session(descriptor)
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
        self.host_inner()
            .create_video_session_parameters(session, descriptor)
    }

    /// Allocate a video DPB (Decoded Picture Buffer) image bound to a
    /// codec profile — the engine-RHI primitive the codec layer uses
    /// for reference-picture and decode-target images.
    ///
    /// FullAccess-only and host-only: codecs live inside the
    /// host engine, so helper processes do not construct DPB images
    /// directly — they consume codec output through the surface-share
    /// registry.
    #[cfg(target_os = "linux")]
    pub fn create_video_dpb_texture(
        &self,
        descriptor: &crate::vulkan::rhi::VideoDpbTextureDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanTexture> {
        self.host_inner().create_video_dpb_texture(descriptor)
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
        self.host_inner().create_video_bitstream_buffer(descriptor)
    }

    /// Allocate a Vulkan query pool — the generic engine-RHI primitive
    /// servicing every query class (timestamp, occlusion,
    /// pipeline-statistics, video-encode-feedback). FullAccess-only;
    /// helper processes do not construct query pools — they consume
    /// codec results (when applicable) through the surface-share /
    /// escalate IPC channels, not by reaching into pool primitives.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_query_pool(
        &self,
        descriptor: &crate::vulkan::rhi::QueryPoolDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::HostVulkanQueryPool> {
        self.host_inner().create_query_pool(descriptor)
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

    /// Import a caller-owned host range for GPU writes. See
    /// [`GpuContext::import_host_mapping_for_gpu_writes`](crate::core::context::GpuContext::import_host_mapping_for_gpu_writes).
    ///
    /// FullAccess-only: it allocates device memory (an import or a staging
    /// buffer), which is setup-time work under the escalate gate.
    ///
    /// # Safety
    ///
    /// The range must stay mapped, writable and unaliased until the returned
    /// value drops.
    #[cfg(target_os = "linux")]
    pub unsafe fn import_host_mapping_for_gpu_writes(
        &self,
        host_range_ptr: *mut u8,
        host_range_byte_len: usize,
    ) -> Result<crate::vulkan::rhi::HostMappingWrittenByGpu> {
        // SAFETY: the caller upholds this method's own contract.
        unsafe {
            self.host_inner()
                .import_host_mapping_for_gpu_writes(host_range_ptr, host_range_byte_len)
        }
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
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::core::test_support::wait_for_every_gpu_submission_made_so_far;

    /// A kernel id names a pipeline, and two pipelines built from one module
    /// against different entry points are different pipelines. Serving the
    /// first for the second would dispatch a function the caller did not ask
    /// for.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_kernel_id_tells_two_entry_points_over_one_module_apart() {
        let spv = b"not really spir-v, and this key never parses it";
        assert_ne!(
            compute_kernel_cache_key(spv, 0, "main"),
            compute_kernel_cache_key(spv, 0, "sharpen"),
        );
    }

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

    /// A same-process producer that publishes only a pixel buffer (no
    /// texture registration) must resolve through the pool's local cache —
    /// the cross-process surface store cannot serve OPAQUE_FD-backed buffers
    /// to a host-side consumer at all. Mental-revert: removing the
    /// pool-cache arm in `resolve_texture_registration_by_surface_id`'s
    /// Path 3 makes this test fail with "No texture or pixel buffer found".
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn same_process_pixel_buffer_resolves_without_the_surface_store() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        let (pool_id, pixel_buffer) = gpu
            .acquire_pixel_buffer(16, 16, PixelFormat::Rgba32)
            .expect("acquire pixel buffer");

        let registration = gpu
            .resolve_texture_registration_by_surface_id(&pool_id.to_string(), None, 16, 16)
            .expect("pixel-buffer-only surface must resolve same-process");
        assert_eq!(registration.texture().width(), 16);
        assert_eq!(registration.texture().height(), 16);
        assert_eq!(
            registration.current_layout(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            "the refresh upload leaves the texture shader-readable"
        );
        drop(pixel_buffer);
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
            .wrap_storage_buffer_as_pixel_buffer(
                &storage,
                W,
                H,
                BPP,
                crate::core::rhi::PixelFormat::Bgra32,
            )
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

    /// A privileged FullAccess plugin passing oversized dimensions (or a
    /// source buffer smaller than width*height*4) must get a typed
    /// `Error::GpuError` from `upload_pixel_buffer_as_texture` BEFORE any
    /// `vkCmdCopyBufferToImage` submit — not a GPU-side out-of-bounds read
    /// of the HOST_VISIBLE staging buffer (`VK_ERROR_DEVICE_LOST`). #1388 is
    /// the first SDK/plugin exposure of this slot, so this closes the
    /// device-fault mode the ABI is meant to close.
    ///
    /// Mental-revert: drop the required-byte-size guard in
    /// `upload_pixel_buffer_as_texture` and this call records a
    /// 4096x4096x4 (64 MiB) tightly-packed copy region against a 64-byte
    /// source buffer, faulting the device instead of returning. GPU-gated:
    /// skips cleanly with no device (CI is GPU-free) — a /verify-live
    /// regression.
    #[test]
    #[cfg(target_os = "linux")]
    fn upload_pixel_buffer_as_texture_rejects_oversized_dimensions() {
        use std::sync::Arc;

        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        // 4x4 RGBA8 HOST_VISIBLE source = 64 bytes.
        const SRC_W: u32 = 4;
        const SRC_H: u32 = 4;
        const SRC_BYTES: u64 = (SRC_W as u64) * (SRC_H as u64) * 4;
        let host_buffer =
            match crate::vulkan::rhi::HostVulkanBuffer::new(&gpu.device().inner, SRC_BYTES) {
                Ok(b) => b,
                Err(e) => {
                    println!("Skipping - HOST_VISIBLE buffer allocation failed: {e}");
                    return;
                }
            };
        let pixel_buffer = crate::core::rhi::PixelBuffer::from_host_vulkan_buffer(
            Arc::new(host_buffer),
            SRC_W,
            SRC_H,
            4,
            crate::core::rhi::PixelFormat::Rgba32,
        );

        // Oversized copy region: 4096x4096x4 = 64 MiB, far past the 64-byte
        // source. Without the guard this is a GPU out-of-bounds read of the
        // staging buffer.
        let result = gpu.upload_pixel_buffer_as_texture(
            "oversized_upload_regression",
            &pixel_buffer,
            4096,
            4096,
        );

        let err = result.expect_err(
            "oversized upload_pixel_buffer_as_texture must return a typed error, not submit a \
             GPU out-of-bounds copy",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("upload_pixel_buffer_as_texture") && msg.contains("bytes"),
            "expected a byte-size validation error, got: {msg}"
        );
        println!("upload_pixel_buffer_as_texture oversized-dimension guard OK — {msg}");
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
    fn test_texture_cache_miss() {
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

        println!("Texture cache miss: OK");
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

    /// Two processors driving one format pair from their own threads must
    /// not share a kernel's staged bindings: the cached handle is one
    /// object, an owned converter is the caller's alone.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — needs a GPU device; see docs/testing-hardware.md"
    )]
    #[test]
    fn an_owned_color_converter_shares_no_kernel_with_the_cached_one() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(gpu) => gpu,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };
        let cached_once = gpu
            .color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
            .expect("cached");
        let cached_twice = gpu
            .color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
            .expect("cached again");
        let owned_first = gpu
            .create_color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
            .expect("owned");
        let owned_second = gpu
            .create_color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
            .expect("owned again");

        assert!(
            std::ptr::eq(cached_once.host_inner(), cached_twice.host_inner()),
            "the cache hands out one converter per format pair"
        );
        assert!(
            !std::ptr::eq(owned_first.host_inner(), cached_once.host_inner()),
            "an owned converter is not the cached one"
        );
        assert!(
            !std::ptr::eq(owned_first.host_inner(), owned_second.host_inner()),
            "two owners get two converters"
        );
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
    /// kernel handle's Drop is independent of any active escalate scope
    /// (the scope token only validates FullAccess CALL dispatch; drop is
    /// a refcount decrement on an opaque handle).
    ///
    /// Mental revert: wiring the drop to require a live escalate
    /// scope would crash here because the scope is closed before the
    /// drop runs.
    #[cfg(target_os = "linux")]
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
                    entry_point: "main",
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

        let buffer = limited
            .acquire_storage_buffer(
                &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_size)
                    .expect("a non-zero size"),
            )
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
            .acquire_storage_buffer(
                &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_size)
                    .expect("a non-zero size"),
            )
            .expect("FullAccess mirror should succeed");
        assert_eq!(buffer2.byte_size(), byte_size);

        println!(
            "GpuContextLimitedAccess::acquire_storage_buffer: {} bytes; FullAccess mirror also OK",
            byte_size
        );
    }

    /// A node's limited tier and the full tier its escalate hands out count
    /// one-offs against the one node, while the shared tier counts none.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_nodes_tiers_count_one_offs_against_the_node_while_the_shared_tier_counts_none() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };
        let layout =
            crate::core::rhi::TensorStorageBufferLayout::of_bytes(256).expect("a non-zero size");

        let node_tier = GpuContextLimitedAccess::counting_one_offs_against_the_node(
            gpu.clone(),
            Some(OneOffAcquisitionsCountedForOneNativeNode::new()),
        );
        let mut held_by_the_node = Vec::new();
        for _ in 0..8 {
            held_by_the_node.push(
                node_tier
                    .acquire_storage_buffer(&layout)
                    .expect("the node's limited tier acquires up to its count"),
            );
        }
        let held_through_the_escalate = node_tier
            .escalate(|full| {
                (0..8)
                    .map(|_| full.acquire_storage_buffer(&layout))
                    .collect::<Result<Vec<_>>>()
            })
            .expect("the escalate's full tier acquires up to the same count");
        held_by_the_node.extend(held_through_the_escalate);

        let refusal = node_tier
            .clone()
            .acquire_storage_buffer(&layout)
            .expect_err("a clone of the tier counts against the same node");
        assert!(
            matches!(
                refusal,
                Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld { .. }
            ),
            "the node's seventeenth is refused by its own count: {refusal:?}"
        );

        held_by_the_node.pop();
        let _in_the_room_handed_back = node_tier
            .acquire_storage_buffer(&layout)
            .expect("a dropped one-off hands its count back");

        let shared_tier = GpuContextLimitedAccess::new(gpu);
        let _held_by_no_node: Vec<_> = (0..17)
            .map(|_| {
                shared_tier
                    .acquire_storage_buffer(&layout)
                    .expect("the shared tier counts no one-offs")
            })
            .collect();
    }

    /// A node's output texture stays registered and counted while its handle
    /// lives, and dropping the handle unregisters its id and hands the count
    /// back, so a node acquiring one every frame cannot grow the cache.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn dropping_a_nodes_output_texture_unregisters_its_id_and_hands_its_count_back() {
        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };
        let one_offs_of_the_node = OneOffAcquisitionsCountedForOneNativeNode::new();
        let node_tier = GpuContextLimitedAccess::counting_one_offs_against_the_node(
            gpu.clone(),
            Some(Arc::clone(&one_offs_of_the_node)),
        );
        let output_texture_shape = OneOffAcquisitionShape::Texture {
            width: 64,
            height: 64,
            format: TextureFormat::Rgba8Unorm,
            usage: output_texture_usages(),
        };
        let is_registered = |id: &str| {
            gpu.texture_cache
                .lock()
                .unwrap()
                .contains_key(pool_slot_key_of_surface_id(id))
        };

        let (registered_under_id, output_texture) = node_tier
            .escalate(|full| full.acquire_output_texture(64, 64, TextureFormat::Rgba8Unorm))
            .expect("the node acquires an output texture");
        assert!(is_registered(&registered_under_id));
        assert_eq!(
            one_offs_of_the_node.held_count_of_shape(&output_texture_shape),
            1
        );

        drop(output_texture);
        assert!(
            !is_registered(&registered_under_id),
            "dropping the handle unregisters its id"
        );
        assert_eq!(
            one_offs_of_the_node.held_count_of_shape(&output_texture_shape),
            0,
            "dropping the handle hands its count back"
        );
    }

    // =====================================================================
    // The pool's cross-process taken-until-released test
    //
    // In-process, a held slot is an Arc above its baseline. These cover the
    // other holder the pool could not see until #1866: a helper child that
    // checked the surface out over the surface-share socket. See
    // `docs/decisions/surface-id-lifetime-contract.md`.
    // =====================================================================

    #[cfg(target_os = "linux")]
    const LEASE_TEST_SURFACE_WIDTH: u32 = 32;
    #[cfg(target_os = "linux")]
    const LEASE_TEST_SURFACE_HEIGHT: u32 = 32;

    /// A context whose pool can see cross-process holders.
    ///
    /// The store is deliberately never connected: the pool reads the lease
    /// table in this address space, and the socket exists only to carry the
    /// checkouts that write it. Registration warns and moves on, which is the
    /// same path a store whose service died already takes.
    #[cfg(target_os = "linux")]
    fn gpu_context_reading_check_out_leases_or_skip() -> Option<(
        GpuContext,
        Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
    )> {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            println!("Skipping - no GPU device available");
            return None;
        };
        let check_out_leases = Arc::new(crate::core::context::SurfaceCheckOutLeaseRegistry::new());
        gpu.install_the_engines_surface_store(SurfaceStore::new_reading_check_out_leases(
            "the-pool-reads-this-lease-table-in-process".to_string(),
            "surface-check-out-lease-test-runtime".to_string(),
            Arc::clone(&check_out_leases),
        ))
        .expect("the engine's surface store installs once");
        Some((gpu, check_out_leases))
    }

    #[cfg(target_os = "linux")]
    fn acquire_one_pool_slot_id(gpu: &GpuContext) -> Result<String> {
        // The handle is dropped here on purpose: from this point the slot is
        // free as far as the in-process refcount is concerned, so anything
        // that keeps the pool off it is a lease and nothing else.
        gpu.acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .map(|(published_frame_id, _returned_to_the_pool_immediately)| {
            published_frame_id.to_string()
        })
    }

    /// Slot-key equality: ids of *different frames* of one slot must count
    /// as the same slot, or a rehand would hide behind a fresh generation.
    #[cfg(target_os = "linux")]
    fn same_pool_slot(one_published_id: &str, another_published_id: &str) -> bool {
        pool_slot_key_of_surface_id(one_published_id)
            == pool_slot_key_of_surface_id(another_published_id)
    }

    /// The ids of the pool's pre-allocated slots, learned by cycling the ring
    /// once.
    #[cfg(target_os = "linux")]
    fn pool_slot_ids_in_ring_order(gpu: &GpuContext) -> Vec<String> {
        (0..POOL_PRE_ALLOCATE_COUNT)
            .map(|_| {
                acquire_one_pool_slot_id(gpu).expect("the pool hands out a slot when none is held")
            })
            .collect()
    }

    /// The contract itself: a frame a child holds is never handed back to the
    /// producer to overwrite, and comes back the moment the child lets go.
    ///
    /// Mental-revert: dropping the lease consult from `acquire` makes the
    /// first loop rehand the leased slot on its very next ring cycle — #1755's
    /// reproduction, at pool level.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_leased_slot_is_never_rehanded_to_its_producer() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let ring = pool_slot_ids_in_ring_order(&gpu);
        let held_by_a_child = ring[0].clone();
        let child = check_out_leases.mint_holder_id();
        check_out_leases
            .record_check_out_lease(&held_by_a_child, child)
            .expect("the child checks the frame out");

        for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 2) {
            let handed = acquire_one_pool_slot_id(&gpu).expect("free slots remain");
            assert!(
                !same_pool_slot(&handed, &held_by_a_child),
                "the producer was handed back the slot a child is still reading"
            );
        }

        check_out_leases
            .release_one_check_out_lease(&held_by_a_child, child)
            .expect("the child releases the frame");
        let comes_back = (0..(POOL_PRE_ALLOCATE_COUNT * 4)).any(|_| {
            acquire_one_pool_slot_id(&gpu)
                .is_ok_and(|handed| same_pool_slot(&handed, &held_by_a_child))
        });
        assert!(
            comes_back,
            "a released slot must return to the producer, or the lease is a leak"
        );
    }

    /// Under lease pressure the pool grows rather than making the producer
    /// wait or making a consumer's frame change — the "slow consumer costs
    /// memory first" half of the ruling.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_pool_grows_rather_than_rehanding_any_leased_slot() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let ring = pool_slot_ids_in_ring_order(&gpu);
        let child = check_out_leases.mint_holder_id();
        for slot in &ring {
            check_out_leases
                .record_check_out_lease(slot, child)
                .expect("the child checks every published frame out");
        }

        let grown_slot = acquire_one_pool_slot_id(&gpu)
            .expect("with every slot leased the pool grows instead of refusing");
        assert!(
            !ring
                .iter()
                .any(|leased| same_pool_slot(leased, &grown_slot)),
            "the pool handed back a leased slot instead of growing"
        );
    }

    /// The shared pool caps no shape by count: while a consumer holds every
    /// frame it is handed, each next acquire grows the pool. Past the heap
    /// budget the producer drops its own frame, refused at once and naming the
    /// heap — the consumer's cost is memory and then frames, never the
    /// producer's cadence.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn past_the_heap_budget_the_shared_pixel_buffer_pool_refuses_its_producer_at_once_rather_than_making_it_wait()
     {
        const FRAMES_THE_CHILD_HOLDS_WHILE_THE_SHARED_POOL_GROWS: usize = 68;

        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let child = check_out_leases.mint_holder_id();
        let leased_slots: Vec<String> = (0..FRAMES_THE_CHILD_HOLDS_WHILE_THE_SHARED_POOL_GROWS)
            .map(|_| {
                let slot = acquire_one_pool_slot_id(&gpu)
                    .expect("with every slot leased the shared pool grows");
                check_out_leases
                    .record_check_out_lease(&slot, child)
                    .expect("the child holds on to every frame it is given");
                slot
            })
            .collect();
        let pool_memory_type_index = gpu
            .get_pixel_buffer(&leased_slots[0])
            .expect("a leased frame still resolves")
            .buffer_ref()
            .inner
            .vma_allocation_memory_type_index()
            .expect("a pool slot is the allocator's own allocation");
        let pool_heap_index = gpu
            .device()
            .inner
            .device_memory_heap_of_memory_type(pool_memory_type_index)
            .expect("the slot's memory type is one of the device's");

        gpu.device()
            .inner
            .leave_no_room_in_any_frame_allocation_budget_for_this_test();
        let acquire_started = std::time::Instant::now();
        let refusal = acquire_one_pool_slot_id(&gpu)
            .expect_err("past the heap budget the producer is refused rather than served");
        let acquire_took = acquire_started.elapsed();

        match &refusal {
            Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget { heap_index, .. } => {
                assert_eq!(
                    *heap_index, pool_heap_index,
                    "the refusal names the heap the pool's buffers come from: {refusal}"
                );
            }
            other => panic!("expected the heap-budget refusal, got {other:?}"),
        }
        assert!(
            acquire_took < std::time::Duration::from_millis(250),
            "the refusal came after {acquire_took:?}: the producer was made to wait"
        );
    }

    /// Fail closed. A lease table that cannot be read cannot prove any slot is
    /// free, and a slot reused on a guess is a frame changing under a reader.
    /// Growth still serves the producer — a slot that has never existed cannot
    /// be checked out — so the observable rule is "never reuse", not "never
    /// acquire".
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_unreadable_lease_table_stops_the_pool_reusing_any_slot() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let ring = pool_slot_ids_in_ring_order(&gpu);

        let poisoning = Arc::clone(&check_out_leases);
        let _ = std::thread::spawn(move || {
            let _held = poisoning.hold_for_pool_slot_hand_off().unwrap();
            panic!("poison the lease table");
        })
        .join();

        for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 2) {
            let handed = acquire_one_pool_slot_id(&gpu).expect("growth still serves the producer");
            assert!(
                !ring.iter().any(|known| same_pool_slot(known, &handed)),
                "a slot was reused while the lease table was unreadable"
            );
        }
    }

    /// #1872 itself, at pool level: one slot, two acquisitions, two ids —
    /// and the first id stops resolving the moment the slot is recycled,
    /// in-process (cache eviction) and in the lease registry (generation
    /// ledger) alike.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn recycling_a_slot_retires_the_id_the_previous_frame_published() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };

        let first_published = acquire_one_pool_slot_id(&gpu).expect("first acquisition");
        gpu.resolve_pixel_buffer_by_surface_id(&first_published)
            .expect("a live published id resolves");

        // Cycle the whole ring so the first slot is recycled.
        let second_published = (0..POOL_PRE_ALLOCATE_COUNT)
            .map(|_| acquire_one_pool_slot_id(&gpu).expect("free slots remain"))
            .find(|later| same_pool_slot(later, &first_published))
            .expect("cycling the ring once rehands the first slot");

        assert_ne!(
            second_published, first_published,
            "recycling the slot must publish a new frame id"
        );
        gpu.resolve_pixel_buffer_by_surface_id(&second_published)
            .expect("the current frame id resolves");
        assert!(
            gpu.resolve_pixel_buffer_by_surface_id(&first_published)
                .is_err(),
            "a recycled slot's previous id must stop resolving — resolving it silently \
             serves somebody else's pixels, which is #1872"
        );
        let child = check_out_leases.mint_holder_id();
        assert!(
            matches!(
                check_out_leases.record_check_out_lease(&first_published, child),
                Err(Error::SurfaceFrameRecycled { .. })
            ),
            "a cross-process checkout of the retired id must be refused"
        );
    }

    /// The refusal is the pool's own, not the lease registry's: a context
    /// with no surface store wired in — no service, no registry — still
    /// refuses a retired id on the texture path, whose slot-keyed cache
    /// would otherwise serve the slot's current texture under the dead id.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_retired_id_is_refused_even_with_no_lease_registry() {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            println!("Skipping - no GPU device available");
            return;
        };

        let first_published = acquire_one_pool_slot_id(&gpu).expect("first acquisition");
        let desc = TextureDescriptor::new(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            TextureFormat::Rgba8Unorm,
        )
        .with_usage(TextureUsages::TEXTURE_BINDING);
        let producer_texture = gpu
            .device()
            .create_texture(&desc)
            .expect("texture creation");
        gpu.register_texture_with_layout(
            &first_published,
            producer_texture,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        gpu.resolve_texture_registration_by_surface_id(
            &first_published,
            None,
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
        )
        .expect("the live published id resolves its producer's texture");

        let second_published = (0..POOL_PRE_ALLOCATE_COUNT)
            .map(|_| acquire_one_pool_slot_id(&gpu).expect("free slots remain"))
            .find(|later| same_pool_slot(later, &first_published))
            .expect("cycling the ring once rehands the first slot");

        assert!(
            matches!(
                gpu.resolve_texture_registration_by_surface_id(
                    &first_published,
                    None,
                    LEASE_TEST_SURFACE_WIDTH,
                    LEASE_TEST_SURFACE_HEIGHT,
                ),
                Err(Error::SurfaceFrameRecycled { .. })
            ),
            "with no registry anywhere, the retired id must still be refused"
        );
        gpu.resolve_texture_registration_by_surface_id(
            &second_published,
            None,
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
        )
        .expect("the current frame id keeps resolving");
    }

    // =====================================================================
    // Native nodes' output pools over the shared pixel-buffer pool
    //
    // A native built-in's per-frame pixel buffers come from an output pool of
    // its own, whose slots it borrows from the shared ring of their shape: the
    // count is the node's, the memory the runtime's.
    // =====================================================================

    #[cfg(target_os = "linux")]
    const LEASE_TEST_SURFACE_SHAPE: PixelBufferPoolKey = PixelBufferPoolKey {
        width: LEASE_TEST_SURFACE_WIDTH,
        height: LEASE_TEST_SURFACE_HEIGHT,
        format: PixelFormat::Rgba32,
    };

    /// The slots of a shape one native node borrows from the shared ring while
    /// nothing holds its frames: one per frame of its rotation.
    #[cfg(target_os = "linux")]
    const SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD: usize =
        super::super::processor_output_pixel_buffer_pool_of_one_native_node::NATIVE_NODE_OUTPUT_PIXEL_BUFFER_POOL_ROTATION_DEPTH;

    /// The pre-allocated slots a shape's shared ring keeps once one native
    /// node has borrowed a slot per frame of its rotation.
    #[cfg(target_os = "linux")]
    const PRE_ALLOCATED_SLOTS_THE_SHARED_RING_KEEPS_PAST_ONE_NATIVE_NODES_ROTATION: usize =
        POOL_PRE_ALLOCATE_COUNT.saturating_sub(SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD);

    /// The next frame `native_node_output_pool` publishes, its buffer handed
    /// back at once so only a lease can keep the slot.
    #[cfg(target_os = "linux")]
    fn acquire_one_native_node_frame_id(
        native_node_output_pool: &mut super::super::ProcessorOutputPixelBufferPoolOfOneNativeNode,
    ) -> Result<String> {
        native_node_output_pool
            .acquire_the_next_output_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .map(|(published_frame_id, _returned_to_the_pool_immediately)| {
                published_frame_id.to_string()
            })
    }

    /// Each slot of `shape`'s shared ring, in ring order, with the generation
    /// it last published.
    #[cfg(target_os = "linux")]
    fn shared_ring_slots_and_generations(
        gpu: &GpuContext,
        shape: PixelBufferPoolKey,
    ) -> Vec<(String, u64)> {
        let pools = gpu.pixel_buffer_pool_manager.lock_the_shared_rings();
        pools.get(&shape).map_or_else(Vec::new, |ring_pool| {
            (0..ring_pool.ring.slot_count())
                .map(|slot_index| {
                    let slot = ring_pool.ring.slot(slot_index);
                    (
                        slot.pool_slot_key().to_string(),
                        slot.published_frame_generation(),
                    )
                })
                .collect()
        })
    }

    /// The generation `published_frame_id` names.
    #[cfg(target_os = "linux")]
    fn frame_generation_of(published_frame_id: &str) -> u64 {
        crate::core::rhi::split_pool_slot_and_frame_generation(published_frame_id)
            .expect("a published frame id carries its generation")
            .1
    }

    /// Lease every frame `native_node_output_pool` may hold, to `holder`.
    #[cfg(target_os = "linux")]
    fn lease_every_frame_a_native_node_may_hold(
        native_node_output_pool: &mut super::super::ProcessorOutputPixelBufferPoolOfOneNativeNode,
        check_out_leases: &crate::core::context::SurfaceCheckOutLeaseRegistry,
        holder: crate::core::context::SurfaceCheckOutLeaseHolderId,
    ) -> Vec<String> {
        (0..super::super::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY)
            .map(|_| {
                let frame = acquire_one_native_node_frame_id(native_node_output_pool)
                    .expect("a node under its cap is served");
                check_out_leases
                    .record_check_out_lease(&frame, holder)
                    .expect("a consumer holds the node's frame");
                frame
            })
            .collect()
    }

    /// A native node whose consumers hold every frame its pool may is refused
    /// only its own next frame: a second native node publishing the same
    /// shape, and the shared pool itself, go on being served.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_native_nodes_held_frames_never_refuse_a_second_native_nodes_frames_of_the_same_shape() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let consumer = check_out_leases.mint_holder_id();
        let mut first_native_node =
            gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("first-native-node");
        let mut second_native_node =
            gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("second-native-node");

        lease_every_frame_a_native_node_may_hold(
            &mut first_native_node,
            &check_out_leases,
            consumer,
        );
        assert!(
            matches!(
                acquire_one_native_node_frame_id(&mut first_native_node),
                Err(Error::EverySlotInTheProcessorOutputPoolIsInUse { .. })
            ),
            "the first node is refused at its own cap"
        );

        let second_nodes_frames: Vec<String> = (0
            ..super::super::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY)
            .map(|_| {
                let frame = acquire_one_native_node_frame_id(&mut second_native_node)
                    .expect("another node's frames of the same shape are served");
                check_out_leases
                    .record_check_out_lease(&frame, consumer)
                    .expect("a consumer holds the second node's frame");
                frame
            })
            .collect();
        assert_eq!(
            second_nodes_frames.len(),
            super::super::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY,
            "the second node reaches its own cap, untouched by the first's"
        );
        acquire_one_pool_slot_id(&gpu).expect(
            "the shared pool serves a frame of the same shape while both nodes hold theirs",
        );
    }

    /// A node at its cap is refused at once by the processor output pool's
    /// refusal, naming the node and the shape, before anything is allocated;
    /// a frame its consumer lets go of serves the next one.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_native_nodes_seventeenth_held_frame_is_refused_naming_its_pool() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let consumer = check_out_leases.mint_holder_id();
        let mut native_node =
            gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-held-native-node");
        let held_frames =
            lease_every_frame_a_native_node_may_hold(&mut native_node, &check_out_leases, consumer);
        let shared_ring_before_the_refusal =
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);

        let refusal_started = std::time::Instant::now();
        let refusal = acquire_one_native_node_frame_id(&mut native_node)
            .expect_err("a node holding every frame its pool may is refused");
        let refusal_took = refusal_started.elapsed();

        let expected_pool_key = format!(
            "native-node-output-pixel-buffer-pool-a-held-native-node-{LEASE_TEST_SURFACE_WIDTH}x{LEASE_TEST_SURFACE_HEIGHT}-{}",
            PixelFormat::Rgba32.wire_name()
        );
        match &refusal {
            Error::EverySlotInTheProcessorOutputPoolIsInUse {
                pool_key,
                pool_capacity,
            } => {
                assert_eq!(pool_key, &expected_pool_key);
                assert_eq!(
                    *pool_capacity,
                    super::super::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
                );
            }
            other => panic!("expected the node output pool's refusal, got {other:?}"),
        }
        assert!(
            refusal.to_string().contains(&expected_pool_key),
            "the refusal reads the node's pool: {refusal}"
        );
        assert!(
            refusal_took < std::time::Duration::from_millis(250),
            "the refusal came after {refusal_took:?}: the node was made to wait"
        );
        assert_eq!(
            native_node.slot_count(),
            super::super::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
        );
        assert_eq!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE),
            shared_ring_before_the_refusal,
            "a refusal borrows and allocates nothing"
        );

        check_out_leases
            .release_one_check_out_lease(&held_frames[0], consumer)
            .expect("the consumer lets one frame go");
        let after_the_release = acquire_one_native_node_frame_id(&mut native_node)
            .expect("a frame let go of serves the node's next");
        assert!(same_pool_slot(&after_the_release, &held_frames[0]));
    }

    /// The device memory this context's allocator holds across every heap,
    /// read from the driver now.
    #[cfg(target_os = "linux")]
    fn device_memory_bytes_the_allocator_holds_on_every_heap(gpu: &GpuContext) -> u64 {
        gpu.device()
            .inner
            .read_every_heaps_device_memory_budget_fetched_from_the_driver_now()
            .expect("every heap's budget reads")
            .iter()
            .map(|heap| heap.bytes_this_runtimes_allocator_pools_hold_on_the_heap)
            .sum()
    }

    /// Whether `published_frame_id` is refused as a recycled frame, in this
    /// process and by the lease table alike.
    #[cfg(target_os = "linux")]
    fn is_refused_as_recycled_everywhere(
        gpu: &GpuContext,
        check_out_leases: &crate::core::context::SurfaceCheckOutLeaseRegistry,
        published_frame_id: &str,
    ) -> bool {
        let refused_in_this_process = matches!(
            gpu.refuse_a_retired_frame_id(published_frame_id),
            Err(Error::SurfaceFrameRecycled { .. })
        );
        let refused_by_the_lease_table = matches!(
            check_out_leases
                .record_check_out_lease(published_frame_id, check_out_leases.mint_holder_id()),
            Err(Error::SurfaceFrameRecycled { .. })
        );
        refused_in_this_process
            && refused_by_the_lease_table
            && gpu
                .resolve_pixel_buffer_by_surface_id(published_frame_id)
                .is_err()
    }

    /// A torn-down node's pool frees each slot nothing holds — registration
    /// released, last frame refused as recycled — and hands each held slot
    /// back to the shared ring, generation kept, freed by the next acquire
    /// once it is let go rather than after the idle window.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_torn_down_native_nodes_unheld_slots_are_freed_and_its_held_ones_handed_back_until_let_go()
    {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            tracing::warn!("no GPU device, so the native node teardown freeing test skips");
            return;
        };
        let live_service =
            crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
        let check_out_leases = live_service.check_out_leases();
        let mut native_node =
            gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-torn-down-node");
        let mut frames_the_node_published = Vec::new();
        let mut buffer_held_in_this_process = None;
        for _ in 0..SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD {
            let (published_frame_id, buffer) = native_node
                .acquire_the_next_output_pixel_buffer(
                    LEASE_TEST_SURFACE_WIDTH,
                    LEASE_TEST_SURFACE_HEIGHT,
                    PixelFormat::Rgba32,
                )
                .expect("a node under its cap is served");
            buffer_held_in_this_process.get_or_insert(buffer);
            frames_the_node_published.push(published_frame_id.to_string());
        }
        let [held_in_this_process, leased, unheld @ ..] = frames_the_node_published.as_slice()
        else {
            panic!("the node published from one borrowed slot per frame of its rotation");
        };
        let consumer = check_out_leases.mint_holder_id();
        check_out_leases
            .record_check_out_lease(leased, consumer)
            .expect("a consumer holds one of the node's frames");
        for frame in &frames_the_node_published {
            assert!(live_service.is_registered(pool_slot_key_of_surface_id(frame)));
        }
        wait_for_every_gpu_submission_made_so_far(&gpu);

        drop(native_node);
        for frame in unheld {
            assert!(
                !live_service.is_registered(pool_slot_key_of_surface_id(frame)),
                "an unheld slot of the torn-down node kept its registration"
            );
            assert!(
                is_refused_as_recycled_everywhere(&gpu, check_out_leases, frame),
                "the last frame of a freed slot is not refused as recycled"
            );
        }
        let mut handed_back: Vec<(String, u64)> =
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        handed_back.sort_unstable();
        let mut held: Vec<(String, u64)> = [held_in_this_process, leased]
            .into_iter()
            .map(|frame| {
                (
                    pool_slot_key_of_surface_id(frame).to_string(),
                    frame_generation_of(frame),
                )
            })
            .collect();
        held.sort_unstable();
        assert_eq!(
            handed_back, held,
            "exactly the held slots came back to the shared ring, generations kept"
        );
        gpu.resolve_pixel_buffer_by_surface_id(leased)
            .expect("a leased frame of the torn-down node still resolves");

        drop(buffer_held_in_this_process);
        check_out_leases
            .release_one_check_out_lease(leased, consumer)
            .expect("the consumer lets the frame go");
        for _ in 0..held.len() {
            gpu.acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH * 2,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("an acquire of another shape");
        }
        assert!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
            "a handed-back slot let go of waited out the idle window, or its ring outlived it"
        );
        for frame in [held_in_this_process, leased] {
            assert!(!live_service.is_registered(pool_slot_key_of_surface_id(frame)));
            assert!(is_refused_as_recycled_everywhere(
                &gpu,
                check_out_leases,
                frame
            ));
        }
        live_service.stop();
    }

    /// A stopped stream's shared pool entries nothing holds are freed at
    /// once — registrations released, ids refused as recycled — while its
    /// held ones and the engine's own entries stay; a held one is freed by
    /// the next acquire once let go.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_stopped_streams_pool_entries_are_freed_and_their_ids_retired() {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            tracing::warn!("no GPU device, so the stopped stream freeing test skips");
            return;
        };
        let live_service =
            crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
        let check_out_leases = live_service.check_out_leases();
        let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
        let stopped_streams_view =
            gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
        let engines_shape = PixelBufferPoolKey {
            width: LEASE_TEST_SURFACE_WIDTH * 2,
            ..LEASE_TEST_SURFACE_SHAPE
        };

        let (held_frame, held_buffer) = stopped_streams_view
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("the stream's held frame");
        let held_frame = held_frame.to_string();
        let let_go_frame = stopped_streams_view
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("the stream's frame it let go of")
            .0
            .to_string();
        let streams_texture_shape = TexturePoolDescriptor::new(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            TextureFormat::Rgba8Unorm,
        )
        .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST);
        drop(
            stopped_streams_view
                .acquire_texture(&streams_texture_shape)
                .expect("the stream's texture"),
        );
        gpu.acquire_pixel_buffer(
            engines_shape.width,
            engines_shape.height,
            engines_shape.format,
        )
        .expect("the engine's own frame");
        let streams_slots = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        let engines_slots_before_the_stop = shared_ring_slots_and_generations(&gpu, engines_shape);
        assert_eq!(streams_slots.len(), POOL_PRE_ALLOCATE_COUNT);
        for (pool_slot_key, _) in &streams_slots {
            assert!(live_service.is_registered(pool_slot_key));
        }
        wait_for_every_gpu_submission_made_so_far(&gpu);
        let device_memory_held_before_the_stop =
            device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);

        gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
        assert!(
            device_memory_bytes_the_allocator_holds_on_every_heap(&gpu)
                < device_memory_held_before_the_stop,
            "the stopped stream's freed entries gave no device memory back"
        );
        let held_slot_key = pool_slot_key_of_surface_id(&held_frame).to_string();
        assert_eq!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE),
            vec![(held_slot_key.clone(), frame_generation_of(&held_frame))],
            "only the slot the stream still holds outlived its stop"
        );
        for (pool_slot_key, _) in &streams_slots {
            assert_eq!(
                live_service.is_registered(pool_slot_key),
                *pool_slot_key == held_slot_key,
                "slot {pool_slot_key}'s registration does not follow whether it was freed"
            );
        }
        assert!(
            is_refused_as_recycled_everywhere(&gpu, check_out_leases, &let_go_frame),
            "a freed slot's last frame is not refused as recycled"
        );
        gpu.resolve_pixel_buffer_by_surface_id(&held_frame)
            .expect("the frame the stream still holds resolves");
        assert_eq!(
            gpu.texture_pool()
                .slot_count_of_the_shape_of(&streams_texture_shape),
            0,
            "the stopped stream's texture slot outlived its stop"
        );
        assert_eq!(
            shared_ring_slots_and_generations(&gpu, engines_shape),
            engines_slots_before_the_stop,
            "the engine's own slots went with a stream's stop"
        );

        drop(held_buffer);
        let _engines_next_frame = gpu
            .acquire_pixel_buffer(
                engines_shape.width,
                engines_shape.height,
                engines_shape.format,
            )
            .expect("the engine's next frame");
        assert!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
            "the stopped stream's slot it let go of waited out the idle window"
        );
        assert!(!live_service.is_registered(&held_slot_key));
        assert!(is_refused_as_recycled_everywhere(
            &gpu,
            check_out_leases,
            &held_frame
        ));
        live_service.stop();
    }

    /// A stopped stream's entry a helper still holds checked out when the stop
    /// begins is freed by that stop once the helper's connection closes,
    /// with no acquire after it.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_stopped_streams_entry_leased_until_its_helpers_connection_closes_is_freed_by_the_stop() {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            tracing::warn!("no GPU device, so the leased stopped stream freeing test skips");
            return;
        };
        let live_service =
            crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
        let check_out_leases = live_service.check_out_leases();
        let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
        let stopped_streams_view =
            gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
        let leased_frame = stopped_streams_view
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("the stream's frame a helper leases")
            .0
            .to_string();
        let helpers_connection = check_out_leases.mint_holder_id();
        check_out_leases
            .record_check_out_lease(&leased_frame, helpers_connection)
            .expect("the helper checks the frame out");
        wait_for_every_gpu_submission_made_so_far(&gpu);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Late enough that the stop is already waiting on the lease,
                // well inside the longest it waits.
                std::thread::sleep(std::time::Duration::from_millis(100));
                check_out_leases
                    .release_every_check_out_lease_held_by(helpers_connection)
                    .expect("the helper's connection closes");
            });
            gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
        });

        assert!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
            "the stop left the slot whose lease cleared during it for a later acquire"
        );
        assert!(!live_service.is_registered(pool_slot_key_of_surface_id(&leased_frame)));
        assert!(is_refused_as_recycled_everywhere(
            &gpu,
            check_out_leases,
            &leased_frame
        ));
        live_service.stop();
    }

    /// A freed slot whose canvas texture a reader still holds keeps its
    /// memory, retired and out of every cache, until the reader lets go;
    /// the next acquire then destroys it.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_freed_pixel_buffer_slot_whose_canvas_a_reader_holds_is_destroyed_only_once_let_go() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
        let stopped_streams_view =
            gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
        let (frame, buffer) = stopped_streams_view
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("the stream's frame");
        let frame = frame.to_string();
        let canvas_reader = gpu
            .resolve_texture_registration_by_surface_id(
                &frame,
                None,
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
            )
            .expect("the frame's canvas texture");
        drop(buffer);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
        assert!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
            "the stopped stream's slots outlived its stop"
        );
        assert_eq!(
            gpu.pixel_buffer_pool_manager
                .slots_awaiting_destruction
                .count(),
            1,
            "the slot whose canvas a reader holds was destroyed under it, or another slot waits"
        );
        assert!(is_refused_as_recycled_everywhere(
            &gpu,
            &check_out_leases,
            &frame
        ));

        drop(canvas_reader);
        gpu.acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH * 2,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("an acquire of another shape");
        assert_eq!(
            gpu.pixel_buffer_pool_manager
                .slots_awaiting_destruction
                .count(),
            0,
            "the slot let go of was not destroyed by the next acquire"
        );
    }

    /// A stopped stream's freed entries — a pixel-buffer slot and a texture
    /// slot — leave their pools and retire their ids at once, but their
    /// memory stays while GPU work submitted before their release, naming
    /// them, is still pending; once it completes, the next acquire destroys
    /// one of each.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_freed_entry_is_destroyed_only_once_the_gpu_work_submitted_before_its_release_completes() {
        use crate::core::rhi::VulkanLayout;
        use crate::host_rhi::{VulkanAccess, VulkanStage};
        use crate::vulkan::rhi::HostVulkanTimelineSemaphore;

        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
        let stopped_streams_view =
            gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
        let (frame, buffer) = stopped_streams_view
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("the stream's frame");
        let frame = frame.to_string();
        let streams_texture_shape = TexturePoolDescriptor::new(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            TextureFormat::Rgba8Unorm,
        )
        .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST);
        let texture = stopped_streams_view
            .acquire_texture(&streams_texture_shape)
            .expect("the stream's texture");
        let streams_slot_count =
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).len();
        wait_for_every_gpu_submission_made_so_far(&gpu);

        let host_signalled_gate = HostVulkanTimelineSemaphore::new(gpu.device().inner.device(), 0)
            .expect("gate timeline");
        let mut recorder = gpu
            .create_command_recorder("gpu-work-naming-freed-entries-test")
            .expect("recorder");
        recorder.begin().expect("begin");
        recorder
            .record_buffer_barrier(
                &buffer,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::MEMORY_READ,
            )
            .expect("a barrier naming the pixel buffer");
        recorder
            .record_image_barrier(
                texture.texture(),
                VulkanLayout::UNDEFINED,
                VulkanLayout::GENERAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::MEMORY_READ,
            )
            .expect("a barrier naming the texture");
        recorder
            .host_inner_mut()
            .submit_waiting_and_signaling_timeline(Some((&host_signalled_gate, 1)), None)
            .expect("the gated submission");
        drop(buffer);
        drop(texture);
        let device_memory_held_before_the_stop =
            device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);

        gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
        // Read while the gate is shut, asserted once it is open: a failed
        // assertion must not leave the gated submission pending forever.
        let ring_slots_while_gated =
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        let texture_slots_while_gated = gpu
            .texture_pool()
            .slot_count_of_the_shape_of(&streams_texture_shape);
        let frame_refused_as_recycled_while_gated =
            is_refused_as_recycled_everywhere(&gpu, &check_out_leases, &frame);
        let pixel_buffer_slots_awaiting_destruction_while_gated = gpu
            .pixel_buffer_pool_manager
            .slots_awaiting_destruction
            .count();
        let texture_slots_awaiting_destruction_while_gated =
            gpu.texture_pool().slot_count_awaiting_destruction();
        let device_memory_held_while_gated =
            device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);
        host_signalled_gate.signal_host(1).expect("open the gate");
        recorder
            .wait_for_completion()
            .expect("the gated submission completes");
        // The pools read completion from the device's per-queue completion
        // timeline, not the recorder's fence, and lavapipe under load can
        // report the fence signalled while that same submission's timeline
        // value has not yet advanced.
        wait_for_every_gpu_submission_made_so_far(&gpu);

        assert!(
            ring_slots_while_gated.is_empty(),
            "the stopped stream's slots stayed in their ring"
        );
        assert_eq!(
            texture_slots_while_gated, 0,
            "the stopped stream's texture slot stayed in its bucket"
        );
        assert!(frame_refused_as_recycled_while_gated);
        assert_eq!(
            pixel_buffer_slots_awaiting_destruction_while_gated, streams_slot_count,
            "a pixel-buffer slot was destroyed under GPU work still pending"
        );
        assert_eq!(
            texture_slots_awaiting_destruction_while_gated, 1,
            "the texture slot was destroyed under GPU work still pending"
        );
        assert!(
            device_memory_held_while_gated >= device_memory_held_before_the_stop,
            "memory was given back while GPU work naming it was pending"
        );

        gpu.acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH * 2,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("an acquire of another shape");
        drop(
            gpu.texture_pool()
                .acquire(
                    &TexturePoolDescriptor::new(
                        LEASE_TEST_SURFACE_WIDTH * 2,
                        LEASE_TEST_SURFACE_HEIGHT,
                        TextureFormat::Rgba8Unorm,
                    ),
                    None,
                )
                .expect("a texture of another shape"),
        );
        assert_eq!(
            gpu.pixel_buffer_pool_manager
                .slots_awaiting_destruction
                .count(),
            streams_slot_count - 1,
            "the acquire after the GPU work completed destroyed other than one pixel-buffer slot"
        );
        assert_eq!(
            gpu.texture_pool().slot_count_awaiting_destruction(),
            0,
            "the acquire after the GPU work completed did not destroy the texture slot"
        );
    }

    /// A one-off pixel buffer flagged through its buffer, as a torn-down
    /// helper's are, is freed by the next acquire once let go rather than
    /// after the idle window.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_pixel_buffer_slot_flagged_through_its_buffer_skips_the_idle_window() {
        let Some((gpu, _check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let (frame, buffer) = gpu
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("a one-off frame");
        let flagged_slot_key = pool_slot_key_of_surface_id(&frame.to_string()).to_string();
        gpu.flag_the_pixel_buffer_pool_slot_of_the_buffer_freed_as_soon_as_unheld(&buffer);
        drop(buffer);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        gpu.acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH * 2,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("an acquire of another shape");
        let slots_left = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        assert_eq!(
            slots_left.len(),
            POOL_PRE_ALLOCATE_COUNT - 1,
            "other than exactly the flagged slot was freed"
        );
        assert!(
            !slots_left
                .iter()
                .any(|(pool_slot_key, _)| *pool_slot_key == flagged_slot_key),
            "the flagged slot waited out the idle window"
        );
    }

    /// A slot left unused is kept inside the idle window and handed out
    /// again; past it the next acquire frees it — one per acquire —
    /// releasing its registration and its cached frame and retiring its id.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_entry_unused_past_the_idle_window_is_freed_and_releases_its_registration() {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            tracing::warn!("no GPU device, so the idle window freeing test skips");
            return;
        };
        let live_service =
            crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
        let check_out_leases = live_service.check_out_leases();
        let acquire_and_hold = || {
            gpu.acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("a frame")
        };

        let unused_frame = acquire_and_hold().0.to_string();
        wait_for_every_gpu_submission_made_so_far(&gpu);
        let held_inside_the_window = acquire_and_hold();
        let slots_inside_the_window =
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        assert_eq!(
            slots_inside_the_window.len(),
            POOL_PRE_ALLOCATE_COUNT,
            "an unused slot was freed inside the idle window"
        );

        gpu.pixel_buffer_pool_manager
            .idle_window
            .set_for_a_test(std::time::Duration::ZERO);
        let device_memory_held_inside_the_window =
            device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);
        let held_past_the_window = acquire_and_hold();
        assert!(
            device_memory_bytes_the_allocator_holds_on_every_heap(&gpu)
                < device_memory_held_inside_the_window,
            "the slot freed past the idle window gave no device memory back"
        );
        let slots_past_the_window =
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        assert_eq!(
            slots_past_the_window.len(),
            POOL_PRE_ALLOCATE_COUNT - 1,
            "an acquire freed other than exactly one of the two unused slots"
        );
        let freed_slot_key = pool_slot_key_of_surface_id(&unused_frame);
        assert!(
            !slots_past_the_window
                .iter()
                .any(|(pool_slot_key, _)| pool_slot_key == freed_slot_key),
            "the slot unused longest was not the one freed"
        );
        assert!(
            !live_service.is_registered(freed_slot_key),
            "a freed slot kept its surface-share registration"
        );
        for (pool_slot_key, _) in &slots_past_the_window {
            assert!(live_service.is_registered(pool_slot_key));
        }
        assert!(
            !gpu.published_pixel_buffer_frame_ids_cached_in_this_process()
                .iter()
                .any(|cached| pool_slot_key_of_surface_id(cached) == freed_slot_key),
            "a freed slot's frame stayed in the buffer cache"
        );
        assert!(is_refused_as_recycled_everywhere(
            &gpu,
            check_out_leases,
            &unused_frame
        ));
        drop((held_inside_the_window, held_past_the_window));
        live_service.stop();
    }

    /// Where the device exports no DMA-BUF, a pooled pixel buffer registers as
    /// OPAQUE_FD stating its memory type index and exporting device; where it
    /// does, it registers as DMA-BUF stating neither.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_pixel_buffer_registers_as_opaque_fd_with_its_memory_type_index_where_dma_buf_is_absent() {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            tracing::warn!("no GPU device, so the pixel-buffer registration flavour test skips");
            return;
        };
        let live_service =
            crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
        let (published_frame_id, pixel_buffer) = gpu
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("a pooled pixel buffer");
        let published_frame_id = published_frame_id.to_string();
        let registration = live_service
            .state
            .get_surface_planes(pool_slot_key_of_surface_id(&published_frame_id))
            .expect("the pool registered its slot");

        let host_device = &gpu.device().inner;
        let device_name = host_device.name();
        if host_device.supports_dma_buf_export() {
            assert_eq!(registration.handle_type, "dma_buf", "{device_name}");
            assert_eq!(registration.vk_memory_type_index, None, "{device_name}");
            assert_eq!(registration.exporting_device_uuid, None, "{device_name}");
        } else {
            let exporting_device_uuid_in_lowercase_hex =
                streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(
                    host_device.physical_device_uuid(),
                );
            assert_eq!(registration.handle_type, "opaque_fd", "{device_name}");
            assert_eq!(
                registration.vk_memory_type_index,
                pixel_buffer
                    .buffer_ref()
                    .inner
                    .vma_allocation_memory_type_index(),
                "the registration states the slot's own memory type on {device_name}"
            );
            assert_eq!(
                registration.exporting_device_uuid.as_deref(),
                Some(exporting_device_uuid_in_lowercase_hex.as_str()),
                "{device_name}"
            );
        }
        live_service.stop();
    }

    /// A pooled pixel buffer the service registered resolves through the
    /// store's `lookup_buffer` into an import of the same memory, holding the
    /// bytes the producer wrote — over DMA-BUF where the device exports it,
    /// over OPAQUE_FD at the stated memory type index where it does not.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_pooled_pixel_buffer_reads_back_through_lookup_buffer_with_its_bytes_intact() {
        let Ok(gpu) = GpuContext::init_for_platform() else {
            tracing::warn!("no GPU device, so the lookup_buffer read-back test skips");
            return;
        };
        let live_service =
            crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
        let slot_bytes = (LEASE_TEST_SURFACE_WIDTH * LEASE_TEST_SURFACE_HEIGHT * 4) as usize;
        let producer_pattern_byte = |index: usize| (index.wrapping_mul(31).wrapping_add(7)) as u8;

        let (published_frame_id, pixel_buffer) = gpu
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("a pooled pixel buffer");
        let producer_bytes = unsafe {
            std::slice::from_raw_parts_mut(pixel_buffer.buffer_ref().inner.mapped_ptr(), slot_bytes)
        };
        for (index, byte) in producer_bytes.iter_mut().enumerate() {
            *byte = producer_pattern_byte(index);
        }

        let published_frame_id = published_frame_id.to_string();
        let looked_up = gpu
            .surface_store()
            .expect("the engine's store")
            .lookup_buffer(pool_slot_key_of_surface_id(&published_frame_id))
            .unwrap_or_else(|refusal| {
                panic!(
                    "lookup_buffer resolves the slot on {}: {refusal}",
                    gpu.device().inner.name()
                )
            });
        assert_ne!(
            looked_up.buffer_ref().inner.mapped_ptr(),
            pixel_buffer.buffer_ref().inner.mapped_ptr(),
            "the lookup imports the memory rather than handing back the producer's own mapping"
        );
        let imported_bytes = unsafe {
            std::slice::from_raw_parts(looked_up.buffer_ref().inner.mapped_ptr(), slot_bytes)
        };
        assert!(
            imported_bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == producer_pattern_byte(index)),
            "the imported buffer holds the producer's bytes"
        );
        drop(looked_up);
        live_service.stop();
    }

    /// With a swapchain live and presenting, every exportable flavour the
    /// device offers allocates again after a stopped stream's entries of it
    /// were all freed: the pixel buffer in whichever flavour the device gives
    /// it, the sampled image, the OPAQUE_FD image, and — only on a device that
    /// exports DMA-BUF — the tiled DMA-BUF image. NVIDIA refuses the first
    /// export allocation of a flavour with none live after a swapchain; the
    /// device's sentinels keep each above zero.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn every_exportable_flavour_reacquires_after_its_pool_is_freed_with_a_swapchain_live() {
        const FRAMES_PRESENTED_SO_THE_COMPOSITOR_IMPORTS_THE_SWAPCHAIN: usize = 30;
        const FLAVOUR_TEST_EXTENT: u32 = 64;

        use crate::core::test_support::rig_only_capability_skip::skip_unless_this_process_can_reach_a_display_server;
        if skip_unless_this_process_can_reach_a_display_server() {
            return;
        }
        let Ok(gpu) = GpuContext::init_for_platform() else {
            tracing::warn!("no GPU device, so the swapchain re-acquire test skips");
            return;
        };
        let window = match crate::core::window_event_pump::process_wide_window_event_pump()
            .and_then(|pump| {
                pump.request_window_for_owning_processor(
                    crate::core::window_event_pump::WindowRegistrationRequestFromOwningProcessor {
                        window_title: "streamlib-swapchain-reacquire-test".to_string(),
                        initial_width_in_logical_pixels: 640,
                        initial_height_in_logical_pixels: 360,
                    },
                )
            }) {
            Ok(window) => window,
            Err(no_window) => panic!(
                "a display server is named, so the swapchain's window must mint: {no_window}"
            ),
        };
        let (window_width, window_height) = window.physical_size_when_minted();
        let mut present_target = gpu
            .create_vulkan_present_target(
                window.present_surface_source(),
                window_width,
                window_height,
                true,
                None,
            )
            .expect("a swapchain on the window");
        for _ in 0..FRAMES_PRESENTED_SO_THE_COMPOSITOR_IMPORTS_THE_SWAPCHAIN {
            present_target
                .render_frame(|frame| {
                    frame.begin_rendering(Some([0.0, 0.0, 0.0, 1.0]))?;
                    frame.end_rendering()
                })
                .expect("a frame presents on the swapchain");
        }

        let host_device = &gpu.device().inner;
        let device_name = host_device.name();
        let device_exports_dma_buf = host_device.supports_dma_buf_export();
        if device_exports_dma_buf {
            assert!(host_device.dma_buf_buffer_pool().is_some());
            assert!(host_device.dma_buf_image_pool().is_some());
            assert!(host_device.dma_buf_image_pool_tiled().is_some());
            assert!(
                host_device
                    .has_render_target_modifier_for_texture_format(TextureFormat::Rgba8Unorm),
                "the device advertises no render-target modifier, so the tiled flavour is untested"
            );
        } else {
            assert!(host_device.dma_buf_buffer_pool().is_none(), "{device_name}");
            assert!(host_device.dma_buf_image_pool().is_none(), "{device_name}");
            assert!(
                host_device.dma_buf_image_pool_tiled().is_none(),
                "{device_name}"
            );
        }
        assert!(host_device.opaque_fd_image_pool().is_some());
        let mut texture_flavours = vec![
            (
                "sampled image",
                TexturePoolDescriptor::new(
                    FLAVOUR_TEST_EXTENT,
                    FLAVOUR_TEST_EXTENT,
                    TextureFormat::Rgba8Unorm,
                )
                .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
            ),
            (
                "OPAQUE_FD image",
                TexturePoolDescriptor::new(
                    FLAVOUR_TEST_EXTENT,
                    FLAVOUR_TEST_EXTENT,
                    TextureFormat::Rgba8Unorm,
                )
                .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST)
                .with_cross_process_importability(
                    super::super::texture_pool::TextureCrossProcessImportability::OpaqueFd,
                ),
            ),
        ];
        if device_exports_dma_buf {
            texture_flavours.push((
                "tiled DMA-BUF image",
                TexturePoolDescriptor::new(
                    FLAVOUR_TEST_EXTENT,
                    FLAVOUR_TEST_EXTENT,
                    TextureFormat::Rgba8Unorm,
                )
                .with_usage(TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING)
                .with_cross_process_importability(
                    super::super::texture_pool::TextureCrossProcessImportability::RenderTargetDmaBuf,
                ),
            ));
        }

        let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
        let stopped_streams_view =
            gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
        drop(
            stopped_streams_view
                .acquire_pixel_buffer(
                    LEASE_TEST_SURFACE_WIDTH,
                    LEASE_TEST_SURFACE_HEIGHT,
                    PixelFormat::Rgba32,
                )
                .expect("a pixel buffer with the swapchain live"),
        );
        for (flavour, descriptor) in &texture_flavours {
            drop(
                stopped_streams_view
                    .acquire_texture(descriptor)
                    .unwrap_or_else(|refusal| {
                        panic!("a {flavour} with the swapchain live was refused: {refusal}")
                    }),
            );
        }
        wait_for_every_gpu_submission_made_so_far(&gpu);

        gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
        assert!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
            "the stopped stream's pixel buffers outlived its stop"
        );
        for (flavour, descriptor) in &texture_flavours {
            assert_eq!(
                gpu.texture_pool().slot_count_of_the_shape_of(descriptor),
                0,
                "the stopped stream's {flavour} outlived its stop"
            );
        }

        let (_, reacquired_pixel_buffer) = gpu
            .acquire_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("a pixel buffer allocates again after its pool was freed");
        match (
            reacquired_pixel_buffer
                .buffer_ref()
                .inner
                .export_external_handle(),
            device_exports_dma_buf,
        ) {
            (Ok(crate::core::rhi::RhiExternalHandle::DmaBuf { fd, .. }), true)
            | (Ok(crate::core::rhi::RhiExternalHandle::OpaqueFd { fd, .. }), false) => unsafe {
                libc::close(fd);
            },
            (other, _) => panic!(
                "the re-acquired pixel buffer on {device_name} exported the wrong flavour: \
                 {other:?}"
            ),
        }
        drop(reacquired_pixel_buffer);
        for (flavour, descriptor) in &texture_flavours {
            gpu.acquire_texture(descriptor).unwrap_or_else(|refusal| {
                panic!("a {flavour} did not allocate again after its pool was freed: {refusal}")
            });
        }
        present_target
            .render_frame(|frame| {
                frame.begin_rendering(Some([0.0, 0.0, 0.0, 1.0]))?;
                frame.end_rendering()
            })
            .expect("the swapchain still presents");
        drop(present_target);
        drop(window);
    }

    /// A slot handed back while a consumer still leases the node's last frame
    /// from it stays off the producer's ring until the consumer lets go, and
    /// is then freed rather than handed out again.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_returned_slot_is_never_rehanded_while_its_last_frame_is_leased_and_is_freed_once_let_go() {
        let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let mut native_node =
            gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-leased-node");
        let leased_frame =
            acquire_one_native_node_frame_id(&mut native_node).expect("the node's first frame");
        let consumer = check_out_leases.mint_holder_id();
        check_out_leases
            .record_check_out_lease(&leased_frame, consumer)
            .expect("a consumer holds the node's frame");
        drop(native_node);

        for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 2) {
            let handed = acquire_one_pool_slot_id(&gpu).expect("free slots remain");
            assert!(
                !same_pool_slot(&handed, &leased_frame),
                "the shared ring rehanded a returned slot whose last frame a consumer holds"
            );
        }
        gpu.resolve_pixel_buffer_by_surface_id(&leased_frame)
            .expect("the leased frame still resolves to its own pixels");

        check_out_leases
            .release_one_check_out_lease(&leased_frame, consumer)
            .expect("the consumer lets the frame go");
        wait_for_every_gpu_submission_made_so_far(&gpu);
        for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 4) {
            let handed = acquire_one_pool_slot_id(&gpu).expect("free slots remain");
            assert!(
                !same_pool_slot(&handed, &leased_frame),
                "a returned slot let go of was handed out again rather than freed"
            );
        }
        assert!(
            !shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE)
                .iter()
                .any(|(pool_slot_key, _)| pool_slot_key
                    == pool_slot_key_of_surface_id(&leased_frame)),
            "a returned slot let go of stayed in the shared ring"
        );
        assert!(is_refused_as_recycled_everywhere(
            &gpu,
            &check_out_leases,
            &leased_frame
        ));
    }

    /// Native nodes may borrow every slot a shape's shared ring holds; the
    /// ring then grows rather than refusing its own producers.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_shared_pixel_buffer_pool_grows_when_native_nodes_have_borrowed_every_slot_of_a_shape() {
        let Some((gpu, _check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let native_nodes_that_borrow_every_pre_allocated_slot =
            POOL_PRE_ALLOCATE_COUNT.div_ceil(SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD);
        let mut native_nodes: Vec<_> = (0..native_nodes_that_borrow_every_pre_allocated_slot)
            .map(|native_node_index| {
                gpu.create_processor_output_pixel_buffer_pool_for_a_native_node(&format!(
                    "a-borrowing-node-{native_node_index}"
                ))
            })
            .collect();
        let borrowed_frames: Vec<String> = native_nodes
            .iter_mut()
            .flat_map(|native_node| {
                (0..SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD)
                    .map(|_| {
                        acquire_one_native_node_frame_id(native_node).expect("the node's frame")
                    })
                    .collect::<Vec<String>>()
            })
            .collect();
        assert!(
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
            "the nodes borrowed every slot the shared ring pre-allocated"
        );

        let shared_frame = acquire_one_pool_slot_id(&gpu)
            .expect("the shared ring grows when every slot of the shape is borrowed");
        assert!(
            !borrowed_frames
                .iter()
                .any(|borrowed| same_pool_slot(borrowed, &shared_frame)),
            "the shared ring handed out a slot a node has borrowed"
        );
        assert!(
            !shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
            "the shared ring grew"
        );
    }

    /// A node whose frames change shape hands every slot of the old shape
    /// back to that shape's shared ring and borrows the new shape's.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_native_nodes_shape_change_returns_the_old_shapes_slots() {
        let Some((gpu, _check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
            return;
        };
        let mut native_node =
            gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-resized-node");
        let old_shapes_frames: Vec<String> = (0..SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD)
            .map(|_| acquire_one_native_node_frame_id(&mut native_node).expect("the node's frame"))
            .collect();
        let slots_the_shared_ring_kept =
            shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        assert_eq!(
            slots_the_shared_ring_kept.len(),
            PRE_ALLOCATED_SLOTS_THE_SHARED_RING_KEEPS_PAST_ONE_NATIVE_NODES_ROTATION
        );

        let (new_shapes_frame, new_shapes_buffer) = native_node
            .acquire_the_next_output_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH * 2,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("the node's first frame of its new shape");
        assert_eq!(
            (new_shapes_buffer.width, new_shapes_buffer.height),
            (LEASE_TEST_SURFACE_WIDTH * 2, LEASE_TEST_SURFACE_HEIGHT)
        );
        assert!(
            !old_shapes_frames
                .iter()
                .any(|old| same_pool_slot(old, &new_shapes_frame.to_string())),
            "a frame of the new shape is published from a slot of the new shape"
        );
        assert_eq!(native_node.slot_count(), 1);

        let shared_ring_slots = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
        let mut shared_ring_slot_keys: Vec<&str> = shared_ring_slots
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        let mut kept_and_old_shapes_slot_keys: Vec<&str> = slots_the_shared_ring_kept
            .iter()
            .map(|(key, _)| key.as_str())
            .chain(
                old_shapes_frames
                    .iter()
                    .map(|frame| pool_slot_key_of_surface_id(frame)),
            )
            .collect();
        shared_ring_slot_keys.sort_unstable();
        kept_and_old_shapes_slot_keys.sort_unstable();
        assert_eq!(
            shared_ring_slot_keys, kept_and_old_shapes_slot_keys,
            "every slot of the old shape is back in its shared ring"
        );
    }

    /// The upload's terminal barrier — and the layout it publishes to the
    /// registration — follow the destination's create-time usage.
    /// `SHADER_READ_ONLY_OPTIMAL` requires `SAMPLED` or `INPUT_ATTACHMENT`
    /// (VUID-VkImageMemoryBarrier2-oldLayout-01211), so a storage-only or
    /// colour-attachment-only destination ends in `GENERAL` instead.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn an_upload_publishes_the_terminal_layout_its_destinations_usage_allows() {
        const UPLOAD_EXTENT: u32 = 64;

        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };

        for (destination_shape, usage, expected_terminal_layout) in [
            (
                "storage-only — an escalate ray-tracing output being seeded",
                TextureUsages::STORAGE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
                VulkanLayout::GENERAL,
            ),
            (
                "colour-attachment-only — a scissored-draw target being seeded",
                TextureUsages::RENDER_ATTACHMENT
                    | TextureUsages::COPY_DST
                    | TextureUsages::COPY_SRC,
                VulkanLayout::GENERAL,
            ),
            (
                "sampled-capable — every ring slot and cached upload texture",
                TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            ),
        ] {
            let desc =
                TextureDescriptor::new(UPLOAD_EXTENT, UPLOAD_EXTENT, TextureFormat::Rgba8Unorm)
                    .with_usage(usage);
            let texture = gpu
                .device()
                .create_texture_local(&desc)
                .unwrap_or_else(|e| panic!("create the {destination_shape} destination: {e}"));
            let surface_id = uuid::Uuid::new_v4().to_string();
            gpu.register_texture_with_layout(&surface_id, texture.clone(), VulkanLayout::UNDEFINED);

            let (_pool_id, pixel_buffer) = gpu
                .acquire_pixel_buffer(UPLOAD_EXTENT, UPLOAD_EXTENT, PixelFormat::Rgba32)
                .unwrap_or_else(|e| panic!("acquire the upload source: {e}"));
            gpu.copy_pixel_buffer_to_texture(
                &pixel_buffer,
                &texture,
                &surface_id,
                UPLOAD_EXTENT,
                UPLOAD_EXTENT,
            )
            .unwrap_or_else(|e| panic!("upload into the {destination_shape} destination: {e}"));

            let registration = gpu
                .resolve_texture_registration_by_surface_id(
                    &surface_id,
                    None,
                    UPLOAD_EXTENT,
                    UPLOAD_EXTENT,
                )
                .expect("the seeded destination resolves its registration");
            assert_eq!(
                registration.current_layout(),
                expected_terminal_layout,
                "a {destination_shape} destination must be published in the terminal layout its \
                 usage can legally hold"
            );
        }
    }

    /// Seeding a destination whose usage cannot hold
    /// `SHADER_READ_ONLY_OPTIMAL` must raise no validation finding at the
    /// upload, and none at a consumer's barrier out of the layout that
    /// upload published. The `GENERAL` transition below is that consumer —
    /// the barrier an escalate ray-tracing dispatch takes to bind its
    /// output as a storage descriptor.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn seeding_a_non_sampled_destination_and_consuming_it_raises_no_validation_finding() {
        use crate::host_rhi::{VulkanAccess, VulkanStage};

        const UPLOAD_EXTENT: u32 = 64;

        let gpu = match GpuContext::init_for_platform() {
            Ok(g) => g,
            Err(_) => {
                println!("Skipping - no GPU device available");
                return;
            }
        };
        let counts_before = gpu.device().inner.validation_layer_message_counts();
        let Some(counts_before) = counts_before else {
            println!(
                "Skipping — no validation messenger installed. Re-run with \
                 STREAMLIB_VULKAN_VALIDATION=1 and VK_LAYER_KHRONOS_validation present."
            );
            return;
        };

        let desc = TextureDescriptor::new(UPLOAD_EXTENT, UPLOAD_EXTENT, TextureFormat::Rgba8Unorm)
            .with_usage(
                TextureUsages::STORAGE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
            );
        let texture = gpu
            .device()
            .create_texture_local(&desc)
            .expect("create the storage-only destination");
        let surface_id = uuid::Uuid::new_v4().to_string();
        gpu.register_texture_with_layout(&surface_id, texture.clone(), VulkanLayout::UNDEFINED);

        let (_pool_id, pixel_buffer) = gpu
            .acquire_pixel_buffer(UPLOAD_EXTENT, UPLOAD_EXTENT, PixelFormat::Rgba32)
            .expect("acquire the upload source");
        gpu.copy_pixel_buffer_to_texture(
            &pixel_buffer,
            &texture,
            &surface_id,
            UPLOAD_EXTENT,
            UPLOAD_EXTENT,
        )
        .expect("seed the storage-only destination");

        let registration = gpu
            .resolve_texture_registration_by_surface_id(
                &surface_id,
                None,
                UPLOAD_EXTENT,
                UPLOAD_EXTENT,
            )
            .expect("the seeded destination resolves its registration");
        let published_layout = registration.current_layout();

        let mut recorder = gpu
            .create_command_recorder("seeded_storage_output_into_general")
            .expect("a recorder for the consuming barrier");
        recorder.begin().expect("begin the consuming barrier");
        recorder
            .record_image_barrier(
                &texture,
                published_layout,
                VulkanLayout::GENERAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
            )
            .expect("barrier the seeded output to its storage-descriptor layout");
        recorder
            .submit_and_wait()
            .expect("submit the consuming barrier");

        let counts_after = gpu
            .device()
            .inner
            .validation_layer_message_counts()
            .expect("the messenger stays installed for the whole test");
        assert_eq!(
            counts_after.error_count, counts_before.error_count,
            "seeding a storage-only destination and barriering out of the layout that seed \
             published must raise no validation error"
        );
    }
}
