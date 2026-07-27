// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The shutdown-request handshake end-to-end (#1599): a processor inside the
//! graph asks for shutdown, and the harness that owns the run loop observes it
//! and runs the normal teardown.
//!
//! What it locks, against a REAL `Runner` (not a stub):
//! - A processor reaching `ctx.runtime().request_runtime_shutdown(..)` — the
//!   same `Arc<dyn RuntimeOperations>` handle every in-graph processor holds —
//!   ends `wait_for_signal_with` well inside a watchdog bound.
//! - The harness exits cleanly (`Ok(())`) and the runtime reaches
//!   `RuntimeStatus::Stopped`, i.e. teardown ran; the request never tore the
//!   runtime down behind the harness's back.
//! - The request lands during `start()`, BEFORE `wait_for_signal_with`
//!   subscribes its shutdown listener — so this also covers the window the
//!   latch exists to close: a request whose `RuntimeShutdown` event nobody was
//!   listening for yet is still observed.
//!
//! Mental revert: drop the `is_runtime_shutdown_requested()` term from the
//! poll loop's condition and, with no subscriber up in time to catch the
//! event, the loop runs to the watchdog `Break` — the elapsed-time assertion
//! then fails.
//!
//! Starts a real `Runner` (GPU + iceoryx2), so this is rig/CI-run, not part of
//! the `--lib` gate, which never builds `tests/` integration binaries.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{Runner, RuntimeStatus};
use streamlib_engine::core::processors::PROCESSOR_REGISTRY;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

/// How long the harness is allowed to keep running before the test gives up on
/// the request being observed. Generous against the run loop's 100 ms poll
/// granularity — the point is that the loop ends because of the request, not
/// because of this bound.
const SHUTDOWN_OBSERVED_WATCHDOG: Duration = Duration::from_secs(5);

/// A processor that asks the runtime to stop as soon as it starts — the
/// "a processor decides the run is over" case, using nothing but the public
/// `RuntimeOperations` handle its context hands it.
#[streamlib::sdk::processor(
    "@tatolab/streamlib-engine/ShutdownRequestingTestProcessor",
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
        ctx.runtime()
            .request_runtime_shutdown("integration test: the processor decided the run is over")
    }
}

#[test]
#[serial]
fn a_processor_shutdown_request_ends_the_harness_run_loop() {
    PROCESSOR_REGISTRY.register::<ShutdownRequestingTestProcessor::Processor>();

    let runtime = Runner::new().expect("Runner::new");
    runtime
        .add_processor(ProcessorSpec::new(
            ShutdownRequestingTestProcessor::schema_ident(),
            serde_json::json!({}),
        ))
        .expect("add the shutdown-requesting processor");
    runtime.start().expect("runtime start");

    let started = Instant::now();
    let watchdog_deadline = started + SHUTDOWN_OBSERVED_WATCHDOG;
    runtime
        .wait_for_signal_with(|_| {
            if Instant::now() >= watchdog_deadline {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .expect("the harness must exit its run loop cleanly");
    let elapsed = started.elapsed();

    assert!(
        elapsed < SHUTDOWN_OBSERVED_WATCHDOG,
        "the run loop must end on the processor's shutdown request, not on the \
         watchdog break; ran for {elapsed:?}",
    );
    assert_eq!(
        runtime.status(),
        RuntimeStatus::Stopped,
        "the harness must run the normal teardown after observing the request",
    );
}
