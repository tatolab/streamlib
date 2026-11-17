// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Values set live on inputs their nodes read each run: checked, held in the stream's live
//! values under the graph's read lock, handed to the helper and the islands that read them, and
//! announced as `InputValueDidChange`, so no node, link or port changes and nothing compiles. A
//! set is whole or not at all: one a helper or island is not handed leaves every value as it was.

use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::Result;
use crate::core::compiler::compiler_ops::function_node_island::{
    hand_each_reader_an_island_evaluating_the_node_again_when_it_changed,
    runs_inside_the_read_of_each_node_it_feeds,
};
use crate::core::compiler::compiler_ops::input_port_pin_values::hand_the_helper_of_the_node_the_pin_values_its_input_ports_read;
use crate::core::graph::input_values::{
    InputValueSetLive, InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
    NodeAnInputValueIsSetOn, refuse_an_input_value_its_input_does_not_take, the_input_value_change,
};
use crate::core::graph::legacy_config_of_a_node::{
    LegacyConfigOfANodeType, input_values_of_a_config_and_pin_values,
};
use crate::core::graph::{Graph, GraphNode, NodeRunId};
use crate::core::pubsub::RuntimeEvent;

impl LoadedStreamInThisRuntime {
    /// Set each of `sets` live, apart from the graph, when each is on an input its node reads
    /// each run; none when one is held on the graph, so the set takes the path a graph edit
    /// takes. Each value is checked before any is set, handed to the helper and the islands
    /// reading it, and announced, one change of an input value of this stream at a time.
    pub(crate) fn set_input_values_live(
        &self,
        sets: &[InputValueToSetLive],
    ) -> std::result::Result<InputValuesSetLive, LiveInputValueSetRefusal> {
        let _one_change_at_a_time = self.one_input_value_change_at_a_time.lock();
        let held_live = self.compiler.scope_reading(|graph| {
            set_the_input_values_live_and_hand_each_helper_its_own(graph, sets)
        })?;
        let Some(held_live) = held_live else {
            return Ok(InputValuesSetLive::OnTheGraph);
        };
        if !held_live.function_nodes_read_inside_an_island.is_empty() {
            let handed = self.compiler.scope(|graph, _tx| {
                hand_each_reader_an_island_evaluating_the_nodes_again(
                    graph,
                    &held_live.function_nodes_read_inside_an_island,
                )
            });
            if let Err(not_handed_over) = handed {
                self.compiler.scope(|graph, _tx| {
                    put_back_the_live_values_a_set_replaced(graph, sets, &held_live.made);
                    hand_each_helper_reading_the_nodes_its_own(graph, sets);
                    if let Err(not_handed_back) =
                        hand_each_reader_an_island_evaluating_the_nodes_again(
                            graph,
                            &held_live.function_nodes_read_inside_an_island,
                        )
                    {
                        tracing::error!(
                            %not_handed_back,
                            "an island was not handed back the values a refused live set replaced"
                        );
                    }
                });
                return Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over));
            }
        }
        for change in held_live.changes_announced {
            self.publish_on_this_streams_topic(change);
        }
        Ok(InputValuesSetLive::ApartFromTheGraph(held_live.made))
    }

    /// Run `change`, a change of one of this stream's input values, while no other is made.
    pub(super) fn with_no_other_input_value_change<T>(&self, change: impl FnOnce() -> T) -> T {
        let _one_change_at_a_time = self.one_input_value_change_at_a_time.lock();
        change()
    }
}

/// What a live set held apart from the graph made: each value set, the change of each input to
/// announce, and the function nodes set on that run inside an island, whose islands are handed
/// again under the graph's write lock.
pub(crate) struct InputValuesHeldLive {
    pub(crate) made: Vec<InputValueSetLive>,
    pub(crate) changes_announced: Vec<RuntimeEvent>,
    pub(crate) function_nodes_read_inside_an_island: Vec<NodeRunId>,
}

