// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's one lease-aware slot ring: which slot a producer publishes its
//! next frame into, under the surface-id lifetime contract
//! (`docs/decisions/surface-id-lifetime-contract.md`).
//!
//! A slot is rehanded only when no holder in this address space has its
//! resource and no cross-process holder has it checked out. Each hand-off
//! mints `<slot>#<generation>` and retires the previous generation's id, so a
//! consumer that outwaits the ring resolves an error, never newer pixels.
//! What a slot holds — a pixel buffer, a texture — is the resource type's
//! business; the ring decides only reuse.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{SurfaceCheckOutLeaseHandOff, SurfaceCheckOutLeaseRegistry};
use crate::core::{Error, Result};

/// What a lease-aware ring needs to know about one slot's resource.
pub(crate) trait LeaseAwarePoolSlotResource {
    /// Whether a holder in this address space still has the resource.
    fn is_held_in_this_process(&self) -> bool;

    /// Whether the platform itself reports the resource in use by some process
    /// — a signal that holds a slot even where no checkout lease was taken.
    fn is_in_use_per_the_platform(&self) -> bool {
        false
    }
}

/// One slot of a lease-aware ring: its resource and how many frames it has
/// published.
pub(crate) struct LeaseAwarePoolSlot<Resource> {
    pool_slot_key: String,
    resource: Resource,
    /// The generation of the id the most recent hand-off published; 0 before
    /// the first.
    published_frame_generation: u64,
}

impl<Resource> LeaseAwarePoolSlot<Resource> {
    /// The key per-slot resources (surface-share registration, texture cache,
    /// export stagings) are filed under.
    pub(crate) fn pool_slot_key(&self) -> &str {
        &self.pool_slot_key
    }

    pub(crate) fn resource(&self) -> &Resource {
        &self.resource
    }

    /// The generation the most recent hand-off published.
    pub(crate) fn published_frame_generation(&self) -> u64 {
        self.published_frame_generation
    }

    /// The generation the hand-off before the most recent one published, once
    /// there has been one.
    pub(crate) fn previously_published_frame_generation(&self) -> Option<u64> {
        (self.published_frame_generation > 1).then(|| self.published_frame_generation - 1)
    }

    /// The surface id the most recent hand-off published.
    pub(crate) fn currently_published_frame_id(&self) -> String {
        format!("{}#{}", self.pool_slot_key, self.published_frame_generation)
    }

    fn mint_next_frame_generation(&mut self) -> u64 {
        self.published_frame_generation += 1;
        self.published_frame_generation
    }
}

/// Slot key → the generation its ring most recently minted, across every
/// lease-aware ring a [`super::GpuContext`] owns.
///
/// The in-process read index over the slots' own counters: a retired id is
/// refusable with no lease registry wired in. Its own short lock, so a resolve
/// never waits behind a ring that is allocating.
#[derive(Default)]
pub(crate) struct LeaseAwarePoolMintedFrameGenerations {
    minted_frame_generation_by_pool_slot: Mutex<HashMap<String, u64>>,
}

impl LeaseAwarePoolMintedFrameGenerations {
    fn record<Resource>(&self, slot: &LeaseAwarePoolSlot<Resource>) {
        self.minted_frame_generation_by_pool_slot
            .lock()
            .unwrap()
            .insert(
                slot.pool_slot_key.clone(),
                slot.published_frame_generation,
            );
    }

    /// The generation most recently minted over `pool_slot_key`, if a
    /// lease-aware ring owns that slot.
    pub(crate) fn minted_frame_generation_of_slot(&self, pool_slot_key: &str) -> Option<u64> {
        self.minted_frame_generation_by_pool_slot
            .lock()
            .unwrap()
            .get(pool_slot_key)
            .copied()
    }

    /// Drop a slot whose resource is gone, so the index stays bounded by the
    /// slots that exist.
    pub(crate) fn forget_slot(&self, pool_slot_key: &str) {
        self.minted_frame_generation_by_pool_slot
            .lock()
            .unwrap()
            .remove(pool_slot_key);
    }

    /// Refuse `surface_id` when it names a generation older than the one
    /// minted over its slot. `None` when no lease-aware ring owns the slot, so
    /// the caller can ask elsewhere.
    pub(crate) fn refusal_of_a_retired_frame_id(&self, surface_id: &str) -> Option<Result<()>> {
        let (pool_slot, published_generation) =
            crate::core::rhi::split_pool_slot_and_frame_generation(surface_id)?;
        let minted = self.minted_frame_generation_of_slot(pool_slot)?;
        Some(if minted == published_generation {
            Ok(())
        } else {
            Err(Error::SurfaceFrameRecycled {
                surface_id: surface_id.to_string(),
                published_generation,
                current_generation: minted,
            })
        })
    }
}

/// What one hand-off is allowed to conclude about reusing an existing slot.
///
/// Held for the whole scan, so the answer a slot is tested against is still
/// the answer when that slot is handed over.
enum PoolSlotReuse<'leases> {
    /// No surface-share service, so no cross-process consumer can exist.
    RefcountIsTheWholeAnswer,
    /// Leases are readable and pinned for the length of this decision.
    LeaseAware(SurfaceCheckOutLeaseHandOff<'leases>),
    /// The lease table could not be read, so no slot can be shown to be free
    /// and none may be reused. Growth still serves the producer.
    NothingCanBeProvenFree,
}

