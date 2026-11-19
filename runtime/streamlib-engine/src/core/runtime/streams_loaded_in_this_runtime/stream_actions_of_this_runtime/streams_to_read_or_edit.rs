// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The streams a read or an edit names: each loaded stream as it is loaded, and each stream the
//! runtime lists and has not loaded held from its graph document alone, so its graph is read
//! and edited as a loaded stream's is.

use super::recorded_stream_reload::the_graph_the_document_of_the_stream_at_holds;
use super::*;
use crate::core::descriptors::NodeDescriptor;
use crate::core::runtime::RuntimeOperations;
use crate::core::runtime::runtime::tear_down_a_stream_that_never_entered_the_table;
use parking_lot::MutexGuard;

/// The streams a read or an edit names, each as the runtime operations it answers through:
/// a loaded stream as it is loaded, and a stream the runtime lists and has not loaded held
/// from its graph document alone. While it holds a stream from its document no stream action
/// runs, so no load reads that document, or enters that stream in the table, before an edit
/// made through it is in the document.
pub struct StreamsToReadOrEdit<'runner> {
    runtime_operations_of_each_stream: HashMap<String, Arc<dyn RuntimeOperations>>,
    each_stream_of_this_runtime: HashMap<String, Arc<LoadedStreamInThisRuntime>>,
    held_from_their_documents_alone: Vec<StreamHeldFromItsDocumentAlone>,
    no_stream_action_while_any_is_held: Option<MutexGuard<'runner, ()>>,
}

impl StreamsToReadOrEdit<'_> {
    /// Streams each already loaded, as `runtime_operations_of_each_stream` answers them.
    pub fn of_streams_each_loaded(
        runtime_operations_of_each_stream: HashMap<String, Arc<dyn RuntimeOperations>>,
    ) -> Self {
        Self {
            runtime_operations_of_each_stream,
            each_stream_of_this_runtime: HashMap::new(),
            held_from_their_documents_alone: Vec::new(),
            no_stream_action_while_any_is_held: None,
        }
    }

    /// The runtime operations of the stream `stream_id`; refused for a stream these were not
    /// asked for.
    pub fn runtime_operations_of(&self, stream_id: &str) -> Result<Arc<dyn RuntimeOperations>> {
        self.runtime_operations_of_each_stream
            .get(stream_id)
            .cloned()
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "the stream `{stream_id}` is not among the streams asked for to read or edit"
                ))
            })
    }

    /// The node types the stream `stream_id` resolves: a loaded stream's as it is loaded, and,
    /// for one held from its graph document alone, the native ones and the views its project
    /// brings, as none of its node code is read. Refused for a stream these were not asked for.
    pub(in crate::core::runtime) fn node_catalog_of(
        &self,
        stream_id: &str,
    ) -> Result<Vec<NodeDescriptor>> {
        self.each_stream_of_this_runtime
            .get(stream_id)
            .map(|stream| stream.node_types_this_stream_resolves().node_catalog())
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "the stream `{stream_id}` is not among the streams asked for to read or edit"
                ))
            })
    }

    fn hold(&mut self, stream_id: &str, stream: Arc<LoadedStreamInThisRuntime>) {
        self.runtime_operations_of_each_stream.insert(
            stream_id.to_owned(),
            Arc::clone(&stream) as Arc<dyn RuntimeOperations>,
        );
        self.each_stream_of_this_runtime
            .insert(stream_id.to_owned(), stream);
    }
}

impl Drop for StreamsToReadOrEdit<'_> {
    fn drop(&mut self) {
        self.runtime_operations_of_each_stream.clear();
        self.each_stream_of_this_runtime.clear();
        for held in self.held_from_their_documents_alone.drain(..) {
            tear_down_a_stream_that_never_entered_the_table(&held.stream);
        }
        self.no_stream_action_while_any_is_held.take();
    }
}

/// A stream the runtime lists and has not loaded — its project still preparing, closed, or
/// its load given up — held stopped from its graph document, reading none of its node code,
/// so each node is held as a placeholder of its type, drawn from the graph as written. It is
/// never entered in the stream table and plays nothing.
struct StreamHeldFromItsDocumentAlone {
    stream: Arc<LoadedStreamInThisRuntime>,
}

