// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Class nodes an island reads: the hidden input each reader reads them at, and the stream facts it reaches.

use super::*;

#[test]
fn a_reader_reaches_the_stream_facts_of_the_class_node_an_island_hands_on_unread() {
    let mut gate = a_function_node_type("test_ops:gate", &["value"], &["result"]);
    gate.outputs[0].pin_declaration = Some(crate::core::descriptors::PinDeclaration {
        pin_type: crate::core::descriptors::DeclaredPinType::SameAsInput {
            input_port_name: "value".into(),
        },
        ..crate::core::descriptors::PinDeclaration::exactly(
            crate::core::descriptors::PinType::Float { exact_size: None },
        )
    });
    let mut graph = a_graph_resolving_these_function_node_types([
        gate,
        a_function_node_type("test_ops:remap", &["x"], &["result"]),
    ]);
    let source = add_a_node(
        &mut graph,
        crate::core::test_support::MockOutputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    graph
        .traversal_mut()
        .v(&source)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive),
        )));
    let gate = add_a_node(&mut graph, "test_ops:gate", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&source, "out1"), (&gate, "value"));
    let into_the_reader = link(&mut graph, (&gate, "result"), (&reader, "in1"));
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    let channel_of_the_class_node = crate::iceoryx2::source_channel_name(source.as_str(), "out1")
        .unwrap()
        .into_string();

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &iceoryx2_node,
    )
    .unwrap();
    assert!(
        the_reader_input_port_records_an_inbound_link_named(
            &graph,
            &reader,
            "in1",
            &channel_of_the_class_node
        ),
        "the reader's input port reads the class node's output through the gate"
    );

    unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
        .unwrap();
    assert!(
        !the_reader_input_port_records_an_inbound_link_named(
            &graph,
            &reader,
            "in1",
            &channel_of_the_class_node
        ),
        "an unwired island link leaves no inbound link behind"
    );
}

#[test]
fn an_island_handing_out_values_of_its_own_records_no_upstream_output_at_its_reader() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (_remap, reader, into_the_island, into_the_reader) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let source = link_in(&graph, &into_the_island)
        .unwrap()
        .source()
        .node_run_id()
        .clone();

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    assert!(!the_reader_input_port_records_an_inbound_link_named(
        &graph,
        &reader,
        "in1",
        &crate::iceoryx2::source_channel_name(source.as_str(), "out1")
            .unwrap()
            .into_string()
    ));
}

#[test]
fn a_class_node_linked_into_an_island_is_read_at_a_hidden_input_of_its_reader() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (remap, reader, into_the_island, into_the_reader) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &iceoryx2_node,
    )
    .unwrap();

    let reads = the_reads_of(&graph, &into_the_island);
    assert_eq!(
        reads.len(),
        1,
        "the link into the island is read at its reader once"
    );
    let read_at = reads[0].clone();
    assert_eq!(read_at.node_run_id(), &reader);
    assert_eq!(
        read_at.port_name(),
        the_reader_input_port_a_link_into_a_function_node_is_read_at(
            link_in(&graph, &into_the_island).expect("the link into the island"),
            &into_the_reader
        )
    );
    let island = &reader_setup_command_ports(&graph, &reader)["islands"][0];
    let remap_in_the_island = island["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["node"] == remap.as_str())
        .unwrap();
    assert_eq!(
        remap_in_the_island["links_into"],
        serde_json::json!([{
            "port": "x",
            "from": {"reader_input_port": read_at.port_name()},
        }])
    );
    let source = link_in(&graph, &into_the_island)
        .unwrap()
        .source()
        .node_run_id()
        .clone();
    let ports_of_both_ends_wired_once = [
        reader_setup_command_ports(&graph, &source),
        reader_setup_command_ports(&graph, &reader),
    ];

    wire_a_link_into_a_function_node_island_through_its_reader(
        &mut graph,
        &into_the_island,
        &iceoryx2_node,
    )
    .unwrap();
    assert_eq!(
        link_state_of(&graph, &into_the_island),
        LinkState::Wired,
        "a link its reader already opened is wired once"
    );
    assert_eq!(
        [
            reader_setup_command_ports(&graph, &source),
            reader_setup_command_ports(&graph, &reader),
        ],
        ports_of_both_ends_wired_once,
        "wiring it again adds no entry at either end"
    );
}

