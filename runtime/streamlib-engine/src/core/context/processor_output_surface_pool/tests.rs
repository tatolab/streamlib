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

/// A pool whose allocations the test can count and hold.
struct PoolUnderTest {
    pool: ProcessorOutputSurfacePool<SlotResourceHeldOnlyWhenTheTestSays>,
    leases: SurfaceCheckOutLeaseRegistry,
    minted: LeaseAwarePoolMintedFrameGenerations,
    in_process_hold_by_slot: Vec<Arc<AtomicBool>>,
}

impl PoolUnderTest {
    fn new() -> Self {
        Self {
            pool: ProcessorOutputSurfacePool::new("pool-under-test".to_string()),
            leases: SurfaceCheckOutLeaseRegistry::new(),
            minted: LeaseAwarePoolMintedFrameGenerations::default(),
            in_process_hold_by_slot: Vec::new(),
        }
    }

    fn next_frame(&mut self, rotation_depth: usize) -> Result<String> {
        let handed_off = self.pool.hand_off_a_reusable_frame(
            rotation_depth,
            Some(&self.leases),
            &self.minted,
        )?;
        if let ProcessorOutputSurfacePoolHandOff::ReusedSlot(slot) = handed_off {
            return Ok(slot.currently_published_frame_id());
        }
        let fresh_slot_number = self.in_process_hold_by_slot.len();
        let held = Arc::new(AtomicBool::new(false));
        self.in_process_hold_by_slot.push(Arc::clone(&held));
        Ok(self
            .pool
            .hand_off_a_fresh_slot(
                format!("slot-{fresh_slot_number}"),
                SlotResourceHeldOnlyWhenTheTestSays {
                    held_in_this_process: held,
                },
                Some(&self.leases),
                &self.minted,
            )
            .map_err(|refused| refused.refusal)?
            .currently_published_frame_id())
    }

    fn check_out(&self, published_id: &str) {
        self.leases
            .record_check_out_lease(published_id, self.leases.mint_holder_id())
            .unwrap();
    }
}

#[test]
fn with_nothing_held_the_pool_never_grows_past_its_rotation_depth() {
    let mut pool = PoolUnderTest::new();
    let published: Vec<_> = (0..6).map(|_| pool.next_frame(2).unwrap()).collect();
    assert_eq!(
        published,
        [
            "slot-0#1", "slot-1#1", "slot-0#2", "slot-1#2", "slot-0#3", "slot-1#3"
        ]
    );
    assert_eq!(pool.pool.slot_count(), 2);
}

#[test]
fn a_held_frames_slot_is_skipped_never_rewritten() {
    let mut pool = PoolUnderTest::new();
    let held_frame = pool.next_frame(2).unwrap();
    pool.check_out(&held_frame);

    let while_held: Vec<_> = (0..4).map(|_| pool.next_frame(2).unwrap()).collect();
    assert!(
        while_held
            .iter()
            .all(|published| !published.starts_with("slot-0#")),
        "the held frame's slot was rehanded: {while_held:?}"
    );
    assert_eq!(
        pool.minted
            .refusal_of_a_retired_frame_id_named(&held_frame)
            .unwrap()
            .ok(),
        Some(()),
        "the held frame's id still names a live frame"
    );
}

#[test]
fn while_a_frame_is_held_a_frame_nobody_claimed_stays_resolvable_for_rotation_depth_publishes() {
    let mut pool = PoolUnderTest::new();
    let held_frame = pool.next_frame(2).unwrap();
    pool.check_out(&held_frame);
    let unclaimed_frame = pool.next_frame(2).unwrap();

    let published_after_the_unclaimed_frame = pool.next_frame(2).unwrap();
    assert_eq!(
        pool.minted
            .refusal_of_a_retired_frame_id_named(&unclaimed_frame)
            .unwrap()
            .ok(),
        Some(()),
        "{unclaimed_frame} was retired one publish later, by {published_after_the_unclaimed_frame}"
    );

    let published: Vec<_> = (0..2).map(|_| pool.next_frame(2).unwrap()).collect();
    assert_eq!(
        [
            held_frame.as_str(),
            unclaimed_frame.as_str(),
            published_after_the_unclaimed_frame.as_str(),
            published[0].as_str(),
            published[1].as_str(),
        ],
        ["slot-0#1", "slot-1#1", "slot-2#1", "slot-1#2", "slot-2#2"]
    );
    assert!(matches!(
        pool.minted
            .refusal_of_a_retired_frame_id_named(&unclaimed_frame),
        Some(Err(Error::SurfaceFrameRecycled { .. }))
    ));
    assert_eq!(pool.pool.slot_count(), 3);
}

