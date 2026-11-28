// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Each loaded stream placing a stream a committed transaction changes, brought to the streams
//! it places inside the transaction's batch, so a placement that refuses the edit refuses the
//! whole transaction and leaves every stream and document as it was.

use super::live_application_and_reversal::{StreamATransactionEdits, the_open_batch_of};
use super::*;
use streamlib_project_repository::{StreamDocumentsAnEditStaged, the_streams_a_live_graph_places};

/// Each loaded stream a transaction brought, in its batch, to the streams it places, by its
/// stream id, beside what it is put back to when the transaction is refused: a stream the
/// transaction names to its graph document as it stood, and any other to its live graph as
/// drawn, each to the streams it placed before.
#[derive(Default)]
pub(super) struct PlacementsReconciledInTheirBatches {
    graph_put_back_to_of_each_stream_id: BTreeMap<String, Option<Value>>,
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

    /// Each of `loaded`, none a stream `named` names, that places, at any depth, a stream of the
    /// project `project_repository` holds that an edit of the stream assets `edited` can change:
    /// one of them, or one they place, at any depth, as their documents are on disk now.
    pub(super) fn the_placements_an_edit_can_reach(
        &self,
        loaded: Vec<LoadedStreamHoldingAStreamFragment>,
        named: &[String],
        edited: impl IntoIterator<Item = Uuid>,
        project_repository: &ProjectRepository,
    ) -> Vec<LoadedStreamHoldingAStreamFragment> {
        let project = project_repository.project_file().project;
        let mut an_edit_can_change: BTreeSet<Uuid> = BTreeSet::new();
        let mut to_visit: Vec<Uuid> = edited.into_iter().collect();
        while let Some(stream_asset) = to_visit.pop() {
            if !an_edit_can_change.insert(stream_asset) {
                continue;
            }
            if let Ok(document) = project_repository.stream_graph_document(stream_asset) {
                to_visit.extend(stream_assets_of_the_project_a_graph_document_places(
                    &document.to_value(),
                ));
            }
        }
        loaded
            .into_iter()
            .filter(|placement| !named.contains(&placement.stream_id))
            .filter(|placement| {
                places_one_of_the_streams(
                    &placement.stream_location,
                    &placement.live_graph,
                    None,
                    project_repository,
                    |held_project, stream_asset| {
                        held_project == project && an_edit_can_change.contains(&stream_asset)
                    },
                )
            })
            .collect()
    }

    /// Bring each of `streams` that places, at any depth, a stream whose document the
    /// transaction changes, and each of `taking_their_edits_through_their_staged_graph`, whose
    /// edits place a stream or follow one that does, to the streams it places, inside its batch:
    /// a stream `named` names, or one taking its edits through its staged graph, to its graph as
    /// staged, any other to its live graph as drawn, each stream it places as the transaction has
    /// staged it; each is recorded in `reconciled`. Refused at the first stream that refuses, in
    /// that stream's words, the streams before it left in `reconciled` to be put back.
    pub(super) fn reconcile_each_placement_in_its_batch(
        &self,
        streams: &BTreeMap<String, StreamATransactionEdits>,
        named: &[String],
        taking_their_edits_through_their_staged_graph: &BTreeSet<String>,
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
            let takes_its_edits_through_its_staged_graph =
                taking_their_edits_through_their_staged_graph.contains(stream_id);
            let places_a_changed_stream = || {
                self.async_runtime
                    .block_on(stream.stream_operations.to_json_async())
                    .is_ok_and(|live_graph| {
                        places_one_of_the_streams(
                            &stream.stream_location,
                            &live_graph,
                            Some(&stream.staged_graph.to_value()),
                            project_repository,
                            |project, stream_asset| {
                                stream_documents_the_edit_changed.holds(project, stream_asset)
                            },
                        )
                    })
            };
            if !takes_its_edits_through_its_staged_graph && !places_a_changed_stream() {
                continue;
            }
            // A stream the transaction names, or one taking its edits through its staged graph,
            // is brought to its graph as staged, which holds each edit of its own that places a
            // stream or follows one; any other to its live graph as drawn, which holds what only
            // it runs.
            let (graph_document, graph_put_back_to) =
                if named.contains(stream_id) || takes_its_edits_through_its_staged_graph {
                    (
                        Some(stream.staged_graph.to_value()),
                        Some(
                            self.the_graph_document_of_a_loaded_stream_as_it_stands(
                                Some(project_repository),
                                &stream.stream_location,
                                stream.stream_operations.as_ref(),
                            )?
                            .to_value(),
                        ),
                    )
                } else {
                    (None, None)
                };
            let graph_edit_batch = the_open_batch_of(streams, stream_id)
                .map_err(GraphTransactionRefusal::of_the_whole_transaction)?;
            reconciled
                .graph_put_back_to_of_each_stream_id
                .insert(stream_id.clone(), graph_put_back_to);
            self.async_runtime
                .block_on(
                    graph_edit_batch.reconcile_the_live_graph_to_the_streams_an_edit_staged(
                        graph_document,
                        &stream_documents_the_edit_changed,
                    ),
                )
                .map_err(GraphTransactionRefusal::of_the_whole_transaction)?;
        }
        Ok(())
    }

    /// Put back what a refused transaction ran: each edit of `applied_live` reversed, then each
    /// running stream of `reconciled` brought back to what it records, each stream it places as
    /// its document is on disk, which the refused transaction left as it was.
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
                        batch.reconcile_the_live_graph_to_the_streams_an_edit_staged(
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
                     back to the streams it placed before"
                );
            }
        }
    }
}

/// Whether the loaded stream at `stream_location` places, at any depth, a stream `is_one`
/// holds: one its live graph `live_graph` places, or, for a stream of the project
/// `project_repository` holds, one its graph document, `staged_graph` or else the one on disk,
/// places through that project's streams as their documents are on disk now.
fn places_one_of_the_streams(
    stream_location: &StreamLocation,
    live_graph: &Value,
    staged_graph: Option<&Value>,
    project_repository: &ProjectRepository,
    is_one: impl Fn(Uuid, Uuid) -> bool,
) -> bool {
    if the_streams_a_live_graph_places(live_graph, stream_location.project)
        .into_iter()
        .any(|(project, stream_asset)| is_one(project, stream_asset))
    {
        return true;
    }
    let project = project_repository.project_file().project;
    if stream_location.project != project {
        return false;
    }
    let document = match staged_graph {
        Some(staged_graph) => staged_graph.clone(),
        None => match project_repository.stream_graph_document(stream_location.stream_asset) {
            Ok(document) => document.to_value(),
            Err(_) => return false,
        },
    };
    let mut reached = BTreeSet::new();
    let mut to_visit: Vec<Uuid> = stream_assets_of_the_project_a_graph_document_places(&document)
        .into_iter()
        .collect();
    while let Some(placed_stream) = to_visit.pop() {
        if !reached.insert(placed_stream) {
            continue;
        }
        if is_one(project, placed_stream) {
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
