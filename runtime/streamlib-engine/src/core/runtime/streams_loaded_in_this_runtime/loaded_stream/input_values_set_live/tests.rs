// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use parking_lot::RwLock;
use serde_json::{Value, json};

use super::super::live_graph_edits::{
    SavedValuesOfAKeptNode, set_the_saved_value_and_hand_it_over,
    set_the_saved_values_of_each_kept_node_and_hand_them_over,
};
use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_graph_resolving_three_function_node_types, a_helper_reader_before_its_setup, add_a_node, link,
};
use crate::core::compiler::compiler_ops::function_node_island::{
    hand_each_reader_the_island_a_compile_changed,
    wire_a_link_from_a_function_node_island_into_its_reader,
};
use crate::core::compiler::compiler_ops::input_port_pin_values::tests::{
    a_graph_resolving_a_process_node_type_with_channels_inputs, a_level_meter_in_a_helper,
    the_link_wiring_of,
};
use crate::core::descriptors::NodeValueLifetime;
use crate::core::graph::WhyTheNodeLeaves;
use crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery;
use crate::iceoryx2::Iceoryx2Node;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    InputValueComesFromOnTheWire, InputValueReadOnTheWire,
};

fn the_uuid_of(graph: &Graph, node_run_id: &NodeRunId) -> Uuid {
    graph.traversal().v(node_run_id).first().unwrap().node_uuid
}

