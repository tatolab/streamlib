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
#[cfg(feature = "machine-directories-under-a-test-root")]
use streamlib_runtime_client_contract::machine_directories_test_root::TestMachineRoot;
#[cfg(feature = "machine-directories-under-a-test-root")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// A native node with one input, the end of a link a test makes into a stream.
#[streamlib::sdk::processor(execution = manual, input("video", delivery_profile = "newest"))]
pub struct TwoStreamsTestReaderNode;

impl ManualProcessor for TwoStreamsTestReaderNode::Processor {
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

    /// Add a reader node named `reader` to the second stream.
    fn with_a_reader_in_the_second_stream(self) -> Self {
        PROCESSOR_REGISTRY.register::<TwoStreamsTestReaderNode::Processor>();
        let mut reader = ProcessorSpec::new(
            TwoStreamsTestReaderNode::processor_class_import_path(),
            json!({}),
        );
        reader.display_name = Some("reader".to_string());
        self.second
            .add_processor_reporting_its_name(reader)
            .expect("the reader is added");
        self
    }

    /// `connect` into the second stream's reader from `from_node/video` of
    /// `from_stream`, with any further arguments.
    async fn connect_into_the_reader_from(
        &self,
        from_stream: &str,
        from_node: &str,
        further_arguments: Value,
    ) -> std::result::Result<Value, rmcp::ErrorData> {
        let mut arguments = json!({
            "stream": "second",
            "from_stream": from_stream,
            "from_node": from_node,
            "from_port": "video",
            "to_node": "reader",
            "to_port": "video",
        });
        if let (Some(arguments), Some(further)) =
            (arguments.as_object_mut(), further_arguments.as_object())
        {
            arguments.extend(further.clone());
        }
        crate::mcp::tests::tool_call_outcome(
            self.operations_on_the_loaded_streams(),
            "connect",
            arguments,
        )
        .await
    }
}

