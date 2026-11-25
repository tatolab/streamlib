// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value set on a setting a node reads at setup rebuilds that one node while its stream runs:
//! once for every setting set before the rebuild, refused in the node's words when it does not
//! build with the value, and read at the start of a stream that was stopped when it was set. A
//! change in how many slots a pin group holds, by a link or by its count, rebuilds it the same way.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::error::Result;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
use crate::core::nodes::NodeSpec;
use crate::core::runtime::Runner;
use crate::core::runtime::a_node_reading_its_settings_at_setup_for_a_test::{
    A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH, THE_LAYERED_NODES_PIN_GROUP, THE_SOURCES_OUTPUT,
    THE_TAPPED_NODES_PIN_GROUP, forget_every_setup_run, the_node_type_built_for_its_layers,
    the_node_type_built_for_its_taps, the_node_type_reading_its_settings_at_setup,
    the_node_type_whose_output_a_test_writes, the_settings_each_setup_read,
};
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::operations::RuntimeOperations;

/// A stream of the test project at `project_directory` holding one fixture node, its kernel
/// size 3, started when `started` is.
fn a_stream_holding_a_node_reading_its_settings_at_setup(
    project_directory: &std::path::Path,
    started: bool,
) -> (Arc<Runner>, Arc<LoadedStreamInThisRuntime>, Uuid) {
    let node_type = the_node_type_reading_its_settings_at_setup();
    forget_every_setup_run();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads");
    let node_uuid = Uuid::new_v4();
    stream
        .add_node(NodeSpec::new(node_type, Value::Null).with_node_uuid(node_uuid))
        .expect("the node is added");
    set(&stream, node_uuid, "kernel_size", json!(3)).expect("a stored kernel size is set");
    if started {
        stream.start().expect("the stream starts");
        stream
            .wait_until_every_node_is_running(Duration::from_secs(30))
            .expect("the node runs");
    }
    (runner, stream, node_uuid)
}

fn on_the_tests_own_runtime<T>(operation: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(operation)
}

fn set(stream: &LoadedStreamInThisRuntime, node: Uuid, input: &str, to: Value) -> Result<()> {
    on_the_tests_own_runtime(stream.apply_a_stream_graph_operation_live(
        StreamGraphOperation::SetNodeInputValue {
            node,
            input: input.to_owned(),
            member: None,
            from: None,
            to: Some(to),
        },
    ))
    .map(|_| ())
}

fn rebuild(stream: &LoadedStreamInThisRuntime) -> Result<Vec<Uuid>> {
    on_the_tests_own_runtime(
        RuntimeOperations::rebuild_each_node_a_setting_read_at_setup_changed(stream),
    )
}

fn the_components_the_graph_renders_for(stream: &LoadedStreamInThisRuntime, node: Uuid) -> Value {
    let graph = stream.to_json().expect("the graph serializes");
    graph["nodes"]
        .as_array()
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|rendered| rendered["id"] == node.to_string())
        })
        .map(|rendered| rendered["components"].clone())
        .expect("the graph holds the node")
}

#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_setting_read_at_setup_set_while_the_stream_runs_rebuilds_its_node_with_it() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, node) =
        a_stream_holding_a_node_reading_its_settings_at_setup(project_directory.path(), true);

    set(&stream, node, "kernel_size", json!(5)).expect("the running node takes the kernel size");
    assert_eq!(
        the_settings_each_setup_read(node).len(),
        1,
        "a set leaves its node to rebuild and rebuilds nothing yet"
    );
    let rebuilt = rebuild(&stream).expect("the node builds with the kernel size");

    assert_eq!(rebuilt, vec![node]);
    let setups = the_settings_each_setup_read(node);
    assert_eq!(setups.len(), 2, "{setups:?}");
    assert_eq!(setups[1].get("kernel_size"), Some(&json!(5)));
    assert_eq!(
        the_components_the_graph_renders_for(&stream, node)["state"],
        json!("Running")
    );
    assert_eq!(
        stream.status(),
        crate::core::runtime::RuntimeStatus::Started,
        "the stream keeps running"
    );
    assert_eq!(
        rebuild(&stream).expect("nothing is left to rebuild"),
        Vec::<Uuid>::new()
    );
    stream.stop().expect("the stream stops");
}

