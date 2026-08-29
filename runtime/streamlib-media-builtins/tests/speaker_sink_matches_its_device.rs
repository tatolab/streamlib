// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SpeakerSink` settling its `audio_window = match_device` contract, in a real
//! graph, from whichever audio arm the machine running this has.
//!
//! **This is the only thing that runs the two production call sites.** The unit
//! tests either side of them exercise the parts — the settle on a bare
//! `InputMailboxesInner`, the refusal as a free function — and neither notices
//! if `SpeakerSink::setup()` stops calling the settle, or if the spawn seam
//! stops asking whether one was left unsettled. Both scenarios below go red on
//! exactly those reverts.
//!
//! Rig-tier by construction, not by choice: `App::new()` brings up a real
//! `GpuContext`, and no GPU-free construction of one exists, so nothing that
//! drives a processor's `setup()` can run on a CI runner. That is the same
//! reason every graph test in this tree is rig-gated.
//!
//! Deliberately arm-agnostic. The backend chain is probed once per process with
//! no configuration dial and no environment override by design, so a test
//! cannot force the null arm — it takes PipeWire or ALSA on a workstation and
//! the null arm in a container. Every assertion below holds on all three: none
//! names a rate or a channel count, because the settled values are the
//! machine's and that is the whole point of resolving them from the device.
//! What *is* fixed is the source's format — 16 kHz mono, which no audio device
//! opens at — so the stage has a real rate conversion to do on every arm.

use std::sync::Once;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use streamlib::sdk::App;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{
    ContinuousProcessor, GeneratedProcessor, PROCESSOR_REGISTRY, ReactiveProcessor,
};
use streamlib_media_builtins::{SpeakerSink, register_media_builtin_processor_types};

/// The source's rate. No audio device opens at 16 kHz, so every arm's settled
/// contract differs from this and the stage resamples on all of them.
const SOURCE_SAMPLE_RATE: u32 = 16_000;

/// Per-channel samples in one published block — 10 ms at the source rate.
const SOURCE_SAMPLES_PER_BLOCK: u32 = SOURCE_SAMPLE_RATE / 100;

/// How long a processor may take to open a device and reach Running.
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the graph runs once every processor is up, so blocks really cross
/// the link and pass through the stage rather than only being wired for.
const BLOCKS_REALLY_FLOW_FOR: Duration = Duration::from_secs(2);

/// Publishes 16 kHz mono `f32` blocks — the format nothing plays natively.
#[streamlib::sdk::processor(
    description = "Publishes 16 kHz mono blocks, the format no device opens at",
    execution = continuous(interval_ms = 10),
    output("audio", description = "Timestamped blocks of interleaved samples")
)]
pub struct SixteenKilohertzMonoSource;

impl ContinuousProcessor for SixteenKilohertzMonoSource::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let first_sample_timestamp_ns =
            streamlib::sdk::media_clock::MediaClock::now().as_nanos() as i64;
        // A ramp rather than silence: silence survives a stage that dropped
        // every sample and re-synthesised the window, which is exactly the
        // failure a "did it play?" assertion must not be blind to.
        let samples: Vec<u8> = (0..SOURCE_SAMPLES_PER_BLOCK)
            .flat_map(|sample| (sample as f32 / SOURCE_SAMPLES_PER_BLOCK as f32).to_le_bytes())
            .collect();
        self.outputs.write_with_timestamp(
            "audio",
            &json!({
                "samples": serde_bytes::ByteBuf::from(samples),
                "sample_rate": SOURCE_SAMPLE_RATE,
                "channels": 1u32,
                "sample_count": SOURCE_SAMPLES_PER_BLOCK,
                "dtype": "f32",
                "first_sample_timestamp_ns": first_sample_timestamp_ns,
            }),
            first_sample_timestamp_ns,
        )
    }
}

/// Declares the sentinel and opens no device stream — the case the spawn seam
/// exists to refuse.
#[streamlib::sdk::processor(
    description = "Declares match_device and opens no device, so nothing can settle it",
    execution = reactive,
    input("audio", delivery_profile = "ordered", audio_window = match_device)
)]
pub struct ConsumerThatOpensNoDeviceStream;

impl ReactiveProcessor for ConsumerThatOpensNoDeviceStream::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Deliberately settles nothing. A processor that opens no device stream
        // has no format to settle its port's sentinel with.
        Ok(())
    }
}

/// Register this file's two processor types and the media built-ins, once for
/// the whole binary.
///
/// The registry never overwrites a live registration, and both tests below need
/// the same source — so registering per test would make the second one fail on
/// the identity clash rather than on anything it asserts.
fn ensure_every_processor_type_is_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        register_media_builtin_processor_types();
        PROCESSOR_REGISTRY.register::<SixteenKilohertzMonoSource::Processor>();
        PROCESSOR_REGISTRY.register::<ConsumerThatOpensNoDeviceStream::Processor>();
    });
}

