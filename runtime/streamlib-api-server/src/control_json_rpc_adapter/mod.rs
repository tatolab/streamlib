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
    ControlOperationCallParseRefusal, PROJECT_LOG_RECORDS_PER_NOTIFICATION,
    ProjectChangeKindOnTheWire, ProjectChanged, ProjectChangedNotificationParams,
    ProjectLogRecordsWritten, ProjectLogRecordsWrittenNotificationParams,
    StreamActionChangeKindOnTheWire, StreamActionChanged, StreamActionChangedNotificationParams,
    StreamGraphChangeKindOnTheWire, StreamGraphChanged, StreamGraphChangedNotificationParams,
};
use streamlib_project_repository::{
    ProjectChangeFeedAnnouncement, ProjectChangeOnTheFeed, ProjectRepositoriesOpenInThisRuntime,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_CLIENT_NAME_HEADER_NAME, CONTROL_CONTRACT_VERSION_HEADER_NAME,
    NodeTypeOfOneProjectSource,
};
use streamlib_runtime_client_contract::project_log_records::{
    ProjectLogRecordsPage, project_log_records_after,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::control_operation_dispatcher::{
    ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal,
    ControlRequestOrigin, refuse_another_contract_version,
};
use crate::control_subscriptions_of_one_connection::{
    ControlSubscriptionsOfOneConnection, ProjectLogNextPush,
};
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
    tokio::spawn(send_project_log_writes_until_the_connection_ends(
        streamlib::sdk::logging::listen_for_each_project_log_written(),
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
                        files: None,
                        node_types: None,
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

/// Send the new records of each subscribed project's log as `project_log_records_written`
/// notifications, read on from the connection's cursor each time the log is written, until the
/// connection ends. A connection that fell behind the writes reads every subscribed project's log
/// on from its cursor.
async fn send_project_log_writes_until_the_connection_ends(
    mut project_log_writes: tokio::sync::broadcast::Receiver<std::path::PathBuf>,
    subscriptions: Arc<ControlSubscriptionsOfOneConnection>,
    outgoing_message_sender: mpsc::Sender<Value>,
    connection_ended: CancellationToken,
) {
    loop {
        let received = tokio::select! {
            () = connection_ended.cancelled() => return,
            received = project_log_writes.recv() => received,
        };
        let project_logs_written_with_symlinks_resolved = match received {
            Ok(project_directory_with_symlinks_resolved) => {
                vec![project_directory_with_symlinks_resolved]
            }
            Err(RecvError::Lagged(missed_project_log_write_count)) => {
                tracing::debug!(
                    missed_project_log_write_count,
                    "a control WebSocket fell behind the project log writes; every subscribed project's log is read on from its cursor"
                );
                subscriptions.every_subscribed_project_log_with_symlinks_resolved()
            }
            Err(RecvError::Closed) => return,
        };
        for project_directory_with_symlinks_resolved in project_logs_written_with_symlinks_resolved
        {
            if send_the_new_records_of_a_project_log(
                &project_directory_with_symlinks_resolved,
                &subscriptions,
                &outgoing_message_sender,
            )
            .await
            .is_err()
            {
                return;
            }
        }
    }
}

/// Send the records of the log of the project in `project_directory_with_symlinks_resolved` after
/// the connection's cursor, at most [`PROJECT_LOG_RECORDS_PER_NOTIFICATION`] to a notification,
/// once for each directory the connection subscribed to it as; nothing when it did not subscribe
/// to it. An error once the connection has ended.
async fn send_the_new_records_of_a_project_log(
    project_directory_with_symlinks_resolved: &std::path::Path,
    subscriptions: &ControlSubscriptionsOfOneConnection,
    outgoing_message_sender: &mpsc::Sender<Value>,
) -> Result<(), ()> {
    loop {
        let Some(ProjectLogNextPush {
            directories_as_subscribed,
            cursor_the_next_push_follows,
        }) = subscriptions
            .the_next_push_of_the_project_log(project_directory_with_symlinks_resolved)
        else {
            return Ok(());
        };
        let read_project_directory = project_directory_with_symlinks_resolved.to_path_buf();
        let read_after = cursor_the_next_push_follows.clone();
        let page = tokio::task::spawn_blocking(move || {
            project_log_records_after(
                &read_project_directory,
                None,
                None,
                Some(&read_after),
                PROJECT_LOG_RECORDS_PER_NOTIFICATION,
            )
        })
        .await;
        let ProjectLogRecordsPage {
            records,
            next_after,
            records_no_longer_held,
        } = match page {
            Ok(Ok(page)) => page,
            Ok(Err(not_read)) => {
                tracing::warn!(
                    %not_read,
                    project_directory = %project_directory_with_symlinks_resolved.display(),
                    "a control WebSocket could not read a subscribed project's new log records"
                );
                return Ok(());
            }
            Err(not_finished) => {
                tracing::warn!(
                    %not_finished,
                    project_directory = %project_directory_with_symlinks_resolved.display(),
                    "a control WebSocket's read of a subscribed project's new log records did not finish"
                );
                return Ok(());
            }
        };
        let the_page_was_full = records.len() >= PROJECT_LOG_RECORDS_PER_NOTIFICATION;
        subscriptions.move_the_project_log_cursor_past_a_push(
            project_directory_with_symlinks_resolved,
            &cursor_the_next_push_follows,
            next_after.clone(),
        );
        if records.is_empty() {
            return Ok(());
        }
        for project_directory in directories_as_subscribed {
            send_a_notification::<ProjectLogRecordsWritten>(
                outgoing_message_sender,
                ProjectLogRecordsWrittenNotificationParams {
                    project_directory,
                    records: records.clone(),
                    after: cursor_the_next_push_follows.clone(),
                    next_after: next_after.clone(),
                    records_no_longer_held,
                },
            )
            .await?;
        }
        if !the_page_was_full {
            return Ok(());
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
/// now, `None` for one whose graph did not export.
async fn the_drops_raises_and_link_states_of_each_followed_stream(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    subscriptions: &ControlSubscriptionsOfOneConnection,
) -> BTreeMap<String, Option<Value>> {
    let mut sampled_by_stream = BTreeMap::new();
    for stream in subscriptions.every_followed_stream_graph_loaded_now() {
        let Ok(runtime_operations) =
            operations_on_the_loaded_streams.runtime_operations_of_the_stream_a_call_names(&stream)
        else {
            continue;
        };
        let sampled = match runtime_operations.to_json_async().await {
            Ok(stream_graph) => Some(the_drops_raises_and_link_states_of_a_stream_graph(
                &stream_graph,
            )),
            Err(not_exported) => {
                tracing::debug!(
                    %not_exported,
                    stream,
                    "a followed stream's graph did not export for its drops, raises and link states"
                );
                None
            }
        };
        sampled_by_stream.insert(stream, sampled);
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
            files: None,
            node_types: None,
        },
        ProjectChangeOnTheFeed::Refused => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::Refused,
            transaction: None,
            files: None,
            node_types: None,
        },
        ProjectChangeOnTheFeed::MediaFactsRead => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::MediaFactsRead,
            transaction: None,
            files: None,
            node_types: None,
        },
        ProjectChangeOnTheFeed::FilesChanged(files) => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::FilesChanged,
            transaction: None,
            files: Some(files),
            node_types: None,
        },
        ProjectChangeOnTheFeed::VenvMadeCurrent => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::VenvMadeCurrent,
            transaction: None,
            files: None,
            node_types: None,
        },
        ProjectChangeOnTheFeed::NodeTypesAdded(node_types) => the_notification_naming_node_types(
            project,
            ProjectChangeKindOnTheWire::NodeTypesAdded,
            node_types,
        ),
        ProjectChangeOnTheFeed::NodeTypesRemoved(node_types) => the_notification_naming_node_types(
            project,
            ProjectChangeKindOnTheWire::NodeTypesRemoved,
            node_types,
        ),
        ProjectChangeOnTheFeed::NodeTypesUpdated(node_types) => the_notification_naming_node_types(
            project,
            ProjectChangeKindOnTheWire::NodeTypesUpdated,
            node_types,
        ),
        ProjectChangeOnTheFeed::FilesChangedUnnamed => ProjectChangedNotificationParams {
            project,
            change: ProjectChangeKindOnTheWire::Missed,
            transaction: None,
            files: None,
            node_types: None,
        },
    }
}

fn the_notification_naming_node_types(
    project: Uuid,
    change: ProjectChangeKindOnTheWire,
    node_types: Vec<NodeTypeOfOneProjectSource>,
) -> ProjectChangedNotificationParams {
    ProjectChangedNotificationParams {
        project,
        change,
        transaction: None,
        files: None,
        node_types: Some(node_types),
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
pub(crate) mod tests;
