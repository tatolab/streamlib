// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The cross-process half of the pixel-buffer pool's taken-until-released test.
//!
//! In one address space a held surface is an `Arc` refcount the pool can see; a
//! helper child holding the same surface bumps nothing, so its checkout records
//! a lease here instead. Rationale:
//! `docs/decisions/surface-id-lifetime-contract.md`.
//!
//! What the guard below buys is that a checkout cannot interleave with the
//! pool's decision. It does not make the publish-to-claim transit safe: a
//! child that has been sent a bag but has not yet cast or resolved its
//! surface holds nothing, so the pool may rehand that slot — only pool depth
//! bounds that window. What closes it loudly rather than silently is the
//! frame-generation ledger this registry also carries: recycling retires the
//! published id, so the late checkout is refused instead of landing against
//! a frame the producer is already overwriting.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::core::rhi::{pool_slot_key_of_surface_id, split_pool_slot_and_frame_generation};
use crate::core::{Error, Result};

/// The holder a checkout lease is charged to — one surface-share connection.
///
/// Reclaim is per-connection rather than per-runtime because a pure consumer
/// never sends a `runtime_id`: it only ever checks surfaces out. The
/// connection is the only identity the service reliably has for it, and it is
/// the one the kernel closes when the child dies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceCheckOutLeaseHolderId(u64);

impl std::fmt::Display for SurfaceCheckOutLeaseHolderId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "check-out-holder-{}", self.0)
    }
}

#[derive(Default)]
struct SurfaceCheckOutLeaseTable {
    /// Pool slot key (or plain surface id) → holder → outstanding checkouts
    /// that holder has taken.
    ///
    /// Counted, never a set: a child can hold one frame twice — a typed
    /// cast's claim and its own later `resolve_surface` — and releasing
    /// either must leave the other standing. Published frame ids normalize
    /// to their slot key on the way in, because what a lease protects is the
    /// slot's memory.
    outstanding_check_outs_by_surface_id:
        HashMap<String, HashMap<SurfaceCheckOutLeaseHolderId, u32>>,

    /// Pool slot key → the frame generation its producer most recently
    /// published. A checkout naming any other generation is refused: the
    /// frame that id named no longer exists.
    current_frame_generation_by_pool_slot: HashMap<String, u64>,
}

impl SurfaceCheckOutLeaseTable {
    /// The lease key `surface_id` records under, refusing ids that name a
    /// frame the producer has already recycled — one lock, so the answer
    /// cannot go stale between the test and the record.
    fn lease_key_of_a_live_frame<'id>(&self, surface_id: &'id str) -> Result<&'id str> {
        let Some((pool_slot, published_generation)) =
            split_pool_slot_and_frame_generation(surface_id)
        else {
            return Ok(surface_id);
        };
        match self.current_frame_generation_by_pool_slot.get(pool_slot) {
            Some(&current) if current == published_generation => Ok(pool_slot),
            Some(&current) => Err(Error::SurfaceFrameRecycled {
                surface_id: surface_id.to_string(),
                published_generation,
                current_generation: current,
            }),
            // Fail closed: an id that claims a generation over a slot this
            // registry never published cannot be shown to name a live frame.
            None => Err(Error::Runtime(format!(
                "surface '{surface_id}' carries a frame generation, but no producer has \
                 published that pool slot through this service; nothing proves the frame \
                 still exists, so the checkout is refused"
            ))),
        }
    }

    /// The one place the ledger advances. Generations only move forward: a
    /// lower one would silently un-retire ids the pool already promised are
    /// dead.
    fn publish_frame_generation(&mut self, pool_slot_key: &str, frame_generation: u64) {
        let previous = self
            .current_frame_generation_by_pool_slot
            .insert(pool_slot_key.to_string(), frame_generation);
        debug_assert!(
            previous.is_none_or(|earlier| earlier < frame_generation),
            "generation {frame_generation} of pool slot {pool_slot_key} does not advance {previous:?}"
        );
    }
}

/// The set of surfaces cross-process consumers currently hold checked out.
///
/// The surface-share service mints into it at checkout and clears entries on
/// explicit release or connection drop; the pixel-buffer pool reads it to
/// decide whether a slot may be rehanded to its producer.
pub struct SurfaceCheckOutLeaseRegistry {
    table: Mutex<SurfaceCheckOutLeaseTable>,
    next_holder_id: AtomicU64,
}

impl Default for SurfaceCheckOutLeaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SurfaceCheckOutLeaseRegistry {
    /// An empty registry — no surface is checked out.
    pub fn new() -> Self {
        Self {
            table: Mutex::new(SurfaceCheckOutLeaseTable::default()),
            next_holder_id: AtomicU64::new(0),
        }
    }

    /// Mint the identity one surface-share connection charges its leases to.
    pub fn mint_holder_id(&self) -> SurfaceCheckOutLeaseHolderId {
        SurfaceCheckOutLeaseHolderId(self.next_holder_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Record that `holder` has checked `surface_id` out once more.
    ///
    /// A published frame id is validated and recorded under one lock: refused
    /// with [`Error::SurfaceFrameRecycled`] when the producer has recycled the
    /// slot since, because a lease minted against a dead id would pin the
    /// slot's *current* frame while the holder believes it pinned the old one.
    ///
    /// Errors rather than proceeding when the table cannot be read: an
    /// unrecorded lease is a surface the pool believes is free while a
    /// consumer reads it, which is the silent wrongness this whole mechanism
    /// exists to remove.
    pub fn record_check_out_lease(
        &self,
        surface_id: &str,
        holder: SurfaceCheckOutLeaseHolderId,
    ) -> Result<()> {
        let mut table = self.readable_table()?;
        let lease_key = table.lease_key_of_a_live_frame(surface_id)?.to_string();
        *table
            .outstanding_check_outs_by_surface_id
            .entry(lease_key)
            .or_default()
            .entry(holder)
            .or_insert(0) += 1;
        Ok(())
    }

    /// Refuse `surface_id` when it names a frame its producer has recycled;
    /// a lease-free read (`lookup`) runs this so a stale id fails loudly
    /// instead of resolving to somebody else's pixels.
    pub fn refuse_a_retired_frame_id(&self, surface_id: &str) -> Result<()> {
        self.readable_table()?
            .lease_key_of_a_live_frame(surface_id)
            .map(|_| ())
    }

    /// The frame generation `pool_slot_key` most recently published, if the
    /// slot has published through this registry at all.
    pub fn current_frame_generation(&self, pool_slot_key: &str) -> Result<Option<u64>> {
        Ok(self
            .readable_table()?
            .current_frame_generation_by_pool_slot
            .get(pool_slot_key)
            .copied())
    }

    /// Publish `frame_generation` as `pool_slot_key`'s current frame,
    /// retiring every earlier generation's id.
    ///
    /// For a slot being *reused* this must run inside the pool's
    /// [`Self::hold_for_pool_slot_hand_off`] guard (use
    /// [`SurfaceCheckOutLeaseHandOff::publish_frame_generation`]); this
    /// standalone form exists for freshly allocated slots, whose id no
    /// consumer can have seen yet.
    pub fn publish_frame_generation(
        &self,
        pool_slot_key: &str,
        frame_generation: u64,
    ) -> Result<()> {
        self.readable_table()?
            .publish_frame_generation(pool_slot_key, frame_generation);
        Ok(())
    }

    /// Drop one of `holder`'s leases on `surface_id`.
    ///
    /// Returns whether there was one to drop, so the wire handler can answer
    /// a release for a surface this connection never checked out honestly
    /// instead of reporting success.
    pub fn release_one_check_out_lease(
        &self,
        surface_id: &str,
        holder: SurfaceCheckOutLeaseHolderId,
    ) -> Result<bool> {
        let lease_key = pool_slot_key_of_surface_id(surface_id);
        let mut table = self.readable_table()?;
        let Some(holders) = table
            .outstanding_check_outs_by_surface_id
            .get_mut(lease_key)
        else {
            return Ok(false);
        };
        let Some(outstanding) = holders.get_mut(&holder) else {
            return Ok(false);
        };
        *outstanding = outstanding.saturating_sub(1);
        if *outstanding == 0 {
            holders.remove(&holder);
        }
        if holders.is_empty() {
            table.outstanding_check_outs_by_surface_id.remove(lease_key);
        }
        Ok(true)
    }

    /// Drop every lease `holder` holds, returning how many surfaces were
    /// freed. This is what a dropped surface-share connection runs — the
    /// backstop for a child that dies mid-frame.
    pub fn release_every_check_out_lease_held_by(
        &self,
        holder: SurfaceCheckOutLeaseHolderId,
    ) -> Result<usize> {
        let mut table = self.readable_table()?;
        let freed_before = table.outstanding_check_outs_by_surface_id.len();
        table
            .outstanding_check_outs_by_surface_id
            .retain(|_, holders| {
                holders.remove(&holder);
                !holders.is_empty()
            });
        Ok(freed_before - table.outstanding_check_outs_by_surface_id.len())
    }

    /// How many outstanding checkouts stand against `surface_id`'s slot,
    /// across every holder and every published generation.
    pub fn outstanding_check_out_count(&self, surface_id: &str) -> Result<u32> {
        let table = self.readable_table()?;
        Ok(table
            .outstanding_check_outs_by_surface_id
            .get(pool_slot_key_of_surface_id(surface_id))
            .map(|holders| holders.values().sum())
            .unwrap_or(0))
    }

    /// Take the table for the length of one pool availability test and the
    /// slot hand-off that follows it, so a checkout — which takes the same
    /// lock — lands strictly before or strictly after, never between them
    /// where it would lease a slot already promised to a producer.
    ///
    /// `None` when the table cannot be read: a panic left the lock poisoned,
    /// and the pool must then skip reuse rather than guess.
    pub fn hold_for_pool_slot_hand_off(&self) -> Option<SurfaceCheckOutLeaseHandOff<'_>> {
        self.table
            .lock()
            .ok()
            .map(|table| SurfaceCheckOutLeaseHandOff { table })
    }

    fn readable_table(&self) -> Result<MutexGuard<'_, SurfaceCheckOutLeaseTable>> {
        self.table.lock().map_err(|_| {
            Error::Runtime(
                "the surface checkout lease table is poisoned; a thread panicked holding it, so \
                 no lease it records can be trusted"
                    .into(),
            )
        })
    }
}