/// Set each of `sets` live in `graph`'s live values and hand the helper of each node set on its
/// new pin values; `None`, with nothing set, when one is held on the graph. A value its input
/// does not take is refused before any is set, and a helper not handed its values leaves every
/// live value as it was.
pub(crate) fn set_the_input_values_live_and_hand_each_helper_its_own(
    graph: &Graph,
    sets: &[InputValueToSetLive],
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    for (set_index, set) in sets.iter().enumerate() {
        let Some(set_on) = graph.node_by_uuid(set.node) else {
            return Ok(None);
        };
        if is_held_on_the_graph(graph, set_on, &set.input) {
            return Ok(None);
        }
        refuse_an_input_value_its_input_does_not_take(
            graph.node_types_this_stream_resolves(),
            &NodeAnInputValueIsSetOn {
                name: &set_on.display_name,
                node_type: &set_on.node_type,
                input_values_after_the_set: &input_values_of_a_config_and_pin_values(
                    set_on.config.as_ref(),
                    &set_on.pins,
                ),
            },
            &set.input,
            set.value.as_ref(),
        )
        .map_err(|refusal| LiveInputValueSetRefusal::TheInputDoesNotTakeIt {
            set_index,
            refusal,
        })?;
    }

    let made: Vec<InputValueSetLive> = sets
        .iter()
        .map(|set| {
            let live_value_before =
                graph
                    .live_input_values()
                    .set(set.node, &set.input, set.value.clone());
            let set_on = graph.node_by_uuid(set.node);
            InputValueSetLive {
                input: set.input.clone(),
                live_value_before,
                saved_value: set_on.and_then(|set_on| set_on.pins.get(&set.input).cloned()),
                linked_from: set_on
                    .and_then(|set_on| the_output_feeding_the_input(graph, set_on, &set.input)),
            }
        })
        .collect();

    for set_on in the_nodes_set_on(graph, sets) {
        if let Err(not_handed_over) =
            hand_the_helper_of_the_node_the_pin_values_its_input_ports_read(graph, set_on)
        {
            put_back_the_live_values_a_set_replaced(graph, sets, &made);
            hand_each_helper_reading_the_nodes_its_own(graph, sets);
            return Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over));
        }
    }

    let changes_announced = sets
        .iter()
        .filter_map(|set| {
            graph
                .node_by_uuid(set.node)
                .and_then(|set_on| the_input_value_change(graph, set_on, &set.input))
        })
        .collect();
    let function_nodes_read_inside_an_island = the_nodes_set_on(graph, sets)
        .into_iter()
        .filter(|set_on| runs_inside_the_read_of_each_node_it_feeds(graph, &set_on.id))
        .map(|set_on| set_on.id.clone())
        .collect();
    Ok(Some(InputValuesHeldLive {
        made,
        changes_announced,
        function_nodes_read_inside_an_island,
    }))
}

/// Whether a value set on input `input` of `set_on` is held on `graph` rather than live: a
/// setting its type holds in its config, a config field it exposes as an input, or an input it
/// does not have.
fn is_held_on_the_graph(graph: &Graph, set_on: &GraphNode, input: &str) -> bool {
    LegacyConfigOfANodeType::of(graph.node_types_this_stream_resolves(), &set_on.node_type)
        .is_a_config_field(input)
        || set_on.is_an_exposed_config_field_input(input)
        || !set_on.has_input(input)
}

/// Each node of `graph` one of `sets` names, once, in the order first named.
fn the_nodes_set_on<'graph>(
    graph: &'graph Graph,
    sets: &[InputValueToSetLive],
) -> Vec<&'graph GraphNode> {
    let mut nodes: Vec<Uuid> = Vec::with_capacity(sets.len());
    for set in sets {
        if !nodes.contains(&set.node) {
            nodes.push(set.node);
        }
    }
    nodes
        .into_iter()
        .filter_map(|node| graph.node_by_uuid(node))
        .collect()
}

/// Set back, last first, the live value each of `sets` replaced, as `made` holds it.
fn put_back_the_live_values_a_set_replaced(
    graph: &Graph,
    sets: &[InputValueToSetLive],
    made: &[InputValueSetLive],
) {
    for (set, made) in sets.iter().zip(made).rev() {
        graph
            .live_input_values()
            .set(set.node, &set.input, made.live_value_before.clone());
    }
}

/// Hand the helper of each node `sets` names the pin values it reads now, logging one not handed.
fn hand_each_helper_reading_the_nodes_its_own(graph: &Graph, sets: &[InputValueToSetLive]) {
    for set_on in the_nodes_set_on(graph, sets) {
        if let Err(not_handed_back) =
            hand_the_helper_of_the_node_the_pin_values_its_input_ports_read(graph, set_on)
        {
            tracing::error!(
                node = %set_on.display_name,
                %not_handed_back,
                "a helper was not handed back the values a refused live set replaced"
            );
        }
    }
}

/// Hand each reader of an island evaluating one of `function_nodes` that island again, when it
/// changed.
fn hand_each_reader_an_island_evaluating_the_nodes_again(
    graph: &mut Graph,
    function_nodes: &[NodeRunId],
) -> Result<()> {
    for function_node in function_nodes {
        hand_each_reader_an_island_evaluating_the_node_again_when_it_changed(graph, function_node)?;
    }
    Ok(())
}

/// The output that a link of `graph` feeds input `input` of `node` from, as `(node UUID, port)`.
pub(crate) fn the_output_feeding_the_input(
    graph: &Graph,
    node: &GraphNode,
    input: &str,
) -> Option<(Uuid, String)> {
    let source = graph.link_into(&node.id, input)?.source();
    graph
        .traversal()
        .v(source.node_run_id())
        .first()
        .map(|feeding| (feeding.node_uuid, source.port_name().to_owned()))
}

#[cfg(test)]
mod tests;
