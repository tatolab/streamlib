// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration smoke tests for `@tatolab/jpeg::JpegDecoder`.
//!
//! Pipeline: `JpegBytesSource` (debug-utilities) → `JpegDecoder` (this
//! crate) → `VideoFrameCounter` (debug-utilities — sink that records
//! observations into process-global atomics so the test asserts on
//! frame count + first-frame dimensions + first-frame surface_id length
//! after `runtime.stop()`).
//! The source republishes the same JPEG bytes on a paced timer; the
//! decoder feeds each into `SimpleJpegDecoder::decode`; the counter
//! captures what arrives so the test locks the decoder actually emitted
//! VideoFrames (not just that start/stop bracketed cleanly).
//!
//! Two scenarios:
//!
//! 1. **Happy path** — a real 320×180 baseline JPEG runs through the
//!    pipeline. Asserts the counter saw ≥1 VideoFrame whose width /
//!    height match the fixture (320 × 180) and whose `surface_id` is
//!    non-empty (the decoder's internal TextureRing registered a slot).
//! 2. **Malformed bytes** — non-JPEG garbage flows through. The
//!    decoder surfaces a typed `Error::Runtime` per the error-path
//!    exit criterion; the runtime survives (logs WARN, processor
//!    stays alive); the counter sees zero VideoFrames because every
//!    decode failed.
//!
//! Both tests use `#[serial]` so the `VideoFrameCounter`'s
//! process-global atomics and the iceoryx2 service-name space don't
//! race against parallel test binaries.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serial_test::serial;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;
use streamlib_debug_utilities::video_frame_counter::{
    FIRST_HEIGHT, FIRST_SURFACE_ID_LEN, FIRST_WIDTH, FRAMES_OBSERVED,
};

// Force-link the package lib crates so their `inventory::submit!`
// factory registrations are pulled into the test binary's link line.
// Without this rustc's dead-code elimination drops the libs entirely
// and `add_processor` errors with `UnknownProcessorType`.
#[allow(unused_imports)]
use streamlib_debug_utilities::{
    JpegBytesSourceProcessor as _, VideoFrameCounterProcessor as _,
};
#[allow(unused_imports)]
use streamlib_jpeg::JpegDecoderProcessor as _;

const FIXTURE_WIDTH: u32 = 320;
const FIXTURE_HEIGHT: u32 = 180;
const FIXTURE_FRAME_COUNT: u32 = 5;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
#[serial]
fn valid_jpeg_runs_through_pipeline_cleanly() {
    streamlib_debug_utilities::video_frame_counter::reset();

    let runtime = Runner::new().expect("Runner::new");

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "debug-utilities", "JpegBytesSource", "1.0.0"),
            serde_json::json!({
                "file_path": fixture_path("test_320x180.jpg")
                    .to_str()
                    .expect("fixture path utf-8"),
                "fps": 30,
                "frame_count": FIXTURE_FRAME_COUNT,
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
            schema_ident!("tatolab", "debug-utilities", "VideoFrameCounter", "1.0.0"),
            serde_json::json!({}),
        ))
        .expect("add VideoFrameCounter");

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
        .expect("connect JpegDecoder → VideoFrameCounter");

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

    let frames = FRAMES_OBSERVED.load(Ordering::Relaxed);
    let width = FIRST_WIDTH.load(Ordering::Relaxed);
    let height = FIRST_HEIGHT.load(Ordering::Relaxed);
    let surface_id_len = FIRST_SURFACE_ID_LEN.load(Ordering::Relaxed);

    assert!(
        frames >= 1,
        "VideoFrameCounter saw {frames} frames; expected ≥1. \
         decoder never published — reverting `outputs.write` to a no-op \
         would falsify only this assertion."
    );
    assert_eq!(
        width, FIXTURE_WIDTH,
        "first VideoFrame width was {width}, expected {FIXTURE_WIDTH} (the fixture's actual width)"
    );
    assert_eq!(
        height, FIXTURE_HEIGHT,
        "first VideoFrame height was {height}, expected {FIXTURE_HEIGHT} (the fixture's actual height)"
    );
    assert!(
        surface_id_len > 0,
        "first VideoFrame surface_id was empty — decoder did not register \
         a TextureRing slot in the texture cache before emitting"
    );
}

#[test]
#[serial]
fn malformed_jpeg_bytes_do_not_crash_runtime() {
    streamlib_debug_utilities::video_frame_counter::reset();

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
            schema_ident!("tatolab", "debug-utilities", "VideoFrameCounter", "1.0.0"),
            serde_json::json!({}),
        ))
        .expect("add VideoFrameCounter");

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
        .expect("connect JpegDecoder → VideoFrameCounter");

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

    let frames = FRAMES_OBSERVED.load(Ordering::Relaxed);
    assert_eq!(
        frames, 0,
        "VideoFrameCounter saw {frames} frames; expected 0 (every input was \
         malformed and the decoder should have emitted nothing downstream)."
    );
}
