// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node written as one function, run in a stream: a value set on its input while the stream
//! plays builds the computed value that reads it once more and runs its body no second time; a
//! value it holds before the stream starts is the one its first build reads; no frame runs
//! before its computed value is first built; removing it drops every value it built, with no
//! build after; stopping its stream drops every value it built; and a computed value still
//! building its first value, or whose build failed, shows in the stream's diagnostics.

use std::time::Duration;

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticOnTheWire, DiagnosticRecoveryOnTheWire,
};
use uuid::Uuid;

use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::a_node_written_as_one_function_for_a_test::{
    WhatTheNodeFunctionRan, forget_what_the_node_function_ran, hold_the_builds,
    the_node_type_written_as_one_function, what_the_node_function_ran,
};
use crate::core::runtime::a_stream_of_one_node_for_a_test::{
    AStreamOfOneNode, a_stream_of_one_node, once_it_holds, set_the_input, start,
};

/// A stream holding one node written as one function, holding `input_values`: `Value::Null`
/// leaves its `gain` at its default, 1.
fn a_stream_of_a_node_function(
    project_directory: &std::path::Path,
    input_values: Value,
) -> AStreamOfOneNode {
    let node_type = the_node_type_written_as_one_function();
    forget_what_the_node_function_ran();
    a_stream_of_one_node(project_directory, node_type, input_values)
}

fn set_the_gain(stream: &LoadedStreamInThisRuntime, node: Uuid, gain: i32) {
    set_the_input(stream, node, "gain", json!(gain));
}

/// What the node ran once `settled` holds of it, or when the wait runs out.
fn what_the_node_ran_once_it_holds(
    settled: impl Fn(&WhatTheNodeFunctionRan) -> bool,
) -> WhatTheNodeFunctionRan {
    once_it_holds(what_the_node_function_ran, settled)
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
    let building_7 = what_the_node_ran_once_it_holds(|ran| ran.builds_started == 2);
    assert_eq!(
        building_7.builds_started, 2,
        "the build of 7 starts: {building_7:?}"
    );
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

/// Mental-revert: a stop that kept the node, or what its body made, would leave every value it
/// built alive once the stream stops.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn stopping_the_stream_drops_every_value_the_node_built() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), Value::Null);
    start(&played.stream);
    what_the_node_ran_once_it_holds(|ran| ran.gain_the_latest_frame_read.is_some());

    played.stream.stop().expect("the stream stops");

    let stopped = what_the_node_ran_once_it_holds(every_value_built_is_dropped);
    assert!(
        every_value_built_is_dropped(&stopped),
        "every value the node built is dropped once its stream stops: {stopped:?}"
    );
    assert_eq!(stopped.bodies_run, 1, "{stopped:?}");
}

/// The diagnostics the stream's nodes carry once `settled` holds of them, or when the wait runs
/// out.
fn the_diagnostics_once_they_hold(
    stream: &LoadedStreamInThisRuntime,
    settled: impl Fn(&[DiagnosticOnTheWire]) -> bool,
) -> Vec<DiagnosticOnTheWire> {
    once_it_holds(
        || stream.diagnostics_of_its_nodes(),
        |diagnostics| settled(diagnostics),
    )
}

/// The one diagnostic `diagnostics` hold, of `kind`, naming the node's computed value where the
/// test node declares it.
fn the_one_diagnostic_of(
    diagnostics: &[DiagnosticOnTheWire],
    kind: DiagnosticKindOnTheWire,
) -> DiagnosticOnTheWire {
    let [diagnostic] = diagnostics else {
        panic!("one diagnostic of {kind:?}, not {diagnostics:?}");
    };
    assert_eq!(diagnostic.kind, kind, "{diagnostic:?}");
    assert!(
        diagnostic.words.contains("BuiltFromAGain` declared at")
            && diagnostic
                .words
                .contains("a_node_written_as_one_function_for_a_test.rs:"),
        "the words name the computed value: {diagnostic:?}"
    );
    diagnostic.clone()
}

