// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `live` transaction of value sets alone, on one stream, each on an input its node reads each
//! run: handed to the stream whole, which checks every value, sets each apart from the graph and
//! announces it, with no graph write lock, no graph render, no compile and no document staged.

use streamlib::sdk::graph::input_values::{
    InputValueSetLive, InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
};

use super::*;

/// The values a live transaction sets on one stream: the stream, and each value with the index
/// of the action setting it.
struct InputValuesALiveTransactionSets {
    stream_id: String,
    sets: Vec<InputValueToSetLive>,
    request_index_of_each_set: Vec<usize>,
}

impl StreamGraphTransactionExecutor<'_> {
    /// Apply `requests`, a `live` transaction whose actions name the loaded streams
    /// `stream_id_of_each_request`, when each is a value set at the stream's own level on one
    /// stream, on an input its node reads each run: every value set live apart from the graph,
    /// whole or not at all. `None` when one action is anything else, so the transaction takes
    /// the staged path.
    pub(super) fn apply_input_values_set_live_apart_from_the_graph(
        &self,
        requests: &[StreamGraphAction],
        stream_id_of_each_request: &[String],
    ) -> Result<Option<GraphTransactionToolResult>, GraphTransactionRefusal> {
        let Some(InputValuesALiveTransactionSets {
            stream_id,
            sets,
            request_index_of_each_set,
        }) = the_input_values_a_transaction_sets_live(requests, stream_id_of_each_request)
        else {
            return Ok(None);
        };
        let stream_operations = self
            .operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(&stream_id)
            .map_err(|refusal| {
                GraphTransactionRefusal::of_the_whole_transaction(refusal.to_string())
            })?;
        let refusal_of_the_set = |set_index: usize, refusal: GraphRequestRefusal| {
            let request_index = request_index_of_each_set
                .get(set_index)
                .copied()
                .unwrap_or_default();
            GraphTransactionRefusal::of_the_operation(
                request_index,
                requests
                    .get(request_index)
                    .map(what_a_graph_action_names)
                    .unwrap_or_default(),
                refusal,
            )
        };
        let made = match stream_operations.set_input_values_live(&sets) {
            Ok(InputValuesSetLive::ApartFromTheGraph(made)) => made,
            Ok(InputValuesSetLive::OnTheGraph) => return Ok(None),
            Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { set_index, refusal }) => {
                return Err(refusal_of_the_set(
                    set_index,
                    GraphRequestRefusal::of_an_input_value(refusal, "/value".to_owned()),
                ));
            }
            Err(LiveInputValueSetRefusal::TheInputIsResetWhileStopped {
                set_index,
                runtime_words,
            }) => {
                return Err(refusal_of_the_set(
                    set_index,
                    GraphRequestRefusal::of_a_reset_input_while_stopped(runtime_words),
                ));
            }
            Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over)) => {
                return Err(GraphTransactionRefusal::of_the_whole_transaction(
                    not_handed_over.to_string(),
                ));
            }
        };

        let mut overridden_by_links = Vec::new();
        let mut operations_answered = Vec::with_capacity(made.len());
        for (set, made_live) in sets.into_iter().zip(made) {
            let InputValueSetLive {
                input,
                live_value_before,
                saved_value,
                linked_from,
            } = made_live;
            if set.value.is_some()
                && let Some((from_node, from_output)) = linked_from
            {
                overridden_by_links.push(GraphTransactionInputOverriddenByALink {
                    stream: stream_id.clone(),
                    node: set.node.to_string(),
                    input: input.clone(),
                    from_node: from_node.to_string(),
                    from_output,
                });
            }
            operations_answered.push((
                stream_id.clone(),
                StreamGraphOperation::SetNodeInputValue {
                    node: set.node,
                    input,
                    from: live_value_before.or(saved_value),
                    to: set.value,
                },
            ));
        }
        Ok(Some(self.the_answer_of_a_graph_transaction(
            GraphEditMode::Live,
            None,
            None,
            &|asked| (asked == stream_id).then(|| Arc::clone(&stream_operations)),
            operations_answered,
            WhatAnAnswerNamesBesideItsOperations {
                labels_of_added_nodes: &HashMap::new(),
                overridden_by_links,
            },
        )))
    }
}

/// Each value `requests` sets live, in order, when every one is a `set_node_input_value` or a
/// `set_pin_value` at its stream's own level naming its node by UUID, and all name one stream;
/// `None` otherwise.
fn the_input_values_a_transaction_sets_live(
    requests: &[StreamGraphAction],
    stream_id_of_each_request: &[String],
) -> Option<InputValuesALiveTransactionSets> {
    let stream_id = stream_id_of_each_request.first()?;
    let mut sets = Vec::with_capacity(requests.len());
    let mut request_index_of_each_set = Vec::with_capacity(requests.len());
    for (request_index, (request, request_stream_id)) in
        requests.iter().zip(stream_id_of_each_request).enumerate()
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
            return None;
        };
        if !level.is_empty() || request_stream_id != stream_id {
            return None;
        }
        sets.push(InputValueToSetLive {
            node: Uuid::parse_str(node).ok()?,
            input: input.clone(),
            value: value.clone().filter(|value| !value.is_null()),
        });
        request_index_of_each_set.push(request_index);
    }
    Some(InputValuesALiveTransactionSets {
        stream_id: stream_id.clone(),
        sets,
        request_index_of_each_set,
    })
}
