// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::super::live_graph_edits::set_the_saved_value_and_hand_it_over;
use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_graph_resolving_three_function_node_types, a_helper_reader_before_its_setup, add_a_node, link,
};
use crate::core::compiler::compiler_ops::function_node_island::wire_a_link_from_a_function_node_island_into_its_reader;
use crate::core::compiler::compiler_ops::input_port_pin_values::tests::{
    a_graph_resolving_a_process_node_type_with_channels_inputs, a_level_meter_in_a_helper,
    the_link_wiring_of,
};
use crate::core::graph::input_values::{InputValueComesFrom, InputValueRead};
use crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery;
use crate::iceoryx2::Iceoryx2Node;

fn the_uuid_of(graph: &Graph, node_run_id: &NodeRunId) -> Uuid {
    graph.traversal().v(node_run_id).first().unwrap().node_uuid
}

fn a_set(node: Uuid, input: &str, value: Option<Value>) -> InputValueToSetLive {
    InputValueToSetLive {
        node,
        input: input.to_owned(),
        value,
    }
}

/// A level meter in a helper past its setup command, handing what it is handed to the delivery
/// answered.
fn a_level_meter_past_its_setup(
    graph: &mut Graph,
) -> (NodeRunId, RecordingOutOfProcessFarSideLinkDelivery) {
    let level_meter = a_level_meter_in_a_helper(graph);
    let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
    the_link_wiring_of(graph, &level_meter)
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();
    (level_meter, delivery)
}

fn the_values_set_live(
    graph: &Graph,
    sets: &[InputValueToSetLive],
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    set_the_input_values_live_and_hand_each_helper_its_own(graph, sets)
}

/// Mental-revert: writing the live values and returning the helper's refusal with `?` leaves
/// the refused value live, unannounced and out of what the set made, so nothing reverses it.
#[test]
fn a_helper_refusing_its_values_leaves_every_live_value_as_it_was_and_announces_nothing() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    the_values_set_live(&graph, &[a_set(node, "level", Some(json!(0.25)))])
        .unwrap()
        .expect("held live");

    delivery.refuses_pin_values.store(true, Ordering::SeqCst);
    let refused = the_values_set_live(
        &graph,
        &[
            a_set(node, "level", Some(json!(0.75))),
            a_set(node, "gain", Some(json!(2.0))),
        ],
    );

    assert!(
        matches!(refused, Err(LiveInputValueSetRefusal::NotHandedOver(_))),
        "{:?}",
        refused.err()
    );
    let live_values = graph.live_input_values();
    assert_eq!(live_values.value_of(node, "level"), Some(json!(0.25)));
    assert_eq!(live_values.value_of(node, "gain"), None);
    delivery.refuses_pin_values.store(false, Ordering::SeqCst);
    hand_each_helper_reading_the_nodes_its_own(&graph, &[a_set(node, "level", None)]);
    assert_eq!(
        delivery.pin_values_handed_over.lock().last(),
        json!({ "level": 0.25 }).as_object(),
        "the helper is handed back the value it held"
    );
}

/// Mental-revert: handing every helper of the graph its pin values hands the other meter its
/// values on each set, and fails this set on the other meter's dead helper.
#[test]
fn only_the_helper_of_the_node_set_on_is_handed_its_live_values_over_the_saved_ones() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, delivery) = a_level_meter_past_its_setup(&mut graph);
    let (_other_meter, other_delivery) = a_level_meter_past_its_setup(&mut graph);
    other_delivery
        .refuses_pin_values
        .store(true, Ordering::SeqCst);
    graph
        .traversal_mut()
        .v(&level_meter)
        .first_mut()
        .unwrap()
        .pins
        .insert("gain".to_owned(), json!(2.0));
    let node = the_uuid_of(&graph, &level_meter);

    let held_live = the_values_set_live(&graph, &[a_set(node, "level", Some(json!(0.75)))])
        .unwrap()
        .expect("held live");

    assert_eq!(
        *delivery.pin_values_handed_over.lock(),
        vec![
            json!({ "gain": 2.0, "level": 0.75 })
                .as_object()
                .unwrap()
                .clone()
        ]
    );
    assert!(other_delivery.pin_values_handed_over.lock().is_empty());
    assert_eq!(
        held_live.changes_announced,
        vec![RuntimeEvent::InputValueDidChange {
            node,
            input: "level".to_owned(),
            value_read: Some(InputValueRead {
                value: Some(json!(0.75)),
                value_comes_from: Some(InputValueComesFrom::Live),
            }),
        }]
    );
    assert_eq!(held_live.made[0].saved_value, None);
    assert!(held_live.function_nodes_read_inside_an_island.is_empty());
}

#[test]
fn a_set_naming_an_input_or_node_the_graph_does_not_hold_is_held_on_the_graph_and_sets_nothing() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    for held_on_the_graph in [
        a_set(node, "not_an_input", Some(json!(1.0))),
        a_set(Uuid::new_v4(), "level", Some(json!(1.0))),
    ] {
        let held = the_values_set_live(
            &graph,
            &[a_set(node, "level", Some(json!(0.5))), held_on_the_graph],
        )
        .unwrap();
        assert!(held.is_none());
    }
    assert_eq!(graph.live_input_values().value_of(node, "level"), None);
    assert!(delivery.pin_values_handed_over.lock().is_empty());
}

#[test]
fn a_value_its_input_does_not_take_is_refused_by_its_index_before_any_is_set() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let refused = the_values_set_live(
        &graph,
        &[
            a_set(node, "level", Some(json!(0.5))),
            a_set(node, "gain", Some(json!("loud"))),
        ],
    );
    assert!(
        matches!(
            refused,
            Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { set_index: 1, .. })
        ),
        "{:?}",
        refused.err()
    );
    assert_eq!(graph.live_input_values().value_of(node, "level"), None);
}

