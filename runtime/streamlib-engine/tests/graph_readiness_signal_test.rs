// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `Runner::wait_until_every_processor_is_running` — the signal an app process
//! polls to know the graph is up.
//!
//! What makes it load-bearing: a processor in a helper process attaches its
//! iceoryx2 subscriber during `setup`, tens of milliseconds after the graph
//! compiles, and a link drops whatever it carries before its consumer attaches.
//! A publisher gated on this call cannot lose those bags.
//!
//! The trap these tests exist for is the *false ready* — a graph reporting
//! itself up because it has not been asked to start yet, which is
//! indistinguishable from success at the call site and reintroduces the exact
//! drop the signal removes.

use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::descriptors::{
    Org, Package, PortDescriptor, ProcessorClassImportPath, ProcessorDescriptor, SchemaIdent,
    SemVer, TypeName,
};
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

/// Short enough that a test waiting it out stays quick, long enough that
/// reaching it means nothing transitioned rather than that the machine stalled.
const SHORT_TIMEOUT: Duration = Duration::from_millis(250);

fn register_test_type(short: &str) -> ProcessorClassImportPath {
    let import_path =
        ProcessorClassImportPath::new(format!("{}::{short}", module_path!())).unwrap();
    let id = SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("graph-readiness-signal-test").unwrap(),
        TypeName::new(short).unwrap(),
        SemVer::new(1, 0, 0),
    );
    let descriptor = ProcessorDescriptor::new(
        id,
        import_path.clone(),
        "graph readiness signal test",
    )
    .with_input(PortDescriptor::new("bags_from_upstream", "", false))
    .with_output(PortDescriptor::new("bags_to_downstream", "", false));
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

#[test]
#[serial]
fn a_graph_with_no_processors_has_nothing_to_wait_for() {
    let runtime = Runner::new().unwrap();

    runtime
        .wait_until_every_processor_is_running(SHORT_TIMEOUT)
        .expect("an empty graph is up by definition");
}

/// The false-ready guard. A processor is observable from the moment it is
/// added, not from the moment the compiler prepares it — so a wait that begins
/// before `start()` sees a `Pending` processor and keeps waiting.
///
/// Mentally move the state back to where the compiler attaches it and this
/// fails: the graph reports zero processors, every one of them is vacuously
/// running, and the call returns `Ok` on a pipeline that has not started.
#[test]
#[serial]
fn a_graph_that_was_never_started_is_not_reported_as_running() {
    let runtime = Runner::new().unwrap();
    let processor_id = runtime
        .add_processor(ProcessorSpec::new(
            register_test_type("NeverStarted"),
            serde_json::json!({}),
        ))
        .expect("a registered type adds cleanly");

    let failure = runtime
        .wait_until_every_processor_is_running(SHORT_TIMEOUT)
        .expect_err("a graph that never started must not report itself running");

    let reported = failure.to_string();
    assert!(
        reported.contains(processor_id.as_str()),
        "the timeout must name the processor it gave up on, got: {reported}"
    );
    assert!(
        reported.contains("Pending"),
        "the timeout must say what state it gave up in, got: {reported}"
    );
}

/// A failure behind a processor that never starts still reaches the report.
///
/// Processors are waited on in turn, so a failure is observed when the wait
/// reaches it — here it never does, because the processor ahead of it stays
/// `Pending` until the deadline. What must not happen is the failure going
/// unmentioned: the caller is told which processor ended the wait *and* what
/// state every other one was in, so the real cause is in the message either
/// way.
#[test]
#[serial]
fn a_failure_behind_a_processor_that_never_starts_is_still_reported() {
    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(
            register_test_type("StaysPending"),
            serde_json::json!({}),
        ))
        .expect("a registered type adds cleanly");
    // A registry miss lands the node in `Error` without spawning anything.
    let _ = runtime.add_processor(ProcessorSpec::new(
        ProcessorClassImportPath::new("ghost_package::BehindTheSlowOne").unwrap(),
        serde_json::json!({}),
    ));

    let reported = runtime
        .wait_until_every_processor_is_running(SHORT_TIMEOUT)
        .expect_err("a graph holding a failed processor is not running")
        .to_string();

    assert!(
        reported.contains("=Error"),
        "the failed processor must be in the report even when the wait gave up on another, \
         got: {reported}"
    );
}

/// A processor that cannot run ends the wait where it is rather than burning
/// the caller's whole timeout — the graph will never come up, and saying so
/// late is worse than saying so now.
#[test]
#[serial]
fn a_processor_that_failed_ends_the_wait_without_burning_the_timeout() {
    let runtime = Runner::new().unwrap();
    // A registry miss leaves the node in the graph in `Error` — the same state
    // a failed `setup` lands a processor in, reached without spawning one.
    let _ = runtime.add_processor(ProcessorSpec::new(
        ProcessorClassImportPath::new("ghost_package::NotRegistered").unwrap(),
        serde_json::json!({}),
    ));

    let began_waiting = Instant::now();
    let failure = runtime
        .wait_until_every_processor_is_running(Duration::from_secs(30))
        .expect_err("a graph holding a failed processor is not running");
    let waited = began_waiting.elapsed();

    assert!(
        failure.to_string().contains("Error rather than Running"),
        "the failure must distinguish a broken processor from a slow one, got: {failure}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "waited {waited:?} on a processor that was already Error — the wait is not \
         short-circuiting on failure"
    );
}