#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn three_settings_set_before_a_rebuild_rebuild_their_node_once() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, node) =
        a_stream_holding_a_node_reading_its_settings_at_setup(project_directory.path(), true);

    set(&stream, node, "kernel_size", json!(5)).unwrap();
    set(&stream, node, "width", json!(1280)).unwrap();
    set(&stream, node, "height", json!(720)).unwrap();
    let rebuilt = rebuild(&stream).expect("the node builds with the three settings");

    assert_eq!(rebuilt, vec![node]);
    let setups = the_settings_each_setup_read(node);
    assert_eq!(
        setups.len(),
        2,
        "one setup at the start, one rebuild: {setups:?}"
    );
    assert_eq!(
        Value::Object(setups[1].clone()),
        json!({"kernel_size": 5, "width": 1280, "height": 720})
    );
    stream.stop().expect("the stream stops");
}

#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_setting_its_node_cannot_build_with_is_refused_in_the_nodes_words_and_its_old_value_builds_it_again()
 {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, node) =
        a_stream_holding_a_node_reading_its_settings_at_setup(project_directory.path(), true);

    set(
        &stream,
        node,
        "kernel_size",
        json!(A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH),
    )
    .unwrap();
    let refusal = rebuild(&stream)
        .expect_err("a kernel size the node cannot build with is refused")
        .to_string();

    assert!(
        refusal.contains("did not build with them")
            && refusal.contains("blurs nothing")
            && refusal.contains("`nodereadingitssettingsatsetup`"),
        "{refusal}"
    );
    let components = the_components_the_graph_renders_for(&stream, node);
    assert_eq!(components["state"], json!("Error"), "{components}");
    assert!(
        components["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("blurs nothing")),
        "{components}"
    );

    set(&stream, node, "kernel_size", json!(3)).unwrap();
    assert_eq!(
        rebuild(&stream).expect("its old kernel size builds it"),
        vec![node]
    );
    assert_eq!(
        the_components_the_graph_renders_for(&stream, node)["state"],
        json!("Running")
    );
    let setups = the_settings_each_setup_read(node);
    assert_eq!(
        setups
            .last()
            .and_then(|settings| settings.get("kernel_size")),
        Some(&json!(3))
    );
    stream.stop().expect("the stream stops");
}

#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_setting_set_on_a_stream_not_started_is_read_at_its_start() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, node) =
        a_stream_holding_a_node_reading_its_settings_at_setup(project_directory.path(), false);

    set(&stream, node, "kernel_size", json!(5)).unwrap();
    assert_eq!(
        rebuild(&stream).expect("a stream not started rebuilds nothing"),
        Vec::<Uuid>::new()
    );
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");

    let setups = the_settings_each_setup_read(node);
    assert_eq!(setups.len(), 1, "{setups:?}");
    assert_eq!(setups[0].get("kernel_size"), Some(&json!(5)));
    stream.stop().expect("the stream stops");
}

/// A node left to rebuild when its stream stops waits on no rebuild once the stream starts
/// again: the start builds it from the value set, and a rebuild after it rebuilds nothing.
///
/// Mental-revert: keeping the nodes left to rebuild across the stop rebuilds the node again
/// after the start built it.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_node_left_to_rebuild_when_its_stream_stops_is_built_once_by_the_next_start() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, node) =
        a_stream_holding_a_node_reading_its_settings_at_setup(project_directory.path(), true);
    set(&stream, node, "kernel_size", json!(5)).expect("the running node takes the kernel size");
    let left_to_rebuild = stream
        .the_live_nodes()
        .into_iter()
        .find(|held| held.node_uuid == node)
        .expect("the stream holds the node")
        .node_run_id;
    assert!(stream.is_left_to_rebuild(&left_to_rebuild));

    stream
        .stop_playback_keeping_the_graph()
        .expect("the stream's playback stops");
    assert!(!stream.is_left_to_rebuild(&left_to_rebuild));
    stream.start().expect("the stream starts again");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");

    assert_eq!(
        rebuild(&stream).expect("nothing is left to rebuild"),
        Vec::<Uuid>::new()
    );
    let setups = the_settings_each_setup_read(node);
    assert_eq!(setups.len(), 2, "one setup at each start: {setups:?}");
    assert_eq!(setups[1].get("kernel_size"), Some(&json!(5)));
    stream.stop().expect("the stream stops");
}

