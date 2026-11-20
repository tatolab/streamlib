// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Values set live on inputs their nodes read each run: checked and held in the stream's live
//! values under the graph's read lock, handed to each node set on alone, wherever it is hosted,
//! with the lock released, and announced as `InputValueDidChange`, so no node, link or port
//! changes and nothing compiles. A set is whole or not at all: one a node or island is not handed
//! leaves every value as it was. A set of no value on an unlinked Trigger input fires it instead,
//! as `trigger_fires_set_live` says.

use std::collections::HashMap;

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
use crate::core::error::WhyANodeOfAStreamIsNotRunning;
use crate::core::graph::input_values::{
    InputValueRefusal, InputValueRefusalCode, InputValueSetLive, InputValueToSetLive,
    InputValuesSetLive, LiveInputValueSetRefusal, NodeAnInputValueIsSetOn,
    refuse_an_input_value_its_input_does_not_take, the_control_value_of_a_pin,
    the_input_value_change, the_input_values_a_live_set_is_checked_against,
    the_lifetime_of_the_input, the_words_of_an_input_set_twice_live,
};
use crate::core::graph::legacy_config_of_a_node::LegacyConfigOfANodeType;
use crate::core::graph::{Graph, GraphNode, NodeRunId};
use crate::core::nodes::{NodeTypesOneStreamResolves, is_the_input_of_an_output_a_panel_writes};
use crate::core::pubsub::RuntimeEvent;

mod trigger_fires_set_live;

use trigger_fires_set_live::{
    TriggerFiresDelivery, is_a_fire, refuse_a_fire_no_running_node_reads,
};

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
            let playback = match self.why_a_node_of_this_stream_that_has_not_failed_does_not_run() {
                WhyANodeOfAStreamIsNotRunning::PlaybackStopped => PlaybackOfTheStreamSetOn::Stopped,
                why_a_node_not_built_does_not_run => PlaybackOfTheStreamSetOn::NotStopped {
                    why_a_node_not_built_does_not_run,
                },
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
/// holds no live value and no node runs to read a fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackOfTheStreamSetOn {
    /// Stopped, its graph held to read and edit: a live set on a `kept` input is held for the
    /// next Play, and one on a `reset` input refused, as is a fire.
    Stopped,
    /// Playing, paused or starting; a node not built yet does not run for the reason given.
    NotStopped {
        why_a_node_not_built_does_not_run: WhyANodeOfAStreamIsNotRunning,
    },
}

/// What a live set held apart from the graph made: each value set, the change of each input to
/// announce, how each node set on outside an island is handed its values, the function nodes
/// set on that run inside an island, whose islands are handed again under the graph's write lock,
/// and how each node fired takes its fires.
pub(crate) struct InputValuesHeldLive {
    pub(crate) made: Vec<InputValueSetLive>,
    pub(crate) changes_announced: Vec<RuntimeEvent>,
    deliveries: Vec<UnlinkedInputValuesDelivery>,
    function_nodes_read_inside_an_island: Vec<NodeRunId>,
    fires: Vec<TriggerFiresDelivery>,
}

/// Set each of `sets` live, under its input's current name, in the live values of the graph
/// `graph` locks, then hand each node set
/// on its values, its helper or itself with the lock released and its islands under the write
/// lock; `None`, with nothing set, when one is held on the graph. A value its input does not
/// take, and a value on a `reset` input while `playback` is stopped, is refused before any is
/// set, and a node or island not handed its values leaves every live value as it was. Each fire
/// is queued once every value is handed over; a fire not queued puts the values back, though a
/// fire queued before it stays queued.
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
    if !held_live.function_nodes_read_inside_an_island.is_empty() {
        let handed_to_each_island = hand_each_island_again_or_put_the_values_back(
            &mut graph.write(),
            &held_live.function_nodes_read_inside_an_island,
            |graph| put_back_the_live_set(graph, sets, &held_live.made),
        );
        if let Err((not_handed_over, handed_back)) = handed_to_each_island {
            hand_back_each(handed_back);
            return Err(LiveInputValueSetRefusal::NotHandedOver(not_handed_over));
        }
    }
    let fired = std::mem::take(&mut held_live.fires)
        .into_iter()
        .try_for_each(TriggerFiresDelivery::hand_over);
    if let Err(not_fired) = fired {
        let handed_back = put_back_the_live_set(&graph.read(), sets, &held_live.made);
        hand_back_each(handed_back);
        return Err(not_fired);
    }
    Ok(Some(held_live))
}

