// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The two lifecycle fixtures, run in a stream, prove what replaces the lifecycle hooks: a panic
//! in a build is reported on the node while its frames run on the value built before it; a
//! value set live builds once more and drops the value it replaces; and removing the node, or
//! stopping its stream, drops every value it built.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::descriptors::NodeClassImportPath;
use streamlib::sdk::graph::NodeRunId;
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner, RuntimeOperations};
use streamlib::sdk::serde_json::{Value, json};
use streamlib_project_repository::StreamGraphOperation;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticOnTheWire, DiagnosticRecoveryOnTheWire,
};
use streamlib_test_fixtures::lifecycle_probe_node::LifecycleProbe;
use streamlib_test_fixtures::panicking_lifecycle_node::{
    PanickingLifecycle, forget_the_levels_the_frames_of_the_panicking_node_read,
    levels_the_frames_of_the_panicking_node_read,
};
use uuid::Uuid;

/// The longest a test waits for what it waits on.
const WAITED_ON_WITHIN: Duration = Duration::from_secs(10);

/// A playing stream holding one node.
struct AStreamOfOneNode {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    node: Uuid,
    node_run_id: NodeRunId,
}

/// A stream holding one node of `node_type`, holding `input_values`, started and running.
fn a_playing_stream_of_one_node(
    project_directory: &Path,
    node_type: NodeClassImportPath,
    input_values: Value,
) -> AStreamOfOneNode {
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads");
    let node = Uuid::new_v4();
    let node_run_id = stream
        .add_node(NodeSpec::new(node_type, input_values).with_node_uuid(node))
        .expect("the node is added");
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");
    AStreamOfOneNode {
        _runner: runner,
        stream,
        node,
        node_run_id,
    }
}

/// Set input `input` of `node` to `value` while `stream` plays.
fn set_the_input(stream: &LoadedStreamInThisRuntime, node: Uuid, input: &str, value: Value) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(stream.apply_a_stream_graph_operation_live(
            StreamGraphOperation::SetNodeInputValue {
                node,
                input: input.to_owned(),
                member: None,
                from: None,
                to: Some(value),
            },
        ))
        .expect("an input is set while the stream plays");
}

/// What `read` reads once `settled` holds of it, or when the wait runs out.
fn once_it_holds<T>(read: impl Fn() -> T, settled: impl Fn(&T) -> bool) -> T {
    let deadline = Instant::now() + WAITED_ON_WITHIN;
    loop {
        let read = read();
        if settled(&read) || Instant::now() >= deadline {
            return read;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn the_type_of_the_panicking_node() -> NodeClassImportPath {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<PanickingLifecycle::Node>());
    PanickingLifecycle::Node::node_class_import_path()
}

fn the_type_of_the_lifecycle_probe() -> NodeClassImportPath {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<LifecycleProbe::Node>());
    LifecycleProbe::Node::node_class_import_path()
}

/// The last level the frames of the panicking node read.
fn the_last_level_read() -> Option<i32> {
    levels_the_frames_of_the_panicking_node_read()
        .last()
        .copied()
}

fn a_panicked_build(diagnostics: &[DiagnosticOnTheWire]) -> Option<&DiagnosticOnTheWire> {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == DiagnosticKindOnTheWire::ComputedValueBuildPanicked)
}

