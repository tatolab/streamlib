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
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{SurfaceCheckOutLeaseHandOff, SurfaceCheckOutLeaseRegistry};
use crate::core::rhi::published_frame_id_of_pool_slot;
use crate::core::{Error, Result};

/// The `unheld_slots_a_reuse_needs` of a ring that reuses any slot nobody
/// holds.
pub(crate) const ANY_ONE_UNHELD_SLOT_IS_REUSABLE: usize = 1;

/// What a lease-aware ring needs to know about one slot's resource.
pub(crate) trait LeaseAwarePoolSlotResource {
    /// Whether a holder in this address space still has the resource.
    fn is_held_in_this_process(&self) -> bool;

    /// Whether the platform itself reports the resource in use by some process
    /// — a signal that holds a slot even where no checkout lease was taken.
    fn is_in_use_per_the_platform(&self) -> bool {
        false
    }

    /// Whether the resource is flagged to be freed once unheld, so a hand-off
    /// reuses it only when no other unheld slot is left.
    fn is_flagged_freed_as_soon_as_unheld(&self) -> bool {
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
    /// Where the most recent hand-off falls in its ring's hand-offs; 0 before
    /// the first, so a slot never published is the oldest.
    ring_hand_off_order: u64,
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

    /// The resource, for bookkeeping the ring does not read.
    pub(crate) fn resource_mut(&mut self) -> &mut Resource {
        &mut self.resource
    }

    /// The resource, for a slot being torn down.
    pub(crate) fn into_resource(self) -> Resource {
        self.resource
    }

    /// The generation the most recent hand-off published.
    pub(crate) fn published_frame_generation(&self) -> u64 {
        self.published_frame_generation
    }

    /// The surface id the most recent hand-off published.
    pub(crate) fn currently_published_frame_id(&self) -> String {
        published_frame_id_of_pool_slot(&self.pool_slot_key, self.published_frame_generation)
    }

    /// The surface id the hand-off before the most recent one published, once
    /// there has been one.
    pub(crate) fn previously_published_frame_id(&self) -> Option<String> {
        (self.published_frame_generation > 1).then(|| {
            published_frame_id_of_pool_slot(
                &self.pool_slot_key,
                self.published_frame_generation - 1,
            )
        })
    }

    fn mint_next_frame_generation(&mut self) -> u64 {
        self.published_frame_generation += 1;
        self.published_frame_generation
    }
}

impl<Resource: LeaseAwarePoolSlotResource> LeaseAwarePoolSlot<Resource> {
    /// Whether nothing holds the slot: no holder in this address space, no
    /// platform use, and no cross-process holder `cross_process_holders`
    /// cannot rule out.
    pub(crate) fn is_unheld(&self, cross_process_holders: &PoolSlotReuse<'_>) -> bool {
        cross_process_holders.permits(&self.pool_slot_key)
            && !self.resource.is_in_use_per_the_platform()
            && !self.resource.is_held_in_this_process()
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
    /// The index held across one availability test and the publish or retire
    /// it decides.
    pub(crate) fn held_for_a_hand_off(
        &self,
    ) -> LeaseAwarePoolMintedFrameGenerationsHeldForAHandOff<'_> {
        LeaseAwarePoolMintedFrameGenerationsHeldForAHandOff {
            minted_frame_generation_by_pool_slot: self
                .minted_frame_generation_by_pool_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        }
    }

    /// The generation most recently minted over `pool_slot_key`, if a
    /// lease-aware ring owns that slot.
    pub(crate) fn minted_frame_generation_of_slot(&self, pool_slot_key: &str) -> Option<u64> {
        self.minted_frame_generation_by_pool_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(pool_slot_key)
            .copied()
    }

    /// Drop the record of a slot whose resource is gone. A slot freed out of
    /// a shared pool keeps its retired record instead, so an old id reads as
    /// recycled; those records grow with the slots ever freed.
    pub(crate) fn forget_slot(&self, pool_slot_key: &str) {
        self.minted_frame_generation_by_pool_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(pool_slot_key);
    }

    /// Refuse `surface_id` — published as `published_generation` over
    /// `pool_slot` — when that generation is older than the one minted over
    /// the slot. `None` when no lease-aware ring owns the slot, so the caller
    /// can ask elsewhere.
    pub(crate) fn refusal_of_a_retired_frame_id(
        &self,
        surface_id: &str,
        pool_slot: &str,
        published_generation: u64,
    ) -> Option<Result<()>> {
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

/// The minted-generation index held from an availability test through the
/// publish or retire it decides.
///
/// A claim takes its share of a slot's backing and then checks the frame's
/// generation under this same lock, so the check lands wholly before the test
/// — which then sees the share and keeps the slot — or wholly after the
/// publish, and refuses the retired id.
pub(crate) struct LeaseAwarePoolMintedFrameGenerationsHeldForAHandOff<'index> {
    minted_frame_generation_by_pool_slot: MutexGuard<'index, HashMap<String, u64>>,
}

impl LeaseAwarePoolMintedFrameGenerationsHeldForAHandOff<'_> {
    fn record<Resource>(&mut self, slot: &LeaseAwarePoolSlot<Resource>) {
        self.minted_frame_generation_by_pool_slot
            .insert(slot.pool_slot_key.clone(), slot.published_frame_generation);
    }