fn a_set(node: Uuid, input: &str, value: Option<Value>) -> InputValueToSetLive {
    InputValueToSetLive {
        member: None,
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
    graph: &RwLock<Graph>,
    sets: &[InputValueToSetLive],
) -> std::result::Result<Option<InputValuesHeldLive>, LiveInputValueSetRefusal> {
    set_the_input_values_live_and_hand_each_node_its_own(
        graph,
        sets,
        PlaybackOfTheStreamSetOn::NotStopped {
            why_a_node_not_built_does_not_run: WhyANodeOfAStreamIsNotRunning::Preparing,
        },
    )
}

/// A set on two helpers, the second refusing it, leaves every live value as it was: the first
/// helper, which took its value, is handed back the one it held, and the refusing one keeps its
/// own.
///
/// Mental-revert: writing the live values and returning the helper's refusal with `?` leaves
/// the refused values live, unannounced and out of what the set made, so nothing reverses them;
/// putting the values back without handing them back leaves the first helper on 0.75.
#[test]
fn a_helper_refusing_its_values_leaves_every_live_value_as_it_was_and_announces_nothing() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, delivery) = a_level_meter_past_its_setup(&mut graph);
    let (refusing_meter, refusing_delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let refusing_node = the_uuid_of(&graph, &refusing_meter);
    let graph = RwLock::new(graph);
    the_values_set_live(
        &graph,
        &[
            a_set(node, "level", Some(json!(0.25))),
            a_set(refusing_node, "level", Some(json!(0.25))),
        ],
    )
    .unwrap()
    .expect("held live");

    let handed_over_before_the_refusal = delivery.input_values_handed_over.lock().len();
    refusing_delivery
        .refuses_input_values_once
        .store(true, Ordering::SeqCst);
    let refused = the_values_set_live(
        &graph,
        &[
            a_set(node, "level", Some(json!(0.75))),
            a_set(node, "gain", Some(json!(2.0))),
            a_set(refusing_node, "level", Some(json!(0.75))),
        ],
    );

    assert!(
        matches!(refused, Err(LiveInputValueSetRefusal::NotHandedOver(_))),
        "{:?}",
        refused.err()
    );
    let graph = graph.read();
    let live_values = graph.live_input_values();
    assert_eq!(live_values.value_of(node, "level"), Some(json!(0.25)));
    assert_eq!(live_values.value_of(node, "gain"), None);
    assert_eq!(
        live_values.value_of(refusing_node, "level"),
        Some(json!(0.25))
    );
    let handed_over = delivery.input_values_handed_over.lock();
    assert_eq!(
        handed_over[handed_over_before_the_refusal..],
        [
            json!({ "level": 0.75, "gain": 2.0 })
                .as_object()
                .unwrap()
                .clone(),
            json!({ "level": 0.25 }).as_object().unwrap().clone()
        ],
        "the helper that took its values is handed back the one it held, after the refusal"
    );
    assert_eq!(
        refusing_delivery
            .input_values_handed_over
            .lock()
            .last()
            .unwrap()["level"],
        json!(0.25),
        "the refusing helper is handed nothing new"
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
        .refuses_input_values
        .store(true, Ordering::SeqCst);
    graph
        .traversal_mut()
        .v(&level_meter)
        .first_mut()
        .unwrap()
        .input_values
        .insert("gain".to_owned(), json!(2.0));
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);

    let held_live = the_values_set_live(&graph, &[a_set(node, "level", Some(json!(0.75)))])
        .unwrap()
        .expect("held live");

    assert_eq!(
        *delivery.input_values_handed_over.lock(),
        vec![
            json!({ "gain": 2.0, "level": 0.75 })
                .as_object()
                .unwrap()
                .clone()
        ]
    );
    assert!(other_delivery.input_values_handed_over.lock().is_empty());
    assert_eq!(
        held_live.changes_announced,
        vec![RuntimeEvent::InputValueDidChange {
            member: None,
            node,
            input: "level".to_owned(),
            value_read: Some(InputValueReadOnTheWire {
                value: Some(json!(0.75)),
                value_comes_from: Some(InputValueComesFromOnTheWire::Live),
                member_values: None,
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
    let graph = RwLock::new(graph);
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
    assert_eq!(
        graph.read().live_input_values().value_of(node, "level"),
        None
    );
    assert!(delivery.input_values_handed_over.lock().is_empty());
}

#[test]
fn a_value_its_input_does_not_take_is_refused_by_its_index_before_any_is_set() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);
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
    assert_eq!(
        graph.read().live_input_values().value_of(node, "level"),
        None
    );
}

/// A set naming one input of one node twice is refused at the second value before any is set,
/// naming the node and the input, wherever the two stand among the others, a set held on the
/// graph before them included: the second value would replace the first before the node is
/// handed either, so the first would never be published.
///
/// Mental-revert: holding both values hands the node the last alone; checking each set as the
/// loop reaches it returns the set held on the graph first, and the two take the graph's path
/// unrefused.
#[test]
fn a_set_naming_one_input_twice_is_refused_at_the_second_before_any_is_set() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);
    for (sets, second_index) in [
        (
            vec![
                a_set(node, "level", Some(json!(0.25))),
                a_set(node, "gain", Some(json!(0.5))),
                a_set(node, "level", Some(json!(0.75))),
            ],
            2,
        ),
        (
            vec![
                a_set(node, "not_an_input", Some(json!(1.0))),
                a_set(node, "level", Some(json!(0.25))),
                a_set(node, "level", Some(json!(0.75))),
            ],
            2,
        ),
    ] {
        let refused = the_values_set_live(&graph, &sets);
        let Err(LiveInputValueSetRefusal::TheInputIsSetTwice {
            set_index,
            runtime_words,
        }) = refused
        else {
            panic!("{:?}", refused.err());
        };
        assert_eq!(set_index, second_index);
        assert!(
            runtime_words.starts_with("input `level` of `"),
            "{runtime_words}"
        );
    }
    assert_eq!(
        graph.read().live_input_values().value_of(node, "level"),
        None
    );
    assert_eq!(
        graph.read().live_input_values().value_of(node, "gain"),
        None
    );
}

#[test]
fn a_node_dropped_from_the_graph_forgets_its_live_values() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);
    the_values_set_live(&graph, &[a_set(node, "level", Some(json!(0.5)))]).unwrap();
    let mut graph = graph.write();
    assert!(graph.drop_the_node(&level_meter, WhyTheNodeLeaves::Removed));
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
    let mix_uuid = the_uuid_of(&graph, &mix);
    let graph = RwLock::new(graph);
    set_the_saved_value_and_hand_it_over(&graph, &mix, "b", None, Some(json!(0.1))).unwrap();
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

    let rendered_mix = render(&graph.read(), "test_ops:mix");
    assert_eq!(rendered_mix["inputs_waiting_for_a_value"], json!(["a"]));
    assert_eq!(input_of(&rendered_mix, "a")["value"], Value::Null);
    assert!(
        input_of(&rendered_mix, "a")
            .get("value_comes_from")
            .is_none()
    );
    assert_eq!(input_of(&rendered_mix, "b")["value_comes_from"], "saved");
    let rendered_x = input_of(&render(&graph.read(), "test_ops:remap"), "x");
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

    let rendered_mix = render(&graph.read(), "test_ops:mix");
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
    assert_eq!(rendered_mix["input_values"], json!({ "b": 0.1 }), "saved");
}

