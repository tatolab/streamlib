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
        let fresh_slot_number = self.in_process_hold_by_slot.len();
        let held = Arc::new(AtomicBool::new(false));
        let mut allocated = None;
        let published = self
            .pool
            .hand_off_next_frame(rotation_depth, Some(&self.leases), &self.minted, || {
                allocated = Some(Arc::clone(&held));
                Ok((
                    format!("slot-{fresh_slot_number}"),
                    SlotResourceHeldOnlyWhenTheTestSays {
                        held_in_this_process: held,
                    },
                ))
            })?
            .currently_published_frame_id();
        self.in_process_hold_by_slot.extend(allocated);
        Ok(published)
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
        ["slot-0#1", "slot-1#1", "slot-0#2", "slot-1#2", "slot-0#3", "slot-1#3"]
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
        while_held.iter().all(|published| !published.starts_with("slot-0#")),
        "the held frame's slot was rehanded: {while_held:?}"
    );
    assert_eq!(
        pool.minted.refusal_of_a_retired_frame_id(&held_frame).unwrap().ok(),
        Some(()),
        "the held frame's id still names a live frame"
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

    let refusal = pool.next_frame(2).expect_err("every slot is held at the cap");
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
        pool.minted.refusal_of_a_retired_frame_id(&first),
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
    assert!(pool.next_frame(PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY + 1).is_err());
    assert_eq!(pool.pool.slot_count(), 0);
}

#[test]
fn a_failed_allocation_reaches_the_caller_and_adds_no_slot() {
    let mut pool = ProcessorOutputSurfacePool::<SlotResourceHeldOnlyWhenTheTestSays>::new(
        "pool-under-test".to_string(),
    );
    let minted = LeaseAwarePoolMintedFrameGenerations::default();
    let refusal = pool
        .hand_off_next_frame(2, None, &minted, || {
            Err(Error::GpuError("out of device memory".into()))
        })
        .err()
        .expect("the allocation failure propagates");
    assert!(matches!(refusal, Error::GpuError(_)));
    assert_eq!(pool.slot_count(), 0);
}