/// Mental-revert: a panic that escaped its build would end the node's builds and report
/// nothing; one that dropped the previous value would stop the frames.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_panic_in_a_build_is_reported_and_the_frames_run_on_the_previous_value() {
    forget_the_levels_the_frames_of_the_panicking_node_read();
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_playing_stream_of_one_node(
        project_directory.path(),
        the_type_of_the_panicking_node(),
        json!({ "level": 1 }),
    );
    once_it_holds(the_last_level_read, |level| *level == Some(1));

    set_the_input(
        &played.stream,
        played.node,
        "panic_in_its_build",
        json!(true),
    );
    set_the_input(&played.stream, played.node, "level", json!(2));

    let diagnostics = once_it_holds(
        || played.stream.diagnostics_of_its_nodes(),
        |diagnostics| a_panicked_build(diagnostics).is_some(),
    );
    let panicked = a_panicked_build(&diagnostics)
        .unwrap_or_else(|| panic!("the panicked build is reported: {diagnostics:?}"));
    assert!(
        panicked.words.contains(
            "panicked: PanickingLifecycle: injected panic in the build of level 2; the node runs \
             on its previous value"
        ),
        "the report carries the panic: {panicked:?}"
    );
    assert_eq!(panicked.recovery, DiagnosticRecoveryOnTheWire::FixTheCode);
    let frames_at_the_failure = levels_the_frames_of_the_panicking_node_read().len();
    let levels = once_it_holds(levels_the_frames_of_the_panicking_node_read, |levels| {
        levels.len() > frames_at_the_failure + 3
    });
    assert!(
        levels.len() > frames_at_the_failure + 3,
        "frames keep running: {levels:?}"
    );
    assert_eq!(
        levels.last(),
        Some(&1),
        "the frames run on the value built before the panic"
    );

    set_the_input(
        &played.stream,
        played.node,
        "panic_in_its_build",
        json!(false),
    );
    let recovered = once_it_holds(the_last_level_read, |level| *level == Some(2));
    assert_eq!(
        recovered,
        Some(2),
        "the next build that does not panic lands"
    );
    let diagnostics = once_it_holds(
        || played.stream.diagnostics_of_its_nodes(),
        |diagnostics| diagnostics.is_empty(),
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// The lines the lifecycle probe appended to `path`.
fn the_lines_appended_to(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The lines of `lines` that record a build or a drop, in order.
fn the_builds_and_drops(lines: &[String]) -> Vec<&str> {
    lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("BUILT:") || line.starts_with("DROPPED:"))
        .collect()
}

/// A playing stream of one lifecycle probe appending to `probe_file` up to 3 frames, once it has
/// appended them.
fn a_playing_lifecycle_probe(project_directory: &Path, probe_file: &Path) -> AStreamOfOneNode {
    let played = a_playing_stream_of_one_node(
        project_directory,
        the_type_of_the_lifecycle_probe(),
        json!({ "max_iterations": 3, "output_path": probe_file.to_str().unwrap() }),
    );
    let lines = once_it_holds(
        || the_lines_appended_to(probe_file),
        |lines| lines.iter().any(|line| line == "FRAME:3"),
    );
    assert_eq!(
        the_builds_and_drops(&lines),
        ["BUILT:3"],
        "one value is built: {lines:?}"
    );
    played
}

/// Mental-revert: a set that rebuilt the node would build its value again from the old inputs
/// first; a removal that kept what the node built would leave the value of 5 undropped.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn removing_the_probe_drops_every_value_it_built() {
    let project_directory = tempfile::tempdir().unwrap();
    let probe_file = project_directory.path().join("lifecycle_probe.txt");
    let played = a_playing_lifecycle_probe(project_directory.path(), &probe_file);

    set_the_input(&played.stream, played.node, "max_iterations", json!(5));
    let lines = once_it_holds(
        || the_lines_appended_to(&probe_file),
        |lines| lines.iter().any(|line| line == "DROPPED:3"),
    );
    assert_eq!(
        the_builds_and_drops(&lines),
        ["BUILT:3", "BUILT:5", "DROPPED:3"],
        "the set builds once more and drops the value it replaces: {lines:?}"
    );

    played
        .stream
        .remove_node(&played.node_run_id)
        .expect("the node is removed while the stream plays");
    let lines = once_it_holds(
        || the_lines_appended_to(&probe_file),
        |lines| lines.iter().any(|line| line == "DROPPED:5"),
    );
    std::thread::sleep(Duration::from_millis(200));
    let after = the_lines_appended_to(&probe_file);
    assert_eq!(
        the_builds_and_drops(&after),
        ["BUILT:3", "BUILT:5", "DROPPED:3", "DROPPED:5"],
        "removing the node drops every value it built, and builds nothing after: {lines:?}"
    );
}

/// Mental-revert: a stop that kept the node, or what its body made, would leave the value it
/// built undropped.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn stopping_the_stream_drops_every_value_the_probe_built() {
    let project_directory = tempfile::tempdir().unwrap();
    let probe_file = project_directory.path().join("lifecycle_probe.txt");
    let played = a_playing_lifecycle_probe(project_directory.path(), &probe_file);

    played.stream.stop().expect("the stream stops");

    let lines = once_it_holds(
        || the_lines_appended_to(&probe_file),
        |lines| lines.iter().any(|line| line == "DROPPED:3"),
    );
    assert_eq!(
        the_builds_and_drops(&lines),
        ["BUILT:3", "DROPPED:3"],
        "stopping the stream drops the value the node built: {lines:?}"
    );
}
