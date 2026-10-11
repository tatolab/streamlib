// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Links between started streams where a test has to reach inside the engine:
//! a compile that fails after it created a source, and a source stream
//! abandoned by its teardown watchdog. Starts real streams, so these run on
//! the rig only.

use std::sync::Arc;
use std::time::Duration;

use serial_test::serial;

use super::{LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner};
use crate::core::graph::{
    InputLinkPortRef, LinkUniqueId, OutputPortExposureLevel, OutputPortInAnotherStreamOnThisMachine,
};
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use crate::core::test_support::a_temporary_directory_at_owner_only_mode;
use crate::core::{Result, RuntimeContextLimitedAccess};

use crate::core::test_support::links_between_streams_harness::{
    EveryEventOfOneStream, THE_ENGINE_ANSWERS_WITHIN, bags_tallied_as,
    reads_awaiting_remote_because, reads_wired, start, tally_bags_read,
    the_link_as_graph_renders_it, wait_until_tallied,
};

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
            tally_bags_read(&self.config.tally, read);
        }
        Ok(())
    }
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

fn camera_video_of_the_stream_a() -> OutputPortInAnotherStreamOnThisMachine {
    OutputPortInAnotherStreamOnThisMachine::new("a", "camera", "video").unwrap()
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
fn awaiting_links_never_fail_the_commit_and_one_awaiting_its_port_wires_once_it_opens() {
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
    let display = add_a_display_tallying_into(&b, "carries-once-the-port-opens");
    let awaiting_its_port = b
        .connect_from_an_output_port_in_another_stream(
            camera_video_of_the_stream_a(),
            display.clone(),
        )
        .expect("a link added before its stream starts is checked when it wires");
    let awaiting_its_stream = b
        .connect_from_an_output_port_in_another_stream(
            OutputPortInAnotherStreamOnThisMachine::new("nowhere", "camera", "video").unwrap(),
            display,
        )
        .unwrap();

    b.start()
        .expect("a link that waits never fails the stream's start");

    let rendered = the_link_as_graph_renders_it(&b, &awaiting_its_port);
    assert!(
        reads_awaiting_remote_because("port `video` of node `camera` in stream `a` is internal")(
            &rendered
        ),
        "{rendered}"
    );
    assert!(
        reads_awaiting_remote_because("the link wires when the port is exposed private or public")(
            &rendered
        ),
        "{rendered}"
    );
    let rendered = the_link_as_graph_renders_it(&b, &awaiting_its_stream);
    assert!(
        reads_awaiting_remote_because("the stream `nowhere` is not loaded")(&rendered),
        "{rendered}"
    );
    b.wait_until_every_processor_is_running(THE_ENGINE_ANSWERS_WITHIN)
        .expect("the display runs beside its links that carry nothing");
    assert_eq!(bags_tallied_as("carries-once-the-port-opens"), 0);

    let events_of_b = EveryEventOfOneStream::listen_to(&b);
    a.set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();
    let rendered = events_of_b.until_the_link(&b, &awaiting_its_port, reads_wired);
    assert!(reads_wired(&rendered), "{rendered}");
    assert!(
        wait_until_tallied("carries-once-the-port-opens", 5),
        "opening the port wired the link, and nothing reached the display"
    );
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

/// A paused reading stream still takes a cut at once: lowering the source
/// port to `internal` takes the link out of its graph and releases the
/// source's notifier before it resumes, and the link stays gone after.
///
/// Fail-without-fix: compile the cut only while the reading stream is started
/// and the link stays wired in the paused stream's graph, its notifier on the
/// source's port.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_paused_reading_stream_takes_a_cut_at_once() {
    let streams = ACameraStreamReadByADisplayStream::start(
        "a-paused-reader-cut",
        super::ENGINE_TEARDOWN_WATCHDOG_BUDGET,
    );
    let camera_output = streams.the_cameras_output_writer();
    assert_eq!(camera_output.channel_notifier_count("video"), 1);
    streams.b.pause().expect("the stream `b` pauses");
    let events_of_b = EveryEventOfOneStream::listen_to(&streams.b);

    streams
        .a
        .set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Internal)
        .unwrap();

    let rendered = events_of_b.until_the_link(&streams.b, &streams.link_id, |link| link.is_null());
    assert!(
        rendered.is_null(),
        "the paused stream kept the cut link: {rendered}"
    );
    assert_eq!(camera_output.channel_notifier_count("video"), 0);
    streams.b.resume().expect("the stream `b` resumes");
    assert!(
        the_link_as_graph_renders_it(&streams.b, &streams.link_id).is_null(),
        "the resumed stream holds the cut link again"
    );
    assert_eq!(camera_output.channel_notifier_count("video"), 0);
}

