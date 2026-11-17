// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link's state while helpers answer for it, the depth its channel is created at, and wiring beside a helper still setting up.

use super::*;

fn add_mock_ordered_input_only(graph: &mut Graph) -> String {
    crate::core::test_support::ensure_test_mocks_registered();
    graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            crate::core::test_support::MockOrderedInputOnlyNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("mock_ordered_input_only_node must be in the registry")
        .id
        .to_string()
}

fn graph_with_one_wired_link_between_two_helper_stubs() -> (Graph, String, LinkUniqueId) {
    let (graph, source_id, link_id, _) = graph_and_owed_answers_of_a_helper_to_helper_link();
    (graph, source_id, link_id)
}

/// The same graph, with the answers both helper ends still owe — which is
/// what a test plays a helper with, since the op does not wait for them.
fn graph_and_owed_answers_of_a_helper_to_helper_link() -> (
    Graph,
    String,
    LinkUniqueId,
    Vec<Arc<OutOfProcessLinkWireReply>>,
) {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);
    let answers_owed: Arc<Mutex<Vec<Arc<OutOfProcessLinkWireReply>>>> = Arc::default();
    for helper_id in [&source_id, &dest_id] {
        attach_node_instance(
            &mut graph,
            helper_id,
            NodeInstance::new(Box::new(
                OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                    RecordingOutOfProcessFarSideLinkDelivery {
                        wire_answers_owed: answers_owed.clone(),
                        ..Default::default()
                    },
                    ProcessExecution::Reactive,
                ),
            )),
        );
    }
    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
    open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
        .expect("the helper-to-helper link wires");
    let answers_owed = answers_owed.lock().clone();
    (graph, source_id, link_id, answers_owed)
}

/// A running output port that already feeds a `newest` consumer takes an
/// `ordered` one, and each reads at its own port's depth.
///
/// Ten bags published while neither reads: the `newest` port's ring holds
/// four, so its mailbox takes four and evicts none, while the `ordered` port
/// receives all ten. Fail-without-fix: restore the one-profile-per-channel
/// refusal and the second `open_iceoryx2_service` is refused; give every
/// subscriber the service's depth and the `newest` port's mailbox evicts six.
#[test]
fn a_newest_and_an_ordered_consumer_share_one_running_output_port_each_at_its_own_depth() {
    use crate::core::test_support::{
        MockInputOnlyNode, MockOrderedInputOnlyNode, MockOutputOnlyNode,
    };
    const BAGS_PUBLISHED_WHILE_NEITHER_CONSUMER_READS: usize = 10;

    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let (_, source_output, _) =
        attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let source_output = source_output.expect("an output-only mock holds an output writer");
    let newest_consumer_id = add_mock_input_only(&mut graph);
    let (_, _, newest_consumer_input) =
        attach_mock_instance::<MockInputOnlyNode::Node>(&mut graph, &newest_consumer_id);
    let newest_consumer_input =
        newest_consumer_input.expect("an input-only mock holds input mailboxes");
    let ordered_consumer_id = add_mock_ordered_input_only(&mut graph);
    let (_, _, ordered_consumer_input) =
        attach_mock_instance::<MockOrderedInputOnlyNode::Node>(&mut graph, &ordered_consumer_id);
    let ordered_consumer_input =
        ordered_consumer_input.expect("an input-only mock holds input mailboxes");

    let newest_link = add_link_from_out1_to_in1(&mut graph, &source_id, &newest_consumer_id);
    open_iceoryx2_service(&mut graph, &newest_link, &node).expect("the newest consumer wires");
    source_output
        .write_bytes_with_timestamp(
            "out1",
            b"a bag the port carried before the ordered consumer",
            0,
            MessageGroupBoundary::StartsGroup,
        )
        .expect("the port runs with one consumer");
    newest_consumer_input.receive_pending();
    newest_consumer_input.drain("in1");

    let ordered_link = add_link_from_out1_to_in1(&mut graph, &source_id, &ordered_consumer_id);
    open_iceoryx2_service(&mut graph, &ordered_link, &node)
        .expect("an ordered consumer connects onto the running port");

    for bag in 1..=BAGS_PUBLISHED_WHILE_NEITHER_CONSUMER_READS {
        source_output
            .write_bytes_with_timestamp(
                "out1",
                b"a bag both consumers are fed",
                bag as i64,
                MessageGroupBoundary::StartsGroup,
            )
            .expect("the port publishes to both consumers");
    }
    newest_consumer_input.receive_pending();
    ordered_consumer_input.receive_pending();

    assert_eq!(
        newest_consumer_input.drain("in1").len(),
        DeliveryProfile::NEWEST_DEPTH,
        "the newest port holds its own depth"
    );
    assert_eq!(
        graph
            .traversal_mut()
            .v(NodeRunId::from(newest_consumer_id.as_str()))
            .first()
            .expect("the newest consumer is in the graph")
            .serialize_components()["metrics"]["dropped_bags_by_link"][newest_link.as_str()],
        serde_json::json!(0),
        "the newest port's ring, not its mailbox, held it to its depth"
    );
    assert_eq!(
        ordered_consumer_input.drain("in1").len(),
        BAGS_PUBLISHED_WHILE_NEITHER_CONSUMER_READS,
        "the ordered port receives every bag its deeper ring holds"
    );
}

