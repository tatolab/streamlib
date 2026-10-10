// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API over a real engine loading two streams: each call reaches the
//! stream it names, a stream not loaded is refused naming the loaded ones,
//! `graph` naming none renders both, and no graph holds a node of the local
//! API's own.

use std::sync::Arc;

use serde_json::{Value, json};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{ManualProcessor, PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::{
    LoadedStreamInThisRuntime, OperationsOnTheStreamsLoadedInThisRuntime,
    OptionsForLoadingOneStream, Runner,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::mcp::tests::{first_text_block_json, prompt_text, resource_document, tool_call_result};

/// A native node with nothing to do, added to each stream so each graph has a
/// node of its own to tell it apart; its one output gives a prompt a port.
#[streamlib::sdk::processor(execution = manual, output("video"))]
pub struct TwoStreamsTestIdleNode;

impl ManualProcessor for TwoStreamsTestIdleNode::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// An engine with the streams `first` and `second` loaded, each holding one
/// idle node named after its stream.
struct AnEngineLoadingTwoStreams {
    engine: Arc<Runner>,
    first: Arc<LoadedStreamInThisRuntime>,
    second: Arc<LoadedStreamInThisRuntime>,
    _project_directories: [tempfile::TempDir; 2],
}

impl AnEngineLoadingTwoStreams {
    fn load() -> Self {
        PROCESSOR_REGISTRY.register::<TwoStreamsTestIdleNode::Processor>();
        let engine = Runner::new().expect("the engine builds");
        let project_directories = [
            tempfile::tempdir().expect("a project directory"),
            tempfile::tempdir().expect("a project directory"),
        ];
        let [first, second] = [("first", 0), ("second", 1)].map(|(stream_name, project)| {
            let stream = engine
                .load_an_empty_stream(
                    OptionsForLoadingOneStream::in_project_directory(
                        project_directories[project].path(),
                    )
                    .named(stream_name),
                )
                .expect("the stream loads");
            let mut idle_node = ProcessorSpec::new(
                TwoStreamsTestIdleNode::processor_class_import_path(),
                json!({}),
            );
            idle_node.display_name = Some(format!("idle-in-{stream_name}"));
            stream
                .add_processor_reporting_its_name(idle_node)
                .expect("the idle node is added");
            stream
        });
        Self {
            engine,
            first,
            second,
            _project_directories: project_directories,
        }
    }

    fn operations_on_the_loaded_streams(
        &self,
    ) -> Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime> {
        Arc::clone(&self.engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>
    }
}

fn node_names_in(graph: &Value) -> Vec<String> {
    graph["nodes"]
        .as_array()
        .expect("a graph lists its nodes")
        .iter()
        .filter_map(|node| node["name"].as_str().map(str::to_string))
        .collect()
}

fn node_types_in(graph: &Value) -> Vec<String> {
    graph["nodes"]
        .as_array()
        .expect("a graph lists its nodes")
        .iter()
        .filter_map(|node| node["type"].as_str().map(str::to_string))
        .collect()
}

fn tool_error_text(tool_result: &Value) -> String {
    assert_eq!(tool_result["isError"], true, "{tool_result}");
    tool_result["content"][0]["text"]
        .as_str()
        .expect("a text block")
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_local_api_serves_each_of_two_streams_by_name() {
    let loaded = AnEngineLoadingTwoStreams::load();

    for (stream_name, own_node) in [("first", "idle-in-first"), ("second", "idle-in-second")] {
        let graph = first_text_block_json(
            &tool_call_result(
                loaded.operations_on_the_loaded_streams(),
                "graph",
                json!({ "stream": stream_name }),
            )
            .await,
        );
        assert_eq!(graph["stream"], stream_name, "{graph}");
        assert_eq!(
            node_names_in(&graph),
            [own_node],
            "the stream's own node alone — none of the other stream's, and none of the local \
             API's: {graph}"
        );
        assert!(
            node_types_in(&graph)
                .iter()
                .all(|node_type| !node_type.contains("ApiServer")),
            "{graph}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mutation_lands_only_in_the_stream_it_names() {
    let loaded = AnEngineLoadingTwoStreams::load();

    let added = first_text_block_json(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "add_node",
            json!({
                "stream": "second",
                "type": TwoStreamsTestIdleNode::processor_class_import_path().as_str(),
                "name": "added-to-second",
            }),
        )
        .await,
    );

    assert_eq!(added["name"], "added-to-second");
    assert!(
        node_names_in(&loaded.second.to_json().unwrap()).contains(&"added-to-second".to_string())
    );
    assert!(
        !node_names_in(&loaded.first.to_json().unwrap()).contains(&"added-to-second".to_string()),
        "the stream the call did not name is untouched"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_not_loaded_is_refused_naming_the_loaded_ones() {
    let loaded = AnEngineLoadingTwoStreams::load();

    for stream_not_loaded in ["third", "", ".."] {
        let refusal = tool_error_text(
            &tool_call_result(
                loaded.operations_on_the_loaded_streams(),
                "remove_node",
                json!({ "stream": stream_not_loaded, "name": "idle-in-first" }),
            )
            .await,
        );

        assert!(
            refusal.contains(&format!("`{stream_not_loaded}`")),
            "{refusal}"
        );
        assert!(
            refusal.contains("first") && refusal.contains("second"),
            "{refusal}"
        );
        assert_eq!(
            refusal.contains("cannot name a stream"),
            stream_not_loaded != "third",
            "only a name that casts to nothing is refused as one that cannot name a stream: \
             {refusal}"
        );
    }
    assert!(
        node_names_in(&loaded.first.to_json().unwrap()).contains(&"idle-in-first".to_string()),
        "a refused call removes nothing"
    );
}

/// `graph` naming no stream renders both under the runtime's name; a tool
/// acting on one stream that names none is refused naming the argument,
/// however many streams are loaded.
#[tokio::test(flavor = "multi_thread")]
async fn graph_without_a_stream_renders_both_and_a_one_stream_tool_naming_none_is_refused() {
    let loaded = AnEngineLoadingTwoStreams::load();

    let machine_wide = first_text_block_json(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "graph",
            json!({}),
        )
        .await,
    );
    assert_eq!(
        machine_wide["runtime_name"],
        loaded.engine.runtime_name().as_str()
    );
    let streams = machine_wide["streams"].as_array().expect("a streams array");
    assert_eq!(
        streams
            .iter()
            .map(|graph| graph["stream"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["first", "second"],
        "{machine_wide}"
    );
    assert_eq!(node_names_in(&streams[0]), ["idle-in-first"]);
    assert_eq!(node_names_in(&streams[1]), ["idle-in-second"]);

    loaded
        .engine
        .unload_stream("second")
        .expect("the second stream unloads");
    let unnamed = tool_error_text(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "remove_node",
            json!({ "name": "idle-in-first" }),
        )
        .await,
    );
    assert!(unnamed.contains("stream"), "{unnamed}");
    assert!(
        node_names_in(&loaded.first.to_json().unwrap()).contains(&"idle-in-first".to_string()),
        "a call naming no stream reaches none, even with one loaded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_resources_render_every_stream_and_a_prompt_the_stream_it_names() {
    let loaded = AnEngineLoadingTwoStreams::load();

    let graph = resource_document(
        loaded.operations_on_the_loaded_streams(),
        crate::mcp_resources::LIVE_GRAPH_RESOURCE_URI,
    )
    .await;
    assert_eq!(
        graph["streams"]
            .as_array()
            .expect("a streams array")
            .iter()
            .map(|stream_graph| stream_graph["stream"].clone())
            .collect::<Vec<_>>(),
        [json!("first"), json!("second")],
        "{graph}"
    );
    let catalog = resource_document(
        loaded.operations_on_the_loaded_streams(),
        crate::mcp_resources::NODE_CATALOG_RESOURCE_URI,
    )
    .await;
    assert!(
        catalog["nodes"]
            .as_array()
            .expect("the native types")
            .iter()
            .any(|entry| entry["type"]
                == TwoStreamsTestIdleNode::processor_class_import_path().as_str()),
        "{catalog}"
    );
    assert_eq!(
        catalog["streams"],
        json!([
            { "stream": "first", "nodes": [] },
            { "stream": "second", "nodes": [] },
        ]),
        "each stream lists the types its own interpreter described"
    );

    let recipe = prompt_text(
        loaded.operations_on_the_loaded_streams(),
        "fan_output_to_another_consumer",
        json!({
            "stream": "second",
            "from_node": "idle-in-second",
            "from_port": "video",
            "type": TwoStreamsTestIdleNode::processor_class_import_path().as_str(),
        }),
    )
    .await;
    assert!(
        recipe.contains("`stream`: `second`")
            && recipe.contains("idle-in-second")
            && !recipe.contains("`stream`: `first`"),
        "{recipe}"
    );
}

/// One HTTP/1.1 GET over the socket at `local_api_socket_path`: its status
/// line and body.
async fn http_get_over_the_local_api_socket(
    local_api_socket_path: &std::path::Path,
    request_target: &str,
) -> (String, String) {
    let mut connection = tokio::net::UnixStream::connect(local_api_socket_path)
        .await
        .expect("the local API socket accepts a connection");
    connection
        .write_all(
            format!(
                "GET {request_target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = Vec::new();
    connection.read_to_end(&mut answer).await.unwrap();
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let (head, body) = answer
        .split_once("\r\n\r\n")
        .expect("a complete response head");
    (
        head.lines().next().unwrap_or_default().to_string(),
        body.to_string(),
    )
}

/// The engine's local API is served at `local-api.sock`, the one fixed path
/// in the runtime directory, and nothing is written under the old node
/// registry's `nodes/`.
///
/// The path is the real runtime directory's: where a runtime already answers
/// there — one running on this machine — the bind is refused naming that very
/// path, which proves the path as well.
#[tokio::test(flavor = "multi_thread")]
async fn the_engines_local_api_is_served_at_the_fixed_socket_path_and_registers_nothing() {
    let loaded = AnEngineLoadingTwoStreams::load();
    let runtime_directory = loaded.engine.runtime_directory().path().to_path_buf();
    let local_api_socket_path = runtime_directory.join("local-api.sock");
    assert_eq!(
        loaded.engine.runtime_directory().local_api_socket_path(),
        local_api_socket_path
    );
    let node_registry_entry_path = runtime_directory
        .join("nodes")
        .join(format!("{}.json", loaded.engine.runtime_id()));

    let served = match crate::serve_the_local_api_for_an_engine(&loaded.engine) {
        Ok(served) => served,
        Err(refusal) => {
            let refusal = refusal.to_string();
            assert!(
                refusal.contains(&local_api_socket_path.display().to_string())
                    && refusal.contains("already bound by a live process"),
                "{refusal}"
            );
            return;
        }
    };

    assert!(
        !node_registry_entry_path.exists(),
        "no registry entry is written: {}",
        node_registry_entry_path.display()
    );
    let (status_line, body) =
        http_get_over_the_local_api_socket(&local_api_socket_path, "/api/graph?stream=second")
            .await;
    assert!(status_line.contains(" 200 "), "{status_line}");
    assert!(
        body.contains("idle-in-second") && !body.contains("idle-in-first"),
        "{body}"
    );
    let (status_line, body) =
        http_get_over_the_local_api_socket(&local_api_socket_path, "/api/graph").await;
    assert!(status_line.contains(" 200 "), "{status_line}");
    assert!(
        body.contains("idle-in-first") && body.contains("idle-in-second"),
        "without `stream`, every loaded stream: {body}"
    );
    for request_naming_no_loaded_stream in [
        "/api/graph?stream=third",
        "/api/graph?stream=",
        "/api/registry?stream=..",
    ] {
        let (status_line, body) = http_get_over_the_local_api_socket(
            &local_api_socket_path,
            request_naming_no_loaded_stream,
        )
        .await;
        assert!(
            status_line.contains(" 404 "),
            "{request_naming_no_loaded_stream}: {status_line}"
        );
        assert!(
            body.contains("first") && body.contains("second"),
            "{request_naming_no_loaded_stream}: {body}"
        );
    }

    drop(served);

    assert!(!local_api_socket_path.exists());
}
