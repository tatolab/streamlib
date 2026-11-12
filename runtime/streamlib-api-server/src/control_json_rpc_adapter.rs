// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The control operations over JSON-RPC 2.0 on the local API socket: one request per `POST
//! /control`, or newline-delimited messages both ways on a connection upgraded at
//! `/control/stream`, which also carries the notifications a subscription asks for. The adapter
//! parses, calls the dispatcher and renders; it holds no operation logic.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::header::{CONNECTION, UPGRADE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use streamlib_control_contract::{
    CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE, CONTROL_OPERATION_FAILED_ERROR_CODE,
    CONTROL_OPERATION_REFUSED_ERROR_CODE, ControlNotification, ControlOperationCall,
    ControlOperationCallParseRefusal, ProjectChangeKindOnTheWire, ProjectChanged,
    ProjectChangedNotificationParams,
};
use streamlib_project_repository::{
    ProjectChangeFeedAnnouncement, ProjectChangeOnTheFeed, ProjectRepositoriesOpenInThisRuntime,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_CLIENT_NAME_HEADER_NAME, CONTROL_CONTRACT_VERSION_HEADER_NAME,
    CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN, CONTROL_JSON_RPC_STREAM_UPGRADE_REQUEST_TARGET,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::control_operation_dispatcher::{
    ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal,
    ControlRequestOrigin, ProjectChangeSubscriptionsOfOneConnection,
    refuse_another_contract_version,
};
use crate::mcp_project_change_feed::project_transaction_on_the_wire;
use crate::mcp_stdio_upgrade::header_lists_token;

const JSON_RPC_VERSION: &str = "2.0";
const JSON_RPC_PARSE_ERROR_CODE: i64 = -32700;
const JSON_RPC_INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE: i64 = -32601;
const JSON_RPC_INVALID_PARAMS_ERROR_CODE: i64 = -32602;

/// How many answers and notifications one stream connection may have waiting to be written.
const CONTROL_STREAM_OUTGOING_MESSAGE_CAPACITY: usize = 256;

/// `POST /control`'s route: each request body is one JSON-RPC message, answered in the response.
pub(crate) fn control_json_rpc_route<S>(dispatcher: ControlOperationDispatcher) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    post(move |request_headers: HeaderMap, request_body: Bytes| {
        answer_one_control_json_rpc_post(dispatcher.clone(), request_headers, request_body)
    })
}

/// `/control/stream`'s route: each upgraded connection is served until it closes or
/// `local_api_stopping_token` is cancelled.
pub(crate) fn control_json_rpc_stream_upgrade_route<S>(
    dispatcher: ControlOperationDispatcher,
    project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    local_api_stopping_token: CancellationToken,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(move |request: Request| {
        answer_the_control_json_rpc_stream_upgrade(
            dispatcher.clone(),
            Arc::clone(&project_repositories),
            local_api_stopping_token.clone(),
            request,
        )
    })
}