fn the_node_a_spawn_creates_for(graph: &mut Graph, proc_id: &str) -> NodeInstance {
    graph
        .node_types_this_stream_resolves()
        .create(
            graph
                .traversal()
                .v(proc_id)
                .first()
                .expect("the node is in the graph"),
        )
        .expect("the node's type creates its node")
}

#[test]
fn a_spawned_nodes_writer_and_mailboxes_reach_their_nodes_stream_facts() {
    use crate::core::descriptors::ImageStreamFacts;

    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let source = the_node_a_spawn_creates_for(&mut graph, &source_id);
    let source_output = crate::iceoryx2::OutputWriter::from_inner_arc(
        source
            .iceoryx2_output_writer_inner()
            .expect("an output-only mock holds an output writer"),
    );
    attach_node_instance(&mut graph, &source_id, source);
    let dest_id = add_mock_input_only(&mut graph);
    let dest = the_node_a_spawn_creates_for(&mut graph, &dest_id);
    let dest_input = crate::iceoryx2::InputMailboxes::from_inner_arc(
        dest.iceoryx2_input_mailboxes_inner()
            .expect("an input-only mock holds input mailboxes"),
    );
    attach_node_instance(&mut graph, &dest_id, dest);

    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
    open_iceoryx2_service(&mut graph, &link_id, &node).expect("the link wires");
    assert_eq!(
        dest_input
            .upstream_stream_facts("in1", None)
            .expect("a port fed by one link answers"),
        None,
        "the upstream Records port states no facts"
    );

    let refusal = source_output
        .set_stream_facts("out1", ImageStreamFacts::default())
        .expect_err("the node's Records port carries no Image facts")
        .to_string();
    assert!(
        refusal.contains("out1") && refusal.contains("Records"),
        "the writer reached the node's ports; got {refusal}"
    );

    close_iceoryx2_service(&mut graph, &link_id).expect("the link disconnects");
    assert_eq!(
        dest_input
            .upstream_stream_facts("in1", None)
            .expect("an unfed port answers"),
        None
    );
}

/// The first consumer of a channel does not size it: one first wired to a
/// `newest` port is created at the depth any consumer needs.
///
/// Fail-without-fix: size the service by the first consumer's profile and
/// the link's held service states four.
#[test]
fn a_channel_first_wired_to_a_newest_consumer_is_created_deep_enough_for_any_consumer() {
    use crate::core::test_support::{MockInputOnlyNode, MockOutputOnlyNode};

    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let newest_consumer_id = add_mock_input_only(&mut graph);
    attach_mock_instance::<MockInputOnlyNode::Node>(&mut graph, &newest_consumer_id);
    let newest_link = add_link_from_out1_to_in1(&mut graph, &source_id, &newest_consumer_id);

    open_iceoryx2_service(&mut graph, &newest_link, &node).expect("the newest consumer wires");

    let held_creation_depth = graph
        .traversal_mut()
        .e(&newest_link)
        .first()
        .expect("the link is in the graph")
        .get::<Iceoryx2ServicesHeldOpenForLinkComponent>()
        .expect("a wired link holds its services")
        .channel_data_service
        .channel_service_creation_depth();
    assert_eq!(held_creation_depth, DeliveryProfile::ORDERED_DEPTH);
}

