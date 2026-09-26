// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A processor's output slots under one pool key — the lease-aware ring every
//! processor output ring asks for its next slot.
//!
//! The producer never waits on a consumer: a held slot is skipped, the pool
//! grows to its cap, and at the cap the acquire refuses by name so the
//! producer drops its own frame.

use super::SurfaceCheckOutLeaseRegistry;
use super::lease_aware_pool_slot_ring::{
    LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot, LeaseAwarePoolSlotResource,
    LeaseAwarePoolSlotRing,
};
use crate::core::{Error, Result};

/// The most slots one processor output pool grows to while consumers hold its
/// frames.
pub(crate) const PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY: usize = 16;

/// The slots a processor publishes its output frames from, under one pool key.
pub(crate) struct ProcessorOutputSurfacePool<Resource> {
    pool_key: String,
    ring: LeaseAwarePoolSlotRing<Resource>,
}

impl<Resource: LeaseAwarePoolSlotResource> ProcessorOutputSurfacePool<Resource> {
    /// An empty pool; its slots are allocated by the hand-offs that need them.
    pub(crate) fn new(pool_key: String) -> Self {
        Self {
            pool_key,
            ring: LeaseAwarePoolSlotRing::default(),
        }
    }

    /// Hand out the slot this frame publishes into, under a freshly minted
    /// `<slot>#<generation>`.
    ///
    /// The pool first grows to `rotation_depth` slots, so an unheld frame
    /// stays resolvable for that many publishes behind the newest. Past that it
    /// reuses the next slot nobody holds, grows when every slot is held, and
    /// refuses at [`PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY`].
    pub(crate) fn hand_off_next_frame(
        &mut self,
        rotation_depth: usize,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
        allocate_fresh_slot: impl FnOnce() -> Result<(String, Resource)>,
    ) -> Result<&LeaseAwarePoolSlot<Resource>> {
        if rotation_depth == 0 || rotation_depth > PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY {
            return Err(Error::Configuration(format!(
                "processor output pool '{}' was asked for a rotation depth of {rotation_depth}; \
                 it must be at least 1 and at most the pool's capacity of \
                 {PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY}",
                self.pool_key
            )));
        }
        let slot_count = self.ring.slot_count();
        if slot_count >= rotation_depth
            && let Some(slot_index) = self
                .ring
                .hand_off_a_reusable_slot(check_out_leases, minted_frame_generations)
        {
            return Ok(self.ring.slot(slot_index));
        }
        if slot_count >= PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY {
            tracing::warn!(
                "processor output pool '{}': all {} slots are held by consumers; the producer \
                 drops this frame",
                self.pool_key,
                slot_count
            );
            return Err(Error::EverySlotInTheProcessorOutputPoolIsInUse {
                pool_key: self.pool_key.clone(),
                pool_capacity: PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY,
            });
        }
        let (pool_slot_key, resource) = allocate_fresh_slot()?;
        let slot_index = self.ring.push_fresh_slot(pool_slot_key, resource);
        self.ring
            .hand_off_fresh_slot(slot_index, check_out_leases, minted_frame_generations);
        Ok(self.ring.slot(slot_index))
    }

    /// How many slots the pool holds.
    pub(crate) fn slot_count(&self) -> usize {
        self.ring.slot_count()
    }

    /// Every slot, for teardown.
    pub(crate) fn into_slots(self) -> impl Iterator<Item = LeaseAwarePoolSlot<Resource>> {
        self.ring.into_slots()
    }
}

#[cfg(test)]
mod tests;
