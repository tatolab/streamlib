// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration tests for iceoryx2 publisher payload size limits.
//!
//! Tests are grouped into three layers:
//!
//! A) Direct iceoryx2 — validates the slice loan limit behavior independently of
//!    the schema system.  These are the boundary tests that prove the problem and the fix.
//!
//! B) Schema parser — validates that `max_payload_bytes_for_port_spec` returns the values
//!    declared in the schema YAML metadata.
//!
//! C) Schema-driven publish/subscribe — creates a publisher via the production
//!    `Iceoryx2Node::create_publisher(max_payload_bytes_for_port_spec(...))` path and
//!    verifies that large payloads (which would fail with the old hardcoded 64 KB limit)
//!    are sent and received correctly.

use std::time::{Duration, Instant};

use super::{max_payload_bytes_for_port_spec, test_support};
use crate::iceoryx2::{FrameHeader, Iceoryx2Node, SchemaIdentWire, FRAME_HEADER_SIZE};
use iceoryx2::prelude::*;
use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};
use streamlib_processor_schema::PortSchemaSpec;

/// Build a `PortSchemaSpec::Specific` for a `@tatolab/core/<Type>@1.0.0` lookup.
fn core_spec(type_name: &str) -> PortSchemaSpec {
    PortSchemaSpec::Specific(SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("core").unwrap(),
        TypeName::new(type_name).unwrap(),
        SemVer::new(1, 0, 0),
    ))
}

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
// Verify max_payload_bytes_for_port_spec() returns the values declared in the
// schema YAML metadata section.
// =============================================================================

#[test]
fn test_schema_max_payload_bytes_audioframe() {
    test_support::register_core_wire_vocabulary();
    let bytes = max_payload_bytes_for_port_spec(&core_spec("AudioFrame")).unwrap();
    assert_eq!(bytes, 65536, "audioframe should declare 64 KB");
}

#[test]
fn test_schema_max_payload_bytes_videoframe() {
    test_support::register_core_wire_vocabulary();
    let bytes = max_payload_bytes_for_port_spec(&core_spec("VideoFrame")).unwrap();
    assert_eq!(
        bytes, 65536,
        "videoframe carries surface IDs only — 64 KB default is correct"
    );
}

/// A `Specific` spec for an unregistered schema must surface as a typed
/// configuration error naming the missing canonical id and pointing at
/// `add_module`. Silent fallback to the iceoryx2 default on registry
/// miss would defer the failure to first-frame `ExceedsMaxLoanSize`
/// instead of catching the "forgot `runtime.add_module(...)`" footgun
/// at wire time.
#[test]
fn test_schema_max_payload_bytes_unknown_schema_errors_with_add_module_hint() {
    let spec = PortSchemaSpec::Specific(SchemaIdent::new(
        Org::new("unknown").unwrap(),
        Package::new("does-not-exist-integration").unwrap(),
        TypeName::new("Nothing").unwrap(),
        SemVer::new(1, 0, 0),
    ));
    let err = max_payload_bytes_for_port_spec(&spec)
        .expect_err("registry miss must surface as Err, not as a silent default fallback");
    let msg = err.to_string();
    assert!(
        msg.contains("@unknown/does-not-exist-integration/Nothing"),
        "error must name the missing canonical id; got: {msg}"
    );
    assert!(
        msg.contains("add_module"),
        "error must point at runtime.add_module(...) as the fix; got: {msg}"
    );
}