/// On a link between two helpers the engine opens no port of its own, yet
/// it still decides the channel's size: a helper opening its end — first,
/// and asking for only its own port's depth — joins the service the engine
/// created.
///
/// Fail-without-fix: drop the held services from the link and the service
/// is gone when the op returns, so the helper creates it at four.
#[test]
fn a_helper_opening_first_joins_the_channel_the_engine_created_between_two_helpers() {
    let (_graph_holding_the_link, source_id, _) =
        graph_with_one_wired_link_between_two_helper_stubs();

    assert_eq!(
        creation_depth_a_helper_opening_out1_finds(&source_id),
        DeliveryProfile::ORDERED_DEPTH,
    );
}

/// What one link reports to `graph`, and why where that is an error.
fn link_state_in_graph(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> (crate::core::json_schema::LinkStateOutput, Option<String>) {
    let link = graph
        .traversal()
        .e(link_id)
        .first()
        .expect("the link must be in the graph");
    let rendered = crate::core::json_schema::LinkOutput::of_a_link(
        link,
        &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
    );
    (rendered.state, rendered.error_reason)
}

/// A link handed to a helper is `pending` until that helper says it opened
/// its port, and both ends of a helper-to-helper link have to say so.
///
/// Fail-without-fix: stamp `Wired` in the op as before and the first
/// assertion reads `wired` with neither helper having opened anything.
#[test]
fn a_link_handed_to_a_helper_is_pending_until_the_helper_says_it_opened_its_port() {
    use crate::core::json_schema::LinkStateOutput;
    let (graph, _, link_id, answers_owed) = graph_and_owed_answers_of_a_helper_to_helper_link();
    let [source_answer, dest_answer] = &answers_owed[..] else {
        panic!(
            "both helper ends owe an answer; got {} ",
            answers_owed.len()
        );
    };

    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Pending, None),
        "`connect` returns before either helper has opened anything"
    );

    source_answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Pending, None),
        "one end's answer does not wire a link the other end never opened"
    );

    dest_answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Wired, None)
    );
}

/// The runtime's publisher opens a helper source's port as a link's source
/// side would — the helper is handed the port's wiring and owes its answer,
/// which the hold keeps — whether or not a link out of the port exists, and
/// tells the helper to drop the port once the hold goes.
#[test]
fn the_runtimes_publisher_opens_a_helper_sources_port_and_keeps_its_answer() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
    let late_wired_links = far_side.late_wired_links.clone();
    let wire_answers_owed = far_side.wire_answers_owed.clone();
    let reclaimed_links = far_side.reclaimed_links.clone();
    attach_node_instance(
        &mut graph,
        &source_id,
        NodeInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                far_side,
                ProcessExecution::Reactive,
            ),
        )),
    );

    let held = open_the_source_side_for_a_reader_on_the_reserved_slot(
        &mut graph,
        &OutputLinkPortRef::new(&source_id, "out1"),
        &Iceoryx2Node::for_this_test_process(),
    )
    .expect("a helper source's port opens for the publisher");

    let handed_over = late_wired_links.lock().clone();
    assert_eq!(handed_over.len(), 1, "{handed_over:?}");
    assert_eq!(handed_over[0].0, crate::core::PortDirection::Output);
    assert_eq!(handed_over[0].1["name"], "out1");
    let answer_kept = held
        .wire_reply_awaited_from_the_source
        .clone()
        .expect("a helper source owes its answer");
    assert!(Arc::ptr_eq(&answer_kept, &wire_answers_owed.lock()[0]));

    drop(held);
    assert_eq!(
        reclaimed_links.lock().len(),
        1,
        "the helper is told to drop the port the publisher held open"
    );
}

/// A helper that could not open its port leaves the link in error with its
/// own reason, and `graph` renders that reason.
#[test]
fn a_helper_that_cannot_open_its_port_leaves_the_link_in_error_with_its_reason() {
    use crate::core::json_schema::LinkStateOutput;
    let (graph, _, link_id, answers_owed) = graph_and_owed_answers_of_a_helper_to_helper_link();

    answers_owed[1].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
        reason: "ExceedsMaxSupportedSubscribers".to_string(),
    });

    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (
            LinkStateOutput::Error,
            Some("ExceedsMaxSupportedSubscribers".to_string())
        ),
        "a refusal is the link's answer even while the other end is still silent, and the \
         reason is what the caller of a live `connect` has to read"
    );

    answers_owed[0].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    assert_eq!(
        link_state_in_graph(&graph, &link_id).0,
        LinkStateOutput::Error,
        "the other end opening does not rescue a link one end refused"
    );
}

