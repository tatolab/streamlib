// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Value wire into a setting a node reads at setup rebuilds that node once with each value
//! it carries while its stream runs, a burst of them once with the last; a value the node does
//! not build with leaves it built with its old one; and the value the wire last carried is the
//! one the node is built with at the stream's next start.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use serial_test::serial;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use super::node_rebuild::A_BURST_OF_VALUES_ON_A_WIRE_INTO_A_SETTING_SETTLES_WITHIN;
use crate::core::graph::{InputLinkPortRef, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::NodeSpec;
use crate::core::runtime::Runner;
use crate::core::runtime::a_node_reading_its_settings_at_setup_for_a_test::{
    A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH, THE_SOURCES_OUTPUT, forget_every_setup_run,
    the_node_type_reading_its_settings_at_setup, the_node_type_whose_output_a_test_writes,
    the_settings_each_setup_read, write_from_the_source,
};
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::operations::RuntimeOperations;

/// The longest a test waits for a rebuild a wire asked for.
const A_REBUILD_IS_RUN_WITHIN: Duration = Duration::from_secs(10);

/// A started stream holding a source whose Integer output is wired into the setting
/// `kernel_size` of a node reading its settings at setup, that setting stored as 3: the
/// runner, the stream, the source's UUID and the reader's.
fn a_running_stream_with_a_wire_into_a_setting(
    project_directory: &std::path::Path,
) -> (Arc<Runner>, Arc<LoadedStreamInThisRuntime>, Uuid, Uuid) {
    let reader_type = the_node_type_reading_its_settings_at_setup();
    let source_type = the_node_type_whose_output_a_test_writes();
    forget_every_setup_run();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads");
    let source = Uuid::new_v4();
    let reader = Uuid::new_v4();
    let source_run_id: NodeRunId = stream
        .add_node(NodeSpec::new(source_type, Value::Null).with_node_uuid(source))
        .expect("the source is added");
    let reader_run_id = stream
        .add_node(NodeSpec::new(reader_type, Value::Null).with_node_uuid(reader))
        .expect("the reader is added");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(stream.apply_a_stream_graph_operation_live(
            StreamGraphOperation::SetNodeInputValue {
                node: reader,
                input: "kernel_size".to_owned(),
                member: None,
                from: None,
                to: Some(json!(3)),
            },
        ))
        .expect("a stored kernel size is set");
    stream
        .connect(
            OutputLinkPortRef::new(source_run_id, THE_SOURCES_OUTPUT),
            InputLinkPortRef::new(reader_run_id, "kernel_size"),
        )
        .expect("a Value wire into a setting read at setup connects");
    start(&stream);
    (runner, stream, source, reader)
}

fn start(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");
}

/// The settings each setup of `reader` read once it has run `setups`, waiting for them.
fn once_it_has_run(reader: Uuid, setups: usize) -> Vec<Map<String, Value>> {
    let deadline = Instant::now() + A_REBUILD_IS_RUN_WITHIN;
    loop {
        let read = the_settings_each_setup_read(reader);
        if read.len() >= setups || Instant::now() >= deadline {
            return read;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn the_kernel_sizes(setups: &[Map<String, Value>]) -> Vec<Value> {
    setups
        .iter()
        .map(|settings| settings.get("kernel_size").cloned().unwrap_or(Value::Null))
        .collect()
}

/// Long enough for any rebuild a wire asked for to have run.
fn after_any_rebuild_would_have_run() {
    std::thread::sleep(A_BURST_OF_VALUES_ON_A_WIRE_INTO_A_SETTING_SETTLES_WITHIN * 10);
}

/// Mental-revert: refusing the link into the setting fails the connect; reading the wire's
/// values without rebuilding leaves the node built with 3.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn each_value_a_wire_carries_into_a_setting_rebuilds_its_node_once_with_it() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, source, reader) =
        a_running_stream_with_a_wire_into_a_setting(project_directory.path());

    write_from_the_source(source, 5).expect("the source writes");
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 2)),
        [json!(3), json!(5)]
    );
    write_from_the_source(source, 7).expect("the source writes");
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 3)),
        [json!(3), json!(5), json!(7)]
    );
    write_from_the_source(source, 7).expect("the source writes");
    after_any_rebuild_would_have_run();
    assert_eq!(
        the_settings_each_setup_read(reader).len(),
        3,
        "a value its node is built with rebuilds nothing"
    );
    stream.stop().expect("the stream stops");
}

/// Mental-revert: rebuilding on each value the wire carries runs a setup for each of the four.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_burst_of_values_on_a_wire_into_a_setting_rebuilds_its_node_once_with_the_last() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, source, reader) =
        a_running_stream_with_a_wire_into_a_setting(project_directory.path());

    for kernel_size in [4, 5, 6, 9] {
        write_from_the_source(source, kernel_size).expect("the source writes");
    }
    once_it_has_run(reader, 2);
    after_any_rebuild_would_have_run();
    assert_eq!(
        the_kernel_sizes(&the_settings_each_setup_read(reader)),
        [json!(3), json!(9)]
    );
    stream.stop().expect("the stream stops");
}

/// Mental-revert: leaving a node its rebuild refused in its failed state never builds it with
/// 3 again, and retrying the refused value rebuilds it over and over.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_value_on_a_wire_its_node_does_not_build_with_leaves_it_built_with_its_old_one() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, source, reader) =
        a_running_stream_with_a_wire_into_a_setting(project_directory.path());

    write_from_the_source(source, A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH).expect("the source writes");
    once_it_has_run(reader, 3);
    after_any_rebuild_would_have_run();
    assert_eq!(
        the_kernel_sizes(&the_settings_each_setup_read(reader)),
        [
            json!(3),
            json!(A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH),
            json!(3)
        ],
        "the refused build, then the old value's"
    );
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs with its old value");

    write_from_the_source(source, 5).expect("the source writes");
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 4)).last(),
        Some(&json!(5)),
        "another value is taken"
    );
    stream.stop().expect("the stream stops");
}

/// Mental-revert: building a node at start with its stored value alone builds it with 3, then
/// rebuilds it with 6 once its source writes again.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn the_value_a_wire_last_carried_into_a_setting_builds_its_node_at_the_next_start() {
    let project_directory = tempfile::tempdir().unwrap();
    let (_runner, stream, source, reader) =
        a_running_stream_with_a_wire_into_a_setting(project_directory.path());

    write_from_the_source(source, 6).expect("the source writes");
    once_it_has_run(reader, 2);
    stream
        .stop_playback_keeping_the_graph()
        .expect("the playback stops, its graph held");
    start(&stream);

    let setups = once_it_has_run(reader, 3);
    assert_eq!(the_kernel_sizes(&setups), [json!(3), json!(6), json!(6)]);
    stream.stop().expect("the stream stops");
}
