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

/// The stream a fixture compile's events publish under.
fn the_fixtures_stream() -> crate::core::pubsub::LoadedStreamIdentity {
    crate::core::pubsub::LoadedStreamIdentity {
        runtime_id: "refused-link-wired-again-test".to_string(),
        stream_id: "main".to_string(),
        stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
    }
}

/// A link between two helpers past their setup that the reader's helper refused, and the
/// compile an add of it plans, the link already closed as that compile closes it before its
/// wiring.
struct ALinkItsReaderRefused {
    graph: RwLock<Graph>,
    link_id: LinkUniqueId,
    reader: NodeRunId,
    source_side: crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery,
    reader_side: crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery,
    plan: CompilationPlan,
    iceoryx2_node: crate::iceoryx2::Iceoryx2Node,
}

fn a_link_its_reader_refused_closed_to_be_wired_again() -> ALinkItsReaderRefused {
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
    assert!(
        why_the_link_is_not_wired(graph.traversal().e(&link_id).first().expect("held")).is_some()
    );

    let graph = RwLock::new(graph);
    let plan =
        the_plan_these_operations_make(&graph, vec![PendingOperation::AddLink(link_id.clone())]);
    unwire_each_link_an_end_refused_keeping_it(&mut graph.write(), &plan, &[])
        .expect("the refused link is closed");
    assert_eq!(
        graph.read().the_links_of_a_node_an_end_refused(&reader),
        vec![link_id.clone()],
        "a closed link still reads refused until it is handed over again"
    );
    ALinkItsReaderRefused {
        graph,
        link_id,
        reader,
        source_side,
        reader_side,
        plan,
        iceoryx2_node,
    }
}

impl ALinkItsReaderRefused {
    fn wire_the_links_the_plan_adds(&self) -> usize {
        super::links_a_plan_adds_wired::wire_every_link_a_plan_adds(
            &self.graph,
            &self.plan.links_to_add,
            &self.plan.links_to_wire_again,
            &self.iceoryx2_node,
            &the_fixtures_stream(),
        )
        .expect("a link an end refused never fails the compile")
    }

    fn rendered(&self) -> crate::core::json_schema::LinkOutput {
        crate::core::json_schema::rendered_link(
            self.graph
                .read()
                .traversal()
                .e(&self.link_id)
                .first()
                .expect("held"),
            &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
            None,
        )
    }
}

/// A link between two helpers that the reader's helper refused is closed on both ends, kept,
/// and handed to both helpers again with answers of its own by the wiring of the compile an add
/// of it makes, and it reads wired once both open it, with no stop of the stream.
///
/// Mental-revert: wiring it again without closing it first hands both helpers the link again
/// while each still holds the port it opened or refused for it, and tells neither to drop it.
#[test]
fn a_link_its_reader_refused_is_closed_and_wired_again_and_reads_wired_once_both_ends_open_it() {
    let refused = a_link_its_reader_refused_closed_to_be_wired_again();

    assert_eq!(refused.wire_the_links_the_plan_adds(), 1);

    let told_to_drop =
        |side: &crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery| {
            side.reclaimed_links
                .lock()
                .iter()
                .filter(|reclaimed| reclaimed.link_id == refused.link_id.as_str())
                .count()
        };
    assert_eq!(
        (
            told_to_drop(&refused.source_side),
            told_to_drop(&refused.reader_side)
        ),
        (1, 1)
    );
    let answers_owed =
        |side: &crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery| {
            side.wire_answers_owed.lock().clone()
        };
    let (source_answers, reader_answers) = (
        answers_owed(&refused.source_side),
        answers_owed(&refused.reader_side),
    );
    assert_eq!((source_answers.len(), reader_answers.len()), (2, 2));
    assert_eq!(
        refused.rendered().state,
        crate::core::json_schema::LinkStateOutput::Pending,
        "a link handed over again awaits its new answers"
    );
    for answer in [&source_answers[1], &reader_answers[1]] {
        answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    }
    assert_eq!(
        refused.rendered().state,
        crate::core::json_schema::LinkStateOutput::Wired
    );
}

/// A link an end refused whose handover fails when it is wired again stays refused in the
/// failure's words, and the compile goes on, so the edits it carries land and a later retry
/// wires the link.
///
/// Mental-revert: passing the failure up fails the whole commit with every edit in it; leaving
/// the link closed as it was names it to no later retry.
#[test]
fn a_link_an_end_refused_whose_handover_fails_again_stays_refused_and_fails_no_compile() {
    let refused = a_link_its_reader_refused_closed_to_be_wired_again();
    refused
        .reader_side
        .refuses_links
        .store(true, std::sync::atomic::Ordering::SeqCst);

    assert_eq!(refused.wire_the_links_the_plan_adds(), 0);

    let rendered = refused.rendered();
    assert_eq!(
        rendered.state,
        crate::core::json_schema::LinkStateOutput::Error
    );
    assert!(
        rendered
            .error_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("could not be written to the far side")),
        "{rendered:?}"
    );
    assert_eq!(
        refused
            .graph
            .read()
            .the_links_of_a_node_an_end_refused(&refused.reader),
        vec![refused.link_id.clone()]
    );
}
