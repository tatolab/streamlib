// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Links between two started streams on one machine: a link is made in the
//! stream that holds its input, reads an output port another stream exposes,
//! carries that stream's bags, and follows the source stream through its
//! level changes, unloads and reloads.
//!
//! Starts real streams (GPU + iceoryx2), so this runs on the rig only.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serial_test::serial;
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::{
    InputLinkPortRef, LinkUniqueId, OutputPortExposureLevel, OutputPortInAnotherStreamOnThisMachine,
};
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::{
    HowALoadedStreamEnded, LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner,
    RuntimeOperations,
};
use streamlib::sdk::schemars::JsonSchema;
use streamlib_engine::iceoryx2::OutputWriterInner;
use streamlib_engine::{Result, RuntimeContextLimitedAccess};

use streamlib_engine as the_engine_under_test;
#[path = "support/links_between_streams_harness.rs"]
mod links_between_streams_harness;
use links_between_streams_harness::{
    EveryEventOfOneStream, bags_tallied_as, reads_awaiting_remote_because, reads_wired, start,
    tally_bags_read, the_link_as_graph_renders_it, wait_until_tallied,
};

/// How long a level lowered to `internal` may take to cut a link.
const A_LOWERED_LEVEL_CUTS_A_LINK_WITHIN: Duration = Duration::from_secs(1);

/// How many bags a sink must count before a link counts as carrying.
const ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES: usize = 5;

/// What the bag source registers its output writer as.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BagSourceConfig {
    /// The name the test reads the source's output writer back by.
    pub output_writer_registered_as: String,
}

/// Every bag source's output writer, by the name its config gives it.
static OUTPUT_WRITERS_OF_THE_BAG_SOURCES: LazyLock<
    Mutex<HashMap<String, Weak<OutputWriterInner>>>,
> = LazyLock::new(Mutex::default);

/// Publishes a bag on `video` and on `preview` every few milliseconds.
#[streamlib::sdk::processor(
    execution = continuous(interval_ms = 2),
    config = BagSourceConfig,
    output("video"),
    output("preview"),
)]
pub struct BagSource;

impl streamlib_engine::ContinuousProcessor for BagSource::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if let Some(output_writer) = self.outputs.inner_arc() {
            OUTPUT_WRITERS_OF_THE_BAG_SOURCES
                .lock()
                .unwrap()
                .entry(self.config.output_writer_registered_as.clone())
                .or_insert_with(|| Arc::downgrade(&output_writer));
        }
        self.outputs.write_raw("video", b"a video bag", 0)?;
        self.outputs.write_raw("preview", b"a preview bag", 0)
    }
}

/// What the bag sink tallies its bags under.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BagTallyingSinkConfig {
    /// The tally this sink counts every bag it reads into.
    pub tally: String,
}

/// Counts every bag that reaches `video`.
#[streamlib::sdk::processor(
    execution = reactive,
    config = BagTallyingSinkConfig,
    input("video", delivery_profile = "ordered"),
)]
pub struct BagTallyingSink;

impl streamlib_engine::ReactiveProcessor for BagTallyingSink::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let mut read = 0;
        while self.inputs.read_raw("video")?.is_some() {
            read += 1;
        }
        if read > 0 {
            tally_bags_read(&self.config.tally, read);
        }
        Ok(())
    }
}

/// The output writer the bag source registered as `name`, once it has
/// published.
fn the_output_writer_registered_as(name: &str) -> Arc<OutputWriterInner> {
    OUTPUT_WRITERS_OF_THE_BAG_SOURCES
        .lock()
        .unwrap()
        .get(name)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| panic!("no bag source registered its output writer as `{name}`"))
}

fn ensure_the_test_processors_are_registered() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        PROCESSOR_REGISTRY.register::<BagSource::Processor>();
        PROCESSOR_REGISTRY.register::<BagTallyingSink::Processor>();
    });
}

/// A name no other test of this binary uses, for a tally or an output writer.
fn a_name_for_this_test(what: &str) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    format!(
        "{what}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    )
}

/// One engine and the project directory its streams load from.
struct OneMachine {
    runner: Arc<Runner>,
    project_directory: tempfile::TempDir,
}