/// Refuse `sets` when two of them name one input of one node, at the second, whatever else
/// they set, unless each names another member of it: the second value would replace the first
/// before the node is handed either. A fire replaces nothing, so an input fired twice fires
/// twice.
fn refuse_an_input_set_twice(
    graph: &Graph,
    sets: &[InputValueToSetLive],
) -> std::result::Result<(), LiveInputValueSetRefusal> {
    let mut members_set_of_each_input: HashMap<(Uuid, &str), Vec<Option<&str>>> =
        HashMap::with_capacity(sets.len());
    for (set_index, set) in sets.iter().enumerate() {
        if is_a_fire(graph, set) {
            continue;
        }
        let member = set.member.as_deref();
        let members_set = members_set_of_each_input
            .entry((set.node, set.input.as_str()))
            .or_default();
        let set_twice = members_set
            .iter()
            .any(|member_set| member.is_none() || member_set.is_none() || *member_set == member);
        members_set.push(member);
        if set_twice {
            let node_name = graph
                .node_by_uuid(set.node)
                .map_or_else(|| set.node.to_string(), |node| node.display_name.clone());
            return Err(LiveInputValueSetRefusal::TheInputIsSetTwice {
                set_index,
                runtime_words: the_words_of_an_input_set_twice_live(&set.input, &node_name),
            });
        }
    }
    Ok(())
}

