// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Values set live on inputs their nodes read each run: checked and held in the stream's live
//! values under the graph's read lock, handed to each node set on alone, wherever it is hosted,
//! with the lock released, and announced as `InputValueDidChange`, so no node, link or port
//! changes and nothing compiles. A set is whole or not at all: one a node or island is not handed
//! leaves every value as it was.

use parking_lot::RwLock;
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::Error;
use crate::core::compiler::compiler_ops::function_node_island::{
    hand_each_reader_an_island_evaluating_the_nodes_again_when_it_changed,
    runs_inside_the_read_of_each_node_it_feeds,
};
use crate::core::compiler::compiler_ops::unlinked_input_values_delivery::UnlinkedInputValuesDelivery;
use crate::core::descriptors::{NodeKind, NodeValueLifetime, PortFamily};
use crate::core::graph::input_values::{
    InputValueRefusal, InputValueRefusalCode, InputValueSetLive, InputValueToSetLive,
    InputValuesSetLive, LiveInputValueSetRefusal, NodeAnInputValueIsSetOn,
    refuse_an_input_value_its_input_does_not_take, the_control_value_of_a_pin,
    the_input_value_change, the_lifetime_of_the_input,
};
use crate::core::graph::legacy_config_of_a_node::{
    LegacyConfigOfANodeType, input_values_of_a_config_and_pin_values,
};
use crate::core::graph::{Graph, GraphNode, NodeRunId};
use crate::core::nodes::{NodeTypesOneStreamResolves, is_the_input_of_an_output_a_panel_writes};
use crate::core::pubsub::RuntimeEvent;

impl LoadedStreamInThisRuntime {
    /// Set each of `sets` live, under its input's current name, apart from the graph, when each
    /// is on an input its node reads each run; none when one is held on the graph, so the set
    /// takes the path a graph edit takes. Each value is checked before any is set, handed to the
    /// helper and the islands reading it, and announced, one change of an input value of this
    /// stream at a time.
    pub(crate) fn set_input_values_live(
        &self,
        sets: &[InputValueToSetLive],
    ) -> std::result::Result<InputValuesSetLive, LiveInputValueSetRefusal> {
        self.with_no_other_input_value_change(|| {
            let playback = if self.is_held_stopped() {
                PlaybackOfTheStreamSetOn::Stopped
            } else {
                PlaybackOfTheStreamSetOn::NotStopped
            };
            let Some(held_live) = set_the_input_values_live_and_hand_each_node_its_own(
                self.compiler.graph_lock(),
                sets,
                playback,
            )?
            else {
                return Ok(InputValuesSetLive::OnTheGraph);
            };
            for change in held_live.changes_announced {
                self.publish_on_this_streams_topic(change);
            }
            Ok(InputValuesSetLive::ApartFromTheGraph(held_live.made))
        })
    }

    /// Run `change`, a change of one of this stream's input values, while no other is made.
    pub(in crate::core::runtime) fn with_no_other_input_value_change<T>(
        &self,
        change: impl FnOnce() -> T,
    ) -> T {
        let _one_change_at_a_time = self.one_input_value_change_at_a_time.lock();
        change()
    }
}

/// Each of `sets` with its input under the name the input of its node in `graph` has now; a set
/// on a node or an input the graph does not hold is left as given.
fn the_sets_under_the_current_input_names(
    graph: &Graph,
    sets: &[InputValueToSetLive],
) -> Vec<InputValueToSetLive> {
    sets.iter()
        .map(|set| InputValueToSetLive {
            input: graph
                .node_by_uuid(set.node)
                .and_then(|set_on| set_on.input_port(&set.input))
                .map_or_else(|| set.input.clone(), |input| input.name.clone()),
            ..set.clone()
        })
        .collect()
}

/// Whether the playback of the stream a live set is made on is stopped, so a `reset` input
/// holds no live value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackOfTheStreamSetOn {
    /// Stopped, its graph held to read and edit: a live set on a `kept` input is held for the
    /// next Play, and one on a `reset` input refused.
    Stopped,
    /// Playing, paused or starting.
    NotStopped,
}

/// What a live set held apart from the graph made: each value set, the change of each input to
/// announce, how each node set on outside an island is handed its values, and the function nodes
/// set on that run inside an island, whose islands are handed again under the graph's write lock.
pub(crate) struct InputValuesHeldLive {
    pub(crate) made: Vec<InputValueSetLive>,
    pub(crate) changes_announced: Vec<RuntimeEvent>,
    deliveries: Vec<UnlinkedInputValuesDelivery>,
    function_nodes_read_inside_an_island: Vec<NodeRunId>,
}

