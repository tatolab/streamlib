// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A setting its node reads at setup is set live as any other input is.

use parking_lot::RwLock;
use serde_json::json;

use super::tests::{a_level_meter_past_its_setup, a_set, the_uuid_of, the_values_set_live};
use crate::core::compiler::compiler_ops::input_port_pin_values::tests::a_graph_resolving_a_process_node_type_with_channels_inputs;

/// A setting its node reads at setup, set while the node runs, is set live and handed to the
/// node as any input is.
///
/// Mental-revert: holding a setting read at setup on the graph, for a rebuild that no longer
/// runs, leaves it unset live and hands the node nothing.
#[test]
fn a_setting_read_at_setup_set_while_its_node_runs_is_set_live_and_handed_to_it() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, delivery) = a_level_meter_past_its_setup(&mut graph);
    let gain = graph
        .traversal_mut()
        .v(&level_meter)
        .first_mut()
        .unwrap()
        .ports
        .inputs
        .iter_mut()
        .find(|port| port.name == "gain")
        .expect("the level meter has an input `gain`");
    gain.input_mode = Some(crate::core::descriptors::NodeInputMode::SettingOnly {
        read: crate::core::descriptors::SettingReadTime::AtSetup,
    });
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);

    let held_live = the_values_set_live(&graph, &[a_set(node, "gain", Some(json!(2.5)))])
        .unwrap()
        .expect("held live");

    assert_eq!(
        graph.read().live_input_values().value_of(node, "gain"),
        Some(json!(2.5))
    );
    assert_eq!(
        *delivery.input_values_handed_over.lock(),
        vec![
            json!({ "level": 0.5, "gain": 2.5 })
                .as_object()
                .unwrap()
                .clone()
        ]
    );
    assert_eq!(held_live.made.len(), 1);
}