impl<'leases> PoolSlotReuse<'leases> {
    fn over(check_out_leases: Option<&'leases SurfaceCheckOutLeaseRegistry>) -> Self {
        match check_out_leases {
            None => Self::RefcountIsTheWholeAnswer,
            Some(leases) => match leases.hold_for_pool_slot_hand_off() {
                Some(hand_off) => Self::LeaseAware(hand_off),
                None => Self::NothingCanBeProvenFree,
            },
        }
    }

    fn permits(&self, pool_slot_key: &str) -> bool {
        match self {
            Self::RefcountIsTheWholeAnswer => true,
            Self::LeaseAware(hand_off) => !hand_off.is_checked_out_by_any_holder(pool_slot_key),
            Self::NothingCanBeProvenFree => false,
        }
    }

    /// The retire step of a reuse, on the guard the availability test held: a
    /// checkout of the outgoing id lands strictly before the test (leased — the
    /// slot is never rehanded) or strictly after this publish (refused as
    /// recycled). A no-op with no service, because then no cross-process
    /// consumer can exist to look the id up.
    fn publish_frame_generation(&mut self, pool_slot_key: &str, frame_generation: u64) {
        if let Self::LeaseAware(hand_off) = self {
            hand_off.publish_frame_generation(pool_slot_key, frame_generation);
        }
    }
}

/// The slots one producer (or one pool key) publishes frames from, handed out
/// in ring order with held slots skipped.
pub(crate) struct LeaseAwarePoolSlotRing<Resource> {
    slots: Vec<LeaseAwarePoolSlot<Resource>>,
    next_slot_index: usize,
}

impl<Resource> Default for LeaseAwarePoolSlotRing<Resource> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            next_slot_index: 0,
        }
    }
}

impl<Resource: LeaseAwarePoolSlotResource> LeaseAwarePoolSlotRing<Resource> {
    /// How many slots the ring holds.
    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn slot(&self, slot_index: usize) -> &LeaseAwarePoolSlot<Resource> {
        &self.slots[slot_index]
    }

    /// Every slot, for teardown.
    pub(crate) fn into_slots(self) -> impl Iterator<Item = LeaseAwarePoolSlot<Resource>> {
        self.slots.into_iter()
    }

    /// Add a freshly allocated slot no id has ever named, answering its index.
    /// It publishes nothing until [`Self::hand_off_fresh_slot`] or a reuse
    /// hands it out.
    pub(crate) fn push_fresh_slot(&mut self, pool_slot_key: String, resource: Resource) -> usize {
        self.slots.push(LeaseAwarePoolSlot {
            pool_slot_key,
            resource,
            published_frame_generation: 0,
        });
        self.slots.len() - 1
    }

    /// Hand out the next slot in ring order that nobody holds, minting and
    /// publishing its next generation; `None` when every slot is held.
    ///
    /// The whole scan and the retire step run under one hold of the lease
    /// table, so a checkout lands strictly before the test or strictly after
    /// the retire, never between them where it would lease a frame already
    /// promised back to the producer.
    pub(crate) fn hand_off_a_reusable_slot(
        &mut self,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> Option<usize> {
        let slot_count = self.slots.len();
        if slot_count == 0 {
            return None;
        }
        let mut reuse = PoolSlotReuse::over(check_out_leases);
        for _ in 0..slot_count {
            let slot_index = self.next_slot_index % slot_count;
            self.next_slot_index = (self.next_slot_index + 1) % slot_count;

            let slot = &mut self.slots[slot_index];
            if !reuse.permits(&slot.pool_slot_key)
                || slot.resource.is_in_use_per_the_platform()
                || slot.resource.is_held_in_this_process()
            {
                continue;
            }
            let frame_generation = slot.mint_next_frame_generation();
            reuse.publish_frame_generation(&slot.pool_slot_key, frame_generation);
            minted_frame_generations.record(slot);
            return Some(slot_index);
        }
        None
    }

    /// Publish the first frame of a slot [`Self::push_fresh_slot`] just added.
    ///
    /// Needs no hand-off guard — a consumer cannot race to check out an id that
    /// has never been published. The standalone publish tells the service what
    /// generation is current before any consumer can hold the id; if it fails,
    /// checkouts of this id fail closed at the service rather than succeeding
    /// silently.
    pub(crate) fn hand_off_fresh_slot(
        &mut self,
        slot_index: usize,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) {
        let slot = &mut self.slots[slot_index];
        let frame_generation = slot.mint_next_frame_generation();
        minted_frame_generations.record(slot);
        if let Some(leases) = check_out_leases
            && let Err(unpublishable) =
                leases.publish_frame_generation(&slot.pool_slot_key, frame_generation)
        {
            tracing::warn!(
                "could not publish generation {} of fresh pool slot {}: {} — cross-process \
                 checkouts of this frame will fail closed",
                frame_generation,
                slot.pool_slot_key,
                unpublishable
            );
        }
    }
}

#[cfg(test)]
mod tests;