/// One node's rendering out of the live graph, by display name.
fn node_named<'a>(graph: &'a Value, display_name: &str) -> &'a Value {
    graph["nodes"]
        .as_array()
        .expect("the graph renders its nodes")
        .iter()
        .find(|node| node["display_name"] == display_name)
        .unwrap_or_else(|| panic!("no node named '{display_name}' in {graph}"))
}

/// The rung's flagship case end to end: a 16 kHz mono source drives whatever
/// this machine's speaker opened at, with nothing between them.
///
/// Mental revert: delete the settle from `SpeakerSink::setup()`
/// (`speaker_sink.rs`). Its port stays awaiting its device, the spawn seam
/// refuses it, the sink never reaches Running, and the readiness wait below
/// fails — before the rendering assertion is ever reached.
#[test]
fn a_sixteen_kilohertz_source_reaches_whatever_this_machines_speaker_opened_at() {
    ensure_every_processor_type_is_registered();

    let app = App::new().expect("a runtime");
    let source = app
        .add(
            SixteenKilohertzMonoSource::Processor::processor_class_import_path(),
            json!({}),
            Some("sixteen-kilohertz-source"),
        )
        .expect("the source");
    let speaker = app
        .add(
            SpeakerSink::Processor::processor_class_import_path(),
            json!({}),
            Some("speaker"),
        )
        .expect("the speaker");
    app.connect((&source, "audio"), (&speaker, "audio"))
        .expect("the source to the speaker");

    app.runner().start().expect("the graph starts");
    let readiness = app
        .runner()
        .wait_until_every_processor_is_running(READINESS_TIMEOUT);
    let held_up_from = Instant::now();
    while held_up_from.elapsed() < BLOCKS_REALLY_FLOW_FOR {
        std::thread::sleep(Duration::from_millis(100));
    }
    let graph = app.runner().to_json().expect("the graph renders");
    let stop_outcome = app.runner().stop();

    readiness.expect(
        "every processor must reach Running — a speaker whose `match_device` port was never \
         settled is refused at the end of its own setup() and stays in Error",
    );

    let audio_port = &node_named(&graph, "speaker")["ports"]["inputs"][0];
    assert_eq!(audio_port["name"], "audio");
    let settled = &audio_port["audio_window"];
    assert_eq!(
        settled["resolved_from"], "device",
        "graph must render values this machine's device settled, said to have come from the \
         device rather than from an author: {settled}"
    );
    assert_eq!(
        settled["window_size"], settled["hop"],
        "a sink converts format rather than re-framing, so window and hop are one device \
         period: {settled}"
    );
    for field in ["sample_rate", "channels", "window_size", "hop"] {
        assert!(
            settled[field].as_u64().unwrap_or(0) > 0,
            "`{field}` must carry the device's own value: {settled}"
        );
    }
    assert_ne!(
        settled["sample_rate"].as_u64(),
        Some(u64::from(SOURCE_SAMPLE_RATE)),
        "no audio arm opens at 16 kHz, so a settled contract matching the source's rate \
         means the device format never reached the port: {settled}"
    );

    stop_outcome.expect("the graph stops cleanly");
}

/// A processor that declares the sentinel and opens no device stream is refused
/// at the end of its own `setup()`, rather than running forever with a port
/// that hands it nothing.
///
/// Mental revert: delete the
/// `refuse_a_port_setup_left_awaiting_its_device_stream_format` call from
/// `spawn_dedicated_thread` (`spawn_processor_op.rs`). This consumer reaches
/// Running with a port that can never produce a window, and the readiness wait
/// below stops failing.
#[test]
fn a_processor_that_opens_no_device_stream_never_reaches_running() {
    ensure_every_processor_type_is_registered();

    let app = App::new().expect("a runtime");
    let source = app
        .add(
            SixteenKilohertzMonoSource::Processor::processor_class_import_path(),
            json!({}),
            Some("sixteen-kilohertz-source"),
        )
        .expect("the source");
    let consumer = app
        .add(
            ConsumerThatOpensNoDeviceStream::Processor::processor_class_import_path(),
            json!({}),
            Some("consumer"),
        )
        .expect("the consumer");
    app.connect((&source, "audio"), (&consumer, "audio"))
        .expect("the source to the consumer");

    app.runner().start().expect("the graph starts");
    let readiness = app
        .runner()
        .wait_until_every_processor_is_running(Duration::from_secs(5));
    let graph = app.runner().to_json().expect("the graph renders");
    let _ = app.runner().stop();

    readiness.expect_err(
        "a processor whose setup() left its `match_device` port unsettled must not reach \
         Running — nothing can ever settle it, so its port would hand it nothing for the \
         whole run",
    );
    assert_eq!(
        node_named(&graph, "consumer")["components"]["state"],
        "Error",
        "the refusal belongs at the end of setup(), where it can still be reported: {graph}"
    );
}
