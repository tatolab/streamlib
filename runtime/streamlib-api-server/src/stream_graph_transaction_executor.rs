// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one executor of a stream graph transaction: each operation staged, applied whole or
//! refused whole, written to the graph documents and committed to the project's history, or
//! applied live to the running streams alone.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations, StreamLocation,
};
use streamlib_control_contract::GraphEditMode;
use streamlib_project_repository::{
    AUTHORED_FRAGMENT_NODE_TYPE, AppliedProjectTransaction, ProjectAction,
    ProjectActionIdempotencyKey, ProjectOperation, ProjectRepositoriesOpenInThisRuntime,
    ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionLineage, ProjectTransactionReversal, STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY,
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphAction, StreamGraphActionLinkEnd, StreamGraphDocument,
    StreamGraphDocumentGroup, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, StreamGraphDocumentNote, StreamGraphFragmentPlacement,
    StreamGraphGroupFields, StreamGraphNodeStudioFields, StreamGraphNoteFields,
    StreamGraphOperation, StreamGraphOperationOnAStream,
    fragment_with_each_node_uuid_kept_by_its_name, graph_document_holds_a_stream_fragment,
    the_fragment_a_stream_fragment_places, the_stream_document_of_an_import,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphLinkEndOnTheWire, GraphTransactionAddedGroup, GraphTransactionAddedNode,
    GraphTransactionAddedNote, GraphTransactionConnectedLink, GraphTransactionToolResult,
    ProjectActionRefusalOnTheWire, RefusedGraphLinkOnTheWire,
};
use uuid::Uuid;

use crate::control_operations_on_project_assets::{
    bring_the_loaded_streams_in_line_with_the_stream_documents_of,
    the_idempotency_key_a_caller_sent,
};
use crate::mcp_project_change_feed::project_transaction_on_the_wire;

/// The node, link and field `action` names, as its caller named them.
fn what_a_graph_action_names(action: &StreamGraphAction) -> WhatAGraphOperationNames {
    let names_a_node = |node: &str| WhatAGraphOperationNames {
        node: Some(node.to_owned()),
        ..WhatAGraphOperationNames::default()
    };
    match action {
        StreamGraphAction::RemoveNode { node, .. }
        | StreamGraphAction::SetNodeConfig { node, .. }
        | StreamGraphAction::RenameNode { node, .. }
        | StreamGraphAction::SetStudioFields { node, .. }
        | StreamGraphAction::SetExposedConfigFields { node, .. } => names_a_node(node),
        StreamGraphAction::SetPinValue { node, input, .. } => WhatAGraphOperationNames {
            field: Some(input.clone()),
            ..names_a_node(node)
        },
        StreamGraphAction::Connect { source, target, .. }
        | StreamGraphAction::Disconnect { source, target, .. } => WhatAGraphOperationNames {
            link: Some(RefusedGraphLinkOnTheWire {
                source: GraphLinkEndOnTheWire {
                    node: source.node.clone(),
                    port: source.port.clone(),
                },
                target: GraphLinkEndOnTheWire {
                    node: target.node.clone(),
                    port: target.port.clone(),
                },
            }),
            ..WhatAGraphOperationNames::default()
        },
        StreamGraphAction::AddNode { label, name, .. }
        | StreamGraphAction::ImportGraphGenerator { label, name, .. }
        | StreamGraphAction::UseDependencyStream { label, name, .. } => WhatAGraphOperationNames {
            node: label.clone().or_else(|| name.clone()),
            ..WhatAGraphOperationNames::default()
        },
        StreamGraphAction::CollapseSelectionIntoSubgraph { label, name, .. } => {
            WhatAGraphOperationNames {
                node: label.clone().or_else(|| Some(name.clone())),
                ..WhatAGraphOperationNames::default()
            }
        }
        StreamGraphAction::UnlockGeneratorFragment { fragment, .. } => names_a_node(fragment),
        StreamGraphAction::AddGroup { .. }
        | StreamGraphAction::SetGroupFields { .. }
        | StreamGraphAction::RemoveGroup { .. }
        | StreamGraphAction::AddNote { .. }
        | StreamGraphAction::SetNoteFields { .. }
        | StreamGraphAction::RemoveNote { .. } => WhatAGraphOperationNames::default(),
    }
}
/// One loaded stream a transaction edits: its operations, where it lives, and its graph as the
/// transaction has staged it.
struct StreamATransactionEdits {
    stream_operations: Arc<dyn RuntimeOperations>,
    stream_location: StreamLocation,
    staged_graph: StreamGraphDocument,
}

/// The node, link and field one operation of a graph action names, as its caller named them.
#[derive(Default)]
pub(crate) struct WhatAGraphOperationNames {
    node: Option<String>,
    link: Option<RefusedGraphLinkOnTheWire>,
    field: Option<String>,
}

/// Why a graph action was refused, in the runtime's own words, and which of its operations
/// was refused when one was.
pub(crate) struct GraphTransactionRefusal {
    refused_operation_index: Option<usize>,
    what_the_refused_operation_names: Box<WhatAGraphOperationNames>,
    runtime_words: String,
}

impl GraphTransactionRefusal {
    fn of_the_operation(
        refused_operation_index: usize,
        what_the_refused_operation_names: WhatAGraphOperationNames,
        runtime_words: String,
    ) -> Self {
        Self {
            refused_operation_index: Some(refused_operation_index),
            what_the_refused_operation_names: Box::new(what_the_refused_operation_names),
            runtime_words,
        }
    }

    pub(crate) fn of_the_whole_transaction(runtime_words: String) -> Self {
        Self {
            refused_operation_index: None,
            what_the_refused_operation_names: Box::default(),
            runtime_words,
        }
    }

    /// The refusal of the action the tool `tool_name` was sent, as the wire carries it.
    pub(crate) fn into_the_refusal_on_the_wire(
        self,
        tool_name: &str,
    ) -> ProjectActionRefusalOnTheWire {
        let what_the_refused_operation_names = *self.what_the_refused_operation_names;
        ProjectActionRefusalOnTheWire {
            action: tool_name.to_owned(),
            refused_operation_index: self.refused_operation_index,
            refused_node: what_the_refused_operation_names.node,
            refused_link: what_the_refused_operation_names.link,
            refused_field: what_the_refused_operation_names.field,
            not_running_because: None,
            runtime_words: self.runtime_words,
        }
    }
}

impl From<ProjectRepositoryRefusal> for GraphTransactionRefusal {
    fn from(refusal: ProjectRepositoryRefusal) -> Self {
        Self::of_the_whole_transaction(refusal.to_string())
    }
}

/// What a graph transaction works through on its blocking task.
pub(crate) struct StreamGraphTransactionExecutor<'transaction> {
    pub(crate) operations_on_the_loaded_streams:
        &'transaction dyn OperationsOnTheStreamsLoadedInThisRuntime,
    pub(crate) project_repositories: &'transaction ProjectRepositoriesOpenInThisRuntime,
    pub(crate) async_runtime: &'transaction tokio::runtime::Handle,
}

