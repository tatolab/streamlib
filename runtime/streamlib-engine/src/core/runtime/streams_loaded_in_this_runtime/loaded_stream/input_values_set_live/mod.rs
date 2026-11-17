// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Values set live on inputs their nodes read each run: checked, held in the stream's live
//! values under the graph's read lock, handed to the helper and the islands that read them, and
//! announced as `InputValueDidChange`, so no node, link or port changes and nothing compiles. A
//! set is whole or not at all: one a helper or island is not handed leaves every value as it was.

use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::compiler::compiler_ops::function_node_island::{
    hand_each_reader_an_island_evaluating_the_nodes_again_when_it_changed,
    runs_inside_the_read_of_each_node_it_feeds,
};
use crate::core::compiler::compiler_ops::input_port_pin_values::{
    hand_the_helper_of_the_node_the_pin_values_its_input_ports_read, the_control_value_of_a_pin,
};
use crate::core::descriptors::{NodeKind, NodeValueLifetime, PortFamily};
use crate::core::error::{Error, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::input_values::{
    InputValueRefusal, InputValueRefusalCode, InputValueSetLive, InputValueToSetLive,
    InputValuesSetLive, LiveInputValueSetRefusal, NodeAnInputValueIsSetOn,
    refuse_an_input_value_its_input_does_not_take, the_input_value_change,
    the_lifetime_of_the_input,
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
        self.with_no_other_input_value_change(|| {
            let playback = if self.is_held_stopped() {
                PlaybackOfTheStreamSetOn::Stopped
            } else {
                PlaybackOfTheStreamSetOn::NotStopped
            };
            let held_live = self.compiler.scope_reading(|graph| {
                set_the_input_values_live_and_hand_each_helper_its_own(graph, sets, playback)
            })?;
            let Some(held_live) = held_live else {
                return Ok(InputValuesSetLive::OnTheGraph);
            };
            if !held_live.function_nodes_read_inside_an_island.is_empty() {
                self.compiler.scope(|graph, _tx| {
                    hand_each_island_the_live_set_or_put_it_back(graph, sets, &held_live)
                })?;
            }
            for change in held_live.changes_announced {
                self.publish_on_this_streams_topic(change);
            }
            Ok(InputValuesSetLive::ApartFromTheGraph(held_live.made))
        })
    }

    /// Run `change`, a change of one of this stream's input values, while no other is made.
    pub(super) fn with_no_other_input_value_change<T>(&self, change: impl FnOnce() -> T) -> T {
        let _one_change_at_a_time = self.one_input_value_change_at_a_time.lock();
        change()
    }
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
/// announce, and the function nodes set on that run inside an island, whose islands are handed
/// again under the graph's write lock.
pub(crate) struct InputValuesHeldLive {
    pub(crate) made: Vec<InputValueSetLive>,
    pub(crate) changes_announced: Vec<RuntimeEvent>,
    pub(crate) function_nodes_read_inside_an_island: Vec<NodeRunId>,
}

/// Set each of `sets` live in `graph`'s live values and hand the helper of each node set on its
/// new pin values; `None`, with nothing set, when one is held on the graph. A value its input
/// does not take, and a value on a `reset` input while `playback` is stopped, is refused before
/// any is set, and a helper not handed its values leaves every live value as it was.
pub(crate) fn set_the_input_values_live_and_hand_each_helper_its_own(
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
        refuse_a_value_on_a_reset_input_while_stopped(sets, &set_on_each)?;
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

    let nodes_set_on = each_once(&set_on_each);
    for set_on in &nodes_set_on {
        if let Err(not_handed_over) =
            hand_the_helper_of_the_node_the_pin_values_its_input_ports_read(graph, set_on)
        {
            put_back_the_live_set(graph, sets, &made);
            return Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over));
        }
    }

    let changes_announced = sets
        .iter()
        .zip(&set_on_each)
        .filter_map(|(set, set_on)| the_input_value_change(graph, set_on, &set.input))
        .collect();
    let function_nodes_read_inside_an_island = nodes_set_on
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

/// Hand each reader of an island evaluating a function node `held_live` set on that island
/// again; when one is not handed it, put every value of `sets` back as it was and hand each
/// helper and island back what it held.
pub(crate) fn hand_each_island_the_live_set_or_put_it_back(
    graph: &mut Graph,
    sets: &[InputValueToSetLive],
    held_live: &InputValuesHeldLive,
) -> std::result::Result<(), LiveInputValueSetRefusal> {
    let Err(not_handed_over) =
        hand_each_reader_an_island_evaluating_the_nodes_again_when_it_changed(
            graph,
            &held_live.function_nodes_read_inside_an_island,
        )
    else {
        return Ok(());
    };
    put_back_the_live_set(graph, sets, &held_live.made);
    if let Err(not_handed_back) =
        hand_each_reader_an_island_evaluating_the_nodes_again_when_it_changed(
            graph,
            &held_live.function_nodes_read_inside_an_island,
        )
    {
        tracing::error!(
            %not_handed_back,
            "an island was not handed back the values a refused live set replaced"
        );
    }
    Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over))
}

/// Whether a value set on input `input` of `set_on` is held on `graph` rather than live: a
/// setting its type holds in its config, a config field it exposes as an input, an input it
/// does not have, or any input of a shader node, whose evaluator is built from the values its
/// graph holds.
pub(super) fn is_held_on_the_graph(graph: &Graph, set_on: &GraphNode, input: &str) -> bool {
    LegacyConfigOfANodeType::of(graph.node_types_this_stream_resolves(), &set_on.node_type)
        .is_a_config_field(input)
        || set_on.is_an_exposed_config_field_input(input)
        || !set_on.has_input(input)
        || matches!(set_on.node_kind(), Some(NodeKind::Shader))
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
    Err(LiveInputValueSetRefusal::TheInputIsResetWhileStopped {
        set_index,
        refusal: Error::NodeOfAStreamIsNotRunning {
            not_running_because: WhyANodeOfAStreamIsNotRunning::PlaybackStopped,
            runtime_words: format!(
                "input `{}` of `{}` returns to its saved value at Stop, so it keeps no value set \
                 live while the stream is stopped; `start_playback` plays it",
                set.input, set_on.display_name
            ),
        },
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
/// node the graph no longer holds, then hand the helper of each node set on the pin values it
/// reads now, logging one not handed them.
fn put_back_the_live_set(graph: &Graph, sets: &[InputValueToSetLive], made: &[InputValueSetLive]) {
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
    for set_on in each_once(&nodes_set_on) {
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
