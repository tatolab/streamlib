// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Island formation, its wiring into helpers, and the live edits an island takes or refuses.

use super::*;

#[test]
fn each_link_is_wired_by_the_kinds_of_its_two_ends() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    let source = add_a_node(
        &mut graph,
        crate::core::test_support::MockOutputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    let call = link(&mut graph, (&lfo, "result"), (&remap, "x"));
    let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
    assert_eq!(
        how_a_link_is_wired(&graph, &call).unwrap(),
        HowALinkIsWired::AsACallInsideAFunctionNodeIsland
    );
    assert_eq!(
        how_a_link_is_wired(&graph, &into_the_reader).unwrap(),
        HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead
    );
    let into_a_function_node_from_a_class_node = link(&mut graph, (&source, "out1"), (&remap, "x"));
    assert_eq!(
        how_a_link_is_wired(&graph, &into_a_function_node_from_a_class_node).unwrap(),
        HowALinkIsWired::ThroughAChannelReadByTheReaderOfItsFunctionNodeIsland
    );
}

#[test]
fn the_island_lists_each_node_after_what_it_reads_once_with_its_config() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(
        &mut graph,
        "test_ops:lfo",
        serde_json::json!({"rate_hz": 2.0}),
    );
    let low = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let high = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&lfo, "result"), (&low, "x"));
    link(&mut graph, (&lfo, "result"), (&high, "x"));
    link(&mut graph, (&low, "result"), (&mix, "a"));
    link(&mut graph, (&high, "result"), (&mix, "b"));
    let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

    let island = the_island_feeding_a_link(&graph, &into_the_reader).unwrap();
    assert_eq!(island["input_port"], "in1");
    assert_eq!(island["output"]["node"], mix.to_string());
    let nodes_in_order: Vec<&str> = island["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["node"].as_str().unwrap())
        .collect();
    assert_eq!(nodes_in_order.len(), 4, "{nodes_in_order:?}");
    let position = |node_id: &NodeRunId| {
        nodes_in_order
            .iter()
            .position(|listed| *listed == node_id.as_str())
            .unwrap()
    };
    assert!(position(&lfo) < position(&low) && position(&lfo) < position(&high));
    assert!(position(&low) < position(&mix) && position(&high) < position(&mix));
    assert_eq!(island["nodes"][0]["config"]["rate_hz"], 2.0);
    assert_eq!(island["nodes"][0]["import_path"], "test_ops:lfo");
}

#[test]
fn a_cycle_of_function_nodes_is_refused_naming_it() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let first = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let second = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&first, "result"), (&second, "a"));
    link(&mut graph, (&second, "result"), (&first, "x"));
    let into_the_reader = link(&mut graph, (&second, "result"), (&reader, "in1"));
    let refusal = the_island_feeding_a_link(&graph, &into_the_reader)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("in a cycle"), "{refusal}");
}

#[test]
fn a_link_into_a_helper_carries_its_island_on_the_setup_command_and_is_wired() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();

    let islands = reader_setup_command_ports(&graph, &reader)["islands"].clone();
    assert_eq!(islands.as_array().unwrap().len(), 1);
    assert_eq!(islands[0]["link_id"], into_the_reader.to_string());
    assert!(matches!(
        graph
            .traversal()
            .e(&into_the_reader)
            .first()
            .unwrap()
            .get::<LinkStateComponent>()
            .map(|state| state.0),
        Some(LinkState::Wired)
    ));

    unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
        .unwrap();
    assert_eq!(
        reader_setup_command_ports(&graph, &reader)["islands"],
        serde_json::json!([])
    );
}

/// `graph` renders `ran_compiled` on each node an island carries: `null`
/// once its reader is wired, then what the reader's helper reported.
#[test]
fn each_island_node_renders_how_its_readers_helper_ran_it() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&lfo, "result"), (&remap, "x"));
    let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    let ran_compiled = |graph: &Graph, node_id: &NodeRunId| {
        graph
            .traversal()
            .v(node_id)
            .first()
            .unwrap()
            .render_components()
            .ran_compiled
    };
    assert_eq!(ran_compiled(&graph, &lfo), None);
    assert_eq!(ran_compiled(&graph, &remap), None);

    graph
        .traversal()
        .v(&reader)
        .first()
        .unwrap()
        .get::<OutOfProcessLinkWiringComponent>()
        .unwrap()
        .0
        .note_how_its_far_side_ran_its_function_nodes(Some(&serde_json::json!({
            lfo.to_string(): true,
            remap.to_string(): false,
        })));
    assert_eq!(ran_compiled(&graph, &lfo), Some(true));
    assert_eq!(ran_compiled(&graph, &remap), Some(false));
}

