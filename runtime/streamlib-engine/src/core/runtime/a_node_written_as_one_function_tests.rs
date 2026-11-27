// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node written as one function, run in a stream: a value set on its input while the stream
//! plays builds the computed value that reads it once more and runs its body no second time; a
//! value it holds before the stream starts is the one its first build reads; no frame runs
//! before its computed value is first built; and removing it drops every value it built, with no
//! build after.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use crate::core::graph::NodeRunId;
use crate::core::nodes::NodeSpec;
use crate::core::runtime::a_node_written_as_one_function_for_a_test::{
    WhatTheNodeFunctionRan, forget_what_the_node_function_ran, hold_the_builds,
    the_node_type_written_as_one_function, what_the_node_function_ran,
};
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::operations::RuntimeOperations;
use crate::core::runtime::{LoadedStreamInThisRuntime, Runner};

/// The longest a test waits for what it waits on.
const WAITED_ON_WITHIN: Duration = Duration::from_secs(10);

/// A stream holding one node written as one function.
struct AStreamOfANodeFunction {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    node: Uuid,
    node_run_id: NodeRunId,
}

/// A stream holding one node written as one function, holding `input_values`: `Value::Null`
/// leaves its `gain` at its default, 1.
fn a_stream_of_a_node_function(
    project_directory: &std::path::Path,
    input_values: Value,
) -> AStreamOfANodeFunction {
    let node_type = the_node_type_written_as_one_function();
    forget_what_the_node_function_ran();
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
        .expect("the node function is added");
    AStreamOfANodeFunction {
        _runner: runner,
        stream,
        node,
        node_run_id,
    }
}

fn start(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");
}

fn on_the_tests_own_runtime<T>(operation: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(operation)
}

fn set_the_gain(stream: &LoadedStreamInThisRuntime, node: Uuid, gain: i32) {
    on_the_tests_own_runtime(stream.apply_a_stream_graph_operation_live(
        StreamGraphOperation::SetNodeInputValue {
            node,
            input: "gain".to_owned(),
            member: None,
            from: None,
            to: Some(json!(gain)),
        },
    ))
    .expect("a gain is set while the stream plays");
}

/// What the node ran once `settled` holds of it, or when the wait runs out.
fn what_the_node_ran_once_it_holds(
    settled: impl Fn(&WhatTheNodeFunctionRan) -> bool,
) -> WhatTheNodeFunctionRan {
    let deadline = Instant::now() + WAITED_ON_WITHIN;
    loop {
        let ran = what_the_node_function_ran();
        if settled(&ran) || Instant::now() >= deadline {
            return ran;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Mental-revert: had a set rebuilt the node, its body would have run twice and the first value
/// been dropped with the node's teardown before the second was built; had the set not reached
/// the scope, no frame would read 5.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_value_set_while_the_stream_plays_builds_what_read_it_once_and_runs_the_body_once() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), Value::Null);
    start(&played.stream);
    let before = what_the_node_ran_once_it_holds(|ran| ran.gain_the_latest_frame_read == Some(1));
    assert_eq!(before.gain_the_latest_frame_read, Some(1), "{before:?}");

    set_the_gain(&played.stream, played.node, 5);

    let after = what_the_node_ran_once_it_holds(|ran| {
        ran.gain_the_latest_frame_read == Some(5) && ran.gains_dropped == [1]
    });
    assert_eq!(after.gain_the_latest_frame_read, Some(5), "{after:?}");
    assert_eq!(after.bodies_run, 1, "the body runs once: {after:?}");
    assert_eq!(after.gains_built_from, [1, 5], "{after:?}");
    assert_eq!(
        after.gains_dropped,
        [1],
        "the replaced value is dropped, and nothing else is: {after:?}"
    );
    played
        .stream
        .wait_until_every_node_is_running(Duration::from_secs(1))
        .expect("the node keeps running");
}

/// Mental-revert: had setup declared each value input at its default instead of the value the
/// node holds, the first build would read 1.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_value_held_before_the_stream_starts_is_the_one_the_first_build_reads() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), json!({ "gain": 3 }));
    start(&played.stream);

    let ran = what_the_node_ran_once_it_holds(|ran| ran.gain_the_latest_frame_read.is_some());
    assert_eq!(ran.gains_built_from, [3], "{ran:?}");
    assert_eq!(ran.gain_the_latest_frame_read, Some(3), "{ran:?}");
    assert_eq!(ran.bodies_run, 1, "{ran:?}");
}

/// Mental-revert: a frame run before the first build lands is counted, and reads a computed
/// value with none, which panics in the frame.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn no_frame_runs_before_the_computed_value_is_first_built() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), Value::Null);
    let builds_held = hold_the_builds();
    start(&played.stream);

    // The node is continuous at 5 ms, so an ungated node runs frames many times over in this.
    std::thread::sleep(Duration::from_millis(200));
    let while_held = what_the_node_function_ran();
    assert_eq!(while_held.bodies_run, 1, "{while_held:?}");
    assert!(
        while_held.gains_built_from.is_empty(),
        "the build is held: {while_held:?}"
    );
    assert_eq!(
        while_held.frames_run, 0,
        "no frame runs before the computed value has its first value: {while_held:?}"
    );

    drop(builds_held);
    let built = what_the_node_ran_once_it_holds(|ran| ran.gain_the_latest_frame_read.is_some());
    assert_eq!(built.gains_built_from, [1], "{built:?}");
    assert_eq!(built.gain_the_latest_frame_read, Some(1), "{built:?}");
}

/// Mental-revert: a scope left running past its node's removal would build the gain set as it
/// was removed after the removal returned, and keep every value it built.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn removing_the_node_drops_every_value_it_built_and_builds_nothing_after() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), Value::Null);
    start(&played.stream);
    what_the_node_ran_once_it_holds(|ran| ran.gain_the_latest_frame_read.is_some());
    let builds_held = hold_the_builds();
    set_the_gain(&played.stream, played.node, 7);
    // The build of 7 is held as the removal starts, and goes while the removal waits on it.
    let letting_go = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(builds_held);
    });

    played
        .stream
        .remove_node(&played.node_run_id)
        .expect("the node is removed while the stream plays");
    let removed = what_the_node_ran_once_it_holds(every_value_built_is_dropped);
    assert!(
        every_value_built_is_dropped(&removed),
        "every value the node built is dropped once it is removed: {removed:?}"
    );
    letting_go.join().expect("the builds are let go");
    std::thread::sleep(Duration::from_millis(400));
    let after = what_the_node_function_ran();

    assert_eq!(
        after.gains_built_from, removed.gains_built_from,
        "nothing is built once the node is removed: {after:?}"
    );
    assert_eq!(after.bodies_run, 1, "{after:?}");
}

fn every_value_built_is_dropped(ran: &WhatTheNodeFunctionRan) -> bool {
    let mut dropped = ran.gains_dropped.clone();
    dropped.sort_unstable();
    let mut built = ran.gains_built_from.clone();
    built.sort_unstable();
    !built.is_empty() && dropped == built
}
