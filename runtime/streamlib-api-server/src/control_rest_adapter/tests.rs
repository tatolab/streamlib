// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Each control operation over REST answers what it answers over `/control`: the same result, or
//! the same error in the same words, at the status the error's code gives.

use serde_json::{Value, json};
use streamlib_control_contract::{
    CONTROL_CONTRACT_REVISION, CONTROL_CONTRACT_VERSION,
    CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE, CONTROL_OPERATIONS,
    JSON_RPC_INVALID_REQUEST_ERROR_CODE, JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE,
    JSON_RPC_PARSE_ERROR_CODE, the_http_status_of_a_control_error,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_CONTRACT_REVISION_HEADER_NAME, CONTROL_CONTRACT_VERSION_HEADER_NAME,
    control_rest_operation_route_path,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient;

/// One REST call of `method_name` with `extra_headers` and `body`: its status and its body.
async fn rest_post_with_headers(
    served: &ControlServedWithAnMcpClient,
    method_name: &str,
    extra_headers: &str,
    body: &str,
) -> (u16, Value) {
    let mut connection = UnixStream::connect(&served.served.local_api_socket_path)
        .await
        .unwrap();
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}\r\n{body}",
        control_rest_operation_route_path(method_name),
        body.len()
    );
    connection.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    connection.read_to_string(&mut response).await.unwrap();
    let (head, response_body) = response.split_once("\r\n\r\n").unwrap();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or_else(|| panic!("a status line: {head}"));
    let response_body = serde_json::from_str(response_body)
        .unwrap_or_else(|not_json| panic!("{not_json}: {response_body}"));
    (status, response_body)
}

async fn rest_post(
    served: &ControlServedWithAnMcpClient,
    method_name: &str,
    body: &str,
) -> (u16, Value) {
    rest_post_with_headers(served, method_name, "", body).await
}

/// What a REST answer says, as `/control` says it: its result, or its error.
fn the_rest_answer((status, body): (u16, Value)) -> Result<Value, Value> {
    if status == 200 {
        return Ok(body);
    }
    let code = body["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("an error code: {body}"));
    assert_eq!(
        status,
        the_http_status_of_a_control_error(code),
        "the status of error {code}: {body}"
    );
    Err(body)
}

/// What a JSON-RPC answer says: its result, or its error.
fn the_json_rpc_answer(answer: &Value) -> Result<Value, Value> {
    match answer.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(answer["error"].clone()),
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
async fn each_operation_answers_over_rest_what_it_answers_over_control() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let stream_asset = crate::control_plane_stub_support::STUB_STREAM_ASSET;
    let stream_id = crate::control_plane_stub_support::STUB_STREAM_ID;
    let calls = [
        ("describe_runtime", json!({})),
        (
            "discover",
            json!({ "contract_version": CONTROL_CONTRACT_VERSION }),
        ),
        ("list_streams", json!({})),
        ("graph", json!({})),
        ("graph", json!({ "stream": stream_id })),
        ("graph", json!({ "stream": "elsewhere" })),
        ("list_streams", json!({ "unexpected": true })),
        (
            "add_node",
            json!({ "stream": stream_asset, "type": "nodes.fx:Fx", "mode": "live" }),
        ),
        (
            "transact",
            json!({
                "mode": "live",
                "operations": [{
                    "operation": "remove_node",
                    "stream": stream_asset,
                    "node": uuid::Uuid::nil(),
                }],
            }),
        ),
        (
            "write_graph_input",
            json!({ "stream": stream_id, "input": "speed", "value": 1.0 }),
        ),
        (
            "subscribe_project_changes",
            json!({ "projects": [uuid::Uuid::nil()] }),
        ),
        (
            "open_project",
            json!({ "project_directory": "relative/rig" }),
        ),
        (
            "list_assets",
            json!({ "project_directory": "/no/such/project" }),
        ),
        ("no_such_operation", json!({})),
    ];
    for (method, params) in calls {
        let over_rest = the_rest_answer(rest_post(&served, method, &params.to_string()).await);
        let over_control = the_json_rpc_answer(&served.control_call(method, params.clone()).await);
        assert_eq!(
            with_each_minted_uuid_masked(over_rest),
            with_each_minted_uuid_masked(over_control),
            "`{method}` with {params}"
        );
    }
}

#[tokio::test]
async fn every_registered_operation_is_served_over_rest() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    for declaration in CONTROL_OPERATIONS {
        let (status, answer) = rest_post(&served, declaration.method_name, "{}").await;
        assert_ne!(
            status, 404,
            "`{}` is registered but not served over REST: {answer}",
            declaration.method_name
        );
        drop(the_rest_answer((status, answer)));
    }
}

