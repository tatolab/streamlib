// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rmcp::RoleClient;
use rmcp::model::ProtocolVersion;
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use serde_json::{Value, json};
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_control_contract::CONTROL_CONTRACT_VERSION;
use streamlib_project_repository::{ProjectRepository, ProjectTransactionAuthor};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::control_plane_stub_support::{
    LocalApiServedOnAFreshSocket, the_stub_runtimes_description,
};
use crate::mcp::tests::{ControlPlaneMcpDispatchStubRuntime, mcp_transport_to};

/// A runtime's local API served over a stub runtime, with an MCP client of its own.
pub(crate) struct ControlServedWithAnMcpClient {
    pub(crate) served: LocalApiServedOnAFreshSocket,
    pub(crate) mcp_client: RunningService<RoleClient, ()>,
}

impl ControlServedWithAnMcpClient {
    pub(crate) async fn over(
        operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    ) -> Self {
        let served = LocalApiServedOnAFreshSocket::over(operations_on_the_loaded_streams);
        let mcp_client = ()
            .serve_with_lifecycle(
                mcp_transport_to(&served),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::LATEST],
                },
            )
            .await
            .expect("the runtime answers `server/discover`");
        Self { served, mcp_client }
    }

    async fn over_the_stub() -> Self {
        Self::over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await
    }

    /// One MCP tool call's result as it crossed the wire.
    pub(crate) async fn mcp_tool_result(&self, tool_name: &str, arguments: Value) -> Value {
        let request = crate::mcp::tests::the_tool_call_that_reaches(tool_name, &arguments);
        serde_json::to_value(self.mcp_client.call_tool(request).await.unwrap()).unwrap()
    }

    /// One `POST /control` with `extra_headers`: its status line and its body, `Null` when empty.
    async fn control_post_with_headers(&self, extra_headers: &str, body: &str) -> (String, Value) {
        let mut connection = UnixStream::connect(&self.served.local_api_socket_path)
            .await
            .unwrap();
        let request = format!(
            "POST /control HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}\r\n{body}",
            body.len()
        );
        connection.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        connection.read_to_string(&mut response).await.unwrap();
        let (head, response_body) = response.split_once("\r\n\r\n").unwrap();
        let status_line = head.lines().next().unwrap_or_default().to_owned();
        let response_body = if response_body.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(response_body).unwrap_or_else(|e| panic!("{e}: {response_body}"))
        };
        (status_line, response_body)
    }

    pub(crate) async fn control_call(&self, method: &str, params: Value) -> Value {
        let request = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
        let (status_line, answer) = self
            .control_post_with_headers("", &request.to_string())
            .await;
        assert!(status_line.contains(" 200 "), "{status_line}");
        assert_eq!(answer["id"], 7, "{answer}");
        answer
    }

    /// A WebSocket opened at `/control/ws` with `extra_headers`, or the status the
    /// runtime refused it with.
    pub(crate) async fn control_websocket_with_headers(
        &self,
        extra_headers: &[(&str, String)],
    ) -> Result<ControlWebSocketMessages, u16> {
        let stream = UnixStream::connect(&self.served.local_api_socket_path)
            .await
            .unwrap();
        let mut request = "ws://localhost/control/ws".into_client_request().unwrap();
        for (name, value) in extra_headers {
            request.headers_mut().insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        match tokio_tungstenite::client_async(request, stream).await {
            Ok((websocket, _)) => Ok(ControlWebSocketMessages { websocket }),
            Err(tokio_tungstenite::tungstenite::Error::Http(refusal)) => {
                Err(refusal.status().as_u16())
            }
            Err(not_opened) => panic!("the control WebSocket did not open: {not_opened}"),
        }
    }

    pub(crate) async fn control_websocket(&self) -> ControlWebSocketMessages {
        self.control_websocket_with_headers(&[])
            .await
            .expect("the runtime opens the control WebSocket")
    }
}

/// A WebSocket open at `/control/ws`, one JSON-RPC message per text frame.
pub(crate) struct ControlWebSocketMessages {
    websocket: tokio_tungstenite::WebSocketStream<UnixStream>,
}

impl ControlWebSocketMessages {
    pub(crate) async fn send(&mut self, message: Value) {
        self.websocket
            .send(WebSocketMessage::Text(message.to_string().into()))
            .await
            .unwrap();
    }

