// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
    ProjectStreamDocumentOperation, ProjectTransactionAsSentAndAnswered, ProjectTransactionAuthor,
    ProjectTransactionLineage, ProjectTransactionReversal, STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY,
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphAction, StreamGraphActionLinkEnd, StreamGraphDocument,
    StreamGraphDocumentGroup, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, StreamGraphDocumentNote, StreamGraphFragmentPlacement,
    StreamGraphGroupFields, StreamGraphLevelOfAPlacedStream, StreamGraphNodeStudioFields,
    StreamGraphNoteFields, StreamGraphOperation, StreamGraphOperationOnAStream,
    asset_references_made_references_into_the_import,
    config_holding_the_default_name_of_a_graph_port_or_variable, fold_into_input_values,
    fragment_with_each_node_uuid_kept_by_its_name, graph_document_holds_a_stream_fragment,
    is_a_graph_interface_node_type, stream_assets_of_the_project_a_graph_document_places,
    the_fragment_a_stream_fragment_places, the_stream_document_of_an_import,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, GraphLinkEndOnTheWire, GraphTransactionAddedGroup,
    GraphTransactionAddedNode, GraphTransactionAddedNote, GraphTransactionConnectedLink,
    GraphTransactionInputOverriddenByALink, GraphTransactionToolResult,
    NodeNotRunningBecauseOnTheWire, ProjectActionRefusalOnTheWire, RefusedGraphLinkOnTheWire,
};
use uuid::Uuid;

use crate::control_operations_on_project_assets::{
    bring_the_loaded_streams_in_line_with_the_stream_documents_of,
    the_idempotency_key_a_caller_sent,
};
use crate::mcp_project_change_feed::project_transaction_on_the_wire;
use crate::refusal_code_and_place::RefusalCodeAndPlace;

mod former_port_names;
mod input_value_sets;
mod input_values_set_live;
mod live_application_and_reversal;
mod request_application;
mod request_to_graph_operation;
mod transaction_answers;

use former_port_names::{
    move_each_former_port_name, the_placed_streams_with_each_former_port_name_moved,
    the_request_under_the_current_port_names,
};
use input_value_sets::*;
use request_to_graph_operation::graph_operation_for_the_request;
use transaction_answers::WhatAnAnswerNamesBesideItsOperations;

/// The input of a `GraphInput` whose value is the value of the stream input it declares.
const GRAPH_INPUT_VALUE_INPUT: &str = "value";

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
        StreamGraphAction::SetNodeInputValue { node, input, .. } => WhatAGraphOperationNames {
            node: node.clone(),
            field: Some(input.clone()),
            ..WhatAGraphOperationNames::default()
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

/// One graph operation applied: the operation its answer reports, as the stream it names draws
/// it; what it made on that running stream; the project operations it records, in order; each
/// operation a loaded stream takes once the project's documents hold it; and the link that
/// overrides the input value it sets, when one feeds that input.
struct OneGraphRequestApplied {
    operation_answered: StreamGraphOperation,
    applied_live: Option<OneOperationAppliedLive>,
    project_operations: Vec<ProjectOperation>,
    made_live_once_the_documents_hold_them: Vec<(String, StreamGraphOperation)>,
    overridden_by_a_link: Option<GraphTransactionInputOverriddenByALink>,
}

/// One operation made on a running stream, and the value set live on the input it set that it
/// replaced, which its reversal sets live again once the saved value is back.
struct OneOperationAppliedLive {
    operation: StreamGraphOperation,
    live_value_it_replaced: Option<streamlib::sdk::graph::input_values::InputValueToSetLive>,
}

impl OneOperationAppliedLive {
    /// `operation`, made on a running stream, replacing no value set live.
    fn replacing_no_live_value(operation: StreamGraphOperation) -> Self {
        Self {
            operation,
            live_value_it_replaced: None,
        }
    }
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
) -> Result<StreamGraphDocument, GraphRequestRefusal> {
    if let Some(held) = streams
        .values()
        .find(|held| held.stream_location.stream_asset == stream)
    {
        return Ok(held.staged_graph.clone());
    }
    if let Some(staged) = staged_graph_of_each_stream_not_running.get(&stream) {
        return Ok(staged.clone());
    }
    Ok(project_repository.stream_graph_document(stream)?)
}

/// The graph document of the stream a stream fragment's `held_stream`, its config's `stream`,
/// places: one of the project as the transaction has staged it, or one of an import as its
/// folder holds it, each asset it references made a reference into that import.
fn the_document_a_placed_stream_holds(
    held_stream: &Value,
    streams: &BTreeMap<String, StreamATransactionEdits>,
    staged_graph_of_each_stream_not_running: &BTreeMap<Uuid, StreamGraphDocument>,
    project_repository: &ProjectRepository,
) -> Result<StreamGraphDocument, GraphRequestRefusal> {
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
            Ok(StreamGraphDocument::from_value(document)?)
        }
    }
}

