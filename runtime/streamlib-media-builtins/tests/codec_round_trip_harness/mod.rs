// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(any(target_os = "linux", target_os = "macos"))]

//! `TestPatternSource` → encoder → decoder in a real graph: every media frame
//! the decoder is handed comes back out as a published video surface, and a
//! loss on the encoded link is survived by discarding to the next group.
//!
//! One harness, one arm per codec — the H.264 and H.265 built-in pairs share
//! their whole body, so what is worth running twice is the round trip, not
//! the assertions written twice.
//!
//! The failure the round trip guards against showed as
//! `frames_encoded ≈ 50, frames_decoded = 0` — the decoder's first received
//! frame was the encoder's second, the IDR never arrived, and the periodic
//! IDRs were not recognised. The assertions below are exactly that shape
//! read forwards: the decoded stream is not empty, it tracks the encoded one
//! to within what the link can hold, and it runs past the stream's second
//! keyframe, which is the "periodic IDRs recognised" half.
//!
//! The pattern is 1920x1080 on purpose, for both arms: 1080 is a multiple of
//! neither the H.264 macroblock nor the H.265 CTU, so the coded height is
//! 1088 either way and only the SPS's conformance window brings it back. A
//! decoder publishing the coded extent instead of the windowed one shows up
//! as 1088 in the extent assertion below — which is the whole of the H.265
//! CTU-crop contract, asserted rather than described.
//!
//! Rig-tier by construction, not by choice: starting the `App`'s stream
//! brings up a real `GpuContext` and the sessions need a hardware encoder
//! *and* decoder — Vulkan Video queues on Linux, VideoToolbox on macOS — so
//! CI compiles these binaries and the rig runs them. The source is the test
//! pattern rather than a camera on purpose: it needs no `/dev/video*` device,
//! so what this asserts is the codec round trip and nothing else.

// Each arm binary runs one of the two entry points.
#![allow(dead_code)]

#[path = "../../../streamlib-engine/tests/support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;
use std::collections::HashSet;
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use streamlib::sdk::App;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::error::Result;
use streamlib::sdk::hang_legacy_container_media_frame::read_hang_legacy_container_media_frame;
use streamlib::sdk::iceoryx2::MessageGroupBoundary;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ReactiveProcessor};
use streamlib_media_builtins::{
    TestPatternSource, VideoFrame, register_media_builtin_processor_types,
};

/// How long a processor may take to bring up the GPU and reach Running.
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);

/// The run is bounded by observation, not wall time. It ends once the
/// decoded stream has run past the encoded stream's second keyframe, under a
/// generous cap — an encoder's real cadence is whatever the session
/// negotiated, so no wall-clock run length proves anything about it.
const ENOUGH_KEYFRAMES_ENCODED: usize = 2;
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(90);

/// The encoder's keyframe cadence for this run, short so the second IDR
/// lands within a few seconds of the test pattern's 30 fps.
const KEYFRAME_INTERVAL_SECONDS: u32 = 1;

/// The pattern's extent. 1080 is aligned to neither codec's block size, so
/// the coded height is 1088 for both and each decoder must publish the SPS's
/// windowed 1080.
const PATTERN_WIDTH: u32 = 1920;
const PATTERN_HEIGHT: u32 = 1080;

/// Media frames the burst forwarder passes through before it starts holding,
/// so the decoder has entered the stream when the burst lands.
const MEDIA_FRAMES_FORWARDED_BEFORE_THE_BURST: usize = 45;

/// Media frames held back and released at once — far past the 16 an ordered
/// link's ring and mailbox each hold, so the ring overwrites the oldest of
/// them and the decoder's next read reports the loss.
const MEDIA_FRAMES_HELD_FOR_THE_BURST: usize = 60;

/// Media frames forwarded after the burst before the run may end: two
/// keyframe intervals at the pattern's 30 fps, so the decoder has re-entered
/// at a keyframe and decoded a whole group after it.
const MEDIA_FRAMES_FORWARDED_AFTER_THE_BURST: usize = 70;