async fn answer_one_control_json_rpc_post(
    dispatcher: ControlOperationDispatcher,
    request_headers: HeaderMap,
    request_body: Bytes,
) -> Response {
    let request_origin = ControlRequestOrigin {
        client_name: the_client_name_a_request_sent(&request_headers),
        project_change_subscriptions: None,
        viewers_held_open: None,
    };
    let answer = match the_contract_version_a_request_sent(&request_headers) {
        Err(why) => Some(a_json_rpc_error(
            Value::Null,
            JSON_RPC_INVALID_REQUEST_ERROR_CODE,
            why,
            None,
        )),
        Ok(client_contract_version) => {
            match refuse_another_contract_version(client_contract_version) {
                Err(refusal) => Some(the_json_rpc_error_of_a_refusal(Value::Null, refusal)),
                Ok(()) => match serde_json::from_slice::<Value>(&request_body) {
                    Ok(message) => {
                        the_answer_to_one_json_rpc_message(&dispatcher, &request_origin, message)
                            .await
                    }
                    Err(not_json) => Some(a_json_rpc_error(
                        Value::Null,
                        JSON_RPC_PARSE_ERROR_CODE,
                        format!("the request body is not JSON: {not_json}"),
                        None,
                    )),
                },
            }
        }
    };
    match answer {
        Some(answer) => (StatusCode::OK, axum::Json(answer)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn answer_the_control_json_rpc_stream_upgrade(
    dispatcher: ControlOperationDispatcher,
    project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    local_api_stopping_token: CancellationToken,
    mut request: Request,
) -> Response {
    if !requests_the_control_json_rpc_stream_upgrade(request.headers()) {
        return (
            StatusCode::UPGRADE_REQUIRED,
            control_json_rpc_stream_upgrade_response_headers(),
            format!(
                "`{CONTROL_JSON_RPC_STREAM_UPGRADE_REQUEST_TARGET}` serves JSON-RPC only after \
                 `Connection: upgrade` and `Upgrade: {CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN}`"
            ),
        )
            .into_response();
    }
    match the_contract_version_a_request_sent(request.headers())
        .map_err(|why| (StatusCode::BAD_REQUEST, why))
        .and_then(|client_contract_version| {
            refuse_another_contract_version(client_contract_version)
                .map_err(|refusal| (StatusCode::CONFLICT, refusal.to_string()))
        }) {
        Ok(()) => {}
        Err(refused) => return refused.into_response(),
    }
    let request_origin = ControlRequestOrigin {
        client_name: the_client_name_a_request_sent(request.headers()),
        project_change_subscriptions: Some(Arc::default()),
        viewers_held_open: Some(Arc::default()),
    };
    let pending_upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        match pending_upgrade.await {
            Ok(upgraded) => {
                serve_control_json_rpc_over_the_upgraded_stream(
                    dispatcher,
                    project_repositories,
                    request_origin,
                    TokioIo::new(upgraded),
                    local_api_stopping_token,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(%error, "a `control-jsonrpc` upgrade was answered but never completed");
            }
        }
    });
    (
        StatusCode::SWITCHING_PROTOCOLS,
        control_json_rpc_stream_upgrade_response_headers(),
    )
        .into_response()
}

fn control_json_rpc_stream_upgrade_response_headers() -> [(HeaderName, HeaderValue); 2] {
    [
        (CONNECTION, HeaderValue::from_static("upgrade")),
        (
            UPGRADE,
            HeaderValue::from_static(CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN),
        ),
    ]
}

fn requests_the_control_json_rpc_stream_upgrade(request_headers: &HeaderMap) -> bool {
    header_lists_token(request_headers, CONNECTION, "upgrade")
        && header_lists_token(
            request_headers,
            UPGRADE,
            CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN,
        )
}

fn the_client_name_a_request_sent(request_headers: &HeaderMap) -> Option<String> {
    request_headers
        .get(CONTROL_CLIENT_NAME_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn the_contract_version_a_request_sent(request_headers: &HeaderMap) -> Result<Option<u32>, String> {
    let Some(value) = request_headers.get(CONTROL_CONTRACT_VERSION_HEADER_NAME) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map(Some)
        .ok_or_else(|| {
            format!("`{CONTROL_CONTRACT_VERSION_HEADER_NAME}` must be a contract version, a whole number")
        })
}

/// Serve one upgraded connection: answer each request line as it completes, send each change
/// of a subscribed project, and end once the client closes and every answer is written, or
/// the local API stops.
async fn serve_control_json_rpc_over_the_upgraded_stream(
    dispatcher: ControlOperationDispatcher,
    project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    request_origin: ControlRequestOrigin,
    upgraded_stream: TokioIo<hyper::upgrade::Upgraded>,
    local_api_stopping_token: CancellationToken,
) {
    let (read_half, mut write_half) = tokio::io::split(upgraded_stream);
    let (outgoing_message_sender, mut outgoing_messages) =
        mpsc::channel::<Value>(CONTROL_STREAM_OUTGOING_MESSAGE_CAPACITY);
    let connection_ended = CancellationToken::new();
    let project_change_subscriptions = request_origin
        .project_change_subscriptions
        .clone()
        .unwrap_or_default();
    tokio::spawn(send_project_changes_until_the_connection_ends(
        project_repositories.subscribe_to_the_change_feed(),
        project_change_subscriptions,
        outgoing_message_sender.clone(),
        connection_ended.clone(),
    ));
    let reading_requests = tokio::spawn(read_and_answer_requests_until_the_client_closes(
        dispatcher,
        request_origin,
        BufReader::new(read_half),
        outgoing_message_sender,
        local_api_stopping_token.clone(),
        connection_ended.clone(),
    ));
    loop {
        let outgoing_message = tokio::select! {
            () = local_api_stopping_token.cancelled() => break,
            outgoing_message = outgoing_messages.recv() => outgoing_message,
        };
        let Some(outgoing_message) = outgoing_message else {
            break;
        };
        let mut line = outgoing_message.to_string();
        line.push('\n');
        if let Err(not_written) = write_half.write_all(line.as_bytes()).await {
            tracing::debug!(%not_written, "a control stream closed before an answer was written");
            break;
        }
    }
    connection_ended.cancel();
    reading_requests.abort();
    if let Err(not_shut_down) = write_half.shutdown().await {
        tracing::debug!(%not_shut_down, "a control stream did not shut down cleanly");
    }
}

/// Read request lines until the client closes its side, answering each on its own task, then
/// cancel `requests_answered` once every answer is queued, which ends the change notifications
/// and, once the queue drains, the writer.
async fn read_and_answer_requests_until_the_client_closes(
    dispatcher: ControlOperationDispatcher,
    request_origin: ControlRequestOrigin,
    mut request_lines: BufReader<tokio::io::ReadHalf<TokioIo<hyper::upgrade::Upgraded>>>,
    outgoing_message_sender: mpsc::Sender<Value>,
    local_api_stopping_token: CancellationToken,
    requests_answered: CancellationToken,
) {
    let request_origin = Arc::new(request_origin);
    let mut answers_in_flight = tokio::task::JoinSet::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = tokio::select! {
            () = local_api_stopping_token.cancelled() => break,
            read = request_lines.read_line(&mut line) => read,
        };
        match read {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => continue,
            Ok(_) => {
                let parsed: Result<Value, _> = serde_json::from_str(line.trim());
                let dispatcher = dispatcher.clone();
                let request_origin = Arc::clone(&request_origin);
                let outgoing_message_sender = outgoing_message_sender.clone();
                answers_in_flight.spawn(async move {
                    let answer = match parsed {
                        Ok(message) => {
                            the_answer_to_one_json_rpc_message(
                                &dispatcher,
                                &request_origin,
                                message,
                            )
                            .await
                        }
                        Err(not_json) => Some(a_json_rpc_error(
                            Value::Null,
                            JSON_RPC_PARSE_ERROR_CODE,
                            format!("the line is not JSON: {not_json}"),
                            None,
                        )),
                    };
                    if let Some(answer) = answer {
                        drop(outgoing_message_sender.send(answer).await);
                    }
                });
            }
            Err(not_read) => {
                tracing::debug!(%not_read, "a control stream could not be read");
                break;
            }
        }
    }
    while answers_in_flight.join_next().await.is_some() {}
    requests_answered.cancel();
}

/// Send each change on a subscribed project's feed as a `project_changed` notification until the
/// connection ends.
async fn send_project_changes_until_the_connection_ends(
    mut change_feed: tokio::sync::broadcast::Receiver<ProjectChangeFeedAnnouncement>,
    project_change_subscriptions: Arc<ProjectChangeSubscriptionsOfOneConnection>,
    outgoing_message_sender: mpsc::Sender<Value>,
    connection_ended: CancellationToken,
) {
    loop {
        let received = tokio::select! {
            () = connection_ended.cancelled() => return,
            received = change_feed.recv() => received,
        };
        let notifications: Vec<ProjectChangedNotificationParams> = match received {
            Ok(announcement) if project_change_subscriptions.holds(announcement.project) => {
                vec![the_notification_of_an_announcement(announcement)]
            }
            Ok(_) => Vec::new(),
            Err(RecvError::Lagged(missed_announcement_count)) => {
                tracing::debug!(
                    missed_announcement_count,
                    "a control stream fell behind the change feed; every subscribed project is sent `missed`"
                );
                project_change_subscriptions
                    .every_subscribed_project()
                    .into_iter()
                    .map(|project| ProjectChangedNotificationParams {
                        project,
                        change: ProjectChangeKindOnTheWire::Missed,
                        transaction: None,
                    })
                    .collect()
            }
            Err(RecvError::Closed) => return,
        };
        for params in notifications {
            let notification = json!({
                "jsonrpc": JSON_RPC_VERSION,
                "method": ProjectChanged::METHOD_NAME,
                "params": params,
            });
            if outgoing_message_sender.send(notification).await.is_err() {
                return;
            }
        }
    }
}

fn the_notification_of_an_announcement(
    announcement: ProjectChangeFeedAnnouncement,
) -> ProjectChangedNotificationParams {
    let project: Uuid = announcement.project;
    match announcement.change {
        ProjectChangeOnTheFeed::Applied(applied) => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::Applied,
            transaction: Some(project_transaction_on_the_wire(
                *applied,
                &Default::default(),
            )),
        },
        ProjectChangeOnTheFeed::Refused => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::Refused,
            transaction: None,
        },
        ProjectChangeOnTheFeed::MediaFactsRead => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::MediaFactsRead,
            transaction: None,
        },
    }
}