/// Mental-revert: without a node left to rebuild when a link changes how many slots its pin
/// groups hold, the rebuild after each link rebuilds nothing, and the node runs built for the
/// layers it held before.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_link_changing_how_many_slots_a_running_node_holds_rebuilds_it() {
    let project_directory = tempfile::tempdir().unwrap();
    forget_every_setup_run();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    let layered = Uuid::new_v4();
    let layered_run_id = stream
        .add_node(
            NodeSpec::new(the_node_type_built_for_its_layers(), Value::Null)
                .with_node_uuid(layered),
        )
        .expect("the layered node is added");
    let source_run_id = stream
        .add_node(NodeSpec::new(
            the_node_type_whose_output_a_test_writes(),
            Value::Null,
        ))
        .expect("the source is added");
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the nodes run");

    let link = stream
        .connect(
            OutputLinkPortRef::new(source_run_id, THE_SOURCES_OUTPUT),
            InputLinkPortRef::new(
                layered_run_id,
                format!("{THE_LAYERED_NODES_PIN_GROUP}.0.layer"),
            ),
        )
        .expect("an Integer links into an Integer layer");
    assert_eq!(
        rebuild(&stream).expect("the node builds for two slots"),
        vec![layered],
        "a link into its empty slot gives it another"
    );

    stream.disconnect(&link).expect("the link is taken out");
    assert_eq!(
        rebuild(&stream).expect("the node builds for one slot"),
        vec![layered],
        "the last slot's link taken out gives its empty slot back"
    );
    assert_eq!(the_settings_each_setup_read(layered).len(), 3);
    stream.stop().expect("the stream stops");
}

/// The names of the outputs the graph renders for `node`.
fn the_outputs_the_graph_renders_for(
    stream: &LoadedStreamInThisRuntime,
    node: Uuid,
) -> Vec<String> {
    let graph = stream.to_json().expect("the graph serializes");
    graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|rendered| rendered["id"] == node.to_string())
        .map(|rendered| {
            rendered["ports"]["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|output| output["name"].as_str().map(str::to_owned))
                .collect()
        })
        .expect("the graph holds the node")
}

#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_pin_groups_count_set_while_the_stream_runs_rebuilds_its_node_once_for_its_slots() {
    let project_directory = tempfile::tempdir().unwrap();
    forget_every_setup_run();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    let tapped = Uuid::new_v4();
    stream
        .add_node(
            NodeSpec::new(the_node_type_built_for_its_taps(), Value::Null).with_node_uuid(tapped),
        )
        .expect("the tapped node is added");
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");

    set(&stream, tapped, THE_TAPPED_NODES_PIN_GROUP, json!(3))
        .expect("the running node takes a count of 3");
    assert_eq!(
        the_settings_each_setup_read(tapped).len(),
        1,
        "a set count leaves its node to rebuild and rebuilds nothing yet"
    );
    assert_eq!(
        rebuild(&stream).expect("the node builds for three slots"),
        vec![tapped]
    );

    let setups = the_settings_each_setup_read(tapped);
    assert_eq!(setups.len(), 2, "rebuilt once: {setups:?}");
    assert_eq!(setups[1].get(THE_TAPPED_NODES_PIN_GROUP), Some(&json!(3)));
    assert_eq!(
        the_outputs_the_graph_renders_for(&stream, tapped),
        ["taps.0.tap", "taps.1.tap", "taps.2.tap"]
    );
    assert_eq!(
        the_components_the_graph_renders_for(&stream, tapped)["state"],
        json!("Running")
    );
    assert_eq!(
        rebuild(&stream).expect("nothing is left to rebuild"),
        Vec::<Uuid>::new()
    );
    stream.stop().expect("the stream stops");
}
