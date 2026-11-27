// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream holding one node, for the tests of a node written as one function: loading it,
//! starting it, setting an input while it plays, and waiting on what its node records.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use crate::core::descriptors::NodeClassImportPath;
use crate::core::graph::NodeRunId;
use crate::core::nodes::NodeSpec;
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::operations::RuntimeOperations;
use crate::core::runtime::{LoadedStreamInThisRuntime, Runner};

/// The longest a test waits for what it waits on.
const WAITED_ON_WITHIN: Duration = Duration::from_secs(10);

/// A stream holding one node.
pub struct AStreamOfOneNode {
    _runner: Arc<Runner>,
    pub stream: Arc<LoadedStreamInThisRuntime>,
    pub node: Uuid,
    pub node_run_id: NodeRunId,
}

/// A stream holding one node of `node_type`, holding `input_values`, not started.
pub fn a_stream_of_one_node(
    project_directory: &std::path::Path,
    node_type: NodeClassImportPath,
    input_values: Value,
) -> AStreamOfOneNode {
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads");
    let node = Uuid::new_v4();
    let node_run_id = stream
        .add_node(NodeSpec::new(node_type, input_values).with_node_uuid(node))
        .expect("the node is added");
    AStreamOfOneNode {
        _runner: runner,
        stream,
        node,
        node_run_id,
    }
}

/// A stream holding one node of `node_type`, holding `input_values`, started and running.
pub fn a_playing_stream_of_one_node(
    project_directory: &std::path::Path,
    node_type: NodeClassImportPath,
    input_values: Value,
) -> AStreamOfOneNode {
    let played = a_stream_of_one_node(project_directory, node_type, input_values);
    start(&played.stream);
    played
}

/// Start `stream` and wait until every node runs.
pub fn start(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");
}

/// `operation` run to its end on a current-thread runtime of the test's own.
pub fn on_the_tests_own_runtime<T>(operation: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(operation)
}

/// Set input `input` of `node` to `value` while `stream` plays.
pub fn set_the_input(stream: &LoadedStreamInThisRuntime, node: Uuid, input: &str, value: Value) {
    on_the_tests_own_runtime(stream.apply_a_stream_graph_operation_live(
        StreamGraphOperation::SetNodeInputValue {
            node,
            input: input.to_owned(),
            member: None,
            from: None,
            to: Some(value),
        },
    ))
    .expect("an input is set while the stream plays");
}

/// What `read` reads once `settled` holds of it, or when the wait runs out.
pub fn once_it_holds<T>(read: impl Fn() -> T, settled: impl Fn(&T) -> bool) -> T {
    let deadline = Instant::now() + WAITED_ON_WITHIN;
    loop {
        let read = read();
        if settled(&read) || Instant::now() >= deadline {
            return read;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
