// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! `TestPatternSource` → encoder → decoder in a real graph: every encoded
//! frame the decoder is handed comes back out as a published video surface.
//!
//! One harness, one arm per codec — the H.264 and H.265 built-in pairs share
//! their whole body, so what is worth running twice is the round trip, not
//! the assertions written twice.
//!
//! This is #1077 made reproducible. Its recorded symptom was
//! `frames_encoded ≈ 50, frames_decoded = 0` — the decoder's first received
//! frame was the encoder's second, the IDR never arrived, and the periodic
//! IDRs were not recognised. The assertions below are exactly that shape
//! read forwards: the decoded stream is not empty, it tracks the encoded one
//! to within what the link can hold, and it runs past the stream's second
//! sync point, which is the "periodic IDRs recognised" half.
//!
//! The pattern is 1920x1080 on purpose, for both arms: 1080 is a multiple of
//! neither the H.264 macroblock nor the H.265 CTU, so the coded height is
//! 1088 either way and only the SPS's conformance window brings it back. A
//! decoder publishing the coded extent instead of the windowed one shows up
//! as 1088 in the extent assertion below — which is the whole of the H.265
//! CTU-crop contract, asserted rather than described.
//!
//! Rig-tier by construction, not by choice: `App::new()` brings up a real
//! `GpuContext` and the sessions need Vulkan Video encode *and* decode
//! queues — so CI compiles these binaries and the rig runs them. The source
//! is the test pattern rather than a camera on purpose: it needs no
//! `/dev/video*` device, so what this asserts is the codec round trip and
//! nothing else.

use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use streamlib::sdk::App;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ReactiveProcessor};
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib_media_builtins::{
    TestPatternSource, VideoFrame, read_encoded_video_frame_bag,
    register_media_builtin_processor_types,
};

/// How long a processor may take to bring up the GPU and reach Running.
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);

/// The run is bounded by observation, not wall time. It ends once the
/// decoded stream has run past the encoded stream's second sync point,
/// under a generous cap — an encoder's real cadence is whatever the session
/// negotiated, so no wall-clock run length proves anything about it.
const ENOUGH_SYNC_POINTS_ENCODED: usize = 2;
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(90);

/// The encoder's sync-point cadence for this run, short so the second IDR
/// lands within a few seconds of the test pattern's 30 fps.
const KEYFRAME_INTERVAL_SECONDS: u32 = 1;

/// The pattern's extent. 1080 is aligned to neither codec's block size, so
/// the coded height is 1088 for both and each decoder must publish the SPS's
/// windowed 1080.
const PATTERN_WIDTH: u32 = 1920;
const PATTERN_HEIGHT: u32 = 1080;

/// The codec pair one arm binary runs the harness through. Named by the arm
/// itself rather than chosen from an enum here, so neither binary carries a
/// branch for the codec it does not run.
pub struct CodecRoundTripArm {
    /// The wire spelling, so a failed assertion says which arm failed.
    pub codec_name: &'static str,
    pub encoder_class_import_path: ProcessorClassImportPath,
    pub decoder_class_import_path: ProcessorClassImportPath,
}

/// One encoded frame as the tap beside the decoder saw it.
struct TalliedEncodedFrame {
    sequence_index: u64,
    is_sync_point: bool,
    frame_header_timestamp_ns: i64,
}

/// What the encoded channel's tap saw.
#[derive(Default)]
struct EncodedChannelTally {
    frames: Vec<TalliedEncodedFrame>,
    refusals: Vec<String>,
}

impl EncodedChannelTally {
    fn sync_point_sequence_indices(&self) -> Vec<u64> {
        self.frames
            .iter()
            .filter(|frame| frame.is_sync_point)
            .map(|frame| frame.sequence_index)
            .collect()
    }

    fn sync_point_timestamps_ns(&self) -> std::collections::HashSet<i64> {
        self.frames
            .iter()
            .filter(|frame| frame.is_sync_point)
            .map(|frame| frame.frame_header_timestamp_ns)
            .collect()
    }

