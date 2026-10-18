// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Camera → `H264Encoder` in a real graph: frames leave as the media frames a
//! moq-dev player reads — a varint timestamp then an Annex-B access unit, a
//! keyframe opening a group — under a catalog entry naming the codec string
//! the stream's own SPS spells.
//!
//! This is the only thing that runs the encoder's production seam — the
//! lazy session mint inside its escalate window, the per-frame resolve and
//! submit, the catalog entry and the publish. The unit tests either side
//! exercise the parts and none of them notices if `process()` stops
//! publishing what a player reads.
//!
//! Rig-tier by construction, not by choice: starting the `App`'s stream
//! brings up a real `GpuContext`, the mint needs a Vulkan Video encode queue,
//! and the source needs a `/dev/video*` capture device (vivid acceptable) —
//! so CI compiles this binary and the rig runs it.

#[path = "../../streamlib-engine/tests/support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use streamlib::sdk::App;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::{CatalogEntry, CatalogSection};
use streamlib::sdk::error::Result;
use streamlib::sdk::hang_legacy_container_media_frame::read_hang_legacy_container_media_frame;
use streamlib::sdk::iceoryx2::MessageGroupBoundary;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ReactiveProcessor};
use streamlib_media_builtins::{CameraSource, H264Encoder, register_media_builtin_processor_types};

/// How long a processor may take to open the camera, bring up the GPU, and
/// reach Running.
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);

/// The run is bounded by observation, not wall time: the IDR cadence is
/// `keyframe_interval_seconds × the session's fps` counted in *encoded
/// frames*, and the session's fps is whatever the camera negotiated — so a
/// wall-clock run length proves nothing about how many sync points landed.
/// The graph runs until this many media frames and this many keyframes are
/// collected, under a generous cap.
const ENOUGH_COLLECTED_MEDIA_FRAMES: usize = 10;
const ENOUGH_SYNC_POINTS: usize = 2;
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(60);

/// One collected media frame and the envelope it rode in.
struct CollectedMediaFrame {
    varint_timestamp_microseconds: u64,
    annex_b_access_unit_bytes: Vec<u8>,
    group_boundary: MessageGroupBoundary,
    lost_since_last_read: u64,
    frame_header_timestamp_ns: i64,
    /// The encoder port's catalog entry as the collector read it beside
    /// this message.
    upstream_catalog_entry: Option<CatalogEntry>,
}

impl CollectedMediaFrame {
    fn starts_group(&self) -> bool {
        self.group_boundary == MessageGroupBoundary::StartsGroup
    }
}

/// What the collector saw, readable by the test after the graph stops.
#[derive(Default)]
struct EncodedChannelObservations {
    collected: Vec<CollectedMediaFrame>,
    refusals: Vec<String>,
}

/// The collector instance is minted by the registry's factory, so the test
/// reaches its observations through binary-global state rather than a handle.
fn encoded_channel_observations() -> &'static Mutex<EncodedChannelObservations> {
    static OBSERVATIONS: OnceLock<Mutex<EncodedChannelObservations>> = OnceLock::new();
    OBSERVATIONS.get_or_init(Mutex::default)
}

/// Reads every message off the encoded channel as a media frame, and the
/// upstream port's catalog entry beside it — what a player does.
#[streamlib::sdk::processor(
    description = "Collects media frames for the test's assertions",
    execution = reactive,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "Media frames under test"
    )
)]
pub struct MediaFrameCollector;

