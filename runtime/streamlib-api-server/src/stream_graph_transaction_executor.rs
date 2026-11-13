// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one executor of a stream graph transaction: each operation staged, applied whole or
//! refused whole, written to the graph documents and committed to the project's history, or
//! applied live to the running streams alone.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations, StreamLocation,
};
use streamlib_control_contract::GraphEditMode;
use streamlib_project_repository::{
    AppliedProjectTransaction, GRAPH_DOCUMENT_FILE_SUFFIX, PREVIOUS_VALUE_NODE_INPUT_PORT,
    PREVIOUS_VALUE_NODE_OUTPUT_PORT, PREVIOUS_VALUE_NODE_TYPE, ProjectAction,
    ProjectActionIdempotencyKey, ProjectAssetKind, ProjectOperation, ProjectRelativeAssetPath,
    ProjectRepositoriesOpenInThisRuntime, ProjectRepository, ProjectRepositoryRefusal,
    ProjectStreamDocumentOperation, ProjectTransactionAuthor, ProjectTransactionLineage,
    ProjectTransactionReversal, STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE,
    StreamGraphAction, StreamGraphActionLinkEnd, StreamGraphDocument, StreamGraphDocumentGroup,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphDocumentNote, StreamGraphFragmentPlacement, StreamGraphGroupFields,
    StreamGraphLevelOfAPlacedStream, StreamGraphNodeStudioFields, StreamGraphNoteFields,
    StreamGraphOperation, StreamGraphOperationOnAStream,
    asset_references_made_references_into_the_import,
    fragment_with_each_node_uuid_kept_by_its_name, graph_document_holds_a_stream_fragment,
    is_a_graph_interface_node_type, the_fragment_a_stream_fragment_places,
    the_stream_document_of_an_import,
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
        StreamGraphAction::UnlockSubgraph { fragment, .. } => names_a_node(fragment),
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

/// One graph action applied: the operation its answer reports, as the stream it names draws it;
/// what it made on that running stream; the project operations it records, in order; and each
/// operation a loaded stream takes once the project's documents hold it.
struct OneGraphRequestApplied {
    operation_answered: StreamGraphOperation,
    applied_live: Option<StreamGraphOperation>,
    project_operations: Vec<ProjectOperation>,
    made_live_once_the_documents_hold_them: Vec<(String, StreamGraphOperation)>,
}

/// The stream a collapse saves: its stream asset, its graph document, and the operation
/// writing that document into the project.
struct PlacedStreamACollapseSaves {
    placed_stream: Uuid,
    document: StreamGraphDocument,
    creation: ProjectOperation,
}

/// The graph document of the stream asset `stream` as the transaction has staged it: a loaded
/// stream's, one staged for a stream not running, or the project's own.
fn the_staged_graph_of_the_stream(
    stream: Uuid,
    streams: &BTreeMap<String, StreamATransactionEdits>,
    staged_graph_of_each_stream_not_running: &BTreeMap<Uuid, StreamGraphDocument>,
    project_repository: &ProjectRepository,
) -> Result<StreamGraphDocument, String> {
    if let Some(held) = streams
        .values()
        .find(|held| held.stream_location.stream_asset == stream)
    {
        return Ok(held.staged_graph.clone());
    }
    if let Some(staged) = staged_graph_of_each_stream_not_running.get(&stream) {
        return Ok(staged.clone());
    }
    project_repository
        .stream_graph_document(stream)
        .map_err(|refusal| refusal.to_string())
}

/// The graph document of the stream a stream fragment's `held_stream`, its config's `stream`,
/// places: one of the project as the transaction has staged it, or one of an import as its
/// folder holds it, each asset it references made a reference into that import.
fn the_document_a_placed_stream_holds(
    held_stream: &Value,
    streams: &BTreeMap<String, StreamATransactionEdits>,
    staged_graph_of_each_stream_not_running: &BTreeMap<Uuid, StreamGraphDocument>,
    project_repository: &ProjectRepository,
) -> Result<StreamGraphDocument, String> {
    let stream = held_stream["asset"]
        .as_str()
        .and_then(|asset| Uuid::parse_str(asset).ok())
        .ok_or("a placed stream names its stream asset under `asset`")?;
    match held_stream.get("from") {
        None => the_staged_graph_of_the_stream(
            stream,
            streams,
            staged_graph_of_each_stream_not_running,
            project_repository,
        ),
        Some(from) => {
            let from = from
                .as_str()
                .and_then(|from| Uuid::parse_str(from).ok())
                .ok_or("a placed stream's `from` is no project's UUID")?;
            let (mut document, _) = the_stream_document_of_an_import(
                project_repository.project_directory(),
                from,
                stream,
            )?;
            asset_references_made_references_into_the_import(&mut document, from);
            StreamGraphDocument::from_value(document)
        }
    }
}

/// The type, name and config of the node `edit` adds to `document` or configures there; none
/// for a subgraph node or a Graph Input or Output, whose types name no processor.
fn the_processor_node_an_edit_adds_or_configures(
    edit: &StreamGraphOperation,
    document: &StreamGraphDocument,
) -> Option<(String, String, Value)> {
    let (node_type, node_name, config, holds_a_fragment) = match edit {
        StreamGraphOperation::AddNode { node, .. } => (
            node.node_type.clone(),
            node.name.clone(),
            node.config.clone(),
            node.fragment.is_some(),
        ),
        StreamGraphOperation::SetNodeConfig { node, to, .. } => {
            let configured = document.node(*node).ok()?;
            (
                configured.node_type,
                configured.name,
                to.clone(),
                configured.fragment.is_some(),
            )
        }
        _ => return None,
    };
    let names_a_processor = !holds_a_fragment
        && node_type != STREAM_FRAGMENT_NODE_TYPE
        && !is_a_graph_interface_node_type(&node_type);
    names_a_processor.then_some((node_type, node_name, config))
}

/// Record `label` against the node `operation` adds, under the UUID `drawn_uuid_of` gives it.
fn label_what_the_operation_adds(
    label: Option<String>,
    operation: &StreamGraphOperation,
    drawn_uuid_of: &dyn Fn(Uuid) -> Uuid,
    labels_of_added_nodes: &mut HashMap<Uuid, String>,
) {
    let added_node = match operation {
        StreamGraphOperation::AddNode { node, .. } => Some(node.id),
        StreamGraphOperation::FoldIntoFragment { fold } => fold.fragment_node(),
        _ => None,
    };
    if let (Some(label), Some(added_node)) = (label, added_node) {
        labels_of_added_nodes.insert(drawn_uuid_of(added_node), label);
    }
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
    /// keeps its live graph as it was, and the refusal is logged. The streams the edited project
    /// runs as its own are then brought in line with what its streams place.
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
        self.operations_on_the_loaded_streams
            .bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
                edited_project_directory,
            );
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
        let mut operations_answered: Vec<(String, StreamGraphOperation)> = Vec::new();
        let mut project_operations: Vec<ProjectOperation> = Vec::new();
        let mut made_live_once_the_documents_hold_them: Vec<(String, StreamGraphOperation)> =
            Vec::new();
        let mut staged_graph_of_each_stream_not_running: BTreeMap<Uuid, StreamGraphDocument> =
            BTreeMap::new();
        let mut streams_taking_their_edits_once_the_documents_hold_them: BTreeSet<String> =
            BTreeSet::new();
        let project_repository_read_during_the_requests = project_repository_held.as_deref();
        for (request_index, (request, stream_id)) in requests_and_the_stream_id_each_names
            .into_iter()
            .enumerate()
        {
            let what_the_request_names = what_a_graph_action_names(&request);
            let applied = self.apply_one_request(
                request,
                &stream_id,
                &mut streams,
                &mut staged_graph_of_each_stream_not_running,
                &mut streams_taking_their_edits_once_the_documents_hold_them,
                project_repository_read_during_the_requests,
                &mut node_by_label,
                &mut labels_of_added_nodes,
            );
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
            operations_answered.push((stream_id.clone(), applied.operation_answered));
            project_operations.extend(applied.project_operations);
            made_live_once_the_documents_hold_them
                .extend(applied.made_live_once_the_documents_hold_them);
            if let Some(applied_live_now) = applied.applied_live {
                applied_live.push((stream_id, applied_live_now));
            }
        }

        let (transaction, commit) = match project_repository_held {
            None => (None, None),
            Some(project_repository) => {
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
                        self.make_live_what_the_documents_now_hold(
                            made_live_once_the_documents_hold_them,
                        );
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
                    .or_else(|| {
                        self.operations_on_the_loaded_streams
                            .runtime_operations_of_the_stream_a_call_names(stream_id)
                            .ok()
                    })
            },
            operations_answered,
            &labels_of_added_nodes,
        ))
    }

    /// Apply `request`, an action on the loaded stream `stream_id`: an edit of its own level
    /// made on its running stream and staged on its document, and one that reaches a running
    /// stream only from the documents — inside a placed stream, or placing one — staged on the
    /// documents it changes, each loaded stream it changes taking it once they are written. A
    /// stream of `streams_taking_their_edits_once_the_documents_hold_them` takes each later edit
    /// that way too, so its running stream takes the transaction's edits in order.
    #[allow(clippy::too_many_arguments)]
    fn apply_one_request(
        &self,
        request: StreamGraphAction,
        stream_id: &str,
        streams: &mut BTreeMap<String, StreamATransactionEdits>,
        staged_graph_of_each_stream_not_running: &mut BTreeMap<Uuid, StreamGraphDocument>,
        streams_taking_their_edits_once_the_documents_hold_them: &mut BTreeSet<String>,
        project_repository: Option<&ProjectRepository>,
        node_by_label: &mut HashMap<String, Uuid>,
        labels_of_added_nodes: &mut HashMap<Uuid, String>,
    ) -> Result<OneGraphRequestApplied, String> {
        let label = match &request {
            StreamGraphAction::AddNode { label, .. }
            | StreamGraphAction::ImportGraphGenerator { label, .. }
            | StreamGraphAction::UseDependencyStream { label, .. }
            | StreamGraphAction::CollapseSelectionIntoSubgraph { label, .. } => label.clone(),
            _ => None,
        };
        let level = request.level().to_vec();
        let documents_first =
            || "it is made on the project's graph documents, with `mode` `commit`".to_owned();
        let stream = streams
            .get(stream_id)
            .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
        let stream_operations = Arc::clone(&stream.stream_operations);
        let stream_asset = stream.stream_location.stream_asset;
        let reached = stream
            .staged_graph
            .clone()
            .the_level_a_path_of_placed_streams_reaches(
                stream_asset,
                &level,
                &mut |placed_stream| {
                    let project_repository = project_repository.ok_or_else(|| {
                        format!(
                            "an edit inside a placed stream is refused live: {}",
                            documents_first()
                        )
                    })?;
                    the_staged_graph_of_the_stream(
                        placed_stream,
                        streams,
                        staged_graph_of_each_stream_not_running,
                        project_repository,
                    )
                },
            )?;
        let level_as_drawn = reached.the_document_as_drawn()?;

        let run_the_graph_generator =
            |generator: &str,
             fragment_name: Option<&str>,
             promoted_settings: &Map<String, Value>| {
                self.operations_on_the_loaded_streams
                    .run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
                        stream_id,
                        generator,
                        fragment_name,
                        promoted_settings,
                    )
                    .map_err(|refusal| refusal.to_string())
            };
        let place_the_imported_stream = |fragment_node: Uuid, held_stream: &Value| {
            let project_repository = project_repository.ok_or_else(|| {
                "an imported stream is placed from the project's `imports/` folder; place it \
                 with `mode` `commit`"
                    .to_owned()
            })?;
            the_fragment_an_imported_stream_places(
                project_repository.project_directory(),
                fragment_node,
                held_stream,
            )
        };
        let document_of_a_placed_stream = |held_stream: &Value| {
            let project_repository = project_repository.ok_or_else(|| {
                format!(
                    "a placed stream is unlocked from its document: {}",
                    documents_first()
                )
            })?;
            the_document_a_placed_stream_holds(
                held_stream,
                streams,
                staged_graph_of_each_stream_not_running,
                project_repository,
            )
        };
        let family_of_an_output_port = |end: &StreamGraphDocumentLinkEnd| {
            let rendered_graph = self
                .async_runtime
                .block_on(stream_operations.to_json_async())
                .map_err(|refusal| refusal.to_string())?;
            the_family_of_an_output_port_in(&rendered_graph, end)
        };
        let function_nodes_of_the_running_stream = || {
            let rendered_graph = self
                .async_runtime
                .block_on(stream_operations.to_json_async())
                .map_err(|refusal| refusal.to_string())?;
            let mut function_nodes = HashSet::new();
            the_function_nodes_rendered_in(&rendered_graph, &mut function_nodes);
            Ok(function_nodes)
        };
        let mut placed_stream_a_collapse_saves = None;
        let mut edit_as_drawn = graph_operation_for_the_request(
            request,
            &level_as_drawn,
            node_by_label,
            &WhatAGraphRequestReadsOfItsStreamAndProject {
                run_the_graph_generator: &run_the_graph_generator,
                place_the_imported_stream: &place_the_imported_stream,
                document_of_a_placed_stream: &document_of_a_placed_stream,
                family_of_an_output_port: &family_of_an_output_port,
                function_nodes_of_the_running_stream: &function_nodes_of_the_running_stream,
            },
            &mut placed_stream_a_collapse_saves,
        )?;
        let made_on_the_running_stream_now = reached.is_the_streams_own_level()
            && !edit_as_drawn.reaches_a_running_stream_once_the_documents_hold_it()
            && !streams_taking_their_edits_once_the_documents_hold_them.contains(stream_id);
        if let StreamGraphOperation::AddNode { node, .. } = &mut edit_as_drawn
            && node.name.is_empty()
            && !made_on_the_running_stream_now
        {
            let names_at_the_level: std::collections::HashSet<String> = level_as_drawn
                .nodes()
                .into_iter()
                .map(|held| held.name)
                .collect();
            node.name = stream_operations
                .the_name_a_node_added_with_none_takes(&node.node_type, &names_at_the_level)
                .map_err(|refusal| refusal.to_string())?;
        }

        let mut project_operations = Vec::new();
        if let Some(collapsed) = placed_stream_a_collapse_saves {
            project_operations.push(collapsed.creation);
            staged_graph_of_each_stream_not_running
                .insert(collapsed.placed_stream, collapsed.document);
        }
        if made_on_the_running_stream_now {
            let applied = self
                .async_runtime
                .block_on(stream_operations.apply_a_stream_graph_operation_live(edit_as_drawn))
                .map_err(|refusal| refusal.to_string())?;
            let stream = streams
                .get_mut(stream_id)
                .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
            if let Err(why) = stream.staged_graph.apply(&applied) {
                if let Err(not_reversed) = self.async_runtime.block_on(
                    stream_operations.apply_a_stream_graph_operation_live(applied.inverse()),
                ) {
                    tracing::error!(
                        stream = %stream_id,
                        %not_reversed,
                        "could not reverse part of a refused graph transaction on its running \
                         stream"
                    );
                }
                return Err(format!(
                    "stream `{stream_id}`'s graph document does not hold what its running graph \
                     does: {why}"
                ));
            }
            label_what_the_operation_adds(label, &applied, &|own| own, labels_of_added_nodes);
            project_operations.push(ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: stream_asset,
                    graph_operation: applied.clone(),
                },
            ));
            return Ok(OneGraphRequestApplied {
                operation_answered: applied.clone(),
                applied_live: Some(applied),
                project_operations,
                made_live_once_the_documents_hold_them: Vec::new(),
            });
        }

        if project_repository.is_none() {
            return Err(format!(
                "the edit places a stream, or is made inside one, and is refused live: {}",
                documents_first()
            ));
        }
        let edit = reached.the_operation_on_the_document(edit_as_drawn)?;
        if let Some((node_type, node_name, config)) =
            the_processor_node_an_edit_adds_or_configures(&edit, &reached.document)
        {
            self.async_runtime
                .block_on(
                    stream_operations
                        .refuse_a_node_a_live_add_would_refuse(&node_type, &node_name, &config),
                )
                .map_err(|refusal| refusal.to_string())?;
        }
        let mut placed_document_after_the_edit = reached.document.clone();
        placed_document_after_the_edit.apply(&edit)?;
        let operation_answered =
            reached.the_operation_as_drawn(&edit, &placed_document_after_the_edit)?;
        label_what_the_operation_adds(
            label,
            &edit,
            &|own| reached.the_drawn_uuid_of(own),
            labels_of_added_nodes,
        );
        for labelled in node_by_label.values_mut() {
            if edit.node() == Some(*labelled) && !reached.is_the_streams_own_level() {
                *labelled = reached.the_drawn_uuid_of(*labelled);
            }
        }
        let mut made_live_once_the_documents_hold_them = Vec::new();
        if reached.is_the_streams_own_level() {
            made_live_once_the_documents_hold_them.push((stream_id.to_owned(), edit.clone()));
            streams_taking_their_edits_once_the_documents_hold_them.insert(stream_id.to_owned());
        }
        project_operations.push(ProjectOperation::StreamGraph(
            StreamGraphOperationOnAStream {
                stream: reached.stream,
                graph_operation: edit,
            },
        ));
        if let Some(project_repository) = project_repository {
            for (placing_stream, carry) in self.the_links_carried_in_every_stream_placing(
                &reached,
                &placed_document_after_the_edit,
                streams,
                staged_graph_of_each_stream_not_running,
                project_repository,
            )? {
                if let Some((placing_stream_id, _)) = streams
                    .iter()
                    .find(|(_, held)| held.stream_location.stream_asset == placing_stream)
                {
                    made_live_once_the_documents_hold_them
                        .push((placing_stream_id.clone(), carry.clone()));
                    streams_taking_their_edits_once_the_documents_hold_them
                        .insert(placing_stream_id.clone());
                }
                project_operations.push(ProjectOperation::StreamGraph(
                    StreamGraphOperationOnAStream {
                        stream: placing_stream,
                        graph_operation: carry,
                    },
                ));
            }
        }
        match streams
            .values_mut()
            .find(|held| held.stream_location.stream_asset == reached.stream)
        {
            Some(held) => held.staged_graph = placed_document_after_the_edit,
            None => {
                staged_graph_of_each_stream_not_running
                    .insert(reached.stream, placed_document_after_the_edit);
            }
        }
        Ok(OneGraphRequestApplied {
            operation_answered,
            applied_live: None,
            project_operations,
            made_live_once_the_documents_hold_them,
        })
    }

    /// Each stream of the project placing the stream `reached` is the level of, by its stream
    /// asset, with the operation carrying its links onto that stream's ports across the edit that
    /// leaves its document as `placed_document_after_the_edit`, each staged on the placing
    /// stream's document; a stream the transaction saves is among them.
    fn the_links_carried_in_every_stream_placing(
        &self,
        reached: &StreamGraphLevelOfAPlacedStream,
        placed_document_after_the_edit: &StreamGraphDocument,
        streams: &mut BTreeMap<String, StreamATransactionEdits>,
        staged_graph_of_each_stream_not_running: &mut BTreeMap<Uuid, StreamGraphDocument>,
        project_repository: &ProjectRepository,
    ) -> Result<Vec<(Uuid, StreamGraphOperation)>, String> {
        if reached.is_the_streams_own_level() {
            return Ok(Vec::new());
        }
        let mut stream_assets_of_the_project: BTreeSet<Uuid> = project_repository
            .assets()
            .map_err(|refusal| refusal.to_string())?
            .into_iter()
            .filter(|asset| asset.kind == ProjectAssetKind::Stream)
            .map(|asset| asset.asset)
            .collect();
        stream_assets_of_the_project.extend(staged_graph_of_each_stream_not_running.keys());
        let mut carried = Vec::new();
        for placing_stream in stream_assets_of_the_project {
            if placing_stream == reached.stream {
                continue;
            }
            let mut placing_document = the_staged_graph_of_the_stream(
                placing_stream,
                streams,
                staged_graph_of_each_stream_not_running,
                project_repository,
            )?;
            let Some(carry) = placing_document
                .the_links_carried_onto_the_ports_of_a_placed_stream(
                    reached.stream,
                    &reached.document,
                    placed_document_after_the_edit,
                )?
            else {
                continue;
            };
            placing_document.apply(&carry)?;
            match streams
                .values_mut()
                .find(|held| held.stream_location.stream_asset == placing_stream)
            {
                Some(held) => held.staged_graph = placing_document,
                None => {
                    staged_graph_of_each_stream_not_running
                        .insert(placing_stream, placing_document);
                }
            }
            carried.push((placing_stream, carry));
        }
        Ok(carried)
    }

    /// Make each of `operations`, each by the loaded stream it changes, on that running stream,
    /// once the project's graph documents hold it; a running stream that refuses one is logged
    /// and takes it at its next load.
    fn make_live_what_the_documents_now_hold(
        &self,
        operations: Vec<(String, StreamGraphOperation)>,
    ) {
        for (stream_id, operation) in operations {
            let made_live = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(&stream_id)
                .and_then(|stream_operations| {
                    self.async_runtime
                        .block_on(stream_operations.apply_a_stream_graph_operation_live(operation))
                });
            if let Err(refusal) = made_live {
                tracing::warn!(
                    stream = %stream_id,
                    %refusal,
                    "the project's graph documents hold an edit its running stream refused; the \
                     stream takes it at its next load"
                );
            }
        }
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
        let mut made_live_once_the_documents_hold_them: Vec<(String, StreamGraphOperation)> =
            Vec::new();
        let mut streams_taking_their_operations_once_the_documents_hold_them: BTreeSet<String> =
            BTreeSet::new();
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
            if graph_operation.reaches_a_running_stream_once_the_documents_hold_it()
                || streams_taking_their_operations_once_the_documents_hold_them.contains(stream_id)
            {
                made_live_once_the_documents_hold_them
                    .push((stream_id.clone(), graph_operation.clone()));
                streams_taking_their_operations_once_the_documents_hold_them
                    .insert(stream_id.clone());
                continue;
            }
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
                self.make_live_what_the_documents_now_hold(made_live_once_the_documents_hold_them);
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
            match &applied {
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

/// The graph document of the stream a stream fragment places, from its config's `stream`.
type DocumentOfAPlacedStream<'repository> =
    dyn Fn(&Value) -> Result<StreamGraphDocument, String> + 'repository;

/// The family wire name of an output port of the running stream, read from its `graph`.
type FamilyOfAnOutputPortOfTheRunningStream<'stream> =
    dyn Fn(&StreamGraphDocumentLinkEnd) -> Result<String, String> + 'stream;

/// The UUID of each node the running stream's `graph` renders as a function node.
type FunctionNodesOfTheRunningStream<'stream> = dyn Fn() -> Result<HashSet<Uuid>, String> + 'stream;

/// What building a graph request's operation reads of the running stream and its project.
struct WhatAGraphRequestReadsOfItsStreamAndProject<'reads> {
    run_the_graph_generator: &'reads GraphGeneratorRunOfTheStream<'reads>,
    place_the_imported_stream: &'reads ImportedStreamPlacement<'reads>,
    document_of_a_placed_stream: &'reads DocumentOfAPlacedStream<'reads>,
    family_of_an_output_port: &'reads FamilyOfAnOutputPortOfTheRunningStream<'reads>,
    function_nodes_of_the_running_stream: &'reads FunctionNodesOfTheRunningStream<'reads>,
}

/// The operation `request` asks of the graph `staged_graph` holds, with everything its inverse
/// needs read from it; an added node takes a new UUID, recorded under its `as` when it has one.
fn graph_operation_for_the_request(
    request: StreamGraphAction,
    staged_graph: &StreamGraphDocument,
    node_by_label: &mut HashMap<String, Uuid>,
    what_the_request_reads: &WhatAGraphRequestReadsOfItsStreamAndProject<'_>,
    placed_stream_a_collapse_saves: &mut Option<PlacedStreamACollapseSaves>,
) -> Result<StreamGraphOperation, String> {
    let WhatAGraphRequestReadsOfItsStreamAndProject {
        run_the_graph_generator,
        place_the_imported_stream,
        document_of_a_placed_stream,
        family_of_an_output_port,
        function_nodes_of_the_running_stream,
    } = what_the_request_reads;
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
        StreamGraphAction::Connect { source, target, .. } => {
            let link = link_between(&source, &target, node_by_label)?;
            if the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
                staged_graph,
                &link,
                function_nodes_of_the_running_stream,
            )? {
                Ok(the_previous_value_node_added_on_the_link(
                    staged_graph,
                    link,
                ))
            } else {
                Ok(StreamGraphOperation::Connect { link })
            }
        }
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
            let placed_stream = Uuid::new_v4();
            let collapsed = staged_graph.the_collapse_of_nodes_into_a_placed_stream(
                &nodes,
                fragment,
                &name,
                placed_stream,
                family_of_an_output_port,
                &mut Uuid::new_v4,
            )?;
            let path = ProjectRelativeAssetPath::parse(&format!(
                "{}/{name}{GRAPH_DOCUMENT_FILE_SUFFIX}",
                ProjectAssetKind::Stream.standard_folder_in_a_project()
            ))
            .map_err(|refusal| refusal.to_string())?;
            *placed_stream_a_collapse_saves = Some(PlacedStreamACollapseSaves {
                placed_stream,
                document: StreamGraphDocument::from_value(
                    collapsed.placed_stream_document.clone(),
                )?,
                creation: ProjectOperation::StreamDocument(
                    ProjectStreamDocumentOperation::CreateStreamDocument {
                        stream: placed_stream,
                        path,
                        document: collapsed.placed_stream_document,
                    },
                ),
            });
            label_the_node(label, fragment, node_by_label)?;
            Ok(StreamGraphOperation::FoldIntoFragment {
                fold: collapsed.fold,
            })
        }
        StreamGraphAction::UnlockSubgraph { fragment, .. } => {
            Ok(StreamGraphOperation::UnfoldFragment {
                fold: staged_graph.the_fold_a_fragment_unfolds(
                    node_named(&fragment, node_by_label)?,
                    document_of_a_placed_stream,
                    &mut Uuid::new_v4,
                )?,
            })
        }
    }
}