/// The codec pair one arm binary runs the harness through. Named by the arm
/// itself rather than chosen from an enum here, so neither binary carries a
/// branch for the codec it does not run.
pub struct CodecRoundTripArm {
    /// The codec's name, so a failed assertion says which arm failed.
    pub codec_name: &'static str,
    pub encoder_class_import_path: ProcessorClassImportPath,
    pub decoder_class_import_path: ProcessorClassImportPath,
}

/// One media frame as the tap beside the decoder saw it.
struct TalliedMediaFrame {
    starts_group: bool,
    frame_header_timestamp_ns: i64,
}

/// What the encoded channel's tap saw, in publication order.
#[derive(Default)]
struct EncodedChannelTally {
    media_frames: Vec<TalliedMediaFrame>,
    refusals: Vec<String>,
    messages_lost_ahead_of_the_tap: u64,
}

impl EncodedChannelTally {
    /// Where in publication order each keyframe sits.
    fn keyframe_positions(&self) -> Vec<usize> {
        self.media_frames
            .iter()
            .enumerate()
            .filter(|(_, media_frame)| media_frame.starts_group)
            .map(|(position, _)| position)
            .collect()
    }

    fn keyframe_timestamps_ns(&self) -> HashSet<i64> {
        self.media_frames
            .iter()
            .filter(|media_frame| media_frame.starts_group)
            .map(|media_frame| media_frame.frame_header_timestamp_ns)
            .collect()
    }

    fn every_published_timestamp_ns(&self) -> HashSet<i64> {
        self.media_frames
            .iter()
            .map(|media_frame| media_frame.frame_header_timestamp_ns)
            .collect()
    }

    /// The published stream cut into groups, each the stamps of a keyframe
    /// and the media frames after it up to the next keyframe.
    fn groups_of_timestamps_ns(&self) -> Vec<Vec<i64>> {
        let mut groups: Vec<Vec<i64>> = Vec::new();
        for media_frame in &self.media_frames {
            match groups.last_mut() {
                Some(group) if !media_frame.starts_group => {
                    group.push(media_frame.frame_header_timestamp_ns);
                }
                _ => groups.push(vec![media_frame.frame_header_timestamp_ns]),
            }
        }
        groups
    }
}

/// What the decoded channel's collector saw.
#[derive(Default)]
struct DecodedChannelObservations {
    frames: Vec<VideoFrame>,
    frame_header_timestamps_ns: Vec<i64>,
    unreadable_bags: Vec<String>,
}

/// How far the burst forwarder has got.
#[derive(Default)]
struct BurstForwarderProgress {
    media_frames_forwarded: usize,
    burst_released: bool,
}

fn encoded_channel_tally() -> &'static Mutex<EncodedChannelTally> {
    static TALLY: OnceLock<Mutex<EncodedChannelTally>> = OnceLock::new();
    TALLY.get_or_init(Mutex::default)
}

fn decoded_channel_observations() -> &'static Mutex<DecodedChannelObservations> {
    static OBSERVATIONS: OnceLock<Mutex<DecodedChannelObservations>> = OnceLock::new();
    OBSERVATIONS.get_or_init(Mutex::default)
}

fn burst_forwarder_progress() -> &'static Mutex<BurstForwarderProgress> {
    static PROGRESS: OnceLock<Mutex<BurstForwarderProgress>> = OnceLock::new();
    PROGRESS.get_or_init(Mutex::default)
}

/// Taps the encoded channel beside the decoder, so the test knows what the
/// encoder published rather than inferring it from what came out.
#[streamlib::sdk::processor(
    description = "Tallies the media frames the encoder publishes",
    execution = reactive,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "Media frames, tapped beside the decoder"
    )
)]
pub struct EncodedMediaFrameTally;

impl ReactiveProcessor for EncodedMediaFrameTally::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("encoded_video")? {
            let mut tally = encoded_channel_tally().lock().unwrap();
            tally.messages_lost_ahead_of_the_tap += message.lost_since_last_read;
            match read_hang_legacy_container_media_frame(&message.payload_bytes) {
                Ok(_) => tally.media_frames.push(TalliedMediaFrame {
                    starts_group: message.group_boundary == MessageGroupBoundary::StartsGroup,
                    frame_header_timestamp_ns: message.timestamp_ns,
                }),
                Err(refusal) => tally.refusals.push(refusal.to_string()),
            }
        }
        Ok(())
    }
}