/// The reason lives as long as the link does, and goes with it.
#[test]
fn a_disconnected_link_stops_rendering_the_reason_it_was_refused_for() {
    use crate::core::json_schema::LinkStateOutput;
    let (mut graph, _, link_id, answers_owed) = graph_and_owed_answers_of_a_helper_to_helper_link();
    answers_owed[0].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
        reason: "no such service".to_string(),
    });

    close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Disconnected, None),
    );
}

/// A link wholly inside the app process waits on nobody and is wired the
/// moment the op returns — the startup envelope's arm is the same one,
/// since a helper with no bridge yet hands back no answer to wait on.
#[test]
fn a_link_no_helper_has_to_answer_for_is_wired_as_soon_as_it_is_opened() {
    use crate::core::json_schema::LinkStateOutput;
    use crate::core::test_support::{MockInputOnlyNode, MockOutputOnlyNode};

    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_input_only(&mut graph);
    attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    attach_mock_instance::<MockInputOnlyNode::Node>(&mut graph, &dest_id);
    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);

    open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
        .expect("an app-process link wires");

    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Wired, None)
    );
}

#[test]
fn a_link_between_one_helpers_own_ports_waits_on_both_of_its_ends() {
    use crate::core::json_schema::LinkStateOutput;
    crate::core::test_support::ensure_test_mocks_registered();
    let mut graph = Graph::new();
    let helper_id = graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            crate::core::test_support::MockNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("the four-port mock must be in the registry")
        .id
        .to_string();
    let answers_owed: Arc<Mutex<Vec<Arc<OutOfProcessLinkWireReply>>>> = Arc::default();
    attach_node_instance(
        &mut graph,
        &helper_id,
        NodeInstance::new(Box::new(
            OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                RecordingOutOfProcessFarSideLinkDelivery {
                    wire_answers_owed: answers_owed.clone(),
                    ..Default::default()
                },
                ProcessExecution::Reactive,
            ),
        )),
    );
    let link_id = add_link_from_out1_to_in1(&mut graph, &helper_id, &helper_id);

    open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
        .expect("a link between one helper's own ports wires");

    let answers_owed = answers_owed.lock().clone();
    assert_eq!(
        answers_owed.len(),
        2,
        "the engine hands this helper both ends of the link, so it owes two answers"
    );
    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Pending, None)
    );

    answers_owed[0].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Pending, None),
        "one end opening does not wire a link whose other end is the same helper"
    );

    answers_owed[1].note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Wired, None)
    );
}

const LONGEST_A_WIRING_OP_MAY_TAKE_BESIDE_A_HELPER_SETTING_UP: std::time::Duration =
    std::time::Duration::from_secs(5);

fn run_while_both_node_locks_are_held<T: Send>(
    source: &Arc<Mutex<NodeInstance>>,
    dest: &Arc<Mutex<NodeInstance>>,
    wiring_op: impl FnOnce() -> T + Send,
) -> Option<T> {
    let source_setting_up = source.lock();
    let dest_setting_up = dest.lock();
    std::thread::scope(|scope| {
        let (wiring_op_returned_tx, wiring_op_returned_rx) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let _ = wiring_op_returned_tx.send(wiring_op());
        });
        let returned = wiring_op_returned_rx
            .recv_timeout(LONGEST_A_WIRING_OP_MAY_TAKE_BESIDE_A_HELPER_SETTING_UP)
            .ok();
        drop(source_setting_up);
        drop(dest_setting_up);
        returned
    })
}

/// Two helper stubs whose far sides record into one delivery, and a link
/// from the source's `out1` to the destination's `in1` not yet wired.
struct TwoHelperStubsAndAnUnwiredLinkBetweenThem {
    graph: Graph,
    source: Arc<Mutex<NodeInstance>>,
    dest: Arc<Mutex<NodeInstance>>,
    link_id: LinkUniqueId,
    far_side: RecordingOutOfProcessFarSideLinkDelivery,
}

impl TwoHelperStubsAndAnUnwiredLinkBetweenThem {
    fn new() -> Self {
        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let dest_id = add_mock_input_only(&mut graph);
        let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
        let mut attach_a_helper_stub = |proc_id: &str| {
            attach_node_instance(
                &mut graph,
                proc_id,
                NodeInstance::new(Box::new(
                    OutOfProcessSpawnHostStub::with_a_far_side_past_its_setup_command(
                        far_side.clone(),
                        ProcessExecution::Reactive,
                    ),
                )),
            )
        };
        let source = attach_a_helper_stub(&source_id);
        let dest = attach_a_helper_stub(&dest_id);
        let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
        Self {
            graph,
            source,
            dest,
            link_id,
            far_side,
        }
    }
}

