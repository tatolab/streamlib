// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Links between started streams where a test has to reach inside the engine:
//! a compile that fails after it created a source, and a source stream
//! abandoned by its teardown watchdog. Starts real streams, so these run on
//! the rig only.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serial_test::serial;

use super::{LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner};
use crate::core::graph::{
    InputLinkPortRef, LinkUniqueId, OutputPortExposureLevel, OutputPortInAnotherStreamOnThisMachine,
};
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use crate::core::pubsub::{Event, EventListener, PUBSUB, topics};
use crate::core::test_support::a_temporary_directory_at_owner_only_mode;
use crate::core::{Result, RuntimeContextLimitedAccess};

/// How long a test waits for something the engine does on its own threads.
const THE_ENGINE_ANSWERS_WITHIN: Duration = Duration::from_secs(20);

/// Publishes a bag on `video` every few milliseconds.
#[crate::processor(execution = continuous(interval_ms = 2), output("video"))]
pub(crate) struct BagSourceOfALinkBetweenStreams;

impl crate::core::ContinuousProcessor for BagSourceOfALinkBetweenStreams::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.outputs.write_raw("video", b"a bag", 0)
    }
}

/// What the bag sink tallies its bags under.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct BagTallyingSinkOfALinkBetweenStreamsConfig {
    /// The tally this sink counts every bag it reads into.
    pub tally: String,
}

/// Every sink's tally by name, and the signal a sink raises on each bag.
static BAG_TALLIES: LazyLock<(Mutex<HashMap<String, usize>>, Condvar)> =
    LazyLock::new(|| (Mutex::default(), Condvar::new()));

/// Counts every bag that reaches `video`.
#[crate::processor(
    execution = reactive,
    config = crate::core::runtime::links_between_started_streams_tests::BagTallyingSinkOfALinkBetweenStreamsConfig,
    input("video", delivery_profile = "ordered"),
)]
pub(crate) struct BagTallyingSinkOfALinkBetweenStreams;

impl crate::core::ReactiveProcessor for BagTallyingSinkOfALinkBetweenStreams::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let mut read = 0;
        while self.inputs.read_raw("video")?.is_some() {
            read += 1;
        }
        if read > 0 {
            let (tallies, a_bag_arrived) = &*BAG_TALLIES;
            *tallies
                .lock()
                .unwrap()
                .entry(self.config.tally.clone())
                .or_default() += read;
            a_bag_arrived.notify_all();
        }
        Ok(())
    }
}

fn bags_tallied_as(tally: &str) -> usize {
    BAG_TALLIES
        .0
        .lock()
        .unwrap()
        .get(tally)
        .copied()
        .unwrap_or(0)
}

/// Wait until `tally` counts at least `at_least` bags, saying whether it did.
fn wait_until_tallied(tally: &str, at_least: usize) -> bool {
    let (tallies, a_bag_arrived) = &*BAG_TALLIES;
    let (tallies, _) = a_bag_arrived
        .wait_timeout_while(
            tallies.lock().unwrap(),
            THE_ENGINE_ANSWERS_WITHIN,
            |tallies| tallies.get(tally).copied().unwrap_or(0) < at_least,
        )
        .unwrap();
    tallies.get(tally).copied().unwrap_or(0) >= at_least
}

fn ensure_the_test_processors_are_registered() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        PROCESSOR_REGISTRY.register::<BagSourceOfALinkBetweenStreams::Processor>();
        PROCESSOR_REGISTRY.register::<BagTallyingSinkOfALinkBetweenStreams::Processor>();
    });
}

fn add_a_camera(stream: &LoadedStreamInThisRuntime) {
    stream
        .add_processor(
            ProcessorSpec::new(
                BagSourceOfALinkBetweenStreams::processor_class_import_path(),
                serde_json::Value::Null,
            )
            .with_display_name("camera".to_string()),
        )
        .expect("the camera is added");
}

fn add_a_display_tallying_into(
    stream: &LoadedStreamInThisRuntime,
    tally: &str,
) -> InputLinkPortRef {
    let display = stream
        .add_processor_reporting_its_name(
            ProcessorSpec::new(
                BagTallyingSinkOfALinkBetweenStreams::processor_class_import_path(),
                serde_json::json!({ "tally": tally }),
            )
            .with_display_name("display".to_string()),
        )
        .expect("the display is added");
    InputLinkPortRef::new(display.processor_id, "video")
}

fn start(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_processor_is_running(THE_ENGINE_ANSWERS_WITHIN)
        .expect("every processor of the stream runs");
}

fn camera_video_of_the_stream_a() -> OutputPortInAnotherStreamOnThisMachine {
    OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "video").unwrap()
}