/// `graph` renders a raise the reader's helper reports for an island node as
/// that node's own `process_raising_because`, and `null` once it runs cleanly.
#[test]
fn each_island_node_renders_the_raise_its_readers_helper_reports_on_it() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&lfo, "result"), (&remap, "x"));
    let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    let process_raising_because = |graph: &Graph, node_id: &NodeRunId| {
        graph
            .traversal()
            .v(node_id)
            .first()
            .unwrap()
            .render_components()
            .process_raising_because
    };
    let how_the_readers_helper_ran_each_function_node = graph
        .traversal()
        .v(&reader)
        .first()
        .unwrap()
        .get::<OutOfProcessLinkWiringComponent>()
        .unwrap()
        .0
        .how_its_far_side_ran_each_function_node();
    assert_eq!(process_raising_because(&graph, &lfo), None);

    how_the_readers_helper_ran_each_function_node.note_why_the_helper_finds_it_raising(
        lfo.as_str(),
        Some("raised RuntimeError: broke on purpose (nodes/lfo.py, line 3)".to_string()),
    );
    assert_eq!(
        process_raising_because(&graph, &lfo),
        Some("raised RuntimeError: broke on purpose (nodes/lfo.py, line 3)".to_string())
    );
    assert_eq!(process_raising_because(&graph, &remap), None);

    how_the_readers_helper_ran_each_function_node
        .note_why_the_helper_finds_it_raising(lfo.as_str(), None);
    assert_eq!(process_raising_because(&graph, &lfo), None);
}

#[test]
fn graph_names_each_function_nodes_host_as_the_readers_its_island_feeds() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let first_reader = a_helper_reader_before_its_setup(&mut graph);
    let second_reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&lfo, "result"), (&remap, "x"));
    link(&mut graph, (&remap, "result"), (&first_reader, "in1"));
    link(&mut graph, (&lfo, "result"), (&second_reader, "in1"));

    note_where_each_function_node_runs(&mut graph);

    let host = |node_id: &NodeRunId| {
        graph
            .traversal()
            .v(node_id)
            .first()
            .unwrap()
            .render_components()
            .host
            .map(|host| serde_json::to_value(host).unwrap())
    };
    let name_of = |node_id: &NodeRunId| {
        graph
            .traversal()
            .v(node_id)
            .first()
            .unwrap()
            .display_name
            .clone()
    };
    let mut both_readers = vec![name_of(&first_reader), name_of(&second_reader)];
    both_readers.sort();
    assert_ne!(
        both_readers[0], both_readers[1],
        "each reader has its own name"
    );
    assert_eq!(
        host(&lfo),
        Some(serde_json::json!({ "inside_the_read_of": both_readers }))
    );
    assert_eq!(
        host(&remap),
        Some(serde_json::json!({ "inside_the_read_of": [name_of(&first_reader)] }))
    );
    assert_eq!(host(&first_reader), None, "a class node names no host");
}

#[test]
fn a_reactive_reader_fed_only_by_islands_is_refused_naming_continuous_execution() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup_driven_in(&mut graph, ProcessExecution::Reactive);
    let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    let refusal = wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap_err()
    .to_string();
    assert!(refusal.contains("execution=\"continuous\""), "{refusal}");
}

/// A reader whose helper is past its setup command, handing every later
/// link and island to `delivery`.
fn a_helper_reader_past_its_setup(
    graph: &mut Graph,
    delivery: &crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery,
) -> NodeRunId {
    let reader = a_helper_reader_before_its_setup(graph);
    the_link_wiring_of(graph, &reader)
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();
    reader
}

