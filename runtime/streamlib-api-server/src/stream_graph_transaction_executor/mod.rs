// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one executor of a stream graph transaction: each operation staged, applied whole or
//! refused whole, written to the graph documents and committed to the project's history, or
//! applied live to the running streams alone.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::descriptors::{ConvertNodeToInsert, PinLinkOutcome};
use streamlib::sdk::runtime::{
    OperationsOnTheStreamsLoadedInThisRuntime, PortOfANodeOfAType, RuntimeOperations,
    StreamLocation, WhatALiveConnectMakesOfALink,
};
use streamlib_control_contract::GraphEditMode;
use streamlib_project_repository::{
    AppliedProjectTransaction, GRAPH_DOCUMENT_FILE_SUFFIX, GraphEditStream,
    PREVIOUS_VALUE_NODE_INPUT_PORT, PREVIOUS_VALUE_NODE_OUTPUT_PORT, PREVIOUS_VALUE_NODE_TYPE,
    ProjectAction, ProjectActionIdempotencyKey, ProjectAssetKind, ProjectOperation,
    ProjectRelativeAssetPath, ProjectRepositoriesOpenInThisRuntime, ProjectRepository,
    ProjectRepositoryRefusal, ProjectStreamDocumentOperation, ProjectTransactionAsSentAndAnswered,
    ProjectTransactionAuthor, ProjectTransactionLineage, ProjectTransactionLinkMadeUnjudged,
    ProjectTransactionNodeInsertedOnALink, ProjectTransactionReversal,
    STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, StreamGraphAction,
    StreamGraphActionLinkEnd, StreamGraphDocument, StreamGraphDocumentGroup,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphDocumentNote, StreamGraphFragmentPlacement, StreamGraphGroupFields,
    StreamGraphLevelOfAPlacedStream, StreamGraphNodeStudioFields, StreamGraphNoteFields,
    StreamGraphOperation, StreamGraphOperationOnAStream,
    asset_references_made_references_into_the_import,
    config_holding_the_default_name_of_a_graph_port_or_variable,
    fragment_with_each_node_uuid_kept_by_its_name, graph_document_holds_a_stream_fragment,
    input_values_after_the_set, is_a_node_type_of_the_graph_itself,
    stream_assets_of_the_project_a_graph_document_places, the_fragment_a_stream_fragment_places,
    the_stream_document_of_an_import, the_value_held_on_the_input_or_its_member,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, GraphLinkEndOnTheWire, GraphTransactionAddedGroup,
    GraphTransactionAddedNode, GraphTransactionAddedNote, GraphTransactionConnectedLink,
    GraphTransactionInputOverriddenByALink, GraphTransactionNodeInsertedBecause,
    GraphTransactionRebuiltNode, GraphTransactionSavedValueRetyped, GraphTransactionToolResult,
};
use uuid::Uuid;

use crate::control_operations_on_project_assets::{
    bring_the_loaded_streams_in_line_with_the_stream_documents_of,
    the_idempotency_key_a_caller_sent,
};
use crate::mcp_project_change_feed::project_transaction_on_the_wire;
use crate::refusal_code_and_place::RefusalCodeAndPlace;

mod connect_asked_at_a_level;
mod former_port_names;
mod graph_request_refusals;
use graph_request_refusals::what_a_graph_action_names;
pub(crate) use graph_request_refusals::{
    GraphRequestRefusal, GraphTransactionRefusal, WhereAnOperationEdits,
    the_level_holds_both_ends_by_uuid,
};
mod fragments_a_changed_module_defines;
mod graph_input_retype_carrying_its_saved_value;
pub(crate) use fragments_a_changed_module_defines::the_refresh_of_each_fragment_a_changed_module_defines;
use graph_input_retype_carrying_its_saved_value::SavedValueRetypesWorkedOutBeforeTheEdit;
mod input_value_sets;
mod input_values_set_live;
mod live_application_and_reversal;
mod pin_group_gaps_closed;
use live_application_and_reversal::{StreamATransactionEdits, the_open_batch_of};
mod ports_of_the_node_a_refusal_names;
mod request_application;
mod request_to_graph_operation;
mod streams_each_operation_names;
use connect_asked_at_a_level::why_each_link_an_edit_makes_at_a_placed_level_goes_unjudged;
pub(crate) use connect_asked_at_a_level::{
    AConnectJudgedInsideAPlacedStream, a_connect_judged_inside_a_placed_stream,
};
pub(crate) use streams_each_operation_names::StreamsTheRuntimeHolds;
mod transaction_answers;

