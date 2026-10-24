// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API over a real engine loading two streams: each call reaches the
//! stream it names, a stream not loaded is refused naming the loaded ones,
//! `graph` naming none renders both, and no graph holds a node of the local
//! API's own.

use std::sync::Arc;

#[cfg(feature = "machine-directories-under-a-test-root")]
use crate::control_plane_stub_support::http_get_over_the_local_api_socket;
use serde_json::{Value, json};
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::OutputPortExposureLevel;
use streamlib::sdk::processors::{ManualProcessor, PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::{
    LoadedStreamInThisRuntime, OperationsOnTheStreamsLoadedInThisRuntime,
    OptionsForLoadingOneStream, Runner,
};
#[cfg(feature = "machine-directories-under-a-test-root")]
use streamlib_runtime_client_contract::machine_directories_test_root::TestMachineRoot;

use crate::mcp::tests::{
    first_text_block_json, listed_tools, prompt_text, resource_document, tool_call_result,
};

/// A native node with nothing to do, added to each stream so each graph has a
/// node of its own to tell it apart; its one output gives a prompt a port.
#[streamlib::sdk::processor(execution = manual, output("video"))]
pub struct TwoStreamsTestIdleNode;

impl ManualProcessor for TwoStreamsTestIdleNode::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// A native node with nothing to do whose one output declares a catalog entry.
#[streamlib::sdk::processor(
    execution = manual,
    output("blocks", catalog = BinaryEntry { mode: Snapshot, mime: "application/msgpack" }),
)]
pub struct TwoStreamsTestCataloguedNode;

impl ManualProcessor for TwoStreamsTestCataloguedNode::Processor {
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

/// `graph` naming no stream renders both under the machine's name; a tool
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
    assert_eq!(machine_wide["machine"], loaded.engine.machine_name().name());
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

/// `connect` takes `stream` and the four ends, and nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn the_connect_schema_takes_the_stream_and_the_four_ends_and_nothing_else() {
    let loaded = AnEngineLoadingTwoStreams::load();

    let tools = listed_tools(loaded.operations_on_the_loaded_streams()).await;
    let connect = tools
        .iter()
        .find(|tool| tool["name"] == "connect")
        .expect("`connect` is listed");
    let mut arguments: Vec<&str> = connect["inputSchema"]["properties"]
        .as_object()
        .expect("`connect` lists its arguments")
        .keys()
        .map(String::as_str)
        .collect();
    arguments.sort_unstable();
    assert_eq!(
        arguments,
        ["from_node", "from_port", "stream", "to_node", "to_port"]
    );
}

/// The instructions an agent reads before its first call say both ends of a
/// link are in the stream `connect` names, and how a stream reads another
/// stream's port.
#[test]
fn the_instructions_say_both_ends_of_a_connect_are_in_its_stream_and_name_relay_track_source() {
    let instructions = crate::mcp::LOCAL_API_MCP_SERVER_INSTRUCTIONS;
    for said in [
        "Both ends of a `connect` are ports in the one stream it names",
        "a stream reads another stream's port, on this machine or another, through a \
         `RelayTrackSource` node",
    ] {
        assert!(
            instructions.contains(said),
            "{said:?} not in the instructions"
        );
    }
}

/// The engine's local API is served at `local-api.sock`, the one fixed path
/// in the runtime directory under this run's test machine root, and the socket
/// is gone once the served API drops.
///
/// A test build only: binding `local-api.sock` at the real machine's runtime
/// directory would replace the socket of the runtime running there.
#[cfg(feature = "machine-directories-under-a-test-root")]
#[tokio::test(flavor = "multi_thread")]
async fn the_engines_local_api_is_served_at_the_fixed_socket_path_under_the_test_machine_root() {
    let test_machine_root = TestMachineRoot::from_the_environment()
        .expect("a test build runs under TATOLAB_TEST_MACHINE_ROOT");
    let loaded = AnEngineLoadingTwoStreams::load();
    let runtime_directory = loaded.engine.runtime_directory().path().to_path_buf();
    assert_eq!(runtime_directory, test_machine_root.runtime_directory());
    let local_api_socket_path = runtime_directory.join("local-api.sock");
    assert_eq!(
        loaded.engine.runtime_directory().local_api_socket_path(),
        local_api_socket_path
    );

    let served = crate::serve_the_local_api_for_an_engine(
        &loaded.engine,
        crate::control_plane_stub_support::RelayInstancesOfThisMachineStub::shared(),
    )
    .expect("the local API binds the fixed socket under this run's test machine root");

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

/// The loopback listing names the machine and every loaded stream, each with
/// the ports it exposes at their levels and catalog entries; a stream exposing
/// nothing is listed with none, and an internal port is not listed.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(tcp_listeners_of_this_test_process)]
async fn the_loopback_listing_names_every_stream_and_the_ports_each_exposes() {
    let loaded = AnEngineLoadingTwoStreams::load();
    PROCESSOR_REGISTRY.register::<TwoStreamsTestCataloguedNode::Processor>();
    let mut catalogued_node = ProcessorSpec::new(
        TwoStreamsTestCataloguedNode::processor_class_import_path(),
        json!({}),
    );
    catalogued_node.display_name = Some("catalogued".to_string());
    loaded
        .first
        .add_processor_reporting_its_name(catalogued_node)
        .expect("the catalogued node is added");
    let served = crate::serve_the_loopback_http_listener(
        loaded.operations_on_the_loaded_streams(),
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a free loopback port binds"),
        tempfile::tempdir().unwrap().path().to_path_buf(),
        &tokio::runtime::Handle::current(),
    )
    .expect("a bound loopback listener is served");

    let (_, nothing_exposed) =
        crate::loopback_http_listener::tests::http_get(served.bound_address(), "/").await;
    let nothing_exposed: Value = serde_json::from_str(&nothing_exposed).unwrap();
    assert_eq!(
        nothing_exposed["streams"],
        json!([
            { "name": "first", "exposed_output_ports": [] },
            { "name": "second", "exposed_output_ports": [] },
        ])
    );

    loaded
        .first
        .set_output_port_exposure_level("catalogued", "blocks", OutputPortExposureLevel::Public)
        .unwrap();
    loaded
        .first
        .set_output_port_exposure_level("idle-in-first", "video", OutputPortExposureLevel::Private)
        .unwrap();
    let (status_code, listing) =
        crate::loopback_http_listener::tests::http_get(served.bound_address(), "/").await;

    assert_eq!(status_code, 200, "{listing}");
    let listing: Value = serde_json::from_str(&listing).unwrap();
    assert_eq!(listing["machine"], loaded.engine.machine_name().name());
    let [first, second] = [0, 1].map(|stream| listing["streams"][stream].clone());
    assert_eq!(
        second,
        json!({ "name": "second", "exposed_output_ports": [] })
    );
    assert_eq!(first["name"], "first");
    let mut exposed_output_ports = first["exposed_output_ports"]
        .as_array()
        .expect("a stream lists its exposed ports")
        .clone();
    exposed_output_ports.sort_by_key(|exposed_output_port| exposed_output_port["node"].to_string());
    assert_eq!(
        exposed_output_ports,
        [
            json!({
                "node": "catalogued",
                "port": "blocks",
                "level": "public",
                "catalog_entry": {
                    "section": "binary",
                    "entry": { "mode": "snapshot", "mime": "application/msgpack" },
                },
            }),
            json!({
                "node": "idle-in-first",
                "port": "video",
                "level": "private",
                "catalog_entry": null,
            }),
        ]
    );
}
