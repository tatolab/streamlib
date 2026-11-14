// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::pixel_buffer_pool_slot::{
    PixelBufferPoolKey, PixelBufferPoolSlot, published_pixel_buffer_frame_id,
};
use crate::core::Result;
use crate::core::context::gpu_pool_claim_table::{
    GpuPoolClaimTablesOfOnePool, gpu_pool_claim_table_row_for_a_shared_slot,
};
use crate::core::context::lease_aware_pool_slot_ring::{
    ANY_ONE_UNHELD_SLOT_IS_REUSABLE, LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot,
    LeaseAwarePoolSlotRing, PoolSlotReuse,
};
use crate::core::context::shared_pool_entry_freeing::{
    LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK,
    SharedPoolEntriesAwaitingDestruction, SharedPoolEntryFreeingScan, SharedPoolEntryIdleWindow,
    SharedPoolEntryObservationCursor, SharedPoolEntryTakenOutOfItsPool,
};
use crate::core::context::surface_store::SurfaceStore;
use crate::core::rhi::{
    GpuDevice, PixelBuffer, PixelBufferPoolSlotId, PixelFormat, PublishedPixelBufferFrameId,
    RhiPixelBufferPool, Texture, pool_slot_key_of_surface_id,
};
use crate::core::runtime::LoadedStreamTag;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Number of buffers to pre-allocate per pool.
pub(super) const POOL_PRE_ALLOCATE_COUNT: usize = 4;

/// How many buffers a shape's shared ring adds when every buffer it holds is held.
pub(super) const PIXEL_BUFFERS_A_SHARED_RING_ADDS_WHEN_EVERY_ONE_IS_HELD: usize = 4;

/// The most surface-store lookups the buffer cache keeps before it evicts them.
pub(super) const MAX_BUFFER_CACHE_SIZE: usize = 512;

/// Ring pool of pixel buffers for a given (width, height, format).
///
/// Buffers are pre-allocated at pool creation. `acquire()` cycles through
/// buffers, skipping any currently in use, and grows when every one is held;
/// no count caps it. A slot is freed once unheld and idle, or once the stream
/// that last acquired it stops. A slot may be lent to a native node's output
/// pool, which publishes from it under its own count; at that pool's teardown
/// an unheld slot is freed and a held one comes back, generation kept.
pub(super) struct PixelBufferRingPool {
    /// The allocator the ring's buffers came from, holding its own share of
    /// each until the slot is freed.
    pool: RhiPixelBufferPool,
    pub(super) ring: LeaseAwarePoolSlotRing<PixelBufferPoolSlot>,
    /// The claim tables the ring's slots lease their rows from.
    claim_tables: GpuPoolClaimTablesOfOnePool,
    /// Slots out on loan to native nodes' output pools; the ring is dropped
    /// only once it is empty and none is out.
    slots_lent_to_native_node_output_pools: usize,
}

