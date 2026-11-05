// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Channel sizing under a fixed initial prime.
//!
//! Every channel publisher is primed at [`DEFAULT_EXPECTED_PAYLOAD_BYTES`] and
//! grows on the first oversized loan; no per-port hint is resolved. These tests
//! hold the line that the prime is a starting size and never a cap — an
//! encoded-video channel must carry a frame several times its prime, header and
//! payload bytes intact.

use std::time::{Duration, Instant};

use crate::iceoryx2::channel_sample_test_support::{TestChannelSample, stamped_payload_of};
use crate::iceoryx2::{
    ChannelDataServiceSubscriber, DEFAULT_EXPECTED_PAYLOAD_BYTES, DataChannelPayloadCacheLine,
    DataChannelSampleUserHeader, DeliveryProfile, Iceoryx2Node,
    data_channel_payload_cache_lines_for,
};

/// Derived from the prime rather than written as a literal, so raising
/// [`DEFAULT_EXPECTED_PAYLOAD_BYTES`] past it can never quietly turn the growth
/// tests below into no-ops.
const OVERSIZED_PAYLOAD_BYTES: usize = DEFAULT_EXPECTED_PAYLOAD_BYTES * 4;

/// Poll a subscriber until it yields one sample or the deadline passes, handing
/// back its user header and stamped payload. A transport error is a failure in
/// its own right, never a timeout.
fn receive_one_sample_within(
    subscriber: &ChannelDataServiceSubscriber,
    timeout: Duration,
) -> Option<(DataChannelSampleUserHeader, Vec<u8>)> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                return Some((*sample.user_header(), stamped_payload_of(&sample).to_vec()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(err) => panic!("subscriber.receive() failed: {err:?}"),
        }
    }
    None
}

/// The counterfactual that gives the growth tests their teeth: without
/// [`AllocationStrategy::PowerOfTwo`], a publisher primed at the default rejects
/// an oversized loan. `create_publisher` sets that strategy, which is the whole
/// reason a fixed prime is safe.
#[test]
fn loan_past_the_prime_fails_when_the_publisher_cannot_grow() {
    let service = Iceoryx2Node::for_this_test_process()
        .open_or_create_service(
            "streamlib/test/sizing-no-growth",
            2,
            DeliveryProfile::ORDERED_DEPTH,
        )
        .unwrap();
    let publisher = service.create_publisher_that_cannot_grow(DEFAULT_EXPECTED_PAYLOAD_BYTES);

    assert!(
        publisher
            .loan_slice_uninit(data_channel_payload_cache_lines_for(
                OVERSIZED_PAYLOAD_BYTES
            ))
            .is_err(),
        "a publisher primed at the default with no growth strategy must reject an \
         oversized loan — if this passes, the growth tests below prove nothing"
    );
}

/// A channel publisher primed at the default grows to loan an oversized payload.
/// Mentally dropping the `PowerOfTwo` allocation strategy from `create_publisher`
/// trips the `expect`.
#[test]
fn default_primed_publisher_grows_to_loan_an_oversized_payload() {
    let max_subscribers = 2;
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_service(
            "streamlib/test/sizing-default-grows",
            max_subscribers,
            DeliveryProfile::ORDERED_DEPTH,
        )
        .unwrap();
    let publisher = service
        .create_publisher(DEFAULT_EXPECTED_PAYLOAD_BYTES)
        .unwrap();

    let cache_lines = data_channel_payload_cache_lines_for(OVERSIZED_PAYLOAD_BYTES);
    let sample = publisher.loan_slice_uninit(cache_lines).expect(
        "a publisher primed at DEFAULT_EXPECTED_PAYLOAD_BYTES must grow to loan an \
         oversized slice rather than rejecting it",
    );
    sample
        .write_from_fn(|_| DataChannelPayloadCacheLine { bytes: [0u8; 64] })
        .send()
        .unwrap();
}