/// The reader input ports `link_id` is read at, in island link order.
fn the_reads_of(graph: &Graph, link_id: &LinkUniqueId) -> Vec<InputLinkPortRef> {
    graph
        .traversal()
        .e(link_id)
        .first()
        .unwrap()
        .get::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
        .map(|read_through| {
            read_through
                .reads_by_island_link
                .iter()
                .map(|read| read.reader_input_port.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The `reader_input_port` the island `island_link_id` carries names for
/// `function_node_id`'s input `port`.
fn the_reader_input_port_an_island_names(
    graph: &Graph,
    island_link_id: &LinkUniqueId,
    function_node_id: &NodeRunId,
) -> JsonValue {
    let island = the_island_feeding_a_link(graph, island_link_id).unwrap();
    island["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["node"] == function_node_id.as_str())
        .unwrap()["links_into"][0]["from"]["reader_input_port"]
        .clone()
}

#[test]
fn an_island_reading_a_class_node_that_feeds_two_readers_is_read_at_each_of_them() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (remap, reader, into_the_island, into_the_reader) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let second_reader = a_helper_reader_before_its_setup(&mut graph);
    let into_the_second_reader = link(&mut graph, (&remap, "result"), (&second_reader, "in1"));
    assert_eq!(
        the_refusal_of_a_commit(
            &graph,
            &[],
            &[],
            std::slice::from_ref(&into_the_second_reader),
            &[],
        ),
        None
    );
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();

    for island_link in [&into_the_reader, &into_the_second_reader] {
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            island_link,
            &iceoryx2_node,
        )
        .unwrap();
    }

    let reads = the_reads_of(&graph, &into_the_island);
    assert_eq!(
        reads,
        vec![
            InputLinkPortRef::new(
                reader.clone(),
                the_reader_input_port_a_link_into_a_function_node_is_read_at(
                    link_in(&graph, &into_the_island).expect("the link into the island"),
                    &into_the_reader
                )
            ),
            InputLinkPortRef::new(
                second_reader.clone(),
                the_reader_input_port_a_link_into_a_function_node_is_read_at(
                    link_in(&graph, &into_the_island).expect("the link into the island"),
                    &into_the_second_reader
                )
            ),
        ]
    );
    for (island_link, read_at) in [&into_the_reader, &into_the_second_reader]
        .into_iter()
        .zip(&reads)
    {
        assert_eq!(
            the_reader_input_port_an_island_names(&graph, island_link, &remap),
            read_at.port_name(),
            "each island names the input its own reader reads the link at"
        );
    }
    let source = link_in(&graph, &into_the_island)
        .unwrap()
        .source()
        .node_run_id()
        .clone();
    let wiring_link_ids_at_the_source: HashSet<String> =
        reader_setup_command_ports(&graph, &source)["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|output_link| output_link["link_id"].as_str().unwrap().to_string())
            .collect();
    assert_eq!(
        wiring_link_ids_at_the_source.len(),
        2,
        "the source notifies each read under its own wiring identity"
    );

    unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
        .unwrap();

    assert_eq!(
        the_reads_of(&graph, &into_the_island),
        vec![reads[1].clone()]
    );
    assert_eq!(link_state_of(&graph, &into_the_island), LinkState::Wired);
}

#[test]
fn an_island_reading_a_class_node_that_feeds_two_input_ports_of_one_reader_is_read_at_each() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (remap, reader, into_the_island, into_in1) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let into_in2 = link(&mut graph, (&remap, "result"), (&reader, "in2"));
    assert_eq!(
        the_refusal_of_a_commit(&graph, &[], &[], std::slice::from_ref(&into_in2), &[]),
        None
    );
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();

    for island_link in [&into_in1, &into_in2] {
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            island_link,
            &iceoryx2_node,
        )
        .unwrap();
    }

    let reads = the_reads_of(&graph, &into_the_island);
    assert_eq!(reads.len(), 2);
    assert!(reads.iter().all(|read_at| read_at.node_run_id() == &reader));
    assert_ne!(
        reads[0].port_name(),
        reads[1].port_name(),
        "two islands of one reader never share a read"
    );
    let islands = reader_setup_command_ports(&graph, &reader)["islands"].clone();
    assert_eq!(islands.as_array().map(Vec::len), Some(2));

    close_the_channels_no_island_reads_any_more(&mut graph).unwrap();
    assert_eq!(the_reads_of(&graph, &into_the_island).len(), 2);
}

#[test]
fn a_class_node_link_wired_after_two_readers_opens_a_read_at_each() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (remap, reader, into_the_island, into_the_reader) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let second_reader = a_helper_reader_before_its_setup(&mut graph);
    let into_the_second_reader = link(&mut graph, (&remap, "result"), (&second_reader, "in1"));
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();

    wire_a_link_into_a_function_node_island_through_its_reader(
        &mut graph,
        &into_the_island,
        &iceoryx2_node,
    )
    .unwrap();
    for island_link in [&into_the_reader, &into_the_second_reader] {
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            island_link,
            &iceoryx2_node,
        )
        .unwrap();
    }

    let readers: HashSet<NodeRunId> = the_reads_of(&graph, &into_the_island)
        .iter()
        .map(|read_at| read_at.node_run_id().clone())
        .collect();
    assert_eq!(readers, HashSet::from([reader, second_reader]));
}

fn is_read_at_a_hidden_input_of(graph: &Graph, link_id: &LinkUniqueId) -> bool {
    !the_reads_of(graph, link_id).is_empty()
}

