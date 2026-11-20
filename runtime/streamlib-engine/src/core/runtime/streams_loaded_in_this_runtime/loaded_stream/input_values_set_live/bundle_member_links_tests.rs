// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A bundle input with one member wired, one set live and one saved: `graph` renders each member
//! from where it comes, each member's notification equals its row, and the island reading the
//! input is handed the members set beside the members its links carry.

use serde_json::{Value, json};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    BundleMemberValueReadOnTheWire, InputValueComesFromOnTheWire,
};

use super::super::live_graph_edits::set_the_saved_value_and_hand_it_over;
use super::bundle_member_sets_tests::{
    BUNDLE_READER, a_bundle_reader_with_a_saved_reading, a_graph_resolving_a_bundle_reader,
    a_member_set, the_change_of_the_row, the_member_rows, the_values_set_live,
};
use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_class_node_feeding, a_helper_reader_before_its_setup, add_a_node, link,
    reader_setup_command_ports,
};
use crate::core::compiler::compiler_ops::function_node_island::{
    refuse_a_link_an_island_cannot_take, wire_a_link_from_a_function_node_island_into_its_reader,
};
use crate::core::graph::input_values::the_input_value_change;
use crate::core::graph::{InputLinkPortRef, LinkUniqueId, OutputLinkPortRef};
use crate::iceoryx2::Iceoryx2Node;

/// A link from `out1` of `source` into member `member` of input `reading` of `reader`.
fn a_link_into_the_member(
    graph: &mut Graph,
    source: &NodeRunId,
    reader: &NodeRunId,
    member: &str,
) -> LinkUniqueId {
    graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(source, "out1"),
            InputLinkPortRef::new(reader, "reading").at_member(Some(member.to_owned())),
        )
        .first()
        .unwrap()
        .id
        .clone()
}

fn a_row(
    member: &str,
    value: Option<Value>,
    value_comes_from: InputValueComesFromOnTheWire,
) -> BundleMemberValueReadOnTheWire {
    BundleMemberValueReadOnTheWire {
        member: member.to_owned(),
        value,
        value_comes_from: Some(value_comes_from),
    }
}

/// Mental-revert: a row that reads `wire` only while a link feeds the whole input renders the
/// wired `level` as the saved 0.2.
#[test]
fn one_member_wired_one_live_and_one_saved_each_read_from_where_it_comes() {
    let (graph, reader, reader_uuid) = a_bundle_reader_with_a_saved_reading();
    {
        let mut graph = graph.write();
        let source = a_class_node_feeding(&mut graph);
        a_link_into_the_member(&mut graph, &source, &reader, "level");
    }
    the_values_set_live(
        &graph,
        &[a_member_set(
            reader_uuid,
            "reading",
            Some("gain"),
            Some(json!(2.0)),
        )],
    )
    .unwrap()
    .expect("a bundle input of a function node is set apart from the graph");

    let graph = graph.read();
    let rows = the_member_rows(&graph);
    assert_eq!(
        rows,
        vec![
            a_row("level", None, InputValueComesFromOnTheWire::Wire),
            a_row(
                "label",
                Some(json!("kitchen")),
                InputValueComesFromOnTheWire::Saved
            ),
            a_row("gain", Some(json!(2.0)), InputValueComesFromOnTheWire::Live),
        ]
    );
    let node = graph.node_by_uuid(reader_uuid).unwrap();
    assert_eq!(
        the_input_value_change(&graph, node, "reading", None),
        rows.iter()
            .map(|row| the_change_of_the_row(reader_uuid, row))
            .collect::<Vec<_>>()
    );
}

/// The island reading a bundle input fed member by member is handed the input's saved members
/// as its pin, and reads the members its links carry at one input port of its reader, where
/// each link is wired to put its member into what that port reads.
///
/// Mental-revert: reading each member link at a port of its own hands the island two values for
/// one input, and wiring the links with no member to put them into hands it a bare Float.
#[test]
fn the_island_reads_every_member_link_of_one_input_at_one_port_over_its_pin() {
    let mut graph = a_graph_resolving_a_bundle_reader();
    let bundle_reader = add_a_node(&mut graph, BUNDLE_READER, Value::Null);
    let helper_reader = a_helper_reader_before_its_setup(&mut graph);
    let source = a_class_node_feeding(&mut graph);
    a_link_into_the_member(&mut graph, &source, &bundle_reader, "level");
    a_link_into_the_member(&mut graph, &source, &bundle_reader, "gain");
    let into_the_reader = link(
        &mut graph,
        (&bundle_reader, "result"),
        (&helper_reader, "in1"),
    );
    let graph = RwLock::new(graph);
    set_the_saved_value_and_hand_it_over(
        &graph,
        &bundle_reader,
        "reading",
        None,
        Some(json!({ "level": 0.2, "label": "kitchen" })),
    )
    .unwrap();
    let mut graph = graph.into_inner();

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();

    let ports = reader_setup_command_ports(&graph, &helper_reader);
    let island_node = &ports["islands"][0]["nodes"][0];
    assert_eq!(
        island_node["pins"]["reading"],
        json!({ "level": 0.2, "label": "kitchen" })
    );
    let links_into = island_node["links_into"].as_array().unwrap();
    assert_eq!(links_into.len(), 1, "{links_into:?}");
    assert_eq!(links_into[0]["port"], "reading");
    assert_eq!(links_into[0]["from"]["reads_members_over_its_pin"], true);
    let reader_input_port = links_into[0]["from"]["reader_input_port"].clone();
    let mut members_put: Vec<(Value, Value)> = ports["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|input| input["name"] == reader_input_port)
        .map(|input| {
            let put = &input["member_put_at_the_read"];
            (put["member"]["name"].clone(), put["put_into"].clone())
        })
        .collect();
    members_put.sort_by_key(|(member, _)| member.to_string());
    assert_eq!(
        members_put,
        vec![
            (json!("gain"), json!("the_members_the_links_carried")),
            (json!("level"), json!("the_members_the_links_carried")),
        ]
    );
}

/// A function node hands the input it feeds one whole value inside its island, with no message
/// to build a bundle from, so a link from one ending at a member is refused; the same member fed
/// from a node outside any island is not.
#[test]
fn a_link_from_a_function_node_into_a_member_is_refused() {
    let mut graph = a_graph_resolving_a_bundle_reader();
    let upstream = add_a_node(&mut graph, BUNDLE_READER, Value::Null);
    let bundle_reader = add_a_node(&mut graph, BUNDLE_READER, Value::Null);
    let source = a_class_node_feeding(&mut graph);
    let into_the_member =
        InputLinkPortRef::new(&bundle_reader, "reading").at_member(Some("level".to_owned()));

    let refused = refuse_a_link_an_island_cannot_take(
        &graph,
        &OutputLinkPortRef::new(&upstream, "result"),
        &into_the_member,
        None,
    )
    .expect_err("a function node feeds an input whole");

    assert!(
        refused.to_string().contains("starts at a function node"),
        "{refused}"
    );
    refuse_a_link_an_island_cannot_take(
        &graph,
        &OutputLinkPortRef::new(&source, "out1"),
        &into_the_member,
        None,
    )
    .expect("a node outside any island feeds the member through its messages");
}
