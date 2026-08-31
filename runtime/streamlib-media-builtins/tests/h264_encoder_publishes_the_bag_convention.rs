// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Camera → `H264Encoder` in a real graph: frames leave as well-formed
//! Annex-B encoded-frame bags matching the wire convention.
//!
//! This is the only thing that runs the encoder's production seam — the
//! lazy session mint inside its escalate window, the per-frame resolve and
//! submit, and the bag publish. The unit tests either side exercise the
//! parts (dimension resolution, VUI translation, the wire codec) and none
//! of them notices if `process()` stops publishing what the convention
//! states.
//!
//! Rig-tier by construction, not by choice: `App::new()` brings up a real
//! `GpuContext`, the mint needs a Vulkan Video encode queue, and the source
//! needs a `/dev/video*` capture device (vivid acceptable) — so CI compiles
//! this binary and the rig runs it.

use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use streamlib::sdk::App;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ReactiveProcessor};
use streamlib_media_builtins::{
    CameraSource, EncodedVideoCodec, EncodedVideoFrame, H264Encoder, read_encoded_video_frame_bag,
    register_media_builtin_processor_types,
};

/// How long a processor may take to open the camera, bring up the GPU, and
/// reach Running.
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the graph runs once every processor is up. Longer than the
/// encoder's default 2 s IDR interval, so the collected stream must carry a
/// periodic sync point beyond the opening one.
const FRAMES_REALLY_FLOW_FOR: Duration = Duration::from_secs(5);

/// One collected bag: the decoded convention fields plus the frame-header
/// timestamp it rode with.
struct CollectedEncodedFrameBag {
    encoded_frame: EncodedVideoFrame,
    frame_header_timestamp_ns: i64,
}

/// What the collector saw, readable by the test after the graph stops.
#[derive(Default)]
struct EncodedChannelObservations {
    collected: Vec<CollectedEncodedFrameBag>,
    refusals: Vec<String>,
}

/// The collector instance is minted by the registry's factory, so the test
/// reaches its observations through binary-global state rather than a handle.
fn encoded_channel_observations() -> &'static Mutex<EncodedChannelObservations> {
    static OBSERVATIONS: OnceLock<Mutex<EncodedChannelObservations>> = OnceLock::new();
    OBSERVATIONS.get_or_init(Mutex::default)
}

/// Reads every bag off the encoded channel through the convention's own
/// refusal-naming reader — what any downstream consumer of an encoded
/// stream does.
#[streamlib::sdk::processor(
    description = "Collects encoded-frame bags for the test's assertions",
    execution = reactive,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "Encoded-frame bags under test"
    )
)]
pub struct EncodedFrameBagCollector;

impl ReactiveProcessor for EncodedFrameBagCollector::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some((bag_bytes, frame_header_timestamp_ns)) =
            self.inputs.read_raw("encoded_video")?
        {
            let mut observations = encoded_channel_observations().lock().unwrap();
            match read_encoded_video_frame_bag(&bag_bytes) {
                Ok(encoded_frame) => observations.collected.push(CollectedEncodedFrameBag {
                    encoded_frame,
                    frame_header_timestamp_ns,
                }),
                Err(refusal) => observations.refusals.push(refusal.to_string()),
            }
        }
        Ok(())
    }
}

/// Register the media built-ins and this file's collector, once for the
/// whole binary — the registry never overwrites a live registration.
fn ensure_every_processor_type_is_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        register_media_builtin_processor_types();
        PROCESSOR_REGISTRY.register::<EncodedFrameBagCollector::Processor>();
    });
}

/// `true` when `bytes` at `at` starts an Annex-B start code, returning the
/// offset of the NAL header byte behind it.
fn annex_b_nal_header_offset(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at..].starts_with(&[0, 0, 0, 1]) {
        Some(at + 4)
    } else if bytes[at..].starts_with(&[0, 0, 1]) {
        Some(at + 3)
    } else {
        None
    }
}

/// Every H.264 NAL unit type in an Annex-B access unit, in stream order.
fn h264_nal_unit_types(access_unit: &[u8]) -> Vec<u8> {
    let mut nal_unit_types = Vec::new();
    let mut cursor = 0;
    while cursor < access_unit.len() {
        match annex_b_nal_header_offset(access_unit, cursor) {
            Some(nal_header) if nal_header < access_unit.len() => {
                nal_unit_types.push(access_unit[nal_header] & 0x1F);
                cursor = nal_header + 1;
            }
            _ => cursor += 1,
        }
    }
    nal_unit_types
}

