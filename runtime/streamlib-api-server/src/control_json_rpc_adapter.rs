// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The control operations over JSON-RPC 2.0 on the local API socket: one request per `POST
//! /control`, or one message per text frame both ways on a WebSocket opened at `/control/ws`,
//! which also carries the notifications a subscription asks for. The adapter parses, calls the
//! dispatcher and renders; it holds no operation logic.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get, post};
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_control_contract::{
    CONTROL_CONTRACT_VERSION_MISMATCH_ERROR_CODE, CONTROL_OPERATION_FAILED_ERROR_CODE,
    CONTROL_OPERATION_REFUSED_ERROR_CODE, ControlNotification, ControlOperationCall,
    ControlOperationCallParseRefusal, ProjectChangeKindOnTheWire, ProjectChanged,
    ProjectChangedNotificationParams, StreamActionChangeKindOnTheWire, StreamActionChanged,
    StreamActionChangedNotificationParams, StreamGraphChangeKindOnTheWire, StreamGraphChanged,
    StreamGraphChangedNotificationParams,
};
use streamlib_project_repository::{
    ProjectChangeFeedAnnouncement, ProjectChangeOnTheFeed, ProjectRepositoriesOpenInThisRuntime,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_CLIENT_NAME_HEADER_NAME, CONTROL_CONTRACT_VERSION_HEADER_NAME,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::control_operation_dispatcher::{
    ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal,
    ControlRequestOrigin, refuse_another_contract_version,
};
use crate::control_subscriptions_of_one_connection::ControlSubscriptionsOfOneConnection;
use crate::mcp_project_change_feed::project_transaction_on_the_wire;
use crate::stream_graph_drops_raises_and_link_states::{
    STREAM_GRAPH_DROPS_RAISES_AND_LINK_STATES_SAMPLED_EVERY,
    StreamGraphDropsRaisesAndLinkStatesLastSampled,
    the_drops_raises_and_link_states_of_a_stream_graph,
};

const JSON_RPC_VERSION: &str = "2.0";
const JSON_RPC_PARSE_ERROR_CODE: i64 = -32700;
const JSON_RPC_INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const JSON_RPC_METHOD_NOT_FOUND_ERROR_CODE: i64 = -32601;
const JSON_RPC_INVALID_PARAMS_ERROR_CODE: i64 = -32602;

/// How many answers and notifications one WebSocket may have waiting to be written.
const CONTROL_WEBSOCKET_OUTGOING_MESSAGE_CAPACITY: usize = 256;

/// How many stream graph changes heard for one WebSocket may wait to become notifications; past
/// it, its `PUBSUB` listeners wait, and their queues lose events.
const CONTROL_WEBSOCKET_STREAM_GRAPH_CHANGES_HEARD_CAPACITY: usize = 64;

/// `POST /control`'s route: each request body is one JSON-RPC message, answered in the response.
pub(crate) fn control_json_rpc_route<S>(dispatcher: ControlOperationDispatcher) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    post(move |request_headers: HeaderMap, request_body: Bytes| {
        answer_one_control_json_rpc_post(dispatcher.clone(), request_headers, request_body)
    })
}

/// `/control/ws`'s route: each WebSocket is served until it closes or `local_api_stopping_token`
/// is cancelled.
pub(crate) fn control_json_rpc_websocket_route<S>(
    dispatcher: ControlOperationDispatcher,
    project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    local_api_stopping_token: CancellationToken,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(
        move |websocket_upgrade: WebSocketUpgrade, request_headers: HeaderMap| {
            answer_the_control_json_rpc_websocket_upgrade(
                dispatcher.clone(),
                Arc::clone(&project_repositories),
                local_api_stopping_token.clone(),
                websocket_upgrade,
                request_headers,
            )
        },
    )
}

