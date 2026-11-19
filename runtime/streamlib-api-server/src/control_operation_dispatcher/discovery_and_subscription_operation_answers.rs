// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use serde_json::Value;
use streamlib_control_contract::{
    CONTROL_CONTRACT_REVISION, CONTROL_CONTRACT_VERSION, CONTROL_OPERATIONS, DiscoverResult,
    SubscribeInputValueChangesResult, SubscribeProjectChangesResult,
    SubscribeProjectLogRecordsResult, SubscribeStreamActionChangesResult,
    SubscribeStreamGraphChangesResult,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET, ListStreamsToolResult, ListStreamsToolResultStream,
};
use streamlib_runtime_client_contract::project_log_records::project_log_cursor_at_its_end;
use uuid::Uuid;

use crate::control_subscriptions_of_one_connection::{
    ControlSubscriptionsOfOneConnection, ProjectLogAsSubscribedWithItsEnd,
    WhatAConnectionFollowsOfAStream,
};

use super::{
    ContractAClientNames, ControlOperationDispatcher, ControlOperationRefusal,
    ControlRequestOrigin, capture_device_on_the_wire, listed_stream_state_on_the_wire,
    refuse_a_relative_project_directory, stream_action_step_on_the_wire,
    stream_location_on_the_wire,
};

impl ControlOperationDispatcher {
    pub(super) fn discover(
        &self,
        client: ContractAClientNames,
    ) -> Result<DiscoverResult, ControlOperationRefusal> {
        self.refuse_a_client_of_another_contract(client)
            .map_err(ControlOperationRefusal::ContractVersionMismatch)?;
        let this_runtime = self
            .operations_on_the_loaded_streams
            .describe_this_runtime();
        Ok(DiscoverResult {
            contract_version: CONTROL_CONTRACT_VERSION,
            contract_revision: CONTROL_CONTRACT_REVISION,
            runtime_version: this_runtime.version,
            build_id: this_runtime.build_id,
            operations: CONTROL_OPERATIONS
                .iter()
                .map(|declaration| declaration.method_name.to_owned())
                .collect(),
        })
    }

    pub(super) async fn list_streams(
        &self,
    ) -> Result<ListStreamsToolResult, ControlOperationRefusal> {
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let listings =
            tokio::task::spawn_blocking(move || operations_on_the_loaded_streams.list_streams())
                .await
                .map_err(ControlOperationRefusal::the_task_did_not_finish(
                    "the stream action",
                ))?;
        Ok(ListStreamsToolResult {
            streams: listings
                .into_iter()
                .map(|listing| ListStreamsToolResultStream {
                    stream: listing.stream_id,
                    display_name: listing.display_name,
                    location: listing.stream_location.map(stream_location_on_the_wire),
                    state: listed_stream_state_on_the_wire(listing.state),
                    node_count: listing.node_count,
                    failed_because: listing.failed_because,
                    condition: Some(listing.condition),
                    capture_devices_whose_access_was_never_asked_for: listing
                        .capture_devices_whose_access_was_never_asked_for
                        .into_iter()
                        .map(capture_device_on_the_wire)
                        .collect(),
                    step: listing
                        .stream_action_under_way
                        .as_ref()
                        .and_then(|working_on_it| working_on_it.step)
                        .map(stream_action_step_on_the_wire),
                    action: listing
                        .stream_action_under_way
                        .map(|working_on_it| working_on_it.action_id),
                })
                .collect(),
        })
    }