/// The lease table, held across the pool's availability test and its slot
/// hand-off. See the module doc for why the two must not be separable.
pub struct SurfaceCheckOutLeaseHandOff<'registry> {
    table: MutexGuard<'registry, SurfaceCheckOutLeaseTable>,
}

impl SurfaceCheckOutLeaseHandOff<'_> {
    /// Whether any cross-process holder has this surface's slot checked out.
    pub fn is_checked_out_by_any_holder(&self, surface_id: &str) -> bool {
        self.table
            .outstanding_check_outs_by_surface_id
            .contains_key(pool_slot_key_of_surface_id(surface_id))
    }

    /// Publish `frame_generation` as `pool_slot_key`'s current frame while
    /// the pool still holds the hand-off.
    ///
    /// This is the retire step: run under the same guard as the availability
    /// test, a checkout of the outgoing generation lands either strictly
    /// before it (recording a lease the test then sees, so the slot is never
    /// rehanded) or strictly after it (refused as recycled) — never between,
    /// where it would lease a frame the producer is already overwriting.
    pub fn publish_frame_generation(&mut self, pool_slot_key: &str, frame_generation: u64) {
        self.table
            .publish_frame_generation(pool_slot_key, frame_generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_surface_is_checked_out_by_nobody() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let hand_off = registry.hold_for_pool_slot_hand_off().unwrap();
        assert!(!hand_off.is_checked_out_by_any_holder("surface-never-seen"));
    }

    #[test]
    fn a_checkout_holds_the_surface_until_its_own_release() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let child = registry.mint_holder_id();

        registry.record_check_out_lease("frame-7", child).unwrap();
        assert!(
            registry
                .hold_for_pool_slot_hand_off()
                .unwrap()
                .is_checked_out_by_any_holder("frame-7")
        );

        assert!(
            registry
                .release_one_check_out_lease("frame-7", child)
                .unwrap()
        );
        assert!(
            !registry
                .hold_for_pool_slot_hand_off()
                .unwrap()
                .is_checked_out_by_any_holder("frame-7")
        );
    }

    /// A typed cast's claim and that holder's own later `resolve_surface`
    /// are two leases on one frame. Releasing either must leave the surface
    /// held, or the pool rehands the slot while the other is still reading.
    #[test]
    fn one_holder_checking_a_surface_out_twice_needs_two_releases() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let child = registry.mint_holder_id();

        registry.record_check_out_lease("frame-7", child).unwrap();
        registry.record_check_out_lease("frame-7", child).unwrap();
        assert_eq!(registry.outstanding_check_out_count("frame-7").unwrap(), 2);

        registry
            .release_one_check_out_lease("frame-7", child)
            .unwrap();
        assert!(
            registry
                .hold_for_pool_slot_hand_off()
                .unwrap()
                .is_checked_out_by_any_holder("frame-7"),
            "the second checkout still stands"
        );

        registry
            .release_one_check_out_lease("frame-7", child)
            .unwrap();
        assert!(
            !registry
                .hold_for_pool_slot_hand_off()
                .unwrap()
                .is_checked_out_by_any_holder("frame-7")
        );
    }

    #[test]
    fn releasing_a_lease_this_holder_never_took_reports_it() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let holding_child = registry.mint_holder_id();
        let other_child = registry.mint_holder_id();
        registry
            .record_check_out_lease("frame-7", holding_child)
            .unwrap();

        assert!(
            !registry
                .release_one_check_out_lease("frame-7", other_child)
                .unwrap()
        );
        assert!(
            registry
                .hold_for_pool_slot_hand_off()
                .unwrap()
                .is_checked_out_by_any_holder("frame-7"),
            "one child's bogus release must not free another child's frame"
        );
    }

    /// The EPOLLHUP backstop: a child that dies holding surfaces releases
    /// every one of them, and its siblings' leases survive.
    #[test]
    fn dropping_a_holder_frees_only_its_own_leases() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let dying_child = registry.mint_holder_id();
        let surviving_child = registry.mint_holder_id();

        registry
            .record_check_out_lease("frame-7", dying_child)
            .unwrap();
        registry
            .record_check_out_lease("frame-8", dying_child)
            .unwrap();
        registry
            .record_check_out_lease("frame-8", surviving_child)
            .unwrap();

        assert_eq!(
            registry
                .release_every_check_out_lease_held_by(dying_child)
                .unwrap(),
            1,
            "only frame-7 becomes free; frame-8 still has a live reader"
        );
        let hand_off = registry.hold_for_pool_slot_hand_off().unwrap();
        assert!(!hand_off.is_checked_out_by_any_holder("frame-7"));
        assert!(hand_off.is_checked_out_by_any_holder("frame-8"));
    }

    /// The no-interleaving invariant. A checkout cannot land between the
    /// pool's availability test and its slot hand-off: both run under this
    /// guard and the minting side takes the same lock, so what the pool reads
    /// at the top of the decision is still true when it hands the slot over.
    ///
    /// Mental-revert: give the mint path a lock of its own and the spawned
    /// checkout lands mid-guard, so the pool promises a producer a slot a
    /// child has just started reading.
    #[test]
    fn a_checkout_cannot_land_inside_the_pools_hand_off() {
        let registry = std::sync::Arc::new(SurfaceCheckOutLeaseRegistry::new());
        let child = registry.mint_holder_id();

        let hand_off = registry.hold_for_pool_slot_hand_off().unwrap();
        assert!(!hand_off.is_checked_out_by_any_holder("frame-7"));

        let checking_out = std::sync::Arc::clone(&registry);
        let racing_checkout = std::thread::spawn(move || {
            checking_out
                .record_check_out_lease("frame-7", child)
                .unwrap();
        });

        for _ in 0..10_000 {
            assert!(
                !hand_off.is_checked_out_by_any_holder("frame-7"),
                "a checkout landed inside the pool's hand-off"
            );
        }

        drop(hand_off);
        racing_checkout.join().unwrap();
        assert!(
            registry
                .hold_for_pool_slot_hand_off()
                .unwrap()
                .is_checked_out_by_any_holder("frame-7"),
            "the checkout must land the moment the pool is done deciding"
        );
    }

    /// The frame-generation ledger: recycling a slot retires the previous
    /// published id, and a checkout naming it is refused loudly — the exact
    /// silent wrongness of #1872 (same id, frame 17's picture) made an error.
    #[test]
    fn a_checkout_of_a_retired_frame_id_is_refused_naming_the_recycling() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let child = registry.mint_holder_id();
        registry.publish_frame_generation("slot-a", 1).unwrap();

        registry
            .record_check_out_lease("slot-a#1", child)
            .expect("the current generation checks out");
        registry
            .release_one_check_out_lease("slot-a#1", child)
            .unwrap();

        registry.publish_frame_generation("slot-a", 2).unwrap();
        let refusal = registry
            .record_check_out_lease("slot-a#1", child)
            .expect_err("generation 1 was retired when generation 2 published");
        assert!(
            matches!(
                &refusal,
                Error::SurfaceFrameRecycled {
                    surface_id,
                    published_generation: 1,
                    current_generation: 2,
                } if surface_id == "slot-a#1"
            ),
            "got: {refusal}"
        );

        registry
            .record_check_out_lease("slot-a#2", child)
            .expect("the new current generation checks out");
    }

    /// A lease-free read gets the same loudness a checkout does.
    #[test]
    fn a_lookup_of_a_retired_frame_id_is_refused_too() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        registry.publish_frame_generation("slot-a", 3).unwrap();

        registry.refuse_a_retired_frame_id("slot-a#3").unwrap();
        registry
            .refuse_a_retired_frame_id("an-id-with-no-generation")
            .unwrap();
        assert!(registry.refuse_a_retired_frame_id("slot-a#2").is_err());
    }

    /// Fail closed: an id claiming a generation over a slot nobody published
    /// cannot be shown to name a live frame.
    #[test]
    fn a_generation_over_an_unpublished_slot_is_refused() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let child = registry.mint_holder_id();
        assert!(
            registry
                .record_check_out_lease("never-published#4", child)
                .is_err()
        );
        assert!(
            registry
                .refuse_a_retired_frame_id("never-published#4")
                .is_err()
        );
    }

    /// Two generations of one slot are one lease key: the lease protects the
    /// slot's memory, and the pool must see it whichever generation string
    /// the holder or the pool happens to ask with.
    #[test]
    fn leases_on_different_generations_of_one_slot_share_the_slot_key() {
        let registry = SurfaceCheckOutLeaseRegistry::new();
        let child = registry.mint_holder_id();
        registry.publish_frame_generation("slot-a", 1).unwrap();
        registry.record_check_out_lease("slot-a#1", child).unwrap();

        let hand_off = registry.hold_for_pool_slot_hand_off().unwrap();
        assert!(hand_off.is_checked_out_by_any_holder("slot-a"));
        assert!(hand_off.is_checked_out_by_any_holder("slot-a#1"));
        drop(hand_off);

        assert_eq!(registry.outstanding_check_out_count("slot-a").unwrap(), 1);
        registry
            .release_one_check_out_lease("slot-a#1", child)
            .unwrap();
        assert_eq!(registry.outstanding_check_out_count("slot-a").unwrap(), 0);
    }

    /// The refused half of the hand-off ordering: a checkout that loses the
    /// race to the retire is refused, never silently leased. (The other half
    /// — a checkout landing first is *seen* and the slot never rehanded — is
    /// [`a_checkout_cannot_land_inside_the_pools_hand_off`].)
    ///
    /// Mental-revert: publish the generation after dropping the guard and
    /// the racer can lease generation 1 between the availability test and
    /// the retire — a lease on a frame the producer is already overwriting.
    #[test]
    fn a_checkout_that_loses_the_race_to_the_retire_is_refused() {
        let registry = std::sync::Arc::new(SurfaceCheckOutLeaseRegistry::new());
        let child = registry.mint_holder_id();
        registry.publish_frame_generation("slot-a", 1).unwrap();

        let mut hand_off = registry.hold_for_pool_slot_hand_off().unwrap();
        let racing = std::sync::Arc::clone(&registry);
        let racing_checkout =
            std::thread::spawn(move || racing.record_check_out_lease("slot-a#1", child));

        // The guard is held: the slot shows no lease, so the pool retires
        // generation 1 and hands the slot over.
        assert!(!hand_off.is_checked_out_by_any_holder("slot-a"));
        hand_off.publish_frame_generation("slot-a", 2);
        drop(hand_off);

        let raced = racing_checkout.join().unwrap();
        assert!(
            matches!(raced, Err(Error::SurfaceFrameRecycled { .. })),
            "a checkout that lost the race must be refused, not silently leased: {raced:?}"
        );
    }

    /// Fail closed: a poisoned table is unreadable, and an unreadable table
    /// must not answer "free" for anything.
    #[test]
    fn a_poisoned_table_refuses_to_answer_rather_than_guessing() {
        let registry = std::sync::Arc::new(SurfaceCheckOutLeaseRegistry::new());
        let child = registry.mint_holder_id();
        registry.record_check_out_lease("frame-7", child).unwrap();

        let poisoning = std::sync::Arc::clone(&registry);
        let _ = std::thread::spawn(move || {
            let _held = poisoning.hold_for_pool_slot_hand_off().unwrap();
            panic!("poison the lease table");
        })
        .join();

        assert!(
            registry.hold_for_pool_slot_hand_off().is_none(),
            "the pool must be told it cannot read the table, not handed a stale answer"
        );
        assert!(registry.outstanding_check_out_count("frame-7").is_err());
        assert!(registry.record_check_out_lease("frame-8", child).is_err());
    }
}