    fn every_published_timestamp_ns(&self) -> std::collections::HashSet<i64> {
        self.frames
            .iter()
            .map(|frame| frame.frame_header_timestamp_ns)
            .collect()
    }
}

/// What the decoded channel's collector saw.
#[derive(Default)]
struct DecodedChannelObservations {
    frames: Vec<VideoFrame>,
    frame_header_timestamps_ns: Vec<i64>,
    unreadable_bags: Vec<String>,
}

fn encoded_channel_tally() -> &'static Mutex<EncodedChannelTally> {
    static TALLY: OnceLock<Mutex<EncodedChannelTally>> = OnceLock::new();
    TALLY.get_or_init(Mutex::default)
}

fn decoded_channel_observations() -> &'static Mutex<DecodedChannelObservations> {
    static OBSERVATIONS: OnceLock<Mutex<DecodedChannelObservations>> = OnceLock::new();
    OBSERVATIONS.get_or_init(Mutex::default)
}

/// Taps the encoded channel beside the decoder, so the test knows what the
/// decoder was handed rather than inferring it from what came out.
#[streamlib::sdk::processor(
    description = "Tallies the encoded-frame bags the decoder is handed",
    execution = reactive,
    input(
        "encoded_video",
        delivery_profile = "ordered",
        description = "Encoded-frame bags, tapped beside the decoder"
    )
)]
pub struct EncodedFrameBagTally;

impl ReactiveProcessor for EncodedFrameBagTally::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some((bag_bytes, frame_header_timestamp_ns)) =
            self.inputs.read_raw("encoded_video")?
        {
            let mut tally = encoded_channel_tally().lock().unwrap();
            match read_encoded_video_frame_bag(&bag_bytes) {
                Ok(encoded_frame) => tally.frames.push(TalliedEncodedFrame {
                    sequence_index: encoded_frame.sequence_index,
                    is_sync_point: encoded_frame.is_sync_point,
                    frame_header_timestamp_ns,
                }),
                Err(refusal) => tally.refusals.push(refusal.to_string()),
            }
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
        while let Some((bag_bytes, frame_header_timestamp_ns)) = self.inputs.read_raw("video")? {
            let mut observations = decoded_channel_observations().lock().unwrap();
            match rmp_serde::from_slice::<VideoFrame>(&bag_bytes) {
                Ok(frame) => {
                    observations.frames.push(frame);
                    observations
                        .frame_header_timestamps_ns
                        .push(frame_header_timestamp_ns);
                }
                Err(cast_failure) => observations.unreadable_bags.push(cast_failure.to_string()),
            }
        }
        Ok(())
    }
}

/// Register the media built-ins and this file's two readers, once for the
/// whole binary — the registry never overwrites a live registration.
fn ensure_every_processor_type_is_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        register_media_builtin_processor_types();
        PROCESSOR_REGISTRY.register::<EncodedFrameBagTally::Processor>();
        PROCESSOR_REGISTRY.register::<DecodedVideoFrameCollector::Processor>();
    });
}

