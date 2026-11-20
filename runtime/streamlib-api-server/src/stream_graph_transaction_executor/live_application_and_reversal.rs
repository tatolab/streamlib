// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use streamlib::sdk::runtime::{GraphEditBatch, HowFarARefusedBatchWasTakenBack};

/// One loaded stream a transaction edits: its operations, where it lives, its graph as the
/// transaction has staged it, and the batch of graph edits it makes on the running stream.
pub(super) struct StreamATransactionEdits {
    pub(super) stream_operations: Arc<dyn RuntimeOperations>,
    pub(super) stream_location: StreamLocation,
    pub(super) staged_graph: StreamGraphDocument,
    graph_edit_batch: GraphEditBatchState,
}

/// Where the batch of graph edits a transaction makes on one running stream stands.
enum GraphEditBatchState {
    /// Open: each graph edit of the stream is applied through it.
    Open(Box<dyn GraphEditBatch>),
    /// Closed, its edits compiled in one commit.
    Closed,
    /// Taken back: whole, which leaves only the values its edits set to reverse, or only as far
    /// as nothing built or wired holds it, since its commit failed after it changed what runs,
    /// which leaves every edit to reverse.
    TakenBack(HowFarARefusedBatchWasTakenBack),
}

impl StreamATransactionEdits {
    /// The stream `stream_operations` serves, at `stream_location`, its graph staged as
    /// `staged_graph`, with a batch of graph edits opened on it, once no other writer's is open.
    pub(super) fn new(
        stream_operations: Arc<dyn RuntimeOperations>,
        stream_location: StreamLocation,
        staged_graph: StreamGraphDocument,
    ) -> Result<Self, String> {
        let graph_edit_batch = GraphEditBatchState::Open(
            Arc::clone(&stream_operations)
                .open_a_graph_edit_batch()
                .map_err(|refusal| refusal.to_string())?,
        );
        Ok(Self {
            stream_operations,
            stream_location,
            staged_graph,
            graph_edit_batch,
        })
    }
}

/// The batch of graph edits open on the stream `stream_id` of `streams`, to apply a graph edit
/// through.
pub(super) fn the_open_batch_of<'streams>(
    streams: &'streams BTreeMap<String, StreamATransactionEdits>,
    stream_id: &str,
) -> Result<&'streams dyn GraphEditBatch, String> {
    match streams
        .get(stream_id)
        .map(|stream| &stream.graph_edit_batch)
    {
        Some(GraphEditBatchState::Open(graph_edit_batch)) => Ok(graph_edit_batch.as_ref()),
        Some(_) => Err(format!(
            "stream `{stream_id}` holds no batch of graph edits open to apply to"
        )),
        None => Err(format!(
            "stream `{stream_id}` holds no batch of graph edits"
        )),
    }
}