/// A sample's user header and every payload byte survive the
/// grow-on-first-loan path intact.
#[test]
fn default_primed_channel_round_trips_a_header_and_an_oversized_payload() {
    let max_subscribers = 2;
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_service(
            "streamlib/test/sizing-default-roundtrip",
            max_subscribers,
            DeliveryProfile::ORDERED_DEPTH,
        )
        .unwrap();
    let publisher = service
        .create_publisher(DEFAULT_EXPECTED_PAYLOAD_BYTES)
        .unwrap();
    let subscriber = service
        .create_subscriber(DeliveryProfile::ORDERED_DEPTH)
        .unwrap();

    let payload: Vec<u8> = (0..OVERSIZED_PAYLOAD_BYTES)
        .map(|i| (i % 251) as u8)
        .collect();

    TestChannelSample::carrying(42, &payload).send_under(
        &publisher,
        TestChannelSample::carrying(42, &payload).user_header_numbered(0),
    );

    let (user_header, received) =
        receive_one_sample_within(&subscriber, Duration::from_secs(2)).expect("frame within 2s");
    assert_eq!(user_header.timestamp_ns, 42);
    assert_eq!(
        user_header.payload_byte_len as usize,
        OVERSIZED_PAYLOAD_BYTES
    );
    assert_eq!(
        received.as_slice(),
        payload.as_slice(),
        "payload bytes must survive the grow-on-first-loan path — a mismatch means \
         the subscriber dropped data past the prime"
    );
}

/// Every channel service opens under safe overflow, read off the service's own
/// static config. It is not a parameter and no profile derives it: a full
/// subscriber buffer evicts its oldest sample so the publisher's `send()`
/// never blocks.
///
/// Mentally-revert: drop the `.enable_safe_overflow(true)` line in
/// [`Iceoryx2Node::open_or_create_service`] and this test still passes —
/// iceoryx2's own default is `true`. Pass `false` there instead, which is
/// the only way the contract can actually break now, and it goes red.
#[test]
fn every_channel_service_opens_under_safe_overflow() {
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_service(
            "streamlib/test/sizing-safe-overflow",
            2,
            DeliveryProfile::ORDERED_DEPTH,
        )
        .unwrap();

    assert!(
        service.has_safe_overflow(),
        "a channel service must be created with safe overflow on — without it the \
         publisher back-pressures on a full subscriber buffer, which is the \
         producer-blocking this engine deleted"
    );
}

/// The sample layout `streamlib_ipc_types` mirrors matches the one iceoryx2
/// really lays out: a bag sized to the most a power-of-two ceiling admits fills
/// that ceiling's chunk to the byte, and one byte more takes the next power of
/// two.
///
/// The mirror is composed from iceoryx2's types rather than read off it, so
/// nothing else holds it to the real layout — an iceoryx2 upgrade that moves
/// the header or the alignment rule leaves every other sizing test green.
/// `__internal_available_payload_memory` is hidden upstream, and it is the one
/// view iceoryx2 gives of the chunk a loan really took.
///
/// Fail-without-fix: pad the user header by `user_header_alignment - 1` rather
/// than aligning it and the mirror claims 7 bytes of overhead the chunk does not
/// have, so the ceiling-sized bag leaves them empty.
#[test]
fn a_ceiling_sized_bag_fills_the_ceilings_chunk_and_one_byte_more_takes_the_next() {
    let chunk_ceiling_bytes = (DEFAULT_EXPECTED_PAYLOAD_BYTES * 4).next_power_of_two();
    let largest_admitted_frame_bytes =
        streamlib_ipc_types::largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes);

    for (frame_bytes, frame_bytes_its_chunk_must_hold) in [
        (largest_admitted_frame_bytes, largest_admitted_frame_bytes),
        (
            largest_admitted_frame_bytes + 1,
            largest_admitted_frame_bytes + chunk_ceiling_bytes,
        ),
    ] {
        let service = Iceoryx2Node::for_this_test_process()
            .open_or_create_service(
                &format!("streamlib/test/sizing-real-chunk-{frame_bytes}"),
                2,
                DeliveryProfile::ORDERED_DEPTH,
            )
            .unwrap();
        let publisher = service
            .create_publisher(DEFAULT_EXPECTED_PAYLOAD_BYTES)
            .unwrap();
        let sample = publisher
            .loan_slice_uninit(data_channel_payload_cache_lines_for(frame_bytes))
            .expect("a growing publisher loans past its prime");

        assert_eq!(
            sample.__internal_available_payload_memory(),
            frame_bytes_its_chunk_must_hold,
            "a {frame_bytes}-byte frame under a {chunk_ceiling_bytes}-byte ceiling"
        );
    }
}
