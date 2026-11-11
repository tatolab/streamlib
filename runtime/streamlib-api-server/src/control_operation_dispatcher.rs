// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one dispatcher every adapter calls: it runs a registered operation against the runtime
//! and answers its result or its refusal, holding no adapter's wire shape.

use std::sync::Arc;

use serde_json::Value;
use streamlib::sdk::error::PrivacyGatedCaptureDevice;
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, StreamActionStep, StreamListingState, StreamLocation,
};
use streamlib_control_contract::{
    AddNodeRequest, ControlOperationCall, DescribeRuntimeRequest, GraphEditMode, GraphRequest,
    ListStreamsRequest, OneLinkGraphEditRequest, ProjectTransactionAuthorArgument,
    ProjectTransactionAuthorKindArgument, TransactRequest,
};
use streamlib_project_repository::{
    ProjectRepositoriesOpenInThisRuntime, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
    StreamGraphAction,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CaptureDeviceOnTheWire, DescribeRuntimeToolResult, GraphTransactionToolResult,
    ListStreamsToolResult, ListStreamsToolResultStream, ListedStreamState,
    ProjectActionRefusalOnTheWire, StreamActionStepOnTheWire, StreamLocationOnTheWire,
};
use uuid::Uuid;

use crate::mcp_project_asset_tools::the_idempotency_key_a_caller_sent;
use crate::mcp_project_change_feed::{
    ProjectActionTheRuntimeRefused, ProjectActionsRefusedSinceTheRuntimeStarted,
};
use crate::stream_graph_transaction_executor::{
    GraphTransactionRefusal, StreamGraphTransactionExecutor,
};

/// The author a change by an unnamed client is recorded under.
const AUTHOR_NAME_OF_AN_UNNAMED_CLIENT: &str = "an MCP client";

/// Where a control request came from, as the runtime saw it arrive.
#[derive(Clone, Debug)]
pub(crate) struct ControlRequestOrigin {
    /// The name the client gave itself, if it gave one.
    pub(crate) client_name: Option<String>,
}

/// What a registered operation answers when it is not refused.
#[derive(Debug)]
pub(crate) enum ControlOperationAnswer {
    /// `describe_runtime`'s.
    DescribeRuntime(DescribeRuntimeToolResult),
    /// `list_streams`'s.
    ListStreams(ListStreamsToolResult),
    /// `graph`'s: one stream's graph, or every loaded stream's.
    Graph(Value),
    /// A graph edit's: the transaction it applied.
    GraphTransaction(GraphTransactionToolResult),
}

/// Why a registered operation answered no result.
#[derive(Debug)]
pub(crate) enum ControlOperationRefusal {
    /// The runtime refused the action, naming what it refused in its own words.
    RefusedAction(Box<ProjectActionRefusalOnTheWire>),
    /// The operation could not be carried out, in the runtime's own words.
    Failed(String),
}

/// The runtime's services every operation reaches.
pub(crate) struct ControlOperationDispatcher {
    pub(crate) operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    pub(crate) project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    pub(crate) project_actions_refused: Arc<ProjectActionsRefusedSinceTheRuntimeStarted>,
}

