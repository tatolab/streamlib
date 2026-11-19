// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! MCP wire tests: an `rmcp` client drives the real `/mcp` endpoint that
//! [`crate::handlers::build_router`] wires in, served on a real local API
//! socket, through discovery, the tool catalog, and each tool through to
//! the runtime. The router is the real one; only the backend is a stub — a
//! runtime loading one stream, the stub's own `RuntimeOperations` — so the
//! MCP → runtime seam is what's under test.
//!
//! The catalog assertions are two-sided on purpose — what is advertised,
//! and what must never be again.

use crate::control_plane_stub_support::{
    LocalApiServedOnAFreshSocket, STUB_MACHINE_NAME, STUB_STREAM_ASSET, STUB_STREAM_ID,
};
use parking_lot::Mutex;
use rmcp::RoleClient;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, GetPromptRequestParams, ReadResourceRequestParams,
    RequestMetaObject,
};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RunningService, ServiceError,
    SubscriptionEnd,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, UnixSocketHttpClient};
use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::{
    BoxFuture, OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations, StreamListingState,
    StreamLocation,
};
use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;

use std::time::Duration;

use super::*;
use serde_json::json;
use streamlib::sdk::descriptors::NodeClassImportPath;

mod graph_edit_tool_tests;
mod graph_logs_and_stream_tool_tests;
mod mcp_discovery_catalog_and_transport_tests;
mod resource_and_prompt_tests;

/// Stub runtime answering the observation ops without a live engine.
///
/// Every graph-mutating op records what it was handed and answers a fixed
/// id, so a mutation tool's test asserts the op it reached and the
/// arguments it carried.
#[derive(Clone)]
pub(crate) struct ControlPlaneMcpDispatchStubRuntime {
    exported_graph: Arc<Mutex<Value>>,
    recorded_graph_mutations: crate::control_plane_stub_support::RecordedGraphMutations,
    armed_add_node_refusal: crate::control_plane_stub_support::ArmedAddNodeRefusal,
    add_node_answers_after: Option<Duration>,
}

impl ControlPlaneMcpDispatchStubRuntime {
    pub(crate) fn new() -> Self {
        Self {
            exported_graph: Arc::new(Mutex::new(json!({ "nodes": [], "links": [] }))),
            recorded_graph_mutations: Arc::new(Mutex::new(Vec::new())),
            armed_add_node_refusal: Arc::new(Mutex::new(None)),
            add_node_answers_after: None,
        }
    }

    /// A stub whose `add_node` refuses with `refusal`, standing in for an
    /// engine-side refusal the front end has to carry back.
    fn refusing_every_add_node(refusal: &str) -> Self {
        let refusal = refusal.to_owned();
        Self::refusing_every_add_node_with(move || {
            streamlib::sdk::error::Error::Configuration(refusal.clone())
        })
    }

    /// A stub whose `add_node` refuses with the engine error `the_refusal` makes, standing in
    /// for a typed engine refusal the front end has to carry back with its code.
    pub(crate) fn refusing_every_add_node_with(
        the_refusal: impl Fn() -> streamlib::sdk::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            armed_add_node_refusal: Arc::new(Mutex::new(Some(Arc::new(the_refusal)))),
            ..Self::new()
        }
    }

    /// A stub whose `add_node` answers only once `answers_after` has
    /// passed, so it is the request still in flight.
    pub(crate) fn with_add_node_answering_after(answers_after: Duration) -> Self {
        Self {
            add_node_answers_after: Some(answers_after),
            ..Self::new()
        }
    }
}

impl RuntimeOperations for ControlPlaneMcpDispatchStubRuntime {
    fn to_json_async(&self) -> BoxFuture<'_, Result<Value>> {
        let exported_graph = self.exported_graph.lock().clone();
        Box::pin(async move { Ok(exported_graph) })
    }
    crate::control_plane_stub_support::every_reserved_slot_reader_is_unreachable!("tool");
    crate::control_plane_stub_support::graph_mutation_ops_record_the_call!();
    fn to_json(&self) -> Result<Value> {
        Ok(json!({}))
    }
}

crate::control_plane_stub_support::a_stub_runtime_loading_this_stub_as_its_only_stream!(
    ControlPlaneMcpDispatchStubRuntime
);

/// The committed contract file, which every expectation of the served surface is read from.
pub(crate) fn the_committed_control_contract() -> Value {
    serde_json::from_str(include_str!(
        "../../../../streamlib-control-contract/contract.json"
    ))
    .expect("contract.json is JSON")
}

/// The operations `contract.json` exposes on the agent surface.
pub(crate) fn the_operations_the_contract_serves_to_agents() -> Vec<Value> {
    the_committed_control_contract()["methods"]
        .as_array()
        .expect("the contract lists its methods")
        .iter()
        .filter(|method| {
            method["x-exposures"]
                .as_array()
                .is_some_and(|exposures| exposures.contains(&json!("agent")))
        })
        .cloned()
        .collect()
}

