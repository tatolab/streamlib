// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value set live on an input a node reads each run: checked and held in the stream's live
//! values under the graph's read lock, handed to the helpers and islands that read it, and
//! announced as `InputValueDidChange`, so no node, link or port changes and nothing compiles.

use serde_json::Value;
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::compiler::compiler_ops::function_node_island::{
    hand_each_reader_the_island_a_compile_changed, runs_inside_the_read_of_each_node_it_feeds,
};
use crate::core::compiler::compiler_ops::input_port_pin_values::hand_each_helper_the_pin_values_its_input_ports_read;
use crate::core::graph::input_values::{
    InputValueRefusal, InputValueSetLive, LiveInputValueHolding, NodeAnInputValueIsSetOn,
    refuse_an_input_value_its_input_does_not_take, the_value_an_input_reads,
};
use crate::core::graph::legacy_config_of_a_node::{
    LegacyConfigOfANodeType, input_values_of_a_config_and_pin_values,
};
use crate::core::graph::{Graph, GraphNode};
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, Result};

impl LoadedStreamInThisRuntime {
    /// How `value`, set live on input `input` of the node `node`, is held, refused when the input
    /// does not take it: apart from the graph for an input the node reads each run, on the graph
    /// for a setting, a config field the node exposes as an input, an input it does not have, or
    /// a node the running graph does not hold.
    pub(crate) fn how_a_live_input_value_is_held(
        &self,
        node: Uuid,
        input: &str,
        value: Option<&Value>,
    ) -> std::result::Result<LiveInputValueHolding, InputValueRefusal> {
        self.compiler.scope_reading(|graph| {
            let Some(set_on) = the_node_by_uuid(graph, node) else {
                return Ok(LiveInputValueHolding::OnTheGraph);
            };
            let is_a_setting = LegacyConfigOfANodeType::of(
                &self.node_types_this_stream_resolves,
                &set_on.node_type,
            )
            .is_a_config_field(input);
            if is_a_setting
                || set_on.is_an_exposed_config_field_input(input)
                || !set_on.has_input(input)
            {
                return Ok(LiveInputValueHolding::OnTheGraph);
            }
            refuse_an_input_value_its_input_does_not_take(
                &self.node_types_this_stream_resolves,
                &NodeAnInputValueIsSetOn {
                    name: &set_on.display_name,
                    node_type: &set_on.node_type,
                    input_values_after_the_set: &input_values_of_a_config_and_pin_values(
                        set_on.config.as_ref(),
                        &set_on.pins,
                    ),
                },
                input,
                value,
            )?;
            Ok(LiveInputValueHolding::ApartFromTheGraph)
        })
    }

    /// Set each of `sets`, each checked and held apart from the graph, live on the node `node`,
    /// `None` taking an input's live value off; hand each helper and island reading one its new
    /// value, and announce each input's new value.
    pub(crate) fn set_input_values_live(
        &self,
        node: Uuid,
        sets: Vec<(String, Option<Value>)>,
    ) -> Result<Vec<InputValueSetLive>> {
        let (made, changes_announced, read_inside_an_island) =
            self.compiler.scope_reading(|graph| -> Result<_> {
                let set_on = the_node_by_uuid(graph, node).ok_or_else(|| {
                    Error::GraphError(format!(
                        "stream `{}` holds no node {node}",
                        self.stream_id()
                    ))
                })?;
                let mut made = Vec::with_capacity(sets.len());
                let mut changes_announced = Vec::with_capacity(sets.len());
                for (input, value) in sets {
                    let port = set_on
                        .ports
                        .inputs
                        .iter()
                        .find(|port| port.name == input)
                        .ok_or_else(|| Error::NodePortNotFound {
                            node_run_id: set_on.display_name.clone(),
                            port_name: input.clone(),
                            direction: crate::core::PortDirection::Input,
                        })?;
                    let live_value_before = graph.live_input_values().set(node, &input, value);
                    let linked_from = the_output_feeding_the_input(graph, set_on, &input);
                    changes_announced.push(RuntimeEvent::InputValueDidChange {
                        node,
                        input: input.clone(),
                        member: None,
                        value_read: the_value_an_input_reads(
                            graph,
                            set_on,
                            port,
                            linked_from.is_some(),
                        ),
                    });
                    made.push(InputValueSetLive {
                        saved_value: set_on.pins.get(&input).cloned(),
                        input,
                        live_value_before,
                        linked_from,
                    });
                }
                hand_each_helper_the_pin_values_its_input_ports_read(graph)?;
                Ok((
                    made,
                    changes_announced,
                    runs_inside_the_read_of_each_node_it_feeds(graph, &set_on.id),
                ))
            })?;
        if read_inside_an_island {
            self.compiler
                .scope(|graph, _tx| hand_each_reader_the_island_a_compile_changed(graph, &[]))?;
        }
        for change in changes_announced {
            self.publish_on_this_streams_topic(change);
        }
        Ok(made)
    }
}

/// The node of `graph` whose UUID is `node`.
fn the_node_by_uuid(graph: &Graph, node: Uuid) -> Option<&GraphNode> {
    graph
        .traversal()
        .v(())
        .iter()
        .find(|held| held.node_uuid == node)
}

/// The output that a link of `graph` feeds input `input` of `node` from, as `(node UUID, port)`.
fn the_output_feeding_the_input(
    graph: &Graph,
    node: &GraphNode,
    input: &str,
) -> Option<(Uuid, String)> {
    graph.traversal().e(()).iter().find_map(|link| {
        (link.to_port().node_run_id() == &node.id && link.to_port().port_name() == input)
            .then(|| {
                let source = link.source();
                graph
                    .traversal()
                    .v(source.node_run_id())
                    .first()
                    .map(|feeding| (feeding.node_uuid, source.port_name().to_owned()))
            })
            .flatten()
    })
}