#[test]
fn a_consumer_holding_an_older_frame_never_costs_a_newer_unclaimed_frame_its_window() {
    let mut pool = PoolUnderTest::new();
    let published_before_the_hold: Vec<_> = (0..3).map(|_| pool.next_frame(2).unwrap()).collect();
    assert_eq!(
        published_before_the_hold,
        ["slot-0#1", "slot-1#1", "slot-0#2"]
    );
    pool.check_out(&published_before_the_hold[1]);

    let grown_into = pool.next_frame(2).unwrap();
    assert_eq!(grown_into, "slot-2#1");
    let published_after_it = pool.next_frame(2).unwrap();
    assert_eq!(
        published_after_it, "slot-0#3",
        "the oldest unheld frame is the one retired"
    );
    assert_eq!(
        pool.minted
            .refusal_of_a_retired_frame_id_named(&grown_into)
            .unwrap()
            .ok(),
        Some(()),
        "{grown_into} was retired one publish later, by {published_after_it}"
    );
}

#[test]
fn at_its_cap_with_fewer_than_rotation_depth_slots_unheld_the_pool_reuses_one_rather_than_refusing()
{
    let mut pool = PoolUnderTest::new();
    for _ in 0..PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY - 1 {
        let published = pool.next_frame(2).unwrap();
        pool.check_out(&published);
    }
    let the_one_unheld_frame = pool.next_frame(2).unwrap();
    assert_eq!(the_one_unheld_frame, "slot-15#1");

    assert_eq!(pool.next_frame(2).unwrap(), "slot-15#2");
    assert_eq!(
        pool.pool.slot_count(),
        PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
    );
}

#[test]
fn every_slot_held_grows_the_pool_until_its_cap_then_refuses_by_name() {
    let mut pool = PoolUnderTest::new();
    for _ in 0..PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY {
        let published = pool.next_frame(2).unwrap();
        pool.check_out(&published);
    }
    assert_eq!(
        pool.pool.slot_count(),
        PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
    );

    let refusal = pool
        .next_frame(2)
        .expect_err("every slot is held at the cap");
    assert!(
        matches!(
            &refusal,
            Error::EverySlotInTheProcessorOutputPoolIsInUse {
                pool_key,
                pool_capacity: PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY,
            } if pool_key == "pool-under-test"
        ),
        "got: {refusal}"
    );
    assert_eq!(
        pool.pool.slot_count(),
        PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY,
        "a refusal allocates nothing"
    );
}

#[test]
fn a_slot_held_in_this_process_counts_as_held() {
    let mut pool = PoolUnderTest::new();
    pool.next_frame(1).unwrap();
    pool.in_process_hold_by_slot[0].store(true, Ordering::SeqCst);
    assert_eq!(pool.next_frame(1).unwrap(), "slot-1#1");
    pool.in_process_hold_by_slot[0].store(false, Ordering::SeqCst);
    assert_eq!(pool.next_frame(1).unwrap(), "slot-0#2");
}

#[test]
fn a_recycled_frames_id_is_refused_naming_the_recycling() {
    let mut pool = PoolUnderTest::new();
    let first = pool.next_frame(1).unwrap();
    pool.next_frame(1).unwrap();
    assert!(matches!(
        pool.minted.refusal_of_a_retired_frame_id_named(&first),
        Some(Err(Error::SurfaceFrameRecycled { .. }))
    ));
    assert!(matches!(
        pool.leases.refuse_a_retired_frame_id(&first),
        Err(Error::SurfaceFrameRecycled { .. })
    ));
}

