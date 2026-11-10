// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The stream actions as resources a client follows live: `streamlib://stream-actions` holds
//! every action queued or under way, and `streamlib://stream-action/<action>` one action as
//! `describe_stream_action` answers it; a `subscriptions/listen` naming either is sent
//! `notifications/resources/updated` each time an action it covers is queued or changes —
//! its state, its step, its streams or its venv sync.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{
    Notification, ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
    ResourceUpdatedNotificationParam, ServerNotification, SubscriptionFilter,
};
use rmcp::service::SubscriptionContext;
use serde_json::json;
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_runtime_client_contract::local_api_wire_contract::StreamActionsUnderWayResourceContents;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::mcp::describe_stream_action_tool_result_of;

/// Every stream action queued or under way.
pub(crate) const STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI: &str = "streamlib://stream-actions";

const STREAM_ACTION_RESOURCE_URI_PREFIX: &str = "streamlib://stream-action/";

const JSON_RESOURCE_MIME_TYPE: &str = "application/json";

/// The resource of the stream action `action_id`.
pub(crate) fn stream_action_resource_uri(action_id: &str) -> String {
    format!("{STREAM_ACTION_RESOURCE_URI_PREFIX}{action_id}")
}

/// The action a stream action resource URI names, or `None` for any other URI.
pub(crate) fn the_action_a_stream_action_resource_uri_names(uri: &str) -> Option<&str> {
    uri.strip_prefix(STREAM_ACTION_RESOURCE_URI_PREFIX)
        .filter(|action_id| !action_id.is_empty() && !action_id.contains('/'))
}

/// Whether `uri` is a stream action resource a listen may name.
fn is_a_stream_action_resource_uri(uri: &str) -> bool {
    uri == STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI
        || the_action_a_stream_action_resource_uri_names(uri).is_some()
}

/// The `resources/list` entry of the actions under way.
pub(crate) fn stream_actions_under_way_resource() -> Resource {
    Resource::new(STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI, "stream-actions")
        .with_title("Stream actions under way")
        .with_description("Every stream action queued or running on this runtime, whoever queued it — `start_project`, `reload_project`, `create_project`, `stop_project`, `remove_project`, `delete_project` or the runtime at its start — the earliest queued first, under `actions`, each as `describe_stream_action` answers it: while its `step` is `making_the_venv`, its `venv_sync` follows uv's sync of the project's `.venv` package by package. An action leaves the list as it finishes; `streamlib://stream-action/<action>` and `describe_stream_action` answer how it ended. Name the URI in `subscriptions/listen`'s resource subscriptions to be sent `notifications/resources/updated` for it each time an action is queued or changes, then read it.")
        .with_mime_type(JSON_RESOURCE_MIME_TYPE)
}

/// The `resources/templates/list` entry every stream action is read through.
pub(crate) fn stream_action_resource_template() -> ResourceTemplate {
    ResourceTemplate::new(
        format!("{STREAM_ACTION_RESOURCE_URI_PREFIX}{{action}}"),
        "stream-action",
    )
    .with_title("Stream action")
    .with_description("One stream action by the `action` id the call that queued it answered, as `describe_stream_action` answers it, held while it is queued or running and among the last 64 finished actions of its project. Name the URI in `subscriptions/listen`'s resource subscriptions to be sent `notifications/resources/updated` for it each time it changes — its `state`, its `step`, its `streams` or its `venv_sync` — then read it.")
    .with_mime_type(JSON_RESOURCE_MIME_TYPE)
}

/// The resource subscriptions in `requested` that name a stream action resource.
pub(crate) fn accepted_stream_action_subscriptions(requested: &SubscriptionFilter) -> Vec<String> {
    requested
        .resource_subscriptions
        .iter()
        .flatten()
        .filter(|uri| is_a_stream_action_resource_uri(uri))
        .cloned()
        .collect()
}

