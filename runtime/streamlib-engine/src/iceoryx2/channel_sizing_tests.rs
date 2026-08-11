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

use crate::iceoryx2::{
    DEFAULT_EXPECTED_PAYLOAD_BYTES, DEFAULT_MAX_QUEUED_MESSAGES, FRAME_HEADER_SIZE, FrameHeader,
    Iceoryx2Node,
};
use iceoryx2::prelude::*;

/// Derived from the prime rather than written as a literal, so raising
/// [`DEFAULT_EXPECTED_PAYLOAD_BYTES`] past it can never quietly turn the growth
/// tests below into no-ops.
const OVERSIZED_PAYLOAD_BYTES: usize = DEFAULT_EXPECTED_PAYLOAD_BYTES * 4;

/// Poll a subscriber until it yields one sample or the deadline passes. A
/// transport error is a failure in its own right, never a timeout.
fn receive_one_sample_within(
    subscriber: &iceoryx2::port::subscriber::Subscriber<ipc::Service, [u8], ()>,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match subscriber.receive() {
            Ok(Some(sample)) => return Some(sample.payload().to_vec()),
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
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let service = node
        .service_builder(&"streamlib/test/sizing-no-growth".try_into().unwrap())
        .publish_subscribe::<[u8]>()
        .open_or_create()
        .unwrap();
    let publisher = service
        .publisher_builder()
        .initial_max_slice_len(DEFAULT_EXPECTED_PAYLOAD_BYTES)
        .create()
        .unwrap();

    assert!(
        publisher
            .loan_slice_uninit(OVERSIZED_PAYLOAD_BYTES)
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
    let enable_safe_overflow = true;
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service(
            "streamlib/test/sizing-default-grows",
            max_subscribers,
            DEFAULT_MAX_QUEUED_MESSAGES,
            enable_safe_overflow,
        )
        .unwrap();
    let publisher = service
        .create_publisher(DEFAULT_EXPECTED_PAYLOAD_BYTES)
        .unwrap();

    let sample = publisher.loan_slice_uninit(OVERSIZED_PAYLOAD_BYTES).expect(
        "a publisher primed at DEFAULT_EXPECTED_PAYLOAD_BYTES must grow to loan an \
         oversized slice rather than rejecting it",
    );
    sample
        .write_from_slice(&vec![0u8; OVERSIZED_PAYLOAD_BYTES])
        .send()
        .unwrap();
}

/// The full wire format a helper process speaks — `[FrameHeader][payload]` —
/// survives the grow-on-first-loan path with every header field and every
/// payload byte intact.
#[test]
fn default_primed_channel_round_trips_a_header_and_an_oversized_payload() {
    let max_subscribers = 2;
    let enable_safe_overflow = true;
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service(
            "streamlib/test/sizing-default-roundtrip",
            max_subscribers,
            DEFAULT_MAX_QUEUED_MESSAGES,
            enable_safe_overflow,
        )
        .unwrap();
    let publisher = service
        .create_publisher(DEFAULT_EXPECTED_PAYLOAD_BYTES)
        .unwrap();
    let subscriber = service.create_subscriber().unwrap();

    let payload: Vec<u8> = (0..OVERSIZED_PAYLOAD_BYTES)
        .map(|i| (i % 251) as u8)
        .collect();

    let total_len = FRAME_HEADER_SIZE + OVERSIZED_PAYLOAD_BYTES;
    let mut frame = vec![0u8; total_len];
    FrameHeader::new("dest_port", 42, OVERSIZED_PAYLOAD_BYTES as u32)
        .expect("dest_port fits PortKey bounds")
        .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
    frame[FRAME_HEADER_SIZE..].copy_from_slice(&payload);

    let sample = publisher
        .loan_slice_uninit(total_len)
        .expect("header + oversized payload must loan from a default-primed publisher");
    sample.write_from_slice(&frame).send().expect("send");

    let received =
        receive_one_sample_within(&subscriber, Duration::from_secs(2)).expect("frame within 2s");
    assert_eq!(
        received.len(),
        total_len,
        "received frame length must match header + payload"
    );

    let header = FrameHeader::read_from_slice(&received);
    assert_eq!(header.port(), "dest_port");
    assert_eq!(header.timestamp_ns, 42);
    assert_eq!(header.len as usize, OVERSIZED_PAYLOAD_BYTES);
    assert_eq!(
        &received[FRAME_HEADER_SIZE..],
        payload.as_slice(),
        "payload bytes must survive the grow-on-first-loan path — a mismatch means \
         the subscriber dropped data past the prime"
    );
}