    pub(super) async fn graph(
        &self,
        stream: Option<String>,
    ) -> Result<Value, ControlOperationRefusal> {
        match stream {
            Some(stream) => {
                let operations_on_the_loaded_streams =
                    Arc::clone(&self.operations_on_the_loaded_streams);
                let async_runtime = tokio::runtime::Handle::current();
                tokio::task::spawn_blocking(move || {
                    let streams = operations_on_the_loaded_streams
                        .the_streams_to_read_or_edit(std::slice::from_ref(&stream))
                        .map_err(|refusal| {
                            ControlOperationRefusal::of_an_engine_error("graph", &refusal)
                        })?;
                    let mut stream_graph = streams
                        .runtime_operations_of(&stream)
                        .and_then(|stream_operations| {
                            async_runtime.block_on(stream_operations.the_graph_as_answered_async())
                        })
                        .map_err(|refusal| {
                            ControlOperationRefusal::of_an_engine_error("graph export", &refusal)
                        })?;
                    crate::handlers::give_the_stream_graph_its_listed_condition(
                        &mut stream_graph,
                        &operations_on_the_loaded_streams.list_streams(),
                    );
                    Ok(stream_graph)
                })
                .await
                .map_err(ControlOperationRefusal::the_task_did_not_finish(
                    "the graph read",
                ))?
            }
            None => {
                crate::handlers::machine_wide_graph_json(&self.operations_on_the_loaded_streams)
                    .await
                    .map_err(|refusal| {
                        ControlOperationRefusal::of_an_engine_error("graph export", &refusal)
                    })
            }
        }
    }
}

/// The subscriptions of the connection a subscription by `method_name` arrived on, refused for a
/// connection that does not stay open.
fn the_subscriptions_of_the_connection_a_subscription_arrived_on<'origin>(
    method_name: &str,
    request_origin: &'origin ControlRequestOrigin,
) -> Result<&'origin Arc<ControlSubscriptionsOfOneConnection>, ControlOperationRefusal> {
    request_origin.subscriptions.as_ref().ok_or_else(|| {
        ControlOperationRefusal::failed(format!(
            "{method_name} failed: it is served only on a connection that stays open to be sent \
             notifications, `{CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET}`"
        ))
    })
}

pub(super) fn subscribe_to_project_log_records(
    project_directories: Vec<std::path::PathBuf>,
    request_origin: &ControlRequestOrigin,
) -> Result<SubscribeProjectLogRecordsResult, ControlOperationRefusal> {
    let subscriptions = the_subscriptions_of_the_connection_a_subscription_arrived_on(
        "subscribe_project_log_records",
        request_origin,
    )?;
    if project_directories.is_empty() {
        return Err(ControlOperationRefusal::failed(
            "subscribe_project_log_records failed: `project_directories` names no project"
                .to_owned(),
        ));
    }
    for project_directory in &project_directories {
        refuse_a_relative_project_directory(project_directory).map_err(|why| {
            ControlOperationRefusal::failed(format!("subscribe_project_log_records failed: {why}"))
        })?;
    }
    let project_logs_as_subscribed_with_symlinks_resolved_and_their_end = project_directories
        .into_iter()
        .map(|project_directory| {
            let project_directory_with_symlinks_resolved = std::fs::canonicalize(
                &project_directory,
            )
            .map_err(|not_resolved| {
                ControlOperationRefusal::failed(format!(
                    "subscribe_project_log_records failed: `{}` is not a folder on this machine: \
                     {not_resolved}",
                    project_directory.display()
                ))
            })?;
            let cursor_at_the_logs_end = project_log_cursor_at_its_end(
                &project_directory_with_symlinks_resolved,
            )
            .map_err(|not_read| {
                ControlOperationRefusal::failed(format!(
                    "subscribe_project_log_records failed: {not_read}"
                ))
            })?;
            Ok(ProjectLogAsSubscribedWithItsEnd {
                project_directory_as_subscribed: project_directory,
                project_directory_with_symlinks_resolved,
                cursor_at_the_logs_end,
            })
        })
        .collect::<Result<Vec<_>, ControlOperationRefusal>>()?;
    Ok(SubscribeProjectLogRecordsResult {
        project_directories: subscriptions.subscribe_to_the_project_logs(
            project_logs_as_subscribed_with_symlinks_resolved_and_their_end,
        ),
    })
}