/// Whether `link`, once `staged_graph` holds it, closes a loop every node of which is a
/// function node and none a `PreviousValue` node: the loop the compiler refuses.
fn the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
    staged_graph: &StreamGraphDocument,
    link: &StreamGraphDocumentLink,
    function_nodes_of_the_running_stream: &FunctionNodesOfTheRunningStream<'_>,
) -> Result<bool, String> {
    let links = staged_graph.links();
    let nodes_reached_from = |node: Uuid, may_pass_through: &dyn Fn(Uuid) -> bool| {
        let mut reached = HashSet::from([node]);
        let mut nodes_to_walk_from = vec![node];
        while let Some(walked_from) = nodes_to_walk_from.pop() {
            for held in &links {
                if held.source.node == walked_from
                    && may_pass_through(held.target.node)
                    && reached.insert(held.target.node)
                {
                    nodes_to_walk_from.push(held.target.node);
                }
            }
        }
        reached
    };
    if !nodes_reached_from(link.target.node, &|_| true).contains(&link.source.node) {
        return Ok(false);
    }
    let previous_value_nodes: HashSet<Uuid> = staged_graph
        .nodes()
        .into_iter()
        .filter(|node| node.node_type == PREVIOUS_VALUE_NODE_TYPE)
        .map(|node| node.id)
        .collect();
    let function_nodes = function_nodes_of_the_running_stream()?;
    let may_be_in_the_loop =
        |node: Uuid| function_nodes.contains(&node) && !previous_value_nodes.contains(&node);
    Ok(may_be_in_the_loop(link.target.node)
        && nodes_reached_from(link.target.node, &may_be_in_the_loop).contains(&link.source.node))
}

