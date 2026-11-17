// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A live value follows its input's lifetime: a `kept` input's value is read back from the
//! project's store as the stream loads and held across Stop, and a `reset` input's value is
//! forgotten at Stop, its followers told the input reads its saved value again.

use std::collections::HashSet;

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
    /// graph holds, as the stream loads; a value kept under a name the input had moves to the
    /// input's name there, unless one is kept under that name already, and a value its input no
    /// longer keeps live or takes is forgotten there, and the input reads its saved value.
    pub(super) fn hold_the_live_values_the_project_store_keeps(&self) {
        self.compiler.scope_reading(|graph| {
            let live_values = graph.live_input_values();
            let Some(project_store) = live_values.project_store_mirrored_into() else {
                return;
            };
            let kept =
                project_store.every_value_kept_for(|node| graph.node_by_uuid(node).is_some());
            let kept_keys: HashSet<(Uuid, String)> = kept
                .iter()
                .map(|(node, kept_under, _)| (*node, kept_under.clone()))
                .collect();
            for (node, kept_under, value) in kept {
                let Some(kept_for) = graph.node_by_uuid(node) else {
                    continue;
                };
                let input = kept_for
                    .input_port(&kept_under)
                    .map_or_else(|| kept_under.clone(), |port| port.name.clone());
                let kept_under_a_former_name = input != kept_under;
                if kept_under_a_former_name {
                    project_store.forget(node, &kept_under);
                    if kept_keys.contains(&(node, input.clone())) {
                        continue;
                    }
                }
                if its_input_keeps_and_takes_the_live_value(graph, kept_for, &input, &value) {
                    if kept_under_a_former_name {
                        project_store.keep(node, &input, &value);
                    }
                    live_values.hold_the_kept_value_the_store_holds(node, &input, value);
                } else if !kept_under_a_former_name {
                    project_store.forget(node, &input);
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