    async fn send_a_binary_frame(&mut self, bytes: &'static [u8]) {
        self.websocket
            .send(WebSocketMessage::Binary(bytes.into()))
            .await
            .unwrap();
    }

    pub(crate) async fn next_message(&mut self) -> Value {
        loop {
            let received = tokio::time::timeout(Duration::from_secs(10), self.websocket.next())
                .await
                .expect("the runtime writes a message")
                .expect("the WebSocket is open")
                .unwrap();
            if let WebSocketMessage::Text(text) = received {
                return serde_json::from_str(text.as_str())
                    .unwrap_or_else(|e| panic!("{e}: {text:?}"));
            }
        }
    }

    /// The next message but a `project_changed` of `files_changed`, which a transaction's own
    /// writes are sent as too.
    pub(crate) async fn next_message_but_files_changed(&mut self) -> Value {
        loop {
            let message = self.next_message().await;
            if message["params"]["change"] != "files_changed" {
                return message;
            }
        }
    }

    async fn is_closed_by_the_runtime_within(&mut self, wait: Duration) -> bool {
        let closed = async {
            while let Some(received) = self.websocket.next().await {
                if received.is_err() {
                    break;
                }
            }
        };
        tokio::time::timeout(wait, closed).await.is_ok()
    }

    async fn close(&mut self) {
        self.websocket.close(None).await.unwrap();
    }
}

/// What an MCP tool result says: its result, or its refusal's structured content.
fn the_mcp_answer(tool_result: &Value) -> Result<Value, Value> {
    let text = tool_result["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    if tool_result["isError"] == true {
        return Err(tool_result["structuredContent"]["refusal"].clone());
    }
    Ok(serde_json::from_str(text).unwrap())
}

/// What a JSON-RPC answer says: its result, or its error's data.
fn the_json_rpc_answer(answer: &Value) -> Result<Value, Value> {
    match answer.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(answer["error"]["data"].clone()),
    }
}

/// `answer` with every UUID it holds read as one placeholder, since each call mints its own.
fn with_each_minted_uuid_masked(answer: Result<Value, Value>) -> Result<Value, Value> {
    fn masked(value: Value) -> Value {
        match value {
            Value::String(text) if uuid::Uuid::parse_str(&text).is_ok() => json!("<uuid>"),
            Value::Array(items) => Value::Array(items.into_iter().map(masked).collect()),
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, masked(value)))
                    .collect(),
            ),
            other => other,
        }
    }
    answer.map(masked).map_err(masked)
}

#[tokio::test]
async fn each_operation_answers_over_control_what_its_mcp_tool_answers() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let stream_asset = crate::control_plane_stub_support::STUB_STREAM_ASSET;
    let calls = [
        ("describe_runtime", json!({})),
        ("list_streams", json!({})),
        ("graph", json!({})),
        ("transact", json!({ "operations": [], "mode": "live" })),
        (
            "add_node",
            json!({ "stream": stream_asset, "type": "nodes.fx:Fx", "mode": "live" }),
        ),
        (
            "connect",
            json!({ "stream": stream_asset, "mode": "live",
                        "source": { "node": "nowhere", "port": "video" },
                        "target": { "node": "nobody", "port": "video" } }),
        ),
        (
            "open_project",
            json!({ "project_directory": "relative/rig" }),
        ),
        (
            "pause_playback",
            json!({ "project_directory": "relative/rig" }),
        ),
        (
            "describe_stream_action",
            json!({ "action": "an action nobody queued" }),
        ),
        (
            "remove_node",
            json!({ "stream": stream_asset, "node": "nobody", "mode": "live" }),
        ),
        ("list_samples", json!({})),
        ("logs", json!({ "project_directory": "relative/rig" })),
        (
            "open_viewer",
            json!({ "stream": "no such stream", "node_id": "nobody", "port": "video" }),
        ),
        (
            "close_viewer",
            json!({ "viewer": "a viewer nobody opened" }),
        ),
        (
            "set_stream_clock_scale",
            json!({ "stream": "no such stream", "scale": 0.5 }),
        ),
        (
            "write_graph_input",
            json!({ "stream": "no such stream", "input": "gain", "value": 1 }),
        ),
        ("undo", json!({ "project_directory": "relative/rig" })),
        (
            "refresh_fragments",
            json!({ "project_directory": "relative/rig", "changed_files": [] }),
        ),
        (
            "list_assets",
            json!({ "project_directory": "/no/such/project" }),
        ),
        (
            "rename_project",
            json!({ "project_directory": "/no/such/project", "name": "Rig" }),
        ),
        (
            "list_dependencies",
            json!({ "project_directory": "/no/such/project" }),
        ),
        (
            "list_project_files",
            json!({ "project_directory": "/no/such/project" }),
        ),
        (
            "list_project_node_types",
            json!({ "project_directory": "/no/such/project" }),
        ),
        (
            "list_project_views",
            json!({ "project_directory": "/no/such/project" }),
        ),
    ];
    for (method, params) in calls {
        let over_mcp = the_mcp_answer(&served.mcp_tool_result(method, params.clone()).await);
        let over_control = the_json_rpc_answer(&served.control_call(method, params).await);
        assert_eq!(
            with_each_minted_uuid_masked(over_control),
            with_each_minted_uuid_masked(over_mcp),
            "`{method}`"
        );
    }
}