    /// Retire every frame `pool_slot_key` has published by recording a
    /// generation none of them carries — for a slot leaving its ring.
    pub(crate) fn retire_every_published_frame_of_slot(
        &mut self,
        pool_slot_key: &str,
        generation_past_every_published_frame: u64,
    ) {
        self.minted_frame_generation_by_pool_slot.insert(
            pool_slot_key.to_string(),
            generation_past_every_published_frame,
        );
    }
}

#[cfg(test)]
impl LeaseAwarePoolMintedFrameGenerations {
    /// [`Self::refusal_of_a_retired_frame_id`] for an id the test has not
    /// split; `None` for an id with no generation, too.
    pub(crate) fn refusal_of_a_retired_frame_id_named(
        &self,
        surface_id: &str,
    ) -> Option<Result<()>> {
        let (pool_slot, published_generation) =
            crate::core::rhi::split_pool_slot_and_frame_generation(surface_id)?;
        self.refusal_of_a_retired_frame_id(surface_id, pool_slot, published_generation)
    }
}

/// What one hand-off — or one freeing scan — is allowed to conclude about an
/// existing slot's cross-process holders.
///
/// Held for the whole scan, so the answer a slot is tested against is still
/// the answer when that slot is handed over or freed.
pub(crate) enum PoolSlotReuse<'leases> {
    /// No surface-share service, so no cross-process consumer can exist.
    RefcountIsTheWholeAnswer,
    /// Leases are readable and pinned for the length of this decision.
    LeaseAware(SurfaceCheckOutLeaseHandOff<'leases>),
    /// The lease table could not be read, so no slot can be shown to be free
    /// and none may be reused. Growth still serves the producer.
    NothingCanBeProvenFree,
}

impl<'leases> PoolSlotReuse<'leases> {
    /// The conclusion `check_out_leases` allows, its table held when readable.
    pub(crate) fn over(check_out_leases: Option<&'leases SurfaceCheckOutLeaseRegistry>) -> Self {
        match check_out_leases {
            None => Self::RefcountIsTheWholeAnswer,
            Some(leases) => match leases.hold_for_pool_slot_hand_off() {
                Some(hand_off) => Self::LeaseAware(hand_off),
                None => Self::NothingCanBeProvenFree,
            },
        }
    }

    /// The conclusion the lease table of the engine's own surface store
    /// allows, once that store is installed.
    pub(crate) fn over_the_engines_own_surface_store(
        engines_own_surface_store: &'leases std::sync::OnceLock<super::SurfaceStore>,
    ) -> Self {
        Self::over(
            super::SurfaceStore::check_out_lease_registry_of_the_engines_own(
                engines_own_surface_store,
            ),
        )
    }

    /// Whether no cross-process holder can have `pool_slot_key` checked out.
    pub(crate) fn permits(&self, pool_slot_key: &str) -> bool {
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

    /// Retire every frame `slot` published, for a slot being freed: a
    /// generation none of them carries goes to the lease table on this guard
    /// and to `minted_frame_generations`, so an old id is refused as recycled
    /// rather than resolving to nothing.
    pub(crate) fn retire_every_frame_a_slot_being_freed_published<Resource>(
        &mut self,
        slot: &LeaseAwarePoolSlot<Resource>,
        minted_frame_generations: &mut LeaseAwarePoolMintedFrameGenerationsHeldForAHandOff<'_>,
    ) {
        let generation_past_every_published_frame = slot.published_frame_generation + 1;
        self.publish_frame_generation(&slot.pool_slot_key, generation_past_every_published_frame);
        minted_frame_generations.retire_every_published_frame_of_slot(
            &slot.pool_slot_key,
            generation_past_every_published_frame,
        );
    }
}

/// The slots one producer (or one pool key) publishes frames from, handed out
/// in ring order with held slots skipped.
pub(crate) struct LeaseAwarePoolSlotRing<Resource> {
    slots: Vec<LeaseAwarePoolSlot<Resource>>,
    hand_offs_so_far: u64,
}

impl<Resource> Default for LeaseAwarePoolSlotRing<Resource> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            hand_offs_so_far: 0,
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