/// Forwards the encoder's media frames to the decoder unchanged, except that
/// once it has passed [`MEDIA_FRAMES_FORWARDED_BEFORE_THE_BURST`] it holds the
/// next [`MEDIA_FRAMES_HELD_FOR_THE_BURST`] and writes them in one go — more
/// than the decoder's link can hold, so the engine itself drops the oldest.
#[streamlib::sdk::processor(
    description = "Forwards media frames, releasing one held-back burst the link cannot hold",
    execution = reactive,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "Media frames from the encoder"
    ),
    output(
        "bunched_encoded_video",
        description = "The same media frames, one burst bunched up"
    )
)]
pub struct MediaFrameBurstForwarder {
    held_for_the_burst: Vec<(Vec<u8>, i64, MessageGroupBoundary)>,
}

impl ReactiveProcessor for MediaFrameBurstForwarder::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("encoded_video")? {
            let mut progress = burst_forwarder_progress().lock().unwrap();
            let holding = !progress.burst_released
                && progress.media_frames_forwarded >= MEDIA_FRAMES_FORWARDED_BEFORE_THE_BURST;
            progress.media_frames_forwarded += 1;
            if holding {
                self.held_for_the_burst.push((
                    message.payload_bytes,
                    message.timestamp_ns,
                    message.group_boundary,
                ));
                if self.held_for_the_burst.len() == MEDIA_FRAMES_HELD_FOR_THE_BURST {
                    for (payload_bytes, timestamp_ns, group_boundary) in
                        self.held_for_the_burst.drain(..)
                    {
                        self.outputs.write_bytes_with_timestamp(
                            "bunched_encoded_video",
                            &payload_bytes,
                            timestamp_ns,
                            group_boundary,
                        )?;
                    }
                    progress.burst_released = true;
                }
                continue;
            }
            self.outputs.write_bytes_with_timestamp(
                "bunched_encoded_video",
                &message.payload_bytes,
                message.timestamp_ns,
                message.group_boundary,
            )?;
        }
        Ok(())
    }
}

/// Reads the decoder's published surfaces the way any downstream consumer
/// does — an ordinary video-frame bag, cast at the read.
#[streamlib::sdk::processor(
    description = "Collects the decoder's published video frames for the test's assertions",
    execution = reactive,
    input(
        "video",
        delivery_profile = "ordered",
        description = "Decoded video frames under test"
    )
)]
pub struct DecodedVideoFrameCollector;

impl ReactiveProcessor for DecodedVideoFrameCollector::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw("video")? {
            let mut observations = decoded_channel_observations().lock().unwrap();
            match rmp_serde::from_slice::<VideoFrame>(&message.payload_bytes) {
                Ok(frame) => {
                    observations.frames.push(frame);
                    observations
                        .frame_header_timestamps_ns
                        .push(message.timestamp_ns);
                }
                Err(cast_failure) => observations.unreadable_bags.push(cast_failure.to_string()),
            }
        }
        Ok(())
    }
}

/// Register the media built-ins and this file's readers, once for the whole
/// binary — the registry never overwrites a live registration.
fn ensure_every_processor_type_is_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        register_media_builtin_processor_types();
        PROCESSOR_REGISTRY.register::<EncodedMediaFrameTally::Processor>();
        PROCESSOR_REGISTRY.register::<MediaFrameBurstForwarder::Processor>();
        PROCESSOR_REGISTRY.register::<DecodedVideoFrameCollector::Processor>();
    });
}

/// Whether this machine runs the arm at all, printing the skip line when not.
fn this_machine_skips_the_codec_round_trip() -> bool {
    #[cfg(target_os = "linux")]
    if rig_only_capability_skip::skip_unless_this_machine_has(
        rig_only_capability_skip::RigOnlyCapability::VulkanVideo,
        this_device_encodes_and_decodes_vulkan_video(),
    ) {
        return true;
    }
    false
}

