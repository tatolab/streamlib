// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A live value follows its input's lifetime: a `kept` input's value is read back from the
//! project's store as the stream loads and held across Stop, and a `reset` input's value is
//! forgotten at Stop, its followers told the input reads its saved value again.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use super::input_values_set_live::is_held_on_the_graph;
use crate::core::graph::input_values::{
    NodeAnInputValueIsSetOn, refuse_an_input_value_its_input_does_not_take, the_input_value_change,
    the_lifetime_of_the_input,
};
use crate::core::graph::legacy_config_of_a_node::input_values_of_a_config_and_pin_values;
use crate::core::graph::{Graph, GraphNode};

impl LoadedStreamInThisRuntime {
    /// Hold as live each value the project's store keeps for an input of a node this stream's
    /// graph holds, as the stream loads; a value its input no longer keeps live or takes is
    /// forgotten there, and the input reads its saved value.
    ///
    /// An input holding values under names it had keeps the one it reads, by the one order
    /// [`crate::core::graph::GraphNodePorts::inputs_holding_a_value_under_a_name_they_had`]
    /// reads them in, under its current name; each name it had is forgotten once that value is
    /// written, and kept when the write fails, so a later load moves it again.
    pub(super) fn hold_the_live_values_the_project_store_keeps(&self) {
        self.compiler.scope_reading(|graph| {
            let live_values = graph.live_input_values();
            let Some(project_store) = live_values.project_store_mirrored_into() else {
                return;
            };
            let mut kept_for_each_node: BTreeMap<Uuid, serde_json::Map<String, serde_json::Value>> =
                BTreeMap::new();
            for (node, input, value) in
                project_store.every_value_kept_for(|node| graph.node_by_uuid(node).is_some())
            {
                kept_for_each_node
                    .entry(node)
                    .or_default()
                    .insert(input, value);
            }
            for (node, mut kept) in kept_for_each_node {
                let Some(kept_for) = graph.node_by_uuid(node) else {
                    continue;
                };
                for moved in kept_for
                    .ports
                    .inputs_holding_a_value_under_a_name_they_had(|name| kept.contains_key(name))
                {
                    let current = &moved.current_name;
                    let read = moved.names_in_the_order_read[0].clone();
                    let names_it_had = moved
                        .names_in_the_order_read
                        .iter()
                        .filter(|name| *name != current);
                    let held_under_the_current_name = read == *current
                        || kept
                            .get(&read)
                            .is_some_and(|value| project_store.keep(node, current, value));
                    for name_it_had in names_it_had {
                        if held_under_the_current_name {
                            project_store.forget(node, name_it_had);
                        }
                        let value = kept.remove(name_it_had);
                        if *name_it_had == read
                            && held_under_the_current_name
                            && let Some(value) = value
                        {
                            kept.insert(current.clone(), value);
                        }
                    }
                }
                for (input, value) in kept {
                    if its_input_keeps_and_takes_the_live_value(graph, kept_for, &input, &value) {
                        live_values.hold_the_kept_value_the_store_holds(node, &input, value);
                    } else {
                        project_store.forget(node, &input);
                    }
                }
            }
        });
    }

    /// Forget each value set live on a `reset` input of this stream, as it stops, answering
    /// each input forgotten as `(node, input)`.
    pub(super) fn forget_every_live_value_reset_at_the_stop(&self) -> Vec<(Uuid, String)> {
        self.compiler
            .scope_reading(|graph| graph.live_input_values().forget_every_reset_value())
    }

    /// Announce what each input of `inputs_reset`, by `(node, input)`, reads now that its live
    /// value is forgotten.
    pub(super) fn announce_each_input_reset_at_the_stop(&self, inputs_reset: &[(Uuid, String)]) {
        let changes = self.compiler.scope_reading(|graph| {
            inputs_reset
                .iter()
                .filter_map(|(node, input)| {
                    the_input_value_change(graph, graph.node_by_uuid(*node)?, input)
                })
                .collect::<Vec<_>>()
        });
        for change in changes {
            self.publish_on_this_streams_topic(change);
        }
    }
}

/// Whether input `input` of `kept_for` keeps a live value across Stop, takes one apart from the
/// graph and takes `value`, so a value the project's store keeps for it is its live value.
fn its_input_keeps_and_takes_the_live_value(
    graph: &Graph,
    kept_for: &GraphNode,
    input: &str,
    value: &serde_json::Value,
) -> bool {
    kept_for.has_input(input)
        && the_lifetime_of_the_input(kept_for, input).is_kept()
        && !is_held_on_the_graph(graph, kept_for, input)
        && refuse_an_input_value_its_input_does_not_take(
            graph.node_types_this_stream_resolves(),
            &NodeAnInputValueIsSetOn {
                name: &kept_for.display_name,
                node_type: &kept_for.node_type,
                input_values_after_the_set: &input_values_of_a_config_and_pin_values(
                    kept_for.config.as_ref(),
                    &kept_for.pins,
                ),
            },
            input,
            Some(value),
        )
        .is_ok()
}