#[test]
fn a_node_dropped_from_the_graph_forgets_its_live_values() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    the_values_set_live(&graph, &[a_set(node, "level", Some(json!(0.5)))]).unwrap();
    assert!(!graph.drop_the_node(&level_meter));
    assert_eq!(graph.live_input_values().value_of(node, "level"), None);
}

/// `graph` renders each input's value and where it comes from, live over saved over default,
/// `wire` for a linked input; `pins` stay the saved values; and a function node does not wait on
/// an input set live.
#[test]
fn the_graph_renders_the_value_each_input_reads_and_keeps_its_pins_saved() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", Value::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", Value::Null);
    let mix = add_a_node(&mut graph, "test_ops:mix", Value::Null);
    link(&mut graph, (&lfo, "result"), (&remap, "x"));
    set_the_saved_value_and_hand_it_over(&mut graph, &mix, "b", Some(json!(0.1))).unwrap();
    let mix_uuid = the_uuid_of(&graph, &mix);
    let render = |graph: &Graph, node_type: &str| -> Value {
        serde_json::to_value(graph.to_graph_response("this-machine".to_owned())).unwrap()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["type"] == node_type)
            .cloned()
            .unwrap()
    };
    let input_of = |rendered: &Value, input: &str| -> Value {
        rendered["ports"]["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|port| port["name"] == input)
            .cloned()
            .unwrap()
    };

    let rendered_mix = render(&graph, "test_ops:mix");
    assert_eq!(rendered_mix["inputs_waiting_for_a_value"], json!(["a"]));
    assert_eq!(input_of(&rendered_mix, "a")["value"], Value::Null);
    assert!(
        input_of(&rendered_mix, "a")
            .get("value_comes_from")
            .is_none()
    );
    assert_eq!(input_of(&rendered_mix, "b")["value_comes_from"], "saved");
    let rendered_x = input_of(&render(&graph, "test_ops:remap"), "x");
    assert_eq!(rendered_x["value_comes_from"], "wire");
    assert!(rendered_x.get("value").is_none(), "{rendered_x}");

    the_values_set_live(
        &graph,
        &[
            a_set(mix_uuid, "a", Some(json!(0.3))),
            a_set(mix_uuid, "b", Some(json!(0.6))),
        ],
    )
    .unwrap()
    .expect("held live");

    let rendered_mix = render(&graph, "test_ops:mix");
    assert!(
        rendered_mix["inputs_waiting_for_a_value"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{rendered_mix}"
    );
    for (input, value) in [("a", 0.3), ("b", 0.6)] {
        let rendered_input = input_of(&rendered_mix, input);
        assert_eq!(rendered_input["value"], json!(value));
        assert_eq!(rendered_input["value_comes_from"], "live");
    }
    assert_eq!(rendered_mix["pins"], json!({ "b": 0.1 }), "pins stay saved");
}

#[test]
fn an_input_reading_its_declared_default_renders_it_as_the_default() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = graph.traversal().v(&level_meter).first().unwrap();
    assert_eq!(
        crate::core::graph::input_values::the_input_value_change(&graph, node, "level"),
        Some(RuntimeEvent::InputValueDidChange {
            node: node.node_uuid,
            input: "level".to_owned(),
            value_read: Some(InputValueRead {
                value: Some(json!(0.5)),
                value_comes_from: Some(InputValueComesFrom::Default),
            }),
        })
    );
}

/// A function node read inside an island hands its reader the island carrying the live value,
/// and a commit of the input then hands it the island carrying the saved one.
///
/// Mental-revert: dropping the live value after the islands are handed again leaves the reader
/// evaluating the live value while `graph` renders the saved one.
#[test]
fn a_function_node_read_inside_an_island_hands_its_reader_the_live_value_then_the_committed_one() {
    let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut graph = a_graph_resolving_three_function_node_types();
    let remap = add_a_node(&mut graph, "test_ops:remap", Value::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    the_link_wiring_of(&graph, &reader)
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();
    let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    let remap_uuid = the_uuid_of(&graph, &remap);
    let the_pins_last_handed = || {
        delivery
            .late_fed_function_node_islands
            .lock()
            .last()
            .map(|island| island["nodes"][0]["pins"].clone())
    };

    let held_live = the_values_set_live(&graph, &[a_set(remap_uuid, "x", Some(json!(0.5)))])
        .unwrap()
        .expect("held live");
    assert_eq!(
        held_live.function_nodes_read_inside_an_island,
        vec![remap.clone()]
    );
    hand_each_reader_an_island_evaluating_the_nodes_again(
        &mut graph,
        &held_live.function_nodes_read_inside_an_island,
    )
    .unwrap();
    assert_eq!(the_pins_last_handed(), Some(json!({ "x": 0.5 })));

    let committed =
        set_the_saved_value_and_hand_it_over(&mut graph, &remap, "x", Some(json!(0.25))).unwrap();
    assert_eq!(the_pins_last_handed(), Some(json!({ "x": 0.25 })));
    assert_eq!(graph.live_input_values().value_of(remap_uuid, "x"), None);
    assert!(
        matches!(
            committed,
            Some(RuntimeEvent::InputValueDidChange {
                value_read: Some(InputValueRead {
                    value_comes_from: Some(InputValueComesFrom::Saved),
                    ..
                }),
                ..
            })
        ),
        "{committed:?}"
    );
}