impl StreamGraphTransactionExecutor<'_> {
    /// Apply `requests` as one transaction by `author`; committed under an `idempotency_key`
    /// the project's history already holds, it applies nothing and answers that transaction.
    pub(crate) fn apply(
        &self,
        requests: Vec<StreamGraphAction>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
    ) -> Result<GraphTransactionToolResult, GraphTransactionRefusal> {
        if requests.is_empty() {
            return Err(GraphTransactionRefusal::of_the_whole_transaction(
                "a transaction holds at least one operation".to_owned(),
            ));
        }
        let stream_locations = self.the_location_of_each_loaded_stream();
        let stream_id_of_each_request = self.the_loaded_stream_each_request_names(&requests)?;
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for stream_id in &stream_id_of_each_request {
            if !stream_ids_in_order.contains(stream_id) {
                stream_ids_in_order.push(stream_id.clone());
            }
        }
        let project_repository = match mode {
            GraphEditMode::Live => None,
            GraphEditMode::Commit => Some(
                self.the_one_project_the_streams_belong_to(&stream_ids_in_order, &stream_locations)
                    .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
            ),
        };
        let mut project_repository_held = project_repository.as_ref().map(|held| held.lock());
        if let Some(project_repository_held) = project_repository_held.as_deref()
            && let Some(applied_before) = project_repository_held
                .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(self.the_answer_of_a_graph_transaction_applied_before(
                project_repository_held.project_file().project,
                applied_before,
            ));
        }
        let applied = self.apply_the_requests(
            requests
                .into_iter()
                .zip(stream_id_of_each_request)
                .collect(),
            mode,
            author,
            idempotency_key,
            &stream_locations,
            project_repository_held.as_deref_mut(),
        )?;
        if let Some(project_repository_held) = project_repository_held {
            let edited_project_directory =
                project_repository_held.project_directory().to_path_buf();
            drop(project_repository_held);
            self.reconcile_each_loaded_stream_holding_a_stream_fragment(&edited_project_directory);
        }
        Ok(applied)
    }

    /// Bring each loaded stream whose graph holds a stream fragment, of the project at
    /// `edited_project_directory` or of a project enclosing it as an import, to the streams its
    /// fragments reference, as their graph documents are on disk now. A stream that refuses
    /// keeps its live graph as it was, and the refusal is logged.
    pub(crate) fn reconcile_each_loaded_stream_holding_a_stream_fragment(
        &self,
        edited_project_directory: &std::path::Path,
    ) {
        let loaded_streams_the_edit_can_reach = self
            .operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter(|listing| listing.node_count.is_some())
            .filter_map(|listing| {
                let stream_location = listing.stream_location?;
                edited_project_directory
                    .starts_with(&stream_location.project_directory)
                    .then_some((stream_location.stream_asset, listing.stream_id))
            });
        for (stream_asset, stream_id) in loaded_streams_the_edit_can_reach {
            let stream_operations = match self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(&stream_id)
            {
                Ok(stream_operations) => stream_operations,
                Err(_) => continue,
            };
            let holds_a_stream_fragment = self
                .async_runtime
                .block_on(stream_operations.to_json_async())
                .is_ok_and(|graph| graph_document_holds_a_stream_fragment(&graph));
            if !holds_a_stream_fragment {
                continue;
            }
            if let Err(refusal) = self.async_runtime.block_on(
                stream_operations.reconcile_each_stream_fragment_to_the_stream_it_references_live(),
            ) {
                tracing::warn!(
                    stream = %stream_id,
                    %stream_asset,
                    %refusal,
                    "a loaded stream's fragments were not brought to the streams they reference; \
                     its live graph is kept as it was until its next load"
                );
            }
        }
    }

    /// The project of the first of `stream_ids` that is a loaded stream of a project.
    pub(crate) fn the_project_of_the_first_stream_asset_of(
        &self,
        stream_assets: &[Uuid],
    ) -> Option<Uuid> {
        let project_of_each_loaded_stream_asset: HashMap<Uuid, Uuid> = self
            .the_location_of_each_loaded_stream()
            .into_values()
            .map(|stream_location| (stream_location.stream_asset, stream_location.project))
            .collect();
        stream_assets.iter().find_map(|stream_asset| {
            project_of_each_loaded_stream_asset
                .get(stream_asset)
                .copied()
        })
    }

    /// The stream id of the stream running each request's stream asset, in request order; a
    /// loaded one over one only recorded.
    fn the_loaded_stream_each_request_names(
        &self,
        requests: &[StreamGraphAction],
    ) -> Result<Vec<String>, GraphTransactionRefusal> {
        let mut listings_loaded_last = self.operations_on_the_loaded_streams.list_streams();
        listings_loaded_last.sort_by_key(|listing| listing.node_count.is_some());
        let stream_id_of_each_loaded_stream_asset: HashMap<Uuid, String> = listings_loaded_last
            .into_iter()
            .filter_map(|listing| Some((listing.stream_location?.stream_asset, listing.stream_id)))
            .collect();
        requests
            .iter()
            .enumerate()
            .map(|(request_index, request)| {
                stream_id_of_each_loaded_stream_asset
                    .get(&request.stream())
                    .cloned()
                    .ok_or_else(|| {
                        let mut loaded_stream_assets: Vec<String> =
                            stream_id_of_each_loaded_stream_asset
                                .iter()
                                .map(|(stream_asset, stream_id)| {
                                    format!("{stream_asset} (stream `{stream_id}`)")
                                })
                                .collect();
                        loaded_stream_assets.sort();
                        GraphTransactionRefusal::of_the_operation(
                            request_index,
                            what_a_graph_action_names(request),
                            format!(
                                "no stream the runtime holds runs the stream asset {}; the stream \
                                 assets it holds are: {}",
                                request.stream(),
                                if loaded_stream_assets.is_empty() {
                                    "none".to_owned()
                                } else {
                                    loaded_stream_assets.join(", ")
                                }
                            ),
                        )
                    })
            })
            .collect()
    }

    /// The location of each listed stream by its stream id.
    fn the_location_of_each_loaded_stream(&self) -> HashMap<String, StreamLocation> {
        self.operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter_map(|listing| Some((listing.stream_id, listing.stream_location?)))
            .collect()
    }

    /// The stream id of each of the project `project`'s loaded streams, by its stream asset.
    pub(crate) fn the_stream_id_of_each_loaded_stream_asset(
        &self,
        project: Uuid,
    ) -> HashMap<Uuid, String> {
        self.operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter(|listing| listing.node_count.is_some())
            .filter_map(|listing| {
                let stream_location = listing.stream_location?;
                (stream_location.project == project)
                    .then_some((stream_location.stream_asset, listing.stream_id))
            })
            .collect()
    }

    /// Apply each request, to the loaded stream it names, as one transaction by `author`, to the
    /// project `project_repository_held` holds when the transaction is committed.
    fn apply_the_requests(
        &self,
        requests_and_the_stream_id_each_names: Vec<(StreamGraphAction, String)>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        stream_locations: &HashMap<String, StreamLocation>,
        project_repository_held: Option<&mut ProjectRepository>,
    ) -> Result<GraphTransactionToolResult, GraphTransactionRefusal> {
        let actions_as_sent: Vec<ProjectAction> = requests_and_the_stream_id_each_names
            .iter()
            .map(|(request, _)| ProjectAction::StreamGraph(request.clone()))
            .collect();
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for (_, stream_id) in &requests_and_the_stream_id_each_names {
            if !stream_ids_in_order.contains(stream_id) {
                stream_ids_in_order.push(stream_id.clone());
            }
        }

        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        for stream_id in &stream_ids_in_order {
            let stream_operations = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(stream_id)
                .map_err(|refusal| {
                    GraphTransactionRefusal::of_the_whole_transaction(refusal.to_string())
                })?;
            let stream_location = stream_locations.get(stream_id).cloned().ok_or_else(|| {
                GraphTransactionRefusal::of_the_whole_transaction(format!(
                    "stream `{stream_id}` is not loaded"
                ))
            })?;
            let staged_graph = match &project_repository_held {
                Some(project_repository) => {
                    if stream_location.project != project_repository.project_file().project {
                        return Err(GraphTransactionRefusal::of_the_whole_transaction(format!(
                            "a transaction edits the streams of one project, and `{stream_id}` \
                             is of the project {}, not {}",
                            stream_location.project,
                            project_repository.project_file().project
                        )));
                    }
                    project_repository.stream_graph_document(stream_location.stream_asset)?
                }
                _ => StreamGraphDocument::from_value(
                    self.async_runtime
                        .block_on(stream_operations.to_json_async())
                        .map_err(|refusal| {
                            GraphTransactionRefusal::of_the_whole_transaction(refusal.to_string())
                        })?,
                )
                .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
            };
            streams.insert(
                stream_id.clone(),
                StreamATransactionEdits {
                    stream_operations,
                    stream_location,
                    staged_graph,
                },
            );
        }

        let mut node_by_label: HashMap<String, Uuid> = HashMap::new();
        let mut labels_of_added_nodes: HashMap<Uuid, String> = HashMap::new();
        let mut applied_live: Vec<(String, StreamGraphOperation)> = Vec::new();
        let project_repository_read_during_the_requests = project_repository_held.as_deref();
        for (request_index, (request, stream_id)) in requests_and_the_stream_id_each_names
            .into_iter()
            .enumerate()
        {
            let what_the_request_names = what_a_graph_action_names(&request);
            let applied = streams
                .get_mut(&stream_id)
                .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))
                .and_then(|stream| {
                    let label = match &request {
                        StreamGraphAction::AddNode { label, .. }
                        | StreamGraphAction::ImportGraphGenerator {
                            label, ..
                        }
                        | StreamGraphAction::UseDependencyStream {
                            label, ..
                        }
                        | StreamGraphAction::CollapseSelectionIntoSubgraph {
                            label, ..
                        } => label.clone(),
                        _ => None,
                    };
                    let run_the_graph_generator =
                        |generator: &str,
                         fragment_name: Option<&str>,
                         promoted_settings: &Map<String, Value>| {
                            self.operations_on_the_loaded_streams
                                .run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
                                    &stream_id,
                                    generator,
                                    fragment_name,
                                    promoted_settings,
                                )
                                .map_err(|refusal| refusal.to_string())
                        };
                    let place_the_imported_stream =
                        |fragment_node: Uuid, held_stream: &Value| {
                            let project_repository = project_repository_read_during_the_requests
                                .ok_or_else(|| {
                                    "an imported stream is placed from the project's \
                                     `imports/` folder; place it with `mode` `commit`"
                                        .to_owned()
                                })?;
                            the_fragment_an_imported_stream_places(
                                project_repository.project_directory(),
                                fragment_node,
                                held_stream,
                            )
                        };
                    let family_of_an_output_port = |end: &StreamGraphDocumentLinkEnd| {
                        let rendered_graph = self
                            .async_runtime
                            .block_on(stream.stream_operations.to_json_async())
                            .map_err(|refusal| refusal.to_string())?;
                        the_family_of_an_output_port_in(&rendered_graph, end)
                    };
                    let level = request.level().to_vec();
                    let edit_at_the_level = graph_operation_for_the_request(
                        request,
                        &stream.staged_graph.the_level_inside(&level)?,
                        &mut node_by_label,
                        &run_the_graph_generator,
                        &place_the_imported_stream,
                        &family_of_an_output_port,
                    )?;
                    let operation = stream
                        .staged_graph
                        .the_edit_at_the_level(&level, edit_at_the_level)?;
                    let applied = self
                        .async_runtime
                        .block_on(
                            stream
                                .stream_operations
                                .apply_a_stream_graph_operation_live(operation),
                        )
                        .map_err(|refusal| refusal.to_string())?;
                    let added_node = match applied.the_edit_at_its_own_level().1 {
                        StreamGraphOperation::AddNode { node, .. } => Some(node.id),
                        StreamGraphOperation::FoldIntoFragment { fold } => fold.fragment_node(),
                        _ => None,
                    };
                    if let (Some(label), Some(added_node)) = (label, added_node) {
                        labels_of_added_nodes.insert(added_node, label);
                    }
                    Ok(applied)
                });
            let applied = match applied {
                Ok(applied) => applied,
                Err(refusal) => {
                    self.reverse_what_was_applied_live(&streams, applied_live);
                    return Err(GraphTransactionRefusal::of_the_operation(
                        request_index,
                        what_the_request_names,
                        refusal,
                    ));
                }
            };
            let Some(stream) = streams.get_mut(&stream_id) else {
                return Err(GraphTransactionRefusal::of_the_operation(
                    request_index,
                    what_the_request_names,
                    format!("stream `{stream_id}` is not loaded"),
                ));
            };
            let staged = stream.staged_graph.apply(&applied);
            applied_live.push((stream_id.clone(), applied));
            if let Err(why) = staged {
                self.reverse_what_was_applied_live(&streams, applied_live);
                return Err(GraphTransactionRefusal::of_the_operation(
                    request_index,
                    what_the_request_names,
                    format!(
                        "stream `{stream_id}`'s graph document does not hold what its running \
                         graph does: {why}"
                    ),
                ));
            }
        }

        let (transaction, commit) = match project_repository_held {
            None => (None, None),
            Some(project_repository) => {
                let project_operations = applied_live
                    .iter()
                    .filter_map(|(stream_id, graph_operation)| {
                        let stream_location = &streams.get(stream_id)?.stream_location;
                        Some(ProjectOperation::StreamGraph(
                            StreamGraphOperationOnAStream {
                                stream: stream_location.stream_asset,
                                graph_operation: graph_operation.clone(),
                            },
                        ))
                    })
                    .collect();
                match project_repository.apply_project_operations(
                    author,
                    idempotency_key,
                    actions_as_sent,
                    project_operations,
                    ProjectTransactionLineage::NewEdit,
                ) {
                    Ok(outcome) => {
                        self.project_repositories
                            .announce_the_transaction_if_applied_now(project_repository, &outcome);
                        let applied = outcome.into_applied_transaction();
                        (
                            Some(applied.transaction.transaction.to_string()),
                            Some(applied.commit),
                        )
                    }
                    Err(refusal) => {
                        self.reverse_what_was_applied_live(&streams, applied_live);
                        return Err(refusal.into());
                    }
                }
            }
        };

        Ok(self.the_answer_of_a_graph_transaction(
            mode,
            transaction,
            commit,
            &|stream_id| {
                streams
                    .get(stream_id)
                    .map(|stream| Arc::clone(&stream.stream_operations))
            },
            applied_live,
            &labels_of_added_nodes,
        ))
    }

    /// The project every stream of `stream_ids` belongs to, its one writer, refused for
    /// streams of two projects.
    fn the_one_project_the_streams_belong_to(
        &self,
        stream_ids: &[String],
        stream_locations: &HashMap<String, StreamLocation>,
    ) -> Result<streamlib_project_repository::ProjectRepositoryOneWriterAtATime, String> {
        let mut project: Option<&StreamLocation> = None;
        for stream_id in stream_ids {
            let stream_location = match stream_locations.get(stream_id) {
                None => {
                    return Err(self
                        .operations_on_the_loaded_streams
                        .runtime_operations_of_the_stream_a_call_names(stream_id)
                        .err()
                        .map_or_else(
                            || format!("stream `{stream_id}` is not loaded"),
                            |refusal| refusal.to_string(),
                        ));
                }
                Some(stream_location) => stream_location,
            };
            match project {
                Some(first) if first.project != stream_location.project => {
                    return Err(format!(
                        "a transaction edits the streams of one project, and `{stream_id}` is \
                         of the project {} while `{}` is of {}",
                        stream_location.project, stream_ids[0], first.project
                    ));
                }
                _ => project = Some(stream_location),
            }
        }
        let project = project.ok_or("a transaction holds at least one operation")?;
        self.project_repositories
            .the_project_repository_at(&project.project_directory)
            .map_err(|refusal| refusal.to_string())
    }

    /// Apply the undo or redo `reversal_of` names for `author` in the project at
    /// `project_directory`: its graph edits to the running streams, then the whole of it to
    /// the project, recorded as a new transaction.
    pub(crate) fn apply_a_reversal(
        &self,
        project_directory: &std::path::Path,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        reversal_of: fn(
            &ProjectRepository,
            &ProjectTransactionAuthor,
        ) -> Result<
            ProjectTransactionReversal,
            streamlib_project_repository::ProjectRepositoryRefusal,
        >,
    ) -> Result<
        streamlib_runtime_client_contract::local_api_wire_contract::ProjectTransactionOnTheWire,
        String,
    > {
        let project_repository = self
            .project_repositories
            .the_project_repository_at(project_directory)
            .map_err(|refusal| refusal.to_string())?;
        let mut project_repository = project_repository.lock();
        let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
            .map_err(|refusal| refusal.to_string())?;
        if let Some(applied_before) = project_repository
            .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key.as_ref())
            .map_err(|refusal| refusal.to_string())?
        {
            let undo_state_of_each_transaction = project_repository
                .the_undo_state_of_each_transaction()
                .map_err(|refusal| refusal.to_string())?;
            return Ok(project_transaction_on_the_wire(
                applied_before,
                &undo_state_of_each_transaction,
            ));
        }
        let reversal =
            reversal_of(&project_repository, author).map_err(|refusal| refusal.to_string())?;
        let project = project_repository.project_file().project;
        let stream_id_of_each_stream_asset =
            self.the_stream_id_of_each_loaded_stream_asset(project);
        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        let mut applied_live: Vec<(String, StreamGraphOperation)> = Vec::new();
        for operation in &reversal.operations {
            let ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) = operation
            else {
                continue;
            };
            let Some(stream_id) = stream_id_of_each_stream_asset.get(stream) else {
                continue;
            };
            let mut applied_to_the_running_stream = || {
                let stream_operations = match streams.get(stream_id) {
                    Some(held) => Arc::clone(&held.stream_operations),
                    None => {
                        let stream_operations = self
                            .operations_on_the_loaded_streams
                            .runtime_operations_of_the_stream_a_call_names(stream_id)
                            .map_err(|refusal| refusal.to_string())?;
                        streams.insert(
                            stream_id.clone(),
                            StreamATransactionEdits {
                                stream_operations: Arc::clone(&stream_operations),
                                stream_location: StreamLocation {
                                    project_directory: project_repository
                                        .project_directory()
                                        .to_path_buf(),
                                    project,
                                    stream_asset: *stream,
                                },
                                staged_graph: StreamGraphDocument::from_value(Value::Object(
                                    Map::new(),
                                ))?,
                            },
                        );
                        stream_operations
                    }
                };
                self.async_runtime
                    .block_on(
                        stream_operations
                            .apply_a_stream_graph_operation_live(graph_operation.clone()),
                    )
                    .map_err(|refusal| refusal.to_string())
                    .map(|applied| (stream_id.clone(), applied))
            };
            let applied = applied_to_the_running_stream();
            match applied {
                Ok(applied) => applied_live.push(applied),
                Err(refusal) => {
                    self.reverse_what_was_applied_live(&streams, applied_live);
                    return Err(refusal);
                }
            }
        }
        match project_repository.apply_project_operations(
            author,
            idempotency_key.as_ref(),
            Vec::new(),
            reversal.operations,
            reversal.lineage,
        ) {
            Ok(outcome) => {
                self.project_repositories
                    .announce_the_transaction_if_applied_now(&project_repository, &outcome);
                let applied = outcome.into_applied_transaction();
                let undo_state_of_each_transaction = project_repository
                    .the_undo_state_of_each_transaction()
                    .map_err(|refusal| refusal.to_string())?;
                let edited_project_directory = project_repository.project_directory().to_path_buf();
                drop(project_repository);
                bring_the_loaded_streams_in_line_with_the_stream_documents_of(
                    self.operations_on_the_loaded_streams,
                    project,
                    &edited_project_directory,
                    &applied.transaction,
                )
                .map_err(|not_in_line| {
                    format!(
                        "the transaction {} applied, and the loaded streams were not brought in \
                         line with it: {not_in_line}",
                        applied.transaction.transaction
                    )
                })?;
                self.reconcile_each_loaded_stream_holding_a_stream_fragment(
                    &edited_project_directory,
                );
                Ok(project_transaction_on_the_wire(
                    applied,
                    &undo_state_of_each_transaction,
                ))
            }
            Err(refusal) => {
                self.reverse_what_was_applied_live(&streams, applied_live);
                Err(refusal.to_string())
            }
        }
    }

    /// Apply the inverse of each operation of `applied_live`, newest first, to the running
    /// stream it was applied to, so a refused transaction leaves the streams as they were.
    fn reverse_what_was_applied_live(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        applied_live: Vec<(String, StreamGraphOperation)>,
    ) {
        for (stream_id, applied) in applied_live.into_iter().rev() {
            let Some(stream) = streams.get(&stream_id) else {
                continue;
            };
            if let Err(not_reversed) = self.async_runtime.block_on(
                stream
                    .stream_operations
                    .apply_a_stream_graph_operation_live(applied.inverse()),
            ) {
                tracing::error!(
                    stream = %stream_id,
                    %not_reversed,
                    "could not reverse part of a refused graph transaction on its running stream"
                );
            }
        }
    }

    /// The answer of `applied_before`, the transaction of the project `project` an action sent
    /// again under its key first applied, each edit of a loaded stream under its stream id.
    fn the_answer_of_a_graph_transaction_applied_before(
        &self,
        project: Uuid,
        applied_before: AppliedProjectTransaction,
    ) -> GraphTransactionToolResult {
        let stream_id_of_each_stream_asset =
            self.the_stream_id_of_each_loaded_stream_asset(project);
        let mut applied_live: Vec<(String, StreamGraphOperation)> = Vec::new();
        for operation in applied_before.transaction.operations {
            let ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) = operation
            else {
                continue;
            };
            let stream_id = stream_id_of_each_stream_asset
                .get(&stream)
                .cloned()
                .unwrap_or_else(|| stream.to_string());
            applied_live.push((stream_id, graph_operation));
        }
        self.the_answer_of_a_graph_transaction(
            GraphEditMode::Commit,
            Some(applied_before.transaction.transaction.to_string()),
            Some(applied_before.commit),
            &|stream_id| {
                self.operations_on_the_loaded_streams
                    .runtime_operations_of_the_stream_a_call_names(stream_id)
                    .ok()
            },
            applied_live,
            &HashMap::new(),
        )
    }

    fn the_answer_of_a_graph_transaction(
        &self,
        mode: GraphEditMode,
        transaction: Option<String>,
        commit: Option<String>,
        stream_operations_of: &dyn Fn(&str) -> Option<Arc<dyn RuntimeOperations>>,
        applied_live: Vec<(String, StreamGraphOperation)>,
        labels_of_added_nodes: &HashMap<Uuid, String>,
    ) -> GraphTransactionToolResult {
        let mut added_nodes = Vec::new();
        let mut added_groups = Vec::new();
        let mut added_notes = Vec::new();
        let mut connected_links = Vec::new();
        let mut graph_render_of_each_stream: HashMap<String, Option<Value>> = HashMap::new();
        let mut operations = Vec::with_capacity(applied_live.len());
        for (stream_id, applied) in applied_live {
            match applied.the_edit_at_its_own_level().1 {
                StreamGraphOperation::AddNode { node, .. } => {
                    added_nodes.push(GraphTransactionAddedNode {
                        stream: stream_id.clone(),
                        node: node.id.to_string(),
                        name: node.name.clone(),
                        label: labels_of_added_nodes.get(&node.id).cloned(),
                    });
                }
                StreamGraphOperation::FoldIntoFragment { fold } => {
                    if let Some(fragment) = fold.fragment_node() {
                        added_nodes.push(GraphTransactionAddedNode {
                            stream: stream_id.clone(),
                            node: fragment.to_string(),
                            name: fold.fragment_name().to_owned(),
                            label: labels_of_added_nodes.get(&fragment).cloned(),
                        });
                    }
                }
                StreamGraphOperation::AddNote { note } => {
                    added_notes.push(GraphTransactionAddedNote {
                        stream: stream_id.clone(),
                        note: note.id.to_string(),
                    });
                }
                StreamGraphOperation::AddGroup { group, .. } => {
                    added_groups.push(GraphTransactionAddedGroup {
                        stream: stream_id.clone(),
                        group: group.id.to_string(),
                        title: group.fields.title.clone(),
                    });
                }
                StreamGraphOperation::Connect { link } => {
                    let graph_render = graph_render_of_each_stream
                        .entry(stream_id.clone())
                        .or_insert_with(|| {
                            let stream_operations = stream_operations_of(&stream_id)?;
                            match self
                                .async_runtime
                                .block_on(stream_operations.to_json_async())
                            {
                                Ok(graph_render) => Some(graph_render),
                                Err(unreadable) => {
                                    tracing::warn!(
                                        stream = %stream_id,
                                        "a link was connected and its stream's graph could not \
                                         be read to say what state it is in: {unreadable}"
                                    );
                                    None
                                }
                            }
                        });
                    let (state, error_reason) = graph_render
                        .as_ref()
                        .map(|graph_render| how_the_graph_reads_the_link(graph_render, link))
                        .unwrap_or_default();
                    connected_links.push(GraphTransactionConnectedLink {
                        stream: stream_id.clone(),
                        source: link_end_on_the_wire(&link.source),
                        target: link_end_on_the_wire(&link.target),
                        state,
                        error_reason,
                    });
                }
                _ => {}
            }
            let mut operation = serde_json::to_value(&applied).unwrap_or_default();
            if let Value::Object(operation) = &mut operation {
                operation.insert("stream".to_owned(), Value::String(stream_id));
            }
            operations.push(operation);
        }
        GraphTransactionToolResult {
            mode: mode.on_the_wire().to_owned(),
            transaction,
            commit,
            operations,
            added_nodes,
            connected_links,
            added_groups,
            added_notes,
        }
    }
}

