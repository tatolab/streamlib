// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end test for the full AGP vision pipeline:
//!
//!     test UDP socket → UdpSource(5600-style) → VadrVisionDepayloader
//!                     → JpegDecoder → VideoFrameCounter
//!
//! The test injects a real 320×180 baseline JPEG (the same fixture
//! `@tatolab/jpeg` uses for its own smoke tests) split into VADR-TS-002
//! §4.6 chunked datagrams via the public `header::encode` API, sends
//! each datagram via a loopback UDP socket, and asserts the
//! `VideoFrameCounter` saw ≥1 VideoFrame whose width / height / surface_id
//! match the fixture. This locks the cross-process pieces the unit tests
//! and state-machine integration tests don't: the iceoryx2 NetworkPacket →
//! EncodedJpegFrame → VideoFrame byte-shape mailbox path, the
//! `#[streamlib::sdk::processor]` macro wiring, the
//! `inventory::submit!` factory registration, and the actual UDP path
//! the deployed pipeline uses.
//!
//! Marked `#[serial]` so multiple test binaries don't race on UDP port
//! binding or the `VideoFrameCounter`'s process-global atomics.

use std::net::{SocketAddr, UdpSocket};
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
use streamlib_vadr_vision::header::{ChunkHeader, encode};

// Force-link the package lib crates so their `inventory::submit!`
// factory registrations are pulled into the test binary's link line.
// Without this rustc's dead-code elimination drops the libs entirely
// and `add_processor` errors with `UnknownProcessorType`.
#[allow(unused_imports)]
use streamlib_debug_utilities::VideoFrameCounterProcessor as _;
#[allow(unused_imports)]
use streamlib_jpeg::JpegDecoderProcessor as _;
#[allow(unused_imports)]
use streamlib_network::UdpSourceProcessor as _;
#[allow(unused_imports)]
use streamlib_vadr_vision::VadrVisionDepayloaderProcessor as _;

const FIXTURE_WIDTH: u32 = 320;
const FIXTURE_HEIGHT: u32 = 180;
/// Per-datagram VADR-TS-002 payload bytes. Well under typical 1500-MTU
/// (1500 − 20 IP − 8 UDP − 24 VADR header = 1448), but the fixture is
/// only ~7 KB so 1200 keeps chunk counts in the 6-range — representative
/// of the spec's 15–30 chunks at 640×360 without being so dense it
/// stresses the loopback kernel queue.
const CHUNK_PAYLOAD_BYTES: usize = 1200;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Bind an ephemeral UDP port, capture its address, drop the socket so
/// the processor can bind it. Same pattern as
/// `packages/network/tests/udp_loopback.rs`.
fn pick_free_udp_port() -> SocketAddr {
    let probe = UdpSocket::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

/// Encode one logical JPEG frame as a Vec of VADR-TS-002 §4.6 chunked
/// datagrams. Each chunk has the 24-byte header followed by up to
/// `CHUNK_PAYLOAD_BYTES` of JPEG bytes.
fn chunked_datagrams(frame_id: u32, sim_time_ns: u64, jpeg: &[u8]) -> Vec<Vec<u8>> {
    let pieces: Vec<&[u8]> = jpeg.chunks(CHUNK_PAYLOAD_BYTES).collect();
    let total_chunks = pieces.len() as u16;
    let jpeg_size = jpeg.len() as u32;
    pieces
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let header = ChunkHeader {
                frame_id,
                chunk_id: i as u16,
                total_chunks,
                jpeg_size,
                payload_size: c.len() as u32,
                sim_time_ns,
            };
            encode(&header, c)
        })
        .collect()
}