impl ReactiveProcessor for MediaFrameCollector::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("encoded_video")? {
            let upstream_catalog_entry = self
                .inputs
                .inbound_link_catalog_entry("encoded_video", None)?;
            let mut observations = encoded_channel_observations().lock().unwrap();
            match read_hang_legacy_container_media_frame(&message.payload_bytes) {
                Ok(media_frame) => observations.collected.push(CollectedMediaFrame {
                    varint_timestamp_microseconds: media_frame.timestamp_microseconds,
                    annex_b_access_unit_bytes: media_frame.codec_payload_bytes.to_vec(),
                    group_boundary: message.group_boundary,
                    lost_since_last_read: message.lost_since_last_read,
                    frame_header_timestamp_ns: message.timestamp_ns,
                    upstream_catalog_entry,
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
        PROCESSOR_REGISTRY.register::<MediaFrameCollector::Processor>();
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

/// A camera's frames leave the graph as media frames a player reads: the
/// varint carries the envelope's stamp in microseconds, a keyframe and only a
/// keyframe opens a group and carries its parameter sets, and the port's
/// catalog entry names the codec string the SPS spells.
///
/// Mental revert: publish every access unit with `StartsGroup`. The group
/// flag then no longer tracks the IDRs, and the keyframe assertion below is
/// what notices.
#[test]
fn camera_frames_leave_the_graph_as_media_frames_a_player_reads() {
    use streamlib::sdk::context::probe_video_device_backend;
    let this_machine_has_a_camera = probe_video_device_backend()
        .list_capture_devices()
        .is_ok_and(|cameras| !cameras.is_empty());
    if rig_only_capability_skip::skip_unless_this_machine_has(
        rig_only_capability_skip::RigOnlyCapability::Camera,
        this_machine_has_a_camera,
    ) || rig_only_capability_skip::skip_unless_this_machine_has(
        rig_only_capability_skip::RigOnlyCapability::VulkanVideo,
        streamlib::sdk::engine::host_rhi::HostVulkanDevice::new()
            .expect("a Vulkan device comes up on this machine")
            .supports_video_encode(),
    ) {
        return;
    }
    ensure_every_processor_type_is_registered();

    let project_directory = tempfile::tempdir().expect("a project directory");
    let app = App::new_in_project_directory(project_directory.path()).expect("a runtime");
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
            MediaFrameCollector::Processor::processor_class_import_path(),
            serde_json::json!({}),
            Some("collector"),
        )
        .expect("the collector");
    app.connect((&camera, "video"), (&encoder, "video"))
        .expect("the camera to the encoder");
    app.connect((&encoder, "encoded_video"), (&collector, "encoded_video"))
        .expect("the encoder to the collector");

    app.stream().start().expect("the graph starts");
    let readiness = app
        .stream()
        .wait_until_every_processor_is_running(READINESS_TIMEOUT);
    let collecting_since = Instant::now();
    let enough_was_collected = loop {
        {
            let observations = encoded_channel_observations().lock().unwrap();
            let keyframes_collected = observations
                .collected
                .iter()
                .filter(|collected| collected.starts_group())
                .count();
            if observations.collected.len() >= ENOUGH_COLLECTED_MEDIA_FRAMES
                && keyframes_collected >= ENOUGH_SYNC_POINTS
            {
                break true;
            }
        }
        if collecting_since.elapsed() > COLLECTION_TIMEOUT {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stop_outcome = app.stream().stop();

    // Readiness covers setup only — the camera opening its device and the
    // GPU bring-up. The lazy session mint happens in the first process()
    // and a failed mint leaves the processor Running; what notices it is
    // the collected stream staying empty below.
    readiness.expect("every processor must reach Running");
    stop_outcome.expect("the graph stops cleanly");

    let observations = encoded_channel_observations().lock().unwrap();
    assert_eq!(
        observations.refusals,
        Vec::<String>::new(),
        "every published message must read as a media frame"
    );
    assert!(
        enough_was_collected,
        "a camera run must land {ENOUGH_COLLECTED_MEDIA_FRAMES} media frames carrying \
         {ENOUGH_SYNC_POINTS} keyframes inside {COLLECTION_TIMEOUT:?}; got {} frames with {} \
         keyframes — an encoder whose session mint failed leaves this stream empty",
        observations.collected.len(),
        observations
            .collected
            .iter()
            .filter(|collected| collected.starts_group())
            .count()
    );
    assert!(
        observations.collected[0].starts_group(),
        "the stream opens at a keyframe, which opens a group"
    );

    let mut previous_frame_header_timestamp_ns = None;
    for collected in &observations.collected {
        assert_eq!(
            collected.varint_timestamp_microseconds,
            (collected.frame_header_timestamp_ns / 1_000) as u64,
            "the varint is the envelope's stamp in microseconds"
        );
        assert_eq!(
            collected.lost_since_last_read, 0,
            "a collector keeping up loses nothing"
        );
        assert!(
            annex_b_nal_header_offset(&collected.annex_b_access_unit_bytes, 0).is_some(),
            "every payload after the varint is a start-code-prefixed Annex-B access unit"
        );
        let nal_unit_types = h264_nal_unit_types(&collected.annex_b_access_unit_bytes);
        let is_an_idr = nal_unit_types.contains(&5);
        assert_eq!(
            collected.starts_group(),
            is_an_idr,
            "a message opens a group exactly when it carries an IDR, got NAL types \
             {nal_unit_types:?}"
        );
        if is_an_idr {
            assert!(
                nal_unit_types.contains(&7) && nal_unit_types.contains(&8),
                "a keyframe is a self-sufficient decode entry: SPS (7) and PPS (8) ride \
                 inline, got NAL types {nal_unit_types:?}"
            );
        }
        if let Some(previous) = previous_frame_header_timestamp_ns {
            assert!(
                collected.frame_header_timestamp_ns >= previous,
                "frame-header timestamps never run backwards on an ordered channel"
            );
        }
        previous_frame_header_timestamp_ns = Some(collected.frame_header_timestamp_ns);
    }

    let catalog_entry = observations.collected[0]
        .upstream_catalog_entry
        .as_ref()
        .expect("the entry is set before the first media frame it describes");
    assert_eq!(catalog_entry.section, CatalogSection::Video);
    let codec_string = catalog_entry.entry["codec"]
        .as_str()
        .expect("a video entry names its codec");
    assert!(
        codec_string.starts_with("avc1.64"),
        "the Vulkan Video arm encodes High, and the entry spells what the SPS says: \
         {codec_string}"
    );
    assert!(
        catalog_entry.entry["codedWidth"]
            .as_u64()
            .is_some_and(|width| width > 0)
            && catalog_entry.entry["codedHeight"]
                .as_u64()
                .is_some_and(|height| height > 0),
        "the entry carries the session's coded extent: {catalog_entry:?}"
    );
    assert!(
        catalog_entry.entry.get("description").is_none(),
        "an absent description is how a player knows the parameter sets ride inline"
    );

    let opening_access_unit_bytes = observations.collected[0].annex_b_access_unit_bytes.len();
    assert!(
        opening_access_unit_bytes > 1_000,
        "an IDR of a camera frame is KBs, got {opening_access_unit_bytes} bytes"
    );
}
