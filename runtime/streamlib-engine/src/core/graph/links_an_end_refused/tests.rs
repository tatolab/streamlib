// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{
    Graph, GraphEdgeWithComponents, InputLinkPortRef, LinkState, LinkStateComponent, LinkUniqueId,
    NodeRunId, OutOfProcessLinkWireRepliesComponent, OutputLinkPortRef,
};
use crate::core::nodes::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
use crate::core::test_support::{
    MockNode, add_mock_input_only, add_mock_output_only, ensure_test_mocks_registered,
};

fn add_a_node_with_an_input_and_an_output(graph: &mut Graph) -> String {
    ensure_test_mocks_registered();
    graph
        .traversal_mut()
        .add_v(crate::core::nodes::NodeSpec::new(
            MockNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("the mock node is registered")
        .id
        .to_string()
}

fn a_link(graph: &mut Graph, from: &str, to: &str) -> LinkUniqueId {
    graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(from, "out1"),
            InputLinkPortRef::new(to, "in1"),
        )
        .first()
        .expect("the link is added")
        .id
        .clone()
}

fn answered(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    outcome: Option<OutOfProcessLinkWireOutcome>,
) {
    let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
    if let Some(outcome) = outcome {
        reply.note_the_far_sides_answer(outcome);
    }
    graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .expect("the link is held")
        .insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![reply]));
}

fn refused() -> Option<OutOfProcessLinkWireOutcome> {
    Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
        reason: "the helper process this link was handed to died before answering".to_owned(),
    })
}

/// Only a link that reads as refused by an end is named, into the node or out of it; one still
/// awaiting its answer, one opened, one wired in this process and one a later wiring stamped
/// wired over an old refusal are not.
///
/// Mental-revert: naming every link holding answers names a link still awaiting one, which a
/// second wiring would open twice; reading the answers past the stamp closes and opens again a
/// link that is wired, at every restart of its helper.
#[test]
fn a_node_names_each_link_into_or_out_of_it_an_end_refused_and_no_other() {
    let mut graph = Graph::new();
    let source = add_mock_output_only(&mut graph);
    let node = add_a_node_with_an_input_and_an_output(&mut graph);
    let reader = add_mock_input_only(&mut graph);
    let refused_into_it = a_link(&mut graph, &source, &node);
    let awaited_into_it = a_link(&mut graph, &source, &node);
    let refused_out_of_it = a_link(&mut graph, &node, &reader);
    let opened_out_of_it = a_link(&mut graph, &node, &reader);
    let _wired_in_this_process = a_link(&mut graph, &node, &reader);
    let refused_then_stamped_wired = a_link(&mut graph, &node, &reader);
    answered(&mut graph, &refused_into_it, refused());
    answered(&mut graph, &refused_then_stamped_wired, refused());
    graph
        .traversal_mut()
        .e(&refused_then_stamped_wired)
        .first_mut()
        .expect("the link is held")
        .insert(LinkStateComponent(LinkState::Wired));
    answered(&mut graph, &awaited_into_it, None);
    answered(&mut graph, &refused_out_of_it, refused());
    answered(
        &mut graph,
        &opened_out_of_it,
        Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide),
    );

    let named = graph.the_links_of_a_node_an_end_refused(&NodeRunId::from(node.as_str()));

    assert_eq!(named, vec![refused_into_it, refused_out_of_it]);
}
