// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::links_an_end_refused_wired_again::unwire_each_link_an_end_refused_keeping_it;
use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
use crate::core::nodes::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
use crate::core::test_support::{add_mock_input_only, add_mock_output_only};

/// A link from a mock source into a mock reader, holding the answer `outcome` of the reader's
/// far side, or none yet.
fn a_link_answered(
    graph: &mut Graph,
    source: &str,
    reader: &str,
    outcome: Option<OutOfProcessLinkWireOutcome>,
) -> LinkUniqueId {
    let link_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(source, "out1"),
            InputLinkPortRef::new(reader, "in1"),
        )
        .first()
        .expect("the link is added")
        .id
        .clone();
    let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
    if let Some(outcome) = outcome {
        reply.note_the_far_sides_answer(outcome);
    }
    let link = graph
        .traversal_mut()
        .e(&link_id)
        .first_mut()
        .expect("the link is held");
    link.insert(LinkStateComponent(LinkState::Pending));
    link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![reply]));
    link_id
}

fn refused_by_the_far_side() -> Option<OutOfProcessLinkWireOutcome> {
    Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
        reason: "the helper process this link was handed to died before answering".to_owned(),
    })
}

/// An add of a link an end refused plans it to be closed and wired again, where an add of one
/// still awaiting its answer passes it over.
///
/// Mental-revert: reading a refused link as one awaiting its helper's answer passes it over, so
/// it carries nothing until the stream starts again.
#[test]
fn an_add_of_a_link_an_end_refused_wires_it_again_and_one_still_awaited_is_passed_over() {
    let mut graph = Graph::new();
    let source = add_mock_output_only(&mut graph);
    let reader = add_mock_input_only(&mut graph);
    let refused = a_link_answered(&mut graph, &source, &reader, refused_by_the_far_side());
    let awaited = a_link_answered(&mut graph, &source, &reader, None);

    let plan = the_plan_these_operations_make(
        &RwLock::new(graph),
        vec![
            PendingOperation::AddLink(refused.clone()),
            PendingOperation::AddLink(awaited),
            PendingOperation::AddLink(refused.clone()),
        ],
    );

    assert_eq!(plan.links_to_add, vec![refused.clone()]);
    assert_eq!(plan.links_to_wire_again, vec![refused]);
}

/// A node's links an end refused are logged as adds, and a node with none logs nothing, so its
/// helper asking after every restart commits nothing then.
///
/// Mental-revert: logging every link of the node rewires links already wired.
#[test]
fn a_node_logs_an_add_of_each_link_an_end_refused_and_a_node_with_none_logs_nothing() {
    let compiler = Compiler::new();
    let (reader, refused, other_reader) = compiler.scope(|graph, _tx| {
        let source = add_mock_output_only(graph);
        let reader = add_mock_input_only(graph);
        let other_reader = add_mock_input_only(graph);
        let refused = a_link_answered(graph, &source, &reader, refused_by_the_far_side());
        a_link_answered(
            graph,
            &source,
            &reader,
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide),
        );
        a_link_answered(
            graph,
            &source,
            &other_reader,
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide),
        );
        (
            NodeRunId::from(reader.as_str()),
            refused,
            NodeRunId::from(other_reader.as_str()),
        )
    });

    assert!(!compiler.log_each_link_of_a_node_an_end_refused_to_wire_it_again(&other_reader));
    assert!(compiler.logged_pending_operations().is_empty());

    assert!(compiler.log_each_link_of_a_node_an_end_refused_to_wire_it_again(&reader));
    let logged = compiler.logged_pending_operations();
    assert!(
        matches!(logged.as_slice(), [PendingOperation::AddLink(logged_link)] if *logged_link == refused),
        "{logged:?}"
    );
}