/// The addition of a `PreviousValue` node to `staged_graph` with `link` carried through it,
/// drawn halfway between the two nodes `link` joins when the canvas places both.
fn the_previous_value_node_added_on_the_link(
    staged_graph: &StreamGraphDocument,
    link: StreamGraphDocumentLink,
) -> StreamGraphOperation {
    let canvas_position_of = |node: Uuid| {
        let [x, y] = staged_graph.node(node).ok()?.studio.position?;
        Some((x.as_f64()?, y.as_f64()?))
    };
    let position_halfway_along_the_link = canvas_position_of(link.source.node)
        .zip(canvas_position_of(link.target.node))
        .and_then(|((source_x, source_y), (target_x, target_y))| {
            Some([
                serde_json::Number::from_f64((source_x + target_x) / 2.0)?,
                serde_json::Number::from_f64((source_y + target_y) / 2.0)?,
            ])
        });
    let previous_value_node = Uuid::new_v4();
    StreamGraphOperation::AddNode {
        node: StreamGraphDocumentNode {
            id: previous_value_node,
            name: String::new(),
            node_type: PREVIOUS_VALUE_NODE_TYPE.to_owned(),
            config: Value::Object(Map::new()),
            pins: Map::new(),
            exposed_config_fields: Vec::new(),
            studio: Box::new(StreamGraphNodeStudioFields {
                position: position_halfway_along_the_link,
                ..StreamGraphNodeStudioFields::default()
            }),
            fragment: None,
        },
        links: vec![
            StreamGraphDocumentLink {
                source: link.source,
                target: StreamGraphDocumentLinkEnd {
                    node: previous_value_node,
                    port: PREVIOUS_VALUE_NODE_INPUT_PORT.to_owned(),
                },
            },
            StreamGraphDocumentLink {
                source: StreamGraphDocumentLinkEnd {
                    node: previous_value_node,
                    port: PREVIOUS_VALUE_NODE_OUTPUT_PORT.to_owned(),
                },
                target: link.target,
            },
        ],
    }
}

