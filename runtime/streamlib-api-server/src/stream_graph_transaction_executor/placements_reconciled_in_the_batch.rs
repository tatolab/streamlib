// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Each loaded stream placing a stream a committed transaction changes, brought to the streams
//! it places inside the transaction's batch, so a placement that refuses the edit refuses the
//! whole transaction and leaves every stream and document as it was.

use super::live_application_and_reversal::{StreamATransactionEdits, the_open_batch_of};
use super::*;
use streamlib_project_repository::{StreamDocumentsAnEditStaged, the_streams_a_live_graph_places};

/// Each loaded stream a transaction brought, in its batch, to the streams it places, by its
/// stream id: the graph its running stream is put back to when the transaction is refused.
#[derive(Default)]
pub(super) struct PlacementsReconciledInTheirBatches {
    graph_put_back_to_of_each_stream_id: BTreeMap<String, Value>,
}

impl PlacementsReconciledInTheirBatches {
    /// Whether the stream `stream_id` was brought to the streams it places in its batch.
    pub(super) fn holds(&self, stream_id: &str) -> bool {
        self.graph_put_back_to_of_each_stream_id
            .contains_key(stream_id)
    }
}

impl StreamGraphTransactionExecutor<'_> {
    /// The graph document of the stream at `stream_location`, whose loaded stream
    /// `stream_operations` runs, as it stands: the one `project_repository` holds when the
    /// stream is of its project, or else the one the stream's live graph renders.
    pub(super) fn the_graph_document_of_a_loaded_stream_as_it_stands(
        &self,
        project_repository: Option<&ProjectRepository>,
        stream_location: &StreamLocation,
        stream_operations: &dyn RuntimeOperations,
    ) -> Result<StreamGraphDocument, GraphTransactionRefusal> {
        self.the_stream_graph_document_as_it_stands(
            project_repository
                .filter(|held| held.project_file().project == stream_location.project),
            stream_location.stream_asset,
            stream_operations,
        )
    }

    /// Bring each of `streams` that places, at any depth, a stream whose document the
    /// transaction changes, or, for a stream `named` does not hold, whose own document it
    /// changes, to its graph as the transaction has staged it, inside its batch, each stream it
    /// places as the transaction has staged it; each is recorded in `reconciled`. Refused at the
    /// first stream that refuses, in that stream's words, the streams before it left in
    /// `reconciled` to be put back.
    pub(super) fn reconcile_each_placement_in_its_batch(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        named: &[String],
        staged_graph_of_each_stream_not_running: &BTreeMap<Uuid, StreamGraphDocument>,
        project_repository: &ProjectRepository,
        reconciled: &mut PlacementsReconciledInTheirBatches,
    ) -> Result<(), GraphTransactionRefusal> {
        let stream_documents_the_edit_changed = the_stream_documents_the_transaction_changes(
            streams,
            staged_graph_of_each_stream_not_running,
            project_repository,
        )?;
        for (stream_id, stream) in streams {
            let StreamLocation {
                project,
                stream_asset,
                ..
            } = stream.stream_location;
            let staged_graph = stream_documents_the_edit_changed
                .the_document_staged(project, stream_asset)
                .cloned()
                .unwrap_or_else(|| stream.staged_graph.to_value());
            let its_own_document_changed = !named.contains(stream_id)
                && stream_documents_the_edit_changed.holds(project, stream_asset);
            if !its_own_document_changed
                && !self.places_a_stream_the_edit_changed(
                    stream,
                    &staged_graph,
                    &stream_documents_the_edit_changed,
                    project_repository,
                )
            {
                continue;
            }
            let graph_put_back_to = self
                .the_graph_document_of_a_loaded_stream_as_it_stands(
                    Some(project_repository),
                    &stream.stream_location,
                    stream.stream_operations.as_ref(),
                )?
                .to_value();
            let graph_edit_batch = the_open_batch_of(streams, stream_id)
                .map_err(GraphTransactionRefusal::of_the_whole_transaction)?;
            reconciled
                .graph_put_back_to_of_each_stream_id
                .insert(stream_id.clone(), graph_put_back_to);
            self.async_runtime
                .block_on(
                    graph_edit_batch.reconcile_the_live_graph_to_the_document_an_edit_staged(
                        staged_graph,
                        &stream_documents_the_edit_changed,
                    ),
                )
                .map_err(GraphTransactionRefusal::of_the_whole_transaction)?;
        }
        Ok(())
    }

    /// Whether `stream`, its graph staged as `staged_graph`, places, at any depth, a stream
    /// `changed` holds: one its live graph places, or one its staged graph places through the
    /// streams of the project `project_repository` holds, each read as its document is on disk.
    fn places_a_stream_the_edit_changed(
        &self,
        stream: &StreamATransactionEdits,
        staged_graph: &Value,
        changed: &StreamDocumentsAnEditStaged,
        project_repository: &ProjectRepository,
    ) -> bool {
        let project_of_the_stream = stream.stream_location.project;
        let live_graph_places_one = self
            .async_runtime
            .block_on(stream.stream_operations.to_json_async())
            .is_ok_and(|live_graph| {
                the_streams_a_live_graph_places(&live_graph, project_of_the_stream)
                    .into_iter()
                    .any(|(project, stream_asset)| changed.holds(project, stream_asset))
            });
        if live_graph_places_one {
            return true;
        }
        let project = project_repository.project_file().project;
        if project_of_the_stream != project {
            return false;
        }
        let mut reached = BTreeSet::new();
        let mut to_visit: Vec<Uuid> =
            stream_assets_of_the_project_a_graph_document_places(staged_graph)
                .into_iter()
                .collect();
        while let Some(placed_stream) = to_visit.pop() {
            if !reached.insert(placed_stream) {
                continue;
            }
            if changed.holds(project, placed_stream) {
                return true;
            }
            if let Ok(placed_graph) = project_repository.stream_graph_document(placed_stream) {
                to_visit.extend(stream_assets_of_the_project_a_graph_document_places(
                    &placed_graph.to_value(),
                ));
            }
        }
        false
    }

    /// Put back what a refused transaction ran: each edit of `applied_live` reversed, and each
    /// running stream of `reconciled` brought back to the graph it ran before.
    pub(super) fn put_back_what_the_refused_transaction_ran(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        applied_live: Vec<(String, OneOperationAppliedLive)>,
        reconciled: PlacementsReconciledInTheirBatches,
    ) {
        self.reverse_what_was_applied_live(streams, applied_live);
        for (stream_id, graph_put_back_to) in reconciled.graph_put_back_to_of_each_stream_id {
            let Some(stream) = streams.get(&stream_id) else {
                continue;
            };
            let put_back = Arc::clone(&stream.stream_operations)
                .open_a_graph_edit_batch()
                .and_then(|batch| {
                    self.async_runtime.block_on(
                        batch.reconcile_the_live_graph_to_the_document_an_edit_staged(
                            graph_put_back_to,
                            &StreamDocumentsAnEditStaged::default(),
                        ),
                    )?;
                    batch.close().map_err(|refused| refused.refusal)
                });
            if let Err(not_put_back) = put_back {
                tracing::error!(
                    stream = %stream_id,
                    %not_put_back,
                    "a stream a refused transaction brought to the streams it places was not put \
                     back to the graph it ran before"
                );
            }
        }
    }
}