/// The tools `contract.json` says `tools/list` holds: each operation it lists as a tool of
/// its own, then its discovery tools.
pub(crate) fn the_tools_the_contract_lists() -> Vec<Value> {
    let contract = the_committed_control_contract();
    the_operations_the_contract_serves_to_agents()
        .into_iter()
        .filter(|method| method["x-agent-tool-listing"] == "listed_as_a_tool")
        .chain(
            contract["x-agent-discovery-tools"]
                .as_array()
                .expect("the contract lists its discovery tools")
                .iter()
                .cloned(),
        )
        .collect()
}

/// Exact, not a superset: a tool appearing that `contract.json` does not list is a surface
/// the contract does not grant.
pub(crate) fn assert_names_exactly_the_tools_the_contract_lists(advertised_tool_names: Vec<&str>) {
    let tools_the_contract_lists = the_tools_the_contract_lists();
    let contract_tool_names: Vec<&str> = tools_the_contract_lists
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        advertised_tool_names, contract_tool_names,
        "tools/list must advertise exactly the tools contract.json lists, in its order"
    );
}

/// The authority the MCP client names in `Host`; the socket path is the
/// address.
const LOCAL_API_MCP_URI: &str = "http://localhost/mcp";

pub(crate) fn mcp_transport_to(
    served: &LocalApiServedOnAFreshSocket,
) -> StreamableHttpClientTransport<UnixSocketHttpClient> {
    mcp_transport_naming(served, LOCAL_API_MCP_URI)
}

fn mcp_transport_naming(
    served: &LocalApiServedOnAFreshSocket,
    local_api_mcp_uri: &str,
) -> StreamableHttpClientTransport<UnixSocketHttpClient> {
    StreamableHttpClientTransport::with_client(
        UnixSocketHttpClient::new(
            served.local_api_socket_path.to_str().unwrap(),
            local_api_mcp_uri,
        ),
        StreamableHttpClientTransportConfig::with_uri(local_api_mcp_uri.to_string()),
    )
}

/// An `rmcp` client at the latest revision, connected to the real router
/// over `runtime`.
async fn connected_mcp_client(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> (LocalApiServedOnAFreshSocket, RunningService<RoleClient, ()>) {
    let served = LocalApiServedOnAFreshSocket::over(runtime);
    let client = ()
        .serve_with_lifecycle(
            mcp_transport_to(&served),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::LATEST],
            },
        )
        .await
        .expect("the node answers `server/discover` at the latest revision");
    (served, client)
}

/// One `tools/call`: the result as it crossed the wire, or the protocol
/// error that refused it.
pub(crate) async fn tool_call_outcome(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    tool_name: &str,
    arguments: Value,
) -> std::result::Result<Value, McpError> {
    let (_served, client) = connected_mcp_client(runtime).await;
    wire_outcome(
        client
            .call_tool(the_tool_call_that_reaches(tool_name, &arguments))
            .await,
    )
}

/// The `tools/call` an agent sends to run `operation_name` with `arguments`: the operation's
/// own tool when the server lists it, `call_operation` otherwise.
pub(crate) fn the_tool_call_that_reaches(
    operation_name: &str,
    arguments: &Value,
) -> CallToolRequestParams {
    let (tool_name, tool_arguments) = streamlib_control_contract::the_agent_tool_call_that_reaches(
        operation_name,
        arguments.as_object().cloned().unwrap_or_default(),
    );
    CallToolRequestParams::new(tool_name).with_arguments(tool_arguments)
}

/// A result or refusal as it crossed the wire.
fn wire_outcome<T: serde::Serialize>(
    outcome: std::result::Result<T, ServiceError>,
) -> std::result::Result<Value, McpError> {
    match outcome {
        Ok(result) => Ok(serde_json::to_value(result).unwrap()),
        Err(ServiceError::McpError(refusal)) => Err(refusal),
        Err(other) => panic!("the request failed below the protocol: {other}"),
    }
}

/// The refusal a failing tool result carries, asserting its text block is the same refusal as
/// its `structuredContent.refusal`.
pub(crate) fn the_refusal_of_a_tool_result(
    tool_result: &Value,
) -> streamlib_runtime_client_contract::local_api_wire_contract::ProjectActionRefusalOnTheWire {
    assert_eq!(tool_result["isError"], true, "{tool_result}");
    let refusal_as_text: Value = serde_json::from_str(
        tool_result["content"][0]["text"]
            .as_str()
            .expect("a text block"),
    )
    .expect("a refusal's text block is JSON");
    assert_eq!(
        refusal_as_text, tool_result["structuredContent"]["refusal"],
        "the text and the structured refusal are one refusal"
    );
    serde_json::from_value(refusal_as_text).expect("a refusal reads as the wire's refusal")
}

