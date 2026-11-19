// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use streamlib::sdk::runtime::GraphEditBatch;

/// One loaded stream a transaction edits: its operations, where it lives, and its graph as the
/// transaction has staged it.
pub(super) struct StreamATransactionEdits {
    pub(super) stream_operations: Arc<dyn RuntimeOperations>,
    pub(super) stream_location: StreamLocation,
    pub(super) staged_graph: StreamGraphDocument,
    /// The batch of graph edits the transaction makes on the running stream, open until the
    /// transaction closes it or takes it back.
    pub(super) graph_edit_batch: Option<Box<dyn GraphEditBatch>>,
    /// Whether the running stream took back the graph edits of its batch, so only the values
    /// they set are left to reverse.
    pub(super) its_graph_edits_were_taken_back: bool,
}

impl StreamGraphTransactionExecutor<'_> {
    /// Make each of `operations`, each by the loaded stream it changes, on that running stream,
    /// once the project's graph documents hold it; a running stream that refuses one is logged
    /// and takes it at its next load.
    pub(super) fn make_live_what_the_documents_now_hold(
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
        GraphRequestRefusal,
    > {
        let project_repository = self
            .project_repositories
            .the_project_repository_at(project_directory)?;
        let mut project_repository = project_repository.lock();
        let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
        if let Some(applied_before) = project_repository
            .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key.as_ref())?
        {
            let undo_state_of_each_transaction =
                project_repository.the_undo_state_of_each_transaction()?;
            return Ok(project_transaction_on_the_wire(
                applied_before,
                &undo_state_of_each_transaction,
            ));
        }
        let reversal = reversal_of(&project_repository, author)?;
        let project = project_repository.project_file().project;
        let stream_id_of_each_stream_asset =
            self.the_stream_id_of_each_loaded_stream_asset(project);
        let mut streams: BTreeMap<String, StreamATransactionEdits> = BTreeMap::new();
        let mut applied_live: Vec<(String, OneOperationAppliedLive)> = Vec::new();
        let mut made_live_once_the_documents_hold_them: Vec<(String, StreamGraphOperation)> =
            Vec::new();
        let mut streams_taking_their_operations_once_the_documents_hold_them: BTreeSet<String> =
            BTreeSet::new();
        self.refuse_an_input_value_set_its_input_no_longer_takes(
            &project_repository,
            &stream_id_of_each_stream_asset,
            &reversal.operations,
        )?;
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
                if !streams.contains_key(stream_id) {
                    let stream_operations = self
                        .operations_on_the_loaded_streams
                        .runtime_operations_of_the_stream_a_call_names(stream_id)?;
                    streams.insert(
                        stream_id.clone(),
                        StreamATransactionEdits {
                            graph_edit_batch: Some(
                                Arc::clone(&stream_operations).open_a_graph_edit_batch(),
                            ),
                            its_graph_edits_were_taken_back: false,
                            stream_operations,
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
                }
                let graph_edit_batch = streams
                    .get(stream_id)
                    .and_then(|stream| stream.graph_edit_batch.as_deref())
                    .ok_or_else(|| {
                        format!(
                            "stream `{stream_id}` holds no batch of graph edits open to apply to"
                        )
                    })?;
                self.async_runtime
                    .block_on(
                        graph_edit_batch
                            .apply_a_stream_graph_operation_live(graph_operation.clone()),
                    )
                    .map_err(GraphRequestRefusal::from)
                    .map(|applied| {
                        (
                            stream_id.clone(),
                            OneOperationAppliedLive::replacing_no_live_value(applied),
                        )
                    })
            };
            let applied = applied_to_the_running_stream();
            match applied {
                Ok(applied) => applied_live.push(applied),
                Err(refusal) => {
                    take_back_the_graph_edits_of_each_stream(&mut streams);
                    self.reverse_what_was_applied_live(&streams, applied_live);
                    return Err(refusal);
                }
            }
        }
        if let Err(refusal) = close_the_graph_edits_of_each_stream(&mut streams) {
            self.reverse_what_was_applied_live(&streams, applied_live);
            return Err(refusal);
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
                let undo_state_of_each_transaction =
                    project_repository.the_undo_state_of_each_transaction()?;
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
                Err(refusal.into())
            }
        }
    }

    /// Refuse, before any of `operations` is applied, an input value one of them sets on a node
    /// of a running stream when the input no longer takes it, as the stream resolves the node's
    /// type now: an undo or redo of an edit whose input was taken out since stops there, in the
    /// runtime's words. Each node holds every value an earlier set of `operations` left it.
    fn refuse_an_input_value_set_its_input_no_longer_takes(
        &self,
        project_repository: &ProjectRepository,
        stream_id_of_each_stream_asset: &HashMap<Uuid, String>,
        operations: &[ProjectOperation],
    ) -> Result<(), GraphRequestRefusal> {
        let mut node_as_the_operations_leave_it: HashMap<(Uuid, Uuid), StreamGraphDocumentNode> =
            HashMap::new();
        for operation in operations {
            let ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation:
                    StreamGraphOperation::SetNodeInputValue {
                        node, input, to, ..
                    },
            }) = operation
            else {
                continue;
            };
            let Some(stream_id) = stream_id_of_each_stream_asset.get(stream) else {
                continue;
            };
            let held = match node_as_the_operations_leave_it.remove(&(*stream, *node)) {
                Some(held) => Some(held),
                None => project_repository
                    .stream_graph_document(*stream)
                    .ok()
                    .and_then(|document| document.node(*node).ok()),
            };
            let Some(mut set_on) = held.filter(its_input_values_are_checked) else {
                continue;
            };
            let stream_operations = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(stream_id)?;
            let input_values_after_the_set =
                input_values_after_the_set(&set_on.input_values, input, to.as_ref());
            refuse_a_value_the_input_does_not_take(
                stream_operations.as_ref(),
                &set_on,
                &input_values_after_the_set,
                input,
                to.as_ref(),
                None,
            )?;
            set_on.input_values = input_values_after_the_set;
            node_as_the_operations_leave_it.insert((*stream, *node), set_on);
        }
        Ok(())
    }

    /// Apply the inverse of each operation of `applied_live`, newest first, to the running
    /// stream it was applied to, setting live again each value set live it replaced, so a
    /// refused transaction leaves the streams as they were. A stream that took back the graph
    /// edits of its batch has only the values they set left to reverse.
    pub(super) fn reverse_what_was_applied_live(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        applied_live: Vec<(String, OneOperationAppliedLive)>,
    ) {
        for (stream_id, applied) in applied_live.into_iter().rev() {
            let Some(stream) = streams.get(&stream_id) else {
                continue;
            };
            if stream.its_graph_edits_were_taken_back
                && !sets_a_value_a_take_back_leaves(&applied.operation)
            {
                continue;
            }
            self.reverse_one_operation_applied_live(
                &stream_id,
                stream.stream_operations.as_ref(),
                &applied,
            );
        }
    }

    /// Reverse `applied` on the running stream `stream_operations` serves: apply its inverse,
    /// then set live again the live value it replaced, logging a step the stream refuses.
    pub(super) fn reverse_one_operation_applied_live(
        &self,
        stream_id: &str,
        stream_operations: &dyn RuntimeOperations,
        applied: &OneOperationAppliedLive,
    ) {
        if let Err(not_reversed) = self.async_runtime.block_on(
            stream_operations.apply_a_stream_graph_operation_live(applied.operation.inverse()),
        ) {
            tracing::error!(
                stream = %stream_id,
                %not_reversed,
                "could not reverse part of a refused graph transaction on its running stream"
            );
            return;
        }
        if let Some(live_value) = &applied.live_value_it_replaced
            && let Err(not_set_live_again) =
                stream_operations.set_input_values_live(std::slice::from_ref(live_value))
        {
            tracing::error!(
                stream = %stream_id,
                node = %live_value.node,
                input = live_value.input,
                ?not_set_live_again,
                "could not set live again a value a refused graph transaction replaced"
            );
        }
    }
}