/// A re-plan that lands while the reading stream is paused waits for its
/// resume, which compiles it: opening the source port while `b` is paused
/// leaves the link awaiting, and resuming `b` wires it.
///
/// Fail-without-fix: resume without a commit and the re-plan logged while
/// paused is never compiled, so the link waits on a port already open.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn resuming_a_reading_stream_compiles_the_re_plan_logged_while_it_was_paused() {
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
    let display = add_a_display_tallying_into(&b, "wired-once-b-resumes");
    let link_id = b
        .connect_from_an_output_port_in_another_stream(camera_video_of_the_stream_a(), display)
        .expect("a link added before its stream starts is checked when it wires");
    start(&b);
    let events_of_b = EveryEventOfOneStream::listen_to(&b);
    let rendered =
        events_of_b.until_the_link(&b, &link_id, reads_awaiting_remote_because("internal"));
    assert!(
        reads_awaiting_remote_because("internal")(&rendered),
        "{rendered}"
    );
    b.pause().expect("the stream `b` pauses");

    a.set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();
    let the_re_plan_is_logged =
        || {
            b.compiler
            .logged_pending_operations()
            .iter()
            .any(|operation| matches!(
                operation,
                crate::core::compiler::PendingOperation::AddLink(logged) if *logged == link_id
            ))
        };
    events_of_b.until_the_link(&b, &link_id, |_| the_re_plan_is_logged());
    assert!(
        the_re_plan_is_logged(),
        "opening the port re-planned nothing"
    );
    let rendered = the_link_as_graph_renders_it(&b, &link_id);
    assert!(
        reads_awaiting_remote_because("internal")(&rendered),
        "{rendered}"
    );

    b.resume().expect("the stream `b` resumes");

    let rendered = the_link_as_graph_renders_it(&b, &link_id);
    assert!(reads_wired(&rendered), "{rendered}");
    assert!(wait_until_tallied("wired-once-b-resumes", 5));
}

/// A live connect onto a source stream whose graph is held past the bound
/// adds the link, which waits saying that stream did not answer, and wires on
/// that stream's next compile — a config update that creates no processor.
///
/// Fail-without-fix: re-plan a link only when its source stream creates a
/// processor, opens a port or is unloaded, and the link waits on a healthy
/// source stream for good.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
)]
#[test]
#[serial]
fn a_link_its_source_stream_did_not_answer_wires_on_that_streams_next_compile() {
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
    let display_of_a = add_a_display_tallying_into(&a, "a-display-beside-a-busy-camera");
    a.set_output_port_exposure_level("camera", "video", OutputPortExposureLevel::Private)
        .unwrap();
    start(&a);
    let b = load("b");
    let display_of_b = add_a_display_tallying_into(&b, "wired-on-the-sources-next-compile");
    start(&b);
    let events_of_b = EveryEventOfOneStream::listen_to(&b);

    let link_id = std::thread::scope(|scope| {
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let source_stream = &a;
        scope.spawn(move || {
            source_stream.compiler.scope(|_graph, _tx| {
                let _ = held_tx.send(());
                let _ = release_rx.recv_timeout(THE_ENGINE_ANSWERS_WITHIN);
            })
        });
        held_rx.recv().expect("the source's graph is held");
        let connected = b.connect_from_an_output_port_in_another_stream(
            camera_video_of_the_stream_a(),
            display_of_b,
        );
        let _ = release_tx.send(());
        connected.expect("a source stream that does not answer the check still takes the link")
    });
    let rendered = the_link_as_graph_renders_it(&b, &link_id);
    assert!(
        reads_awaiting_remote_because("the stream `a` did not answer")(&rendered),
        "{rendered}"
    );

    a.update_processor_config(
        display_of_a.processor_id(),
        serde_json::json!({ "tally": "a-display-beside-a-healthy-camera" }),
    )
    .expect("the display of `a` takes a new tally");

    let rendered = events_of_b.until_the_link(&b, &link_id, reads_wired);
    assert!(reads_wired(&rendered), "{rendered}");
    assert!(wait_until_tallied("wired-on-the-sources-next-compile", 5));
}
