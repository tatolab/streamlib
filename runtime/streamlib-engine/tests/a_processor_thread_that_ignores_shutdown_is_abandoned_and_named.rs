// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A native processor thread that ignores shutdown past its budget, against a
//! real `Runner`.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs: such a thread is abandoned rather
//! than joined, the engine stays alive beneath it, and the caller is told which
//! processor it was. What it locks:
//! - `LoadedStreamInThisRuntime::stop()` finishes the teardown and fails naming
//!   the processor by display name and id, and the stream still lists the
//!   thread as abandoned.
//! - A live `remove_processor` takes the node out of the graph, announces the
//!   removal, and fails naming the processor.
//!
//! Starts a real stream (GPU + iceoryx2), so this runs on the rig only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serial_test::serial;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
use streamlib::sdk::runtime::{
    LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner, RuntimeStatus,
};
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

/// A started stream holding one processor that ignores shutdown, the project
/// directory it is loaded from, the runner it is loaded in, and the
/// processor's id.
fn a_running_stream_with_a_processor_that_ignores_shutdown(
    display_name: &str,
) -> (
    tempfile::TempDir,
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
    String,
) {
    PROCESSOR_REGISTRY.register::<StopIgnoringShutdownTestProcessor::Processor>();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(
            OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                .named("main"),
        )
        .expect("an empty stream loads");
    let processor_id = stream
        .add_processor(
            ProcessorSpec::new(
                StopIgnoringShutdownTestProcessor::processor_class_import_path(),
                serde_json::json!({}),
            )
            .with_display_name(display_name),
        )
        .expect("add the processor that ignores shutdown");
    stream.start().expect("stream start");
    stream
        .wait_until_every_processor_is_running(Duration::from_secs(30))
        .expect("the processor starts");
    (project_directory, runner, stream, processor_id.to_string())
}

/// Records the ids of the processors a `RuntimeDidRemoveProcessor` names.
struct RemovedProcessorIdsRecorder(Arc<Mutex<Vec<String>>>);

impl EventListener for RemovedProcessorIdsRecorder {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        if let Event::OfALoadedStream {
            event: RuntimeEvent::RuntimeDidRemoveProcessor { processor_id },
            ..
        } = event
        {
            self.0.lock().push(processor_id.to_string());
        }
        Ok(())
    }
}

fn assert_the_graph_holds_no_processor(stream: &LoadedStreamInThisRuntime) {
    let graph = stream.to_json().expect("the graph serializes");
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
fn stopping_the_stream_abandons_the_thread_and_names_the_processor() {
    let (_project_directory, _runner, stream, processor_id) =
        a_running_stream_with_a_processor_that_ignores_shutdown("stuck-on-stop");

    let started = Instant::now();
    let refusal = stream
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
        refusal.contains("'stuck-on-stop'") && refusal.contains(&processor_id),
        "the refusal must name the processor by display name and id: {refusal}"
    );
    assert_eq!(
        stream.status(),
        RuntimeStatus::Stopped,
        "the rest of the teardown must still run"
    );
    assert_the_graph_holds_no_processor(&stream);
    let still_running = stream.processor_threads_abandoned_and_still_running();
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
    let (_project_directory, _runner, stream, processor_id) =
        a_running_stream_with_a_processor_that_ignores_shutdown("stuck-on-removal");
    let removed_processor_ids = Arc::new(Mutex::new(Vec::new()));
    let recorder: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(
        RemovedProcessorIdsRecorder(Arc::clone(&removed_processor_ids)),
    ));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(stream.loaded_stream_identity()),
            Arc::clone(&recorder),
        )
        .expect("subscribe to the removal events");

    let refusal = stream
        .remove_processor(&processor_id.as_str().into())
        .expect_err("a removal that abandoned a thread must say so")
        .to_string();

    assert!(
        refusal.contains("'stuck-on-removal'") && refusal.contains(&processor_id),
        "the refusal must name the processor by display name and id: {refusal}"
    );
    assert_the_graph_holds_no_processor(&stream);
    assert_eq!(
        stream.processor_threads_abandoned_and_still_running().len(),
        1
    );
    let announced_by = Instant::now() + Duration::from_secs(5);
    while !removed_processor_ids.lock().contains(&processor_id) {
        assert!(
            Instant::now() < announced_by,
            "a removal that left the graph was never announced"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    stream
        .stop()
        .expect("a stop that abandons nothing new succeeds");
}