/// The ticket's rig demo, made reproducible: a camera's frames leave the
/// graph as tapped Annex-B bags whose fields match the convention, with
/// sync-point cadence at the GOP boundary and sane bitstream sizes.
///
/// Mental revert: delete the `publish_encoded_frame_bag` call from
/// `H264Encoder::process()`. Every processor still reaches Running and the
/// collector collects nothing — the emptiness assertion below is what
/// notices.
#[test]
fn camera_frames_leave_the_graph_as_annex_b_bags_matching_the_convention() {
    ensure_every_processor_type_is_registered();

    let app = App::new().expect("a runtime");
    let camera = app
        .add(
            CameraSource::Processor::processor_class_import_path(),
            serde_json::json!({}),
            Some("camera"),
        )
        .expect("the camera");
    let encoder = app
        .add(
            H264Encoder::Processor::processor_class_import_path(),
            serde_json::json!({}),
            Some("encoder"),
        )
        .expect("the encoder");
    let collector = app
        .add(
            EncodedFrameBagCollector::Processor::processor_class_import_path(),
            serde_json::json!({}),
            Some("collector"),
        )
        .expect("the collector");
    app.connect((&camera, "video"), (&encoder, "video"))
        .expect("the camera to the encoder");
    app.connect((&encoder, "encoded_video"), (&collector, "encoded_video"))
        .expect("the encoder to the collector");

    app.runner().start().expect("the graph starts");
    let readiness = app
        .runner()
        .wait_until_every_processor_is_running(READINESS_TIMEOUT);
    let held_up_from = Instant::now();
    while held_up_from.elapsed() < FRAMES_REALLY_FLOW_FOR {
        std::thread::sleep(Duration::from_millis(100));
    }
    let stop_outcome = app.runner().stop();

    readiness.expect(
        "every processor must reach Running — an encoder whose session mint \
         failed errors in its first process()",
    );
    stop_outcome.expect("the graph stops cleanly");

    let observations = encoded_channel_observations().lock().unwrap();
    assert_eq!(
        observations.refusals,
        Vec::<String>::new(),
        "every published bag must read back through the convention's own reader"
    );
    assert!(
        observations.collected.len() >= 10,
        "a {FRAMES_REALLY_FLOW_FOR:?} camera run must land a real stream of encoded bags, \
         got {}",
        observations.collected.len()
    );

    let first = &observations.collected[0].encoded_frame;
    assert!(
        first.is_sync_point && first.sequence_index == 0 && first.group_index == 0,
        "the stream opens at a sync point with the ordering pair at zero"
    );

    let mut previous: Option<&CollectedEncodedFrameBag> = None;
    let mut sync_point_count = 0usize;
    for collected in &observations.collected {
        let encoded_frame = &collected.encoded_frame;
        assert_eq!(encoded_frame.codec, EncodedVideoCodec::H264);
        assert!(
            encoded_frame.width > 0 && encoded_frame.height > 0,
            "the coded extent is the session's aligned extent, never zero"
        );
        assert!(
            !encoded_frame.annex_b_access_unit_bytes.is_empty()
                && annex_b_nal_header_offset(&encoded_frame.annex_b_access_unit_bytes, 0).is_some(),
            "every bitstream is a start-code-prefixed Annex-B access unit"
        );
        assert!(
            collected.frame_header_timestamp_ns > 0,
            "the timestamp rides the frame header"
        );
        if encoded_frame.is_sync_point {
            sync_point_count += 1;
            let nal_unit_types = h264_nal_unit_types(&encoded_frame.annex_b_access_unit_bytes);
            assert!(
                nal_unit_types.contains(&7) && nal_unit_types.contains(&8),
                "a sync point is a self-sufficient decode entry: SPS (7) and PPS (8) ride \
                 every IDR bag, got NAL types {nal_unit_types:?}"
            );
        }
        if let Some(previous) = previous {
            let previous_frame = &previous.encoded_frame;
            assert!(
                encoded_frame.sequence_index > previous_frame.sequence_index,
                "sequence_index is strictly monotonic in publication order"
            );
            assert!(
                collected.frame_header_timestamp_ns >= previous.frame_header_timestamp_ns,
                "frame-header timestamps never run backwards on an ordered channel"
            );
            // Group cadence is exact between adjacent frames — a drop-proof
            // spelling of "the group advances at sync points and only there".
            if encoded_frame.sequence_index == previous_frame.sequence_index + 1 {
                let expected_group =
                    previous_frame.group_index + u64::from(encoded_frame.is_sync_point);
                assert_eq!(
                    encoded_frame.group_index, expected_group,
                    "group_index advances exactly at sync points"
                );
            }
        }
        previous = Some(collected);
    }
    assert!(
        sync_point_count >= 2,
        "a {FRAMES_REALLY_FLOW_FOR:?} run outlasts the 2 s IDR interval, so the opening \
         sync point cannot be the only one, got {sync_point_count}"
    );

    let opening_access_unit_bytes = observations.collected[0]
        .encoded_frame
        .annex_b_access_unit_bytes
        .len();
    assert!(
        opening_access_unit_bytes > 1_000,
        "an IDR of a camera frame is KBs, got {opening_access_unit_bytes} bytes"
    );
}
