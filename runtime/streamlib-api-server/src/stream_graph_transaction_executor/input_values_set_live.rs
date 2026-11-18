// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `live` transaction of value sets alone, on one stream, each on an input its node reads each
//! run: handed to the stream whole, which checks every value, sets each apart from the graph and
//! announces it, with no graph write lock, no graph render, no compile and no document staged.

use streamlib::sdk::error::PortDirection;
use streamlib::sdk::graph::GraphNodePorts;
use streamlib::sdk::graph::input_values::{
    InputValueSetLive, InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
    the_words_of_an_input_set_twice_live,
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
    /// Refuse `requests`, a `live` transaction whose actions name the loaded streams
    /// `stream_id_of_each_request`, when two of its value sets name one input of one node, at
    /// the second, whatever else it holds and whichever path applies it: the second value would
    /// replace the first before the node is handed either, so the first would never be
    /// published. A node is the one its UUID or an earlier `add_node`'s `as` names at its level;
    /// an input, the one its name or a name it had finds, on a node of the stream's own level or
    /// one the transaction adds, and the one its name finds on any other.
    pub(super) fn refuse_an_input_set_twice_in_a_live_transaction(
        &self,
        requests: &[StreamGraphAction],
        stream_id_of_each_request: &[String],
    ) -> Result<(), GraphTransactionRefusal> {
        let sets = the_value_set_of_each_request(requests, stream_id_of_each_request);
        let mut input_names_of_each_node: HashMap<&NodeAValueSetNames, HashSet<&str>> =
            HashMap::new();
        for (_, node, input) in &sets {
            input_names_of_each_node
                .entry(node)
                .or_default()
                .insert(input.as_str());
        }
        let mut ports_of_each_node: HashMap<&NodeAValueSetNames, Option<GraphNodePorts>> =
            HashMap::new();
        let mut document_of_each_stream: HashMap<String, Option<StreamGraphDocument>> =
            HashMap::new();
        let mut inputs_set: HashSet<(&NodeAValueSetNames, String)> = HashSet::new();
        for (request_index, node, input) in &sets {
            let current_input = if input_names_of_each_node[node].len() > 1 {
                ports_of_each_node
                    .entry(node)
                    .or_insert_with(|| {
                        self.the_ports_of_the_node_a_value_set_names(
                            node,
                            requests,
                            stream_id_of_each_request,
                            &mut document_of_each_stream,
                        )
                    })
                    .as_ref()
                    .and_then(|ports| ports.current_name(PortDirection::Input, input))
                    .unwrap_or(input)
                    .to_owned()
            } else {
                input.clone()
            };
            if inputs_set.contains(&(node, current_input.clone())) {
                let (stream_id, level, named) = node;
                let node_name = Uuid::parse_str(named)
                    .ok()
                    .filter(|_| level.is_empty())
                    .and_then(|node_uuid| {
                        document_of_each_stream
                            .entry(stream_id.clone())
                            .or_insert_with(|| self.the_document_a_stream_runs(stream_id))
                            .as_ref()?
                            .node(node_uuid)
                            .ok()
                    })
                    .map_or_else(|| named.clone(), |node| node.name);
                return Err(GraphTransactionRefusal::of_the_operation(
                    *request_index,
                    what_a_graph_action_names(&requests[*request_index]),
                    GraphRequestRefusal::from(the_words_of_an_input_set_twice_live(
                        &current_input,
                        &node_name,
                    )),
                ));
            }
            inputs_set.insert((node, current_input));
        }
        Ok(())
    }

    /// The ports of `node`, a node a value set of `requests` names: on the stream's own level
    /// by UUID, as the stream runs it, and by an earlier `add_node`'s `as`, as its type
    /// declares; `None` for a node on another level or one neither finds.
    fn the_ports_of_the_node_a_value_set_names(
        &self,
        (stream_id, level, named): &NodeAValueSetNames,
        requests: &[StreamGraphAction],
        stream_id_of_each_request: &[String],
        document_of_each_stream: &mut HashMap<String, Option<StreamGraphDocument>>,
    ) -> Option<GraphNodePorts> {
        let node_type = match Uuid::parse_str(named) {
            Ok(node_uuid) if level.is_empty() => {
                document_of_each_stream
                    .entry(stream_id.clone())
                    .or_insert_with(|| self.the_document_a_stream_runs(stream_id))
                    .as_ref()?
                    .node(node_uuid)
                    .ok()?
                    .node_type
            }
            Ok(_) => return None,
            Err(_) => requests.iter().zip(stream_id_of_each_request).find_map(
                |(request, request_stream_id)| match request {
                    StreamGraphAction::AddNode {
                        level: added_level,
                        node_type,
                        label: Some(label),
                        ..
                    } if request_stream_id == stream_id
                        && added_level == level
                        && label == named =>
                    {
                        Some(node_type.clone())
                    }
                    _ => None,
                },
            )?,
        };
        self.operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(stream_id)
            .ok()?
            .the_ports_a_node_type_declares(&node_type)
    }

    /// The graph the loaded stream `stream_id` runs, as a document; `None` when it cannot be
    /// read.
    fn the_document_a_stream_runs(&self, stream_id: &str) -> Option<StreamGraphDocument> {
        let stream_operations = self
            .operations_on_the_loaded_streams
            .runtime_operations_of_the_stream_a_call_names(stream_id)
            .ok()?;
        StreamGraphDocument::from_value(
            self.async_runtime
                .block_on(stream_operations.to_json_async())
                .ok()?,
        )
        .ok()
    }

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
            .map_err(GraphTransactionRefusal::of_the_whole_transaction)?;
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
                    GraphRequestRefusal::of_an_input_value(refusal, Some("/value".to_owned())),
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
            Err(LiveInputValueSetRefusal::TheInputIsSetTwice {
                set_index,
                runtime_words,
            }) => {
                return Err(refusal_of_the_set(
                    set_index,
                    GraphRequestRefusal::from(runtime_words),
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

/// A node, by the stream id, level and UUID or `as` a value set gives it.
type NodeAValueSetNames = (String, Vec<Uuid>, String);

/// Each value set of `requests` — a `set_node_input_value` or a `set_pin_value` — in order: its
/// index, the node it names, a UUID as the runtime writes one, and the input as it names it.
fn the_value_set_of_each_request(
    requests: &[StreamGraphAction],
    stream_id_of_each_request: &[String],
) -> Vec<(usize, NodeAValueSetNames, String)> {
    requests
        .iter()
        .zip(stream_id_of_each_request)
        .enumerate()
        .filter_map(|(request_index, (request, stream_id))| match request {
            StreamGraphAction::SetNodeInputValue {
                level, node, input, ..
            }
            | StreamGraphAction::SetPinValue {
                level, node, input, ..
            } => {
                let node = Uuid::parse_str(node)
                    .map_or_else(|_| node.clone(), |node_uuid| node_uuid.to_string());
                Some((
                    request_index,
                    (stream_id.clone(), level.clone(), node),
                    input.clone(),
                ))
            }
            _ => None,
        })
        .collect()
}
