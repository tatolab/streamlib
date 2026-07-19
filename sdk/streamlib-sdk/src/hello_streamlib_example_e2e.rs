// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Headless E2E for the `hello-streamlib` example's inline processor.
//!
//! This test `#[path]`-includes the example's *actual* source file
//! (`examples/hello-streamlib/src/hello_forward.rs`) and drives a fixture frame
//! through it: a fixture-populated input mailbox on one side, a real iceoryx2
//! publish/subscribe edge capturing the forwarded frame on the other. It runs
//! under `cargo test --lib -p streamlib`, so it is the CI gate for the
//! zero-ceremony path — no GPU, no camera, no display (those are the
//! `/verify-live` scenario).
//!
//! Two independent mechanisms keep the zero-ceremony budget honest, together:
//!
//! - The `#[path]` source-include compiles `hello_forward.rs` inside this
//!   crate's own test build, so any inline dependence the example grows on a
//!   generated or codegen module (a `_generated_` import, a schema-derived
//!   type) fails to compile here and breaks the build.
//! - [`example_dir_has_no_ceremony_files`] walks the example directory and
//!   asserts it contains no `build.rs`, no `streamlib.yaml`, no `schemas/`
//!   dir, and no `_generated_` dir — because the source-include alone never
//!   compiles the standalone example crate, a reintroduced ceremony file that
//!   `hello_forward.rs` doesn't reference would otherwise be invisible to CI.

// The example authors `#[streamlib::sdk::processor(...)]` against the public
// facade; inside this crate `streamlib` resolves to self via the crate's
// `extern crate self as streamlib`, so the identical source compiles here and
// in the standalone example.
#[path = "../../../examples/hello-streamlib/src/hello_forward.rs"]
mod hello_forward_example;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use iceoryx2::prelude::*;

use crate::sdk::iceoryx2::{
    FRAME_HEADER_SIZE, FrameHeader, InputMailboxes, InputMailboxesInner, OutputWriter,
    OutputWriterInner, ReadMode, SchemaIdentWire,
};
use crate::sdk::processors::{EmptyConfig, GeneratedProcessor};

use hello_forward_example::HelloForward;

/// A per-test-unique iceoryx2 service name so parallel test binaries never
/// collide on the machine-global `/dev/shm` namespace.
fn unique_service_name(tag: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "test/hello-streamlib/{}/{}/{}",
        tag,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// The single `@tatolab/core/VideoFrame` wire identity for this edge. Both the
/// input framing and the output connection must agree on it or the edge
/// silently mismatches, so it lives in exactly one place.
fn video_frame_schema() -> SchemaIdentWire {
    SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0)
        .expect("VideoFrame identity fits the wire capacity")
}

/// Frame the given opaque payload for `port` exactly as the runtime does, so it
/// can be routed straight into an [`InputMailboxesInner`] without an iceoryx2
/// subscriber (the in-memory injection path the runtime's own tests use).
fn framed_payload(port: &str, payload: &[u8], timestamp_ns: i64) -> Vec<u8> {
    let schema = video_frame_schema();
    let mut buf = vec![0u8; FRAME_HEADER_SIZE + payload.len()];
    FrameHeader::new(port, schema, timestamp_ns, payload.len() as u32)
        .expect("port name fits the wire capacity")
        .write_to_slice(&mut buf[..FRAME_HEADER_SIZE]);
    buf[FRAME_HEADER_SIZE..].copy_from_slice(payload);
    buf
}

