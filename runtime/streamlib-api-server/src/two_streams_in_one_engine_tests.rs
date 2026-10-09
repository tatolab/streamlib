// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API over a real engine loading two streams: each call reaches the
//! stream it names, a stream not loaded is refused naming the loaded ones, an
//! unnamed call reaches the sole stream and is refused while two are loaded,
//! and no graph holds a node of the local API's own.

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
/// node of its own to tell it apart.
#[streamlib::sdk::processor(execution = manual)]
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
    }
    assert!(
        node_names_in(&loaded.first.to_json().unwrap()).contains(&"idle-in-first".to_string()),
        "a refused call removes nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unnamed_call_is_refused_while_two_are_loaded_and_reaches_the_sole_one_after() {
    let loaded = AnEngineLoadingTwoStreams::load();

    let ambiguous = tool_error_text(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "graph",
            json!({}),
        )
        .await,
    );
    assert!(ambiguous.contains("`stream`"), "{ambiguous}");
    assert!(
        ambiguous.contains("first") && ambiguous.contains("second"),
        "{ambiguous}"
    );

    loaded
        .engine
        .unload_stream("second")
        .expect("the second stream unloads");
    let sole = first_text_block_json(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "graph",
            json!({}),
        )
        .await,
    );
    assert_eq!(sole["stream"], "first", "{sole}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_resources_and_prompts_say_which_streams_are_loaded_until_one_is_sole() {
    let loaded = AnEngineLoadingTwoStreams::load();

    for uri in [
        crate::mcp_resources::LIVE_GRAPH_RESOURCE_URI,
        crate::mcp_resources::NODE_CATALOG_RESOURCE_URI,
    ] {
        let document = resource_document(loaded.operations_on_the_loaded_streams(), uri).await;
        assert_eq!(
            document["loaded_streams"],
            json!(["first", "second"]),
            "{uri}: {document}"
        );
    }
    let recipe = prompt_text(
        loaded.operations_on_the_loaded_streams(),
        "look_at_what_a_channel_carries",
        json!({ "from_node": "idle-in-first", "from_port": "video" }),
    )
    .await;
    assert!(
        recipe.contains("first") && recipe.contains("second") && recipe.contains("`stream`"),
        "{recipe}"
    );

    loaded
        .engine
        .unload_stream("second")
        .expect("the second stream unloads");
    let graph = resource_document(
        loaded.operations_on_the_loaded_streams(),
        crate::mcp_resources::LIVE_GRAPH_RESOURCE_URI,
    )
    .await;
    assert_eq!(graph["stream"], "first", "{graph}");
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

#[tokio::test(flavor = "multi_thread")]
async fn the_engines_local_api_is_served_at_its_socket_and_registered_until_let_go() {
    let loaded = AnEngineLoadingTwoStreams::load();
    let runtime_id = loaded.engine.runtime_id().to_string();
    let local_api_socket_path = loaded
        .engine
        .runtime_directory()
        .local_api_socket_path(runtime_id.as_str());
    let node_registry_entry_path = loaded
        .engine
        .runtime_directory()
        .node_registry_directory()
        .join(format!("{runtime_id}.json"));

    let served = crate::serve_the_local_api_for_an_engine(&loaded.engine)
        .expect("the engine's local API is served");

    assert!(node_registry_entry_path.exists());
    let (status_line, body) =
        http_get_over_the_local_api_socket(&local_api_socket_path, "/api/graph?stream=second")
            .await;
    assert!(status_line.contains(" 200 "), "{status_line}");
    assert!(
        body.contains("idle-in-second") && !body.contains("idle-in-first"),
        "{body}"
    );
    for request_naming_no_loaded_stream in [
        "/api/graph",
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
    assert!(!node_registry_entry_path.exists());
}
