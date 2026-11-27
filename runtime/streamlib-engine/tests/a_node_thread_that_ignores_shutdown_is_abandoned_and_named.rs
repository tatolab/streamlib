// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::{Duration, Instant};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

use parking_lot::Mutex;
use serial_test::serial;
use streamlib::sdk::nodes::{NodeSpec, computed};
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner, RuntimeStatus};
use streamlib_engine::core::Result;
use streamlib_engine::core::nodes::NODE_REGISTRY;

/// Longer than the native join budget, so the thread is still dropping what the
/// node made when it is abandoned — and short enough not to outlive the test
/// binary by much.
const DROP_THAT_IGNORES_SHUTDOWN: Duration = Duration::from_secs(20);

/// The native join budget the engine chooses, which these tests measure against.
const NATIVE_NODE_THREAD_JOIN_BUDGET: Duration = Duration::from_secs(5);

/// The computed value of a [`StopIgnoringShutdownTestNode`], whose drop, as its stream stops,
/// ignores the shutdown for longer than the join budget.
#[derive(PartialEq)]
struct IgnoresShutdownAsItDrops;

impl Drop for IgnoresShutdownAsItDrops {
    fn drop(&mut self) {
        std::thread::sleep(DROP_THAT_IGNORES_SHUTDOWN);
    }
}

#[streamlib::sdk::node(execution = manual)]
pub fn stop_ignoring_shutdown_test_node() {
    computed(|_build_ctx| Ok::<_, String>(IgnoresShutdownAsItDrops));
}

fn a_running_stream_with_a_node_that_ignores_shutdown(
    display_name: &str,
) -> (
    tempfile::TempDir,
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
    String,
) {
    NODE_REGISTRY.register::<StopIgnoringShutdownTestNode::Node>();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    let node_run_id = stream
        .add_node(
            NodeSpec::new(
                StopIgnoringShutdownTestNode::node_class_import_path(),
                serde_json::json!({}),
            )
            .with_display_name(display_name),
        )
        .expect("add the node that ignores shutdown");
    stream.start().expect("stream start");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node starts");
    (project_directory, runner, stream, node_run_id.to_string())
}

struct RemovedNodeRunIdsRecorder(Arc<Mutex<Vec<String>>>);

impl EventListener for RemovedNodeRunIdsRecorder {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        if let Event::OfALoadedStream {
            event: RuntimeEvent::RuntimeDidRemoveNode { node_run_id },
            ..
        } = event
        {
            self.0.lock().push(node_run_id.to_string());
        }
        Ok(())
    }

    fn on_events_lost(&mut self, _lost_event_count: u64) -> Result<()> {
        Ok(())
    }
}

fn assert_the_graph_holds_no_node(stream: &LoadedStreamInThisRuntime) {
    let graph = stream.to_json().expect("the graph serializes");
    assert!(
        graph["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.is_empty()),
        "an abandoned node's node must still leave the graph: {}",
        graph["nodes"]
    );
}

#[test]
#[serial]
fn stopping_the_stream_abandons_the_thread_and_names_the_node() {
    let (_project_directory, _runner, stream, node_run_id) =
        a_running_stream_with_a_node_that_ignores_shutdown("stuck-on-stop");

    let started = Instant::now();
    let refusal = stream
        .stop()
        .expect_err("a stop that abandoned a thread must say so")
        .to_string();
    let stopped_in = started.elapsed();

    assert!(
        stopped_in >= NATIVE_NODE_THREAD_JOIN_BUDGET && stopped_in < DROP_THAT_IGNORES_SHUTDOWN,
        "the stop took {stopped_in:?}, not the native join budget"
    );
    assert!(
        refusal.contains("'stuck-on-stop'") && refusal.contains(&node_run_id),
        "the refusal must name the node by display name and id: {refusal}"
    );
    assert_eq!(
        stream.status(),
        RuntimeStatus::Stopped,
        "the rest of the teardown must still run"
    );
    assert_the_graph_holds_no_node(&stream);
    let still_running = stream.node_threads_abandoned_and_still_running();
    assert_eq!(
        still_running
            .iter()
            .map(|node| node.node_run_id.to_string())
            .collect::<Vec<_>>(),
        vec![node_run_id],
        "the thread still holds the engine, so teardown must still see it"
    );
}

#[test]
#[serial]
fn a_live_removal_abandons_the_thread_removes_the_node_and_fails_naming_it() {
    let (_project_directory, _runner, stream, node_run_id) =
        a_running_stream_with_a_node_that_ignores_shutdown("stuck-on-removal");
    let removed_node_run_ids = Arc::new(Mutex::new(Vec::new()));
    let recorder: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(RemovedNodeRunIdsRecorder(
        Arc::clone(&removed_node_run_ids),
    )));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(stream.loaded_stream_identity()),
            Arc::clone(&recorder),
        )
        .expect("subscribe to the removal events");

    let refusal = stream
        .remove_node(&node_run_id.as_str().into())
        .expect_err("a removal that abandoned a thread must say so")
        .to_string();

    assert!(
        refusal.contains("'stuck-on-removal'") && refusal.contains(&node_run_id),
        "the refusal must name the node by display name and id: {refusal}"
    );
    assert_the_graph_holds_no_node(&stream);
    assert_eq!(stream.node_threads_abandoned_and_still_running().len(), 1);
    let announced_by = Instant::now() + Duration::from_secs(5);
    while !removed_node_run_ids.lock().contains(&node_run_id) {
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
