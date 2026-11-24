// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Settings set one at a time on a paused stream, whose config updates wait for the resume's
//! commit, each build on the one before: the running node holds every one of them once the
//! stream resumes.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serial_test::serial;
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner, RuntimeOperations};
use streamlib::sdk::schemars::JsonSchema;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct FrameSizeTakingTestNodeConfig {
    #[serde(default)]
    pub frame_width: u32,
    #[serde(default)]
    pub frame_height: u32,
}

#[streamlib::sdk::node(
    execution = manual,
)]
pub struct FrameSizeTakingTestNode;

impl streamlib_engine::ManualNode for FrameSizeTakingTestNode::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn a_running_stream_with_a_frame_size_taking_node() -> (
    tempfile::TempDir,
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
    Uuid,
) {
    NODE_REGISTRY.register_with_a_config_schema_for_a_test::<FrameSizeTakingTestNode::Node, FrameSizeTakingTestNodeConfig>();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    let node_uuid = Uuid::new_v4();
    stream
        .add_node(
            NodeSpec::new(
                FrameSizeTakingTestNode::node_class_import_path(),
                serde_json::json!({"frame_width": 640, "frame_height": 480}),
            )
            .with_node_uuid(node_uuid),
        )
        .expect("the node is added");
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");
    (project_directory, runner, stream, node_uuid)
}

#[test]
#[serial]
fn two_settings_set_one_at_a_time_while_paused_are_both_held_once_resumed() {
    let (_project_directory, _runner, stream, node) =
        a_running_stream_with_a_frame_size_taking_node();
    stream.pause().expect("the stream pauses");

    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime");
    for (input, from, to) in [("frame_width", 640, 1280), ("frame_height", 480, 720)] {
        tokio_runtime
            .block_on(stream.apply_a_stream_graph_operation_live(
                StreamGraphOperation::SetNodeInputValue {
                    member: None,
                    node,
                    input: input.to_owned(),
                    from: Some(serde_json::json!(from)),
                    to: Some(serde_json::json!(to)),
                },
            ))
            .expect("the setting is set on the paused stream");
    }
    stream.resume().expect("the stream resumes");

    let graph = stream.to_json().expect("the graph serializes");
    let config = graph["nodes"]
        .as_array()
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|rendered| rendered["id"] == node.to_string())
        })
        .map(|rendered| rendered["config"].clone())
        .expect("the graph holds the node");
    assert_eq!(
        config,
        serde_json::json!({"frame_width": 1280, "frame_height": 720})
    );
    stream.stop().expect("the stream stops");
}