/// Send `context` an update for each stream action resource it subscribed to as the actions
/// it covers change, until it is cancelled or the local API stops.
pub(crate) async fn announce_stream_action_changes_until_the_listen_ends(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    context: &SubscriptionContext,
    local_api_stopping_token: &CancellationToken,
) {
    let subscribed_resource_uris: Vec<String> = context
        .accepted()
        .resource_subscriptions
        .iter()
        .flatten()
        .filter(|uri| is_a_stream_action_resource_uri(uri))
        .cloned()
        .collect();
    if subscribed_resource_uris.is_empty() {
        return;
    }
    let mut stream_action_changes =
        operations_on_the_loaded_streams.listen_for_each_stream_action_change();
    loop {
        let updated_resource_uris: Vec<String> = tokio::select! {
            () = context.cancelled() => return,
            () = local_api_stopping_token.cancelled() => return,
            received = stream_action_changes.recv() => match received {
                Ok(changed_action_id) => vec![
                    STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI.to_owned(),
                    stream_action_resource_uri(&changed_action_id),
                ],
                Err(RecvError::Lagged(missed_change_count)) => {
                    tracing::debug!(missed_change_count, "a stream action listener fell behind; every subscribed action is marked updated");
                    subscribed_resource_uris.clone()
                }
                Err(RecvError::Closed) => return,
            },
        };
        for updated_resource_uri in updated_resource_uris
            .into_iter()
            .filter(|uri| subscribed_resource_uris.contains(uri))
        {
            let update = ServerNotification::ResourceUpdatedNotification(Notification::new(
                ResourceUpdatedNotificationParam::new(updated_resource_uri),
            ));
            if let Err(not_sent) = context.sink().send(update).await {
                tracing::debug!(%not_sent, "a stream action update was not sent; the listen ends");
                return;
            }
        }
    }
}

/// Read the stream action resource at `uri`, or `None` when `uri` names none.
pub(crate) async fn read_stream_action_resource(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    uri: &str,
) -> Option<Result<ReadResourceResult, McpError>> {
    let operations_on_the_loaded_streams = Arc::clone(operations_on_the_loaded_streams);
    let rendering = if uri == STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI {
        tokio::task::spawn_blocking(move || {
            serde_json::to_string_pretty(&StreamActionsUnderWayResourceContents {
                actions: operations_on_the_loaded_streams
                    .stream_actions_under_way()
                    .into_iter()
                    .map(describe_stream_action_tool_result_of)
                    .collect(),
            })
            .map_err(|e| {
                McpError::internal_error(format!("the stream actions did not render: {e}"), None)
            })
        })
        .await
    } else {
        let action_id = the_action_a_stream_action_resource_uri_names(uri)?.to_owned();
        tokio::task::spawn_blocking(move || {
            let stream_action = operations_on_the_loaded_streams
                .describe_stream_action(&action_id)
                .map_err(|refusal| {
                    McpError::resource_not_found(
                        refusal.to_string(),
                        Some(json!({ "action": action_id })),
                    )
                })?;
            serde_json::to_string_pretty(&describe_stream_action_tool_result_of(stream_action))
                .map_err(|e| {
                    McpError::internal_error(format!("the stream action did not render: {e}"), None)
                })
        })
        .await
    };
    Some(
        rendering
            .map_err(|join_failure| {
                McpError::internal_error(format!("the read did not finish: {join_failure}"), None)
            })
            .and_then(|rendering| rendering)
            .map(|rendering| {
                ReadResourceResult::new(vec![
                    ResourceContents::text(rendering, uri).with_mime_type(JSON_RESOURCE_MIME_TYPE),
                ])
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_resource_uri_names_its_action_and_no_other_uri_names_one() {
        assert_eq!(
            the_action_a_stream_action_resource_uri_names(&stream_action_resource_uri("Rabc-3")),
            Some("Rabc-3")
        );
        assert_eq!(
            the_action_a_stream_action_resource_uri_names("streamlib://stream-action/"),
            None
        );
        assert_eq!(
            the_action_a_stream_action_resource_uri_names("streamlib://stream-action/a/b"),
            None
        );
        assert_eq!(
            the_action_a_stream_action_resource_uri_names(STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI),
            None
        );
    }

    #[test]
    fn a_listen_is_accepted_for_the_stream_action_resources_alone() {
        let requested = SubscriptionFilter::builder()
            .resource_subscriptions(vec![
                STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI.to_owned(),
                stream_action_resource_uri("Rabc-3"),
                "streamlib://graph".to_owned(),
            ])
            .build();
        assert_eq!(
            accepted_stream_action_subscriptions(&requested),
            [
                STREAM_ACTIONS_UNDER_WAY_RESOURCE_URI.to_owned(),
                stream_action_resource_uri("Rabc-3"),
            ]
        );
    }
}