#[test]
fn a_rotation_depth_of_zero_or_past_the_cap_is_refused() {
    let mut pool = PoolUnderTest::new();
    assert!(pool.next_frame(0).is_err());
    assert!(
        pool.next_frame(PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY + 1)
            .is_err()
    );
    assert_eq!(pool.pool.slot_count(), 0);
}

#[test]
fn a_pool_short_of_its_depth_asks_for_a_fresh_slot_until_one_is_handed_in() {
    let mut pool = ProcessorOutputSurfacePool::<SlotResourceHeldOnlyWhenTheTestSays>::new(
        "pool-under-test".to_string(),
    );
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    for _ in 0..2 {
        assert!(matches!(
            pool.hand_off_a_reusable_frame(2, None, &minted),
            Ok(ProcessorOutputSurfacePoolHandOff::NeedsAFreshSlot)
        ));
    }
    assert_eq!(pool.slot_count(), 0, "asking for a fresh slot adds none");
}

#[test]
fn at_its_rotation_depth_with_nothing_held_the_pool_reuses_without_asking_for_a_slot() {
    let mut pool = PoolUnderTest::new();
    pool.next_frame(2).unwrap();
    pool.next_frame(2).unwrap();
    for _ in 0..4 {
        assert!(matches!(
            pool.pool
                .hand_off_a_reusable_frame(2, Some(&pool.leases), &pool.minted),
            Ok(ProcessorOutputSurfacePoolHandOff::ReusedSlot(_))
        ));
    }
    assert_eq!(pool.pool.slot_count(), 2);
}

#[test]
fn a_fresh_slot_handed_in_at_the_cap_is_refused_by_name_and_handed_back() {
    let mut pool = PoolUnderTest::new();
    for _ in 0..PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY {
        let published = pool.next_frame(2).unwrap();
        pool.check_out(&published);
    }
    let Err(refused) = pool.pool.hand_off_a_fresh_slot(
        "slot-past-the-cap".to_string(),
        SlotResourceHeldOnlyWhenTheTestSays {
            held_in_this_process: Arc::new(AtomicBool::new(false)),
        },
        Some(&pool.leases),
        &pool.minted,
    ) else {
        panic!("a fresh slot past the cap was added");
    };
    assert!(matches!(
        refused.refusal,
        Error::EverySlotInTheProcessorOutputPoolIsInUse { .. }
    ));
    assert_eq!(refused.pool_slot_key, "slot-past-the-cap");
    assert_eq!(
        pool.pool.slot_count(),
        PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
    );
}

/// A slot `lending_ring` published `frames_published_before_lending` frames
/// from, taken out of it to lend.
fn slot_lent_out_of(
    lending_ring: &mut LeaseAwarePoolSlotRing<SlotResourceHeldOnlyWhenTheTestSays>,
    pool_slot_key: &str,
    frames_published_before_lending: usize,
    leases: &SurfaceCheckOutLeaseRegistry,
    minted: &LeaseAwarePoolMintedFrameGenerations,
) -> LeaseAwarePoolSlot<SlotResourceHeldOnlyWhenTheTestSays> {
    let slot_index = lending_ring.push_fresh_slot(
        pool_slot_key.to_string(),
        SlotResourceHeldOnlyWhenTheTestSays {
            held_in_this_process: Arc::new(AtomicBool::new(false)),
        },
    );
    lending_ring.hand_off_fresh_slot(slot_index, Some(leases), minted);
    for _ in 1..frames_published_before_lending {
        assert_eq!(
            lending_ring.hand_off_a_reusable_slot(
                ANY_ONE_UNHELD_SLOT_IS_REUSABLE,
                Some(leases),
                minted
            ),
            Some(slot_index),
            "the lending ring republishes its one unheld slot"
        );
    }
    lending_ring.take_out_the_slot_at(slot_index)
}