impl OneMachine {
    fn new() -> Self {
        ensure_the_test_processors_are_registered();
        Self {
            runner: Runner::new().expect("the engine builds"),
            project_directory: tempfile::tempdir().expect("a project directory"),
        }
    }

    fn load(&self, stream_name: &str) -> Arc<LoadedStreamInThisRuntime> {
        self.runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(self.project_directory.path())
                    .named(stream_name),
            )
            .expect("the stream loads")
    }

    /// Load the stream `stream_name` holding a bag source named `camera`,
    /// registering its writer as `output_writer_registered_as`, each port at
    /// the level given.
    fn load_a_camera_stream(
        &self,
        stream_name: &str,
        output_writer_registered_as: &str,
        levels: &[(&str, OutputPortExposureLevel)],
    ) -> Arc<LoadedStreamInThisRuntime> {
        let stream = self.load(stream_name);
        add_a_bag_source(&stream, "camera", output_writer_registered_as);
        for (port, level) in levels {
            stream
                .set_output_port_exposure_level("camera", port, *level)
                .expect("the port is exposed");
        }
        stream
    }
}

fn add_a_bag_source(
    stream: &LoadedStreamInThisRuntime,
    node_name: &str,
    output_writer_registered_as: &str,
) {
    stream
        .add_processor_reporting_its_name(
            ProcessorSpec::new(
                BagSource::processor_class_import_path(),
                serde_json::json!({ "output_writer_registered_as": output_writer_registered_as }),
            )
            .with_display_name(node_name.to_string()),
        )
        .expect("the bag source is added");
}

/// Add a bag sink named `node_name` tallying into `tally`, and the input it
/// reads.
fn add_a_bag_sink(
    stream: &LoadedStreamInThisRuntime,
    node_name: &str,
    tally: &str,
) -> InputLinkPortRef {
    let added = stream
        .add_processor_reporting_its_name(
            ProcessorSpec::new(
                BagTallyingSink::processor_class_import_path(),
                serde_json::json!({ "tally": tally }),
            )
            .with_display_name(node_name.to_string()),
        )
        .expect("the bag sink is added");
    InputLinkPortRef::new(added.processor_id, "video")
}

