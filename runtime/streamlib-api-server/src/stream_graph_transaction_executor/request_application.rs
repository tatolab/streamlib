// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl StreamGraphTransactionExecutor<'_> {
    /// Apply each request, to the loaded stream it names, as one transaction by `author`, to the
    /// project `project_repository_held` holds when the transaction is committed.
    pub(super) fn apply_the_requests(
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

        self.refuse_a_value_an_input_does_not_take_before_any_is_applied(
            &requests_and_the_stream_id_each_names,
            &streams,
        )?;

        let mut node_by_label: HashMap<String, Uuid> = HashMap::new();
        let mut overridden_by_links: Vec<GraphTransactionInputOverriddenByALink> = Vec::new();
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
            let applied = self.apply_one_action(
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
            for applied in applied {
                operations_answered.push((stream_id.clone(), applied.operation_answered));
                project_operations.extend(applied.project_operations);
                made_live_once_the_documents_hold_them
                    .extend(applied.made_live_once_the_documents_hold_them);
                overridden_by_links.extend(applied.overridden_by_a_link);
                if let Some(applied_live_now) = applied.applied_live {
                    applied_live.push((stream_id.clone(), applied_live_now));
                }
            }
        }

        let (transaction, commit) = match project_repository_held {
            None => (None, None),
            Some(_) if project_operations.is_empty() => (None, None),
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
            overridden_by_links,
        ))
    }

    /// Refuse, before any edit is applied, a value set on an input of a node of a stream's own
    /// level that the input does not take; the value is checked again when it is applied, against
    /// the node as the transaction has staged it by then.
    fn refuse_a_value_an_input_does_not_take_before_any_is_applied(
        &self,
        requests_and_the_stream_id_each_names: &[(StreamGraphAction, String)],
        streams: &BTreeMap<String, StreamATransactionEdits>,
    ) -> Result<(), GraphTransactionRefusal> {
        for (request_index, (request, stream_id)) in
            requests_and_the_stream_id_each_names.iter().enumerate()
        {
            let (StreamGraphAction::SetNodeInputValue {
                level,
                node,
                input,
                value,
                ..
            }
            | StreamGraphAction::SetPinValue {
                level,
                node,
                input,
                value,
                ..
            }) = request
            else {
                continue;
            };
            let Some(stream) = streams.get(stream_id) else {
                continue;
            };
            let Some(set_on) = Uuid::parse_str(node)
                .ok()
                .filter(|_| level.is_empty())
                .and_then(|node| stream.staged_graph.node(node).ok())
            else {
                continue;
            };
            let value = value.as_ref().filter(|value| !value.is_null());
            let mut input_values_after_the_set = set_on.input_values.clone();
            match value {
                Some(value) => input_values_after_the_set.insert(input.clone(), value.clone()),
                None => input_values_after_the_set.remove(input),
            };
            refuse_a_value_the_input_does_not_take(
                stream.stream_operations.as_ref(),
                &set_on,
                &input_values_after_the_set,
                input,
                value,
                "/value",
            )
            .map_err(|refusal| {
                GraphTransactionRefusal::of_the_operation(
                    request_index,
                    what_a_graph_action_names(request),
                    refusal,
                )
            })?;
        }
        Ok(())
    }

    /// Apply `request` as [`Self::apply_one_request`] does, as each operation it stands for: a
    /// `set_node_config` sets each input value it names that differs and, on a node type that
    /// takes a config, takes off each config field it does not name; a `set_exposed_config_fields`
    /// on a node type that takes no config records nothing.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate piece of the transaction the caller already holds"
    )]
    fn apply_one_action(
        &self,
        request: StreamGraphAction,
        stream_id: &str,
        streams: &mut BTreeMap<String, StreamATransactionEdits>,
        staged_graph_of_each_stream_not_running: &mut BTreeMap<Uuid, StreamGraphDocument>,
        streams_taking_their_edits_once_the_documents_hold_them: &mut BTreeSet<String>,
        project_repository: Option<&ProjectRepository>,
        node_by_label: &mut HashMap<String, Uuid>,
        labels_of_added_nodes: &mut HashMap<Uuid, String>,
    ) -> Result<Vec<OneGraphRequestApplied>, GraphRequestRefusal> {
        let node_set_on = |level: &[Uuid], node: &str, streams: &BTreeMap<_, _>, staged: &_| {
            let node = node_by_label
                .get(node)
                .copied()
                .or_else(|| Uuid::parse_str(node).ok())?;
            self.the_level_a_request_reaches(stream_id, level, streams, staged, project_repository)
                .ok()?
                .the_document_as_drawn()
                .ok()?
                .node(node)
                .ok()
                .filter(|held| {
                    held.fragment.is_none()
                        && held.node_type != STREAM_FRAGMENT_NODE_TYPE
                        && !is_a_graph_interface_node_type(&held.node_type)
                })
        };
        let stream_operations = streams
            .get(stream_id)
            .map(|stream| Arc::clone(&stream.stream_operations))
            .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
        let sets_of_a_config = match &request {
            StreamGraphAction::SetNodeConfig {
                stream,
                level,
                node,
                config,
            } => node_set_on(
                level,
                node,
                streams,
                staged_graph_of_each_stream_not_running,
            )
            .map(|configured| {
                let sets = the_input_value_sets_a_config_stands_for(
                    stream_operations.as_ref(),
                    &configured,
                    config,
                )?;
                Ok::<_, GraphRequestRefusal>(
                    sets.into_iter()
                        .map(|(input, value)| StreamGraphAction::SetNodeInputValue {
                            stream: *stream,
                            level: level.clone(),
                            node: node.clone(),
                            input,
                            value,
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .transpose()?,
            StreamGraphAction::SetExposedConfigFields {
                level,
                node,
                exposed_config_fields,
                ..
            } => {
                if let Some(exposed_on) = node_set_on(
                    level,
                    node,
                    streams,
                    staged_graph_of_each_stream_not_running,
                ) && !stream_operations
                    .an_exposure_of_config_fields_is_recorded_on(
                        &exposed_on.node_type,
                        exposed_config_fields,
                    )
                    .map_err(|refusal| refusal.to_string())?
                {
                    return Ok(Vec::new());
                }
                None
            }
            _ => None,
        };
        let Some(sets_of_a_config) = sets_of_a_config else {
            return self
                .apply_one_request(
                    request,
                    stream_id,
                    streams,
                    staged_graph_of_each_stream_not_running,
                    streams_taking_their_edits_once_the_documents_hold_them,
                    project_repository,
                    node_by_label,
                    labels_of_added_nodes,
                    InputValueCheck::MadeOnApply,
                )
                .map(|applied| vec![applied]);
        };
        let mut applied_by_the_config = Vec::with_capacity(sets_of_a_config.len());
        for set in sets_of_a_config {
            let StreamGraphAction::SetNodeInputValue { input, .. } = &set else {
                continue;
            };
            let input = input.clone();
            match self.apply_one_request(
                set,
                stream_id,
                streams,
                staged_graph_of_each_stream_not_running,
                streams_taking_their_edits_once_the_documents_hold_them,
                project_repository,
                node_by_label,
                labels_of_added_nodes,
                InputValueCheck::MadeBefore,
            ) {
                Ok(applied) => applied_by_the_config.push(applied),
                Err(refusal) => {
                    let applied_live = applied_by_the_config
                        .into_iter()
                        .filter_map(|applied: OneGraphRequestApplied| {
                            Some((stream_id.to_owned(), applied.applied_live?))
                        })
                        .collect();
                    self.reverse_what_was_applied_live(streams, applied_live);
                    return Err(refusal.of_the_field(&input));
                }
            }
        }
        Ok(applied_by_the_config)
    }

    /// The level of the loaded stream `stream_id` that `level`, a path of placed streams,
    /// reaches, as the transaction has staged each document on the way.
    fn the_level_a_request_reaches(
        &self,
        stream_id: &str,
        level: &[Uuid],
        streams: &BTreeMap<String, StreamATransactionEdits>,
        staged_graph_of_each_stream_not_running: &BTreeMap<Uuid, StreamGraphDocument>,
        project_repository: Option<&ProjectRepository>,
    ) -> Result<StreamGraphLevelOfAPlacedStream, String> {
        let stream = streams
            .get(stream_id)
            .ok_or_else(|| format!("stream `{stream_id}` is not loaded"))?;
        stream
            .staged_graph
            .clone()
            .the_level_a_path_of_placed_streams_reaches(
                stream.stream_location.stream_asset,
                level,
                &mut |placed_stream| {
                    let project_repository = project_repository.ok_or_else(|| {
                        "an edit inside a placed stream is refused live: it is made on the \
                         project's graph documents, with `mode` `commit`"
                            .to_owned()
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
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate piece of the transaction the caller already holds"
    )]
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
        input_value_check: InputValueCheck,
    ) -> Result<OneGraphRequestApplied, GraphRequestRefusal> {
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
        let reached = self.the_level_a_request_reaches(
            stream_id,
            &level,
            streams,
            staged_graph_of_each_stream_not_running,
            project_repository,
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
        let config_a_node_takes = |node_type: &str, input_values: &Map<String, Value>| {
            stream_operations
                .the_config_a_node_holding_the_input_values_takes(node_type, input_values)
                .map_err(|refusal| refusal.to_string())
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
                config_a_node_takes: &config_a_node_takes,
            },
            &mut placed_stream_a_collapse_saves,
        )?;
        let overridden_by_a_link = match &edit_as_drawn {
            StreamGraphOperation::SetNodeInputValue {
                node,
                input,
                from: _,
                to,
            } => {
                let set_on = level_as_drawn.node(*node)?;
                if input_value_check == InputValueCheck::MadeOnApply
                    && set_on.fragment.is_none()
                    && set_on.node_type != STREAM_FRAGMENT_NODE_TYPE
                    && !is_a_graph_interface_node_type(&set_on.node_type)
                {
                    let mut input_values_after_the_set = set_on.input_values.clone();
                    match to {
                        Some(to) => input_values_after_the_set.insert(input.clone(), to.clone()),
                        None => input_values_after_the_set.remove(input),
                    };
                    refuse_a_value_the_input_does_not_take(
                        stream_operations.as_ref(),
                        &set_on,
                        &input_values_after_the_set,
                        input,
                        to.as_ref(),
                        "/value",
                    )?;
                }
                to.as_ref().and_then(|_| {
                    level_as_drawn
                        .links_touching(*node)
                        .into_iter()
                        .find(|link| link.target.node == *node && link.target.port == *input)
                        .map(|link| GraphTransactionInputOverriddenByALink {
                            stream: stream_id.to_owned(),
                            node: node.to_string(),
                            input: input.clone(),
                            from_node: link.source.node.to_string(),
                            from_output: link.source.port,
                        })
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
                )
                .into());
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
                overridden_by_a_link,
            });
        }

        if project_repository.is_none() {
            return Err(format!(
                "the edit places a stream, or is made inside one, and is refused live: {}",
                documents_first()
            )
            .into());
        }
        let edit = reached.the_operation_on_the_document(edit_as_drawn)?;
        if let Some((node_type, node_name, input_values)) =
            the_graph_node_an_edit_adds_or_configures(&edit, &reached.document)
        {
            self.async_runtime
                .block_on(stream_operations.refuse_a_node_a_live_add_would_refuse(
                    &node_type,
                    &node_name,
                    &input_values,
                ))
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
            overridden_by_a_link,
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
}

/// Whether [`StreamGraphTransactionExecutor::apply_one_request`] checks a value it sets on an
/// input, or the caller checked it already against every value its action sets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputValueCheck {
    MadeOnApply,
    MadeBefore,
}

/// Refuse `value`, set on input `input` of `set_on` with `None` taking it off, when the input
/// does not take it, as the stream `stream_operations` runs resolves the node's type; the value
/// is at `refused_params_pointer` in the action's params.
fn refuse_a_value_the_input_does_not_take(
    stream_operations: &dyn RuntimeOperations,
    set_on: &StreamGraphDocumentNode,
    input_values_after_the_set: &Map<String, Value>,
    input: &str,
    value: Option<&Value>,
    refused_params_pointer: &str,
) -> Result<(), GraphRequestRefusal> {
    stream_operations
        .refuse_an_input_value_its_input_does_not_take(
            &set_on.node_type,
            &set_on.name,
            input_values_after_the_set,
            input,
            value,
        )
        .map_err(|refusal| {
            GraphRequestRefusal::of_an_input_value(refusal, refused_params_pointer.to_owned())
        })
}

/// The input values `config`, sent to `set_node_config`, sets on `configured`, checked: each it
/// names that differs from what the node holds, and, on a node type that takes a config, each
/// config field it does not name taken off.
fn the_input_value_sets_a_config_stands_for(
    stream_operations: &dyn RuntimeOperations,
    configured: &StreamGraphDocumentNode,
    config: &Map<String, Value>,
) -> Result<Vec<(String, Option<Value>)>, GraphRequestRefusal> {
    let takes_a_config = stream_operations
        .an_exposure_of_config_fields_is_recorded_on(&configured.node_type, &[])
        .map_err(|refusal| refusal.to_string())?;
    let mut sets: Vec<(String, Option<Value>)> = config
        .iter()
        .map(|(input, value)| {
            (
                input.clone(),
                Some(value.clone()).filter(|value| !value.is_null()),
            )
        })
        .filter(|(input, value)| configured.input_values.get(input) != value.as_ref())
        .collect();
    if takes_a_config {
        let config_held = stream_operations
            .the_config_a_node_holding_the_input_values_takes(
                &configured.node_type,
                &configured.input_values,
            )
            .map_err(|refusal| refusal.to_string())?;
        sets.extend(
            config_held
                .as_object()
                .into_iter()
                .flat_map(|config_held| config_held.keys())
                .filter(|input| {
                    !config.contains_key(*input) && configured.input_values.contains_key(*input)
                })
                .map(|input| (input.clone(), None)),
        );
    }
    let mut input_values_after_the_sets = configured.input_values.clone();
    for (input, value) in &sets {
        match value {
            Some(value) => input_values_after_the_sets.insert(input.clone(), value.clone()),
            None => input_values_after_the_sets.remove(input),
        };
    }
    for (input, value) in &sets {
        refuse_a_value_the_input_does_not_take(
            stream_operations,
            configured,
            &input_values_after_the_sets,
            input,
            value.as_ref(),
            &format!("/config/{}", input.replace('~', "~0").replace('/', "~1")),
        )
        .map_err(|refusal| refusal.of_the_field(input))?;
    }
    Ok(sets)
}