/// A run of one of a stream's graph generators, `(generator, fragment name, promoted settings)`,
/// answering the fragment node it placed or the refusal.
type GraphGeneratorRunOfTheStream<'stream> =
    dyn Fn(&str, Option<&str>, &Map<String, Value>) -> Result<Value, String> + 'stream;

/// The placement of an imported stream under the fragment node `(fragment node, held stream)`
/// names, `held_stream` its config's `stream`: the fragment it places and the stream's name.
type ImportedStreamPlacement<'repository> =
    dyn Fn(Uuid, &Value) -> Result<(Value, String), String> + 'repository;

/// The family wire name of an output port of the running stream, read from its `graph`.
type FamilyOfAnOutputPortOfTheRunningStream<'stream> =
    dyn Fn(&StreamGraphDocumentLinkEnd) -> Result<String, String> + 'stream;

/// The operation `request` asks of the graph `staged_graph` holds, with everything its inverse
/// needs read from it; an added node takes a new UUID, recorded under its `as` when it has one.
fn graph_operation_for_the_request(
    request: StreamGraphAction,
    staged_graph: &StreamGraphDocument,
    node_by_label: &mut HashMap<String, Uuid>,
    run_the_graph_generator: &GraphGeneratorRunOfTheStream<'_>,
    place_the_imported_stream: &ImportedStreamPlacement<'_>,
    family_of_an_output_port: &FamilyOfAnOutputPortOfTheRunningStream<'_>,
) -> Result<StreamGraphOperation, String> {
    let node_named = |node: &str, node_by_label: &HashMap<String, Uuid>| -> Result<Uuid, String> {
        node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())
            .ok_or_else(|| {
                format!(
                    "`{node}` is neither a node's UUID nor the `as` of a node an earlier \
                     `add_node` of this transaction adds"
                )
            })
    };
    let link_between = |source: &StreamGraphActionLinkEnd,
                        target: &StreamGraphActionLinkEnd,
                        node_by_label: &HashMap<String, Uuid>|
     -> Result<StreamGraphDocumentLink, String> {
        Ok(StreamGraphDocumentLink {
            source: StreamGraphDocumentLinkEnd {
                node: node_named(&source.node, node_by_label)?,
                port: source.port.clone(),
            },
            target: StreamGraphDocumentLinkEnd {
                node: node_named(&target.node, node_by_label)?,
                port: target.port.clone(),
            },
        })
    };
    let label_the_node = |label: Option<String>,
                          node: Uuid,
                          node_by_label: &mut HashMap<String, Uuid>|
     -> Result<(), String> {
        match label {
            Some(label) if node_by_label.insert(label.clone(), node).is_some() => Err(format!(
                "two operations of this transaction both name a node `as` `{label}`"
            )),
            _ => Ok(()),
        }
    };
    match request {
        StreamGraphAction::ImportGraphGenerator {
            generator,
            name,
            config,
            label,
            ..
        } => {
            let placed: StreamGraphFragmentNodeAGeneratorRunPlaced = serde_json::from_value(
                run_the_graph_generator(&generator, name.as_deref(), &config.unwrap_or_default())?,
            )
            .map_err(|unread| {
                format!("graph generator `{generator}`'s run placed no fragment node: {unread}")
            })?;
            let operation = match staged_graph.node_named(&placed.name) {
                None => {
                    let node = Uuid::new_v4();
                    let fragment = fragment_with_each_node_uuid_kept_by_its_name(
                        &placed.fragment,
                        None,
                        &mut Uuid::new_v4,
                    )?;
                    label_the_node(label, node, node_by_label)?;
                    StreamGraphOperation::AddNode {
                        node: StreamGraphDocumentNode {
                            id: node,
                            name: placed.name,
                            node_type: placed.generator,
                            config: placed.config,
                            pins: Map::new(),
                            exposed_config_fields: Vec::new(),
                            studio: Default::default(),
                            fragment: Some(Box::new(fragment)),
                        },
                        links: Vec::new(),
                    }
                }
                Some(StreamGraphDocumentNode {
                    id,
                    node_type,
                    config,
                    fragment: Some(previous_fragment),
                    ..
                }) => {
                    let fragment = fragment_with_each_node_uuid_kept_by_its_name(
                        &placed.fragment,
                        Some(&previous_fragment),
                        &mut Uuid::new_v4,
                    )?;
                    label_the_node(label, id, node_by_label)?;
                    StreamGraphOperation::RefreshFragment {
                        node: id,
                        from: StreamGraphFragmentPlacement {
                            generator: node_type,
                            config,
                            fragment: *previous_fragment,
                        },
                        to: StreamGraphFragmentPlacement {
                            generator: placed.generator,
                            config: placed.config,
                            fragment,
                        },
                    }
                }
                Some(StreamGraphDocumentNode { id, node_type, .. }) => {
                    return Err(format!(
                        "node {id} `{}` is a `{node_type}`, no fragment, so no graph \
                         generator's run refreshes it; give the import another `name`",
                        placed.name
                    ));
                }
            };
            Ok(operation)
        }
        StreamGraphAction::AddNode {
            node_type,
            name,
            config,
            pins,
            exposed_config_fields,
            studio,
            label,
            ..
        } => {
            let node = Uuid::new_v4();
            label_the_node(label, node, node_by_label)?;
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name: name.unwrap_or_default(),
                    node_type,
                    config: Value::Object(config.unwrap_or_default()),
                    pins: pins.unwrap_or_default(),
                    exposed_config_fields: exposed_config_fields.unwrap_or_default(),
                    studio: studio
                        .map(StreamGraphNodeStudioFields::from_a_callers_studio)
                        .transpose()?
                        .map(Box::new)
                        .unwrap_or_default(),
                    fragment: None,
                },
                links: Vec::new(),
            })
        }
        StreamGraphAction::RemoveNode { node, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RemoveNode {
                node: staged_graph.node(node)?,
                links: staged_graph.links_touching(node),
            })
        }
        StreamGraphAction::Connect { source, target, .. } => Ok(StreamGraphOperation::Connect {
            link: link_between(&source, &target, node_by_label)?,
        }),
        StreamGraphAction::Disconnect { source, target, .. } => {
            Ok(StreamGraphOperation::Disconnect {
                link: link_between(&source, &target, node_by_label)?,
            })
        }
        StreamGraphAction::SetNodeConfig { node, config, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetNodeConfig {
                node,
                from: staged_graph.node(node)?.config,
                to: Value::Object(config),
            })
        }
        StreamGraphAction::SetPinValue {
            node, input, value, ..
        } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetPinValue {
                node,
                from: staged_graph.node(node)?.pins.get(&input).cloned(),
                input,
                to: value.filter(|value| !value.is_null()),
            })
        }
        StreamGraphAction::RenameNode { node, name, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::RenameNode {
                node,
                from: staged_graph.node(node)?.name,
                to: name,
            })
        }
        StreamGraphAction::SetStudioFields { node, studio, .. } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetStudioFields {
                node,
                from: staged_graph.node(node)?.studio,
                to: Box::new(StreamGraphNodeStudioFields::from_a_callers_studio(studio)?),
            })
        }
        StreamGraphAction::SetExposedConfigFields {
            node,
            exposed_config_fields,
            ..
        } => {
            let node = node_named(&node, node_by_label)?;
            Ok(StreamGraphOperation::SetExposedConfigFields {
                node,
                from: staged_graph.node(node)?.exposed_config_fields,
                to: exposed_config_fields,
            })
        }
        StreamGraphAction::AddGroup {
            title,
            description,
            colour,
            nodes,
            ..
        } => {
            if title.is_empty() {
                return Err("a group needs a `title`".to_owned());
            }
            let nodes = nodes
                .iter()
                .map(|node| node_named(node, node_by_label))
                .collect::<Result<Vec<Uuid>, String>>()?;
            Ok(StreamGraphOperation::AddGroup {
                group: StreamGraphDocumentGroup {
                    id: Uuid::new_v4(),
                    fields: StreamGraphGroupFields {
                        title,
                        description: description.unwrap_or_default(),
                        colour: colour.unwrap_or_default(),
                    },
                },
                nodes,
            })
        }
        StreamGraphAction::SetGroupFields {
            group,
            title,
            description,
            colour,
            ..
        } => {
            let from = staged_graph.group(the_group_named(&group)?)?;
            if title.as_deref() == Some("") {
                return Err("a group needs a `title`".to_owned());
            }
            let to = StreamGraphGroupFields {
                title: title.unwrap_or_else(|| from.fields.title.clone()),
                description: description.unwrap_or_else(|| from.fields.description.clone()),
                colour: colour.unwrap_or_else(|| from.fields.colour.clone()),
            };
            Ok(StreamGraphOperation::SetGroupFields {
                group: from.id,
                from: from.fields,
                to,
            })
        }
        StreamGraphAction::UseDependencyStream {
            project,
            dependency_stream,
            name,
            label,
            ..
        } => {
            let project = Uuid::parse_str(&project)
                .map_err(|_| format!("`{project}` is no project's UUID"))?;
            let dependency_stream = Uuid::parse_str(&dependency_stream)
                .map_err(|_| format!("`{dependency_stream}` is no stream asset's UUID"))?;
            let config = json!({ STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY: {
                "asset": dependency_stream.to_string(),
                "from": project.to_string(),
            }});
            let node = Uuid::new_v4();
            let (fragment, stream_name) =
                place_the_imported_stream(node, &config[STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY])?;
            let name = name.unwrap_or(stream_name);
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the fragment name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            if let Some(held) = staged_graph.node_named(&name) {
                return Err(
                    if held.node_type == STREAM_FRAGMENT_NODE_TYPE && held.config == config {
                        format!(
                            "node {} `{name}` already places this stream, and follows its import's \
                         folder as it is on disk, so there is nothing to place again",
                            held.id
                        )
                    } else {
                        format!(
                            "node {} `{name}` is a `{}`; give the stream another `name`",
                            held.id, held.node_type
                        )
                    },
                );
            }
            label_the_node(label, node, node_by_label)?;
            Ok(StreamGraphOperation::AddNode {
                node: StreamGraphDocumentNode {
                    id: node,
                    name,
                    node_type: STREAM_FRAGMENT_NODE_TYPE.to_owned(),
                    config,
                    pins: Map::new(),
                    exposed_config_fields: Vec::new(),
                    studio: Default::default(),
                    fragment: Some(Box::new(fragment)),
                },
                links: Vec::new(),
            })
        }
        StreamGraphAction::AddNote {
            text,
            position,
            size,
            ..
        } => Ok(StreamGraphOperation::AddNote {
            note: StreamGraphDocumentNote {
                id: Uuid::new_v4(),
                fields: StreamGraphNoteFields {
                    text,
                    position,
                    size,
                },
            },
        }),
        StreamGraphAction::SetNoteFields {
            note,
            text,
            position,
            size,
            ..
        } => {
            let from = staged_graph.note(the_note_named(&note)?)?;
            let to = StreamGraphNoteFields {
                text: text.unwrap_or_else(|| from.fields.text.clone()),
                position: position.unwrap_or_else(|| from.fields.position.clone()),
                size: size.unwrap_or_else(|| from.fields.size.clone()),
            };
            Ok(StreamGraphOperation::SetNoteFields {
                note: from.id,
                from: from.fields,
                to,
            })
        }
        StreamGraphAction::RemoveNote { note, .. } => Ok(StreamGraphOperation::RemoveNote {
            note: staged_graph.note(the_note_named(&note)?)?,
        }),
        StreamGraphAction::RemoveGroup { group, .. } => {
            let group = staged_graph.group(the_group_named(&group)?)?;
            Ok(StreamGraphOperation::RemoveGroup {
                nodes: staged_graph.nodes_of_the_group(group.id),
                group,
            })
        }
        StreamGraphAction::CollapseSelectionIntoSubgraph {
            nodes, name, label, ..
        } => {
            let nodes = nodes
                .iter()
                .map(|node| node_named(node, node_by_label))
                .collect::<Result<Vec<Uuid>, String>>()?;
            let name = streamlib::sdk::graph::cast_exposed_name_to_url_safe(&name)
                .map_err(|casts_to_nothing| {
                    format!("the subgraph name `{name}` names nothing: {casts_to_nothing}")
                })?
                .into_owned();
            let fragment = Uuid::new_v4();
            let fold = staged_graph.the_fold_of_nodes_into_an_authored_fragment(
                &nodes,
                fragment,
                &name,
                family_of_an_output_port,
                &mut Uuid::new_v4,
            )?;
            label_the_node(label, fragment, node_by_label)?;
            Ok(StreamGraphOperation::FoldIntoFragment { fold })
        }
        StreamGraphAction::UnlockGeneratorFragment { fragment, .. } => {
            Ok(StreamGraphOperation::UnfoldFragment {
                fold: staged_graph
                    .the_fold_a_fragment_unfolds(node_named(&fragment, node_by_label)?)?,
            })
        }
    }
}

