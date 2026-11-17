// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
use crate::core::nodes::OutOfProcessLinkWireReply;

#[test]
fn a_link_nothing_has_wired_yet_is_planned() {
    let link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    assert!(!this_link_is_already_wired_or_awaiting_its_helpers_answer(
        &link
    ));
}

#[test]
fn a_wired_link_is_not_planned_again() {
    let mut link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    link.insert(LinkStateComponent(LinkState::Wired));
    assert!(this_link_is_already_wired_or_awaiting_its_helpers_answer(
        &link
    ));
}

/// Fail-without-fix: read `LinkStateComponent` alone and this link — the
/// ordinary state of a live `connect` onto a running helper — is planned
/// again on the next compile, wiring one link twice.
#[test]
fn a_link_a_helper_has_not_answered_for_is_not_planned_again() {
    let mut link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    link.insert(LinkStateComponent(LinkState::Pending));
    link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
        OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
    ]));
    assert!(this_link_is_already_wired_or_awaiting_its_helpers_answer(
        &link
    ));
}

/// A link a batch logs twice — a live connect and the start that commits
/// it — is planned once, in the order it was first logged.
///
/// Fail-without-fix: queue each `AddLink` as logged and both links are
/// planned twice, so the WIRE phase opens a second subscriber and notifier
/// for one link.
#[test]
fn a_link_queued_twice_in_one_batch_is_wired_once() {
    use crate::core::test_support::{add_mock_input_only, add_mock_output_only};

    let mut graph = Graph::new();
    let source = add_mock_output_only(&mut graph);
    let first_reader = add_mock_input_only(&mut graph);
    let second_reader = add_mock_input_only(&mut graph);
    let mut a_link_into = |reader: &str| {
        graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(source.as_str(), "out1"),
                InputLinkPortRef::new(reader, "in1"),
            )
            .first()
            .expect("the link is added")
            .id
            .clone()
    };
    let link_into_the_first_reader = a_link_into(first_reader.as_str());
    let link_into_the_second_reader = a_link_into(second_reader.as_str());

    let plan = the_plan_these_operations_make(
        &RwLock::new(graph),
        vec![
            PendingOperation::AddLink(link_into_the_first_reader.clone()),
            PendingOperation::AddLink(link_into_the_second_reader.clone()),
            PendingOperation::AddLink(link_into_the_first_reader.clone()),
            PendingOperation::AddLink(link_into_the_second_reader.clone()),
        ],
    );

    assert_eq!(
        plan.links_to_add,
        vec![link_into_the_first_reader, link_into_the_second_reader]
    );
}

/// A node a batch logs as removed and as torn down at a Stop, in either order, leaves once and
/// removed, so the project's store forgets its kept live values.
///
/// Fail-without-fix: let the last logged reason win and a Stop's teardown logged after a removal
/// keeps a removed node's kept values in the store.
#[test]
fn a_node_removed_and_torn_down_at_a_stop_in_one_batch_leaves_once_removed() {
    use crate::core::graph::WhyTheNodeLeaves;

    for reasons in [
        [WhyTheNodeLeaves::Removed, WhyTheNodeLeaves::TornDownAtAStop],
        [WhyTheNodeLeaves::TornDownAtAStop, WhyTheNodeLeaves::Removed],
    ] {
        let node_run_id = NodeRunId::new();

        let plan = the_plan_these_operations_make(
            &RwLock::new(Graph::new()),
            reasons
                .iter()
                .map(|why| PendingOperation::RemoveNode(node_run_id.clone(), *why))
                .collect(),
        );

        assert_eq!(
            plan.nodes_to_remove,
            vec![(node_run_id, WhyTheNodeLeaves::Removed)],
            "logged as {reasons:?}",
        );
    }
}