#[test]
fn an_input_reading_its_declared_default_renders_it_as_the_default() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = graph.traversal().v(&level_meter).first().unwrap();
    assert_eq!(
        crate::core::graph::input_values::the_input_value_change(&graph, node, "level", None),
        vec![RuntimeEvent::InputValueDidChange {
            member: None,
            node: node.node_uuid,
            input: "level".to_owned(),
            value_read: Some(InputValueReadOnTheWire {
                value: Some(json!(0.5)),
                value_comes_from: Some(InputValueComesFromOnTheWire::Default),
                member_values: None,
            }),
        }]
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
    let graph = RwLock::new(graph);
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
    assert_eq!(the_pins_last_handed(), Some(json!({ "x": 0.5 })));

    let committed =
        set_the_saved_value_and_hand_it_over(&graph, &remap, "x", None, Some(json!(0.25))).unwrap();
    assert_eq!(the_pins_last_handed(), Some(json!({ "x": 0.25 })));
    assert_eq!(
        graph.read().live_input_values().value_of(remap_uuid, "x"),
        None
    );
    assert!(
        matches!(
            committed.as_slice(),
            [RuntimeEvent::InputValueDidChange {
                value_read: Some(InputValueReadOnTheWire {
                    value_comes_from: Some(InputValueComesFromOnTheWire::Saved),
                    ..
                }),
                ..
            }]
        ),
        "{committed:?}"
    );
}

/// A setting its type's config schema lists, set on a function node read inside an island,
/// hands its reader the island carrying it under the node's `config`, which the island's
/// evaluation of the node reads.
///
/// Mental-revert: drawing the island's `config` from anything but the node's input values hands
/// the reader the gain the node held before the set.
#[test]
fn a_setting_set_on_a_function_node_read_inside_an_island_hands_its_reader_the_island_holding_it() {
    let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut graph = crate::core::compiler::compiler_ops::function_node_island::tests::a_graph_resolving_these_function_node_types([
        crate::core::compiler::compiler_ops::function_node_island::tests::a_function_node_type(
            "test_ops:scaled",
            &["x"],
            &["result"],
        )
        .with_config_schema(json!({
            "type": "object",
            "properties": {"gain": {"type": "number", "default": 1.0}},
        })),
    ]);
    let scaled = add_a_node(&mut graph, "test_ops:scaled", json!({ "gain": 2.0 }));
    let reader = a_helper_reader_before_its_setup(&mut graph);
    the_link_wiring_of(&graph, &reader)
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();
    let into_the_reader = link(&mut graph, (&scaled, "result"), (&reader, "in1"));
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    let graph = RwLock::new(graph);

    set_the_saved_value_and_hand_it_over(&graph, &scaled, "gain", None, Some(json!(5.0)))
        .expect("the setting is set");

    let handed = delivery
        .late_fed_function_node_islands
        .lock()
        .last()
        .cloned()
        .expect("the reader is handed the island again");
    assert_eq!(handed["nodes"][0]["config"], json!({ "gain": 5.0 }));
}

/// A function node read inside an island, reading `remap`'s `x`, whose reader past its setup
/// hands what it is handed to the delivery answered.
fn a_function_node_read_inside_an_island()
-> (Graph, NodeRunId, RecordingOutOfProcessFarSideLinkDelivery) {
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
    (graph, remap, delivery)
}

fn the_saved_values_of(node_run_id: &NodeRunId, pins: Value) -> SavedValuesOfAKeptNode {
    SavedValuesOfAKeptNode {
        node_run_id: node_run_id.clone(),
        input_values: pins.as_object().unwrap().clone(),
        studio: serde_json::Map::new(),
        handed_over_now: true,
    }
}

