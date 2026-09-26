// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// A slot resource whose in-process hold the test flips by hand.
struct SlotResourceHeldOnlyWhenTheTestSays {
    held_in_this_process: Arc<AtomicBool>,
}

impl LeaseAwarePoolSlotResource for SlotResourceHeldOnlyWhenTheTestSays {
    fn is_held_in_this_process(&self) -> bool {
        self.held_in_this_process.load(Ordering::SeqCst)
    }
}

fn ring_of_fresh_slots(
    pool_slot_keys: &[&str],
) -> (
    LeaseAwarePoolSlotRing<SlotResourceHeldOnlyWhenTheTestSays>,
    Vec<Arc<AtomicBool>>,
) {
    let mut ring = LeaseAwarePoolSlotRing::default();
    let holds = pool_slot_keys
        .iter()
        .map(|pool_slot_key| {
            let held = Arc::new(AtomicBool::new(false));
            ring.push_fresh_slot(
                pool_slot_key.to_string(),
                SlotResourceHeldOnlyWhenTheTestSays {
                    held_in_this_process: Arc::clone(&held),
                },
            );
            held
        })
        .collect();
    (ring, holds)
}

fn next_published_id(
    ring: &mut LeaseAwarePoolSlotRing<SlotResourceHeldOnlyWhenTheTestSays>,
    leases: Option<&SurfaceCheckOutLeaseRegistry>,
    minted: &LeaseAwarePoolMintedFrameGenerations,
) -> Option<String> {
    ring.hand_off_a_reusable_slot(leases, minted)
        .map(|slot_index| ring.slot(slot_index).currently_published_frame_id())
}

#[test]
fn unheld_slots_are_handed_out_in_ring_order_under_advancing_generations() {
    let (mut ring, _holds) = ring_of_fresh_slots(&["slot-a", "slot-b"]);
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    let published: Vec<_> = (0..5)
        .map(|_| next_published_id(&mut ring, None, &minted).unwrap())
        .collect();
    assert_eq!(
        published,
        ["slot-a#1", "slot-b#1", "slot-a#2", "slot-b#2", "slot-a#3"]
    );
}

#[test]
fn a_slot_a_consumer_has_checked_out_is_skipped_until_released() {
    let (mut ring, _holds) = ring_of_fresh_slots(&["slot-a", "slot-b"]);
    let leases = SurfaceCheckOutLeaseRegistry::new();
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    let consumer = leases.mint_holder_id();

    let first = next_published_id(&mut ring, Some(&leases), &minted).unwrap();
    assert_eq!(first, "slot-a#1");
    leases.record_check_out_lease(&first, consumer).unwrap();

    let while_held: Vec<_> = (0..3)
        .map(|_| next_published_id(&mut ring, Some(&leases), &minted).unwrap())
        .collect();
    assert_eq!(
        while_held,
        ["slot-b#1", "slot-b#2", "slot-b#3"],
        "a leased slot must never be rehanded to its producer"
    );

    leases.release_one_check_out_lease(&first, consumer).unwrap();
    assert_eq!(
        next_published_id(&mut ring, Some(&leases), &minted).unwrap(),
        "slot-a#2"
    );
}

#[test]
fn a_slot_held_in_this_process_is_skipped() {
    let (mut ring, holds) = ring_of_fresh_slots(&["slot-a", "slot-b"]);
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    holds[0].store(true, Ordering::SeqCst);
    assert_eq!(
        next_published_id(&mut ring, None, &minted).unwrap(),
        "slot-b#1"
    );
    assert_eq!(
        next_published_id(&mut ring, None, &minted).unwrap(),
        "slot-b#2"
    );
}

#[test]
fn with_every_slot_held_nothing_is_handed_out_and_no_generation_moves() {
    let (mut ring, holds) = ring_of_fresh_slots(&["slot-a", "slot-b"]);
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    for held in &holds {
        held.store(true, Ordering::SeqCst);
    }
    assert_eq!(next_published_id(&mut ring, None, &minted), None);
    assert_eq!(ring.slot(0).published_frame_generation(), 0);
    assert_eq!(ring.slot(1).published_frame_generation(), 0);
}

/// Fail closed: an unreadable lease table proves no slot free.
#[test]
fn a_poisoned_lease_table_lets_no_slot_be_reused() {
    let (mut ring, _holds) = ring_of_fresh_slots(&["slot-a"]);
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    let leases = Arc::new(SurfaceCheckOutLeaseRegistry::new());
    let poisoning = Arc::clone(&leases);
    let _ = std::thread::spawn(move || {
        let _held = poisoning.hold_for_pool_slot_hand_off().unwrap();
        panic!("poison the lease table");
    })
    .join();
    assert_eq!(next_published_id(&mut ring, Some(&leases), &minted), None);
}

#[test]
fn recycling_a_slot_retires_its_previous_id_in_process_and_at_the_service() {
    let (mut ring, _holds) = ring_of_fresh_slots(&["slot-a"]);
    let leases = SurfaceCheckOutLeaseRegistry::new();
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    let first = next_published_id(&mut ring, Some(&leases), &minted).unwrap();
    let second = next_published_id(&mut ring, Some(&leases), &minted).unwrap();

    assert!(matches!(
        minted.refusal_of_a_retired_frame_id(&first),
        Some(Err(Error::SurfaceFrameRecycled {
            published_generation: 1,
            current_generation: 2,
            ..
        }))
    ));
    assert!(matches!(
        minted.refusal_of_a_retired_frame_id(&second),
        Some(Ok(()))
    ));
    assert!(
        leases
            .record_check_out_lease(&first, leases.mint_holder_id())
            .is_err(),
        "the service refuses a checkout of the retired id too"
    );
    assert!(
        minted
            .refusal_of_a_retired_frame_id("a-slot-no-ring-owns#1")
            .is_none(),
        "a slot this index never minted is someone else's question"
    );
}

#[test]
fn a_fresh_slots_first_frame_is_published_at_the_service() {
    let mut ring = LeaseAwarePoolSlotRing::default();
    let leases = SurfaceCheckOutLeaseRegistry::new();
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    let slot_index = ring.push_fresh_slot(
        "slot-a".to_string(),
        SlotResourceHeldOnlyWhenTheTestSays {
            held_in_this_process: Arc::new(AtomicBool::new(false)),
        },
    );
    ring.hand_off_fresh_slot(slot_index, Some(&leases), &minted);
    assert_eq!(ring.slot(slot_index).currently_published_frame_id(), "slot-a#1");
    assert_eq!(leases.current_frame_generation("slot-a").unwrap(), Some(1));
    assert_eq!(minted.minted_frame_generation_of_slot("slot-a"), Some(1));
}