#[tokio::test]
async fn every_registered_operation_is_served_over_control() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    for declaration in streamlib_control_contract::CONTROL_OPERATIONS {
        let answer = served
            .control_call(declaration.method_name, json!({}))
            .await;
        assert_ne!(
            answer["error"]["code"], -32601,
            "`{}` is registered but not served: {answer}",
            declaration.method_name
        );
    }
}

#[tokio::test]
async fn discover_answers_the_contract_this_runtime_serves() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let discovered = served
        .control_call(
            "discover",
            json!({ "contract_version": CONTROL_CONTRACT_VERSION }),
        )
        .await;
    let this_runtime = the_stub_runtimes_description();
    let registered: Vec<&str> = streamlib_control_contract::CONTROL_OPERATIONS
        .iter()
        .map(|declaration| declaration.method_name)
        .collect();
    assert_eq!(
        discovered["result"],
        json!({
            "contract_version": CONTROL_CONTRACT_VERSION,
            "runtime_version": this_runtime.version,
            "build_id": this_runtime.build_id,
            "operations": registered,
        })
    );
}

#[tokio::test]
async fn a_client_built_against_another_contract_version_is_refused_naming_both() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let other_version = CONTROL_CONTRACT_VERSION + 1;
    let refused = served
        .control_call("discover", json!({ "contract_version": other_version }))
        .await;
    assert_eq!(refused["error"]["code"], 3, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains(&other_version.to_string()), "{message}");
    assert!(
        message.contains(&CONTROL_CONTRACT_VERSION.to_string()),
        "{message}"
    );
    assert_eq!(
        refused["error"]["data"],
        json!({
            "client_contract_version": other_version,
            "runtime_contract_version": CONTROL_CONTRACT_VERSION,
        })
    );

    let header = format!("tatolab-control-contract-version: {other_version}\r\n");
    let (_, refused_by_header) = served
        .control_post_with_headers(
            &header,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "list_streams" }).to_string(),
        )
        .await;
    assert_eq!(refused_by_header["error"]["code"], 3, "{refused_by_header}");

    let refused_websocket = served
        .control_websocket_with_headers(&[(
            "tatolab-control-contract-version",
            other_version.to_string(),
        )])
        .await;
    assert_eq!(refused_websocket.err(), Some(409));
}

#[tokio::test]
async fn a_malformed_call_is_refused_with_the_json_rpc_code_for_it() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let unknown = served.control_call("no_such_operation", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
    let invalid = served
        .control_call("list_streams", json!({ "unexpected": true }))
        .await;
    assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
    let (_, not_json) = served.control_post_with_headers("", "{ not json").await;
    assert_eq!(not_json["error"]["code"], -32700, "{not_json}");
    let (status_line, unanswered) = served
        .control_post_with_headers(
            "",
            &json!({ "jsonrpc": "2.0", "method": "list_streams" }).to_string(),
        )
        .await;
    assert!(status_line.contains(" 204 "), "{status_line}");
    assert_eq!(unanswered, Value::Null);
}

#[tokio::test]
async fn a_subscription_over_a_one_shot_post_is_refused_naming_the_websocket_route() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let refused = served
        .control_call(
            "subscribe_project_changes",
            json!({ "projects": [uuid::Uuid::new_v4()] }),
        )
        .await;
    assert_eq!(refused["error"]["code"], 2, "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("/control/ws"),
        "{refused}"
    );
}