/// The saved values a reconcile sets on the nodes it keeps reach each: the island evaluating a
/// function node, and a node's helper. A helper refusing them has its former values put back,
/// the next kept node is still handed its own, and the refusal is answered once every node is
/// set; the same document reconciled again hands the refusing helper its values.
///
/// Mental-revert: leaving the island to a compile hands the reader no island with 0.25; handing
/// the helper nothing leaves it on its default; returning at the refusal leaves the second meter
/// unset; logging the refusal and answering `Ok` reports it as done; leaving the document's pins
/// on the refused node makes a reconcile of the same document skip it; recording the refused
/// values as held leaves the retry unhanded.
#[test]
fn a_reconcile_hands_each_kept_node_the_document_values_and_answers_a_refusal_after_the_last() {
    let (graph, remap, island_delivery) = a_function_node_read_inside_an_island();
    let island_graph = RwLock::new(graph);
    set_the_saved_values_of_each_kept_node_and_hand_them_over(
        &island_graph,
        vec![the_saved_values_of(&remap, json!({ "x": 0.25 }))],
    )
    .unwrap();
    assert_eq!(
        island_delivery
            .late_fed_function_node_islands
            .lock()
            .last()
            .map(|island| island["nodes"][0]["pins"].clone()),
        Some(json!({ "x": 0.25 }))
    );

    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (refusing_meter, refusing_delivery) = a_level_meter_past_its_setup(&mut graph);
    let (level_meter, delivery) = a_level_meter_past_its_setup(&mut graph);
    let helper_graph = RwLock::new(graph);
    refusing_delivery
        .refuses_input_values_once
        .store(true, Ordering::SeqCst);
    let the_document_sets_on_the_helpers = || {
        vec![
            the_saved_values_of(&refusing_meter, json!({ "level": 0.1 })),
            the_saved_values_of(&level_meter, json!({ "level": 0.75 })),
        ]
    };

    let refused = set_the_saved_values_of_each_kept_node_and_hand_them_over(
        &helper_graph,
        the_document_sets_on_the_helpers(),
    );

    let refusal = refused.expect_err("the refusal is answered").to_string();
    assert!(refusal.contains(&refusing_meter.to_string()), "{refusal}");
    assert!(!refusal.contains(&level_meter.to_string()), "{refusal}");
    let the_pins_of = |node_run_id: &NodeRunId| {
        helper_graph
            .read()
            .traversal()
            .v(node_run_id)
            .first()
            .unwrap()
            .input_values
            .clone()
    };
    assert_eq!(
        the_pins_of(&refusing_meter),
        serde_json::Map::new(),
        "the refused node holds its former values"
    );
    assert_eq!(the_pins_of(&level_meter)["level"], json!(0.75));
    assert_eq!(
        delivery.input_values_handed_over.lock().last().unwrap()["level"],
        json!(0.75)
    );

    let kept_nodes_whose_saved_values_change: Vec<SavedValuesOfAKeptNode> =
        the_document_sets_on_the_helpers()
            .into_iter()
            .filter(|kept_node| the_pins_of(&kept_node.node_run_id) != kept_node.input_values)
            .collect();
    assert_eq!(kept_nodes_whose_saved_values_change.len(), 1);
    set_the_saved_values_of_each_kept_node_and_hand_them_over(
        &helper_graph,
        kept_nodes_whose_saved_values_change,
    )
    .unwrap();
    assert_eq!(
        refusing_delivery
            .input_values_handed_over
            .lock()
            .last()
            .unwrap()["level"],
        json!(0.1),
        "the same document reconciled again hands the refused node its values"
    );
}

/// Mental-revert: returning the island's refusal without putting the set back leaves the
/// refused value live, while the reader still evaluates the value before it.
#[test]
fn an_island_refusing_its_values_leaves_every_live_value_as_it_was() {
    let (graph, remap, delivery) = a_function_node_read_inside_an_island();
    let remap_uuid = the_uuid_of(&graph, &remap);
    let graph = RwLock::new(graph);
    the_values_set_live(&graph, &[a_set(remap_uuid, "x", Some(json!(0.25)))])
        .unwrap()
        .expect("held live");

    delivery
        .refuses_function_node_islands
        .store(true, Ordering::SeqCst);
    let refused = the_values_set_live(&graph, &[a_set(remap_uuid, "x", Some(json!(0.75)))]);

    assert!(
        matches!(refused, Err(LiveInputValueSetRefusal::NotHandedOver(_))),
        "{:?}",
        refused.err()
    );
    assert_eq!(
        graph.read().live_input_values().value_of(remap_uuid, "x"),
        Some(json!(0.25))
    );
}

