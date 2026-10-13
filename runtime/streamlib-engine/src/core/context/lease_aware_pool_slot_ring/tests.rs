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

    leases
        .release_one_check_out_lease(&first, consumer)
        .unwrap();
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
        minted.refusal_of_a_retired_frame_id_named(&first),
        Some(Err(Error::SurfaceFrameRecycled {
            published_generation: 1,
            current_generation: 2,
            ..
        }))
    ));
    assert!(matches!(
        minted.refusal_of_a_retired_frame_id_named(&second),
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
            .refusal_of_a_retired_frame_id_named("a-slot-no-ring-owns#1")
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
    assert_eq!(
        ring.slot(slot_index).currently_published_frame_id(),
        "slot-a#1"
    );
    assert_eq!(leases.current_frame_generation("slot-a").unwrap(), Some(1));
    assert_eq!(minted.minted_frame_generation_of_slot("slot-a"), Some(1));
}

#[test]
fn a_slot_taken_out_of_one_ring_keeps_counting_its_generations_in_the_next() {
    let (mut lending_ring, _lending_holds) = ring_of_fresh_slots(&["slot-a"]);
    let (mut borrowing_ring, _borrowing_holds) = ring_of_fresh_slots(&[]);
    let leases = SurfaceCheckOutLeaseRegistry::new();
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    next_published_id(&mut lending_ring, Some(&leases), &minted).unwrap();
    let last_published_in_the_lending_ring =
        next_published_id(&mut lending_ring, Some(&leases), &minted).unwrap();
    assert_eq!(last_published_in_the_lending_ring, "slot-a#2");

    let lent_slot = lending_ring.take_out_the_slot_at(0);
    assert_eq!(lending_ring.slot_count(), 0);
    let lent_slot_index =
        borrowing_ring.push_a_slot_keeping_its_published_frame_generation(lent_slot);
    assert_eq!(
        borrowing_ring
            .slot(lent_slot_index)
            .published_frame_generation(),
        2,
        "the slot arrives with the generation its lender last published"
    );

    assert_eq!(
        next_published_id(&mut borrowing_ring, Some(&leases), &minted).unwrap(),
        "slot-a#3"
    );
    assert!(
        matches!(
            minted.refusal_of_a_retired_frame_id_named(&last_published_in_the_lending_ring),
            Some(Err(Error::SurfaceFrameRecycled {
                published_generation: 2,
                current_generation: 3,
                ..
            }))
        ),
        "the borrowing ring's hand-off retires the last id the lending ring published"
    );
    assert!(
        leases
            .record_check_out_lease(&last_published_in_the_lending_ring, leases.mint_holder_id())
            .is_err(),
        "the service refuses a checkout of the id the borrowing ring retired"
    );
}

#[test]
fn taking_a_slot_out_keeps_the_rest_of_the_ring_in_ring_order() {
    let (mut ring, _holds) = ring_of_fresh_slots(&["slot-a", "slot-b", "slot-c", "slot-d"]);
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    assert_eq!(
        next_published_id(&mut ring, None, &minted).unwrap(),
        "slot-a#1"
    );
    assert_eq!(
        next_published_id(&mut ring, None, &minted).unwrap(),
        "slot-b#1"
    );

    let taken_out = ring.take_out_the_slot_at(1);
    assert_eq!(taken_out.pool_slot_key(), "slot-b");
    let after_the_take_out: Vec<_> = (0..4)
        .map(|_| next_published_id(&mut ring, None, &minted).unwrap())
        .collect();
    assert_eq!(
        after_the_take_out,
        ["slot-c#1", "slot-d#1", "slot-a#2", "slot-c#2"],
        "the ring goes on from the slot after the one taken out"
    );

    let last_slot_index = ring.slot_count() - 1;
    let taken_out_last = ring.take_out_the_slot_at(last_slot_index);
    assert_eq!(taken_out_last.pool_slot_key(), "slot-d");
    assert_eq!(
        next_published_id(&mut ring, None, &minted).unwrap(),
        "slot-a#3",
        "taking out the slot due next hands out the one after it, wrapping round"
    );
}

#[test]
fn only_unheld_slots_chosen_to_be_freed_are_taken_out_and_their_ids_retired_everywhere() {
    let (mut ring, holds) = ring_of_fresh_slots(&["slot-a", "slot-b", "slot-c", "slot-d"]);
    let leases = SurfaceCheckOutLeaseRegistry::new();
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    let published: Vec<_> = (0..4)
        .map(|_| next_published_id(&mut ring, Some(&leases), &minted).unwrap())
        .collect();
    let consumer = leases.mint_holder_id();
    leases
        .record_check_out_lease(&published[1], consumer)
        .expect("the consumer checks slot-b's frame out");
    holds[2].store(true, Ordering::SeqCst);

    let mut cross_process_holders = PoolSlotReuse::over(Some(&leases));
    let mut slots_shown = Vec::new();
    let taken_out = ring.take_out_and_retire_every_unheld_slot_chosen(
        &mut cross_process_holders,
        &minted,
        |slot, is_unheld| {
            slots_shown.push((slot.pool_slot_key().to_string(), is_unheld));
            slot.pool_slot_key() != "slot-d"
        },
    );
    drop(cross_process_holders);

    assert_eq!(
        slots_shown,
        [
            ("slot-a".to_string(), true),
            ("slot-b".to_string(), false),
            ("slot-c".to_string(), false),
            ("slot-d".to_string(), true),
        ],
        "every slot is shown once, with whether it is unheld"
    );
    let taken_out_keys: Vec<_> = taken_out
        .iter()
        .map(|slot| slot.pool_slot_key().to_string())
        .collect();
    assert_eq!(
        taken_out_keys,
        ["slot-a"],
        "a held or leased slot was taken out, or an unheld one not chosen"
    );
    assert_eq!(ring.slot_count(), 3);
    assert!(matches!(
        minted.refusal_of_a_retired_frame_id_named(&published[0]),
        Some(Err(Error::SurfaceFrameRecycled {
            published_generation: 1,
            current_generation: 2,
            ..
        }))
    ));
    assert!(matches!(
        leases.record_check_out_lease(&published[0], leases.mint_holder_id()),
        Err(Error::SurfaceFrameRecycled { .. })
    ));
    for still_current in &published[1..] {
        assert!(matches!(
            minted.refusal_of_a_retired_frame_id_named(still_current),
            Some(Ok(()))
        ));
    }
}

#[test]
fn observing_the_ring_shows_every_slot_and_takes_none_out() {
    let (mut ring, holds) = ring_of_fresh_slots(&["slot-a", "slot-b"]);
    holds[0].store(true, Ordering::SeqCst);
    let mut slots_shown = Vec::new();
    ring.observe_whether_each_slot_is_unheld(
        &PoolSlotReuse::RefcountIsTheWholeAnswer,
        |slot, is_unheld| slots_shown.push((slot.pool_slot_key().to_string(), is_unheld)),
    );
    assert_eq!(
        slots_shown,
        [("slot-a".to_string(), false), ("slot-b".to_string(), true)]
    );
    assert_eq!(ring.slot_count(), 2);
}
