// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration tests for iceoryx2 publisher payload size limits.
//!
//! Tests are grouped into three layers:
//!
//! A) Direct iceoryx2 — validates the slice loan limit behavior independently of
//!    the schema system.  These are the boundary tests that prove the problem and the fix.
//!
//! B) Schema parser — validates that `max_payload_bytes_for_schema` returns the values
//!    declared in the schema YAML metadata.
//!
//! C) Schema-driven publish/subscribe — creates a publisher via the production
//!    `Iceoryx2Node::create_publisher(max_payload_bytes_for_schema(...))` path and
//!    verifies that large payloads (which would fail with the old hardcoded 64 KB limit)
//!    are sent and received correctly.

use std::time::{Duration, Instant};

use iceoryx2::prelude::*;
use streamlib::core::embedded_schemas::max_payload_bytes_for_schema;
use streamlib::iceoryx2::{Iceoryx2Node, MAX_PAYLOAD_SIZE};

// =============================================================================
// A) Direct iceoryx2 slice limit tests
//
// These tests bypass our wrapper and call iceoryx2 directly so the expected
// behavior of `loan_slice_uninit` is observable in isolation.
// =============================================================================

/// Small payload (1 KB) loans successfully from a publisher configured with a 64 KB limit.
/// GREEN today and after the fix.
#[test]
fn test_loan_1kb_succeeds_with_64kb_publisher_limit() {
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let service = node
        .service_builder(&"streamlib/test/size-1kb-ok".try_into().unwrap())
        .publish_subscribe::<[u8]>()
        .open_or_create()
        .unwrap();
    let publisher = service
        .publisher_builder()
        .initial_max_slice_len(64 * 1024)
        .create()
        .unwrap();

    let result = publisher.loan_slice_uninit(1024);
    assert!(
        result.is_ok(),
        "Expected 1 KB loan to succeed with 64 KB limit, got: {:?}",
        result.err()
    );
}

/// 256 KB payload loan FAILS when the publisher is configured with a 64 KB limit.
///
/// RED today — this documents the problem we are solving.
/// After the fix this test still passes: audioframe schema declares 64 KB, so a
/// connection carrying audio frames would still reject a 256 KB loan (correctly).
#[test]
fn test_loan_256kb_fails_with_64kb_publisher_limit() {
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let service = node
        .service_builder(&"streamlib/test/size-256kb-fail".try_into().unwrap())
        .publish_subscribe::<[u8]>()
        .open_or_create()
        .unwrap();
    let publisher = service
        .publisher_builder()
        .initial_max_slice_len(64 * 1024)
        .create()
        .unwrap();

    let result = publisher.loan_slice_uninit(256 * 1024);
    assert!(
        result.is_err(),
        "Expected 256 KB loan to fail with a 64 KB publisher limit — \
         the problem is not reproducible on this platform"
    );
}

/// 256 KB payload loans successfully when the publisher is sized at 512 KB.
/// Proves the fix mechanism works at the iceoryx2 level.
/// GREEN today and after the fix.
#[test]
fn test_loan_256kb_succeeds_with_512kb_publisher_limit() {
    let node = NodeBuilder::new().create::<ipc::Service>().unwrap();
    let service = node
        .service_builder(&"streamlib/test/size-256kb-ok".try_into().unwrap())
        .publish_subscribe::<[u8]>()
        .open_or_create()
        .unwrap();
    let publisher = service
        .publisher_builder()
        .initial_max_slice_len(512 * 1024)
        .create()
        .unwrap();

    let result = publisher.loan_slice_uninit(256 * 1024);
    assert!(
        result.is_ok(),
        "Expected 256 KB loan to succeed with 512 KB limit, got: {:?}",
        result.err()
    );
}

// =============================================================================
// B) Schema parser tests
//
// Verify max_payload_bytes_for_schema() returns the values declared in the
// schema YAML metadata section.
// =============================================================================

#[test]
fn test_schema_max_payload_bytes_audioframe() {
    let bytes = max_payload_bytes_for_schema("com.tatolab.audioframe");
    assert_eq!(bytes, 65536, "audioframe should declare 64 KB");
}

#[test]
fn test_schema_max_payload_bytes_audioframe_with_version_suffix() {
    // Schema names arrive from PROCESSOR_REGISTRY with a version like "@1.0.0" appended.
    let bytes = max_payload_bytes_for_schema("com.tatolab.audioframe@1.0.0");
    assert_eq!(
        bytes, 65536,
        "version suffix should be stripped before lookup"
    );
}