fn the_link_wiring_of(graph: &Graph, node_id: &NodeRunId) -> Arc<OutOfProcessLinkWiringEnvelope> {
    Arc::clone(
        &graph
            .traversal()
            .v(node_id)
            .first()
            .unwrap()
            .get::<OutOfProcessLinkWiringComponent>()
            .unwrap()
            .0,
    )
}

fn the_node_ids_of_an_island(island: &JsonValue) -> Vec<String> {
    island["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["node"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_function_node_linked_live_into_a_running_reader_is_handed_over_as_its_island() {
    let delivery = crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_past_its_setup(&mut graph, &delivery);
    let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));

    assert!(
        the_refusal_of_a_commit(
            &graph,
            std::slice::from_ref(&lfo),
            &[],
            std::slice::from_ref(&into_the_reader),
            &[],
            &[]
        )
        .is_none(),
        "a running reader takes a live link from a function node"
    );
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();

    let handed_over = delivery.late_fed_function_node_islands.lock().clone();
    assert_eq!(handed_over.len(), 1);
    assert_eq!(handed_over[0]["input_port"], "in1");
    assert_eq!(handed_over[0]["link_id"], into_the_reader.as_str());
    assert_eq!(
        the_node_ids_of_an_island(&handed_over[0]),
        vec![lfo.to_string()]
    );
    assert!(
        handed_over[0]["nodes"][0]["breadcrumb_number"].is_u64(),
        "the reader writes each node's breadcrumb while calling it"
    );
    assert_eq!(
        link_state_of(&graph, &into_the_reader),
        LinkState::Pending,
        "pending until the reader says it evaluates the island"
    );
}

#[test]
fn a_pin_edit_on_an_island_node_hands_its_running_reader_the_island_carrying_it() {
    let delivery = crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_past_its_setup(&mut graph, &delivery);
    let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    assert_eq!(
        delivery.late_fed_function_node_islands.lock()[0]["nodes"][0]["pins"],
        serde_json::json!({})
    );

    graph
        .traversal_mut()
        .v(&lfo)
        .first_mut()
        .unwrap()
        .pins
        .insert("rate_hz".to_string(), serde_json::json!(4.0));
    hand_each_reader_the_island_a_compile_changed(&mut graph, &[]).unwrap();

    let handed_over = delivery.late_fed_function_node_islands.lock().clone();
    assert_eq!(
        handed_over.len(),
        2,
        "the pin edit re-sends the island once"
    );
    assert_eq!(
        handed_over[1]["nodes"][0]["pins"],
        serde_json::json!({ "rate_hz": 4.0 })
    );
    hand_each_reader_the_island_a_compile_changed(&mut graph, &[]).unwrap();
    assert_eq!(
        delivery.late_fed_function_node_islands.lock().len(),
        2,
        "an island no edit changed is not re-sent"
    );
}

#[test]
fn an_edit_inside_an_island_hands_its_running_reader_the_island_again_and_no_other() {
    let delivery = crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let untouched_lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
    let untouched_into_the_reader = link(&mut graph, (&untouched_lfo, "result"), (&reader, "in2"));
    for island_link in [&into_the_reader, &untouched_into_the_reader] {
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            island_link,
            &Iceoryx2Node::for_this_test_process(),
        )
        .unwrap();
    }
    the_link_wiring_of(&graph, &reader)
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();

    hand_each_reader_the_island_a_compile_changed(&mut graph, &[]).unwrap();
    assert!(
        delivery.late_fed_function_node_islands.lock().is_empty(),
        "an island no edit changed is not handed over again"
    );

    let into_the_remap = link(&mut graph, (&lfo, "result"), (&remap, "x"));
    wire_a_call_inside_a_function_node_island(&mut graph, &into_the_remap).unwrap();
    hand_each_reader_the_island_a_compile_changed(&mut graph, &[into_the_remap]).unwrap();

    let handed_over = delivery.late_fed_function_node_islands.lock().clone();
    assert_eq!(
        handed_over.len(),
        1,
        "only the island the edit changed: {handed_over:?}"
    );
    assert_eq!(handed_over[0]["link_id"], into_the_reader.as_str());
    assert_eq!(
        the_node_ids_of_an_island(&handed_over[0]),
        vec![lfo.to_string(), remap.to_string()],
        "each node after what it reads"
    );
    assert_eq!(link_state_of(&graph, &into_the_reader), LinkState::Pending);
    assert_eq!(
        link_state_of(&graph, &untouched_into_the_reader),
        LinkState::Wired
    );
}