#[tokio::test]
async fn the_websocket_answers_each_request_by_its_id_and_a_binary_frame_as_not_a_message() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let mut stream = served.control_websocket().await;
    stream
        .send(json!({ "jsonrpc": "2.0", "id": "a", "method": "discover", "params": {} }))
        .await;
    stream
        .send(json!({ "jsonrpc": "2.0", "id": "b", "method": "list_streams" }))
        .await;
    let mut answered_ids = vec![
        stream.next_message().await["id"].clone(),
        stream.next_message().await["id"].clone(),
    ];
    answered_ids.sort_by_key(Value::to_string);
    assert_eq!(answered_ids, [json!("a"), json!("b")]);
    stream.send_a_binary_frame(b"{}").await;
    assert_eq!(stream.next_message().await["error"]["code"], -32700);
    stream.close().await;
    assert!(
        stream
            .is_closed_by_the_runtime_within(Duration::from_secs(5))
            .await
    );
}

#[tokio::test]
async fn a_websocket_subscriber_is_sent_a_change_another_client_made() {
    let project_parent_directory = tempfile::tempdir().unwrap();
    let project_directory = project_parent_directory.path().join("arm-control");
    ProjectRepository::create(
        &project_directory,
        "arm-control",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let listed = crate::mcp::tests::first_text_block_json(
        &served
            .mcp_tool_result(
                "list_assets",
                json!({ "project_directory": project_directory }),
            )
            .await,
    );
    let project = listed["project"].clone();

    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_changes",
                          "params": { "projects": [project] } }),
        )
        .await;
    let subscribed = subscriber.next_message().await;
    assert_eq!(
        subscribed["result"]["projects"],
        json!([project]),
        "{subscribed}"
    );

    let source = project_parent_directory.path().join("intro.mp4");
    std::fs::write(&source, b"video").unwrap();
    let imported = served
        .mcp_tool_result(
            "import_asset",
            json!({ "project_directory": project_directory, "source": source,
                        "asset_path": "media/intro.mp4",
                        "author": { "kind": "person", "name": "Dana" } }),
        )
        .await;
    assert_eq!(imported["isError"], false, "{imported}");

    let notification = subscriber.next_message_but_files_changed().await;
    assert_eq!(notification["method"], "project_changed", "{notification}");
    assert!(notification.get("id").is_none(), "{notification}");
    assert_eq!(notification["params"]["project"], project);
    assert_eq!(notification["params"]["change"], "applied");
    assert_eq!(notification["params"]["transaction"]["author_name"], "Dana");
}

/// Two spellings of one project's directory are each answered and kept as spelled, and a
/// directory that names no folder on this machine is refused rather than kept unresolved.
///
/// Mental-revert: keying the spellings by path equality answers `/project/` as `/project`;
/// keeping an unresolved directory answers the missing folder and never matches its log.
#[tokio::test]
async fn a_project_log_subscription_keeps_each_spelling_and_refuses_a_missing_folder() {
    let project_directory = tempfile::tempdir().unwrap();
    let spelled_plain = project_directory.path().display().to_string();
    let spelled_with_a_trailing_separator = format!("{spelled_plain}/");
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let mut subscriber = served.control_websocket().await;

    subscriber
            .send(
                json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_log_records",
                          "params": { "project_directories": [spelled_plain, spelled_with_a_trailing_separator] } }),
            )
            .await;
    let subscribed = subscriber.next_message().await;
    let mut answered: Vec<String> =
        serde_json::from_value(subscribed["result"]["project_directories"].clone())
            .unwrap_or_else(|e| panic!("{e}: {subscribed}"));
    answered.sort();
    assert_eq!(
        answered,
        vec![spelled_plain.clone(), spelled_with_a_trailing_separator]
    );

    subscriber
            .send(
                json!({ "jsonrpc": "2.0", "id": 2, "method": "subscribe_project_log_records",
                          "params": { "project_directories": [format!("{spelled_plain}/not-yet-made")] } }),
            )
            .await;
    let refused = subscriber.next_message().await;
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("is not a folder on this machine")),
        "{refused}"
    );
}