/// The type, name and input values of the node `edit` adds, or the config it sets on one, when
/// that node runs as a node of its type.
fn the_graph_node_an_edit_adds_or_configures(
    edit: &StreamGraphOperation,
    document: &StreamGraphDocument,
) -> Option<(String, String, Map<String, Value>)> {
    let (node_type, node_name, input_values, holds_a_fragment) = match edit {
        StreamGraphOperation::AddNode { node, .. } => (
            node.node_type.clone(),
            node.name.clone(),
            node.input_values.clone(),
            node.fragment.is_some(),
        ),
        StreamGraphOperation::SetNodeConfig { node, to, .. } => {
            let configured = document.node(*node).ok()?;
            (
                configured.node_type,
                configured.name,
                to.as_object().cloned().unwrap_or_default(),
                configured.fragment.is_some(),
            )
        }
        _ => return None,
    };
    let names_a_node = !holds_a_fragment
        && node_type != STREAM_FRAGMENT_NODE_TYPE
        && !is_a_graph_interface_node_type(&node_type);
    names_a_node.then_some((node_type, node_name, input_values))
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

/// Why one graph action was refused: its code and the place it names, the runtime's own words,
/// and the JSON pointer, into the action's own params, of the value refused.
#[derive(Debug)]
pub(super) struct GraphRequestRefusal {
    code_and_place: Box<RefusalCodeAndPlace>,
    runtime_words: String,
    refused_params_pointer: Option<String>,
}

impl GraphRequestRefusal {
    /// A refusal coded and placed by `code_and_place`, in `runtime_words`.
    fn coded(code_and_place: RefusalCodeAndPlace, runtime_words: String) -> Self {
        Self {
            code_and_place: Box::new(code_and_place),
            runtime_words,
            refused_params_pointer: None,
        }
    }

    /// A refusal of a value set on an input, coded by `refusal`; the value is at
    /// `refused_params_pointer` in the action's params.
    fn of_an_input_value(
        refusal: streamlib::sdk::graph::input_values::InputValueRefusal,
        refused_params_pointer: String,
    ) -> Self {
        use streamlib::sdk::graph::input_values::InputValueRefusalCode;
        Self {
            refused_params_pointer: Some(refused_params_pointer),
            ..Self::coded(
                RefusalCodeAndPlace::coded(match refusal.code {
                    InputValueRefusalCode::PortNotFound => {
                        ControlOperationRefusalCode::PortNotFound
                    }
                    InputValueRefusalCode::InputTakesNoValue => {
                        ControlOperationRefusalCode::InputTakesNoValue
                    }
                    InputValueRefusalCode::InputValueNotValid => {
                        ControlOperationRefusalCode::InputValueNotValid
                    }
                }),
                refusal.words,
            )
        }
    }

    /// A refusal of a value set live on a `reset` input while its stream's playback is stopped,
    /// in the runtime's words: coded `node_not_running`, because the playback is stopped.
    fn of_a_reset_input_while_stopped(runtime_words: String) -> Self {
        Self::coded(
            RefusalCodeAndPlace {
                not_running_because: Some(NodeNotRunningBecauseOnTheWire::PlaybackStopped),
                ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotRunning)
            },
            runtime_words,
        )
    }

    /// The refusal of a node `node` the action names that its stream does not hold, in the
    /// runtime's words `why_no_node_is_it`.
    fn of_a_node_not_found(node: &str, why_no_node_is_it: String) -> Self {
        Self::coded(
            RefusalCodeAndPlace {
                refused_node: Some(node.to_owned()),
                ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotFound)
            },
            why_no_node_is_it,
        )
    }

    /// This refusal's words, for a project repository call that takes a reader answering words;
    /// the refusal itself is kept in `kept`, so the call's refusal reads back typed.
    fn words_kept_in(self, kept: &std::cell::RefCell<Option<Self>>) -> String {
        let runtime_words = self.runtime_words.clone();
        *kept.borrow_mut() = Some(self);
        runtime_words
    }

    /// The refusal of a project repository call that answered `why`: the one its reader kept in
    /// `kept`, when it kept one.
    fn kept_in_or(kept: &std::cell::RefCell<Option<Self>>, why: String) -> Self {
        kept.take().unwrap_or_else(|| why.into())
    }

    /// A refusal of a port a request names that is not there, in the runtime's words.
    fn of_a_port_not_found(runtime_words: String) -> Self {
        Self {
            refusal_code: ControlOperationRefusalCode::PortNotFound,
            ..Self::from(runtime_words)
        }
    }

    /// The refusal naming `input` as the field refused.
    fn of_the_field(mut self, input: &str) -> Self {
        self.code_and_place.refused_field = Some(input.to_owned());
        self
    }
}