#[tokio::test]
async fn an_operation_no_contract_names_is_refused_with_404_naming_it() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let (status, refused) = rest_post(&served, "no_such_operation", "{}").await;
    assert_eq!(status, 404, "{refused}");
    assert_eq!(
        refused["code"], JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE,
        "{refused}"
    );
    assert_eq!(
        refused["message"],
        "the runtime serves no operation named `no_such_operation`"
    );
    assert_eq!(
        refused["data"]["refusal_code"], "operation_not_found",
        "{refused}"
    );
}

#[tokio::test]
async fn an_empty_body_sends_no_params_and_a_body_that_is_not_json_is_refused() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    assert_eq!(
        rest_post(&served, "graph", "").await,
        rest_post(&served, "graph", "{}").await,
        "an empty body sends what no params do"
    );
    let (status, refused) = rest_post(&served, "graph", "{ not json").await;
    assert_eq!(
        (status, &refused["code"]),
        (400, &json!(JSON_RPC_PARSE_ERROR_CODE)),
        "{refused}"
    );
    let (_, over_control) = served.control_post_with_headers("", "{ not json").await;
    assert_eq!(
        refused, over_control["error"],
        "a body that is not JSON is the same error over REST and over `/control`"
    );
}

/// An operation the table serves only on a connection that stays open is refused over REST as
/// over `POST /control`, in the same words.
#[tokio::test]
async fn an_operation_that_needs_a_connection_is_refused_over_rest_as_over_control() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let stream_id = crate::control_plane_stub_support::STUB_STREAM_ID;
    let calls = [
        (
            "subscribe_project_changes",
            json!({ "projects": [uuid::Uuid::nil()] }),
        ),
        (
            "subscribe_project_log_records",
            json!({ "project_directories": ["/tmp"] }),
        ),
        (
            "subscribe_stream_graph_changes",
            json!({ "streams": [stream_id] }),
        ),
        (
            "subscribe_input_value_changes",
            json!({ "streams": [stream_id] }),
        ),
        ("subscribe_stream_action_changes", json!({})),
        (
            "open_viewer",
            json!({ "stream": stream_id, "node_id": "nobody", "port": "video" }),
        ),
        (
            "close_viewer",
            json!({ "viewer": "a viewer nobody opened" }),
        ),
    ];
    let mut needing_a_connection: Vec<&str> = CONTROL_OPERATIONS
        .iter()
        .filter(|declaration| declaration.needs_a_connection_that_stays_open)
        .map(|declaration| declaration.method_name)
        .collect();
    needing_a_connection.sort_unstable();
    let mut called: Vec<&str> = calls.iter().map(|(method, _)| *method).collect();
    called.sort_unstable();
    assert_eq!(called, needing_a_connection);
    for (method, params) in calls {
        let over_rest = the_rest_answer(rest_post(&served, method, &params.to_string()).await);
        let over_control = the_json_rpc_answer(&served.control_call(method, params).await);
        assert_eq!(over_rest, over_control, "`{method}`");
        let refused = over_rest.expect_err("refused over a request answered once");
        assert!(
            refused["message"]
                .as_str()
                .is_some_and(|words| words.contains("stays open")),
            "`{method}`: {refused}"
        );
    }
}

#[tokio::test]
async fn a_client_of_another_contract_is_refused_over_rest_as_over_control() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let headers = format!(
        "{CONTROL_CONTRACT_VERSION_HEADER_NAME}: {CONTROL_CONTRACT_VERSION}\r\n\
         {CONTROL_CONTRACT_REVISION_HEADER_NAME}: {}\r\n",
        CONTROL_CONTRACT_REVISION + 1
    );
    let (status, refused) = rest_post_with_headers(&served, "list_streams", &headers, "{}").await;
    let (_, over_control) = served
        .control_post_with_headers(
            &headers,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "list_streams" }).to_string(),
        )
        .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        refused["code"],
        CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE
    );
    assert_eq!(refused, over_control["error"]);

    let (status, refused) = rest_post_with_headers(
        &served,
        "list_streams",
        &format!("{CONTROL_CONTRACT_REVISION_HEADER_NAME}: one\r\n"),
        "{}",
    )
    .await;
    assert_eq!(
        (status, &refused["code"]),
        (400, &json!(JSON_RPC_INVALID_REQUEST_ERROR_CODE)),
        "{refused}"
    );
}