pub(crate) use former_port_names::the_current_name_of_a_port;
use former_port_names::{
    move_each_former_port_name, the_link_ends_under_their_current_port_names,
    the_placed_streams_with_each_former_port_name_moved, the_request_under_the_current_port_names,
};
use input_value_sets::*;
pub(crate) use request_to_graph_operation::WhatAConnectAsks;
pub(crate) use request_to_graph_operation::the_node_a_connects_end_names;
use request_to_graph_operation::{
    graph_operation_for_the_request, refuse_a_link_the_graph_already_holds,
    the_operation_a_connect_asks,
};
use streams_each_operation_names::the_stream_each_operation_names;
use transaction_answers::{WhatAnAnswerNamesBesideItsOperations, the_links_an_operation_makes};

/// What a `connect` from `source` to `target` at the own level of the running stream
/// `stream_operations` runs asks of it, made as a transaction's `connect` makes it on the
/// stream's graph as `rendered_graph` renders it and `level_as_drawn` reads that render: each
/// port under its current name, each node by its UUID, a loop of function nodes closed by a
/// `PreviousValue` node added on it, a link whose pin types connect through a built-in
/// conversion carried through its convert node added on it, and a link the graph already holds
/// refused, in the order [`StreamGraphTransactionExecutor::apply_one_request`] takes those
/// steps.
pub(crate) fn what_a_connect_asks_of_the_running_stream(
    source: StreamGraphActionLinkEnd,
    target: StreamGraphActionLinkEnd,
    rendered_graph: &Value,
    level_as_drawn: &StreamGraphDocument,
    stream_operations: &dyn RuntimeOperations,
) -> Result<WhatAConnectAsks, GraphRequestRefusal> {
    what_a_connect_asks_at_a_level(
        source,
        target,
        rendered_graph,
        level_as_drawn,
        stream_operations,
        &|link| the_convert_node_a_live_connect_places(stream_operations, link),
    )
}

/// What a `connect` from `source` to `target` asks of `level_as_drawn`, a level of the running
/// stream `stream_operations` runs as drawn, made as
/// [`what_a_connect_asks_of_the_running_stream`] makes it, the convert node a link needs as
/// `convert_node_a_link_needs` says.
fn what_a_connect_asks_at_a_level(
    source: StreamGraphActionLinkEnd,
    target: StreamGraphActionLinkEnd,
    rendered_graph: &Value,
    level_as_drawn: &StreamGraphDocument,
    stream_operations: &dyn RuntimeOperations,
    convert_node_a_link_needs: &ConvertNodeALinkOfTheRunningStreamNeeds<'_>,
) -> Result<WhatAConnectAsks, GraphRequestRefusal> {
    let no_node_by_label = HashMap::new();
    let (source, target) = the_link_ends_under_their_current_port_names(
        source,
        target,
        level_as_drawn,
        &no_node_by_label,
        stream_operations,
    );
    let asked = the_operation_a_connect_asks(
        &source,
        &target,
        level_as_drawn,
        &no_node_by_label,
        &|| Ok(the_function_nodes_of_the_render(rendered_graph)),
        convert_node_a_link_needs,
        &|_| Ok(()),
    )?;
    if let WhatAConnectAsks::Link(link) = &asked {
        refuse_a_link_the_graph_already_holds(level_as_drawn, link)?;
    }
    Ok(asked)
}