fn links_in(graph: &Value) -> Vec<Value> {
    graph["links"].as_array().cloned().unwrap_or_default()
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

/// A tap reads the stream its channel's address names, with no `stream`
/// beside it; an address naming a stream not loaded, or a stream other than
/// the one beside it, is refused naming the streams.
#[tokio::test(flavor = "multi_thread")]
async fn a_tap_reads_the_stream_its_channel_names() {
    let loaded = AnEngineLoadingTwoStreams::load();
    let tap_refusal = |arguments: Value| {
        let operations = loaded.operations_on_the_loaded_streams();
        async move { tool_error_text(&tool_call_result(operations, "tap", arguments).await) }
    };

    let in_the_second = tap_refusal(json!({ "channel": "second/idle-in-second/video" })).await;
    assert!(
        in_the_second.contains("no tappable channel"),
        "the second stream was reached and has no link out of that port: {in_the_second}"
    );
    let machine = loaded.engine.machine_name().name().to_string();
    let with_this_machine =
        tap_refusal(json!({ "channel": format!("{machine}/second/idle-in-second/video") })).await;
    assert!(
        with_this_machine.contains("no tappable channel"),
        "{with_this_machine}"
    );

    let not_loaded = tap_refusal(json!({ "channel": "third/idle/video" })).await;
    assert!(
        not_loaded.contains("`third`") && not_loaded.contains("first, second"),
        "{not_loaded}"
    );
    let two_streams =
        tap_refusal(json!({ "channel": "first/idle-in-first/video", "stream": "second" })).await;
    assert!(
        two_streams.contains("`first`") && two_streams.contains("`second`"),
        "{two_streams}"
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

/// A link is made in the stream that holds its input, and takes its source
/// from another stream on this machine by `from_stream`: one from a stream
/// not loaded awaits naming it, one from a loaded stream that has not started
/// is pending, and `graph` renders each source as `{stream, node, port}` in
/// the reading stream alone.
#[tokio::test(flavor = "multi_thread")]
async fn connect_with_from_stream_holds_the_link_in_the_input_stream_awaiting_or_pending() {
    let loaded = AnEngineLoadingTwoStreams::load().with_a_reader_in_the_second_stream();

    let from_a_stream_not_loaded = first_text_block_json(
        &loaded
            .connect_into_the_reader_from("Camera Rig", "camera", json!({}))
            .await
            .expect("a link from a stream not loaded is made"),
    );
    assert_eq!(from_a_stream_not_loaded["state"], "awaiting_remote");
    assert_eq!(
        from_a_stream_not_loaded["awaiting_remote_reason"],
        "the stream `camera-rig` is not loaded on this machine; the link wires when it loads"
    );
    let from_the_first_stream = first_text_block_json(
        &loaded
            .connect_into_the_reader_from("first", "idle-in-first", json!({}))
            .await
            .expect("a link from a loaded stream is made"),
    );
    assert_eq!(from_the_first_stream["state"], "pending");
    assert!(
        from_the_first_stream
            .get("awaiting_remote_reason")
            .is_none(),
        "{from_the_first_stream}"
    );

    let graph = first_text_block_json(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "graph",
            json!({ "stream": "second" }),
        )
        .await,
    );
    let links = links_in(&graph);
    assert_eq!(links.len(), 2, "{graph}");
    let link_with_id = |link_id: &Value| {
        links
            .iter()
            .find(|link| link["id"] == *link_id)
            .unwrap_or_else(|| panic!("no link {link_id} in {graph}"))
    };
    let awaiting = link_with_id(&from_a_stream_not_loaded["link_id"]);
    assert_eq!(
        awaiting["source"],
        json!({"stream": "camera-rig", "node": "camera", "port": "video"})
    );
    assert_eq!(
        awaiting["target"],
        json!({"node": "reader", "port": "video"})
    );
    assert_eq!(awaiting["state"], "awaiting_remote");
    assert_eq!(
        awaiting["awaiting_remote_reason"],
        from_a_stream_not_loaded["awaiting_remote_reason"]
    );
    assert_eq!(
        link_with_id(&from_the_first_stream["link_id"])["source"],
        json!({"stream": "first", "node": "idle-in-first", "port": "video"})
    );
    assert!(
        links_in(&loaded.first.to_json().unwrap()).is_empty(),
        "the source's stream holds no link of the reading stream's"
    );
}

/// The recipe splicing a node into a link reading another stream connects the
/// new node from that stream with `from_stream`, since the source's node is in
/// no graph of the reading stream.
#[tokio::test(flavor = "multi_thread")]
async fn the_splice_recipe_takes_a_source_in_another_stream_with_from_stream() {
    let loaded = AnEngineLoadingTwoStreams::load().with_a_reader_in_the_second_stream();
    let connected = first_text_block_json(
        &loaded
            .connect_into_the_reader_from("first", "idle-in-first", json!({}))
            .await
            .expect("the link is made"),
    );

    let recipe = prompt_text(
        loaded.operations_on_the_loaded_streams(),
        "insert_node_between_linked_nodes",
        json!({
            "stream": "second",
            "link_id": connected["link_id"],
            "type": TwoStreamsTestReaderNode::processor_class_import_path().as_str(),
        }),
    )
    .await;
    for said in [
        "`from_stream`: `first`, `from_node`: `idle-in-first`, `from_port`: `video`",
        "carries `idle-in-first` of the stream `first` port `video` to `reader` port `video`",
    ] {
        assert!(recipe.contains(said), "{said:?} not in: {recipe}");
    }
}

/// A `from_stream` naming the stream the call names is a link of that one
/// stream, rendered `{node, port}`.
#[tokio::test(flavor = "multi_thread")]
async fn a_from_stream_naming_the_input_stream_makes_a_link_of_that_stream() {
    let loaded = AnEngineLoadingTwoStreams::load().with_a_reader_in_the_second_stream();

    loaded
        .connect_into_the_reader_from("Second", "idle-in-second", json!({}))
        .await
        .expect("a link within the second stream is made");

    let links = links_in(&loaded.second.to_json().unwrap());
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(
        links[0]["source"],
        json!({"node": "idle-in-second", "port": "video"})
    );
}

/// A `to_stream` other than `stream` is refused naming the rule and which
/// stream to name where, and makes no link.
#[tokio::test(flavor = "multi_thread")]
async fn a_to_stream_other_than_the_stream_named_is_refused_naming_the_rule() {
    let loaded = AnEngineLoadingTwoStreams::load().with_a_reader_in_the_second_stream();

    let refusal = tool_error_text(
        &loaded
            .connect_into_the_reader_from("first", "idle-in-first", json!({"to_stream": "first"}))
            .await
            .expect("a refusal is a tool error"),
    );
    for named in [
        "a link is made in the stream that holds its input",
        "Name `first` as `stream` and `first` as `from_stream`",
    ] {
        assert!(refusal.contains(named), "{named:?} not in: {refusal}");
    }
    loaded
        .connect_into_the_reader_from("first", "idle-in-first", json!({"to_stream": "Second"}))
        .await
        .expect("a `to_stream` naming `stream` itself is the input's stream");
    assert_eq!(links_in(&loaded.second.to_json().unwrap()).len(), 1);
    assert!(links_in(&loaded.first.to_json().unwrap()).is_empty());
}

/// `disconnect` by `link_id` in the reading stream removes a link from
/// another stream: on a stream not started yet, the link is marked for
/// deletion, and its start takes it out. The source stream holds no such
/// link, so naming it there is refused and leaves the link as it was.
#[tokio::test(flavor = "multi_thread")]
async fn disconnect_removes_a_link_from_another_stream() {
    let loaded = AnEngineLoadingTwoStreams::load().with_a_reader_in_the_second_stream();
    let connected = first_text_block_json(
        &loaded
            .connect_into_the_reader_from("first", "idle-in-first", json!({}))
            .await
            .expect("the link is made"),
    );

    let refusal = tool_error_text(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "disconnect",
            json!({ "stream": "first", "link_id": connected["link_id"] }),
        )
        .await,
    );
    assert!(refusal.contains("disconnect failed"), "{refusal}");
    let links = links_in(&loaded.second.to_json().unwrap());
    assert_eq!(links.len(), 1, "{links:?}");
    assert!(
        links[0]["components"].get("pending_deletion").is_none(),
        "{links:?}"
    );

    let disconnected = first_text_block_json(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "disconnect",
            json!({ "stream": "second", "link_id": connected["link_id"] }),
        )
        .await,
    );

    assert_eq!(disconnected["disconnected_link_id"], connected["link_id"]);
    let links = links_in(&loaded.second.to_json().unwrap());
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(
        links[0]["components"]["pending_deletion"], true,
        "{links:?}"
    );
}