/// Drive a full pipeline run: bind UdpSource at `bind_addr`, wire it to
/// depayloader → decoder → counter, start, send `frames` chunked JPEGs
/// via a test socket, drain, stop. Returns nothing — assertions run on
/// the counter atomics afterward.
fn run_pipeline(bind_addr: SocketAddr, jpeg: &[u8], frames: u32) {
    let runtime = Runner::new().expect("Runner::new");

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSource", "1.0.0"),
            serde_json::json!({
                "bind_addr": bind_addr.to_string(),
                // 1 MiB SO_RCVBUF — well above the default ~200 KB Linux
                // ceiling without rmem_max tuning, comfortable for the
                // ~6-chunk × 3-frame burst this test sends.
                "recv_buffer_bytes": 1 << 20,
            }),
        ))
        .expect("add UdpSource");

    let depayloader_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "vadr-vision", "VadrVisionDepayloader", "1.0.0"),
            serde_json::json!({
                // 1 s reassembly timeout covers the test's loopback
                // latency with three orders of magnitude headroom; the
                // 200 ms default would also work but the higher value
                // documents that this test isn't probing timeout edges.
                "reassembly_timeout_ms": 1000,
                "max_pending_frames": 8,
                "warn_on_drop": true,
            }),
        ))
        .expect("add VadrVisionDepayloader");

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
            OutputLinkPortRef::new(source_id.as_str(), "packets"),
            InputLinkPortRef::new(depayloader_id.as_str(), "chunks_in"),
        )
        .expect("connect UdpSource → VadrVisionDepayloader");

    runtime
        .connect(
            OutputLinkPortRef::new(depayloader_id.as_str(), "jpeg_out"),
            InputLinkPortRef::new(decoder_id.as_str(), "encoded_jpeg_in"),
        )
        .expect("connect VadrVisionDepayloader → JpegDecoder");

    runtime
        .connect(
            OutputLinkPortRef::new(decoder_id.as_str(), "video_out"),
            InputLinkPortRef::new(sink_id.as_str(), "input"),
        )
        .expect("connect JpegDecoder → VideoFrameCounter");

    runtime.start().expect("runtime.start");

    // PUBSUB / iceoryx2 service-open warm-up
    // (docs/learnings/pubsub-lazy-init-silent-noop.md).
    std::thread::sleep(Duration::from_millis(500));

    let test_socket = UdpSocket::bind("127.0.0.1:0").expect("test socket bind");

    for frame_id in 0..frames {
        let dgrams = chunked_datagrams(frame_id, (frame_id as u64) * 33_333_333, jpeg);
        for d in &dgrams {
            test_socket
                .send_to(d, bind_addr)
                .expect("send chunk to UdpSource");
            // Pace inter-chunk so the kernel's loopback queue + iceoryx2
            // service hand-off keep up without dropping. Empirically
            // 2 ms is comfortable; 0 ms occasionally drops on a hot
            // machine.
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(50)); // inter-frame gap
    }

    // Let the depayloader emit, the decoder dispatch + GPU complete,
    // the counter observe. 2 s is comfortable headroom over the
    // ~150 ms steady-state latency.
    std::thread::sleep(Duration::from_millis(2000));

    runtime.stop().expect("runtime.stop");
}

/// Full E2E happy path. Sends a real 320×180 baseline JPEG split into
/// 6 VADR-TS-002 chunks, three frames in a row, then asserts the
/// `VideoFrameCounter` saw ≥1 frame whose dimensions match the fixture.
///
/// This is the test the issue body called "deferred until consumer
/// arrives". The two consumers (`UdpSource` upstream and `JpegDecoder`
/// downstream) shipped in #845 / #858, so the deferral is no longer
/// load-bearing — this E2E proves the depayloader correctly bridges
/// them.
///
/// What this test catches that the in-tree unit + state-machine
/// integration tests don't:
/// 1. iceoryx2 byte-shape mailbox round-trip for `NetworkPacket` →
///    depayloader and `EncodedJpegFrame` → decoder (the unit tests
///    drive `DepayloaderState::ingest` directly, never crossing
///    msgpack serialization).
/// 2. `#[streamlib::sdk::processor]` macro plumbing — `setup` runs
///    with `FullAccess`, `process` runs with `LimitedAccess`, ports
///    `chunks_in` / `jpeg_out` are wired to the right schema variants,
///    `inventory::submit!` factory is reachable from the link line.
/// 3. The real UDP path (`recv_loop` → `outputs.write` → iceoryx2 →
///    depayloader's `inputs.read`) — the udp_loopback test exercises
///    UdpSource + UdpSink only; this test exercises UdpSource +
///    a real consumer.
/// 4. The full vision pipeline lights up GPU resources: a non-empty
///    `surface_id` proves the decoder registered a TextureRing slot,
///    which requires every preceding stage (depayloader emitting
///    valid `EncodedJpegFrame`, iceoryx2 transporting it, decoder
///    binding GPU memory) to have worked.
#[test]
#[serial]
fn vadr_chunks_reassemble_and_decode_to_video_frame() {
    streamlib_debug_utilities::video_frame_counter::reset();

    let bind_addr = pick_free_udp_port();
    let jpeg = std::fs::read(fixture_path("test_320x180.jpg")).expect("read JPEG fixture");

    // Pre-flight: the fixture chunks into multiple datagrams under our
    // declared chunk-size cap. The whole point of this test is to
    // exercise the multi-chunk reassembly path through real UDP — a
    // fixture small enough to fit in one chunk would silently bypass
    // it. Lock the multi-chunk shape so future fixture changes can't
    // demote this to a single-chunk happy-path-only test.
    assert!(
        jpeg.len() > CHUNK_PAYLOAD_BYTES,
        "fixture is {} bytes; needs > {} bytes to exercise multi-chunk reassembly",
        jpeg.len(),
        CHUNK_PAYLOAD_BYTES
    );

    run_pipeline(bind_addr, &jpeg, 3);

    let frames = FRAMES_OBSERVED.load(Ordering::Relaxed);
    let width = FIRST_WIDTH.load(Ordering::Relaxed);
    let height = FIRST_HEIGHT.load(Ordering::Relaxed);
    let surface_id_len = FIRST_SURFACE_ID_LEN.load(Ordering::Relaxed);

    assert!(
        frames >= 1,
        "VideoFrameCounter saw {frames} frames; expected ≥1. \
         Pipeline broken: either UdpSource didn't receive the test \
         datagrams, depayloader didn't reassemble, decoder didn't \
         decode, or the iceoryx2 transport silently dropped frames."
    );
    assert_eq!(
        width, FIXTURE_WIDTH,
        "first VideoFrame width was {width}, expected {FIXTURE_WIDTH}"
    );
    assert_eq!(
        height, FIXTURE_HEIGHT,
        "first VideoFrame height was {height}, expected {FIXTURE_HEIGHT}"
    );
    assert!(
        surface_id_len > 0,
        "first VideoFrame surface_id was empty — decoder did not register \
         a TextureRing slot, meaning either the JPEG bytes were corrupted \
         in flight (reassembly bug) or the decoder swallowed the input."
    );
}