#[test]
fn unwiring_an_island_link_tells_its_running_reader_to_stop_evaluating_it() {
    let delivery = crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_past_its_setup(&mut graph, &delivery);
    let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();

    assert!(
        the_refusal_of_a_commit(
            &graph,
            &[],
            std::slice::from_ref(&lfo),
            &[],
            std::slice::from_ref(&into_the_reader),
            &[]
        )
        .is_none(),
        "a running reader's island may be removed live"
    );
    unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
        .unwrap();
    assert_eq!(
        delivery.unfed_function_node_islands.lock().clone(),
        vec![("in1".to_string(), into_the_reader.to_string())]
    );
    assert!(
        the_link_wiring_of(&graph, &reader)
            .the_function_node_island_recorded_for_a_link(into_the_reader.as_str())
            .is_none(),
        "a later setup command no longer carries it"
    );
}

#[test]
fn a_link_closing_a_cycle_of_function_nodes_is_refused_naming_the_cycle() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    link(&mut graph, (&lfo, "result"), (&mix, "a"));
    link(&mut graph, (&mix, "result"), (&remap, "x"));
    let closing_the_cycle = link(&mut graph, (&remap, "result"), (&mix, "b"));

    let refusal = the_refusal_of_a_commit(
        &graph,
        &[],
        &[],
        std::slice::from_ref(&closing_the_cycle),
        &[],
        &[],
    )
    .expect("refused");
    assert!(
        refusal.contains("closes a cycle of function nodes"),
        "{refusal}"
    );
    assert!(
        refusal.contains(&format!("{remap} → {mix} → {remap}")),
        "{refusal}"
    );
    assert!(refusal.contains("`PreviousValue` node"), "{refusal}");
}

fn a_graph_resolving_the_test_function_node_types_and_previous_value() -> Graph {
    a_graph_resolving_these_function_node_types([
        a_function_node_type("test_ops:lfo", &[], &["result"]),
        a_function_node_type("test_ops:remap", &["x"], &["result"]),
        a_function_node_type("test_ops:mix", &["a", "b"], &["result"]),
        a_function_node_type(PREVIOUS_VALUE_NODE_TYPE, &["value"], &["result"]),
    ])
}

#[test]
fn a_link_closing_a_loop_through_previous_value_is_taken_live() {
    let mut graph = a_graph_resolving_the_test_function_node_types_and_previous_value();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let previous_value = add_a_node(&mut graph, PREVIOUS_VALUE_NODE_TYPE, JsonValue::Null);
    link(&mut graph, (&lfo, "result"), (&mix, "a"));
    link(&mut graph, (&previous_value, "result"), (&mix, "b"));
    let closing_the_loop = link(&mut graph, (&mix, "result"), (&previous_value, "value"));

    assert_eq!(
        the_refusal_of_a_commit(
            &graph,
            &[],
            &[],
            std::slice::from_ref(&closing_the_loop),
            &[],
            &[]
        ),
        None
    );
}

#[test]
fn an_island_closing_a_loop_through_previous_value_evaluates_it_before_what_it_reads() {
    let mut graph = a_graph_resolving_the_test_function_node_types_and_previous_value();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let previous_value = add_a_node(&mut graph, PREVIOUS_VALUE_NODE_TYPE, JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&lfo, "result"), (&mix, "a"));
    link(&mut graph, (&previous_value, "result"), (&mix, "b"));
    link(&mut graph, (&mix, "result"), (&previous_value, "value"));
    let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

    let island = the_island_feeding_a_link(&graph, &into_the_reader).unwrap();

    let order = the_node_ids_of_an_island(&island);
    let position_of = |node_id: &NodeRunId| {
        order
            .iter()
            .position(|each| each == node_id.as_str())
            .unwrap()
    };
    assert_eq!(order.len(), 3, "each node once: {order:?}");
    assert!(
        position_of(&previous_value) < position_of(&mix),
        "{order:?}"
    );
    assert!(position_of(&lfo) < position_of(&mix), "{order:?}");
    let previous_value_node = &island["nodes"][position_of(&previous_value)];
    assert_eq!(
        previous_value_node["links_into"],
        serde_json::json!([{ "port": "value", "from": { "node": mix.to_string(), "port": "result" } }]),
        "`PreviousValue` still names what it reads, which the reader takes from the last evaluation"
    );
}