/// Set each of `sets` live, under its input's current name, in the live values of the graph
/// `graph` locks, then hand each node set
/// on its values, its helper or itself with the lock released and its islands under the write
/// lock; `None`, with nothing set, when one is held on the graph. A value its input does not
/// take, and a value on a `reset` input while `playback` is stopped, is refused before any is
/// set, and a node or island not handed its values leaves every live value as it was.
pub(crate) fn set_the_input_values_live_and_hand_each_node_its_own(
    graph: &RwLock<Graph>,
    sets: &[InputValueToSetLive],
    playback: PlaybackOfTheStreamSetOn,
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    let (sets, held_live) = {
        let graph = graph.read();
        let sets = the_sets_under_the_current_input_names(&graph, sets);
        let held_live = set_the_input_values_live(&graph, &sets, playback);
        (sets, held_live)
    };
    let sets = sets.as_slice();
    let Some(mut held_live) = held_live? else {
        return Ok(None);
    };
    let handed_over = std::mem::take(&mut held_live.deliveries)
        .into_iter()
        .try_for_each(UnlinkedInputValuesDelivery::hand_over);
    if let Err(not_handed_over) = handed_over {
        let handed_back = put_back_the_live_set(&graph.read(), sets, &held_live.made);
        hand_back_each(handed_back);
        return Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over));
    }
    if held_live.function_nodes_read_inside_an_island.is_empty() {
        return Ok(Some(held_live));
    }
    let handed_to_each_island = hand_each_island_again_or_put_the_values_back(
        &mut graph.write(),
        &held_live.function_nodes_read_inside_an_island,
        |graph| put_back_the_live_set(graph, sets, &held_live.made),
    );
    if let Err((not_handed_over, handed_back)) = handed_to_each_island {
        hand_back_each(handed_back);
        return Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over));
    }
    Ok(Some(held_live))
}

/// Set each of `sets` live in `graph`'s live values and answer how each node set on takes its
/// values; `None`, with nothing set, when one is held on the graph. A value its input does not
/// take, and a value on a `reset` input while `playback` is stopped, is refused before any is
/// set.
fn set_the_input_values_live(
    graph: &Graph,
    sets: &[InputValueToSetLive],
    playback: PlaybackOfTheStreamSetOn,
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    let mut set_on_each: Vec<&GraphNode> = Vec::with_capacity(sets.len());
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
        .and_then(|()| {
            refuse_a_value_its_own_helper_does_not_read(
                graph,
                set_on,
                &set.input,
                set.value.as_ref(),
            )
        })
        .map_err(|refusal| LiveInputValueSetRefusal::TheInputDoesNotTakeIt {
            set_index,
            refusal,
        })?;
        set_on_each.push(set_on);
    }
    if playback == PlaybackOfTheStreamSetOn::Stopped {
        refuse_a_value_on_a_reset_input_while_stopped(
            graph.node_types_this_stream_resolves(),
            sets,
            &set_on_each,
        )?;
    }

    let made: Vec<InputValueSetLive> = sets
        .iter()
        .zip(&set_on_each)
        .map(|(set, set_on)| InputValueSetLive {
            input: set.input.clone(),
            live_value_before: graph.live_input_values().set(
                set.node,
                &set.input,
                set.value.clone(),
                the_lifetime_of_the_input(set_on, &set.input),
            ),
            saved_value: set_on.pins.get(&set.input).cloned(),
            linked_from: the_output_feeding_the_input(graph, set_on, &set.input),
        })
        .collect();
    let changes_announced = sets
        .iter()
        .zip(&set_on_each)
        .filter_map(|(set, set_on)| the_input_value_change(graph, set_on, &set.input))
        .collect();
    let mut deliveries = Vec::new();
    let mut function_nodes_read_inside_an_island = Vec::new();
    for set_on in each_once(&set_on_each) {
        match UnlinkedInputValuesDelivery::of_the_node(graph, set_on) {
            UnlinkedInputValuesDelivery::EachIslandEvaluatingIt(function_node) => {
                function_nodes_read_inside_an_island.push(function_node);
            }
            delivery => deliveries.push(
                delivery.naming_the_inputs_set_live(
                    sets.iter()
                        .filter(|set| set.node == set_on.node_uuid)
                        .map(|set| set.input.clone())
                        .collect(),
                ),
            ),
        }
    }
    Ok(Some(InputValuesHeldLive {
        made,
        changes_announced,
        deliveries,
        function_nodes_read_inside_an_island,
    }))
}

/// Hand each reader of an island evaluating one of `function_nodes` that island again; when one
/// is not handed it, put the values set back with `put_back`, hand each of those islands back
/// what it reads then, logging one not handed it, and answer the refusal with what `put_back`
/// answered.
pub(super) fn hand_each_island_again_or_put_the_values_back<PutBack>(
    graph: &mut Graph,
    function_nodes: &[NodeRunId],
    put_back: impl FnOnce(&mut Graph) -> PutBack,
) -> std::result::Result<(), (Error, PutBack)> {
    let Err(not_handed_over) =
        hand_each_reader_an_island_evaluating_the_nodes_again_when_it_changed(
            graph,
            function_nodes,
        )
    else {
        return Ok(());
    };
    let put_back = put_back(graph);
    if let Err(not_handed_back) =
        hand_each_reader_an_island_evaluating_the_nodes_again_when_it_changed(graph, function_nodes)
    {
        tracing::error!(
            %not_handed_back,
            "an island was not handed back the values a refused set replaced"
        );
    }
    Err((not_handed_over, put_back))
}