/// Mental-revert: writing the pin and dropping the live value, then returning the helper's
/// refusal with `?`, leaves the graph holding a saved value no helper was handed and the live
/// value gone; recording the refused value as held leaves the same value set again unhanded.
#[test]
fn a_helper_refusing_a_saved_value_puts_the_saved_and_live_values_back() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);
    the_values_set_live(&graph, &[a_set(node, "level", Some(json!(0.25)))])
        .unwrap()
        .expect("held live");

    let handed_over_before_the_refusal = delivery.input_values_handed_over.lock().len();
    delivery
        .refuses_input_values_once
        .store(true, Ordering::SeqCst);
    let refused = set_the_saved_value_and_hand_it_over(
        &graph,
        &level_meter,
        "level",
        None,
        Some(json!(0.75)),
    );

    assert!(refused.is_err());
    {
        let graph = graph.read();
        assert!(graph.node_by_uuid(node).unwrap().input_values.is_empty());
        assert_eq!(
            graph.live_input_values().value_of(node, "level"),
            Some(json!(0.25))
        );
    }
    assert_eq!(
        delivery.input_values_handed_over.lock().len(),
        handed_over_before_the_refusal,
        "the helper, which refused the value, is handed nothing back"
    );
    set_the_saved_value_and_hand_it_over(&graph, &level_meter, "level", None, Some(json!(0.75)))
        .unwrap();
    assert_eq!(
        delivery.input_values_handed_over.lock().last().unwrap()["level"],
        json!(0.75),
        "the same value set again is handed over, the refused one not recorded as held"
    );
}

/// A string set live on a Float input of a function node, read inside an island or run in a
/// helper of its own, is refused, and the saved value still reads.
///
/// Mental-revert: holding the string live hands the helper no value for the input, and the live
/// string hides the saved one in `graph`.
#[test]
fn a_string_set_live_on_a_function_node_in_a_helper_of_its_own_is_refused() {
    use crate::core::execution::ProcessExecution;
    use crate::core::graph::{
        FunctionNodePromotedToItsOwnProcessComponent, GraphNodeWithComponents,
        HowItsHelpersRanAFunctionNode,
    };
    let mut graph = a_graph_resolving_three_function_node_types();
    let remap = add_a_node(&mut graph, "test_ops:remap", Value::Null);
    let remap_uuid = the_uuid_of(&graph, &remap);
    let graph = RwLock::new(graph);
    assert!(
        the_values_set_live(&graph, &[a_set(remap_uuid, "x", Some(json!("loud")))]).is_err(),
        "a Float input of a function node read inside an island refuses a string"
    );
    graph
        .read()
        .live_input_values()
        .set(remap_uuid, "x", None, NodeValueLifetime::Kept);
    set_the_saved_value_and_hand_it_over(&graph, &remap, "x", None, Some(json!(0.25))).unwrap();
    graph
        .write()
        .traversal_mut()
        .v(&remap)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(FunctionNodePromotedToItsOwnProcessComponent {
            process_execution: ProcessExecution::Continuous { interval_ms: 10 },
            evaluated_at_setup: false,
            how_its_own_helper_ran_it: std::sync::Arc::new(
                HowItsHelpersRanAFunctionNode::default(),
            ),
        });

    let refused = the_values_set_live(&graph, &[a_set(remap_uuid, "x", Some(json!("loud")))]);

    assert!(
        matches!(
            refused,
            Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { set_index: 0, .. })
        ),
        "{:?}",
        refused.err()
    );
    let graph = graph.read();
    let node = graph.node_by_uuid(remap_uuid).unwrap();
    assert!(matches!(
        crate::core::graph::input_values::the_input_value_change(&graph, node, "x", None).as_slice(),
        [RuntimeEvent::InputValueDidChange { value_read: Some(InputValueReadOnTheWire {
            value: Some(value), value_comes_from: Some(InputValueComesFromOnTheWire::Saved), .. }), .. }]
            if value == &json!(0.25)
    ));
}