/// The convert node a transaction's `connect` places on `link`, a link of the running stream
/// `stream_operations` runs whose nodes it runs, when its two typed ends connect only through a
/// built-in conversion; the refusal of such a link that the island checks of the two links
/// through that node refuse; `None` for any other link, a refused one among them, which
/// `connect` then refuses as it judges it.
fn the_convert_node_a_live_connect_places(
    stream_operations: &dyn RuntimeOperations,
    link: &StreamGraphDocumentLink,
) -> Result<Option<ConvertNodeToInsert>, GraphRequestRefusal> {
    match stream_operations.what_a_live_connect_makes_of_the_link(link) {
        Ok(WhatALiveConnectMakesOfALink::Makes(Some(verdict))) => match verdict.outcome {
            PinLinkOutcome::ConnectsThroughConvertNode(convert_node) => Ok(Some(*convert_node)),
            PinLinkOutcome::Connects
            | PinLinkOutcome::ConnectsWithFreeConversion(_)
            | PinLinkOutcome::Refused(_) => Ok(None),
        },
        Ok(_) => Ok(None),
        Err(refusal)
            if stream_operations
                .the_verdict_of_a_link_between_two_running_nodes(link)
                .is_some_and(|verdict| {
                    matches!(
                        verdict.outcome,
                        PinLinkOutcome::ConnectsThroughConvertNode(_)
                    )
                }) =>
        {
            Err(refusal.into())
        }
        Err(_) => Ok(None),
    }
}

/// Why a transaction's `connect` places no convert node on a link only one bridges.
#[derive(Clone, Copy)]
enum WhyConnectPlacesNoConvertNode {
    /// The link is inside a placed stream, not at the stream's own level.
    TheLinkIsAtAPlacedLevel,
    /// An earlier operation of the transaction has the stream take its edits once the
    /// documents hold them.
    TheStreamTakesItsEditsLater,
}

/// The refusal of a link only `convert_node` bridges, which `connect` does not place `because`.
fn the_refusal_of_a_link_only_a_convert_node_bridges(
    convert_node: &ConvertNodeToInsert,
    because: WhyConnectPlacesNoConvertNode,
) -> GraphRequestRefusal {
    let words = match because {
        WhyConnectPlacesNoConvertNode::TheLinkIsAtAPlacedLevel => format!(
            "{}: `connect` places that node itself only at a stream's own level, and this link \
             is inside a placed stream; add the node and its two links there yourself",
            convert_node.refusal_without_it
        ),
        WhyConnectPlacesNoConvertNode::TheStreamTakesItsEditsLater => format!(
            "{}, or connect the link in a transaction of its own: `connect` places that node \
             itself only on a stream that takes the edit live, and an earlier operation of this \
             transaction has the stream take its edits once the documents hold them",
            convert_node.refusal_without_it
        ),
    };
    GraphRequestRefusal::coded(
        RefusalCodeAndPlace::coded(ControlOperationRefusalCode::LinkPinTypesDoNotConnect),
        words,
    )
}

/// Refuse `link`, drawn at `level_as_drawn`, before anything is written, as the stream's own
/// level refuses a link made live, when its two typed ends, judged by the ports the node types
/// of that level state as the running stream `stream_operations` resolves them, and not by the
/// settings any of those nodes holds, do not connect as they are: one only a convert node
/// bridges, which `connect` does not place there `because`, and any their pin types refuse. A
/// link either end of which is untyped, or no node of the level, is left to the stream to judge
/// as it takes the edit.
fn refuse_a_link_judged_by_the_node_types_of_its_level(
    stream_operations: &dyn RuntimeOperations,
    level_as_drawn: &StreamGraphDocument,
    link: &StreamGraphDocumentLink,
    because: WhyConnectPlacesNoConvertNode,
) -> Result<(), GraphRequestRefusal> {
    let (Ok(source), Ok(target)) = (
        level_as_drawn.node(link.source.node),
        level_as_drawn.node(link.target.node),
    ) else {
        return Ok(());
    };
    let verdict = stream_operations.the_verdict_of_a_link_between_two_node_types(
        PortOfANodeOfAType {
            node_name: &source.name,
            node_type: &source.node_type,
            port: &link.source.port,
        },
        PortOfANodeOfAType {
            node_name: &target.name,
            node_type: &target.node_type,
            port: &link.target.port,
        },
    );
    match verdict.map(|verdict| verdict.outcome) {
        Some(PinLinkOutcome::ConnectsThroughConvertNode(convert_node)) => Err(
            the_refusal_of_a_link_only_a_convert_node_bridges(&convert_node, because),
        ),
        Some(PinLinkOutcome::Refused(refusal)) => {
            Err(streamlib::sdk::error::Error::PinLinkRefused(refusal).into())
        }
        Some(PinLinkOutcome::Connects | PinLinkOutcome::ConnectsWithFreeConversion(_)) | None => {
            Ok(())
        }
    }
}

