// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The stream each operation of a transaction names, by its stream id or its stream asset's
//! UUID, read from one listing of the streams the runtime holds as the stream asset history
//! records and the stream the operation applies to.

use streamlib::sdk::runtime::StreamListing;

use super::*;

impl StreamGraphTransactionExecutor<'_> {
    /// The stream asset each of `operations` names, in order, as the project's change feed
    /// keeps a refusal of them under: a stream asset's UUID names itself, and a stream id the
    /// asset of the stream the runtime holds under it; a stream id it holds no stream under
    /// names none.
    pub(crate) fn the_stream_assets_the_operations_name(
        &self,
        operations: &[StreamGraphAction<GraphEditStream>],
    ) -> Vec<Uuid> {
        let listings = self.operations_on_the_loaded_streams.list_streams();
        operations
            .iter()
            .filter_map(|operation| match operation.stream() {
                GraphEditStream::StreamAsset(stream_asset) => Some(*stream_asset),
                GraphEditStream::StreamId(stream_id) => {
                    the_location_of_the_stream_listed_as(&listings, stream_id)
                        .map(|stream_location| stream_location.stream_asset)
                }
            })
            .collect()
    }
}

/// The location of the stream `listings` lists under the stream id `stream_id`, when it lists
/// one with a location.
fn the_location_of_the_stream_listed_as<'listed>(
    listings: &'listed [StreamListing],
    stream_id: &str,
) -> Option<&'listed StreamLocation> {
    listings
        .iter()
        .find(|listing| listing.stream_id == stream_id)
        .and_then(|listing| listing.stream_location.as_ref())
}

/// Each of `operations` to the stream asset its `stream` names, with the stream id of the
/// stream it applies to, read from `listings`, one listing of the streams the runtime holds: a
/// stream id keeps the stream it names, and a stream asset's UUID takes the stream running it,
/// a loaded one over one only recorded. A name that matches no stream is refused with
/// `stream_not_found` at its operation, naming the streams the runtime holds.
pub(super) fn the_stream_each_operation_names(
    operations: Vec<StreamGraphAction<GraphEditStream>>,
    listings: &[StreamListing],
) -> Result<Vec<(StreamGraphAction, String)>, GraphTransactionRefusal> {
    let mut listings_loaded_last: Vec<&StreamListing> = listings.iter().collect();
    listings_loaded_last.sort_by_key(|listing| listing.node_count.is_some());
    let stream_id_of_each_stream_asset: HashMap<Uuid, &str> = listings_loaded_last
        .into_iter()
        .filter_map(|listing| {
            Some((
                listing.stream_location.as_ref()?.stream_asset,
                listing.stream_id.as_str(),
            ))
        })
        .collect();
    operations
        .into_iter()
        .enumerate()
        .map(|(operation_index, operation)| {
            let named = match operation.stream() {
                GraphEditStream::StreamId(stream_id) => {
                    the_location_of_the_stream_listed_as(listings, stream_id)
                        .map(|stream_location| (stream_location.stream_asset, stream_id.clone()))
                        .ok_or_else(|| {
                            the_words_of_a_stream_id_no_stream_is_held_under(stream_id, listings)
                        })
                }
                GraphEditStream::StreamAsset(stream_asset) => stream_id_of_each_stream_asset
                    .get(stream_asset)
                    .map(|stream_id| (*stream_asset, (*stream_id).to_owned()))
                    .ok_or_else(|| {
                        the_words_of_a_stream_asset_no_stream_runs(
                            *stream_asset,
                            &stream_id_of_each_stream_asset,
                        )
                    }),
            };
            match named {
                Ok((stream_asset, stream_id)) => {
                    Ok((operation.map_stream(|_| stream_asset), stream_id))
                }
                Err(runtime_words) => Err(GraphTransactionRefusal::of_the_operation(
                    operation_index,
                    what_a_graph_action_names(&operation),
                    GraphRequestRefusal::coded(
                        RefusalCodeAndPlace::coded(ControlOperationRefusalCode::StreamNotFound),
                        runtime_words,
                    ),
                )),
            }
        })
        .collect()
}

/// Why the stream id `stream_id` names no stream, naming each stream `listings` lists with a
/// location, the streams the runtime holds.
fn the_words_of_a_stream_id_no_stream_is_held_under(
    stream_id: &str,
    listings: &[StreamListing],
) -> String {
    format!(
        "the runtime holds no stream `{stream_id}`; the streams it holds are: {}",
        sorted_and_joined_or_none(
            listings
                .iter()
                .filter(|listing| listing.stream_location.is_some())
                .map(|listing| format!("`{}`", listing.stream_id))
                .collect()
        )
    )
}

/// Why no stream runs the stream asset `stream_asset`, naming each stream asset of
/// `stream_id_of_each_stream_asset` with the stream running it.
fn the_words_of_a_stream_asset_no_stream_runs(
    stream_asset: Uuid,
    stream_id_of_each_stream_asset: &HashMap<Uuid, &str>,
) -> String {
    format!(
        "no stream the runtime holds runs the stream asset {stream_asset}; the stream assets it \
         holds are: {}",
        sorted_and_joined_or_none(
            stream_id_of_each_stream_asset
                .iter()
                .map(|(stream_asset, stream_id)| format!("{stream_asset} (stream `{stream_id}`)"))
                .collect()
        )
    )
}

/// `names`, sorted and joined by commas, or `none` when there are none.
fn sorted_and_joined_or_none(mut names: Vec<String>) -> String {
    if names.is_empty() {
        return "none".to_owned();
    }
    names.sort();
    names.join(", ")
}