/// The graph document of each stream of the project `project_repository` holds that the
/// transaction changes, a loaded one's and one not running, as the transaction has staged it.
fn the_stream_documents_the_transaction_changes(
    streams: &BTreeMap<String, StreamATransactionEdits>,
    staged_graph_of_each_stream_not_running: &BTreeMap<Uuid, StreamGraphDocument>,
    project_repository: &ProjectRepository,
) -> Result<StreamDocumentsAnEditStaged, GraphTransactionRefusal> {
    let project = project_repository.project_file().project;
    let name_of_each_stream: HashMap<Uuid, String> = project_repository
        .assets()?
        .into_iter()
        .filter(|asset| asset.kind == ProjectAssetKind::Stream)
        .map(|asset| (asset.asset, asset.path.stream_name().to_owned()))
        .collect();
    // A stream not running staged beside a loaded one of the same stream asset holds the
    // edits made through a placement of it, so it is staged last.
    let staged_graph_of_each_stream = streams
        .values()
        .filter(|stream| stream.stream_location.project == project)
        .map(|stream| (stream.stream_location.stream_asset, &stream.staged_graph))
        .chain(
            staged_graph_of_each_stream_not_running
                .iter()
                .map(|(stream_asset, document)| (*stream_asset, document)),
        );
    let mut changed = StreamDocumentsAnEditStaged::default();
    for (stream_asset, staged_graph) in staged_graph_of_each_stream {
        let staged_graph = staged_graph.to_value();
        if project_repository
            .stream_graph_document(stream_asset)
            .is_ok_and(|on_disk| on_disk.to_value() == staged_graph)
        {
            continue;
        }
        let stream_name = name_of_each_stream
            .get(&stream_asset)
            .cloned()
            .unwrap_or_else(|| stream_asset.to_string());
        changed.stage(project, stream_asset, stream_name, staged_graph);
    }
    Ok(changed)
}
