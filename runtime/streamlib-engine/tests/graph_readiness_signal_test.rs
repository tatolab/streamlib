// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::{Duration, Instant};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

use serial_test::serial;
use streamlib::sdk::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType,
    PortDescriptor,
};
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::Runner;

/// An empty stream loaded into `runner`, its project in `project_directory`.
fn an_empty_stream_loaded_into(
    runner: &Runner,
    project_directory: &std::path::Path,
) -> std::sync::Arc<streamlib::sdk::runtime::LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads")
}

/// Short enough that a test waiting it out stays quick, long enough that
/// reaching it means nothing transitioned rather than that the machine stalled.
const SHORT_TIMEOUT: Duration = Duration::from_millis(250);

fn register_test_type(short: &str) -> NodeClassImportPath {
    let import_path = NodeClassImportPath::new(format!("{}::{short}", module_path!())).unwrap();
    let class_short_name = NodeClassShortName::new(short).unwrap();
    let descriptor = NodeDescriptor::new(
        class_short_name,
        import_path.clone(),
        "graph readiness signal test",
    )
    .with_input(PortDescriptor::new("bags_from_upstream", "", false, PinDeclaration::exactly(PinType::Texture { pixel_formats: Vec::new() })))
    .with_output(PortDescriptor::new("bags_to_downstream", "", false, PinDeclaration::exactly(PinType::Texture { pixel_formats: Vec::new() })));
    let _ = NODE_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

#[test]
#[serial]
fn a_graph_with_no_nodes_has_nothing_to_wait_for() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());

    stream
        .wait_until_every_node_is_running(SHORT_TIMEOUT)
        .expect("an empty graph is up by definition");
}

#[test]
#[serial]
fn a_graph_that_was_never_started_is_not_reported_as_running() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
    let node_run_id = stream
        .add_node(NodeSpec::new(
            register_test_type("NeverStarted"),
            serde_json::json!({}),
        ))
        .expect("a registered type adds cleanly");

    let failure = stream
        .wait_until_every_node_is_running(SHORT_TIMEOUT)
        .expect_err("a graph that never started must not report itself running");

    let reported = failure.to_string();
    assert!(
        reported.contains(node_run_id.as_str()),
        "the timeout must name the node it gave up on, got: {reported}"
    );
    assert!(
        reported.contains("Pending"),
        "the timeout must say what state it gave up in, got: {reported}"
    );
}

#[test]
#[serial]
fn a_failure_behind_a_node_that_never_starts_is_still_reported() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
    stream
        .add_node(NodeSpec::new(
            register_test_type("StaysPending"),
            serde_json::json!({}),
        ))
        .expect("a registered type adds cleanly");
    // A registry miss lands the node in `Error` without spawning anything.
    let _ = stream.add_node(NodeSpec::new(
        NodeClassImportPath::new("ghost_package::BehindTheSlowOne").unwrap(),
        serde_json::json!({}),
    ));

    let reported = stream
        .wait_until_every_node_is_running(SHORT_TIMEOUT)
        .expect_err("a graph holding a failed node is not running")
        .to_string();

    assert!(
        reported.contains("=Error"),
        "the failed node must be in the report even when the wait gave up on another, \
         got: {reported}"
    );
}

#[test]
#[serial]
fn a_node_that_failed_ends_the_wait_without_burning_the_timeout() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
    let _ = stream.add_node(NodeSpec::new(
        NodeClassImportPath::new("ghost_package::NotRegistered").unwrap(),
        serde_json::json!({}),
    ));

    let began_waiting = Instant::now();
    let failure = stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect_err("a graph holding a failed node is not running");
    let waited = began_waiting.elapsed();

    assert!(
        failure.to_string().contains("Error rather than Running"),
        "the failure must distinguish a broken node from a slow one, got: {failure}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "waited {waited:?} on a node that was already Error — the wait is not \
         short-circuiting on failure"
    );
}