/// The family wire name the output port `end` declares in `rendered_graph`, a stream's
/// `graph` render, its node at any level of the fragments the render holds.
fn the_family_of_an_output_port_in(
    rendered_graph: &Value,
    end: &StreamGraphDocumentLinkEnd,
) -> Result<String, String> {
    fn the_rendered_node_at_any_level<'render>(
        rendered_level: &'render Value,
        node: &str,
    ) -> Option<&'render Value> {
        rendered_level["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|rendered_node| {
                if rendered_node["id"] == node {
                    Some(rendered_node)
                } else {
                    the_rendered_node_at_any_level(&rendered_node["fragment"], node)
                }
            })
    }
    let node = end.node.to_string();
    the_rendered_node_at_any_level(rendered_graph, &node)
        .and_then(|rendered_node| {
            rendered_node["ports"]["outputs"]
                .as_array()?
                .iter()
                .find(|port| port["name"] == end.port.as_str())
        })
        .and_then(|port| port["family"]["family"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "the running stream declares no family for the output port {node}/{}, so the \
                 subgraph port carrying it cannot be declared",
                end.port
            )
        })
}

/// The fragment the stream fragment node `fragment_node` places for `held_stream`,
/// `{"asset", "from"}`, in a stream of the project at `project_directory`, and the name the
/// imported stream's file gives it.
fn the_fragment_an_imported_stream_places(
    project_directory: &std::path::Path,
    fragment_node: Uuid,
    held_stream: &Value,
) -> Result<(Value, String), String> {
    let from = held_stream["from"]
        .as_str()
        .and_then(|from| Uuid::parse_str(from).ok())
        .ok_or("an imported stream names the project it is imported from under `from`")?;
    let stream_asset = held_stream["asset"]
        .as_str()
        .and_then(|asset| Uuid::parse_str(asset).ok())
        .ok_or("an imported stream names its stream asset under `asset`")?;
    let (_, stream_name) = the_stream_document_of_an_import(project_directory, from, stream_asset)?;
    let fragment =
        the_fragment_a_stream_fragment_places(project_directory, fragment_node, held_stream)?;
    Ok((fragment, stream_name))
}

