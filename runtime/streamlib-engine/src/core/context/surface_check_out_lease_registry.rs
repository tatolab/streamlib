// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The cross-process half of the pixel-buffer pool's taken-until-released test.
//!
//! In one address space a held surface is an `Arc` refcount the pool can see; a
//! helper child holding the same surface bumps nothing, so its checkout records
//! a lease here instead. Rationale:
//! `docs/decisions/surface-id-lifetime-contract.md`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

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
    /// Surface id → holder → outstanding checkouts that holder has taken.
    ///
    /// Counted, never a set: a child checks one surface out twice — once
    /// eagerly when the bag carrying it is queued, once when user code
    /// resolves it — and releasing either must leave the other standing.
    outstanding_check_outs_by_surface_id:
        HashMap<String, HashMap<SurfaceCheckOutLeaseHolderId, u32>>,
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
        *table
            .outstanding_check_outs_by_surface_id
            .entry(surface_id.to_string())
            .or_default()
            .entry(holder)
            .or_insert(0) += 1;
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
        let mut table = self.readable_table()?;
        let Some(holders) = table
            .outstanding_check_outs_by_surface_id
            .get_mut(surface_id)
        else {
            return Ok(false);
        };
        let Some(outstanding) = holders.get_mut(&holder) else {
            return Ok(false);
        };
        *outstanding -= 1;
        if *outstanding == 0 {
            holders.remove(&holder);
        }
        if holders.is_empty() {
            table
                .outstanding_check_outs_by_surface_id
                .remove(surface_id);
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

    /// How many outstanding checkouts stand against `surface_id`, across every
    /// holder.
    pub fn outstanding_check_out_count(&self, surface_id: &str) -> Result<u32> {
        let table = self.readable_table()?;
        Ok(table
            .outstanding_check_outs_by_surface_id
            .get(surface_id)
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
    /// Whether any cross-process holder has this surface checked out.
    pub fn is_checked_out_by_any_holder(&self, surface_id: &str) -> bool {
        self.table
            .outstanding_check_outs_by_surface_id
            .contains_key(surface_id)
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

    /// The eager checkout at bag receipt and the checkout user code takes when
    /// it resolves the surface are two leases on one id. Releasing the eager
    /// one must leave the surface held, or the pool rehands the slot while the
    /// callback is still reading it.
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
