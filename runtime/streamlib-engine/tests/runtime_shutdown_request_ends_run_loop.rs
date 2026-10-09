// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The shutdown-request handshake end-to-end: a processor inside a stream
//! asks for its stream's shutdown, and the host waiting on that stream sees
//! the stream end through its normal teardown.
//!
//! What it locks, against a REAL `Runner` (not a stub):
//! - A processor reaching `ctx.runtime().request_this_streams_shutdown(..)` —
//!   the same `Arc<dyn RuntimeOperations>` handle every in-graph processor
//!   holds — ends `Runner::wait_until_the_stream_ends` well inside a watchdog
//!   bound, and the wait returns `Ok(())` with the stream at
//!   `RuntimeStatus::Stopped` and out of the stream table: teardown ran, and
//!   the request never tore the stream down behind the host's back.
//! - A request issued before the host waits still ends the wait.
//! - A machine shutdown requested BEFORE the stream starts — by a host that
//!   decides to abort before its run begins — is honored by the wait of the
//!   engine that owns the machine's shutdown signals rather than discarded.
//!
//! Starts a real stream (GPU + iceoryx2), so this runs outside the `--lib`
//! gate, which never builds `tests/` integration binaries.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{
    LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner, RuntimeStatus,
};
use streamlib_engine::core::processors::PROCESSOR_REGISTRY;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

/// How long the host's wait is allowed to run before the test gives up on the
/// request being observed. Generous against the wait's 100 ms poll — the point
/// is that the wait ends because of the request, not because of this bound.
const SHUTDOWN_OBSERVED_WATCHDOG: Duration = Duration::from_secs(5);

/// A processor that asks for its stream's shutdown as soon as it starts — the
/// "a processor decides the run is over" case, using nothing but the public
/// `RuntimeOperations` handle its context hands it.
#[streamlib::sdk::processor(
    execution = manual,
)]
pub struct ShutdownRequestingTestProcessor;

impl streamlib_engine::ManualProcessor for ShutdownRequestingTestProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        ctx.runtime().request_this_streams_shutdown(
            "integration test: the processor decided the run is over",
        )
    }
}

fn an_empty_stream_loaded_into(
    runner: &Runner,
    project_directory: &std::path::Path,
) -> Arc<LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(project_directory).named("main"),
        )
        .expect("an empty stream loads")
}

/// Run `wait` on a thread of its own under a watchdog, and assert it ended
/// cleanly before the watchdog with the stream stopped and unloaded.
/// `what_requested_the_shutdown` names the leg under test in the failure
/// message.
fn assert_the_wait_ends_before_the_watchdog(
    runner: &Arc<Runner>,
    stream: &Arc<LoadedStreamInThisRuntime>,
    what_requested_the_shutdown: &str,
    wait: impl FnOnce() -> Result<()> + Send + 'static,
) {
    let started = Instant::now();
    let (wait_ended, wait_has_ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_ended.send(wait());
    });
    let wait_outcome = wait_has_ended
        .recv_timeout(SHUTDOWN_OBSERVED_WATCHDOG)
        .unwrap_or_else(|_| {
            panic!(
                "the wait did not end on {what_requested_the_shutdown} within \
                 {SHUTDOWN_OBSERVED_WATCHDOG:?}"
            )
        });
    wait_outcome.expect("the wait must end cleanly");

    assert!(
        started.elapsed() < SHUTDOWN_OBSERVED_WATCHDOG,
        "the wait must end on {what_requested_the_shutdown}; ran for {:?}",
        started.elapsed()
    );
    assert_eq!(
        stream.status(),
        RuntimeStatus::Stopped,
        "the stream must run its normal teardown after {what_requested_the_shutdown}",
    );
    assert!(
        runner.names_of_the_loaded_streams().is_empty(),
        "a stream that ended must leave the stream table"
    );
}

#[test]
#[serial]
fn a_processor_shutdown_request_ends_the_hosts_wait_on_its_stream() {
    PROCESSOR_REGISTRY.register::<ShutdownRequestingTestProcessor::Processor>();

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
    stream
        .add_processor(ProcessorSpec::new(
            ShutdownRequestingTestProcessor::processor_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("add the shutdown-requesting processor");
    stream.start().expect("stream start");

    let waiting_runner = Arc::clone(&runner);
    let waited_on = Arc::clone(&stream);
    assert_the_wait_ends_before_the_watchdog(
        &runner,
        &stream,
        "the processor's shutdown request",
        move || waiting_runner.wait_until_the_stream_ends(&waited_on),
    );
}

/// A request issued from the host thread after `start()` and before the host
/// waits is never lost to the wait starting late.
#[test]
#[serial]
fn a_request_issued_before_the_host_waits_still_ends_the_wait() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
    stream.start().expect("stream start");

    stream.ask_for_this_streams_shutdown("integration test: requested before the host waited");

    let waiting_runner = Arc::clone(&runner);
    let waited_on = Arc::clone(&stream);
    assert_the_wait_ends_before_the_watchdog(
        &runner,
        &stream,
        "the request issued before the wait",
        move || waiting_runner.wait_until_the_stream_ends(&waited_on),
    );
}

/// A host that decides to abort asks for the machine's shutdown before it
/// starts its stream. The machine's level is cleared when the signal owner's
/// run ends, never when a stream starts, so the request survives to the wait
/// instead of being discarded — otherwise the caller gets `Ok(())` and a
/// stream that never stops.
///
/// Mental revert: clear the machine's level at the top of
/// `run_owning_the_machine_shutdown_signals` and the wait runs to the
/// watchdog — the elapsed-time assertion then fails.
#[test]
#[serial]
fn a_machine_shutdown_requested_before_start_is_observed_by_the_signal_owners_wait() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path());

    runner
        .request_the_shutdown_of_every_loaded_stream(
            "integration test: the host aborted before start()",
        )
        .expect("the machine's shutdown is requested");

    let waiting_runner = Arc::clone(&runner);
    let waited_on = Arc::clone(&stream);
    assert_the_wait_ends_before_the_watchdog(
        &runner,
        &stream,
        "the machine's shutdown requested before start",
        move || {
            waiting_runner.run_owning_the_machine_shutdown_signals(|| {
                waited_on.start()?;
                waiting_runner.wait_until_the_stream_ends(&waited_on)
            })
        },
    );
}