#[test]
fn test_encodedvideoframe_larger_than_audioframe() {
    test_support::register_core_wire_vocabulary();
    let audio = max_payload_bytes_for_port_spec(&core_spec("AudioFrame")).unwrap();
    let video = max_payload_bytes_for_port_spec(&core_spec("EncodedVideoFrame")).unwrap();
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
//   Iceoryx2Node -> create_publisher(max_payload_bytes_for_port_spec(...))
// and verify that large payloads actually transit end-to-end.
// =============================================================================

/// Publisher sized from the audioframe schema (64 KB) rejects a 256 KB payload.
/// This mirrors the pre-fix failure mode for any connection carrying audioframes.
#[test]
fn test_audioframe_schema_publisher_rejects_256kb() {
    test_support::register_core_wire_vocabulary();
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service(
            "streamlib/test/schema-audio-reject",
            crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES,
            true,
        )
        .unwrap();

    let max_bytes = max_payload_bytes_for_port_spec(&core_spec("AudioFrame")).unwrap();
    let publisher = service.create_publisher(max_bytes).unwrap();

    // 256 KB exceeds the 64 KB audioframe limit.
    let result = publisher.loan_slice_uninit(256 * 1024);
    assert!(
        result.is_err(),
        "Expected 256 KB loan to fail on audioframe-sized publisher ({} bytes)",
        max_bytes
    );
}

/// Publisher sized from the encodedvideoframe schema accepts a 256 KB payload.
/// This is the GREEN-after-fix test: before the fix all publishers used ~64 KB.
#[test]
fn test_encodedvideoframe_schema_publisher_accepts_256kb() {
    test_support::register_core_wire_vocabulary();
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service(
            "streamlib/test/schema-video-ok",
            crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES,
            true,
        )
        .unwrap();

    let max_bytes = max_payload_bytes_for_port_spec(&core_spec("EncodedVideoFrame")).unwrap();
    let publisher = service.create_publisher(max_bytes).unwrap();

    let result = publisher.loan_slice_uninit(256 * 1024);
    assert!(
        result.is_ok(),
        "Expected 256 KB loan to succeed on encodedvideoframe-sized publisher ({} bytes), got: {:?}",
        max_bytes,
        result.err()
    );
}

/// Full publish/subscribe round-trip using the subprocess FFI wire format:
/// `[FrameHeader (204 bytes)][encoded video data (256 KB)]` — the exact layout
/// `sldn_output_write` / `slpn_output_write` build and `sldn_input_poll` /
/// `slpn_input_poll` parse when a Deno or Python subprocess carries an
/// encodedvideoframe. Before the per-input `max_payload_bytes` wiring fix,
/// the TS/Python read buffer was hard-coded to 32 KB and this payload would
/// have been silently truncated on receipt.
#[test]
fn test_frame_header_plus_256kb_roundtrip_through_slice_service() {
    test_support::register_core_wire_vocabulary();
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service(
            "streamlib/test/frame-header-256kb",
            crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES,
            true,
        )
        .unwrap();

    let data_size = 256 * 1024;
    let max_bytes = max_payload_bytes_for_port_spec(&core_spec("EncodedVideoFrame")).unwrap();
    // Publisher sized like the FFI layer: schema max + header.
    let publisher = service.create_publisher(max_bytes).unwrap();
    let subscriber = service.create_subscriber().unwrap();

    let mut data = vec![0u8; data_size];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }

    let total_len = FRAME_HEADER_SIZE + data_size;
    let mut frame = vec![0u8; total_len];
    let schema_ident =
        SchemaIdentWire::from_segments("tatolab", "core", "EncodedVideoFrame", 1, 0, 0)
            .expect("EncodedVideoFrame segments fit SchemaIdentWire bounds");
    FrameHeader::new("dest_port", schema_ident, 42, data_size as u32)
        .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
    frame[FRAME_HEADER_SIZE..].copy_from_slice(&data);

    let sample = publisher.loan_slice_uninit(total_len).expect(
        "loan_slice_uninit should succeed at FRAME_HEADER_SIZE + 256 KB on an \
         encodedvideoframe-sized publisher",
    );
    let sample = sample.write_from_slice(&frame);
    sample.send().expect("send should succeed");

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut received: Option<Vec<u8>> = None;
    while received.is_none() && Instant::now() < deadline {
        if let Ok(Some(sample)) = subscriber.receive() {
            received = Some(sample.payload().to_vec());
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let buf = received.expect("subscriber should have received the frame within 2s");
    assert_eq!(
        buf.len(),
        total_len,
        "received frame length should match header + data"
    );

    let header = FrameHeader::read_from_slice(&buf);
    assert_eq!(header.port(), "dest_port");
    let expected_ident = SchemaIdentWire::from_segments("tatolab", "core", "EncodedVideoFrame", 1, 0, 0)
        .unwrap();
    assert_eq!(header.schema(), &expected_ident);
    assert_eq!(header.timestamp_ns, 42);
    assert_eq!(header.len as usize, data_size);
    assert_eq!(
        &buf[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + data_size],
        data.as_slice(),
        "received payload bytes should match sent payload — truncation or \
         corruption would indicate the slice subscriber dropped data past the \
         old 32 KB limit"
    );
}

/// Full publish/subscribe round-trip: publisher sized from encodedvideoframe schema,
/// 256 KB payload written and received on the same service.
#[test]
fn test_encodedvideoframe_schema_publisher_subscriber_roundtrip_256kb() {
    test_support::register_core_wire_vocabulary();
    let node = Iceoryx2Node::new().unwrap();
    let service = node
        .open_or_create_service(
            "streamlib/test/schema-video-roundtrip",
            crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES,
            true,
        )
        .unwrap();

    let max_bytes = max_payload_bytes_for_port_spec(&core_spec("EncodedVideoFrame")).unwrap();
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
