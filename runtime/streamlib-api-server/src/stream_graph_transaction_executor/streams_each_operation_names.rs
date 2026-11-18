// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The stream each operation of a transaction names, by its stream id or its stream asset's
//! UUID, read as the stream asset history records and the loaded stream it runs on.

use super::*;

impl StreamGraphTransactionExecutor<'_> {
    /// Each of `operations` to the stream asset its `stream` names: a stream asset's UUID names
    /// itself, and a stream id the stream asset of the stream the runtime holds under it. A
    /// stream id the runtime holds no stream under is refused with `stream_not_found`, naming
    /// the streams it holds.
    pub(crate) fn each_operation_to_the_stream_asset_it_names(
        &self,
        operations: Vec<StreamGraphAction<GraphEditStream>>,
    ) -> Result<Vec<StreamGraphAction>, GraphTransactionRefusal> {
        let stream_location_of_each_stream_id = self.the_location_of_each_loaded_stream();
        operations
            .into_iter()
            .enumerate()
            .map(|(operation_index, operation)| {
                let stream_asset = match operation.stream() {
                    GraphEditStream::StreamAsset(stream_asset) => *stream_asset,
                    GraphEditStream::StreamId(stream_id) => {
                        match stream_location_of_each_stream_id.get(stream_id) {
                            Some(stream_location) => stream_location.stream_asset,
                            None => {
                                return Err(GraphTransactionRefusal::of_the_operation(
                                    operation_index,
                                    what_a_graph_action_names(&operation),
                                    GraphRequestRefusal::coded(
                                        RefusalCodeAndPlace::coded(
                                            ControlOperationRefusalCode::StreamNotFound,
                                        ),
                                        the_words_of_a_stream_id_no_stream_is_held_under(
                                            stream_id,
                                            &stream_location_of_each_stream_id,
                                        ),
                                    ),
                                ));
                            }
                        }
                    }
                };
                Ok(operation.to_the_stream(stream_asset))
            })
            .collect()
    }

    /// The stream id of the stream running each request's stream asset, in request order; a
    /// loaded one over one only recorded.
    pub(super) fn the_loaded_stream_each_request_names(
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
                    .get(request.stream())
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
}

/// Why the stream id `stream_id` names no stream, naming each stream id of
/// `stream_location_of_each_stream_id`, the streams the runtime holds.
fn the_words_of_a_stream_id_no_stream_is_held_under(
    stream_id: &str,
    stream_location_of_each_stream_id: &HashMap<String, StreamLocation>,
) -> String {
    let mut stream_ids_held: Vec<String> = stream_location_of_each_stream_id
        .keys()
        .map(|held| format!("`{held}`"))
        .collect();
    stream_ids_held.sort();
    format!(
        "the runtime holds no stream `{stream_id}`; the streams it holds are: {}",
        if stream_ids_held.is_empty() {
            "none".to_owned()
        } else {
            stream_ids_held.join(", ")
        }
    )
}