#[test]
fn a_link_between_helpers_still_setting_up_is_handed_over_without_their_node_locks() {
    use crate::core::json_schema::LinkStateOutput;
    let TwoHelperStubsAndAnUnwiredLinkBetweenThem {
        mut graph,
        source,
        dest,
        link_id,
        far_side,
    } = TwoHelperStubsAndAnUnwiredLinkBetweenThem::new();

    let wired = run_while_both_node_locks_are_held(&source, &dest, || {
        open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
    })
    .expect("the op returns while both helpers still hold their node locks");

    wired.expect("the link wires");
    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Pending, None)
    );
    let handed_over = far_side.late_wired_links.lock();
    let directions: Vec<crate::core::PortDirection> = handed_over
        .iter()
        .map(|(direction, _)| *direction)
        .collect();
    assert_eq!(
        directions,
        [
            crate::core::PortDirection::Output,
            crate::core::PortDirection::Input
        ],
        "both helpers are handed their end of the link"
    );
}

#[test]
fn a_disconnect_between_helpers_still_setting_up_reclaims_both_ends_without_their_node_locks() {
    use crate::core::json_schema::LinkStateOutput;
    let TwoHelperStubsAndAnUnwiredLinkBetweenThem {
        mut graph,
        source,
        dest,
        link_id,
        far_side,
    } = TwoHelperStubsAndAnUnwiredLinkBetweenThem::new();
    open_iceoryx2_service(&mut graph, &link_id, &Iceoryx2Node::for_this_test_process())
        .expect("the link wires");

    let closed = run_while_both_node_locks_are_held(&source, &dest, || {
        close_iceoryx2_service(&mut graph, &link_id)
    })
    .expect("the op returns while both helpers still hold their node locks");

    closed.expect("the disconnect succeeds");
    assert_eq!(
        link_state_in_graph(&graph, &link_id),
        (LinkStateOutput::Disconnected, None)
    );
    assert_eq!(
        far_side.reclaimed_links.lock().len(),
        2,
        "both helpers are told to drop their end of the link"
    );
}

/// A disconnected link lets go of the services it held, so a channel whose
/// last link went is created afresh by whoever opens it next.
///
/// Fail-without-fix: keep the held services on a disconnected link and the
/// reopen below still finds the old service's depth.
#[test]
fn a_disconnected_link_releases_the_services_it_held() {
    let (mut graph, source_id, link_id) = graph_with_one_wired_link_between_two_helper_stubs();

    close_iceoryx2_service(&mut graph, &link_id).expect("the disconnect must succeed");

    assert_eq!(
        creation_depth_a_helper_opening_out1_finds(&source_id),
        DeliveryProfile::NEWEST_DEPTH,
        "nothing may still hold the channel of a link that is gone"
    );
}

/// A destination linked after its source's port kept a message reads that message as soon as
/// the link is opened, with nothing written after it.
///
/// Fail-without-fix: drop the send after the destination side opens, and the destination's
/// mailbox is empty until the source's own scheduling loop comes round.
#[test]
fn a_destination_linked_after_a_kept_message_reads_it_once_the_link_opens() {
    use crate::core::test_support::{MockInputOnlyNode, MockOutputOnlyNode};

    let node = Iceoryx2Node::for_this_test_process();
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let (_, source_output, _) =
        attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let source_output = source_output.expect("an output-only mock holds an output writer");
    source_output.keep_the_last_message_of_output_ports(["out1".to_string()]);
    source_output
        .write_bytes_with_timestamp(
            "out1",
            b"the value before any link",
            0,
            MessageGroupBoundary::StartsGroup,
        )
        .expect("a write to a declared port no link reached sends nothing");
    let dest_id = add_mock_input_only(&mut graph);
    let (_, _, dest_input) = attach_mock_instance::<MockInputOnlyNode::Node>(&mut graph, &dest_id);
    let dest_input = dest_input.expect("an input-only mock holds input mailboxes");

    let link_id = add_link_from_out1_to_in1(&mut graph, &source_id, &dest_id);
    open_iceoryx2_service(&mut graph, &link_id, &node).expect("the link wires");
    dest_input.receive_pending();

    assert_eq!(
        dest_input.drain("in1").len(),
        1,
        "the destination reads the kept message"
    );
}
