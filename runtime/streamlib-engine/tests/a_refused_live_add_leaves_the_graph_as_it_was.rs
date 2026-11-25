// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An `add_node` applied to a stream's live graph and refused leaves no node behind: the
//! graph a client reads after the refusal is the graph it read before.

use std::sync::Arc;
use std::time::Duration;

use serial_test::serial;
use streamlib::sdk::nodes::{NODE_REGISTRY, NodeSpec};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner, RuntimeOperations};
use streamlib_engine::core::{Result, RuntimeContextFullAccess};
use streamlib_project_repository::{StreamGraphDocumentNode, StreamGraphOperation};
use uuid::Uuid;

#[streamlib::sdk::node(execution = manual)]
pub struct RefusedLiveAddTestNode;

impl streamlib_engine::ManualNode for RefusedLiveAddTestNode::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn a_stream_holding_one_node(
    started: bool,
) -> (
    tempfile::TempDir,
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
) {
    NODE_REGISTRY.register::<RefusedLiveAddTestNode::Node>();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    stream
        .add_node(
            NodeSpec::new(
                RefusedLiveAddTestNode::node_class_import_path(),
                serde_json::json!({}),
            )
            .with_display_name("held"),
        )
        .expect("the node is added");
    if started {
        stream.start().expect("the stream starts");
        stream
            .wait_until_every_node_is_running(Duration::from_secs(30))
            .expect("the node runs");
    }
    (project_directory, runner, stream)
}

fn add_a_node_live(
    stream: &LoadedStreamInThisRuntime,
    node: serde_json::Value,
) -> Result<StreamGraphOperation> {
    let node: StreamGraphDocumentNode =
        serde_json::from_value(node).expect("a graph document node");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(
            stream.apply_a_stream_graph_operation_live(StreamGraphOperation::AddNode {
                node,
                links: Vec::new(),
            }),
        )
}

fn the_node_names_the_graph_answers(stream: &LoadedStreamInThisRuntime) -> Vec<String> {
    let graph = stream.to_json().expect("the graph serializes");
    graph["nodes"]
        .as_array()
        .expect("the graph lists its nodes")
        .iter()
        .map(|node| node["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
#[serial]
fn an_add_whose_node_the_running_stream_cannot_build_leaves_no_node() {
    let (_project_directory, _runner, stream) = a_stream_holding_one_node(true);
    let before = the_node_names_the_graph_answers(&stream);

    let refusal = add_a_node_live(
        &stream,
        serde_json::json!({
            "id": Uuid::new_v4(),
            "name": "leak_probe",
            "type": "tatolab.stream:GraphInput",
            "input_values": {"port": "leak_probe", "pin_type": {"type": "video"}},
        }),
    )
    .expect_err("a GraphInput choosing no pin type is refused")
    .to_string();

    assert!(
        refusal.contains("`GraphInput` node `leak_probe`") && refusal.contains("`pin_type`"),
        "{refusal}"
    );
    assert_eq!(the_node_names_the_graph_answers(&stream), before);
    stream.stop().expect("the stream stops");
}

#[test]
#[serial]
fn an_add_of_a_type_nothing_resolves_leaves_no_node() {
    let (_project_directory, _runner, stream) = a_stream_holding_one_node(false);
    let before = the_node_names_the_graph_answers(&stream);

    add_a_node_live(
        &stream,
        serde_json::json!({
            "id": Uuid::new_v4(),
            "name": "ghost",
            "type": "ghost_package::DefinitelyNotARegisteredNode",
        }),
    )
    .expect_err("a type nothing resolves is refused");

    assert_eq!(the_node_names_the_graph_answers(&stream), before);
}

#[test]
#[serial]
fn an_add_refused_for_a_uuid_another_node_holds_leaves_that_node() {
    let (_project_directory, _runner, stream) = a_stream_holding_one_node(false);
    let graph = stream.to_json().expect("the graph serializes");
    let held_node_uuid = graph["nodes"][0]["id"].clone();
    let before = the_node_names_the_graph_answers(&stream);

    add_a_node_live(
        &stream,
        serde_json::json!({
            "id": held_node_uuid,
            "name": "another",
            "type": RefusedLiveAddTestNode::node_class_import_path().as_str(),
        }),
    )
    .expect_err("a UUID the graph's node holds is refused");

    assert_eq!(the_node_names_the_graph_answers(&stream), before);
}
