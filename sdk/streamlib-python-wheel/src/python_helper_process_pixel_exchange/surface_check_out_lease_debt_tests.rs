// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

/// The client needs escalate callables it never uses here — the lease
/// path speaks only to the surface-share socket.
fn exchange_client_on(share: &SurfaceShareUnderTest) -> Arc<HelperProcessGpuExchangeClient> {
    Python::initialize();
    Python::attach(|python| {
        Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            python.None(),
            share.channel_name_for_the_helper(),
            "helper:lease-debt-under-test".to_string(),
        ))
    })
}

/// The checkout claims the frame; dropping the debt — the last share of
/// the surface going away — is what returns the slot to its producer.
#[test]
fn a_check_out_is_held_until_its_debt_drops() {
    let share = SurfaceShareUnderTest::start("debt");
    let surface_id = share.publish_one_surface();
    let exchange_client = exchange_client_on(&share);

    let (response, _plane_fds_closed_by_scope) = exchange_client
        .check_out_surface(&surface_id)
        .expect("the checkout round trip");
    assert!(
        response.get("error").is_none(),
        "the service refused the checkout: {response}"
    );
    let debt = HelperSurfaceCheckOutLeaseDebt {
        exchange_client: Arc::clone(&exchange_client),
        surface_id: surface_id.clone(),
    };
    assert_eq!(
        share.outstanding_claims_on(&surface_id),
        1,
        "a checked-out frame is claimed against producer reuse"
    );

    drop(debt);
    assert_eq!(
        share.outstanding_claims_on(&surface_id),
        0,
        "the slot returns to its producer when the last share lets go"
    );
}

/// The claim a cast takes: the same lease the resolve path mints, without
/// the memory — an object that only needs the frame to hold still owes no
/// Vulkan import for it, which is also why this is provable with no GPU.
#[test]
fn a_claim_pins_the_frame_without_importing_it() {
    let share = SurfaceShareUnderTest::start("claim");
    let surface_id = share.publish_one_surface();
    let exchange_client = exchange_client_on(&share);

    let claim = exchange_client
        .claim_surface_against_producer_reuse(&surface_id)
        .expect("the claim round trip");
    assert_eq!(share.outstanding_claims_on(&surface_id), 1);

    // Claims are counted: a second one on the same surface is its own, and
    // releasing it leaves the first standing.
    let second_claim = exchange_client
        .claim_surface_against_producer_reuse(&surface_id)
        .expect("a second claim on one surface");
    assert_eq!(share.outstanding_claims_on(&surface_id), 2);
    drop(second_claim);
    assert_eq!(
        share.outstanding_claims_on(&surface_id),
        1,
        "one holder letting go must not release another holder's claim"
    );

    drop(claim);
    assert_eq!(share.outstanding_claims_on(&surface_id), 0);
}

/// #1872 as the wheel sees it: a frame the producer recycled is not a
/// claim to be taken quietly. The typed cast on a stale bag must raise —
/// pinning the slot's *current* frame while the caller believes it pinned
/// the delivered one would be the same silent wrongness one layer up.
#[test]
fn claiming_a_recycled_frame_is_refused_naming_the_recycling() {
    let share = SurfaceShareUnderTest::start("recycled");
    share.publish_pool_slot_frame("pool-slot-under-test", 1);
    let exchange_client = exchange_client_on(&share);

    let claim_while_current = exchange_client
        .claim_surface_against_producer_reuse("pool-slot-under-test#1")
        .expect("the current frame claims");
    assert_eq!(share.outstanding_claims_on("pool-slot-under-test"), 1);
    drop(claim_while_current);

    // The producer laps the pool: generation 2 publishes, 1 retires.
    share.publish_pool_slot_frame("pool-slot-under-test", 2);

    let Err(refusal) =
        exchange_client.claim_surface_against_producer_reuse("pool-slot-under-test#1")
    else {
        panic!("claiming a recycled frame must refuse");
    };
    assert!(
        refusal.to_string().contains("recycled"),
        "the refusal must say the frame was recycled: {refusal}"
    );
    assert_eq!(
        share.outstanding_claims_on("pool-slot-under-test"),
        0,
        "a refused claim must leave no lease behind"
    );

    exchange_client
        .claim_surface_against_producer_reuse("pool-slot-under-test#2")
        .expect("the new current frame claims");
}

/// A surface the service does not know is not a claim to be taken quietly:
/// the caller decides what an unclaimable frame means, so the refusal has
/// to reach it.
#[test]
fn claiming_a_surface_the_service_does_not_know_is_refused_by_name() {
    let share = SurfaceShareUnderTest::start("unknown");
    let exchange_client = exchange_client_on(&share);

    let Err(refusal) = exchange_client.claim_surface_against_producer_reuse("no-such-surface")
    else {
        panic!("claiming a surface the service does not know must refuse");
    };
    assert!(
        refusal.to_string().contains("no-such-surface"),
        "the refusal must name the surface: {refusal}"
    );
}