#[test]
fn a_loop_whose_way_back_skips_previous_value_is_still_refused() {
    let mut graph = a_graph_resolving_the_test_function_node_types_and_previous_value();
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let previous_value = add_a_node(&mut graph, PREVIOUS_VALUE_NODE_TYPE, JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&previous_value, "result"), (&mix, "a"));
    link(&mut graph, (&mix, "result"), (&remap, "x"));
    link(&mut graph, (&remap, "result"), (&mix, "b"));
    let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

    let refusal = the_island_feeding_a_link(&graph, &into_the_reader)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("in a cycle"), "{refusal}");
    assert!(refusal.contains("`PreviousValue` node"), "{refusal}");
}

#[test]
fn a_live_config_update_to_a_function_node_is_refused_by_name() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let refusal = the_refusal_of_a_commit(
        &graph,
        &[],
        &[],
        &[],
        &[],
        &[(lfo, serde_json::json!({"rate_hz": 3.0}))],
    )
    .expect("refused");
    assert!(
        refusal.contains("a live config update is not carried to an island"),
        "{refusal}"
    );
}

#[test]
fn a_second_link_into_a_port_an_island_feeds_is_refused_by_name() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let first_lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let second_lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    let first = link(&mut graph, (&first_lfo, "result"), (&reader, "in1"));
    let second = link(&mut graph, (&second_lfo, "result"), (&reader, "in1"));
    let refusal = the_refusal_of_a_commit(
        &graph,
        &[first_lfo, second_lfo, reader],
        &[],
        &[first, second],
        &[],
        &[],
    )
    .expect("refused");
    assert!(
        refusal.contains("shares the port with no other link"),
        "{refusal}"
    );
}

#[test]
fn an_island_into_a_port_declaring_an_audio_window_is_refused_by_name() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let windowed_consumer = add_a_node(
        &mut graph,
        crate::core::test_support::MockWindowedAudioConsumerNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    let into_the_window = link(&mut graph, (&lfo, "result"), (&windowed_consumer, "audio"));
    let refusal = the_refusal_of_a_commit(
        &graph,
        &[lfo, windowed_consumer],
        &[],
        &[into_the_window],
        &[],
        &[],
    )
    .expect("refused");
    assert!(refusal.contains("declares an `audio_window`"), "{refusal}");
}

#[test]
fn removing_the_last_source_of_a_reactive_reader_an_island_feeds_is_refused() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let source = add_a_node(
        &mut graph,
        crate::core::test_support::MockOutputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    let reader = a_helper_reader_before_its_setup_driven_in(&mut graph, ProcessExecution::Reactive);
    link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    let from_the_source = link(&mut graph, (&source, "out2"), (&reader, "in2"));
    let refusal =
        the_refusal_of_a_commit(&graph, &[], &[], &[], &[from_the_source], &[]).expect("refused");
    assert!(refusal.contains("would never run again"), "{refusal}");
}

#[test]
fn an_island_into_a_node_in_the_app_process_is_refused_by_name() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let native_reader = add_a_node(
        &mut graph,
        crate::core::test_support::MockInputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    let into_the_reader = link(&mut graph, (&lfo, "result"), (&native_reader, "in1"));
    let refusal = wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap_err()
    .to_string();
    assert!(refusal.contains("runs in the app process"), "{refusal}");
}

#[test]
fn an_island_wired_while_its_readers_setup_command_is_being_built_rides_that_command() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    the_link_wiring_of(&graph, &reader).hold_every_later_link_until_the_setup_command_goes_out();
    let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    assert_eq!(link_state_of(&graph, &into_the_reader), LinkState::Wired);
    let setup_command_islands =
        the_link_wiring_of(&graph, &reader).as_setup_command_ports()["islands"].clone();
    assert_eq!(
        setup_command_islands[0]["link_id"],
        into_the_reader.as_str()
    );
}