/// The answer to one JSON-RPC message, or none for a notification.
async fn the_answer_to_one_json_rpc_message(
    dispatcher: &ControlOperationDispatcher,
    request_origin: &ControlRequestOrigin,
    message: Value,
) -> Option<Value> {
    let Value::Object(mut message) = message else {
        return Some(a_json_rpc_error(
            Value::Null,
            JSON_RPC_INVALID_REQUEST_ERROR_CODE,
            "a request is one JSON-RPC 2.0 object; batches are not served".to_owned(),
            None,
        ));
    };
    let request_id = message.remove("id");
    let id_to_answer = request_id.clone().unwrap_or(Value::Null);
    if message.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
        return Some(a_json_rpc_error(
            id_to_answer,
            JSON_RPC_INVALID_REQUEST_ERROR_CODE,
            "`jsonrpc` must be \"2.0\"".to_owned(),
            None,
        ));
    }
    let Some(Value::String(method_name)) = message.remove("method") else {
        return Some(a_json_rpc_error(
            id_to_answer,
            JSON_RPC_INVALID_REQUEST_ERROR_CODE,
            "`method` must be a string".to_owned(),
            None,
        ));
    };
    let answer = match ControlOperationCall::parse(&method_name, message.remove("params")) {
        Err(ControlOperationCallParseRefusal::NoSuchMethod { method_name }) => a_json_rpc_error(
            id_to_answer,
            JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE,
            ControlOperationCallParseRefusal::NoSuchMethod { method_name }.to_string(),
            None,
        ),
        Err(invalid_params) => a_json_rpc_error(
            id_to_answer,
            JSON_RPC_INVALID_PARAMS_ERROR_CODE,
            invalid_params.to_string(),
            None,
        ),
        Ok(call) => match dispatcher.dispatch(call, request_origin).await {
            Ok(answered) => json!({
                "jsonrpc": JSON_RPC_VERSION,
                "id": id_to_answer,
                "result": the_result_of_an_answer(answered),
            }),
            Err(refusal) => the_json_rpc_error_of_a_refusal(id_to_answer, refusal),
        },
    };
    request_id.map(|_| answer)
}