/// #1077, read forwards. Encode a test pattern, decode it in the same graph,
/// and require the decoded stream to be non-empty, to track the encoded one,
/// and to run past the second sync point.
///
/// Mental revert: make the shared decode body's sync-point gate admit
/// everything from the start. A subscriber that attaches mid-GOP then feeds slices into an
/// unconfigured session, the log fills with `Slice NAL received before
/// session configured — skipping`, and the emptiness assertion below is what
/// notices.
pub fn every_encoded_frame_the_decoder_is_handed_comes_back_as_a_published_surface(
    arm: CodecRoundTripArm,
) {
    ensure_every_processor_type_is_registered();

    let app = App::new().expect("a runtime");
    let source = app
        .add(
            TestPatternSource::Processor::processor_class_import_path(),
            serde_json::json!({ "width": PATTERN_WIDTH, "height": PATTERN_HEIGHT }),
            Some("pattern"),
        )
        .expect("the test pattern");
    let encoder = app
        .add(
            arm.encoder_class_import_path,
            serde_json::json!({ "keyframe_interval_seconds": KEYFRAME_INTERVAL_SECONDS }),
            Some("encoder"),
        )
        .expect("the encoder");
    let decoder = app
        .add(
            arm.decoder_class_import_path,
            serde_json::json!({}),
            Some("decoder"),
        )
        .expect("the decoder");
    let encoded_tally = app
        .add(
            EncodedFrameBagTally::Processor::processor_class_import_path(),
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
    app.connect((&encoder, "encoded_video"), (&decoder, "encoded_video"))
        .expect("the encoder to the decoder");
    app.connect(
        (&encoder, "encoded_video"),
        (&encoded_tally, "encoded_video"),
    )
    .expect("the encoder to the tally");
    app.connect((&decoder, "video"), (&decoded_collector, "video"))
        .expect("the decoder to the collector");

    app.runner().start().expect("the graph starts");
    let readiness = app
        .runner()
        .wait_until_every_processor_is_running(READINESS_TIMEOUT);

    let collecting_since = Instant::now();
    let ran_past_the_second_sync_point = loop {
        {
            let sync_point_sequence_indices = encoded_channel_tally()
                .lock()
                .unwrap()
                .sync_point_sequence_indices();
            let decoded_frames = decoded_channel_observations().lock().unwrap().frames.len();
            if sync_point_sequence_indices.len() >= ENOUGH_SYNC_POINTS_ENCODED
                && decoded_frames as u64 > sync_point_sequence_indices[1]
            {
                break true;
            }
        }
        if collecting_since.elapsed() > COLLECTION_TIMEOUT {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stop_outcome = app.runner().stop();

    // Readiness covers setup only — the GPU bring-up and the decoder's own
    // session mint. The encoder's mint is lazy and a failed one leaves it
    // Running, so what notices either failure is the emptiness assertion.
    readiness.expect("every processor must reach Running");
    stop_outcome.expect("the graph stops cleanly");

    let tally = encoded_channel_tally().lock().unwrap();
    let observations = decoded_channel_observations().lock().unwrap();
    assert_eq!(
        tally.refusals,
        Vec::<String>::new(),
        "every encoded bag must read back through the convention's own reader"
    );
    assert_eq!(
        observations.unreadable_bags,
        Vec::<String>::new(),
        "every decoded bag must cast to a video frame"
    );

    let encoded_frames = tally.frames.len() as u64;
    let decoded_frames = observations.frames.len() as u64;
    let sync_point_sequence_indices = tally.sync_point_sequence_indices();
    assert!(
        ran_past_the_second_sync_point,
        "the decoded stream must run past the encoded stream's second sync point inside \
         {COLLECTION_TIMEOUT:?}; the encoder published {encoded_frames} frames with sync \
         points at {sync_point_sequence_indices:?} and the decoder published {decoded_frames} \
         frames — a decoder that never configures its session leaves this at zero"
    );
    assert!(
        decoded_frames <= encoded_frames,
        "a decoder publishes at most one picture per access unit it was handed, got \
         {decoded_frames} from {encoded_frames}"
    );

    // How far the decoded count trails the encoded one is not a contract: on
    // a link that lost bags the doctrine discards a whole group on purpose,
    // and in a debug build at 1080p that happens. What IS a contract is that
    // every picture published corresponds to an access unit the encoder
    // actually produced, and that the stream is only ever entered at a sync
    // point. The gate's own arithmetic is unit-covered in
    // `encoded_video_frame`; these are its observable consequences.
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

    let sync_point_timestamps_ns = tally.sync_point_timestamps_ns();
    let first_decoded_timestamp_ns = observations.frame_header_timestamps_ns[0];
    assert!(
        sync_point_timestamps_ns.contains(&first_decoded_timestamp_ns),
        "the stream is entered at a sync point and never mid-group: the first decoded frame \
         carries timestamp {first_decoded_timestamp_ns}, which is no sync point's"
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
            "a decoded frame carries the encoded frame's timestamp, on the header and in the \
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
