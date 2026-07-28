// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The shutdown-request handshake end-to-end (#1599): a processor inside the
//! graph asks for shutdown, and the harness that owns the run loop observes it
//! and runs the normal teardown.
//!
//! What it locks, against a REAL `Runner` (not a stub):
//! - A processor reaching `ctx.runtime().request_runtime_shutdown(..)` — the
//!   same `Arc<dyn RuntimeOperations>` handle every in-graph processor holds —
//!   ends `wait_for_signal_with` well inside a watchdog bound, and the harness
//!   exits cleanly (`Ok(())`) with the runtime at `RuntimeStatus::Stopped`,
//!   i.e. teardown ran and the request never tore the runtime down behind the
//!   harness's back.
//! - The latch leg on its own: a request published while nothing is subscribed
//!   to `RuntimeShutdown` still ends the loop. Which of the two legs (event or
//!   latch) observes the processor-driven request above is a race — the
//!   processor's `start()` runs on its own thread against the harness reaching
//!   `PUBSUB.subscribe` — so the latch gets its own deterministic test rather
//!   than riding on that timing.
//!
//! Starts a real `Runner` (GPU + iceoryx2), so this runs outside the `--lib`
//! gate, which never builds `tests/` integration binaries.

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

/// The latch's whole reason to exist: a request published while nothing is
/// subscribed to `RuntimeShutdown` leaves no event for the run loop's listener
/// to receive, so only the latch can end the loop.
///
/// Deterministic where the processor-driven test is not — the request is issued
/// from the harness thread after `start()` (which clears the latch at entry)
/// and before `wait_for_signal_with` subscribes, so the event is provably
/// unobserved.
///
/// Mental revert: drop the `is_runtime_shutdown_requested()` term from the poll
/// loop's condition and the loop runs to the watchdog `Break` — the
/// elapsed-time assertion then fails.
#[test]
#[serial]
fn a_request_latched_before_the_run_loop_subscribes_still_ends_it() {
    let runtime = Runner::new().expect("Runner::new");
    runtime.start().expect("runtime start");

    runtime
        .request_runtime_shutdown("integration test: requested before the loop subscribed")
        .expect("the host arm never fails");

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
        "the run loop must end on the latched request, not on the watchdog \
         break; ran for {elapsed:?}",
    );
    assert_eq!(
        runtime.status(),
        RuntimeStatus::Stopped,
        "the harness must run the normal teardown after observing the latch",
    );
}