fn the_link_as_graph_renders_it(
    stream: &LoadedStreamInThisRuntime,
    link_id: &LinkUniqueId,
) -> serde_json::Value {
    stream.to_json().expect("the graph renders")["links"]
        .as_array()
        .and_then(|links| links.iter().find(|link| link["id"] == link_id.as_str()))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

struct ForwardsEveryEvent(std::sync::mpsc::Sender<()>);

impl EventListener for ForwardsEveryEvent {
    fn on_event(&mut self, _event: &Event) -> Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// Every event on one stream's topic, as a signal to look at its graph again.
struct EveryEventOfOneStream {
    _listener: Arc<parking_lot::Mutex<dyn EventListener>>,
    an_event_arrived: std::sync::mpsc::Receiver<()>,
}

impl EveryEventOfOneStream {
    fn listen_to(stream: &LoadedStreamInThisRuntime) -> Self {
        let (forward, an_event_arrived) = std::sync::mpsc::channel();
        let listener: Arc<parking_lot::Mutex<dyn EventListener>> =
            Arc::new(parking_lot::Mutex::new(ForwardsEveryEvent(forward)));
        PUBSUB
            .subscribe(
                &topics::loaded_stream(stream.loaded_stream_identity()),
                Arc::clone(&listener),
            )
            .expect("the listener subscribes");
        Self {
            _listener: listener,
            an_event_arrived,
        }
    }

    /// Wait, looking again at each event, until link `link_id` of `stream`
    /// renders as `holds` wants it; the last rendering either way.
    fn until_the_link(
        &self,
        stream: &LoadedStreamInThisRuntime,
        link_id: &LinkUniqueId,
        holds: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + THE_ENGINE_ANSWERS_WITHIN;
        loop {
            let rendered = the_link_as_graph_renders_it(stream, link_id);
            if holds(&rendered) {
                return rendered;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return rendered;
            };
            if self.an_event_arrived.recv_timeout(left).is_err() {
                return the_link_as_graph_renders_it(stream, link_id);
            }
        }
    }
}

fn reads_wired(link: &serde_json::Value) -> bool {
    link["state"] == "wired"
}

fn reads_awaiting_remote_because(why: &'static str) -> impl Fn(&serde_json::Value) -> bool {
    move |link| {
        link["state"] == "awaiting_remote"
            && link["awaiting_remote_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(why))
    }
}

/// One engine, its streams' project directory, and the streams `a`, holding
/// a running camera whose `video` is private, and `b`, holding a running
/// display reading it.
struct ACameraStreamReadByADisplayStream {
    runner: Arc<Runner>,
    a: Arc<LoadedStreamInThisRuntime>,
    b: Arc<LoadedStreamInThisRuntime>,
    link_id: LinkUniqueId,
    _project_directory: tempfile::TempDir,
}

impl ACameraStreamReadByADisplayStream {
    fn start(display_tally: &str, a_teardown_watchdog_budget: Duration) -> Self {
        ensure_the_test_processors_are_registered();
        let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
        let runner = Runner::new().expect("the engine builds");
        let a = runner
            .load_an_empty_stream_whose_teardown_watchdog_fires_after(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named("a"),
                a_teardown_watchdog_budget,
            )
            .expect("the stream `a` loads");
        add_a_camera(&a);
        a.set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
            .unwrap();
        let b = runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named("b"),
            )
            .expect("the stream `b` loads");
        let display = add_a_display_tallying_into(&b, display_tally);
        start(&a);
        start(&b);
        let link_id = b
            .connect_from_an_output_port_in_another_stream(camera_video_of_the_stream_a(), display)
            .expect("a private port is read by another stream");
        assert!(wait_until_tallied(display_tally, 5));
        Self {
            runner,
            a,
            b,
            link_id,
            _project_directory: project_directory,
        }
    }

    fn the_cameras_output_writer(&self) -> Arc<crate::iceoryx2::OutputWriterInner> {
        self.a.compiler.scope(|graph, _tx| {
            use crate::core::graph::{GraphNodeWithComponents, ProcessorInstanceComponent};
            graph
                .traversal()
                .v_with_node_name("camera")
                .first()
                .and_then(|camera| camera.get::<ProcessorInstanceComponent>())
                .and_then(|instance| instance.0.lock().iceoryx2_output_writer_inner())
                .expect("the camera runs with an output writer")
        })
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn the_source_streams_watchdog_abandoning_it_returns_the_link_to_awaiting() {
    let streams =
        ACameraStreamReadByADisplayStream::start("source-abandoned", Duration::from_millis(300));
    let camera_output = streams.the_cameras_output_writer();
    assert_eq!(camera_output.channel_notifier_count("video"), 1);
    let events_of_b = EveryEventOfOneStream::listen_to(&streams.b);

    // The stop takes this lock after it marks the stream stopping, so holding
    // it hangs `a`'s teardown before any processor is removed.
    let the_hung_teardown_waits_on = streams.a.runtime_context.lock();
    streams
        .a
        .ask_for_this_streams_shutdown("the test hangs this stream's teardown");
    let rendered = events_of_b.until_the_link(
        &streams.b,
        &streams.link_id,
        reads_awaiting_remote_because("the stream `a` is not loaded on this machine"),
    );
    let notifiers_while_the_teardown_hangs = camera_output.channel_notifier_count("video");
    let ended = streams
        .a
        .wait_for_this_streams_end_within(THE_ENGINE_ANSWERS_WITHIN);
    drop(the_hung_teardown_waits_on);

    assert!(ended, "the watchdog never ended the hung stream");

    assert!(
        reads_awaiting_remote_because("the stream `a` is not loaded on this machine")(&rendered),
        "{rendered}"
    );
    assert!(matches!(
        streams.a.how_this_stream_ended(),
        Some(super::HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog { .. })
    ));
    assert_eq!(
        notifiers_while_the_teardown_hangs, 0,
        "the link let go of the abandoned stream's port without its graph lock"
    );
    assert_eq!(streams.runner.names_of_the_loaded_streams(), ["b"]);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn an_awaiting_link_never_fails_the_commit() {
    ensure_the_test_processors_are_registered();
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    let runner = Runner::new().expect("the engine builds");
    let load = |stream_name: &str| {
        runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named(stream_name),
            )
            .expect("the stream loads")
    };
    let a = load("a");
    add_a_camera(&a);
    start(&a);
    let b = load("b");
    let display = add_a_display_tallying_into(&b, "never-carries");
    let refused = b
        .connect_from_an_output_port_in_another_stream(
            camera_video_of_the_stream_a(),
            display.clone(),
        )
        .expect("a link added before its stream starts is checked when it wires");
    let awaiting = b
        .connect_from_an_output_port_in_another_stream(
            OutputPortInAnotherStreamOnThisMachine::new("nowhere", "camera", "video").unwrap(),
            display,
        )
        .unwrap();

    b.start()
        .expect("a link that waits or is refused never fails the stream's start");

    let refused = the_link_as_graph_renders_it(&b, &refused);
    assert_eq!(refused["state"], "error", "{refused}");
    assert!(
        refused["error_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("internal")),
        "{refused}"
    );
    let awaiting = the_link_as_graph_renders_it(&b, &awaiting);
    assert!(
        reads_awaiting_remote_because("the stream `nowhere` is not loaded")(&awaiting),
        "{awaiting}"
    );
    b.wait_until_every_processor_is_running(THE_ENGINE_ANSWERS_WITHIN)
        .expect("the display runs beside its links that carry nothing");
    assert_eq!(bags_tallied_as("never-carries"), 0);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_compile_that_creates_a_source_and_then_fails_still_wakes_the_links_waiting_on_it() {
    ensure_the_test_processors_are_registered();
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    let runner = Runner::new().expect("the engine builds");
    let load = |stream_name: &str| {
        runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                    .named(stream_name),
            )
            .expect("the stream loads")
    };
    let a = load("a");
    let display_of_a = add_a_display_tallying_into(&a, "a-display-of-the-failed-compile");
    let b = load("b");
    let display_of_b = add_a_display_tallying_into(&b, "woken-by-a-compile-that-failed");
    let link_id = b
        .connect_from_an_output_port_in_another_stream(camera_video_of_the_stream_a(), display_of_b)
        .expect("a link added before its stream starts is checked when it wires");
    start(&a);
    start(&b);
    let events_of_b = EveryEventOfOneStream::listen_to(&b);
    let rendered =
        events_of_b.until_the_link(&b, &link_id, reads_awaiting_remote_because("\"camera\""));
    assert!(
        reads_awaiting_remote_because("\"camera\"")(&rendered),
        "{rendered}"
    );

    // One batch creates the camera and then refuses a config update of the
    // display beside it, after the camera already runs.
    a.compiler.scope(|graph, tx| {
        let camera = graph
            .traversal_mut()
            .add_v(
                ProcessorSpec::new(
                    BagSourceOfALinkBetweenStreams::processor_class_import_path(),
                    serde_json::Value::Null,
                )
                .with_display_name("camera".to_string()),
            )
            .expect("the camera's type resolves")
            .first()
            .expect("the camera is added")
            .id
            .clone();
        tx.log(crate::core::compiler::PendingOperation::AddProcessor(
            camera,
        ));
        tx.log(
            crate::core::compiler::PendingOperation::UpdateProcessorConfig {
                processor_id: display_of_a.processor_id().clone(),
                config_to_apply: serde_json::json!({ "tally": 7 }),
            },
        );
    });
    a.set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();
    let compiled = a.compiler.commit(
        &a.runtime_context_while_started()
            .expect("a started stream holds its runtime context"),
    );
    assert!(
        compiled.is_err(),
        "the display of `a` refuses a tally that is not a string"
    );

    let rendered = events_of_b.until_the_link(&b, &link_id, reads_wired);
    assert!(reads_wired(&rendered), "{rendered}");
    assert!(wait_until_tallied("woken-by-a-compile-that-failed", 5));
}
