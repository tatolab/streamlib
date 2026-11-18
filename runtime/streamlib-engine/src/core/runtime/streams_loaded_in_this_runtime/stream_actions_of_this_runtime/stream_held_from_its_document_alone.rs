// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream the runtime lists and has not loaded, held from its graph document on its own so
//! its graph is read and edited as a loaded stream's is.

use super::recorded_stream_reload::the_graph_the_document_of_the_stream_at_holds;
use super::*;
use crate::core::runtime::RuntimeOperations;
use crate::core::runtime::runtime::tear_down_a_stream_that_never_entered_the_table;

/// A stream the runtime lists and has not loaded — its project still preparing, closed, or
/// its load given up — held stopped from its graph document, reading none of its node code,
/// so each node is held as a placeholder of its type, drawn from the graph as written. It is
/// never entered in the stream table, plays nothing, and is torn down when it drops.
pub struct StreamHeldFromItsDocumentAlone {
    stream: Arc<LoadedStreamInThisRuntime>,
}

impl StreamHeldFromItsDocumentAlone {
    /// The stream as the runtime operations a loaded stream answers through.
    pub fn runtime_operations(&self) -> Arc<dyn RuntimeOperations> {
        Arc::clone(&self.stream) as Arc<dyn RuntimeOperations>
    }
}

impl Drop for StreamHeldFromItsDocumentAlone {
    fn drop(&mut self) {
        tear_down_a_stream_that_never_entered_the_table(&self.stream);
    }
}

impl Runner {
    /// The stream `stream_id`, which this runtime lists and has not loaded, held from its graph
    /// document on its own. Refused for a loaded stream, which is read as it is loaded; for one
    /// this runtime does not list; for one an earlier runtime recorded, which names no
    /// document; and for one whose document holds no graph.
    pub fn the_stream_held_from_its_document_alone(
        &self,
        stream_id: &str,
    ) -> Result<StreamHeldFromItsDocumentAlone> {
        if self
            .loaded_stream_of_the_id_when_loaded(stream_id)
            .is_some()
        {
            return Err(Error::Runtime(format!(
                "the stream `{stream_id}` is loaded, and is read and edited as it is loaded"
            )));
        }
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
