// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! An `add_node` applied to a stream's live graph and refused leaves no node behind: the
//! graph a client reads after the refusal is the graph it read before.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serial_test::serial;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use streamlib::sdk::runtime::{LoadedStreamInThisRuntime, Runner, RuntimeOperations};
use streamlib::sdk::schemars::JsonSchema;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};
use streamlib_project_repository::{StreamGraphDocumentNode, StreamGraphOperation};
use uuid::Uuid;

#[streamlib::sdk::processor_config]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct RefusedLiveAddTestProcessorConfig {}

#[streamlib::sdk::processor(
    execution = manual,
    config = crate::RefusedLiveAddTestProcessorConfig,
)]
pub struct RefusedLiveAddTestProcessor;

impl streamlib_engine::ManualProcessor for RefusedLiveAddTestProcessor::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// A stream holding one [`RefusedLiveAddTestProcessor`], started when `started`, with the
/// project directory and runner it lives in.
fn a_stream_holding_one_node(
    started: bool,
) -> (
    tempfile::TempDir,
    Arc<Runner>,
    Arc<LoadedStreamInThisRuntime>,
) {
    PROCESSOR_REGISTRY.register::<RefusedLiveAddTestProcessor::Processor>();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    stream
        .add_processor(
            ProcessorSpec::new(
                RefusedLiveAddTestProcessor::processor_class_import_path(),
                serde_json::json!({}),
            )
            .with_display_name("held"),
        )
        .expect("the node is added");
    if started {
        stream.start().expect("the stream starts");
        stream
            .wait_until_every_processor_is_running(Duration::from_secs(30))
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
fn an_add_whose_processor_the_running_stream_cannot_build_leaves_no_node() {
    let (_project_directory, _runner, stream) = a_stream_holding_one_node(true);
    let before = the_node_names_the_graph_answers(&stream);

    let refusal = add_a_node_live(
        &stream,
        serde_json::json!({
            "id": Uuid::new_v4(),
            "name": "leak_probe",
            "type": "tatolab.stream:GraphInput",
            "config": {"port": "leak_probe", "family": "Channels"},
        }),
    )
    .expect_err("a GraphInput naming no family is refused")
    .to_string();

    assert!(refusal.contains("names family \"Channels\""), "{refusal}");
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
            "type": "ghost_package::DefinitelyNotARegisteredProcessor",
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
            "type": RefusedLiveAddTestProcessor::processor_class_import_path().as_str(),
        }),
    )
    .expect_err("a UUID the graph's node holds is refused");

    assert_eq!(the_node_names_the_graph_answers(&stream), before);
}