/// Whether `operation` sets a value a running stream does not take back with the graph edits
/// of a batch: a node's config, input, name, studio fields or exposed config fields.
fn sets_a_value_a_take_back_leaves(operation: &StreamGraphOperation) -> bool {
    matches!(
        operation,
        StreamGraphOperation::SetNodeConfig { .. }
            | StreamGraphOperation::SetPinValue { .. }
            | StreamGraphOperation::SetNodeInputValue { .. }
            | StreamGraphOperation::RenameNode { .. }
            | StreamGraphOperation::SetStudioFields { .. }
            | StreamGraphOperation::SetExposedConfigFields { .. }
    )
}

/// Take back the batch of graph edits open on each of `streams`, which compiles none of them.
pub(super) fn take_back_the_graph_edits_of_each_stream(
    streams: &mut BTreeMap<String, StreamATransactionEdits>,
) {
    for stream in streams.values_mut() {
        if stream.graph_edit_batch.take().is_some() {
            stream.its_graph_edits_were_taken_back = true;
        }
    }
}

/// Close the batch of graph edits open on each of `streams`, every one of them closed: each
/// running stream judges the graph its batch leaves and compiles it in one commit, or takes the
/// batch back, and the transaction is refused as the first running stream refuses.
pub(super) fn close_the_graph_edits_of_each_stream(
    streams: &mut BTreeMap<String, StreamATransactionEdits>,
) -> Result<(), GraphRequestRefusal> {
    let mut first_refusal = None;
    for stream in streams.values_mut() {
        let Some(graph_edit_batch) = stream.graph_edit_batch.take() else {
            continue;
        };
        if let Err(refusal) = graph_edit_batch.close() {
            stream.its_graph_edits_were_taken_back = true;
            first_refusal.get_or_insert(refusal);
        }
    }
    match first_refusal {
        Some(refusal) => Err(GraphRequestRefusal::from(refusal)),
        None => Ok(()),
    }
}
