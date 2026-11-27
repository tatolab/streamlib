// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node that paces itself (`execution = manual`), written as one function, run in a stream: a
//! value set on its input while the stream plays opens a publishing of it and closes the one it
//! replaces, with its body run once; and a set never waits on a build.

use std::time::Duration;

use serde_json::{Value, json};
use serial_test::serial;

use crate::core::runtime::a_node_pacing_itself_written_as_one_function_for_a_test::{
    WhatTheNodePacingItselfDid, forget_what_the_node_pacing_itself_did, hold_the_publishing_builds,
    the_node_type_pacing_itself, what_the_node_pacing_itself_did,
};
use crate::core::runtime::a_stream_of_one_node_for_a_test::{
    AStreamOfOneNode, a_stream_of_one_node, once_it_holds, set_the_input, start,
};

/// The longest a set may take while a build is held, well under what a set waiting on the
/// held build would take.
const A_SET_RETURNS_WITHIN: Duration = Duration::from_secs(2);

/// A stream holding one node pacing itself, its gain at its default, 1, started.
fn a_started_stream_of_a_node_pacing_itself(
    project_directory: &std::path::Path,
) -> AStreamOfOneNode {
    let node_type = the_node_type_pacing_itself();
    forget_what_the_node_pacing_itself_did();
    let played = a_stream_of_one_node(project_directory, node_type, Value::Null);
    start(&played.stream);
    played
}

fn set_the_gain(played: &AStreamOfOneNode, gain: i32) {
    set_the_input(&played.stream, played.node, "gain", json!(gain));
}

/// What the node did once `settled` holds of it, or when the wait runs out.
fn what_the_node_did_once_it_holds(
    settled: impl Fn(&WhatTheNodePacingItselfDid) -> bool,
) -> WhatTheNodePacingItselfDid {
    once_it_holds(what_the_node_pacing_itself_did, settled)
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
fn a_value_set_while_the_stream_plays_replaces_the_publishing_and_runs_the_body_once() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_started_stream_of_a_node_pacing_itself(project_directory.path());
    what_the_node_did_once_it_holds(|did| did.gains_opened == [1]);

    set_the_gain(&played, 5);

    let after = what_the_node_did_once_it_holds(|did| did.gains_closed == [1]);
    assert_eq!(after.gains_opened, [1, 5], "{after:?}");
    assert_eq!(
        after.gains_closed,
        [1],
        "the replaced publishing is closed once the new one is built: {after:?}"
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

/// Set the gain on a thread of its own, failing the test, its builds let go as it unwinds, when
/// the set has not returned within `A_SET_RETURNS_WITHIN`, rather than waiting on it forever.
fn set_the_gain_or_fail_if_it_waits(played: &AStreamOfOneNode, gain: i32) {
    let (returned, set_returned) = std::sync::mpsc::channel();
    let stream = std::sync::Arc::clone(&played.stream);
    let node = played.node;
    std::thread::spawn(move || {
        set_the_input(&stream, node, "gain", json!(gain));
        let _ = returned.send(());
    });
    assert!(
        set_returned.recv_timeout(A_SET_RETURNS_WITHIN).is_ok(),
        "a set of {gain} while a build is held returns at once, not after the build"
    );
}

/// Mental-revert: a write to the scope that waited for the build it starts would hold the node
/// until the held build is let go, so the second set would wait on it.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_set_never_waits_on_a_build_of_a_node_pacing_itself() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_started_stream_of_a_node_pacing_itself(project_directory.path());
    what_the_node_did_once_it_holds(|did| did.gains_opened == [1]);
    let builds_held = hold_the_publishing_builds();
    set_the_gain_or_fail_if_it_waits(&played, 7);
    let building_7 = what_the_node_did_once_it_holds(|did| did.builds_started == 2);
    assert_eq!(
        building_7.builds_started, 2,
        "the build of 7 starts: {building_7:?}"
    );
    std::thread::sleep(Duration::from_millis(300));

    set_the_gain_or_fail_if_it_waits(&played, 9);
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