/// Malformed chunks must not crash the runtime. Sends a mix of
/// too-short datagrams (header parse fails) and garbage bytes, then
/// asserts: zero frames decoded (correct — every input was malformed),
/// runtime.stop() succeeds (correct — depayloader survived).
///
/// Locks the issue body's "drop, log, advance" requirement at the
/// transport boundary, not just inside the state machine.
#[test]
#[serial]
fn malformed_vadr_datagrams_do_not_crash_runtime() {
    streamlib_debug_utilities::video_frame_counter::reset();

    let bind_addr = pick_free_udp_port();
    let runtime = Runner::new().expect("Runner::new");

    let source_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "network", "UdpSource", "1.0.0"),
            serde_json::json!({"bind_addr": bind_addr.to_string()}),
        ))
        .expect("add UdpSource");

    let depayloader_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "vadr-vision", "VadrVisionDepayloader", "1.0.0"),
            serde_json::json!({}),
        ))
        .expect("add VadrVisionDepayloader");

    let decoder_id = runtime
        .add_processor(ProcessorSpec::new(
            schema_ident!("tatolab", "jpeg", "JpegDecoder", "1.0.0"),
            serde_json::json!({"max_width": 640, "max_height": 480}),
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
            OutputLinkPortRef::new(source_id.as_str(), "packets"),
            InputLinkPortRef::new(depayloader_id.as_str(), "chunks_in"),
        )
        .expect("connect UdpSource → VadrVisionDepayloader");
    runtime
        .connect(
            OutputLinkPortRef::new(depayloader_id.as_str(), "jpeg_out"),
            InputLinkPortRef::new(decoder_id.as_str(), "encoded_jpeg_in"),
        )
        .expect("connect VadrVisionDepayloader → JpegDecoder");
    runtime
        .connect(
            OutputLinkPortRef::new(decoder_id.as_str(), "video_out"),
            InputLinkPortRef::new(sink_id.as_str(), "input"),
        )
        .expect("connect JpegDecoder → VideoFrameCounter");

    runtime.start().expect("runtime.start");
    std::thread::sleep(Duration::from_millis(500));

    let test_socket = UdpSocket::bind("127.0.0.1:0").expect("test socket bind");
    // 1) Datagram too short to carry a 24-byte header.
    test_socket
        .send_to(&[0u8; 8], bind_addr)
        .expect("send short");
    // 2) Garbage that's long enough for parse() to read 24 bytes but
    //    fails the `total_chunks == 0` / payload-size check.
    test_socket
        .send_to(&[0u8; 40], bind_addr)
        .expect("send garbage");
    // 3) Header that parses but claims more payload than the datagram
    //    carries (24-byte header + 0 trailing bytes but
    //    payload_size = 50).
    let bad_header = ChunkHeader {
        frame_id: 1,
        chunk_id: 0,
        total_chunks: 2,
        jpeg_size: 100,
        payload_size: 50,
        sim_time_ns: 0,
    };
    let mut bad_bytes = encode(&bad_header, &[]);
    bad_bytes.extend_from_slice(&[0u8; 10]); // declared 50, actual 10
    test_socket
        .send_to(&bad_bytes, bind_addr)
        .expect("send size-mismatch");

    std::thread::sleep(Duration::from_millis(750));
    runtime.stop().expect("runtime.stop");

    let frames = FRAMES_OBSERVED.load(Ordering::Relaxed);
    assert_eq!(
        frames, 0,
        "VideoFrameCounter saw {frames} frames; expected 0. Every datagram \
         was malformed; if any decoded, the depayloader is forwarding bad \
         bytes downstream."
    );
}