fn the_note_named(note: &str) -> Result<Uuid, String> {
    Uuid::parse_str(note)
        .map_err(|_| format!("`{note}` is no note's UUID; `graph` lists each note's under `notes`"))
}

fn the_group_named(group: &str) -> Result<Uuid, String> {
    Uuid::parse_str(group).map_err(|_| {
        format!("`{group}` is no group's UUID; `graph` lists each group's under `groups`")
    })
}

/// The module a project's Python file at `changed_file` is imported as: its path from the
/// project directory, `/` read as `.`, a package's `__init__.py` as the package; `None` for a
/// file that is no `.py` or lies outside the project.
fn the_module_a_project_file_is_imported_as(
    project_directory: &std::path::Path,
    changed_file: &std::path::Path,
) -> Option<String> {
    let relative_file = if changed_file.is_absolute() {
        changed_file.strip_prefix(project_directory).ok()?
    } else {
        changed_file
    };
    if relative_file.extension()? != "py" {
        return None;
    }
    let mut module_parts: Vec<String> = relative_file
        .with_extension("")
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part) => part.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if module_parts.last().is_some_and(|last| last == "__init__") {
        module_parts.pop();
    }
    (!module_parts.is_empty()).then(|| module_parts.join("."))
}

/// An `import_graph_generator` for each top-level fragment of each loaded stream of the project
/// at `project_directory` whose generator, or one nested in it, a module of `changed_files`
/// defines, run with the promoted settings the fragment keeps.
pub(crate) fn the_refresh_of_each_fragment_a_changed_module_defines(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project_repositories: &ProjectRepositoriesOpenInThisRuntime,
    project_directory: &std::path::Path,
    changed_files: &[PathBuf],
) -> Result<Vec<StreamGraphAction>, String> {
    let changed_modules: std::collections::HashSet<String> = changed_files
        .iter()
        .filter_map(|changed_file| {
            the_module_a_project_file_is_imported_as(project_directory, changed_file)
        })
        .collect();
    if changed_modules.is_empty() {
        return Ok(Vec::new());
    }
    let project_repository = project_repositories
        .the_project_repository_at(project_directory)
        .map_err(|refusal| refusal.to_string())?;
    let project_repository = project_repository.lock();
    let project = project_repository.project_file().project;
    let mut refreshes = Vec::new();
    for listing in operations_on_the_loaded_streams.list_streams() {
        let Some(stream_location) = listing.stream_location.filter(|stream_location| {
            stream_location.project == project && listing.node_count.is_some()
        }) else {
            continue;
        };
        let staged_graph = project_repository
            .stream_graph_document(stream_location.stream_asset)
            .map_err(|refusal| refusal.to_string())?;
        for fragment_node in staged_graph.fragment_nodes() {
            if fragment_node.node_type == STREAM_FRAGMENT_NODE_TYPE
                || fragment_node.node_type == AUTHORED_FRAGMENT_NODE_TYPE
            {
                continue;
            }
            let defined_in_a_changed_module = fragment_node
                .graph_generators_a_refresh_runs()
                .iter()
                .any(|generator| {
                    generator
                        .split_once(':')
                        .is_some_and(|(module, _)| changed_modules.contains(module))
                });
            if !defined_in_a_changed_module {
                continue;
            }
            refreshes.push(StreamGraphAction::ImportGraphGenerator {
                stream: stream_location.stream_asset,
                generator: fragment_node.node_type,
                name: Some(fragment_node.name),
                config: match fragment_node.config {
                    Value::Object(promoted_settings) => Some(promoted_settings),
                    _ => None,
                },
                label: None,
            });
        }
    }
    Ok(refreshes)
}