impl From<String> for GraphRequestRefusal {
    fn from(runtime_words: String) -> Self {
        Self::coded(
            RefusalCodeAndPlace::coded(ControlOperationRefusalCode::Refused),
            runtime_words,
        )
    }
}

impl From<streamlib_project_repository::NodeNotRead> for GraphRequestRefusal {
    fn from(not_read: streamlib_project_repository::NodeNotRead) -> Self {
        match not_read {
            streamlib_project_repository::NodeNotRead::NoSuchNode(why) => Self::coded(
                RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotFound),
                why,
            ),
            streamlib_project_repository::NodeNotRead::DoesNotRead(why) => why.into(),
        }
    }
}

impl From<&str> for GraphRequestRefusal {
    fn from(runtime_words: &str) -> Self {
        runtime_words.to_owned().into()
    }
}

impl From<streamlib::sdk::error::Error> for GraphRequestRefusal {
    fn from(refusal: streamlib::sdk::error::Error) -> Self {
        Self::coded(
            RefusalCodeAndPlace::of_an_engine_error(&refusal),
            refusal.to_string(),
        )
    }
}

impl From<ProjectRepositoryRefusal> for GraphRequestRefusal {
    fn from(refusal: ProjectRepositoryRefusal) -> Self {
        Self::coded(
            RefusalCodeAndPlace::of_a_project_repository_refusal(&refusal),
            refusal.to_string(),
        )
    }
}

/// Why a graph action was refused, in the runtime's own words, and which of its operations
/// was refused when one was.
pub(crate) struct GraphTransactionRefusal {
    refused_operation_index: Option<usize>,
    what_the_refused_operation_names: Box<WhatAGraphOperationNames>,
    refusal: GraphRequestRefusal,
}

impl GraphTransactionRefusal {
    fn of_the_operation(
        refused_operation_index: usize,
        what_the_refused_operation_names: WhatAGraphOperationNames,
        refusal: impl Into<GraphRequestRefusal>,
    ) -> Self {
        Self {
            refused_operation_index: Some(refused_operation_index),
            what_the_refused_operation_names: Box::new(what_the_refused_operation_names),
            refusal: refusal.into(),
        }
    }

    pub(crate) fn of_the_whole_transaction(refusal: impl Into<GraphRequestRefusal>) -> Self {
        Self {
            refused_operation_index: None,
            what_the_refused_operation_names: Box::default(),
            refusal: refusal.into(),
        }
    }

    /// The refusal of the action the tool `tool_name` was sent, as the wire carries it: the
    /// node as the refused operation's caller named it, else as the refusal names it; the input
    /// the refusal names, else the one the operation names; and the link the operation names.
    pub(crate) fn into_the_refusal_on_the_wire(
        self,
        tool_name: &str,
    ) -> ProjectActionRefusalOnTheWire {
        let what_the_refused_operation_names = *self.what_the_refused_operation_names;
        let GraphRequestRefusal {
            code_and_place,
            runtime_words,
            refused_params_pointer,
        } = self.refusal;
        let mut code_and_place = *code_and_place;
        let refused_params_pointer = match (refused_params_pointer, self.refused_operation_index) {
            (Some(pointer), Some(index)) if tool_name == <streamlib_control_contract::Transact as streamlib_control_contract::ControlOperation>::METHOD_NAME => {
                Some(format!("/operations/{index}{pointer}"))
            }
            (pointer, _) => pointer,
        };
        code_and_place.refused_node = what_the_refused_operation_names
            .node
            .or(code_and_place.refused_node);
        code_and_place.refused_field = code_and_place
            .refused_field
            .or(what_the_refused_operation_names.field);
        ProjectActionRefusalOnTheWire {
            refused_operation_index: self.refused_operation_index,
            refused_link: what_the_refused_operation_names.link,
            refused_params_pointer,
            ..code_and_place.on_the_refusal_of(tool_name, runtime_words)
        }
    }
}