impl ControlOperationDispatcher {
    /// Run `call` for a request from `request_origin`.
    pub(crate) async fn dispatch(
        &self,
        call: ControlOperationCall,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        match call {
            ControlOperationCall::DescribeRuntime(DescribeRuntimeRequest {}) => {
                Ok(ControlOperationAnswer::DescribeRuntime(
                    self.operations_on_the_loaded_streams
                        .describe_this_runtime(),
                ))
            }
            ControlOperationCall::ListStreams(ListStreamsRequest {}) => self
                .list_streams()
                .await
                .map(ControlOperationAnswer::ListStreams),
            ControlOperationCall::Graph(GraphRequest { stream }) => {
                self.graph(stream).await.map(ControlOperationAnswer::Graph)
            }
            ControlOperationCall::Transact(TransactRequest {
                operations,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "transact",
                    operations,
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::AddNode(AddNodeRequest {
                stream,
                node_type,
                name,
                config,
                pins,
                exposed_config_fields,
                studio,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "add_node",
                    vec![StreamGraphAction::AddNode {
                        stream,
                        node_type,
                        name,
                        config,
                        pins,
                        exposed_config_fields,
                        studio,
                        label: None,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
            ControlOperationCall::Connect(OneLinkGraphEditRequest {
                stream,
                source,
                target,
                mode,
                author,
                idempotency_key,
            }) => self
                .apply_a_graph_transaction(
                    "connect",
                    vec![StreamGraphAction::Connect {
                        stream,
                        source,
                        target,
                    }],
                    mode,
                    author,
                    idempotency_key,
                    request_origin,
                )
                .await
                .map(ControlOperationAnswer::GraphTransaction),
        }
    }

    async fn list_streams(&self) -> Result<ListStreamsToolResult, ControlOperationRefusal> {
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let listings =
            tokio::task::spawn_blocking(move || operations_on_the_loaded_streams.list_streams())
                .await
                .map_err(|join_failure| {
                    ControlOperationRefusal::Failed(format!(
                        "the stream action did not finish: {join_failure}"
                    ))
                })?;
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

    async fn graph(&self, stream: Option<String>) -> Result<Value, ControlOperationRefusal> {
        match stream {
            Some(stream) => self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(&stream)
                .map_err(|refusal| {
                    ControlOperationRefusal::Failed(format!("graph failed: {refusal}"))
                })?
                .to_json_async()
                .await
                .map_err(|e| ControlOperationRefusal::Failed(format!("graph export failed: {e}"))),
            None => {
                crate::handlers::machine_wide_graph_json(&self.operations_on_the_loaded_streams)
                    .await
                    .map_err(|e| {
                        ControlOperationRefusal::Failed(format!("graph export failed: {e}"))
                    })
            }
        }
    }

    /// Apply `operations` as one transaction, its refusal kept on the project's change feed
    /// under the action `action_name`.
    pub(crate) async fn apply_a_graph_transaction(
        &self,
        action_name: &'static str,
        operations: Vec<StreamGraphAction>,
        mode: Option<GraphEditMode>,
        author: Option<ProjectTransactionAuthorArgument>,
        idempotency_key: Option<String>,
        request_origin: &ControlRequestOrigin,
    ) -> Result<GraphTransactionToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        let mode = mode.unwrap_or_default();
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories);
        let project_actions_refused = Arc::clone(&self.project_actions_refused);
        let async_runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let executor = StreamGraphTransactionExecutor {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            };
            let operations_as_requested: Vec<Value> = operations
                .iter()
                .map(|operation| serde_json::to_value(operation).unwrap_or_default())
                .collect();
            let stream_assets_named: Vec<Uuid> =
                operations.iter().map(StreamGraphAction::stream).collect();
            let (idempotency_key, applied) =
                match the_idempotency_key_a_caller_sent(idempotency_key) {
                    Ok(idempotency_key) => {
                        let applied =
                            executor.apply(operations, mode, &author, idempotency_key.as_ref());
                        (idempotency_key, applied)
                    }
                    Err(refusal) => (None, Err(GraphTransactionRefusal::from(refusal))),
                };
            applied.map_err(|refusal| {
                let refusal = refusal.into_the_refusal_on_the_wire(action_name);
                if let Some(project) =
                    executor.the_project_of_the_first_stream_asset_of(&stream_assets_named)
                {
                    project_actions_refused.keep_and_announce(
                        &project_repositories,
                        project,
                        ProjectActionTheRuntimeRefused {
                            author: &author,
                            mode: mode.on_the_wire(),
                            operations_as_requested,
                            idempotency_key: idempotency_key.as_ref(),
                            refusal: refusal.clone(),
                        },
                    );
                }
                ControlOperationRefusal::RefusedAction(Box::new(refusal))
            })
        })
        .await
        .map_err(|join_failure| {
            ControlOperationRefusal::Failed(format!(
                "{action_name} failed: the transaction did not finish: {join_failure}"
            ))
        })?
    }
}

/// The author a change names: the caller's own, or the agent the request's client named itself.
pub(crate) fn the_author_of_a_change(
    author: Option<ProjectTransactionAuthorArgument>,
    request_origin: &ControlRequestOrigin,
) -> ProjectTransactionAuthor {
    match author {
        Some(ProjectTransactionAuthorArgument { kind, name }) => ProjectTransactionAuthor {
            kind: match kind {
                ProjectTransactionAuthorKindArgument::Person => {
                    ProjectTransactionAuthorKind::Person
                }
                ProjectTransactionAuthorKindArgument::Agent => ProjectTransactionAuthorKind::Agent,
            },
            name,
        },
        None => ProjectTransactionAuthor {
            kind: ProjectTransactionAuthorKind::Agent,
            name: request_origin
                .client_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| AUTHOR_NAME_OF_AN_UNNAMED_CLIENT.to_owned()),
        },
    }
}

pub(crate) fn stream_location_on_the_wire(
    stream_location: StreamLocation,
) -> StreamLocationOnTheWire {
    StreamLocationOnTheWire {
        project_directory: stream_location.project_directory,
        project: stream_location.project,
        stream_asset: stream_location.stream_asset,
    }
}

pub(crate) fn listed_stream_state_on_the_wire(state: StreamListingState) -> ListedStreamState {
    match state {
        StreamListingState::Running => ListedStreamState::Running,
        StreamListingState::Stopped => ListedStreamState::Stopped,
        StreamListingState::Paused => ListedStreamState::Paused,
        StreamListingState::Failed => ListedStreamState::Failed,
        StreamListingState::Preparing => ListedStreamState::Preparing,
        StreamListingState::Stopping => ListedStreamState::Stopping,
    }
}

fn capture_device_on_the_wire(device: PrivacyGatedCaptureDevice) -> CaptureDeviceOnTheWire {
    match device {
        PrivacyGatedCaptureDevice::Camera => CaptureDeviceOnTheWire::Camera,
        PrivacyGatedCaptureDevice::Microphone => CaptureDeviceOnTheWire::Microphone,
    }
}

pub(crate) fn stream_action_step_on_the_wire(step: StreamActionStep) -> StreamActionStepOnTheWire {
    match step {
        StreamActionStep::MakingTheVenv => StreamActionStepOnTheWire::MakingTheVenv,
        StreamActionStep::ReadingTheGraphDocuments => {
            StreamActionStepOnTheWire::ReadingTheGraphDocuments
        }
        StreamActionStep::Starting => StreamActionStepOnTheWire::Starting,
        StreamActionStep::Stopping => StreamActionStepOnTheWire::Stopping,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_origin_named(client_name: Option<&str>) -> ControlRequestOrigin {
        ControlRequestOrigin {
            client_name: client_name.map(str::to_owned),
        }
    }

    #[test]
    fn a_change_with_no_author_is_by_the_agent_its_client_named() {
        let author = the_author_of_a_change(None, &an_origin_named(Some("tato-studio")));
        assert_eq!(author.kind, ProjectTransactionAuthorKind::Agent);
        assert_eq!(author.name, "tato-studio");
    }

    #[test]
    fn a_change_from_an_unnamed_or_blank_client_is_by_an_unnamed_client() {
        for client_name in [None, Some("  ")] {
            let author = the_author_of_a_change(None, &an_origin_named(client_name));
            assert_eq!(author.name, AUTHOR_NAME_OF_AN_UNNAMED_CLIENT);
        }
    }

    #[test]
    fn a_named_author_is_recorded_as_named_whatever_the_client() {
        let author = the_author_of_a_change(
            Some(ProjectTransactionAuthorArgument {
                kind: ProjectTransactionAuthorKindArgument::Person,
                name: "Dana".to_owned(),
            }),
            &an_origin_named(Some("tato-studio")),
        );
        assert_eq!(author.kind, ProjectTransactionAuthorKind::Person);
        assert_eq!(author.name, "Dana");
    }
}