/// The UUID of each node `rendered_level`, a level of a stream's `graph` render, renders as a
/// function node, at any level of the fragments it holds.
fn the_function_nodes_rendered_in(rendered_level: &Value, function_nodes: &mut HashSet<Uuid>) {
    for rendered_node in rendered_level["nodes"].as_array().into_iter().flatten() {
        if rendered_node["kind"] == "function"
            && let Some(node) = rendered_node["id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
        {
            function_nodes.insert(node);
        }
        the_function_nodes_rendered_in(&rendered_node["fragment"], function_nodes);
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
            if fragment_node.node_type == STREAM_FRAGMENT_NODE_TYPE {
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
    use std::collections::HashSet;
    use std::path::Path;

    use serde_json::json;
    use streamlib_project_repository::{
        PREVIOUS_VALUE_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentLink,
        StreamGraphDocumentLinkEnd, StreamGraphOperation,
    };
    use uuid::Uuid;

    use super::{
        the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in,
        the_module_a_project_file_is_imported_as, the_previous_value_node_added_on_the_link,
    };

    fn a_link_between(source: Uuid, target: Uuid) -> StreamGraphDocumentLink {
        StreamGraphDocumentLink {
            source: StreamGraphDocumentLinkEnd {
                node: source,
                port: "result".to_owned(),
            },
            target: StreamGraphDocumentLinkEnd {
                node: target,
                port: "x".to_owned(),
            },
        }
    }

    /// A document of an `add` node at `[0, 0]` linked into a `scale` node at `[200, 100]`, whose
    /// type is `type_of_the_scale_node`.
    fn add_linked_into_scale(type_of_the_scale_node: &str) -> (StreamGraphDocument, Uuid, Uuid) {
        let (add, scale) = (Uuid::new_v4(), Uuid::new_v4());
        let document = StreamGraphDocument::from_value(json!({
            "asset": Uuid::new_v4(),
            "nodes": [
                { "id": add, "name": "add", "type": "maths:Add",
                  "studio": { "position": [0, 0] } },
                { "id": scale, "name": "scale", "type": type_of_the_scale_node,
                  "studio": { "position": [200, 100] } },
            ],
            "links": [{
                "source": { "node": add, "port": "result" },
                "target": { "node": scale, "port": "x" },
            }],
        }))
        .unwrap();
        (document, add, scale)
    }

    #[test]
    fn a_link_closing_a_loop_of_function_nodes_is_carried_through_a_previous_value_node() {
        let (document, add, scale) = add_linked_into_scale("maths:Scale");
        let closing_link = a_link_between(scale, add);
        assert!(
            the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
                &document,
                &closing_link,
                &|| Ok(HashSet::from([add, scale])),
            )
            .unwrap()
        );

        let StreamGraphOperation::AddNode { node, links } =
            the_previous_value_node_added_on_the_link(&document, closing_link)
        else {
            panic!("the loop closes by adding a node");
        };
        assert_eq!(node.node_type, PREVIOUS_VALUE_NODE_TYPE);
        assert_eq!(
            node.studio
                .position
                .map(|pair| pair.map(|number| number.as_f64())),
            Some([Some(100.0), Some(50.0)])
        );
        assert_eq!(
            links,
            [
                StreamGraphDocumentLink {
                    source: StreamGraphDocumentLinkEnd {
                        node: scale,
                        port: "result".to_owned(),
                    },
                    target: StreamGraphDocumentLinkEnd {
                        node: node.id,
                        port: "value".to_owned(),
                    },
                },
                StreamGraphDocumentLink {
                    source: StreamGraphDocumentLinkEnd {
                        node: node.id,
                        port: "result".to_owned(),
                    },
                    target: StreamGraphDocumentLinkEnd {
                        node: add,
                        port: "x".to_owned(),
                    },
                },
            ]
        );
    }

    #[test]
    fn a_function_node_linked_into_itself_closes_a_loop() {
        let (document, add, _) = add_linked_into_scale("maths:Scale");
        assert!(
            the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
                &document,
                &a_link_between(add, add),
                &|| Ok(HashSet::from([add])),
            )
            .unwrap()
        );
    }

    #[test]
    fn a_loop_through_a_node_that_is_no_function_node_is_linked_as_asked() {
        let (document, add, scale) = add_linked_into_scale("nodes.scale:ScaleProcessor");
        assert!(
            !the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
                &document,
                &a_link_between(scale, add),
                &|| Ok(HashSet::from([add])),
            )
            .unwrap()
        );
    }

    #[test]
    fn a_loop_a_previous_value_node_is_already_in_is_linked_as_asked() {
        let (document, add, previous_value) = add_linked_into_scale(PREVIOUS_VALUE_NODE_TYPE);
        assert!(
            !the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
                &document,
                &a_link_between(previous_value, add),
                &|| Ok(HashSet::from([add, previous_value])),
            )
            .unwrap()
        );
    }

    #[test]
    fn a_link_closing_no_loop_reads_nothing_of_the_running_stream() {
        let (document, add, scale) = add_linked_into_scale("maths:Scale");
        let another_link_the_same_way = StreamGraphDocumentLink {
            target: StreamGraphDocumentLinkEnd {
                node: scale,
                port: "y".to_owned(),
            },
            ..a_link_between(add, scale)
        };
        assert!(
            !the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
                &document,
                &another_link_the_same_way,
                &|| Err("the running stream was read".to_owned()),
            )
            .unwrap()
        );
    }

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