async fn answer_one_control_json_rpc_post(
    dispatcher: ControlOperationDispatcher,
    request_headers: HeaderMap,
    request_body: Bytes,
) -> Response {
    let request_origin = ControlRequestOrigin {
        client_name: the_client_name_a_request_sent(&request_headers),
        subscriptions: None,
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

async fn answer_the_control_json_rpc_websocket_upgrade(
    dispatcher: ControlOperationDispatcher,
    project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    local_api_stopping_token: CancellationToken,
    websocket_upgrade: WebSocketUpgrade,
    request_headers: HeaderMap,
) -> Response {
    match the_contract_version_a_request_sent(&request_headers)
        .map_err(|why| (StatusCode::BAD_REQUEST, why))
        .and_then(|client_contract_version| {
            refuse_another_contract_version(client_contract_version)
                .map_err(|refusal| (StatusCode::CONFLICT, refusal.to_string()))
        }) {
        Ok(()) => {}
        Err(refused) => return refused.into_response(),
    }
    let (stream_graph_changes_to_the_connection, stream_graph_changes_heard) =
        mpsc::channel(CONTROL_WEBSOCKET_STREAM_GRAPH_CHANGES_HEARD_CAPACITY);
    let request_origin = ControlRequestOrigin {
        client_name: the_client_name_a_request_sent(&request_headers),
        subscriptions: Some(Arc::new(ControlSubscriptionsOfOneConnection::new(
            stream_graph_changes_to_the_connection,
        ))),
        viewers_held_open: Some(Arc::default()),
    };
    websocket_upgrade.on_upgrade(move |websocket| {
        serve_control_json_rpc_over_a_websocket(
            dispatcher,
            project_repositories,
            request_origin,
            stream_graph_changes_heard,
            websocket,
            local_api_stopping_token,
        )
    })
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

/// Serve one WebSocket: answer each request as it completes, send each change of a subscribed
/// project, stream graph and stream action, and end once the client closes and every answer is
/// written, or the local API stops.
async fn serve_control_json_rpc_over_a_websocket(
    dispatcher: ControlOperationDispatcher,
    project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    request_origin: ControlRequestOrigin,
    stream_graph_changes_heard: mpsc::Receiver<StreamGraphChangedNotificationParams>,
    websocket: WebSocket,
    local_api_stopping_token: CancellationToken,
) {
    let Some(subscriptions) = request_origin.subscriptions.clone() else {
        tracing::error!("a control WebSocket was served with no subscriptions of its own");
        return;
    };
    let (mut messages_to_the_client, messages_from_the_client) = websocket.split();
    let (outgoing_message_sender, mut outgoing_messages) =
        mpsc::channel::<Value>(CONTROL_WEBSOCKET_OUTGOING_MESSAGE_CAPACITY);
    let connection_ended = CancellationToken::new();
    tokio::spawn(send_project_changes_until_the_connection_ends(
        project_repositories.subscribe_to_the_change_feed(),
        Arc::clone(&subscriptions),
        outgoing_message_sender.clone(),
        connection_ended.clone(),
    ));
    tokio::spawn(
        send_stream_graph_and_action_changes_until_the_connection_ends(
            Arc::clone(&dispatcher.operations_on_the_loaded_streams),
            subscriptions,
            stream_graph_changes_heard,
            outgoing_message_sender.clone(),
            connection_ended.clone(),
        ),
    );
    let reading_requests = tokio::spawn(read_and_answer_requests_until_the_client_closes(
        dispatcher,
        request_origin,
        messages_from_the_client,
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
        if let Err(not_written) = messages_to_the_client
            .send(Message::Text(outgoing_message.to_string().into()))
            .await
        {
            tracing::debug!(%not_written, "a control WebSocket closed before an answer was written");
            break;
        }
    }
    connection_ended.cancel();
    reading_requests.abort();
    if let Err(not_closed) = messages_to_the_client.close().await {
        tracing::debug!(%not_closed, "a control WebSocket did not close cleanly");
    }
}

/// Read requests, one per text frame, until the client closes, answering each on its own task,
/// then cancel `requests_answered` once every answer is queued, which ends the change
/// notifications and, once the queue drains, the writer.
async fn read_and_answer_requests_until_the_client_closes(
    dispatcher: ControlOperationDispatcher,
    request_origin: ControlRequestOrigin,
    mut messages_from_the_client: SplitStream<WebSocket>,
    outgoing_message_sender: mpsc::Sender<Value>,
    local_api_stopping_token: CancellationToken,
    requests_answered: CancellationToken,
) {
    let request_origin = Arc::new(request_origin);
    let mut answers_in_flight = tokio::task::JoinSet::new();
    loop {
        let received = tokio::select! {
            () = local_api_stopping_token.cancelled() => break,
            received = messages_from_the_client.next() => received,
        };
        let parsed: Result<Value, String> = match received {
            None | Some(Ok(Message::Close(_))) => break,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Text(request_text))) => serde_json::from_str(request_text.as_str())
                .map_err(|not_json| format!("the message is not JSON: {not_json}")),
            Some(Ok(Message::Binary(_))) => Err(
                "a control message is one JSON-RPC message in a text frame, never a binary one"
                    .to_owned(),
            ),
            Some(Err(not_read)) => {
                tracing::debug!(%not_read, "a control WebSocket could not be read");
                break;
            }
        };
        let dispatcher = dispatcher.clone();
        let request_origin = Arc::clone(&request_origin);
        let outgoing_message_sender = outgoing_message_sender.clone();
        answers_in_flight.spawn(async move {
            let answer = match parsed {
                Ok(message) => {
                    the_answer_to_one_json_rpc_message(&dispatcher, &request_origin, message).await
                }
                Err(why_not_a_message) => Some(a_json_rpc_error(
                    Value::Null,
                    JSON_RPC_PARSE_ERROR_CODE,
                    why_not_a_message,
                    None,
                )),
            };
            if let Some(answer) = answer {
                drop(outgoing_message_sender.send(answer).await);
            }
        });
    }
    while answers_in_flight.join_next().await.is_some() {}
    requests_answered.cancel();
}

/// Send each change on a subscribed project's feed as a `project_changed` notification until the
/// connection ends.
async fn send_project_changes_until_the_connection_ends(
    mut change_feed: tokio::sync::broadcast::Receiver<ProjectChangeFeedAnnouncement>,
    subscriptions: Arc<ControlSubscriptionsOfOneConnection>,
    outgoing_message_sender: mpsc::Sender<Value>,
    connection_ended: CancellationToken,
) {
    loop {
        let received = tokio::select! {
            () = connection_ended.cancelled() => return,
            received = change_feed.recv() => received,
        };
        let notifications: Vec<ProjectChangedNotificationParams> = match received {
            Ok(announcement) if subscriptions.holds_the_project(announcement.project) => {
                vec![the_notification_of_an_announcement(announcement)]
            }
            Ok(_) => Vec::new(),
            Err(RecvError::Lagged(missed_announcement_count)) => {
                tracing::debug!(
                    missed_announcement_count,
                    "a control stream fell behind the change feed; every subscribed project is sent `missed`"
                );
                subscriptions
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
            if send_a_notification::<ProjectChanged>(&outgoing_message_sender, params)
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// Send `Notification` with `params` to the client; an error once the connection has ended.
async fn send_a_notification<Notification: ControlNotification>(
    outgoing_message_sender: &mpsc::Sender<Value>,
    params: Notification::Params,
) -> Result<(), ()> {
    outgoing_message_sender
        .send(json!({
            "jsonrpc": JSON_RPC_VERSION,
            "method": Notification::METHOD_NAME,
            "params": params,
        }))
        .await
        .map_err(drop)
}

/// Send each change heard in a followed stream's graph as a `stream_graph_changed`
/// notification, each followed stream whose drops, raises or link states changed since the last
/// sample as one too, and each stream action change as a `stream_action_changed` once the
/// connection subscribed to them, until the connection ends. Each stream action change also
/// moves each followed stream to the load it has now, sending `missed` for one whose load
/// changed.
async fn send_stream_graph_and_action_changes_until_the_connection_ends(
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    subscriptions: Arc<ControlSubscriptionsOfOneConnection>,
    mut stream_graph_changes_heard: mpsc::Receiver<StreamGraphChangedNotificationParams>,
    outgoing_message_sender: mpsc::Sender<Value>,
    connection_ended: CancellationToken,
) {
    let mut stream_action_changes =
        operations_on_the_loaded_streams.listen_for_each_stream_action_change();
    let mut drops_raises_and_link_states_sampling =
        tokio::time::interval(STREAM_GRAPH_DROPS_RAISES_AND_LINK_STATES_SAMPLED_EVERY);
    drops_raises_and_link_states_sampling
        .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut drops_raises_and_link_states_last_sampled =
        StreamGraphDropsRaisesAndLinkStatesLastSampled::default();
    loop {
        tokio::select! {
            () = connection_ended.cancelled() => return,
            _ = drops_raises_and_link_states_sampling.tick() => {
                let sampled_by_stream = the_drops_raises_and_link_states_of_each_followed_stream(
                    operations_on_the_loaded_streams.as_ref(),
                    &subscriptions,
                )
                .await;
                for stream in drops_raises_and_link_states_last_sampled
                    .streams_whose_sample_changed(sampled_by_stream)
                {
                    let changed = StreamGraphChangedNotificationParams {
                        stream,
                        change: StreamGraphChangeKindOnTheWire::DropsRaisesOrLinkStatesChanged,
                        node: None,
                        node_state: None,
                    };
                    if send_a_notification::<StreamGraphChanged>(&outgoing_message_sender, changed)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            stream_graph_change = stream_graph_changes_heard.recv() => {
                let Some(stream_graph_change) = stream_graph_change else {
                    return;
                };
                if send_a_notification::<StreamGraphChanged>(&outgoing_message_sender, stream_graph_change)
                    .await
                    .is_err()
                {
                    return;
                }
            }
            stream_action_change = stream_action_changes.recv() => {
                let stream_action_notification = match stream_action_change {
                    Ok(action) => StreamActionChangedNotificationParams {
                        change: StreamActionChangeKindOnTheWire::Changed,
                        action: Some(action),
                    },
                    Err(RecvError::Lagged(missed_stream_action_change_count)) => {
                        tracing::debug!(
                            missed_stream_action_change_count,
                            "a control WebSocket fell behind the stream actions; it is sent `missed`"
                        );
                        StreamActionChangedNotificationParams {
                            change: StreamActionChangeKindOnTheWire::Missed,
                            action: None,
                        }
                    }
                    Err(RecvError::Closed) => return,
                };
                if subscriptions.is_subscribed_to_stream_actions()
                    && send_a_notification::<StreamActionChanged>(
                        &outgoing_message_sender,
                        stream_action_notification,
                    )
                    .await
                    .is_err()
                {
                    return;
                }
                let operations_on_the_loaded_streams = Arc::clone(&operations_on_the_loaded_streams);
                let subscriptions = Arc::clone(&subscriptions);
                let Ok(streams_whose_load_changed) = tokio::task::spawn_blocking(move || {
                    subscriptions.follow_each_stream_graph_to_its_current_load(
                        operations_on_the_loaded_streams.as_ref(),
                    )
                })
                .await
                else {
                    return;
                };
                for stream in streams_whose_load_changed {
                    let missed = StreamGraphChangedNotificationParams {
                        stream,
                        change: StreamGraphChangeKindOnTheWire::Missed,
                        node: None,
                        node_state: None,
                    };
                    if send_a_notification::<StreamGraphChanged>(&outgoing_message_sender, missed)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
}

/// The drops, raises and link states of each stream graph the connection follows that is loaded
/// now; a stream whose graph does not export is left out until it does.
async fn the_drops_raises_and_link_states_of_each_followed_stream(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    subscriptions: &ControlSubscriptionsOfOneConnection,
) -> BTreeMap<String, Value> {
    let mut sampled_by_stream = BTreeMap::new();
    for stream in subscriptions.every_followed_stream_graph_loaded_now() {
        let Ok(runtime_operations) =
            operations_on_the_loaded_streams.runtime_operations_of_the_stream_a_call_names(&stream)
        else {
            continue;
        };
        match runtime_operations.to_json_async().await {
            Ok(stream_graph) => {
                sampled_by_stream.insert(
                    stream,
                    the_drops_raises_and_link_states_of_a_stream_graph(&stream_graph),
                );
            }
            Err(not_exported) => tracing::debug!(
                %not_exported,
                stream,
                "a followed stream's graph did not export for its drops, raises and link states"
            ),
        }
    }
    sampled_by_stream
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
