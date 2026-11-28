// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::live_application_and_reversal::{
    close_the_graph_edits_of_each_stream, take_back_the_graph_edits_of_each_stream,
};
use super::placements_reconciled_in_the_batch::PlacementsReconciledInTheirBatches;
use super::*;
use streamlib::sdk::graph::input_values::InputValueToSetLive;

/// The requests of one transaction, each with the stream id it names, and what becomes of each
/// saved value they retype, worked out before the transaction took any lock.
pub(super) struct TheRequestsOfATransaction<'worked_out> {
    pub(super) requests_and_the_stream_id_each_names: Vec<(StreamGraphAction, String)>,
    pub(super) saved_value_retypes_worked_out_before_the_edit:
        &'worked_out SavedValueRetypesWorkedOutBeforeTheEdit,
}

impl StreamGraphTransactionExecutor<'_> {
    /// The graph document of the stream asset `stream_asset`, whose loaded stream
    /// `stream_operations` runs, as it stands: the one `project_repository` holds, or, with no
    /// repository, the one the stream's live graph renders.
    pub(super) fn the_stream_graph_document_as_it_stands(
        &self,
        project_repository: Option<&ProjectRepository>,
        stream_asset: Uuid,
        stream_operations: &dyn RuntimeOperations,
    ) -> Result<StreamGraphDocument, GraphTransactionRefusal> {
        match project_repository {
            Some(project_repository) => Ok(project_repository.stream_graph_document(stream_asset)?),
            None => StreamGraphDocument::from_value(
                self.async_runtime
                    .block_on(stream_operations.to_json_async())
                    .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
            )
            .map_err(GraphTransactionRefusal::of_the_whole_transaction),
        }
    }

    /// Apply each request, to the loaded stream it names, as one transaction by `author`, to the
    /// project `project_repository_held` holds when the transaction is committed.
    pub(super) fn apply_the_requests(
        &self,
        requests: TheRequestsOfATransaction<'_>,
        mode: GraphEditMode,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        stream_locations: &HashMap<String, StreamLocation>,
        project_repository_held: Option<&mut ProjectRepository>,
    ) -> Result<GraphTransactionToolResult, GraphTransactionRefusal> {
        let TheRequestsOfATransaction {
            requests_and_the_stream_id_each_names,
            saved_value_retypes_worked_out_before_the_edit,
        } = requests;
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

        let streams_to_edit = match project_repository_held {
            Some(_) => Some(
                self.operations_on_the_loaded_streams
                    .the_streams_to_read_or_edit(&stream_ids_in_order)
                    .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
            ),
            None => None,
        };
        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        let mut former_port_names_moved: Vec<ProjectOperation> = Vec::new();
        // A committed transaction also holds a batch open on each loaded stream an edit of the
        // project's streams reaches through a placement, so each is brought to what it places
        // within the transaction, and refuses it whole.
        let mut placements_not_named: BTreeMap<String, LoadedStreamHoldingAStreamFragment> =
            match &project_repository_held {
                Some(project_repository) => self
                    .the_placements_an_edit_can_reach(
                        self.the_loaded_streams_holding_a_stream_fragment_an_edit_reaches(
                            project_repository.project_directory(),
                        ),
                        &stream_ids_in_order,
                        stream_ids_in_order
                            .iter()
                            .filter_map(|stream_id| stream_locations.get(stream_id))
                            .filter(|stream_location| {
                                stream_location.project == project_repository.project_file().project
                            })
                            .map(|stream_location| stream_location.stream_asset),
                        project_repository,
                    )
                    .into_iter()
                    .map(|placement| (placement.stream_id.clone(), placement))
                    .collect(),
                None => BTreeMap::new(),
            };
        let mut placements: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        // Every batch opens before any edit is applied, in the order of the stream ids, so two
        // transactions editing the same streams never each wait on a batch the other holds.
        let mut stream_ids_a_batch_opens_on = stream_ids_in_order.clone();
        stream_ids_a_batch_opens_on.extend(placements_not_named.keys().cloned());
        stream_ids_a_batch_opens_on.sort();
        for stream_id in &stream_ids_a_batch_opens_on {
            if let Some(placement) = placements_not_named.remove(stream_id) {
                placements.insert(
                    stream_id.clone(),
                    self.a_placement_with_its_batch_open(
                        placement,
                        project_repository_held.as_deref(),
                    )?,
                );
                continue;
            }
            let stream_operations = match &streams_to_edit {
                Some(streams_to_edit) => streams_to_edit.runtime_operations_of(stream_id),
                None => self
                    .operations_on_the_loaded_streams
                    .runtime_operations_of_the_stream_a_call_names(stream_id),
            }
            .map_err(GraphTransactionRefusal::of_the_whole_transaction)?;
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
                    let mut staged_graph = self.the_stream_graph_document_as_it_stands(
                        Some(&**project_repository),
                        stream_location.stream_asset,
                        stream_operations.as_ref(),
                    )?;
                    move_each_former_port_name(
                        stream_location.stream_asset,
                        &mut staged_graph,
                        stream_operations.as_ref(),
                        &mut former_port_names_moved,
                    )?;
                    staged_graph
                }
                None => self.the_stream_graph_document_as_it_stands(
                    None,
                    stream_location.stream_asset,
                    stream_operations.as_ref(),
                )?,
            };
            streams.insert(
                stream_id.clone(),
                StreamATransactionEdits::new(stream_operations, stream_location, staged_graph)
                    .map_err(GraphTransactionRefusal::of_the_whole_transaction)?,
            );
        }

        let staged_graph_of_each_stream_not_running = match project_repository_held.as_deref() {
            Some(project_repository) => the_placed_streams_with_each_former_port_name_moved(
                &streams,
                project_repository,
                &mut former_port_names_moved,
            )?,
            None => BTreeMap::new(),
        };
        // Joined once the named streams' placed streams are staged, so a placement of a stream
        // no stream the transaction names places is never staged as one a request reaches.
        streams.extend(placements);

        let mut staged = WhatTheTransactionHasStaged {
            streams,
            staged_graph_of_each_stream_not_running,
            streams_taking_their_edits_once_the_documents_hold_them: BTreeSet::new(),
            project_repository: project_repository_held.as_deref(),
            node_by_label: HashMap::new(),
            what_the_answer_names_of_each_added_node: HashMap::new(),
            saved_value_retypes_worked_out_before_the_edit,
        };
        self.refuse_a_value_an_input_does_not_take_before_any_is_applied(
            &requests_and_the_stream_id_each_names,
            &staged,
        )?;

        let mut overridden_by_links: Vec<GraphTransactionInputOverriddenByALink> = Vec::new();
        let mut saved_values_retyped: Vec<GraphTransactionSavedValueRetyped> = Vec::new();
        let mut applied_live: Vec<(String, OneOperationAppliedLive)> = Vec::new();
        let mut operations_answered: Vec<(String, StreamGraphOperation)> = Vec::new();
        let mut why_each_link_was_made_unjudged = BTreeMap::new();
        let mut links_made_unjudged_as_kept = Vec::new();
        let mut project_operations: Vec<ProjectOperation> = Vec::new();
        let mut made_live_once_the_documents_hold_them: Vec<(String, StreamGraphOperation)> =
            Vec::new();
        for (request_index, (request, stream_id)) in requests_and_the_stream_id_each_names
            .into_iter()
            .enumerate()
        {
            let what_the_request_names =
                what_a_graph_action_names(&request).made_in(WhereAnOperationEdits {
                    stream_id: &stream_id,
                    level: request.level(),
                    level_holds_both_link_ends: self
                        .the_level_holds_both_ends_of_the_link_the_request_names(
                            &request, &stream_id, &staged,
                        ),
                });
            let applied = match self.apply_one_action(request, &stream_id, &mut staged) {
                Ok(applied) => applied,
                Err(refusal) => {
                    take_back_the_graph_edits_of_each_stream(&mut staged.streams);
                    self.put_back_what_the_refused_transaction_ran(
                        &staged.streams,
                        applied_live,
                        PlacementsReconciledInTheirBatches::default(),
                    );
                    return Err(GraphTransactionRefusal::of_the_operation(
                        request_index,
                        what_the_request_names,
                        refusal,
                    ));
                }
            };
            for applied in applied {
                why_each_link_was_made_unjudged.extend(
                    applied
                        .links_made_unjudged
                        .into_iter()
                        .map(|(link, why)| ((stream_id.clone(), link), why)),
                );
                links_made_unjudged_as_kept.extend(applied.links_made_unjudged_as_kept);
                operations_answered.push((stream_id.clone(), applied.operation_answered));
                project_operations.extend(applied.project_operations);
                made_live_once_the_documents_hold_them
                    .extend(applied.made_live_once_the_documents_hold_them);
                overridden_by_links.extend(applied.overridden_by_a_link);
                saved_values_retyped.extend(applied.saved_value_retyped);
                if let Some(applied_live_now) = applied.applied_live {
                    applied_live.push((stream_id.clone(), applied_live_now));
                }
            }
        }

        let mut placements_reconciled = PlacementsReconciledInTheirBatches::default();
        if let Some(project_repository) = staged.project_repository
            && let Err(refusal) = self.reconcile_each_placement_in_its_batch(
                &staged.streams,
                &stream_ids_in_order,
                &staged.staged_graph_of_each_stream_not_running,
                project_repository,
                &mut placements_reconciled,
            )
        {
            take_back_the_graph_edits_of_each_stream(&mut staged.streams);
            self.put_back_what_the_refused_transaction_ran(
                &staged.streams,
                applied_live,
                placements_reconciled,
            );
            return Err(refusal);
        }
        // A named stream brought to its graph as staged in its batch has taken each edit of its
        // own the documents would have handed it once they hold it.
        made_live_once_the_documents_hold_them
            .retain(|(stream_id, _)| !placements_reconciled.holds_a_named_stream(stream_id));
        if let Err(refusal) = close_the_graph_edits_of_each_stream(&mut staged.streams) {
            self.put_back_what_the_refused_transaction_ran(
                &staged.streams,
                applied_live,
                placements_reconciled,
            );
            return Err(GraphTransactionRefusal::of_the_whole_transaction(refusal));
        }
        let WhatTheTransactionHasStaged {
            streams,
            what_the_answer_names_of_each_added_node,
            ..
        } = staged;
        if !project_operations.is_empty() {
            project_operations.splice(0..0, former_port_names_moved);
        }
        let (transaction, commit) = match project_repository_held {
            None => (None, None),
            Some(_) if project_operations.is_empty() => (None, None),
            Some(project_repository) => {
                match project_repository.apply_project_operations_answered(
                    author,
                    idempotency_key,
                    ProjectTransactionAsSentAndAnswered {
                        actions_as_sent,
                        inputs_overridden_by_links: overridden_by_links.clone(),
                        nodes_inserted_on_links: the_nodes_inserted_on_links(
                            &what_the_answer_names_of_each_added_node,
                        ),
                        links_made_unjudged: links_made_unjudged_as_kept,
                    },
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
                        self.put_back_what_the_refused_transaction_ran(
                            &streams,
                            applied_live,
                            placements_reconciled,
                        );
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
            WhatAnAnswerNamesBesideItsOperations {
                what_the_answer_names_of_each_added_node: &what_the_answer_names_of_each_added_node,
                overridden_by_links,
                why_each_link_was_made_unjudged,
                saved_values_retyped,
            },
        ))
    }

    /// `placement`, a loaded stream an edit of the project's streams can reach through a
    /// placement, with a batch of graph edits opened on it, its graph staged as it stands.
    fn a_placement_with_its_batch_open(
        &self,
        placement: LoadedStreamHoldingAStreamFragment,
        project_repository: Option<&ProjectRepository>,
    ) -> Result<StreamATransactionEdits, GraphTransactionRefusal> {
        let staged_graph = self.the_graph_document_of_a_loaded_stream_as_it_stands(
            project_repository,
            &placement.stream_location,
            placement.stream_operations.as_ref(),
        )?;
        StreamATransactionEdits::new(
            placement.stream_operations,
            placement.stream_location,
            staged_graph,
        )
        .map_err(GraphTransactionRefusal::of_the_whole_transaction)
    }

    /// Whether the level `request`, an action on the loaded stream `stream_id`, edits holds, as
    /// the transaction has staged it, the node at each end of the link the request names, each
    /// named by its UUID and neither by the `as` of a node this transaction adds.
    fn the_level_holds_both_ends_of_the_link_the_request_names(
        &self,
        request: &StreamGraphAction,
        stream_id: &str,
        staged: &WhatTheTransactionHasStaged<'_>,
    ) -> bool {
        let (StreamGraphAction::Connect { source, target, .. }
        | StreamGraphAction::Disconnect { source, target, .. }) = request
        else {
            return false;
        };
        let Ok(reached) = self.the_level_a_request_reaches(
            stream_id,
            request.level(),
            &staged.streams,
            &staged.staged_graph_of_each_stream_not_running,
            staged.project_repository,
        ) else {
            return false;
        };
        let Ok(level_as_drawn) = reached.the_document_as_drawn() else {
            return false;
        };
        the_level_holds_both_ends_by_uuid(&level_as_drawn, source, target, &staged.node_by_label)
    }

    /// The level of the loaded stream `stream_id` that `level`, a path of placed streams,
    /// reaches, as the transaction has staged each document on the way.
    pub(super) fn the_level_a_request_reaches(
        &self,
        stream_id: &str,
        level: &[Uuid],
        streams: &BTreeMap<String, StreamATransactionEdits>,
        staged_graph_of_each_stream_not_running: &BTreeMap<Uuid, StreamGraphDocument>,
        project_repository: Option<&ProjectRepository>,
    ) -> Result<StreamGraphLevelOfAPlacedStream, GraphRequestRefusal> {
        let stream = streams
            .get(stream_id)
            .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
        the_level_reached(
            stream.staged_graph.clone(),
            stream.stream_location.stream_asset,
            level,
            &|placed_stream| {
                let project_repository = project_repository.ok_or_else(|| {
                    format!("an edit inside a placed stream is refused live: {DOCUMENTS_FIRST}")
                })?;
                the_staged_graph_of_the_stream(
                    placed_stream,
                    streams,
                    staged_graph_of_each_stream_not_running,
                    project_repository,
                )
            },
        )
    }

    /// Apply `request`, an action on the loaded stream `stream_id`: an edit of its own level
    /// made on its running stream and staged on its document, and one that reaches a running
    /// stream only from the documents — inside a placed stream, or placing one — staged on the
    /// documents it changes, each loaded stream it changes taking it once they are written. A
    /// stream of `streams_taking_their_edits_once_the_documents_hold_them` takes each later edit
    /// that way too, so its running stream takes the transaction's edits in order.
    pub(super) fn apply_one_request(
        &self,
        request: StreamGraphAction,
        stream_id: &str,
        staged: &mut WhatTheTransactionHasStaged<'_>,
    ) -> Result<OneGraphRequestApplied, GraphRequestRefusal> {
        let WhatTheTransactionHasStaged {
            streams,
            staged_graph_of_each_stream_not_running,
            streams_taking_their_edits_once_the_documents_hold_them,
            project_repository,
            node_by_label,
            what_the_answer_names_of_each_added_node,
            ..
        } = staged;
        let project_repository = *project_repository;
        let label = match &request {
            StreamGraphAction::AddNode { label, .. }
            | StreamGraphAction::ImportGraphGenerator { label, .. }
            | StreamGraphAction::UseDependencyStream { label, .. }
            | StreamGraphAction::CollapseSelectionIntoSubgraph { label, .. } => label.clone(),
            _ => None,
        };
        let level = request.level().to_vec();
        let stream = streams
            .get(stream_id)
            .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
        let stream_operations = Arc::clone(&stream.stream_operations);
        let stream_asset = stream.stream_location.stream_asset;
        let reached = self.the_level_a_request_reaches(
            stream_id,
            &level,
            streams,
            staged_graph_of_each_stream_not_running,
            project_repository,
        )?;
        let level_as_drawn = reached.the_document_as_drawn()?;
        let request = the_request_under_the_current_port_names(
            request,
            &level_as_drawn,
            node_by_label,
            stream_operations.as_ref(),
        );

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
                    .map_err(GraphRequestRefusal::from)
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
                    DOCUMENTS_FIRST
                )
            })?;
            the_document_a_placed_stream_holds(
                held_stream,
                streams,
                staged_graph_of_each_stream_not_running,
                project_repository,
            )
        };
        let pin_type_of_an_output_port = |end: &StreamGraphDocumentLinkEnd| {
            let rendered_graph = self
                .async_runtime
                .block_on(stream_operations.to_json_async())?;
            Ok(the_pin_type_of_an_output_port_in(
                &rendered_graph,
                end,
                &|node_type| stream_operations.the_ports_a_node_type_declares(node_type),
            )?)
        };
        let function_nodes_of_the_running_stream = || {
            let rendered_graph = self
                .async_runtime
                .block_on(stream_operations.to_json_async())?;
            Ok(the_function_nodes_of_the_render(&rendered_graph))
        };
        let the_stream_takes_its_edits_once_the_documents_hold_them =
            streams_taking_their_edits_once_the_documents_hold_them.contains(stream_id);
        let judged_by_node_types_before_commit = if !reached.is_the_streams_own_level() {
            Some(WhyConnectPlacesNoConvertNode::TheLinkIsAtAPlacedLevel)
        } else if the_stream_takes_its_edits_once_the_documents_hold_them {
            Some(WhyConnectPlacesNoConvertNode::TheStreamTakesItsEditsLater)
        } else {
            None
        };
        let refuse_a_link_judged_before_commit = |link: &StreamGraphDocumentLink| {
            judged_by_node_types_before_commit.map_or(Ok(()), |because| {
                refuse_a_link_judged_by_the_node_types_of_its_level(
                    stream_operations.as_ref(),
                    &level_as_drawn,
                    link,
                    because,
                )
            })
        };
        let convert_node_a_link_needs = |link: &StreamGraphDocumentLink| {
            if !reached.is_the_streams_own_level() {
                // Judged once the edit is made on the level's document, which refuses a link
                // only a convert node bridges there.
                return Ok(None);
            }
            if judged_by_node_types_before_commit.is_some() {
                refuse_a_link_judged_before_commit(link)?;
                return Ok(None);
            }
            the_convert_node_a_live_connect_places(stream_operations.as_ref(), link)
        };
        let mut placed_stream_a_collapse_saves = None;
        let mut node_inserted_because = None;
        let mut edit_as_drawn = graph_operation_for_the_request(
            request,
            &level_as_drawn,
            node_by_label,
            &WhatAGraphRequestReadsOfItsStreamAndProject {
                run_the_graph_generator: &run_the_graph_generator,
                place_the_imported_stream: &place_the_imported_stream,
                document_of_a_placed_stream: &document_of_a_placed_stream,
                pin_type_of_an_output_port: &pin_type_of_an_output_port,
                function_nodes_of_the_running_stream: &function_nodes_of_the_running_stream,
                convert_node_a_link_needs: &convert_node_a_link_needs,
                refuse_a_link_judged_before_commit: &refuse_a_link_judged_before_commit,
            },
            &mut placed_stream_a_collapse_saves,
            &mut node_inserted_because,
        )?;
        if let StreamGraphOperation::Connect { link } = &edit_as_drawn {
            refuse_a_link_the_graph_already_holds(&level_as_drawn, link)?;
        }
        let overridden_by_a_link = match &edit_as_drawn {
            StreamGraphOperation::SetNodeInputValue {
                node,
                input,
                member,
                to,
                ..
            } => {
                let set_on = level_as_drawn.node(*node)?;
                if its_input_values_are_checked(&set_on) {
                    refuse_a_value_the_input_does_not_take(
                        stream_operations.as_ref(),
                        &set_on,
                        &input_values_after_the_set(
                            &set_on.input_values,
                            input,
                            member.as_deref(),
                            to.as_ref(),
                        ),
                        input,
                        member.as_deref(),
                        to.as_ref(),
                        Some("/value"),
                    )?;
                }
                to.as_ref().and_then(|_| {
                    the_link_overriding_the_input(&level_as_drawn, stream_id, *node, input)
                })
            }
            _ => None,
        };
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
                .the_name_a_node_added_with_none_takes(&node.node_type, &names_at_the_level)?;
        }

        let mut project_operations = Vec::new();
        if let Some(collapsed) = placed_stream_a_collapse_saves {
            project_operations.push(collapsed.creation);
            staged_graph_of_each_stream_not_running
                .insert(collapsed.placed_stream, collapsed.document);
        }
        if made_on_the_running_stream_now {
            let value_the_running_input_held = match &edit_as_drawn {
                StreamGraphOperation::SetNodeInputValue { node, input, .. } => {
                    Some(the_value_a_running_node_holds_on_the_input(
                        &self
                            .async_runtime
                            .block_on(stream_operations.to_json_async())?,
                        *node,
                        input,
                    ))
                }
                _ => None,
            };
            let graph_edit_batch = the_open_batch_of(streams, stream_id)?;
            let applied = self
                .async_runtime
                .block_on(graph_edit_batch.apply_a_stream_graph_operation_live(edit_as_drawn))?;
            let applied_on_the_running_stream = match (&applied, value_the_running_input_held) {
                (
                    StreamGraphOperation::SetNodeInputValue {
                        node,
                        input,
                        member,
                        to,
                        ..
                    },
                    Some(held),
                ) => OneOperationAppliedLive {
                    operation: StreamGraphOperation::SetNodeInputValue {
                        node: *node,
                        input: input.clone(),
                        member: member.clone(),
                        from: the_value_held_on_the_input_or_its_member(
                            held.saved.as_ref(),
                            member.as_deref(),
                        )
                        .cloned(),
                        to: to.clone(),
                    },
                    live_value_it_replaced: held.live.map(|value| InputValueToSetLive {
                        node: *node,
                        input: input.clone(),
                        member: None,
                        value: Some(value),
                    }),
                },
                _ => OneOperationAppliedLive::replacing_no_live_value(applied.clone()),
            };
            let staged = streams
                .get_mut(stream_id)
                .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?
                .staged_graph
                .apply(&applied);
            if let Err(why) = staged {
                self.reverse_one_operation_applied_live(
                    stream_id,
                    stream_operations.as_ref(),
                    the_open_batch_of(streams, stream_id).ok(),
                    &applied_on_the_running_stream,
                );
                return Err(format!(
                    "stream `{stream_id}`'s graph document does not hold what its running graph \
                     does: {why}"
                )
                .into());
            }
            record_what_the_answer_names_of_the_node_the_operation_adds(
                label,
                node_inserted_because,
                &applied,
                &|own| own,
                what_the_answer_names_of_each_added_node,
            );
            project_operations.push(ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: stream_asset,
                    graph_operation: applied.clone(),
                },
            ));
            return Ok(OneGraphRequestApplied {
                operation_answered: applied,
                applied_live: Some(applied_on_the_running_stream),
                project_operations,
                made_live_once_the_documents_hold_them: Vec::new(),
                overridden_by_a_link,
                links_made_unjudged: Vec::new(),
                links_made_unjudged_as_kept: Vec::new(),
                saved_value_retyped: None,
            });
        }

        if project_repository.is_none() {
            return Err(format!(
                "the edit places a stream, or is made inside one, and is refused live: {}",
                DOCUMENTS_FIRST
            )
            .into());
        }
        let edit = reached.the_operation_on_the_document(edit_as_drawn)?;
        if let Some((node_type, node_name, input_values)) =
            the_graph_node_an_edit_adds_or_configures(&edit, &reached.document)
        {
            self.async_runtime.block_on(
                stream_operations.refuse_a_node_a_live_add_would_refuse(
                    &node_type,
                    &node_name,
                    &input_values,
                ),
            )?;
        }
        let mut placed_document_after_the_edit = reached.document.clone();
        placed_document_after_the_edit.apply(&edit)?;
        let why_each_link_goes_unjudged = if reached.is_the_streams_own_level() {
            Vec::new()
        } else {
            why_each_link_an_edit_makes_at_a_placed_level_goes_unjudged(
                stream_operations.as_ref(),
                &reached.document,
                &edit,
            )?
        };
        let operation_answered =
            reached.the_operation_as_drawn(&edit, &placed_document_after_the_edit)?;
        let mut links_made_unjudged = Vec::new();
        let mut links_made_unjudged_as_kept = Vec::new();
        for ((drawn, kept), why) in the_links_an_operation_makes(&operation_answered)
            .iter()
            .zip(the_links_an_operation_makes(&edit))
            .zip(why_each_link_goes_unjudged)
        {
            let Some(why) = why else { continue };
            links_made_unjudged.push((drawn.clone(), why.clone()));
            links_made_unjudged_as_kept.push(ProjectTransactionLinkMadeUnjudged {
                stream: reached.stream,
                link: kept.clone(),
                why,
            });
        }
        record_what_the_answer_names_of_the_node_the_operation_adds(
            label,
            node_inserted_because,
            &edit,
            &|own| reached.the_drawn_uuid_of(own),
            what_the_answer_names_of_each_added_node,
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
            overridden_by_a_link,
            links_made_unjudged,
            links_made_unjudged_as_kept,
            saved_value_retyped: None,
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
    ) -> Result<Vec<(Uuid, StreamGraphOperation)>, GraphRequestRefusal> {
        if reached.is_the_streams_own_level() {
            return Ok(Vec::new());
        }
        let mut stream_assets_of_the_project: BTreeSet<Uuid> = project_repository
            .assets()?
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
}

/// The level `level` of placed streams reaches from `document`, the graph document of the
/// stream asset `stream`, each placed stream's document as `document_of_the_stream` reads it;
/// refused as that read refuses, or as the path refuses.
pub(super) fn the_level_reached(
    document: StreamGraphDocument,
    stream: Uuid,
    level: &[Uuid],
    document_of_the_stream: &dyn Fn(Uuid) -> Result<StreamGraphDocument, GraphRequestRefusal>,
) -> Result<StreamGraphLevelOfAPlacedStream, GraphRequestRefusal> {
    let kept = std::cell::RefCell::new(None);
    document
        .the_level_a_path_of_placed_streams_reaches(stream, level, &mut |placed_stream| {
            document_of_the_stream(placed_stream).map_err(|refusal| refusal.words_kept_in(&kept))
        })
        .map_err(|why| GraphRequestRefusal::kept_in_or(&kept, why))
}
