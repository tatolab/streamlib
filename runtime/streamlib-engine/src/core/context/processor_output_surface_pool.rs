// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A processor's output slots under one pool key — the lease-aware ring every
//! processor output ring asks for its next slot.
//!
//! The producer never waits on a consumer: a held slot is skipped, the pool
//! grows to its cap, and at the cap the acquire refuses by name so the
//! producer drops its own frame. Reuse and growth are separate calls so the
//! caller can allocate outside whatever lock it holds the pool under.

use super::SurfaceCheckOutLeaseRegistry;
use super::lease_aware_pool_slot_ring::{
    LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot, LeaseAwarePoolSlotResource,
    LeaseAwarePoolSlotRing,
};
use crate::core::{Error, Result};

/// The most slots one processor output pool grows to while consumers hold its
/// frames.
pub(crate) const PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY: usize = 16;

/// What a processor output pool answers when asked for its next frame
/// without allocating.
pub(crate) enum ProcessorOutputSurfacePoolHandOff<'pool, Resource> {
    /// An unheld slot, republished under a freshly minted generation.
    ReusedSlot(&'pool LeaseAwarePoolSlot<Resource>),
    /// Every slot is held or the pool is short of its rotation depth: the
    /// caller allocates one and hands it to
    /// [`ProcessorOutputSurfacePool::hand_off_a_fresh_slot`].
    NeedsAFreshSlot,
}

/// A fresh slot the pool refused at its capacity, handed back to the caller
/// that allocated it.
pub(crate) struct ProcessorOutputSurfacePoolRefusedFreshSlot<Resource> {
    pub(crate) refusal: Error,
    pub(crate) pool_slot_key: String,
    pub(crate) resource: Resource,
}

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

    /// Hand out the slot this frame publishes into when the pool needs no new
    /// one: an unheld slot republished under a freshly minted
    /// `<slot>#<generation>`.
    ///
    /// The pool first grows to `rotation_depth` slots, so an unheld frame
    /// stays resolvable for that many publishes behind the newest. Past that it
    /// reuses the next slot nobody holds, asks for a fresh slot when every slot
    /// is held, and refuses at [`PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY`].
    pub(crate) fn hand_off_a_reusable_frame(
        &mut self,
        rotation_depth: usize,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> Result<ProcessorOutputSurfacePoolHandOff<'_, Resource>> {
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
            return Ok(ProcessorOutputSurfacePoolHandOff::ReusedSlot(
                self.ring.slot(slot_index),
            ));
        }
        if slot_count >= PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY {
            return Err(self.every_slot_in_use_refusal());
        }
        Ok(ProcessorOutputSurfacePoolHandOff::NeedsAFreshSlot)
    }

    /// Add a slot allocated after [`Self::hand_off_a_reusable_frame`] asked
    /// for one, and hand it out under its first generation; at
    /// [`PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY`] the slot is handed back.
    pub(crate) fn hand_off_a_fresh_slot(
        &mut self,
        pool_slot_key: String,
        resource: Resource,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> std::result::Result<
        &LeaseAwarePoolSlot<Resource>,
        ProcessorOutputSurfacePoolRefusedFreshSlot<Resource>,
    > {
        if self.ring.slot_count() >= PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY {
            return Err(ProcessorOutputSurfacePoolRefusedFreshSlot {
                refusal: self.every_slot_in_use_refusal(),
                pool_slot_key,
                resource,
            });
        }
        let slot_index = self.ring.push_fresh_slot(pool_slot_key, resource);
        self.ring
            .hand_off_fresh_slot(slot_index, check_out_leases, minted_frame_generations);
        Ok(self.ring.slot(slot_index))
    }

    fn every_slot_in_use_refusal(&self) -> Error {
        tracing::warn!(
            "processor output pool '{}': all {} slots are held by consumers; the producer \
             drops this frame",
            self.pool_key,
            self.ring.slot_count()
        );
        Error::EverySlotInTheProcessorOutputPoolIsInUse {
            pool_key: self.pool_key.clone(),
            pool_capacity: PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY,
        }
    }

    /// How many slots the pool holds.
    #[cfg(test)]
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
