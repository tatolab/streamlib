// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node that paces itself (`execution = manual`), written as one function, run in a stream: a
//! value set on its input while the stream plays is taken at its next tick, closing the
//! publishing it replaces, with its body run once; and a tick never waits on a build.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use crate::core::nodes::NodeSpec;
use crate::core::runtime::a_node_pacing_itself_written_as_one_function_for_a_test::{
    WhatTheNodePacingItselfDid, forget_what_the_node_pacing_itself_did, hold_the_publishing_builds,
    the_node_type_pacing_itself, what_the_node_pacing_itself_did,
};
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::operations::RuntimeOperations;
use crate::core::runtime::{LoadedStreamInThisRuntime, Runner};

/// The longest a test waits for what it waits on.
const WAITED_ON_WITHIN: Duration = Duration::from_secs(10);

/// The longest a set may take while a build is held, well under what a set waiting on the
/// held build would take.
const A_SET_RETURNS_WITHIN: Duration = Duration::from_secs(2);

/// A stream holding one node pacing itself, started.
struct AStreamOfANodePacingItself {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    node: Uuid,
}

fn a_started_stream_of_a_node_pacing_itself(
    project_directory: &std::path::Path,
) -> AStreamOfANodePacingItself {
    let node_type = the_node_type_pacing_itself();
    forget_what_the_node_pacing_itself_did();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads");
    let node = Uuid::new_v4();
    stream
        .add_node(NodeSpec::new(node_type, Value::Null).with_node_uuid(node))
        .expect("the node pacing itself is added");
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");
    AStreamOfANodePacingItself {
        _runner: runner,
        stream,
        node,
    }
}

fn set_the_gain(stream: &LoadedStreamInThisRuntime, node: Uuid, gain: i32) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(stream.apply_a_stream_graph_operation_live(
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

/// What the node did once `settled` holds of it, or when the wait runs out.
fn what_the_node_did_once_it_holds(
    settled: impl Fn(&WhatTheNodePacingItselfDid) -> bool,
) -> WhatTheNodePacingItselfDid {
    let deadline = Instant::now() + WAITED_ON_WITHIN;
    loop {
        let did = what_the_node_pacing_itself_did();
        if settled(&did) || Instant::now() >= deadline {
            return did;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Mental-revert: a node function refused `execution = manual` does not compile; a set that did
/// not reach the scope would open no publishing of 5 and close none of 1; one that rebuilt the
/// node would run its body twice.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_value_set_while_the_stream_plays_is_taken_at_the_next_tick_and_runs_the_body_once() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_started_stream_of_a_node_pacing_itself(project_directory.path());
    what_the_node_did_once_it_holds(|did| did.gains_opened == [1]);

    set_the_gain(&played.stream, played.node, 5);

    let after = what_the_node_did_once_it_holds(|did| did.gains_closed == [1]);
    assert_eq!(after.gains_opened, [1, 5], "{after:?}");
    assert_eq!(
        after.gains_closed,
        [1],
        "the replaced publishing is closed once the tick takes the new one: {after:?}"
    );
    assert_eq!(after.bodies_run, 1, "the body runs once: {after:?}");
    played
        .stream
        .wait_until_every_node_is_running(Duration::from_secs(1))
        .expect("the node keeps running");

    played.stream.stop().expect("the stream stops");
    let stopped = what_the_node_did_once_it_holds(|did| did.gains_closed == [1, 5]);
    assert_eq!(stopped.gains_closed, [1, 5], "{stopped:?}");
}

/// Mental-revert: a tick that waited on the held build would hold the node while it waits, so
/// the second set, which writes through the node, would wait on the build too.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_tick_never_waits_on_a_build() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_started_stream_of_a_node_pacing_itself(project_directory.path());
    what_the_node_did_once_it_holds(|did| did.gains_opened == [1]);
    let builds_held = hold_the_publishing_builds();
    set_the_gain(&played.stream, played.node, 7);
    let building_7 = what_the_node_did_once_it_holds(|did| did.builds_started == 2);
    assert_eq!(
        building_7.builds_started, 2,
        "the build of 7 starts: {building_7:?}"
    );
    // Several ticks pass while the build of 7 is held.
    std::thread::sleep(Duration::from_millis(300));

    let setting_9 = Instant::now();
    set_the_gain(&played.stream, played.node, 9);
    let set_in = setting_9.elapsed();
    assert!(
        set_in < A_SET_RETURNS_WITHIN,
        "a set while a build is held returns at once, not after the build: {set_in:?}"
    );
    played
        .stream
        .wait_until_every_node_is_running(Duration::from_secs(1))
        .expect("the node keeps running while its build is held");
    let while_held = what_the_node_pacing_itself_did();
    assert_eq!(
        while_held.gains_opened,
        [1],
        "nothing opens while the build is held: {while_held:?}"
    );

    drop(builds_held);
    let after = what_the_node_did_once_it_holds(|did| did.gains_closed == [1]);
    assert_eq!(
        after.gains_opened,
        [1, 9],
        "the build that read 7 is thrown away and 9 built: {after:?}"
    );
    assert_eq!(after.gains_closed, [1], "{after:?}");
    assert_eq!(after.bodies_run, 1, "{after:?}");
}