/// The pixel buffers this process resolves by surface id with no surface-share
/// round trip: each pool slot's current published frame, and the surface-store
/// lookups [`GpuContext::get_pixel_buffer`](super::GpuContext::get_pixel_buffer) caches under ids that name no
/// published frame.
#[derive(Default)]
pub(super) struct PixelBufferCacheBySurfaceId {
    pub(super) buffers_by_surface_id: HashMap<String, PixelBuffer>,
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
pub(in crate::core::context) struct PixelBufferPoolManager {
    pools: Mutex<HashMap<PixelBufferPoolKey, PixelBufferRingPool>>,
    /// Global cache for UUID -> PixelBuffer lookups (includes buffers from all pools).
    /// Used by consumers (e.g., display processor) to resolve UUIDs received via IPC.
    pub(super) buffer_cache: Mutex<PixelBufferCacheBySurfaceId>,
    /// The generation every lease-aware ring most recently minted per slot,
    /// shared with the [`GpuContext`](super::GpuContext) that owns this manager.
    minted_frame_generations: Arc<LeaseAwarePoolMintedFrameGenerations>,
    /// The engine's own surface store, once installed: every slot registers
    /// through it, and its lease table is where a helper's checkout shows.
    engines_own_surface_store: Arc<std::sync::OnceLock<SurfaceStore>>,
    /// The [`GpuContext`](super::GpuContext)'s textures synthesized from slots' pixel buffers,
    /// keyed by slot; a freed slot's entry goes with it.
    buffer_texture_cache: Arc<Mutex<HashMap<String, Texture>>>,
    pub(super) idle_window: SharedPoolEntryIdleWindow,
    observation_cursor: SharedPoolEntryObservationCursor,
    pub(super) slots_awaiting_destruction:
        SharedPoolEntriesAwaitingDestruction<PixelBufferPoolSlotTakenOutOfItsRing>,
    device: Arc<GpuDevice>,
}

impl PixelBufferPoolManager {
    pub(super) fn new(
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
    pub(super) fn lock_the_shared_rings(
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
    pub(super) fn acquire(
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
        // the surface-id lifetime contract — and on macOS also
        // the kernel's own in-use answer for the slot's IOSurface.
        let check_out_leases = surface_store.and_then(SurfaceStore::check_out_lease_registry);
        if let Some(slot_index) = ring_pool.ring.hand_off_a_reusable_slot(
            ANY_ONE_UNHELD_SLOT_IS_REUSABLE,
            check_out_leases,
            &self.minted_frame_generations,
        ) {
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
    pub(in crate::core::context) fn lend_a_slot_to_a_native_node_output_pool(
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
        let lent_slot_index = match ring_pool.ring.hand_off_a_reusable_slot(
            ANY_ONE_UNHELD_SLOT_IS_REUSABLE,
            check_out_leases,
            &self.minted_frame_generations,
        ) {
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
    pub(in crate::core::context) fn take_back_slots_lent_to_a_native_node_output_pool(
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
    pub(in crate::core::context) fn free_or_take_back_the_slots_of_a_torn_down_native_node_output_pool(
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
            let lent_slot_is_freed = {
                let mut minted_frame_generations_held =
                    self.minted_frame_generations.held_for_a_hand_off();
                let is_unheld = lent_slot.is_unheld(&cross_process_holders);
                let freeing_record = &mut lent_slot.resource_mut().freeing_record;
                freeing_record.flag_freed_as_soon_as_unheld();
                let is_due_to_be_freed = scan.observe(freeing_record, is_unheld);
                if is_due_to_be_freed {
                    cross_process_holders.retire_every_frame_a_slot_being_freed_published(
                        &lent_slot,
                        &mut minted_frame_generations_held,
                    );
                }
                is_due_to_be_freed
            };
            if lent_slot_is_freed {
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
    pub(in crate::core::context) fn free_at_most_one_slot_due_to_be_freed(&self) {
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
    pub(in crate::core::context) fn flag_every_slot_the_stream_last_acquired(
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
    pub(in crate::core::context) fn flag_the_slot_of_the_buffer_freed_as_soon_as_unheld(
        &self,
        buffer: &PixelBuffer,
    ) {
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
    pub(in crate::core::context) fn free_every_unheld_slot_due_to_be_freed_waiting_for_its_gpu_work(
        &self,
    ) -> usize {
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
    pub(in crate::core::context) fn publish_the_handed_off_frame_in_the_buffer_cache(
        &self,
        slot: &LeaseAwarePoolSlot<PixelBufferPoolSlot>,
    ) -> (PublishedPixelBufferFrameId, PixelBuffer) {
        let handed_off_buffer = slot.resource().buffer.clone();
        self.retire_previous_frame_in_cache(slot, &handed_off_buffer);
        (published_pixel_buffer_frame_id(slot), handed_off_buffer)
    }

    /// The generation every lease-aware ring most recently minted per slot.
    pub(in crate::core::context) fn minted_frame_generations(
        &self,
    ) -> &LeaseAwarePoolMintedFrameGenerations {
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
        let mut claim_tables = GpuPoolClaimTablesOfOnePool::named(format!(
            "pixel-buffers-{width}x{height}-{format:?}"
        ));
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
                    let gpu_pool_claim_table_row = gpu_pool_claim_table_row_for_a_shared_slot(
                        &mut claim_tables,
                        surface_store,
                        pool_id.as_str(),
                    );
                    ring.push_fresh_slot(
                        pool_id.as_str().to_string(),
                        PixelBufferPoolSlot::allocated_for(
                            buffer,
                            acquiring_stream,
                            gpu_pool_claim_table_row,
                        ),
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
            claim_tables,
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
        let gpu_pool_claim_table_row = gpu_pool_claim_table_row_for_a_shared_slot(
            &mut ring_pool.claim_tables,
            surface_store,
            pool_id.as_str(),
        );
        Ok(ring_pool.ring.push_fresh_slot(
            pool_id.as_str().to_string(),
            PixelBufferPoolSlot::allocated_for(buffer, acquiring_stream, gpu_pool_claim_table_row),
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
    pub(super) fn publishes_a_frame_of_the_slot(&self, pool_slot_key: &str) -> bool {
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
    pub(super) fn get_from_cache(&self, pool_id: &str) -> Option<PixelBuffer> {
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
    pub(super) fn cache_pixel_buffer_looked_up_through_the_surface_store(
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
    pub(super) fn forget_pixel_buffer_looked_up_through_the_surface_store(&self, surface_id: &str) {
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
pub(super) struct PixelBufferPoolSlotTakenOutOfItsRing {
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

pub(super) fn warn_of_no_shared_ring_to_take_back_slots_lent_to_a_native_node(
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