impl ControlOperationDispatcher {
    pub(super) fn subscribe_to_project_changes(
        &self,
        projects: Vec<Uuid>,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SubscribeProjectChangesResult, ControlOperationRefusal> {
        let subscriptions = the_subscriptions_of_the_connection_a_subscription_arrived_on(
            "subscribe_project_changes",
            request_origin,
        )?;
        if projects.is_empty() {
            return Err(ControlOperationRefusal::failed(
                "subscribe_project_changes failed: `projects` names no project".to_owned(),
            ));
        }
        Ok(SubscribeProjectChangesResult {
            projects: subscriptions.subscribe_to_the_projects(projects, |project| {
                self.project_repositories.watch_the_files_of(project)
            }),
        })
    }

    pub(super) async fn subscribe_to_stream_graph_changes(
        &self,
        streams: Vec<String>,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SubscribeStreamGraphChangesResult, ControlOperationRefusal> {
        self.follow_the_streams(
            "subscribe_stream_graph_changes",
            streams,
            WhatAConnectionFollowsOfAStream::Graph,
            request_origin,
        )
        .await
        .map(|streams| SubscribeStreamGraphChangesResult { streams })
    }

    pub(super) async fn subscribe_to_input_value_changes(
        &self,
        streams: Vec<String>,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SubscribeInputValueChangesResult, ControlOperationRefusal> {
        self.follow_the_streams(
            "subscribe_input_value_changes",
            streams,
            WhatAConnectionFollowsOfAStream::InputValues,
            request_origin,
        )
        .await
        .map(|streams| SubscribeInputValueChangesResult { streams })
    }

    /// Follow `what` of each of `streams`, by stream id, on the connection `request_origin`
    /// arrived on, for the subscription `method_name`, answering every stream it follows that of.
    async fn follow_the_streams(
        &self,
        method_name: &'static str,
        streams: Vec<String>,
        what: WhatAConnectionFollowsOfAStream,
        request_origin: &ControlRequestOrigin,
    ) -> Result<Vec<String>, ControlOperationRefusal> {
        let subscriptions = Arc::clone(
            the_subscriptions_of_the_connection_a_subscription_arrived_on(
                method_name,
                request_origin,
            )?,
        );
        if streams.is_empty() {
            return Err(ControlOperationRefusal::failed(format!(
                "{method_name} failed: `streams` names no stream"
            )));
        }
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || {
            let streams_as_loaded_now = streams
                .iter()
                .map(|stream| {
                    operations_on_the_loaded_streams
                        .loaded_stream_identity_of_the_stream_a_call_names(stream)
                })
                .collect::<streamlib::sdk::error::Result<Vec<_>>>()
                .map_err(|refusal| {
                    ControlOperationRefusal::of_an_engine_error(method_name, &refusal)
                })?;
            subscriptions
                .follow_the_streams(streams_as_loaded_now, what)
                .map_err(|not_subscribed| {
                    ControlOperationRefusal::failed(format!(
                        "{method_name} failed: {not_subscribed}"
                    ))
                })
        })
        .await
        .map_err(ControlOperationRefusal::the_task_did_not_finish(format!(
            "{method_name} failed: the subscription"
        )))?
    }

    pub(super) async fn subscribe_to_stream_action_changes(
        &self,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SubscribeStreamActionChangesResult, ControlOperationRefusal> {
        let subscriptions = the_subscriptions_of_the_connection_a_subscription_arrived_on(
            "subscribe_stream_action_changes",
            request_origin,
        )?;
        subscriptions.subscribe_to_stream_actions();
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        tokio::task::spawn_blocking(move || SubscribeStreamActionChangesResult {
            actions_under_way: operations_on_the_loaded_streams
                .stream_actions_under_way()
                .into_iter()
                .map(|action| action.action_id)
                .collect(),
        })
        .await
        .map_err(ControlOperationRefusal::the_task_did_not_finish(
            "subscribe_stream_action_changes failed: the subscription",
        ))
    }
}