/// While the stream is stopped, a live value on a `kept` input is held for the next Play, one on
/// a `reset` input is refused naming the stopped playback and leaves every value as it was, and
/// taking a `reset` input's live value off is no refusal.
///
/// Mental-revert: dropping the check of a `reset` input while stopped holds `gain`'s value live,
/// which the next Stop would forget and no Play would ever read; checking it after the values
/// are set leaves `level` at 0.75.
#[test]
fn while_stopped_a_kept_input_holds_a_live_value_and_a_reset_one_refuses_it() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);
    let set_while_stopped = |sets: &[InputValueToSetLive]| {
        set_the_input_values_live_and_hand_each_node_its_own(
            &graph,
            sets,
            PlaybackOfTheStreamSetOn::Stopped,
        )
    };

    set_while_stopped(&[a_set(node, "level", Some(json!(0.25)))])
        .unwrap()
        .expect("a kept input holds its live value while stopped");
    let refused = set_while_stopped(&[
        a_set(node, "level", Some(json!(0.75))),
        a_set(node, "gain", Some(json!(2.0))),
    ]);

    match refused {
        Err(LiveInputValueSetRefusal::TheInputIsResetWhileStopped {
            set_index: 1,
            runtime_words,
        }) => assert!(runtime_words.contains("input `gain`"), "{runtime_words}"),
        other => panic!(
            "a reset input refuses a live value while stopped: {:?}",
            other.err()
        ),
    }
    let live_values = graph.read().live_input_values().clone();
    assert_eq!(live_values.value_of(node, "level"), Some(json!(0.25)));
    assert_eq!(live_values.value_of(node, "gain"), None);
    set_while_stopped(&[a_set(node, "gain", None)])
        .unwrap()
        .expect("taking a reset input's live value off is no refusal");
}

/// At Stop a stream forgets the live value of each `reset` input, naming each; a node torn down
/// at the Stop keeps each `kept` input's value, and a removed node forgets every one.
///
/// Mental-revert: forgetting every live value of a node torn down at a Stop drops `level`'s;
/// keeping every one leaves `gain`'s, set again before the teardown; keeping the `kept` ones of
/// a removed node leaves `level`'s live after the removal.
#[test]
fn a_stop_forgets_each_reset_live_value_and_a_removal_forgets_every_one() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let (level_meter, _delivery) = a_level_meter_past_its_setup(&mut graph);
    let node = the_uuid_of(&graph, &level_meter);
    let graph = RwLock::new(graph);
    the_values_set_live(
        &graph,
        &[
            a_set(node, "level", Some(json!(0.25))),
            a_set(node, "gain", Some(json!(2.0))),
        ],
    )
    .unwrap()
    .expect("held live");
    let graph = graph.read();
    let live_values = graph.live_input_values();

    assert_eq!(
        live_values.forget_every_reset_value(),
        [(node, "gain".to_owned())]
    );
    live_values.set(node, "gain", Some(json!(3.0)), NodeValueLifetime::Reset);
    live_values.forget_the_node(node, WhyTheNodeLeaves::TornDownAtAStop);
    assert_eq!(live_values.value_of(node, "level"), Some(json!(0.25)));
    assert_eq!(live_values.value_of(node, "gain"), None);

    live_values.forget_the_node(node, WhyTheNodeLeaves::Removed);
    assert_eq!(live_values.value_of(node, "level"), None);
}

