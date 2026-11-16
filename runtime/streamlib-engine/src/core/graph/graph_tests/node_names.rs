// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::error::Error;

/// Read every node's name in node-iteration order.
fn node_names_in_the_graph(graph: &Graph) -> Vec<String> {
    graph
        .traversal()
        .v(())
        .iter()
        .map(|node| node.display_name.clone())
        .collect()
}

fn add_a_mock_node_named(graph: &mut Graph, name: Option<&str>) -> crate::core::Result<()> {
    let spec = MockNode::Node::node(Default::default());
    let spec = match name {
        Some(name) => spec.with_display_name(name),
        None => spec,
    };
    graph.traversal_mut().add_v(spec).map(|_| ())
}

/// A defaulted name is the class's short name, cast, and each defaulted
/// duplicate takes the next free `-2`, `-3` … — the one string the handle,
/// `graph`, the port address and the log prefix all show.
#[test]
fn a_defaulted_name_is_the_cast_short_name_and_a_duplicate_takes_the_next_suffix() {
    let mut graph = test_graph();

    for _ in 0..4 {
        add_a_mock_node_named(&mut graph, None).expect("a defaulted name is never refused");
    }

    assert_eq!(
        node_names_in_the_graph(&graph),
        vec!["mocknode", "mocknode-2", "mocknode-3", "mocknode-4"]
    );
}

/// A name the author typed is cast, and a second node whose typed name
/// casts alike is refused by name rather than suffixed — a typed name is
/// an address.
#[test]
fn a_typed_name_is_cast_and_a_typed_duplicate_is_refused_naming_both() {
    let mut graph = test_graph();

    add_a_mock_node_named(&mut graph, Some("Front Camera")).expect("a free name");
    let refusal = add_a_mock_node_named(&mut graph, Some("front-CAMERA"));

    match refusal {
        Err(Error::NodeNameTaken { name, cast }) => {
            assert_eq!(name, "front-CAMERA");
            assert_eq!(cast, "front-camera");
        }
        other => panic!("expected NodeNameTaken, got {other:?}"),
    }
    assert_eq!(node_names_in_the_graph(&graph), vec!["front-camera"]);
}

/// A typed name that casts to nothing is refused by name.
#[test]
fn a_typed_name_that_casts_to_nothing_is_refused() {
    let mut graph = test_graph();

    let refusal = add_a_mock_node_named(&mut graph, Some("✨"));

    assert!(
        matches!(refusal, Err(Error::ExposedNameCastsToNothing(_))),
        "got {refusal:?}"
    );
    assert!(node_names_in_the_graph(&graph).is_empty());
}

/// The suffix search skips a name an author already took, rather than
/// minting a second `-2`.
#[test]
fn a_taken_suffix_is_skipped_rather_than_duplicated() {
    let mut graph = test_graph();

    add_a_mock_node_named(&mut graph, Some("mocknode-2")).expect("a free name");
    add_a_mock_node_named(&mut graph, None).expect("defaulted");
    add_a_mock_node_named(&mut graph, None).expect("defaulted");

    assert_eq!(
        node_names_in_the_graph(&graph),
        vec!["mocknode-2", "mocknode", "mocknode-3"]
    );
}

/// A suffix is fitted by cutting the name before it, so a defaulted
/// duplicate of a name already at the bound stays within it. An
/// unregistered type defaults to its import path, which is long enough.
#[test]
fn a_suffix_on_a_name_at_the_bound_cuts_the_name_to_fit() {
    let mut graph = test_graph();
    let a_long_import_path = crate::core::descriptors::NodeClassImportPath::new(format!(
        "{}:Node",
        "m".repeat(crate::core::graph::EXPOSED_NAME_MAXIMUM_LENGTH)
    ))
    .expect("an import path");

    for _ in 0..2 {
        graph
            .traversal_mut()
            .add_v(crate::core::nodes::NodeSpec::new(
                a_long_import_path.clone(),
                serde_json::json!({}),
            ))
            .expect("a defaulted name is never refused");
    }

    let names = node_names_in_the_graph(&graph);
    assert_eq!(
        names[0],
        "m".repeat(crate::core::graph::EXPOSED_NAME_MAXIMUM_LENGTH)
    );
    assert_eq!(
        names[1],
        format!(
            "{}-2",
            "m".repeat(crate::core::graph::EXPOSED_NAME_MAXIMUM_LENGTH - 2)
        )
    );
}

/// Nodes of different types never collide, so neither is suffixed.
#[test]
fn distinct_names_are_left_alone() {
    let mut graph = test_graph();

    add_a_mock_node_named(&mut graph, None).expect("defaulted");
    graph
        .traversal_mut()
        .add_v(MockOutputOnlyNode::Node::node(Default::default()))
        .expect("the node is named");

    assert_eq!(
        node_names_in_the_graph(&graph),
        vec!["mocknode", "mockoutputonlynode"]
    );
}

/// Identity never derives from the name: the suffix reaches the name and
/// nothing else on the node.
#[test]
fn the_suffix_never_leaks_into_the_node_type_or_id() {
    let mut graph = test_graph();

    add_a_mock_node_named(&mut graph, None).expect("defaulted");
    add_a_mock_node_named(&mut graph, None).expect("defaulted");

    for node in graph.traversal().v(()).iter() {
        assert_eq!(node.node_type, MockNode::Node::node_class_import_path());
        assert!(
            !node.id.to_string().contains("mocknode"),
            "the minted id must not carry the name, got {}",
            node.id
        );
    }
}
