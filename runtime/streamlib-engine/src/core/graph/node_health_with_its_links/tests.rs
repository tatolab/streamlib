// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticPlaceOnTheWire, NodeHealthOnTheWire,
};

use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, InputLinkPortRef, LinkUniqueId,
    NodeRunId, OutOfProcessLinkWireRepliesComponent, OutputLinkPortRef, StateComponent,
};
use crate::core::nodes::{NodeState, OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
use crate::core::test_support::{add_mock_input_only, add_mock_output_only};

/// A running source linked into a running reader, both healthy by their states.
fn a_source_linked_into_a_running_reader() -> (Graph, NodeRunId, LinkUniqueId) {
    let mut graph = Graph::new();
    let source = add_mock_output_only(&mut graph);
    let reader = add_mock_input_only(&mut graph);
    for node in [&source, &reader] {
        graph
            .traversal_mut()
            .v(NodeRunId::from(node.as_str()))
            .first_mut()
            .expect("the node is held")
            .insert(StateComponent::new(NodeState::Running));
    }
    let link = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(source.as_str(), "out1"),
            InputLinkPortRef::new(reader.as_str(), "in1"),
        )
        .first()
        .expect("the link is added")
        .id
        .clone();
    (graph, NodeRunId::from(reader.as_str()), link)
}

fn the_health_and_diagnostics_of_the_reader(
    graph: &Graph,
    reader: &NodeRunId,
) -> (
    NodeHealthOnTheWire,
    Vec<streamlib_runtime_client_contract::local_api_wire_contract::DiagnosticOnTheWire>,
) {
    let node = graph
        .traversal()
        .v(reader)
        .first()
        .expect("the reader is held");
    graph
        .the_health_and_diagnostics_of(node)
        .expect("the reader keeps a state")
}

#[test]
fn a_reader_whose_links_wire_reads_as_its_state_says() {
    let (graph, reader, _link) = a_source_linked_into_a_running_reader();
    assert_eq!(
        the_health_and_diagnostics_of_the_reader(&graph, &reader),
        (NodeHealthOnTheWire::Healthy, Vec::new())
    );
}

/// Mental-revert: health read from the reader's state alone answers healthy, while its input
/// reads nothing.
#[test]
fn a_reader_whose_link_its_helper_refused_reads_not_ready_with_the_refusal_at_the_link() {
    let (mut graph, reader, link) = a_source_linked_into_a_running_reader();
    let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
    reply.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
        reason: "Failed to create subscriber: BufferSizeExceedsMaxSupportedBufferSizeOfService"
            .to_owned(),
    });
    graph
        .traversal_mut()
        .e(&link)
        .first_mut()
        .expect("the link is held")
        .insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
            Arc::clone(&reply),
        ]));

    let (health, diagnostics) = the_health_and_diagnostics_of_the_reader(&graph, &reader);

    assert_eq!(health, NodeHealthOnTheWire::NotReady);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.kind, DiagnosticKindOnTheWire::LinkNotWired);
    assert!(
        matches!(
            &diagnostic.place,
            DiagnosticPlaceOnTheWire::Link { from_port, to_port, .. }
                if from_port == "out1" && to_port == "in1"
        ),
        "{:?}",
        diagnostic.place
    );
    assert!(
        diagnostic
            .words
            .contains("BufferSizeExceedsMaxSupportedBufferSizeOfService"),
        "{}",
        diagnostic.words
    );
}

#[test]
fn a_failed_reader_with_a_link_not_wired_stays_failed() {
    let (mut graph, reader, link) = a_source_linked_into_a_running_reader();
    graph
        .traversal_mut()
        .e(&link)
        .first_mut()
        .expect("the link is held")
        .insert(crate::core::graph::LinkStateComponent(
            crate::core::graph::LinkState::Error,
        ));
    graph
        .traversal()
        .v(&reader)
        .first()
        .and_then(|node| node.get::<StateComponent>())
        .expect("the reader keeps a state")
        .fail_with("setup failed: no device");

    let (health, diagnostics) = the_health_and_diagnostics_of_the_reader(&graph, &reader);

    assert_eq!(health, NodeHealthOnTheWire::Failed);
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.kind)
            .collect::<Vec<_>>(),
        [
            DiagnosticKindOnTheWire::NodeFailed,
            DiagnosticKindOnTheWire::LinkNotWired
        ]
    );
}

/// A link whose two ends no longer carry types that connect is reported as its own kind, which
/// the link is fixed to recover from, naming both pin types.
///
/// Mental-revert: reporting it as a link not wired names restarting the stream as the recovery,
/// which wires it again into the input that does not read its type.
#[test]
fn a_reader_whose_link_carries_a_type_its_input_does_not_read_reads_the_pin_types_differing() {
    let (mut graph, reader, link) = a_source_linked_into_a_running_reader();
    graph
        .traversal_mut()
        .e(&link)
        .first_mut()
        .expect("the link is held")
        .insert_component_without_rendering_it(crate::core::graph::LinkPinTypesDifferComponent {
            output_pin_type: "Audio".to_owned(),
            input_pin_type: "String".to_owned(),
            why: "Audio does not connect to String".to_owned(),
        });

    let (health, diagnostics) = the_health_and_diagnostics_of_the_reader(&graph, &reader);

    assert_eq!(health, NodeHealthOnTheWire::NotReady);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.kind, DiagnosticKindOnTheWire::LinkPinTypesDiffer);
    assert_eq!(
        diagnostic.recovery,
        streamlib_runtime_client_contract::local_api_wire_contract::DiagnosticRecoveryOnTheWire::FixTheLink
    );
    assert!(
        diagnostic.words.contains("carries Audio") && diagnostic.words.contains("reads String"),
        "{}",
        diagnostic.words
    );
}