impl Runner {
    /// The streams `stream_ids` names, to read or edit: each loaded stream as it is loaded, and
    /// each stream this runtime lists and has not loaded held from its graph document alone.
    /// Refused for a stream this runtime does not hold; for one an earlier runtime recorded,
    /// which names no document; and for one whose document holds no graph.
    pub fn the_streams_to_read_or_edit(
        &self,
        stream_ids: &[String],
    ) -> Result<StreamsToReadOrEdit<'_>> {
        let each_loaded: Option<Vec<Arc<LoadedStreamInThisRuntime>>> = stream_ids
            .iter()
            .map(|stream_id| self.loaded_stream_of_the_id_when_loaded(stream_id))
            .collect();
        if let Some(each_loaded) = each_loaded {
            let mut streams = StreamsToReadOrEdit::of_streams_each_loaded(HashMap::new());
            for (stream_id, loaded) in stream_ids.iter().zip(each_loaded) {
                streams.hold(stream_id, loaded);
            }
            return Ok(streams);
        }
        // One of them is not loaded: resolve every one of them while no stream action runs, so
        // none is loaded or unloaded between its lookup and the edits made through it.
        let mut streams = StreamsToReadOrEdit::of_streams_each_loaded(HashMap::new());
        streams.no_stream_action_while_any_is_held =
            Some(self.stream_actions.one_stream_action_at_a_time.lock());
        for stream_id in stream_ids {
            if streams
                .runtime_operations_of_each_stream
                .contains_key(stream_id)
            {
                continue;
            }
            let stream = match self.loaded_stream_of_the_id_when_loaded(stream_id) {
                Some(loaded) => loaded,
                None => {
                    let held = self.the_stream_held_from_its_document_alone(stream_id)?;
                    let stream = Arc::clone(&held.stream);
                    streams.held_from_their_documents_alone.push(held);
                    stream
                }
            };
            streams.hold(stream_id, stream);
        }
        Ok(streams)
    }

    /// The stream `stream_id`, which this runtime lists and has not loaded, held from its graph
    /// document on its own, built while no stream action runs.
    fn the_stream_held_from_its_document_alone(
        &self,
        stream_id: &str,
    ) -> Result<StreamHeldFromItsDocumentAlone> {
        let listing = self
            .list_streams()
            .into_iter()
            .find(|listing| listing.stream_id == stream_id)
            .ok_or_else(|| self.a_stream_this_runtime_does_not_hold(stream_id))?;
        let Some(stream_location) = listing.stream_location else {
            return Err(Error::NotFound(format!(
                "the stream `{stream_id}` was recorded by an earlier runtime, and names no graph \
                 document to hold it from"
            )));
        };
        let graph = the_graph_the_document_of_the_stream_at_holds(&stream_location).map_err(
            |why_it_holds_none| {
                Error::Runtime(format!(
                    "the stream `{stream_id}` is not loaded, and is not held from its graph \
                     document: {why_it_holds_none}"
                ))
            },
        )?;
        let load_options =
            OptionsForLoadingOneStream::at_stream_location(stream_id, stream_location)
                .with_display_name(listing.display_name)
                .reading_none_of_its_node_code(
                    why_a_stream_not_loaded_reads_none_of_its_node_code(
                        listing.state,
                        listing.failed_because.as_deref(),
                    ),
                );
        let stream = self.build_the_stream_a_load_names(
            load_options,
            LoadedStreamHolding::RecordedByTheRuntime,
        )?;
        self.load_the_graph_into_the_stream(&stream, &graph)?;
        stream.hold_stopped_without_a_start();
        Ok(StreamHeldFromItsDocumentAlone { stream })
    }
}

/// Why a stream the runtime lists in `state`, and has not loaded, reads none of its node code
/// as it is held from its document; `failed_because` is why it failed, when it did.
fn why_a_stream_not_loaded_reads_none_of_its_node_code(
    state: StreamListingState,
    failed_because: Option<&str>,
) -> String {
    match (state, failed_because) {
        (StreamListingState::Preparing, _) => {
            "its project is being prepared, so none of its node code is read yet".to_owned()
        }
        (StreamListingState::Closed, _) => {
            "its project is closed, so none of its node code is read".to_owned()
        }
        (_, Some(failed_because)) => {
            format!("it is not loaded, so none of its node code is read: {failed_because}")
        }
        (_, None) => "it is not loaded, so none of its node code is read".to_owned(),
    }
}