/// A node of `node_type` whose far side is past its setup command, handing what it is handed to
/// the delivery answered.
fn a_helper_past_its_setup(
    graph: &mut Graph,
    node_type: crate::core::descriptors::NodeClassImportPath,
) -> (
    NodeRunId,
    crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery,
) {
    crate::core::test_support::ensure_test_mocks_registered();
    let node_run_id = graph
        .traversal_mut()
        .add_v(crate::core::nodes::NodeSpec::new(
            node_type,
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("the mock is registered")
        .id
        .clone();
    let delivery = crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery::default();
    let link_wiring = Arc::new(
        crate::core::nodes::OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
            crate::core::execution::ProcessExecution::Reactive,
        ),
    );
    link_wiring
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .expect("the setup command goes out");
    graph
        .traversal_mut()
        .v(&node_run_id)
        .first_mut()
        .expect("the node is held")
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(link_wiring));
    (node_run_id, delivery)
}

/// A link between two helpers that the reader's helper refused is closed on both ends, kept,
/// and handed to both helpers again with answers of its own by the compile an add of it makes,
/// and it reads wired once both open it, with no stop of the stream.
///
/// Mental-revert: wiring it again without closing it first hands both helpers the link again
/// while each still holds the port it opened or refused for it, and tells neither to drop it.
#[test]
fn a_link_its_reader_refused_is_closed_and_wired_again_and_reads_wired_once_both_ends_open_it() {
    use crate::core::json_schema::why_the_link_is_not_wired;
    use crate::core::test_support::{MockInputOnlyNode, MockOutputOnlyNode};

    let iceoryx2_node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let (source, source_side) =
        a_helper_past_its_setup(&mut graph, MockOutputOnlyNode::node_class_import_path());
    let (reader, reader_side) =
        a_helper_past_its_setup(&mut graph, MockInputOnlyNode::node_class_import_path());
    let link_id = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(source.as_str(), "out1"),
            InputLinkPortRef::new(reader.as_str(), "in1"),
        )
        .first()
        .expect("the link is added")
        .id
        .clone();
    super::super::compiler_ops::open_iceoryx2_service(&mut graph, &link_id, &iceoryx2_node)
        .expect("the link is handed to both helpers");
    source_side.wire_answers_owed.lock()[0]
        .note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    reader_side.wire_answers_owed.lock()[0]
        .note_the_far_sides_answer(refused_by_the_far_side().expect("a refusal"));
    let is_not_wired = |graph: &Graph| {
        why_the_link_is_not_wired(graph.traversal().e(&link_id).first().expect("held")).is_some()
    };
    let rendered_state = |graph: &Graph| {
        crate::core::json_schema::rendered_link(
            graph.traversal().e(&link_id).first().expect("held"),
            &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
            None,
        )
        .state
    };
    assert!(is_not_wired(&graph));

    let graph = RwLock::new(graph);
    let plan =
        the_plan_these_operations_make(&graph, vec![PendingOperation::AddLink(link_id.clone())]);
    let mut graph = graph.into_inner();
    unwire_each_link_an_end_refused_keeping_it(&mut graph, &plan, &[])
        .expect("the refused link is closed");
    for link_to_add in &plan.links_to_add {
        super::super::compiler_ops::open_iceoryx2_service(&mut graph, link_to_add, &iceoryx2_node)
            .expect("the refused link is handed over again");
    }

    let told_to_drop =
        |side: &crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery| {
            side.reclaimed_links
                .lock()
                .iter()
                .filter(|reclaimed| reclaimed.link_id == link_id.as_str())
                .count()
        };
    assert_eq!(
        (told_to_drop(&source_side), told_to_drop(&reader_side)),
        (1, 1)
    );
    let answers_owed =
        |side: &crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery| {
            side.wire_answers_owed.lock().clone()
        };
    let (source_answers, reader_answers) = (answers_owed(&source_side), answers_owed(&reader_side));
    assert_eq!((source_answers.len(), reader_answers.len()), (2, 2));
    assert_eq!(
        (is_not_wired(&graph), rendered_state(&graph)),
        (false, crate::core::json_schema::LinkStateOutput::Pending),
        "a link handed over again awaits its new answers"
    );
    for answer in [&source_answers[1], &reader_answers[1]] {
        answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    }
    assert_eq!(
        rendered_state(&graph),
        crate::core::json_schema::LinkStateOutput::Wired
    );
}