#[test]
fn test_schema_max_payload_bytes_encodedvideoframe() {
    let bytes = max_payload_bytes_for_schema("com.tatolab.encodedvideoframe");
    assert_eq!(
        bytes,
        512 * 1024,
        "encodedvideoframe should declare 512 KB for 1080p60 H.264/H.265 at 8 Mbps CBR"
    );
}

#[test]
fn test_schema_max_payload_bytes_videoframe() {
    let bytes = max_payload_bytes_for_schema("com.tatolab.videoframe");
    assert_eq!(
        bytes, 65536,
        "videoframe carries surface IDs only — 64 KB default is correct"
    );
}

#[test]
fn test_schema_max_payload_bytes_unknown_schema_returns_default() {
    let bytes = max_payload_bytes_for_schema("com.unknown.does.not.exist");
    assert_eq!(
        bytes,
        MAX_PAYLOAD_SIZE as usize,
        "unknown schema should fall back to MAX_PAYLOAD_SIZE"
    );
}

#[test]
fn test_encodedvideoframe_larger_than_audioframe() {
    let audio = max_payload_bytes_for_schema("com.tatolab.audioframe");
    let video = max_payload_bytes_for_schema("com.tatolab.encodedvideoframe");
    assert!(
        video > audio,
        "encodedvideoframe ({} bytes) should declare more capacity than audioframe ({} bytes)",
        video,
        audio
    );
}

// =============================================================================
// C) Schema-driven publish/subscribe
//
// These tests use the production path:
//   Iceoryx2Node -> create_publisher(max_payload_bytes_for_schema(...))
// and verify that large payloads actually transit end-to-end.
// =============================================================================

/// Publisher sized from the audioframe schema (64 KB) rejects a 256 KB payload.
/// This mirrors the pre-fix failure mode for any connection carrying audioframes.
#[test]
fn test_audioframe_schema_publisher_rejects_256kb() {
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service("streamlib/test/schema-audio-reject")
        .unwrap();

    let max_bytes = max_payload_bytes_for_schema("com.tatolab.audioframe");
    let publisher = service.create_publisher(max_bytes).unwrap();

    // 256 KB exceeds the 64 KB audioframe limit.
    let result = publisher.loan_slice_uninit(256 * 1024);
    assert!(
        result.is_err(),
        "Expected 256 KB loan to fail on audioframe-sized publisher ({} bytes)",
        max_bytes
    );
}

/// Publisher sized from the encodedvideoframe schema (4 MB) accepts a 256 KB payload.
/// This is the GREEN-after-fix test: before the fix all publishers used ~64 KB.
#[test]
fn test_encodedvideoframe_schema_publisher_accepts_256kb() {
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service("streamlib/test/schema-video-ok")
        .unwrap();

    let max_bytes = max_payload_bytes_for_schema("com.tatolab.encodedvideoframe");
    let publisher = service.create_publisher(max_bytes).unwrap();

    let result = publisher.loan_slice_uninit(256 * 1024);
    assert!(
        result.is_ok(),
        "Expected 256 KB loan to succeed on encodedvideoframe-sized publisher ({} bytes), got: {:?}",
        max_bytes,
        result.err()
    );
}

/// Full publish/subscribe round-trip: publisher sized from encodedvideoframe schema,
/// 256 KB payload written and received on the same service.
#[test]
fn test_encodedvideoframe_schema_publisher_subscriber_roundtrip_256kb() {
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service("streamlib/test/schema-video-roundtrip")
        .unwrap();

    let max_bytes = max_payload_bytes_for_schema("com.tatolab.encodedvideoframe");
    let publisher = service.create_publisher(max_bytes).unwrap();
    let subscriber = service.create_subscriber().unwrap();

    // Build 256 KB payload with a recognizable pattern.
    let payload_size = 256 * 1024;
    let mut payload = vec![0u8; payload_size];
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte = (i % 251) as u8; // prime modulus → non-trivial pattern
    }

    // Loan and send.
    let sample = publisher.loan_slice_uninit(payload_size).expect(
        "loan_slice_uninit should succeed — encodedvideoframe schema declares 4 MB",
    );
    let sample = sample.write_from_slice(&payload);
    sample.send().expect("send should succeed");

    // Poll for receipt with a timeout.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut received: Option<Vec<u8>> = None;
    while received.is_none() && Instant::now() < deadline {
        if let Ok(Some(sample)) = subscriber.receive() {
            received = Some(sample.payload().to_vec());
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let received = received.expect("subscriber should have received the 256 KB sample within 2s");
    assert_eq!(
        received.len(),
        payload_size,
        "received payload length should match sent length"
    );
    assert_eq!(
        received, payload,
        "received payload content should match sent content"
    );
}
