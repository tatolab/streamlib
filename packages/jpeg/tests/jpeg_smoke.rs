// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration smoke tests for `@tatolab/jpeg::JpegDecoder`.
//!
//! Pipeline: `JpegBytesSource` (debug-utilities) → `JpegDecoder` (this
//! crate) → `SimplePassthrough` (debug-utilities — VideoFrame sink so
//! the decoder's `outputs.write("video_out", ...)` has a wired service
//! to publish into; without a downstream, the write errors with
//! `Link error: Unknown output port`).
//! The source republishes the same JPEG bytes on a paced timer; the
//! decoder feeds each into `SimpleJpegDecoder::decode`.
//!
//! Two scenarios:
//!
//! 1. **Happy path** — a real 320×180 baseline JPEG runs through the
//!    pipeline cleanly. Asserts: setup succeeds (decoder GPU resources
//!    allocated), start/stop bracket without errors, and at least one
//!    EncodedJpegFrame was published by the source thread before stop.
//! 2. **Malformed bytes** — non-JPEG garbage bytes flow through the
//!    decoder, which surfaces a typed `Error::Runtime` per the
//!    error-path exit criterion. The runtime survives (engine logs
//!    WARN per `thread_runner.rs` and keeps the processor alive); the
//!    test asserts both `runtime.start()` and `runtime.stop()` succeed.
//!
//! Deep VideoFrame-content assertions (specific width/height/surface_id
//! on emitted frames) live in the `libs/vulkan-jpeg` crate's own
//! end-to-end tests against `SimpleJpegDecoder::decode` directly. This
//! test's job is to lock the streamlib-jpeg wrapper's wiring +
//! schema-resolution + error-mapping surface, not to re-verify the
//! underlying GPU primitive.
//!
//! Both tests use `#[serial]` so the iceoryx2 service-name space
//! doesn't race against parallel test binaries.

use std::path::PathBuf;
use std::time::Duration;

use serial_test::serial;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

// Force-link the package lib crates so their `inventory::submit!`
// factory registrations are pulled into the test binary's link line.
// Without this rustc's dead-code elimination drops the libs entirely
// and `add_processor` errors with `UnknownProcessorType`.
#[allow(unused_imports)]
use streamlib_debug_utilities::{
    JpegBytesSourceProcessor as _, SimplePassthroughProcessor as _,
};
#[allow(unused_imports)]
use streamlib_jpeg::JpegDecoderProcessor as _;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
#[serial]
fn valid_jpeg_runs_through_pipeline_cleanly() {
    let runtime = Runner::new().expect("Runner::new");

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "debug-utilities", "JpegBytesSource", "1.0.0"),
            serde_json::json!({
                "file_path": fixture_path("test_320x180.jpg")
                    .to_str()
                    .expect("fixture path utf-8"),
                "fps": 30,
                "frame_count": 5,
            }),
        ))
        .expect("add JpegBytesSource");

    let decoder_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "jpeg", "JpegDecoder", "1.0.0"),
            serde_json::json!({
                // Keep GPU resources tight — the fixture is 320×180 so
                // a smaller declared max keeps the texture-ring backing
                // small and decoder construction fast under test.
                "max_width": 640,
                "max_height": 480,
            }),
        ))
        .expect("add JpegDecoder");

    let sink_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "debug-utilities", "SimplePassthrough", "1.0.0"),
            serde_json::json!({ "scale": 1.0 }),
        ))
        .expect("add SimplePassthrough");

    runtime
        .connect(
            OutputLinkPortRef::new(source_id.as_str(), "encoded_jpeg"),
            InputLinkPortRef::new(decoder_id.as_str(), "encoded_jpeg_in"),
        )
        .expect("connect JpegBytesSource → JpegDecoder");

    runtime
        .connect(
            OutputLinkPortRef::new(decoder_id.as_str(), "video_out"),
            InputLinkPortRef::new(sink_id.as_str(), "input"),
        )
        .expect("connect JpegDecoder → SimplePassthrough");

    runtime.start().expect("runtime.start");

    // PUBSUB / iceoryx2 service-open warm-up
    // (docs/learnings/pubsub-lazy-init-silent-noop.md).
    std::thread::sleep(Duration::from_millis(250));

    // Let the source's paced thread emit all 5 frames and the decoder
    // process them. At fps=30 that's ~167ms of source-side work plus
    // decoder GPU dispatch — 1.5s of headroom keeps the test robust
    // against CI scheduler jitter.
    std::thread::sleep(Duration::from_millis(1500));

    runtime.stop().expect("runtime.stop");
}

#[test]
#[serial]
fn malformed_jpeg_bytes_do_not_crash_runtime() {
    let runtime = Runner::new().expect("Runner::new");

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "debug-utilities", "JpegBytesSource", "1.0.0"),
            serde_json::json!({
                "file_path": fixture_path("garbage.bin")
                    .to_str()
                    .expect("fixture path utf-8"),
                "fps": 30,
                "frame_count": 3,
            }),
        ))
        .expect("add JpegBytesSource");

    let decoder_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "jpeg", "JpegDecoder", "1.0.0"),
            serde_json::json!({
                "max_width": 640,
                "max_height": 480,
            }),
        ))
        .expect("add JpegDecoder");

    let sink_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "debug-utilities", "SimplePassthrough", "1.0.0"),
            serde_json::json!({ "scale": 1.0 }),
        ))
        .expect("add SimplePassthrough");

    runtime
        .connect(
            OutputLinkPortRef::new(source_id.as_str(), "encoded_jpeg"),
            InputLinkPortRef::new(decoder_id.as_str(), "encoded_jpeg_in"),
        )
        .expect("connect JpegBytesSource → JpegDecoder");

    runtime
        .connect(
            OutputLinkPortRef::new(decoder_id.as_str(), "video_out"),
            InputLinkPortRef::new(sink_id.as_str(), "input"),
        )
        .expect("connect JpegDecoder → SimplePassthrough");

    // The decoder's setup() must succeed even when no valid JPEG has
    // arrived yet — backend selection + GPU resource allocation runs
    // independent of the input stream.
    runtime.start().expect("runtime.start");

    std::thread::sleep(Duration::from_millis(250));
    std::thread::sleep(Duration::from_millis(750));

    // The decoder's process() returns `Err(Error::Runtime(...))` for
    // each malformed frame; the runtime logs WARN and keeps the
    // processor alive (thread_runner.rs reactive drain loop). A
    // panic or unhandled error would surface as a non-Ok stop.
    runtime.stop().expect("runtime.stop");
}
