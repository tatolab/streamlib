// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A native processor thread that ignores shutdown past its budget, against a
//! real `Runner`.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs: such a thread is abandoned rather
//! than joined, the engine stays alive beneath it, and the caller is told which
//! processor it was. What it locks:
//! - `Runner::stop()` finishes the teardown and fails naming the processor by
//!   display name and id, and the runner still lists the thread as abandoned.
//! - A live `remove_processor` takes the node out of the graph and fails naming
//!   the processor.
//!
//! Starts a real `Runner` (GPU + iceoryx2), so this runs on the rig only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{Runner, RuntimeStatus};
use streamlib_engine::core::processors::PROCESSOR_REGISTRY;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

/// Longer than the native join budget, so the thread is still inside `stop()`
/// when it is abandoned — and short enough not to outlive the test binary by
/// much.
const STOP_CALLBACK_THAT_IGNORES_SHUTDOWN: Duration = Duration::from_secs(20);

/// The native join budget the engine chooses, which these tests measure against.
const NATIVE_PROCESSOR_THREAD_JOIN_BUDGET: Duration = Duration::from_secs(5);

/// A processor whose `stop()` does not return for twenty seconds.
#[streamlib::sdk::processor(execution = manual)]
pub struct StopIgnoringShutdownTestProcessor;

impl streamlib_engine::ManualProcessor for StopIgnoringShutdownTestProcessor::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        std::thread::sleep(STOP_CALLBACK_THAT_IGNORES_SHUTDOWN);
        Ok(())
    }
}

/// A started runner holding one processor that ignores shutdown, and its id.
fn a_running_runner_with_a_processor_that_ignores_shutdown(
    display_name: &str,
) -> (Arc<Runner>, String) {
    PROCESSOR_REGISTRY.register::<StopIgnoringShutdownTestProcessor::Processor>();
    let runtime = Runner::new().expect("Runner::new");
    let processor_id = runtime
        .add_processor(
            ProcessorSpec::new(
                StopIgnoringShutdownTestProcessor::processor_class_import_path(),
                serde_json::json!({}),
            )
            .with_display_name(display_name),
        )
        .expect("add the processor that ignores shutdown");
    runtime.start().expect("runtime start");
    runtime
        .wait_until_every_processor_is_running(Duration::from_secs(30))
        .expect("the processor starts");
    (runtime, processor_id.to_string())
}

fn assert_the_graph_holds_no_processor(runtime: &Runner) {
    let graph = runtime.to_json().expect("the graph serializes");
    assert!(
        graph["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.is_empty()),
        "an abandoned processor's node must still leave the graph: {}",
        graph["nodes"]
    );
}

#[test]
#[serial]
fn stopping_the_runtime_abandons_the_thread_and_names_the_processor() {
    let (runtime, processor_id) =
        a_running_runner_with_a_processor_that_ignores_shutdown("StuckOnStop");

    let started = Instant::now();
    let refusal = runtime
        .stop()
        .expect_err("a stop that abandoned a thread must say so")
        .to_string();
    let stopped_in = started.elapsed();

    assert!(
        stopped_in >= NATIVE_PROCESSOR_THREAD_JOIN_BUDGET
            && stopped_in < STOP_CALLBACK_THAT_IGNORES_SHUTDOWN,
        "the stop took {stopped_in:?}, not the native join budget"
    );
    assert!(
        refusal.contains("'StuckOnStop'") && refusal.contains(&processor_id),
        "the refusal must name the processor by display name and id: {refusal}"
    );
    assert_eq!(
        runtime.status(),
        RuntimeStatus::Stopped,
        "the rest of the teardown must still run"
    );
    assert_the_graph_holds_no_processor(&runtime);
    let still_running = runtime.processor_threads_abandoned_and_still_running();
    assert_eq!(
        still_running
            .iter()
            .map(|processor| processor.processor_id.to_string())
            .collect::<Vec<_>>(),
        vec![processor_id],
        "the thread still holds the engine, so teardown must still see it"
    );
}

#[test]
#[serial]
fn a_live_removal_abandons_the_thread_removes_the_node_and_fails_naming_it() {
    let (runtime, processor_id) =
        a_running_runner_with_a_processor_that_ignores_shutdown("StuckOnRemoval");

    let refusal = runtime
        .remove_processor(&processor_id.as_str().into())
        .expect_err("a removal that abandoned a thread must say so")
        .to_string();

    assert!(
        refusal.contains("'StuckOnRemoval'") && refusal.contains(&processor_id),
        "the refusal must name the processor by display name and id: {refusal}"
    );
    assert_the_graph_holds_no_processor(&runtime);
    assert_eq!(
        runtime
            .processor_threads_abandoned_and_still_running()
            .len(),
        1
    );
    runtime
        .stop()
        .expect("a stop that abandons nothing new succeeds");
}