    pub(crate) fn slot_mut(&mut self, slot_index: usize) -> &mut LeaseAwarePoolSlot<Resource> {
        &mut self.slots[slot_index]
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
            ring_hand_off_order: 0,
        });
        self.slots.len() - 1
    }

    /// Take one slot out of the ring, its published generation with it; the
    /// slots left keep their ring order.
    pub(crate) fn take_out_the_slot_at(
        &mut self,
        slot_index: usize,
    ) -> LeaseAwarePoolSlot<Resource> {
        self.slots.remove(slot_index)
    }

    /// Add a slot another ring published from, keeping its generation, so its
    /// next hand-off retires the last id it published there; answers its
    /// index.
    pub(crate) fn push_a_slot_keeping_its_published_frame_generation(
        &mut self,
        mut slot: LeaseAwarePoolSlot<Resource>,
    ) -> usize {
        slot.ring_hand_off_order = self.next_ring_hand_off_order();
        self.slots.push(slot);
        self.slots.len() - 1
    }

    /// Hand out the unheld slot whose frame was published longest ago — one
    /// flagged to be freed only when no other is unheld — minting and
    /// publishing its next generation; `None` when fewer than
    /// `unheld_slots_a_reuse_needs` slots are unheld.
    ///
    /// The whole scan and the publish run under one hold of the lease table
    /// and of the minted-generation index, so a checkout or a claim lands
    /// strictly before the test or strictly after the publish, never between
    /// them where it would hold a frame already promised back to the producer.
    pub(crate) fn hand_off_a_reusable_slot(
        &mut self,
        unheld_slots_a_reuse_needs: usize,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> Option<usize> {
        let mut reuse = PoolSlotReuse::over(check_out_leases);
        let mut minted_frame_generations_held = minted_frame_generations.held_for_a_hand_off();
        let mut slot_indices_oldest_hand_off_first: Vec<usize> = (0..self.slots.len()).collect();
        slot_indices_oldest_hand_off_first.sort_by_key(|&slot_index| {
            let slot = &self.slots[slot_index];
            (
                slot.resource.is_flagged_freed_as_soon_as_unheld(),
                slot.ring_hand_off_order,
            )
        });
        let mut unheld_slot_indices_oldest_first = slot_indices_oldest_hand_off_first
            .into_iter()
            .filter(|&slot_index| self.slots[slot_index].is_unheld(&reuse))
            .take(unheld_slots_a_reuse_needs.max(1));
        let oldest_unheld_slot_index = unheld_slot_indices_oldest_first.next()?;
        let unheld_slot_count = 1 + unheld_slot_indices_oldest_first.count();
        if unheld_slot_count < unheld_slots_a_reuse_needs {
            return None;
        }

        let ring_hand_off_order = self.next_ring_hand_off_order();
        let slot = &mut self.slots[oldest_unheld_slot_index];
        slot.ring_hand_off_order = ring_hand_off_order;
        let frame_generation = slot.mint_next_frame_generation();
        reuse.publish_frame_generation(&slot.pool_slot_key, frame_generation);
        minted_frame_generations_held.record(slot);
        Some(oldest_unheld_slot_index)
    }

    fn next_ring_hand_off_order(&mut self) -> u64 {
        self.hand_offs_so_far += 1;
        self.hand_offs_so_far
    }

    /// Every slot, for bookkeeping the ring does not read.
    pub(crate) fn slots_mut(&mut self) -> impl Iterator<Item = &mut LeaseAwarePoolSlot<Resource>> {
        self.slots.iter_mut()
    }

    /// Show `choose` each slot of `slot_indices_to_observe` with whether it is
    /// unheld, and take out each unheld slot it chooses, every frame it
    /// published retired on the same hold of the lease table its test ran
    /// under: a checkout lands strictly before the test (and keeps the slot)
    /// or strictly after the retire (and is refused as recycled).
    pub(crate) fn take_out_and_retire_every_unheld_slot_chosen(
        &mut self,
        slot_indices_to_observe: std::ops::Range<usize>,
        cross_process_holders: &mut PoolSlotReuse<'_>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
        mut choose: impl FnMut(&mut LeaseAwarePoolSlot<Resource>, bool) -> bool,
    ) -> Vec<LeaseAwarePoolSlot<Resource>> {
        let mut minted_frame_generations_held = minted_frame_generations.held_for_a_hand_off();
        let mut chosen_slot_indices = Vec::new();
        for slot_index in slot_indices_to_observe {
            let slot = &mut self.slots[slot_index];
            let is_unheld = slot.is_unheld(cross_process_holders);
            if choose(slot, is_unheld) && is_unheld {
                chosen_slot_indices.push(slot_index);
            }
        }
        let mut slots_taken_out: Vec<_> = chosen_slot_indices
            .into_iter()
            .rev()
            .map(|slot_index| {
                let slot = self.take_out_the_slot_at(slot_index);
                cross_process_holders.retire_every_frame_a_slot_being_freed_published(
                    &slot,
                    &mut minted_frame_generations_held,
                );
                slot
            })
            .collect();
        slots_taken_out.reverse();
        slots_taken_out
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
        let ring_hand_off_order = self.next_ring_hand_off_order();
        let slot = &mut self.slots[slot_index];
        slot.ring_hand_off_order = ring_hand_off_order;
        let frame_generation = slot.mint_next_frame_generation();
        minted_frame_generations.held_for_a_hand_off().record(slot);
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