#[test]
fn a_slot_its_lender_republished_counts_toward_the_cap_and_is_handed_back_at_it() {
    let mut pool = PoolUnderTest::new();
    let mut lending_ring = LeaseAwarePoolSlotRing::default();
    for lent_slot_number in 0..PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY {
        assert!(
            matches!(
                pool.pool
                    .hand_off_a_reusable_frame(2, Some(&pool.leases), &pool.minted),
                Ok(ProcessorOutputSurfacePoolHandOff::NeedsAFreshSlot)
            ),
            "with every slot held the pool asks for one more"
        );
        let lent_slot = slot_lent_out_of(
            &mut lending_ring,
            &format!("lent-slot-{lent_slot_number}"),
            1,
            &pool.leases,
            &pool.minted,
        );
        let published = pool
            .pool
            .hand_off_a_slot_its_lender_already_republished(lent_slot)
            .map_err(|refused| refused.refusal)
            .expect("a lent slot under the cap is added")
            .currently_published_frame_id();
        pool.check_out(&published);
    }

    let refusal = match pool
        .pool
        .hand_off_a_reusable_frame(2, Some(&pool.leases), &pool.minted)
    {
        Err(refusal) => refusal,
        Ok(_) => panic!("lent slots count toward the cap like allocated ones"),
    };
    assert!(
        matches!(
            &refusal,
            Error::EverySlotInTheProcessorOutputPoolIsInUse { pool_key, .. }
                if pool_key == "pool-under-test"
        ),
        "got: {refusal}"
    );

    let lent_past_the_cap = slot_lent_out_of(
        &mut lending_ring,
        "lent-slot-past-the-cap",
        1,
        &pool.leases,
        &pool.minted,
    );
    let Err(refused) = pool
        .pool
        .hand_off_a_slot_its_lender_already_republished(lent_past_the_cap)
    else {
        panic!("a lent slot past the cap was added");
    };
    assert!(matches!(
        refused.refusal,
        Error::EverySlotInTheProcessorOutputPoolIsInUse { .. }
    ));
    assert_eq!(refused.slot.pool_slot_key(), "lent-slot-past-the-cap");
    assert_eq!(
        refused.slot.published_frame_generation(),
        1,
        "the slot goes back to its lender as its lender published it"
    );
    assert_eq!(
        pool.pool.slot_count(),
        PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
    );
}

#[test]
fn a_lent_slot_is_handed_out_under_the_generation_its_lender_minted() {
    let mut pool = PoolUnderTest::new();
    let mut lending_ring = LeaseAwarePoolSlotRing::default();
    let lent_slot = slot_lent_out_of(
        &mut lending_ring,
        "lent-slot",
        3,
        &pool.leases,
        &pool.minted,
    );
    assert!(matches!(
        pool.pool
            .hand_off_a_reusable_frame(1, Some(&pool.leases), &pool.minted),
        Ok(ProcessorOutputSurfacePoolHandOff::NeedsAFreshSlot)
    ));

    let handed_out = pool
        .pool
        .hand_off_a_slot_its_lender_already_republished(lent_slot)
        .map_err(|refused| refused.refusal)
        .expect("an empty pool takes a lent slot")
        .currently_published_frame_id();
    assert_eq!(
        handed_out, "lent-slot#3",
        "the pool hands the lent slot out under the generation its lender minted, minting none"
    );
    assert_eq!(
        pool.minted.minted_frame_generation_of_slot("lent-slot"),
        Some(3)
    );
    assert_eq!(
        pool.leases.current_frame_generation("lent-slot").unwrap(),
        Some(3)
    );

    let reused = match pool
        .pool
        .hand_off_a_reusable_frame(1, Some(&pool.leases), &pool.minted)
    {
        Ok(ProcessorOutputSurfacePoolHandOff::ReusedSlot(slot)) => {
            slot.currently_published_frame_id()
        }
        _ => panic!("at its rotation depth with nothing held the pool reuses the lent slot"),
    };
    assert_eq!(reused, "lent-slot#4");
    assert!(matches!(
        pool.minted.refusal_of_a_retired_frame_id_named(&handed_out),
        Some(Err(Error::SurfaceFrameRecycled { .. }))
    ));
}