/// A node file an editor saves in a subscribed project's folder, outside any runtime call, is
/// sent as `project_changed` with `files_changed` naming it.
///
/// Mental-revert: without the project's folder watched, or without the feed's announcement
/// mapped to the wire, no `files_changed` arrives and the wait times out.
#[tokio::test]
async fn a_websocket_subscriber_is_sent_a_file_an_editor_saved_in_the_project() {
    let project_parent_directory = tempfile::tempdir().unwrap();
    let project_directory = project_parent_directory.path().join("arm-control");
    ProjectRepository::create(
        &project_directory,
        "arm-control",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    std::fs::create_dir_all(project_directory.join("nodes")).unwrap();
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let listed = crate::mcp::tests::first_text_block_json(
        &served
            .mcp_tool_result(
                "list_assets",
                json!({ "project_directory": project_directory }),
            )
            .await,
    );
    let project = listed["project"].clone();
    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_changes",
                          "params": { "projects": [project] } }),
        )
        .await;
    assert_eq!(
        subscriber.next_message().await["result"]["projects"],
        json!([project])
    );

    std::fs::write(project_directory.join("nodes/blur.py"), b"x = 1").unwrap();

    let files_changed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let notification = subscriber.next_message().await;
            if notification["params"]["change"] == "files_changed" {
                return notification;
            }
        }
    })
    .await
    .expect("the save is sent as files_changed");
    assert_eq!(
        files_changed["method"], "project_changed",
        "{files_changed}"
    );
    assert_eq!(files_changed["params"]["project"], project);
    assert!(
        files_changed["params"]["files"]
            .as_array()
            .is_some_and(|files| files.contains(&json!("nodes/blur.py"))),
        "{files_changed}"
    );
}

#[tokio::test]
async fn the_typescript_client_is_served_and_hears_a_change_another_client_made() {
    let project_parent_directory = tempfile::tempdir().unwrap();
    let project_directory = project_parent_directory.path().join("arm-control");
    ProjectRepository::create(
        &project_directory,
        "arm-control",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let listed = crate::mcp::tests::first_text_block_json(
        &served
            .mcp_tool_result(
                "list_assets",
                json!({ "project_directory": project_directory }),
            )
            .await,
    );
    let project = listed["project"].as_str().unwrap().to_owned();

    let fixture_script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../sdk/tatolab-runtime-client/integration_fixtures/subscribe_and_print_one_change.mjs",
    );
    let mut typescript_client = tokio::process::Command::new("node")
        .arg(&fixture_script)
        .arg(&served.served.local_api_socket_path)
        .arg(&project)
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("`node` runs the TypeScript client's integration fixture");
    let mut steps_the_client_printed =
        BufReader::new(typescript_client.stdout.take().unwrap()).lines();
    let mut next_step = async || -> Value {
        let line = tokio::time::timeout(
            Duration::from_secs(20),
            steps_the_client_printed.next_line(),
        )
        .await
        .expect("the TypeScript client prints its next step")
        .unwrap()
        .expect("the TypeScript client prints a step before it exits");
        serde_json::from_str(&line).unwrap()
    };

    let connected = next_step().await;
    assert_eq!(
        connected["discovered"]["contract_version"],
        CONTROL_CONTRACT_VERSION
    );
    assert_eq!(
        connected["listed_streams"],
        crate::mcp::tests::first_text_block_json(
            &served.mcp_tool_result("list_streams", json!({})).await
        )
    );
    assert_eq!(
        connected["listed_streams_over_a_post"],
        connected["listed_streams"]
    );
    assert_eq!(
        connected["empty_subscription_refusal"]["is_a_control_operation_error"], true,
        "{connected}"
    );
    assert_eq!(
        connected["empty_subscription_refusal"]["method"],
        "subscribe_project_changes"
    );
    assert_eq!(connected["subscribed"]["projects"], json!([project]));

    let source = project_parent_directory.path().join("intro.mp4");
    std::fs::write(&source, b"video").unwrap();
    let imported = served
        .mcp_tool_result(
            "import_asset",
            json!({ "project_directory": project_directory, "source": source,
                        "asset_path": "media/intro.mp4",
                        "author": { "kind": "person", "name": "Dana" } }),
        )
        .await;
    assert_eq!(imported["isError"], false, "{imported}");

    let heard = next_step().await;
    assert_eq!(heard["heard"]["project"], project.as_str());
    assert_eq!(heard["heard"]["change"], "applied");
    assert_eq!(heard["heard"]["transaction"]["author_name"], "Dana");
    assert_eq!(next_step().await, json!({ "closed": true }));
    assert!(typescript_client.wait().await.unwrap().success());
}