/// Hand each node of `handed_back` back the values it reads, logging one not handed them.
pub(crate) fn hand_back_each(handed_back: impl IntoIterator<Item = UnlinkedInputValuesDelivery>) {
    for delivery in handed_back {
        if let Err(not_handed_back) = delivery.hand_over() {
            tracing::error!(
                %not_handed_back,
                "a node was not handed back the values a refused set replaced"
            );
        }
    }
}

/// Whether a value set on input `input` of `set_on` is held on `graph` rather than live: a
/// setting its type holds in its config, a config field it exposes as an input, a setting it
/// reads only when it is set up, or an input it does not have.
pub(super) fn is_held_on_the_graph(graph: &Graph, set_on: &GraphNode, input: &str) -> bool {
    LegacyConfigOfANodeType::of(graph.node_types_this_stream_resolves(), &set_on.node_type)
        .is_a_config_field(input)
        || set_on.is_an_exposed_config_field_input(input)
        || set_on
            .input_port(input)
            .is_none_or(|port| !port.takes_a_wire() && !port.is_a_setting_read_each_run())
}

/// Refuse `value`, set live on input `input` of `set_on`, a function node run in a helper of its
/// own, when the input is a Channels input and the value is no number or bool: the pin values
/// that helper is handed carry only those, so the value would reach no run and hide the saved
/// one.
fn refuse_a_value_its_own_helper_does_not_read(
    graph: &Graph,
    set_on: &GraphNode,
    input: &str,
    value: Option<&serde_json::Value>,
) -> std::result::Result<(), InputValueRefusal> {
    let (Some(value), Some(port)) = (value, set_on.input_port(input)) else {
        return Ok(());
    };
    let runs_in_a_helper_of_its_own = matches!(set_on.node_kind(), Some(NodeKind::Function { .. }))
        && !runs_inside_the_read_of_each_node_it_feeds(graph, &set_on.id);
    if !runs_in_a_helper_of_its_own
        || port.family.family != PortFamily::Channels
        || the_control_value_of_a_pin(value).is_some()
    {
        return Ok(());
    }
    Err(InputValueRefusal {
        code: InputValueRefusalCode::InputValueNotValid,
        words: format!(
            "the value of input `{input}` of `{}` is {value}, and `{}` runs in a helper of its \
             own, whose Channels input reads a number or a bool. To fix: send a number or a bool",
            set_on.display_name, set_on.display_name
        ),
    })
}

/// Refuse the first of `sets`, each set on the node `set_on_each` names at its index, that sets
/// a value on a `reset` input: the stream's playback is stopped, and a `reset` input holds no
/// live value until it plays. Taking a live value off is no refusal, there being none to take.
fn refuse_a_value_on_a_reset_input_while_stopped(
    node_types: &NodeTypesOneStreamResolves,
    sets: &[InputValueToSetLive],
    set_on_each: &[&GraphNode],
) -> std::result::Result<(), LiveInputValueSetRefusal> {
    let refused = sets
        .iter()
        .zip(set_on_each)
        .enumerate()
        .find(|(_, (set, set_on))| {
            set.value.is_some()
                && the_lifetime_of_the_input(set_on, &set.input) == NodeValueLifetime::Reset
        });
    let Some((set_index, (set, set_on))) = refused else {
        return Ok(());
    };
    let what_it_returns_to =
        if is_the_input_of_an_output_a_panel_writes(node_types, &set_on.node_type, &set.input) {
            "no value"
        } else {
            "its saved value"
        };
    Err(LiveInputValueSetRefusal::TheInputIsResetWhileStopped {
        set_index,
        runtime_words: format!(
            "input `{}` of `{}` returns to {what_it_returns_to} at Stop, so it keeps no value set \
             live while the stream is stopped; `start_playback` plays it",
            set.input, set_on.display_name
        ),
    })
}

/// Each of `nodes` once, in the order first named.
fn each_once<'graph>(nodes: &[&'graph GraphNode]) -> Vec<&'graph GraphNode> {
    let mut each: Vec<&GraphNode> = Vec::with_capacity(nodes.len());
    for node in nodes {
        if !each.iter().any(|held| held.node_uuid == node.node_uuid) {
            each.push(node);
        }
    }
    each
}

/// Set back, last first, the live value each of `sets` replaced, as `made` holds it, skipping a
/// node the graph no longer holds; answers how each node set on outside an island is handed back
/// the values it reads now.
fn put_back_the_live_set(
    graph: &Graph,
    sets: &[InputValueToSetLive],
    made: &[InputValueSetLive],
) -> Vec<UnlinkedInputValuesDelivery> {
    let mut nodes_set_on: Vec<&GraphNode> = Vec::with_capacity(sets.len());
    for (set, made) in sets.iter().zip(made).rev() {
        let Some(set_on) = graph.node_by_uuid(set.node) else {
            continue;
        };
        graph.live_input_values().set(
            set.node,
            &set.input,
            made.live_value_before.clone(),
            the_lifetime_of_the_input(set_on, &set.input),
        );
        nodes_set_on.push(set_on);
    }
    each_once(&nodes_set_on)
        .into_iter()
        .map(|set_on| UnlinkedInputValuesDelivery::of_the_node(graph, set_on))
        .collect()
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