fn port_of_another_stream(
    stream: &str,
    node: &str,
    port: &str,
) -> OutputPortInAnotherStreamOnThisMachine {
    OutputPortInAnotherStreamOnThisMachine::new(stream, node, port).expect("the names cast")
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_link_to_an_internal_port_of_another_stream_is_refused_naming_the_port_and_its_level() {
    let machine = OneMachine::new();
    let a = machine.load_a_camera_stream("a", &a_name_for_this_test("camera"), &[]);
    let b = machine.load("b");
    let display = add_a_bag_sink(&b, "display", &a_name_for_this_test("display"));
    start(&a);
    start(&b);

    let refusal = b
        .connect_from_an_output_port_in_another_stream(
            port_of_another_stream("a", "camera", "video"),
            display,
        )
        .expect_err("an internal port is refused");

    match &refusal {
        Error::OutputPortNotExposedToTheReader(refused) => assert_eq!(
            (
                refused.stream.as_str(),
                refused.node.as_str(),
                refused.port.as_str(),
                refused.level.as_str()
            ),
            ("a", "camera", "video", "internal")
        ),
        other => panic!("expected OutputPortNotExposedToTheReader, got {other:?}"),
    }
    assert_eq!(
        b.to_json().unwrap()["links"],
        serde_json::json!([]),
        "a refused link is never added"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_link_to_a_private_port_of_another_stream_wires_and_carries_bags() {
    let machine = OneMachine::new();
    let camera = a_name_for_this_test("camera");
    let display_tally = a_name_for_this_test("display");
    let a =
        machine.load_a_camera_stream("a", &camera, &[("video", OutputPortExposureLevel::Private)]);
    let b = machine.load("b");
    let display = add_a_bag_sink(&b, "display", &display_tally);
    start(&a);
    start(&b);

    let link_id = b
        .connect_from_an_output_port_in_another_stream(
            port_of_another_stream("a", "camera", "video"),
            display,
        )
        .expect("a private port is read by another stream");

    let rendered = the_link_as_graph_renders_it(&b, &link_id);
    assert!(reads_wired(&rendered), "{rendered}");
    assert_eq!(
        rendered["source"],
        serde_json::json!({"stream": "a", "node": "camera", "port": "video"})
    );
    assert!(
        wait_until_tallied(&display_tally, ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES),
        "the display counted {} bags from the stream `a`",
        bags_tallied_as(&display_tally)
    );
    assert_eq!(
        a.to_json().unwrap()["links"],
        serde_json::json!([]),
        "the source's stream holds no link for a link another stream holds"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn lowering_the_source_to_internal_disconnects_the_link_at_once_and_other_links_keep_carrying() {
    let machine = OneMachine::new();
    let camera = a_name_for_this_test("camera");
    let monitor_tally = a_name_for_this_test("monitor");
    let video_display_tally = a_name_for_this_test("video-display");
    let preview_display_tally = a_name_for_this_test("preview-display");
    let a = machine.load_a_camera_stream(
        "a",
        &camera,
        &[
            ("video", OutputPortExposureLevel::Private),
            ("preview", OutputPortExposureLevel::Private),
        ],
    );
    let monitor = add_a_bag_sink(&a, "monitor", &monitor_tally);
    let camera_in_a = a.the_node_named("camera").expect("the camera is in `a`");
    a.connect(
        streamlib::sdk::graph::OutputLinkPortRef::new(camera_in_a.processor_id, "video"),
        monitor,
    )
    .expect("the camera feeds its own stream's monitor");
    let b = machine.load("b");
    let video_display = add_a_bag_sink(&b, "video-display", &video_display_tally);
    let c = machine.load("c");
    let preview_display = add_a_bag_sink(&c, "preview-display", &preview_display_tally);
    for stream in [&a, &b, &c] {
        start(stream);
    }
    let video_link = b
        .connect_from_an_output_port_in_another_stream(
            port_of_another_stream("a", "camera", "video"),
            video_display,
        )
        .unwrap();
    c.connect_from_an_output_port_in_another_stream(
        port_of_another_stream("a", "camera", "preview"),
        preview_display,
    )
    .unwrap();
    for tally in [&monitor_tally, &video_display_tally, &preview_display_tally] {
        assert!(
            wait_until_tallied(tally, ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES),
            "{tally}"
        );
    }
    let camera_output = the_output_writer_registered_as(&camera);
    assert_eq!(camera_output.channel_notifier_count("video"), 2);
    let events_of_b = EveryEventOfOneStream::listen_to(&b);

    let lowered_at = Instant::now();
    a.set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Internal)
        .expect("the level is lowered");
    let video_link_once_cut = events_of_b.until_the_link(&b, &video_link, |link| link.is_null());
    let cut_within = lowered_at.elapsed();

    assert!(
        video_link_once_cut.is_null(),
        "the link is still there: {video_link_once_cut}"
    );
    assert!(
        cut_within < A_LOWERED_LEVEL_CUTS_A_LINK_WITHIN,
        "the link was cut {cut_within:?} after the level was lowered"
    );
    assert_eq!(
        camera_output.channel_notifier_count("video"),
        1,
        "the cut link's notifier leaves the camera's port and the monitor's stays"
    );
    let monitor_before = bags_tallied_as(&monitor_tally);
    let preview_display_before = bags_tallied_as(&preview_display_tally);
    assert!(wait_until_tallied(
        &monitor_tally,
        monitor_before + ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
    assert!(wait_until_tallied(
        &preview_display_tally,
        preview_display_before + ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_link_loaded_before_its_source_stream_awaits_naming_it_then_wires_when_it_starts() {
    let machine = OneMachine::new();
    let display_tally = a_name_for_this_test("display");
    let b = machine.load("b");
    let display = add_a_bag_sink(&b, "display", &display_tally);
    let link_id = b
        .connect_from_an_output_port_in_another_stream(
            port_of_another_stream("a", "camera", "video"),
            display,
        )
        .unwrap();
    start(&b);
    let rendered = the_link_as_graph_renders_it(&b, &link_id);
    assert!(
        reads_awaiting_remote_because("the stream `a` is not loaded on this machine")(&rendered),
        "{rendered}"
    );
    let events_of_b = EveryEventOfOneStream::listen_to(&b);

    let a = machine.load_a_camera_stream(
        "a",
        &a_name_for_this_test("camera"),
        &[("video", OutputPortExposureLevel::Private)],
    );
    start(&a);

    let rendered = events_of_b.until_the_link(&b, &link_id, reads_wired);
    assert!(reads_wired(&rendered), "{rendered}");
    assert!(wait_until_tallied(
        &display_tally,
        ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_link_whose_source_stream_starts_with_the_port_internal_awaits_naming_it_then_wires_when_it_opens()
 {
    let machine = OneMachine::new();
    let display_tally = a_name_for_this_test("display");
    let b = machine.load("b");
    let display = add_a_bag_sink(&b, "display", &display_tally);
    let link_id = b
        .connect_from_an_output_port_in_another_stream(
            port_of_another_stream("a", "camera", "video"),
            display,
        )
        .unwrap();
    start(&b);
    let events_of_b = EveryEventOfOneStream::listen_to(&b);

    let a = machine.load_a_camera_stream("a", &a_name_for_this_test("camera"), &[]);
    start(&a);

    let awaits_its_port = |link: &serde_json::Value| {
        reads_awaiting_remote_because("port `video` of node `camera` in stream `a` is internal")(
            link,
        ) && reads_awaiting_remote_because(
            "the link wires when the port is exposed private or public",
        )(link)
    };
    let rendered = events_of_b.until_the_link(&b, &link_id, awaits_its_port);
    assert!(awaits_its_port(&rendered), "{rendered}");
    assert_eq!(bags_tallied_as(&display_tally), 0);

    a.set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();

    let rendered = events_of_b.until_the_link(&b, &link_id, reads_wired);
    assert!(reads_wired(&rendered), "{rendered}");
    assert!(wait_until_tallied(
        &display_tally,
        ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn unloading_the_source_stream_puts_the_link_back_to_awaiting_and_its_reload_rewires_it() {
    let machine = OneMachine::new();
    let display_tally = a_name_for_this_test("display");
    let a = machine.load_a_camera_stream(
        "a",
        &a_name_for_this_test("camera"),
        &[("video", OutputPortExposureLevel::Private)],
    );
    let b = machine.load("b");
    let display = add_a_bag_sink(&b, "display", &display_tally);
    start(&a);
    start(&b);
    let link_id = b
        .connect_from_an_output_port_in_another_stream(
            port_of_another_stream("a", "camera", "video"),
            display,
        )
        .unwrap();
    assert!(wait_until_tallied(
        &display_tally,
        ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
    let events_of_b = EveryEventOfOneStream::listen_to(&b);

    machine
        .runner
        .unload_stream("a")
        .expect("the stream `a` unloads");
    drop(a);

    let rendered = events_of_b.until_the_link(
        &b,
        &link_id,
        reads_awaiting_remote_because("the stream `a` is not loaded on this machine"),
    );
    assert!(
        reads_awaiting_remote_because("the stream `a` is not loaded on this machine")(&rendered),
        "{rendered}"
    );

    let reloaded_camera = a_name_for_this_test("camera");
    let reloaded_a = machine.load_a_camera_stream(
        "a",
        &reloaded_camera,
        &[("video", OutputPortExposureLevel::Private)],
    );
    start(&reloaded_a);
    let rendered = events_of_b.until_the_link(&b, &link_id, reads_wired);
    assert!(reads_wired(&rendered), "{rendered}");
    let tallied_before_the_reload_carried = bags_tallied_as(&display_tally);
    assert!(wait_until_tallied(
        &display_tally,
        tallied_before_the_reload_carried + ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
    assert_eq!(
        the_output_writer_registered_as(&reloaded_camera).channel_notifier_count("video"),
        1,
        "the reload's camera publishes to the rewired link"
    );
}

/// The streams `a`, whose `camera` publishes `video` at `private`, and `b`,
/// whose `display` reads it over a link carrying bags, both started.
struct ACameraStreamReadByADisplayStream {
    a: Arc<LoadedStreamInThisRuntime>,
    b: Arc<LoadedStreamInThisRuntime>,
    link_id: LinkUniqueId,
    display_tally: String,
}

impl ACameraStreamReadByADisplayStream {
    fn start(machine: &OneMachine) -> Self {
        let display_tally = a_name_for_this_test("display");
        let a = machine.load_a_camera_stream(
            "a",
            &a_name_for_this_test("camera"),
            &[("video", OutputPortExposureLevel::Private)],
        );
        let b = machine.load("b");
        let display = add_a_bag_sink(&b, "display", &display_tally);
        start(&a);
        start(&b);
        let link_id = b
            .connect_from_an_output_port_in_another_stream(
                port_of_another_stream("a", "camera", "video"),
                display,
            )
            .expect("a private port is read by another stream");
        assert!(wait_until_tallied(
            &display_tally,
            ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
        ));
        Self {
            a,
            b,
            link_id,
            display_tally,
        }
    }

    /// Remove `a`'s camera and wait until `b`'s link awaits a node named
    /// `camera`.
    fn remove_the_camera(&self, events_of_b: &EveryEventOfOneStream) {
        let camera = self.a.the_node_named("camera").unwrap();
        self.a
            .remove_processor(&camera.processor_id)
            .expect("the camera is removed");
        let rendered = events_of_b.until_the_link(
            &self.b,
            &self.link_id,
            reads_awaiting_remote_because("\"camera\""),
        );
        assert!(
            reads_awaiting_remote_because("\"camera\"")(&rendered),
            "{rendered}"
        );
    }

    /// Add a camera back to `a`, its `video` at `internal` as every added
    /// node's port starts; the name its output writer registers as.
    fn add_the_camera_back(&self) -> String {
        let re_added_camera = a_name_for_this_test("camera");
        add_a_bag_source(&self.a, "camera", &re_added_camera);
        re_added_camera
    }

    /// Wait until the link is wired and carries bags from the camera whose
    /// output writer registered as `camera`, publishing to it alone.
    fn assert_the_link_carries_from(&self, events_of_b: &EveryEventOfOneStream, camera: &str) {
        let rendered = events_of_b.until_the_link(&self.b, &self.link_id, reads_wired);
        assert!(reads_wired(&rendered), "{rendered}");
        let tallied_before_the_re_add_carried = bags_tallied_as(&self.display_tally);
        assert!(
            wait_until_tallied(
                &self.display_tally,
                tallied_before_the_re_add_carried + ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
            ),
            "the display counted no bags from the re-added camera"
        );
        assert_eq!(
            the_output_writer_registered_as(camera).channel_notifier_count("video"),
            1,
            "the re-added camera publishes to the rewired link"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn removing_the_source_node_awaits_naming_it_and_adding_it_back_rewires_it() {
    let machine = OneMachine::new();
    let streams = ACameraStreamReadByADisplayStream::start(&machine);
    let events_of_b = EveryEventOfOneStream::listen_to(&streams.b);
    streams.remove_the_camera(&events_of_b);

    let re_added_camera = streams.add_the_camera_back();
    streams
        .a
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();

    streams.assert_the_link_carries_from(&events_of_b, &re_added_camera);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_re_added_source_node_still_internal_holds_the_link_awaiting_its_port_and_opening_it_rewires_it()
 {
    let machine = OneMachine::new();
    let streams = ACameraStreamReadByADisplayStream::start(&machine);
    let events_of_b = EveryEventOfOneStream::listen_to(&streams.b);
    streams.remove_the_camera(&events_of_b);

    let re_added_camera = streams.add_the_camera_back();
    let rendered = events_of_b.until_the_link(
        &streams.b,
        &streams.link_id,
        reads_awaiting_remote_because("is internal"),
    );
    assert!(
        reads_awaiting_remote_because("is internal")(&rendered),
        "the re-added camera's link does not await its port: {rendered}"
    );
    let reason = rendered["awaiting_remote_reason"].as_str().unwrap();
    for named in [
        "`a`",
        "`camera`",
        "`video`",
        "the link wires when the port is exposed private or public",
    ] {
        assert!(reason.contains(named), "{named:?} not in: {reason}");
    }

    streams
        .a
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();

    streams.assert_the_link_carries_from(&events_of_b, &re_added_camera);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn unloading_the_reader_stream_releases_the_publisher_it_installed_in_the_source() {
    let machine = OneMachine::new();
    let camera = a_name_for_this_test("camera");
    let display_tally = a_name_for_this_test("display");
    let a =
        machine.load_a_camera_stream("a", &camera, &[("video", OutputPortExposureLevel::Private)]);
    let b = machine.load("b");
    let display = add_a_bag_sink(&b, "display", &display_tally);
    start(&a);
    start(&b);
    b.connect_from_an_output_port_in_another_stream(
        port_of_another_stream("a", "camera", "video"),
        display,
    )
    .unwrap();
    assert!(wait_until_tallied(
        &display_tally,
        ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
    let camera_output = the_output_writer_registered_as(&camera);
    assert!(camera_output.has_channel_publisher("video"));
    assert_eq!(camera_output.channel_notifier_count("video"), 1);

    machine
        .runner
        .unload_stream("b")
        .expect("the stream `b` unloads");

    assert_eq!(camera_output.channel_notifier_count("video"), 0);
    assert!(
        !camera_output.has_channel_publisher("video"),
        "the publisher only the reader stream's link needed goes with it"
    );
    assert_eq!(a.to_json().unwrap()["exposed"][0]["level"], "private");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn removing_the_reader_node_releases_the_publisher_its_link_installed_in_the_source() {
    let machine = OneMachine::new();
    let camera = a_name_for_this_test("camera");
    let display_tally = a_name_for_this_test("display");
    let a =
        machine.load_a_camera_stream("a", &camera, &[("video", OutputPortExposureLevel::Private)]);
    let b = machine.load("b");
    let display = add_a_bag_sink(&b, "display", &display_tally);
    let display_id = display.processor_id().clone();
    start(&a);
    start(&b);
    let link_id = b
        .connect_from_an_output_port_in_another_stream(
            port_of_another_stream("a", "camera", "video"),
            display,
        )
        .unwrap();
    assert!(wait_until_tallied(
        &display_tally,
        ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
    let camera_output = the_output_writer_registered_as(&camera);
    assert_eq!(camera_output.channel_notifier_count("video"), 1);

    b.remove_processor(&display_id)
        .expect("the display is removed");

    assert!(the_link_as_graph_renders_it(&b, &link_id).is_null());
    assert_eq!(camera_output.channel_notifier_count("video"), 0);
    assert!(!camera_output.has_channel_publisher("video"));
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib-engine/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn two_streams_reading_each_other_wire_and_shut_down_within_the_budget() {
    const TWO_STREAMS_SHUT_DOWN_WITHIN: Duration = Duration::from_secs(10);

    let machine = OneMachine::new();
    let a_reads_b = a_name_for_this_test("a-reads-b");
    let b_reads_a = a_name_for_this_test("b-reads-a");
    let a = machine.load_a_camera_stream(
        "a",
        &a_name_for_this_test("camera"),
        &[("video", OutputPortExposureLevel::Private)],
    );
    let b = machine.load_a_camera_stream(
        "b",
        &a_name_for_this_test("camera"),
        &[("video", OutputPortExposureLevel::Private)],
    );
    let display_in_a = add_a_bag_sink(&a, "display", &a_reads_b);
    let display_in_b = add_a_bag_sink(&b, "display", &b_reads_a);
    a.connect_from_an_output_port_in_another_stream(
        port_of_another_stream("b", "camera", "video"),
        display_in_a,
    )
    .unwrap();
    b.connect_from_an_output_port_in_another_stream(
        port_of_another_stream("a", "camera", "video"),
        display_in_b,
    )
    .unwrap();
    start(&a);
    start(&b);

    assert!(wait_until_tallied(
        &a_reads_b,
        ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));
    assert!(wait_until_tallied(
        &b_reads_a,
        ENOUGH_BAGS_TO_SAY_A_LINK_CARRIES
    ));

    let shutting_down = Instant::now();
    machine.runner.shut_down().expect("the engine shuts down");
    let shut_down_within = shutting_down.elapsed();

    assert!(
        shut_down_within < TWO_STREAMS_SHUT_DOWN_WITHIN,
        "two streams reading each other took {shut_down_within:?} to shut down"
    );
    for stream in [&a, &b] {
        assert_eq!(
            stream.how_this_stream_ended(),
            Some(HowALoadedStreamEnded::Stopped)
        );
    }
}
