// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `live` transaction of value sets alone, each on an input its node reads each run: resolved,
//! every value checked, every value set live apart from the graph, and answered, with no graph
//! write lock, no graph render, no compile and no document staged.

use streamlib::sdk::graph::input_values::{InputValueSetLive, LiveInputValueHolding};

use super::*;

/// One value a live transaction sets: the index of its action, the loaded stream and node it
/// names, its input and its value, `None` taking the live value off.
struct OneInputValueSetLive {
    request_index: usize,
    stream_id: String,
    node: Uuid,
    input: String,
    value: Option<Value>,
}

impl StreamGraphTransactionExecutor<'_> {
    /// Apply `requests`, a `live` transaction whose actions name the loaded streams
    /// `stream_id_of_each_request`, when each is a value set on an input its node reads each
    /// run, at the stream's own level: every value checked before any is set, then every one set
    /// live apart from the graph, adjacent sets on one node together. `None` when one action is
    /// anything else, so the transaction takes the staged path.
    pub(super) fn apply_input_values_set_live_apart_from_the_graph(
        &self,
        requests: &[StreamGraphAction],
        stream_id_of_each_request: &[String],
    ) -> Result<Option<GraphTransactionToolResult>, GraphTransactionRefusal> {
        let Some(sets) =
            the_input_values_a_transaction_sets_live(requests, stream_id_of_each_request)
        else {
            return Ok(None);
        };
        let mut stream_operations_of_each_stream: HashMap<String, Arc<dyn RuntimeOperations>> =
            HashMap::new();
        for set in &sets {
            if stream_operations_of_each_stream.contains_key(&set.stream_id) {
                continue;
            }
            let stream_operations = self
                .operations_on_the_loaded_streams
                .runtime_operations_of_the_stream_a_call_names(&set.stream_id)
                .map_err(|refusal| {
                    GraphTransactionRefusal::of_the_whole_transaction(refusal.to_string())
                })?;
            stream_operations_of_each_stream.insert(set.stream_id.clone(), stream_operations);
        }
        let stream_operations_of = |stream_id: &str| {
            stream_operations_of_each_stream
                .get(stream_id)
                .cloned()
                .ok_or_else(|| {
                    GraphTransactionRefusal::of_the_whole_transaction(format!(
                        "stream `{stream_id}` is not loaded"
                    ))
                })
        };

        for set in &sets {
            match stream_operations_of(&set.stream_id)?.how_a_live_input_value_is_held(
                set.node,
                &set.input,
                set.value.as_ref(),
            ) {
                Ok(LiveInputValueHolding::ApartFromTheGraph) => {}
                Ok(LiveInputValueHolding::OnTheGraph) => return Ok(None),
                Err(refusal) => {
                    return Err(GraphTransactionRefusal::of_the_operation(
                        set.request_index,
                        what_a_graph_action_names(&requests[set.request_index]),
                        GraphRequestRefusal::of_an_input_value(refusal, "/value".to_owned()),
                    ));
                }
            }
        }

        let mut made: Vec<(&OneInputValueSetLive, InputValueSetLive)> =
            Vec::with_capacity(sets.len());
        for sets_on_one_node in
            sets.chunk_by(|one, next| one.stream_id == next.stream_id && one.node == next.node)
        {
            let first = &sets_on_one_node[0];
            let set_live = stream_operations_of(&first.stream_id)?.set_input_values_live(
                first.node,
                sets_on_one_node
                    .iter()
                    .map(|set| (set.input.clone(), set.value.clone()))
                    .collect(),
            );
            match set_live {
                Ok(made_on_the_node) => made.extend(sets_on_one_node.iter().zip(made_on_the_node)),
                Err(refusal) => {
                    self.reverse_the_input_values_set_live(&stream_operations_of_each_stream, made);
                    return Err(GraphTransactionRefusal::of_the_operation(
                        first.request_index,
                        what_a_graph_action_names(&requests[first.request_index]),
                        refusal.to_string(),
                    ));
                }
            }
        }

        let mut overridden_by_links = Vec::new();
        let mut operations_answered = Vec::with_capacity(made.len());
        for (set, made_live) in made {
            if set.value.is_some()
                && let Some((from_node, from_output)) = &made_live.linked_from
            {
                overridden_by_links.push(GraphTransactionInputOverriddenByALink {
                    stream: set.stream_id.clone(),
                    node: set.node.to_string(),
                    input: set.input.clone(),
                    from_node: from_node.to_string(),
                    from_output: from_output.clone(),
                });
            }
            operations_answered.push((
                set.stream_id.clone(),
                StreamGraphOperation::SetNodeInputValue {
                    node: set.node,
                    input: set.input.clone(),
                    from: made_live.live_value_before.or(made_live.saved_value),
                    to: set.value.clone(),
                },
            ));
        }
        Ok(Some(self.the_answer_of_a_graph_transaction(
            GraphEditMode::Live,
            None,
            None,
            &|stream_id| stream_operations_of_each_stream.get(stream_id).cloned(),
            operations_answered,
            WhatAnAnswerNamesBesideItsOperations {
                labels_of_added_nodes: &HashMap::new(),
                overridden_by_links,
            },
        )))
    }

    /// Set back, last first, the live value each of `made` replaced; one not set back is logged.
    fn reverse_the_input_values_set_live(
        &self,
        stream_operations_of_each_stream: &HashMap<String, Arc<dyn RuntimeOperations>>,
        made: Vec<(&OneInputValueSetLive, InputValueSetLive)>,
    ) {
        for (set, made_live) in made.into_iter().rev() {
            let Some(stream_operations) = stream_operations_of_each_stream.get(&set.stream_id)
            else {
                continue;
            };
            if let Err(not_reversed) = stream_operations.set_input_values_live(
                set.node,
                vec![(set.input.clone(), made_live.live_value_before)],
            ) {
                tracing::error!(
                    stream = %set.stream_id,
                    node = %set.node,
                    input = set.input,
                    %not_reversed,
                    "could not set back a live value of a refused live transaction"
                );
            }
        }
    }
}

/// Each value `requests` sets live, in order, when every one is a `set_node_input_value` or a
/// `set_pin_value` at its stream's own level naming its node by UUID; `None` otherwise.
fn the_input_values_a_transaction_sets_live(
    requests: &[StreamGraphAction],
    stream_id_of_each_request: &[String],
) -> Option<Vec<OneInputValueSetLive>> {
    requests
        .iter()
        .zip(stream_id_of_each_request)
        .enumerate()
        .map(|(request_index, (request, stream_id))| {
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
            if !level.is_empty() {
                return None;
            }
            Some(OneInputValueSetLive {
                request_index,
                stream_id: stream_id.clone(),
                node: Uuid::parse_str(node).ok()?,
                input: input.clone(),
                value: value.clone().filter(|value| !value.is_null()),
            })
        })
        .collect()
}