/// The pattern → encoder → [burst forwarder →] decoder graph, with the tally
/// on the encoder's port and the collector on the decoder's.
fn run_the_codec_round_trip_until(
    arm: &CodecRoundTripArm,
    through_the_burst_forwarder: bool,
    enough_was_seen: impl Fn() -> bool,
) -> bool {
    ensure_every_processor_type_is_registered();

    let project_directory = tempfile::tempdir().expect("a project directory");
    let app = App::new_in_project_directory(project_directory.path()).expect("a runtime");
    let source = app
        .add(
            TestPatternSource::Processor::processor_class_import_path(),
            serde_json::json!({ "width": PATTERN_WIDTH, "height": PATTERN_HEIGHT }),
            Some("pattern"),
        )
        .expect("the test pattern");
    let encoder = app
        .add(
            arm.encoder_class_import_path.clone(),
            serde_json::json!({ "keyframe_interval_seconds": KEYFRAME_INTERVAL_SECONDS }),
            Some("encoder"),
        )
        .expect("the encoder");
    let decoder = app
        .add(
            arm.decoder_class_import_path.clone(),
            serde_json::json!({}),
            Some("decoder"),
        )
        .expect("the decoder");
    let encoded_tally = app
        .add(
            EncodedMediaFrameTally::Processor::processor_class_import_path(),
            serde_json::json!({}),
            Some("encoded_tally"),
        )
        .expect("the encoded tally");
    let decoded_collector = app
        .add(
            DecodedVideoFrameCollector::Processor::processor_class_import_path(),
            serde_json::json!({}),
            Some("decoded_collector"),
        )
        .expect("the decoded collector");

    app.connect((&source, "video"), (&encoder, "video"))
        .expect("the pattern to the encoder");
    if through_the_burst_forwarder {
        let burst_forwarder = app
            .add(
                MediaFrameBurstForwarder::Processor::processor_class_import_path(),
                serde_json::json!({}),
                Some("burst_forwarder"),
            )
            .expect("the burst forwarder");
        app.connect(
            (&encoder, "encoded_video"),
            (&burst_forwarder, "encoded_video"),
        )
        .expect("the encoder to the burst forwarder");
        app.connect(
            (&burst_forwarder, "bunched_encoded_video"),
            (&decoder, "encoded_video"),
        )
        .expect("the burst forwarder to the decoder");
    } else {
        app.connect((&encoder, "encoded_video"), (&decoder, "encoded_video"))
            .expect("the encoder to the decoder");
    }
    app.connect(
        (&encoder, "encoded_video"),
        (&encoded_tally, "encoded_video"),
    )
    .expect("the encoder to the tally");
    app.connect((&decoder, "video"), (&decoded_collector, "video"))
        .expect("the decoder to the collector");

    app.stream().start().expect("the graph starts");
    let readiness = app
        .stream()
        .wait_until_every_processor_is_running(READINESS_TIMEOUT);

    let collecting_since = Instant::now();
    let enough_was_seen_in_time = loop {
        if enough_was_seen() {
            break true;
        }
        if collecting_since.elapsed() > COLLECTION_TIMEOUT {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stop_outcome = app.stream().stop();

    // Readiness covers setup only — the GPU bring-up and the decoder's own
    // session mint. The encoder's mint is lazy and a failed one leaves it
    // Running, so what notices either failure is the emptiness assertion.
    readiness.expect("every processor must reach Running");
    stop_outcome.expect("the graph stops cleanly");
    enough_was_seen_in_time
}

/// What every run holds to, lossy or not: each message read as a media
/// frame, each decoded picture is one the encoder published, at the windowed
/// extent, stamped as it was published and in order, and the stream is
/// entered at a keyframe.
fn assert_every_decoded_frame_is_one_the_encoder_published(
    arm: &CodecRoundTripArm,
    tally: &EncodedChannelTally,
    observations: &DecodedChannelObservations,
) {
    assert_eq!(
        tally.refusals,
        Vec::<String>::new(),
        "every encoded message must read as a media frame"
    );
    assert_eq!(
        observations.unreadable_bags,
        Vec::<String>::new(),
        "every decoded bag must cast to a video frame"
    );
    assert!(
        !observations.frames.is_empty(),
        "the decoder published nothing — a decoder that never configures its session leaves \
         this at zero"
    );

    let published_timestamps_ns = tally.every_published_timestamp_ns();
    let invented: Vec<i64> = observations
        .frame_header_timestamps_ns
        .iter()
        .copied()
        .filter(|timestamp_ns| !published_timestamps_ns.contains(timestamp_ns))
        .collect();
    assert_eq!(
        invented,
        Vec::<i64>::new(),
        "every decoded frame carries the timestamp of an access unit the encoder published — \
         a decoder invents no pictures and stamps none itself"
    );

    let first_decoded_timestamp_ns = observations.frame_header_timestamps_ns[0];
    assert!(
        tally
            .keyframe_timestamps_ns()
            .contains(&first_decoded_timestamp_ns),
        "the stream is entered at a keyframe and never mid-group: the first decoded frame \
         carries timestamp {first_decoded_timestamp_ns}, which is no keyframe's"
    );

    let mut previous_frame_header_timestamp_ns: Option<i64> = None;
    for (frame, frame_header_timestamp_ns) in observations
        .frames
        .iter()
        .zip(&observations.frame_header_timestamps_ns)
    {
        assert_eq!(
            (frame.width, frame.height),
            (PATTERN_WIDTH, PATTERN_HEIGHT),
            "the {} decoder publishes the stream's windowed extent, not the 1088-tall \
             coded one",
            arm.codec_name
        );
        assert!(
            !frame.surface_id.is_empty(),
            "a decoded frame names the pooled surface its pixels were staged into"
        );
        assert!(
            *frame_header_timestamp_ns > 0 && frame.timestamp_ns == *frame_header_timestamp_ns,
            "a decoded frame carries the media frame's timestamp, on the header and in the \
             bag alike — never a reading taken at decode time"
        );
        if let Some(previous) = previous_frame_header_timestamp_ns {
            assert!(
                *frame_header_timestamp_ns > previous,
                "decoded timestamps advance strictly: an ordered channel never runs backwards, \
                 and a picture is never published twice"
            );
        }
        previous_frame_header_timestamp_ns = Some(*frame_header_timestamp_ns);
    }
}

/// Encode a test pattern, decode it in the same graph,
/// and require the decoded stream to be non-empty, to track the encoded one,
/// and to run past the second keyframe.
///
/// Mental revert: make the shared decode body admit every message from the
/// start. A subscriber that attaches mid-GOP then feeds slices into an
/// unconfigured session, the log fills with `Slice NAL received before
/// session configured — skipping`, and the entry assertion is what notices.
pub fn every_encoded_frame_the_decoder_is_handed_comes_back_as_a_published_surface(
    arm: CodecRoundTripArm,
) {
    if this_machine_skips_the_codec_round_trip() {
        return;
    }
    let ran_past_the_second_keyframe = run_the_codec_round_trip_until(&arm, false, || {
        let keyframe_positions = encoded_channel_tally().lock().unwrap().keyframe_positions();
        let decoded_frames = decoded_channel_observations().lock().unwrap().frames.len();
        keyframe_positions.len() >= ENOUGH_KEYFRAMES_ENCODED
            && decoded_frames > keyframe_positions[1]
    });

    let tally = encoded_channel_tally().lock().unwrap();
    let observations = decoded_channel_observations().lock().unwrap();
    let encoded_frames = tally.media_frames.len();
    let decoded_frames = observations.frames.len();
    assert!(
        ran_past_the_second_keyframe,
        "the decoded stream must run past the encoded stream's second keyframe inside \
         {COLLECTION_TIMEOUT:?}; the encoder published {encoded_frames} frames with keyframes \
         at {:?} and the decoder published {decoded_frames} frames",
        tally.keyframe_positions()
    );
    assert!(
        decoded_frames <= encoded_frames,
        "a decoder publishes at most one picture per access unit it was handed, got \
         {decoded_frames} from {encoded_frames}"
    );
    assert_every_decoded_frame_is_one_the_encoder_published(&arm, &tally, &observations);
}

/// Burst the encoded link past what it holds, so the engine overwrites media
/// frames the decoder never reads and its next read reports them lost, and
/// require the decoder to discard to the next keyframe rather than decode
/// past the hole.
///
/// The proof needs no knowledge of which frames the ring dropped: within
/// every group, the frames the decoder published must be a prefix of the
/// group as the encoder published it. A decoder that decoded across the loss
/// publishes a frame whose predecessor in its group it never saw, which
/// breaks the prefix.
///
/// Mental revert: make the shared decode body ignore `lost_since_last_read`.
/// The burst's survivors that continue a group are then decoded against
/// references the decoder never received, and the prefix assertion fails.
pub fn a_loss_on_the_encoded_link_is_survived_by_discarding_to_the_next_keyframe(
    arm: CodecRoundTripArm,
) {
    if this_machine_skips_the_codec_round_trip() {
        return;
    }
    let ran_past_the_burst = run_the_codec_round_trip_until(&arm, true, || {
        let progress = burst_forwarder_progress().lock().unwrap();
        progress.burst_released
            && progress.media_frames_forwarded
                >= MEDIA_FRAMES_FORWARDED_BEFORE_THE_BURST
                    + MEDIA_FRAMES_HELD_FOR_THE_BURST
                    + MEDIA_FRAMES_FORWARDED_AFTER_THE_BURST
    });

    let tally = encoded_channel_tally().lock().unwrap();
    let observations = decoded_channel_observations().lock().unwrap();
    assert!(
        ran_past_the_burst,
        "the burst forwarder must release its burst and forward \
         {MEDIA_FRAMES_FORWARDED_AFTER_THE_BURST} media frames after it inside \
         {COLLECTION_TIMEOUT:?}; the encoder published {} frames",
        tally.media_frames.len()
    );
    assert_eq!(
        tally.messages_lost_ahead_of_the_tap, 0,
        "the tap beside the decoder keeps up, so its record of the published stream is whole"
    );
    assert_every_decoded_frame_is_one_the_encoder_published(&arm, &tally, &observations);

    let decoded_timestamps_ns: HashSet<i64> = observations
        .frame_header_timestamps_ns
        .iter()
        .copied()
        .collect();
    for group in tally.groups_of_timestamps_ns() {
        let decoded_in_this_group: Vec<bool> = group
            .iter()
            .map(|timestamp_ns| decoded_timestamps_ns.contains(timestamp_ns))
            .collect();
        let decoded_prefix_length = decoded_in_this_group
            .iter()
            .take_while(|decoded| **decoded)
            .count();
        assert!(
            decoded_in_this_group[decoded_prefix_length..]
                .iter()
                .all(|decoded| !decoded),
            "the {} decoder published a frame of the group opening at {} after skipping one \
             before it — a picture built from a reference it never received; decoded pattern \
             {decoded_in_this_group:?}",
            arm.codec_name,
            group[0]
        );
    }
    assert!(
        observations.frames.len() < tally.media_frames.len(),
        "the burst must cost the decoder frames, or the run proved nothing about loss: it \
         decoded {} of {}",
        observations.frames.len(),
        tally.media_frames.len()
    );

    let last_held_timestamp_ns = tally.media_frames
        [MEDIA_FRAMES_FORWARDED_BEFORE_THE_BURST + MEDIA_FRAMES_HELD_FOR_THE_BURST - 1]
        .frame_header_timestamp_ns;
    assert!(
        observations
            .frame_header_timestamps_ns
            .iter()
            .any(|timestamp_ns| *timestamp_ns > last_held_timestamp_ns),
        "the decoder re-entered at a keyframe after the loss and kept decoding past the burst"
    );
}

/// Whether the Vulkan device a runtime here builds on encodes and decodes
/// Vulkan Video; on macOS the codec built-ins take VideoToolbox instead.
#[cfg(target_os = "linux")]
fn this_device_encodes_and_decodes_vulkan_video() -> bool {
    let device = streamlib::sdk::engine::host_rhi::HostVulkanDevice::new()
        .expect("a Vulkan device comes up on this machine");
    device.supports_video_encode() && device.supports_video_decode()
}