/// `island_count` function nodes, each read inside the island of a helper reader of its own past
/// its setup, with the delivery each reader hands what it is handed to.
fn islands_each_read_by_a_reader_of_its_own(
    island_count: usize,
) -> (
    Graph,
    Vec<NodeRunId>,
    Vec<RecordingOutOfProcessFarSideLinkDelivery>,
) {
    let mut graph = a_graph_resolving_three_function_node_types();
    let mut remaps = Vec::with_capacity(island_count);
    let mut deliveries = Vec::with_capacity(island_count);
    for _ in 0..island_count {
        let remap = add_a_node(&mut graph, "test_ops:remap", Value::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
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
        remaps.push(remap);
        deliveries.push(delivery);
    }
    (graph, remaps, deliveries)
}

const SETS_MEASURED: usize = 600;
const ISLANDS_MEASURED: usize = 32;

/// A value saved on a function node read inside an island feeds the island evaluating it again,
/// and compares no other: another island left behind its graph is not handed its island. 600
/// saved sets, as a fader sends in ten seconds at 60 a second, are timed against setting the pin
/// and comparing every island the graph holds, as a compile does, and the two times printed.
///
/// Mental-revert: comparing every island the graph holds on a saved set hands the island left
/// behind its graph its island, and its time grows with the islands the graph holds.
#[test]
fn a_value_saved_on_one_island_member_feeds_one_island_and_its_cost_is_measured() {
    let (mut graph, remaps, deliveries) =
        islands_each_read_by_a_reader_of_its_own(ISLANDS_MEASURED);
    let the_islands_fed = |deliveries: &[RecordingOutOfProcessFarSideLinkDelivery]| -> Vec<usize> {
        deliveries
            .iter()
            .map(|delivery| delivery.late_fed_function_node_islands.lock().len())
            .collect()
    };
    graph
        .traversal_mut()
        .v(&remaps[1])
        .first_mut()
        .unwrap()
        .input_values
        .insert("x".to_owned(), json!(0.5));
    let graph = RwLock::new(graph);
    let islands_fed_as_wired = the_islands_fed(&deliveries);

    let the_island_evaluating_it_fed = std::time::Instant::now();
    for set_index in 0..SETS_MEASURED {
        set_the_saved_value_and_hand_it_over(
            &graph,
            &remaps[0],
            "x",
            None,
            Some(json!(set_index as f64 / 1000.0)),
        )
        .unwrap();
    }
    let the_island_evaluating_it_fed = the_island_evaluating_it_fed.elapsed();

    let islands_fed: Vec<usize> = the_islands_fed(&deliveries)
        .into_iter()
        .zip(islands_fed_as_wired)
        .map(|(fed, fed_as_wired)| fed - fed_as_wired)
        .collect();
    assert_eq!(islands_fed[0], SETS_MEASURED, "each set feeds its island");
    assert!(
        islands_fed[1..].iter().all(|fed| *fed == 0),
        "no other island is compared, the one left behind its graph included: {islands_fed:?}"
    );

    let every_island_compared = std::time::Instant::now();
    for set_index in 0..SETS_MEASURED {
        let mut graph = graph.write();
        graph
            .traversal_mut()
            .v(&remaps[0])
            .first_mut()
            .unwrap()
            .input_values
            .insert("x".to_owned(), json!(1.0 + set_index as f64 / 1000.0));
        hand_each_reader_the_island_a_compile_changed(&mut graph, &[]).unwrap();
    }
    let every_island_compared = every_island_compared.elapsed();

    eprintln!(
        "{SETS_MEASURED} saved sets on one member of {ISLANDS_MEASURED} islands: the saved set \
         feeding the island evaluating it, {:?} ({:?} a set); the pin set and every island \
         compared as a compile compares them, {:?} ({:?} a set)",
        the_island_evaluating_it_fed,
        the_island_evaluating_it_fed / SETS_MEASURED as u32,
        every_island_compared,
        every_island_compared / SETS_MEASURED as u32,
    );
}

/// A value set live, and one saved, on the input of a running shader node is held apart from the
/// graph and handed to the evaluator its island runs in this process, so the running shader takes
/// it without a rebuild; the saved one leaves no value live.
///
/// Mental-revert: holding a shader node's value on the graph answers `None` here and leaves the
/// built evaluator on 0, the value it was built with.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_running_shader_node_takes_a_value_set_live_and_one_saved_on_it() {
    use crate::core::compiler::compiler_ops::unlinked_input_values_delivery::tests::a_shader_node_in_this_process::{
        a_shader_node_fused_into_the_island_another_hosts_and_evaluates, the_value_the_host_reads,
    };
    let (graph, _fused, host) = a_shader_node_fused_into_the_island_another_hosts_and_evaluates();
    let host_uuid = the_uuid_of(&graph.read(), &host);

    the_values_set_live(&graph, &[a_set(host_uuid, "amount", Some(json!(0.75)))])
        .unwrap()
        .expect("a shader node's value is held apart from the graph");
    assert_eq!(the_value_the_host_reads(&graph, &host, "amount"), 0.75);

    set_the_saved_value_and_hand_it_over(&graph, &host, "amount", None, Some(json!(0.25))).unwrap();
    assert_eq!(the_value_the_host_reads(&graph, &host, "amount"), 0.25);
    assert_eq!(
        graph
            .read()
            .live_input_values()
            .value_of(host_uuid, "amount"),
        None
    );
}