impl StreamGraphTransactionExecutor<'_> {
    /// Make each of `operations`, each by the loaded stream it changes, on that running stream,
    /// once the project's graph documents hold it. A running stream that refuses one holds it,
    /// and each later one of that stream, as an edit not live yet, which `graph` draws and its
    /// diagnostics list until its playback stops and brings it to its document.
    pub(super) fn make_live_what_the_documents_now_hold(
        &self,
        operations: Vec<(String, StreamGraphOperation)>,
    ) {
        let mut refusal_of_each_stream_that_refused: HashMap<String, String> = HashMap::new();
        let mut sets_made_live_on_each_stream: BTreeMap<
            String,
            (Arc<dyn RuntimeOperations>, Vec<StreamGraphOperation>),
        > = BTreeMap::new();
        for (stream_id, operation) in operations {
            let stream_operations = match self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(&stream_id)
            {
                Ok(stream_operations) => stream_operations,
                Err(why) => {
                    tracing::warn!(
                        stream = %stream_id,
                        %why,
                        "the project's graph documents hold an edit of a stream not loaded; the \
                         stream takes it at its next load"
                    );
                    continue;
                }
            };
            if let Some(refusal) = refusal_of_each_stream_that_refused.get(&stream_id) {
                stream_operations.hold_an_edit_its_document_holds_not_live(
                    &operation,
                    &format!("an earlier edit of the same transaction was not taken: {refusal}"),
                );
                continue;
            }
            let held_if_refused = operation.clone();
            if let Err(refusal) = self
                .async_runtime
                .block_on(stream_operations.apply_a_stream_graph_operation_live(operation))
            {
                let refusal = refusal.to_string();
                tracing::warn!(
                    stream = %stream_id,
                    %refusal,
                    "the project's graph documents hold an edit its running stream refused; the \
                     stream holds it not live until its playback stops"
                );
                stream_operations
                    .hold_an_edit_its_document_holds_not_live(&held_if_refused, &refusal);
                refusal_of_each_stream_that_refused.insert(stream_id, refusal);
                continue;
            }
            if matches!(
                held_if_refused,
                StreamGraphOperation::SetNodeInputValue { .. }
                    | StreamGraphOperation::SetPinValue { .. }
                    | StreamGraphOperation::SetNodeConfig { .. }
            ) {
                sets_made_live_on_each_stream
                    .entry(stream_id)
                    .or_insert_with(|| (Arc::clone(&stream_operations), Vec::new()))
                    .1
                    .push(held_if_refused);
            }
        }
        for (stream_id, (stream_operations, sets)) in sets_made_live_on_each_stream {
            let Err(refusal) = self
                .async_runtime
                .block_on(stream_operations.rebuild_each_node_a_setting_read_at_setup_changed())
            else {
                continue;
            };
            let refusal = refusal.to_string();
            tracing::warn!(
                stream = %stream_id,
                %refusal,
                "the project's graph documents hold a setting its running node did not build \
                 with; the stream holds it not live until its playback stops"
            );
            for set in &sets {
                stream_operations.hold_an_edit_its_document_holds_not_live(set, &refusal);
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
        // Every batch opens before any edit is applied, in the order of the stream ids, so two
        // transactions editing the same streams never each wait on a batch the other holds.
        let stream_assets_a_batch_opens_on: BTreeMap<&String, Uuid> = reversal
            .operations
            .iter()
            .filter_map(|operation| match operation {
                ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                    stream,
                    graph_operation,
                }) if !graph_operation.reaches_a_running_stream_once_the_documents_hold_it() => {
                    stream_id_of_each_stream_asset
                        .get(stream)
                        .map(|stream_id| (stream_id, *stream))
                }
                _ => None,
            })
            .collect();
        for (stream_id, stream_asset) in stream_assets_a_batch_opens_on {
            let stream_operations = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(stream_id)?;
            streams.insert(
                stream_id.clone(),
                StreamATransactionEdits::new(
                    stream_operations,
                    StreamLocation {
                        project_directory: project_repository.project_directory().to_path_buf(),
                        project,
                        stream_asset,
                    },
                    StreamGraphDocument::from_value(Value::Object(Map::new()))?,
                )?,
            );
        }
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
            let applied_to_the_running_stream = || {
                let graph_edit_batch = the_open_batch_of(&streams, stream_id)?;
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
        if let Err(refusal) = close_the_graph_edits_of_each_stream(&mut streams).and_then(|()| {
            self.rebuild_each_node_a_setting_read_at_setup_changed(&streams, &mut Vec::new())
        }) {
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
                        node,
                        input,
                        member,
                        to,
                        ..
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
            // A value an older runtime recorded, or saved, in a form its input's type held then
            // is checked in the form the stream applies it in now.
            let ports = stream_operations.the_ports_a_node_type_declares(&set_on.node_type);
            let to = match (&ports, member, to) {
                (Some(ports), None, Some(to)) => {
                    ports.input_value_in_the_form_it_takes_now(input, to.clone())
                }
                (_, _, to) => to.clone(),
            };
            if let Some(ports) = &ports {
                set_on.input_values = ports
                    .input_values_under_the_current_names(std::mem::take(&mut set_on.input_values));
            }
            let input_values_after_the_set = input_values_after_the_set(
                &set_on.input_values,
                input,
                member.as_deref(),
                to.as_ref(),
            );
            refuse_a_value_the_input_does_not_take(
                stream_operations.as_ref(),
                &set_on,
                &input_values_after_the_set,
                input,
                member.as_deref(),
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
    ///
    /// The inverses of one stream are applied in one batch: the transaction's own while it is
    /// open, or else one opened to reverse them, so none is judged by a graph the others leave
    /// half reversed and a stream's one writer is never waited for while its batch is open.
    pub(super) fn reverse_what_was_applied_live(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        applied_live: Vec<(String, OneOperationAppliedLive)>,
    ) {
        let applied_live: Vec<(String, OneOperationAppliedLive)> = applied_live
            .into_iter()
            .filter(|(stream_id, applied)| {
                streams.get(stream_id).is_some_and(|stream| {
                    !matches!(
                        stream.graph_edit_batch,
                        GraphEditBatchState::TakenBack(HowFarARefusedBatchWasTakenBack::Whole)
                    ) || sets_a_value_a_take_back_leaves(&applied.operation)
                })
            })
            .collect();
        let streams_to_reverse_in_a_batch_of_their_own: BTreeSet<String> = applied_live
            .iter()
            .map(|(stream_id, _)| stream_id)
            .filter(|stream_id| the_open_batch_of(streams, stream_id).is_err())
            .cloned()
            .collect();
        let mut batch_reversing_each_stream: BTreeMap<String, Box<dyn GraphEditBatch>> =
            BTreeMap::new();
        for stream_id in streams_to_reverse_in_a_batch_of_their_own {
            let Some(stream) = streams.get(&stream_id) else {
                continue;
            };
            match Arc::clone(&stream.stream_operations).open_a_graph_edit_batch() {
                Ok(batch) => {
                    batch_reversing_each_stream.insert(stream_id, batch);
                }
                Err(not_opened) => tracing::error!(
                    stream = %stream_id,
                    %not_opened,
                    "no batch of graph edits was opened to reverse a refused graph \
                     transaction; each part of it is reversed on its own"
                ),
            }
        }
        for (stream_id, applied) in applied_live.into_iter().rev() {
            let Some(stream) = streams.get(&stream_id) else {
                continue;
            };
            let batch = the_open_batch_of(streams, &stream_id).ok().or_else(|| {
                batch_reversing_each_stream
                    .get(&stream_id)
                    .map(|batch| batch.as_ref())
            });
            self.reverse_one_operation_applied_live(
                &stream_id,
                stream.stream_operations.as_ref(),
                batch,
                &applied,
            );
        }
        for (stream_id, batch) in batch_reversing_each_stream {
            if let Err(refused) = batch.close() {
                tracing::error!(
                    stream = %stream_id,
                    refusal = %refused.refusal,
                    "could not reverse a refused graph transaction on its running stream"
                );
            }
        }
        for (stream_id, stream) in streams {
            self.rebuild_each_node_a_reversal_put_its_settings_back_on(
                stream_id,
                stream.stream_operations.as_ref(),
            );
        }
    }

    /// Rebuild, on each of `streams`, every running node a value set on one of its settings
    /// read at setup has left to rebuild, once; each node rebuilt, under its stream. Refused in
    /// the node's own words when one does not build with the values set, for the caller to
    /// reverse the transaction, which builds it again from the values it had.
    pub(super) fn rebuild_each_node_a_setting_read_at_setup_changed(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        nodes_rebuilt: &mut Vec<GraphTransactionRebuiltNode>,
    ) -> Result<(), GraphRequestRefusal> {
        for (stream_id, stream) in streams {
            let rebuilt = self.async_runtime.block_on(
                stream
                    .stream_operations
                    .rebuild_each_node_a_setting_read_at_setup_changed(),
            )?;
            for node in rebuilt {
                let rebuilt_node = GraphTransactionRebuiltNode {
                    stream: stream_id.clone(),
                    node: node.to_string(),
                };
                if !nodes_rebuilt.contains(&rebuilt_node) {
                    nodes_rebuilt.push(rebuilt_node);
                }
            }
        }
        Ok(())
    }

    /// Rebuild each node of the running stream `stream_operations` serves whose settings read
    /// at setup a reversal put back, logging a node its earlier values no longer build, which
    /// reads `Error` with `components.failed_because`.
    pub(super) fn rebuild_each_node_a_reversal_put_its_settings_back_on(
        &self,
        stream_id: &str,
        stream_operations: &dyn RuntimeOperations,
    ) {
        if let Err(not_rebuilt) = self
            .async_runtime
            .block_on(stream_operations.rebuild_each_node_a_setting_read_at_setup_changed())
        {
            tracing::error!(
                stream = %stream_id,
                %not_rebuilt,
                "a node whose settings a refused graph transaction put back did not build with \
                 them either"
            );
        }
    }

    /// Reverse `applied` on the running stream `stream_operations` serves: apply its inverse,
    /// through `batch` when one is open on the stream, then set live again the live value it
    /// replaced, logging a step the stream refuses.
    pub(super) fn reverse_one_operation_applied_live(
        &self,
        stream_id: &str,
        stream_operations: &dyn RuntimeOperations,
        batch: Option<&dyn GraphEditBatch>,
        applied: &OneOperationAppliedLive,
    ) {
        let inverse = applied.operation.inverse();
        let reversed = match batch {
            Some(batch) => self
                .async_runtime
                .block_on(batch.apply_a_stream_graph_operation_live(inverse)),
            None => self
                .async_runtime
                .block_on(stream_operations.apply_a_stream_graph_operation_live(inverse)),
        };
        if let Err(not_reversed) = reversed {
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
/// of a batch: a node's config, input, studio fields or exposed config fields.
fn sets_a_value_a_take_back_leaves(operation: &StreamGraphOperation) -> bool {
    matches!(
        operation,
        StreamGraphOperation::SetNodeConfig { .. }
            | StreamGraphOperation::SetPinValue { .. }
            | StreamGraphOperation::SetNodeInputValue { .. }
            | StreamGraphOperation::SetStudioFields { .. }
            | StreamGraphOperation::SetExposedConfigFields { .. }
    )
}

/// Take back the batch of graph edits open on each of `streams`, which compiles none of them.
pub(super) fn take_back_the_graph_edits_of_each_stream(
    streams: &mut BTreeMap<String, StreamATransactionEdits>,
) {
    for stream in streams.values_mut() {
        if let GraphEditBatchState::Open(_) = stream.graph_edit_batch {
            stream.graph_edit_batch =
                GraphEditBatchState::TakenBack(HowFarARefusedBatchWasTakenBack::Whole);
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
        let GraphEditBatchState::Open(graph_edit_batch) =
            std::mem::replace(&mut stream.graph_edit_batch, GraphEditBatchState::Closed)
        else {
            continue;
        };
        if let Err(refused) = graph_edit_batch.close() {
            stream.graph_edit_batch = GraphEditBatchState::TakenBack(refused.taken_back);
            first_refusal.get_or_insert(refused.refusal);
        }
    }
    match first_refusal {
        Some(refusal) => Err(GraphRequestRefusal::from(refusal)),
        None => Ok(()),
    }
}