/// One graph operation applied: the operation its answer reports, as the stream it names draws
/// it; what it made on that running stream; the project operations it records, in order; each
/// operation a loaded stream takes once the project's documents hold it; the link that
/// overrides the input value it sets, when one feeds that input; and each link it made without
/// judging it, with why, as the stream draws it and as the transaction keeps it.
struct OneGraphRequestApplied {
    operation_answered: StreamGraphOperation,
    applied_live: Option<OneOperationAppliedLive>,
    project_operations: Vec<ProjectOperation>,
    made_live_once_the_documents_hold_them: Vec<(String, StreamGraphOperation)>,
    overridden_by_a_link: Option<GraphTransactionInputOverriddenByALink>,
    links_made_unjudged: Vec<(StreamGraphDocumentLink, String)>,
    links_made_unjudged_as_kept: Vec<ProjectTransactionLinkMadeUnjudged>,
    saved_value_retyped: Option<GraphTransactionSavedValueRetyped>,
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

/// The location of each stream of `listings` that has one, by its stream id.
fn the_location_of_each_listed_stream(
    listings: Vec<streamlib::sdk::runtime::StreamListing>,
) -> HashMap<String, StreamLocation> {
    listings
        .into_iter()
        .filter_map(|listing| Some((listing.stream_id, listing.stream_location?)))
        .collect()
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
        && !is_a_node_type_of_the_graph_itself(&node_type);
    names_a_node.then_some((node_type, node_name, input_values))
}

/// What an answer names of a node an operation adds beside its UUID: the `as` its caller gave
/// it, and why a `connect` inserted it, when one did.
struct WhatTheAnswerNamesOfAnAddedNode {
    /// The node's UUID in the document that holds it, which a transaction keeps.
    node_in_its_document: Uuid,
    label: Option<String>,
    inserted_because: Option<GraphTransactionNodeInsertedBecause>,
}

/// Each node of `what_the_answer_names_of_each_added_node` a `connect` inserted on its link, and
/// why, in the order of their UUIDs, as a transaction keeps them for an action sent again.
fn the_nodes_inserted_on_links(
    what_the_answer_names_of_each_added_node: &HashMap<Uuid, WhatTheAnswerNamesOfAnAddedNode>,
) -> Vec<ProjectTransactionNodeInsertedOnALink> {
    let mut inserted: Vec<ProjectTransactionNodeInsertedOnALink> =
        what_the_answer_names_of_each_added_node
            .values()
            .filter_map(|named| {
                Some(ProjectTransactionNodeInsertedOnALink {
                    node: named.node_in_its_document,
                    inserted_because: named.inserted_because?,
                })
            })
            .collect();
    inserted.sort_by_key(|inserted| inserted.node);
    inserted
}

/// Record what the answer names of the node `operation` adds, its `label` and why a `connect`
/// inserted it, under the UUID `drawn_uuid_of` gives it, in
/// `what_the_answer_names_of_each_added_node`, when it names anything.
fn record_what_the_answer_names_of_the_node_the_operation_adds(
    label: Option<String>,
    inserted_because: Option<GraphTransactionNodeInsertedBecause>,
    operation: &StreamGraphOperation,
    drawn_uuid_of: &dyn Fn(Uuid) -> Uuid,
    what_the_answer_names_of_each_added_node: &mut HashMap<Uuid, WhatTheAnswerNamesOfAnAddedNode>,
) {
    let added_node = match operation {
        StreamGraphOperation::AddNode { node, .. } => Some(node.id),
        StreamGraphOperation::FoldIntoFragment { fold } => fold.fragment_node(),
        _ => None,
    };
    let Some(node_in_its_document) = added_node else {
        return;
    };
    if label.is_some() || inserted_because.is_some() {
        what_the_answer_names_of_each_added_node.insert(
            drawn_uuid_of(node_in_its_document),
            WhatTheAnswerNamesOfAnAddedNode {
                node_in_its_document,
                label,
                inserted_because,
            },
        );
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
        requests: Vec<StreamGraphAction<GraphEditStream>>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
    ) -> Result<GraphTransactionToolResult, GraphTransactionRefusal> {
        if requests.is_empty() {
            return Err(GraphTransactionRefusal::of_the_whole_transaction(
                "a transaction holds at least one operation".to_owned(),
            ));
        }
        let listings = self.operations_on_the_loaded_streams.list_streams();
        let (requests, stream_id_of_each_request): (Vec<StreamGraphAction>, Vec<String>) =
            the_stream_each_operation_names(requests, &listings)?
                .into_iter()
                .unzip();
        let stream_locations = the_location_of_each_listed_stream(listings);
        self.apply_to_the_streams_each_names(
            requests,
            &stream_id_of_each_request,
            &stream_locations,
            mode,
            author,
            idempotency_key,
        )
        .map_err(|refusal| {
            self.suggesting_the_ports_of_the_node_type_a_port_not_found_names(
                refusal,
                &stream_id_of_each_request,
                &stream_locations,
            )
        })
    }

    /// Apply `requests`, each on the stream of the same place in `stream_id_of_each_request`,
    /// as one transaction by `author`.
    fn apply_to_the_streams_each_names(
        &self,
        requests: Vec<StreamGraphAction>,
        stream_id_of_each_request: &[String],
        stream_locations: &HashMap<String, StreamLocation>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
    ) -> Result<GraphTransactionToolResult, GraphTransactionRefusal> {
        let mut stream_ids_in_order: Vec<String> = Vec::new();
        for stream_id in stream_id_of_each_request {
            if !stream_ids_in_order.contains(stream_id) {
                stream_ids_in_order.push(stream_id.clone());
            }
        }
        if mode == GraphEditMode::Live {
            self.refuse_an_input_set_twice_in_a_live_transaction(
                &requests,
                stream_id_of_each_request,
            )?;
        }
        if mode == GraphEditMode::Live
            && let Some(applied) = self.apply_input_values_set_live_apart_from_the_graph(
                &requests,
                stream_id_of_each_request,
            )?
        {
            return Ok(applied);
        }
        let project_repository = match mode {
            GraphEditMode::Live => None,
            GraphEditMode::Commit => Some(
                self.the_one_project_the_streams_belong_to(&stream_ids_in_order, stream_locations)
                    .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
            ),
        };
        let requests_and_the_stream_id_each_names: Vec<(StreamGraphAction, String)> = requests
            .into_iter()
            .zip(stream_id_of_each_request.iter().cloned())
            .collect();
        let saved_value_retypes_worked_out_before_the_edit = self
            .the_saved_value_retypes_worked_out_before_the_edit(
                &requests_and_the_stream_id_each_names,
                stream_locations,
                project_repository.as_ref(),
            );
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
            request_application::TheRequestsOfATransaction {
                requests_and_the_stream_id_each_names,
                saved_value_retypes_worked_out_before_the_edit:
                    &saved_value_retypes_worked_out_before_the_edit,
            },
            mode,
            author,
            idempotency_key,
            stream_locations,
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

    /// The location of each listed stream by its stream id.
    fn the_location_of_each_loaded_stream(&self) -> HashMap<String, StreamLocation> {
        the_location_of_each_listed_stream(self.operations_on_the_loaded_streams.list_streams())
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

/// The built-in convert node a transaction's `connect` places on a link of the running stream,
/// when its two ends' pin types connect only through one, or the refusal of such a link that
/// `connect` cannot carry through it; `None` for a link the running stream does not judge so,
/// which is connected, or refused, as it is.
type ConvertNodeALinkOfTheRunningStreamNeeds<'stream> = dyn Fn(&StreamGraphDocumentLink) -> Result<Option<ConvertNodeToInsert>, GraphRequestRefusal>
    + 'stream;

/// The refusal, before anything is written, of a link a transaction's `connect` cannot have the
/// running stream judge as it makes it; `Ok` for any other link, which the stream judges.
type RefusalOfALinkJudgedBeforeCommit<'stream> =
    dyn Fn(&StreamGraphDocumentLink) -> Result<(), GraphRequestRefusal> + 'stream;

/// What building a graph request's operation reads of the running stream and its project.
struct WhatAGraphRequestReadsOfItsStreamAndProject<'reads> {
    run_the_graph_generator: &'reads GraphGeneratorRunOfTheStream<'reads>,
    place_the_imported_stream: &'reads ImportedStreamPlacement<'reads>,
    document_of_a_placed_stream: &'reads DocumentOfAPlacedStream<'reads>,
    family_of_an_output_port: &'reads FamilyOfAnOutputPortOfTheRunningStream<'reads>,
    function_nodes_of_the_running_stream: &'reads FunctionNodesOfTheRunningStream<'reads>,
    convert_node_a_link_needs: &'reads ConvertNodeALinkOfTheRunningStreamNeeds<'reads>,
    refuse_a_link_judged_before_commit: &'reads RefusalOfALinkJudgedBeforeCommit<'reads>,
}

/// The UUID of each node `rendered_graph`, a stream's `graph` render, renders as a function
/// node, at any level of the fragments it holds.
fn the_function_nodes_of_the_render(rendered_graph: &Value) -> HashSet<Uuid> {
    let mut function_nodes = HashSet::new();
    the_function_nodes_rendered_in(rendered_graph, &mut function_nodes);
    function_nodes
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

/// The family wire name the output port `end` carries in `rendered_graph`, a stream's `graph`
/// render, its node at any level of the fragments the render holds: the family its pin type
/// travels on, else, for a port of no one type yet, the family the port's node type declares
/// it on, as `ports_of_a_node_type` reads the type.
fn the_family_of_an_output_port_in(
    rendered_graph: &Value,
    end: &StreamGraphDocumentLinkEnd,
    ports_of_a_node_type: &dyn Fn(&str) -> Option<streamlib::sdk::graph::GraphNodePorts>,
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
    let no_family = || {
        format!(
            "the running stream declares no family for the output port {node}/{}, so the \
             subgraph port carrying it cannot be declared",
            end.port
        )
    };
    let rendered_node =
        the_rendered_node_at_any_level(rendered_graph, &node).ok_or_else(no_family)?;
    let carried_by_its_pin_type = rendered_node["ports"]["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|port| port["name"] == end.port.as_str())
        .and_then(|port| {
            <streamlib::sdk::descriptors::PinType as serde::Deserialize>::deserialize(
                &port["pin_type"],
            )
            .ok()
        })
        .and_then(|pin_type| streamlib::sdk::descriptors::PortFamily::carrying(&pin_type));
    carried_by_its_pin_type
        .or_else(|| {
            ports_of_a_node_type(rendered_node["type"].as_str()?)?
                .outputs
                .into_iter()
                .find(|port| port.name == end.port)
                .map(|port| port.family.family)
        })
        .map(|family| family.wire_name().to_owned())
        .ok_or_else(no_family)
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

#[cfg(test)]
mod tests;