/// `connect` takes `from_stream` and `to_stream` beside the four ends and
/// `stream`, and nothing else — no machine, since a link between machines
/// arrives with the sharing step.
#[tokio::test(flavor = "multi_thread")]
async fn the_connect_schema_takes_from_stream_and_to_stream_and_nothing_else() {
    let loaded = AnEngineLoadingTwoStreams::load().with_a_reader_in_the_second_stream();

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
        [
            "from_node",
            "from_port",
            "from_stream",
            "stream",
            "to_node",
            "to_port",
            "to_stream"
        ]
    );
    let mut required: Vec<&str> = connect["inputSchema"]["required"]
        .as_array()
        .expect("`connect` requires its ends")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    required.sort_unstable();
    assert_eq!(
        required,
        ["from_node", "from_port", "stream", "to_node", "to_port"]
    );

    let refusal = tool_error_text(
        &tool_call_result(
            loaded.operations_on_the_loaded_streams(),
            "connect",
            json!({
                "stream": "second",
                "from_machine": "desk",
                "from_stream": "first",
                "from_node": "idle-in-first",
                "from_port": "video",
                "to_node": "reader",
                "to_port": "video",
            }),
        )
        .await,
    );
    assert!(refusal.contains("`from_machine`"), "{refusal}");
    assert!(links_in(&loaded.second.to_json().unwrap()).is_empty());
}

/// The instructions an agent reads before its first call say where a link is
/// made and what a link reading another stream renders.
#[test]
fn the_instructions_say_a_link_is_made_in_its_inputs_stream_and_may_read_another() {
    let instructions = crate::mcp::LOCAL_API_MCP_SERVER_INSTRUCTIONS;
    for said in [
        "A link is made in the stream that holds its input",
        "`from_stream`, whose port must be `private` or `public`",
        "`awaiting_remote` with `awaiting_remote_reason`",
        "Lowering the source port to `internal` removes the link at once",
        "`{stream, node, port}`",
    ] {
        assert!(
            instructions.contains(said),
            "{said:?} not in the instructions"
        );
    }
    assert!(!instructions.contains("Both ends of a `connect` are ports in the one stream"));
}

/// One HTTP/1.1 GET over the socket at `local_api_socket_path`: its status
/// line and body.
#[cfg(feature = "machine-directories-under-a-test-root")]
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

    let served = crate::serve_the_local_api_for_an_engine(&loaded.engine)
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