/// Mental-revert: a report read only once a frame runs would show nothing while the first build
/// is held, since no frame runs before it lands.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_computed_value_whose_first_build_is_pending_shows_the_node_not_ready_until_it_lands() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), Value::Null);
    let builds_held = hold_the_builds();
    start(&played.stream);

    let pending =
        the_diagnostics_once_they_hold(&played.stream, |diagnostics| !diagnostics.is_empty());
    let pending =
        the_one_diagnostic_of(&pending, DiagnosticKindOnTheWire::ComputedValueNotYetBuilt);
    assert_eq!(
        pending.recovery,
        DiagnosticRecoveryOnTheWire::WaitForTheNode
    );
    assert_eq!(what_the_node_function_ran().frames_run, 0);

    drop(builds_held);
    let landed = the_diagnostics_once_they_hold(&played.stream, <[_]>::is_empty);
    assert!(landed.is_empty(), "the first value landed: {landed:?}");
}

/// Mental-revert: a failed build reported as still building, or not at all, would carry no
/// error and no recovery.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_computed_value_whose_first_build_fails_shows_its_error_and_a_recovery_until_an_input_changes()
{
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), json!({ "gain": -1 }));
    start(&played.stream);

    let failed = the_diagnostics_once_they_hold(&played.stream, |diagnostics| {
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKindOnTheWire::ComputedValueBuildFailed)
    });
    let failed = the_one_diagnostic_of(&failed, DiagnosticKindOnTheWire::ComputedValueBuildFailed);
    assert!(
        failed
            .words
            .contains("failed: a gain below zero, -1, amplifies nothing; the node runs no frame"),
        "{failed:?}"
    );
    assert_eq!(
        failed.recovery,
        DiagnosticRecoveryOnTheWire::ChangeAnInputTheBuildRead
    );
    assert_eq!(what_the_node_function_ran().frames_run, 0);

    set_the_gain(&played.stream, played.node, 2);
    let recovered = the_diagnostics_once_they_hold(&played.stream, <[_]>::is_empty);
    assert!(recovered.is_empty(), "{recovered:?}");
    let ran = what_the_node_ran_once_it_holds(|ran| ran.gain_the_latest_frame_read == Some(2));
    assert_eq!(ran.gain_the_latest_frame_read, Some(2), "{ran:?}");
}

/// Mental-revert: a failed rebuild that dropped the previous value would stop the frames, and
/// one left unreported would show the node healthy on a gain it was never handed.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_failed_rebuild_shows_its_error_while_the_node_runs_on_the_previous_value() {
    let project_directory = tempfile::tempdir().unwrap();
    let played = a_stream_of_a_node_function(project_directory.path(), Value::Null);
    start(&played.stream);
    what_the_node_ran_once_it_holds(|ran| ran.gain_the_latest_frame_read == Some(1));

    set_the_gain(&played.stream, played.node, -3);

    let failed =
        the_diagnostics_once_they_hold(&played.stream, |diagnostics| !diagnostics.is_empty());
    let failed = the_one_diagnostic_of(&failed, DiagnosticKindOnTheWire::ComputedValueBuildFailed);
    assert!(
        failed.words.contains(
            "failed: a gain below zero, -3, amplifies nothing; the node runs on its previous value"
        ),
        "{failed:?}"
    );
    let frames_at_the_failure = what_the_node_function_ran().frames_run;
    let ran = what_the_node_ran_once_it_holds(|ran| ran.frames_run > frames_at_the_failure + 3);
    assert!(
        ran.frames_run > frames_at_the_failure + 3,
        "frames keep running: {ran:?}"
    );
    assert_eq!(ran.gain_the_latest_frame_read, Some(1), "{ran:?}");
    assert_eq!(ran.bodies_run, 1, "{ran:?}");
}