/// Set each of `sets` live in `graph`'s live values and answer how each node set on takes its
/// values and each node fired its fires; `None`, with nothing set, when one is held on the graph.
/// A value its input does not take, a second value on one input of one node, a value on a
/// `reset` input while `playback` is stopped, and a fire no running node reads, is refused
/// before any is set.
fn set_the_input_values_live(
    graph: &Graph,
    sets: &[InputValueToSetLive],
    playback: PlaybackOfTheStreamSetOn,
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    refuse_an_input_set_twice(graph, sets)?;
    let why_a_node_not_built_does_not_run = match playback {
        PlaybackOfTheStreamSetOn::Stopped => WhyANodeOfAStreamIsNotRunning::PlaybackStopped,
        PlaybackOfTheStreamSetOn::NotStopped {
            why_a_node_not_built_does_not_run,
        } => why_a_node_not_built_does_not_run,
    };
    let mut value_sets: Vec<(usize, &InputValueToSetLive)> = Vec::with_capacity(sets.len());
    let mut set_on_each: Vec<&GraphNode> = Vec::with_capacity(sets.len());
    let mut fires: Vec<(usize, &InputValueToSetLive)> = Vec::new();
    for (set_index, set) in sets.iter().enumerate() {
        let Some(set_on) = graph.node_by_uuid(set.node) else {
            return Ok(None);
        };
        if is_a_fire(graph, set) {
            refuse_a_fire_no_running_node_reads(
                graph,
                set_on,
                set_index,
                set,
                why_a_node_not_built_does_not_run,
            )?;
            fires.push((set_index, set));
            continue;
        }
        if is_held_on_the_graph(graph, set_on, &set.input) {
            return Ok(None);
        }
        let live_after_the_set = streamlib_project_repository::the_input_value_after_the_set(
            graph.live_input_values().value_of(set.node, &set.input),
            set.member.as_deref(),
            set.value.as_ref(),
        );
        refuse_an_input_value_its_input_does_not_take(
            graph.node_types_this_stream_resolves(),
            &NodeAnInputValueIsSetOn {
                name: &set_on.display_name,
                node_type: &set_on.node_type,
                input_values_after_the_set: &the_input_values_a_live_set_is_checked_against(
                    set_on,
                    &set.input,
                    live_after_the_set.as_ref(),
                ),
            },
            &set.input,
            set.member.as_deref(),
            set.value.as_ref(),
        )
        .and_then(|()| {
            refuse_a_value_its_own_helper_does_not_read(
                graph,
                set_on,
                &set.input,
                live_after_the_set.as_ref(),
            )
        })
        .map_err(|refusal| LiveInputValueSetRefusal::TheInputDoesNotTakeIt {
            set_index,
            refusal,
        })?;
        value_sets.push((set_index, set));
        set_on_each.push(set_on);
    }
    if playback == PlaybackOfTheStreamSetOn::Stopped {
        refuse_a_value_on_a_reset_input_while_stopped(
            graph.node_types_this_stream_resolves(),
            &value_sets,
            &set_on_each,
        )?;
    }

    let mut value_sets_made = value_sets
        .iter()
        .zip(&set_on_each)
        .map(|((_, set), set_on)| InputValueSetLive {
            input: set.input.clone(),
            live_value_before: graph.live_input_values().set_on_the_input_or_its_member(
                set.node,
                &set.input,
                set.member.as_deref(),
                set.value.clone(),
                the_lifetime_of_the_input(set_on, &set.input),
            ),
            saved_value: set_on.pins.get(&set.input).cloned(),
            linked_from: the_output_feeding_the_input(graph, set_on, &set.input),
        });
    let made: Vec<InputValueSetLive> = sets
        .iter()
        .map(|set| {
            if is_a_fire(graph, set) {
                InputValueSetLive {
                    input: set.input.clone(),
                    live_value_before: None,
                    saved_value: None,
                    linked_from: None,
                }
            } else {
                value_sets_made
                    .next()
                    .expect("one value made for each set that is no fire")
            }
        })
        .collect();
    let changes_announced = value_sets
        .iter()
        .zip(&set_on_each)
        .flat_map(|((_, set), set_on)| {
            the_input_value_change(graph, set_on, &set.input, set.member.as_deref())
        })
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
                    value_sets
                        .iter()
                        .filter(|(_, set)| set.node == set_on.node_uuid)
                        .map(|(_, set)| set.input.clone())
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
        fires: TriggerFiresDelivery::of_each_node_fired(graph, &fires),
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

/// Refuse the first of `sets`, each with its index among those asked and set on the node
/// `set_on_each` names at its place, that sets a value on a `reset` input: the stream's playback
/// is stopped, and a `reset` input holds no live value until it plays. Taking a live value off is
/// no refusal, there being none to take.
fn refuse_a_value_on_a_reset_input_while_stopped(
    node_types: &NodeTypesOneStreamResolves,
    sets: &[(usize, &InputValueToSetLive)],
    set_on_each: &[&GraphNode],
) -> std::result::Result<(), LiveInputValueSetRefusal> {
    let refused = sets.iter().zip(set_on_each).find(|((_, set), set_on)| {
        set.value.is_some()
            && the_lifetime_of_the_input(set_on, &set.input) == NodeValueLifetime::Reset
    });
    let Some(((set_index, set), set_on)) = refused else {
        return Ok(());
    };
    let what_it_returns_to =
        if is_the_input_of_an_output_a_panel_writes(node_types, &set_on.node_type, &set.input) {
            "no value"
        } else {
            "its saved value"
        };
    Err(LiveInputValueSetRefusal::TheInputIsResetWhileStopped {
        set_index: *set_index,
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
/// fire, which replaced none, and a node the graph no longer holds; answers how each node set on outside an island is handed back
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
        if is_a_fire(graph, set) {
            continue;
        }
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
mod bundle_member_sets_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod trigger_fires_set_live_tests;