fn the_result_of_an_answer(answered: ControlOperationAnswer) -> Value {
    serde_json::to_value(answered)
        .unwrap_or_else(|_| unreachable!("a result type always serializes to JSON"))
}

fn the_json_rpc_error_of_a_refusal(request_id: Value, refusal: ControlOperationRefusal) -> Value {
    let runtime_words = refusal.to_string();
    match refusal {
        ControlOperationRefusal::RefusedAction(refused) => a_json_rpc_error(
            request_id,
            CONTROL_OPERATION_REFUSED_ERROR_CODE,
            runtime_words,
            serde_json::to_value(*refused).ok(),
        ),
        ControlOperationRefusal::Failed(_) => a_json_rpc_error(
            request_id,
            CONTROL_OPERATION_FAILED_ERROR_CODE,
            runtime_words,
            None,
        ),
        ControlOperationRefusal::ContractVersionMismatch(mismatch) => a_json_rpc_error(
            request_id,
            CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE,
            runtime_words,
            serde_json::to_value(mismatch).ok(),
        ),
    }
}

fn a_json_rpc_error(request_id: Value, code: i64, message: String, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": JSON_RPC_VERSION, "id": request_id, "error": error })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rmcp::RoleClient;
    use rmcp::model::ProtocolVersion;
    use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
    use serde_json::{Value, json};
    use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
    use streamlib_control_contract::CONTROL_CONTRACT_VERSION;
    use streamlib_project_repository::{ProjectRepository, ProjectTransactionAuthor};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    use crate::control_plane_stub_support::{
        LocalApiServedOnAFreshSocket, response_head_over_the_socket, the_stub_runtimes_description,
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
        async fn control_post_with_headers(
            &self,
            extra_headers: &str,
            body: &str,
        ) -> (String, Value) {
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
                serde_json::from_str(response_body)
                    .unwrap_or_else(|e| panic!("{e}: {response_body}"))
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

        pub(crate) async fn control_stream(&self, extra_headers: &str) -> ControlStreamLines {
            let mut stream = UnixStream::connect(&self.served.local_api_socket_path)
                .await
                .unwrap();
            let head = response_head_over_the_socket(
                &mut stream,
                &format!(
                    "GET /control/stream HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                     Upgrade: control-jsonrpc\r\n{extra_headers}\r\n"
                ),
            )
            .await;
            assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
            let (read_half, write_half) = stream.into_split();
            ControlStreamLines {
                from_the_runtime: BufReader::new(read_half),
                to_the_runtime: write_half,
            }
        }
    }

    /// An upgraded `/control/stream` connection, written and read as raw lines.
    pub(crate) struct ControlStreamLines {
        from_the_runtime: BufReader<OwnedReadHalf>,
        to_the_runtime: OwnedWriteHalf,
    }

    impl ControlStreamLines {
        pub(crate) async fn send(&mut self, message: Value) {
            let mut line = serde_json::to_vec(&message).unwrap();
            line.push(b'\n');
            self.to_the_runtime.write_all(&line).await.unwrap();
        }

        pub(crate) async fn next_message(&mut self) -> Value {
            let mut line = String::new();
            tokio::time::timeout(
                Duration::from_secs(10),
                self.from_the_runtime.read_line(&mut line),
            )
            .await
            .expect("the runtime writes a message")
            .unwrap();
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
        }

        async fn reaches_end_of_stream_within(&mut self, wait: Duration) -> bool {
            let mut rest = Vec::new();
            matches!(
                tokio::time::timeout(wait, self.from_the_runtime.read_to_end(&mut rest)).await,
                Ok(Ok(_))
            )
        }

        async fn close_the_write_side(&mut self) {
            self.to_the_runtime.shutdown().await.unwrap();
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

        let mut connection = UnixStream::connect(&served.served.local_api_socket_path)
            .await
            .unwrap();
        let head = response_head_over_the_socket(
            &mut connection,
            &format!(
                "GET /control/stream HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                 Upgrade: control-jsonrpc\r\n{header}\r\n"
            ),
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 409 "), "{head}");
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
    async fn a_subscription_over_a_one_shot_post_is_refused_naming_the_stream_route() {
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
                .contains("/control/stream"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn the_stream_answers_each_request_by_its_id_and_closes_after_the_last_answer() {
        let served = ControlServedWithAnMcpClient::over_the_stub().await;
        let mut stream = served.control_stream("").await;
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
        stream.close_the_write_side().await;
        assert!(
            stream
                .reaches_end_of_stream_within(Duration::from_secs(5))
                .await
        );
    }

    #[tokio::test]
    async fn a_stream_subscriber_is_sent_a_change_another_client_made() {
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

        let mut subscriber = served.control_stream("").await;
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

        let notification = subscriber.next_message().await;
        assert_eq!(notification["method"], "project_changed", "{notification}");
        assert!(notification.get("id").is_none(), "{notification}");
        assert_eq!(notification["params"]["project"], project);
        assert_eq!(notification["params"]["change"], "applied");
        assert_eq!(notification["params"]["transaction"]["author_name"], "Dana");
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
}