/// The JSON a successful tool result states in its first text block.
pub(crate) fn first_text_block_json(tool_result: &Value) -> Value {
    assert_eq!(tool_result["isError"], false, "{tool_result}");
    serde_json::from_str(
        tool_result["content"][0]["text"]
            .as_str()
            .expect("a text block"),
    )
    .expect("the text block is JSON")
}

pub(crate) async fn tool_call_result(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    tool_name: &str,
    arguments: Value,
) -> Value {
    tool_call_outcome(runtime, tool_name, arguments)
        .await
        .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal:?}"))
}

/// A client that knows the handshake revisions only.
pub(crate) struct HandshakeRevisionMcpClient {
    pub(crate) handshake_protocol_version: ProtocolVersion,
}

impl rmcp::ClientHandler for HandshakeRevisionMcpClient {
    fn get_info(&self) -> rmcp::model::ClientConfig {
        rmcp::model::ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("handshake-test", "0"),
        )
        .with_protocol_version(self.handshake_protocol_version.clone())
    }
}

/// The operations the stub was handed live, in call order.
fn recorded_stream_graph_operations(
    recorded: &crate::control_plane_stub_support::RecordedGraphMutations,
) -> Vec<streamlib_project_repository::StreamGraphOperation> {
    recorded.lock().clone()
}

/// `graph`, the stub stream's graph as a graph call answers it: with the condition the stub's
/// listing gives its stream.
fn as_a_graph_call_answers_it(mut graph: Value) -> Value {
    graph["condition"] = the_stub_streams_condition();
    graph
}

/// The condition the stub's listing gives its stream: loaded, playing and healthy.
fn the_stub_streams_condition() -> Value {
    json!({ "load": "loaded", "playback": "playing", "health": "healthy", "diagnostics": [] })
}

/// Two nodes and the one link between them, in the shape the engine's
/// graph export takes, so the prompts parse what a real node answers.
fn two_linked_nodes_graph() -> Value {
    json!({
        "stream": crate::control_plane_stub_support::STUB_STREAM_ID,
        "nodes": [
            {
                "id": "PatternSourceId",
                "type": "graph_probes:PatternSource",
                "name": "pattern",
                "ports": {
                    "inputs": [],
                    "outputs": [{ "name": "video", "description": "", "delivery_profile": null }]
                },
                "components": { "state": "Running" }
            },
            {
                "id": "WindowSinkId",
                "type": "graph_probes:WindowSink",
                "name": "window",
                "ports": {
                    "inputs": [{ "name": "video", "description": "", "delivery_profile": "newest" }],
                    "outputs": []
                },
                "components": { "state": "Running" }
            }
        ],
        "links": [{
            "id": "link-pattern-to-window",
            "source": { "node": "pattern", "port": "video" },
            "target": { "node": "window", "port": "video" },
            "state": "wired",
            "components": {}
        }],
        "machine": "desk"
    })
}

async fn resource_read_outcome(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    uri: &str,
) -> std::result::Result<Value, McpError> {
    let (_served, client) = connected_mcp_client(runtime).await;
    wire_outcome(
        client
            .read_resource(ReadResourceRequestParams::new(uri))
            .await,
    )
}

async fn prompt_outcome(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    prompt_name: &str,
    arguments: Value,
) -> std::result::Result<Value, McpError> {
    let (_served, client) = connected_mcp_client(runtime).await;
    let request = GetPromptRequestParams::new(prompt_name)
        .with_arguments(arguments.as_object().cloned().unwrap_or_default());
    wire_outcome(client.get_prompt(request).await)
}

pub(crate) async fn resource_document(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    uri: &str,
) -> Value {
    let result = resource_read_outcome(runtime, uri)
        .await
        .unwrap_or_else(|refusal| panic!("reading `{uri}` was refused: {refusal:?}"));
    let contents = result["contents"].as_array().expect("a contents array");
    assert_eq!(contents.len(), 1, "one document per resource: {result}");
    assert_eq!(contents[0]["uri"], uri);
    assert_eq!(contents[0]["mimeType"], "application/json");
    serde_json::from_str(contents[0]["text"].as_str().expect("a text document"))
        .expect("the document is JSON")
}

pub(crate) async fn prompt_text(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    prompt_name: &str,
    arguments: Value,
) -> String {
    let result = prompt_outcome(runtime, prompt_name, arguments)
        .await
        .unwrap_or_else(|refusal| panic!("`{prompt_name}` was refused: {refusal:?}"));
    let messages = result["messages"].as_array().expect("a messages array");
    assert_eq!(messages.len(), 1, "{result}");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"]["type"], "text");
    messages[0]["content"]["text"].as_str().unwrap().to_string()
}