impl From<ProjectRepositoryRefusal> for GraphTransactionRefusal {
    fn from(refusal: ProjectRepositoryRefusal) -> Self {
        Self::of_the_whole_transaction(refusal)
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
        let requests =
            self.each_request_naming_the_graph_input_it_sets(requests, &stream_id_of_each_request)?;
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for stream_id in &stream_id_of_each_request {
            if !stream_ids_in_order.contains(stream_id) {
                stream_ids_in_order.push(stream_id.clone());
            }
        }
        if mode == GraphEditMode::Live
            && let Some(applied) = self.apply_input_values_set_live_apart_from_the_graph(
                &requests,
                &stream_id_of_each_request,
            )?
        {
            return Ok(applied);
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
                            GraphRequestRefusal::coded(
                                RefusalCodeAndPlace::coded(
                                    ControlOperationRefusalCode::StreamNotFound,
                                ),
                                format!(
                                    "no stream the runtime holds runs the stream asset {}; the \
                                     stream assets it holds are: {}",
                                    request.stream(),
                                    if loaded_stream_assets.is_empty() {
                                        "none".to_owned()
                                    } else {
                                        loaded_stream_assets.join(", ")
                                    }
                                ),
                            ),
                        )
                    })
            })
            .collect()
    }

    /// `requests` with each `set_node_input_value` naming no node made the set of the `value`
    /// input of the top-level `GraphInput` declaring the stream input it names, in the stream
    /// `stream_id_of_each_request` holds for it. Refused for one below the top level, or naming
    /// an input no top-level `GraphInput` declares.
    fn each_request_naming_the_graph_input_it_sets(
        &self,
        requests: Vec<StreamGraphAction>,
        stream_id_of_each_request: &[String],
    ) -> Result<Vec<StreamGraphAction>, GraphTransactionRefusal> {
        requests
            .into_iter()
            .zip(stream_id_of_each_request)
            .enumerate()
            .map(|(request_index, (request, stream_id))| {
                let StreamGraphAction::SetNodeInputValue {
                    stream,
                    level,
                    node: None,
                    input,
                    value,
                } = request
                else {
                    return Ok(request);
                };
                let refused = |refusal: GraphRequestRefusal| {
                    GraphTransactionRefusal::of_the_operation(
                        request_index,
                        WhatAGraphOperationNames {
                            field: Some(input.clone()),
                            ..WhatAGraphOperationNames::default()
                        },
                        refusal.of_the_field(&input),
                    )
                };
                if !level.is_empty() {
                    return Err(refused(GraphRequestRefusal::from(format!(
                        "a `set_node_input_value` naming no node sets an input of the stream, \
                         which a top-level `GraphInput` declares, so its `level` is the top \
                         level; the input `{input}` was named at a level below it. To fix: \
                         leave `level` out, or name the node whose input it is"
                    ))));
                }
                let graph_input = self
                    .operations_on_the_loaded_streams
                    .runtime_operations_of_the_stream_a_call_names(stream_id)
                    .and_then(|stream_operations| {
                        self.async_runtime.block_on(
                            stream_operations.the_top_level_graph_input_declaring(input.clone()),
                        )
                    })
                    .map_err(|refusal| {
                        refused(match refusal {
                            streamlib::sdk::error::Error::NodeNotFound(runtime_words) => {
                                GraphRequestRefusal::of_a_port_not_found(runtime_words)
                            }
                            refusal => GraphRequestRefusal::from(refusal.to_string()),
                        })
                    })?;
                Ok(StreamGraphAction::SetNodeInputValue {
                    stream,
                    level,
                    node: Some(graph_input.node.to_string()),
                    input: GRAPH_INPUT_VALUE_INPUT.to_owned(),
                    value,
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

    /// The project every stream of `stream_ids` belongs to, its one writer, refused for
    /// streams of two projects.
    fn the_one_project_the_streams_belong_to(
        &self,
        stream_ids: &[String],
        stream_locations: &HashMap<String, StreamLocation>,
    ) -> Result<streamlib_project_repository::ProjectRepositoryOneWriterAtATime, GraphRequestRefusal>
    {
        let mut project: Option<&StreamLocation> = None;
        for stream_id in stream_ids {
            let stream_location = match stream_locations.get(stream_id) {
                None => {
                    return Err(self
                        .operations_on_the_loaded_streams
                        .runtime_operations_of_the_stream_a_call_names(stream_id)
                        .err()
                        .map_or_else(
                            || format!("stream `{stream_id}` is not loaded").into(),
                            GraphRequestRefusal::from,
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
                    )
                    .into());
                }
                _ => project = Some(stream_location),
            }
        }
        let project = project.ok_or("a transaction holds at least one operation")?;
        Ok(self
            .project_repositories
            .the_project_repository_at(&project.project_directory)?)
    }
}

/// A run of one of a stream's graph generators, `(generator, fragment name, promoted settings)`,
/// answering the fragment node it placed or the refusal.
type GraphGeneratorRunOfTheStream<'stream> =
    dyn Fn(&str, Option<&str>, &Map<String, Value>) -> Result<Value, GraphRequestRefusal> + 'stream;

/// The placement of an imported stream under the fragment node `(fragment node, held stream)`
/// names, `held_stream` its config's `stream`: the fragment it places and the stream's name.
type ImportedStreamPlacement<'repository> =
    dyn Fn(Uuid, &Value) -> Result<(Value, String), GraphRequestRefusal> + 'repository;

/// The graph document of the stream a stream fragment places, from its config's `stream`.
type DocumentOfAPlacedStream<'repository> =
    dyn Fn(&Value) -> Result<StreamGraphDocument, GraphRequestRefusal> + 'repository;

/// The family wire name of an output port of the running stream, read from its `graph`.
type FamilyOfAnOutputPortOfTheRunningStream<'stream> =
    dyn Fn(&StreamGraphDocumentLinkEnd) -> Result<String, GraphRequestRefusal> + 'stream;

/// The UUID of each node the running stream's `graph` renders as a function node.
type FunctionNodesOfTheRunningStream<'stream> =
    dyn Fn() -> Result<HashSet<Uuid>, GraphRequestRefusal> + 'stream;

/// The config a node of a type holding some input values takes whole, as the running stream
/// resolves the type.
type ConfigANodeOfTheRunningStreamTakes<'stream> =
    dyn Fn(&str, &Map<String, Value>) -> Result<Value, GraphRequestRefusal> + 'stream;

/// What building a graph request's operation reads of the running stream and its project.
struct WhatAGraphRequestReadsOfItsStreamAndProject<'reads> {
    run_the_graph_generator: &'reads GraphGeneratorRunOfTheStream<'reads>,
    place_the_imported_stream: &'reads ImportedStreamPlacement<'reads>,
    document_of_a_placed_stream: &'reads DocumentOfAPlacedStream<'reads>,
    family_of_an_output_port: &'reads FamilyOfAnOutputPortOfTheRunningStream<'reads>,
    function_nodes_of_the_running_stream: &'reads FunctionNodesOfTheRunningStream<'reads>,
    config_a_node_takes: &'reads ConfigANodeOfTheRunningStreamTakes<'reads>,
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
) -> Result<(Value, String), GraphRequestRefusal> {
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
) -> Result<Vec<StreamGraphAction>, ProjectRepositoryRefusal> {
    let changed_modules: std::collections::HashSet<String> = changed_files
        .iter()
        .filter_map(|changed_file| {
            the_module_a_project_file_is_imported_as(project_directory, changed_file)
        })
        .collect();
    if changed_modules.is_empty() {
        return Ok(Vec::new());
    }
    let project_repository = project_repositories.the_project_repository_at(project_directory)?;
    let project_repository = project_repository.lock();
    let project = project_repository.project_file().project;
    let mut refreshes = Vec::new();
    for listing in operations_on_the_loaded_streams.list_streams() {
        let Some(stream_location) = listing.stream_location.filter(|stream_location| {
            stream_location.project == project && listing.node_count.is_some()
        }) else {
            continue;
        };
        let staged_graph =
            project_repository.stream_graph_document(stream_location.stream_asset)?;
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
                config: Some(fragment_node.input_values),
                label: None,
            });
        }
    }
    Ok(refreshes)
}

#[cfg(test)]
mod tests;
