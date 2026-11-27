// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serial_test::serial;
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner, RuntimeOperations};
use streamlib::sdk::schemars::JsonSchema;
use streamlib_engine::core::Result;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct FrameWidthTakingTestNodeConfig {
    #[serde(default)]
    pub frame_width: u32,
}

#[streamlib::sdk::node(
    execution = manual,
)]
pub fn frame_width_taking_test_node() {}

fn a_running_stream_with_a_frame_width_taking_node() -> (
    tempfile::TempDir,
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
    Uuid,
) {
    NODE_REGISTRY.register_with_a_config_schema_for_a_test::<FrameWidthTakingTestNode::Node, FrameWidthTakingTestNodeConfig>();
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
                FrameWidthTakingTestNode::node_class_import_path(),
                serde_json::json!({"frame_width": 640}),
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

fn set_the_node_config_live(
    stream: &LoadedStreamInThisRuntime,
    node: Uuid,
    to: serde_json::Value,
) -> Result<StreamGraphOperation> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(
            stream.apply_a_stream_graph_operation_live(StreamGraphOperation::SetNodeConfig {
                node,
                from: serde_json::json!({"frame_width": 640}),
                to,
            }),
        )
}

fn the_config_the_graph_answers_for(
    stream: &LoadedStreamInThisRuntime,
    node: Uuid,
) -> serde_json::Value {
    let graph = stream.to_json().expect("the graph serializes");
    graph["nodes"]
        .as_array()
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|rendered| rendered["id"] == node.to_string())
        })
        .map(|rendered| rendered["input_values"].clone())
        .expect("the graph holds the node")
}

#[test]
#[serial]
fn the_graph_answers_a_config_the_running_node_took_as_soon_as_the_edit_answers() {
    let (_project_directory, _runner, stream, node) =
        a_running_stream_with_a_frame_width_taking_node();

    set_the_node_config_live(&stream, node, serde_json::json!({"frame_width": 1280}))
        .expect("the running node takes the width");

    assert_eq!(
        the_config_the_graph_answers_for(&stream, node),
        serde_json::json!({"frame_width": 1280})
    );
    stream.stop().expect("the stream stops");
}

#[test]
#[serial]
fn a_config_the_schema_of_its_setting_refuses_is_refused_and_the_node_keeps_its_config() {
    let (_project_directory, _runner, stream, node) =
        a_running_stream_with_a_frame_width_taking_node();

    let refusal =
        set_the_node_config_live(&stream, node, serde_json::json!({"frame_width": "wide"}))
            .expect_err("a width the schema of the setting refuses is refused")
            .to_string();

    assert!(refusal.contains("`frame_width`"), "{refusal}");
    assert_eq!(
        the_config_the_graph_answers_for(&stream, node),
        serde_json::json!({"frame_width": 640})
    );
    stream.stop().expect("the stream stops");
}