/// The fragment node one graph generator run places, as the run entry prints it: its nodes
/// and links by name.
#[derive(Deserialize)]
struct StreamGraphFragmentNodeAGeneratorRunPlaced {
    name: String,
    #[serde(rename = "type")]
    generator: String,
    config: Value,
    fragment: Value,
}

fn link_end_on_the_wire(end: &StreamGraphDocumentLinkEnd) -> GraphLinkEndOnTheWire {
    GraphLinkEndOnTheWire {
        node: end.node.to_string(),
        port: end.port.clone(),
    }
}

/// The `state` and `error_reason` the `graph` render `graph_render` gives `link`, whose ends it
/// names by node name.
fn how_the_graph_reads_the_link(
    graph_render: &Value,
    link: &StreamGraphDocumentLink,
) -> (Option<String>, Option<String>) {
    let node_name_of = |node: Uuid| {
        graph_render["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|rendered| rendered["id"].as_str() == Some(node.to_string().as_str()))
            .and_then(|rendered| rendered["name"].as_str())
            .map(str::to_owned)
    };
    let (Some(source_name), Some(target_name)) = (
        node_name_of(link.source.node),
        node_name_of(link.target.node),
    ) else {
        return (None, None);
    };
    let end_is = |end: &Value, name: &str, port: &str| {
        end["node"].as_str() == Some(name) && end["port"].as_str() == Some(port)
    };
    let Some(rendered_link) =
        graph_render["links"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|rendered| {
                end_is(&rendered["source"], &source_name, &link.source.port)
                    && end_is(&rendered["target"], &target_name, &link.target.port)
            })
    else {
        return (None, None);
    };
    let text_at = |key: &str| rendered_link[key].as_str().map(str::to_owned);
    (text_at("state"), text_at("error_reason"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::the_module_a_project_file_is_imported_as;

    #[test]
    fn a_project_file_is_imported_as_its_dotted_path_and_a_package_as_its_folder() {
        let project_directory = Path::new("/rig");
        for (changed_file, module) in [
            ("generators/looks.py", Some("generators.looks")),
            ("/rig/generators/looks.py", Some("generators.looks")),
            ("generators/__init__.py", Some("generators")),
            ("stream.py", Some("stream")),
            ("streams/main.graph.json", None),
            ("/elsewhere/looks.py", None),
        ] {
            assert_eq!(
                the_module_a_project_file_is_imported_as(
                    project_directory,
                    Path::new(changed_file)
                )
                .as_deref(),
                module,
                "{changed_file}"
            );
        }
    }
}