/// A fixture frame injected on `video_in` traverses the inline `HelloForward`
/// processor byte-for-byte onto `video_out` and lands in the downstream sink.
///
/// Mentally revert the `self.outputs.write_raw(...)` line in the example's
/// `forward_pending` and the sink never receives the frame (this test's
/// capture assertion fails); revert the counter increment and the
/// `frames_forwarded()` assertion fails. Either way the test exercises the
/// forward, not just its scaffolding.
#[test]
fn fixture_frame_traverses_the_inline_forward_processor() {
    let payload: Vec<u8> = b"hello-streamlib-fixture-frame".to_vec();
    let timestamp_ns: i64 = 4_242;

    // Source: a fixture-populated input mailbox. `route` pushes the framed
    // bytes straight into the in-memory port mailbox — no iceoryx2 needed on
    // the injection side.
    let source_inputs = Arc::new(InputMailboxesInner::new());
    source_inputs.add_port("video_in", 8, ReadMode::ReadNextInOrder);
    assert!(
        source_inputs.route(framed_payload("video_in", &payload, timestamp_ns)),
        "the fixture frame must route to the source's video_in port"
    );

    // Output edge: one real iceoryx2 publish/subscribe pair so the frame the
    // processor writes is genuinely published and can be captured downstream.
    let node = NodeBuilder::new()
        .create::<ipc::Service>()
        .expect("iceoryx2 node");
    let pubsub = node
        .service_builder(&ServiceName::new(&unique_service_name("pubsub")).unwrap())
        .publish_subscribe::<[u8]>()
        .max_publishers(1)
        .open_or_create()
        .expect("pubsub service");
    let publisher = pubsub
        .publisher_builder()
        .initial_max_slice_len(4096)
        .create()
        .expect("publisher");
    let subscriber = pubsub.subscriber_builder().create().expect("subscriber");
    let event = node
        .service_builder(&ServiceName::new(&unique_service_name("event")).unwrap())
        .event()
        .max_notifiers(1)
        .max_listeners(1)
        .open_or_create()
        .expect("event service");
    let notifier = event.notifier_builder().create().expect("notifier");

    let output_writer_inner = Arc::new(OutputWriterInner::new());
    output_writer_inner.add_connection(
        "video_out",
        video_frame_schema(),
        "video_in",
        publisher,
        notifier,
    );

    // Sink: an input mailbox subscribed to the same edge. `read_raw` drains the
    // subscriber and hands back the forwarded payload.
    let sink_inputs = Arc::new(InputMailboxesInner::new());
    sink_inputs.add_port("video_in", 8, ReadMode::ReadNextInOrder);
    sink_inputs.set_subscriber(subscriber);

    // Build the example processor and wire in the real host-side inners.
    let mut processor = HelloForward::Processor::from_config(EmptyConfig)
        .expect("HelloForward has an EmptyConfig and constructs cleanly");
    processor.inputs = InputMailboxes::from_inner_arc(source_inputs);
    processor.outputs = OutputWriter::from_inner_arc(output_writer_inner);

    assert_eq!(processor.frames_forwarded(), 0);

    // Drive the forward directly (no live runtime context required).
    let forwarded = processor
        .forward_pending()
        .expect("forwarding a pending fixture frame must not error");
    assert!(
        forwarded,
        "a fixture frame was pending and must be forwarded"
    );
    assert_eq!(
        processor.frames_forwarded(),
        1,
        "the processor must count the forwarded frame"
    );

    // Capture the forwarded frame on the sink. Same-process delivery is prompt;
    // the bounded poll only guards against a transient scheduling hiccup.
    let deadline = Instant::now() + Duration::from_secs(2);
    let captured = loop {
        if let Some(frame) = sink_inputs
            .read_raw("video_in")
            .expect("sink read must not error")
        {
            break frame;
        }
        assert!(
            Instant::now() < deadline,
            "the forwarded frame never arrived at the downstream sink"
        );
    };
    let (captured_bytes, captured_timestamp) = captured;
    assert_eq!(
        captured_bytes, payload,
        "the sink must receive the forwarded frame byte-for-byte"
    );
    assert_eq!(
        captured_timestamp, timestamp_ns,
        "the forward must preserve the frame timestamp"
    );

    // Nothing else is pending — a second drive forwards nothing and leaves the
    // count unchanged.
    let forwarded_again = processor
        .forward_pending()
        .expect("a no-data drive must not error");
    assert!(
        !forwarded_again,
        "no frame is pending after the single fixture frame is consumed"
    );
    assert_eq!(processor.frames_forwarded(), 1);
}

/// The example directory carries no ceremony: acceptance criterion #4 ("the
/// file list is the ceremony budget") is machine-checked here rather than left
/// to review. The `#[path]` source-include never compiles the standalone
/// example crate, so a reintroduced `build.rs` / `streamlib.yaml` / `schemas/`
/// / `_generated_` that `hello_forward.rs` doesn't reference would otherwise
/// slip past CI.
#[test]
fn example_dir_has_no_ceremony_files() {
    let example_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello-streamlib");
    assert!(
        example_dir.is_dir(),
        "the hello-streamlib example directory must exist at {}",
        example_dir.display()
    );

    let forbidden_files = ["build.rs", "streamlib.yaml"];
    let forbidden_dirs = ["schemas", "_generated_"];
    // Gitignored build/link artifacts (the `node_modules`-equivalents) carry
    // their own committed ceremony from the packages they mirror; the budget
    // this test enforces is the example's own committed tree.
    let skip_dirs = ["streamlib_modules", "target"];

    let mut offenders: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![example_dir.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("the example directory must be readable") {
            let entry = entry.expect("a directory entry must be readable");
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = entry.file_type().expect("entry file type must be readable");
            if file_type.is_dir() {
                if forbidden_dirs.iter().any(|forbidden| *forbidden == name) {
                    offenders.push(path.clone());
                }
                if !skip_dirs.iter().any(|skip| *skip == name) {
                    stack.push(path);
                }
            } else if forbidden_files.iter().any(|forbidden| *forbidden == name) {
                offenders.push(path);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the hello-streamlib example must stay zero-ceremony, but these files reintroduce it: {offenders:?}"
    );
}