#[test]
fn removing_the_one_class_node_link_into_an_island_is_refused_when_it_leaves_its_reactive_reader_idle()
 {
    let mut graph = a_graph_resolving_three_function_node_types();
    let source = a_class_node_feeding(&mut graph);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup_driven_in(&mut graph, ProcessExecution::Reactive);
    let into_the_island = link(&mut graph, (&source, "out1"), (&remap, "x"));
    link(&mut graph, (&remap, "result"), (&reader, "in1"));

    let refusal = the_refusal_of_a_commit(
        &graph,
        &[],
        &[],
        &[],
        std::slice::from_ref(&into_the_island),
    )
    .expect("the reader's only wake source is the class node link");
    assert!(refusal.contains(&format!("leaves `{reader}`")), "{refusal}");
}

#[test]
fn unwiring_one_island_of_a_reader_keeps_the_class_node_link_another_of_its_islands_reads() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (_remap, reader, into_the_first_island, into_the_reader_at_in1) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let second_source = a_class_node_feeding(&mut graph);
    let second_remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let into_the_second_island = link(&mut graph, (&second_source, "out1"), (&second_remap, "x"));
    let into_the_reader_at_in2 = link(&mut graph, (&second_remap, "result"), (&reader, "in2"));
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    for island_link in [&into_the_reader_at_in1, &into_the_reader_at_in2] {
        wire_a_link_from_a_function_node_island_into_its_reader(
            &mut graph,
            island_link,
            &iceoryx2_node,
        )
        .unwrap();
    }

    unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader_at_in1)
        .unwrap();

    assert!(!is_read_at_a_hidden_input_of(
        &graph,
        &into_the_first_island
    ));
    assert!(is_read_at_a_hidden_input_of(
        &graph,
        &into_the_second_island
    ));
}

#[test]
fn a_class_node_link_into_a_function_node_that_joins_a_read_island_is_opened_at_its_reader() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &iceoryx2_node,
    )
    .unwrap();
    let source = a_class_node_feeding(&mut graph);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let into_the_reader_less_remap = link(&mut graph, (&source, "out1"), (&remap, "x"));
    wire_a_link_into_a_function_node_island_through_its_reader(
        &mut graph,
        &into_the_reader_less_remap,
        &iceoryx2_node,
    )
    .unwrap();
    assert!(!is_read_at_a_hidden_input_of(
        &graph,
        &into_the_reader_less_remap
    ));

    link(&mut graph, (&remap, "result"), (&mix, "a"));
    open_the_channels_each_island_newly_reads(&mut graph, &iceoryx2_node).unwrap();

    assert!(is_read_at_a_hidden_input_of(
        &graph,
        &into_the_reader_less_remap
    ));
}

#[test]
fn a_class_node_link_into_a_function_node_an_edit_takes_out_of_its_island_is_closed() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));
    let source = a_class_node_feeding(&mut graph);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let into_remap = link(&mut graph, (&source, "out1"), (&remap, "x"));
    let remap_into_mix = link(&mut graph, (&remap, "result"), (&mix, "a"));
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &iceoryx2_node,
    )
    .unwrap();
    assert!(is_read_at_a_hidden_input_of(&graph, &into_remap));

    graph.traversal_mut().e(&remap_into_mix).drop();
    close_the_channels_no_island_reads_any_more(&mut graph).unwrap();

    assert!(!is_read_at_a_hidden_input_of(&graph, &into_remap));
    assert_eq!(link_state_of(&graph, &into_remap), LinkState::Wired);
}

#[test]
fn a_class_node_link_its_island_still_reads_stays_open() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (_remap, _reader, into_the_island, into_the_reader) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();

    close_the_channels_no_island_reads_any_more(&mut graph).unwrap();

    assert!(is_read_at_a_hidden_input_of(&graph, &into_the_island));
}

/// A class node link read through the reader of the island it feeds, whose read that reader's
/// helper refused, is named among the links of the reader an end refused, though the reader is
/// no end of it, so the reader's host wires it again.
///
/// Mental-revert: naming only the links into or out of the reader leaves the read refused until
/// the stream starts again, while the reader's host asks for a retry that wires nothing.
#[test]
fn a_class_node_link_whose_read_its_islands_reader_refused_is_named_a_link_of_that_reader() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (_remap, reader, into_the_island, into_the_reader) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    let reader_side =
        crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
    graph
        .traversal()
        .v(&reader)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .map(|link_wiring| Arc::clone(&link_wiring.0))
        .expect("the reader is wired out of process")
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), reader_side.clone())
        .unwrap();
    let iceoryx2_node = Iceoryx2Node::for_this_test_process();
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &iceoryx2_node,
    )
    .unwrap();
    wire_a_link_into_a_function_node_island_through_its_reader(
        &mut graph,
        &into_the_island,
        &iceoryx2_node,
    )
    .unwrap();
    assert!(graph.the_links_of_a_node_an_end_refused(&reader).is_empty());

    for answer in reader_side.wire_answers_owed.lock().iter() {
        answer.note_the_far_sides_answer(
            crate::core::nodes::OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: "could not open the read".to_owned(),
            },
        );
    }

    assert!(
        graph
            .the_links_of_a_node_an_end_refused(&reader)
            .contains(&into_the_island),
        "{:?}",
        graph.the_links_of_a_node_an_end_refused(&reader)
    );
}
